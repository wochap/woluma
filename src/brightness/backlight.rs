//! `/sys/class/backlight/<name>`: sysfs writes with a logind `SetBrightness` fallback,
//! inotify for external changes.

use super::{mapping::BACKLIGHT_MIN, Device};
use anyhow::{anyhow, Context, Result};
use inotify::{Inotify, WatchMask};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

pub const SYSFS_ROOT: &str = "/sys/class/backlight";

enum WritePath {
    Sysfs,
    Logind(Option<zbus::blocking::Connection>),
}

pub struct Backlight {
    name: String,
    file: PathBuf,
    max: u32,
    write: WritePath,
}

fn read_raw(path: &Path) -> Result<u32> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("unable to read {}", path.display()))?;
    text.trim()
        .parse()
        .with_context(|| format!("invalid value {:?} in {}", text.trim(), path.display()))
}

impl Backlight {
    pub fn open(output: &str, name: &str) -> Result<Self> {
        Self::open_at(Path::new(SYSFS_ROOT), output, name)
    }

    pub fn open_at(root: &Path, output: &str, name: &str) -> Result<Self> {
        let dir = root.join(name);
        if !dir.is_dir() {
            return Err(anyhow!(
                "output '{output}': backlight device '{name}' not found in {}",
                root.display()
            ));
        }
        let file = dir.join("brightness");
        let max = read_raw(&dir.join("max_brightness"))
            .with_context(|| format!("output '{output}': unable to read backlight '{name}'"))?;
        read_raw(&file)
            .with_context(|| format!("output '{output}': unable to read backlight '{name}'"))?;
        let write = match std::fs::OpenOptions::new().write(true).open(&file) {
            Ok(_) => WritePath::Sysfs,
            Err(e) if e.kind() == ErrorKind::PermissionDenied => {
                log::info!(
                    "{output}: no write access to {}, using logind SetBrightness",
                    file.display()
                );
                WritePath::Logind(None)
            }
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("output '{output}': unable to open {}", file.display())
                })
            }
        };
        Ok(Self {
            name: name.into(),
            file,
            max,
            write,
        })
    }

    fn logind_set(&mut self, raw: u32) -> Result<()> {
        let WritePath::Logind(connection) = &mut self.write else {
            unreachable!()
        };
        if connection.is_none() {
            *connection = Some(zbus::blocking::Connection::system()?);
        }
        let connection = connection.as_ref().unwrap();
        let result = connection.call_method(
            Some("org.freedesktop.login1"),
            "/org/freedesktop/login1/session/auto",
            Some("org.freedesktop.login1.Session"),
            "SetBrightness",
            &("backlight", self.name.as_str(), raw),
        );
        if result.is_err() {
            // Reconnect next time in case the bus connection went stale.
            self.write = WritePath::Logind(None);
        }
        result?;
        Ok(())
    }
}

impl Device for Backlight {
    fn read(&mut self) -> Result<u32> {
        read_raw(&self.file)
    }

    fn write(&mut self, raw: u32) -> Result<()> {
        if let WritePath::Sysfs = self.write {
            match std::fs::write(&self.file, raw.to_string()) {
                Ok(()) => return Ok(()),
                Err(e) if e.kind() == ErrorKind::PermissionDenied => {
                    self.write = WritePath::Logind(None);
                }
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("unable to write {}", self.file.display()))
                }
            }
        }
        self.logind_set(raw).map_err(|e| {
            anyhow!(
                "no write access to {} and logind SetBrightness failed ({e:#}); \
                 add a udev rule or join the `video` group (see README)",
                self.file.display()
            )
        })
    }

    fn min(&self) -> u32 {
        BACKLIGHT_MIN.min(self.max)
    }

    fn max(&self) -> u32 {
        self.max
    }

    /// Reports the value after every inotify modification event. Events caused by our
    /// own writes are filtered by the controller, which remembers what it wrote.
    fn watch(&mut self, sink: async_channel::Sender<u32>) -> Result<()> {
        let mut inotify = Inotify::init().context("unable to initialize inotify")?;
        inotify
            .watches()
            .add(&self.file, WatchMask::MODIFY)
            .with_context(|| format!("unable to watch {}", self.file.display()))?;
        let file = self.file.clone();
        std::thread::Builder::new()
            .name(format!("backlight-{}", self.name))
            .spawn(move || {
                let mut buffer = [0u8; 1024];
                while inotify.read_events_blocking(&mut buffer).is_ok() {
                    let Ok(raw) = read_raw(&file) else { continue };
                    if sink.send_blocking(raw).is_err() {
                        break;
                    }
                }
            })?;
        Ok(())
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::time::Duration;

    pub fn fake_sysfs(test: &str, name: &str, value: u32, max: u32) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("woluma-backlight-{}-{test}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(name)).unwrap();
        std::fs::write(root.join(name).join("brightness"), format!("{value}\n")).unwrap();
        std::fs::write(root.join(name).join("max_brightness"), format!("{max}\n")).unwrap();
        root
    }

    #[test]
    fn missing_device_names_output_and_device() {
        let root = fake_sysfs("missing", "real", 1, 1);
        let error = Backlight::open_at(&root, "eDP-1", "does_not_exist")
            .err()
            .unwrap()
            .to_string();
        assert!(
            error.contains("eDP-1") && error.contains("does_not_exist"),
            "{error}"
        );
    }

    #[test]
    fn read_write_and_watch() {
        let root = fake_sysfs("rw", "amdgpu_bl1", 179, 255);
        let mut device = Backlight::open_at(&root, "eDP-1", "amdgpu_bl1").unwrap();
        assert_eq!(
            (device.min(), device.max(), device.read().unwrap()),
            (1, 255, 179)
        );
        device.write(102).unwrap();
        assert_eq!(device.read().unwrap(), 102);

        let (sink, seen) = async_channel::unbounded();
        device.watch(sink).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        std::fs::write(root.join("amdgpu_bl1/brightness"), "50").unwrap();
        let raw = smol::block_on(async {
            futures_lite::future::or(async { seen.recv().await.ok() }, async {
                smol::Timer::after(Duration::from_secs(2)).await;
                None
            })
            .await
        });
        assert_eq!(raw, Some(50));
    }
}
