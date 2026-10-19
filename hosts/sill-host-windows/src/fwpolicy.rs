//! Whether Windows Firewall lets devices reach the home door's port (portable: unit-tested on any
//! machine; `win::firewall` reads the facts from the firewall's COM API, INetFwPolicy2, which needs
//! no administrator).
//!
//! The rules, as Windows applies them for an inbound TCP connection on the active profile: a
//! firewall that is off lets everything in; "block all incoming connections" lets nothing in; a
//! matching block rule beats any allow rule; then a matching allow rule, else the profile's default
//! inbound action. Only a rule for the port itself counts as allowing Sill: one that names the port
//! (not `*`), for any program and no service, from any address or the local subnet, on any kind of
//! interface or Wi-Fi or LAN. A rule for a program path never matches an exe on a Dev Drive or
//! another volume mounted in a folder (NOTES-windows.md), and a broad built-in rule is narrowed by
//! conditions this reads only in part, so neither is trusted: a wrong "Allowed" would hide the very
//! problem the menu is there for, a wrong "Not allowed" costs one needless rule. A block rule for
//! Sill.exe's own path (made when Windows' "allow access" prompt was cancelled) counts.

use crate::traymenu::Firewall;

pub const DOMAIN: i32 = 1;
pub const PRIVATE: i32 = 2;
pub const PUBLIC: i32 = 4;

/// One profile's settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Profile {
    /// DOMAIN, PRIVATE or PUBLIC.
    pub kind: i32,
    pub enabled: bool,
    pub block_all_inbound: bool,
    pub default_allow_inbound: bool,
}

/// One rule, as far as an inbound TCP connection to the port cares.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub enabled: bool,
    pub inbound: bool,
    pub allow: bool,
    /// The profiles' bitmask it applies to.
    pub profiles: i32,
    /// 6 TCP, 17 UDP, 256 any.
    pub protocol: i32,
    /// "*", "7454", "7000-8000,7454", or a keyword ("RPC").
    pub local_ports: String,
    /// The program it is for; empty for any program.
    pub application: String,
    /// The service it is for; empty (or "*") for any.
    pub service: String,
    /// "*", "LocalSubnet", addresses…
    pub remote_addresses: String,
    pub local_addresses: String,
    /// "All", or a list of "Wireless", "Lan", "RemoteAccess".
    pub interface_types: String,
    pub name: String,
}

/// What the firewall does with inbound TCP to `port` for this Sill.exe (`exe`), on the active
/// profiles (`active`: their bitmask). The network devices use is the active Private one if there
/// is one, else Domain, else Public; failing on Public alone is "this network is Public".
pub fn decide(active: i32, profiles: &[Profile], rules: &[Rule], port: u16, exe: &str, managed: bool) -> Firewall {
    let pick = [PRIVATE, DOMAIN, PUBLIC].into_iter().find(|k| active & k != 0);
    let Some(kind) = pick else { return Firewall::Checking };
    let Some(profile) = profiles.iter().find(|p| p.kind == kind) else { return Firewall::Checking };
    if allows(profile, rules, port, exe) {
        return Firewall::Allowed;
    }
    if kind == PUBLIC {
        return Firewall::PublicNetwork;
    }
    if managed {
        return Firewall::Managed;
    }
    Firewall::NotAllowed
}

fn allows(p: &Profile, rules: &[Rule], port: u16, exe: &str) -> bool {
    if !p.enabled {
        return true;
    }
    if p.block_all_inbound {
        return false;
    }
    if rules.iter().any(|r| blocks(r, p.kind, port, exe)) {
        return false;
    }
    rules.iter().any(|r| lets_in(r, p.kind, port)) || p.default_allow_inbound
}

fn any_service(r: &Rule) -> bool {
    r.service.is_empty() || r.service == "*"
}

fn applies(r: &Rule, kind: i32, port: u16) -> bool {
    r.enabled && r.inbound && r.profiles & kind != 0 && (r.protocol == 6 || r.protocol == 256) && ports_cover(&r.local_ports, port)
}

/// A block rule that stops inbound TCP to the port for this exe.
fn blocks(r: &Rule, kind: i32, port: u16, exe: &str) -> bool {
    applies(r, kind, port) && !r.allow && any_service(r) && (r.application.is_empty() || r.application.eq_ignore_ascii_case(exe))
}

/// An allow rule this trusts to let devices reach the port (the module's doc).
fn lets_in(r: &Rule, kind: i32, port: u16) -> bool {
    let spec = r.local_ports.trim();
    let named = !spec.is_empty() && spec != "*";
    let anywhere = |a: &str| {
        let a = a.trim();
        a.is_empty() || a == "*" || a.eq_ignore_ascii_case("LocalSubnet")
    };
    let interfaces = {
        let t = r.interface_types.to_ascii_lowercase();
        t.is_empty() || t == "all" || t.contains("wireless") || t.contains("lan")
    };
    applies(r, kind, port)
        && r.allow
        && named
        && r.application.is_empty()
        && any_service(r)
        && anywhere(&r.remote_addresses)
        && (r.local_addresses.trim().is_empty() || r.local_addresses.trim() == "*")
        && interfaces
}

/// The rule that lets devices in on the active profile (`decide`'s), for the log.
pub fn allowing_rule(active: i32, rules: &[Rule], port: u16) -> Option<String> {
    let kind = [PRIVATE, DOMAIN, PUBLIC].into_iter().find(|k| active & k != 0)?;
    rules.iter().find(|r| lets_in(r, kind, port)).map(|r| r.name.clone())
}

/// Whether a rule's LocalPorts covers `port`: "*", a number, a range "a-b", or a list of them.
/// Keywords (RPC, RPC-EPMap, IPHTTPS, Teredo…) never do.
pub fn ports_cover(spec: &str, port: u16) -> bool {
    let spec = spec.trim();
    if spec.is_empty() || spec == "*" {
        return true;
    }
    spec.split(',').map(str::trim).any(|part| match part.split_once('-') {
        Some((a, b)) => match (a.trim().parse::<u16>(), b.trim().parse::<u16>()) {
            (Ok(a), Ok(b)) => (a..=b).contains(&port),
            _ => false,
        },
        None => part.parse::<u16>() == Ok(port),
    })
}

/// The arguments for the elevated `netsh` that adds Sill's rule: TCP `port`, inbound, Private.
pub fn netsh_arguments(port: u16) -> String {
    format!("advfirewall firewall add rule name=\"Sill (TCP {port})\" dir=in action=allow protocol=TCP localport={port} profile=private")
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = r"S:\sill\target\release\Sill.exe";

    fn private(enabled: bool) -> Profile {
        Profile { kind: PRIVATE, enabled, block_all_inbound: false, default_allow_inbound: false }
    }

    fn rule(allow: bool, ports: &str, app: &str) -> Rule {
        Rule {
            enabled: true,
            inbound: true,
            allow,
            profiles: PRIVATE,
            protocol: 6,
            local_ports: ports.into(),
            application: app.into(),
            service: String::new(),
            remote_addresses: "*".into(),
            local_addresses: "*".into(),
            interface_types: "All".into(),
            name: "Sill (TCP 7454)".into(),
        }
    }

    #[test]
    fn ports() {
        assert!(ports_cover("*", 7454) && ports_cover("", 7454) && ports_cover("7454", 7454));
        assert!(ports_cover("80, 7000-8000", 7454) && ports_cover("443,7454", 7454));
        assert!(!ports_cover("7455", 7454) && !ports_cover("RPC", 7454) && !ports_cover("8000-7000", 7454));
    }

    #[test]
    fn the_rules_in_windows_order() {
        let p = [private(true)];
        // Nothing lets it in.
        assert_eq!(decide(PRIVATE, &p, &[], 7454, EXE, false), Firewall::NotAllowed);
        // A port rule does (this PC's "Sill host (dev build, TCP 7454)").
        assert_eq!(decide(PRIVATE, &p, &[rule(true, "7454", "")], 7454, EXE, false), Firewall::Allowed);
        // A rule for a program path is not trusted to match; nor a broad rule narrowed by a service,
        // remote addresses or interface types, nor one for every port.
        assert_eq!(decide(PRIVATE, &p, &[rule(true, "7454", EXE)], 7454, EXE, false), Firewall::NotAllowed);
        let mut service = rule(true, "7454", "");
        service.service = "Dnscache".into();
        let mut remote = rule(true, "7454", "");
        remote.remote_addresses = "10.1.2.3".into();
        let mut ras = rule(true, "7454", "");
        ras.interface_types = "RemoteAccess".into();
        assert_eq!(decide(PRIVATE, &p, &[service, remote, ras, rule(true, "*", "")], 7454, EXE, false), Firewall::NotAllowed);
        let mut subnet = rule(true, "7000-8000", "");
        subnet.remote_addresses = "LocalSubnet".into();
        subnet.interface_types = "Wireless,Lan".into();
        assert_eq!(decide(PRIVATE, &p, &[subnet], 7454, EXE, false), Firewall::Allowed);
        assert_eq!(allowing_rule(PRIVATE, &[rule(true, "7454", "")], 7454).as_deref(), Some("Sill (TCP 7454)"));
        // A block rule for this exe (a cancelled prompt) beats the port rule; another program's does not.
        let blocked = [rule(true, "7454", ""), rule(false, "*", &EXE.to_lowercase())];
        assert_eq!(decide(PRIVATE, &p, &blocked, 7454, EXE, false), Firewall::NotAllowed);
        let other = [rule(true, "7454", ""), rule(false, "*", r"C:\other.exe")];
        assert_eq!(decide(PRIVATE, &p, &other, 7454, EXE, false), Firewall::Allowed);
        // A disabled rule, an outbound one, a UDP one, another profile's: none counts.
        let mut off = rule(true, "7454", "");
        off.enabled = false;
        let mut out = rule(true, "7454", "");
        out.inbound = false;
        let mut udp = rule(true, "7454", "");
        udp.protocol = 17;
        let mut public_only = rule(true, "7454", "");
        public_only.profiles = PUBLIC;
        assert_eq!(decide(PRIVATE, &p, &[off, out, udp, public_only], 7454, EXE, false), Firewall::NotAllowed);
        // The firewall off; block-all; an allowing default; Group Policy.
        assert_eq!(decide(PRIVATE, &[private(false)], &[], 7454, EXE, false), Firewall::Allowed);
        let all = [Profile { block_all_inbound: true, ..private(true) }];
        assert_eq!(decide(PRIVATE, &all, &[rule(true, "*", "")], 7454, EXE, false), Firewall::NotAllowed);
        let open = [Profile { default_allow_inbound: true, ..private(true) }];
        assert_eq!(decide(PRIVATE, &open, &[], 7454, EXE, false), Firewall::Allowed);
        assert_eq!(decide(PRIVATE, &p, &[], 7454, EXE, true), Firewall::Managed);
    }

    #[test]
    fn a_public_network() {
        let profiles = [private(true), Profile { kind: PUBLIC, enabled: true, block_all_inbound: false, default_allow_inbound: false }];
        let sill = [rule(true, "7454", "")];
        assert_eq!(decide(PUBLIC, &profiles, &sill, 7454, EXE, false), Firewall::PublicNetwork);
        // Private and Public both active (Wi-Fi at home, a VPN's adapter Public): the Private one.
        assert_eq!(decide(PRIVATE | PUBLIC, &profiles, &sill, 7454, EXE, false), Firewall::Allowed);
        assert!(netsh_arguments(7454).contains("localport=7454 profile=private"));
    }
}
