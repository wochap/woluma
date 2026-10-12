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
`WOLUMA_TIME` and `WOLUMA_BRIGHTNESS`. Writing brightness to hardware is not implemented yet.

## License

ISC. Screen capture code is adapted from wluma; see `NOTICE`.
