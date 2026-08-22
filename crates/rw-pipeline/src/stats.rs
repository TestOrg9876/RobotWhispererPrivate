//! How fast a topic is going.
//!
//! `ros2 topic hz` is the most-run command in robotics, and every time someone
//! runs it they leave the tool they were already looking at to do it. So the
//! numbers are computed here, where every frame already passes: a rolling
//! window per subscription, giving a rate, a bandwidth and — when the clocks
//! allow it — a latency.
//!
//! Arrival time is passed in rather than read here, so the whole thing is pure
//! and a test can feed it a schedule instead of sleeping through one.

use std::collections::VecDeque;

use rw_transport::Frame;

/// How much history a meter keeps by default, in nanoseconds.
///
/// Five seconds: long enough that a 1 Hz topic gets a rate at all, short enough
/// that a topic which has just stopped says so rather than reporting the
/// average of the minute before it did. Settable per meter, because what counts
/// as "just stopped" is different for a 200 Hz IMU and a 0.2 Hz map update.
pub const WINDOW_NS: u64 = 5_000_000_000;

/// A hard ceiling on samples, whatever the window says — a 10 kHz topic would
/// otherwise keep fifty thousand of them.
pub const MAX_SAMPLES: usize = 1_024;

/// The largest latency worth believing, in nanoseconds.
///
/// A publisher's stamp and this machine's clock are two different clocks, and
/// on a robot they are routinely years apart — an unsynchronised bridge, a
/// simulator counting from zero, a bag replayed from 2019. A reading outside
/// this is a clock difference rather than a latency, and reporting it as a
/// latency would be worse than reporting nothing.
const PLAUSIBLE_LATENCY_NS: u64 = 60_000_000_000;

/// What a topic is doing, right now.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Stats {
    /// Messages per second across the window. `None` until two have arrived.
    pub hz: Option<f64>,
    /// Bytes per second across the window.
    ///
    /// `None` when the transport does not keep the encoded bytes — a bridge
    /// that hands over decoded JSON has no wire size to report, and inventing
    /// one from the decoded value would be a number about this program rather
    /// than about the robot.
    pub bytes_per_second: Option<f64>,
    /// The median of arrival minus the publisher's own stamp.
    ///
    /// `None` when the two clocks disagree by more than any latency could
    /// explain, which is most of the time on a real robot.
    pub latency_ns: Option<u64>,
    /// Every message since the subscription opened, not only those in the
    /// window.
    pub count: u64,
}

impl Stats {
    /// Whether there is anything here worth putting on screen.
    pub fn is_empty(&self) -> bool {
        self.hz.is_none() && self.bytes_per_second.is_none() && self.latency_ns.is_none()
    }

    /// The rate, as it should read beside a topic name.
    pub fn hz_label(&self) -> Option<String> {
        let hz = self.hz?;
        Some(match hz {
            hz if hz >= 100. => format!("{hz:.0} Hz"),
            hz if hz >= 10. => format!("{hz:.1} Hz"),
            _ => format!("{hz:.2} Hz"),
        })
    }

    /// The bandwidth, in the unit a person would have chosen.
    pub fn bandwidth_label(&self) -> Option<String> {
        let rate = self.bytes_per_second?;
        Some(match rate {
            rate if rate >= 1e6 => format!("{:.1} MB/s", rate / 1e6),
            rate if rate >= 1e3 => format!("{:.1} kB/s", rate / 1e3),
            rate => format!("{rate:.0} B/s"),
        })
    }

    pub fn latency_label(&self) -> Option<String> {
        let latency = self.latency_ns?;
        Some(match latency {
            ns if ns >= 1_000_000_000 => format!("{:.1} s", ns as f64 / 1e9),
            ns if ns >= 1_000_000 => format!("{:.0} ms", ns as f64 / 1e6),
            ns => format!("{:.0} µs", ns as f64 / 1e3),
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct Sample {
    arrived_ns: u64,
    /// The encoded size, when the transport kept it.
    bytes: Option<usize>,
    /// Arrival minus the publisher's stamp, when that was believable.
    latency_ns: Option<u64>,
}

/// A rolling window of arrivals on one subscription.
#[derive(Debug)]
pub struct Meter {
    samples: VecDeque<Sample>,
    count: u64,
    window_ns: u64,
}

impl Default for Meter {
    fn default() -> Self {
        Self::with_window(WINDOW_NS)
    }
}

impl Meter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_window(window_ns: u64) -> Self {
        Self {
            samples: VecDeque::new(),
            count: 0,
            window_ns: window_ns.max(1),
        }
    }

    /// Changes the window, dropping what no longer fits.
    ///
    /// Applied to the meters already running rather than only to the next
    /// subscription: a rate is what you are looking at right now, and one that
    /// waits for a resubscribe to honour the setting looks broken.
    pub fn set_window(&mut self, window_ns: u64) {
        self.window_ns = window_ns.max(1);
        if let Some(newest) = self.samples.back().map(|sample| sample.arrived_ns) {
            self.trim(newest);
        }
    }

    /// Records that a frame arrived at `arrived_ns`.
    pub fn observe(&mut self, arrived_ns: u64, frame: &Frame) {
        self.count = self.count.saturating_add(1);
        self.samples.push_back(Sample {
            arrived_ns,
            bytes: frame.raw.as_ref().map(|raw| raw.len()),
            latency_ns: latency(arrived_ns, frame.timestamp_ns),
        });
        self.trim(arrived_ns);
    }

    fn trim(&mut self, now_ns: u64) {
        let horizon = now_ns.saturating_sub(self.window_ns);
        while self
            .samples
            .front()
            .is_some_and(|sample| sample.arrived_ns < horizon)
        {
            self.samples.pop_front();
        }
        while self.samples.len() > MAX_SAMPLES {
            self.samples.pop_front();
        }
    }

    pub fn count(&self) -> u64 {
        self.count
    }

    /// What the window says, as of `now_ns`.
    ///
    /// `now_ns` rather than the last arrival, so a topic that has stopped
    /// reports a falling rate and then nothing — a rate frozen at whatever it
    /// was when the robot went away is the one number nobody wants.
    pub fn stats(&self, now_ns: u64) -> Stats {
        let mut stats = Stats {
            count: self.count,
            ..Stats::default()
        };
        let live: Vec<&Sample> = self
            .samples
            .iter()
            .filter(|sample| sample.arrived_ns + self.window_ns >= now_ns)
            .collect();
        let Some(first) = live.first() else {
            return stats;
        };

        if let (Some(last), true) = (live.last(), live.len() >= 2) {
            // Completed intervals over the time they took. n arrivals bound
            // n−1 intervals, not n — counting the one still open at `now` as
            // if it had closed reads about 1/2(n−1) high, which is nothing at
            // 100 Hz and a quarter at 1 Hz. This is the estimator
            // `ros2 topic hz` uses, and agreeing with it matters more than any
            // refinement: a number that disagrees with the tool beside it is a
            // number nobody trusts.
            let intervals = (live.len() - 1) as f64;
            let closed_ns = last.arrived_ns.saturating_sub(first.arrived_ns);

            // The open interval counts only once it has outlasted a normal
            // one. A topic keeping pace reads its true rate; one that has gone
            // quiet reads a falling one rather than staying frozen at whatever
            // it was when the robot went away.
            let mean_ns = (closed_ns as f64 / intervals) as u64;
            let overdue_ns = now_ns
                .saturating_sub(last.arrived_ns)
                .saturating_sub(mean_ns);

            let measured_ns = closed_ns.saturating_add(overdue_ns).max(1);
            let measured = measured_ns as f64 / 1e9;
            stats.hz = Some(intervals / measured);

            // The same n−1 messages over the same span, so bandwidth is the
            // rate times the mean message size — which is the arithmetic
            // anyone reading both numbers will do in their head.
            if live.iter().any(|sample| sample.bytes.is_some()) {
                let sized: usize = live.iter().skip(1).filter_map(|sample| sample.bytes).sum();
                stats.bytes_per_second = Some(sized as f64 / measured);
            }
        }

        let mut latencies: Vec<u64> = live.iter().filter_map(|sample| sample.latency_ns).collect();
        if !latencies.is_empty() {
            // The median rather than the mean: one frame that waited behind a
            // garbage collection should not move the reading.
            latencies.sort_unstable();
            stats.latency_ns = Some(latencies[latencies.len() / 2]);
        }

        stats
    }
}

/// Arrival minus the publisher's stamp, when the two clocks are close enough
/// for that difference to mean anything.
fn latency(arrived_ns: u64, stamped_ns: u64) -> Option<u64> {
    // An unstamped message — the dummy transport's, a bridge that dropped the
    // header — is not a message that arrived instantly.
    if stamped_ns == 0 || arrived_ns < stamped_ns {
        return None;
    }
    let latency = arrived_ns - stamped_ns;
    (latency <= PLAUSIBLE_LATENCY_NS).then_some(latency)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use rw_canonical::{
        CanonicalSchema, CanonicalValue, Dialect, MessageDef, ParsedSchema, SchemaKind,
        VisualizationRole,
    };

    const MS: u64 = 1_000_000;
    const SECOND: u64 = 1_000_000_000;

    fn schema() -> Arc<CanonicalSchema> {
        Arc::new(CanonicalSchema {
            id: "test".into(),
            name: "test/Msg".into(),
            kind: SchemaKind::Message,
            dialect: Dialect::Custom("test".into()),
            definition: String::new(),
            parsed: ParsedSchema::Message(MessageDef {
                fields: vec![],
                constants: vec![],
            }),
            dependencies: vec![],
            viz_role: VisualizationRole::default(),
        })
    }

    fn frame(stamped_ns: u64, bytes: Option<usize>) -> Frame {
        Frame {
            timestamp_ns: stamped_ns,
            schema: schema(),
            value: CanonicalValue::Null,
            raw: bytes.map(|len| Arc::<[u8]>::from(vec![0u8; len])),
            perf: None,
        }
    }

    /// Feeds `count` frames `period_ns` apart, starting at `start_ns`.
    fn steady(
        meter: &mut Meter,
        start_ns: u64,
        period_ns: u64,
        count: usize,
        bytes: Option<usize>,
    ) {
        for index in 0..count {
            let at = start_ns + index as u64 * period_ns;
            meter.observe(at, &frame(0, bytes));
        }
    }

    #[test]
    fn a_topic_at_ten_hertz_reads_as_ten_hertz() {
        let mut meter = Meter::new();
        steady(&mut meter, SECOND, 100 * MS, 41, None);
        let hz = meter
            .stats(SECOND + 4000 * MS)
            .hz
            .expect("two samples is a rate");
        assert!((hz - 10.).abs() < 0.01, "got {hz} Hz");
    }

    #[test]
    fn a_topic_at_one_hertz_reads_as_one_hertz() {
        // The tolerance is tight on purpose. The old estimator counted the
        // still-open interval as a message and read 1.25 Hz here — inside a
        // loose tolerance, and 25% wrong.
        let mut meter = Meter::new();
        steady(&mut meter, SECOND, SECOND, 5, None);
        let hz = meter.stats(SECOND + 4 * SECOND).hz.expect("a rate");
        assert!((hz - 1.).abs() < 0.01, "got {hz} Hz");
    }

    #[test]
    fn a_rate_does_not_jump_while_waiting_for_the_next_message() {
        // Between two arrivals of a 10 Hz topic the reading should stay 10 Hz,
        // not climb as the gap since the last one grows.
        let mut meter = Meter::new();
        steady(&mut meter, SECOND, 100 * MS, 20, None);
        let last = SECOND + 1900 * MS;
        for wait in [0, 20 * MS, 50 * MS, 90 * MS] {
            let hz = meter.stats(last + wait).hz.expect("a rate");
            assert!(
                (hz - 10.).abs() < 0.01,
                "at {wait} ns into the gap the rate read {hz} Hz"
            );
        }
    }

    #[test]
    fn the_rate_matches_the_way_ros2_topic_hz_computes_it() {
        // n arrivals bound n−1 intervals. Anyone checking this app against the
        // command line is comparing against exactly this number.
        let mut meter = Meter::new();
        let arrivals = [0, 90 * MS, 210 * MS, 300 * MS, 405 * MS];
        for at in arrivals {
            meter.observe(SECOND + at, &frame(0, None));
        }
        let expected = 4. / 0.405;
        let hz = meter.stats(SECOND + 405 * MS).hz.expect("a rate");
        assert!(
            (hz - expected).abs() < 0.01,
            "got {hz} Hz, ros2 topic hz would say {expected}"
        );
    }

    #[test]
    fn one_message_is_not_a_rate() {
        let mut meter = Meter::new();
        meter.observe(SECOND, &frame(0, None));
        let stats = meter.stats(SECOND);
        assert_eq!(
            stats.hz, None,
            "a rate needs two arrivals to measure between"
        );
        assert_eq!(stats.count, 1, "but the message still counted");
    }

    #[test]
    fn a_topic_that_stopped_reports_a_falling_rate_and_then_nothing() {
        // The number nobody wants is a rate frozen at whatever it was when the
        // robot went away.
        let mut meter = Meter::new();
        steady(&mut meter, SECOND, 100 * MS, 10, None);
        let last = SECOND + 900 * MS;

        let live = meter.stats(last).hz.expect("still going");
        let stalling = meter.stats(last + 2 * SECOND).hz.expect("still in window");
        assert!(
            stalling < live / 2.,
            "{stalling} should be well under {live}"
        );
        assert_eq!(
            meter.stats(last + 10 * SECOND).hz,
            None,
            "everything has fallen out of the window"
        );
    }

    #[test]
    fn samples_older_than_the_window_are_dropped() {
        let mut meter = Meter::new();
        steady(&mut meter, 0, 100 * MS, 200, None);
        // 200 samples 100 ms apart is 20 s of history in a 5 s window.
        assert!(
            meter.samples.len() <= 51,
            "kept {} samples",
            meter.samples.len()
        );
        assert_eq!(meter.count(), 200, "the true count survives the trimming");
    }

    #[test]
    fn a_wider_window_keeps_what_the_default_would_have_dropped() {
        let mut meter = Meter::with_window(30 * SECOND);
        steady(&mut meter, 0, 100 * MS, 200, None);
        // The same 20 s of history the default window trimmed to about 5 s.
        assert!(
            meter.samples.len() > 150,
            "a 30 s window should have kept nearly all of it, kept {}",
            meter.samples.len()
        );
    }

    #[test]
    fn shortening_the_window_drops_the_arrivals_outside_it() {
        let mut meter = Meter::with_window(30 * SECOND);
        steady(&mut meter, 0, 100 * MS, 200, None);
        let wide = meter.samples.len();

        meter.set_window(SECOND);
        assert!(
            meter.samples.len() < wide,
            "narrowing the window should have trimmed, still {} samples",
            meter.samples.len()
        );
        assert!(
            meter.samples.len() <= 11,
            "a 1 s window over a 10 Hz topic, got {}",
            meter.samples.len()
        );
        assert_eq!(meter.count(), 200, "the true count survives the trimming");
    }

    #[test]
    fn a_flood_is_capped_even_inside_the_window() {
        let mut meter = Meter::new();
        // Everything at the same instant, so the window never trims it.
        for _ in 0..MAX_SAMPLES + 500 {
            meter.observe(SECOND, &frame(0, None));
        }
        assert_eq!(meter.samples.len(), MAX_SAMPLES);
    }

    #[test]
    fn bandwidth_follows_the_bytes_that_actually_arrived() {
        let mut meter = Meter::new();
        // Ten frames of 1000 bytes, 100 ms apart: 10 kB/s.
        steady(&mut meter, SECOND, 100 * MS, 10, Some(1000));
        let rate = meter
            .stats(SECOND + 1000 * MS)
            .bytes_per_second
            .expect("bytes were kept");
        assert!((rate - 10_000.).abs() < 10., "got {rate} B/s");
    }

    #[test]
    fn bandwidth_is_the_rate_times_the_message_size() {
        // The arithmetic anyone reading both numbers does in their head. If
        // these two disagree, one of them is wrong and neither gets believed.
        let mut meter = Meter::new();
        steady(&mut meter, SECOND, 100 * MS, 30, Some(4096));
        let stats = meter.stats(SECOND + 2950 * MS);
        let hz = stats.hz.expect("a rate");
        let bytes = stats.bytes_per_second.expect("a bandwidth");
        assert!(
            (bytes - hz * 4096.).abs() < 1.,
            "{bytes} B/s is not {hz} Hz of 4096-byte messages"
        );
    }

    #[test]
    fn a_transport_that_keeps_no_bytes_reports_no_bandwidth() {
        // A bridge that hands over decoded JSON has no wire size, and a number
        // invented from the decoded value would be about this program rather
        // than about the robot.
        let mut meter = Meter::new();
        steady(&mut meter, SECOND, 100 * MS, 10, None);
        assert_eq!(meter.stats(SECOND + SECOND).bytes_per_second, None);
    }

    #[test]
    fn latency_is_the_gap_between_the_stamp_and_the_arrival() {
        let mut meter = Meter::new();
        for index in 0..5u64 {
            let stamped = SECOND + index * 100 * MS;
            meter.observe(stamped + 20 * MS, &frame(stamped, None));
        }
        assert_eq!(meter.stats(SECOND + 500 * MS).latency_ns, Some(20 * MS));
    }

    #[test]
    fn one_slow_frame_does_not_move_the_latency() {
        // The median, not the mean: a frame that waited behind a garbage
        // collection is not what the link is doing.
        let mut meter = Meter::new();
        for index in 0..9u64 {
            let stamped = SECOND + index * 100 * MS;
            let delay = if index == 4 { 900 * MS } else { 10 * MS };
            meter.observe(stamped + delay, &frame(stamped, None));
        }
        assert_eq!(meter.stats(SECOND + 900 * MS).latency_ns, Some(10 * MS));
    }

    #[test]
    fn a_clock_that_disagrees_by_years_reports_no_latency_rather_than_years() {
        // A simulator counting from zero, a bag replayed from 2019: the
        // difference is a clock offset and calling it a latency would be worse
        // than saying nothing.
        assert_eq!(latency(1_700_000_000 * SECOND, 5 * SECOND), None);
        assert_eq!(latency(SECOND, 0), None, "unstamped is not instant");
        assert_eq!(
            latency(SECOND, 2 * SECOND),
            None,
            "a stamp from the future is a clock, not a negative latency"
        );
        assert_eq!(latency(SECOND + 30 * MS, SECOND), Some(30 * MS));
    }

    #[test]
    fn an_untouched_meter_has_nothing_to_show() {
        let stats = Meter::new().stats(SECOND);
        assert!(stats.is_empty());
        assert_eq!(stats.count, 0);
    }

    #[test]
    fn the_labels_pick_the_unit_a_person_would_have_chosen() {
        let stats = |hz, bytes, latency| Stats {
            hz: Some(hz),
            bytes_per_second: Some(bytes),
            latency_ns: Some(latency),
            count: 0,
        };
        assert_eq!(stats(999.4, 0., 0).hz_label().as_deref(), Some("999 Hz"));
        assert_eq!(stats(29.97, 0., 0).hz_label().as_deref(), Some("30.0 Hz"));
        assert_eq!(stats(0.5, 0., 0).hz_label().as_deref(), Some("0.50 Hz"));

        assert_eq!(
            stats(1., 2_500_000., 0).bandwidth_label().as_deref(),
            Some("2.5 MB/s")
        );
        assert_eq!(
            stats(1., 4_096., 0).bandwidth_label().as_deref(),
            Some("4.1 kB/s")
        );
        assert_eq!(
            stats(1., 80., 0).bandwidth_label().as_deref(),
            Some("80 B/s")
        );

        assert_eq!(
            stats(1., 0., 30 * MS).latency_label().as_deref(),
            Some("30 ms")
        );
        assert_eq!(stats(1., 0., 400).latency_label().as_deref(), Some("0 µs"));
        assert_eq!(
            stats(1., 0., 3 * SECOND).latency_label().as_deref(),
            Some("3.0 s")
        );
    }
}
