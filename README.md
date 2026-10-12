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
