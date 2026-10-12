# Config Specification

## Purpose

Defines where woluma's TOML configuration lives, its schema for general settings, hysteresis, ambient light, outputs and rules, and how it is validated at startup with actionable errors.

## Requirements

### Requirement: Config file location
The daemon SHALL load its configuration from `$XDG_CONFIG_HOME/woluma/config.toml`, falling back to `$HOME/.config/woluma/config.toml` when `XDG_CONFIG_HOME` is unset. A `--config <path>` option SHALL override the location.

#### Scenario: Default location
- **WHEN** the daemon starts with `XDG_CONFIG_HOME=/home/u/.config` and no `--config`
- **THEN** it reads `/home/u/.config/woluma/config.toml`

#### Scenario: Explicit path
- **WHEN** the daemon starts with `--config /tmp/w.toml`
- **THEN** it reads `/tmp/w.toml` and ignores the default location

#### Scenario: Missing file
- **WHEN** the resolved config file does not exist
- **THEN** the daemon exits with a non-zero status and an error naming the path it looked for

### Requirement: General section
The `[general]` table SHALL accept `enabled` (bool, default `true`), `settle` (duration, default `2s`), `transition` (duration, default `400ms`), and `manual_override` (`"until-rule-change"` or a duration, default `"until-rule-change"`). Durations SHALL be strings with a unit suffix (`ms`, `s`, `m`, `h`).

#### Scenario: Defaults applied
- **WHEN** the config omits `[general]`
- **THEN** the effective values are enabled=true, settle=2s, transition=400ms, manual_override=until-rule-change

#### Scenario: Invalid duration
- **WHEN** `settle = "two seconds"`
- **THEN** startup fails with an error identifying `general.settle` and the expected format

### Requirement: Hysteresis section
The `[hysteresis]` table SHALL accept `luma` and `als` as non-negative numbers in the 0–100 scale, defaulting to `5` and `3`.

#### Scenario: Out of range hysteresis
- **WHEN** `[hysteresis] luma = 150`
- **THEN** startup fails with an error identifying `hysteresis.luma`

### Requirement: ALS section parsing
The `[als]` table SHALL be optional. When present it SHALL accept `source` (`"webcam"`, `"iio"`, or `"none"`) plus source-specific keys, which SHALL be parsed and validated by this capability. When absent or `source = "none"`, the ambient reading SHALL be permanently unknown.

#### Scenario: No ALS section
- **WHEN** the config has no `[als]` table
- **THEN** the daemon starts and treats the ambient reading as unknown

#### Scenario: Unknown source
- **WHEN** `[als] source = "lidar"`
- **THEN** startup fails with an error listing the valid sources

### Requirement: Output definitions
Each `[outputs.<name>]` table SHALL be keyed by the Wayland output name and SHALL declare exactly one brightness backend: `backlight = "<sysfs device>"` or `ddcutil = "<model or serial>"`. `poll` (duration) SHALL be accepted for ddcutil outputs. At least one output SHALL be defined.

#### Scenario: Both backends declared
- **WHEN** an output sets both `backlight` and `ddcutil`
- **THEN** startup fails with an error naming the output

#### Scenario: No outputs
- **WHEN** the config defines no `[outputs.*]`
- **THEN** startup fails with an error stating at least one output is required

### Requirement: Rule definitions
Each output SHALL have an ordered, non-empty `[[outputs.<name>.rules]]` array. Each rule SHALL accept `name` (string, required, unique within the output), `when` (inline table, optional, default empty), `brightness` (optional integer 0–100), and `run` (optional array of strings). A rule SHALL define at least one of `brightness` or `run`. Unknown keys SHALL be rejected.

#### Scenario: Duplicate rule names
- **WHEN** two rules on `eDP-1` are both named `bright`
- **THEN** startup fails with an error naming the output and the duplicate

#### Scenario: Brightness out of range
- **WHEN** a rule sets `brightness = 120`
- **THEN** startup fails with an error naming the rule

#### Scenario: Relative brightness rejected
- **WHEN** a rule sets `brightness = "-10"`
- **THEN** startup fails with an error stating brightness must be an absolute integer 0–100

#### Scenario: Unknown key
- **WHEN** a rule contains `continue = true`
- **THEN** startup fails with an error naming the unknown key `continue`

#### Scenario: Unreachable rules warned
- **WHEN** a rule with an empty `when` is followed by further rules
- **THEN** the daemon logs a warning that the later rules are unreachable and continues

### Requirement: Validation errors are aggregated
Config validation SHALL report all detected errors in one run, each with its TOML path (e.g. `outputs.eDP-1.rules[2].when.luma`), rather than stopping at the first.

#### Scenario: Multiple errors
- **WHEN** the config has an invalid operator in one rule and an out-of-range brightness in another
- **THEN** both errors are printed before the daemon exits non-zero
