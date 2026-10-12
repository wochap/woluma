//! IIO light sensor source: reads lux from sysfs (channel names from wluma).

use super::{normalize_lux, AlsReading, AlsSource, SourceKind, UnknownReason};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const DEVICES: &str = "/sys/bus/iio/devices";
const CHANNELS: [&str; 3] = ["in_illuminance", "in_illuminance0", "in_illuminance_clear"];

pub struct Iio {
    /// Directory searched for sensors when `device` is unset.
    root: PathBuf,
    device: Option<PathBuf>,
    warned_missing: bool,
}

impl Iio {
    pub fn new(device: Option<PathBuf>) -> Self {
        Self::with_root(DEVICES.into(), device)
    }

    fn with_root(root: PathBuf, device: Option<PathBuf>) -> Self {
        Self {
            root,
            device,
            warned_missing: false,
        }
    }

    /// The device directory and channel prefix to read.
    fn find(&self) -> Option<(PathBuf, &'static str)> {
        let candidates = match &self.device {
            Some(device) => vec![device.clone()],
            None => {
                let mut dirs: Vec<_> = fs::read_dir(&self.root)
                    .ok()?
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .collect();
                dirs.sort();
                dirs
            }
        };
        candidates.into_iter().find_map(|dir| {
            let channel = CHANNELS.into_iter().find(|c| {
                dir.join(format!("{c}_input")).exists() || dir.join(format!("{c}_raw")).exists()
            })?;
            Some((dir, channel))
        })
    }

    fn try_sample(&mut self) -> Result<u8, UnknownReason> {
        let Some((dir, channel)) = self.find() else {
            if !self.warned_missing {
                self.warned_missing = true;
                log::warn!("No IIO illuminance sensor found; the ambient reading stays unknown");
            }
            return Err(UnknownReason::Missing);
        };
        read_lux(&dir, channel).map(normalize_lux).map_err(|e| {
            log::debug!("IIO read from {} failed: {e}", dir.display());
            UnknownReason::Error
        })
    }
}

impl AlsSource for Iio {
    fn sample(&mut self) -> AlsReading {
        AlsReading::from_result(SourceKind::Iio, self.try_sample())
    }
}

fn read_f64(path: &Path) -> io::Result<f64> {
    fs::read_to_string(path)?
        .trim()
        .parse()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Lux from `<channel>_input`, else `(raw + offset) × scale`.
fn read_lux(dir: &Path, channel: &str) -> io::Result<f64> {
    let input = dir.join(format!("{channel}_input"));
    if input.exists() {
        return read_f64(&input);
    }
    let raw = read_f64(&dir.join(format!("{channel}_raw")))?;
    let optional = |suffix: &str, default: f64| {
        let path = dir.join(format!("{channel}_{suffix}"));
        if path.exists() {
            read_f64(&path)
        } else {
            Ok(default)
        }
    };
    Ok((raw + optional("offset", 0.0)?) * optional("scale", 1.0)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(name: &str, files: &[(&str, &str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("woluma-iio-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for (device, file, content) in files {
            let dir = root.join(device);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(file), content).unwrap();
        }
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn input_channel() {
        let root = tree(
            "input",
            &[
                ("iio:device0", "in_accel_x_raw", "1"),
                ("iio:device1", "in_illuminance_input", "100\n"),
            ],
        );
        let mut iio = Iio::with_root(root.clone(), None);
        assert_eq!(iio.sample().value, Some(50));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn raw_scale_offset() {
        let root = tree(
            "raw",
            &[
                ("iio:device0", "in_illuminance0_raw", "40"),
                ("iio:device0", "in_illuminance0_offset", "10"),
                ("iio:device0", "in_illuminance0_scale", "2.0"),
            ],
        );
        assert_eq!(
            read_lux(&root.join("iio:device0"), "in_illuminance0").unwrap(),
            100.0
        );
        let mut iio = Iio::with_root("/nonexistent".into(), Some(root.join("iio:device0")));
        assert_eq!(iio.sample().value, Some(50));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_and_error() {
        let root = tree("missing", &[("iio:device0", "in_accel_x_raw", "1")]);
        let mut iio = Iio::with_root(root.clone(), None);
        let reading = iio.sample();
        assert_eq!(reading.value, None);
        assert_eq!(reading.reason, Some(UnknownReason::Missing));
        fs::remove_dir_all(root).unwrap();

        let root = tree("error", &[("iio:device0", "in_illuminance_raw", "garbage")]);
        let mut iio = Iio::with_root(root.clone(), None);
        assert_eq!(iio.sample().reason, Some(UnknownReason::Error));
        fs::remove_dir_all(root).unwrap();
    }
}
