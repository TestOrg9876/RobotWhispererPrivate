//! Capturing what arrives, so it can be played back later.
//!
//! Recording taps the request editors rather than the pipeline: what a person
//! means by "record this session" is the topics they are watching, and those
//! are exactly the ones with a subscription open.
//!
//! Playback is not here. A recording is opened as a `TransportConfig::Replay`
//! connection and arrives through the same pipeline as a robot, so every pane
//! works on it unchanged.

use std::sync::{Arc, Mutex};

use gpui::Context;
use rw_canonical::CanonicalValue;
use rw_record::{Recording, Topic};

/// How many messages one recording may hold.
///
/// A camera at 30 Hz fills this in about twenty minutes. The cap is here
/// because the whole recording is in memory and a session left running
/// overnight should not take the app down with it.
pub const MAX_MESSAGES: usize = 50_000;

/// The part a subscription callback writes to.
///
/// Frames arrive on whichever thread the transport decodes on, so the tap has
/// to be shareable and must not touch a GPUI entity. The entity holds the same
/// handle and reads through it.
#[derive(Default)]
struct Live {
    recording: Recording,
    started_ns: u64,
    /// Set when the cap was reached, so the UI can say why it stopped.
    full: bool,
}

/// A handle a subscription holds for its whole life.
///
/// Deliberately not a snapshot of "are we recording": a subscription opened
/// before the record button was pressed still has to be captured, or recording
/// would only ever catch topics subscribed afterwards.
#[derive(Clone)]
pub struct Tap(Arc<Mutex<Option<Live>>>);

impl Tap {
    /// Records one message. Does nothing when recording is off.
    ///
    /// Takes the schema by parts rather than a `CanonicalSchema` so nothing has
    /// to be kept alive: the recording only needs the name and the text.
    pub fn observe(
        &self,
        topic: &str,
        schema_name: &str,
        schema_definition: Option<&str>,
        value: &CanonicalValue,
    ) {
        let Ok(mut state) = self.0.lock() else { return };
        let Some(live) = state.as_mut() else { return };
        if live.recording.messages.len() >= MAX_MESSAGES {
            live.full = true;
            return;
        }
        let at_ns = now_ns().saturating_sub(live.started_ns);
        live.recording.push(
            at_ns,
            Topic {
                name: topic.to_string(),
                schema_name: schema_name.to_string(),
                schema_definition: schema_definition.map(str::to_string),
            },
            value.clone(),
        );
    }
}

pub struct Recorder {
    /// Holds a [`Live`] while recording. Shared with every [`Tap`].
    state: Arc<Mutex<Option<Live>>>,
    /// The last recording stopped, kept so it can be saved or replayed.
    finished: Option<Recording>,
    /// Set when the cap was reached, so the UI can say why it stopped.
    full: bool,
}

impl Default for Recorder {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(None)),
            finished: None,
            full: false,
        }
    }
}

impl Recorder {
    pub fn is_recording(&self) -> bool {
        self.state
            .lock()
            .map(|state| state.is_some())
            .unwrap_or(false)
    }

    pub fn is_full(&self) -> bool {
        self.full
    }

    /// The handle a subscription holds for as long as it is open.
    pub fn tap(&self) -> Tap {
        Tap(Arc::clone(&self.state))
    }

    /// Reads something out of the live recording, or the last finished one.
    fn measure<T: Default>(
        &self,
        of_live: impl Fn(&Recording) -> T,
        of_finished: impl Fn(&Recording) -> T,
    ) -> T {
        if let Ok(state) = self.state.lock()
            && let Some(live) = state.as_ref()
        {
            return of_live(&live.recording);
        }
        self.finished.as_ref().map(of_finished).unwrap_or_default()
    }

    /// How many messages have been captured, live or last time.
    pub fn count(&self) -> usize {
        self.measure(
            |recording| recording.messages.len(),
            |recording| recording.messages.len(),
        )
    }

    /// How long the current or last recording runs, in seconds.
    pub fn seconds(&self) -> f64 {
        self.measure(
            |recording| recording.duration_ns() as f64 / 1e9,
            |recording| recording.duration_ns() as f64 / 1e9,
        )
    }

    pub fn finished(&self) -> Option<&Recording> {
        self.finished.as_ref()
    }

    pub fn start(&mut self, name: impl Into<String>, cx: &mut Context<Self>) {
        if let Ok(mut state) = self.state.lock() {
            *state = Some(Live {
                recording: Recording::new(name),
                started_ns: now_ns(),
                full: false,
            });
        }
        self.full = false;
        cx.notify();
    }

    /// Stops, keeping what was captured. Returns false if there was nothing.
    pub fn stop(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(live) = self.state.lock().ok().and_then(|mut state| state.take()) else {
            return false;
        };
        self.full = live.full;
        let captured = !live.recording.is_empty();
        if captured {
            self.finished = Some(live.recording);
        }
        cx.notify();
        captured
    }
}

/// A monotonic-enough clock that exists on both targets.
///
/// `std::time::Instant` does not work on `wasm32-unknown-unknown`; `rw_wire`
/// already cfg-switches this and every crate here uses it rather than reaching
/// for `std::time` directly.
fn now_ns() -> u64 {
    rw_transport::perf::now_ns()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_recorder_is_not_recording_and_has_nothing() {
        let recorder = Recorder::default();
        assert!(!recorder.is_recording());
        assert_eq!(recorder.count(), 0);
        assert_eq!(recorder.seconds(), 0.);
        assert!(recorder.finished().is_none());
    }

    #[test]
    fn a_tap_costs_nothing_while_recording_is_off() {
        // Every subscription holds one of these for its whole life, so the
        // off case has to be free of consequences as well as cheap.
        let recorder = Recorder::default();
        let tap = recorder.tap();
        for _ in 0..1000 {
            tap.observe("/t", "std_msgs/Int64", None, &CanonicalValue::Uint(1));
        }
        assert_eq!(recorder.count(), 0);
        assert!(!recorder.is_recording());
    }
}
