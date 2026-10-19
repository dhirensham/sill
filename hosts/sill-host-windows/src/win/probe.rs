//! The check behind the way back from the software encoder (encoderhealth.rs): whether a hardware
//! HEVC encoder keeps up at a stream's size now. Frames of a test pattern go through a new encoder
//! on a Direct3D device of its own, the next when the encoder takes one (as a stream feeds it: one
//! frame inside at a time), and the rate the last seven came back at is the answer. Three frames
//! are drawn before the encoder opens and reused, as a capture reuses its surfaces; the first
//! three outputs (the encoder's warm-up, each surface's first trip) are left out of the timing.
//! A frame that stays inside for a second ends it: no answer. Blocks its thread for about 10 / the
//! rate plus the drawing, a second at most past the last progress: never call it on a stream's
//! thread.
//!
//! TEST ONLY: SILL_TEST_PROBE_HOLD=S holds every frame S seconds before it goes in (0.08: a starved
//! encoder, about 12 fps; 1 or more: no answer), as the Mac's EncoderProbe does.

use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use sill_host_core::platform::EncodedFrame;
use windows::Win32::Foundation::LUID;

use super::d3d::D3d;
use super::encoder::{self, Encoder, Input, Settings};
use super::util::{com_init, log};
use crate::encoderhealth::Check;

/// Frames timed, after the warm-up.
const TIMED: usize = 7;
/// Outputs left out of the timing.
const WARM_UP: usize = 3;
/// A frame inside this long without anything coming back: no answer.
const TIMEOUT: Duration = Duration::from_secs(1);

fn test_hold() -> Duration {
    static HOLD: OnceLock<Duration> = OnceLock::new();
    *HOLD.get_or_init(|| {
        let hold = std::env::var("SILL_TEST_PROBE_HOLD")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|s| s.is_finite() && *s > 0.0)
            .map(Duration::from_secs_f64)
            .unwrap_or_default();
        if !hold.is_zero() {
            log(&format!("TEST: every frame of every encoder check waits {} ms before it goes in (SILL_TEST_PROBE_HOLD)", hold.as_millis()));
        }
        hold
    })
}

/// The first hardware HEVC encoder on `adapter` (or on any adapter when None), checked at
/// `width`×`height` for a stream at `fps` and `bitrate`.
pub fn throughput(adapter: Option<LUID>, width: u32, height: u32, fps: u32, bitrate: u32) -> Check {
    com_init();
    if let Err(e) = encoder::startup() {
        return Check::Refused(e);
    }
    let candidates = encoder::candidates();
    let Some(candidate) = candidates
        .iter()
        .filter(|c| c.hardware)
        .find(|c| adapter.is_none() || c.adapter.is_none() || c.adapter.map(luid_key) == adapter.map(luid_key))
    else {
        return Check::Refused("no hardware HEVC encoder on this GPU".into());
    };
    let d3d = match D3d::new(candidate.adapter.or(adapter)) {
        Ok(d) => d,
        Err(e) => return Check::Refused(format!("Direct3D 11: {e}")),
    };
    let (width, height) = (width.max(64) & !1, height.max(64) & !1);
    let drawn: Vec<Vec<u8>> = (0..3).map(|i| pattern(width, height, i)).collect();
    let textures: Vec<_> = drawn.iter().filter_map(|bytes| d3d.nv12_with(width, height, bytes).ok()).collect();

    // Each output's time, and a wake for every output and every request for input.
    let signal = Arc::new((Mutex::new(Vec::<Instant>::new()), Condvar::new()));
    let deliver: Box<dyn Fn(EncodedFrame) + Send + Sync> = {
        let signal = signal.clone();
        Box::new(move |_frame| {
            if let Ok(mut outputs) = signal.0.lock() {
                outputs.push(Instant::now());
            }
            signal.1.notify_all();
        })
    };
    let wake: Arc<dyn Fn() + Send + Sync> = {
        let signal = signal.clone();
        Arc::new(move || signal.1.notify_all())
    };
    let settings = Settings { width, height, fps: fps.max(1), bitrate: bitrate.max(100_000), prioritize_speed: false };
    let enc = match Encoder::new(candidate, &d3d, settings, deliver, wake) {
        Ok(e) => e,
        Err(e) => return Check::Refused(e),
    };
    let check = if enc.d3d_aware && textures.len() < drawn.len() {
        Check::Answered(None) // cannot feed it a texture: nothing to time, so assume it keeps up
    } else {
        measure(&enc, &textures, &drawn, settings, &signal)
    };
    enc.retire();
    if matches!(check, Check::Answered(_)) {
        // Healthy: shut down before the answer, so the stream's next encoder is never made while
        // this one shuts down (Intel's MFT crashed the process when the two overlapped).
        drop(enc);
    } else {
        // The answer first: a hung driver can block the encoder's teardown, which goes on elsewhere
        // (the stream stays on the software encoder meanwhile).
        let _ = std::thread::Builder::new().name("sill-probe-teardown".into()).spawn(move || {
            com_init();
            drop(enc)
        });
    }
    check
}

/// Feeds `enc` as a stream does and times what comes back (`throughput`).
fn measure(
    enc: &Encoder,
    textures: &[windows::Win32::Graphics::Direct3D11::ID3D11Texture2D],
    drawn: &[Vec<u8>],
    settings: Settings,
    signal: &Arc<(Mutex<Vec<Instant>>, Condvar)>,
) -> Check {
    let hold = test_hold();
    let total = WARM_UP + TIMED;
    let duration = 10_000_000 / settings.fps as i64;
    let mut submitted = 0usize;
    let started = Instant::now();
    let mut progress = Instant::now();
    let mut outputs_seen = 0usize;
    loop {
        let outputs = signal.0.lock().map(|o| o.len()).unwrap_or(0);
        if outputs >= total {
            break;
        }
        if outputs > outputs_seen {
            outputs_seen = outputs;
            progress = Instant::now();
        }
        if enc.error().is_some() || progress.elapsed() > TIMEOUT {
            return Check::NoAnswer(started.elapsed());
        }
        // A few more than are timed, for an encoder that drops a frame under its rate control.
        if submitted < total + 5 && enc.accepts() {
            if !hold.is_zero() {
                if hold >= TIMEOUT {
                    std::thread::sleep(TIMEOUT);
                    return Check::NoAnswer(started.elapsed());
                }
                std::thread::sleep(hold);
            }
            let i = submitted % drawn.len();
            let input = if enc.d3d_aware { Input::Texture(textures[i].clone()) } else { Input::Bytes(drawn[i].clone()) };
            if let Err(e) = enc.submit(input, submitted as i64 * duration, submitted == 0) {
                return Check::Refused(e);
            }
            submitted += 1;
            if submitted == 1 {
                progress = Instant::now();
            }
            continue;
        }
        let guard = match signal.0.lock() {
            Ok(g) => g,
            Err(_) => return Check::NoAnswer(started.elapsed()),
        };
        let _ = signal.1.wait_timeout(guard, Duration::from_millis(20));
    }
    let outputs = signal.0.lock().map(|o| o.clone()).unwrap_or_default();
    let fps = match (outputs.get(WARM_UP - 1), outputs.get(total - 1)) {
        (Some(first), Some(last)) if last > first => Some(TIMED as f64 / (*last - *first).as_secs_f64()),
        _ => None,
    };
    Check::Answered(fps)
}

fn luid_key(l: LUID) -> (u32, i32) {
    (l.LowPart, l.HighPart)
}

/// Frame `index` of the test pattern: a diagonal gradient with a bright bar that moves from frame
/// to frame and a little noise, so the encoder has edges, motion and texture to work on, as on a
/// desktop, without the incompressible bits pure noise would make.
fn pattern(width: u32, height: u32, index: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut out = vec![0u8; w * h * 3 / 2];
    let bar = (index as usize * w / 3 + w / 8) % w;
    let bar_width = (w / 16).max(4);
    let mut seed = 0x9E37_79B9_7F4A_7C15u64 ^ index as u64;
    for y in 0..h {
        let row = &mut out[y * w..(y + 1) * w];
        for (x, px) in row.iter_mut().enumerate() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let base = ((x + y + index as usize * 37) / 8) as u8;
            let lit = x >= bar && x < bar + bar_width;
            *px = if lit { 235 } else { base.wrapping_add((seed & 0x0F) as u8).clamp(16, 235) };
        }
    }
    for (i, px) in out[w * h..].iter_mut().enumerate() {
        *px = if i % 2 == 0 { 128u8.wrapping_add((index * 12) as u8) } else { 128u8.wrapping_sub((index * 8) as u8) };
    }
    out
}
