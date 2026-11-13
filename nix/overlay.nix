# Marathon's packages as a nixpkgs overlay (the flake's
# `overlays.default`; ../flake.nix builds its `packages` from it too).
#
# Each program comes in two variants:
#
# - `marathond`, `marathon-desktop`, `marathon-desktop-cec`: wrappers you
#   configure with `.override { settings = ...; }` (see
#   ./marathond-wrapped.nix and ./marathon-desktop-wrapped.nix). Without
#   settings they behave like the unwrapped programs.
# - `marathond-unwrapped`, `marathon-desktop-unwrapped`,
#   `marathon-desktop-cec-unwrapped`: the plain builds, for wrapping
#   differently or for the NixOS module, which writes its own config.
final: prev: {
  marathond-unwrapped = final.callPackage ./marathond.nix { };
  marathond = final.callPackage ./marathond-wrapped.nix { };

  marathon-desktop-unwrapped = final.callPackage ./marathon-desktop.nix { };
  marathon-desktop = final.callPackage ./marathon-desktop-wrapped.nix { };

  # With HDMI-CEC; libcec is GPL-2.0-or-later, so these builds are GPL.
  marathon-desktop-cec-unwrapped = final.marathon-desktop-unwrapped.override { withCec = true; };
  marathon-desktop-cec = final.marathon-desktop.override {
    marathon-desktop-unwrapped = final.marathon-desktop-cec-unwrapped;
  };
}
