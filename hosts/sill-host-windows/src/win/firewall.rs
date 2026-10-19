//! Windows Firewall for Sill.exe's menu (in place of Sill.app's Permissions): the facts read through
//! the firewall's COM API (INetFwPolicy2: no administrator needed) on a worker thread, decided by
//! `crate::fwpolicy`, cached for the menu; "Allow…" runs `netsh` elevated (one UAC prompt) to add
//! one inbound TCP rule for the port on Private networks, unless the port is already allowed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, WAIT_OBJECT_0};
use windows::Win32::NetworkManagement::WindowsFirewall::{
    INetFwPolicy2, INetFwRule, NetFwPolicy2, NET_FW_ACTION_ALLOW, NET_FW_MODIFY_STATE_OK, NET_FW_PROFILE_TYPE2,
    NET_FW_RULE_DIR_IN,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::Ole::IEnumVARIANT;
use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
use windows::Win32::System::Variant::{VariantClear, VARIANT, VT_DISPATCH};
use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

use super::util::{com_init, wide};
use crate::fwpolicy::{self, Profile, Rule, DOMAIN, PRIVATE, PUBLIC};
use crate::traymenu::Firewall;

static CURRENT: Mutex<Firewall> = Mutex::new(Firewall::Checking);
static BUSY: AtomicBool = AtomicBool::new(false);
/// The rule that allowed it, for the log line.
static RULE: Mutex<Option<String>> = Mutex::new(None);

/// What the menu shows: the last check's answer.
pub fn current() -> Firewall {
    CURRENT.lock().map(|f| f.clone()).unwrap_or(Firewall::Checking)
}

/// Checks again on a worker thread (once at a time; the menu keeps the last answer meanwhile).
pub fn refresh(port: u16) {
    if BUSY.swap(true, Ordering::AcqRel) {
        return;
    }
    std::thread::Builder::new()
        .name("sill-firewall".into())
        .spawn(move || {
            let answer = check(port);
            let changed = {
                let mut c = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
                let changed = *c != answer;
                *c = answer.clone();
                changed
            };
            if changed {
                sill_host_core::log(&format!("Windows Firewall: {}", describe(&answer, port)));
            }
            BUSY.store(false, Ordering::Release);
        })
        .ok();
}

fn describe(f: &Firewall, port: u16) -> String {
    match f {
        Firewall::Checking => "not known".into(),
        Firewall::Allowed => match RULE.lock().ok().and_then(|r| r.clone()) {
            Some(rule) => format!("devices may connect on TCP port {port} (rule “{rule}”)."),
            None => format!("devices may connect on TCP port {port} (the firewall lets them in on this network)."),
        },
        Firewall::NotAllowed => format!("nothing lets devices in on TCP port {port}; the menu offers to allow it."),
        Firewall::PublicNetwork => "this network is Public, where Sill's rule does not apply.".into(),
        Firewall::Managed => format!("an organization's policy decides; TCP port {port} is not allowed."),
    }
}

/// Reads the profiles and rules and decides; Checking when the API can't be read.
fn check(port: u16) -> Firewall {
    com_init();
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
    match read() {
        Ok((active, profiles, rules, managed)) => {
            let answer = fwpolicy::decide(active, &profiles, &rules, port, &exe, managed);
            if answer == Firewall::Allowed {
                if let Some(name) = fwpolicy::allowing_rule(active, &rules, port) {
                    *RULE.lock().unwrap_or_else(|e| e.into_inner()) = Some(name);
                }
            }
            answer
        }
        Err(e) => {
            sill_host_core::log(&format!("Windows Firewall could not be read ({e})."));
            Firewall::Checking
        }
    }
}

type Facts = (i32, Vec<Profile>, Vec<Rule>, bool);

fn read() -> windows::core::Result<Facts> {
    unsafe {
        let policy: INetFwPolicy2 = CoCreateInstance(&NetFwPolicy2, None, CLSCTX_INPROC_SERVER)?;
        let active = policy.CurrentProfileTypes()?;
        let mut profiles = Vec::new();
        for kind in [DOMAIN, PRIVATE, PUBLIC] {
            let t = NET_FW_PROFILE_TYPE2(kind);
            profiles.push(Profile {
                kind,
                enabled: policy.get_FirewallEnabled(t)?.as_bool(),
                block_all_inbound: policy.get_BlockAllInboundTraffic(t)?.as_bool(),
                default_allow_inbound: policy.get_DefaultInboundAction(t)? == NET_FW_ACTION_ALLOW,
            });
        }
        let managed = policy.LocalPolicyModifyState().map(|s| s != NET_FW_MODIFY_STATE_OK).unwrap_or(false);
        let mut rules = Vec::new();
        let all = policy.Rules()?;
        let items: IEnumVARIANT = all._NewEnum()?.cast()?;
        loop {
            let mut slot = [VARIANT::default()];
            let mut fetched = 0u32;
            let _ = items.Next(&mut slot, &mut fetched);
            if fetched == 0 {
                break;
            }
            let v = &slot[0];
            if v.Anonymous.Anonymous.vt == VT_DISPATCH {
                if let Some(dispatch) = &*v.Anonymous.Anonymous.Anonymous.pdispVal {
                    if let Ok(r) = dispatch.cast::<INetFwRule>() {
                        rules.push(Rule {
                            enabled: r.Enabled().map(|b| b.as_bool()).unwrap_or(false),
                            inbound: r.Direction().map(|d| d == NET_FW_RULE_DIR_IN).unwrap_or(false),
                            allow: r.Action().map(|a| a == NET_FW_ACTION_ALLOW).unwrap_or(false),
                            profiles: r.Profiles().unwrap_or(0),
                            protocol: r.Protocol().unwrap_or(0),
                            local_ports: r.LocalPorts().map(|b| b.to_string()).unwrap_or_default(),
                            application: r.ApplicationName().map(|b| b.to_string()).unwrap_or_default(),
                            service: r.ServiceName().map(|b| b.to_string()).unwrap_or_default(),
                            remote_addresses: r.RemoteAddresses().map(|b| b.to_string()).unwrap_or_default(),
                            local_addresses: r.LocalAddresses().map(|b| b.to_string()).unwrap_or_default(),
                            interface_types: r.InterfaceTypes().map(|b| b.to_string()).unwrap_or_default(),
                            name: r.Name().map(|b| b.to_string()).unwrap_or_default(),
                        });
                    }
                }
            }
            let _ = VariantClear(&mut slot[0]);
        }
        Ok((active, profiles, rules, managed))
    }
}

/// "Allow Through Windows Firewall…": `netsh` elevated on a worker thread (the UAC prompt blocks
/// it), only when the port is not allowed already (netsh adds a duplicate every time); then checks
/// again. Declining the prompt changes nothing.
pub fn allow(port: u16) {
    std::thread::Builder::new()
        .name("sill-firewall-allow".into())
        .spawn(move || {
            if check(port) == Firewall::Allowed {
                refresh(port);
                return;
            }
            let netsh = std::env::var("SystemRoot").map(|r| format!("{r}\\System32\\netsh.exe")).unwrap_or_else(|_| "netsh.exe".into());
            let (file, args, verb) = (wide(&netsh), wide(&fwpolicy::netsh_arguments(port)), wide("runas"));
            let mut info = SHELLEXECUTEINFOW {
                cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
                fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC,
                lpVerb: PCWSTR(verb.as_ptr()),
                lpFile: PCWSTR(file.as_ptr()),
                lpParameters: PCWSTR(args.as_ptr()),
                nShow: SW_HIDE.0,
                ..Default::default()
            };
            unsafe {
                match ShellExecuteExW(&mut info) {
                    Ok(()) => {
                        let mut code = 1u32;
                        if WaitForSingleObject(info.hProcess, 60_000) == WAIT_OBJECT_0 {
                            let _ = GetExitCodeProcess(info.hProcess, &mut code);
                        }
                        let _ = CloseHandle(info.hProcess);
                        if code == 0 {
                            sill_host_core::log(&format!("Windows Firewall: added \"Sill (TCP {port})\" for Private networks."));
                        } else {
                            sill_host_core::log(&format!("Windows Firewall: netsh could not add the rule (exit {code})."));
                        }
                    }
                    Err(e) if e.code() == ERROR_CANCELLED.to_hresult() => {
                        sill_host_core::log("Windows Firewall: the administrator prompt was declined; nothing changed.");
                    }
                    Err(e) => sill_host_core::log(&format!("Windows Firewall: the rule could not be added ({e}).")),
                }
            }
            refresh(port);
        })
        .ok();
}
