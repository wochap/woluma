//! Control socket: JSON-lines protocol types, the state hub and the daemon-side server.

mod hub;
pub mod server;

pub use hub::Hub;

use serde::{Deserialize, Serialize};

pub const SOCKET_NAME: &str = "woluma.sock";
pub const LOCK_NAME: &str = "woluma.lock";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "lowercase", deny_unknown_fields)]
pub enum Request {
    Status,
    Subscribe,
    Toggle {
        #[serde(default = "all")]
        target: String,
    },
    Pause {
        target: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_secs: Option<u64>,
    },
    Resume {
        target: String,
    },
}

fn all() -> String {
    "*".into()
}

const COMMANDS: [&str; 5] = ["status", "subscribe", "toggle", "pause", "resume"];

impl Request {
    /// Parses one request line, with messages suitable for an `ok: false` response.
    pub fn parse(line: &str) -> Result<Self, String> {
        let value: serde_json::Value =
            serde_json::from_str(line).map_err(|e| format!("malformed request: {e}"))?;
        let cmd = value
            .get("cmd")
            .and_then(|c| c.as_str())
            .ok_or("malformed request: missing \"cmd\"")?
            .to_string();
        if !COMMANDS.contains(&cmd.as_str()) {
            return Err(format!("unknown command: {cmd}"));
        }
        serde_json::from_value(value).map_err(|e| format!("invalid {cmd} request: {e}"))
    }
}

/// `ok: true` with a result, or `ok: false` with an error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<Snapshot>,
    /// Resulting state of the outputs a toggle/pause/resume acted on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outputs: Option<Vec<OutputSnapshot>>,
}

impl Response {
    pub fn status(snapshot: Snapshot) -> Self {
        Self {
            ok: true,
            error: None,
            status: Some(snapshot),
            outputs: None,
        }
    }

    pub fn outputs(outputs: Vec<OutputSnapshot>) -> Self {
        Self {
            ok: true,
            error: None,
            status: None,
            outputs: Some(outputs),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(message.into()),
            status: None,
            outputs: None,
        }
    }
}

/// An output name or `*` for every configured output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    All,
    Output(String),
}

impl Target {
    pub fn parse(text: &str) -> Self {
        match text {
            "*" => Self::All,
            name => Self::Output(name.to_string()),
        }
    }

    /// Indices of the targeted outputs, or an error listing the valid names.
    pub fn resolve(&self, names: &[String]) -> Result<Vec<usize>, String> {
        match self {
            Self::All => Ok((0..names.len()).collect()),
            Self::Output(name) => names
                .iter()
                .position(|n| n == name)
                .map(|i| vec![i])
                .ok_or_else(|| format!("unknown output {name} (configured: {})", names.join(", "))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: String,
    /// Local time, RFC 3339.
    pub time: String,
    pub als: AlsSnapshot,
    pub outputs: Vec<OutputSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlsSnapshot {
    /// `webcam`, `iio` or `none`.
    pub source: String,
    pub value: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Active,
    Settling,
    Paused,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PauseReason {
    /// Paused on request (`woluma pause`).
    Manual,
    /// Paused because the brightness was changed by hand.
    ManualOverride,
}

/// `until` value for a pause that ends at the next rule change.
pub const UNTIL_RULE_CHANGE: &str = "rule-change";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputSnapshot {
    pub name: String,
    pub enabled: bool,
    pub luma: Option<u8>,
    pub rule: Option<String>,
    /// Current brightness percent, `null` when unknown.
    pub brightness: Option<u8>,
    pub state: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_reason: Option<PauseReason>,
    /// RFC 3339 expiry, or `"rule-change"`; absent for a pause that lasts until resumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle_remaining_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug>(
        value: &T,
        json: &str,
    ) {
        assert_eq!(serde_json::to_string(value).unwrap(), json);
        assert_eq!(&serde_json::from_str::<T>(json).unwrap(), value);
    }

    #[test]
    fn requests_round_trip() {
        round_trip(&Request::Status, r#"{"cmd":"status"}"#);
        round_trip(&Request::Subscribe, r#"{"cmd":"subscribe"}"#);
        round_trip(
            &Request::Toggle { target: "*".into() },
            r#"{"cmd":"toggle","target":"*"}"#,
        );
        round_trip(
            &Request::Pause {
                target: "eDP-1".into(),
                duration_secs: Some(900),
            },
            r#"{"cmd":"pause","target":"eDP-1","duration_secs":900}"#,
        );
        round_trip(
            &Request::Pause {
                target: "*".into(),
                duration_secs: None,
            },
            r#"{"cmd":"pause","target":"*"}"#,
        );
        round_trip(
            &Request::Resume {
                target: "DP-1".into(),
            },
            r#"{"cmd":"resume","target":"DP-1"}"#,
        );
        assert_eq!(
            Request::parse(r#"{"cmd":"toggle"}"#),
            Ok(Request::Toggle { target: "*".into() })
        );
    }

    #[test]
    fn bad_requests_are_explained() {
        assert_eq!(
            Request::parse(r#"{"cmd":"explode"}"#),
            Err("unknown command: explode".into())
        );
        assert!(Request::parse("nope").unwrap_err().starts_with("malformed"));
        assert!(Request::parse(r#"{"x":1}"#).unwrap_err().contains("cmd"));
        assert!(Request::parse(r#"{"cmd":"pause"}"#)
            .unwrap_err()
            .starts_with("invalid pause request"));
    }

    fn snapshot() -> Snapshot {
        Snapshot {
            version: "0.1.0".into(),
            time: "2026-10-12T14:05:02+02:00".into(),
            als: AlsSnapshot {
                source: "webcam".into(),
                value: None,
            },
            outputs: vec![
                OutputSnapshot {
                    name: "eDP-1".into(),
                    enabled: true,
                    luma: None,
                    rule: None,
                    brightness: None,
                    state: State::Settling,
                    pause_reason: None,
                    until: None,
                    settle_remaining_ms: Some(1200),
                },
                OutputSnapshot {
                    name: "DP-1".into(),
                    enabled: true,
                    luma: Some(88),
                    rule: Some("bright".into()),
                    brightness: Some(40),
                    state: State::Paused,
                    pause_reason: Some(PauseReason::ManualOverride),
                    until: Some(UNTIL_RULE_CHANGE.into()),
                    settle_remaining_ms: None,
                },
            ],
        }
    }

    #[test]
    fn responses_and_snapshots_round_trip() {
        round_trip(
            &Response::status(snapshot()),
            concat!(
                r#"{"ok":true,"status":{"version":"0.1.0","time":"2026-10-12T14:05:02+02:00","#,
                r#""als":{"source":"webcam","value":null},"outputs":["#,
                r#"{"name":"eDP-1","enabled":true,"luma":null,"rule":null,"brightness":null,"#,
                r#""state":"settling","settle_remaining_ms":1200},"#,
                r#"{"name":"DP-1","enabled":true,"luma":88,"rule":"bright","brightness":40,"#,
                r#""state":"paused","pause_reason":"manual-override","until":"rule-change"}]}}"#
            ),
        );
        round_trip(
            &Response::error("unknown command: explode"),
            r#"{"ok":false,"error":"unknown command: explode"}"#,
        );
        let mut disabled = snapshot().outputs[0].clone();
        disabled.state = State::Disabled;
        disabled.enabled = false;
        disabled.settle_remaining_ms = None;
        round_trip(
            &Response::outputs(vec![disabled]),
            concat!(
                r#"{"ok":true,"outputs":[{"name":"eDP-1","enabled":false,"luma":null,"#,
                r#""rule":null,"brightness":null,"state":"disabled"}]}"#
            ),
        );
    }

    #[test]
    fn targets_resolve_against_configured_outputs() {
        let names = vec!["eDP-1".to_string(), "DP-1".to_string()];
        assert_eq!(Target::parse("*").resolve(&names), Ok(vec![0, 1]));
        assert_eq!(Target::parse("DP-1").resolve(&names), Ok(vec![1]));
        assert_eq!(
            Target::parse("HDMI-9").resolve(&names),
            Err("unknown output HDMI-9 (configured: eDP-1, DP-1)".into())
        );
    }
}
