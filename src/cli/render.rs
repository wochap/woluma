//! Human, raw and JSON renderings of snapshots.

use crate::control::{OutputSnapshot, PauseReason, Snapshot, State, UNTIL_RULE_CHANGE};
use chrono::{DateTime, Local};
use std::io::{ErrorKind, Write};

/// Writes one line; a closed pipe (`| head`) ends the process quietly.
pub fn emit(out: &mut impl Write, line: &str) {
    if let Err(e) = writeln!(out, "{line}").and_then(|()| out.flush()) {
        if e.kind() == ErrorKind::BrokenPipe {
            std::process::exit(0);
        }
        eprintln!("woluma: unable to write output: {e}");
        std::process::exit(1);
    }
}

fn local(rfc3339: &str) -> Option<DateTime<Local>> {
    DateTime::parse_from_rfc3339(rfc3339)
        .ok()
        .map(|t| t.with_timezone(&Local))
}

fn time(rfc3339: &str, format: &str) -> String {
    local(rfc3339).map_or_else(|| "--".into(), |t| t.format(format).to_string())
}

fn opt<T: ToString>(value: Option<T>) -> String {
    value.map_or_else(|| "--".into(), |v| v.to_string())
}

fn reason(reason: Option<PauseReason>) -> &'static str {
    match reason {
        Some(PauseReason::ManualOverride) => "manual-override",
        _ => "manual",
    }
}

/// The STATE column: `active`, `settling (1.2s)`, `paused until-rule-change (manual)`,
/// `paused until 14:59 (manual)`, `paused (manual)` or `disabled`.
pub fn state(output: &OutputSnapshot) -> String {
    match output.state {
        State::Active => "active".into(),
        State::Disabled => "disabled".into(),
        State::Settling => match output.settle_remaining_ms {
            Some(ms) => format!("settling ({:.1}s)", ms as f64 / 1000.0),
            None => "settling".into(),
        },
        State::Paused => {
            let reason = reason(output.pause_reason);
            match output.until.as_deref() {
                Some(UNTIL_RULE_CHANGE) => format!("paused until-rule-change ({reason})"),
                Some(at) => format!("paused until {} ({reason})", time(at, "%H:%M")),
                None => format!("paused ({reason})"),
            }
        }
    }
}

/// Fixed-width columns: every column but the last is padded to its width.
fn row(columns: &[(&str, usize)]) -> String {
    let mut line = String::new();
    for (i, (text, width)) in columns.iter().enumerate() {
        if i + 1 == columns.len() {
            line += text;
        } else {
            line += &format!("{text:<width$}  ");
        }
    }
    line
}

const OUTPUT: usize = 10;
const NUM: usize = 4;
const RULE: usize = 12;
const BRIGHTNESS: usize = 10;
const TIME: usize = 8;

pub fn status_table(snapshot: &Snapshot) -> Vec<String> {
    let als = opt(snapshot.als.value);
    let mut lines = vec![row(&[
        ("OUTPUT", OUTPUT),
        ("LUMA", NUM),
        ("ALS", NUM),
        ("RULE", RULE),
        ("BRIGHTNESS", BRIGHTNESS),
        ("STATE", 0),
    ])];
    for o in &snapshot.outputs {
        lines.push(row(&[
            (&o.name, OUTPUT),
            (&opt(o.luma), NUM),
            (&als, NUM),
            (o.rule.as_deref().unwrap_or("--"), RULE),
            (&opt(o.brightness), BRIGHTNESS),
            (&state(o), 0),
        ]));
    }
    lines
}

/// Output format of `watch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Human,
    Raw,
    Json,
}

pub fn watch_header(format: Format) -> Option<String> {
    match format {
        Format::Human => Some(row(&[
            ("TIME", TIME),
            ("OUTPUT", OUTPUT),
            ("LUMA", NUM),
            ("ALS", NUM),
            ("RULE", RULE),
            ("BRIGHTNESS", BRIGHTNESS),
            ("STATE", 0),
        ])),
        Format::Raw => Some(row(&[
            ("TIME", TIME),
            ("OUTPUT", OUTPUT),
            ("LUMA", NUM),
            ("ALS", NUM),
            ("CLOCK", 0),
        ])),
        Format::Json => None,
    }
}

/// One line per output (human/raw), or the whole snapshot on one line (JSON).
pub fn watch_lines(snapshot: &Snapshot, format: Format) -> Vec<String> {
    if format == Format::Json {
        return vec![serde_json::to_string(snapshot).unwrap()];
    }
    let at = time(&snapshot.time, "%H:%M:%S");
    let als = opt(snapshot.als.value);
    snapshot
        .outputs
        .iter()
        .map(|o| match format {
            Format::Raw => row(&[
                (&at, TIME),
                (&o.name, OUTPUT),
                (&opt(o.luma), NUM),
                (&als, NUM),
                (&time(&snapshot.time, "%H:%M"), 0),
            ]),
            _ => row(&[
                (&at, TIME),
                (&o.name, OUTPUT),
                (&opt(o.luma), NUM),
                (&als, NUM),
                (o.rule.as_deref().unwrap_or("--"), RULE),
                (&opt(o.brightness), BRIGHTNESS),
                (&state(o), 0),
            ]),
        })
        .collect()
}

/// Result line for toggle/pause/resume, e.g. `eDP-1: disabled`.
pub fn action_line(output: &OutputSnapshot) -> String {
    format!("{}: {}", output.name, state(output))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::AlsSnapshot;

    fn output(state: State) -> OutputSnapshot {
        OutputSnapshot {
            name: "eDP-1".into(),
            enabled: state != State::Disabled,
            luma: Some(88),
            rule: Some("bright".into()),
            brightness: Some(40),
            state,
            pause_reason: None,
            until: None,
            settle_remaining_ms: None,
        }
    }

    fn snapshot(outputs: Vec<OutputSnapshot>) -> Snapshot {
        Snapshot {
            version: "0.1.0".into(),
            time: Local::now()
                .date_naive()
                .and_hms_opt(14, 5, 2)
                .unwrap()
                .and_local_timezone(Local)
                .unwrap()
                .to_rfc3339(),
            als: AlsSnapshot {
                source: "none".into(),
                value: None,
            },
            outputs,
        }
    }

    #[test]
    fn state_strings() {
        assert_eq!(state(&output(State::Active)), "active");
        assert_eq!(state(&output(State::Disabled)), "disabled");
        let mut o = output(State::Settling);
        o.settle_remaining_ms = Some(1200);
        assert_eq!(state(&o), "settling (1.2s)");
        let mut o = output(State::Paused);
        o.pause_reason = Some(PauseReason::Manual);
        o.until = Some(UNTIL_RULE_CHANGE.into());
        assert_eq!(state(&o), "paused until-rule-change (manual)");
        o.until = Some(snapshot(vec![]).time.replace("14:05:02", "14:59:00"));
        assert_eq!(state(&o), "paused until 14:59 (manual)");
        o.until = None;
        o.pause_reason = Some(PauseReason::ManualOverride);
        assert_eq!(state(&o), "paused (manual-override)");
        assert_eq!(action_line(&output(State::Disabled)), "eDP-1: disabled");
    }

    #[test]
    fn human_lines_and_unknowns() {
        let mut unknown = output(State::Active);
        unknown.luma = None;
        unknown.rule = None;
        unknown.brightness = None;
        let s = snapshot(vec![output(State::Active), unknown]);
        assert_eq!(
            watch_header(Format::Human).unwrap(),
            "TIME      OUTPUT      LUMA  ALS   RULE          BRIGHTNESS  STATE"
        );
        assert_eq!(
            watch_lines(&s, Format::Human),
            vec![
                "14:05:02  eDP-1       88    --    bright        40          active",
                "14:05:02  eDP-1       --    --    --            --          active",
            ]
        );
        assert_eq!(
            status_table(&s)[1],
            "eDP-1       88    --    bright        40          active"
        );
    }

    #[test]
    fn raw_and_json() {
        let s = snapshot(vec![output(State::Active)]);
        assert_eq!(
            watch_header(Format::Raw).unwrap(),
            "TIME      OUTPUT      LUMA  ALS   CLOCK"
        );
        assert_eq!(
            watch_lines(&s, Format::Raw),
            vec!["14:05:02  eDP-1       88    --    14:05"]
        );
        assert!(watch_header(Format::Json).is_none());
        let json = watch_lines(&s, Format::Json);
        assert_eq!(json.len(), 1);
        assert_eq!(serde_json::from_str::<Snapshot>(&json[0]).unwrap(), s);
    }
}
