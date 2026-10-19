//! Sill.exe's log (portable: unit-tested on any machine), as Sill.app's HostLog: every line the host
//! logs goes to `%LOCALAPPDATA%\Sill\Sill.log` with the time in front (moved to Sill.1.log at
//! 10 MB) and to the last 5,000 lines in memory, which the Show Log window reads. A windowless exe
//! has no stdout: without this its lines are lost.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub const RING_LINES: usize = 5_000;
const ROTATE_BYTES: u64 = 10 * 1024 * 1024;

struct Log {
    path: Option<PathBuf>,
    file: Option<File>,
    written: u64,
    rotate_at: u64,
    ring: VecDeque<String>,
    /// How many lines were ever logged: the log window asks for the ones after its last.
    total: u64,
}

fn log() -> &'static Mutex<Log> {
    static LOG: OnceLock<Mutex<Log>> = OnceLock::new();
    LOG.get_or_init(|| Mutex::new(Log { path: None, file: None, written: 0, rotate_at: ROTATE_BYTES, ring: VecDeque::new(), total: 0 }))
}

/// Opens (or creates) `dir`\Sill.log for appending and makes it, and the ring, where every host
/// line goes (`sill_host_core::set_log_sink`); a panic is logged there too. The ring works even
/// when the file can't be opened. SILL_TEST_LOG_ROTATE_BYTES makes the rotation testable.
pub fn install(dir: &Path) -> Result<PathBuf, String> {
    let path = dir.join("Sill.log");
    let opened = std::fs::create_dir_all(dir).map_err(|e| e.to_string()).and_then(|_| open(&path));
    {
        let mut l = log().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = std::env::var("SILL_TEST_LOG_ROTATE_BYTES").ok().and_then(|v| v.parse().ok()) {
            l.rotate_at = n;
        }
        l.path = Some(path.clone());
        if let Ok((file, len)) = &opened {
            l.file = file.try_clone().ok();
            l.written = *len;
        }
    }
    sill_host_core::set_log_sink(append);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        append(&format!("Panic: {info}"));
        previous(info);
    }));
    opened.map(|_| path)
}

fn open(path: &Path) -> Result<(File, u64), String> {
    let file = OpenOptions::new().create(true).append(true).open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    Ok((file, len))
}

/// One line: into the ring, and with the time in front into the file. Never calls the core's
/// `log` (it is the core's sink).
pub fn append(line: &str) {
    let mut l = log().lock().unwrap_or_else(|e| e.into_inner());
    if l.ring.len() >= RING_LINES {
        l.ring.pop_front();
    }
    l.ring.push_back(line.to_string());
    l.total += 1;
    let stamped = format!("{} {line}\r\n", now_text());
    if l.written + stamped.len() as u64 > l.rotate_at {
        rotate(&mut l);
    }
    if let Some(f) = l.file.as_mut() {
        if f.write_all(stamped.as_bytes()).is_ok() {
            l.written += stamped.len() as u64;
        }
    }
}

/// Sill.log becomes Sill.1.log (the one before it goes), and a new Sill.log starts.
fn rotate(l: &mut Log) {
    let Some(path) = l.path.clone() else { return };
    l.file = None;
    let old = path.with_file_name("Sill.1.log");
    let _ = std::fs::remove_file(&old);
    let _ = std::fs::rename(&path, &old);
    if let Ok((file, len)) = open(&path) {
        l.file = Some(file);
        l.written = len;
    }
}

/// The lines logged after the first `after` ever (for the log window: everything still in the
/// ring when it is further behind), and how many were logged in all.
pub fn lines_after(after: u64) -> (Vec<String>, u64) {
    let l = log().lock().unwrap_or_else(|e| e.into_inner());
    let first_kept = l.total - l.ring.len() as u64;
    let skip = after.saturating_sub(first_kept) as usize;
    (l.ring.iter().skip(skip).cloned().collect(), l.total)
}

pub fn path() -> Option<PathBuf> {
    log().lock().unwrap_or_else(|e| e.into_inner()).path.clone()
}

/// "2026-10-19 18:46:31.123", local time (as Sill.app's log).
#[cfg(windows)]
fn now_text() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds)
}

#[cfg(not(windows))]
fn now_text() -> String {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    format!("{}.{:03}", d.as_secs(), d.subsec_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    // One test: the log is one per process.
    #[test]
    fn ring_file_and_rotation() {
        let dir = std::env::temp_dir().join(format!("sill-applog-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        {
            let mut l = log().lock().unwrap();
            l.rotate_at = 4_000;
        }
        let path = install(&dir).expect("opens");
        for i in 0..6_000 {
            append(&format!("line {i}"));
        }
        let (lines, total) = lines_after(0);
        assert_eq!(total, 6_000);
        assert_eq!(lines.len(), RING_LINES);
        assert_eq!(lines.first().map(String::as_str), Some("line 1000"));
        let (tail, _) = lines_after(5_998);
        assert_eq!(tail, vec!["line 5998".to_string(), "line 5999".to_string()]);
        assert!(lines_after(6_000).0.is_empty());
        // Rotated: both files exist, neither past the limit by more than a line.
        let now = std::fs::read_to_string(&path).unwrap();
        let before = std::fs::read_to_string(dir.join("Sill.1.log")).unwrap();
        assert!(now.len() <= 4_000 && before.len() <= 4_000, "{} {}", now.len(), before.len());
        assert!(now.trim_end().ends_with("line 5999"), "{now}");
        assert!(now.lines().next().unwrap().split(' ').count() >= 4);
        let _ = std::fs::remove_dir_all(dir);
    }
}
