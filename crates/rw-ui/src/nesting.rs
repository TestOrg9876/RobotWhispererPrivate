//! The rules that keep the collection tree a tree.
//!
//! The sidebar's *rendering* is `gpui_component::tree`, which flattens, indents,
//! expands and virtualises on its own. What it cannot know is how deep this app
//! allows collections to nest and which moves would detach a branch — so that
//! lives here, where it can be tested without a window.

use rw_core::domain::Collection;

/// How many levels of collection there may be.
///
/// A top-level collection is depth 0, so with a cap of 3 the deepest one cannot
/// contain another. Deeper than this a sidebar stops being navigable; requests
/// can still live at any level.
pub const MAX_DEPTH: usize = 3;

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

    fn collection(id: i64, name: &str, parent: Option<i64>) -> Collection {
        Collection {
            id,
            parent_id: parent,
            name: name.into(),
            created_at: Utc.timestamp_opt(0, 0).unwrap(),
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
