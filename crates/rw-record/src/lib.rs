//! Recording what arrived on a topic, and playing it back.
//!
//! A recording is the frames themselves plus enough of each topic's schema to
//! rebuild it, so a file opened on another machine draws the same pictures as
//! the session that captured it. Nothing here talks to a transport or a UI: it
//! is a document format and a clock.

use std::collections::BTreeMap;

use rw_canonical::CanonicalValue;
use serde::{Deserialize, Serialize};

/// Bumped when the format changes in a way an older reader would misread.
pub const FORMAT_VERSION: u32 = 1;

/// What is known about a recorded topic, so playback can rebuild its schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Topic {
    pub name: String,
    pub schema_name: String,
    /// The schema text, when the transport gave one. Without it a replayed
    /// topic still carries values; it just cannot say what type they are.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_definition: Option<String>,
}

/// One captured message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// Nanoseconds since the recording started, so a file plays back at the
    /// rate it was captured whatever the wall clock said at the time.
    pub at_ns: u64,
    pub topic: String,
    pub value: CanonicalValue,
}

/// A whole recording.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Recording {
    #[serde(default)]
    pub version: u32,
    /// What the user called it, or the connection it came from.
    #[serde(default)]
    pub name: String,
    pub topics: Vec<Topic>,
    /// Ordered by `at_ns`. Every reader assumes this, so [`Recording::read`]
    /// enforces it rather than trusting the file.
    pub messages: Vec<Message>,
}

#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error("not a recording: {0}")]
    Malformed(#[from] serde_json::Error),
    #[error("recording was written by a newer version of the app ({0})")]
    TooNew(u32),
}

impl Recording {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            version: FORMAT_VERSION,
            name: name.into(),
            topics: Vec::new(),
            messages: Vec::new(),
        }
    }

    /// Adds a message, remembering its topic the first time one is seen.
    pub fn push(&mut self, at_ns: u64, topic: Topic, value: CanonicalValue) {
        if !self.topics.iter().any(|known| known.name == topic.name) {
            self.topics.push(topic.clone());
        }
        self.messages.push(Message {
            at_ns,
            topic: topic.name,
            value,
        });
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// How long the recording runs, in nanoseconds.
    pub fn duration_ns(&self) -> u64 {
        self.messages.last().map(|last| last.at_ns).unwrap_or(0)
    }

    /// How many messages each topic holds, for the summary line.
    pub fn counts(&self) -> BTreeMap<&str, usize> {
        let mut counts = BTreeMap::new();
        for message in &self.messages {
            *counts.entry(message.topic.as_str()).or_insert(0) += 1;
        }
        counts
    }

    pub fn topic(&self, name: &str) -> Option<&Topic> {
        self.topics.iter().find(|topic| topic.name == name)
    }

    pub fn write(&self) -> String {
        serde_json::to_string(self).expect("a recording always serialises")
    }

    /// Reads a recording, and puts it in the order every reader assumes.
    pub fn read(source: &str) -> Result<Self, RecordError> {
        let mut recording: Self = serde_json::from_str(source)?;
        if recording.version > FORMAT_VERSION {
            return Err(RecordError::TooNew(recording.version));
        }
        // A stable sort, so messages captured in the same nanosecond keep the
        // order they arrived in.
        recording.messages.sort_by_key(|message| message.at_ns);
        Ok(recording)
    }
}

/// Where playback has got to.
///
/// Kept apart from the recording so seeking, pausing and changing speed are
/// arithmetic on a few numbers rather than anything that touches the messages.
#[derive(Debug, Clone, PartialEq)]
pub struct Cursor {
    /// How far into the recording playback has reached, in nanoseconds.
    at_ns: u64,
    /// Index of the first message not yet delivered.
    next: usize,
    duration_ns: u64,
    /// 1.0 is the rate it was captured at.
    pub speed: f32,
    pub playing: bool,
    /// Whether to start again at the end.
    pub looping: bool,
}

impl Cursor {
    pub fn new(recording: &Recording) -> Self {
        Self {
            at_ns: 0,
            next: 0,
            duration_ns: recording.duration_ns(),
            speed: 1.,
            playing: false,
            looping: true,
        }
    }

    pub fn at_ns(&self) -> u64 {
        self.at_ns
    }

    pub fn duration_ns(&self) -> u64 {
        self.duration_ns
    }

    /// How far through, as 0..1. An empty recording reads as finished.
    pub fn progress(&self) -> f32 {
        if self.duration_ns == 0 {
            return 1.;
        }
        (self.at_ns as f32 / self.duration_ns as f32).clamp(0., 1.)
    }

    pub fn finished(&self) -> bool {
        self.at_ns >= self.duration_ns
    }

    /// Jumps to a point, as 0..1 of the whole.
    ///
    /// Everything strictly before the new point counts as already delivered, so
    /// a seek forward does not flush the skipped messages all at once — while a
    /// seek to the start still replays the message sitting at zero.
    pub fn seek(&mut self, progress: f32, recording: &Recording) {
        self.at_ns = (self.duration_ns as f32 * progress.clamp(0., 1.)) as u64;
        self.next = recording
            .messages
            .partition_point(|message| message.at_ns < self.at_ns);
    }

    pub fn rewind(&mut self) {
        self.at_ns = 0;
        self.next = 0;
    }

    /// Advances the clock and returns everything now due, in order.
    ///
    /// `elapsed_ns` is real time since the last call; the speed multiplier is
    /// applied here so the caller can tick on a fixed timer.
    pub fn advance<'a>(&mut self, elapsed_ns: u64, recording: &'a Recording) -> Vec<&'a Message> {
        if !self.playing || recording.is_empty() {
            return Vec::new();
        }
        let step = (elapsed_ns as f64 * self.speed.max(0.) as f64) as u64;
        self.at_ns = self.at_ns.saturating_add(step);

        let mut due = Vec::new();
        while let Some(message) = recording.messages.get(self.next) {
            if message.at_ns > self.at_ns {
                break;
            }
            due.push(message);
            self.next += 1;
        }

        if self.at_ns >= self.duration_ns && self.next >= recording.messages.len() {
            if self.looping {
                // Wrapped rather than reset to zero, so a long tick does not
                // silently lose the overshoot every time round.
                self.at_ns = self.at_ns.saturating_sub(self.duration_ns.max(1));
                self.next = 0;
            } else {
                self.at_ns = self.duration_ns;
                self.playing = false;
            }
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn topic(name: &str) -> Topic {
        Topic {
            name: name.into(),
            schema_name: "std_msgs/Int64".into(),
            schema_definition: Some("int64 data\n".into()),
        }
    }

    fn sample(count: u64) -> Recording {
        let mut recording = Recording::new("test");
        for index in 0..count {
            recording.push(
                index * 100_000_000,
                topic("/a"),
                CanonicalValue::Uint(index),
            );
        }
        recording
    }

    #[test]
    fn a_topic_is_remembered_once_however_many_messages_it_carries() {
        let recording = sample(5);
        assert_eq!(recording.topics.len(), 1);
        assert_eq!(recording.messages.len(), 5);
        assert_eq!(recording.counts()["/a"], 5);
    }

    #[test]
    fn duration_is_the_last_message() {
        assert_eq!(sample(5).duration_ns(), 400_000_000);
        assert_eq!(Recording::new("empty").duration_ns(), 0);
    }

    #[test]
    fn a_recording_survives_a_round_trip() {
        let recording = sample(3);
        let back = Recording::read(&recording.write()).expect("reads");
        assert_eq!(back, recording);
        assert_eq!(
            back.topic("/a")
                .and_then(|topic| topic.schema_definition.as_deref()),
            Some("int64 data\n")
        );
    }

    #[test]
    fn messages_are_put_in_order_rather_than_trusted() {
        let mut recording = sample(0);
        recording.push(300, topic("/a"), CanonicalValue::Uint(3));
        recording.push(100, topic("/a"), CanonicalValue::Uint(1));
        recording.push(200, topic("/a"), CanonicalValue::Uint(2));
        let back = Recording::read(&recording.write()).expect("reads");
        let times: Vec<u64> = back.messages.iter().map(|message| message.at_ns).collect();
        assert_eq!(times, [100, 200, 300]);
    }

    #[test]
    fn a_file_from_a_newer_version_is_refused_rather_than_misread() {
        let mut recording = sample(1);
        recording.version = FORMAT_VERSION + 1;
        assert!(matches!(
            Recording::read(&recording.write()),
            Err(RecordError::TooNew(_))
        ));
    }

    #[test]
    fn nonsense_is_refused() {
        assert!(matches!(
            Recording::read("not json"),
            Err(RecordError::Malformed(_))
        ));
    }

    #[test]
    fn a_paused_cursor_delivers_nothing() {
        let recording = sample(5);
        let mut cursor = Cursor::new(&recording);
        assert!(cursor.advance(1_000_000_000, &recording).is_empty());
        assert_eq!(cursor.at_ns(), 0);
    }

    #[test]
    fn playing_delivers_messages_as_their_time_arrives() {
        let recording = sample(5);
        let mut cursor = Cursor::new(&recording);
        cursor.playing = true;
        cursor.looping = false;

        // Nothing has elapsed past the first message's timestamp of zero, but
        // zero is due immediately.
        let due = cursor.advance(50_000_000, &recording);
        assert_eq!(due.len(), 1);
        // Crossing the next two timestamps delivers exactly two more.
        let due = cursor.advance(200_000_000, &recording);
        assert_eq!(due.len(), 2);
    }

    #[test]
    fn no_message_is_delivered_twice() {
        let recording = sample(20);
        let mut cursor = Cursor::new(&recording);
        cursor.playing = true;
        cursor.looping = false;
        let mut seen = Vec::new();
        for _ in 0..40 {
            seen.extend(
                cursor
                    .advance(100_000_000, &recording)
                    .into_iter()
                    .map(|message| message.at_ns),
            );
        }
        let mut unique = seen.clone();
        unique.dedup();
        assert_eq!(seen, unique);
        assert_eq!(seen.len(), 20, "every message arrived exactly once");
    }

    #[test]
    fn speed_scales_the_clock() {
        let recording = sample(11);
        let mut cursor = Cursor::new(&recording);
        cursor.playing = true;
        cursor.speed = 2.;
        let due = cursor.advance(250_000_000, &recording);
        // Half a second of recording in a quarter second of real time: the
        // messages at 0, 0.1, 0.2, 0.3, 0.4 and 0.5 are all due.
        assert_eq!(due.len(), 6);
    }

    #[test]
    fn a_stopped_clock_still_delivers_nothing_at_zero_speed() {
        let recording = sample(5);
        let mut cursor = Cursor::new(&recording);
        cursor.playing = true;
        cursor.speed = 0.;
        // The message at zero is due the moment playback starts, and nothing
        // after it ever becomes due.
        assert_eq!(cursor.advance(1_000_000_000, &recording).len(), 1);
        assert!(cursor.advance(1_000_000_000, &recording).is_empty());
    }

    #[test]
    fn seeking_forward_does_not_flush_everything_it_skipped() {
        let recording = sample(20);
        let mut cursor = Cursor::new(&recording);
        cursor.playing = true;
        cursor.seek(0.5, &recording);
        let due = cursor.advance(1, &recording);
        assert!(due.len() <= 1, "a seek delivered {} messages", due.len());
        assert!(cursor.progress() > 0.4 && cursor.progress() < 0.6);
    }

    #[test]
    fn seeking_back_replays_from_there() {
        let recording = sample(10);
        let mut cursor = Cursor::new(&recording);
        cursor.playing = true;
        cursor.advance(1_000_000_000, &recording);
        cursor.seek(0., &recording);
        assert_eq!(cursor.at_ns(), 0);
        assert_eq!(cursor.advance(1_000_000_000, &recording).len(), 10);
    }

    #[test]
    fn a_recording_that_is_not_looping_stops_at_the_end() {
        let recording = sample(5);
        let mut cursor = Cursor::new(&recording);
        cursor.playing = true;
        cursor.looping = false;
        cursor.advance(10_000_000_000, &recording);
        assert!(!cursor.playing);
        assert!(cursor.finished());
        assert_eq!(cursor.progress(), 1.);
    }

    #[test]
    fn a_looping_recording_starts_again() {
        let recording = sample(5);
        let mut cursor = Cursor::new(&recording);
        cursor.playing = true;
        cursor.advance(10_000_000_000, &recording);
        assert!(cursor.playing, "looping playback keeps going");
        assert!(!cursor.advance(500_000_000, &recording).is_empty());
    }

    #[test]
    fn an_empty_recording_plays_without_dividing_by_zero() {
        let recording = Recording::new("empty");
        let mut cursor = Cursor::new(&recording);
        cursor.playing = true;
        assert!(cursor.advance(1_000_000_000, &recording).is_empty());
        assert_eq!(cursor.progress(), 1.);
    }
}
