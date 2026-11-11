//! The Android client is generated from a committed snapshot of the API's
//! OpenAPI document, `android/api/openapi.json` (docs/ANDROID.md). This
//! fails when the server's API changes without the snapshot being
//! refreshed, so the app can't silently drift from the server. Refresh it,
//! and regenerate the Kotlin client, with `scripts/gen-android-api.sh`.
//!
//! It lives here because `marathond --print-openapi` is what writes the
//! snapshot.

// Test code: panicking on I/O or serialization failure is the right failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use marathon_server::http::ApiDoc;
use utoipa::OpenApi;

#[test]
fn android_openapi_snapshot_is_up_to_date() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../android/api/openapi.json");
    // Compared as JSON, not text: whether serde_json keeps keys in order
    // depends on features other workspace crates turn on, so the same
    // document can print in a different key order depending on how it was
    // built.
    let want = serde_json::to_value(ApiDoc::openapi()).unwrap();
    let have: Option<serde_json::Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    assert!(
        have.as_ref() == Some(&want),
        "{} is stale. Regenerate it with scripts/gen-android-api.sh",
        path.display()
    );
}
