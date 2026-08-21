use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::domain::{RequestKind, Value};
use crate::ids::{ConnectionId, HistoryId, RequestId};

/// How a run ended.
///
/// Kept apart from the value because a failure has no value and an answer has
/// no reason, and a single nullable column would make every read of the table
/// guess which it was looking at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "lowercase")]
pub enum Outcome {
    /// It answered: a service's response, an action's result, a node's
    /// parameters.
    Answered,
    /// It did not, and this is what was said about why.
    Failed { reason: String },
}

impl Outcome {
    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }

    /// What to show beside the entry when there is no value to show.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Answered => None,
            Self::Failed { reason } => Some(reason),
        }
    }
}

/// One run of a request, kept after the fact.
///
/// A request panel holds the *current* response and nothing else: send again
/// and the previous answer is gone, close the tab and it goes with it. That is
/// fine for watching a topic and useless for what people actually do with a
/// service — call it, change one argument, call it again, and want to compare.
///
/// The input is stored beside the response because half of what makes an entry
/// worth keeping is what was *sent*: "that worked" is not a useful memory
/// without the arguments that made it work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: HistoryId,
    pub request_id: RequestId,
    /// Denormalised from the request, because history outlives edits: renaming
    /// a request or repointing it must not rewrite what you did yesterday.
    pub kind: RequestKind,
    pub target: String,
    pub connection_id: Option<ConnectionId>,
    pub at: DateTime<Utc>,
    pub outcome: Outcome,
    /// What was sent.
    pub input: Value,
    /// What came back. Absent on a failure.
    pub response: Option<Value>,
}

/// A run about to be recorded, before storage gives it an id.
#[derive(Debug, Clone, PartialEq)]
pub struct NewHistoryEntry {
    pub request_id: RequestId,
    pub kind: RequestKind,
    pub target: String,
    pub connection_id: Option<ConnectionId>,
    pub outcome: Outcome,
    pub input: Value,
    pub response: Option<Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn entry(outcome: Outcome, response: Option<Value>) -> HistoryEntry {
        HistoryEntry {
            id: 1,
            request_id: 7,
            kind: RequestKind::Service,
            target: "/dummy/add_two_ints".into(),
            connection_id: Some(2),
            at: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            outcome,
            input: Value::empty_struct(),
            response,
        }
    }

    #[test]
    fn an_entry_round_trips_through_json() {
        let answered = entry(Outcome::Answered, Some(Value::Int(42)));
        let json = serde_json::to_string(&answered).unwrap();
        assert_eq!(
            serde_json::from_str::<HistoryEntry>(&json).unwrap(),
            answered
        );
    }

    #[test]
    fn a_failure_round_trips_with_its_reason_and_no_value() {
        let failed = entry(
            Outcome::Failed {
                reason: "no such service".into(),
            },
            None,
        );
        let json = serde_json::to_string(&failed).unwrap();
        let back: HistoryEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back, failed);
        assert!(back.outcome.is_failure());
        assert_eq!(back.outcome.reason(), Some("no such service"));
        assert!(back.response.is_none());
    }

    #[test]
    fn an_answer_has_no_reason_and_a_failure_has_no_value() {
        assert_eq!(Outcome::Answered.reason(), None);
        assert!(!Outcome::Answered.is_failure());
    }
}
