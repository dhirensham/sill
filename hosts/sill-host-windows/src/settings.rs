//! Sill.exe's saved settings (portable: unit-tested on any machine), as Sill.app keeps them in its
//! UserDefaults (HostSettings.swift): `%APPDATA%\Sill\settings.json`, beside the identity and the
//! trust list the core keeps there. Unknown keys are ignored and missing ones take their defaults,
//! so an older or newer Sill.exe reads the same file; values are snapped to what the menu offers.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sill_host_core::config::HostConfig;
use sill_protocol::messages::settings_choices;

/// The home door's port by default: fixed, so one firewall rule fits it (the Mac picks any free
/// port: its firewall asks per app). 7455 is the remote door's, as on the Mac.
pub const DEFAULT_PORT: u16 = 7454;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub port: u16,
    #[serde(rename = "maxFPS")]
    pub max_fps: i64,
    pub bitrate: i64,
    pub capture_scale: f64,
    pub away_bitrate: i64,
    pub away_capture_scale: f64,
    pub prioritize_speed: bool,
    /// The first launch's balloon ("Sill is in the notification area") was shown.
    pub shown_tray_hint: bool,
}

impl Default for Settings {
    fn default() -> Self {
        let c = HostConfig::standard();
        Settings {
            port: DEFAULT_PORT,
            max_fps: c.max_fps,
            bitrate: c.bitrate,
            capture_scale: c.capture_scale,
            away_bitrate: c.away_bitrate,
            away_capture_scale: c.away_capture_scale,
            prioritize_speed: c.prioritize_speed,
            shown_tray_hint: false,
        }
    }
}

impl Settings {
    /// `dir`\settings.json, or the defaults when it is missing or unreadable (never an error: a
    /// broken file must not keep Sill from starting; the next save writes a good one).
    pub fn load(dir: &Path) -> Settings {
        std::fs::read(dir.join("settings.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Settings>(&b).ok())
            .unwrap_or_default()
            .snapped()
    }

    /// Written whole to a temporary file, then renamed over the old one.
    pub fn save(&self, dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = dir.join("settings.json");
        let tmp = dir.join(".settings.json.tmp");
        let json = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, json).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Within what the menu offers: 60 or 120 fps, Retina or Standard, 1–200 Mbps (a bitrate off
    /// the presets stays: the menu shows it as the Mac does, checked and greyed), a port of 1024 or
    /// more (0 and the reserved ones fall back to the default).
    pub fn snapped(mut self) -> Settings {
        self.max_fps = if self.max_fps <= 90 { 60 } else { 120 };
        self.capture_scale = snap_scale(self.capture_scale);
        self.away_capture_scale = snap_scale(self.away_capture_scale);
        self.bitrate = self.bitrate.clamp(1_000_000, 200_000_000);
        self.away_bitrate = self.away_bitrate.clamp(1_000_000, 200_000_000);
        if self.port < 1024 {
            self.port = DEFAULT_PORT;
        }
        self
    }

    /// The streaming knobs the host starts with.
    pub fn config(&self) -> HostConfig {
        HostConfig {
            max_fps: self.max_fps,
            capture_scale: self.capture_scale,
            bitrate: self.bitrate,
            away_bitrate: self.away_bitrate,
            away_capture_scale: self.away_capture_scale,
            prioritize_speed: self.prioritize_speed,
        }
    }

    /// Takes the running host's knobs (a device or the menu changed them); true when anything
    /// changed and the file should be written.
    pub fn take(&mut self, c: &HostConfig) -> bool {
        let before = self.clone();
        self.max_fps = c.max_fps;
        self.capture_scale = c.capture_scale;
        self.bitrate = c.bitrate;
        self.away_bitrate = c.away_bitrate;
        self.away_capture_scale = c.away_capture_scale;
        self.prioritize_speed = c.prioritize_speed;
        *self != before
    }
}

fn snap_scale(s: f64) -> f64 {
    if s >= 1.5 {
        2.0
    } else {
        1.0
    }
}

/// `%APPDATA%\Sill` (the core's config directory on Windows).
pub fn default_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Sill"))
}

/// Whether `bitrate` is one of the menu's seven presets.
pub fn is_preset(bitrate: i64) -> bool {
    settings_choices::BITRATE.contains(&bitrate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("sill-settings-test-{}-{:?}", std::process::id(), std::thread::current().id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn missing_and_broken_files_give_the_defaults() {
        let d = temp_dir();
        assert_eq!(Settings::load(&d), Settings::default());
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("settings.json"), b"{ not json").unwrap();
        assert_eq!(Settings::load(&d), Settings::default());
        assert_eq!(Settings::default().port, 7454);
        assert_eq!(Settings::default().config(), HostConfig::standard());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn round_trip_unknown_keys_and_snapping() {
        let d = temp_dir();
        let s = Settings { bitrate: 40_000_000, max_fps: 60, capture_scale: 1.0, shown_tray_hint: true, ..Settings::default() };
        s.save(&d).unwrap();
        assert_eq!(Settings::load(&d), s);
        let text = String::from_utf8(std::fs::read(d.join("settings.json")).unwrap()).unwrap();
        assert!(text.contains("\"maxFPS\": 60") && text.contains("\"captureScale\": 1.0"), "{text}");
        // A newer Sill's key is ignored; a value off the menu snaps; a missing one is the default.
        std::fs::write(d.join("settings.json"), br#"{"maxFPS": 75, "captureScale": 1.7, "bitrate": 900000000, "port": 80, "remoteAccess": true}"#).unwrap();
        let t = Settings::load(&d);
        assert_eq!((t.max_fps, t.capture_scale, t.bitrate, t.port), (60, 2.0, 200_000_000, 7454));
        assert_eq!(t.away_bitrate, 4_000_000);
        // An off-preset bitrate stays (shown greyed in the menu).
        std::fs::write(d.join("settings.json"), br#"{"bitrate": 12000000}"#).unwrap();
        assert_eq!(Settings::load(&d).bitrate, 12_000_000);
        assert!(!is_preset(12_000_000) && is_preset(15_000_000));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn take_says_whether_anything_changed() {
        let mut s = Settings::default();
        assert!(!s.take(&HostConfig::standard()));
        let c = HostConfig { bitrate: 25_000_000, ..HostConfig::standard() };
        assert!(s.take(&c));
        assert_eq!(s.bitrate, 25_000_000);
        assert!(!s.take(&c));
    }
}
