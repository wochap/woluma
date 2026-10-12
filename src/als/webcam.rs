//! Webcam source: per sample, open the device, lock exposure, average the Y channel of a
//! short burst of frames, restore the controls and close the device.

use super::v4l2::{fourcc, Device, PixFormat};
use super::{io_reason, normalize_y, AlsReading, AlsSource, SourceKind, UnknownReason};
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::Hasher;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

const CID_AUTO_EXPOSURE: u32 = 0x009a_0901;
const CID_EXPOSURE_ABSOLUTE: u32 = 0x009a_0902;
const EXPOSURE_MANUAL: i32 = 1;
/// Below this spatial stddev of Y the mean frame is a flat, synthetic field.
const PLACEHOLDER_STDDEV: f64 = 1.0;
const FRAME_TIMEOUT: Duration = Duration::from_secs(2);
const BUFFERS: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    Yuyv,
    Grey,
}

impl Layout {
    fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Yuyv => 2,
            Self::Grey => 1,
        }
    }
}

pub struct Webcam {
    device: PathBuf,
    frames: u32,
    exposure: u32,
    marker: Option<PathBuf>,
    warned_clamp: bool,
}

impl Webcam {
    pub fn new(device: PathBuf, frames: u32, exposure: u32) -> Self {
        let marker = std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|d| !d.is_empty())
            .map(|d| Path::new(&d).join("woluma").join("webcam-controls"));
        Self {
            device,
            frames,
            exposure,
            marker,
            warned_clamp: false,
        }
    }

    fn try_sample(&mut self) -> Result<u8, UnknownReason> {
        let device = Device::open(&self.device).map_err(|e| {
            log::debug!("Unable to open {}: {e}", self.device.display());
            io_reason(&e)
        })?;
        if let Some(marker) = &self.marker {
            recover(&device, &self.device, marker);
        }
        let (layout, format) = select_format(&device)?;
        let exposure = self.clamped_exposure(&device)?;

        let analysis = {
            let _guard = Guard::lock(&device, exposure, &self.device, self.marker.as_deref())?;
            capture(&device, layout, &format, self.frames)?
        }; // controls restored here, device closed when it drops below
        drop(device);

        log::debug!(
            "webcam sample: mean Y {:.2}, stddev {:.2}, identical frames {}",
            analysis.mean,
            analysis.stddev,
            analysis.identical
        );
        if analysis.is_placeholder() {
            return Err(UnknownReason::Placeholder);
        }
        Ok(normalize_y(analysis.mean))
    }

    /// The configured exposure clamped into the device's range, warning once.
    fn clamped_exposure(&mut self, device: &Device) -> Result<i32, UnknownReason> {
        let (min, max) = device.control_range(CID_EXPOSURE_ABSOLUTE).map_err(|e| {
            log::debug!(
                "{} has no exposure_time_absolute control: {e}",
                self.device.display()
            );
            io_reason(&e)
        })?;
        let wanted = self.exposure.min(i32::MAX as u32) as i32;
        let clamped = wanted.clamp(min, max);
        if clamped != wanted && !self.warned_clamp {
            self.warned_clamp = true;
            log::warn!(
                "als.exposure {wanted} is outside {}'s range {min}-{max}, using {clamped}",
                self.device.display(),
            );
        }
        Ok(clamped)
    }
}

impl AlsSource for Webcam {
    fn sample(&mut self) -> AlsReading {
        AlsReading::from_result(SourceKind::Webcam, self.try_sample())
    }
}

fn get(device: &Device, id: u32) -> io::Result<i32> {
    device.control(id)
}

fn set(device: &Device, id: u32, value: i32) -> io::Result<()> {
    device.set_control(id, value)
}

/// Picks YUYV (or GREY) at the smallest frame size and applies it.
fn select_format(device: &Device) -> Result<(Layout, PixFormat), UnknownReason> {
    let formats = device.formats().map_err(|e| io_reason(&e))?;
    let (layout, pixelformat) = [
        (Layout::Yuyv, fourcc(b"YUYV")),
        (Layout::Grey, fourcc(b"GREY")),
    ]
    .into_iter()
    .find(|(_, f)| formats.contains(f))
    .ok_or(UnknownReason::UnsupportedFormat)?;
    let (width, height) = device
        .frame_sizes(pixelformat)
        .map_err(|e| io_reason(&e))?
        .into_iter()
        .min_by_key(|(w, h)| w * h)
        .ok_or(UnknownReason::UnsupportedFormat)?;
    let format = device
        .set_format(width, height, pixelformat)
        .map_err(|e| io_reason(&e))?;
    if format.pixelformat != pixelformat {
        return Err(UnknownReason::UnsupportedFormat);
    }
    Ok((layout, format))
}

/// Saved control values, written back on drop. Only controls that were changed are kept.
struct Guard<'a> {
    device: &'a Device,
    saved: Vec<(u32, i32)>,
    marker: Option<&'a Path>,
}

impl<'a> Guard<'a> {
    fn lock(
        device: &'a Device,
        exposure: i32,
        path: &Path,
        marker: Option<&'a Path>,
    ) -> Result<Self, UnknownReason> {
        let auto = get(device, CID_AUTO_EXPOSURE).map_err(|e| io_reason(&e))?;
        let current = get(device, CID_EXPOSURE_ABSOLUTE).map_err(|e| io_reason(&e))?;
        if let Some(marker) = marker {
            write_marker(marker, path, auto, current);
        }
        let mut guard = Self {
            device,
            saved: Vec::new(),
            marker,
        };
        for (id, previous, value) in [
            (CID_AUTO_EXPOSURE, auto, EXPOSURE_MANUAL),
            (CID_EXPOSURE_ABSOLUTE, current, exposure),
        ] {
            if previous != value {
                guard.saved.push((id, previous));
                set(device, id, value).map_err(|e| {
                    log::debug!("Unable to set control {id:#x}: {e}");
                    io_reason(&e)
                })?;
            }
        }
        Ok(guard)
    }
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        // Reverse order: the exposure time goes back while still in manual mode.
        for &(id, value) in self.saved.iter().rev() {
            if let Err(e) = set(self.device, id, value) {
                log::warn!("Unable to restore webcam control {id:#x} to {value}: {e}");
            }
        }
        if let Some(marker) = self.marker {
            let _ = fs::remove_file(marker);
        }
    }
}

fn write_marker(marker: &Path, device: &Path, auto: i32, exposure: i32) {
    let content = format!(
        "device={}\nauto_exposure={auto}\nexposure_time_absolute={exposure}\n",
        device.display()
    );
    let result = marker
        .parent()
        .map_or(Ok(()), fs::create_dir_all)
        .and_then(|_| fs::write(marker, content));
    if let Err(e) = result {
        log::debug!("Unable to write {}: {e}", marker.display());
    }
}

/// Parses a marker into (device, auto_exposure, exposure_time_absolute).
fn parse_marker(text: &str) -> Option<(PathBuf, i32, i32)> {
    let mut device = None;
    let mut auto = None;
    let mut exposure = None;
    for line in text.lines() {
        match line.split_once('=')? {
            ("device", v) => device = Some(PathBuf::from(v)),
            ("auto_exposure", v) => auto = v.parse().ok(),
            ("exposure_time_absolute", v) => exposure = v.parse().ok(),
            _ => {}
        }
    }
    Some((device?, auto?, exposure?))
}

/// Restores controls left locked by a previous run that died mid-sample.
fn recover(device: &Device, path: &Path, marker: &Path) {
    let Ok(text) = fs::read_to_string(marker) else {
        return;
    };
    match parse_marker(&text) {
        Some((saved_path, auto, exposure)) if saved_path == path => {
            log::info!("Restoring webcam controls left by an interrupted sample");
            for (id, value) in [(CID_EXPOSURE_ABSOLUTE, exposure), (CID_AUTO_EXPOSURE, auto)] {
                if let Err(e) = set(device, id, value) {
                    log::warn!("Unable to restore webcam control {id:#x} to {value}: {e}");
                }
            }
            let _ = fs::remove_file(marker);
        }
        Some(_) => {}
        None => {
            let _ = fs::remove_file(marker);
        }
    }
}

fn capture(
    device: &Device,
    layout: Layout,
    format: &PixFormat,
    frames: u32,
) -> Result<Analysis, UnknownReason> {
    let mut acc = Accumulator::new(
        layout,
        format.width as usize,
        format.height as usize,
        format.bytesperline as usize,
    );
    device
        .capture(BUFFERS, frames, FRAME_TIMEOUT, |data| {
            acc.add(data).map_err(Failure::Frame)
        })
        .map_err(|e| match e {
            Failure::Io(e) => {
                log::debug!("Webcam capture failed: {e}");
                io_reason(&e)
            }
            Failure::Frame(reason) => reason,
        })?;
    acc.finish().ok_or(UnknownReason::Error)
}

enum Failure {
    Io(io::Error),
    Frame(UnknownReason),
}

impl From<io::Error> for Failure {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Analysis {
    mean: f64,
    /// Spatial standard deviation of Y over the mean frame.
    stddev: f64,
    /// More than one frame, all byte-identical.
    identical: bool,
}

impl Analysis {
    fn is_placeholder(&self) -> bool {
        self.identical || self.stddev < PLACEHOLDER_STDDEV
    }
}

/// Sums Y per pixel across frames and hashes each frame's Y plane.
struct Accumulator {
    layout: Layout,
    width: usize,
    height: usize,
    stride: usize,
    sums: Vec<u64>,
    hashes: Vec<u64>,
}

impl Accumulator {
    fn new(layout: Layout, width: usize, height: usize, stride: usize) -> Self {
        Self {
            layout,
            width,
            height,
            stride: stride.max(width * layout.bytes_per_pixel()),
            sums: vec![0; width * height],
            hashes: Vec::new(),
        }
    }

    fn add(&mut self, data: &[u8]) -> Result<(), UnknownReason> {
        let row_bytes = self.width * self.layout.bytes_per_pixel();
        if self.height == 0 || data.len() < self.stride * (self.height - 1) + row_bytes {
            log::debug!("Short webcam frame: {} bytes", data.len());
            return Err(UnknownReason::Error);
        }
        let step = self.layout.bytes_per_pixel();
        let mut hasher = DefaultHasher::new();
        for row in 0..self.height {
            let line = &data[row * self.stride..row * self.stride + row_bytes];
            let sums = &mut self.sums[row * self.width..(row + 1) * self.width];
            for (sum, y) in sums.iter_mut().zip(line.iter().step_by(step)) {
                *sum += *y as u64;
                hasher.write_u8(*y);
            }
        }
        self.hashes.push(hasher.finish());
        Ok(())
    }

    fn finish(self) -> Option<Analysis> {
        let n = self.hashes.len() as f64;
        if n == 0.0 || self.sums.is_empty() {
            return None;
        }
        let pixels = self.sums.len() as f64;
        let mean = self.sums.iter().map(|&s| s as f64 / n).sum::<f64>() / pixels;
        let variance = self
            .sums
            .iter()
            .map(|&s| (s as f64 / n - mean).powi(2))
            .sum::<f64>()
            / pixels;
        let identical = self.hashes.len() > 1 && self.hashes.iter().all(|h| *h == self.hashes[0]);
        Some(Analysis {
            mean,
            stddev: variance.sqrt(),
            identical,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 32;
    const H: usize = 24;

    /// YUYV frame with Y from `y(x, y)` and fixed chroma.
    fn yuyv(y: impl Fn(usize, usize) -> u8) -> Vec<u8> {
        let mut data = Vec::with_capacity(W * H * 2);
        for row in 0..H {
            for col in 0..W {
                data.push(y(col, row));
                data.push(128);
            }
        }
        data
    }

    fn analyze(frames: &[Vec<u8>]) -> Analysis {
        let mut acc = Accumulator::new(Layout::Yuyv, W, H, W * 2);
        for f in frames {
            acc.add(f).unwrap();
        }
        acc.finish().unwrap()
    }

    /// Deterministic pseudo-noise in -2..=2.
    fn noise(seed: usize, x: usize, y: usize) -> i32 {
        ((seed * 7919 + x * 31 + y * 17) * 2654435761 % 5) as i32 - 2
    }

    #[test]
    fn identical_frames_are_placeholder() {
        let frame = yuyv(|x, y| 40 + ((x + y) % 30) as u8);
        let a = analyze(&vec![frame; 10]);
        assert!(a.identical);
        assert!(a.stddev > PLACEHOLDER_STDDEV);
        assert!(a.is_placeholder());
    }

    #[test]
    fn flat_noisy_field_is_placeholder() {
        // Per-frame noise averages out to a flat mean frame.
        let frames: Vec<_> = (0..10)
            .map(|i| yuyv(|x, y| (137 + noise(i, x, y)) as u8))
            .collect();
        let a = analyze(&frames);
        assert!(!a.identical);
        assert!(a.stddev < PLACEHOLDER_STDDEV);
        assert!(a.is_placeholder());
        assert!((a.mean - 137.0).abs() < 1.0);
    }

    #[test]
    fn gradient_with_noise_is_real() {
        let frames: Vec<_> = (0..10)
            .map(|i| yuyv(|x, y| (40 + x as i32 + y as i32 + noise(i, x, y)) as u8))
            .collect();
        let a = analyze(&frames);
        assert!(!a.identical);
        assert!(!a.is_placeholder());
        // Mean of 40 + x + y over the grid.
        let expected = 40.0 + (W - 1) as f64 / 2.0 + (H - 1) as f64 / 2.0;
        assert!((a.mean - expected).abs() < 0.5, "{}", a.mean);
    }

    #[test]
    fn mean_and_stddev_exact() {
        // Left half 20, right half 60: mean 40, stddev 20.
        let frame = yuyv(|x, _| if x < W / 2 { 20 } else { 60 });
        let a = analyze(&[frame]);
        assert_eq!(a.mean, 40.0);
        assert_eq!(a.stddev, 20.0);
        assert!(
            !a.identical,
            "a single frame cannot be temporally identical"
        );
    }

    #[test]
    fn grey_layout_and_stride() {
        let stride = W + 8;
        let mut data = vec![255u8; stride * H];
        for row in 0..H {
            for col in 0..W {
                data[row * stride + col] = if col % 2 == 0 { 16 } else { 36 };
            }
        }
        let mut acc = Accumulator::new(Layout::Grey, W, H, stride);
        acc.add(&data).unwrap();
        let a = acc.finish().unwrap();
        assert_eq!(a.mean, 26.0);
    }

    #[test]
    fn short_frame_is_error() {
        let mut acc = Accumulator::new(Layout::Yuyv, W, H, W * 2);
        assert_eq!(acc.add(&[0; 10]), Err(UnknownReason::Error));
    }

    #[test]
    fn marker_round_trip() {
        assert_eq!(
            parse_marker("device=/dev/video0\nauto_exposure=3\nexposure_time_absolute=156\n"),
            Some(("/dev/video0".into(), 3, 156))
        );
        assert_eq!(parse_marker("device=/dev/video0\n"), None);
    }
}
