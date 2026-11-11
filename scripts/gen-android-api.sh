#!/usr/bin/env bash
# Refreshes everything the Android client derives from the server
# (docs/ANDROID.md), then regenerates the Kotlin API client:
#
#   - android/api/openapi.json, from `marathond --print-openapi`
#   - crates/marathon-api/tests/fixtures/control.json, the JSON of every
#     Action and Event, which the Android client's tests check against
#
# Run it after changing the HTTP API or the Action/Event types. Needs the
# Rust toolchain (and libmpv for the workspace build); the Gradle step needs
# a JDK and the Android SDK, and is skipped with --no-gradle.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

cargo run --quiet --locked -p marathond -- --print-openapi > android/api/openapi.json.tmp
mv android/api/openapi.json.tmp android/api/openapi.json
echo "wrote android/api/openapi.json"

UPDATE_FIXTURES=1 cargo test --quiet --locked -p marathon-api --test control_fixtures >/dev/null
echo "wrote crates/marathon-api/tests/fixtures/control.json"

if [ "${1:-}" != "--no-gradle" ]; then
    (cd android && ./gradlew --quiet :api:openApiGenerate)
    echo "regenerated the Kotlin client (android/api/build/generated)"
fi
