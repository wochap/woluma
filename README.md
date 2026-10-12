# woluma

A rule-based, screen-content aware brightness daemon for Wayland. Instead of learning
from manual adjustments (like [wluma](https://github.com/max-baz/wluma)), woluma applies
rules you declare: per output, the first rule whose conditions match the screen's
whiteness (`luma`), ambient light (`als`) and local time becomes active, and its hooks run.

Requires a compositor with `ext-image-copy-capture-v1` or `wlr-screencopy-unstable-v1`
and a Vulkan-capable GPU.

## Config

`$XDG_CONFIG_HOME/woluma/config.toml` (or `--config <path>`):

```toml
[general]
settle = "2s"
transition = "400ms"                 # ramp duration, "0s" for instant
manual_override = "until-rule-change" # or a duration like "15m"

[hysteresis]
luma = 5

[outputs.eDP-1]
backlight = "intel_backlight"

[[outputs.eDP-1.rules]]
name = "night-bright"
when = { time = "21:00-07:00", luma = ">= 70" }
brightness = 30
run = ["notify-send \"$WOLUMA_RULE on $WOLUMA_OUTPUT\""]

[[outputs.eDP-1.rules]]
name = "bright"
when = { luma = ">= 70" }
brightness = 40

[[outputs.eDP-1.rules]]
name = "default"
brightness = 70
```

Hooks get `WOLUMA_OUTPUT`, `WOLUMA_RULE`, `WOLUMA_PREV_RULE`, `WOLUMA_LUMA`, `WOLUMA_ALS`,
`WOLUMA_TIME` and `WOLUMA_BRIGHTNESS`.

## Ambient light

`als` conditions compare against one shared 0-100 room-light reading. Without an `[als]`
section (or with `source = "none"`) the reading is always unknown, no device is touched,
and `als` conditions never match.

```toml
[als]
source = "webcam"                  # "webcam", "iio" or "none"
device = "/dev/v4l/by-id/usb-...-video-index0" # default "/dev/video0"
interval = "60s"                   # sampling period (default 60s)
exposure = 600                     # exposure_time_absolute while sampling (required)
frames = 10                        # frames averaged per sample, 1-60 (default 10)
```

A sample is also taken at startup.

### Webcam

Each sample opens the camera, saves `auto_exposure` and `exposure_time_absolute`, switches
to manual exposure at `exposure`, averages the luma (Y) of `frames` YUYV (or GREY) frames at
the smallest resolution, restores the saved controls and closes the camera. Exposure is
locked because auto-exposure pulls every scene toward mid-grey, which hides room light.
Limited-range Y is rescaled so 16 (black) is 0 and 235 (white) is 100.

Tune `exposure` so a normal room reads around 30-60: watch the logged readings
(`RUST_LOG=woluma=debug`, or `woluma watch --raw` once available) and raise `exposure` if
the value is too low, lower it if it saturates. Values outside the camera's range are
clamped with a warning.

The camera LED lights for under a second per sample, and the camera is briefly in use
every `interval`. Use `source = "none"` if that is unwanted. Device numbers can change
across boots, so prefer a stable `/dev/v4l/by-id/...` path. Controls are restored on
errors and shutdown. If the daemon is killed mid-sample, the saved values in
`$XDG_RUNTIME_DIR/woluma/webcam-controls` are restored on the next sample.

### IIO sensor

`source = "iio"` reads `in_illuminance*` from the first matching device under
`/sys/bus/iio/devices` (or the sysfs directory given as `device`) and maps lux on a log
scale: 1 lux is 8, 100 lux is 50, 10 000 lux and above is 100. Webcam and IIO values are
not comparable to each other.

### Unknown readings

When no value can be measured, the reading is unknown (never a stale value), `als`
conditions don't match, and hooks get an empty `WOLUMA_ALS`. The reason is logged:

| Reason | Meaning |
|---|---|
| `missing` | The device or sensor doesn't exist |
| `busy` | Another application holds the camera (e.g. a video call) |
| `error` | Opening, configuring, capturing or reading failed |
| `placeholder` | The frames look synthetic (camera privacy switch off): byte-identical frames or a flat image |
| `unsupported-format` | The camera offers neither YUYV nor GREY |
| `disabled` | `source = "none"` |

## Brightness

A rule's `brightness` (0-100 %) is applied to the output, ramping over `transition`.
Backlights never go below raw value 1, so 0 % keeps the panel lit. External monitors use
DDC/CI (VCP 0x10):

```toml
[outputs.DP-1]
ddcutil = "DELL U2720Q" # EDID model name (case-insensitive substring) or serial number
poll = "5s"             # how often to look for OSD changes; "0" turns detection off
```

Set `poll = "0"` for monitors that report stale or jittery values after writes and keep
getting paused for no reason.

### Manual override

Any brightness change woluma didn't make (a brightness key, the monitor OSD) pauses that
output: no brightness writes and no `run` hooks. With `manual_override = "until-rule-change"`
the output resumes, and applies the new rule, when the active rule changes. With a duration
it resumes quietly when the time runs out and acts again at the next rule change. Another
adjustment during a pause restarts it.

Changes made by a rule's own `run` scripts are not manual: once the scripts exit (plus 1 s,
or after 30 s at most), woluma takes the current level as its own.

### Permissions

- **Backlight**: the daemon writes `/sys/class/backlight/<name>/brightness`. Without write
  access it falls back to logind's `SetBrightness`, which works from a graphical login
  session. For direct writes, join the `video` group with a udev rule such as:

  ```
  ACTION=="add", SUBSYSTEM=="backlight", RUN+="/bin/chgrp video /sys/class/backlight/%k/brightness", RUN+="/bin/chmod g+w /sys/class/backlight/%k/brightness"
  ```

- **DDC/CI**: load the `i2c-dev` kernel module (`modprobe i2c-dev`, or add it to
  `/etc/modules-load.d/`) and give your user access to `/dev/i2c-*` (often the `i2c` group).

## License

ISC. Screen capture code is adapted from wluma; see `NOTICE`.
