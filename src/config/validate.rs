use super::{
    parse_duration, raw, Als, Backend, Config, ConfigErrors, General, Hysteresis, ManualOverride,
    Output, DEFAULT_ALS_FRAMES, DEFAULT_ALS_INTERVAL,
};
use crate::rules::{parse_num_cond, parse_time_range, NumCond, Op, Rule, When};
use std::collections::HashSet;
use std::time::Duration;

const ALS_SOURCES: &str = "\"webcam\", \"iio\", \"none\"";

struct Errors(Vec<String>);

impl Errors {
    fn push(&mut self, path: &str, message: impl std::fmt::Display) {
        self.0.push(format!("{path}: {message}"));
    }
}

pub fn validate(raw: raw::Config) -> Result<Config, ConfigErrors> {
    let mut errors = Errors(Vec::new());
    let general = general(raw.general, &mut errors);
    let hysteresis = hysteresis(raw.hysteresis, &mut errors);
    let als = als(raw.als, &mut errors);

    if raw.outputs.is_empty() {
        errors.push("outputs", "at least one [outputs.<name>] table is required");
    }
    let outputs = raw
        .outputs
        .into_iter()
        .filter_map(|(name, output)| self::output(name, output, &mut errors))
        .collect();

    if errors.0.is_empty() {
        Ok(Config {
            general,
            hysteresis,
            als,
            outputs,
        })
    } else {
        Err(ConfigErrors(errors.0))
    }
}

/// Non-fatal findings: logged at startup and printed by `woluma check`.
pub fn warnings(config: &Config) -> Vec<String> {
    let mut warnings = Vec::new();
    for output in &config.outputs {
        let rules = &output.rules;
        for (j, later) in rules.iter().enumerate() {
            if let Some(earlier) = rules[..j].iter().find(|r| covers(&r.when, &later.when)) {
                let why = if earlier.when.is_empty() {
                    "always matches"
                } else {
                    "matches whenever it would"
                };
                warnings.push(format!(
                    "outputs.{}: rule '{}' is unreachable: earlier rule '{}' {why}",
                    output.name, later.name, earlier.name
                ));
            }
        }
        if !rules.last().is_some_and(|r| r.when.is_empty()) {
            warnings.push(format!(
                "outputs.{}: no fallback rule (a last rule without `when`); brightness is left \
                 unchanged when nothing matches",
                output.name
            ));
        }
    }
    warnings
}

/// Whether `a` matches every input `b` matches. Conservative: per-input interval and
/// time-range containment only.
fn covers(a: &When, b: &When) -> bool {
    let num = |a: &Option<NumCond>, b: &Option<NumCond>| match (a, b) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(a), Some(b)) => match (interval(a), interval(b)) {
            (Some(a), Some(b)) => a.contains(&b),
            _ => a == b,
        },
    };
    let time = match (&a.time, &b.time) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(a), Some(b)) => (0..24 * 60)
            .filter_map(|m| chrono::NaiveTime::from_hms_opt(m / 60, m % 60, 0))
            .all(|t| !b.contains(t) || a.contains(t)),
    };
    num(&a.luma, &b.luma) && num(&a.als, &b.als) && time
}

/// A condition's matching set within 0-100; `None` for `!=`.
#[derive(Debug, Clone, Copy)]
struct Interval {
    lo: f64,
    lo_open: bool,
    hi: f64,
    hi_open: bool,
}

impl Interval {
    fn contains(&self, other: &Interval) -> bool {
        let lo = self.lo < other.lo || (self.lo == other.lo && (!self.lo_open || other.lo_open));
        let hi = self.hi > other.hi || (self.hi == other.hi && (!self.hi_open || other.hi_open));
        lo && hi
    }
}

fn interval(cond: &NumCond) -> Option<Interval> {
    let closed = |lo, hi| Interval {
        lo,
        lo_open: false,
        hi,
        hi_open: false,
    };
    Some(match *cond {
        NumCond::Range { lo, hi } => closed(lo, hi),
        NumCond::Cmp { op, value } => match op {
            Op::Lt => Interval {
                hi_open: true,
                ..closed(0.0, value)
            },
            Op::Le => closed(0.0, value),
            Op::Gt => Interval {
                lo_open: true,
                ..closed(value, 100.0)
            },
            Op::Ge => closed(value, 100.0),
            Op::Eq => closed(value, value),
            Op::Ne => return None,
        },
    })
}

fn duration(value: &toml::Value, path: &str, errors: &mut Errors) -> Option<Duration> {
    let parsed = value.as_str().and_then(parse_duration);
    if parsed.is_none() {
        errors.push(
            path,
            format!("invalid duration {value}, expected a string like \"400ms\", \"2s\", \"5m\" or \"1h\""),
        );
    }
    parsed
}

fn general(raw: raw::General, errors: &mut Errors) -> General {
    let mut general = General::default();
    if let Some(enabled) = raw.enabled {
        general.enabled = enabled;
    }
    if let Some(v) = raw
        .settle
        .and_then(|v| duration(&v, "general.settle", errors))
    {
        general.settle = v;
    }
    if let Some(v) = raw
        .transition
        .and_then(|v| duration(&v, "general.transition", errors))
    {
        general.transition = v;
    }
    if let Some(value) = raw.manual_override {
        if value.as_str() == Some("until-rule-change") {
            general.manual_override = ManualOverride::UntilRuleChange;
        } else if let Some(d) = value
            .as_str()
            .and_then(parse_duration)
            .filter(|d| !d.is_zero())
        {
            general.manual_override = ManualOverride::For(d);
        } else {
            errors.push(
                "general.manual_override",
                format!("invalid value {value}, expected \"until-rule-change\" or a positive duration like \"10m\""),
            );
        }
    }
    general
}

fn hysteresis(raw: raw::Hysteresis, errors: &mut Errors) -> Hysteresis {
    let mut hysteresis = Hysteresis::default();
    for (key, value, slot) in [
        ("luma", raw.luma, &mut hysteresis.luma),
        ("als", raw.als, &mut hysteresis.als),
    ] {
        if let Some(value) = value {
            if (0.0..=100.0).contains(&value) {
                *slot = value;
            } else {
                errors.push(
                    &format!("hysteresis.{key}"),
                    format!("{value} is out of range 0-100"),
                );
            }
        }
    }
    hysteresis
}

fn als(raw: Option<raw::Als>, errors: &mut Errors) -> Als {
    let Some(raw) = raw else { return Als::None };
    let reject = |errors: &mut Errors, key: &str, set: bool, source: &str| {
        if set {
            errors.push(
                &format!("als.{key}"),
                format!("not valid for source \"{source}\""),
            );
        }
    };
    let interval = raw
        .interval
        .as_ref()
        .and_then(|v| match duration(v, "als.interval", errors) {
            Some(d) if d.is_zero() => {
                errors.push("als.interval", "must be greater than zero");
                None
            }
            d => d,
        })
        .unwrap_or(DEFAULT_ALS_INTERVAL);
    match raw.source.as_str() {
        "none" => {
            for (key, set) in [
                ("device", raw.device.is_some()),
                ("interval", raw.interval.is_some()),
                ("frames", raw.frames.is_some()),
                ("exposure", raw.exposure.is_some()),
            ] {
                reject(errors, key, set, "none");
            }
            Als::None
        }
        "webcam" => {
            let frames = match raw.frames {
                None => DEFAULT_ALS_FRAMES,
                Some(f @ 1..=60) => f as u32,
                Some(f) => {
                    errors.push("als.frames", format!("{f} is out of range 1-60"));
                    DEFAULT_ALS_FRAMES
                }
            };
            let exposure = match raw.exposure {
                Some(e) if e > 0 && e <= u32::MAX as i64 => e as u32,
                Some(e) => {
                    errors.push("als.exposure", format!("{e} must be a positive integer"));
                    0
                }
                None => {
                    errors.push(
                        "als.exposure",
                        "required for source \"webcam\" (exposure_time_absolute while sampling, e.g. 600)",
                    );
                    0
                }
            };
            Als::Webcam {
                device: raw.device.unwrap_or_else(|| "/dev/video0".into()),
                interval,
                frames,
                exposure,
            }
        }
        "iio" => {
            reject(errors, "frames", raw.frames.is_some(), "iio");
            reject(errors, "exposure", raw.exposure.is_some(), "iio");
            Als::Iio {
                device: raw.device,
                interval,
            }
        }
        other => {
            errors.push(
                "als.source",
                format!("unknown source \"{other}\", valid sources are {ALS_SOURCES}"),
            );
            Als::None
        }
    }
}

fn output(name: String, raw: raw::Output, errors: &mut Errors) -> Option<Output> {
    let path = format!("outputs.{name}");
    let backend = match (raw.backlight, raw.ddcutil) {
        (Some(_), Some(_)) => {
            errors.push(
                &path,
                format!("output '{name}' declares both `backlight` and `ddcutil`, choose one"),
            );
            None
        }
        (None, None) => {
            errors.push(
                &path,
                format!("output '{name}' must declare `backlight` or `ddcutil`"),
            );
            None
        }
        (Some(device), None) => {
            if raw.poll.is_some() {
                errors.push(&format!("{path}.poll"), "only valid for ddcutil outputs");
            }
            Some(Backend::Backlight(device))
        }
        (None, Some(display)) => {
            // `poll = "0"` turns external change detection off.
            let poll = raw.poll.and_then(|p| match p.as_str() {
                Some("0") => Some(Duration::ZERO),
                _ => duration(&p, &format!("{path}.poll"), errors),
            });
            Some(Backend::Ddcutil { display, poll })
        }
    };

    if raw.rules.is_empty() {
        errors.push(
            &format!("{path}.rules"),
            format!("output '{name}' needs at least one rule"),
        );
    }
    let mut seen = HashSet::new();
    let rules: Vec<_> = raw
        .rules
        .into_iter()
        .enumerate()
        .filter_map(|(i, rule)| {
            let rule_path = format!("{path}.rules[{i}]");
            if !seen.insert(rule.name.clone()) {
                errors.push(
                    &format!("{rule_path}.name"),
                    format!("duplicate rule name '{}' on output '{name}'", rule.name),
                );
            }
            self::rule(rule, &rule_path, errors)
        })
        .collect();

    Some(Output {
        name,
        backend: backend?,
        rules,
    })
}

fn rule(raw: raw::Rule, path: &str, errors: &mut Errors) -> Option<Rule> {
    let name = raw.name;
    let before = errors.0.len();

    let brightness = raw.brightness.and_then(|value| match value.as_integer() {
        Some(b @ 0..=100) => Some(b as u8),
        Some(b) => {
            errors.push(
                &format!("{path}.brightness"),
                format!("rule '{name}': brightness {b} is out of range 0-100"),
            );
            None
        }
        None => {
            errors.push(
                &format!("{path}.brightness"),
                format!("rule '{name}': brightness must be an absolute integer 0-100, got {value}"),
            );
            None
        }
    });
    let run = raw.run.unwrap_or_default();
    if brightness.is_none() && run.is_empty() && errors.0.len() == before {
        errors.push(
            path,
            format!("rule '{name}' must set `brightness` or `run`"),
        );
    }

    let mut when = When::default();
    for (key, value) in raw.when {
        let key_path = format!("{path}.when.{key}");
        match key.as_str() {
            "luma" | "als" => {
                let parsed = match value.as_str() {
                    Some(text) => parse_num_cond(text),
                    None => Err(format!("expected a string with an explicit operator (e.g. \">= {value}\"), got {value}")),
                };
                match parsed {
                    Ok(cond) if key == "luma" => when.luma = Some(cond),
                    Ok(cond) => when.als = Some(cond),
                    Err(e) => errors.push(&key_path, format!("rule '{name}': {e}")),
                }
            }
            "time" => match value.as_str().ok_or_else(|| format!("expected a string \"HH:MM-HH:MM\", got {value}")).and_then(parse_time_range) {
                Ok(range) => when.time = Some(range),
                Err(e) => errors.push(&key_path, format!("rule '{name}': {e}")),
            },
            other => errors.push(
                &key_path,
                format!("rule '{name}': unknown condition `{other}`, valid keys are `luma`, `als`, `time`"),
            ),
        }
    }

    (errors.0.len() == before).then_some(Rule {
        name,
        when,
        brightness,
        run,
    })
}
