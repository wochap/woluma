//! Ambient light: one shared 0-100 reading (or unknown) sampled from a webcam with locked
//! exposure, an IIO light sensor, or nothing.

mod iio;
mod sampler;
mod v4l2;
mod webcam;

pub use sampler::Sampler;

use crate::config::Als;
use chrono::{DateTime, Local};
use std::fmt;

/// Why a reading is unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownReason {
    Missing,
    Busy,
    Error,
    Placeholder,
    UnsupportedFormat,
    Disabled,
}

impl fmt::Display for UnknownReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Missing => "missing",
            Self::Busy => "busy",
            Self::Error => "error",
            Self::Placeholder => "placeholder",
            Self::UnsupportedFormat => "unsupported-format",
            Self::Disabled => "disabled",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    None,
    Webcam,
    Iio,
}

impl fmt::Display for SourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::None => "none",
            Self::Webcam => "webcam",
            Self::Iio => "iio",
        })
    }
}

/// A published sample: `value` is `None` exactly when `reason` is set.
#[derive(Debug, Clone, PartialEq)]
pub struct AlsReading {
    pub value: Option<u8>,
    pub reason: Option<UnknownReason>,
    pub source: SourceKind,
    pub taken_at: DateTime<Local>,
}

impl AlsReading {
    pub fn from_result(source: SourceKind, result: Result<u8, UnknownReason>) -> Self {
        let (value, reason) = match result {
            Ok(v) => (Some(v), None),
            Err(r) => (None, Some(r)),
        };
        Self {
            value,
            reason,
            source,
            taken_at: Local::now(),
        }
    }
}

impl fmt::Display for AlsReading {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.value, self.reason) {
            (Some(v), _) => write!(f, "{v} ({})", self.source),
            (None, Some(r)) => write!(f, "unknown: {r} ({})", self.source),
            (None, None) => write!(f, "unknown ({})", self.source),
        }
    }
}

/// A blocking ambient light source, sampled from the sampler thread.
pub trait AlsSource: Send {
    fn sample(&mut self) -> AlsReading;
}

/// Source `none`: always unknown, touches no device.
pub struct NoSource;

impl AlsSource for NoSource {
    fn sample(&mut self) -> AlsReading {
        AlsReading::from_result(SourceKind::None, Err(UnknownReason::Disabled))
    }
}

/// Builds the configured source.
pub fn source(config: &Als) -> Box<dyn AlsSource> {
    match config {
        Als::None => Box::new(NoSource),
        Als::Webcam {
            device,
            frames,
            exposure,
            ..
        } => Box::new(webcam::Webcam::new(device.into(), *frames, *exposure)),
        Als::Iio { device, .. } => Box::new(iio::Iio::new(device.as_ref().map(Into::into))),
    }
}

/// Limited-range luma (16 black, 235 white) to 0-100.
pub fn normalize_y(mean_y: f64) -> u8 {
    ((mean_y - 16.0) / 219.0 * 100.0).clamp(0.0, 100.0).round() as u8
}

/// Lux to 0-100 on a log scale: 100 lux is 50, 10 000 lux and above is 100.
pub fn normalize_lux(lux: f64) -> u8 {
    ((1.0 + lux.max(0.0)).log10() / 4.0 * 100.0)
        .clamp(0.0, 100.0)
        .round() as u8
}

/// Maps an I/O error to an unknown reason.
fn io_reason(error: &std::io::Error) -> UnknownReason {
    match error.raw_os_error() {
        Some(libc::ENOENT) | Some(libc::ENODEV) | Some(libc::ENXIO) => UnknownReason::Missing,
        Some(libc::EBUSY) => UnknownReason::Busy,
        _ => UnknownReason::Error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webcam_normalization() {
        assert_eq!(normalize_y(16.0), 0);
        assert_eq!(normalize_y(68.9), 24);
        assert_eq!(normalize_y(56.8), 19);
        assert_eq!(normalize_y(240.0), 100);
        assert_eq!(normalize_y(0.0), 0);
    }

    #[test]
    fn lux_normalization() {
        assert_eq!(normalize_lux(0.0), 0);
        assert_eq!(normalize_lux(100.0), 50);
        assert_eq!(normalize_lux(20000.0), 100);
        assert_eq!(normalize_lux(1.0), 8);
        assert_eq!(normalize_lux(10.0), 26);
    }

    #[test]
    fn io_reasons() {
        let e = |code| std::io::Error::from_raw_os_error(code);
        assert_eq!(io_reason(&e(libc::ENOENT)), UnknownReason::Missing);
        assert_eq!(io_reason(&e(libc::EBUSY)), UnknownReason::Busy);
        assert_eq!(io_reason(&e(libc::EIO)), UnknownReason::Error);
    }
}
