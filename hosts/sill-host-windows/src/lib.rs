//! Sill host for Windows (owned by the Windows agent; see ../README.md and ../NOTES-windows.md),
//! as a library its two binaries share: `sill-host-windows` (the console host, src/main.rs) and
//! `Sill` (Sill.exe, the tray app, src/bin/sill.rs), as SillHost and Sill.app share SillHostCore.
//!
//! The platform-independent parts (the HID → scancode table, the key state, Annex-B parsing, the
//! pointer's arithmetic, the picture sizes, the image helpers, what the pairing window and the
//! tray menu show, the settings file) compile everywhere and are unit-tested on any machine; the
//! backend itself (`win`) is Windows-only.

#![cfg_attr(not(windows), allow(dead_code))]

pub mod annexb;
pub mod applog;
pub mod encoderhealth;
pub mod fwpolicy;
pub mod glyph;
pub mod imaging;
pub mod keymap;
pub mod keys;
pub mod pairview;
pub mod pointer;
pub mod settings;
pub mod sizing;
pub mod traymenu;

#[cfg(windows)]
pub mod selftest;
#[cfg(windows)]
pub mod win;
