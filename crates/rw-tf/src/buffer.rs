//! The transform tree: who is attached to whom, and where they were when.
//!
//! Every frame except a root has exactly one parent, and the transform stored
//! on that edge places the child's contents in the parent. A lookup walks both
//! frames up to the first ancestor they share and composes what it passed
//! through, which is the whole of what RViz's "fixed frame" does.
//!
//! Two things here are deliberate and load-bearing:
//!
//! * **Samples are interpolated, never extrapolated.** A scan taken at *t* and
//!   a transform published at *t − 40 ms* and *t + 60 ms* gives an answer; the
//!   same scan against a transform tree that stopped 3 seconds ago gives an
//!   error naming both frames and the size of the gap. Guessing where a robot
//!   probably was is how a visualiser ends up confidently wrong, and "nearly
//!   right" is the hardest kind of wrong to notice.
//! * **Nothing here is unbounded.** A tree is fed from a live socket, so each
//!   edge keeps a window of history and drops what falls out of it.

use std::collections::{BTreeMap, HashSet, VecDeque};

use crate::quat::Quat;

/// How much history each edge keeps, in nanoseconds. Ten seconds is tf2's own
/// default, and it is the number people's intuitions about "recent enough" are
/// already calibrated to.
pub const DEFAULT_WINDOW_NS: u64 = 10_000_000_000;

/// A hard ceiling on samples per edge, whatever the window says.
///
/// The window alone bounds a well-behaved publisher; this bounds a
/// misconfigured one flooding a single edge at kilohertz, which would otherwise
/// be ten seconds of unbounded growth.
pub const MAX_SAMPLES: usize = 4096;

/// The stamp meaning "whatever you have most recently".
///
/// ROS spells this `Time(0)`, and it is what a live view wants: a pane drawing
/// the current state of a robot is asking for the newest transform, not for the
/// transform at the epoch.
pub const LATEST: u64 = 0;

/// A rigid placement: a rotation, then a translation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    pub translation: [f32; 3],
    pub rotation: Quat,
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Transform {
    pub const IDENTITY: Self = Self {
        translation: [0.; 3],
        rotation: Quat::IDENTITY,
    };

    pub fn new(translation: [f32; 3], rotation: Quat) -> Self {
        Self {
            translation,
            rotation,
        }
    }

    /// Just a shift, no turn.
    pub fn translation(translation: [f32; 3]) -> Self {
        Self::new(translation, Quat::IDENTITY)
    }

    /// `outer` applied after `self` — the composition a walk up the tree makes,
    /// where `self` places a point in its parent and `outer` places the parent
    /// in the grandparent.
    pub fn then(&self, outer: &Self) -> Self {
        Self {
            translation: add(outer.translation, outer.rotation.rotate(self.translation)),
            rotation: outer.rotation.multiply(self.rotation),
        }
    }

    /// The placement that undoes this one.
    pub fn inverse(&self) -> Self {
        let rotation = self.rotation.conjugate();
        let turned = rotation.rotate(self.translation);
        Self {
            translation: [-turned[0], -turned[1], -turned[2]],
            rotation,
        }
    }

    /// Moves a point out of this frame and into its parent.
    pub fn apply(&self, point: [f32; 3]) -> [f32; 3] {
        add(self.translation, self.rotation.rotate(point))
    }

    /// A column-major 4×4 — the same concrete type as `rw_assets::math::Mat4`
    /// and `rw_render::Mat4`, so it hands to either without a conversion and
    /// without this crate depending on either.
    pub fn to_mat4(&self) -> [[f32; 4]; 4] {
        let mut matrix = self.rotation.to_mat4();
        matrix[3] = [
            self.translation[0],
            self.translation[1],
            self.translation[2],
            1.,
        ];
        matrix
    }

    /// Blends towards `other`: straight-line on the translation, along the arc
    /// on the rotation.
    pub fn blend(&self, other: &Self, t: f32) -> Self {
        let t = t.clamp(0., 1.);
        Self {
            translation: [
                self.translation[0] + (other.translation[0] - self.translation[0]) * t,
                self.translation[1] + (other.translation[1] - self.translation[1]) * t,
                self.translation[2] + (other.translation[2] - self.translation[2]) * t,
            ],
            rotation: self.rotation.slerp(other.rotation, t),
        }
    }
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

/// Which end of an edge's history a lookup fell off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Asked for a time before anything was recorded.
    Past,
    /// Asked for a time after the last thing recorded — the common one, and
    /// what "the robot stopped publishing TF" looks like.
    Future,
}

impl Side {
    pub fn label(self) -> &'static str {
        match self {
            Self::Past => "before",
            Self::Future => "after",
        }
    }
}

/// Why a lookup could not be answered.
///
/// Every variant names both ends of the lookup, because the message is read in
/// a pane listing a dozen layers and "extrapolation into the future" on its own
/// says nothing about which one went wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TfError {
    /// A frame nothing has ever mentioned.
    UnknownFrame {
        target: String,
        source: String,
        /// The one that is missing.
        frame: String,
    },
    /// Both frames exist, but in trees that never meet.
    Disconnected {
        target: String,
        source: String,
        /// As far up as `source` goes.
        source_root: String,
        /// As far up as `target` goes.
        target_root: String,
    },
    /// The edge exists but was not published near enough to the time asked for.
    Extrapolation {
        target: String,
        source: String,
        /// The child end of the edge that could not answer.
        frame: String,
        /// Its parent.
        parent: String,
        /// The time asked for.
        at_ns: u64,
        /// How far outside the edge's history that time is.
        gap_ns: u64,
        side: Side,
    },
    /// The edge exists but has never carried a sample.
    NoSamples {
        target: String,
        source: String,
        frame: String,
        parent: String,
    },
    /// A frame is its own ancestor, so walking up would never end.
    Cycle {
        target: String,
        source: String,
        /// Where the walk came back round to itself.
        frame: String,
    },
}

impl TfError {
    /// The frame the failure is actually about, for a pane wanting to point at
    /// one thing.
    pub fn frame(&self) -> &str {
        match self {
            Self::UnknownFrame { frame, .. }
            | Self::Extrapolation { frame, .. }
            | Self::NoSamples { frame, .. }
            | Self::Cycle { frame, .. } => frame,
            Self::Disconnected { source_root, .. } => source_root,
        }
    }

    /// How far outside its history the lookup fell, when that is what happened.
    pub fn gap_ns(&self) -> Option<u64> {
        match self {
            Self::Extrapolation { gap_ns, .. } => Some(*gap_ns),
            _ => None,
        }
    }
}

/// A duration a person can read, from nanoseconds.
///
/// The gap is the number the reader acts on — 30 ms is a late publisher and
/// 40 s is a robot that has gone away — and eleven digits of nanoseconds hides
/// which of those it is.
fn readable(ns: u64) -> String {
    match ns {
        0..=999 => format!("{ns} ns"),
        1_000..=999_999 => format!("{:.1} µs", ns as f64 / 1e3),
        1_000_000..=999_999_999 => format!("{:.0} ms", ns as f64 / 1e6),
        _ => format!("{:.1} s", ns as f64 / 1e9),
    }
}

impl std::fmt::Display for TfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownFrame {
                target,
                source,
                frame,
            } => write!(
                f,
                "no transform from `{source}` to `{target}`: nothing has published frame `{frame}`"
            ),
            Self::Disconnected {
                target,
                source,
                source_root,
                target_root,
            } => write!(
                f,
                "no transform from `{source}` to `{target}`: they are in separate trees \
                 (`{source}` goes up to `{source_root}`, `{target}` to `{target_root}`)"
            ),
            Self::Extrapolation {
                target,
                source,
                frame,
                parent,
                gap_ns,
                side,
                ..
            } => write!(
                f,
                "no transform from `{source}` to `{target}`: `{parent}` → `{frame}` \
                 is {gap} {side} the end of its history, and extrapolating is refused",
                gap = readable(*gap_ns),
                side = side.label(),
            ),
            Self::NoSamples {
                target,
                source,
                frame,
                parent,
            } => write!(
                f,
                "no transform from `{source}` to `{target}`: `{parent}` → `{frame}` \
                 has been announced but never published"
            ),
            Self::Cycle {
                target,
                source,
                frame,
            } => write!(
                f,
                "no transform from `{source}` to `{target}`: `{frame}` is its own ancestor"
            ),
        }
    }
}

impl std::error::Error for TfError {}

/// One row of [`Buffer::tree`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub frame: String,
    /// `None` for a root.
    pub parent: Option<String>,
    /// How many edges up from its root, so a list can be indented into a tree.
    pub depth: usize,
    /// Published on `/tf_static`, so it never goes stale.
    pub is_static: bool,
    /// How much history the edge above it holds.
    pub samples: usize,
    /// The newest stamp on that edge, if it has one.
    pub newest_ns: Option<u64>,
}

/// One edge of the tree: a child, its parent, and where it has been.
#[derive(Debug, Clone, Default)]
struct Link {
    parent: String,
    /// Oldest first.
    samples: VecDeque<(u64, Transform)>,
    /// Set by `/tf_static`, which carries no meaningful time.
    fixed: Option<Transform>,
}

impl Link {
    fn newest_ns(&self) -> Option<u64> {
        self.samples.back().map(|(at, _)| *at)
    }

    /// Where this edge was at `at_ns`, or why it cannot say.
    ///
    /// Live samples win over a static entry on the same edge. Publishing one
    /// frame on both `/tf` and `/tf_static` is a configuration error, and of
    /// the two the timed samples are the ones carrying information.
    fn at(&self, at_ns: u64) -> Result<Transform, (Side, u64)> {
        let Some((oldest, _)) = self.samples.front() else {
            // `Future` with no gap: there is no history to be outside of, and
            // the caller turns an empty edge into `NoSamples` anyway.
            return self.fixed.ok_or((Side::Future, 0));
        };
        let (newest, newest_value) = self.samples.back().expect("a front implies a back");

        if at_ns == LATEST {
            return Ok(*newest_value);
        }
        if at_ns < *oldest {
            return Err((Side::Past, oldest - at_ns));
        }
        if at_ns > *newest {
            return Err((Side::Future, at_ns - newest));
        }

        // Between the ends, so a bracketing pair exists.
        let upper = self
            .samples
            .partition_point(|(stamp, _)| *stamp < at_ns)
            .min(self.samples.len() - 1);
        let (after_at, after) = self.samples[upper];
        if after_at == at_ns || upper == 0 {
            return Ok(after);
        }
        let (before_at, before) = self.samples[upper - 1];
        let span = after_at - before_at;
        if span == 0 {
            return Ok(after);
        }
        Ok(before.blend(&after, (at_ns - before_at) as f32 / span as f32))
    }
}

/// Every frame this connection has heard about, and where each has been.
#[derive(Debug, Clone)]
pub struct Buffer {
    /// Keyed by the *child* frame: a frame has one parent, which is what makes
    /// the tree a tree.
    links: BTreeMap<String, Link>,
    window_ns: u64,
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new()
    }
}

impl Buffer {
    pub fn new() -> Self {
        Self::with_window(DEFAULT_WINDOW_NS)
    }

    pub fn with_window(window_ns: u64) -> Self {
        Self {
            links: BTreeMap::new(),
            window_ns: window_ns.max(1),
        }
    }

    pub fn window_ns(&self) -> u64 {
        self.window_ns
    }

    /// Changes how much history is kept, trimming what no longer fits.
    ///
    /// Trimming happens here rather than on the next sample so that shortening
    /// the window frees the memory it was asked to free, on a tree that may not
    /// hear from its robot again for a while.
    pub fn set_window(&mut self, window_ns: u64) {
        self.window_ns = window_ns.max(1);
        for link in self.links.values_mut() {
            let Some((newest, _)) = link.samples.back().copied() else {
                continue;
            };
            let horizon = newest.saturating_sub(self.window_ns);
            while link
                .samples
                .front()
                .is_some_and(|(stamp, _)| *stamp < horizon)
            {
                link.samples.pop_front();
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }

    /// Records where `child` was inside `parent` at `at_ns`.
    pub fn insert(&mut self, parent: &str, child: &str, at_ns: u64, transform: Transform) {
        let window_ns = self.window_ns;
        let link = self.relink(parent, child);
        // Out-of-order arrivals are ordinary: `/tf` is several publishers
        // multiplexed onto one topic, and they are not in lockstep. Almost
        // always the new sample belongs at the back, so that case is checked
        // before the search.
        let position = match link.samples.back() {
            Some((last, _)) if *last <= at_ns => link.samples.len(),
            _ => link.samples.partition_point(|(stamp, _)| *stamp < at_ns),
        };
        link.samples.insert(position, (at_ns, transform));

        // Trim from the front: anything older than the window behind the newest
        // sample can never be interpolated to again.
        let newest = link.samples.back().map(|(at, _)| *at).unwrap_or(at_ns);
        let horizon = newest.saturating_sub(window_ns);
        while link
            .samples
            .front()
            .is_some_and(|(stamp, _)| *stamp < horizon)
        {
            link.samples.pop_front();
        }
        while link.samples.len() > MAX_SAMPLES {
            link.samples.pop_front();
        }
    }

    /// Records where `child` sits inside `parent`, for good.
    pub fn insert_static(&mut self, parent: &str, child: &str, transform: Transform) {
        self.relink(parent, child).fixed = Some(transform);
    }

    /// The edge for `child`, reset if it has been re-parented.
    ///
    /// A frame that changes parent is rare and always deliberate — a robot
    /// picking something up, a localiser taking over. Its history under the old
    /// parent describes a different place, so keeping it would let a lookup
    /// interpolate across the moment of the change and put the object
    /// somewhere it never was.
    fn relink(&mut self, parent: &str, child: &str) -> &mut Link {
        let link = self.links.entry(child.to_string()).or_default();
        if link.parent != parent {
            link.parent = parent.to_string();
            link.samples.clear();
            link.fixed = None;
        }
        link
    }

    /// Where `source` sits inside `target`, at `at_ns`.
    ///
    /// Apply the result to a point expressed in `source` and it comes back
    /// expressed in `target` — which is exactly what a layer needs to be drawn
    /// in the pane's fixed frame. Pass [`LATEST`] for the newest available.
    pub fn lookup(&self, target: &str, source: &str, at_ns: u64) -> Result<Transform, TfError> {
        if target == source {
            // Not a special case for speed: a frame is trivially at the origin
            // of itself even when nothing has ever published it, and a pane
            // showing one frame in its own coordinates must not need a tree.
            return Ok(Transform::IDENTITY);
        }
        let source_chain = self.ascend(target, source, source)?;
        let target_chain = self.ascend(target, source, target)?;

        let above_target: HashSet<&str> = target_chain.iter().map(String::as_str).collect();
        let Some(meeting) = source_chain
            .iter()
            .find(|frame| above_target.contains(frame.as_str()))
        else {
            return Err(TfError::Disconnected {
                target: target.to_string(),
                source: source.to_string(),
                source_root: source_chain.last().cloned().unwrap_or_default(),
                target_root: target_chain.last().cloned().unwrap_or_default(),
            });
        };

        let up_from_source = self.compose(target, source, &source_chain, meeting, at_ns)?;
        let up_from_target = self.compose(target, source, &target_chain, meeting, at_ns)?;
        Ok(up_from_source.then(&up_from_target.inverse()))
    }

    /// The frames from `frame` up to its root, `frame` first.
    fn ascend(&self, target: &str, source: &str, frame: &str) -> Result<Vec<String>, TfError> {
        if !self.knows(frame) {
            return Err(TfError::UnknownFrame {
                target: target.to_string(),
                source: source.to_string(),
                frame: frame.to_string(),
            });
        }
        let mut chain = vec![frame.to_string()];
        let mut seen: HashSet<&str> = HashSet::from([frame]);
        let mut current = frame;
        while let Some(link) = self.links.get(current) {
            let parent = link.parent.as_str();
            if !seen.insert(parent) {
                // A frame is its own ancestor. Walking on would never end, and
                // a tree with a loop in it is a publisher bug worth naming
                // rather than a hang worth debugging.
                return Err(TfError::Cycle {
                    target: target.to_string(),
                    source: source.to_string(),
                    frame: parent.to_string(),
                });
            }
            chain.push(parent.to_string());
            current = parent;
        }
        Ok(chain)
    }

    /// Composes the edges of `chain` from its start up to (not including)
    /// `until`, giving the transform that places the first frame in `until`.
    fn compose(
        &self,
        target: &str,
        source: &str,
        chain: &[String],
        until: &str,
        at_ns: u64,
    ) -> Result<Transform, TfError> {
        let mut accumulated = Transform::IDENTITY;
        for frame in chain {
            if frame == until {
                break;
            }
            let link = self
                .links
                .get(frame)
                .expect("a chain only holds known frames");
            let step = link.at(at_ns).map_err(|(side, gap_ns)| {
                if link.samples.is_empty() && link.fixed.is_none() {
                    TfError::NoSamples {
                        target: target.to_string(),
                        source: source.to_string(),
                        frame: frame.clone(),
                        parent: link.parent.clone(),
                    }
                } else {
                    TfError::Extrapolation {
                        target: target.to_string(),
                        source: source.to_string(),
                        frame: frame.clone(),
                        parent: link.parent.clone(),
                        at_ns,
                        gap_ns,
                        side,
                    }
                }
            })?;
            accumulated = accumulated.then(&step);
        }
        Ok(accumulated)
    }

    /// Whether anything has ever mentioned this frame, as a child or a parent.
    pub fn knows(&self, frame: &str) -> bool {
        self.links.contains_key(frame) || self.links.values().any(|link| link.parent == frame)
    }

    /// Every frame in the tree, in name order. The fixed-frame selector's list.
    pub fn frames(&self) -> Vec<String> {
        let mut frames: Vec<String> = self.links.keys().cloned().collect();
        for link in self.links.values() {
            if !self.links.contains_key(&link.parent) {
                frames.push(link.parent.clone());
            }
        }
        frames.sort();
        frames.dedup();
        frames
    }

    /// The tree, depth first from each root, for a view that draws it.
    ///
    /// Frames caught in a cycle are unreachable from any root; they are
    /// appended at the end rather than dropped, because a tree view that
    /// silently omits the broken part is the opposite of useful.
    pub fn tree(&self) -> Vec<Node> {
        let mut children: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (child, link) in &self.links {
            children
                .entry(link.parent.as_str())
                .or_default()
                .push(child.as_str());
        }

        let roots: Vec<&str> = self
            .links
            .values()
            .map(|link| link.parent.as_str())
            .filter(|parent| !self.links.contains_key(*parent))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();

        let mut nodes = Vec::new();
        let mut placed: HashSet<&str> = HashSet::new();
        for root in roots {
            let mut stack = vec![(root, 0usize)];
            while let Some((frame, depth)) = stack.pop() {
                if !placed.insert(frame) {
                    continue;
                }
                nodes.push(self.node(frame, depth));
                // Reversed, so a stack yields them alphabetically.
                for child in children.get(frame).into_iter().flatten().rev() {
                    stack.push((child, depth + 1));
                }
            }
        }
        for frame in self.links.keys() {
            if placed.insert(frame.as_str()) {
                nodes.push(self.node(frame, 0));
            }
        }
        nodes
    }

    fn node(&self, frame: &str, depth: usize) -> Node {
        let link = self.links.get(frame);
        Node {
            frame: frame.to_string(),
            parent: link.map(|link| link.parent.clone()),
            depth,
            is_static: link.is_some_and(|link| link.fixed.is_some() && link.samples.is_empty()),
            samples: link.map_or(0, |link| link.samples.len()),
            newest_ns: link.and_then(Link::newest_ns),
        }
    }

    /// How long ago `frame` was last placed, as of `now_ns`.
    ///
    /// `Some(0)` for a static frame: it was true when it was published and it
    /// is true now, so showing it ageing would be a lie about the data.
    /// `None` for a root, or a frame nothing has published.
    pub fn age_ns(&self, frame: &str, now_ns: u64) -> Option<u64> {
        let link = self.links.get(frame)?;
        match link.newest_ns() {
            Some(newest) => Some(now_ns.saturating_sub(newest)),
            None => link.fixed.map(|_| 0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::FRAC_PI_2;

    const MS: u64 = 1_000_000;

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-4)
    }

    fn shifted(x: f32) -> Transform {
        Transform::translation([x, 0., 0.])
    }

    fn turned(angle: f32) -> Transform {
        Transform::new([0.; 3], Quat::from_axis_angle([0., 0., 1.], angle))
    }

    #[test]
    fn a_frame_is_at_the_origin_of_itself() {
        let buffer = Buffer::new();
        assert_eq!(buffer.lookup("map", "map", LATEST), Ok(Transform::IDENTITY));
    }

    #[test]
    fn one_edge_places_a_point_in_its_parent() {
        let mut buffer = Buffer::new();
        buffer.insert("map", "base", 1000 * MS, shifted(5.));
        let placed = buffer.lookup("map", "base", 1000 * MS).expect("looks up");
        assert!(close(placed.apply([1., 0., 0.]), [6., 0., 0.]));
    }

    #[test]
    fn looking_the_other_way_round_is_the_inverse() {
        let mut buffer = Buffer::new();
        buffer.insert("map", "base", 1000 * MS, shifted(5.));
        let back = buffer.lookup("base", "map", 1000 * MS).expect("looks up");
        assert!(close(back.apply([6., 0., 0.]), [1., 0., 0.]));
    }

    #[test]
    fn a_time_between_two_samples_is_interpolated() {
        let mut buffer = Buffer::new();
        buffer.insert("map", "base", 1000 * MS, shifted(0.));
        buffer.insert("map", "base", 2000 * MS, shifted(10.));

        let quarter = buffer.lookup("map", "base", 1250 * MS).expect("looks up");
        assert!(
            (quarter.translation[0] - 2.5).abs() < 1e-4,
            "got {}",
            quarter.translation[0]
        );
        let half = buffer.lookup("map", "base", 1500 * MS).expect("looks up");
        assert!((half.translation[0] - 5.).abs() < 1e-4);
    }

    #[test]
    fn rotation_is_interpolated_along_the_arc_not_component_by_component() {
        let mut buffer = Buffer::new();
        buffer.insert("map", "base", 0, turned(0.));
        buffer.insert("map", "base", 1000 * MS, turned(FRAC_PI_2));

        let half = buffer.lookup("map", "base", 500 * MS).expect("looks up");
        // A point one metre out along x must land on the 45° ray at unit
        // radius. Averaging the two rotation matrices would put it at radius
        // 0.924 — visibly shrunk, and the classic symptom of lerping rotations.
        let placed = half.apply([1., 0., 0.]);
        let radius = (placed[0] * placed[0] + placed[1] * placed[1]).sqrt();
        assert!((radius - 1.).abs() < 1e-4, "radius {radius}");
        assert!(close(
            placed,
            [(FRAC_PI_2 / 2.).cos(), (FRAC_PI_2 / 2.).sin(), 0.]
        ));
    }

    #[test]
    fn extrapolation_into_the_future_is_refused_and_names_both_frames_and_the_gap() {
        let mut buffer = Buffer::new();
        buffer.insert("map", "base", 1000 * MS, shifted(0.));
        buffer.insert("map", "base", 2000 * MS, shifted(10.));

        let error = buffer
            .lookup("map", "base", 2340 * MS)
            .expect_err("340 ms past the last sample is a guess, not an answer");
        let TfError::Extrapolation {
            target,
            source,
            frame,
            parent,
            gap_ns,
            side,
            ..
        } = &error
        else {
            panic!("expected an extrapolation error, got {error:?}");
        };
        assert_eq!(target, "map");
        assert_eq!(source, "base");
        assert_eq!(frame, "base");
        assert_eq!(parent, "map");
        assert_eq!(*gap_ns, 340 * MS);
        assert_eq!(*side, Side::Future);

        let shown = error.to_string();
        for expected in ["`base`", "`map`", "340 ms", "refused"] {
            assert!(shown.contains(expected), "{expected} missing from {shown}");
        }
    }

    #[test]
    fn extrapolation_into_the_past_is_refused_too() {
        let mut buffer = Buffer::new();
        buffer.insert("map", "base", 5000 * MS, shifted(0.));
        buffer.insert("map", "base", 6000 * MS, shifted(1.));

        let error = buffer
            .lookup("map", "base", 4500 * MS)
            .expect_err("before the history starts");
        assert_eq!(error.gap_ns(), Some(500 * MS));
        assert!(matches!(
            error,
            TfError::Extrapolation {
                side: Side::Past,
                ..
            }
        ));
    }

    #[test]
    fn a_single_sample_answers_at_its_own_stamp_and_nowhere_else() {
        let mut buffer = Buffer::new();
        buffer.insert("map", "base", 1000 * MS, shifted(3.));
        assert!(buffer.lookup("map", "base", 1000 * MS).is_ok());
        assert!(buffer.lookup("map", "base", 1000 * MS + 1).is_err());
        assert!(buffer.lookup("map", "base", 1000 * MS - 1).is_err());
    }

    #[test]
    fn the_latest_stamp_asks_for_the_newest_sample() {
        let mut buffer = Buffer::new();
        buffer.insert("map", "base", 1000 * MS, shifted(1.));
        buffer.insert("map", "base", 2000 * MS, shifted(2.));
        let newest = buffer.lookup("map", "base", LATEST).expect("looks up");
        assert!((newest.translation[0] - 2.).abs() < 1e-6);
    }

    #[test]
    fn a_chain_composes_in_the_right_order() {
        // map → odom → base_link → laser, the canonical navigation stack.
        let mut buffer = Buffer::new();
        let at = 1000 * MS;
        buffer.insert("map", "odom", at, shifted(10.));
        buffer.insert("odom", "base_link", at, turned(FRAC_PI_2));
        buffer.insert("base_link", "laser", at, shifted(2.));

        let placed = buffer.lookup("map", "laser", at).expect("looks up");
        // The laser sits 2 m along base_link's x; base_link is turned a quarter
        // turn inside odom, so that 2 m runs along odom's y; odom is 10 m along
        // map's x.
        assert!(
            close(placed.apply([0., 0., 0.]), [10., 2., 0.]),
            "{placed:?}"
        );
        // And a point one metre further along the laser's own x.
        assert!(close(placed.apply([1., 0., 0.]), [10., 3., 0.]));
    }

    #[test]
    fn two_branches_meet_at_their_common_ancestor_without_going_via_the_root() {
        let mut buffer = Buffer::new();
        let at = 1000 * MS;
        buffer.insert("map", "odom", at, shifted(100.));
        buffer.insert("odom", "base", at, shifted(0.));
        buffer.insert("base", "laser", at, shifted(1.));
        buffer.insert("base", "camera", at, shifted(-1.));

        let placed = buffer.lookup("camera", "laser", at).expect("looks up");
        assert!(close(placed.apply([0., 0., 0.]), [2., 0., 0.]));
    }

    #[test]
    fn a_branch_that_cannot_answer_does_not_break_a_lookup_below_it() {
        // `map → odom` stopped ten seconds ago, but `base → laser` is current,
        // and a lookup between two frames under `base` never needs `map`.
        let mut buffer = Buffer::new();
        buffer.insert("map", "odom", 1000 * MS, shifted(1.));
        buffer.insert("odom", "base", 9000 * MS, shifted(0.));
        buffer.insert("base", "laser", 9000 * MS, shifted(1.));
        buffer.insert("base", "camera", 9000 * MS, shifted(-1.));

        assert!(buffer.lookup("camera", "laser", 9000 * MS).is_ok());
        assert!(
            buffer.lookup("map", "laser", 9000 * MS).is_err(),
            "reaching map does need the stale edge"
        );
    }

    #[test]
    fn a_static_transform_answers_at_any_time() {
        let mut buffer = Buffer::new();
        buffer.insert_static("base_link", "laser", shifted(0.3));
        for at in [0, 1, 500 * MS, u64::MAX / 2] {
            let placed = buffer
                .lookup("base_link", "laser", at)
                .unwrap_or_else(|error| panic!("static lookup at {at} failed: {error}"));
            assert!(close(placed.apply([0., 0., 0.]), [0.3, 0., 0.]));
        }
    }

    #[test]
    fn a_static_edge_composes_with_a_moving_one() {
        let mut buffer = Buffer::new();
        buffer.insert_static("base_link", "laser", shifted(0.3));
        buffer.insert("map", "base_link", 1000 * MS, shifted(0.));
        buffer.insert("map", "base_link", 2000 * MS, shifted(4.));

        let placed = buffer.lookup("map", "laser", 1500 * MS).expect("looks up");
        assert!(
            close(placed.apply([0., 0., 0.]), [2.3, 0., 0.]),
            "{placed:?}"
        );
    }

    #[test]
    fn an_unreachable_frame_says_which_frame_broke_the_chain() {
        let mut buffer = Buffer::new();
        buffer.insert("map", "base", 1000 * MS, shifted(0.));

        let error = buffer
            .lookup("map", "gripper", 1000 * MS)
            .expect_err("gripper is not in the tree");
        assert_eq!(
            error,
            TfError::UnknownFrame {
                target: "map".into(),
                source: "gripper".into(),
                frame: "gripper".into(),
            }
        );
        assert!(error.to_string().contains("`gripper`"), "{error}");
    }

    #[test]
    fn two_separate_trees_name_both_roots() {
        let mut buffer = Buffer::new();
        buffer.insert("map", "base", 1000 * MS, shifted(0.));
        buffer.insert("world", "drone", 1000 * MS, shifted(0.));

        let error = buffer
            .lookup("base", "drone", 1000 * MS)
            .expect_err("nothing connects the two trees");
        let TfError::Disconnected {
            source_root,
            target_root,
            ..
        } = &error
        else {
            panic!("expected a disconnection, got {error:?}");
        };
        assert_eq!(source_root, "world");
        assert_eq!(target_root, "map");
        let shown = error.to_string();
        assert!(
            shown.contains("`world`") && shown.contains("`map`"),
            "{shown}"
        );
    }

    #[test]
    fn a_cycle_terminates_rather_than_walking_forever() {
        let mut buffer = Buffer::new();
        let at = 1000 * MS;
        buffer.insert("a", "b", at, shifted(1.));
        buffer.insert("b", "c", at, shifted(1.));
        buffer.insert("c", "a", at, shifted(1.));

        let error = buffer
            .lookup("a", "c", at)
            .expect_err("a tree with a loop in it cannot be walked");
        assert!(
            matches!(error, TfError::Cycle { .. }),
            "expected a cycle, got {error:?}"
        );
        assert!(error.to_string().contains("own ancestor"), "{error}");
    }

    #[test]
    fn a_frame_that_is_its_own_parent_is_a_cycle_too() {
        let mut buffer = Buffer::new();
        buffer.insert("loop", "loop", 1000 * MS, shifted(1.));
        assert!(matches!(
            buffer.lookup("loop", "elsewhere", 1000 * MS),
            Err(TfError::UnknownFrame { .. })
        ));
        // And the tree view still terminates.
        assert!(!buffer.tree().is_empty());
    }

    #[test]
    fn the_buffer_drops_samples_older_than_its_window() {
        let mut buffer = Buffer::with_window(1000 * MS);
        for step in 0..=20u64 {
            buffer.insert("map", "base", step * 100 * MS, shifted(step as f32));
        }
        // The newest is at 2000 ms, so anything before 1000 ms is gone.
        assert!(
            buffer.lookup("map", "base", 500 * MS).is_err(),
            "a sample outside the window should have been dropped"
        );
        assert!(buffer.lookup("map", "base", 1500 * MS).is_ok());

        let node = &buffer.tree()[1];
        assert_eq!(node.frame, "base");
        assert!(
            node.samples <= 12,
            "the window should bound the ring, got {} samples",
            node.samples
        );
    }

    #[test]
    fn shortening_the_window_trims_what_is_already_held() {
        let mut buffer = Buffer::with_window(10_000 * MS);
        for step in 0..=20u64 {
            buffer.insert("map", "base", step * 100 * MS, shifted(step as f32));
        }
        assert!(buffer.lookup("map", "base", 100 * MS).is_ok());

        buffer.set_window(500 * MS);
        assert_eq!(buffer.window_ns(), 500 * MS);
        assert!(
            buffer.lookup("map", "base", 100 * MS).is_err(),
            "shortening the window should have dropped the old samples"
        );
        assert!(
            buffer.lookup("map", "base", 1900 * MS).is_ok(),
            "the newest samples are still there"
        );
    }

    #[test]
    fn a_flood_is_capped_even_inside_the_window() {
        let mut buffer = Buffer::with_window(u64::MAX);
        for step in 0..(MAX_SAMPLES as u64 + 500) {
            buffer.insert("map", "base", step, shifted(step as f32));
        }
        let node = buffer
            .tree()
            .into_iter()
            .find(|node| node.frame == "base")
            .expect("base is in the tree");
        assert_eq!(node.samples, MAX_SAMPLES);
    }

    #[test]
    fn samples_arriving_out_of_order_are_still_interpolated_correctly() {
        let mut buffer = Buffer::new();
        buffer.insert("map", "base", 2000 * MS, shifted(10.));
        buffer.insert("map", "base", 1000 * MS, shifted(0.));
        let half = buffer.lookup("map", "base", 1500 * MS).expect("looks up");
        assert!((half.translation[0] - 5.).abs() < 1e-4);
    }

    #[test]
    fn re_parenting_a_frame_forgets_where_it_was_under_the_old_parent() {
        // A block on a table, picked up by a gripper: interpolating across the
        // moment it was grasped would draw it somewhere it has never been.
        let mut buffer = Buffer::new();
        buffer.insert("table", "block", 1000 * MS, shifted(1.));
        buffer.insert("gripper", "block", 2000 * MS, shifted(0.));

        assert!(
            buffer.lookup("gripper", "block", 1000 * MS).is_err(),
            "the old history must not answer for the new parent"
        );
        assert!(buffer.lookup("gripper", "block", 2000 * MS).is_ok());
    }

    #[test]
    fn the_tree_lists_roots_first_and_indents_their_children() {
        let mut buffer = Buffer::new();
        let at = 1000 * MS;
        buffer.insert("map", "odom", at, shifted(0.));
        buffer.insert("odom", "base", at, shifted(0.));
        buffer.insert_static("base", "laser", shifted(0.));
        buffer.insert("base", "arm", at, shifted(0.));

        let tree = buffer.tree();
        let shape: Vec<(&str, usize)> = tree
            .iter()
            .map(|node| (node.frame.as_str(), node.depth))
            .collect();
        assert_eq!(
            shape,
            vec![
                ("map", 0),
                ("odom", 1),
                ("base", 2),
                ("arm", 3),
                ("laser", 3),
            ]
        );
        assert_eq!(tree[0].parent, None, "a root has no parent");
        let laser = tree.iter().find(|node| node.frame == "laser").unwrap();
        assert!(laser.is_static);
        assert!(
            !tree
                .iter()
                .find(|node| node.frame == "arm")
                .unwrap()
                .is_static
        );
    }

    #[test]
    fn frames_caught_in_a_cycle_still_appear_in_the_tree() {
        let mut buffer = Buffer::new();
        let at = 1000 * MS;
        buffer.insert("map", "base", at, shifted(0.));
        buffer.insert("a", "b", at, shifted(0.));
        buffer.insert("b", "a", at, shifted(0.));

        let tree = buffer.tree();
        let listed: Vec<&str> = tree.iter().map(|n| n.frame.as_str()).collect();
        for frame in ["map", "base", "a", "b"] {
            assert!(listed.contains(&frame), "{frame} missing from {listed:?}");
        }
    }

    #[test]
    fn the_frame_list_includes_roots_that_are_nobody_s_child() {
        let mut buffer = Buffer::new();
        buffer.insert("map", "odom", 1000 * MS, shifted(0.));
        buffer.insert("odom", "base", 1000 * MS, shifted(0.));
        assert_eq!(buffer.frames(), vec!["base", "map", "odom"]);
    }

    #[test]
    fn age_counts_from_the_newest_sample_and_statics_never_age() {
        let mut buffer = Buffer::new();
        buffer.insert("map", "base", 1000 * MS, shifted(0.));
        buffer.insert_static("base", "laser", shifted(0.));

        assert_eq!(buffer.age_ns("base", 1400 * MS), Some(400 * MS));
        assert_eq!(buffer.age_ns("laser", 9999 * MS), Some(0));
        assert_eq!(buffer.age_ns("map", 1400 * MS), None, "a root has no edge");
        assert_eq!(buffer.age_ns("nowhere", 1400 * MS), None);
    }

    #[test]
    fn a_clock_that_went_backwards_reports_no_age_rather_than_underflowing() {
        let mut buffer = Buffer::new();
        buffer.insert("map", "base", 5000 * MS, shifted(0.));
        assert_eq!(buffer.age_ns("base", 1000 * MS), Some(0));
    }

    #[test]
    fn a_transform_and_its_inverse_cancel() {
        let transform = Transform::new([1., -2., 3.], Quat::from_axis_angle([0.3, 0.5, -0.8], 1.1));
        let point = [4., 5., -6.];
        assert!(close(
            transform.inverse().apply(transform.apply(point)),
            point
        ));
    }

    #[test]
    fn the_matrix_form_places_a_point_the_same_way_apply_does() {
        let transform =
            Transform::new([1., 2., 3.], Quat::from_axis_angle([0., 0., 1.], FRAC_PI_2));
        let matrix = transform.to_mat4();
        let point = [1., 0., 0.];
        let by_matrix = [
            matrix[0][0] * point[0]
                + matrix[1][0] * point[1]
                + matrix[2][0] * point[2]
                + matrix[3][0],
            matrix[0][1] * point[0]
                + matrix[1][1] * point[1]
                + matrix[2][1] * point[2]
                + matrix[3][1],
            matrix[0][2] * point[0]
                + matrix[1][2] * point[1]
                + matrix[2][2] * point[2]
                + matrix[3][2],
        ];
        assert!(close(by_matrix, transform.apply(point)), "{by_matrix:?}");
    }

    #[test]
    fn composition_matches_applying_the_two_transforms_in_turn() {
        let inner = Transform::new([1., 0., 0.], Quat::from_axis_angle([0., 0., 1.], FRAC_PI_2));
        let outer = Transform::new([0., 5., 0.], Quat::from_axis_angle([1., 0., 0.], 0.7));
        let point = [2., -1., 4.];
        assert!(close(
            inner.then(&outer).apply(point),
            outer.apply(inner.apply(point))
        ));
    }

    #[test]
    fn an_edge_announced_but_never_published_says_so() {
        let mut buffer = Buffer::new();
        buffer.insert("map", "base", 1000 * MS, shifted(0.));
        // Re-link `base` under a parent that has nothing, which is what an
        // announced-then-silent publisher leaves behind.
        buffer.relink("odom", "base");
        buffer.insert("map", "odom", 1000 * MS, shifted(0.));

        let error = buffer
            .lookup("map", "base", 1000 * MS)
            .expect_err("base has an edge but no samples");
        assert!(matches!(error, TfError::NoSamples { .. }), "got {error:?}");
        assert!(error.to_string().contains("never published"), "{error}");
    }

    #[test]
    fn a_deep_chain_stays_accurate() {
        // Twenty links of a metre each, so a sign error or a reversed
        // composition is twenty times as visible as it would be on one edge.
        let mut buffer = Buffer::new();
        let at = 1000 * MS;
        for step in 0..20 {
            buffer.insert(
                &format!("link{step}"),
                &format!("link{}", step + 1),
                at,
                shifted(1.),
            );
        }
        let placed = buffer.lookup("link0", "link20", at).expect("looks up");
        assert!(
            close(placed.apply([0., 0., 0.]), [20., 0., 0.]),
            "{placed:?}"
        );
    }
}
