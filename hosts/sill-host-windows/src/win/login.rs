//! Launch at Login (Sill.app's LoginItem, SMAppService on the Mac): the current user's Run value
//! `Sill` = `"…\Sill.exe" --login`. Task Manager's (and Settings' Startup apps) switch is honoured,
//! never overridden: turned off there (StartupApproved\Run's first byte odd), the menu says so and
//! sends the person there to turn it on again.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Registry::{RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_BINARY, RRF_RT_REG_SZ};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use super::util::{from_wide, wide};
use crate::traymenu::LoginState;

const RUN: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const APPROVED: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\Run");
const VALUE: PCWSTR = w!("Sill");

/// The command the Run value holds for this Sill.exe.
fn command() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    Some(format!("\"{}\" --login", exe.display()))
}

pub fn state() -> LoginState {
    let on = run_value().is_some();
    LoginState { on, disabled_by_user: on && disabled_in_startup_apps() }
}

fn run_value() -> Option<String> {
    let mut buf = vec![0u16; 2048];
    let mut size = (buf.len() * 2) as u32;
    let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN, VALUE, RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut size)) };
    (r == ERROR_SUCCESS).then(|| from_wide(&buf))
}

/// StartupApproved's 12 bytes: an even first byte (2, 6) is on; an odd one (3, 7), turned off.
fn disabled_in_startup_apps() -> bool {
    let mut buf = [0u8; 12];
    let mut size = buf.len() as u32;
    let r = unsafe { RegGetValueW(HKEY_CURRENT_USER, APPROVED, VALUE, RRF_RT_REG_BINARY, None, Some(buf.as_mut_ptr() as *mut _), Some(&mut size)) };
    r == ERROR_SUCCESS && size >= 1 && buf[0] & 1 == 1
}

/// On: the Run value for this Sill.exe (written again if it named another copy). Off: gone.
pub fn set(on: bool) -> Result<(), String> {
    let r = if on {
        let cmd = command().ok_or("Sill.exe's own path is unknown")?;
        let data = wide(&cmd);
        unsafe {
            RegSetKeyValueW(HKEY_CURRENT_USER, RUN, VALUE, REG_SZ.0, Some(data.as_ptr() as *const _), (data.len() * 2) as u32)
        }
    } else {
        unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN, VALUE) }
    };
    if r == ERROR_SUCCESS {
        sill_host_core::log(if on { "Launch at Login: on." } else { "Launch at Login: off." });
        Ok(())
    } else {
        Err(format!("the Run key could not be changed ({})", r.0))
    }
}

/// Settings › Apps › Startup, where a switch turned off in Task Manager is turned on again.
pub fn open_startup_settings() {
    unsafe {
        ShellExecuteW(None, w!("open"), w!("ms-settings:startupapps"), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}
