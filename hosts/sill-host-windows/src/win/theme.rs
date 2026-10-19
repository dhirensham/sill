//! Light or dark, as the person set "Choose your app mode" (AppsUseLightTheme): Sill.exe's menus
//! and windows follow it, and follow a change at once (WM_SETTINGCHANGE, "ImmersiveColorSet").
//!
//! Popup menus (the tray's) turn dark only through uxtheme's app mode, which Windows does not
//! document but Explorer, Notepad++ and Windows Terminal rely on (uxtheme.dll's ordinals 135
//! SetPreferredAppMode, 136 FlushMenuThemes, 104 RefreshImmersiveColorPolicyState, 133
//! AllowDarkModeForWindow; Windows 10 1903 and later): with "allow dark" a menu follows the app
//! mode by itself. Windows draw their own colours (`Palette`), a dark title bar through DWM, and
//! their buttons and scroll bars in Explorer's dark theme. Where an ordinal is missing, nothing
//! breaks: the menu stays light.
//!
//! TEST ONLY: SILL_TEST_THEME=dark|light forces one, so a test never changes the person's setting.

use std::sync::atomic::{AtomicU8, Ordering};

use windows::core::{w, BOOL, PCSTR, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};
use windows::Win32::UI::Controls::SetWindowTheme;

/// 0 unknown, 1 light, 2 dark: the forced theme (SILL_TEST_THEME), read once.
static FORCED: AtomicU8 = AtomicU8::new(0);

fn forced() -> Option<bool> {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let v = match std::env::var("SILL_TEST_THEME").as_deref() {
            Ok("dark") => 2,
            Ok("light") => 1,
            _ => 0,
        };
        FORCED.store(v, Ordering::Release);
    });
    match FORCED.load(Ordering::Acquire) {
        2 => Some(true),
        1 => Some(false),
        _ => None,
    }
}

/// The app mode is dark ("Choose your app mode: Dark").
pub fn dark() -> bool {
    if let Some(d) = forced() {
        return d;
    }
    let mut value: u32 = 1;
    let mut size = std::mem::size_of::<u32>() as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    r.is_ok() && value == 0
}

/// Whether a WM_SETTINGCHANGE is the theme's ("ImmersiveColorSet").
pub fn is_theme_change(lparam: LPARAM) -> bool {
    if lparam.0 == 0 {
        return false;
    }
    let text = unsafe { PCWSTR(lparam.0 as *const u16).to_string() }.unwrap_or_default();
    text == "ImmersiveColorSet"
}

fn uxtheme(ordinal: usize) -> Option<unsafe extern "system" fn() -> isize> {
    unsafe {
        let module = LoadLibraryW(w!("uxtheme.dll")).ok()?;
        GetProcAddress(module, PCSTR(ordinal as *const u8))
    }
}

/// Once, before the first menu: menus follow the app mode (or the forced theme).
pub fn init_app() {
    // SetPreferredAppMode: 1 allow dark (follow the app mode), 2 force dark, 3 force light.
    let mode = match forced() {
        Some(true) => 2isize,
        Some(false) => 3isize,
        None => 1isize,
    };
    if let Some(f) = uxtheme(135) {
        let set: unsafe extern "system" fn(isize) -> isize = unsafe { std::mem::transmute(f) };
        unsafe {
            set(mode);
        }
    }
    refresh_menus();
}

/// After a theme change: menus drawn from now on take it.
pub fn refresh_menus() {
    for ordinal in [104usize, 136] {
        if let Some(f) = uxtheme(ordinal) {
            unsafe {
                f();
            }
        }
    }
}

/// A window (and its menus) may draw dark.
pub fn allow_dark(hwnd: HWND, dark: bool) {
    if let Some(f) = uxtheme(133) {
        let allow: unsafe extern "system" fn(HWND, BOOL) -> BOOL = unsafe { std::mem::transmute(f) };
        unsafe {
            let _ = allow(hwnd, dark.into());
        }
    }
}

/// A top-level window's title bar, dark or light.
pub fn title_bar(hwnd: HWND, dark: bool) {
    let on: BOOL = dark.into();
    unsafe {
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &on as *const BOOL as *const _, std::mem::size_of::<BOOL>() as u32);
    }
}

/// A control (a button, an edit's scroll bars) in Explorer's dark or light theme.
pub fn control(hwnd: HWND, dark: bool) {
    allow_dark(hwnd, dark);
    unsafe {
        let _ = SetWindowTheme(hwnd, if dark { w!("DarkMode_Explorer") } else { w!("Explorer") }, PCWSTR::null());
    }
}

/// A solid brush of `colour` for this thread's windows, made once and kept.
pub fn brush(colour: u32) -> windows::Win32::Graphics::Gdi::HBRUSH {
    use std::cell::RefCell;
    use std::collections::HashMap;
    thread_local! {
        static BRUSHES: RefCell<HashMap<u32, isize>> = RefCell::new(HashMap::new());
    }
    let raw = BRUSHES.with(|b| {
        *b.borrow_mut()
            .entry(colour)
            .or_insert_with(|| unsafe { windows::Win32::Graphics::Gdi::CreateSolidBrush(windows::Win32::Foundation::COLORREF(colour)) }.0 as isize)
    });
    windows::Win32::Graphics::Gdi::HBRUSH(raw as _)
}

/// A window's own colours (COLORREF, 0x00BBGGRR), as Windows 11's dialogs use them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub background: u32,
    pub ink: u32,
    pub secondary: u32,
    pub faint: u32,
    pub warning: u32,
    pub success: u32,
}

pub fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            background: 0x0020_2020,
            ink: 0x00FF_FFFF,
            secondary: 0x00C5_C5C5,
            faint: 0x0070_7070,
            warning: 0x005C_A3F7, // #F7A35C
            success: 0x005F_CB6C, // #6CCB5F
        }
    } else {
        Palette {
            background: 0x00FF_FFFF,
            ink: 0x001A_1A1A,
            secondary: 0x005D_5D5D,
            faint: 0x00BD_BDBD,
            warning: 0x0000_5CB8, // #B85C00
            success: 0x000F_7B0F, // #0F7B0F
        }
    }
}

