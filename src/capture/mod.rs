//! Per-output screen capture producing perceived-lightness ("luma") samples.
//!
//! The capturer and Vulkan code are adapted from wluma (ISC License, see NOTICE).

pub mod capturer;
mod object;
pub mod outputs;
pub mod vulkan;

use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub use capturer::WaylandProtocol;

/// Interval between luma samples for one output.
pub const SAMPLE_INTERVAL: Duration = Duration::from_millis(500);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const ABSENT_RETRY: Duration = Duration::from_secs(5);
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

pub fn compute_perceived_lightness_percent(rgbas: &[u8], has_alpha: bool, pixels: usize) -> u8 {
    let channels = if has_alpha { 4 } else { 3 };
    let (rs, gs, bs) = rgbas
        .chunks_exact(channels)
        .take(pixels)
        .fold((0.0, 0.0, 0.0), |(rs, gs, bs), px| {
            (rs + px[0] as f64, gs + px[1] as f64, bs + px[2] as f64)
        });

    let pixels = pixels as f64;
    let (r, g, b) = (rs / pixels, gs / pixels, bs / pixels);

    let result = (0.241 * r * r + 0.691 * g * g + 0.068 * b * b).sqrt() / 255.0 * 100.0;

    result.round() as u8
}

/// Fails unless the compositor offers ext-image-copy-capture-v1 or wlr-screencopy-unstable-v1.
pub fn check_protocols() -> Result<()> {
    let protocols =
        capturer::wayland::Capturer::supported_protocols(Instant::now() + STARTUP_TIMEOUT)?;
    if protocols.iter().any(|p| {
        matches!(
            p,
            WaylandProtocol::ExtImageCopyCaptureV1 | WaylandProtocol::WlrScreencopyUnstableV1
        )
    }) {
        Ok(())
    } else {
        Err(anyhow!(
            "the compositor supports neither ext-image-copy-capture-v1 nor wlr-screencopy-unstable-v1 \
             (with linux-dmabuf-v1); screen capture is not possible"
        ))
    }
}

/// Starts capturing `output` on a dedicated thread, returning its luma stream. The thread
/// keeps retrying while the output is absent or capture fails, and exits once `active` is
/// cleared or the receiver is dropped.
pub fn luma_stream(output: String, active: Arc<AtomicBool>) -> async_channel::Receiver<u8> {
    let (sender, receiver) = async_channel::bounded(4);
    std::thread::Builder::new()
        .name(format!("capture-{output}"))
        .spawn(move || supervise(&output, sender, active))
        .expect("unable to spawn capture thread");
    receiver
}

/// Captures one luma sample from `output`, or `None` if none arrives within `timeout`.
pub fn sample_once(output: &str, timeout: Duration) -> Option<u8> {
    let active = Arc::new(AtomicBool::new(true));
    let stream = luma_stream(output.to_string(), active.clone());
    let luma = smol::block_on(futures_lite::future::or(
        async { stream.recv().await.ok() },
        async {
            smol::Timer::after(timeout).await;
            None
        },
    ));
    active.store(false, Ordering::Relaxed);
    luma
}

fn supervise(output: &str, sender: async_channel::Sender<u8>, active: Arc<AtomicBool>) {
    let mut backoff = BACKOFF_MIN;
    let mut absent_logged = false;
    while active.load(Ordering::Relaxed) && !sender.is_closed() {
        let mut capturer = capturer::wayland::Capturer::new(WaylandProtocol::Any);
        let (established, result) = capturer.run(
            output,
            sender.clone(),
            None,
            active.clone(),
            Instant::now() + STARTUP_TIMEOUT,
        );
        if !active.load(Ordering::Relaxed) {
            return;
        }
        let delay = match result {
            Ok(()) => return,
            Err(error) if !established && error.to_string().contains("Unable to match") => {
                if !absent_logged {
                    log::info!("Output '{output}' is not connected, waiting for it to appear");
                    absent_logged = true;
                }
                ABSENT_RETRY
            }
            Err(error) => {
                absent_logged = false;
                if established {
                    backoff = BACKOFF_MIN;
                }
                log::warn!("Capture failed for '{output}': {error:#}; retrying in {backoff:?}");
                let delay = backoff;
                backoff = (backoff * 2).min(BACKOFF_MAX);
                delay
            }
        };
        if established {
            log::info!("Capture for '{output}' stopped");
        }
        wait_while_active(&active, delay);
    }
}

fn wait_while_active(active: &AtomicBool, duration: Duration) {
    let deadline = Instant::now() + duration;
    while active.load(Ordering::Relaxed) && Instant::now() < deadline {
        std::thread::sleep(
            Duration::from_millis(100).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perceived_lightness() {
        assert_eq!(
            compute_perceived_lightness_percent(&[255, 255, 255, 255], true, 1),
            100
        );
        assert_eq!(
            compute_perceived_lightness_percent(&[0, 0, 0, 255], true, 1),
            0
        );
        assert_eq!(
            compute_perceived_lightness_percent(&[128, 128, 128], false, 1),
            50
        );
        // Mean of white and black is mid-grey.
        assert_eq!(
            compute_perceived_lightness_percent(&[255, 255, 255, 0, 0, 0], false, 2),
            50
        );
    }

    /// Opt-in: `WOLUMA_CAPTURE_SMOKE=1 cargo test -- --ignored capture_smoke`. Captures one
    /// frame per connected output named in `WOLUMA_CAPTURE_OUTPUTS` (comma separated).
    #[test]
    #[ignore]
    fn capture_smoke() {
        if std::env::var_os("WOLUMA_CAPTURE_SMOKE").is_none() {
            return;
        }
        check_protocols().unwrap();
        let outputs = std::env::var("WOLUMA_CAPTURE_OUTPUTS").unwrap_or_else(|_| "eDP-1".into());
        for output in outputs.split(',') {
            let active = Arc::new(AtomicBool::new(true));
            let stream = luma_stream(output.to_string(), active.clone());
            let luma = smol::block_on(async {
                futures_lite::future::or(async { stream.recv().await.ok() }, async {
                    smol::Timer::after(Duration::from_secs(10)).await;
                    None
                })
                .await
            });
            active.store(false, Ordering::Relaxed);
            let luma = luma.unwrap_or_else(|| panic!("no frame captured for {output}"));
            assert!(luma <= 100);
            eprintln!("{output}: luma {luma}");
        }
    }
}
