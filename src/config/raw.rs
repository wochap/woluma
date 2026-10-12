//! Raw serde structs mirroring the TOML file. Values that need specific error messages
//! (durations, conditions, brightness) are kept loosely typed and checked in `validate`.

use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub general: General,
    #[serde(default)]
    pub hysteresis: Hysteresis,
    pub als: Option<Als>,
    #[serde(default)]
    pub outputs: BTreeMap<String, Output>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct General {
    pub enabled: Option<bool>,
    pub settle: Option<toml::Value>,
    pub transition: Option<toml::Value>,
    pub manual_override: Option<toml::Value>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Hysteresis {
    pub luma: Option<f64>,
    pub als: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Als {
    pub source: String,
    /// webcam: video device path.
    pub device: Option<String>,
    /// iio: sysfs device path.
    pub path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Output {
    pub backlight: Option<String>,
    pub ddcutil: Option<String>,
    pub poll: Option<toml::Value>,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub name: String,
    #[serde(default)]
    pub when: toml::Table,
    pub brightness: Option<toml::Value>,
    pub run: Option<Vec<String>>,
}
