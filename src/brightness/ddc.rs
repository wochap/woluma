//! DDC/CI monitors over `/dev/i2c-*`, VCP feature 0x10 (luminance). Displays are matched
//! by the EDID product name (case-insensitive substring) or serial number. All calls block,
//! so the controller runs them on the blocking thread pool.

use super::{mapping::DDC_MIN, Device};
use anyhow::{anyhow, Context, Result};
use ddc::{Ddc as _, Edid as _};
use ddc_i2c::I2cDeviceDdc;
use std::path::PathBuf;

const LUMINANCE: u8 = 0x10;

pub struct Ddc {
    handle: I2cDeviceDdc,
    max: u32,
}

/// True when `identifier` names this display.
pub fn matches(identifier: &str, model: Option<&str>, serial: Option<&str>) -> bool {
    let wanted = identifier.trim().to_lowercase();
    !wanted.is_empty()
        && (model.is_some_and(|m| m.to_lowercase().contains(&wanted))
            || serial.is_some_and(|s| s.trim().eq_ignore_ascii_case(identifier.trim())))
}

fn i2c_devices() -> Vec<PathBuf> {
    let mut devices: Vec<(u32, PathBuf)> = std::fs::read_dir("/dev")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            Some((name.strip_prefix("i2c-")?.parse().ok()?, e.path()))
        })
        .collect();
    devices.sort();
    devices.into_iter().map(|(_, path)| path).collect()
}

fn identify(handle: &mut I2cDeviceDdc) -> Option<(Option<String>, Option<String>)> {
    let mut data = [0u8; 128];
    handle.read_edid(0, &mut data).ok()?;
    let edid = edid::parse(&data).to_result().ok()?;
    let mut model = None;
    let mut serial = None;
    for descriptor in edid.descriptors {
        match descriptor {
            edid::Descriptor::ProductName(name) => model = Some(name.trim().to_string()),
            edid::Descriptor::SerialNumber(s) => serial = Some(s.trim().to_string()),
            _ => {}
        }
    }
    if serial.is_none() && edid.header.serial != 0 {
        serial = Some(edid.header.serial.to_string());
    }
    Some((model, serial))
}

impl Ddc {
    pub fn find(identifier: &str) -> Result<Self> {
        let devices = i2c_devices();
        if devices.is_empty() {
            return Err(anyhow!(
                "no /dev/i2c-* devices (is the i2c-dev module loaded?)"
            ));
        }
        for path in devices {
            let Ok(mut handle) = ddc_i2c::from_i2c_device(&path) else {
                continue;
            };
            let Some((model, serial)) = identify(&mut handle) else {
                continue;
            };
            if !matches(identifier, model.as_deref(), serial.as_deref()) {
                continue;
            }
            let value = handle
                .get_vcp_feature(LUMINANCE)
                .map_err(|e| anyhow!("{e}"))
                .with_context(|| format!("unable to read luminance from {}", path.display()))?;
            log::info!(
                "DDC display '{}' found at {}",
                model.as_deref().unwrap_or(identifier),
                path.display()
            );
            return Ok(Self {
                handle,
                max: u32::from(value.maximum()),
            });
        }
        Err(anyhow!("no DDC display matches '{identifier}'"))
    }
}

impl Device for Ddc {
    fn read(&mut self) -> Result<u32> {
        let value = self
            .handle
            .get_vcp_feature(LUMINANCE)
            .map_err(|e| anyhow!("DDC read failed: {e}"))?;
        Ok(u32::from(value.value()))
    }

    fn write(&mut self, raw: u32) -> Result<()> {
        self.handle
            .set_vcp_feature(LUMINANCE, raw.min(u32::from(u16::MAX)) as u16)
            .map_err(|e| anyhow!("DDC write failed: {e}"))
    }

    fn min(&self) -> u32 {
        DDC_MIN
    }

    fn max(&self) -> u32 {
        self.max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_matching() {
        assert!(matches("DELL U2720Q", Some("DELL U2720Q"), None));
        assert!(matches("u2720", Some("DELL U2720Q"), None));
        assert!(matches("ABC123", Some("DELL U2720Q"), Some("abc123")));
        assert!(!matches("ABC", Some("DELL U2720Q"), Some("ABC123")));
        assert!(!matches("LG", Some("DELL U2720Q"), None));
        assert!(!matches("", Some("DELL"), None));
    }
}
