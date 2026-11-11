#!/usr/bin/env bash
# Generates a small fake video library for testing Marathon.
#
# Usage: [DURATION=<seconds>] scripts/make-test-library.sh <dir>
#
# Every clip is DURATION seconds (default 6) of ffmpeg's `testsrc` pattern with a tone, its
# own filename burned into the frame, and a running timestamp (useful for
# checking resume positions). Layout:
#
#   <dir>/TV/Test Show/Season N/Test Show - SxxEyy - Title.mkv   3 seasons x 4
#   <dir>/TV/Test Show/Season 1/Test Show - S01E01 - Pilot.en.srt  sidecar subs
#   <dir>/Essays/...                                              6 loosely named clips
#   <dir>/Essayists/{Alpha,Beta}/...                              3 clips per "creator"
#   <dir>/Misc/Café Society's Ünïcode Clip.mkv                    awkward filename
#   <dir>/marathon.toml                                           server config for this library
#
# The config serves the library on 127.0.0.1:7878 with its database in
# <dir>/.marathon (hidden, so scans skip it), and defines one channel per
# programming rule. Start the server with:
#
#   cargo run -p marathond -- --config <dir>/marathon.toml
#
# Clips shorter than 30s always count as finished once started (PLAN.md §1's
# max(30s, 2%) threshold), so tuning back in advances instead of resuming.
# To check resuming, generate longer clips, e.g. DURATION=120.
#
# Re-running overwrites existing clips. Needs ffmpeg with the drawtext filter
# (libfreetype + fontconfig). Never commit the output; .gitignore excludes it.

set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: $0 <dir>" >&2
    exit 2
fi

if ! command -v ffmpeg >/dev/null 2>&1; then
    echo "error: ffmpeg not found in PATH" >&2
    exit 1
fi
# Capture listings first: `grep -q` exiting early would SIGPIPE ffmpeg and
# trip pipefail.
filters="$(ffmpeg -hide_banner -filters 2>/dev/null)"
encoders="$(ffmpeg -hide_banner -encoders 2>/dev/null)"
if ! grep -q ' drawtext ' <<<"$filters"; then
    echo "error: this ffmpeg lacks the drawtext filter (needs libfreetype)" >&2
    exit 1
fi

out="$1"
mkdir -p "$out"

# Prefer H.264; fall back to MPEG-4 Part 2, which every ffmpeg build has.
if grep -q ' libx264 ' <<<"$encoders"; then
    vcodec=(-c:v libx264 -preset veryfast -crf 28)
else
    vcodec=(-c:v mpeg4 -q:v 5)
fi

# drawtext reads the label from a file so filenames never need filtergraph
# escaping (apostrophes, colons, unicode). The temp dir path is mktemp's, so it
# contains no characters that need escaping either.
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

DURATION="${DURATION:-6}"
if ! [[ "$DURATION" =~ ^[1-9][0-9]*$ ]]; then
    echo "error: DURATION must be a whole number of seconds, got '$DURATION'" >&2
    exit 2
fi
count=0

# make_clip <path relative to $out> [tone Hz]
make_clip() {
    local rel="$1" freq="${2:-440}"
    local dest="$out/$rel"
    mkdir -p "$(dirname "$dest")"
    basename -- "$rel" >"$tmp/label.txt"

    local vf
    vf="drawtext=textfile=$tmp/label.txt:fontsize=40:fontcolor=white"
    vf+=":box=1:boxcolor=black@0.7:boxborderw=12:x=(w-text_w)/2:y=h*0.38"
    vf+=",drawtext=text='%{pts\\:hms}':fontsize=32:fontcolor=white"
    vf+=":box=1:boxcolor=black@0.7:boxborderw=8:x=(w-text_w)/2:y=h*0.55"

    ffmpeg -hide_banner -loglevel error -nostdin -y \
        -f lavfi -i "testsrc=size=1280x720:rate=30:duration=$DURATION" \
        -f lavfi -i "sine=frequency=$freq:sample_rate=48000:duration=$DURATION" \
        -vf "$vf" \
        "${vcodec[@]}" -pix_fmt yuv420p \
        -c:a aac -b:a 64k \
        -shortest \
        "$dest"
    count=$((count + 1))
    echo "  $rel"
}

echo "Generating test library in $out"

# A show: 3 seasons x 4 episodes, in "Season N" folders.
show="Test Show"
titles=(
    "Pilot" "The Second One" "Halfway There" "Season Finale"
    "New Beginnings" "The Bottle Episode" "Clip Show" "Cliffhanger"
    "Resolution" "The Flashback" "Guest Star" "Series Finale"
)
for season in 1 2 3; do
    for episode in 1 2 3 4; do
        title="${titles[$(((season - 1) * 4 + episode - 1))]}"
        code="$(printf 'S%02dE%02d' "$season" "$episode")"
        make_clip "TV/$show/Season $season/$show - $code - $title.mkv" $((300 + season * 100 + episode * 20))
    done
done

# Sidecar subtitles for S01E01, covering the whole clip.
cat >"$out/TV/$show/Season 1/$show - S01E01 - Pilot.en.srt" <<'SRT'
1
00:00:00,000 --> 00:00:02,000
Sidecar subtitle: first cue

2
00:00:02,000 --> 00:00:04,000
Sidecar subtitle: second cue

3
00:00:04,000 --> 00:00:06,000
Sidecar subtitle: third cue
SRT
echo "  TV/$show/Season 1/$show - S01E01 - Pilot.en.srt"

# Loosely named "essay" clips: no SxxEyy codes, mixed containers, and an
# "Ep 2"/"Ep 10" pair for natural-sort checks.
make_clip "Essays/Ep 2 - Pacing.mkv" 660
make_clip "Essays/Ep 10 - Difficulty Curves.mkv" 680
make_clip "Essays/why_doors_matter.mp4" 700
make_clip "Essays/The Art of the Tutorial [1080p].mkv" 720
make_clip "Essays/level-design-101.mp4" 740
make_clip "Essays/2019-03-14 Camera Systems.mkv" 760

# Two "creators", each a channel of its own, nested in a "Video Essayists"
# channel together with the Essays folder (PLAN.md M11).
make_clip "Essayists/Alpha/Alpha 1 - Envy.mkv" 780
make_clip "Essayists/Alpha/Alpha 2 - Opulence.mkv" 800
make_clip "Essayists/Alpha/Alpha 3 - Cringe.mkv" 820
make_clip "Essayists/Beta/Beta 1 - The Room.mkv" 840
make_clip "Essayists/Beta/Beta 2 - Lockout.mkv" 850
make_clip "Essayists/Beta/Beta 3 - Marvel.mkv" 860

# Spaces, unicode, and an apostrophe in one name.
make_clip "Misc/Café Society's Ünïcode Clip — Ørsted.mkv" 880

# A server config pointing at this library. Paths are absolute so the
# server can be started from anywhere; they are escaped for TOML basic
# strings in case <dir> contains quotes or backslashes.
toml_escape() {
    local s="${1//\\/\\\\}"
    printf '%s' "${s//\"/\\\"}"
}
root="$(toml_escape "$(cd "$out" && pwd)")"
cat >"$out/marathon.toml" <<TOML
# Generated by scripts/make-test-library.sh for the test library in this
# directory. Re-running the script overwrites it.

[server]
bind = "127.0.0.1:7878"
data_dir = "$root/.marathon"

[[source]]
id = "test"
kind = "filesystem"
root = "$root"

# The 12 Test Show episodes in season/episode order (Season N folders are
# included without recursive = true).
[[channel]]
id = "show"
number = 1
name = "Test Show"
items = [{ source = "test", path = "TV/Test Show" }]
rule = { kind = "ordered" }

# Seasons in shuffled order, each season's episodes in order.
[[channel]]
id = "seasons"
number = 2
name = "Test Show (shuffled seasons)"
items = [{ source = "test", path = "TV/Test Show" }]
rule = { kind = "grouped", by = "season", groups = "shuffled", within = "ordered" }

# The 6 loosely named essays, bag-shuffled.
[[channel]]
id = "essays"
number = 3
name = "Essays"
items = [{ source = "test", path = "Essays" }]
rule = { kind = "shuffled" }

# One item with an awkward unicode name; repeats itself.
[[channel]]
id = "misc"
number = 4
name = "Misc"
items = [{ source = "test", path = "Misc" }]
rule = { kind = "ordered" }

# Nested channels (PLAN.md M11). "Video Essayists" shuffles between two
# child channels and the six Essays. It shares the children's playback
# state, so watching Beta here or on channel 6 is the same position.
# Alpha is hidden: no number, not in the guide or channel up/down.
[[channel]]
id = "essayists"
number = 5
name = "Video Essayists"
items = [
  { channel = "alpha" },
  { channel = "beta" },
  { source = "test", path = "Essays" },
]
rule = { kind = "shuffled" }
state = "shared"

[[channel]]
id = "alpha"
name = "Alpha"
hidden = true
items = [{ source = "test", path = "Essayists/Alpha" }]
rule = { kind = "ordered" }

[[channel]]
id = "beta"
number = 6
name = "Beta"
items = [{ source = "test", path = "Essayists/Beta" }]
rule = { kind = "ordered" }
TOML
echo "  marathon.toml"

echo "Done: $count clips, 1 subtitle file, and marathon.toml."
