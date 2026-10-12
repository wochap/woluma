# woluma package. Called with callPackage so overlay consumers can override inputs.
{ lib, rust, installShellFiles, patchelf
, withShellCompletions ? true }:
let
  inherit (rust) craneLib commonArgs cargoArtifacts runtimeLibs;
in
craneLib.buildPackage (commonArgs // {
  inherit cargoArtifacts;
  doCheck = false;
  nativeBuildInputs = commonArgs.nativeBuildInputs ++ [ installShellFiles patchelf ];
  postInstall = lib.optionalString withShellCompletions ''
    installShellCompletion --cmd woluma --zsh <($out/bin/woluma completions zsh)
  '';
  # RPATH instead of an LD_LIBRARY_PATH wrapper, which hook commands would inherit.
  postFixup = ''
    patchelf --add-rpath ${lib.makeLibraryPath runtimeLibs} $out/bin/woluma
  '';
  meta = {
    description = "Rule-based screen-content aware brightness daemon for Wayland";
    license = lib.licenses.isc;
    platforms = lib.platforms.linux;
    mainProgram = "woluma";
  };
})
