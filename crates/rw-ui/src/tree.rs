//! Arranging requests into the collection tree the sidebar draws.
//!
//! Collections nest — up to [`MAX_DEPTH`] levels — requests hang off them, and a
//! search has to be able to hide a collection whose contents all matched nothing
//! while keeping the ones that did. That is enough branching to be worth testing
//! on its own, so none of it needs a window.

use rw_core::domain::{Collection, Request};

/// How many levels of collection there may be.
///
/// A top-level collection is depth 1, so with a cap of 3 the deepest one cannot
/// contain another. Deeper nesting than this is where a sidebar stops being
/// navigable and starts being a puzzle; requests can still live at any level.
pub const MAX_DEPTH: usize = 3;

/// One line of the sidebar, already flattened for rendering.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    Collection {
        id: i64,
        name: String,
        /// How far in to indent it. Zero-based, so a top-level collection is 0
        /// and the deepest permitted one is `MAX_DEPTH - 1`.
        depth: usize,
        /// Requests inside it and inside its descendants.
        total: usize,
        expanded: bool,
        /// Whether another collection may be created inside it, which is false
        /// once it sits at the deepest level.
        can_nest: bool,
    },
    Request {
        id: i64,
        depth: usize,
    },
}

impl Row {
    pub fn depth(&self) -> usize {
        match self {
            Row::Collection { depth, .. } | Row::Request { depth, .. } => *depth,
        }
    }
}

/// Flattens the tree into the rows to draw.
///
/// `matched` decides which requests count — a search passes the ones it found,
/// and everything else passes them all.
///
/// `expanded` is asked about each collection. A collapsed one still reports its
/// total, so the count beside it does not change as it is opened and closed.
///
/// `hide_empty` is for searching. While searching, a collection whose contents
/// all matched nothing is a line the reader has to check and discard. The rest of
/// the time an empty collection must still be drawn — the first thing that
/// happens to a new one is that it is empty, and one you cannot see is one you
/// cannot put anything in.
pub fn rows(
    collections: &[Collection],
    requests: &[Request],
    matched: impl Fn(&Request) -> bool,
    expanded: impl Fn(i64) -> bool,
    hide_empty: bool,
) -> Vec<Row> {
    let kept: Vec<&Request> = requests.iter().filter(|request| matched(request)).collect();

    let mut rows = Vec::new();
    walk(
        collections,
        &kept,
        None,
        0,
        &expanded,
        hide_empty,
        &mut rows,
    );

    // Whatever the walk did not reach goes at the root. Asking what was *drawn*
    // rather than whether the collection exists covers every way a request can be
    // stranded — no collection, a deleted one, or one that is real but
    // unreachable because the stored parents form a cycle. A request must not
    // disappear because of a mistake in the collection above it.
    //
    // A collapsed collection's contents are deliberately not drawn, so those do
    // not count as stranded: they are hidden, and hidden is not lost.
    let hidden_by_collapse = |request: &Request| {
        request.collection_id.is_some_and(|id| {
            rows.iter().any(
                |row| matches!(row, Row::Collection { id: collection, .. } if *collection == id),
            )
        })
    };
    let drawn = |request: &Request| {
        rows.iter()
            .any(|row| matches!(row, Row::Request { id, .. } if *id == request.id))
    };

    let stranded: Vec<_> = kept
        .iter()
        .filter(|request| !drawn(request) && !hidden_by_collapse(request))
        .map(|request| Row::Request {
            id: request.id,
            depth: 0,
        })
        .collect();
    rows.extend(stranded);

    rows
}

#[allow(clippy::too_many_arguments)]
fn walk(
    collections: &[Collection],
    kept: &[&Request],
    parent: Option<i64>,
    depth: usize,
    expanded: &impl Fn(i64) -> bool,
    hide_empty: bool,
    rows: &mut Vec<Row>,
) {
    // Nothing below the cap is drawn, which is also the guard against a cycle in
    // stored parents becoming an endless sidebar.
    if depth >= MAX_DEPTH {
        return;
    }

    for collection in collections
        .iter()
        .filter(|collection| collection.parent_id == parent)
    {
        let total = count(collections, kept, collection.id, depth);
        if total == 0 && hide_empty {
            continue;
        }

        let open = expanded(collection.id);
        rows.push(Row::Collection {
            id: collection.id,
            name: collection.name.clone(),
            depth,
            total,
            expanded: open,
            can_nest: depth + 1 < MAX_DEPTH,
        });

        if !open {
            continue;
        }

        walk(
            collections,
            kept,
            Some(collection.id),
            depth + 1,
            expanded,
            hide_empty,
            rows,
        );
        for request in kept
            .iter()
            .filter(|request| request.collection_id == Some(collection.id))
        {
            rows.push(Row::Request {
                id: request.id,
                depth: depth + 1,
            });
        }
    }
}

/// Requests in `collection` and everything below it.
fn count(collections: &[Collection], kept: &[&Request], collection: i64, depth: usize) -> usize {
    if depth >= MAX_DEPTH {
        return 0;
    }

    let own = kept
        .iter()
        .filter(|request| request.collection_id == Some(collection))
        .count();

    let below: usize = collections
        .iter()
        .filter(|entry| entry.parent_id == Some(collection))
        .map(|entry| count(collections, kept, entry.id, depth + 1))
        .sum();

    own + below
}

/// How deep `collection` sits, counting the root as 0.
///
/// `None` if it cannot be reached from the root at all — a cycle in stored
/// parents, or a missing one.
pub fn depth_of(collections: &[Collection], collection: i64) -> Option<usize> {
    let mut current = Some(collection);
    for depth in 0..=MAX_DEPTH {
        let Some(at) = current else {
            return Some(depth.saturating_sub(1));
        };
        current = collections.iter().find(|entry| entry.id == at)?.parent_id;
    }
    None
}

/// How many levels of collection sit at or below `collection`.
///
/// A collection with no children is 1. Needed because moving a branch has to
/// account for the whole branch, not just the one being dragged.
fn height_of(collections: &[Collection], collection: i64, guard: usize) -> usize {
    if guard >= MAX_DEPTH {
        return 1;
    }
    let tallest_child = collections
        .iter()
        .filter(|entry| entry.parent_id == Some(collection))
        .map(|entry| height_of(collections, entry.id, guard + 1))
        .max()
        .unwrap_or(0);
    1 + tallest_child
}

/// Whether a new collection may be created inside `parent`.
///
/// `None` is the root, which always has room.
pub fn can_nest_inside(collections: &[Collection], parent: Option<i64>) -> bool {
    match parent {
        None => true,
        Some(parent) => depth_of(collections, parent).is_some_and(|depth| depth + 1 < MAX_DEPTH),
    }
}

/// Why a collection cannot be moved somewhere, when it cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Into itself, or into something already beneath it.
    WouldDetachItself,
    /// The branch is too tall to fit under that parent.
    TooDeep,
}

/// Whether `collection` may be moved into `parent`, and why not if not.
///
/// Two things can go wrong. A collection moved inside its own subtree detaches
/// that whole branch from the root, and since the sidebar only draws what it can
/// reach, everything in it would vanish. And a branch three levels tall does not
/// fit under a parent that is already one level down.
pub fn check_move(
    collections: &[Collection],
    collection: i64,
    parent: Option<i64>,
) -> Result<(), Refusal> {
    let Some(parent) = parent else {
        // The root accepts anything that fits, and everything fits at the root.
        return Ok(());
    };
    if parent == collection {
        return Err(Refusal::WouldDetachItself);
    }

    // Walk up from the proposed parent: if `collection` is on that path, the move
    // would put it inside its own subtree. Bounded, because the stored parents
    // may already contain a cycle and this must not be what hangs on it.
    let mut current = Some(parent);
    for _ in 0..=MAX_DEPTH {
        let Some(at) = current else { break };
        if at == collection {
            return Err(Refusal::WouldDetachItself);
        }
        current = collections
            .iter()
            .find(|entry| entry.id == at)
            .and_then(|entry| entry.parent_id);
    }

    let Some(parent_depth) = depth_of(collections, parent) else {
        return Err(Refusal::WouldDetachItself);
    };
    let height = height_of(collections, collection, 0);
    if parent_depth + 1 + height > MAX_DEPTH {
        return Err(Refusal::TooDeep);
    }
    Ok(())
}

/// Whether a move is allowed, for the places that only need a yes or no.
pub fn can_move(collections: &[Collection], collection: i64, parent: Option<i64>) -> bool {
    check_move(collections, collection, parent).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone as _, Utc};
    use rw_core::domain::{RequestKind, Value};

    fn collection(id: i64, name: &str, parent: Option<i64>) -> Collection {
        Collection {
            id,
            parent_id: parent,
            name: name.into(),
            created_at: Utc.timestamp_opt(0, 0).unwrap(),
        }
    }

    fn request(id: i64, name: &str, collection: Option<i64>) -> Request {
        Request {
            id,
            collection_id: collection,
            connection_id: None,
            name: name.into(),
            kind: RequestKind::Topic,
            target: format!("/topic/{id}"),
            schema: None,
            input: Value::Null,
            visualization: None,
            created_at: Utc.timestamp_opt(0, 0).unwrap(),
            updated_at: Utc.timestamp_opt(0, 0).unwrap(),
        }
    }

    /// Three levels: the deepest the tree allows.
    fn three_levels() -> Vec<Collection> {
        vec![
            collection(10, "Robot", None),
            collection(11, "Arm", Some(10)),
            collection(12, "Wrist", Some(11)),
        ]
    }

    fn all(_: &Request) -> bool {
        true
    }

    fn open(_: i64) -> bool {
        true
    }

    #[test]
    fn a_flat_list_stays_flat() {
        let rows = rows(
            &[],
            &[request(1, "A", None), request(2, "B", None)],
            all,
            open,
            false,
        );
        assert_eq!(
            rows,
            [
                Row::Request { id: 1, depth: 0 },
                Row::Request { id: 2, depth: 0 }
            ]
        );
    }

    #[test]
    fn requests_sit_under_their_collection_indented() {
        let rows = rows(
            &[collection(10, "Arm", None)],
            &[request(1, "A", Some(10))],
            all,
            open,
            false,
        );
        assert_eq!(
            rows,
            [
                Row::Collection {
                    id: 10,
                    name: "Arm".into(),
                    depth: 0,
                    total: 1,
                    expanded: true,
                    can_nest: true
                },
                Row::Request { id: 1, depth: 1 },
            ]
        );
    }

    #[test]
    fn nesting_indents_further_and_counts_upwards() {
        let rows = rows(
            &[
                collection(10, "Robot", None),
                collection(11, "Arm", Some(10)),
            ],
            &[request(1, "A", Some(11)), request(2, "B", Some(10))],
            all,
            open,
            false,
        );
        let depths: Vec<_> = rows.iter().map(Row::depth).collect();
        assert_eq!(depths, [0, 1, 2, 1]);
        // The outer collection counts what is below it as well as its own.
        assert!(matches!(rows[0], Row::Collection { total: 2, .. }));
        assert!(matches!(rows[1], Row::Collection { total: 1, .. }));
    }

    #[test]
    fn three_levels_are_drawn_and_a_fourth_is_not() {
        let mut collections = three_levels();
        collections.push(collection(13, "Too deep", Some(12)));
        let rows = rows(&collections, &[request(1, "A", Some(13))], all, open, false);

        let names: Vec<_> = rows
            .iter()
            .filter_map(|row| match row {
                Row::Collection { name, .. } => Some(name.as_str()),
                Row::Request { .. } => None,
            })
            .collect();
        assert_eq!(names, ["Robot", "Arm", "Wrist"]);
        // Its request is not lost with it: unreachable, so it lands at the root.
        assert!(rows.contains(&Row::Request { id: 1, depth: 0 }));
    }

    #[test]
    fn the_deepest_collection_reports_that_it_cannot_nest() {
        let rows = rows(&three_levels(), &[], all, open, false);
        let nesting: Vec<_> = rows
            .iter()
            .filter_map(|row| match row {
                Row::Collection { can_nest, .. } => Some(*can_nest),
                Row::Request { .. } => None,
            })
            .collect();
        assert_eq!(nesting, [true, true, false]);
    }

    #[test]
    fn a_collapsed_collection_hides_its_contents_but_keeps_its_count() {
        // Otherwise the number beside it changes as it is opened and closed,
        // which makes it useless for deciding whether to open it.
        let rows = rows(
            &[collection(10, "Arm", None)],
            &[request(1, "A", Some(10))],
            all,
            |_| false,
            false,
        );
        assert_eq!(
            rows,
            [Row::Collection {
                id: 10,
                name: "Arm".into(),
                depth: 0,
                total: 1,
                expanded: false,
                can_nest: true
            }]
        );
    }

    #[test]
    fn an_empty_collection_is_drawn_so_things_can_be_put_in_it() {
        // The first thing that happens to a new collection is that it is empty,
        // and one you cannot see is one you cannot put anything in.
        let rows = rows(&[collection(10, "Arm", None)], &[], all, open, false);
        assert_eq!(
            rows,
            [Row::Collection {
                id: 10,
                name: "Arm".into(),
                depth: 0,
                total: 0,
                expanded: true,
                can_nest: true
            }]
        );
    }

    #[test]
    fn an_empty_collection_is_hidden_while_searching() {
        // Then it is a line the reader has to check and discard.
        assert!(rows(&[collection(10, "Arm", None)], &[], all, open, true).is_empty());
    }

    #[test]
    fn a_search_hides_collections_that_matched_nothing() {
        let collections = [collection(10, "Arm", None), collection(11, "Base", None)];
        let requests = [request(1, "lift", Some(10)), request(2, "drive", Some(11))];
        let rows = rows(&collections, &requests, |r| r.name == "lift", open, true);

        assert_eq!(
            rows,
            [
                Row::Collection {
                    id: 10,
                    name: "Arm".into(),
                    depth: 0,
                    total: 1,
                    expanded: true,
                    can_nest: true
                },
                Row::Request { id: 1, depth: 1 },
            ]
        );
    }

    #[test]
    fn a_search_keeps_a_collection_whose_child_matched() {
        let collections = [
            collection(10, "Robot", None),
            collection(11, "Arm", Some(10)),
        ];
        let requests = [request(1, "lift", Some(11))];
        let rows = rows(&collections, &requests, |r| r.name == "lift", open, true);
        assert_eq!(rows.len(), 3);
        assert!(matches!(rows[0], Row::Collection { id: 10, .. }));
    }

    #[test]
    fn a_request_whose_collection_is_gone_still_appears() {
        // Losing a collection must not silently lose the work inside it.
        let rows = rows(&[], &[request(1, "orphan", Some(99))], all, open, false);
        assert_eq!(rows, [Row::Request { id: 1, depth: 0 }]);
    }

    #[test]
    fn a_request_in_a_real_but_unreachable_collection_still_appears() {
        // The collection exists, so "does it exist" is not enough of a test; it
        // is simply not reachable from the root.
        let collections = [collection(10, "A", Some(11)), collection(11, "B", Some(10))];
        let rows = rows(&collections, &[request(1, "x", Some(10))], all, open, false);
        assert_eq!(rows, [Row::Request { id: 1, depth: 0 }]);
    }

    #[test]
    fn a_collapsed_collections_contents_are_hidden_rather_than_stranded() {
        // Hidden is not lost: a collapsed collection's requests must not reappear
        // at the root, which is what a naive "was it drawn" check would do.
        let rows = rows(
            &[collection(10, "Arm", None)],
            &[request(1, "A", Some(10))],
            all,
            |_| false,
            false,
        );
        assert_eq!(rows.len(), 1);
        assert!(matches!(
            rows[0],
            Row::Collection {
                expanded: false,
                ..
            }
        ));
    }

    #[test]
    fn root_requests_come_after_the_collections() {
        // Collections first is the convention every file tree uses.
        let rows = rows(
            &[collection(10, "Arm", None)],
            &[request(1, "loose", None), request(2, "inside", Some(10))],
            all,
            open,
            false,
        );
        assert!(matches!(rows[0], Row::Collection { .. }));
        assert_eq!(rows[2], Row::Request { id: 1, depth: 0 });
    }

    // ── moving ─────────────────────────────────────────────────────────────────

    #[test]
    fn depth_is_counted_from_the_root() {
        let collections = three_levels();
        assert_eq!(depth_of(&collections, 10), Some(0));
        assert_eq!(depth_of(&collections, 11), Some(1));
        assert_eq!(depth_of(&collections, 12), Some(2));
        assert_eq!(depth_of(&collections, 99), None);
    }

    #[test]
    fn a_collection_can_move_into_an_unrelated_one() {
        let collections = [collection(10, "A", None), collection(11, "B", None)];
        assert!(can_move(&collections, 10, Some(11)));
    }

    #[test]
    fn a_collection_cannot_move_into_itself() {
        assert_eq!(
            check_move(&[collection(10, "A", None)], 10, Some(10)),
            Err(Refusal::WouldDetachItself)
        );
    }

    #[test]
    fn a_collection_cannot_move_into_its_own_descendant() {
        // That detaches the branch from the root, and the sidebar only draws what
        // it can reach — everything inside would vanish.
        let collections = three_levels();
        assert_eq!(
            check_move(&collections, 10, Some(11)),
            Err(Refusal::WouldDetachItself)
        );
        assert_eq!(
            check_move(&collections, 10, Some(12)),
            Err(Refusal::WouldDetachItself)
        );
        // Outwards is fine.
        assert!(can_move(&collections, 12, None));
    }

    #[test]
    fn a_branch_that_would_not_fit_is_refused_for_being_too_deep() {
        // `Arm` is two levels tall (Arm, Wrist). Dropping it into another
        // top-level collection is fine — that makes three. Dropping it one level
        // further down would make four.
        let mut collections = three_levels();
        collections.push(collection(20, "Base", None));
        collections.push(collection(21, "Wheels", Some(20)));

        assert!(can_move(&collections, 11, Some(20)));
        assert_eq!(
            check_move(&collections, 11, Some(21)),
            Err(Refusal::TooDeep)
        );
    }

    #[test]
    fn a_single_collection_fits_at_the_deepest_level() {
        let collections = three_levels();
        // `Wrist` alone is one level tall, so it fits under `Arm` — which is
        // where it already is.
        assert!(can_move(&collections, 12, Some(11)));
    }

    #[test]
    fn the_root_always_has_room() {
        assert!(can_move(&three_levels(), 12, None));
        assert!(can_move(&[], 10, None));
    }

    #[test]
    fn a_new_collection_fits_until_the_cap() {
        let collections = three_levels();
        assert!(can_nest_inside(&collections, None));
        assert!(can_nest_inside(&collections, Some(10)));
        assert!(can_nest_inside(&collections, Some(11)));
        // `Wrist` is already at the deepest level.
        assert!(!can_nest_inside(&collections, Some(12)));
    }

    #[test]
    fn an_existing_cycle_does_not_hang_the_checks() {
        let collections = [collection(10, "A", Some(11)), collection(11, "B", Some(10))];
        // Whatever the answers are, they have to be answers.
        let _ = check_move(&collections, 12, Some(10));
        let _ = can_nest_inside(&collections, Some(10));
        let _ = depth_of(&collections, 10);
    }
}
