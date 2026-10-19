//! "Sill Log" (Sill.app's Show Log… window, LogWindow.swift): the host's last 5,000 lines in a
//! read-only, monospaced view that follows the tail only while it is scrolled to the bottom, with
//! "Show per-second stats" (the `[1s]`, `[30s]` and `client ` lines, off as on the Mac), Clear (the
//! view only) and Open Log Folder. Lines come from `crate::applog`'s ring, read every 250 ms while
//! the window is open; it lives on the tray's GUI thread and is made afresh each time it opens.

use std::cell::RefCell;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateFontW, DeleteObject, HFONT, HGDIOBJ, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, FF_DONTCARE, FONT_CHARSET, FW_NORMAL, OUT_DEFAULT_PRECIS};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{BST_CHECKED, EM_GETLINECOUNT, EM_LINEINDEX, EM_REPLACESEL, EM_SETLIMITTEXT, EM_SETSEL};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetScrollInfo, KillTimer, LoadCursorW,
    RegisterClassExW, SendMessageW, SetForegroundWindow, SetTimer, SetWindowPos, ShowWindow, BS_AUTOCHECKBOX,
    BS_PUSHBUTTON, BM_GETCHECK, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, ES_AUTOHSCROLL, ES_AUTOVSCROLL, ES_MULTILINE, ES_READONLY, HMENU,
    IDC_ARROW, SB_BOTTOM, SB_VERT, SCROLLINFO, SIF_ALL, SWP_NOACTIVATE, SWP_NOZORDER, SW_SHOWNORMAL, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_DESTROY, WM_DPICHANGED, WM_SETFONT, WM_SETTEXT, WM_SIZE, WM_TIMER,
    WM_VSCROLL, WM_CTLCOLORBTN, WM_CTLCOLOREDIT, WM_CTLCOLORSTATIC, WM_ERASEBKGND, WM_SETTINGCHANGE, WNDCLASSEXW, WS_BORDER, WS_CHILD, WS_HSCROLL, WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

use super::util::wide;
use crate::applog;

const ID_TEXT: i32 = 1;
const ID_STATS: i32 = 2;
const ID_CLEAR: i32 = 3;
const ID_FOLDER: i32 = 4;
const TIMER_PULL: usize = 1;
const CLASS: PCWSTR = w!("SillLogWindow");
/// Lines kept in the view; past it the oldest 500 go at once.
const VIEW_LINES: i32 = 5_000;

/// The window is drawn dark (the colour messages read it).
static DARK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn palette() -> super::theme::Palette {
    super::theme::palette(DARK.load(std::sync::atomic::Ordering::Acquire))
}

struct LogWindow {
    hwnd: HWND,
    text: HWND,
    stats: HWND,
    clear: HWND,
    folder: HWND,
    font: HFONT,
    ui_font: HFONT,
    dpi: u32,
    /// The ring's lines read so far.
    seen: u64,
}

thread_local! {
    static WINDOW: RefCell<Option<LogWindow>> = const { RefCell::new(None) };
}

/// Shows the window (made now if it is not open), in front. On the tray's GUI thread.
pub fn show() {
    let open = WINDOW.with(|w| w.borrow().as_ref().map(|l| l.hwnd));
    if let Some(hwnd) = open {
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
            let _ = SetForegroundWindow(hwnd);
        }
        return;
    }
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else { return };
        let instance: HINSTANCE = module.into();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH((windows::Win32::Graphics::Gdi::COLOR_BTNFACE.0 + 1) as isize as _),
            // Sill.exe's icon (resource 1); the console host has none, and gets Windows' own.
            hIcon: windows::Win32::UI::WindowsAndMessaging::LoadIconW(Some(instance), PCWSTR(1 as _)).unwrap_or_default(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS,
            w!("Sill Log"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            900,
            520,
            None,
            None,
            Some(instance),
            None,
        ) else {
            return;
        };
        let child = |class: PCWSTR, text: PCWSTR, style: WINDOW_STYLE, ex: WINDOW_EX_STYLE, id: i32| {
            CreateWindowExW(ex, class, text, WS_CHILD | WS_VISIBLE | style, 0, 0, 0, 0, Some(hwnd), Some(HMENU(id as isize as _)), Some(instance), None)
                .unwrap_or_default()
        };
        let text = child(
            w!("EDIT"),
            w!(""),
            WS_VSCROLL | WS_HSCROLL | WS_BORDER | WINDOW_STYLE((ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL | ES_AUTOHSCROLL) as u32),
            WINDOW_EX_STYLE(0),
            ID_TEXT,
        );
        let stats = child(w!("BUTTON"), w!("Show per-second stats"), WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX as u32), WINDOW_EX_STYLE(0), ID_STATS);
        let clear = child(w!("BUTTON"), w!("Clear"), WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32), WINDOW_EX_STYLE(0), ID_CLEAR);
        let folder = child(w!("BUTTON"), w!("Open Log Folder"), WS_TABSTOP | WINDOW_STYLE(BS_PUSHBUTTON as u32), WINDOW_EX_STYLE(0), ID_FOLDER);
        // No limit to speak of (the default is 32 KB, about 300 lines).
        SendMessageW(text, EM_SETLIMITTEXT, Some(WPARAM(0x7FFF_FFFE)), Some(LPARAM(0)));
        let dpi = GetDpiForWindow(hwnd).max(96);
        let mut w = LogWindow { hwnd, text, stats, clear, folder, font: HFONT::default(), ui_font: HFONT::default(), dpi, seen: 0 };
        w.fonts();
        w.theme();
        // 900 × 520 points at this display's scale.
        let s = |v: i32| v * dpi as i32 / 96;
        let _ = SetWindowPos(hwnd, None, 0, 0, s(900), s(520), windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE | SWP_NOZORDER);
        WINDOW.with(|cell| *cell.borrow_mut() = Some(w));
        layout();
        pull(true);
        SetTimer(Some(hwnd), TIMER_PULL, 250, None);
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        let _ = SetForegroundWindow(hwnd);
    }
}

impl LogWindow {
    /// The title bar, the text's scroll bars and the buttons in the app mode's theme.
    fn theme(&self) {
        let dark = super::theme::dark();
        DARK.store(dark, std::sync::atomic::Ordering::Release);
        super::theme::allow_dark(self.hwnd, dark);
        super::theme::title_bar(self.hwnd, dark);
        for h in [self.text, self.stats, self.clear, self.folder] {
            super::theme::control(h, dark);
        }
    }

    fn fonts(&mut self) {
        let px = |dip: i32| -(dip * self.dpi as i32 / 96);
        unsafe {
            for f in [self.font, self.ui_font] {
                if !f.is_invalid() {
                    let _ = DeleteObject(HGDIOBJ(f.0));
                }
            }
            let make = |h: i32, face: PCWSTR| {
                CreateFontW(h, 0, 0, 0, FW_NORMAL.0 as i32, 0, 0, 0, FONT_CHARSET(DEFAULT_CHARSET.0), OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY, FF_DONTCARE.0 as u32, face)
            };
            self.font = make(px(14), w!("Consolas"));
            self.ui_font = make(px(14), w!("Segoe UI"));
            SendMessageW(self.text, WM_SETFONT, Some(WPARAM(self.font.0 as usize)), Some(LPARAM(1)));
            for b in [self.stats, self.clear, self.folder] {
                SendMessageW(b, WM_SETFONT, Some(WPARAM(self.ui_font.0 as usize)), Some(LPARAM(1)));
            }
        }
    }
}

/// The text over the bottom bar, the checkbox left, the buttons right.
fn layout() {
    WINDOW.with(|cell| {
        let Ok(b) = cell.try_borrow() else { return };
        let Some(w) = b.as_ref() else { return };
        let s = |v: i32| v * w.dpi as i32 / 96;
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(w.hwnd, &mut r);
            let (cw, ch) = (r.right, r.bottom);
            let bar = s(44);
            let flags = SWP_NOZORDER | SWP_NOACTIVATE;
            let _ = SetWindowPos(w.text, None, 0, 0, cw, (ch - bar).max(0), flags);
            let by = ch - bar + s(8);
            let _ = SetWindowPos(w.stats, None, s(12), by, s(220), s(28), flags);
            let _ = SetWindowPos(w.folder, None, cw - s(12) - s(140), by, s(140), s(28), flags);
            let _ = SetWindowPos(w.clear, None, cw - s(12) - s(140) - s(8) - s(90), by, s(90), s(28), flags);
        }
    });
}

/// Appends what the ring has that the view has not (all of it with `afresh`), keeping the view at
/// its bottom only when it was there.
fn pull(afresh: bool) {
    WINDOW.with(|cell| {
        let Ok(mut b) = cell.try_borrow_mut() else { return };
        let Some(w) = b.as_mut() else { return };
        let stats = unsafe { SendMessageW(w.stats, BM_GETCHECK, None, None).0 == BST_CHECKED.0 as isize };
        let (lines, total) = applog::lines_after(if afresh { 0 } else { w.seen });
        w.seen = total;
        let shown: Vec<&String> = lines.iter().filter(|l| stats || !is_stats_line(l)).collect();
        if afresh {
            unsafe {
                SendMessageW(w.text, WM_SETTEXT, None, Some(LPARAM(wide("").as_ptr() as isize)));
            }
        }
        if shown.is_empty() {
            return;
        }
        let mut text = String::new();
        for l in shown {
            text.push_str(l);
            text.push_str("\r\n");
        }
        unsafe {
            let at_bottom = afresh || {
                let mut si = SCROLLINFO { cbSize: std::mem::size_of::<SCROLLINFO>() as u32, fMask: SIF_ALL, ..Default::default() };
                GetScrollInfo(w.text, SB_VERT, &mut si).is_err() || si.nPos + si.nPage as i32 >= si.nMax
            };
            let t = wide(&text);
            // At the end, without an undo step (an undo buffer would grow with the log).
            SendMessageW(w.text, EM_SETSEL, Some(WPARAM(usize::MAX >> 1)), Some(LPARAM(-1)));
            SendMessageW(w.text, EM_REPLACESEL, Some(WPARAM(0)), Some(LPARAM(t.as_ptr() as isize)));
            let count = SendMessageW(w.text, EM_GETLINECOUNT, None, None).0 as i32;
            if count > VIEW_LINES + 500 {
                let cut = SendMessageW(w.text, EM_LINEINDEX, Some(WPARAM((count - VIEW_LINES) as usize)), None).0;
                let empty = wide("");
                SendMessageW(w.text, EM_SETSEL, Some(WPARAM(0)), Some(LPARAM(cut)));
                SendMessageW(w.text, EM_REPLACESEL, Some(WPARAM(0)), Some(LPARAM(empty.as_ptr() as isize)));
            }
            if at_bottom {
                SendMessageW(w.text, WM_VSCROLL, Some(WPARAM(SB_BOTTOM.0 as usize)), None);
            }
        }
    });
}

/// The once-a-second lines (Stats.swift's, and each device's report): hidden unless asked for.
fn is_stats_line(l: &str) -> bool {
    l.starts_with("[1s]") || l.starts_with("[30s]") || l.starts_with("client ")
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_SIZE => {
                layout();
                LRESULT(0)
            }
            WM_TIMER if wparam.0 == TIMER_PULL => {
                pull(false);
                LRESULT(0)
            }
            WM_COMMAND => {
                match (wparam.0 & 0xFFFF) as i32 {
                    ID_STATS => pull(true),
                    ID_CLEAR => {
                        let empty = wide("");
                        let text = WINDOW.with(|c| c.borrow().as_ref().map(|w| w.text));
                        if let Some(t) = text {
                            SendMessageW(t, WM_SETTEXT, None, Some(LPARAM(empty.as_ptr() as isize)));
                        }
                    }
                    ID_FOLDER => {
                        if let Some(dir) = applog::path().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
                            let d = wide(&dir.to_string_lossy());
                            ShellExecuteW(None, w!("open"), PCWSTR(d.as_ptr()), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
                        }
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_DPICHANGED => {
                let r = *(lparam.0 as *const RECT);
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
                WINDOW.with(|c| {
                    if let Ok(mut b) = c.try_borrow_mut() {
                        if let Some(w) = b.as_mut() {
                            w.dpi = (wparam.0 & 0xFFFF) as u32;
                            w.fonts();
                        }
                    }
                });
                layout();
                LRESULT(0)
            }
            WM_ERASEBKGND => {
                let mut r = RECT::default();
                let _ = GetClientRect(hwnd, &mut r);
                windows::Win32::Graphics::Gdi::FillRect(windows::Win32::Graphics::Gdi::HDC(wparam.0 as _), &r, super::theme::brush(palette().background));
                LRESULT(1)
            }
            // The read-only text and the checkbox draw as statics; the buttons' corners as buttons.
            WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT => {
                let hdc = windows::Win32::Graphics::Gdi::HDC(wparam.0 as _);
                let p = palette();
                windows::Win32::Graphics::Gdi::SetTextColor(hdc, windows::Win32::Foundation::COLORREF(p.ink));
                windows::Win32::Graphics::Gdi::SetBkColor(hdc, windows::Win32::Foundation::COLORREF(p.background));
                LRESULT(super::theme::brush(p.background).0 as isize)
            }
            WM_CTLCOLORBTN => LRESULT(super::theme::brush(palette().background).0 as isize),
            WM_SETTINGCHANGE => {
                if super::theme::is_theme_change(lparam) {
                    WINDOW.with(|c| {
                        if let Ok(b) = c.try_borrow() {
                            if let Some(w) = b.as_ref() {
                                w.theme();
                            }
                        }
                    });
                    let _ = windows::Win32::Graphics::Gdi::RedrawWindow(
                        Some(hwnd),
                        None,
                        None,
                        windows::Win32::Graphics::Gdi::RDW_INVALIDATE
                            | windows::Win32::Graphics::Gdi::RDW_ERASE
                            | windows::Win32::Graphics::Gdi::RDW_ALLCHILDREN
                            | windows::Win32::Graphics::Gdi::RDW_FRAME,
                    );
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_CLOSE => {
                let _ = KillTimer(Some(hwnd), TIMER_PULL);
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                WINDOW.with(|c| {
                    if let Some(w) = c.borrow_mut().take() {
                        for f in [w.font, w.ui_font] {
                            let _ = DeleteObject(HGDIOBJ(f.0));
                        }
                    }
                });
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn stats_lines_are_the_macs() {
        assert!(super::is_stats_line("[1s] enc.out 58 · 1 client"));
        assert!(super::is_stats_line("[30s] idle · 0 clients · 4 windows"));
        assert!(super::is_stats_line("client iPad (iPad13,16): 52 fps, frame age 9/24 ms, rtt 6/9 ms"));
        assert!(!super::is_stats_line("Client connected: [fe80::1]:59848"));
    }
}
