//! What the pairing window shows (portable: unit-tested on any machine; `win::pairui` draws it).
//!
//! Sill.app's "Pair iPhone or iPad" (Sources/SillMenuBar/PairDeviceWindow.swift) is the model: the
//! words, a QR code of the `sill://pair` link, the 12-digit code, "Works once, for the next 4:58.",
//! a wrong code's warning, "Paired with ‹device›." and the window closing 3 s later. The core keeps
//! one code open at a time (NOTES-core.md); this turns its events into what the window shows and
//! whether it should come up, go away, or only redraw.

use std::time::{Duration, Instant};

use sill_host_core::platform::PairingEvent;

/// How long "Paired with ‹device›." stays before the window closes (the Mac's 3 s).
pub const PAIRED_LINGER: Duration = Duration::from_secs(3);

/// A code the window can show.
#[derive(Clone, Debug, PartialEq)]
pub struct Code {
    /// 12 digits.
    pub digits: String,
    /// What the QR code holds.
    pub link: String,
    pub expires_at: Instant,
    /// The device whose ask opened it: then the window is that device's ("Didn’t ask for this?").
    pub asked_by: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Waiting,
    Paired(String),
    Stopped,
    /// No code works (cancelled on this computer); nothing to show.
    Off,
}

/// What the window should do after an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Nothing visible changed.
    None,
    /// Redraw what shows (if it shows).
    Refresh,
    /// Come up; `activate`: take the keyboard too (only at the host's start, when someone just
    /// launched it; a device's ask never takes the keyboard from whatever the person is doing).
    Show { activate: bool },
    Hide,
    /// Redraw now, close after this long (`PairView::linger_ended` then).
    HideAfter(Duration),
}

/// The window's two buttons, left to right; the last is the default (Return).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Buttons {
    /// The host's own code: a fresh one, or close the window (the code keeps working, as the
    /// console's does).
    NewCodeClose,
    /// A device's ask: Cancel stops that code and keeps the device quiet for 10 minutes.
    Cancel,
    Done,
}

/// The lines the window shows, top to bottom; None leaves a line out.
#[derive(Clone, Debug, PartialEq)]
pub struct Content {
    pub headline: Option<String>,
    /// The headline in the success colour (paired).
    pub success: bool,
    pub body: Option<String>,
    /// The QR code's text, and whether it is spent (drawn faint, as the Mac's).
    pub qr: Option<(String, bool)>,
    pub scan_hint: Option<String>,
    /// "1234 5678 9012", and whether it is spent.
    pub code: Option<(String, bool)>,
    pub countdown: Option<String>,
    /// A warning (a wrong code came).
    pub warning: Option<String>,
    pub notes: Vec<String>,
    pub buttons: Buttons,
}

pub struct PairView {
    /// The host's name as devices list it ("tap XPS13").
    pub host: String,
    pub phase: Phase,
    pub code: Option<Code>,
    /// A fresh code that came while "Paired" shows; it takes over when the window closes.
    next: Option<Code>,
    /// A device that asked while the host's own code showed.
    asked: Option<String>,
    wrong: Option<(String, u32)>,
    /// The first code came (the window comes up at the start only when nothing is paired yet).
    started: bool,
}

impl PairView {
    pub fn new(host: &str) -> PairView {
        PairView { host: host.to_string(), phase: Phase::Off, code: None, next: None, asked: None, wrong: None, started: false }
    }

    /// One event from the core. `visible`: whether the window shows now.
    pub fn apply(&mut self, event: PairingEvent, now: Instant, visible: bool) -> Action {
        match event {
            PairingEvent::Code { code, link, expires_in, asked_by, paired, host } => {
                self.host = host;
                let first = !self.started;
                self.started = true;
                let fresh = Code { digits: code, link, expires_at: now + expires_in, asked_by: asked_by.clone() };
                if matches!(self.phase, Phase::Paired(_)) && visible {
                    // "Paired with ‹device›." stays its 3 s; the new code waits behind it.
                    self.next = Some(fresh);
                    return Action::None;
                }
                self.take_code(fresh);
                if asked_by.is_some() {
                    Action::Show { activate: false }
                } else if first && paired == 0 {
                    Action::Show { activate: true }
                } else {
                    Action::Refresh
                }
            }
            PairingEvent::Asked { device } => {
                if self.phase != Phase::Waiting {
                    return Action::None;
                }
                if self.code.as_ref().is_some_and(|c| c.asked_by.is_none()) {
                    self.asked = Some(device);
                }
                Action::Show { activate: false }
            }
            PairingEvent::Withdrawn { device } => {
                if self.code.as_ref().and_then(|c| c.asked_by.as_deref()) == Some(device.as_str()) {
                    Action::Hide
                } else {
                    Action::None
                }
            }
            PairingEvent::WrongCode { device, tries_left } => {
                self.wrong = Some((device, tries_left));
                Action::Refresh
            }
            PairingEvent::Paired { device } => {
                if !visible {
                    return Action::None;
                }
                self.phase = Phase::Paired(device);
                self.wrong = None;
                Action::HideAfter(PAIRED_LINGER)
            }
            PairingEvent::Stopped => {
                self.phase = Phase::Stopped;
                self.wrong = None;
                Action::Refresh
            }
            PairingEvent::Cancelled => {
                self.phase = Phase::Off;
                self.code = None;
                self.next = None;
                self.asked = None;
                self.wrong = None;
                Action::Hide
            }
        }
    }

    /// "Paired" has shown its 3 s: the window closes, and the code that came meanwhile is the one
    /// it shows next time.
    pub fn linger_ended(&mut self) -> Action {
        if let Some(next) = self.next.take() {
            self.take_code(next);
        } else if matches!(self.phase, Phase::Paired(_)) {
            self.phase = if self.code.is_some() { Phase::Waiting } else { Phase::Off };
        }
        Action::Hide
    }

    fn take_code(&mut self, code: Code) {
        self.code = Some(code);
        self.phase = Phase::Waiting;
        self.asked = None;
        self.wrong = None;
    }

    /// Whether a code is up that a device may still use (the countdown runs).
    pub fn counting(&self) -> bool {
        self.phase == Phase::Waiting && self.code.is_some()
    }

    pub fn content(&self, now: Instant) -> Content {
        let grouped = |d: &str| d.as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join(" ");
        let device_note = "A paired device can see and control this PC.".to_string();
        let mut c = Content {
            headline: None,
            success: false,
            body: None,
            qr: None,
            scan_hint: None,
            code: None,
            countdown: None,
            warning: None,
            notes: Vec::new(),
            buttons: Buttons::NewCodeClose,
        };
        let Some(code) = &self.code else {
            return c;
        };
        let spent = !matches!(self.phase, Phase::Waiting);
        c.qr = Some((code.link.clone(), spent));
        c.code = Some((grouped(&code.digits), spent));
        c.warning = self
            .wrong
            .as_ref()
            .map(|(who, left)| format!("A wrong code came from {who}. {left} tr{} left.", if *left == 1 { "y" } else { "ies" }));
        match &self.phase {
            Phase::Waiting => {
                c.countdown = Some(countdown(code.expires_at, now));
                match &code.asked_by {
                    Some(who) => {
                        c.headline = Some(format!("{who} on this network asked to pair."));
                        c.body = Some("Point it at this code, or tap Enter Code Instead and type the code.".into());
                        c.notes = vec![device_note, "Didn’t ask for this? Click Cancel.".into()];
                        c.buttons = Buttons::Cancel;
                    }
                    None => {
                        c.headline = self.asked.as_ref().map(|who| format!("{who} asked to pair."));
                        c.body = Some(format!("Open Sill on your iPhone or iPad, tap {}, then point it at this code.", self.host));
                        c.scan_hint = Some("Can’t scan? Tap Enter Code Instead, and type:".into());
                        c.notes = vec![device_note];
                    }
                }
            }
            Phase::Paired(who) => {
                c.headline = Some(format!("Paired with {who}."));
                c.success = true;
                c.body = Some(format!("It can connect to {} from now on.", self.host));
                c.warning = None;
                c.buttons = Buttons::Done;
            }
            Phase::Stopped => {
                c.headline = Some(format!("Pairing stopped after {} wrong codes.", 5));
                c.body = Some("No code works now. Choose New Code to pair a device.".into());
                c.warning = None;
            }
            Phase::Off => {}
        }
        c
    }
}

/// "Works once, for the next 4:58." (rounded up, as the Mac's).
pub fn countdown(until: Instant, now: Instant) -> String {
    let left = until.saturating_duration_since(now);
    let secs = left.as_secs() + u64::from(left.subsec_nanos() > 0);
    format!("Works once, for the next {}:{:02}.", secs / 60, secs % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(asked_by: Option<&str>, paired: usize) -> PairingEvent {
        PairingEvent::Code {
            code: "123456789012".into(),
            link: "sill://pair?v=1".into(),
            expires_in: Duration::from_secs(300),
            asked_by: asked_by.map(String::from),
            paired,
            host: "XPS13".into(),
        }
    }

    #[test]
    fn comes_up_at_the_start_only_when_nothing_is_paired() {
        let now = Instant::now();
        let mut v = PairView::new("XPS13");
        assert_eq!(v.apply(code(None, 0), now, false), Action::Show { activate: true });
        let mut w = PairView::new("XPS13");
        assert_eq!(w.apply(code(None, 2), now, false), Action::Refresh);
        // Later codes (expiry, New Code) only redraw.
        assert_eq!(v.apply(code(None, 0), now, true), Action::Refresh);
        let c = v.content(now);
        assert_eq!(c.code, Some(("1234 5678 9012".into(), false)));
        assert_eq!(c.countdown.as_deref(), Some("Works once, for the next 5:00."));
        assert_eq!(c.body.as_deref(), Some("Open Sill on your iPhone or iPad, tap XPS13, then point it at this code."));
        assert_eq!(c.buttons, Buttons::NewCodeClose);
        assert_eq!(c.headline, None);
    }

    #[test]
    fn a_device_ask_shows_without_the_keyboard() {
        let now = Instant::now();
        let mut v = PairView::new("XPS13");
        v.apply(code(None, 1), now, false);
        // An ask while the host's own code is up: its headline over the same code.
        assert_eq!(v.apply(PairingEvent::Asked { device: "iPad (iPad13,16)".into() }, now, false), Action::Show { activate: false });
        assert_eq!(v.content(now).headline.as_deref(), Some("iPad (iPad13,16) asked to pair."));
        // A window the ask opened is that device's.
        assert_eq!(v.apply(code(Some("iPad (iPad13,16)"), 1), now, true), Action::Show { activate: false });
        let c = v.content(now);
        assert_eq!(c.headline.as_deref(), Some("iPad (iPad13,16) on this network asked to pair."));
        assert_eq!(c.buttons, Buttons::Cancel);
        assert_eq!(c.notes.last().map(String::as_str), Some("Didn’t ask for this? Click Cancel."));
        // The device cancels on its side: the window goes; the fresh code only redraws, hidden.
        assert_eq!(v.apply(PairingEvent::Withdrawn { device: "iPad (iPad13,16)".into() }, now, true), Action::Hide);
        assert_eq!(v.apply(code(None, 1), now, false), Action::Refresh);
    }

    #[test]
    fn wrong_codes_then_stopped() {
        let now = Instant::now();
        let mut v = PairView::new("XPS13");
        v.apply(code(None, 0), now, true);
        v.apply(PairingEvent::WrongCode { device: "iPad".into(), tries_left: 1 }, now, true);
        assert_eq!(v.content(now).warning.as_deref(), Some("A wrong code came from iPad. 1 try left."));
        assert_eq!(v.apply(PairingEvent::Stopped, now, true), Action::Refresh);
        let c = v.content(now);
        assert_eq!(c.headline.as_deref(), Some("Pairing stopped after 5 wrong codes."));
        assert_eq!(c.code, Some(("1234 5678 9012".into(), true)));
        assert_eq!(c.qr.map(|q| q.1), Some(true));
        assert_eq!(c.countdown, None);
        assert!(!v.counting());
    }

    #[test]
    fn paired_lingers_then_the_next_code_takes_over() {
        let now = Instant::now();
        let mut v = PairView::new("XPS13");
        v.apply(code(None, 0), now, true);
        assert_eq!(v.apply(PairingEvent::Paired { device: "iPad".into() }, now, true), Action::HideAfter(PAIRED_LINGER));
        // The core's fresh code right after waits behind "Paired".
        let fresh = PairingEvent::Code {
            code: "999988887777".into(),
            link: "sill://pair?v=1&s=new".into(),
            expires_in: Duration::from_secs(300),
            asked_by: None,
            paired: 1,
            host: "XPS13".into(),
        };
        assert_eq!(v.apply(fresh, now, true), Action::None);
        let c = v.content(now);
        assert_eq!(c.headline.as_deref(), Some("Paired with iPad."));
        assert!(c.success);
        assert_eq!(c.buttons, Buttons::Done);
        assert_eq!(v.linger_ended(), Action::Hide);
        assert_eq!(v.content(now).code, Some(("9999 8888 7777".into(), false)));
        assert_eq!(v.phase, Phase::Waiting);
        // Paired while hidden (a code typed from the console): nothing shows.
        assert_eq!(v.apply(PairingEvent::Paired { device: "iPhone".into() }, now, false), Action::None);
    }

    #[test]
    fn cancelled_hides_and_shows_nothing() {
        let now = Instant::now();
        let mut v = PairView::new("XPS13");
        v.apply(code(Some("iPad"), 0), now, true);
        assert_eq!(v.apply(PairingEvent::Cancelled, now, true), Action::Hide);
        assert_eq!(v.content(now).code, None);
        assert_eq!(v.apply(PairingEvent::Asked { device: "iPad".into() }, now, false), Action::None);
    }

    #[test]
    fn countdown_rounds_up() {
        let now = Instant::now();
        assert_eq!(countdown(now + Duration::from_millis(298_200), now), "Works once, for the next 4:59.");
        assert_eq!(countdown(now + Duration::from_secs(61), now), "Works once, for the next 1:01.");
        assert_eq!(countdown(now, now + Duration::from_secs(1)), "Works once, for the next 0:00.");
    }
}
