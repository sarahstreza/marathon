//! The Jellyfin API key never leaks (M8): not into any log line, API
//! response, or the database, whether Jellyfin accepts the key or not.
//!
//! This test is alone in its binary, with the log capture installed as the
//! *global* subscriber before anything logs. Both matter. tracing caches
//! each callsite's interest for the whole process the first time any
//! thread reaches it, and while at most one dispatcher is registered
//! (tracing-core 0.1.36's `Rebuilder::JustOne`), it asks only that thread's
//! default. So with a thread-local `set_default` capture and other tests
//! running in parallel, whichever test first logged "listed Jellyfin
//! library" decided for everyone: about a third of runs under load
//! captured nothing, and "the key is not in the logs" passed vacuously.
//! A global subscriber, set before any callsite registers, also sees log
//! lines from every thread (e.g. `spawn_blocking` scans), not just the
//! test's.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod jellyfin_mock;

use std::sync::{Arc, Mutex};

use common::TestEnv;
use jellyfin_mock::{KEY, MockJellyfin, Running};
use marathon_api::Now;

/// A log writer that keeps everything in memory.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The API key appears in no log line (at any level, from any thread), no
/// API response, and not in the database, whether Jellyfin accepts it or
/// not.
#[tokio::test]
async fn the_api_key_never_appears_in_logs_or_responses() {
    let logs = Captured::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    // Global, and first: see the module comment.
    tracing::subscriber::set_global_default(subscriber)
        .expect("nothing else sets a subscriber here");

    let jellyfin = MockJellyfin::start().await;
    let mut bodies = Vec::new();
    for key in [KEY, "wrong-key-5d2a91"] {
        let env = TestEnv::new();
        let server = Running::start(&env, &jellyfin.url(), key).await;
        let mut paths = vec![
            "/api/v1/channels".to_owned(),
            "/api/v1/channels/office/now".to_owned(),
            "/api/v1/history".to_owned(),
            "/api/v1/openapi.json".to_owned(),
        ];
        if key == KEY {
            let now: Now = server.json("/api/v1/channels/office/now").await;
            paths.push(format!("/api/v1/items/{}", now.item.id));
            paths.push(now.stream_url.clone());
        }
        for path in &paths {
            let response = server.get(path).await;
            bodies.push(String::from_utf8_lossy(&response.bytes().await.unwrap()).into_owned());
        }
        let rescan = server
            .http
            .post(server.url("/api/v1/library/rescan"))
            .send()
            .await
            .unwrap();
        let report = rescan.text().await.unwrap();
        if key != KEY {
            // A refused key is reported, without the key.
            assert!(report.contains("refused the API key"), "{report}");
        }
        bodies.push(report);
        let sources: Vec<(String,)> = sqlx::query_as("SELECT config_json FROM sources")
            .fetch_all(
                &marathon_server::db::Db::open(
                    &env.data_dir().join(marathon_server::DATABASE_FILE),
                )
                .await
                .unwrap()
                .pool()
                .clone(),
            )
            .await
            .unwrap();
        bodies.extend(sources.into_iter().map(|(json,)| json));
        server.stop().await;
    }

    let logs = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    // The capture works: requests to Jellyfin were logged in detail.
    assert!(logs.contains("Jellyfin"), "nothing captured?");
    for key in [KEY, "wrong-key-5d2a91"] {
        assert!(!logs.contains(key), "key in logs:\n{logs}");
        for body in &bodies {
            assert!(!body.contains(key), "key in a response: {body}");
        }
    }
}
