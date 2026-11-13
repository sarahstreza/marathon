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
      # Where each package is built and tested. The player is Linux-only
      # (macOS is milestone M9); the server also builds on Apple Silicon
      # Macs. Not on Intel Macs: nixpkgs 26.11 dropped x86_64-darwin.
      linux = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      darwin = [ "aarch64-darwin" ];
      forAll = systems: f: lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      # Defined for every system flakes expose (i686-linux, riscv64-linux,
      # ...), holding only the packages supported there, so a flake that
      # walks all systems (a NUR, say) can check
      # `marathon.packages.${system} ? marathond` instead of failing on a
      # missing attribute. Only the attribute set is per-system; nothing
      # outside `linux`/`darwin` is evaluated or built.
      packages = lib.genAttrs lib.systems.flakeExposed (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        lib.optionalAttrs (lib.elem system linux) rec {
          marathon-desktop = pkgs.callPackage ./nix/marathon-desktop.nix { };
          marathon-desktop-cec = marathon-desktop.override { withCec = true; };
          default = marathon-desktop;
        }
        // lib.optionalAttrs (lib.elem system (linux ++ darwin)) {
          marathond = pkgs.callPackage ./nix/marathond.nix { };
        }
      );

      nixosModules.default =
        { pkgs, ... }:
        {
          imports = [ ./nix/module.nix ];
          services.marathond.package =
            let
              system = pkgs.stdenv.hostPlatform.system;
            in
            lib.mkDefault (
              self.packages.${system}.marathond
                or (throw "Marathon's flake has no marathond for ${system}; set services.marathond.package")
            );
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
