//! What each request is currently doing.
//!
//! A request's own panel knows whether it is subscribed, calling or failed, but
//! the sidebar has to show it without owning the panels — so the state lives here
//! and both read it.

use std::collections::HashMap;

use gpui::{Context, SharedString};

/// A request's state, as far as anything outside its panel needs to know.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RunState {
    /// Never run, or finished cleanly and nothing to report.
    #[default]
    Idle,
    /// A subscription is open, a call is in flight, or a goal is running.
    Live,
    /// The last attempt failed, with the reason.
    Failed(SharedString),
}

impl RunState {
    /// What a hovering pointer should be told, if anything.
    pub fn tooltip(&self) -> Option<SharedString> {
        match self {
            RunState::Idle => None,
            RunState::Live => Some("Running".into()),
            RunState::Failed(reason) => Some(reason.clone()),
        }
    }
}

/// The run state of every request, keyed by request id.
///
/// Only the ones that are doing something are stored: a workspace of two hundred
/// idle requests holds nothing here.
#[derive(Default)]
pub struct Runs {
    states: HashMap<i64, RunState>,
}

impl Runs {
    pub fn get(&self, request: i64) -> RunState {
        self.states.get(&request).cloned().unwrap_or_default()
    }

    pub fn set(&mut self, request: i64, state: RunState, cx: &mut Context<Self>) {
        if state == RunState::Idle {
            self.states.remove(&request);
        } else {
            self.states.insert(request, state);
        }
        cx.notify();
    }

    /// Forgets a request, for when it is deleted.
    pub fn clear(&mut self, request: i64, cx: &mut Context<Self>) {
        if self.states.remove(&request).is_some() {
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_request_is_idle() {
        assert_eq!(Runs::default().get(7), RunState::Idle);
    }

    #[test]
    fn only_a_failure_carries_a_tooltip_reason() {
        assert_eq!(RunState::Idle.tooltip(), None);
        assert_eq!(RunState::Live.tooltip().as_deref(), Some("Running"));
        assert_eq!(
            RunState::Failed("no such topic".into())
                .tooltip()
                .as_deref(),
            Some("no such topic")
        );
    }
}
