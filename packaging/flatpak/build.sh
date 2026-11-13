#!/usr/bin/env bash
# Builds the Marathon player as a Flatpak and, optionally, installs it or
# writes a single-file bundle. See docs/PACKAGING.md.
#
#   packaging/flatpak/build.sh              # build into packaging/flatpak/.build
#   packaging/flatpak/build.sh --install    # ... and install it for this user
#   packaging/flatpak/build.sh --bundle     # ... and write marathon.flatpak
#
# Needs flatpak-builder (the distro package, or the org.flatpak.Builder
# Flatpak, which this script uses if there is no flatpak-builder command),
# python3 (to generate cargo-sources.json), and the Flathub remote, from
# which flatpak-builder installs the runtime and SDK it needs.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
app_id=io.github.sarahstreza.Marathon
manifest="$here/$app_id.yml"
work="$here/.build"

# flatpak-cargo-generator, pinned. Bump the commit to pick up fixes.
generator_commit=9d86d336a39b9af38b86f114b6ce5820fe260f91
generator_url="https://raw.githubusercontent.com/flatpak/flatpak-builder-tools/$generator_commit/cargo/flatpak-cargo-generator.py"

install=false
bundle=false
for arg in "$@"; do
    case "$arg" in
        --install) install=true ;;
        --bundle) bundle=true ;;
        -h | --help)
            sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "build.sh: unknown argument: $arg" >&2
            exit 64
            ;;
    esac
done

mkdir -p "$work"

# cargo-sources.json lists every crate in Cargo.lock with its checksum, so
# flatpak-builder can download them up front and build without network.
# Regenerated whenever Cargo.lock is newer.
sources="$here/cargo-sources.json"
if [ ! -s "$sources" ] || [ "$repo/Cargo.lock" -nt "$sources" ]; then
    venv="$work/venv"
    if [ ! -x "$venv/bin/python" ]; then
        python3 -m venv "$venv"
        "$venv/bin/pip" install --quiet aiohttp tomlkit
    fi
    curl -fsSL "$generator_url" -o "$work/flatpak-cargo-generator.py"
    "$venv/bin/python" "$work/flatpak-cargo-generator.py" "$repo/Cargo.lock" -o "$sources.tmp"
    mv "$sources.tmp" "$sources"
fi

if command -v flatpak-builder > /dev/null; then
    builder=(flatpak-builder)
else
    builder=(flatpak run --filesystem="$repo" org.flatpak.Builder)
fi

args=(--user --install-deps-from=flathub --force-clean --ccache
    --state-dir="$here/.flatpak-builder" --repo="$work/repo")
if "$install"; then
    args+=(--install)
fi
"${builder[@]}" "${args[@]}" "$work/app" "$manifest"

if "$bundle"; then
    flatpak build-bundle --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo \
        "$work/repo" "$work/marathon.flatpak" "$app_id"
    echo "Wrote $work/marathon.flatpak"
fi
