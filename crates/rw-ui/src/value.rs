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
