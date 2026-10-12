//! Config file location, raw TOML parsing and validation into a typed model.

mod duration;
mod raw;
mod validate;

pub use duration::parse_duration;

use crate::rules::Rule;
use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Config {
    pub general: General,
    pub hysteresis: Hysteresis,
    pub als: Als,
    pub outputs: Vec<Output>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct General {
    pub enabled: bool,
    pub settle: Duration,
    pub transition: Duration,
    pub manual_override: ManualOverride,
}

impl Default for General {
    fn default() -> Self {
        Self {
            enabled: true,
            settle: Duration::from_secs(2),
            transition: Duration::from_millis(400),
            manual_override: ManualOverride::UntilRuleChange,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ManualOverride {
    UntilRuleChange,
    For(Duration),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hysteresis {
    pub luma: f64,
    pub als: f64,
}

impl Default for Hysteresis {
    fn default() -> Self {
        Self {
            luma: 5.0,
            als: 3.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Als {
    None,
    Webcam { device: String },
    Iio { path: Option<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Backend {
    Backlight(String),
    Ddcutil {
        display: String,
        poll: Option<Duration>,
    },
}

#[derive(Debug, Clone)]
pub struct Output {
    pub name: String,
    pub backend: Backend,
    pub rules: Vec<Rule>,
}

/// Validation failure carrying every error found, each prefixed with its TOML path.
#[derive(Debug, thiserror::Error)]
#[error("invalid config:\n{}", .0.iter().map(|e| format!("  - {e}")).collect::<Vec<_>>().join("\n"))]
pub struct ConfigErrors(pub Vec<String>);

/// Resolves the config path: `--config` > `$XDG_CONFIG_HOME` > `$HOME/.config`.
pub fn resolve_path(
    explicit: Option<&Path>,
    xdg_config_home: Option<&str>,
    home: Option<&str>,
) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }
    let base = match (xdg_config_home.filter(|s| !s.is_empty()), home) {
        (Some(xdg), _) => PathBuf::from(xdg),
        (None, Some(home)) => Path::new(home).join(".config"),
        (None, None) => return Err(anyhow!("neither XDG_CONFIG_HOME nor HOME is set")),
    };
    Ok(base.join("woluma").join("config.toml"))
}

pub fn load(explicit: Option<&Path>) -> Result<Config> {
    let path = resolve_path(
        explicit,
        std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )?;
    if !path.exists() {
        return Err(anyhow!("config file not found: {}", path.display()));
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("unable to read config file {}", path.display()))?;
    let config =
        parse(&text).with_context(|| format!("unable to load config file {}", path.display()))?;
    for warning in validate::warnings(&config) {
        log::warn!("{warning}");
    }
    Ok(config)
}

pub fn parse(text: &str) -> Result<Config, ConfigErrors> {
    let raw: raw::Config = toml::from_str(text).map_err(|e| ConfigErrors(vec![e.to_string()]))?;
    validate::validate(raw)
}

#[cfg(test)]
mod tests;
