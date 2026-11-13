# Nix packages for Marathon: the desktop player and the server, plus a
# NixOS module for the server. See docs/PACKAGING.md.
#
#   nix build .#marathon-desktop     # the player (default package)
#   nix build .#marathon-desktop-cec # the player with HDMI-CEC (GPL)
#   nix build .#marathond            # the server
#   nix run .# -- --server http://tv-server:7878
#
# NixOS: import `inputs.marathon.nixosModules.default` and set
# `services.marathond` (nix/module.nix documents the options).
{
  description = "Marathon: watch your video collection as TV channels";

  # nixos-unstable: the workspace needs Rust 1.95, newer than the stable
  # NixOS release's.
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      lib = nixpkgs.lib;
      # The player is Linux-only here (macOS is milestone M9); the server
      # also builds on Apple Silicon Macs. Not on Intel Macs: nixpkgs 26.11
      # dropped x86_64-darwin.
      linux = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      darwin = [ "aarch64-darwin" ];
      forAll = systems: f: lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages =
        lib.recursiveUpdate
          (forAll linux (pkgs: rec {
            marathon-desktop = pkgs.callPackage ./nix/marathon-desktop.nix { };
            marathon-desktop-cec = marathon-desktop.override { withCec = true; };
            default = marathon-desktop;
          }))
          (
            forAll (linux ++ darwin) (pkgs: {
              marathond = pkgs.callPackage ./nix/marathond.nix { };
            })
          );

      nixosModules.default =
        { pkgs, ... }:
        {
          imports = [ ./nix/module.nix ];
          services.marathond.package =
            lib.mkDefault
              self.packages.${pkgs.stdenv.hostPlatform.system}.marathond;
        };

      # `nix flake check` runs the module's VM test (Linux only).
      checks = forAll linux (pkgs: {
        module = import ./nix/module-test.nix {
          inherit pkgs;
          module = self.nixosModules.default;
        };
      });

      formatter = forAll (linux ++ darwin) (pkgs: pkgs.nixfmt);
    };
}
