# Nix packages for Marathon: the desktop player and the server, plus a
# NixOS module for the server. See docs/PACKAGING.md.
#
#   nix build .#marathon-desktop     # the player (default package)
#   nix build .#marathon-desktop-cec # the player with HDMI-CEC (GPL)
#   nix build .#marathond            # the server
#   nix run .# -- --server http://tv-server:7878
#
# `marathond` and `marathon-desktop` are wrappers configured with typed
# settings, e.g. `marathond.override { settings = { ... }; }`; the
# `-unwrapped` packages are the plain builds (nix/overlay.nix).
#
# NixOS: import `inputs.marathon.nixosModules.default` and set
# `services.marathond` (nix/module.nix documents the options). Or add
# `inputs.marathon.overlays.default` to nixpkgs for the packages.
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
          # The overlay's packages, without instantiating nixpkgs again.
          scope = lib.makeScope nixpkgs.legacyPackages.${system}.newScope (
            final: import ./nix/overlay.nix final nixpkgs.legacyPackages.${system}
          );
        in
        lib.optionalAttrs (lib.elem system linux) {
          inherit (scope)
            marathon-desktop
            marathon-desktop-unwrapped
            marathon-desktop-cec
            marathon-desktop-cec-unwrapped
            ;
          default = scope.marathon-desktop;
        }
        // lib.optionalAttrs (lib.elem system (linux ++ darwin)) {
          inherit (scope) marathond marathond-unwrapped;
        }
      );

      overlays.default = import ./nix/overlay.nix;

      nixosModules.default =
        { pkgs, ... }:
        {
          imports = [ ./nix/module.nix ];
          services.marathond.package =
            let
              system = pkgs.stdenv.hostPlatform.system;
            in
            lib.mkDefault (
              self.packages.${system}.marathond-unwrapped
                or (throw "Marathon's flake has no marathond for ${system}; set services.marathond.package")
            );
        };

      # `nix flake check` runs the module's VM test and the wrapper checks
      # (Linux only).
      checks = forAll linux (pkgs: {
        module = import ./nix/module-test.nix {
          inherit pkgs;
          module = self.nixosModules.default;
        };
        wrappers = import ./nix/wrappers-test.nix {
          inherit pkgs;
          inherit (self.packages.${pkgs.stdenv.hostPlatform.system}) marathond marathon-desktop-cec;
        };
      });

      formatter = forAll (linux ++ darwin) (pkgs: pkgs.nixfmt);
    };
}
