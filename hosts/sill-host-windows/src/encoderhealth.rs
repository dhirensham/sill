//! When a stream that fell back to the software encoder goes back to the hardware one: the Mac
//! host's rule (CLAUDE.md, "The hardware encoder: busy, not stuck"), for Windows. Pure: tested
//! anywhere.
//!
//! A hardware encoder that keeps a frame for 1.5 s (win/stream.rs) is busy or stuck: another app
//! may be using the GPU's encoder, or the driver wedged. Seen on an XPS 13 on 2026-10-19: Intel's
//! HEVC MFT slowed from 26 to 0 frames a second over three seconds, two minutes into a 3840×2400
//! stream, while the same MFT carried the next stream at 55 fps. Either way the stream goes on with
//! the software encoder, at half scale and 60 fps at most (at full 4K it gave 2–5 fps), and every
//! stream after it starts there too, until a check finds the hardware keeping up: a short run of
//! frames at the size the stream would have on the hardware (win/probe.rs), held against
//! `return_bar`. The first check comes 30 s after the fallback; one that fails doubles the wait,
//! up to 300 s. A hang within 5 minutes of a return doubles it too (the busy spell is still on);
//! a return that lasted 10 minutes starts again from 30 s.

use std::time::{Duration, Instant};

/// The wait before the first check after a fallback.
pub const FIRST_INTERVAL: Duration = Duration::from_secs(30);
/// The longest wait between two checks.
pub const MAX_INTERVAL: Duration = Duration::from_secs(300);
/// A hang this soon after a return: the next check waits twice as long as the last.
pub const RETURN_DID_NOT_LAST: Duration = Duration::from_secs(300);
/// A return that lasted this long: the next fallback's first check comes after FIRST_INTERVAL again.
pub const RETURN_LASTED: Duration = Duration::from_secs(600);
/// The software encoder's share of the stream's capture scale.
pub const SOFTWARE_SCALE: f64 = 0.5;
/// The software encoder's highest frame rate.
pub const SOFTWARE_MAX_FPS: u32 = 60;
/// What a check must give of the stream's rate (and of the engine's own) for a return.
pub const SHARE: f64 = 0.75;
/// What a hardware encoder is taken to do alone, in pixels a second, until a check measured more
/// (the best any check measured is kept): under what Intel's MFT did on an XPS 13 (Iris Xe), one
/// frame inside at a time as a stream feeds it, even beside a 4K stream of its own (3840×2400 at
/// 34–55 fps, 315–508 MP/s; 1920×1200 at 58–65), so `return_bar` never asks a slower GPU for more
/// than it has. At 3840×2400 it asks for 20 fps; a starved encoder showed 9.
pub const ASSUMED_ENGINE_PIXEL_RATE: f64 = 250e6;

/// The rate a check must measure at `pixels` a frame for the stream to go back to the hardware:
/// `SHARE` of the stream's rate (60 at most, what the software encoder gives), but never more than
/// `SHARE` of what the engine does alone at that size (`engine_pixel_rate`): a fixed bar would
/// keep a large Desktop on the software encoder for good on a GPU that cannot reach it even when
/// free, and a starved encoder still shows far below either.
pub fn return_bar(stream_fps: u32, pixels: u64, engine_pixel_rate: f64) -> f64 {
    let by_stream = stream_fps.clamp(1, SOFTWARE_MAX_FPS) as f64 * SHARE;
    let by_engine = engine_pixel_rate / pixels.max(1) as f64 * SHARE;
    by_stream.min(by_engine)
}

/// The software encoder's frame rate for a stream that asks for `fps`.
pub fn software_fps(fps: u32) -> u32 {
    fps.clamp(1, SOFTWARE_MAX_FPS)
}

/// What a check of the hardware found.
#[derive(Clone, Debug, PartialEq)]
pub enum Check {
    /// Frames came back at this rate (None: they came back, but too few to time).
    Answered(Option<f64>),
    /// A frame stayed inside longer than the check waits.
    NoAnswer(Duration),
    /// No hardware encoder would start, for this reason.
    Refused(String),
}

/// What the stream does after a check, and the line that says so.
#[derive(Clone, Debug, PartialEq)]
pub enum Verdict {
    /// Back to the hardware encoder.
    Back(String),
    /// On the software encoder still; the next check after `next`.
    Stay { line: String, next: Duration },
}

/// The host's view of its hardware encoder, shared by every stream: on the software encoder or
/// not, and when to check again.
#[derive(Clone, Debug)]
pub struct Health {
    software: bool,
    interval: Duration,
    due: Option<Instant>,
    returned_at: Option<Instant>,
    engine_pixel_rate: f64,
    first: Duration,
    max: Duration,
}

impl Default for Health {
    fn default() -> Self {
        Health::new(FIRST_INTERVAL, MAX_INTERVAL)
    }
}

impl Health {
    /// `first` and `max`: the waits (FIRST_INTERVAL and MAX_INTERVAL; shorter in tests).
    pub fn new(first: Duration, max: Duration) -> Self {
        Health { software: false, interval: first, due: None, returned_at: None, engine_pixel_rate: ASSUMED_ENGINE_PIXEL_RATE, first, max }
    }

    /// Streams use the software encoder now.
    pub fn software(&self) -> bool {
        self.software
    }

    /// The engine rate `return_bar` holds checks against: the assumed one, or the best measured.
    pub fn engine_pixel_rate(&self) -> f64 {
        self.engine_pixel_rate
    }

    /// A hardware encoder failed at `now`: the software encoder from here on. Returns the wait
    /// before the first check.
    pub fn fell_back(&mut self, now: Instant) -> Duration {
        if let Some(returned) = self.returned_at.take() {
            let lasted = now.saturating_duration_since(returned);
            if lasted < RETURN_DID_NOT_LAST {
                self.interval = (self.interval * 2).min(self.max);
            } else if lasted >= RETURN_LASTED {
                self.interval = self.first;
            }
        }
        self.software = true;
        self.due = Some(now + self.interval);
        self.interval
    }

    /// A check is due: on the software encoder, and its time has come.
    pub fn check_due(&self, now: Instant) -> bool {
        self.software && self.due.is_some_and(|d| now >= d)
    }

    /// How long until the next check (zero when due; None when none is coming).
    pub fn until_check(&self, now: Instant) -> Option<Duration> {
        if !self.software {
            return None;
        }
        self.due.map(|d| d.saturating_duration_since(now))
    }

    /// What a check at `size` (the stream's on the hardware) for a stream at `stream_fps` found, at
    /// `now`: back to the hardware, or the software encoder and a later check.
    pub fn judge(&mut self, now: Instant, check: &Check, size: (u32, u32), stream_fps: u32) -> Verdict {
        let pixels = size.0 as u64 * size.1 as u64;
        let needed = return_bar(stream_fps, pixels, self.engine_pixel_rate);
        let at = format!("{}×{}", size.0, size.1);
        match check {
            Check::Answered(fps) => {
                if let Some(f) = fps {
                    self.engine_pixel_rate = self.engine_pixel_rate.max(f * pixels as f64);
                }
                let measured = match fps {
                    Some(f) => format!("{} fps at {at}", f.round() as u64),
                    None => format!("answered at {at}"),
                };
                if fps.is_none_or(|f| f >= needed) {
                    self.software = false;
                    self.due = None;
                    self.returned_at = Some(now);
                    return Verdict::Back(format!(
                        "Hardware encoder is back ({measured} in a test, {} needed); restarting the stream on it.",
                        needed.round() as u64
                    ));
                }
                let next = self.later(now);
                Verdict::Stay {
                    line: format!(
                        "Hardware encoder answers but is busy ({measured} in a test, under the {} fps a return needs; another app may be using it); next check in {} s.",
                        needed.round() as u64,
                        next.as_secs()
                    ),
                    next,
                }
            }
            Check::NoAnswer(after) => {
                let next = self.later(now);
                Verdict::Stay {
                    line: format!("Hardware encoder still not answering after {} ms; next check in {} s.", after.as_millis(), next.as_secs()),
                    next,
                }
            }
            Check::Refused(why) => {
                let next = self.later(now);
                Verdict::Stay { line: format!("Hardware encoder would not start ({why}); next check in {} s.", next.as_secs()), next }
            }
        }
    }

    /// The next check after a failed one: twice the last wait, up to the longest.
    fn later(&mut self, now: Instant) -> Duration {
        self.interval = (self.interval * 2).min(self.max);
        self.due = Some(now + self.interval);
        self.interval
    }
}

/// The line for a hardware encoder that failed, with the wait before the first check.
pub fn fallback_line(name: &str, why: &str, next: Duration) -> String {
    format!(
        "Hardware encoder {name} failed ({why}); switching to the software encoder at half scale until the hardware keeps up again (next check in {} s)",
        next.as_secs()
    )
}

/// The line for a stream that starts on the software encoder because an earlier one fell back.
pub fn starts_on_software_line(until_check: Option<Duration>) -> String {
    match until_check {
        Some(d) if !d.is_zero() => format!(
            "Starting on the software encoder: the hardware one failed and has not passed a check since (next check in {} s)",
            d.as_secs().max(1)
        ),
        _ => "Starting on the software encoder: the hardware one failed and has not passed a check since (checking now)".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);

    fn at(t0: Instant, secs: u64) -> Instant {
        t0 + Duration::from_secs(secs)
    }

    #[test]
    fn the_bar_is_the_streams_share_or_the_engines() {
        // 60 fps at 1920×1200 (2.3 MP): the stream's share, 45, under the engine's 250 MP/s (81).
        assert_eq!(return_bar(60, 1920 * 1200, ASSUMED_ENGINE_PIXEL_RATE), 45.0);
        // 120 fps asks no more than 60 does: the software encoder gives 60 at most.
        assert_eq!(return_bar(120, 1920 * 1200, ASSUMED_ENGINE_PIXEL_RATE), 45.0);
        // 3840×2400 (9.2 MP) with the assumed engine: 0.75 × 250e6 / 9.2e6 ≈ 20.3.
        let big = return_bar(60, 3840 * 2400, ASSUMED_ENGINE_PIXEL_RATE);
        assert!((big - 20.345).abs() < 0.01, "{big}");
        // A measured engine of 500 MP/s raises it to ≈ 40.7.
        let measured = return_bar(60, 3840 * 2400, 500e6);
        assert!((measured - 40.69).abs() < 0.01, "{measured}");
        // 30 fps: 22.5.
        assert_eq!(return_bar(30, 1280 * 800, ASSUMED_ENGINE_PIXEL_RATE), 22.5);
        // Nonsense in, something finite out.
        assert!(return_bar(0, 0, ASSUMED_ENGINE_PIXEL_RATE).is_finite());
    }

    #[test]
    fn software_runs_at_60_at_most() {
        assert_eq!(software_fps(120), 60);
        assert_eq!(software_fps(60), 60);
        assert_eq!(software_fps(30), 30);
        assert_eq!(software_fps(0), 1);
    }

    #[test]
    fn a_fallback_checks_after_30_s_then_backs_off_to_300() {
        let t0 = Instant::now();
        let mut h = Health::default();
        assert!(!h.software());
        assert_eq!(h.until_check(t0), None);
        assert_eq!(h.fell_back(t0), 30 * S);
        assert!(h.software());
        assert!(!h.check_due(at(t0, 29)));
        assert!(h.check_due(at(t0, 30)));
        assert_eq!(h.until_check(at(t0, 10)), Some(20 * S));
        let mut now = at(t0, 30);
        let mut waits = Vec::new();
        for _ in 0..6 {
            match h.judge(now, &Check::NoAnswer(Duration::from_millis(1000)), (1920, 1200), 60) {
                Verdict::Stay { next, line } => {
                    assert!(line.starts_with("Hardware encoder still not answering after 1000 ms; next check in "), "{line}");
                    waits.push(next.as_secs());
                    assert!(!h.check_due(now));
                    now += next;
                    assert!(h.check_due(now));
                }
                v => panic!("{v:?}"),
            }
        }
        assert_eq!(waits, vec![60, 120, 240, 300, 300, 300]);
        assert!(h.software());
    }

    #[test]
    fn a_check_that_keeps_up_goes_back() {
        let t0 = Instant::now();
        let mut h = Health::default();
        h.fell_back(t0);
        match h.judge(at(t0, 30), &Check::Answered(Some(121.0)), (1920, 1200), 60) {
            Verdict::Back(line) => assert_eq!(line, "Hardware encoder is back (121 fps at 1920×1200 in a test, 45 needed); restarting the stream on it."),
            v => panic!("{v:?}"),
        }
        assert!(!h.software());
        assert!(!h.check_due(at(t0, 1000)));
        assert_eq!(h.until_check(at(t0, 31)), None);
        // The measured rate is the engine's from now on (121 × 2.3 MP ≈ 279 MP/s, over the 250 assumed).
        assert!((h.engine_pixel_rate() - 121.0 * 1920.0 * 1200.0).abs() < 1.0);
    }

    #[test]
    fn a_slow_check_is_busy_and_waits_longer() {
        let t0 = Instant::now();
        let mut h = Health::default();
        h.fell_back(t0);
        match h.judge(at(t0, 30), &Check::Answered(Some(8.0)), (3840, 2400), 60) {
            Verdict::Stay { line, next } => {
                assert_eq!(next, 60 * S);
                assert_eq!(
                    line,
                    "Hardware encoder answers but is busy (8 fps at 3840×2400 in a test, under the 20 fps a return needs; another app may be using it); next check in 60 s."
                );
            }
            v => panic!("{v:?}"),
        }
        // A busy check never lowers what the engine is taken to do.
        assert_eq!(h.engine_pixel_rate(), ASSUMED_ENGINE_PIXEL_RATE);
        assert!(h.software());
    }

    #[test]
    fn the_bar_learns_the_engine_from_its_best_check() {
        let t0 = Instant::now();
        let mut h = Health::default();
        h.fell_back(t0);
        // 3840×2400 at 30 fps: over the assumed engine's 20, so back; the engine is now 276 MP/s.
        assert!(matches!(h.judge(at(t0, 30), &Check::Answered(Some(30.0)), (3840, 2400), 60), Verdict::Back(_)));
        h.fell_back(at(t0, 2000));
        // The next bar at that size is 0.75 × 30 = 22.5: 21 fps is busy now, 23 is back.
        assert!(matches!(h.judge(at(t0, 2030), &Check::Answered(Some(21.0)), (3840, 2400), 60), Verdict::Stay { .. }));
        assert!(matches!(h.judge(at(t0, 2090), &Check::Answered(Some(23.0)), (3840, 2400), 60), Verdict::Back(_)));
    }

    #[test]
    fn an_unmeasured_answer_counts_as_keeping_up() {
        let t0 = Instant::now();
        let mut h = Health::default();
        h.fell_back(t0);
        match h.judge(at(t0, 30), &Check::Answered(None), (1280, 800), 60) {
            Verdict::Back(line) => assert!(line.contains("(answered at 1280×800 in a test, 45 needed)"), "{line}"),
            v => panic!("{v:?}"),
        }
    }

    #[test]
    fn a_refusal_stays_and_says_why() {
        let t0 = Instant::now();
        let mut h = Health::default();
        h.fell_back(t0);
        match h.judge(at(t0, 30), &Check::Refused("no hardware HEVC encoder on this GPU".into()), (1280, 800), 60) {
            Verdict::Stay { line, next } => {
                assert_eq!(next, 60 * S);
                assert_eq!(line, "Hardware encoder would not start (no hardware HEVC encoder on this GPU); next check in 60 s.");
            }
            v => panic!("{v:?}"),
        }
    }

    #[test]
    fn a_hang_soon_after_a_return_waits_longer_and_a_long_return_starts_over() {
        let t0 = Instant::now();
        let mut h = Health::default();
        assert_eq!(h.fell_back(t0), 30 * S);
        assert!(matches!(h.judge(at(t0, 30), &Check::NoAnswer(S), (1280, 800), 60), Verdict::Stay { .. })); // 60
        assert!(matches!(h.judge(at(t0, 90), &Check::Answered(Some(100.0)), (1280, 800), 60), Verdict::Back(_)));
        // Hung again 2 minutes after the return: the busy spell is still on, 120 s.
        assert_eq!(h.fell_back(at(t0, 210)), 120 * S);
        assert!(matches!(h.judge(at(t0, 330), &Check::Answered(Some(100.0)), (1280, 800), 60), Verdict::Back(_)));
        // Between 5 and 10 minutes: the wait stays as it was.
        assert_eq!(h.fell_back(at(t0, 330 + 400)), 120 * S);
        assert!(matches!(h.judge(at(t0, 850), &Check::Answered(Some(100.0)), (1280, 800), 60), Verdict::Back(_)));
        // A return that lasted 10 minutes: 30 s again.
        assert_eq!(h.fell_back(at(t0, 850 + 600)), 30 * S);
    }

    #[test]
    fn the_waits_scale_for_tests() {
        let t0 = Instant::now();
        let mut h = Health::new(2 * S, 20 * S);
        assert_eq!(h.fell_back(t0), 2 * S);
        let mut waits = Vec::new();
        let mut now = at(t0, 2);
        for _ in 0..5 {
            if let Verdict::Stay { next, .. } = h.judge(now, &Check::NoAnswer(S), (64, 64), 60) {
                waits.push(next.as_secs());
                now += next;
            }
        }
        assert_eq!(waits, vec![4, 8, 16, 20, 20]);
    }

    #[test]
    fn the_lines() {
        assert_eq!(
            fallback_line("Intel® Hardware H265 Encoder MFT", "nothing came back for 1502 ms", 30 * S),
            "Hardware encoder Intel® Hardware H265 Encoder MFT failed (nothing came back for 1502 ms); switching to the software encoder at half scale until the hardware keeps up again (next check in 30 s)"
        );
        assert_eq!(
            starts_on_software_line(Some(Duration::from_millis(12_400))),
            "Starting on the software encoder: the hardware one failed and has not passed a check since (next check in 12 s)"
        );
        assert_eq!(
            starts_on_software_line(Some(Duration::from_millis(300))),
            "Starting on the software encoder: the hardware one failed and has not passed a check since (next check in 1 s)"
        );
        assert!(starts_on_software_line(Some(Duration::ZERO)).ends_with("(checking now)"));
        assert!(starts_on_software_line(None).ends_with("(checking now)"));
    }
}
