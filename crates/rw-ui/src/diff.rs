//! What changed between two messages.
//!
//! The thing a person is actually doing when they stare at a topic scrolling
//! past is looking for the one field that moved. A raw view makes them do that
//! by eye, forty times a second, across two hundred fields — and the field they
//! want is usually the one that moved by 0.003.
//!
//! So: pin a message, and let the stream keep running. This walks the pinned
//! one and the live one into a list of what is different, with a delta on every
//! number. Unchanged fields are simply not in the list, which is the whole
//! point — a diff that showed everything would be the raw view again.
//!
//! Pure and tested. It reuses `value::cells`, so the paths here are the same
//! paths the field table shows and a person can look one up in the other.

use std::collections::{BTreeMap, BTreeSet};

use rw_canonical::CanonicalValue;

use crate::value::{self, Cell};

/// What happened to one field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The field is in the live message and was not in the pinned one.
    Added,
    /// The field was in the pinned message and is not in the live one.
    Removed,
    /// The field is in both and reads differently.
    Changed,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Changed => "changed",
        }
    }
}

/// One field that is not the same as it was.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    /// The same path the field table uses.
    pub path: String,
    /// How it read when it was pinned. `None` when the field is new.
    pub before: Option<String>,
    /// How it reads now. `None` when the field has gone.
    pub after: Option<String>,
    /// `after - before`, for fields that are numbers at both ends.
    ///
    /// The number people are actually after: "0.417 → 0.420" says less at a
    /// glance than "+0.003", and a field that has moved by a millionth is
    /// noise a delta makes visible as noise.
    pub delta: Option<f64>,
}

impl Change {
    pub fn kind(&self) -> Kind {
        match (&self.before, &self.after) {
            (None, _) => Kind::Added,
            (_, None) => Kind::Removed,
            _ => Kind::Changed,
        }
    }

    /// The delta as it should read beside the change.
    ///
    /// Signed always: the sign is the information, and a delta without one is
    /// just another number to subtract by eye.
    pub fn delta_label(&self) -> Option<String> {
        let delta = self.delta?;
        if delta == 0. {
            return None;
        }
        Some(if delta.abs() >= 1e9 || delta.abs() < 0.001 {
            format!("{delta:+.3e}")
        } else if delta.fract() == 0. {
            // A counter that went up by 24 went up by 24, not by 24.0000.
            format!("{delta:+.0}")
        } else {
            format!("{delta:+.4}")
        })
    }
}

/// Every field that differs between the pinned message and the live one.
///
/// Ordered by path, so a field stays in the same place in the list from one
/// message to the next — a list that reordered itself as values changed would
/// be unreadable at 40 Hz.
pub fn diff(before: &CanonicalValue, after: &CanonicalValue) -> Vec<Change> {
    let before: BTreeMap<String, Cell> = value::cells(before).into_iter().collect();
    let after: BTreeMap<String, Cell> = value::cells(after).into_iter().collect();

    // The union of both sides' paths, in order and without repeats — which is
    // also the order the rows come out in, so nothing has to be sorted after.
    let paths: BTreeSet<&String> = before.keys().chain(after.keys()).collect();

    let mut changes = Vec::new();
    for path in paths {
        let (was, now) = (before.get(path), after.get(path));
        match (was, now) {
            (Some(was), Some(now)) => {
                // Compared as values, not as text: an integer 3 that became a
                // float 3.0 has not changed, and a field whose text happens to
                // match but whose type did is a change worth seeing.
                if was == now {
                    continue;
                }
                let delta = match (was.number(), now.number()) {
                    (Some(was), Some(now)) => Some(now - was),
                    _ => None,
                };
                // Two numbers that differ only in how they are spelled are not
                // a change: an f32 3.0 and an f64 3.0 are the same reading.
                if delta == Some(0.) && was.shown() == now.shown() {
                    continue;
                }
                changes.push(Change {
                    path: path.clone(),
                    before: Some(was.shown()),
                    after: Some(now.shown()),
                    delta,
                });
            }
            (Some(was), None) => changes.push(Change {
                path: path.clone(),
                before: Some(was.shown()),
                after: None,
                delta: None,
            }),
            (None, Some(now)) => changes.push(Change {
                path: path.clone(),
                before: None,
                after: Some(now.shown()),
                delta: None,
            }),
            (None, None) => unreachable!("the path came from one of the two maps"),
        }
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap as Map;

    fn map<const N: usize>(fields: [(&str, CanonicalValue); N]) -> CanonicalValue {
        CanonicalValue::Struct(
            fields
                .into_iter()
                .map(|(name, value)| (name.to_string(), value))
                .collect::<Map<_, _>>(),
        )
    }

    fn find<'a>(changes: &'a [Change], path: &str) -> Option<&'a Change> {
        changes.iter().find(|change| change.path == path)
    }

    #[test]
    fn an_unchanged_message_has_nothing_in_it() {
        let message = map([
            ("a", CanonicalValue::F64(1.)),
            ("b", CanonicalValue::String("hi".into())),
        ]);
        assert_eq!(diff(&message, &message), Vec::new());
    }

    #[test]
    fn unchanged_branches_collapse_and_only_the_field_that_moved_is_listed() {
        // The whole point: two hundred fields, one of them different.
        let build = |x: f64| {
            map([
                (
                    "pose",
                    map([
                        ("x", CanonicalValue::F64(x)),
                        ("y", CanonicalValue::F64(2.)),
                        ("z", CanonicalValue::F64(3.)),
                    ]),
                ),
                ("name", CanonicalValue::String("arm".into())),
            ])
        };
        let changes = diff(&build(1.), &build(1.5));
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "pose.x");
        assert_eq!(changes[0].kind(), Kind::Changed);
    }

    #[test]
    fn a_numeric_field_carries_its_delta() {
        let changes = diff(
            &map([("value", CanonicalValue::F64(0.417))]),
            &map([("value", CanonicalValue::F64(0.420))]),
        );
        let change = &changes[0];
        assert!((change.delta.expect("a number has a delta") - 0.003).abs() < 1e-9);
        assert_eq!(change.delta_label().as_deref(), Some("+0.0030"));
    }

    #[test]
    fn a_delta_that_went_down_says_so() {
        let changes = diff(
            &map([("value", CanonicalValue::Int(10))]),
            &map([("value", CanonicalValue::Int(4))]),
        );
        assert_eq!(changes[0].delta, Some(-6.));
        assert_eq!(
            changes[0].delta_label().as_deref(),
            Some("-6"),
            "a counter that went down by six went down by six"
        );
    }

    #[test]
    fn a_tiny_delta_is_shown_in_exponent_form_rather_than_as_zero() {
        let changes = diff(
            &map([("value", CanonicalValue::F64(1.0))]),
            &map([("value", CanonicalValue::F64(1.000_000_2))]),
        );
        let label = changes[0].delta_label().expect("still a change");
        assert!(label.starts_with('+') && label.contains('e'), "{label}");
    }

    #[test]
    fn a_field_that_is_not_a_number_changes_without_a_delta() {
        let changes = diff(
            &map([("mode", CanonicalValue::String("idle".into()))]),
            &map([("mode", CanonicalValue::String("running".into()))]),
        );
        assert_eq!(changes[0].delta, None);
        assert_eq!(changes[0].delta_label(), None);
        assert_eq!(changes[0].before.as_deref(), Some("idle"));
        assert_eq!(changes[0].after.as_deref(), Some("running"));
    }

    #[test]
    fn an_added_key_is_marked_added_and_a_removed_one_removed() {
        let before = map([("a", CanonicalValue::Int(1))]);
        let after = map([("a", CanonicalValue::Int(1)), ("b", CanonicalValue::Int(2))]);

        let added = diff(&before, &after);
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].kind(), Kind::Added);
        assert_eq!(added[0].before, None);
        assert_eq!(added[0].after.as_deref(), Some("2"));

        let removed = diff(&after, &before);
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].kind(), Kind::Removed);
        assert_eq!(removed[0].after, None);
    }

    #[test]
    fn the_same_number_spelled_two_ways_is_not_a_change() {
        // A codec that promotes an f32 to an f64 between two messages has not
        // moved the robot, and a diff that said so would cry wolf every frame.
        let changes = diff(
            &map([("value", CanonicalValue::F32(3.0))]),
            &map([("value", CanonicalValue::F64(3.0))]),
        );
        assert_eq!(changes, Vec::new());
    }

    #[test]
    fn a_field_whose_type_changed_under_the_same_text_is_a_change() {
        let changes = diff(
            &map([("value", CanonicalValue::String("3".into()))]),
            &map([("value", CanonicalValue::Int(3))]),
        );
        assert_eq!(changes.len(), 1, "a string 3 and an integer 3 differ");
    }

    #[test]
    fn changes_are_ordered_by_path_so_a_row_stays_where_it_was() {
        let before = map([
            ("zulu", CanonicalValue::Int(0)),
            ("alpha", CanonicalValue::Int(0)),
            ("mike", CanonicalValue::Int(0)),
        ]);
        let after = map([
            ("zulu", CanonicalValue::Int(1)),
            ("alpha", CanonicalValue::Int(1)),
            ("mike", CanonicalValue::Int(1)),
        ]);
        let changes = diff(&before, &after);
        let paths: Vec<&str> = changes.iter().map(|change| change.path.as_str()).collect();
        assert_eq!(paths, vec!["alpha", "mike", "zulu"]);
    }

    #[test]
    fn a_short_array_is_compared_element_by_element() {
        let before = map([(
            "position",
            CanonicalValue::Array(vec![
                CanonicalValue::F64(1.),
                CanonicalValue::F64(2.),
                CanonicalValue::F64(3.),
            ]),
        )]);
        let after = map([(
            "position",
            CanonicalValue::Array(vec![
                CanonicalValue::F64(1.),
                CanonicalValue::F64(2.5),
                CanonicalValue::F64(3.),
            ]),
        )]);
        let changes = diff(&before, &after);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "position[1]");
        assert_eq!(changes[0].delta, Some(0.5));
    }

    #[test]
    fn a_long_array_is_compared_by_its_summary_rather_than_element_by_element() {
        // The field table summarises a long array, and so does this: a lidar
        // sweep changing every point is one row saying so, not four hundred
        // thousand rows saying nothing.
        let long = |extra: usize| {
            map([(
                "data",
                CanonicalValue::Array(vec![CanonicalValue::Int(0); 100 + extra]),
            )])
        };
        assert_eq!(diff(&long(0), &long(0)), Vec::new());
        let changes = diff(&long(0), &long(1));
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].before.as_deref(), Some("[100 items]"));
        assert_eq!(changes[0].after.as_deref(), Some("[101 items]"));
        assert_eq!(changes[0].delta, None);
    }

    #[test]
    fn a_blob_that_changed_length_is_one_row_not_a_million() {
        let before = map([("data", CanonicalValue::Bytes(vec![0; 1024]))]);
        let after = map([("data", CanonicalValue::Bytes(vec![0; 2048]))]);
        let changes = diff(&before, &after);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].after.as_deref(), Some("[2048 bytes]"));
    }

    #[test]
    fn a_whole_message_replaced_by_a_different_shape_still_diffs() {
        let before = map([("a", CanonicalValue::Int(1))]);
        let after = map([("b", CanonicalValue::Int(2))]);
        let changes = diff(&before, &after);
        assert_eq!(changes.len(), 2);
        assert_eq!(find(&changes, "a").unwrap().kind(), Kind::Removed);
        assert_eq!(find(&changes, "b").unwrap().kind(), Kind::Added);
    }

    #[test]
    fn a_scalar_message_with_no_fields_at_all_still_diffs() {
        let changes = diff(&CanonicalValue::Int(1), &CanonicalValue::Int(2));
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].path, "value");
        assert_eq!(changes[0].delta, Some(1.));
    }

    #[test]
    fn a_zero_delta_shows_no_label_but_the_row_survives_if_the_value_really_moved() {
        // Nothing produces this pair through `diff` — it is here because the
        // label is public and a caller can build a Change by hand.
        let change = Change {
            path: "x".into(),
            before: Some("1".into()),
            after: Some("1".into()),
            delta: Some(0.),
        };
        assert_eq!(change.delta_label(), None);
    }
}
