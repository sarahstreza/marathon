# The Marathon desktop player, marathon-desktop. Called from ../flake.nix.
#
# libmpv is linked normally. The windowing libraries are not: winit and
# glutin dlopen libwayland, libxkbcommon, the X11 libraries, and libEGL at
# run time, so they are added to the binary's RUNPATH instead. On NixOS the
# GPU driver comes from /run/opengl-driver via libglvnd; on other distros
# run it through nixGL (docs/PACKAGING.md).
{
  lib,
  rustPlatform,
  pkg-config,
  cacert,
  aws-lc,
  sqlite,
  mpv-unwrapped,
  udev,
  libGL,
  libxkbcommon,
  wayland,
  libx11,
  libxcursor,
  libxi,
  libxrandr,
  libcec,
  libcec_platform,
  # HDMI-CEC input (--cec), the TV's remote. libcec is GPL-2.0-or-later,
  # which then covers the whole binary.
  withCec ? false,
}:

let
  workspace = (lib.importTOML ../Cargo.toml).workspace.package;
  dlopened = [
    libGL
    libxkbcommon
    wayland
    libx11
    libxcursor
    libxi
    libxrandr
  ];
in
rustPlatform.buildRustPackage {
  pname = "marathon-desktop";
  inherit (workspace) version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../crates
      ../apps
      ../packaging/linux
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;

  cargoBuildFlags = [
    "-p"
    "marathon-desktop"
  ];
  cargoTestFlags = [
    "-p"
    "marathon-desktop"
  ];
  buildFeatures = lib.optional withCec "input-cec";

  nativeBuildInputs = [ pkg-config ];
  # The client-server tests build a reqwest client, which loads the
  # system's root certificates and fails without any. The build sandbox
  # has none; cacert's setup hook points SSL_CERT_FILE at its bundle.
  nativeCheckInputs = [ cacert ];
  # aws-lc (rustls' crypto, via reqwest) and SQLite (sqlx, for embedded
  # mode): the workspace bundles both, but nixpkgs' cargo hook makes their
  # -sys crates link the system copies instead.
  buildInputs = [
    aws-lc
    sqlite
    mpv-unwrapped
    udev
  ]
  ++ lib.optionals withCec [
    libcec
    libcec_platform
  ];

  postInstall = ''
    id=io.github.sarahstreza.Marathon
    install -Dm644 packaging/linux/$id.desktop -t $out/share/applications
    install -Dm644 packaging/linux/$id.metainfo.xml -t $out/share/metainfo
    install -Dm644 packaging/linux/$id.svg -t $out/share/icons/hicolor/scalable/apps
  '';

  postFixup = ''
    patchelf --add-rpath ${lib.makeLibraryPath dlopened} $out/bin/marathon-desktop
  '';

  # Read by ./marathon-desktop-wrapped.nix to allow the `cec` options.
  passthru = { inherit withCec; };

  meta = {
    description = "Watch your video collection as TV channels (desktop player)";
    homepage = "https://github.com/sarahstreza/marathon";
    license =
      if withCec then
        lib.licenses.gpl2Plus
      else
        [
          lib.licenses.mit
          lib.licenses.asl20
        ];
    mainProgram = "marathon-desktop";
    platforms = lib.platforms.linux;
  };
}
