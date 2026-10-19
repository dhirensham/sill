//! What Sill.exe's tray icon shows (portable: unit-tested on any machine; `win::tray` draws it).
//!
//! Sill.app is the model: its status card's rules and words (Sources/SillMenuBar/StatusText.swift)
//! and its menu (StatusItemController.swift, `MenuBuilder`), as data, so the tray, the test dump
//! and the tests cannot disagree. A Windows menu has no subtitles and no custom views: the card is
//! a bold header and grey lines (a device's numbers right-aligned in the menu's shortcut column);
//! the Mac's Permissions submenu is the Windows Firewall item here; the Virtual Display, Direct
//! Wireless and Remote Access items are the Mac's alone (or wait for a Settings window).

use sill_host_core::config::HostConfig;
use sill_host_core::status::{DeviceStatus, HostStatus, StreamStatus};
use sill_protocol::messages::settings_choices;

/// Which glyph the tray shows (StatusGlyph.swift's four).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    Idle,
    Connected,
    Streaming,
    Attention,
}

/// What Windows Firewall does with the home door's port, as `win::firewall` found it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Firewall {
    Checking,
    /// Inbound connections to the port get through (a rule, or the firewall is off).
    Allowed,
    /// Nothing lets them in (or a block rule wins).
    NotAllowed,
    /// The network is Public, where Sill's rule (Private networks) does not apply.
    PublicNetwork,
    /// An organization's policy decides; Sill can't add a rule.
    Managed,
}

/// Launch at Login as the menu shows it.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct LoginState {
    pub on: bool,
    /// Turned off in Task Manager's Startup apps: only there can it be turned on again.
    pub disabled_by_user: bool,
}

/// Everything the menu is built from, read when it opens.
pub struct TrayInput<'a> {
    /// None while the host is starting.
    pub status: Option<&'a HostStatus>,
    /// The host could not start (a port in use): why.
    pub start_error: Option<&'a str>,
    /// A device that asked to pair in the last 5 minutes while the pairing window was hidden.
    pub pairing_request: Option<&'a str>,
    pub firewall: Firewall,
    pub login: LoginState,
}

/// One status line under the header: a title and, right-aligned, its numbers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub title: String,
    pub detail: Option<String>,
}

/// The status card (StatusText.present).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Presentation {
    pub header: String,
    pub subtitle: Option<String>,
    pub source: Option<Row>,
    pub devices: Vec<Row>,
    pub glyph: Glyph,
    /// "Sill — ‹header›", at most 127 characters (the tray's limit).
    pub tooltip: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    None,
    SetMaxFps(i64),
    SetBitrate(i64),
    SetCaptureScale(f64),
    ToggleLaunchAtLogin,
    AllowFirewall,
    OpenNetworkSettings,
    OpenStartupSettings,
    ShowPairing,
    ShowLog,
    TryAgain,
    Quit,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// The card's first line: bold, does nothing.
    Header,
    /// A card line: grey, does nothing.
    Line,
    Item,
    Separator,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub kind: Kind,
    /// As the menu shows it (`menu_text`: `&` doubled, tabs gone, long titles shortened).
    pub title: String,
    /// Right-aligned after a tab (the menu's shortcut column).
    pub detail: Option<String>,
    pub checked: bool,
    pub enabled: bool,
    /// Something needs the person (the Mac's orange triangle; here a ⚠ in front).
    pub attention: bool,
    pub action: Action,
    pub children: Vec<Entry>,
}

impl Entry {
    fn item(title: &str, action: Action) -> Entry {
        Entry { kind: Kind::Item, title: menu_text(title), detail: None, checked: false, enabled: true, attention: false, action, children: vec![] }
    }
    fn line(kind: Kind, title: &str, detail: Option<&str>) -> Entry {
        Entry {
            kind,
            title: menu_text(title),
            detail: detail.map(menu_text),
            checked: false,
            enabled: true,
            attention: false,
            action: Action::None,
            children: vec![],
        }
    }
    fn separator() -> Entry {
        Entry { kind: Kind::Separator, title: String::new(), detail: None, checked: false, enabled: true, attention: false, action: Action::None, children: vec![] }
    }
    fn checked(mut self, on: bool) -> Entry {
        self.checked = on;
        self
    }
    fn enabled(mut self, on: bool) -> Entry {
        self.enabled = on;
        self
    }
    fn attention(mut self) -> Entry {
        self.attention = true;
        self
    }
    fn with_children(mut self, children: Vec<Entry>) -> Entry {
        self.children = children;
        self
    }
}

/// The card (StatusText.swift's header rules, the first match winning; Mac → PC).
pub fn present(input: &TrayInput) -> Presentation {
    let (header, subtitle) = headline(input);
    let source = input.status.and_then(|s| s.stream.as_ref().map(|st| source_row(st, s)));
    let devices = input.status.map(|s| s.devices.iter().map(device_row).collect()).unwrap_or_default();
    let attention = input.start_error.is_some()
        || matches!(input.status.and_then(|s| s.advertised.as_ref()), Some(Err(_)))
        || input.pairing_request.is_some()
        || matches!(input.firewall, Firewall::NotAllowed | Firewall::PublicNetwork);
    let glyph = match input.status {
        _ if attention => Glyph::Attention,
        Some(s) if s.stream.is_some() && !s.devices.is_empty() => Glyph::Streaming,
        Some(s) if !s.devices.is_empty() => Glyph::Connected,
        _ => Glyph::Idle,
    };
    let tooltip = truncate(&format!("Sill — {header}"), 127);
    Presentation { header, subtitle, source, devices, glyph, tooltip }
}

fn headline(input: &TrayInput) -> (String, Option<String>) {
    if let Some(e) = input.start_error {
        return ("Sill Couldn’t Start".into(), Some(e.to_string()));
    }
    let Some(s) = input.status else {
        return ("Starting…".into(), None);
    };
    if let Some(Err(e)) = &s.advertised {
        return ("Not Visible on the Network".into(), Some(format!("{e}. Devices can still reach this PC at port {}.", s.port)));
    }
    let n = s.devices.len();
    let first = s.devices.first().map(|d| short_name(&d.name)).unwrap_or_default();
    if n > 0 && s.stream.is_some() {
        let who = if n > 1 {
            format!("{n} Devices")
        } else if first.is_empty() {
            "a Device".into()
        } else {
            first
        };
        return (format!("Streaming to {who}"), None);
    }
    if n > 0 {
        let who = if n > 1 {
            format!("{n} Devices")
        } else if first.is_empty() {
            "Device".into()
        } else {
            first
        };
        return (format!("{who} Connected"), Some("Pick a window on the device to start streaming.".into()));
    }
    match &s.advertised {
        Some(Ok(())) => (
            "Waiting for a Device".into(),
            Some(format!("Open Sill on your iPhone or iPad. This PC appears as “{}”.", s.name)),
        ),
        _ => ("Waiting for a Device".into(), Some(format!("Not advertised; devices reach this PC at port {}.", s.port))),
    }
}

/// "iPad" for "iPad (iPad13,16)".
pub fn short_name(name: &str) -> String {
    name.split(" (").next().unwrap_or(name).trim().to_string()
}

fn source_row(st: &StreamStatus, s: &HostStatus) -> Row {
    let title = match &st.window {
        Some(w) => middle_truncate(w, 44),
        None => "Whole Desktop".into(),
    };
    let mut parts = Vec::new();
    if let Some((w, h)) = st.size {
        parts.push(format!("{w}×{h}"));
    }
    parts.push(format!("{} fps", st.fps));
    parts.push(format!("{} Mbps", st.mbps));
    if s.devices.len() == 1 {
        if let Some(route) = &s.devices[0].route {
            parts.push(route.clone());
        }
    }
    Row { title, detail: Some(parts.join(" · ")) }
}

fn device_row(d: &DeviceStatus) -> Row {
    let ms = |v: i64| if v < 0 { "–".to_string() } else { format!("{v} ms") };
    let mut parts = Vec::new();
    if let Some(r) = d.report {
        parts.push(format!("{} fps", r.fps.max(0)));
        parts.push(format!("frame age {}", ms(r.frame_age_ms)));
        parts.push(format!("RTT {}", ms(r.rtt_ms)));
    }
    if let Some(route) = &d.route {
        parts.push(route.clone());
    }
    Row { title: d.name.clone(), detail: (!parts.is_empty()).then(|| parts.join(" · ")) }
}

/// The menu, top to bottom (MenuBuilder.entries).
pub fn entries(input: &TrayInput, p: &Presentation) -> Vec<Entry> {
    let mut menu = vec![Entry::line(Kind::Header, &p.header, None)];
    if let Some(s) = &p.subtitle {
        // A menu line does not wrap: the subtitle in lines of a menu's width.
        for line in wrap(s, 48) {
            menu.push(Entry::line(Kind::Line, &line, None));
        }
    }
    for row in p.source.iter().chain(p.devices.iter()) {
        menu.push(Entry::line(Kind::Line, &row.title, row.detail.as_deref()));
    }

    // Things that need the person, then the menu proper.
    let mut attention = Vec::new();
    if input.start_error.is_some() {
        attention.push(Entry::item("Try Again", Action::TryAgain).attention());
    }
    if let Some(who) = input.pairing_request {
        attention.push(Entry::item(&format!("{} Wants to Pair", short_name(who)), Action::ShowPairing).attention());
    }
    match input.firewall {
        Firewall::NotAllowed => attention.push(Entry::item("Allow Sill Through Windows Firewall…", Action::AllowFirewall).attention()),
        Firewall::PublicNetwork => attention.push(Entry::item("This Network Is Public…", Action::OpenNetworkSettings).attention()),
        _ => {}
    }
    if !attention.is_empty() {
        menu.push(Entry::separator());
        menu.extend(attention);
    }

    if let Some(s) = input.status {
        let c = &s.config;
        menu.push(Entry::separator());
        menu.push(Entry::item("Frame Rate", Action::None).with_children(
            settings_choices::MAX_FPS
                .iter()
                .map(|&fps| Entry::item(&format!("Up to {fps} fps"), Action::SetMaxFps(fps)).checked(c.max_fps == fps))
                .collect(),
        ));
        menu.push(Entry::item("Quality", Action::None).with_children(quality(c)));
        menu.push(Entry::item("Resolution", Action::None).with_children(vec![
            Entry::item("Retina", Action::SetCaptureScale(2.0)).checked(c.capture_scale >= 1.5),
            Entry::item("Standard", Action::SetCaptureScale(1.0)).checked(c.capture_scale < 1.5),
        ]));
    }

    menu.push(Entry::separator());
    menu.push(match input.firewall {
        Firewall::Checking => Entry::item("Windows Firewall: Checking…", Action::None).enabled(false),
        Firewall::Allowed => Entry::item("Windows Firewall: Allowed", Action::None).checked(true).enabled(false),
        Firewall::NotAllowed => Entry::item("Windows Firewall: Not Allowed…", Action::AllowFirewall),
        Firewall::PublicNetwork => Entry::item("Windows Firewall: Public Network…", Action::OpenNetworkSettings),
        Firewall::Managed => Entry::item("Windows Firewall: Set by Your Organization", Action::None).enabled(false),
    });
    menu.push(if input.login.disabled_by_user {
        Entry::item("Launch at Login (Off in Startup Apps)…", Action::OpenStartupSettings)
    } else {
        Entry::item("Launch at Login", Action::ToggleLaunchAtLogin).checked(input.login.on)
    });

    menu.push(Entry::separator());
    let can_pair = input.status.map(|s| s.home_tls).unwrap_or(false);
    menu.push(Entry::item("Pair iPhone or iPad…", Action::ShowPairing).enabled(can_pair));
    menu.push(Entry::item("Show Log…", Action::ShowLog));
    menu.push(Entry::separator());
    menu.push(Entry::item("Quit Sill", Action::Quit));
    menu
}

/// The seven presets ("Balanced — 15 Mbps"), and an off-preset bitrate checked and greyed after
/// them, as the Mac shows a custom one.
fn quality(c: &HostConfig) -> Vec<Entry> {
    let mut items: Vec<Entry> = settings_choices::BITRATE
        .iter()
        .map(|&b| {
            let title = format!("{} — {} Mbps", settings_choices::quality_name(b), settings_choices::mbps(b));
            Entry::item(&title, Action::SetBitrate(b)).checked(c.bitrate == b)
        })
        .collect();
    if !settings_choices::BITRATE.contains(&c.bitrate) {
        items.push(Entry::item(&format!("Custom — {} Mbps", settings_choices::mbps(c.bitrate)), Action::None).checked(true).enabled(false));
    }
    items
}

/// A title as a menu shows it: `&` doubled (it marks a mnemonic), tabs and line breaks as spaces
/// (a tab starts the shortcut column), at most 60 characters.
pub fn menu_text(s: &str) -> String {
    let clean: String = s.chars().map(|c| if c == '\t' || c == '\n' || c == '\r' { ' ' } else { c }).collect();
    truncate(&clean, 60).replace('&', "&&")
}

/// Words into lines of at most `width` characters (a longer word on a line of its own).
fn wrap(s: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in s.split_whitespace() {
        if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

/// "Visual Studio Co…ain.rs" (StatusText's middle truncation of a window's title).
fn middle_truncate(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let head = (max - 1) / 2 + (max - 1) % 2;
    let tail = (max - 1) / 2;
    let mut out: String = chars[..head].iter().collect();
    out.push('…');
    out.extend(&chars[chars.len() - tail..]);
    out
}

/// The menu as text, as Sill.app's `MenuBuilder.dump` writes menu.txt: "✓" checked, "⚠"
/// attention, "(off)" disabled, "▸" a submenu, "────────" a separator.
pub fn dump(entries: &[Entry]) -> String {
    fn walk(out: &mut String, entries: &[Entry], indent: &str) {
        for e in entries {
            match e.kind {
                Kind::Separator => out.push_str(&format!("{indent}────────\n")),
                Kind::Header => out.push_str(&format!("{indent}[card] {}\n", e.title)),
                Kind::Line => {
                    let detail = e.detail.as_ref().map(|d| format!(" — {d}")).unwrap_or_default();
                    out.push_str(&format!("{indent}       · {}{detail}\n", e.title));
                }
                Kind::Item => {
                    let mark = if e.attention {
                        "⚠ "
                    } else if e.checked {
                        "✓ "
                    } else {
                        "  "
                    };
                    let off = if e.enabled { "" } else { "  (off)" };
                    let more = if e.children.is_empty() { "" } else { " ▸" };
                    out.push_str(&format!("{indent}{mark}{}{off}{more}\n", e.title));
                    walk(out, &e.children, &format!("{indent}    "));
                }
            }
        }
    }
    let mut out = String::new();
    walk(&mut out, entries, "");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use sill_host_core::status::DeviceReport;

    fn status() -> HostStatus {
        HostStatus {
            name: "XPS13".into(),
            port: 7454,
            home_tls: true,
            advertised: Some(Ok(())),
            devices: vec![],
            stream: None,
            config: HostConfig::standard(),
            paired: 1,
        }
    }

    fn ipad(report: bool) -> DeviceStatus {
        DeviceStatus {
            name: "iPad (iPad13,16)".into(),
            endpoint: "[fe80::1]:59848".into(),
            route: None,
            report: report.then_some(DeviceReport { fps: 52, frame_age_ms: 9, rtt_ms: -1 }),
        }
    }

    fn input(s: Option<&HostStatus>) -> TrayInput<'_> {
        TrayInput { status: s, start_error: None, pairing_request: None, firewall: Firewall::Allowed, login: LoginState::default() }
    }

    #[test]
    fn every_header() {
        assert_eq!(present(&input(None)).header, "Starting…");
        let mut s = status();
        let p = present(&input(Some(&s)));
        assert_eq!((p.header.as_str(), p.glyph), ("Waiting for a Device", Glyph::Idle));
        assert_eq!(p.subtitle.as_deref(), Some("Open Sill on your iPhone or iPad. This PC appears as “XPS13”."));
        assert_eq!(p.tooltip, "Sill — Waiting for a Device");
        s.devices.push(ipad(false));
        let p = present(&input(Some(&s)));
        assert_eq!((p.header.as_str(), p.glyph), ("iPad Connected", Glyph::Connected));
        assert_eq!(p.subtitle.as_deref(), Some("Pick a window on the device to start streaming."));
        s.stream = Some(StreamStatus { window: None, size: Some((3840, 2400)), fps: 60, mbps: 15 });
        let p = present(&input(Some(&s)));
        assert_eq!((p.header.as_str(), p.glyph), ("Streaming to iPad", Glyph::Streaming));
        assert_eq!(p.source, Some(Row { title: "Whole Desktop".into(), detail: Some("3840×2400 · 60 fps · 15 Mbps".into()) }));
        s.devices.push(ipad(true));
        assert_eq!(present(&input(Some(&s))).header, "Streaming to 2 Devices");
        let mut t = status();
        t.advertised = Some(Err("DnsServiceRegister failed (5)".into()));
        let p = present(&input(Some(&t)));
        assert_eq!((p.header.as_str(), p.glyph), ("Not Visible on the Network", Glyph::Attention));
        let mut i = input(Some(&t));
        i.start_error = Some("Listener failed: port 7454 is in use");
        assert_eq!(present(&i).header, "Sill Couldn’t Start");
        t.advertised = None;
        assert_eq!(present(&input(Some(&t))).subtitle.as_deref(), Some("Not advertised; devices reach this PC at port 7454."));
    }

    #[test]
    fn device_rows_and_window_titles() {
        let mut s = status();
        s.devices = vec![ipad(true)];
        s.stream = Some(StreamStatus {
            window: Some("Visual Studio Code — sill - a very long window title that goes on and on".into()),
            size: None,
            fps: 60,
            mbps: 15,
        });
        let p = present(&input(Some(&s)));
        assert_eq!(p.devices[0], Row { title: "iPad (iPad13,16)".into(), detail: Some("52 fps · frame age 9 ms · RTT –".into()) });
        let source = p.source.unwrap();
        assert_eq!(source.title.chars().count(), 44);
        assert!(source.title.starts_with("Visual Studio Code — ") && source.title.ends_with("goes on and on"), "{}", source.title);
        assert_eq!(source.detail.as_deref(), Some("60 fps · 15 Mbps"));
        // Away: the route ends the device's row, and the source row while it is the only device.
        s.devices[0].route = Some("through Tailscale".into());
        let p = present(&input(Some(&s)));
        assert!(p.devices[0].detail.as_deref().unwrap().ends_with(" · through Tailscale"));
        assert!(p.source.unwrap().detail.unwrap().ends_with(" · through Tailscale"));
    }

    #[test]
    fn the_menu_as_the_mac_dumps_it() {
        let mut s = status();
        s.devices = vec![ipad(true)];
        let i = input(Some(&s));
        let menu = entries(&i, &present(&i));
        let text = dump(&menu);
        let want = "\
[card] iPad Connected
       · Pick a window on the device to start streaming.
       · iPad (iPad13,16) — 52 fps · frame age 9 ms · RTT –
────────
  Frame Rate ▸
      Up to 60 fps
    ✓ Up to 120 fps
  Quality ▸
      Low — 4 Mbps
      Efficient — 8 Mbps
    ✓ Balanced — 15 Mbps
      High — 25 Mbps
      Pro — 40 Mbps
      Ultra — 80 Mbps
      Extreme — 150 Mbps
  Resolution ▸
    ✓ Retina
      Standard
────────
✓ Windows Firewall: Allowed  (off)
  Launch at Login
────────
  Pair iPhone or iPad…
  Show Log…
────────
  Quit Sill
";
        assert_eq!(text, want, "\n{text}");
    }

    #[test]
    fn attention_items_and_the_firewall() {
        let mut s = status();
        s.config.bitrate = 12_000_000;
        let mut i = input(Some(&s));
        i.pairing_request = Some("iPhone (iPhone16,1)");
        i.firewall = Firewall::NotAllowed;
        i.login = LoginState { on: false, disabled_by_user: true };
        let p = present(&i);
        assert_eq!(p.glyph, Glyph::Attention);
        let text = dump(&entries(&i, &p));
        assert!(text.contains("⚠ iPhone Wants to Pair\n"), "{text}");
        assert!(text.contains("⚠ Allow Sill Through Windows Firewall…\n"), "{text}");
        assert!(text.contains("  Windows Firewall: Not Allowed…\n"), "{text}");
        assert!(text.contains("    ✓ Custom — 12 Mbps  (off)\n"), "{text}");
        assert!(text.contains("  Launch at Login (Off in Startup Apps)…\n"), "{text}");
        i.firewall = Firewall::PublicNetwork;
        assert!(dump(&entries(&i, &present(&i))).contains("⚠ This Network Is Public…\n"));
        // Starting: no settings yet, nothing to pair with.
        let none = input(None);
        let text = dump(&entries(&none, &present(&none)));
        assert!(!text.contains("Quality") && text.contains("  Pair iPhone or iPad…  (off)\n"), "{text}");
    }

    #[test]
    fn menu_text_is_safe() {
        assert_eq!(menu_text("Tom & Jerry\tTab"), "Tom && Jerry Tab");
        let long = "x".repeat(80);
        assert_eq!(menu_text(&long).chars().count(), 60);
        assert_eq!(short_name("iPad (iPad13,16)"), "iPad");
        assert_eq!(short_name("Dhiren’s iPad"), "Dhiren’s iPad");
        assert_eq!(
            wrap("Open Sill on your iPhone or iPad. This PC appears as “XPS13”.", 48),
            vec!["Open Sill on your iPhone or iPad. This PC".to_string(), "appears as “XPS13”.".to_string()]
        );
    }
}
