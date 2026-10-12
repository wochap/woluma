# `nix flake check` gate: Rust fmt/clippy/tests via crane, the package and its
# variants, a home-manager evaluation and a NixOS evaluation of the modules.
{ self, nixpkgs, pkgs, rust, home-manager }:
let
  inherit (rust) craneLib commonArgs cargoArtifacts;
  inherit (pkgs) lib;
  system = pkgs.stdenv.hostPlatform.system;
  woluma = self.packages.${system}.woluma;

  hmWith = woluma: (home-manager.lib.homeManagerConfiguration {
    inherit pkgs;
    modules = [
      self.homeManagerModules.woluma
      {
        home = { username = "test"; homeDirectory = "/home/test"; stateVersion = "25.05"; };
        services.woluma = { enable = true; } // woluma;
      }
    ];
  }).config;
  hm = hmWith {
    extraPackages = [ pkgs.libnotify ];
    settings = {
      general.settle = "2s";
      outputs.eDP-1.backlight = "intel_backlight";
      outputs.eDP-1.rules = [
        { name = "bright"; when.luma = ">= 70"; brightness = 40; run = [ "notify-send woluma test" ]; }
        { name = "default"; brightness = 70; }
      ];
    };
  };
  invalid = checkConfig: hmWith {
    inherit checkConfig;
    settings.outputs.eDP-1.backlight = "intel_backlight";
    settings.outputs.eDP-1.rules = [ { name = "bad"; when.luma = "70"; brightness = 40; } ];
  };
  configOf = c: c.xdg.configFile."woluma/config.toml".source;

  nixos = extra: (nixpkgs.lib.nixosSystem {
    inherit system;
    modules = [
      self.nixosModules.woluma
      {
        boot.loader.grub.enable = false;
        fileSystems."/" = { device = "nodev"; fsType = "ext4"; };
        system.stateVersion = "25.05";
        users.users.gean.isNormalUser = true;
      }
      extra
    ];
  }).config;
  hasRules = c: builtins.any (p: lib.hasPrefix "90-woluma-backlight" (p.name or "")) c.services.udev.packages;
  base = nixos { };
  ddc = nixos { services.woluma = { ddc.enable = true; users = [ "gean" ]; }; };
  noBacklight = nixos { services.woluma.backlight.enable = false; };
  rulesFile = lib.findFirst (p: lib.hasPrefix "90-woluma-backlight" (p.name or "")) null base.services.udev.packages;
in
{
  inherit woluma;

  woluma-fmt = craneLib.cargoFmt { inherit (commonArgs) src pname version; };

  woluma-clippy = craneLib.cargoClippy (commonArgs // {
    inherit cargoArtifacts;
    cargoClippyExtraArgs = "--all-targets -- -D warnings";
  });

  woluma-test = craneLib.cargoTest (commonArgs // {
    inherit cargoArtifacts;
    preCheck = ''
      export XDG_RUNTIME_DIR=$TMPDIR
      export HOME=$TMPDIR
    '';
  });

  woluma-ldpath = pkgs.runCommand "woluma-ldpath" { } ''
    env -u LD_LIBRARY_PATH ${woluma}/bin/woluma --version
    test -e ${woluma}/share/zsh/site-functions/_woluma
    touch $out
  '';

  woluma-no-completions =
    let bare = pkgs.callPackage ./package.nix { inherit rust; withShellCompletions = false; };
    in pkgs.runCommand "woluma-no-completions" { } ''
      test -x ${bare}/bin/woluma
      test ! -e ${bare}/share/zsh/site-functions/_woluma
      touch $out
    '';

  hm-module-eval = pkgs.runCommand "hm-module-eval" { } ''
    files=${hm.home-files}
    unit=$files/.config/systemd/user/woluma.service
    test -e $unit
    grep -q 'PartOf=graphical-session.target' $unit
    grep -q 'WantedBy=graphical-session.target' $unit
    grep -q 'X-Restart-Triggers=.*woluma-config.toml' $unit
    grep -q 'ExecStart=.*/bin/woluma daemon' $unit
    grep -q 'PATH=${pkgs.libnotify}/bin' $unit
    grep -q 'settle = "2s"' $files/.config/woluma/config.toml
    grep -q 'luma = ">= 70"' $files/.config/woluma/config.toml
    touch $out
  '';

  hm-module-invalid-config = pkgs.runCommand "hm-module-invalid-config" { } ''
    log=${pkgs.testers.testBuildFailure (configOf (invalid true))}/testBuildFailure.log
    cat $log
    grep -qi 'luma' $log
    # checkConfig = false writes the file unchanged.
    grep -q 'luma = "70"' ${configOf (invalid false)}
    touch $out
  '';

  nixos-module-eval =
    assert hasRules base && !hasRules noBacklight;
    assert !base.hardware.i2c.enable && ddc.hardware.i2c.enable;
    assert builtins.elem "video" ddc.users.users.gean.extraGroups;
    assert builtins.elem "i2c" ddc.users.users.gean.extraGroups;
    assert !builtins.elem "video" base.users.users.gean.extraGroups;
    pkgs.runCommand "nixos-module-eval" { } ''
      rules=${rulesFile}/lib/udev/rules.d/90-woluma-backlight.rules
      grep -q 'SUBSYSTEM=="backlight", RUN+="${pkgs.coreutils}/bin/chgrp video' $rules
      grep -q 'SUBSYSTEM=="leds", RUN+="${pkgs.coreutils}/bin/chgrp video' $rules
      grep -q '${pkgs.coreutils}/bin/chmod g+w' $rules
      touch $out
    '';
}
