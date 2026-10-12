# Ambient Light Specification

## Purpose

Defines how the daemon measures ambient light from a webcam (with locked exposure) or an IIO light sensor, normalizes it to a shared 0–100 reading (or unknown with a reason), and feeds it to rule evaluation and status inspection.

## Requirements

### Requirement: Source selection
The daemon SHALL select the ambient light source from `[als].source`, which accepts `webcam`, `iio`, or `none`. When the `[als]` section is absent, the source SHALL be `none`. With source `none`, the reading SHALL always be unknown and the daemon SHALL NOT open any camera or sensor device.

#### Scenario: No als section
- **WHEN** the config has no `[als]` section
- **THEN** the ambient reading is unknown and no video or IIO device is opened

#### Scenario: Webcam source configured
- **WHEN** `[als]` has `source = "webcam"` and `device = "/dev/video0"`
- **THEN** the daemon samples ambient light from `/dev/video0`

### Requirement: Shared periodic reading
The daemon SHALL take one ambient sample every `[als].interval` (default 60s) and publish the result as a single reading shared by all outputs. A sample SHALL also be taken immediately at startup, and when the daemon is re-enabled after being toggled off. Each published reading SHALL be either a value in 0–100 or unknown, together with a reason when unknown.

#### Scenario: One reading for all outputs
- **WHEN** outputs `eDP-1` and `DP-1` both have rules with `als` conditions
- **THEN** both are evaluated against the same latest ambient reading

#### Scenario: Startup sample
- **WHEN** the daemon starts with `source = "webcam"` and `interval = "60s"`
- **THEN** the first sample is taken at startup, not after 60 seconds

### Requirement: Webcam locked-exposure sampling
For each webcam sample, the daemon SHALL open the device, record the current values of the exposure controls it will change, set `auto_exposure` to manual mode and `exposure_time_absolute` to `[als].exposure`, capture `[als].frames` frames (default 10) at the smallest supported resolution, compute the mean of the luma (Y) channel over all captured pixels, and close the device. The device SHALL NOT remain open between samples.

#### Scenario: Sample with locked exposure
- **WHEN** a webcam sample is taken with `exposure = 600` and `frames = 10`
- **THEN** auto-exposure is disabled, exposure is 600 during capture, 10 frames are averaged, and the device is closed afterwards

#### Scenario: Exposure outside device range
- **WHEN** `[als].exposure` is 5000 and the device reports a maximum of 1250
- **THEN** the daemon uses 1250 and logs a warning once

### Requirement: Webcam controls are always restored
After every webcam sample, the daemon SHALL restore the exposure controls it changed to the values recorded before the sample. Restoration SHALL happen on success, on capture errors, on placeholder detection, and when the daemon shuts down mid-sample (SIGINT/SIGTERM).

#### Scenario: Restore after success
- **WHEN** a sample completes on a device that had `auto_exposure = 3`
- **THEN** `auto_exposure` is 3 again after the sample

#### Scenario: Restore after capture error
- **WHEN** frame capture fails after exposure was locked
- **THEN** the saved exposure controls are restored before the error is reported

#### Scenario: Restore on shutdown
- **WHEN** the daemon receives SIGTERM while a sample is in progress
- **THEN** the saved exposure controls are restored before the process exits

### Requirement: Webcam value normalization
The daemon SHALL convert the mean limited-range luma to 0–100 as `(Y − 16) / 219 × 100`, clamped to 0–100 and rounded to the nearest integer.

#### Scenario: Black frame
- **WHEN** the mean Y is 16
- **THEN** the reading is 0

#### Scenario: Measured lit room
- **WHEN** the mean Y is 68.9
- **THEN** the reading is 24

#### Scenario: Above white level
- **WHEN** the mean Y is 240
- **THEN** the reading is 100

### Requirement: Unavailable webcam reports unknown
The reading SHALL be unknown, with a reason, when the configured device does not exist, cannot be opened or configured (including busy because another application holds it), or fails during capture. The daemon SHALL NOT reuse the previous value in these cases, and SHALL keep retrying at the normal interval.

#### Scenario: Camera in use by a video call
- **WHEN** opening `/dev/video0` fails with "device busy"
- **THEN** the reading is unknown with reason `busy`, and the next sample is attempted after `interval`

#### Scenario: Device missing
- **WHEN** `/dev/video0` does not exist
- **THEN** the reading is unknown with reason `missing`

#### Scenario: Camera comes back
- **WHEN** a sample returns unknown and the next sample succeeds with value 24
- **THEN** the published reading becomes 24

### Requirement: Placeholder frame detection
The daemon SHALL treat a webcam sample as unknown with reason `placeholder` when the captured frames do not look like a real scene: either all captured frames are byte-identical, or the spatial standard deviation of Y across the averaged frame is below the placeholder threshold. Real sensor output with locked exposure contains noise, so identical frames indicate a synthetic or disabled feed.

#### Scenario: Hardware camera switch off
- **WHEN** the camera switch is off and the device returns identical uniform frames
- **THEN** the reading is unknown with reason `placeholder`

#### Scenario: Real dim scene
- **WHEN** the frames differ by sensor noise and the scene is dim but not uniform
- **THEN** the reading is the normalized mean, not unknown

### Requirement: IIO light sensor source
With `source = "iio"`, the daemon SHALL find an IIO device exposing `in_illuminance_raw`, `in_illuminance_input`, `in_illuminance0_*`, or `in_illuminance_clear_*` under `/sys/bus/iio/devices` (or at `[als].device` when set). It SHALL compute lux as `(raw + offset) × scale` (or read `*_input` directly) and map lux to 0–100 as `log10(1 + lux) / 4 × 100`, clamped to 0–100 and rounded. When no such sensor exists or the read fails, the reading SHALL be unknown with reason `missing` or `error`.

#### Scenario: Lux mapping
- **WHEN** the sensor reports 100 lux
- **THEN** the reading is 50

#### Scenario: Bright daylight
- **WHEN** the sensor reports 20000 lux
- **THEN** the reading is 100

#### Scenario: No sensor present
- **WHEN** `source = "iio"` and no IIO illuminance channel exists
- **THEN** the reading is unknown with reason `missing` and a warning is logged once

### Requirement: Reading feeds rule evaluation
Each published reading SHALL be delivered to the rule engine's ALS input, which applies settle time and `hysteresis.als`. An unknown reading SHALL be delivered as unknown so that `als` conditions do not match, and hooks receive an empty `WOLUMA_ALS`.

#### Scenario: Camera switched off at night
- **WHEN** the active rule for `eDP-1` has `als = "< 20"` and the next sample is unknown
- **THEN** that rule no longer matches and evaluation falls through to the next rule

### Requirement: Reading exposed for inspection
The daemon SHALL expose the latest ambient reading, its source, the time it was taken, and the unknown reason (if any), for the control socket and `woluma watch --raw`.

#### Scenario: Watch shows unknown reason
- **WHEN** the latest sample was unknown with reason `placeholder`
- **THEN** status output reports ALS as unknown with reason `placeholder`
