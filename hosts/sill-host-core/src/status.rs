//! What a host's own UI shows (Sill.exe's tray menu, as Sill.app's status card: HostStatus.swift):
//! the doors, the devices with their last report, what streams, the settings. Read on request
//! through `HostHandle::status()`; nothing here is sent to devices.

use crate::config::HostConfig;

#[derive(Clone, Debug, PartialEq)]
pub struct HostStatus {
    /// The name devices list this host by.
    pub name: String,
    /// The home door's port.
    pub port: u16,
    /// The home door speaks TLS with pairing (`--pairing`); else the plain development door.
    pub home_tls: bool,
    /// Whether advertising on the local network started (None: not asked: `--no-advertise`, a
    /// loopback host); Err says why it did not.
    pub advertised: Option<Result<(), String>>,
    pub devices: Vec<DeviceStatus>,
    pub stream: Option<StreamStatus>,
    pub config: HostConfig,
    /// Paired devices (the trust list), with an identity; 0 without.
    pub paired: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeviceStatus {
    /// "iPad (iPad13,16)" for a paired device, else the name its hello gave, else its endpoint.
    pub name: String,
    pub endpoint: String,
    /// How a device away reaches the host ("through Tailscale", "by address"); None at home.
    pub route: Option<String>,
    /// Its last report (about one a second): None until the first.
    pub report: Option<DeviceReport>,
}

/// What a device measured over its last second (kind 12): -1 for a value it had no sample of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceReport {
    pub fps: i64,
    pub frame_age_ms: i64,
    pub rtt_ms: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StreamStatus {
    /// "App — Title" for a window; None for the whole desktop.
    pub window: Option<String>,
    /// The encoded picture, once the first frame said.
    pub size: Option<(u32, u32)>,
    pub fps: i64,
    pub mbps: i64,
}

/// What the coordinator publishes after every change it makes: the stream and the settings.
#[derive(Clone, Debug, PartialEq)]
pub struct Published {
    pub stream: Option<StreamStatus>,
    pub config: HostConfig,
}
