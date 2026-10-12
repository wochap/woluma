# Hooks Specification

## Purpose

Defines how a rule's `run` commands execute when an output's active rule changes: ordering, environment contract, timeouts, and failure handling, without blocking the daemon.

## Requirements

### Requirement: Run on active-rule change
When an output's active rule changes, including the first activation after startup, the daemon SHALL execute every entry of the new rule's `run` list. Hooks SHALL NOT run while the active rule stays the same, and SHALL NOT run on transitions to "no active rule".

#### Scenario: Rule becomes active
- **WHEN** `bright` with `run = ["a.sh", "b.sh"]` becomes active on `eDP-1`
- **THEN** `a.sh` and `b.sh` are executed once

#### Scenario: Rule stays active
- **WHEN** `bright` remains active across 100 luma samples
- **THEN** its hooks are not executed again

#### Scenario: Startup activation
- **WHEN** the daemon starts and `default` is the first active rule on `eDP-1`
- **THEN** `default`'s hooks run with an empty `WOLUMA_PREV_RULE`

### Requirement: Execution semantics
Each `run` entry SHALL be executed via `sh -c "<entry>"`. Entries within one rule activation SHALL run sequentially in list order; each SHALL start after the previous exits. A non-zero exit SHALL be logged and SHALL NOT prevent later entries from running.

#### Scenario: Ordered execution
- **WHEN** `run = ["echo 1 >> f", "echo 2 >> f"]`
- **THEN** `f` contains `1` then `2`

#### Scenario: Failing entry
- **WHEN** the first entry exits with status 1
- **THEN** the failure is logged with the rule name, and the second entry still runs

### Requirement: Hooks do not block the daemon
Hook execution SHALL happen off the daemon's main loop so sampling and rule evaluation continue during long-running hooks. If the same output's active rule changes again while its previous hook sequence is still running, the new sequence SHALL start after the previous one finishes, preserving order. Each hook entry SHALL be killed after a timeout (default 30s) and the timeout logged.

#### Scenario: Slow hook
- **WHEN** a hook sleeps for 10s
- **THEN** luma sampling and rule evaluation continue during those 10s

#### Scenario: Hung hook
- **WHEN** a hook runs longer than 30s
- **THEN** it is killed and a timeout is logged

### Requirement: Hook environment
Each hook process SHALL inherit the daemon's environment plus: `WOLUMA_OUTPUT` (output name), `WOLUMA_RULE` (new rule name), `WOLUMA_PREV_RULE` (previous rule name or empty), `WOLUMA_LUMA` (integer 0–100), `WOLUMA_ALS` (integer 0–100, or empty when unknown), `WOLUMA_TIME` (local `HH:MM`), and `WOLUMA_BRIGHTNESS` (the rule's brightness, or empty if the rule has none).

#### Scenario: Environment populated
- **WHEN** `night-bright` (brightness 30) replaces `night` on `eDP-1` at 21:40 with luma 85 and unknown ALS
- **THEN** the hook sees `WOLUMA_OUTPUT=eDP-1`, `WOLUMA_RULE=night-bright`, `WOLUMA_PREV_RULE=night`, `WOLUMA_LUMA=85`, `WOLUMA_ALS=`, `WOLUMA_TIME=21:40`, `WOLUMA_BRIGHTNESS=30`

#### Scenario: Rule without brightness
- **WHEN** a rule with only `run` becomes active
- **THEN** `WOLUMA_BRIGHTNESS` is set and empty

### Requirement: Hooks skipped while disabled
When the daemon is disabled (`general.enabled = false`), rules SHALL still be tracked internally but no hooks SHALL run.

#### Scenario: Disabled at startup
- **WHEN** `general.enabled = false` and the active rule changes
- **THEN** no hook is executed
