# NixOS module: hardware permissions for woluma (backlight udev rules, DDC/i2c,
# groups). The daemon itself lives in the home-manager module.
{ config, lib, pkgs, ... }:
let
  cfg = config.services.woluma;
  rule = subsystem: dir:
    ''ACTION=="add", SUBSYSTEM=="${subsystem}", RUN+="${pkgs.coreutils}/bin/chgrp video /sys/class/${dir}/%k/brightness", RUN+="${pkgs.coreutils}/bin/chmod g+w /sys/class/${dir}/%k/brightness"'';
  backlightRules = pkgs.writeTextDir "lib/udev/rules.d/90-woluma-backlight.rules" ''
    ${rule "backlight" "backlight"}
    ${rule "leds" "leds"}
  '';
in
{
  options.services.woluma = {
    backlight.enable = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Install udev rules making every backlight and LED `brightness` file group-owned by
        `video` and group-writable. Rules apply on device add: run
        `udevadm trigger --subsystem-match=backlight --subsystem-match=leds` or reboot after
        the first switch.
      '';
    };
    ddc.enable = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Enable `hardware.i2c` (loads i2c-dev, creates the `i2c` group) for DDC/CI monitors.";
    };
    users = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "alice" ];
      description = ''
        Users added to `video` (and `i2c` when DDC is enabled). Group changes take effect at
        the next login; udev rules need `udevadm trigger` or a reboot after the first switch.
      '';
    };
  };

  config = lib.mkMerge [
    (lib.mkIf cfg.backlight.enable { services.udev.packages = [ backlightRules ]; })
    (lib.mkIf cfg.ddc.enable { hardware.i2c.enable = true; })
    {
      users.users = lib.genAttrs cfg.users (_: {
        extraGroups = [ "video" ] ++ lib.optional cfg.ddc.enable "i2c";
      });
    }
  ];
}
