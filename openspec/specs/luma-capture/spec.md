# Luma Capture Specification

## Purpose

Defines per-output Wayland screen capture and its reduction to a 0–100 perceived-lightness value (luma) at a steady sampling cadence.

## Requirements

### Requirement: Per-output capture
The daemon SHALL capture screen contents independently for every configured output that exists on the Wayland compositor, using a screencopy protocol (`ext-image-copy-capture-v1`, falling back to `wlr-screencopy-unstable-v1`).

#### Scenario: Two outputs
- **WHEN** `eDP-1` and `DP-1` are configured and connected
- **THEN** each produces its own luma stream, and content on one does not affect the other's value

#### Scenario: Configured output not connected
- **WHEN** `DP-1` is configured but not connected
- **THEN** the daemon logs that it is absent, runs the other outputs, and starts capturing `DP-1` when it appears

#### Scenario: Output disconnected at runtime
- **WHEN** a captured output is unplugged
- **THEN** capture for that output stops without affecting other outputs, and resumes if it reconnects

### Requirement: Perceived lightness value
Each captured frame SHALL be reduced to a single integer luma in 0–100 using perceived lightness `sqrt(0.241·R² + 0.691·G² + 0.068·B²) / 255 · 100` over the frame's mean RGB, with the reduction performed on the GPU via Vulkan mipmapping.

#### Scenario: Fully white screen
- **WHEN** the output displays solid white
- **THEN** the reported luma is 100

#### Scenario: Fully black screen
- **WHEN** the output displays solid black
- **THEN** the reported luma is 0

### Requirement: Sampling cadence
Luma SHALL be sampled for each output at a fixed interval (default 500 ms) so that CPU and GPU usage stay low while still detecting content changes well within the configured settle time.

#### Scenario: Static screen
- **WHEN** screen content does not change for one minute
- **THEN** luma samples continue at the configured interval and report a stable value

### Requirement: Capture failure handling
If capture fails for an output (protocol unsupported, GPU error), the daemon SHALL log the error, treat that output's luma as unavailable, and retry with backoff; rules on that output SHALL NOT be re-evaluated while luma is unavailable.

#### Scenario: Compositor without screencopy
- **WHEN** neither screencopy protocol is advertised
- **THEN** the daemon exits non-zero with an error naming the missing protocols

#### Scenario: Transient GPU failure
- **WHEN** a single frame fails to process
- **THEN** the daemon logs it, keeps the output's current active rule, and retries on the next interval
