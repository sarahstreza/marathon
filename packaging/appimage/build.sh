#!/usr/bin/env bash
# Builds the Marathon player as an AppImage. See docs/PACKAGING.md.
#
#   packaging/appimage/build.sh                 # build inside an Ubuntu container
#   packaging/appimage/build.sh --no-container  # build on this machine
#
# The AppImage runs on distros at least as new as the one it was built on
# (it uses that distro's glibc), so by default it is built inside an Ubuntu
# 24.04 container, with podman or docker. Ubuntu 24.04 is the oldest
# Ubuntu whose libmpv is new enough (mpv 0.37; client API 2.x).
# --no-container builds against this machine's libraries instead, and needs
# Rust 1.95+, pkg-config, and the libmpv and libudev headers.
#
# The result is packaging/appimage/.build/Marathon-<version>-<arch>.AppImage.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
app_id=io.github.sarahstreza.Marathon
work="$here/.build"
image=docker.io/library/ubuntu:24.04

# linuxdeploy, pinned (it has no stable releases, only dated tags), with
# the AppImage output plugin built in.
linuxdeploy_tag=1-alpha-20251107-1
declare -A linuxdeploy_sha256=(
    [x86_64]=c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d
    [aarch64]=620095110d693282b8ebeb244a95b5e911cf8f65f76c88b4b47d16ae6346fcff
)

container=true
in_container=false
for arg in "$@"; do
    case "$arg" in
        --no-container) container=false ;;
        # Internal: this script re-run inside the container.
        --in-container) in_container=true ;;
        -h | --help)
            sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "build.sh: unknown argument: $arg" >&2
            exit 64
            ;;
    esac
done

mkdir -p "$work"

if "$container" && ! "$in_container"; then
    engine=$(command -v podman || command -v docker || true)
    if [ -z "$engine" ]; then
        echo "build.sh: needs podman or docker (or pass --no-container)" >&2
        exit 69
    fi
    # The container runs as its root, to install packages. Under rootless
    # podman that root is this user, so the files it writes are already
    # ours. Otherwise (docker, rootful podman) it is the real root, and the
    # script hands the outputs back to this user at the end.
    owner=()
    if [ "$(basename "$engine")" != podman ] \
        || [ "$("$engine" info --format '{{.Host.Security.Rootless}}')" != true ]; then
        owner=(-e HOST_OWNER="$(id -u):$(id -g)")
    fi
    # The repository is mounted at /src. SELinux labelling is disabled
    # for the container rather than relabelling the checkout (`:z`).
    # Cargo's registry and target directory live in .build/, so later
    # builds are incremental.
    exec "$engine" run --rm --security-opt label=disable \
        -v "$repo:/src" -w /src "${owner[@]}" \
        "$image" packaging/appimage/build.sh --in-container
fi

if "$in_container"; then
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -q
    apt-get install -y -q --no-install-recommends \
        ca-certificates curl build-essential pkg-config file \
        libmpv-dev libudev-dev
    export CARGO_HOME="$work/cargo" RUSTUP_HOME="$work/rustup"
    if [ ! -x "$CARGO_HOME/bin/cargo" ]; then
        curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --no-modify-path
    fi
    export PATH="$CARGO_HOME/bin:$PATH"
    rustup update stable
    export CARGO_TARGET_DIR="$work/target"
    if [ -n "${HOST_OWNER:-}" ]; then
        trap 'chown -R "$HOST_OWNER" "$work"' EXIT
    fi
fi

arch=$(uname -m)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$repo/Cargo.toml" | head -n 1)
target_dir="${CARGO_TARGET_DIR:-$repo/target}"

# As in the Dockerfile: a smaller, faster binary without touching the
# workspace's own release profile.
CARGO_PROFILE_RELEASE_LTO=fat \
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1 \
    CARGO_PROFILE_RELEASE_STRIP=symbols \
    cargo build --release --locked -p marathon-desktop --manifest-path "$repo/Cargo.toml"

linuxdeploy="$work/linuxdeploy-$linuxdeploy_tag-$arch.AppImage"
if [ ! -x "$linuxdeploy" ]; then
    expected=${linuxdeploy_sha256[$arch]:-}
    if [ -z "$expected" ]; then
        echo "build.sh: no pinned linuxdeploy for $arch" >&2
        exit 69
    fi
    curl -fsSL -o "$linuxdeploy.tmp" \
        "https://github.com/linuxdeploy/linuxdeploy/releases/download/$linuxdeploy_tag/linuxdeploy-$arch.AppImage"
    echo "$expected  $linuxdeploy.tmp" | sha256sum -c --quiet
    chmod +x "$linuxdeploy.tmp"
    mv "$linuxdeploy.tmp" "$linuxdeploy"
fi

appdir="$work/AppDir"
rm -rf "$appdir"
install -Dm644 "$repo/packaging/linux/$app_id.metainfo.xml" -t "$appdir/usr/share/metainfo"

# linuxdeploy copies libmpv and everything it links (FFmpeg, libass, ...)
# into the AppDir, except the libraries that must come from the host (its
# built-in exclude list: glibc, libGL/libEGL, X11, fontconfig, ...).
# VA-API and VDPAU are left to the host too: the bundled loaders would look
# for drivers at Ubuntu's paths and miss them elsewhere, quietly turning
# hardware decoding off.
#
# APPIMAGE_EXTRACT_AND_RUN: linuxdeploy is itself an AppImage, and
# containers usually have no FUSE.
APPIMAGE_EXTRACT_AND_RUN=1 \
    LINUXDEPLOY_OUTPUT_VERSION="$version" \
    LDAI_OUTPUT="$work/Marathon-$version-$arch.AppImage" \
    "$linuxdeploy" --appdir "$appdir" \
    --executable "$target_dir/release/marathon-desktop" \
    --desktop-file "$repo/packaging/linux/$app_id.desktop" \
    --icon-file "$repo/packaging/linux/$app_id.svg" \
    --exclude-library 'libva.so*' \
    --exclude-library 'libva-*.so*' \
    --exclude-library 'libvdpau.so*' \
    --output appimage

echo "Wrote $work/Marathon-$version-$arch.AppImage"
