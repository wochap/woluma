# Brightness Control Specification

## Purpose

Defines how the daemon reads and writes output brightness through sysfs backlight (with a logind fallback) and DDC/CI, maps percentages to raw device values, ramps smoothly between levels, and detects brightness changes it did not make.

## Requirements

### Requirement: Backlight outputs
The daemon SHALL control an output configured with `backlight = "<name>"` through `/sys/class/backlight/<name>`, reading `brightness` and `max_brightness`. If `<name>` does not exist under `/sys/class/backlight`, the daemon SHALL fail at startup with an error naming the output and the missing device.

#### Scenario: Backlight device found
- **WHEN** the config contains `[outputs.eDP-1] backlight = "amdgpu_bl1"` and `/sys/class/backlight/amdgpu_bl1` exists
- **THEN** the daemon reads the current brightness and max_brightness of that device and uses it for output `eDP-1`

#### Scenario: Backlight device missing
- **WHEN** the config names `backlight = "does_not_exist"`
- **THEN** the daemon exits with an error naming the output and `does_not_exist`

### Requirement: Backlight write fallback through logind
When the sysfs `brightness` file is not writable by the daemon, the daemon SHALL set backlight brightness through logind's `org.freedesktop.login1.Session.SetBrightness` D-Bus method. If neither path is available, the daemon SHALL log an error for that output and keep running for the other outputs.

#### Scenario: No write permission on sysfs
- **WHEN** the daemon cannot open `/sys/class/backlight/amdgpu_bl1/brightness` for writing and a logind session is available
- **THEN** brightness writes for that output go through logind `SetBrightness`

#### Scenario: No write path at all
- **WHEN** neither the sysfs file nor logind can be used for an output
- **THEN** the daemon logs an error for that output and keeps controlling the other outputs

### Requirement: DDC/CI outputs
The daemon SHALL control an output configured with `ddcutil = "<identifier>"` through DDC/CI VCP feature `0x10` (luminance). The identifier SHALL match a detected display's model name or serial number. Reads report the monitor's current and maximum values.

#### Scenario: Monitor matched by model
- **WHEN** the config contains `[outputs.DP-1] ddcutil = "DELL U2720Q"` and a DDC display with that model is detected
- **THEN** the daemon controls VCP 0x10 of that display for output `DP-1`

#### Scenario: Monitor not found
- **WHEN** no detected DDC display matches the identifier
- **THEN** the daemon logs an error for that output, retries detection periodically, and keeps controlling the other outputs

### Requirement: Percentage brightness mapping
Rule brightness values SHALL be percentages from 0 to 100. The daemon SHALL map a percentage to a raw device value linearly between the device's minimum and maximum, and SHALL map raw values back to the nearest whole percentage when reporting. For backlight devices, 0% SHALL map to a minimum floor of 1 raw unit, so the panel is never turned fully off.

#### Scenario: Percent to raw on backlight
- **WHEN** a backlight has max_brightness 255 and the target is 40%
- **THEN** the daemon writes raw value 102

#### Scenario: Zero percent keeps panel lit
- **WHEN** a backlight rule sets `brightness = 0`
- **THEN** the daemon writes raw value 1, not 0

#### Scenario: Reporting current percentage
- **WHEN** a DDC monitor reports current 37 with maximum 100
- **THEN** the daemon reports the output's brightness as 37%

### Requirement: Apply active rule brightness
When an output's active rule changes and the output is neither paused nor disabled, the daemon SHALL move that output toward the new rule's `brightness`. A rule without a `brightness` key SHALL leave brightness untouched.

#### Scenario: Rule with brightness becomes active
- **WHEN** rule `bright` with `brightness = 40` becomes active on eDP-1, which is not paused
- **THEN** the daemon sets eDP-1 to 40%

#### Scenario: Rule without brightness
- **WHEN** a rule that has only `run` becomes active
- **THEN** the daemon does not write brightness for that output

### Requirement: Skip redundant writes
The daemon SHALL NOT write brightness when the output's current brightness already equals the target, compared in raw device units.

#### Scenario: Already at target
- **WHEN** eDP-1 is at 40% and the newly active rule targets 40%
- **THEN** no write to the device occurs

### Requirement: Smooth transitions
The daemon SHALL ramp brightness from the current to the target value over the duration in `[general] transition`, in steps of at least 16 ms apart. When `transition` is `"0s"`, it SHALL write the target directly. A new target that arrives mid-ramp SHALL replace the old one, and the ramp SHALL continue from the current value. For DDC outputs, the ramp SHALL use at most one write per 200 ms, because DDC writes are slow.

#### Scenario: Ramp on backlight
- **WHEN** `transition = "400ms"` and eDP-1 moves from 70% to 40%
- **THEN** the daemon writes intermediate values over about 400 ms and finishes exactly at 40%

#### Scenario: Retarget during ramp
- **WHEN** a ramp toward 40% is in progress at 55% and a new rule targets 70%
- **THEN** the ramp turns around from 55% toward 70% without first reaching 40%

#### Scenario: Instant transition
- **WHEN** `transition = "0s"`
- **THEN** the daemon writes the target in a single write

### Requirement: Detect external brightness changes
The daemon SHALL detect brightness changes it did not write. Backlight outputs SHALL be watched with inotify on the sysfs `brightness` file. DDC outputs SHALL be polled at the output's `poll` interval (default `"5s"`). The daemon's own writes, including every ramp step, SHALL NOT be reported as external changes.

#### Scenario: Brightness key on laptop
- **WHEN** the user presses a brightness key and the sysfs value changes to a value the daemon did not write
- **THEN** the daemon reports an external change for eDP-1 within one event-loop iteration

#### Scenario: Monitor OSD change
- **WHEN** the user changes brightness on DP-1 through the monitor's buttons and `poll = "5s"`
- **THEN** the daemon reports an external change for DP-1 within about 5 seconds

#### Scenario: Own ramp is not external
- **WHEN** the daemon ramps eDP-1 from 70% to 40%
- **THEN** no external change is reported for any intermediate value

### Requirement: Expose brightness state
For each output, the daemon SHALL expose the current brightness percentage, the target brightness percentage (if any), and whether a transition is in progress, so the control socket can read them.

#### Scenario: Status during ramp
- **WHEN** the control socket queries eDP-1 during a ramp from 70% to 40%
- **THEN** the response includes the current percentage, target 40, and transitioning = true
