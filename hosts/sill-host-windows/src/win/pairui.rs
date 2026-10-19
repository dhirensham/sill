//! The pairing window, "Pair iPhone or iPad" (Sill.app's PairDeviceWindow): the QR code of the
//! `sill://pair` link, the 12-digit code, the countdown, a device's ask, a wrong code, "Paired".
//! What it shows and when it comes up is `crate::pairview` (portable, tested); this draws it.
//!
//! One window per process, on a UI thread of its own made at the first pairing event: the core's
//! threads queue events and post a message, never touching the window themselves. The words are
//! STATIC controls (Narrator reads them), the QR code is painted, the buttons are the system's
//! (Common Controls v6 through the binary's manifest, build.rs). Closing it only hides it: the
//! host's own code keeps working, as the console's does; a window a device's ask opened closes
//! with Cancel (that code stops, and the device stays quiet for 10 minutes).

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex, Once, OnceLock};
use std::time::Instant;

use qrcodegen::{QrCode, QrCodeEcc};
use sill_host_core::platform::{PairingControl, PairingEvent};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect, GetDC, GetMonitorInfoW,
    GetStockObject, MonitorFromPoint, ReleaseDC, SelectObject, SetBkColor, SetTextColor, CLEARTYPE_QUALITY,
    CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DT_CALCRECT, DT_NOPREFIX, DT_SINGLELINE, DT_WORDBREAK, FF_DONTCARE,
    FONT_CHARSET, FW_NORMAL, FW_SEMIBOLD, HBRUSH, HDC, HFONT, HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTOPRIMARY,
    OUT_DEFAULT_PRECIS, PAINTSTRUCT, WHITE_BRUSH,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{InitCommonControlsEx, ICC_STANDARD_CLASSES, INITCOMMONCONTROLSEX};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, FlashWindowEx, GetDlgCtrlID, GetMessageW, GetWindowRect,
    IsDialogMessageW, IsWindowVisible, KillTimer, LoadCursorW, PostMessageW, RegisterClassExW, SendMessageW,
    SetForegroundWindow, SetTimer, SetWindowPos, SetWindowTextW, ShowWindow, TranslateMessage, BS_DEFPUSHBUTTON,
    BS_PUSHBUTTON, CS_HREDRAW, CS_VREDRAW, FLASHWINFO, FLASHW_TIMERNOFG, FLASHW_TRAY, HMENU, HWND_NOTOPMOST,
    HWND_TOPMOST, IDC_ARROW, MSG, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
    SW_HIDE, SW_SHOWNOACTIVATE, SW_SHOWNORMAL, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_COMMAND,
    WM_CTLCOLORBTN, WM_CTLCOLORSTATIC, WM_DPICHANGED, WM_ERASEBKGND, WM_PAINT, WM_SETFONT, WM_TIMER, WNDCLASSEXW,
    WS_CAPTION, WS_CHILD, WS_EX_CONTROLPARENT, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP,
};

use super::util::{log, wide};
use crate::pairview::{Action, Buttons, PairView};

/// Posted by the core's threads: events wait in the queue.
const WM_EVENTS: u32 = WM_APP + 1;
/// Lay the window out again (after a DPI change that came while it was busy).
const WM_RELAYOUT: u32 = WM_APP + 2;
/// Show the window (Sill.exe's "Pair iPhone or iPad…"), with a fresh code when none is open.
const WM_SHOW: u32 = WM_APP + 3;

const TIMER_TICK: usize = 1;
const TIMER_LINGER: usize = 2;

const ID_HEADLINE: i32 = 10;
const ID_BODY: i32 = 11;
const ID_SCAN: i32 = 12;
const ID_CODE_LABEL: i32 = 13;
const ID_CODE: i32 = 14;
const ID_COUNTDOWN: i32 = 15;
const ID_WARNING: i32 = 16;
const ID_NOTE0: i32 = 17;
const ID_NOTE1: i32 = 18;
const ID_LEFT: i32 = 20;
const ID_RIGHT: i32 = 21;
/// What IsDialogMessage sends for Return and Esc.
const IDOK: i32 = 1;
const IDCANCEL: i32 = 2;

/// STATIC styles (winuser.h; windows-rs keeps them under System_SystemServices).
const SS_LEFT: u32 = 0x0;
const SS_NOPREFIX: u32 = 0x80;

const STATICS: [i32; 9] = [ID_HEADLINE, ID_BODY, ID_SCAN, ID_CODE_LABEL, ID_CODE, ID_COUNTDOWN, ID_WARNING, ID_NOTE0, ID_NOTE1];

// Colours (COLORREF is 0x00BBGGRR).
const INK: u32 = 0x001A1A1A;
const SECONDARY: u32 = 0x005D5D5D;
const FAINT: u32 = 0x00BDBDBD;
const WARNING: u32 = 0x00005CB8; // #B85C00
const SUCCESS: u32 = 0x000F7B0F; // #0F7B0F
const QR_DARK: u32 = 0x00000000;
const QR_SPENT: u32 = 0x00D9D9D9;

/// What other threads share with the UI thread.
struct Shared {
    queue: Mutex<VecDeque<(PairingEvent, Arc<dyn PairingControl>)>>,
    hwnd: AtomicIsize,
}

fn shared() -> &'static Shared {
    static SHARED: OnceLock<Shared> = OnceLock::new();
    SHARED.get_or_init(|| Shared { queue: Mutex::new(VecDeque::new()), hwnd: AtomicIsize::new(0) })
}

/// From any thread: the window shows this change (made on the first call).
pub fn post(event: PairingEvent, control: Arc<dyn PairingControl>) {
    let s = shared();
    if let Ok(mut q) = s.queue.lock() {
        q.push_back((event, control));
    }
    start_thread();
    // Before the window exists the UI thread drains the queue itself once it does.
    let h = s.hwnd.load(Ordering::Acquire);
    if h != 0 {
        unsafe {
            let _ = PostMessageW(Some(HWND(h as _)), WM_EVENTS, WPARAM(0), LPARAM(0));
        }
    }
}

fn start_thread() {
    static START: Once = Once::new();
    START.call_once(|| {
        if let Err(e) = std::thread::Builder::new().name("sill-pairing-window".into()).spawn(ui_thread) {
            log(&format!("Pairing window: could not start its thread ({e}); the code is on the console."));
        }
    });
}

/// Started at sign-in (Sill.exe's `--login`): the window does not come up by itself at the first
/// code, only when asked for (a device's ask, the menu).
static QUIET_START: AtomicBool = AtomicBool::new(false);
/// "Pair iPhone or iPad…" came before the window existed.
static SHOW_WHEN_MADE: AtomicBool = AtomicBool::new(false);

pub fn set_quiet_start(on: bool) {
    QUIET_START.store(on, Ordering::Release);
}

/// From any thread: the window comes up, in front (the menu's "Pair iPhone or iPad…"), showing
/// the open code or, when none is (cancelled, stopped), a fresh one.
pub fn show() {
    unsafe {
        // The tray's menu was in front: let the window's thread take the foreground.
        let _ = windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(u32::MAX);
    }
    start_thread();
    let h = shared().hwnd.load(Ordering::Acquire);
    if h == 0 {
        SHOW_WHEN_MADE.store(true, Ordering::Release);
        return;
    }
    unsafe {
        let _ = PostMessageW(Some(HWND(h as _)), WM_SHOW, WPARAM(0), LPARAM(0));
    }
}

/// Whether the window shows now.
pub fn visible() -> bool {
    let h = shared().hwnd.load(Ordering::Acquire);
    h != 0 && unsafe { IsWindowVisible(HWND(h as _)).as_bool() }
}

/// Everything the window holds, on its own thread.
struct Ui {
    hwnd: HWND,
    view: PairView,
    control: Option<Arc<dyn PairingControl>>,
    dpi: u32,
    fonts: Fonts,
    children: HashMap<i32, HWND>,
    placed: bool,
    /// The menu asked for the window while no code was open: show the fresh one when it comes.
    show_on_code: bool,
}

/// The QR code and where it is painted (read by WM_PAINT alone).
struct QrPaint {
    code: QrCode,
    rect: RECT,
    spent: bool,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
    static QR: RefCell<Option<QrPaint>> = const { RefCell::new(None) };
    /// Each STATIC's text colour (read by WM_CTLCOLORSTATIC, which can come while `UI` is busy).
    static COLORS: RefCell<HashMap<i32, u32>> = RefCell::new(HashMap::new());
}

struct Fonts {
    headline: HFONT,
    body: HFONT,
    small: HFONT,
    mono: HFONT,
}

impl Fonts {
    fn new(dpi: u32) -> Fonts {
        let px = |dip: i32| -((dip * dpi as i32 + 48) / 96);
        unsafe {
            let make = |height: i32, weight: i32, face: PCWSTR| {
                CreateFontW(
                    height,
                    0,
                    0,
                    0,
                    weight,
                    0,
                    0,
                    0,
                    FONT_CHARSET(DEFAULT_CHARSET.0),
                    OUT_DEFAULT_PRECIS,
                    CLIP_DEFAULT_PRECIS,
                    CLEARTYPE_QUALITY,
                    FF_DONTCARE.0 as u32,
                    face,
                )
            };
            Fonts {
                headline: make(px(18), FW_SEMIBOLD.0 as i32, w!("Segoe UI")),
                body: make(px(14), FW_NORMAL.0 as i32, w!("Segoe UI")),
                small: make(px(12), FW_NORMAL.0 as i32, w!("Segoe UI")),
                mono: make(px(28), FW_NORMAL.0 as i32, w!("Consolas")),
            }
        }
    }

    fn delete(&self) {
        unsafe {
            for f in [self.headline, self.body, self.small, self.mono] {
                let _ = DeleteObject(HGDIOBJ(f.0));
            }
        }
    }
}

fn ui_thread() {
    unsafe {
        let icc = INITCOMMONCONTROLSEX { dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: ICC_STANDARD_CLASSES };
        let _ = InitCommonControlsEx(&icc);
        let instance: HINSTANCE = match GetModuleHandleW(None) {
            Ok(m) => m.into(),
            Err(e) => {
                log(&format!("Pairing window: no module handle ({e}); the code is on the console."));
                return;
            }
        };
        let class = w!("SillPairingWindow");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            // Sill.exe's icon (resource 1); the console host has none, and gets Windows' own.
            hIcon: windows::Win32::UI::WindowsAndMessaging::LoadIconW(Some(instance), PCWSTR(1 as _)).unwrap_or_default(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let hwnd = match CreateWindowExW(
            WS_EX_CONTROLPARENT,
            class,
            w!("Pair iPhone or iPad"),
            style,
            0,
            0,
            440,
            600,
            None,
            None,
            Some(instance),
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                log(&format!("Pairing window: could not be made ({e}); the code is on the console."));
                return;
            }
        };
        let dpi = GetDpiForWindow(hwnd).max(96);
        let fonts = Fonts::new(dpi);
        let mut children = HashMap::new();
        for id in STATICS {
            let h = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("STATIC"),
                w!(""),
                WS_CHILD | WINDOW_STYLE(SS_LEFT | SS_NOPREFIX),
                0,
                0,
                0,
                0,
                Some(hwnd),
                Some(HMENU(id as isize as _)),
                Some(instance),
                None,
            );
            if let Ok(h) = h {
                children.insert(id, h);
            }
        }
        for (id, style) in [(ID_LEFT, BS_PUSHBUTTON), (ID_RIGHT, BS_DEFPUSHBUTTON)] {
            let h = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("BUTTON"),
                w!(""),
                WS_CHILD | WS_TABSTOP | WINDOW_STYLE(style as u32),
                0,
                0,
                0,
                0,
                Some(hwnd),
                Some(HMENU(id as isize as _)),
                Some(instance),
                None,
            );
            if let Ok(h) = h {
                children.insert(id, h);
            }
        }
        UI.with(|ui| {
            *ui.borrow_mut() = Some(Ui {
                hwnd,
                view: PairView::new(""),
                control: None,
                dpi,
                fonts,
                children,
                placed: false,
                show_on_code: false,
            })
        });
        shared().hwnd.store(hwnd.0 as isize, Ordering::Release);
        // Whatever queued before the window existed.
        let _ = PostMessageW(Some(hwnd), WM_EVENTS, WPARAM(0), LPARAM(0));
        if SHOW_WHEN_MADE.swap(false, Ordering::AcqRel) {
            let _ = PostMessageW(Some(hwnd), WM_SHOW, WPARAM(0), LPARAM(0));
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if !IsDialogMessageW(hwnd, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_EVENTS => {
                drain();
                LRESULT(0)
            }
            WM_SHOW => {
                with_ui(|ui| {
                    if ui.view.counting() {
                        ui.act(Action::Show { activate: true });
                    } else if let Some(c) = ui.control.clone() {
                        // The core opens a fresh code; its event shows the window (`drain`).
                        ui.show_on_code = true;
                        c.new_code();
                    }
                });
                LRESULT(0)
            }
            WM_RELAYOUT => {
                with_ui(|ui| ui.layout());
                LRESULT(0)
            }
            WM_TIMER => {
                match wparam.0 {
                    TIMER_TICK => {
                        with_ui(|ui| ui.tick());
                    }
                    TIMER_LINGER => {
                        let _ = KillTimer(Some(hwnd), TIMER_LINGER);
                        with_ui(|ui| {
                            let action = ui.view.linger_ended();
                            ui.act(action);
                        });
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_COMMAND => {
                let id = (wparam.0 & 0xFFFF) as i32;
                with_ui(|ui| ui.command(id));
                LRESULT(0)
            }
            WM_CLOSE => {
                with_ui(|ui| ui.command(IDCANCEL));
                LRESULT(0)
            }
            WM_DPICHANGED => {
                let dpi = (wparam.0 & 0xFFFF) as u32;
                let suggested = *(lparam.0 as *const RECT);
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    suggested.left,
                    suggested.top,
                    suggested.right - suggested.left,
                    suggested.bottom - suggested.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                let done = with_ui(|ui| {
                    ui.fonts.delete();
                    ui.dpi = dpi.max(96);
                    ui.fonts = Fonts::new(ui.dpi);
                    ui.layout();
                });
                if done.is_none() {
                    let _ = PostMessageW(Some(hwnd), WM_RELAYOUT, WPARAM(0), LPARAM(0));
                }
                LRESULT(0)
            }
            WM_ERASEBKGND => {
                let hdc = HDC(wparam.0 as _);
                let mut r = RECT::default();
                let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut r);
                FillRect(hdc, &r, HBRUSH(GetStockObject(WHITE_BRUSH).0));
                LRESULT(1)
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                QR.with(|qr| {
                    if let Some(q) = qr.borrow().as_ref() {
                        paint_qr(hdc, q);
                    }
                });
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_CTLCOLORSTATIC => {
                let hdc = HDC(wparam.0 as _);
                let id = GetDlgCtrlID(HWND(lparam.0 as _));
                let color = COLORS.with(|c| c.borrow().get(&id).copied()).unwrap_or(INK);
                SetTextColor(hdc, COLORREF(color));
                SetBkColor(hdc, COLORREF(0x00FFFFFF));
                LRESULT(GetStockObject(WHITE_BRUSH).0 as isize)
            }
            WM_CTLCOLORBTN => LRESULT(GetStockObject(WHITE_BRUSH).0 as isize),
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// Runs `f` on the window's state unless it is already in use (a message sent while another is
/// handled); None then.
fn with_ui<R>(f: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    UI.with(|ui| match ui.try_borrow_mut() {
        Ok(mut guard) => guard.as_mut().map(f),
        Err(_) => None,
    })
}

/// The queued events, in order.
fn drain() {
    loop {
        let next = shared().queue.lock().ok().and_then(|mut q| q.pop_front());
        let Some((event, control)) = next else { return };
        let handled = with_ui(|ui| {
            ui.control = Some(control.clone());
            let visible = unsafe { IsWindowVisible(ui.hwnd).as_bool() };
            let is_code = matches!(event, PairingEvent::Code { .. });
            let mut action = ui.view.apply(event.clone(), Instant::now(), visible);
            if is_code && ui.show_on_code {
                ui.show_on_code = false;
                action = Action::Show { activate: true };
            } else if action == (Action::Show { activate: true }) && QUIET_START.load(Ordering::Acquire) {
                action = Action::Refresh;
            }
            ui.act(action);
        });
        if handled.is_none() {
            // Busy: put it back and try again from the message loop.
            if let Ok(mut q) = shared().queue.lock() {
                q.push_front((event, control));
            }
            let h = shared().hwnd.load(Ordering::Acquire);
            unsafe {
                let _ = PostMessageW(Some(HWND(h as _)), WM_EVENTS, WPARAM(0), LPARAM(0));
            }
            return;
        }
    }
}

impl Ui {
    fn act(&mut self, action: Action) {
        let hwnd = self.hwnd;
        unsafe {
            match action {
                Action::None => {}
                Action::Refresh => {
                    if IsWindowVisible(hwnd).as_bool() {
                        self.layout();
                    }
                }
                Action::Show { activate } => {
                    let was_visible = IsWindowVisible(hwnd).as_bool();
                    self.layout();
                    if !self.placed {
                        self.center();
                    }
                    if activate {
                        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
                        let _ = SetForegroundWindow(hwnd);
                    } else {
                        if !was_visible {
                            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                        }
                        // In front of other windows without taking the keyboard from them.
                        let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
                        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, flags);
                        let _ = SetWindowPos(hwnd, Some(HWND_NOTOPMOST), 0, 0, 0, 0, flags);
                        let flash = FLASHWINFO {
                            cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
                            hwnd,
                            dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG,
                            uCount: 3,
                            dwTimeout: 0,
                        };
                        let _ = FlashWindowEx(&flash);
                    }
                    self.ticking();
                }
                Action::Hide => {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                    let _ = KillTimer(Some(hwnd), TIMER_TICK);
                    let _ = KillTimer(Some(hwnd), TIMER_LINGER);
                    self.layout();
                }
                Action::HideAfter(after) => {
                    self.layout();
                    let _ = KillTimer(Some(hwnd), TIMER_TICK);
                    SetTimer(Some(hwnd), TIMER_LINGER, after.as_millis() as u32, None);
                }
            }
        }
    }

    /// The countdown runs once a second while a code shows.
    fn ticking(&self) {
        unsafe {
            if self.view.counting() {
                SetTimer(Some(self.hwnd), TIMER_TICK, 1000, None);
            } else {
                let _ = KillTimer(Some(self.hwnd), TIMER_TICK);
            }
        }
    }

    fn tick(&mut self) {
        let content = self.view.content(Instant::now());
        if let (Some(text), Some(h)) = (content.countdown, self.children.get(&ID_COUNTDOWN)) {
            set_text(*h, &text);
        }
        self.ticking();
    }

    fn command(&mut self, id: i32) {
        let buttons = self.view.content(Instant::now()).buttons;
        let what = match (id, buttons) {
            (ID_LEFT, Buttons::NewCodeClose) => "new",
            (ID_RIGHT | IDOK, Buttons::Cancel) | (IDCANCEL, Buttons::Cancel) => "cancel",
            (ID_RIGHT | IDOK | IDCANCEL, _) => "close",
            _ => return,
        };
        match what {
            "new" => {
                // The core opens a fresh code; its event redraws the window.
                if let Some(c) = &self.control {
                    c.new_code();
                }
            }
            "cancel" => {
                if let Some(c) = &self.control {
                    c.cancel();
                }
                self.act(Action::Hide);
            }
            _ => {
                let action = if matches!(self.view.phase, crate::pairview::Phase::Paired(_)) {
                    self.view.linger_ended()
                } else {
                    Action::Hide
                };
                self.act(action);
            }
        }
    }

    /// In the middle of the primary display's work area, the first time it shows.
    fn center(&mut self) {
        unsafe {
            let monitor = MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY);
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            if !GetMonitorInfoW(monitor, &mut info).as_bool() {
                return;
            }
            let mut r = RECT::default();
            let _ = GetWindowRect(self.hwnd, &mut r);
            let (w, h) = (r.right - r.left, r.bottom - r.top);
            let work = info.rcWork;
            let x = work.left + ((work.right - work.left) - w) / 2;
            let y = work.top + ((work.bottom - work.top) - h) / 3;
            let _ = SetWindowPos(self.hwnd, None, x, y.max(work.top), 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
            self.placed = true;
        }
    }

    /// Puts the content in the controls and sizes the window around it.
    fn layout(&mut self) {
        let content = self.view.content(Instant::now());
        let s = |dip: i32| (dip * self.dpi as i32 + 48) / 96;
        let width = s(440);
        let pad = s(20);
        let inner = width - 2 * pad;
        let gap = s(10);
        let hwnd = self.hwnd;
        let hdc = unsafe { GetDC(Some(hwnd)) };
        let mut y = pad;
        let mut colors = HashMap::new();

        put(
            self,
            hdc,
            &mut colors,
            ID_HEADLINE,
            content.headline.as_deref(),
            self.fonts.headline,
            if content.success { SUCCESS } else { INK },
            pad,
            inner,
            &mut y,
            s(6),
        );
        put(self, hdc, &mut colors, ID_BODY, content.body.as_deref(), self.fonts.body, INK, pad, inner, &mut y, gap);

        // The QR code: whole modules, a quiet zone of four, centred.
        let qr = content.qr.as_ref().and_then(|(text, spent)| QrCode::encode_text(text, QrCodeEcc::Medium).ok().map(|c| (c, *spent)));
        let qr_paint = qr.map(|(code, spent)| {
            let modules = code.size() + 8;
            let module = (s(200) / modules).max(2);
            let side = module * modules;
            let x = (width - side) / 2;
            let rect = RECT { left: x, top: y, right: x + side, bottom: y + side };
            y += side + gap;
            QrPaint { code, rect, spent }
        });
        QR.with(|q| *q.borrow_mut() = qr_paint);

        put(self, hdc, &mut colors, ID_SCAN, content.scan_hint.as_deref(), self.fonts.body, INK, pad, inner, &mut y, s(6));

        // "Code" and the digits on one row.
        let code_label = self.children.get(&ID_CODE_LABEL).copied();
        let code_ctl = self.children.get(&ID_CODE).copied();
        match (&content.code, code_label, code_ctl) {
            (Some((digits, spent)), Some(label), Some(code)) => {
                let label_text = "Code";
                let label_w = measure_width(hdc, self.fonts.body, label_text) + s(2);
                let code_h = measure(hdc, self.fonts.mono, digits, inner, true);
                let label_h = measure(hdc, self.fonts.body, label_text, label_w, true);
                set_font(label, self.fonts.body);
                set_text(label, label_text);
                place(label, pad, y + (code_h - label_h) / 2 + s(2), label_w, label_h, true);
                set_font(code, self.fonts.mono);
                set_text(code, digits);
                let code_x = pad + label_w + s(14);
                place(code, code_x, y, width - pad - code_x, code_h, true);
                colors.insert(ID_CODE_LABEL, SECONDARY);
                colors.insert(ID_CODE, if *spent { FAINT } else { INK });
                y += code_h + s(4);
            }
            (_, label, code) => {
                for h in [label, code].into_iter().flatten() {
                    place(h, 0, 0, 0, 0, false);
                }
            }
        }
        put(self, hdc, &mut colors, ID_COUNTDOWN, content.countdown.as_deref(), self.fonts.small, SECONDARY, pad, inner, &mut y, gap);
        put(self, hdc, &mut colors, ID_WARNING, content.warning.as_deref(), self.fonts.body, WARNING, pad, inner, &mut y, gap);
        put(self, hdc, &mut colors, ID_NOTE0, content.notes.first().map(String::as_str), self.fonts.small, SECONDARY, pad, inner, &mut y, s(4));
        put(self, hdc, &mut colors, ID_NOTE1, content.notes.get(1).map(String::as_str), self.fonts.small, SECONDARY, pad, inner, &mut y, s(4));

        // The buttons, right-aligned; the right one is the default.
        y += s(12);
        let (left, right) = match content.buttons {
            Buttons::NewCodeClose => (Some("New Code"), "Close"),
            Buttons::Cancel => (None, "Cancel"),
            Buttons::Done => (None, "Done"),
        };
        let button_h = s(32);
        let button_w = |t: &str| (measure_width(hdc, self.fonts.body, t) + s(32)).max(s(96));
        let mut x = width - pad;
        if let Some(h) = self.children.get(&ID_RIGHT).copied() {
            let w = button_w(right);
            x -= w;
            set_font(h, self.fonts.body);
            set_text(h, right);
            place(h, x, y, w, button_h, true);
        }
        if let Some(h) = self.children.get(&ID_LEFT).copied() {
            match left {
                Some(t) => {
                    let w = button_w(t);
                    x -= w + s(8);
                    set_font(h, self.fonts.body);
                    set_text(h, t);
                    place(h, x, y, w, button_h, true);
                }
                None => place(h, 0, 0, 0, 0, false),
            }
        }
        y += button_h + pad;
        unsafe {
            ReleaseDC(Some(hwnd), hdc);
        }
        COLORS.with(|c| *c.borrow_mut() = colors);
        self.resize_client(width, y);
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, true);
        }
    }

    /// The window around a client area of `width` × `height`, keeping its top-left corner.
    fn resize_client(&self, width: i32, height: i32) {
        unsafe {
            let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
            let mut r = RECT { left: 0, top: 0, right: width, bottom: height };
            let _ = AdjustWindowRectExForDpi(&mut r, style, false, WS_EX_CONTROLPARENT, self.dpi);
            let _ = SetWindowPos(
                self.hwnd,
                None,
                0,
                0,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
}

fn paint_qr(hdc: HDC, q: &QrPaint) {
    let n = q.code.size();
    let modules = n + 8;
    let module = (q.rect.right - q.rect.left) / modules;
    unsafe {
        let brush = CreateSolidBrush(COLORREF(if q.spent { QR_SPENT } else { QR_DARK }));
        for my in 0..n {
            for mx in 0..n {
                if q.code.get_module(mx, my) {
                    let left = q.rect.left + (mx + 4) * module;
                    let top = q.rect.top + (my + 4) * module;
                    let r = RECT { left, top, right: left + module, bottom: top + module };
                    FillRect(hdc, &r, brush);
                }
            }
        }
        let _ = DeleteObject(HGDIOBJ(brush.0));
    }
}

/// The height `text` takes in `font` at `width` (one line when `single`).
fn measure(hdc: HDC, font: HFONT, text: &str, width: i32, single: bool) -> i32 {
    let mut buf: Vec<u16> = text.encode_utf16().collect();
    let mut r = RECT { left: 0, top: 0, right: width, bottom: 0 };
    let flags = DT_CALCRECT | DT_NOPREFIX | if single { DT_SINGLELINE } else { DT_WORDBREAK };
    unsafe {
        let old = SelectObject(hdc, HGDIOBJ(font.0));
        DrawTextW(hdc, &mut buf, &mut r, flags);
        SelectObject(hdc, old);
    }
    r.bottom - r.top
}

fn measure_width(hdc: HDC, font: HFONT, text: &str) -> i32 {
    let mut buf: Vec<u16> = text.encode_utf16().collect();
    let mut r = RECT::default();
    unsafe {
        let old = SelectObject(hdc, HGDIOBJ(font.0));
        DrawTextW(hdc, &mut buf, &mut r, DT_CALCRECT | DT_NOPREFIX | DT_SINGLELINE);
        SelectObject(hdc, old);
    }
    r.right - r.left
}

fn set_text(h: HWND, text: &str) {
    let w = wide(text);
    unsafe {
        let _ = SetWindowTextW(h, PCWSTR(w.as_ptr()));
    }
}

fn set_font(h: HWND, font: HFONT) {
    unsafe {
        SendMessageW(h, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(0)));
    }
}

fn place(h: HWND, x: i32, y: i32, w: i32, height: i32, visible: bool) {
    unsafe {
        if visible {
            let _ = SetWindowPos(h, None, x, y, w, height, SWP_NOZORDER | SWP_NOACTIVATE);
            let _ = ShowWindow(h, windows::Win32::UI::WindowsAndMessaging::SW_SHOWNA);
        } else {
            let _ = ShowWindow(h, SW_HIDE);
        }
    }
}

/// One STATIC: its words in `font` and `color`, `w` wide at (`x`, `y`), `y` moved past it and
/// `after`; hidden when there are no words.
#[allow(clippy::too_many_arguments)]
fn put(
    ui: &Ui,
    hdc: HDC,
    colors: &mut HashMap<i32, u32>,
    id: i32,
    text: Option<&str>,
    font: HFONT,
    color: u32,
    x: i32,
    w: i32,
    y: &mut i32,
    after: i32,
) {
    let Some(h) = ui.children.get(&id).copied() else { return };
    match text {
        Some(t) if !t.is_empty() => {
            let height = measure(hdc, font, t, w, false);
            set_font(h, font);
            set_text(h, t);
            place(h, x, *y, w, height, true);
            colors.insert(id, color);
            *y += height + after;
        }
        _ => place(h, 0, 0, 0, 0, false),
    }
}
