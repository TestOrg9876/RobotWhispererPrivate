//! Arranging requests into the folder tree the sidebar draws.
//!
//! Collections nest, requests hang off them, and a search has to be able to hide
//! a folder whose contents all matched nothing while keeping the ones that did.
//! That is enough branching to be worth testing on its own, so none of it needs
//! a window.

use rw_core::domain::{Collection, Request};

/// One line of the sidebar, already flattened for rendering.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    Folder {
        id: i64,
        name: String,
        /// How far in to indent it.
        depth: usize,
        /// Requests inside it and inside its descendants.
        total: usize,
        expanded: bool,
    },
    Request {
        id: i64,
        depth: usize,
    },
}

impl Row {
    pub fn depth(&self) -> usize {
        match self {
            Row::Folder { depth, .. } | Row::Request { depth, .. } => *depth,
        }
    }
}

/// How deep the tree may nest.
///
/// Collections cannot legitimately contain themselves; this is a guard against a
/// cycle in stored data becoming an infinite loop in the sidebar.
const MAX_DEPTH: usize = 12;

/// Flattens the tree into the rows to draw.
///
/// `matched` decides which requests count — a search passes the ones it found,
/// and everything else passes them all.
///
/// `expanded` is asked about each folder. A collapsed folder still reports its
/// total, so the count beside it does not change as it is opened and closed.
///
/// `hide_empty` is for searching. While searching, a folder whose contents all
/// matched nothing is a line the reader has to check and discard. The rest of
/// the time an empty folder must still be drawn — the first thing that happens
/// to a new folder is that it is empty, and a folder you cannot see is a folder
/// you cannot put anything in.
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
    // rather than whether the folder exists covers every way a request can be
    // stranded — no folder, a deleted folder, or a folder that is real but
    // unreachable because the stored parents form a cycle. A request must not
    // disappear because of a mistake in the folder above it.
    //
    // A collapsed folder's contents are deliberately not drawn, so those do not
    // count as stranded: they are hidden, and hidden is not lost.
    let hidden_by_collapse = |request: &Request| {
        request.collection_id.is_some_and(|id| {
            rows.iter()
                .any(|row| matches!(row, Row::Folder { id: folder, .. } if *folder == id))
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
        rows.push(Row::Folder {
            id: collection.id,
            name: collection.name.clone(),
            depth,
            total,
            expanded: open,
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

/// Requests in `folder` and everything below it.
fn count(collections: &[Collection], kept: &[&Request], folder: i64, depth: usize) -> usize {
    if depth >= MAX_DEPTH {
        return 0;
    }

    let own = kept
        .iter()
        .filter(|request| request.collection_id == Some(folder))
        .count();

    let below: usize = collections
        .iter()
        .filter(|collection| collection.parent_id == Some(folder))
        .map(|collection| count(collections, kept, collection.id, depth + 1))
        .sum();

    own + below
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone as _, Utc};
    use rw_core::domain::{RequestKind, Value};

    fn folder(id: i64, name: &str, parent: Option<i64>) -> Collection {
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
    fn requests_sit_under_their_folder_indented() {
        let rows = rows(
            &[folder(10, "Arm", None)],
            &[request(1, "A", Some(10))],
            all,
            open,
            false,
        );
        assert_eq!(
            rows,
            [
                Row::Folder {
                    id: 10,
                    name: "Arm".into(),
                    depth: 0,
                    total: 1,
                    expanded: true
                },
                Row::Request { id: 1, depth: 1 },
            ]
        );
    }

    #[test]
    fn nesting_indents_further_and_counts_upwards() {
        let rows = rows(
            &[folder(10, "Robot", None), folder(11, "Arm", Some(10))],
            &[request(1, "A", Some(11)), request(2, "B", Some(10))],
            all,
            open,
            false,
        );
        let depths: Vec<_> = rows.iter().map(Row::depth).collect();
        assert_eq!(depths, [0, 1, 2, 1]);
        // The outer folder counts what is below it as well as its own.
        assert!(matches!(rows[0], Row::Folder { total: 2, .. }));
        assert!(matches!(rows[1], Row::Folder { total: 1, .. }));
    }

    #[test]
    fn a_collapsed_folder_hides_its_contents_but_keeps_its_count() {
        // Otherwise the number beside a folder changes as it is opened and
        // closed, which makes it useless for deciding whether to open it.
        let rows = rows(
            &[folder(10, "Arm", None)],
            &[request(1, "A", Some(10))],
            all,
            |_| false,
            false,
        );
        assert_eq!(
            rows,
            [Row::Folder {
                id: 10,
                name: "Arm".into(),
                depth: 0,
                total: 1,
                expanded: false
            }]
        );
    }

    #[test]
    fn an_empty_folder_is_drawn_so_things_can_be_put_in_it() {
        // The first thing that happens to a new folder is that it is empty, and
        // a folder you cannot see is a folder you cannot put anything in.
        let rows = rows(&[folder(10, "Arm", None)], &[], all, open, false);
        assert_eq!(
            rows,
            [Row::Folder {
                id: 10,
                name: "Arm".into(),
                depth: 0,
                total: 0,
                expanded: true
            }]
        );
    }

    #[test]
    fn an_empty_folder_is_hidden_while_searching() {
        // Then it is a line the reader has to check and discard.
        assert!(rows(&[folder(10, "Arm", None)], &[], all, open, true).is_empty());
    }

    #[test]
    fn a_search_hides_folders_that_matched_nothing() {
        let collections = [folder(10, "Arm", None), folder(11, "Base", None)];
        let requests = [request(1, "lift", Some(10)), request(2, "drive", Some(11))];
        let rows = rows(&collections, &requests, |r| r.name == "lift", open, true);

        assert_eq!(
            rows,
            [
                Row::Folder {
                    id: 10,
                    name: "Arm".into(),
                    depth: 0,
                    total: 1,
                    expanded: true
                },
                Row::Request { id: 1, depth: 1 },
            ]
        );
    }

    #[test]
    fn a_search_keeps_a_folder_whose_child_folder_matched() {
        let collections = [folder(10, "Robot", None), folder(11, "Arm", Some(10))];
        let requests = [request(1, "lift", Some(11))];
        let rows = rows(&collections, &requests, |r| r.name == "lift", open, true);
        assert_eq!(rows.len(), 3);
        assert!(matches!(rows[0], Row::Folder { id: 10, .. }));
    }

    #[test]
    fn a_request_whose_folder_is_gone_still_appears() {
        // Losing a folder must not silently lose the work inside it.
        let rows = rows(&[], &[request(1, "orphan", Some(99))], all, open, false);
        assert_eq!(rows, [Row::Request { id: 1, depth: 0 }]);
    }

    #[test]
    fn a_cycle_in_stored_folders_terminates() {
        // Two folders each claiming the other as parent cannot be reached from
        // the root at all, so the tree is simply empty rather than endless.
        let collections = [folder(10, "A", Some(11)), folder(11, "B", Some(10))];
        let rows = rows(&collections, &[request(1, "x", Some(10))], all, open, false);
        assert_eq!(rows, [Row::Request { id: 1, depth: 0 }]);
    }

    #[test]
    fn a_request_in_a_real_but_unreachable_folder_still_appears() {
        // The folder exists, so "does the folder exist" is not enough of a test;
        // it is simply not reachable from the root.
        let collections = [folder(10, "A", Some(11)), folder(11, "B", Some(10))];
        let rows = rows(&collections, &[request(1, "x", Some(10))], all, open, false);
        assert_eq!(rows, [Row::Request { id: 1, depth: 0 }]);
    }

    #[test]
    fn a_collapsed_folders_contents_are_hidden_rather_than_stranded() {
        // Hidden is not lost: a collapsed folder's requests must not reappear at
        // the root, which is what a naive "was it drawn" check would do.
        let rows = rows(
            &[folder(10, "Arm", None)],
            &[request(1, "A", Some(10))],
            all,
            |_| false,
            false,
        );
        assert_eq!(rows.len(), 1);
        assert!(matches!(
            rows[0],
            Row::Folder {
                expanded: false,
                ..
            }
        ));
    }

    #[test]
    fn root_requests_come_after_the_folders() {
        // Folders first is the convention every file tree uses.
        let rows = rows(
            &[folder(10, "Arm", None)],
            &[request(1, "loose", None), request(2, "inside", Some(10))],
            all,
            open,
            false,
        );
        assert!(matches!(rows[0], Row::Folder { .. }));
        assert_eq!(rows[2], Row::Request { id: 1, depth: 0 });
    }
}
