# Packaging

How to build Marathon's Linux packages. The player (`marathon-desktop`)
has a Flatpak, an AppImage, and a Nix package. The server (`marathond`) has
a Nix package and a NixOS module, as well as the container image described
in the README ("Running the server in a container").

| | Player | Server |
|---|---|---|
| Flatpak | `packaging/flatpak/` | no (see below) |
| AppImage | `packaging/appimage/` | no (see below) |
| Nix | `nix build .#marathon-desktop` | `nix build .#marathond`, NixOS module |
| Container | | `Dockerfile` |

The server is left out of the Flatpak and the AppImage on purpose. Both
formats are for desktop apps a user launches. A long-running daemon fits
them badly: Flatpak has no way to run one as a service, and an AppImage of
a binary with no library dependencies beyond libc adds nothing. For a
server on a non-NixOS machine use the container image (or `cargo build -p
marathond`). For an all-in-one setup the packaged player can still run
its own server (embedded mode: `--embedded --config FILE`).

Files shared by the Flatpak and the AppImage (and installed by the Nix
package) are in `packaging/linux/`: the desktop entry, AppStream metainfo,
and icon, all named after the app ID `io.github.sarahstreza.Marathon`.

## Pointing the player at a server

The packaged player starts fullscreen with no arguments and connects to
`http://127.0.0.1:7878`. To use another server, set `MARATHON_SERVER`
(and `MARATHON_TOKEN` if the server needs one) in the environment the app
starts in, or pass `--server`/`--token`:

```sh
# Flatpak: stored for this user, used every time the app starts.
flatpak override --user \
  --env=MARATHON_SERVER=http://tv-server:7878 \
  --env=MARATHON_TOKEN=change-me \
  io.github.sarahstreza.Marathon

# AppImage and Nix: on the command line, or in the environment.
./Marathon-0.1.0-x86_64.AppImage --server http://tv-server:7878 --windowed
```

## Flatpak

```sh
packaging/flatpak/build.sh             # build
packaging/flatpak/build.sh --install   # build and install for this user
packaging/flatpak/build.sh --bundle    # build and write .build/marathon.flatpak
flatpak run io.github.sarahstreza.Marathon --windowed
```

Needs `flatpak-builder` (the distro package, or `flatpak install flathub
org.flatpak.Builder`, which the script uses when there is no
`flatpak-builder` command, e.g. on Silverblue or Bazzite), `python3`, and
the Flathub remote. The first build installs the freedesktop 26.08
runtime, SDK, and Rust SDK extension, and compiles libass, libplacebo, and
libmpv, so it takes a while; later builds reuse them.

The manifest (`packaging/flatpak/io.github.sarahstreza.Marathon.yml`)
builds from the working tree, so uncommitted changes are included. Cargo
can't download crates during a Flatpak build, so `build.sh` first lists
them in `cargo-sources.json` with
[flatpak-cargo-generator](https://github.com/flatpak/flatpak-builder-tools/tree/master/cargo)
(pinned in the script) and regenerates it whenever `Cargo.lock` changes.
The file is generated and git-ignored.

libmpv uses the runtime's FFmpeg. The runtime gets H.264, HEVC, and other
patent-encumbered codecs from the `org.freedesktop.Platform.codecs-extra`
extension, which Flatpak installs with the runtime.

Permissions: Wayland (X11 as a fallback), the GPU, game controllers
(`--device=input`), PulseAudio/PipeWire audio, the network, and read-only
access to `~/Videos` for embedded mode. Embedded mode with a config or
library elsewhere needs more, for example `flatpak override --user
--filesystem=/srv/video:ro io.github.sarahstreza.Marathon`. Embedded
mode's `data_dir` must also be writable. `~` in the config is your real
home directory, where the sandbox can't write, so point `data_dir` into the
app's own data folder,
`~/.var/app/io.github.sarahstreza.Marathon/data/marathon`, or grant write
access to the directory you use.

HDMI-CEC (`--features input-cec`) is not in the Flatpak: it would need
libcec and libp8-platform built in, and the adapter's serial device
(`--device=all`).

## AppImage

```sh
packaging/appimage/build.sh                 # in an Ubuntu 24.04 container
packaging/appimage/build.sh --no-container  # on this machine
./packaging/appimage/.build/Marathon-0.1.0-x86_64.AppImage --windowed
```

An AppImage runs only on distros with a glibc at least as new as the one
it was built against, so the script builds inside Ubuntu 24.04 (with podman
or docker), the oldest Ubuntu with a new enough libmpv. It installs Rust
with rustup inside the container and keeps Cargo's registry, the target
directory, and the toolchain in `packaging/appimage/.build/`, so rebuilds
are incremental.

[linuxdeploy](https://github.com/linuxdeploy/linuxdeploy) (pinned by tag
and checksum) bundles libmpv and the libraries it links: FFmpeg, libass,
libplacebo, and so on. It leaves out the ones that must match the host
(glibc, libGL and libEGL, libX11, libwayland-client, fontconfig, ...).
The script also leaves out VA-API and VDPAU: the bundled copies would look
for GPU drivers at Ubuntu's paths, and on other distros hardware decoding
would quietly fall back to software. The host's own libva is used
instead (installed on practically every desktop). JACK goes the other way:
linuxdeploy normally leaves libjack to the host, but libmpv links it and
most desktops don't have it, so the script bundles it. With no JACK server
running, mpv plays through PipeWire or PulseAudio as usual.

The host therefore needs what any desktop has: libGL/libEGL, libgbm,
libdrm, X11 (libX11, libxcb), libwayland-client, fontconfig, freetype,
fribidi, harfbuzz, ALSA, PipeWire's client library, libusb, libva, and
libvdpau. CI checks that list in a clean Ubuntu 24.04. Running the
AppImage also needs FUSE 2 (`libfuse2`), or set
`APPIMAGE_EXTRACT_AND_RUN=1`.

## Nix

The flake at the repository root has:

| Output | What |
|---|---|
| `packages.<system>.marathon-desktop` (also `default`) | The player (Linux), configurable with typed settings |
| `packages.<system>.marathon-desktop-cec` | The same with HDMI-CEC; GPL because of libcec |
| `packages.<system>.marathond` | The server (Linux and Apple Silicon macOS), configurable with typed settings |
| `packages.<system>.*-unwrapped` | The plain builds of each of the above |
| `overlays.default` | All of these as a nixpkgs overlay |
| `nixosModules.default` | `services.marathond`, the server as a systemd service |
| `checks.<system>.module` | A NixOS VM test of the module (`nix flake check`) |
| `checks.<system>.wrappers` | Tests of the wrapped packages and the settings checks |

```sh
nix build .#marathon-desktop
nix run .# -- --server http://tv-server:7878 --windowed
nix build .#marathond
```

`packages` has an entry for every system flakes expose
(`lib.systems.flakeExposed`, e.g. `i686-linux`), holding only what is
supported there; elsewhere it is empty. So a flake that walks every system,
such as a NUR, can test `marathon.packages.${system} ? marathond`.

It follows `nixos-unstable`, because the workspace needs Rust 1.95 and the
current stable NixOS release has an older one. Update the pin with `nix
flake update`. The packages build with nixpkgs' `buildRustPackage`
straight from `Cargo.lock` (no extra hashes to maintain), and run each
app's own tests.

winit and glutin open the windowing libraries (Wayland, xkbcommon, X11)
and libEGL with `dlopen`, so the player's RUNPATH lists them. On NixOS the
GPU driver comes from `hardware.graphics`. On other distros a Nix-built GL
program can't find the system's driver; run it through
[nixGL](https://github.com/nix-community/nixGL) (`nixGL marathon-desktop`)
or use the Flatpak or AppImage instead.

### Configured packages

`marathond` and `marathon-desktop` are wrappers. Override them with
`settings` and they start already configured; without settings they pass
everything through and behave like the `-unwrapped` builds. The settings
are typed Nix options, so a mistake (an unknown rule kind, a filesystem
source without a `root`, a channel without a number) fails when the
configuration is evaluated, naming the option, instead of when the
program starts.

The server's options, in [`nix/settings.nix`](../nix/settings.nix),
follow the config file ([`config/example.toml`](../config/example.toml))
key for key, except that sources and channels are attribute sets keyed by
ID, so another module can change one (`channel.office.number = 5`):

```nix
marathond.override {
  settings = {
    server = {
      bind = "0.0.0.0:7878";
      data_dir = "/srv/marathon";
      tokenFile = "/run/secrets/marathon-token";
    };
    source.nas = { kind = "filesystem"; root = "/srv/video"; };
    source.jellyfin = {
      kind = "jellyfin";
      url = "http://jellyfin.lan:8096";
      apiKeyFile = "/run/secrets/jellyfin-api-key";
    };
    channel.office = {
      number = 1;
      name = "The Office";
      items = [ { source = "nas"; path = "TV/The Office (US)"; } ];
      rule.kind = "ordered";
    };
    channel.taskmaster = {
      number = 6;
      name = "Taskmaster";
      items = [ { source = "jellyfin"; path = "Shows/Taskmaster"; } ];
      rule = { kind = "grouped"; by = "season"; groups = "shuffled"; within = "ordered"; };
    };
  };
}
```

The player's options, in
[`nix/desktop-settings.nix`](../nix/desktop-settings.nix), mirror its
command-line flags: `server`, `tokenFile`, `channel`, `windowed`,
`clientIdFile`, `cec.*` (on `marathon-desktop-cec` only), `environment`,
and `extraArgs`. For embedded mode, `embedded.settings` takes the same
server options as above:

```nix
marathon-desktop.override {
  settings = {
    server = "http://tv-server:7878";
    tokenFile = "/run/secrets/marathon-token";
    environment.RUST_LOG = "info";
  };
}

marathon-desktop.override {
  settings.embedded = {
    enable = true;
    settings.source.nas = { kind = "filesystem"; root = "/srv/video"; };
    settings.channel.office = { /* as above */ };
  };
}
```

`environment` values and `server` are defaults: a variable already set when
the player starts wins. Command-line arguments are added after the
wrapper's.

**Secrets.** The generated config file is in the world-readable Nix store,
so secrets have `…File` options: `server.tokenFile`, a Jellyfin source's
`apiKeyFile`, and the player's `tokenFile`. The file is read every time
the program starts, so it is never copied into the store. The config file
in the store holds a placeholder instead, and the wrapper hands the
program the filled-in config through a file descriptor
(`--config /dev/fd/3`), so it is never written to disk either. A secret
file holds one line; a trailing newline is fine. The plain `token` and
`api_key` options still work, but put the value in the store.

Both wrappers expose what they generated as `passthru`: `settings` (the
evaluated options) and `configFile` (`embeddedConfigFile` for the player).
The overlay (`overlays.default`) provides the same packages, so in a NixOS
or home-manager configuration with it you can write
`pkgs.marathon-desktop.override { ... }`.

### The NixOS module

```nix
{
  inputs.marathon.url = "github:sarahstreza/marathon";

  outputs = { nixpkgs, marathon, ... }: {
    nixosConfigurations.tv-server = nixpkgs.lib.nixosSystem {
      modules = [
        marathon.nixosModules.default
        {
          services.marathond = {
            enable = true;
            settings = {
              server.bind = "0.0.0.0:7878";
              server.tokenFile = "/run/secrets/marathon-token";
              source.nas = { kind = "filesystem"; root = "/srv/video"; };
              channel.office = {
                number = 1;
                name = "The Office";
                items = [ { source = "nas"; path = "TV/The Office (US)"; } ];
                rule.kind = "ordered";
              };
            };
            openFirewall = true;
          };
        }
      ];
    };
  };
}
```

`services.marathond.settings` takes the same typed options as the
`marathond` package (`server.data_dir` defaults to `/var/lib/marathon`
here), and a mistake fails the system build with the option's name. Secret
files (`server.tokenFile`, `apiKeyFile`) may be readable by root only:
systemd passes them to the service as credentials, and they are filled in
each time it starts, so changing one needs a `systemctl restart
marathond`. You can also set `configFile` to a complete config file of
your own (e.g. from agenix or sops-nix) instead of `settings`:

```nix
services.marathond = {
  enable = true;
  configFile = "/run/secrets/marathon.toml";
};
networking.firewall.allowedTCPPorts = [ 7878 ];
```

That file may be readable by root only as well. Its `server.data_dir`
must be `/var/lib/marathon`, the service's state directory.

The service runs as a transient system user (`DynamicUser`), can write
only `/var/lib/marathon`, and can read world-readable media. If the video
library is readable only by a group, add it with
`services.marathond.supplementaryGroups = [ "media" ];`. With `settings`,
`openFirewall = true` opens the port from `server.bind`.
