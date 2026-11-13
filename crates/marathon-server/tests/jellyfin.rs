//! Jellyfin sources (M8) against a mock Jellyfin (`jellyfin_mock`): the
//! catalog from its API, durations, proxied streams with ranges, rescans,
//! and an unreachable server. Keeping the API key secret is tested in
//! `jellyfin_key_secrecy.rs`, alone in its own binary (see there for why).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;
mod jellyfin_mock;

use std::time::{Duration, Instant};

use common::TestEnv;
use jellyfin_mock::{KEY, MockJellyfin, Running, SHOWS, advance, media_bytes};
use marathon_api::{Item, Now, ScanReport};
use serde_json::{Value, json};

#[tokio::test]
async fn jellyfin_libraries_become_items_with_metadata_and_durations() {
    let jellyfin = MockJellyfin::start().await;
    let env = TestEnv::new();
    let server = Running::start(&env, &jellyfin.url(), KEY).await;

    let office = server.channel("office").await;
    // The virtual (missing) episode is left out.
    assert_eq!(office.item_count, 3);
    let films = server.channel("films").await;
    assert_eq!(films.item_count, 2);

    // Ordered by season and episode from Jellyfin's metadata, with
    // durations known before anyone has played anything.
    let mut now: Now = server.json("/api/v1/channels/office/now").await;
    let mut seen = Vec::new();
    for _ in 0..3 {
        let item = &now.item;
        seen.push((
            item.season,
            item.episode,
            item.title.clone(),
            item.duration_ms,
        ));
        now = advance(&server, "office", &item.id.clone()).await;
    }
    assert_eq!(
        seen,
        vec![
            (Some(1), Some(1), "Pilot".to_owned(), Some(1_374_000)),
            (Some(3), Some(14), "The Injury".to_owned(), Some(1_320_000)),
            (Some(3), Some(15), "The Return".to_owned(), Some(1_308_000)),
        ]
    );
    // Paths mirror the library's folders, under the library's name.
    let item: Item = server.json(&format!("/api/v1/items/{}", now.item.id)).await;
    assert_eq!(item.source_id, "jf");
    assert_eq!(
        item.path,
        "Shows/The Office (US)/Season 1/The Office (US) - S01E01 - Pilot.mkv"
    );
    assert_eq!(item.show.as_deref(), Some("The Office (US)"));
    assert!(item.subtitles.is_empty());

    // Films come from both of the library's folders.
    let now: Now = server.json("/api/v1/channels/films/now").await;
    let mut paths = vec![now.item.path.clone()];
    let next = advance(&server, "films", &now.item.id).await;
    paths.push(next.item.path);
    paths.sort();
    assert_eq!(
        paths,
        [
            "Films/Alien (1979).mp4",
            "Films/Heat (1995)/Heat (1995).mkv"
        ]
    );

    // The client paged through results (the mock returns two at a time),
    // and asked only for videos.
    let requests = jellyfin.requests();
    let pages = requests
        .iter()
        .filter(|r| r.contains("/Items?") && r.contains(SHOWS))
        .count();
    assert_eq!(pages, 2, "{requests:#?}");
    assert!(requests.iter().all(|r| !r.contains(KEY)), "{requests:#?}");

    // Filesystem channels are unaffected.
    assert_eq!(server.channel("show").await.item_count, 4);
    server.stop().await;
}

#[tokio::test]
async fn streams_are_proxied_with_ranges() {
    let jellyfin = MockJellyfin::start().await;
    let env = TestEnv::new();
    let server = Running::start(&env, &jellyfin.url(), KEY).await;
    let now: Now = server.json("/api/v1/channels/office/now").await;
    let jellyfin_id = "0ffe0101aaaaaaaaaaaaaaaaaaaaaaaa"; // Pilot
    let expected = media_bytes(jellyfin_id);
    let stream = server.url(&now.stream_url);

    // The whole file.
    let response = server.http.get(&stream).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-length"], "1000");
    assert_eq!(response.headers()["accept-ranges"], "bytes");
    assert_eq!(response.bytes().await.unwrap(), expected);

    // A range.
    let response = server
        .http
        .get(&stream)
        .header("range", "bytes=100-199")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 206);
    assert_eq!(response.headers()["content-range"], "bytes 100-199/1000");
    assert_eq!(response.headers()["content-length"], "100");
    assert_eq!(response.bytes().await.unwrap(), expected[100..200]);

    // An open-ended range, as players use when seeking.
    let response = server
        .http
        .get(&stream)
        .header("range", "bytes=900-")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 206);
    assert_eq!(response.headers()["content-range"], "bytes 900-999/1000");
    assert_eq!(response.bytes().await.unwrap(), expected[900..]);

    // An unsatisfiable range.
    let response = server
        .http
        .get(&stream)
        .header("range", "bytes=5000-")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 416);
    assert_eq!(response.headers()["content-range"], "bytes */1000");

    // HEAD, which mpv uses to probe.
    let response = server.http.head(&stream).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-length"], "1000");

    // An item Jellyfin has lost is a 404, not a 502.
    std::fs::remove_file(jellyfin.media_file(jellyfin_id)).unwrap();
    let response = server.http.get(&stream).send().await.unwrap();
    assert_eq!(response.status(), 404);
    server.stop().await;
}

#[tokio::test]
async fn rescanning_picks_up_episodes_added_in_jellyfin() {
    let jellyfin = MockJellyfin::start().await;
    let env = TestEnv::new();
    let server = Running::start(&env, &jellyfin.url(), KEY).await;
    assert_eq!(server.channel("office").await.item_count, 3);

    jellyfin.add_episode("0ffe0317aaaaaaaaaaaaaaaaaaaaaaaa", 3, 17, "Lecture Circuit");
    let report: ScanReport = server
        .http
        .post(server.url("/api/v1/library/rescan"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(report.added, 1, "{report:?}");
    assert!(report.errors.is_empty(), "{report:?}");
    assert_eq!(server.channel("office").await.item_count, 4);

    // A client reporting a slightly different duration does not override
    // Jellyfin's...
    let now: Now = server.json("/api/v1/channels/office/now").await;
    let jellyfin_duration = now.item.duration_ms.unwrap();
    let response = server
        .http
        .post(server.url("/api/v1/channels/office/progress"))
        .json(&json!({
            "item_id": now.item.id,
            "position_ms": 60_000,
            "duration_ms": jellyfin_duration + 480,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 204);
    let item: Item = server.json(&format!("/api/v1/items/{}", now.item.id)).await;
    assert_eq!(item.duration_ms, Some(jellyfin_duration));

    // ...so a rescan with nothing new changes nothing.
    let report: ScanReport = server
        .http
        .post(server.url("/api/v1/library/rescan"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!((report.added, report.updated), (0, 0), "{report:?}");
    server.stop().await;
}

#[tokio::test]
async fn an_unreachable_jellyfin_is_a_clear_error_and_files_keep_working() {
    let jellyfin = MockJellyfin::start().await;
    let env = TestEnv::new();
    // First with Jellyfin up, so the catalog has its items...
    let server = Running::start(&env, &jellyfin.url(), KEY).await;
    let now: Now = server.json("/api/v1/channels/office/now").await;
    server.stop().await;

    // ...then with it gone. Not by reusing the mock's port: once freed,
    // another test's mock or server can be given it, and then "gone"
    // Jellyfin answers. Nothing can ever listen on port 0, and connecting
    // to it fails at once (refused on Linux, address not available on
    // macOS). A bound but unlistened socket would refuse on Linux too,
    // but on macOS connections to it hang until they time out.
    drop(jellyfin);
    let server = Running::start(&env, "http://127.0.0.1:0/jellyfin", KEY).await;

    // The server started; the scan reports the source and keeps its items.
    let report: ScanReport = server
        .http
        .post(server.url("/api/v1/library/rescan"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(report.errors.len(), 1, "{report:?}");
    assert!(report.errors[0].contains("source jf"), "{report:?}");
    assert!(
        report.errors[0].contains("cannot reach Jellyfin"),
        "{report:?}"
    );
    assert_eq!(server.channel("office").await.item_count, 3);

    // Streaming fails fast with a 502 that says why, rather than hanging.
    let started = Instant::now();
    let response = server.get(&now.stream_url).await;
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(response.status(), 502);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["code"], "upstream");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("cannot reach Jellyfin source jf"),
        "{body}"
    );

    // Filesystem channels play as usual.
    let show: Now = server.json("/api/v1/channels/show/now").await;
    assert_eq!(server.get(&show.stream_url).await.status(), 200);
    server.stop().await;
}
