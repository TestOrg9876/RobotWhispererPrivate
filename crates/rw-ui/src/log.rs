//! What the robot itself has to say.
//!
//! Every ROS 2 node writes to `/rosout` as `rcl_interfaces/Log`, and it is the
//! first place anyone looks when something did not happen — the console the
//! app already keeps is the second. There is no reason for those to be two
//! windows, so a robot's log lines arrive on the same route as the app's own
//! and land in the same place, in one order.
//!
//! Pure decoding, in the shape of `cloud.rs` and `tf.rs`.

use rw_canonical::CanonicalValue;

use crate::geometry;

/// The topic every ROS 2 graph puts its log on.
pub const TOPIC: &str = "/rosout";

/// The severities `rcl_interfaces/Log` defines, by their own numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
    Fatal,
}

impl Level {
    /// The constants the message itself carries: 10, 20, 30, 40, 50.
    ///
    /// Read as a range rather than as five exact values, because ROS 1's
    /// `rosgraph_msgs/Log` numbers the same five levels 1, 2, 4, 8, 16 and a
    /// bridge is entitled to pass either through.
    pub fn from_code(code: i64) -> Option<Self> {
        Some(match code {
            ..=0 => return None,
            1..=10 => Self::Debug,
            11..=20 => Self::Info,
            21..=30 => Self::Warn,
            31..=40 => Self::Error,
            _ => Self::Fatal,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
            Self::Fatal => "FATAL",
        }
    }
}

/// One line the robot wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub level: Level,
    /// The node that said it. Empty when the message did not name one.
    pub node: String,
    pub message: String,
}

impl Entry {
    /// How the line reads in the console.
    ///
    /// The node first, because with twenty nodes running, *which one* is the
    /// question a person is answering when they read this at all.
    pub fn text(&self) -> String {
        match self.node.is_empty() {
            true => format!("[{}] {}", self.level.label(), self.message),
            false => format!("[{}] {}: {}", self.level.label(), self.node, self.message),
        }
    }
}

/// Reads an `rcl_interfaces/Log`, if this is one.
pub fn decode(value: &CanonicalValue) -> Option<Entry> {
    let level = Level::from_code(geometry::whole(value.get_path("level")?)?)?;
    // `msg` in ROS 2 and ROS 1 alike; a bridge that renamed it is not a bridge
    // this can read, and guessing which other field held the text would be
    // worse than declining.
    let message = geometry::text(value.get_path("msg")?)?;
    Some(Entry {
        level,
        node: value
            .get_path("name")
            .and_then(geometry::text)
            .unwrap_or_default(),
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn map<const N: usize>(fields: [(&str, CanonicalValue); N]) -> CanonicalValue {
        CanonicalValue::Struct(
            fields
                .into_iter()
                .map(|(name, value)| (name.to_string(), value))
                .collect::<BTreeMap<_, _>>(),
        )
    }

    fn entry(level: i64) -> CanonicalValue {
        map([
            ("level", CanonicalValue::Int(level)),
            ("name", CanonicalValue::String("planner".into())),
            ("msg", CanonicalValue::String("goal reached".into())),
        ])
    }

    #[test]
    fn a_log_message_decodes() {
        let decoded = decode(&entry(20)).expect("decodes");
        assert_eq!(decoded.level, Level::Info);
        assert_eq!(decoded.node, "planner");
        assert_eq!(decoded.message, "goal reached");
    }

    #[test]
    fn the_ros_2_level_constants_map_to_their_own_names() {
        for (code, level) in [
            (10, Level::Debug),
            (20, Level::Info),
            (30, Level::Warn),
            (40, Level::Error),
            (50, Level::Fatal),
        ] {
            assert_eq!(decode(&entry(code)).expect("decodes").level, level);
        }
    }

    #[test]
    fn the_ros_1_level_constants_map_to_the_same_five_names() {
        // rosgraph_msgs/Log numbers them 1, 2, 4, 8, 16, and a bridge is
        // entitled to pass either through.
        for (code, level) in [
            (1, Level::Debug),
            (2, Level::Debug),
            (4, Level::Debug),
            (8, Level::Debug),
            (16, Level::Info),
        ] {
            assert_eq!(
                decode(&entry(code)).expect("decodes").level,
                level,
                "code {code}"
            );
        }
    }

    #[test]
    fn a_level_above_fatal_is_fatal_rather_than_refused() {
        assert_eq!(Level::from_code(99), Some(Level::Fatal));
    }

    #[test]
    fn a_level_of_zero_is_not_a_level() {
        assert_eq!(Level::from_code(0), None);
        assert_eq!(Level::from_code(-1), None);
        assert_eq!(decode(&entry(0)), None);
    }

    #[test]
    fn a_message_that_is_not_a_log_is_refused() {
        assert_eq!(decode(&map([("data", CanonicalValue::Int(1))])), None);
        assert_eq!(
            decode(&map([("level", CanonicalValue::Int(20))])),
            None,
            "a level with no message is not a log line"
        );
    }

    #[test]
    fn a_line_with_no_node_still_reads() {
        let decoded = decode(&map([
            ("level", CanonicalValue::Int(30)),
            ("msg", CanonicalValue::String("watch out".into())),
        ]))
        .expect("decodes");
        assert_eq!(decoded.node, "");
        assert_eq!(decoded.text(), "[WARN] watch out");
    }

    #[test]
    fn a_line_names_the_node_that_said_it() {
        // With twenty nodes running, which one is the question being answered.
        assert_eq!(
            decode(&entry(40)).expect("decodes").text(),
            "[ERROR] planner: goal reached"
        );
    }

    #[test]
    fn levels_order_from_quietest_to_loudest() {
        assert!(Level::Debug < Level::Info);
        assert!(Level::Info < Level::Warn);
        assert!(Level::Warn < Level::Error);
        assert!(Level::Error < Level::Fatal);
    }
}
