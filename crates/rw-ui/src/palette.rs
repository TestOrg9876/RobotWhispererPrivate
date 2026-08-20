//! The command palette: one field that finds anything.
//!
//! Commands, saved requests and connections all live in the same list, because
//! "what do I want to do" and "what do I want to open" are the same question
//! when you are typing rather than pointing.

use gpui::SharedString;

/// What a palette row does when chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// Run a named application action.
    Command(&'static str),
    /// Open a saved request.
    Request(i64),
    /// Connect or disconnect a ROS system.
    Connection(i64),
    /// Open a saved dashboard.
    Dashboard(i64),
    /// Point a dashboard pane at a topic.
    ///
    /// The palette is the topic picker: it already ranks, already takes the
    /// keyboard, and is already the thing people reach for to find something
    /// by name. A flat menu of every topic works on the twelve a simulator
    /// publishes and not at all on the three hundred a real robot does, and a
    /// second search box beside this one would be a second set of ranking
    /// rules to disagree with.
    PaneTopic {
        pane: u64,
        connection: i64,
        topic: SharedString,
    },
    /// Put a topic in the 3D world.
    WorldTopic {
        pane: u64,
        connection: i64,
        topic: SharedString,
    },
}

/// One row of the palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub label: SharedString,
    /// The group heading, and what a row is searched by besides its label.
    pub group: &'static str,
    /// Extra searchable text — a request's target, a connection's URL.
    pub detail: Option<SharedString>,
    pub choice: Choice,
}

impl Entry {
    pub fn new(group: &'static str, label: impl Into<SharedString>, choice: Choice) -> Self {
        Self {
            label: label.into(),
            group,
            detail: None,
            choice,
        }
    }

    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        let detail = detail.into();
        self.detail = (!detail.is_empty()).then_some(detail);
        self
    }
}

/// How well a row matched, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    /// The label starts with the query.
    Prefix,
    /// A word of the label starts with the query.
    Word,
    /// The label contains the query.
    Contains,
    /// Only the detail matched — a request found by its topic, say.
    Detail,
}

/// The rows matching `query`, best first.
///
/// An empty query keeps the given order, which is why the caller lists commands
/// before requests: with nothing typed, the palette should read as a menu.
pub fn search(entries: &[Entry], query: &str) -> Vec<Entry> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return entries.to_vec();
    }

    let mut ranked: Vec<_> = entries
        .iter()
        .enumerate()
        .filter_map(|(position, entry)| rank(entry, &query).map(|rank| (rank, position, entry)))
        .collect();

    // Rank first, then the caller's order, so equally good matches do not
    // shuffle as the query grows.
    ranked.sort_by_key(|(rank, position, _)| (*rank, *position));
    ranked
        .into_iter()
        .map(|(_, _, entry)| entry.clone())
        .collect()
}

fn rank(entry: &Entry, query: &str) -> Option<Rank> {
    let label = entry.label.to_lowercase();
    if label.starts_with(query) {
        return Some(Rank::Prefix);
    }
    if label
        .split(|character: char| !character.is_alphanumeric())
        .any(|word| !word.is_empty() && word.starts_with(query))
    {
        return Some(Rank::Word);
    }
    if label.contains(query) {
        return Some(Rank::Contains);
    }
    entry
        .detail
        .as_ref()
        .is_some_and(|detail| detail.to_lowercase().contains(query))
        .then_some(Rank::Detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries() -> Vec<Entry> {
        vec![
            Entry::new("Commands", "New request", Choice::Command("NewRequest")),
            Entry::new("Commands", "Settings", Choice::Command("OpenSettings")),
            Entry::new(
                "Commands",
                "Toggle console",
                Choice::Command("ToggleConsole"),
            ),
            Entry::new("Requests", "Arm home", Choice::Request(1)).detail("/arm/move_to"),
            Entry::new("Requests", "Drive forward", Choice::Request(2)).detail("/cmd_vel"),
            Entry::new("Connections", "Robot", Choice::Connection(9)).detail("ws://localhost:8765"),
        ]
    }

    fn labels(query: &str) -> Vec<String> {
        search(&entries(), query)
            .into_iter()
            .map(|entry| entry.label.to_string())
            .collect()
    }

    #[test]
    fn an_empty_query_keeps_the_given_order() {
        // With nothing typed the palette is a menu, and a menu that reorders
        // itself is not a menu.
        assert_eq!(labels("").len(), 6);
        assert_eq!(labels("")[0], "New request");
    }

    #[test]
    fn a_label_prefix_ranks_above_a_detail_match() {
        // "to" starts "Toggle console" and appears inside "/arm/move_to", so
        // both match — but only one of them is what was meant.
        let found = labels("to");
        assert_eq!(found, vec!["Toggle console", "Arm home"]);
    }

    #[test]
    fn a_word_inside_the_label_matches() {
        let found = labels("console");
        assert_eq!(found, vec!["Toggle console"]);
    }

    #[test]
    fn a_request_is_found_by_its_target() {
        let found = labels("cmd_vel");
        assert_eq!(found, vec!["Drive forward"]);
    }

    #[test]
    fn a_connection_is_found_by_its_url() {
        let found = labels("8765");
        assert_eq!(found, vec!["Robot"]);
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert_eq!(labels("ARM"), vec!["Arm home"]);
    }

    #[test]
    fn nothing_matches_nonsense() {
        assert!(labels("zzzz").is_empty());
    }

    #[test]
    fn a_label_match_outranks_a_detail_match() {
        let entries = vec![
            Entry::new("Requests", "Something else", Choice::Request(1)).detail("arm"),
            Entry::new("Requests", "Arm home", Choice::Request(2)),
        ];
        let found: Vec<_> = search(&entries, "arm")
            .into_iter()
            .map(|entry| entry.label.to_string())
            .collect();
        assert_eq!(found, vec!["Arm home", "Something else"]);
    }
}
