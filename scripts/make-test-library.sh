#!/usr/bin/env bash
# Generates a small fake video library for testing Marathon.
#
# Usage: scripts/make-test-library.sh <dir>
#
# Every clip is a few seconds of ffmpeg's `testsrc` pattern with a tone, its
# own filename burned into the frame, and a running timestamp (useful for
# checking resume positions). Layout:
#
#   <dir>/TV/Test Show/Season N/Test Show - SxxEyy - Title.mkv   3 seasons x 4
#   <dir>/TV/Test Show/Season 1/Test Show - S01E01 - Pilot.en.srt  sidecar subs
#   <dir>/Essays/...                                              6 loosely named clips
#   <dir>/Misc/Café Society's Ünïcode Clip.mkv                    awkward filename
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

DURATION=6
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

# Spaces, unicode, and an apostrophe in one name.
make_clip "Misc/Café Society's Ünïcode Clip — Ørsted.mkv" 880

echo "Done: $count clips and 1 subtitle file."
