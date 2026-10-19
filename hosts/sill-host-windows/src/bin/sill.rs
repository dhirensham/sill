//! Sill.exe: the host as a tray app (Sill.app's menu bar icon, on Windows): no console, the icon in
//! the notification area with the status and the settings, the pairing window, its settings and
//! log kept between runs. `sill-host-windows.exe` is the console host for testing (as SillHost).
//!
//!     Sill.exe [--login]
//!
//! `--login`: started at sign-in by Launch at Login; the pairing window then waits to be asked for.

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn main() {
    let login = std::env::args().skip(1).any(|a| a == "--login");
    std::process::exit(sill_host_windows::win::tray::run(login));
}

#[cfg(not(windows))]
fn main() {
    eprintln!("Sill.exe runs on Windows only.");
    std::process::exit(2);
}
