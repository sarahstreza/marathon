#!/usr/bin/env bash
# Checks that platform libraries stay at the edges (PLAN.md §2 decision 7,
# §9 "Keep winit at the edge", milestone M3.5):
#
# 1. marathon-client does not depend, even transitively, on winit, glutin,
#    or gilrs (with every feature enabled).
# 2. winit, glutin, glutin-winit, and raw-window-handle are referenced in
#    Rust code only inside marathon-desktop's platform module.
#
# Run from anywhere in the repo. Exits non-zero, listing offenders, on
# failure. CI runs it.

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

status=0

forbidden_deps='^(winit|glutin|glutin-winit|gilrs) '
deps="$(cargo tree -p marathon-client --all-features --edges normal,build \
    --prefix none --format '{p}')"
if bad="$(grep -E "$forbidden_deps" <<<"$deps" | sort -u)" && [[ -n "$bad" ]]; then
    echo "error: marathon-client depends on platform libraries:" >&2
    echo "$bad" >&2
    status=1
fi

platform_dir='apps/marathon-desktop/src/platform/'
# Paths and `use`/`extern crate` of the platform crates. Doc comments that
# merely mention "winit" are fine.
pattern='\b(winit|glutin|glutin_winit|raw_window_handle)::|^\s*(pub\s+)?(use|extern\s+crate)\s+(winit|glutin|glutin_winit|raw_window_handle)\b'
# Tracked and new (unignored) files; -s skips ones deleted but not staged.
if bad="$(git ls-files --cached --others --exclude-standard -- '*.rs' \
    | grep -v "^$platform_dir" | xargs grep -snE "$pattern" || true)" \
    && [[ -n "$bad" ]]; then
    echo "error: platform libraries used outside $platform_dir:" >&2
    echo "$bad" >&2
    status=1
fi

if [[ $status -eq 0 ]]; then
    echo "seams ok: marathon-client is platform-free; winit/glutin confined to $platform_dir"
fi
exit $status
