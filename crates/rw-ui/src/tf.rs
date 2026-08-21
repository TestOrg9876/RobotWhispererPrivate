//! TF, off the wire and into a buffer per connection.
//!
//! `tf2_msgs/TFMessage` is an array of `geometry_msgs/TransformStamped`, and
//! every ROS system publishes it on two topics: `/tf` for things that move and
//! `/tf_static` for things bolted down. Decoding follows `cloud.rs` — read the
//! canonical value, take nothing on trust, and hand back plain data that the
//! renderer and the panes can use without knowing a socket exists.
//!
//! A connection subscribes to both topics by itself as soon as discovery
//! mentions them. That is a deliberate behaviour change: TF is not a topic
//! anyone wants to remember to turn on, it is the thing that makes every other
//! topic mean something, and RViz has done exactly this since 2010.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use gpui::{App, Context, Entity, Task};
use rw_canonical::CanonicalValue;
use rw_tf::{Buffer, Transform};

use crate::geometry;
use crate::session::{RobotWhisperer, Sessions};

/// The two topics every ROS graph puts its transform tree on.
pub const TOPIC: &str = "/tf";
pub const STATIC_TOPIC: &str = "/tf_static";

/// One transform, as it arrived.
#[derive(Debug, Clone, PartialEq)]
pub struct Stamped {
    /// `header.frame_id`: the frame this transform is expressed in.
    pub parent: String,
    /// `child_frame_id`: the frame it places.
    pub child: String,
    /// `header.stamp`, in nanoseconds.
    pub at_ns: u64,
    pub transform: Transform,
}

/// Reads a `tf2_msgs/TFMessage`, if this is one.
///
/// `None` rather than an empty list when the message is not a TF message at
/// all, so a caller can tell "not this" from "nothing this time" — an empty
/// `/tf` is legal and means the publisher had nothing to say.
pub fn decode(value: &CanonicalValue) -> Option<Vec<Stamped>> {
    let CanonicalValue::Struct(message) = value else {
        return None;
    };
    let CanonicalValue::Array(entries) = message.get("transforms")? else {
        return None;
    };

    let mut stamped = Vec::with_capacity(entries.len());
    for entry in entries {
        let CanonicalValue::Struct(fields) = entry else {
            continue;
        };
        // Both ends through the same reader, so ROS 1's `/base_link` and ROS
        // 2's `base_link` land on one frame rather than two halves of a tree.
        let (Some(parent), Some(child)) = (
            geometry::frame_id(entry),
            fields
                .get("child_frame_id")
                .and_then(geometry::text)
                .map(|name| name.trim_start_matches('/').to_string()),
        ) else {
            continue;
        };
        // A transform between two frames of the same name places a frame inside
        // itself. It is always a publisher bug, and taking it would put a cycle
        // in the tree for every lookup afterwards to trip over.
        if parent == child || parent.is_empty() || child.is_empty() {
            continue;
        }
        let Some(transform) = fields.get("transform").and_then(geometry::rigid) else {
            continue;
        };
        stamped.push(Stamped {
            parent,
            child,
            // An unstamped transform is taken as time zero rather than dropped:
            // `/tf_static` carries a stamp nobody reads, and some bridges send
            // none at all.
            at_ns: geometry::header_stamp_ns(entry).unwrap_or(0),
            transform,
        });
    }
    Some(stamped)
}

/// A connection's transform tree, held rather than copied.
///
/// Every method takes the lock for the length of one question and no longer.
/// That matters in both directions. Frames arrive off the UI thread and take
/// the same lock, so a render that held it across a decode would stall the
/// subscription — but *copying* the buffer to get out of the lock is worse. A
/// thirty-frame tree publishing at 100 Hz holds thirty thousand stamped
/// transforms inside a `BTreeMap` of `String`s, and a pane resolving one layer
/// per frame would deep-copy all of it sixty times a second. A lookup is
/// microseconds; the copy was a megabyte.
#[derive(Clone)]
pub struct Tree(Arc<Mutex<Buffer>>);

impl Tree {
    /// Wraps a buffer that is not in the store — the tests' way in.
    pub fn of(buffer: Buffer) -> Self {
        Self(Arc::new(Mutex::new(buffer)))
    }

    /// Where `source` sits inside `target`, at `at_ns`.
    pub fn lookup(
        &self,
        target: &str,
        source: &str,
        at_ns: u64,
    ) -> Result<Transform, rw_tf::TfError> {
        self.buffer().lookup(target, source, at_ns)
    }

    /// Every frame this tree knows, for the fixed-frame list.
    pub fn frames(&self) -> Vec<String> {
        self.buffer().frames()
    }

    /// The frame everything else hangs off, which is what a person means by
    /// "the world". `None` until something has been published.
    pub fn root(&self) -> Option<String> {
        self.buffer()
            .tree()
            .into_iter()
            .find(|node| node.parent.is_none())
            .map(|node| node.frame)
    }

    /// A poisoned lock is read through rather than treated as no tree at all.
    /// The panic that poisoned it happened elsewhere; the transforms already in
    /// the buffer are still the last true thing anyone said about the robot,
    /// and dropping them would blank every layer in every pane.
    fn buffer(&self) -> std::sync::MutexGuard<'_, Buffer> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The transform tree of a connection, for a view that needs to place
/// something. `None` for no connection, or for one nothing has arrived on.
pub fn tree(connection: Option<i64>, cx: &App) -> Option<Tree> {
    let held = RobotWhisperer::global(cx).tf.read(cx).peek(connection?)?;
    Some(Tree(held))
}

/// One transform tree per connection.
///
/// Held in the [`RobotWhisperer`] global beside `sessions` and `gpu`, because a
/// tree belongs to a robot rather than to whichever pane happened to ask for it
/// first: two panes looking at the same system must agree about where its
/// frames are, and a third opened later must not have to wait for its own copy
/// to fill up.
#[derive(Default)]
pub struct TfStore {
    /// The buffers, behind a lock because frames arrive off the UI thread.
    trees: HashMap<i64, Arc<Mutex<Buffer>>>,
    /// Which connections are already subscribed, so discovery updating twenty
    /// times a second does not open twenty subscriptions.
    subscribed: HashSet<(i64, &'static str)>,
    /// The tasks that opened them, keyed so reconnecting the same system
    /// replaces its old one rather than piling another on the heap.
    _work: HashMap<(i64, &'static str), Task<()>>,
}

impl TfStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// The tree for a connection, created empty on first ask.
    pub fn tree(&mut self, connection: i64) -> Arc<Mutex<Buffer>> {
        Arc::clone(self.trees.entry(connection).or_default())
    }

    /// The tree for a connection, if it has one yet.
    pub fn peek(&self, connection: i64) -> Option<Arc<Mutex<Buffer>>> {
        self.trees.get(&connection).map(Arc::clone)
    }

    /// Drops the trees of connections that have gone away.
    ///
    /// A disconnected robot's frames are not where they were, and keeping them
    /// would let a pane draw a scene out of a tree that stopped ten minutes
    /// ago. Reconnecting starts a fresh one — and re-subscribes, because the
    /// record of what was already subscribed goes with it.
    pub fn forget_closed(&mut self, sessions: &Entity<Sessions>, cx: &mut Context<Self>) {
        let open: HashSet<i64> = sessions
            .read(cx)
            .connections()
            .filter(|(_, live)| live.session.is_some())
            .map(|(id, _)| id)
            .collect();
        let before = self.trees.len();
        self.trees.retain(|id, _| open.contains(id));
        self.subscribed.retain(|(id, _)| open.contains(id));
        self._work.retain(|(id, _), _| open.contains(id));
        if self.trees.len() != before {
            cx.notify();
        }
    }

    /// Subscribes to `/tf` and `/tf_static` on any connection whose discovery
    /// advertises them, and keeps doing so as connections come and go.
    ///
    /// Idempotent: called on every discovery update, and opens a subscription
    /// at most once per connection and topic.
    pub fn follow(&mut self, sessions: &Entity<Sessions>, cx: &mut Context<Self>) {
        let live = sessions.read(cx);
        let pipeline = live.pipeline();
        let wanted: Vec<(i64, &'static str, rw_transport::ConnectionId)> = live
            .connections()
            .filter_map(|(id, live)| {
                let session = live.session?;
                let advertised: HashSet<&str> = live
                    .discovery
                    .topics
                    .iter()
                    .map(|topic| topic.name.as_str())
                    .collect();
                Some(
                    [TOPIC, STATIC_TOPIC]
                        .into_iter()
                        .filter(move |topic| advertised.contains(topic))
                        .map(move |topic| (id, topic, session)),
                )
            })
            .flatten()
            .filter(|(id, topic, _)| !self.subscribed.contains(&(*id, *topic)))
            .collect();

        for (connection, topic, session) in wanted {
            self.subscribed.insert((connection, topic));
            let tree = self.tree(connection);
            let pipeline = Arc::clone(&pipeline);
            let is_static = topic == STATIC_TOPIC;
            let task = cx.spawn(async move |store, cx| {
                let opened = pipeline
                    .subscribe_topic(session, topic, move |_handle, frame, _lossy| {
                        let Some(stamped) = decode(&frame.value) else {
                            return;
                        };
                        let Ok(mut tree) = tree.lock() else { return };
                        for entry in stamped {
                            if is_static {
                                tree.insert_static(&entry.parent, &entry.child, entry.transform);
                            } else {
                                tree.insert(
                                    &entry.parent,
                                    &entry.child,
                                    entry.at_ns,
                                    entry.transform,
                                );
                            }
                        }
                    })
                    .await;
                if let Err(error) = opened {
                    // Not fatal and not worth a dialog: a system without TF is
                    // a system whose layers will say why they cannot be placed.
                    tracing::warn!("could not subscribe to {topic}: {error}");
                    store
                        .update(cx, |store, _| {
                            store.subscribed.remove(&(connection, topic));
                        })
                        .ok();
                }
            });
            self._work.insert((connection, topic), task);
        }
    }
}

impl TfStore {
    /// The store from the global, for a view that only needs to read a tree.
    pub fn global(cx: &App) -> &Entity<Self> {
        &RobotWhisperer::global(cx).tf
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn map(pairs: Vec<(&str, CanonicalValue)>) -> CanonicalValue {
        CanonicalValue::Struct(
            pairs
                .into_iter()
                .map(|(name, value)| (name.to_string(), value))
                .collect::<BTreeMap<_, _>>(),
        )
    }

    fn vector(x: f64, y: f64, z: f64) -> CanonicalValue {
        map(vec![
            ("x", CanonicalValue::F64(x)),
            ("y", CanonicalValue::F64(y)),
            ("z", CanonicalValue::F64(z)),
        ])
    }

    fn quaternion(x: f64, y: f64, z: f64, w: f64) -> CanonicalValue {
        map(vec![
            ("x", CanonicalValue::F64(x)),
            ("y", CanonicalValue::F64(y)),
            ("z", CanonicalValue::F64(z)),
            ("w", CanonicalValue::F64(w)),
        ])
    }

    fn entry(parent: &str, child: &str, stamp: CanonicalValue) -> CanonicalValue {
        map(vec![
            (
                "header",
                map(vec![
                    ("frame_id", CanonicalValue::String(parent.into())),
                    ("stamp", stamp),
                ]),
            ),
            ("child_frame_id", CanonicalValue::String(child.into())),
            (
                "transform",
                map(vec![
                    ("translation", vector(1., 2., 3.)),
                    ("rotation", quaternion(0., 0., 0., 1.)),
                ]),
            ),
        ])
    }

    fn message(entries: Vec<CanonicalValue>) -> CanonicalValue {
        map(vec![("transforms", CanonicalValue::Array(entries))])
    }

    #[test]
    fn a_transform_message_decodes() {
        let stamp = CanonicalValue::Time {
            sec: 7,
            nanosec: 250_000_000,
        };
        let decoded = decode(&message(vec![entry("map", "base", stamp)])).expect("decodes");
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].parent, "map");
        assert_eq!(decoded[0].child, "base");
        assert_eq!(decoded[0].at_ns, 7_250_000_000);
        assert_eq!(decoded[0].transform.translation, [1., 2., 3.]);
        assert_eq!(decoded[0].transform.rotation, rw_tf::Quat::IDENTITY);
    }

    #[test]
    fn a_stamp_is_read_from_all_three_shapes_it_arrives_in() {
        // The canonical Time variant, a ROS 2 struct off a JSON bridge, and a
        // ROS 1 one — the same instant spelled three ways.
        let shapes = [
            CanonicalValue::Time {
                sec: 7,
                nanosec: 250_000_000,
            },
            map(vec![
                ("sec", CanonicalValue::Int(7)),
                ("nanosec", CanonicalValue::Uint(250_000_000)),
            ]),
            map(vec![
                ("secs", CanonicalValue::Int(7)),
                ("nsecs", CanonicalValue::Int(250_000_000)),
            ]),
        ];
        for shape in shapes {
            let decoded = decode(&message(vec![entry("map", "base", shape.clone())]))
                .unwrap_or_else(|| panic!("{shape:?} did not decode"));
            assert_eq!(decoded[0].at_ns, 7_250_000_000, "{shape:?}");
        }
    }

    #[test]
    fn a_message_that_is_not_tf_is_refused_rather_than_read_as_empty() {
        assert_eq!(decode(&CanonicalValue::Int(5)), None);
        assert_eq!(decode(&map(vec![("data", CanonicalValue::Int(5))])), None);
        assert_eq!(
            decode(&map(vec![("transforms", CanonicalValue::Int(5))])),
            None,
            "a `transforms` field that is not an array is not a TF message"
        );
    }

    #[test]
    fn an_empty_tf_message_is_a_tf_message_with_nothing_in_it() {
        assert_eq!(decode(&message(vec![])), Some(vec![]));
    }

    #[test]
    fn a_frame_parented_to_itself_is_dropped_rather_than_looping_the_tree() {
        let stamp = CanonicalValue::Time { sec: 1, nanosec: 0 };
        let decoded = decode(&message(vec![
            entry("base", "base", stamp.clone()),
            entry("map", "base", stamp),
        ]))
        .expect("decodes");
        assert_eq!(decoded.len(), 1, "only the real edge survived");
        assert_eq!(decoded[0].parent, "map");
    }

    #[test]
    fn an_entry_missing_its_frames_is_skipped_and_the_rest_still_arrive() {
        let stamp = CanonicalValue::Time { sec: 1, nanosec: 0 };
        let broken = map(vec![(
            "transform",
            map(vec![
                ("translation", vector(0., 0., 0.)),
                ("rotation", quaternion(0., 0., 0., 1.)),
            ]),
        )]);
        let decoded = decode(&message(vec![broken, entry("map", "base", stamp)])).expect("decodes");
        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded[0].child, "base");
    }

    #[test]
    fn an_unstamped_transform_is_taken_as_time_zero_rather_than_dropped() {
        // `/tf_static` carries a stamp nobody reads, and some bridges send none
        // at all. A static transform without a time is still a transform.
        let decoded =
            decode(&message(vec![entry("map", "base", CanonicalValue::Null)])).expect("decodes");
        assert_eq!(decoded[0].at_ns, 0);
    }

    #[test]
    fn a_negative_stamp_is_taken_as_unstamped_rather_than_wrapped() {
        let decoded = decode(&message(vec![entry(
            "map",
            "base",
            CanonicalValue::Time {
                sec: -5,
                nanosec: 0,
            },
        )]))
        .expect("decodes");
        assert_eq!(decoded[0].at_ns, 0, "not 18 billion seconds in the future");
    }

    #[test]
    fn a_rotation_off_the_wire_is_normalised() {
        let entry = map(vec![
            (
                "header",
                map(vec![("frame_id", CanonicalValue::String("map".into()))]),
            ),
            ("child_frame_id", CanonicalValue::String("base".into())),
            (
                "transform",
                map(vec![
                    ("translation", vector(0., 0., 0.)),
                    ("rotation", quaternion(0., 0., 3., 3.)),
                ]),
            ),
        ]);
        let decoded = decode(&message(vec![entry])).expect("decodes");
        let length = decoded[0].transform.rotation.length();
        assert!((length - 1.).abs() < 1e-6, "length {length}");
    }

    #[test]
    fn a_transform_with_a_nan_in_it_is_refused() {
        let entry = map(vec![
            (
                "header",
                map(vec![("frame_id", CanonicalValue::String("map".into()))]),
            ),
            ("child_frame_id", CanonicalValue::String("base".into())),
            (
                "transform",
                map(vec![
                    ("translation", vector(f64::NAN, 0., 0.)),
                    ("rotation", quaternion(0., 0., 0., 1.)),
                ]),
            ),
        ]);
        assert_eq!(decode(&message(vec![entry])), Some(vec![]));
    }

    #[test]
    fn integer_coordinates_are_read_rather_than_refused() {
        // A transform of exactly (1, 0, 0) can arrive as integers from a JSON
        // bridge, and refusing it would drop a perfectly good frame.
        let entry = map(vec![
            (
                "header",
                map(vec![("frame_id", CanonicalValue::String("map".into()))]),
            ),
            ("child_frame_id", CanonicalValue::String("base".into())),
            (
                "transform",
                map(vec![
                    (
                        "translation",
                        map(vec![
                            ("x", CanonicalValue::Int(1)),
                            ("y", CanonicalValue::Int(0)),
                            ("z", CanonicalValue::Int(0)),
                        ]),
                    ),
                    ("rotation", quaternion(0., 0., 0., 1.)),
                ]),
            ),
        ]);
        let decoded = decode(&message(vec![entry])).expect("decodes");
        assert_eq!(decoded[0].transform.translation, [1., 0., 0.]);
    }

    #[test]
    fn a_decoded_message_feeds_a_buffer_that_can_then_be_looked_up() {
        // The join between the two halves of this file's job.
        let mut tree = Buffer::new();
        for entry in decode(&message(vec![entry(
            "map",
            "base",
            CanonicalValue::Time { sec: 1, nanosec: 0 },
        )]))
        .expect("decodes")
        {
            tree.insert(&entry.parent, &entry.child, entry.at_ns, entry.transform);
        }
        let placed = tree
            .lookup("map", "base", 1_000_000_000)
            .expect("the tree answers");
        assert_eq!(placed.apply([0., 0., 0.]), [1., 2., 3.]);
    }

    /// A `Tree` is a handle onto the same buffer, so it must answer the same
    /// question the same way — the whole point of it is that no copy happens.
    #[test]
    fn a_tree_answers_what_the_buffer_it_wraps_would() {
        let mut buffer = Buffer::new();
        buffer.insert_static("map", "base", Transform::translation([1., 2., 3.]));
        buffer.insert_static("base", "laser", Transform::translation([0., 0., 1.]));

        let expected = buffer
            .lookup("map", "laser", rw_tf::LATEST)
            .expect("buffer");
        let tree = Tree::of(buffer);
        let placed = tree.lookup("map", "laser", rw_tf::LATEST).expect("tree");

        assert_eq!(placed.translation, expected.translation);
        assert_eq!(placed.apply([0., 0., 0.]), [1., 2., 4.]);
        assert_eq!(tree.frames(), vec!["base", "laser", "map"]);
        assert_eq!(tree.root().as_deref(), Some("map"));
    }

    /// A tree nothing has published has no root to offer, and saying so is how
    /// the world pane knows to fall back to whatever frame a layer arrived in.
    #[test]
    fn an_empty_tree_has_no_root() {
        let tree = Tree::of(Buffer::new());
        assert_eq!(tree.root(), None);
        assert!(tree.frames().is_empty());
    }

    /// A panic while some other thread held the lock poisons it. The transforms
    /// already in the buffer are still the last true thing anyone said about
    /// the robot, so they keep being answered rather than every layer in every
    /// pane going blank.
    #[test]
    fn a_poisoned_tree_still_answers() {
        let mut buffer = Buffer::new();
        buffer.insert_static("map", "base", Transform::translation([1., 0., 0.]));
        let tree = Tree::of(buffer);

        let poisoner = tree.clone();
        std::thread::spawn(move || {
            let _held = poisoner.0.lock().expect("first lock");
            panic!("poisons the lock");
        })
        .join()
        .expect_err("the thread panicked");

        let placed = tree
            .lookup("map", "base", rw_tf::LATEST)
            .expect("still answers");
        assert_eq!(placed.translation, [1., 0., 0.]);
    }
}
