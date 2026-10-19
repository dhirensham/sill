//! Sill.exe's tray icon and menu (Sill.app's status item, StatusItemController.swift). What it shows
//! is `crate::traymenu` (portable, tested); this is the Windows side: the icon in the notification
//! area, its tooltip and glyph, the menu, and the host's life (start, Try Again, Quit, sign-out).
//!
//! One GUI thread (the process's main thread, a single-threaded apartment) owns a hidden top-level
//! window: not a message-only one, which never hears TaskbarCreated (Explorer restarted),
//! WM_SETTINGCHANGE (the taskbar's theme) or WM_ENDSESSION (sign-out). The host runs on its own
//! threads; this thread reads `HostHandle::status()` once a second and when the menu opens, and
//! never holds its state across the menu's modal loop (TrackPopupMenuEx), whose messages
//! (timers, the host's) come back into this window's procedure.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sill_host_core::config::ConfigChange;
use sill_host_core::platform::PairingEvent;
use sill_host_core::{Host, HostHandle, HostOptions};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, DeleteObject, MonitorFromPoint, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    HBITMAP, HGDIOBJ, MONITOR_DEFAULTTOPRIMARY,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, GetSystemMetricsForDpi, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Shell::{
    ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_INFO, NIM_ADD,
    NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NOTIFYICONDATAW, NOTIFYICONDATAW_0, NOTIFYICON_VERSION_4,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu,
    DispatchMessageW, FindWindowW, GetCursorPos, GetMessageW, GetSystemMetrics, PostMessageW, PostQuitMessage,
    RegisterClassExW, RegisterWindowMessageW, SetForegroundWindow, SetMenuDefaultItem, SetTimer, TrackPopupMenuEx,
    TranslateMessage, HICON, HMENU, ICONINFO, MF_CHECKED, MF_GRAYED, MF_POPUP, MF_SEPARATOR, MF_STRING, MSG,
    SM_CXSMICON, SM_MENUDROPALIGNMENT, SW_SHOWNORMAL, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTALIGN,
    TPM_RIGHTBUTTON, WM_APP, WM_CONTEXTMENU, WM_DISPLAYCHANGE, WM_ENDSESSION, WM_NULL,
    WM_QUERYENDSESSION, WM_SETTINGCHANGE, WM_TIMER, WNDCLASSEXW, WS_EX_TOOLWINDOW, WS_POPUP,
};

use super::util::wide;
use super::WindowsPlatform;
use crate::glyph;
use crate::settings::{self, Settings};
use crate::traymenu::{self, Action, Entry, Firewall, Glyph, Kind, LoginState, TrayInput};

/// The icon's messages (NOTIFYICON_VERSION_4: the event in LOWORD(lParam), the anchor in wParam).
const WM_TRAYICON: u32 = WM_APP + 10;
/// The host started, or could not (`STARTED`).
const WM_HOST_STARTED: u32 = WM_APP + 11;
/// The host stopped after Quit.
const WM_HOST_STOPPED: u32 = WM_APP + 12;
/// A second Sill.exe was launched: say where this one is.
const WM_SECOND_LAUNCH: u32 = WM_APP + 13;
/// TEST ONLY (SILL_TEST_ACTIONS=1): a menu action without the menu, so tests never type into the
/// person's screen. wParam: 1 Pair, 2 Show Log, 3 Quit, 4 bitrate (lParam), 5 frame rate
/// (lParam), 6 resolution (lParam 1 or 2), 7 Launch at Login, 8 Allow (firewall), 9 Try Again,
/// 10 Network settings, 11 close an open menu.
const WM_TEST_ACTION: u32 = WM_APP + 20;

const NIN_SELECT: u32 = 0x0400;
const NIN_KEYSELECT: u32 = 0x0401;

const TIMER_TICK: usize = 1;
const TIMER_HARD_EXIT: usize = 2;

const ICON_ID: u32 = 1;
const CLASS: PCWSTR = w!("SillTrayWindow");

/// The host `start()` gave back on its worker thread, for the GUI thread.
static STARTED: Mutex<Option<Result<Host, String>>> = Mutex::new(None);
static TRAY_HWND: AtomicIsize = AtomicIsize::new(0);
/// A device's ask to pair, and when: the menu's "‹device› Wants to Pair" while the pairing window
/// is not showing (5 minutes, as the Mac's).
static PAIRING_ASK: Mutex<Option<(String, Instant)>> = Mutex::new(None);

struct Tray {
    hwnd: HWND,
    platform: Arc<WindowsPlatform>,
    options: HostOptions,
    dir: PathBuf,
    settings: Settings,
    host: Option<Host>,
    handle: Option<Arc<HostHandle>>,
    start_error: Option<String>,
    quitting: bool,
    icon: Option<(HICON, Glyph, u32, bool)>,
    tooltip: String,
    taskbar_created: u32,
    firewall: Firewall,
    login: LoginState,
    /// The menu dump's last text (SILL_TEST_MENU_DUMP).
    dumped: String,
    /// SILL_TEST_ACTIONS=1: WM_TEST_ACTION is honoured.
    test_actions: bool,
    /// When the firewall was last checked (again every minute).
    firewall_checked: Option<Instant>,
}

thread_local! {
    static TRAY: RefCell<Option<Tray>> = const { RefCell::new(None) };
}

fn with_tray<R>(f: impl FnOnce(&mut Tray) -> R) -> Option<R> {
    TRAY.with(|t| match t.try_borrow_mut() {
        Ok(mut g) => g.as_mut().map(f),
        Err(_) => None,
    })
}

/// A pairing event, for the menu's ask row (the pairing window gets its own copy).
pub fn note_pairing(event: &PairingEvent) {
    let mut ask = PAIRING_ASK.lock().unwrap_or_else(|e| e.into_inner());
    match event {
        PairingEvent::Asked { device } => *ask = Some((device.clone(), Instant::now())),
        PairingEvent::Code { asked_by: Some(device), .. } => *ask = Some((device.clone(), Instant::now())),
        PairingEvent::Paired { .. } | PairingEvent::Withdrawn { .. } | PairingEvent::Cancelled | PairingEvent::Stopped => *ask = None,
        _ => {}
    }
}

/// Sill.exe: one per sign-in session; the log, the settings, the icon, then the host. Returns the
/// process's exit code. `login`: started by Windows at sign-in (the Run value's `--login`), so the
/// pairing window waits to be asked for.
pub fn run(login: bool) -> i32 {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        // Menus in the app mode's theme, light or dark (theme.rs).
        super::theme::init_app();
        // One Sill.exe: a second launch asks the first to say where it is, and goes.
        let _mutex = CreateMutexW(None, true, w!("Local\\Sill.Tray.Instance"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            if let Ok(h) = FindWindowW(CLASS, PCWSTR::null()) {
                let _ = PostMessageW(Some(h), WM_SECOND_LAUNCH, WPARAM(0), LPARAM(0));
            }
            return 0;
        }
    }
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("Sill");
    let log_result = crate::applog::install(&local);
    let dir = settings::default_dir().unwrap_or_else(|| local.clone());
    let settings = Settings::load(&dir);
    sill_host_core::log(&format!(
        "Sill {} for Windows starting{} (settings in {}).",
        env!("CARGO_PKG_VERSION"),
        if login { " at sign-in" } else { "" },
        dir.display()
    ));
    if let Err(e) = log_result {
        sill_host_core::log(&format!("The log file could not be opened ({e}); Show Log has this run's lines."));
    }
    super::pairui::set_quiet_start(login);
    let platform = WindowsPlatform::new();
    let options = HostOptions {
        port: settings.port,
        pairing: true,
        persistent: true,
        // The codes are in the pairing window; Sill.log keeps none (as Sill.app's).
        codes_in_log: false,
        config: settings.config(),
        ..HostOptions::default()
    };
    let Some(hwnd) = make_window() else { return 1 };
    let taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
    TRAY.with(|t| {
        *t.borrow_mut() = Some(Tray {
            hwnd,
            platform,
            options,
            dir,
            settings,
            host: None,
            handle: None,
            start_error: None,
            quitting: false,
            icon: None,
            tooltip: String::new(),
            taskbar_created,
            firewall: Firewall::Checking,
            login: LoginState::default(),
            dumped: String::new(),
            test_actions: std::env::var("SILL_TEST_ACTIONS").as_deref() == Ok("1"),
            firewall_checked: None,
        })
    });
    if std::env::var_os("SILL_TEST_ICON_DUMP").is_some() {
        // All four states once, at this taskbar's size and theme.
        let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, tray_dpi()) }.max(16) as usize;
        let ink = if taskbar_is_light() { (0x1A, 0x1A, 0x1A) } else { (0xFF, 0xFF, 0xFF) };
        for g in [Glyph::Idle, Glyph::Connected, Glyph::Streaming, Glyph::Attention] {
            if let Some(icon) = make_icon(g, size, ink) {
                unsafe {
                    let _ = DestroyIcon(icon);
                }
            }
        }
    }
    with_tray(|t| {
        t.add_icon();
        if !t.settings.shown_tray_hint {
            t.balloon("Sill is running", "It’s in the notification area. Open Sill on your iPhone or iPad to connect.");
            t.settings.shown_tray_hint = true;
            t.save();
        }
        t.start_host();
    });
    unsafe {
        SetTimer(Some(hwnd), TIMER_TICK, 1000, None);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    with_tray(|t| t.remove_icon());
    0
}

fn make_window() -> Option<HWND> {
    unsafe {
        let instance: HINSTANCE = GetModuleHandleW(None).ok()?.into();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let hwnd = CreateWindowExW(WS_EX_TOOLWINDOW, CLASS, w!("Sill"), WS_POPUP, 0, 0, 0, 0, None, None, Some(instance), None).ok()?;
        // Its menus may draw dark.
        super::theme::allow_dark(hwnd, true);
        TRAY_HWND.store(hwnd.0 as isize, Ordering::Release);
        Some(hwnd)
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_TRAYICON => {
                let event = (lparam.0 & 0xFFFF) as u32;
                if matches!(event, WM_CONTEXTMENU | NIN_SELECT | NIN_KEYSELECT) {
                    let x = (wparam.0 & 0xFFFF) as u16 as i16 as i32;
                    let y = ((wparam.0 >> 16) & 0xFFFF) as u16 as i16 as i32;
                    show_menu(hwnd, POINT { x, y });
                }
                LRESULT(0)
            }
            WM_TIMER => {
                match wparam.0 {
                    TIMER_TICK => {
                        with_tray(|t| t.tick());
                    }
                    TIMER_HARD_EXIT => {
                        // Quit took too long (a stream that won't stop): go anyway.
                        with_tray(|t| t.remove_icon());
                        std::process::exit(0);
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_HOST_STARTED => {
                let started = STARTED.lock().unwrap_or_else(|e| e.into_inner()).take();
                with_tray(|t| t.host_started(started));
                LRESULT(0)
            }
            WM_HOST_STOPPED => {
                with_tray(|t| t.remove_icon());
                PostQuitMessage(0);
                LRESULT(0)
            }
            WM_TEST_ACTION => {
                if with_tray(|t| t.test_actions) == Some(true) {
                    let n = lparam.0 as i64;
                    let action = match wparam.0 {
                        1 => Some(Action::ShowPairing),
                        2 => Some(Action::ShowLog),
                        3 => Some(Action::Quit),
                        4 => Some(Action::SetBitrate(n)),
                        5 => Some(Action::SetMaxFps(n)),
                        6 => Some(Action::SetCaptureScale(n as f64)),
                        7 => Some(Action::ToggleLaunchAtLogin),
                        8 => Some(Action::AllowFirewall),
                        9 => Some(Action::TryAgain),
                        10 => Some(Action::OpenNetworkSettings),
                        11 => {
                            let _ = windows::Win32::UI::WindowsAndMessaging::EndMenu();
                            None
                        }
                        _ => None,
                    };
                    if let Some(action) = action {
                        sill_host_core::log(&format!("Test action: {action:?}"));
                        with_tray(|t| t.perform(action));
                    }
                }
                LRESULT(0)
            }
            WM_SECOND_LAUNCH => {
                with_tray(|t| t.balloon("Sill is already running", "It’s in the notification area: click its icon for the menu."));
                LRESULT(0)
            }
            WM_QUERYENDSESSION => LRESULT(1),
            WM_ENDSESSION => {
                if wparam.0 != 0 {
                    // Signing out or shutting down: goodbye to every device, within 2 s (Windows may
                    // end the process as soon as this returns).
                    let host = with_tray(|t| {
                        t.quitting = true;
                        t.remove_icon();
                        if let Some(h) = &t.handle {
                            h.quit();
                        }
                        t.host.take()
                    })
                    .flatten();
                    if let Some(host) = host {
                        let (tx, rx) = std::sync::mpsc::channel();
                        std::thread::spawn(move || {
                            let _ = host.join();
                            let _ = tx.send(());
                        });
                        let _ = rx.recv_timeout(Duration::from_secs(2));
                    }
                }
                LRESULT(0)
            }
            WM_DISPLAYCHANGE | WM_SETTINGCHANGE => {
                // A new DPI or taskbar theme: the glyph is drawn again at the next tick; a new app
                // mode: the next menu takes it.
                if msg == WM_SETTINGCHANGE && super::theme::is_theme_change(lparam) {
                    super::theme::refresh_menus();
                }
                with_tray(|t| t.icon_stale());
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            _ => {
                let taskbar_created = with_tray(|t| t.taskbar_created).unwrap_or(0);
                if taskbar_created != 0 && msg == taskbar_created {
                    // Explorer restarted: the icon is gone; add it again.
                    with_tray(|t| {
                        t.icon = None;
                        t.add_icon();
                    });
                    return LRESULT(0);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
    }
}

/// Builds the menu from what is true now, shows it at `at` (or the cursor), and does what was
/// picked once it closes. Nothing of the tray's state is borrowed while it is open.
fn show_menu(hwnd: HWND, mut at: POINT) {
    let Some((entries, _)) = with_tray(|t| t.menu()) else { return };
    let mut actions = Vec::new();
    let Some(menu) = build_menu(&entries, &mut actions) else { return };
    unsafe {
        if at.x == 0 && at.y == 0 {
            let _ = GetCursorPos(&mut at);
        }
        let align = if GetSystemMetrics(SM_MENUDROPALIGNMENT) != 0 { TPM_RIGHTALIGN } else { TPM_LEFTALIGN };
        // The menu closes on a click elsewhere only while its window is in front.
        let _ = SetForegroundWindow(hwnd);
        let picked = TrackPopupMenuEx(menu, (TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | align).0, at.x, at.y, hwnd, None);
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        let id = picked.0 as usize;
        if id >= 1 && id <= actions.len() {
            let action = actions[id - 1];
            with_tray(|t| t.perform(action));
        }
    }
}

/// The entries as a Win32 menu; each item's command ID is its place in `actions`, plus one.
fn build_menu(entries: &[Entry], actions: &mut Vec<Action>) -> Option<HMENU> {
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        for e in entries {
            if e.kind == Kind::Separator {
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                continue;
            }
            let mut text = if e.attention { format!("⚠  {}", e.title) } else { e.title.clone() };
            if let Some(d) = &e.detail {
                text.push('\t');
                text.push_str(d);
            }
            let text_w = wide(&text);
            let mut flags = MF_STRING;
            if e.checked {
                flags |= MF_CHECKED;
            }
            if !e.enabled || e.kind == Kind::Line {
                flags |= MF_GRAYED;
            }
            if e.children.is_empty() {
                actions.push(e.action);
                let id = actions.len();
                let _ = AppendMenuW(menu, flags, id, PCWSTR(text_w.as_ptr()));
                if e.kind == Kind::Header {
                    // The card's first line in bold.
                    let _ = SetMenuDefaultItem(menu, id as u32, 0);
                }
            } else if let Some(sub) = build_menu(&e.children, actions) {
                let _ = AppendMenuW(menu, flags | MF_POPUP, sub.0 as usize, PCWSTR(text_w.as_ptr()));
            }
        }
        Some(menu)
    }
}

impl Tray {
    fn start_host(&mut self) {
        self.start_error = None;
        let platform: Arc<dyn sill_host_core::platform::Platform> = self.platform.clone();
        let options = self.options.clone();
        let hwnd = self.hwnd.0 as isize;
        std::thread::Builder::new()
            .name("sill-start".into())
            .spawn(move || {
                let result = sill_host_core::start(platform, options);
                *STARTED.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
                unsafe {
                    let _ = PostMessageW(Some(HWND(hwnd as _)), WM_HOST_STARTED, WPARAM(0), LPARAM(0));
                }
            })
            .ok();
    }

    fn host_started(&mut self, started: Option<Result<Host, String>>) {
        match started {
            Some(Ok(host)) => {
                sill_host_core::log("Sill is running: pick a window or the desktop on the device.");
                self.handle = Some(host.handle());
                self.host = Some(host);
            }
            Some(Err(e)) => {
                sill_host_core::log(&format!("Sill couldn’t start: {e}"));
                self.start_error = Some(friendly_error(&e, self.options.port));
            }
            None => {}
        }
        self.tick();
    }

    fn perform(&mut self, action: Action) {
        let change = |c: ConfigChange, t: &Tray| {
            if let Some(h) = &t.handle {
                h.change(c);
            }
        };
        match action {
            Action::None => {}
            Action::SetMaxFps(v) => change(ConfigChange { max_fps: Some(v), ..Default::default() }, self),
            Action::SetBitrate(v) => change(ConfigChange { bitrate: Some(v), ..Default::default() }, self),
            Action::SetCaptureScale(v) => change(ConfigChange { capture_scale: Some(v), ..Default::default() }, self),
            Action::ShowPairing => super::pairui::show(),
            Action::ShowLog => super::logwindow::show(),
            Action::ToggleLaunchAtLogin => {
                if let Err(e) = super::login::set(!super::login::state().on) {
                    sill_host_core::log(&format!("Launch at Login could not be changed: {e}"));
                }
            }
            Action::OpenStartupSettings => super::login::open_startup_settings(),
            Action::TryAgain => self.start_host(),
            Action::Quit => self.quit(),
            Action::AllowFirewall => super::firewall::allow(self.options.port),
            // Settings › Network & internet, where a network's profile (Public, Private) is set.
            Action::OpenNetworkSettings => open_path("ms-settings:network-status"),
        }
        self.tick();
    }

    fn quit(&mut self) {
        if self.quitting {
            return;
        }
        self.quitting = true;
        sill_host_core::log("Quitting: goodbye to every device.");
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_HARD_EXIT, 3000, None);
        }
        let hwnd = self.hwnd.0 as isize;
        match (self.handle.take(), self.host.take()) {
            (Some(handle), Some(host)) => {
                handle.quit();
                std::thread::spawn(move || {
                    let _ = host.join();
                    unsafe {
                        let _ = PostMessageW(Some(HWND(hwnd as _)), WM_HOST_STOPPED, WPARAM(0), LPARAM(0));
                    }
                });
            }
            _ => unsafe {
                let _ = PostMessageW(Some(self.hwnd), WM_HOST_STOPPED, WPARAM(0), LPARAM(0));
            },
        }
    }

    /// The menu as it would open now, and the card it starts with.
    fn menu(&mut self) -> (Vec<Entry>, traymenu::Presentation) {
        let status = self.handle.as_ref().map(|h| h.status());
        let ask = pairing_ask();
        self.login = super::login::state();
        self.firewall = super::firewall::current();
        let input = TrayInput {
            status: status.as_ref(),
            start_error: self.start_error.as_deref(),
            pairing_request: ask.as_deref(),
            firewall: self.firewall.clone(),
            login: self.login.clone(),
        };
        let p = traymenu::present(&input);
        (traymenu::entries(&input, &p), p)
    }

    /// Once a second: the glyph and the tooltip; the settings saved when a device (or the menu)
    /// changed them; the test dump.
    fn tick(&mut self) {
        if self.quitting {
            return;
        }
        if self.firewall_checked.is_none_or(|t| t.elapsed() >= Duration::from_secs(60)) {
            self.firewall_checked = Some(Instant::now());
            super::firewall::refresh(self.options.port);
        }
        if let Some(status) = self.handle.as_ref().map(|h| h.status()) {
            if self.settings.take(&status.config) {
                self.save();
            }
        }
        let (entries, p) = self.menu();
        self.set_icon(p.glyph, &p.tooltip);
        if let Some(path) = std::env::var_os("SILL_TEST_MENU_DUMP") {
            let text = traymenu::dump(&entries);
            if text != self.dumped {
                let _ = std::fs::write(path, &text);
                self.dumped = text;
            }
        }
    }

    fn save(&self) {
        if let Err(e) = self.settings.save(&self.dir) {
            sill_host_core::log(&format!("Settings could not be saved ({e})."));
        }
    }

    fn notify_data(&self) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW { cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32, hWnd: self.hwnd, uID: ICON_ID, ..Default::default() }
    }

    fn add_icon(&mut self) {
        let (glyph, tooltip) = (Glyph::Idle, "Sill — Starting…".to_string());
        let icon = self.glyph_icon(glyph);
        let mut d = self.notify_data();
        d.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        d.uCallbackMessage = WM_TRAYICON;
        d.hIcon = icon.0;
        copy_into(&mut d.szTip, &tooltip);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_ADD, &d);
            d.Anonymous = NOTIFYICONDATAW_0 { uVersion: NOTIFYICON_VERSION_4 };
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &d);
        }
        self.icon = Some(icon);
        self.tooltip = tooltip;
    }

    fn remove_icon(&mut self) {
        let d = self.notify_data();
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &d);
        }
    }

    /// The glyph is drawn again at the next change (a new DPI or taskbar theme).
    fn icon_stale(&mut self) {
        if let Some(icon) = self.icon.as_mut() {
            icon.2 = 0;
        }
    }

    fn set_icon(&mut self, glyph: Glyph, tooltip: &str) {
        let (dpi, light) = (tray_dpi(), taskbar_is_light());
        let same_icon = self.icon.map(|(_, g, d, l)| g == glyph && d == dpi && l == light).unwrap_or(false);
        if same_icon && tooltip == self.tooltip {
            return;
        }
        let mut d = self.notify_data();
        d.uFlags = NIF_TIP | NIF_SHOWTIP;
        copy_into(&mut d.szTip, tooltip);
        let old = if same_icon {
            None
        } else {
            let icon = self.glyph_icon(glyph);
            d.uFlags |= NIF_ICON;
            d.hIcon = icon.0;
            self.icon.replace(icon)
        };
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
            if let Some((h, ..)) = old {
                let _ = DestroyIcon(h);
            }
        }
        self.tooltip = tooltip.to_string();
    }

    fn glyph_icon(&self, glyph: Glyph) -> (HICON, Glyph, u32, bool) {
        let dpi = tray_dpi();
        let light = taskbar_is_light();
        let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, dpi) }.max(16) as usize;
        let ink = if light { (0x1A, 0x1A, 0x1A) } else { (0xFF, 0xFF, 0xFF) };
        (make_icon(glyph, size, ink).unwrap_or_default(), glyph, dpi, light)
    }

    fn balloon(&self, title: &str, text: &str) {
        let mut d = self.notify_data();
        d.uFlags = NIF_INFO;
        d.dwInfoFlags = NIIF_INFO;
        copy_into(&mut d.szInfoTitle, title);
        copy_into(&mut d.szInfo, text);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &d);
        }
    }
}

/// The ask row's device: asked in the last 5 minutes while the pairing window is not showing.
fn pairing_ask() -> Option<String> {
    let mut ask = PAIRING_ASK.lock().unwrap_or_else(|e| e.into_inner());
    if ask.as_ref().is_some_and(|(_, at)| at.elapsed() > Duration::from_secs(300)) {
        *ask = None;
    }
    ask.as_ref().filter(|_| !super::pairui::visible()).map(|(d, _)| d.clone())
}

/// What a bind error means to the person.
fn friendly_error(e: &str, port: u16) -> String {
    let lower = e.to_ascii_lowercase();
    if lower.contains("10048") || lower.contains("in use") {
        format!("Port {port} is in use (another Sill or the console host may be running).")
    } else if lower.contains("10013") || lower.contains("access") {
        format!("Windows keeps port {port} for something else (Hyper-V or WSL can reserve ports).")
    } else {
        e.to_string()
    }
}

/// The DPI of the primary display, where Windows 11 keeps the notification area.
fn tray_dpi() -> u32 {
    unsafe {
        let monitor = MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY);
        let (mut x, mut y) = (96u32, 96u32);
        if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y).is_err() {
            return 96;
        }
        x.max(96)
    }
}

/// The taskbar's theme (SystemUsesLightTheme; apps have their own, AppsUseLightTheme).
fn taskbar_is_light() -> bool {
    let mut value: u32 = 0;
    let mut size = std::mem::size_of::<u32>() as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    r.is_ok() && value != 0
}

/// The glyph as an icon: a 32-bit DIB with alpha (the glyph in `ink`, the badge orange).
fn make_icon(state: Glyph, size: usize, ink: (u8, u8, u8)) -> Option<HICON> {
    let masks = glyph::draw(state, size);
    let orange = (0xF7u8, 0x63u8, 0x0Cu8); // #F7630C, Windows' warning orange
    unsafe {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size as i32,
                biHeight: -(size as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let color: HBITMAP = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        let px = std::slice::from_raw_parts_mut(bits as *mut u8, size * size * 4);
        for i in 0..size * size {
            let (g, b) = (masks.glyph[i], masks.badge[i]);
            let (rgb, a) = if b > 0 { (orange, b) } else { (ink, g) };
            px[i * 4] = rgb.2;
            px[i * 4 + 1] = rgb.1;
            px[i * 4 + 2] = rgb.0;
            px[i * 4 + 3] = a;
        }
        // TEST ONLY: SILL_TEST_ICON_DUMP=<dir> keeps each icon made as a PNG (it hides in the
        // taskbar's overflow, where no screenshot reaches it).
        if let Some(dir) = std::env::var_os("SILL_TEST_ICON_DUMP") {
            let rgba: Vec<u8> = px.chunks(4).flat_map(|p| [p[2], p[1], p[0], p[3]]).collect();
            if let Some(png) = crate::imaging::png_rgba(size as u32, size as u32, &rgba) {
                let _ = std::fs::write(PathBuf::from(dir).join(format!("icon-{state:?}-{size}.png")), png);
            }
        }
        let mask_bits = vec![0u8; size.div_ceil(16) * 2 * size];
        let mask = CreateBitmap(size as i32, size as i32, 1, 1, Some(mask_bits.as_ptr() as *const _));
        let icon = CreateIconIndirect(&ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color });
        let _ = DeleteObject(HGDIOBJ(color.0));
        let _ = DeleteObject(HGDIOBJ(mask.0));
        icon.ok()
    }
}

/// `text` into a fixed UTF-16 field, cut to fit with its terminating NUL.
fn copy_into(field: &mut [u16], text: &str) {
    let w: Vec<u16> = text.encode_utf16().take(field.len() - 1).collect();
    field[..w.len()].copy_from_slice(&w);
    field[w.len()] = 0;
}

/// Opens a file or folder as Explorer would (the log in the text editor).
fn open_path(path: &str) {
    let p = wide(path);
    unsafe {
        ShellExecuteW(None, w!("open"), PCWSTR(p.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}
