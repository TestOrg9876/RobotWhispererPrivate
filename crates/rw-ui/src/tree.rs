//! The message, resolved into a tree you can fold.
//!
//! The raw view is the message as text and the plot is its numbers over time;
//! this is the shape in between — the same nesting the request form shows, with
//! every branch foldable, so a `sensor_msgs/PointCloud2` can be read down to
//! `fields[2].name` without scrolling past sixty kilobytes of `data`.
//!
//! Two rules keep it cheap on a message that arrives at 100 Hz:
//!
//! * The rows are built when the message changes, not when the pane paints.
//!   Folding rebuilds them too, and nothing else does.
//! * Building them walks only what is open. A folded branch costs one row
//!   whatever is under it, and an open array stops after [`MAX_CHILDREN`], so
//!   the walk is bounded by what a person could actually be looking at rather
//!   than by what the robot sent.
//!
//! Painting is bounded the same way: the rows go through a uniform list, which
//! builds elements for the visible window and nothing else.

use std::collections::HashMap;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, UniformListScrollHandle, Window, div, px, uniform_list,
};
use gpui_component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use rw_canonical::CanonicalValue;

use crate::tokens;
use crate::value;

/// How many children of one branch are ever built.
///
/// An open `float32[65536]` is not something anyone reads to the end, and a row
/// per element would cost more to build than the message did to arrive. The
/// rest is one row saying how many were left out; the raw view has the lot.
pub const MAX_CHILDREN: usize = 200;

/// How long an array has to be before it arrives folded.
///
/// Short arrays are fields — `position[1]` is something people look for. Long
/// ones are data, and opening one by accident on every message would be a
/// nuisance rather than a feature.
const FOLD_OVER: usize = 16;

/// How far down the tree goes. Canonical values cannot be cyclic, so this is a
/// guard against a decoder that has gone wrong rather than a real limit.
const MAX_DEPTH: usize = 24;

/// One line of the tree.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// How far in it sits.
    pub depth: usize,
    /// The path from the message root, which is what a fold is remembered by.
    /// Stable across messages, so a branch stays open as new ones arrive.
    pub path: String,
    /// The field's own name, or `[3]` for an array element.
    pub name: String,
    /// A leaf's value, rendered.
    pub value: Option<String>,
    /// What a branch holds, as a count.
    pub summary: Option<String>,
    /// Whether this row has children to show.
    pub branch: bool,
    /// Whether those children are hidden.
    pub folded: bool,
}

impl Row {
    fn leaf(depth: usize, path: String, name: String, value: String) -> Self {
        Self {
            depth,
            path,
            name,
            value: Some(value),
            summary: None,
            branch: false,
            folded: false,
        }
    }
}

/// Which branches the reader has opened or closed by hand.
///
/// Only the choices made are kept; everything else follows the default, so a
/// message whose shape changes does not drag stale state along with it.
pub type Folds = HashMap<String, bool>;

/// Whether a branch at `path` holding `children` is folded.
fn folded(folds: &Folds, path: &str, children: usize) -> bool {
    match folds.get(path) {
        Some(chosen) => *chosen,
        None => children > FOLD_OVER,
    }
}

/// The rows to draw for `value`, skipping whatever is folded.
pub fn rows(value: &CanonicalValue, folds: &Folds) -> Vec<Row> {
    let mut rows = Vec::new();
    walk(value, "", "", 0, folds, &mut rows);
    rows
}

fn walk(
    value: &CanonicalValue,
    path: &str,
    name: &str,
    depth: usize,
    folds: &Folds,
    out: &mut Vec<Row>,
) {
    if depth > MAX_DEPTH {
        return;
    }

    // The root has no row of its own: a line saying "9 fields" above the nine
    // fields is a line that spends height on what the rows below already say.
    let root = path.is_empty();

    match value {
        CanonicalValue::Struct(fields) if !fields.is_empty() => {
            let folded = !root && folded(folds, path, fields.len());
            if !root {
                out.push(Row {
                    depth,
                    path: path.to_string(),
                    name: name.to_string(),
                    value: None,
                    summary: Some(count(fields.len(), "field")),
                    branch: true,
                    folded,
                });
            }
            if folded {
                return;
            }
            let depth = if root { depth } else { depth + 1 };
            for (child, field) in fields {
                walk(field, &join(path, child), child, depth, folds, out);
            }
        }
        CanonicalValue::Array(items) if !items.is_empty() => {
            let folded = !root && folded(folds, path, items.len());
            if !root {
                out.push(Row {
                    depth,
                    path: path.to_string(),
                    name: name.to_string(),
                    value: None,
                    summary: Some(count(items.len(), "item")),
                    branch: true,
                    folded,
                });
            }
            if folded {
                return;
            }
            let depth = if root { depth } else { depth + 1 };
            for (index, item) in items.iter().take(MAX_CHILDREN).enumerate() {
                let child = format!("[{index}]");
                walk(item, &join(path, &child), &child, depth, folds, out);
            }
            if let Some(left) = items.len().checked_sub(MAX_CHILDREN).filter(|n| *n > 0) {
                out.push(Row::leaf(
                    depth,
                    join(path, "…"),
                    "…".into(),
                    format!("{left} more, in the raw view"),
                ));
            }
        }
        // An empty struct or array is a leaf: there is nothing to open, and a
        // fold arrow that opens onto nothing is a lie.
        other => {
            if root {
                return;
            }
            out.push(Row::leaf(
                depth,
                path.to_string(),
                name.to_string(),
                value::scalar(other),
            ));
        }
    }
}

fn join(path: &str, child: &str) -> String {
    if path.is_empty() {
        child.to_string()
    } else if child.starts_with('[') {
        format!("{path}{child}")
    } else {
        format!("{path}.{child}")
    }
}

fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// The pane that draws it, and remembers what has been folded.
pub struct TreeView {
    folds: Folds,
    rows: Vec<Row>,
    /// The message the rows came from, kept so folding can rebuild them
    /// without the pane handing it back — one copy per message, rather than
    /// one per frame or one per click.
    value: Option<CanonicalValue>,
    /// Which message that was, so the rows are rebuilt when it changes and not
    /// on every paint.
    at: u64,
    scroll: UniformListScrollHandle,
}

impl TreeView {
    pub fn view(cx: &mut App) -> Entity<Self> {
        cx.new(|_| Self {
            folds: Folds::new(),
            rows: Vec::new(),
            value: None,
            at: 0,
            scroll: UniformListScrollHandle::new(),
        })
    }

    /// Points the tree at a message. `at` counts messages, so the rows are
    /// rebuilt once per message rather than once per frame.
    pub fn show(&mut self, value: &CanonicalValue, at: u64, cx: &mut Context<Self>) {
        if self.at == at && self.value.is_some() {
            return;
        }
        self.at = at;
        self.rows = rows(value, &self.folds);
        self.value = Some(value.clone());
        cx.notify();
    }

    /// Folds or unfolds a branch.
    pub fn toggle(&mut self, path: &str, cx: &mut Context<Self>) {
        let folded = self
            .rows
            .iter()
            .find(|row| row.path == path)
            .is_some_and(|row| row.folded);
        self.folds.insert(path.to_string(), !folded);
        if let Some(value) = &self.value {
            self.rows = rows(value, &self.folds);
        }
        cx.notify();
    }
}

/// How tall one row is. Fixed, because the list only virtualises when every row
/// is the same height — and a tree of one-line rows is exactly that.
const ROW_HEIGHT: f32 = 22.;
/// How far one level indents.
const INDENT: f32 = 14.;

/// Draws the rows, with `on_toggle` called with the path of a branch clicked.
///
/// A free function rather than `Render`, because the pane holding the tree owns
/// the message and has to hand it back to rebuild — and a `Render` impl has no
/// way to ask for it.
pub fn render(
    tree: &Entity<TreeView>,
    on_toggle: impl Fn(&str, &mut Window, &mut App) + 'static,
    cx: &mut App,
) -> AnyElement {
    let rows = tree.read(cx).rows.clone();
    if rows.is_empty() {
        return tokens::empty_state(
            IconName::Inbox,
            "Nothing to show",
            "This message has no fields.",
            cx,
        )
        .into_any_element();
    }

    let scroll = tree.read(cx).scroll.clone();
    let on_toggle = std::rc::Rc::new(on_toggle);

    uniform_list("tree", rows.len(), move |range, _window, cx| {
        let on_toggle = on_toggle.clone();
        rows[range]
            .iter()
            .map(|row| line(row, on_toggle.clone(), cx))
            .collect::<Vec<_>>()
    })
    .track_scroll(&scroll)
    .size_full()
    .into_any_element()
}

fn line(
    row: &Row,
    on_toggle: std::rc::Rc<impl Fn(&str, &mut Window, &mut App) + 'static>,
    cx: &App,
) -> AnyElement {
    let path = row.path.clone();
    let arrow = if row.folded {
        IconName::ChevronRight
    } else {
        IconName::ChevronDown
    };

    h_flex()
        .id(SharedString::from(row.path.clone()))
        .h(px(ROW_HEIGHT))
        .w_full()
        .gap_2()
        .items_center()
        .pl(px(row.depth as f32 * INDENT))
        .when(row.branch, |line| {
            line.cursor_pointer()
                .hover(|line| line.bg(cx.theme().muted))
                .on_click(move |_: &ClickEvent, window, cx| on_toggle(&path, window, cx))
        })
        .child(div().w(px(14.)).flex_shrink_0().when(row.branch, |slot| {
            slot.child(Icon::new(arrow).xsmall().text_color(
                // Quiet: the arrow is an affordance, not a value.
                cx.theme().muted_foreground,
            ))
        }))
        .child(
            tokens::mono(cx)
                .flex_shrink_0()
                .text_xs()
                .text_color(cx.theme().foreground)
                .child(row.name.clone()),
        )
        .when_some(row.summary.clone(), |line, summary| {
            line.child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(summary),
            )
        })
        .when_some(row.value.clone(), |line, value| {
            line.child(
                tokens::mono(cx)
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .truncate()
                    .child(value),
            )
        })
        .into_any_element()
}

impl Render for TreeView {
    /// Never used: the tree is drawn by [`render`], which the pane calls with
    /// the message in hand. The entity exists to hold the folds.
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn structure<const N: usize>(fields: [(&str, CanonicalValue); N]) -> CanonicalValue {
        CanonicalValue::Struct(
            fields
                .into_iter()
                .map(|(name, value)| (name.to_string(), value))
                .collect::<BTreeMap<_, _>>(),
        )
    }

    fn numbers(count: usize) -> CanonicalValue {
        CanonicalValue::Array((0..count).map(|n| CanonicalValue::Int(n as i64)).collect())
    }

    fn paths(rows: &[Row]) -> Vec<&str> {
        rows.iter().map(|row| row.path.as_str()).collect()
    }

    #[test]
    fn the_root_has_no_row_of_its_own() {
        let rows = rows(&structure([("a", CanonicalValue::Int(1))]), &Folds::new());
        assert_eq!(paths(&rows), ["a"]);
        assert_eq!(rows[0].depth, 0);
    }

    #[test]
    fn a_nested_message_is_a_branch_with_its_fields_under_it() {
        let value = structure([(
            "pose",
            structure([
                ("x", CanonicalValue::F64(1.)),
                ("y", CanonicalValue::F64(2.)),
            ]),
        )]);
        let rows = rows(&value, &Folds::new());

        assert_eq!(paths(&rows), ["pose", "pose.x", "pose.y"]);
        assert!(rows[0].branch);
        assert_eq!(rows[0].summary.as_deref(), Some("2 fields"));
        assert_eq!(rows[1].depth, 1);
    }

    #[test]
    fn a_folded_branch_costs_one_row_whatever_is_under_it() {
        let value = structure([("pose", structure([("x", CanonicalValue::F64(1.))]))]);
        let folds = Folds::from([("pose".to_string(), true)]);

        let rows = rows(&value, &folds);
        assert_eq!(paths(&rows), ["pose"]);
        assert!(rows[0].folded);
    }

    #[test]
    fn a_short_array_arrives_open_and_a_long_one_arrives_folded() {
        let short = structure([("footprint", numbers(4))]);
        assert_eq!(rows(&short, &Folds::new()).len(), 5);

        let long = structure([("ranges", numbers(FOLD_OVER + 1))]);
        let rows = rows(&long, &Folds::new());
        assert_eq!(rows.len(), 1);
        assert!(rows[0].folded);
    }

    /// The default is a default, not a rule: a long array opens when asked.
    #[test]
    fn a_long_array_opens_when_it_is_asked_to() {
        let value = structure([("ranges", numbers(20))]);
        let folds = Folds::from([("ranges".to_string(), false)]);

        assert_eq!(rows(&value, &folds).len(), 21);
    }

    #[test]
    fn an_array_element_is_named_by_its_index() {
        let value = structure([("footprint", numbers(2))]);
        let rows = rows(&value, &Folds::new());

        assert_eq!(paths(&rows), ["footprint", "footprint[0]", "footprint[1]"]);
        assert_eq!(rows[1].name, "[0]");
        assert_eq!(rows[1].value.as_deref(), Some("0"));
    }

    /// The cost of an open array is capped: nothing builds a row per element of
    /// a point cloud.
    #[test]
    fn an_open_array_stops_after_the_cap_and_says_how_many_were_left() {
        let value = structure([("ranges", numbers(MAX_CHILDREN + 40))]);
        let folds = Folds::from([("ranges".to_string(), false)]);
        let rows = rows(&value, &folds);

        assert_eq!(rows.len(), 1 + MAX_CHILDREN + 1);
        let last = rows.last().unwrap();
        assert!(!last.branch);
        assert_eq!(last.value.as_deref(), Some("40 more, in the raw view"));
    }

    #[test]
    fn an_empty_branch_is_a_leaf_rather_than_an_arrow_onto_nothing() {
        let value = structure([
            ("empty", CanonicalValue::Array(Vec::new())),
            ("blank", CanonicalValue::Struct(BTreeMap::new())),
        ]);
        let rows = rows(&value, &Folds::new());

        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| !row.branch));
    }

    #[test]
    fn a_message_with_no_fields_has_no_rows() {
        assert!(rows(&CanonicalValue::Struct(BTreeMap::new()), &Folds::new()).is_empty());
        assert!(rows(&CanonicalValue::Null, &Folds::new()).is_empty());
    }

    /// Fold state is keyed on the path, so a branch left open stays open as new
    /// messages arrive — which is the whole point of watching one field.
    #[test]
    fn a_fold_survives_the_next_message() {
        let folds = Folds::from([("pose".to_string(), true)]);
        let first = structure([("pose", structure([("x", CanonicalValue::F64(1.))]))]);
        let second = structure([("pose", structure([("x", CanonicalValue::F64(2.))]))]);

        assert_eq!(rows(&first, &folds), rows(&second, &folds));
    }
}
