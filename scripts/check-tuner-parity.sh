#!/usr/bin/env bash
# The Android client's Tuner is a port of marathon-client's, and must pass
# the same scenarios (PLAN.md M10, docs/ANDROID.md). Each Rust tuner test
# has a Kotlin twin with the same name; this fails if one is missing, so a
# scenario added on one side can't be forgotten on the other.
#
# A Rust test that genuinely has no Android meaning goes in SKIP below, with
# the reason. Kotlin-only tests are allowed (and reported).
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
rust="$root/crates/marathon-client/src/tuner/tests.rs"
kotlin="$root/android/core/src/test/kotlin/marathon/core/TunerTest.kt"

# name<TAB>reason
SKIP=$(cat <<'LIST'
LIST
)

# The function after each `#[test]` / `@Test`.
rust_tests=$(awk '/#\[test\]/ { want = 1; next } want && match($0, /fn [a-z0-9_]+/) { print substr($0, RSTART + 3, RLENGTH - 3); want = 0 }' "$rust" | sort)
kotlin_tests=$(awk '/@Test/ { want = 1; next } want && match($0, /fun [a-z0-9_]+/) { print substr($0, RSTART + 4, RLENGTH - 4); want = 0 }' "$kotlin" | sort)
skipped=$(printf '%s\n' "$SKIP" | cut -f1 | sed '/^$/d' | sort)

missing=$(comm -23 <(printf '%s\n' "$rust_tests") <(printf '%s\n%s\n' "$kotlin_tests" "$skipped" | sort -u))
extra=$(comm -13 <(printf '%s\n' "$rust_tests") <(printf '%s\n' "$kotlin_tests"))

echo "Rust tuner tests: $(printf '%s\n' "$rust_tests" | sed '/^$/d' | wc -l), Kotlin: $(printf '%s\n' "$kotlin_tests" | sed '/^$/d' | wc -l), skipped: $(printf '%s\n' "$skipped" | sed '/^$/d' | wc -l)"
if [ -n "$extra" ]; then
    echo "Kotlin-only tests (fine, but consider adding them to the Rust suite):"
    printf '  %s\n' $extra
fi
if [ -n "$missing" ]; then
    echo "Rust tuner tests with no Kotlin twin in ${kotlin#"$root"/}:" >&2
    printf '  %s\n' $missing >&2
    exit 1
fi
echo "Tuner scenarios match."
