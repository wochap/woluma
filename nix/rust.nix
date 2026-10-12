# Shared crane setup: common args and dependency-only artifacts reused by
# the package and every check.
{ pkgs, crane }:
let
  craneLib = crane.mkLib pkgs;
  # Opened at runtime: wayland-backend's `dlopen` feature and ash's `loaded`.
  runtimeLibs = [ pkgs.wayland pkgs.vulkan-loader ];
  commonArgs = {
    src = pkgs.lib.fileset.toSource {
      root = ./..;
      fileset = pkgs.lib.fileset.unions [
        ../Cargo.toml
        ../Cargo.lock
        ../src
        ../tests
      ];
    };
    strictDeps = true;
    nativeBuildInputs = [ pkgs.pkg-config ];
    buildInputs = runtimeLibs;
  } // { inherit (craneLib.crateNameFromCargoToml { cargoToml = ../Cargo.toml; }) pname version; };
  cargoArtifacts = craneLib.buildDepsOnly commonArgs;
in
{
  inherit craneLib commonArgs cargoArtifacts runtimeLibs;
}
