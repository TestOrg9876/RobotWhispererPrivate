//! Rendering `CanonicalValue` as readable text.
//!
//! Replaces `valuePreviewText` from the old `types.ts`. The budgets matter:
//! a `sensor_msgs/PointCloud2` or a camera frame carries millions of elements,
//! and formatting all of them would stall the frame that asked for them.

use rw_canonical::CanonicalValue;
use std::fmt::Write as _;

/// Longest array or byte string rendered before it is elided.
const MAX_ITEMS: usize = 200;
/// Total nodes visited before the rest is truncated.
const NODE_BUDGET: usize = 20_000;
/// Spaces per nesting level.
const INDENT: usize = 2;

/// Formats a value as indented text, eliding long sequences.
pub fn preview(value: &CanonicalValue) -> String {
    let mut out = String::new();
    let mut budget = NODE_BUDGET;
    write_value(&mut out, value, 0, &mut budget);
    out
}

fn write_value(out: &mut String, value: &CanonicalValue, depth: usize, budget: &mut usize) {
    if *budget == 0 {
        out.push('…');
        return;
    }
    *budget -= 1;

    match value {
        CanonicalValue::Null => out.push_str("null"),
        CanonicalValue::Bool(inner) => {
            let _ = write!(out, "{inner}");
        }
        CanonicalValue::Int(inner) => {
            let _ = write!(out, "{inner}");
        }
        CanonicalValue::Uint(inner) => {
            let _ = write!(out, "{inner}");
        }
        CanonicalValue::F32(inner) => {
            let _ = write!(out, "{inner}");
        }
        CanonicalValue::F64(inner) => {
            let _ = write!(out, "{inner}");
        }
        CanonicalValue::String(inner) => {
            let _ = write!(out, "{inner:?}");
        }
        CanonicalValue::Time { sec, nanosec } | CanonicalValue::Duration { sec, nanosec } => {
            let _ = write!(out, "{sec}.{nanosec:09}");
        }
        CanonicalValue::Bytes(bytes) => {
            let shown = bytes.len().min(MAX_ITEMS);
            *budget = budget.saturating_sub(shown);
            let _ = write!(out, "<{} bytes>", bytes.len());
            if shown > 0 {
                out.push(' ');
                for byte in &bytes[..shown] {
                    let _ = write!(out, "{byte:02x}");
                }
                if bytes.len() > shown {
                    out.push('…');
                }
            }
        }
        CanonicalValue::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            let shown = items.len().min(MAX_ITEMS);
            out.push_str("[\n");
            for item in &items[..shown] {
                indent(out, depth + 1);
                write_value(out, item, depth + 1, budget);
                out.push('\n');
                if *budget == 0 {
                    break;
                }
            }
            if items.len() > shown {
                indent(out, depth + 1);
                let _ = writeln!(out, "… {} more", items.len() - shown);
            }
            indent(out, depth);
            out.push(']');
        }
        CanonicalValue::Struct(fields) => {
            if fields.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            for (name, field) in fields {
                indent(out, depth + 1);
                let _ = write!(out, "{name}: ");
                write_value(out, field, depth + 1, budget);
                out.push('\n');
                if *budget == 0 {
                    indent(out, depth + 1);
                    out.push_str("…\n");
                    break;
                }
            }
            indent(out, depth);
            out.push('}');
        }
    }
}

fn indent(out: &mut String, depth: usize) {
    out.extend(std::iter::repeat_n(' ', depth * INDENT));
}

/// The most rows a field table shows.
///
/// Past this the table has stopped being something a person reads and become
/// something they scroll past; the raw view is there for the whole thing.
const MAX_LEAVES: usize = 400;

/// Flattens a value into `(dotted path, rendered value)` rows.
///
/// One row per leaf, so a large message can be scanned for the one field that
/// matters instead of read as nested text. Structures and arrays are not rows
/// of their own — their leaves are.
pub fn leaves(value: &CanonicalValue) -> Vec<(String, String)> {
    cells(value)
        .into_iter()
        .map(|(path, cell)| (path, cell.shown()))
        .collect()
}

/// What one row of the leaf table holds.
///
/// A scalar keeps its value rather than only its text, so a diff can take a
/// difference instead of comparing two strings — 3 and 3.0 are the same number
/// and "3" and "3.0" are not.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Scalar(CanonicalValue),
    /// A long array or a byte string: summarised rather than expanded, the same
    /// way the table shows it.
    Summary(String),
}

impl Cell {
    /// How the row reads.
    pub fn shown(&self) -> String {
        match self {
            Self::Scalar(value) => scalar(value),
            Self::Summary(text) => text.clone(),
        }
    }

    /// The row as a number, when it is one.
    pub fn number(&self) -> Option<f64> {
        match self {
            Self::Scalar(CanonicalValue::Int(inner)) => Some(*inner as f64),
            Self::Scalar(CanonicalValue::Uint(inner)) => Some(*inner as f64),
            Self::Scalar(CanonicalValue::F32(inner)) => Some(*inner as f64),
            Self::Scalar(CanonicalValue::F64(inner)) => Some(*inner),
            _ => None,
        }
    }
}

/// The same walk as [`leaves`], with the values kept.
pub fn cells(value: &CanonicalValue) -> Vec<(String, Cell)> {
    let mut rows = Vec::new();
    collect(value, &mut String::new(), &mut rows);
    rows
}

fn collect(value: &CanonicalValue, path: &mut String, rows: &mut Vec<(String, Cell)>) {
    if rows.len() >= MAX_LEAVES {
        return;
    }

    match value {
        CanonicalValue::Struct(fields) => {
            for (name, field) in fields {
                let mark = path.len();
                if !path.is_empty() {
                    path.push('.');
                }
                path.push_str(name);
                collect(field, path, rows);
                path.truncate(mark);
            }
        }
        // A short array is worth expanding — `position[1]` is a field people
        // look for. A long one is data, and is summarised instead.
        CanonicalValue::Array(items) if items.len() <= 8 => {
            for (index, item) in items.iter().enumerate() {
                let mark = path.len();
                let _ = write!(path, "[{index}]");
                collect(item, path, rows);
                path.truncate(mark);
            }
        }
        CanonicalValue::Array(items) => rows.push((
            row_path(path),
            Cell::Summary(format!("[{} items]", items.len())),
        )),
        CanonicalValue::Bytes(bytes) => rows.push((
            row_path(path),
            Cell::Summary(format!("[{} bytes]", bytes.len())),
        )),
        leaf => rows.push((row_path(path), Cell::Scalar(leaf.clone()))),
    }
}

fn row_path(path: &str) -> String {
    if path.is_empty() {
        "value".to_string()
    } else {
        path.to_string()
    }
}

/// One scalar, as it should read in a table cell.
/// One scalar, as the tables and the tree both spell it.
pub fn scalar(value: &CanonicalValue) -> String {
    match value {
        CanonicalValue::Null => "null".into(),
        CanonicalValue::Bool(inner) => inner.to_string(),
        CanonicalValue::Int(inner) => inner.to_string(),
        CanonicalValue::Uint(inner) => inner.to_string(),
        CanonicalValue::F32(inner) => inner.to_string(),
        CanonicalValue::F64(inner) => inner.to_string(),
        CanonicalValue::String(inner) => inner.clone(),
        CanonicalValue::Time { sec, nanosec } | CanonicalValue::Duration { sec, nanosec } => {
            format!("{sec}.{nanosec:09}")
        }
        // Handled by `collect`; here only so this is total.
        CanonicalValue::Bytes(_) | CanonicalValue::Array(_) | CanonicalValue::Struct(_) => {
            preview(value)
        }
    }
}

#[cfg(test)]
mod leaf_tests {
    use super::*;
    use std::collections::BTreeMap;

    fn structure(fields: [(&str, CanonicalValue); 2]) -> CanonicalValue {
        CanonicalValue::Struct(BTreeMap::from(
            fields.map(|(name, value)| (name.to_string(), value)),
        ))
    }

    #[test]
    fn a_scalar_is_one_row_named_value() {
        assert_eq!(
            leaves(&CanonicalValue::Int(3)),
            [("value".to_string(), "3".to_string())]
        );
    }

    #[test]
    fn nesting_becomes_dotted_paths_not_indentation() {
        let value = structure([
            (
                "position",
                structure([
                    ("x", CanonicalValue::F64(1.0)),
                    ("y", CanonicalValue::F64(2.0)),
                ]),
            ),
            ("frame", CanonicalValue::String("map".into())),
        ]);
        assert_eq!(
            leaves(&value),
            [
                ("frame".to_string(), "map".to_string()),
                ("position.x".to_string(), "1".to_string()),
                ("position.y".to_string(), "2".to_string()),
            ]
        );
    }

    #[test]
    fn a_short_array_expands_and_a_long_one_is_summarised() {
        let short = CanonicalValue::Array(vec![CanonicalValue::Int(1), CanonicalValue::Int(2)]);
        assert_eq!(
            leaves(&short),
            [
                ("[0]".to_string(), "1".to_string()),
                ("[1]".to_string(), "2".to_string()),
            ]
        );

        let long = CanonicalValue::Array(vec![CanonicalValue::Int(0); 50]);
        assert_eq!(
            leaves(&long),
            [("value".to_string(), "[50 items]".to_string())]
        );
    }

    #[test]
    fn a_blob_reports_its_size_rather_than_its_contents() {
        let value = CanonicalValue::Bytes(vec![0; 1024]);
        assert_eq!(
            leaves(&value),
            [("value".to_string(), "[1024 bytes]".to_string())]
        );
    }

    #[test]
    fn a_huge_message_stops_at_the_cap() {
        let fields: BTreeMap<_, _> = (0..MAX_LEAVES + 100)
            .map(|index| (format!("f{index:04}"), CanonicalValue::Int(index as i64)))
            .collect();
        assert_eq!(leaves(&CanonicalValue::Struct(fields)).len(), MAX_LEAVES);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn structure(fields: &[(&str, CanonicalValue)]) -> CanonicalValue {
        CanonicalValue::Struct(
            fields
                .iter()
                .map(|(name, value)| (name.to_string(), value.clone()))
                .collect::<BTreeMap<_, _>>(),
        )
    }

    #[test]
    fn scalars_render_bare() {
        assert_eq!(preview(&CanonicalValue::Null), "null");
        assert_eq!(preview(&CanonicalValue::Bool(true)), "true");
        assert_eq!(preview(&CanonicalValue::Int(-3)), "-3");
        assert_eq!(preview(&CanonicalValue::Uint(7)), "7");
    }

    #[test]
    fn strings_are_quoted_so_whitespace_is_visible() {
        assert_eq!(preview(&CanonicalValue::String("a b".into())), "\"a b\"");
    }

    #[test]
    fn time_keeps_nanosecond_precision() {
        let stamp = CanonicalValue::Time {
            sec: 12,
            nanosec: 5,
        };
        assert_eq!(preview(&stamp), "12.000000005");
    }

    #[test]
    fn empty_containers_stay_on_one_line() {
        assert_eq!(preview(&CanonicalValue::Array(vec![])), "[]");
        assert_eq!(preview(&structure(&[])), "{}");
    }

    #[test]
    fn structs_render_sorted_and_indented() {
        let value = structure(&[("b", CanonicalValue::Int(2)), ("a", CanonicalValue::Int(1))]);
        assert_eq!(preview(&value), "{\n  a: 1\n  b: 2\n}");
    }

    #[test]
    fn long_arrays_are_elided_with_a_count() {
        let items = (0..MAX_ITEMS + 25)
            .map(|_| CanonicalValue::Uint(1))
            .collect();
        let text = preview(&CanonicalValue::Array(items));
        assert!(text.contains("… 25 more"), "{text}");
    }

    #[test]
    fn bytes_report_length_and_truncate() {
        let text = preview(&CanonicalValue::Bytes(vec![0xab; MAX_ITEMS + 10]));
        assert!(
            text.starts_with(&format!("<{} bytes>", MAX_ITEMS + 10)),
            "{text}"
        );
        assert!(text.ends_with('…'), "{text}");
    }

    #[test]
    fn a_pathological_value_does_not_run_away() {
        // A deep, wide structure must finish quickly and stay bounded.
        let leaf = CanonicalValue::Array((0..500).map(CanonicalValue::Int).collect());
        let mut value = leaf.clone();
        for _ in 0..50 {
            value = structure(&[("child", value), ("leaf", leaf.clone())]);
        }
        let text = preview(&value);
        // Budget is nodes, not bytes, but the output must be far below the
        // ~25k leaves this structure nominally contains.
        assert!(
            text.len() < 2_000_000,
            "runaway output: {} bytes",
            text.len()
        );
    }

    #[test]
    fn nested_arrays_indent_by_depth() {
        let value =
            CanonicalValue::Array(vec![CanonicalValue::Array(vec![CanonicalValue::Int(1)])]);
        assert_eq!(preview(&value), "[\n  [\n    1\n  ]\n]");
    }
}
