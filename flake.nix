{
  description = "woluma: rule-based screen-content aware brightness daemon for Wayland";

  inputs.nixpkgs.url = "github:nixos/nixpkgs?rev=0ad6f47ea4fe188f4bc8f0380f93ae8523337c6c";
  inputs.crane.url = "github:ipetkov/crane";
  inputs.home-manager = {
    url = "github:nix-community/home-manager";
    inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { self, nixpkgs, crane, home-manager }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      rustFor = pkgs: import ./nix/rust.nix { inherit pkgs crane; };
      wolumaFor = pkgs: pkgs.callPackage ./nix/package.nix { rust = rustFor pkgs; };
    in {
      packages = forAllSystems (system:
        let woluma = wolumaFor nixpkgs.legacyPackages.${system};
        in { inherit woluma; default = woluma; });

      overlays.default = final: prev: { woluma = wolumaFor final; };

      homeManagerModules.woluma = import ./nix/hm-module.nix { inherit self; };
      homeManagerModules.default = self.homeManagerModules.woluma;

      nixosModules.woluma = import ./nix/nixos-module.nix;
      nixosModules.default = self.nixosModules.woluma;

      checks = forAllSystems (system:
        import ./nix/checks.nix {
          inherit self nixpkgs home-manager;
          pkgs = nixpkgs.legacyPackages.${system};
          rust = rustFor nixpkgs.legacyPackages.${system};
        });

      devShells = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          rust = rustFor pkgs;
        in {
          default = pkgs.mkShell {
            packages = with pkgs; [ cargo rustc clippy rustfmt rust-analyzer ]
              ++ rust.commonArgs.nativeBuildInputs ++ rust.commonArgs.buildInputs;
            # Wayland and Vulkan are dlopen'ed; the package uses an RPATH instead.
            LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath rust.runtimeLibs;
          };
        });
    };
}
