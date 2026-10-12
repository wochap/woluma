# Nix Packaging Specification

## Purpose

Defines how woluma is packaged and deployed with Nix: the flake outputs (package, overlay, home-manager and NixOS modules, checks, dev shell), runtime library bundling without leaking environment to hooks, optional shell completions, the home-manager user service with TOML config rendering and build-time validation, NixOS hardware permissions for backlight and DDC, the `nix flake check` gate, and the development shell.

## Requirements

### Requirement: Flake exposes package, overlay, modules, checks and dev shell
The repository SHALL provide a `flake.nix` exposing, for each supported system (`x86_64-linux`, `aarch64-linux`):
- `packages.<system>.woluma`
- `packages.<system>.default`, which is the same derivation as `woluma`
- `checks.<system>`
- `devShells.<system>.default`

It SHALL also expose these outputs, which are not per system:
- `overlays.default`
- `homeManagerModules.woluma`, with `homeManagerModules.default` as an alias
- `nixosModules.woluma`, with `nixosModules.default` as an alias

#### Scenario: Default package builds
- **WHEN** a user runs `nix build .#default`
- **THEN** the result contains an executable `bin/woluma`

#### Scenario: Overlay provides the package
- **WHEN** a nixpkgs instance is created with `overlays.default` applied
- **THEN** `pkgs.woluma` evaluates to a derivation providing `bin/woluma`

#### Scenario: Module aliases point to the same module
- **WHEN** a configuration imports `homeManagerModules.default` or `nixosModules.default`
- **THEN** the same options are available as when importing `homeManagerModules.woluma` or `nixosModules.woluma`, respectively

### Requirement: Package bundles runtime libraries without leaking environment to hooks
The `woluma` package SHALL link every native library the binary loads at runtime, including libraries opened with `dlopen` such as the Wayland client and the Vulkan loader, so the binary starts without the user setting `LD_LIBRARY_PATH`. The package SHALL NOT wrap the binary in a way that injects `LD_LIBRARY_PATH` or other library-path variables into the environment of hook commands the daemon spawns.

#### Scenario: Binary starts without extra environment
- **WHEN** `bin/woluma --version` is run from the built package with an empty `LD_LIBRARY_PATH`
- **THEN** it exits successfully and prints the version

#### Scenario: Hooks inherit no library path
- **WHEN** the packaged daemon runs a hook command
- **THEN** the hook's environment contains no `LD_LIBRARY_PATH` added by the package

### Requirement: Package installs zsh completions optionally
The `woluma` package SHALL install the zsh completion produced by the zsh-completions capability as `share/zsh/site-functions/_woluma`. The package SHALL accept a `withShellCompletions` argument, defaulting to `true`. When it is `false`, the package SHALL install no `share/` completion files and SHALL still build and provide `bin/woluma`.

#### Scenario: Completions installed by default
- **WHEN** the default package is built
- **THEN** `share/zsh/site-functions/_woluma` exists in the output

#### Scenario: Completions disabled
- **WHEN** the package is built with `withShellCompletions = false`
- **THEN** the output contains `bin/woluma` and no `share/zsh/site-functions/_woluma`

### Requirement: Home-manager module renders TOML config from settings
The home-manager module SHALL provide `services.woluma.enable` and a freeform `services.woluma.settings` option typed with `pkgs.formats.toml`. When enabled and `settings` is non-empty, the module SHALL write the rendered TOML to `$XDG_CONFIG_HOME/woluma/config.toml`. When `settings` is empty, the module SHALL NOT manage that file, so a hand-written config is left untouched.

#### Scenario: Settings rendered to config file
- **WHEN** `services.woluma.settings.general.settle = "2s"` is set
- **THEN** the generated `~/.config/woluma/config.toml` contains `settle = "2s"` under `[general]`

#### Scenario: Empty settings leave config unmanaged
- **WHEN** `services.woluma.enable = true` and `settings` is left at its default empty value
- **THEN** home-manager does not create or manage `~/.config/woluma/config.toml`

### Requirement: Home-manager module validates config at build time
When `settings` is non-empty and `services.woluma.checkConfig` is `true` (the default), the module SHALL validate the rendered config at build time using the package's config-only validation command, which needs no running daemon and no access to display, backlight or camera hardware. An invalid config SHALL fail the home-manager build with the validator's error message.

#### Scenario: Invalid rule fails the build
- **WHEN** `settings` contains a rule with `when = { luma = "70" }` (no operator)
- **THEN** building the home-manager generation fails and the error names the invalid condition

#### Scenario: Validation can be disabled
- **WHEN** `services.woluma.checkConfig = false` and `settings` contains an invalid rule
- **THEN** the home-manager build succeeds and writes the config unchanged

### Requirement: Home-manager module runs a graphical-session user service
When enabled, the home-manager module SHALL define a `woluma` systemd user service that:
- runs `<package>/bin/woluma` in daemon mode;
- is `PartOf` and `After` the target named by `services.woluma.systemdTarget` (default `graphical-session.target`), and is `WantedBy` that same target;
- restarts on failure, with a delay;
- restarts when the generated config file changes.

The module SHALL add `services.woluma.package` to `home.packages`, so the CLI subcommands (`watch`, `check`, `toggle`, …) are on the user's `PATH`.

#### Scenario: Service bound to graphical session
- **WHEN** `services.woluma.enable = true`
- **THEN** the generated `~/.config/systemd/user/woluma.service` contains `PartOf=graphical-session.target` and `WantedBy=graphical-session.target`

#### Scenario: Config change restarts service
- **WHEN** `services.woluma.settings` changes and the new home-manager generation is activated
- **THEN** the `woluma` user service is restarted, because the rendered config's store path appears in the unit's `X-Restart-Triggers`

#### Scenario: CLI on PATH
- **WHEN** `services.woluma.enable = true`
- **THEN** `woluma` is available in the user's profile

### Requirement: Hook commands find user-specified tools
The home-manager module SHALL provide `services.woluma.extraPackages` (a list of packages, default empty). The `bin` directories of these packages SHALL be prepended to the service's `PATH`, so hook commands in `run` lists can call tools such as `notify-send` or `brightnessctl` by bare name.

#### Scenario: Hook tool resolved from extraPackages
- **WHEN** `services.woluma.extraPackages = [ pkgs.libnotify ]` and a rule has `run = ["notify-send woluma test"]`
- **THEN** the service unit's `PATH` includes `${pkgs.libnotify}/bin`

### Requirement: Home-manager completions toggle
The home-manager module SHALL provide `services.woluma.shellCompletions.enable` (default `true`). When it is `false`, the package installed into `home.packages` SHALL omit shell completion files, while the service SHALL keep using the full package.

#### Scenario: Completions omitted from profile
- **WHEN** `services.woluma.shellCompletions.enable = false`
- **THEN** the home profile contains `bin/woluma` but no `share/zsh/site-functions/_woluma` from woluma

### Requirement: NixOS module grants hardware permissions
The NixOS module SHALL provide these `services.woluma` options:
- `backlight.enable` (default `true`): installs udev rules so that the `brightness` file of every `backlight` and `leds` device is group-owned by `video` and group-writable. Commands are referenced by absolute Nix store paths, not `/bin`.
- `ddc.enable` (default `false`): enables `hardware.i2c`, which loads `i2c-dev` and creates the `i2c` group.
- `users` (list of user names, default empty): adds each listed user to `video`, and also to `i2c` when `ddc.enable` is `true`.

#### Scenario: Backlight rules installed
- **WHEN** the NixOS module is imported with default options
- **THEN** `services.udev.packages` or `services.udev.extraRules` contains rules for `SUBSYSTEM=="backlight"` and `SUBSYSTEM=="leds"` that set group `video` and group-write on `brightness`, invoking `chgrp` and `chmod` from a Nix store path

#### Scenario: DDC enables i2c
- **WHEN** `services.woluma.ddc.enable = true`
- **THEN** `hardware.i2c.enable` is `true` in the evaluated system

#### Scenario: Users added to groups
- **WHEN** `services.woluma.users = [ "gean" ]` and `ddc.enable = true`
- **THEN** `users.users.gean.extraGroups` contains `video` and `i2c`

#### Scenario: Backlight rules disabled
- **WHEN** `services.woluma.backlight.enable = false`
- **THEN** no woluma backlight udev rules are present in the evaluated system

### Requirement: Flake check gate
`nix flake check` SHALL build and pass all of these checks:
- the `woluma` package;
- `cargo fmt --check`;
- `cargo clippy --all-targets -- -D warnings`;
- `cargo test`;
- a home-manager evaluation that enables `services.woluma` with non-empty `settings` and `extraPackages`, then asserts the service unit and config file exist with the expected contents;
- a NixOS evaluation that asserts the udev rules, the `i2c` and group behavior;
- a package variant built with `withShellCompletions = false`, asserting that no completion files are present.

#### Scenario: All checks pass on a clean tree
- **WHEN** `nix flake check` is run on a tree where formatting, lint and tests are clean
- **THEN** the command succeeds

#### Scenario: Lint warning fails the gate
- **WHEN** the Rust code introduces a clippy warning
- **THEN** `nix flake check` fails on the clippy check

### Requirement: Development shell
`devShells.<system>.default` SHALL provide the Rust toolchain (`cargo`, `rustc`, `clippy`, `rustfmt`, `rust-analyzer`), `pkg-config`, and every native library and build tool the package needs (including the bindgen/libclang setup if any dependency uses bindgen). This SHALL be enough for `cargo build` and `cargo test` to succeed inside the shell with no other setup. It SHALL also make runtime-`dlopen`ed libraries (Wayland client, Vulkan loader) loadable, so `cargo run` works from the shell.

#### Scenario: Cargo builds in the dev shell
- **WHEN** a developer runs `nix develop` and then `cargo build`
- **THEN** the build succeeds

#### Scenario: Cargo run can open Wayland
- **WHEN** a developer runs `cargo run -- watch --raw` from the dev shell inside a Wayland session
- **THEN** the binary loads the Wayland client library without a "library not found" error
