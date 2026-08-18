//! Ranking discovered topics, services and actions against what is typed.
//!
//! Discovery is autocomplete, not navigation: a robot advertises hundreds of
//! names, and the useful question is "which of these did you mean", not "show me
//! the tree". The target field stays free text — a name can be valid before it is
//! advertised — so this only ever offers, never constrains.

use rw_core::domain::RequestKind;
use rw_transport::Discovery;

/// How well a candidate matched, best first. The ordering is the point, so the
/// variants are declared in rank order and compared by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rank {
    /// The query is the whole name.
    Exact,
    /// The name starts with the query — `/dia` for `/diagnostics`.
    Prefix,
    /// A path segment starts with the query — `diag` for `/robot/diagnostics`.
    Segment,
    /// The query appears anywhere in the name.
    Contains,
}

/// One offer: the name to insert, and the schema that names what it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub name: String,
    pub schema: String,
}

/// The best `limit` matches for `query` among the names of `kind`.
///
/// An empty query offers everything, which is what makes the field browsable
/// with no typing — the closest thing to the tree it replaces.
pub fn suggestions(
    discovery: &Discovery,
    kind: RequestKind,
    query: &str,
    limit: usize,
) -> Vec<Suggestion> {
    let query = query.trim().to_lowercase();

    let mut ranked: Vec<_> = candidates(discovery, kind)
        .filter_map(|(name, schema)| {
            rank(name, &query).map(|rank| (rank, name.len(), name, schema))
        })
        .collect();

    // Rank, then shorter, then alphabetical: a stable order matters as much as a
    // clever one, because the list is navigated by arrow key.
    ranked.sort_unstable_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then(left.1.cmp(&right.1))
            .then(left.2.cmp(right.2))
    });
    ranked.dedup_by(|left, right| left.2 == right.2);

    ranked
        .into_iter()
        .take(limit)
        .map(|(_, _, name, schema)| Suggestion {
            name: name.to_string(),
            schema: schema.to_string(),
        })
        .collect()
}

fn candidates(
    discovery: &Discovery,
    kind: RequestKind,
) -> Box<dyn Iterator<Item = (&str, &str)> + '_> {
    match kind {
        RequestKind::Topic => Box::new(
            discovery
                .topics
                .iter()
                .map(|topic| (topic.name.as_str(), topic.schema_name.as_str())),
        ),
        RequestKind::Service => Box::new(
            discovery
                .services
                .iter()
                .map(|service| (service.name.as_str(), service.schema_name.as_str())),
        ),
        RequestKind::Action => Box::new(
            discovery
                .actions
                .iter()
                .map(|action| (action.name.as_str(), action.schema_name.as_str())),
        ),
    }
}

fn rank(name: &str, query: &str) -> Option<Rank> {
    if query.is_empty() {
        return Some(Rank::Prefix);
    }
    let lower = name.to_lowercase();
    if lower == query {
        return Some(Rank::Exact);
    }
    if lower.starts_with(query) {
        return Some(Rank::Prefix);
    }
    if lower
        .split('/')
        .any(|segment| !segment.is_empty() && segment.starts_with(query))
    {
        return Some(Rank::Segment);
    }
    lower.contains(query).then_some(Rank::Contains)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rw_transport::{TargetDescriptor, TopicDescriptor};

    fn topic(name: &str, schema: &str) -> TopicDescriptor {
        TopicDescriptor {
            name: name.into(),
            schema_name: schema.into(),
            schema_id: None,
            schema_definition: None,
        }
    }

    fn target(name: &str, schema: &str) -> TargetDescriptor {
        TargetDescriptor {
            name: name.into(),
            schema_name: schema.into(),
            schema_id: None,
            schema_definition: None,
        }
    }

    fn discovery() -> Discovery {
        Discovery {
            topics: vec![
                topic("/scan", "sensor_msgs/LaserScan"),
                topic("/odom", "nav_msgs/Odometry"),
                topic("/cmd_vel", "geometry_msgs/Twist"),
                topic("/robot/diagnostics", "diagnostic_msgs/DiagnosticArray"),
                topic("/diagnostics", "diagnostic_msgs/DiagnosticArray"),
            ],
            services: vec![target("/add_two_ints", "example_interfaces/AddTwoInts")],
            actions: vec![target("/fibonacci", "example_interfaces/Fibonacci")],
            ..Default::default()
        }
    }

    fn names(kind: RequestKind, query: &str) -> Vec<String> {
        suggestions(&discovery(), kind, query, 20)
            .into_iter()
            .map(|suggestion| suggestion.name)
            .collect()
    }

    #[test]
    fn an_empty_query_offers_everything_of_that_kind() {
        assert_eq!(names(RequestKind::Topic, "").len(), 5);
        assert_eq!(names(RequestKind::Service, ""), vec!["/add_two_ints"]);
        assert_eq!(names(RequestKind::Action, ""), vec!["/fibonacci"]);
    }

    #[test]
    fn kinds_do_not_leak_into_each_other() {
        // Subscribing to a service name is not a thing, so it must not be offered.
        assert!(!names(RequestKind::Topic, "add").contains(&"/add_two_ints".to_string()));
        assert!(names(RequestKind::Service, "scan").is_empty());
    }

    #[test]
    fn a_prefix_match_outranks_a_segment_match() {
        // "/diagnostics" starts with the query; "/robot/diagnostics" only has a
        // segment that does, so it comes second.
        assert_eq!(
            names(RequestKind::Topic, "/diag"),
            vec!["/diagnostics", "/robot/diagnostics"]
        );
    }

    #[test]
    fn a_segment_match_finds_a_name_nested_under_a_namespace() {
        assert_eq!(
            names(RequestKind::Topic, "diagnostics"),
            vec!["/diagnostics", "/robot/diagnostics"]
        );
    }

    #[test]
    fn an_exact_match_comes_first() {
        let ranked = names(RequestKind::Topic, "/diagnostics");
        assert_eq!(ranked.first().map(String::as_str), Some("/diagnostics"));
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert_eq!(names(RequestKind::Topic, "SCAN"), vec!["/scan"]);
    }

    #[test]
    fn a_substring_still_matches_when_no_boundary_does() {
        assert_eq!(names(RequestKind::Topic, "md_v"), vec!["/cmd_vel"]);
    }

    #[test]
    fn nothing_matches_a_name_that_is_not_there() {
        assert!(names(RequestKind::Topic, "/nope").is_empty());
    }

    #[test]
    fn the_limit_is_respected() {
        assert_eq!(
            suggestions(&discovery(), RequestKind::Topic, "", 2).len(),
            2
        );
    }

    #[test]
    fn ties_are_broken_by_length_then_name() {
        let discovery = Discovery {
            topics: vec![topic("/a/xx", "T"), topic("/b/xx", "T"), topic("/xx", "T")],
            ..Default::default()
        };
        let ranked: Vec<_> = suggestions(&discovery, RequestKind::Topic, "xx", 10)
            .into_iter()
            .map(|suggestion| suggestion.name)
            .collect();
        assert_eq!(ranked, vec!["/xx", "/a/xx", "/b/xx"]);
    }

    #[test]
    fn the_schema_travels_with_the_name() {
        let offered = suggestions(&discovery(), RequestKind::Topic, "/scan", 1);
        assert_eq!(offered[0].schema, "sensor_msgs/LaserScan");
    }

    #[test]
    fn whitespace_around_the_query_is_ignored() {
        assert_eq!(names(RequestKind::Topic, "  /scan  "), vec!["/scan"]);
    }
}
