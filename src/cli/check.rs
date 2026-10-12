//! `woluma check`: config validation, rule-structure warnings and the current match.

use super::Failure;
use crate::capture;
use crate::config::{self, Config};
use crate::rules::{Engine, Inputs};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const SAMPLE_TIMEOUT: Duration = Duration::from_secs(5);

/// Reads and validates the config, with every error listed.
fn load(config_path: Option<&Path>) -> Result<Config, Failure> {
    let path = config::resolve_path(
        config_path,
        std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
    .map_err(|e| Failure::Runtime(format!("{e:#}")))?;
    let text = std::fs::read_to_string(&path)
        .map_err(|e| Failure::Runtime(format!("unable to read {}: {e}", path.display())))?;
    config::parse(&text).map_err(|e| Failure::Runtime(format!("{}: {e}", path.display())))
}

/// Problems that don't need a display: errors first, then warnings.
pub fn findings(config_path: Option<&Path>) -> Result<(Config, Vec<String>), Failure> {
    let config = load(config_path)?;
    let warnings = config::warnings(&config);
    Ok((config, warnings))
}

/// Configured outputs missing from `connected`.
fn disconnected(config: &Config, connected: &[String]) -> Vec<String> {
    config
        .outputs
        .iter()
        .filter(|o| !connected.contains(&o.name))
        .map(|o| format!("outputs.{}: output is not currently connected", o.name))
        .collect()
}

/// `eDP-1: matching now → bright (brightness 40)`
fn current_match(config: &Config, output: usize, luma: u8, als: Option<u8>) -> String {
    let output = &config.outputs[output];
    let engine = Engine::new(
        output.name.clone(),
        Arc::new(output.rules.clone()),
        config.general.settle,
        config.hysteresis,
    );
    let inputs = Inputs {
        luma,
        als,
        now: chrono::Local::now().time(),
    };
    match engine.evaluate(&inputs).map(|i| &output.rules[i]) {
        Some(rule) => format!(
            "{}: matching now → {} (brightness {})",
            output.name,
            rule.name,
            rule.brightness
                .map_or_else(|| "unchanged".into(), |b| b.to_string())
        ),
        None => format!(
            "{}: matching now → no rule (luma {luma}); brightness unchanged",
            output.name
        ),
    }
}

pub fn run(config_path: Option<&Path>, offline: bool) -> Result<(), Failure> {
    let (config, mut warnings) = findings(config_path)?;
    let connected = if offline {
        None
    } else {
        capture::outputs::list().ok()
    };
    if let Some(connected) = &connected {
        warnings.extend(disconnected(&config, connected));
    }
    println!("config OK");
    for warning in &warnings {
        println!("warning: {warning}");
    }
    let Some(connected) = connected else {
        return Ok(());
    };
    if capture::check_protocols().is_err() {
        return Ok(());
    }
    // ALS is not sampled here: rules using it see it as unknown.
    for (index, output) in config.outputs.iter().enumerate() {
        if !connected.contains(&output.name) {
            continue;
        }
        if let Some(luma) = capture::sample_once(&output.name, SAMPLE_TIMEOUT) {
            println!("{}", current_match(&config, index, luma, None));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str, text: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("woluma-check-{}-{name}.toml", std::process::id()));
        std::fs::write(&path, text).unwrap();
        path
    }

    const VALID: &str = r#"
[outputs.eDP-1]
backlight = "intel_backlight"

[[outputs.eDP-1.rules]]
name = "bright"
when = { luma = ">= 70" }
brightness = 40

[[outputs.eDP-1.rules]]
name = "default"
brightness = 70
"#;

    fn error(text: &str) -> String {
        match findings(Some(&fixture("error", text))) {
            Err(Failure::Runtime(message)) => message,
            _ => panic!("expected an error"),
        }
    }

    #[test]
    fn missing_operator_is_located() {
        let message = error(&VALID.replace(">= 70", "70"));
        assert!(
            message.contains("outputs.eDP-1.rules[0].when.luma"),
            "{message}"
        );
        assert!(message.contains("rule 'bright'"), "{message}");
        assert!(
            message.contains("explicit operator is required"),
            "{message}"
        );
    }

    #[test]
    fn all_errors_listed() {
        let message = error(
            &VALID
                .replace(">= 70", "70")
                .replace("brightness = 70", "brightness = 170"),
        );
        assert!(message.contains("rules[0].when.luma") && message.contains("rules[1].brightness"));
    }

    #[test]
    fn missing_file_is_an_error() {
        let missing = std::env::temp_dir().join("woluma-check-does-not-exist.toml");
        assert!(matches!(findings(Some(&missing)), Err(Failure::Runtime(_))));
    }

    #[test]
    fn valid_config_has_no_warnings() {
        let (_, warnings) = findings(Some(&fixture("valid", VALID))).unwrap();
        assert!(warnings.is_empty(), "{warnings:#?}");
    }

    #[test]
    fn missing_fallback_and_unreachable() {
        let (_, warnings) = findings(Some(&fixture(
            "warn",
            &VALID.replace(
                "name = \"default\"",
                "name = \"default\"\nwhen = { luma = \">= 80\" }",
            ),
        )))
        .unwrap();
        assert_eq!(warnings.len(), 2, "{warnings:#?}");
        assert!(warnings[0].contains("'default' is unreachable"));
        assert!(warnings[1].contains("eDP-1: no fallback rule"));
    }

    #[test]
    fn not_connected_and_current_match() {
        let (config, _) = findings(Some(&fixture("match", VALID))).unwrap();
        assert!(disconnected(&config, &["eDP-1".into()]).is_empty());
        assert_eq!(
            disconnected(&config, &["DP-1".into()]),
            vec!["outputs.eDP-1: output is not currently connected"]
        );
        assert_eq!(
            current_match(&config, 0, 88, None),
            "eDP-1: matching now → bright (brightness 40)"
        );
        assert_eq!(
            current_match(&config, 0, 20, None),
            "eDP-1: matching now → default (brightness 70)"
        );
    }
}
