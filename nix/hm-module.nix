# Home-manager module: services.woluma (package, config and user service).
{ self }:
{ config, lib, pkgs, ... }:
let
  cfg = config.services.woluma;
  inherit (lib) mkEnableOption mkOption mkIf types;
  system = pkgs.stdenv.hostPlatform.system;
  tomlFormat = pkgs.formats.toml { };

  rendered = tomlFormat.generate "woluma-config.toml" cfg.settings;
  configFile =
    if cfg.checkConfig then
      pkgs.runCommand "woluma-config.toml" { } ''
        ${cfg.package}/bin/woluma check --offline --config ${rendered}
        cp ${rendered} $out
      ''
    else rendered;

  cliPackage =
    if cfg.shellCompletions.enable then cfg.package
    else pkgs.symlinkJoin {
      name = "woluma-bin-only";
      paths = [ cfg.package ];
      postBuild = "find $out -mindepth 1 -maxdepth 1 ! -name bin -exec rm -rf {} +";
    };
in
{
  options.services.woluma = {
    enable = mkEnableOption ''
      woluma, a screen-content aware brightness daemon. Screen capture uses Vulkan, so on
      NixOS enable `hardware.graphics.enable`; see also the woluma NixOS module for
      backlight and DDC permissions'';
    package = mkOption {
      type = types.package;
      default = self.packages.${system}.woluma;
      description = "Package providing bin/woluma.";
    };
    settings = mkOption {
      type = tomlFormat.type;
      default = { };
      example = lib.literalExpression ''
        {
          general.settle = "2s";
          outputs.eDP-1.rules = [
            { name = "bright"; when.luma = ">= 70"; brightness = 40; }
            { name = "default"; brightness = 70; }
          ];
        }
      '';
      description = ''
        Configuration written to $XDG_CONFIG_HOME/woluma/config.toml. When empty, the file is
        not managed, so a hand-written config is left alone.
      '';
    };
    checkConfig = mkOption {
      type = types.bool;
      default = true;
      description = "Validate the rendered config at build time with `woluma check --offline`.";
    };
    extraPackages = mkOption {
      type = types.listOf types.package;
      default = [ ];
      example = lib.literalExpression "[ pkgs.libnotify pkgs.brightnessctl ]";
      description = "Packages whose bin directories are prepended to the service PATH, for hook commands in `run`.";
    };
    systemdTarget = mkOption {
      type = types.str;
      default = "graphical-session.target";
      description = "systemd user target the service is bound to.";
    };
    shellCompletions.enable = mkOption {
      type = types.bool;
      default = true;
      description = "Install the zsh completion into the home profile.";
    };
  };

  config = mkIf cfg.enable {
    home.packages = [ cliPackage ];

    xdg.configFile."woluma/config.toml" = mkIf (cfg.settings != { }) { source = configFile; };

    systemd.user.services.woluma = {
      Unit = {
        Description = "woluma brightness daemon";
        PartOf = [ cfg.systemdTarget ];
        After = [ cfg.systemdTarget ];
        X-Restart-Triggers = lib.optional (cfg.settings != { }) "${configFile}";
      };
      Service = {
        ExecStart = "${cfg.package}/bin/woluma daemon";
        Restart = "always";
        RestartSec = 5;
        Environment = [
          "PATH=${lib.makeBinPath cfg.extraPackages}:${config.home.profileDirectory}/bin:/run/wrappers/bin:/etc/profiles/per-user/${config.home.username}/bin:/nix/var/nix/profiles/default/bin:/run/current-system/sw/bin"
        ];
      };
      Install.WantedBy = [ cfg.systemdTarget ];
    };
  };
}
