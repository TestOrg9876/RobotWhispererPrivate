//! Turning a stream of messages into plottable series.
//!
//! A topic delivers whole messages; a plot wants one number over time. This
//! walks each message for its numeric leaves, keyed by dotted path, and keeps a
//! bounded history of them.
//!
//! Pure, so the walking and the windowing are tested without a window or a
//! robot.

use std::collections::BTreeMap;

use rw_canonical::CanonicalValue;

/// How many samples to keep per field.
///
/// At the 100 ms repaint the panel already uses, this is a couple of minutes of
/// history for a 10 Hz topic and about six seconds for a 200 Hz one — enough to
/// see the shape of what is happening, and bounded so a topic left running
/// overnight does not become a memory leak.
pub const WINDOW: usize = 600;

/// How deep to walk into a message looking for numbers.
const MAX_DEPTH: usize = 6;

/// The most fields to track at once.
///
/// A `sensor_msgs/JointState` for a humanoid is hundreds of numbers, and a plot
/// of hundreds of lines is not a plot. The first few are kept and the rest
/// ignored, rather than the whole thing being refused.
pub const MAX_FIELDS: usize = 12;

/// One numeric field's recent history.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Series {
    /// Oldest first, at most [`WINDOW`] long.
    pub samples: Vec<f64>,
}

impl Series {
    fn push(&mut self, sample: f64) {
        if self.samples.len() == WINDOW {
            self.samples.remove(0);
        }
        self.samples.push(sample);
    }

    pub fn last(&self) -> Option<f64> {
        self.samples.last().copied()
    }

    /// The smallest and largest sample, for a caption.
    pub fn range(&self) -> Option<(f64, f64)> {
        let first = *self.samples.first()?;
        Some(
            self.samples
                .iter()
                .fold((first, first), |(low, high), sample| {
                    (low.min(*sample), high.max(*sample))
                }),
        )
    }
}

/// Every numeric field of a topic, over time.
#[derive(Debug, Clone, Default)]
pub struct History {
    /// Ordered by path, so the plot's legend does not reshuffle between frames.
    fields: BTreeMap<String, Series>,
}

impl History {
    /// Adds one message's numbers to the history.
    pub fn observe(&mut self, value: &CanonicalValue) {
        let mut found = Vec::new();
        walk(value, &mut String::new(), 0, &mut found);

        for (path, sample) in found {
            // A path already tracked keeps being tracked even once the cap is
            // reached; only new ones are turned away, so a series does not go
            // ragged because a later message had an extra field.
            if !self.fields.contains_key(&path) && self.fields.len() >= MAX_FIELDS {
                continue;
            }
            self.fields.entry(path).or_default().push(sample);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    pub fn len(&self) -> usize {
        self.fields.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Series)> {
        self.fields
            .iter()
            .map(|(path, series)| (path.as_str(), series))
    }

    pub fn clear(&mut self) {
        self.fields.clear();
    }
}

/// Collects the numeric leaves of `value` as `(dotted path, number)`.
///
/// Arrays are indexed rather than skipped — `position[0]` is exactly the sort of
/// thing worth plotting — but only short ones: a point cloud is a million
/// numbers and none of them belong on a line chart.
fn walk(value: &CanonicalValue, path: &mut String, depth: usize, out: &mut Vec<(String, f64)>) {
    /// Longer than this and the array is data, not fields.
    const MAX_ELEMENTS: usize = 16;

    if depth > MAX_DEPTH {
        return;
    }

    match value {
        CanonicalValue::Int(inner) => out.push((path.clone(), *inner as f64)),
        CanonicalValue::Uint(inner) => out.push((path.clone(), *inner as f64)),
        CanonicalValue::F32(inner) => out.push((path.clone(), *inner as f64)),
        CanonicalValue::F64(inner) => out.push((path.clone(), *inner)),
        // A bool plots as the step function it is, which is how you see a flag
        // flapping.
        CanonicalValue::Bool(inner) => out.push((path.clone(), *inner as u8 as f64)),
        CanonicalValue::Time { sec, nanosec } | CanonicalValue::Duration { sec, nanosec } => {
            out.push((path.clone(), *sec as f64 + *nanosec as f64 / 1e9))
        }
        CanonicalValue::Struct(fields) => {
            for (name, field) in fields {
                let mark = path.len();
                if !path.is_empty() {
                    path.push('.');
                }
                path.push_str(name);
                walk(field, path, depth + 1, out);
                path.truncate(mark);
            }
        }
        CanonicalValue::Array(items) if items.len() <= MAX_ELEMENTS => {
            for (index, item) in items.iter().enumerate() {
                let mark = path.len();
                path.push_str(&format!("[{index}]"));
                walk(item, path, depth + 1, out);
                path.truncate(mark);
            }
        }
        CanonicalValue::Array(_)
        | CanonicalValue::Null
        | CanonicalValue::String(_)
        | CanonicalValue::Bytes(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn structure(fields: [(&str, CanonicalValue); 2]) -> CanonicalValue {
        CanonicalValue::Struct(BTreeMap::from(
            fields.map(|(name, value)| (name.to_string(), value)),
        ))
    }

    fn paths(value: &CanonicalValue) -> Vec<(String, f64)> {
        let mut found = Vec::new();
        walk(value, &mut String::new(), 0, &mut found);
        found
    }

    #[test]
    fn a_bare_number_is_its_own_series() {
        assert_eq!(paths(&CanonicalValue::F64(1.5)), [(String::new(), 1.5)]);
    }

    #[test]
    fn nested_fields_become_dotted_paths() {
        let value = structure([
            (
                "position",
                structure([
                    ("x", CanonicalValue::F64(1.0)),
                    ("y", CanonicalValue::F64(2.0)),
                ]),
            ),
            ("stamp", CanonicalValue::Int(7)),
        ]);

        assert_eq!(
            paths(&value),
            [
                ("position.x".to_string(), 1.0),
                ("position.y".to_string(), 2.0),
                ("stamp".to_string(), 7.0),
            ]
        );
    }

    #[test]
    fn short_arrays_are_indexed() {
        let value = CanonicalValue::Array(vec![CanonicalValue::F64(3.0), CanonicalValue::F64(4.0)]);
        assert_eq!(
            paths(&value),
            [("[0]".to_string(), 3.0), ("[1]".to_string(), 4.0)]
        );
    }

    #[test]
    fn a_long_array_is_data_rather_than_fields() {
        // A point cloud is a million numbers and none of them belong on a line
        // chart.
        let value = CanonicalValue::Array(vec![CanonicalValue::F64(1.0); 64]);
        assert!(paths(&value).is_empty());
    }

    #[test]
    fn strings_and_blobs_are_not_numbers() {
        let value = structure([
            ("name", CanonicalValue::String("arm".into())),
            ("blob", CanonicalValue::Bytes(vec![1, 2, 3])),
        ]);
        assert!(paths(&value).is_empty());
    }

    #[test]
    fn a_bool_plots_as_the_step_function_it_is() {
        assert_eq!(paths(&CanonicalValue::Bool(true)), [(String::new(), 1.0)]);
    }

    #[test]
    fn time_reads_as_seconds() {
        assert_eq!(
            paths(&CanonicalValue::Time {
                sec: 2,
                nanosec: 500_000_000
            }),
            [(String::new(), 2.5)]
        );
    }

    #[test]
    fn history_accumulates_per_field() {
        let mut history = History::default();
        for sample in [1.0, 2.0, 3.0] {
            history.observe(&structure([
                ("a", CanonicalValue::F64(sample)),
                ("b", CanonicalValue::F64(-sample)),
            ]));
        }

        let series: Vec<_> = history.iter().collect();
        assert_eq!(series.len(), 2);
        assert_eq!(series[0].0, "a");
        assert_eq!(series[0].1.samples, [1.0, 2.0, 3.0]);
        assert_eq!(series[1].1.samples, [-1.0, -2.0, -3.0]);
    }

    #[test]
    fn history_is_bounded() {
        let mut history = History::default();
        for sample in 0..WINDOW + 50 {
            history.observe(&CanonicalValue::F64(sample as f64));
        }

        let (_, series) = history.iter().next().expect("one series");
        assert_eq!(series.samples.len(), WINDOW);
        // The oldest samples fell off the front, not the newest off the back.
        assert_eq!(series.samples.first(), Some(&50.0));
        assert_eq!(series.samples.last(), Some(&((WINDOW + 49) as f64)));
    }

    #[test]
    fn a_huge_message_tracks_only_the_first_fields() {
        let mut history = History::default();
        let fields: BTreeMap<_, _> = (0..40)
            .map(|index| (format!("f{index:02}"), CanonicalValue::F64(index as f64)))
            .collect();
        history.observe(&CanonicalValue::Struct(fields));

        assert_eq!(history.len(), MAX_FIELDS);
        // The first by path, so which ones are kept is at least predictable.
        let (first, _) = history.iter().next().expect("a series");
        assert_eq!(first, "f00");
    }

    #[test]
    fn a_field_already_tracked_keeps_being_tracked() {
        // Otherwise a message with one extra field would leave every series
        // ragged from that point on.
        let mut history = History::default();
        for index in 0..MAX_FIELDS {
            let fields: BTreeMap<_, _> = (0..=index)
                .map(|field| (format!("f{field:02}"), CanonicalValue::F64(1.0)))
                .collect();
            history.observe(&CanonicalValue::Struct(fields));
        }
        assert_eq!(history.len(), MAX_FIELDS);

        // `f00` was there from the start, so it has a sample from every message.
        let (_, first) = history.iter().next().expect("a series");
        assert_eq!(first.samples.len(), MAX_FIELDS);
    }

    #[test]
    fn a_series_reports_its_range() {
        let mut series = Series::default();
        for sample in [3.0, -1.0, 7.0, 2.0] {
            series.push(sample);
        }
        assert_eq!(series.range(), Some((-1.0, 7.0)));
        assert_eq!(series.last(), Some(2.0));
        assert_eq!(Series::default().range(), None);
    }
}
