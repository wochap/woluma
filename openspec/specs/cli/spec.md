# CLI Specification

## Purpose

Defines the `woluma` command-line interface: the subcommands for running the daemon, controlling it over the control socket (toggle, pause, resume, status), watching live or locally sampled readings, and validating config, along with client exit codes and error reporting.

## Requirements

### Requirement: Command structure
The `woluma` binary SHALL expose the subcommands `daemon`, `toggle`, `pause`, `resume`, `status`, `watch`, and `check`, defined with clap derive so that help, version, and shell completions come from one definition. Running `woluma` with no subcommand SHALL behave as `woluma daemon`. A global `--config PATH` option SHALL override the default config path for `daemon`, `watch` (standalone), and `check`.

#### Scenario: Default runs daemon
- **WHEN** the user runs `woluma` with no arguments
- **THEN** the daemon starts exactly as with `woluma daemon`

#### Scenario: Help lists commands
- **WHEN** the user runs `woluma --help`
- **THEN** the output lists daemon, toggle, pause, resume, status, watch, and check with one-line descriptions

### Requirement: Toggle command
`woluma toggle [TARGET]` SHALL send a toggle request, where TARGET is an output name or `*` and defaults to `*` when omitted. It SHALL print the resulting enabled state per affected output, for example `eDP-1: disabled`.

#### Scenario: Toggle all by default
- **WHEN** the user runs `woluma toggle` while both outputs are enabled
- **THEN** both are disabled and the command prints `eDP-1: disabled` and `DP-1: disabled`

### Requirement: Pause and resume commands
`woluma pause TARGET [DURATION]` SHALL pause the target. DURATION accepts humantime values such as `90s`, `15m`, or `1h30m`; when omitted, the pause lasts until resumed. `woluma resume TARGET` SHALL clear the pause and re-apply the current rule. TARGET is required for both and may be `*`. An invalid DURATION SHALL be rejected by the client before contacting the daemon.

#### Scenario: Timed pause
- **WHEN** the user runs `woluma pause eDP-1 15m`
- **THEN** eDP-1 is paused with reason manual until 15 minutes from now, and the command prints the until time

#### Scenario: Invalid duration
- **WHEN** the user runs `woluma pause eDP-1 soon`
- **THEN** the command exits with code 2 and a usage error, without connecting to the socket

### Requirement: Status command
`woluma status` SHALL print a human-readable table with one row per output (OUTPUT, LUMA, ALS, RULE, BRIGHTNESS, STATE). `woluma status --json` SHALL print the daemon's status snapshot as a single JSON object on one line, suitable for status bars such as waybar.

#### Scenario: Human status
- **WHEN** the user runs `woluma status`
- **THEN** a header row and one row per output are printed, with `--` for unknown values

#### Scenario: JSON status
- **WHEN** the user runs `woluma status --json`
- **THEN** stdout is exactly one line of valid JSON with an `outputs` array

### Requirement: Watch command with daemon
`woluma watch` SHALL subscribe to the daemon and print one line per output per update with columns TIME OUTPUT LUMA ALS RULE BRIGHTNESS STATE. STATE renders as `active`, `settling (1.2s)`, `paused until-rule-change (manual)`, `paused until HH:MM (manual)`, or `disabled`. A header SHALL be printed once at the start. Unknown values SHALL render as `--`. With `--json`, each update is printed as one JSON object per line (NDJSON) and no header is printed. With `--raw`, only TIME OUTPUT LUMA ALS CLOCK are printed. The command SHALL run until interrupted and SHALL exit 0 on SIGINT.

#### Scenario: Live human output
- **WHEN** the user runs `woluma watch` and eDP-1 enters rule `bright` at 40%
- **THEN** a line like `14:05:02  eDP-1  88  --  bright  40  active` is printed

#### Scenario: NDJSON
- **WHEN** the user runs `woluma watch --json | jq .`
- **THEN** each line is a complete JSON snapshot that jq parses individually

#### Scenario: Raw values for calibration
- **WHEN** the user runs `woluma watch --raw`
- **THEN** lines contain only time, output, luma, ALS, and clock values, with no rule or brightness columns

### Requirement: Watch command without daemon
When no daemon answers on the socket, `woluma watch` SHALL print a one-line notice to stderr and then sample in-process: per-output luma using the same capture pipeline as the daemon, ALS if configured, and local time. It SHALL NOT write brightness or run hooks. If a config is present and valid, it SHALL also show the rule each output would match. If no config exists, it SHALL still show raw values for every detected output. `--raw` and `--json` SHALL behave as in daemon mode.

#### Scenario: Calibrate before config exists
- **WHEN** no daemon is running, no config file exists, and the user runs `woluma watch --raw`
- **THEN** stderr notes "no daemon running; sampling locally" and stdout shows live luma values for every detected output, with ALS as `--`

#### Scenario: Standalone with config
- **WHEN** no daemon is running, a valid config exists, and the user runs `woluma watch`
- **THEN** lines include the RULE each output would match, and brightness stays unchanged

### Requirement: Check command
`woluma check` SHALL load and validate the config without needing a daemon. On errors it SHALL print every problem with its location (output and rule index or name) and exit 1. On success it SHALL print `config OK`, any warnings, and, if luma can be sampled, the rule each output matches right now with its brightness. Warnings SHALL include: an output without a final fallback rule (a rule with no conditions); rules that can never be reached because an earlier rule's conditions always cover them (at minimum, any rule after an unconditional rule); and configured outputs not currently connected.

`woluma check --offline` SHALL validate syntax and semantics only. It SHALL NOT connect to Wayland, the control socket, backlight, DDC, or camera devices, and SHALL skip the "not connected" warning and the current-match output. Its exit codes SHALL be the same as plain `check`. This mode is used for build-time validation (e.g. by the Nix home-manager module).

#### Scenario: Offline validation in a sandbox
- **WHEN** `woluma check --offline --config cfg.toml` runs with no `WAYLAND_DISPLAY`, no `XDG_RUNTIME_DIR`, and no access to `/sys/class/backlight` or `/dev/video*`
- **THEN** it validates the file, prints `config OK` plus any rule-structure warnings, and exits 0 for a valid config (1 for an invalid one) without attempting any hardware or display access

#### Scenario: Invalid config
- **WHEN** a rule has `when = { luma = "70" }` (no operator)
- **THEN** check prints an error naming the output and rule and stating that an explicit operator is required, and exits 1

#### Scenario: Missing fallback warning
- **WHEN** eDP-1's last rule has conditions
- **THEN** check prints `config OK` and a warning that eDP-1 has no fallback rule and brightness is left unchanged when nothing matches

#### Scenario: Unreachable rule warning
- **WHEN** eDP-1 has an unconditional rule followed by another rule
- **THEN** check warns that the later rule is unreachable

#### Scenario: Current match
- **WHEN** the config is valid and eDP-1's current luma is 88
- **THEN** check prints `eDP-1: matching now → bright (brightness 40)`

### Requirement: Client errors and exit codes
Client commands SHALL exit 0 on success, 1 on a runtime error (including an `ok: false` response or invalid config in `check`), 2 on usage errors, and 3 when the daemon is not running (for `toggle`, `pause`, `resume`, `status`). For exit code 3 the message SHALL say the daemon is not running and suggest `woluma daemon` or the systemd service.

#### Scenario: Daemon not running
- **WHEN** no daemon is running and the user runs `woluma toggle`
- **THEN** stderr says the daemon is not running and the exit code is 3

#### Scenario: Daemon error surfaced
- **WHEN** the daemon replies `ok: false` with "unknown output HDMI-9"
- **THEN** the CLI prints that error to stderr and exits 1
