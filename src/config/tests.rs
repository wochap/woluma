use super::*;

const MINIMAL: &str = r#"
[outputs.eDP-1]
backlight = "intel_backlight"

[[outputs.eDP-1.rules]]
name = "default"
brightness = 50
"#;

fn errors(text: &str) -> Vec<String> {
    parse(text).expect_err("config should be rejected").0
}

fn assert_error(text: &str, needles: &[&str]) {
    let errors = errors(text);
    assert!(
        errors.iter().any(|e| needles.iter().all(|n| e.contains(n))),
        "no error containing {needles:?} in {errors:#?}"
    );
}

fn with_rule(rule: &str) -> String {
    format!("[outputs.eDP-1]\nbacklight = \"x\"\n\n[[outputs.eDP-1.rules]]\n{rule}\n")
}

#[test]
fn path_resolution() {
    assert_eq!(
        resolve_path(None, Some("/home/u/.config"), Some("/home/u")).unwrap(),
        PathBuf::from("/home/u/.config/woluma/config.toml")
    );
    assert_eq!(
        resolve_path(None, None, Some("/home/u")).unwrap(),
        PathBuf::from("/home/u/.config/woluma/config.toml")
    );
    assert_eq!(
        resolve_path(Some(Path::new("/tmp/w.toml")), Some("/x"), Some("/y")).unwrap(),
        PathBuf::from("/tmp/w.toml")
    );
}

#[test]
fn missing_file_names_path() {
    let error = load(Some(Path::new("/nonexistent/woluma.toml"))).unwrap_err();
    assert!(error.to_string().contains("/nonexistent/woluma.toml"));
}

#[test]
fn defaults_applied() {
    let config = parse(MINIMAL).unwrap();
    assert_eq!(config.general, General::default());
    assert!(config.general.enabled);
    assert_eq!(config.general.settle, Duration::from_secs(2));
    assert_eq!(config.general.transition, Duration::from_millis(400));
    assert_eq!(
        config.general.manual_override,
        ManualOverride::UntilRuleChange
    );
    assert_eq!(
        config.hysteresis,
        Hysteresis {
            luma: 5.0,
            als: 3.0
        }
    );
    assert_eq!(config.als, Als::None);
    assert_eq!(
        config.outputs[0].backend,
        Backend::Backlight("intel_backlight".into())
    );
}

#[test]
fn full_config() {
    let config = parse(
        r#"
[general]
enabled = false
settle = "500ms"
transition = "1s"
manual_override = "10m"

[hysteresis]
luma = 2
als = 1.5

[als]
source = "webcam"
device = "/dev/video2"

[outputs.DP-1]
ddcutil = "DELL U2720Q"
poll = "5s"

[[outputs.DP-1.rules]]
name = "night-bright"
when = { time = "21:00-07:00", luma = ">= 70" }
brightness = 30
run = ["notify-send hi"]

[[outputs.DP-1.rules]]
name = "default"
run = ["true"]
"#,
    )
    .unwrap();
    assert!(!config.general.enabled);
    assert_eq!(config.general.settle, Duration::from_millis(500));
    assert_eq!(
        config.general.manual_override,
        ManualOverride::For(Duration::from_secs(600))
    );
    assert_eq!(
        config.hysteresis,
        Hysteresis {
            luma: 2.0,
            als: 1.5
        }
    );
    assert_eq!(
        config.als,
        Als::Webcam {
            device: "/dev/video2".into()
        }
    );
    let output = &config.outputs[0];
    assert_eq!(
        output.backend,
        Backend::Ddcutil {
            display: "DELL U2720Q".into(),
            poll: Some(Duration::from_secs(5))
        }
    );
    assert_eq!(output.rules.len(), 2);
    assert!(output.rules[0].when.time.is_some() && output.rules[0].when.luma.is_some());
    assert_eq!(output.rules[1].brightness, None);
}

#[test]
fn invalid_duration() {
    assert_error(
        &format!("[general]\nsettle = \"two seconds\"\n{MINIMAL}"),
        &["general.settle", "\"2s\""],
    );
    assert_error(
        &format!("[general]\nmanual_override = \"forever\"\n{MINIMAL}"),
        &["general.manual_override"],
    );
}

#[test]
fn hysteresis_out_of_range() {
    assert_error(
        &format!("[hysteresis]\nluma = 150\n{MINIMAL}"),
        &["hysteresis.luma"],
    );
}

#[test]
fn als_sources() {
    assert_error(
        &format!("[als]\nsource = \"lidar\"\n{MINIMAL}"),
        &["als.source", "webcam", "iio", "none"],
    );
    assert_eq!(
        parse(&format!("[als]\nsource = \"none\"\n{MINIMAL}"))
            .unwrap()
            .als,
        Als::None
    );
    assert_eq!(
        parse(&format!(
            "[als]\nsource = \"iio\"\npath = \"/sys/x\"\n{MINIMAL}"
        ))
        .unwrap()
        .als,
        Als::Iio {
            path: Some("/sys/x".into())
        }
    );
    assert_error(
        &format!("[als]\nsource = \"iio\"\ndevice = \"/dev/video0\"\n{MINIMAL}"),
        &["als.device"],
    );
}

#[test]
fn output_backends() {
    assert_error(
        "[outputs.eDP-1]\nbacklight = \"a\"\nddcutil = \"b\"\n[[outputs.eDP-1.rules]]\nname = \"d\"\nbrightness = 1\n",
        &["outputs.eDP-1", "eDP-1", "both"],
    );
    assert_error(
        "[outputs.eDP-1]\n[[outputs.eDP-1.rules]]\nname = \"d\"\nbrightness = 1\n",
        &["eDP-1", "backlight"],
    );
    assert_error(
        "[outputs.eDP-1]\nbacklight = \"a\"\npoll = \"1s\"\n[[outputs.eDP-1.rules]]\nname = \"d\"\nbrightness = 1\n",
        &["outputs.eDP-1.poll"],
    );
}

#[test]
fn no_outputs() {
    assert_error("", &["at least one"]);
}

#[test]
fn empty_rules() {
    assert_error(
        "[outputs.eDP-1]\nbacklight = \"a\"\n",
        &["outputs.eDP-1.rules", "at least one rule"],
    );
}

#[test]
fn duplicate_rule_names() {
    let text = format!(
        "{}\n[[outputs.eDP-1.rules]]\nname = \"bright\"\nbrightness = 2\n",
        with_rule("name = \"bright\"\nbrightness = 1")
    );
    assert_error(&text, &["eDP-1", "duplicate", "bright"]);
}

#[test]
fn brightness_validation() {
    assert_error(
        &with_rule("name = \"b\"\nbrightness = 120"),
        &["outputs.eDP-1.rules[0].brightness", "'b'"],
    );
    assert_error(
        &with_rule("name = \"b\"\nbrightness = \"-10\""),
        &["absolute integer 0-100"],
    );
    assert_error(
        &with_rule("name = \"b\""),
        &["'b'", "`brightness` or `run`"],
    );
}

#[test]
fn unknown_rule_key() {
    assert_error(
        &with_rule("name = \"b\"\nbrightness = 1\ncontinue = true"),
        &["continue"],
    );
}

#[test]
fn condition_errors() {
    assert_error(
        &with_rule("name = \"b\"\nbrightness = 1\nwhen = { luma = 70 }"),
        &["when.luma", "explicit operator"],
    );
    assert_error(
        &with_rule("name = \"b\"\nbrightness = 1\nwhen = { luma = \"70\" }"),
        &["when.luma", "explicit operator"],
    );
    assert_error(
        &with_rule("name = \"b\"\nbrightness = 1\nwhen = { luma = \"70..30\" }"),
        &["when.luma", "'b'"],
    );
    assert_error(
        &with_rule("name = \"b\"\nbrightness = 1\nwhen = { battery = \"< 20\" }"),
        &["`luma`", "`als`", "`time`"],
    );
    assert_error(
        &with_rule("name = \"b\"\nbrightness = 1\nwhen = { time = \"07:00-07:00\" }"),
        &["when.time"],
    );
    assert!(parse(&with_rule(
        "name = \"b\"\nbrightness = 1\nwhen = { luma = \">= 70\" }"
    ))
    .is_ok());
}

#[test]
fn errors_are_aggregated() {
    let text = format!(
        "{}\n[[outputs.eDP-1.rules]]\nname = \"b\"\nbrightness = 120\n",
        with_rule("name = \"a\"\nbrightness = 1\nwhen = { luma = \"=> 70\" }")
    );
    let errors = errors(&text);
    assert_eq!(errors.len(), 2, "{errors:#?}");
    assert!(errors[0].starts_with("outputs.eDP-1.rules[0].when.luma"));
    assert!(errors[1].starts_with("outputs.eDP-1.rules[1].brightness"));
}

#[test]
fn unreachable_rules_warned() {
    let text = format!(
        "{}\n[[outputs.eDP-1.rules]]\nname = \"later\"\nbrightness = 2\n",
        with_rule("name = \"fallback\"\nbrightness = 1")
    );
    let warnings = validate::warnings(&parse(&text).unwrap());
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("later") && warnings[0].contains("fallback"));
    assert!(validate::warnings(&parse(MINIMAL).unwrap()).is_empty());
}
