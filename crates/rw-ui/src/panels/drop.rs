//! What a row dragged out of the sidebar means to a live pane.
//!
//! The sidebar already drags its rows — that is how requests are moved between
//! collections — so a pane that watches a topic needs nothing new to be dragged
//! *at*: it needs to know which drops mean something and what they mean.
//!
//! Only a topic request does. A pane subscribes, and there is nothing to
//! subscribe to in a service, an action goal or a node's parameters; a dragged
//! collection is a folder, not a target. Saying so here rather than in each
//! pane is what lets both panes light up for exactly the drops they will act
//! on — an impossible drop that looks possible until it is attempted is worse
//! than one that never lights up at all.

use rw_core::domain::{Request, RequestKind};

use crate::panels::collections::Dragged;
use crate::workspace::Workspace;

/// The connection and topic a dragged row would point a pane at.
///
/// The connection is optional because a request may not have been given one:
/// dropping it still says which topic is wanted, and a pane that already has a
/// connection keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub connection: Option<i64>,
    pub topic: String,
}

/// What this request means to a pane, if it means anything.
pub fn target_of(request: &Request) -> Option<Target> {
    if request.kind != RequestKind::Topic {
        return None;
    }
    let topic = request.target.trim();
    (!topic.is_empty()).then(|| Target {
        connection: request.connection_id,
        topic: topic.to_string(),
    })
}

/// The same, for whatever the sidebar is dragging.
pub fn target_of_drag(dragged: &Dragged, workspace: &Workspace) -> Option<Target> {
    match dragged {
        Dragged::Request { id, .. } => target_of(workspace.request(*id)?),
        Dragged::Collection { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone as _, Utc};
    use rw_core::domain::Value;

    fn request(kind: RequestKind, target: &str, connection: Option<i64>) -> Request {
        let at = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        Request {
            id: 1,
            collection_id: None,
            connection_id: connection,
            name: "A request".into(),
            kind,
            target: target.into(),
            schema: None,
            input: Value::empty_struct(),
            visualization: None,
            created_at: at,
            updated_at: at,
        }
    }

    #[test]
    fn a_topic_request_carries_its_topic_and_its_connection() {
        assert_eq!(
            target_of(&request(RequestKind::Topic, "/scan", Some(7))),
            Some(Target {
                connection: Some(7),
                topic: "/scan".into(),
            })
        );
    }

    /// A request that was never given an environment still names a topic, and
    /// a pane that already has one can use it.
    #[test]
    fn a_topic_request_with_no_environment_still_names_a_topic() {
        assert_eq!(
            target_of(&request(RequestKind::Topic, "/scan", None)),
            Some(Target {
                connection: None,
                topic: "/scan".into(),
            })
        );
    }

    #[test]
    fn nothing_a_pane_cannot_subscribe_to_is_droppable() {
        for kind in [
            RequestKind::Service,
            RequestKind::Action,
            RequestKind::Param,
        ] {
            assert_eq!(target_of(&request(kind, "/whatever", Some(1))), None);
        }
    }

    #[test]
    fn a_request_pointed_at_nothing_is_not_droppable() {
        assert_eq!(
            target_of(&request(RequestKind::Topic, "   ", Some(1))),
            None
        );
    }

    #[test]
    fn a_topic_is_taken_without_the_whitespace_around_it() {
        assert_eq!(
            target_of(&request(RequestKind::Topic, "  /scan  ", None))
                .map(|target| target.topic)
                .as_deref(),
            Some("/scan")
        );
    }
}
