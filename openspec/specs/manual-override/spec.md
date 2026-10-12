# Manual Override Specification

## Purpose

Defines how the daemon classifies external brightness changes as manual, pauses automatic control of the affected output, and resumes it according to the `manual_override` setting or explicit control requests.

## Requirements

### Requirement: Classify manual changes
An external brightness change, as detected by `brightness-control`, SHALL count as manual unless it happens inside a hook grace window. A hook grace window SHALL open when an output's active rule changes and that rule has `run` commands. It SHALL close when all of those commands have exited plus a settle period of 1 second. When the window closes, the daemon SHALL snapshot the output's brightness, and that snapshot becomes the expected brightness. Only deviations from it after the window closes SHALL count as manual.

#### Scenario: Brightness key press
- **WHEN** no grace window is open for eDP-1 and its brightness changes to a value the daemon did not write
- **THEN** the change counts as manual

#### Scenario: Hook script changes brightness
- **WHEN** a rule's `run` script calls `brightnessctl s 30%` and exits
- **THEN** the change does not count as manual, and 30% is snapshotted once the script has exited and 1 second has passed

#### Scenario: Change after hook settles
- **WHEN** the grace window has closed with a snapshot of 30%, and brightness later changes to 50% without a daemon write
- **THEN** the change counts as manual

#### Scenario: Long-running hook
- **WHEN** a hook script is still running 30 seconds after it started
- **THEN** the grace window closes anyway, the daemon logs a warning, and it takes the snapshot

### Requirement: Pause on manual change
A manual change SHALL pause the affected output only. While an output is paused, the daemon SHALL NOT write brightness to it and SHALL NOT run `run` commands for rule changes on it. Rule evaluation SHALL continue, so that the active rule stays tracked.

#### Scenario: Pause only affects one output
- **WHEN** a manual change occurs on eDP-1
- **THEN** eDP-1 is paused and DP-1 keeps being controlled

#### Scenario: Same rule keeps manual level
- **WHEN** eDP-1 is paused at a manual 55% while rule `bright` is active, and the user opens another bright page that still matches `bright`
- **THEN** brightness stays at 55% and no hooks run

#### Scenario: Any ramp in progress is cancelled
- **WHEN** a manual change occurs while a ramp is in progress on that output
- **THEN** the ramp stops immediately and no further ramp writes happen

### Requirement: Until-rule-change mode
When `[general] manual_override = "until-rule-change"` (the default), a paused output SHALL resume when its active rule changes to a different rule. On resuming, the daemon SHALL apply the new rule exactly as it would on any rule change: write its brightness and run its `run` commands.

#### Scenario: Rule change ends pause
- **WHEN** eDP-1 is paused under rule `bright` and the active rule changes to `default`
- **THEN** eDP-1 resumes, its brightness is set to `default`'s value, and `default`'s `run` commands run

#### Scenario: Default mode
- **WHEN** `manual_override` is not set
- **THEN** the daemon behaves as `until-rule-change`

### Requirement: Duration mode
When `[general] manual_override` is a duration (for example `"15m"`), a paused output SHALL resume when the duration expires. On expiry, the daemon SHALL NOT change brightness or run hooks. It SHALL act again only at the next change of active rule. A rule change before expiry SHALL NOT end the pause.

#### Scenario: Pause expires quietly
- **WHEN** `manual_override = "15m"` and eDP-1 was paused 15 minutes ago under rule `bright`, which is still active
- **THEN** eDP-1 resumes with no brightness write and no hook run

#### Scenario: Next rule change after expiry
- **WHEN** the duration-mode pause has expired and the active rule changes to `default`
- **THEN** the daemon applies `default`'s brightness and runs its hooks

#### Scenario: Rule change during duration pause
- **WHEN** `manual_override = "15m"`, eDP-1 is paused, and the active rule changes 5 minutes in
- **THEN** eDP-1 stays paused until the 15 minutes expire

### Requirement: Restart pause on further manual change
A manual change on an output that is already paused SHALL restart its pause. In duration mode, the timer SHALL restart from that moment.

#### Scenario: Second adjustment extends pause
- **WHEN** `manual_override = "15m"`, eDP-1 was paused 10 minutes ago, and the user adjusts brightness again
- **THEN** eDP-1 stays paused until 15 minutes after the second adjustment

### Requirement: Validate manual_override value
The daemon SHALL accept `manual_override` values of `"until-rule-change"` or a positive duration string (such as `"30s"`, `"15m"`, `"1h"`). Any other value SHALL be a config error at startup.

#### Scenario: Invalid value
- **WHEN** the config contains `manual_override = "forever"`
- **THEN** the daemon exits with a config error naming `manual_override`

### Requirement: Expose pause state and controls
For each output, the daemon SHALL expose whether it is paused, the pause reason (`manual`, or `requested` for pauses issued through the control socket), and the resume deadline (a timestamp in duration mode, or `until-rule-change`). It SHALL provide operations to pause an output, with an optional duration, and to resume it. An explicit resume SHALL apply the current active rule immediately: write its brightness and run its `run` commands.

#### Scenario: Status of manually paused output
- **WHEN** the control socket queries eDP-1, which was manually paused in until-rule-change mode
- **THEN** the response reports paused = true, reason = `manual`, until = `until-rule-change`

#### Scenario: Explicit resume
- **WHEN** a resume is requested for eDP-1 while it is paused and rule `bright` (`brightness = 40`) is active
- **THEN** eDP-1 is unpaused, set to 40%, and `bright`'s `run` commands run

#### Scenario: Requested pause
- **WHEN** a pause for 10 minutes is requested for DP-1 through the control socket
- **THEN** DP-1 is paused with reason `requested` and a deadline 10 minutes from now
