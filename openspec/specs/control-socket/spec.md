# Control Socket Specification

## Purpose

Defines the daemon's per-user Unix control socket: its location, permissions, single-instance and self-healing behavior, the line-delimited JSON protocol, and the toggle, pause/resume, status, and subscribe requests it serves.

## Requirements

### Requirement: Socket location and permissions
The daemon SHALL serve a Unix stream socket at `$XDG_RUNTIME_DIR/woluma.sock`. The socket file SHALL have mode `0600`. The daemon MUST refuse to start if `XDG_RUNTIME_DIR` is unset.

#### Scenario: Socket created on start
- **WHEN** the daemon starts with `XDG_RUNTIME_DIR=/run/user/1000`
- **THEN** a socket exists at `/run/user/1000/woluma.sock` with mode `0600`

#### Scenario: Missing runtime dir
- **WHEN** the daemon starts with `XDG_RUNTIME_DIR` unset
- **THEN** it exits non-zero with an error naming `XDG_RUNTIME_DIR`

### Requirement: Single daemon instance
Only one daemon SHALL run per user session. A second daemon MUST detect that the first holds the instance lock or answers on the socket, and exit non-zero without disturbing it. A stale socket file left by a crashed daemon SHALL be replaced.

#### Scenario: Second daemon refused
- **WHEN** a daemon is running and another `woluma daemon` is started
- **THEN** the second exits non-zero with "another woluma daemon is already running" and the first keeps serving

#### Scenario: Stale socket replaced
- **WHEN** a socket file exists but no process is listening on it
- **THEN** a newly started daemon removes it and binds a new socket

### Requirement: Socket self-healing
The daemon SHALL periodically check that its socket file still exists and is the one it created. If the file was removed or replaced, the daemon SHALL rebind it.

#### Scenario: Socket deleted at runtime
- **WHEN** the socket file is deleted while the daemon runs
- **THEN** within the check interval the daemon recreates it and clients can connect again

### Requirement: Line-delimited JSON protocol
Each connection SHALL carry newline-delimited JSON. The client sends exactly one request object. For one-shot requests the daemon replies with exactly one response object, then closes the connection. Every response SHALL contain `"ok": true` with a result, or `"ok": false` with an `"error"` string. Malformed or unknown requests SHALL receive an `ok: false` response and MUST NOT affect daemon state.

#### Scenario: Valid one-shot request
- **WHEN** a client sends `{"cmd":"status"}\n`
- **THEN** the daemon replies with one line `{"ok":true,"status":{...}}` and closes the connection

#### Scenario: Unknown command
- **WHEN** a client sends `{"cmd":"explode"}\n`
- **THEN** the daemon replies `{"ok":false,"error":"unknown command: explode"}` and its state is unchanged

### Requirement: Targets
Requests that act on outputs (`toggle`, `pause`, `resume`) SHALL take a `target` that is either a configured output name or `"*"` for all outputs. An unknown output name SHALL produce an error response that lists the valid names.

#### Scenario: Global target
- **WHEN** a client sends `{"cmd":"pause","target":"*"}`
- **THEN** every configured output becomes paused

#### Scenario: Unknown output
- **WHEN** a client sends `{"cmd":"toggle","target":"HDMI-9"}` and only `eDP-1` and `DP-1` are configured
- **THEN** the response is `ok: false` with an error naming `HDMI-9` and listing `eDP-1, DP-1`

### Requirement: Toggle semantics
`toggle` SHALL flip the enabled state of each targeted output. For `*`, it SHALL disable all outputs if any is enabled, and otherwise enable all. A disabled output MUST NOT have its brightness written, MUST NOT run hooks, and MUST NOT be paused by manual brightness changes. Enabling an output SHALL clear any pause on it, re-evaluate its rules immediately (without waiting for settle), apply the matched rule's brightness, and run that rule's `run` hooks. The response SHALL report the resulting enabled state per output.

#### Scenario: Disable leaves brightness alone
- **WHEN** eDP-1 is at 40% under rule `bright` and the user toggles eDP-1 off
- **THEN** brightness stays at 40% and no hooks run

#### Scenario: Enable applies current rule now
- **WHEN** eDP-1 is disabled, current luma matches rule `default` (brightness 70), and the user toggles eDP-1 on
- **THEN** brightness transitions to 70% right away and `default`'s `run` commands execute once

#### Scenario: Global toggle with mixed state
- **WHEN** eDP-1 is enabled and DP-1 is disabled, and the user toggles `*`
- **THEN** both outputs end up disabled

### Requirement: Manual pause and resume
`pause` SHALL put targeted outputs into a paused state with reason `manual`. With an optional `duration` (seconds), the pause expires on its own; without one, it lasts until resumed. A paused output MUST NOT have its brightness written and MUST NOT run hooks, but SHALL keep tracking inputs and the matched rule. `resume` SHALL clear any pause (reason `manual` or `manual-override`), re-evaluate immediately, apply the matched rule's brightness, and run its hooks.

#### Scenario: Timed pause expires
- **WHEN** the user pauses eDP-1 for 600 seconds
- **THEN** eDP-1 reports `paused` with reason `manual` and an `until` timestamp, and after 600 seconds it returns to `active` following the manual-override expiry semantics

#### Scenario: Resume after manual-override
- **WHEN** eDP-1 is paused because the user changed brightness by hand, and the user runs resume on eDP-1
- **THEN** the pause clears and the current rule's brightness is applied immediately

### Requirement: Status snapshot
The `status` request SHALL return a snapshot with: daemon version, ALS state (source, value, or `null` when unknown or not configured), local time, and for each output: name, enabled, luma (or `null` before the first sample), matched rule name (or `null`), brightness percent, state (`active`, `settling`, `paused`, `disabled`), pause reason and `until` when paused, and settle remaining in milliseconds when settling.

#### Scenario: Snapshot while settling
- **WHEN** eDP-1's luma just crossed into a new rule's range and the 2 s settle window has 1.2 s left
- **THEN** the status for eDP-1 has `"state":"settling"` and `"settle_remaining_ms":1200`

#### Scenario: Unknown ALS
- **WHEN** ALS is configured but the camera is disabled
- **THEN** the status has `"als":{"source":"webcam","value":null}`

### Requirement: Subscribe stream
A `{"cmd":"subscribe"}` request SHALL keep the connection open. The daemon SHALL first send the current full snapshot, then one snapshot line whenever any field changes: a new luma or ALS sample, a rule change, a brightness change, or a state change. A slow subscriber MUST NOT block the daemon; stale queued updates for that subscriber MAY be dropped in favor of the newest. The connection SHALL close when the client disconnects or the daemon shuts down.

#### Scenario: Live updates
- **WHEN** a client subscribes and eDP-1's luma changes from 18 to 88
- **THEN** the client receives the initial snapshot followed by a snapshot with eDP-1 luma 88

#### Scenario: Slow client does not stall daemon
- **WHEN** a subscriber stops reading for 60 seconds
- **THEN** brightness control and other clients keep working, and the subscriber later receives the latest snapshot rather than the full backlog
