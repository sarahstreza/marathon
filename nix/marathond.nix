# The Marathon server, marathond. Called from ../flake.nix; run it as a
# service with the NixOS module in ./module.nix.
{
  lib,
  rustPlatform,
  pkg-config,
  aws-lc,
  sqlite,
}:

let
  workspace = (lib.importTOML ../Cargo.toml).workspace.package;
in
rustPlatform.buildRustPackage {
  pname = "marathond";
  inherit (workspace) version;

  # marathond's tests also read the example config and the Android app's
  # OpenAPI snapshot, so those come along.
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../crates
      ../apps
      ../config/example.toml
      ../android/api/openapi.json
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;

  cargoBuildFlags = [
    "-p"
    "marathond"
  ];
  cargoTestFlags = [
    "-p"
    "marathond"
  ];

  # aws-lc (rustls' crypto, for the Jellyfin client) and SQLite (sqlx):
  # the workspace bundles both, but nixpkgs' cargo hook makes their -sys
  # crates link the system copies instead.
  nativeBuildInputs = [ pkg-config ];
  buildInputs = [
    aws-lc
    sqlite
  ];

  meta = {
    description = "Watch your video collection as TV channels (server)";
    homepage = "https://github.com/sarahstreza/marathon";
    license = [
      lib.licenses.mit
      lib.licenses.asl20
    ];
    mainProgram = "marathond";
    platforms = lib.platforms.unix;
  };
}
