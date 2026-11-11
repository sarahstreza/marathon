//! Jellyfin as a source (PLAN.md M8): its libraries become items, and its
//! streams are proxied through Marathon, so players still only ever see
//! Marathon's own `/stream` URLs (PLAN.md §2 decision 1).
//!
//! # Catalog
//!
//! Every library (`GET /Library/VirtualFolders`, optionally only those
//! named in config) is listed with `GET /Items`, page by page. Each video
//! (episodes, movies, videos, music videos; not virtual "missing"
//! episodes) becomes an item whose path is the library's name followed by
//! the file's path within the library's folder, e.g.
//! `TV Shows/The Office (US)/Season 3/The Office - S03E14.mkv`. Channel
//! selectors therefore work exactly as for a filesystem source, `Season N`
//! folder rule included. Items whose file Jellyfin does not report fall
//! back to a path made from the metadata (`Series/Season N/Name`).
//!
//! Show, season, episode, and title come from Jellyfin's metadata rather
//! than from parsing file names, and the duration from its `RunTimeTicks`,
//! so channels know durations before any client has played anything.
//! Sidecar subtitles are not imported; subtitles inside the video play as
//! usual.
//!
//! # Streaming
//!
//! `GET /stream/{id}` for a Jellyfin item fetches
//! `/Videos/{jellyfin id}/stream?static=true` (the original file, never a
//! transcode) and streams the answer back, passing the conditional and
//! range headers through in both directions, so seeking works as it does
//! for local files.
//!
//! # Credentials
//!
//! The API key goes only in the `Authorization` header (marked sensitive,
//! so it is never printed), never in a URL. Errors name the source and the
//! URL, which carries no secret.

use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use reqwest::Url;
use serde::Deserialize;

use crate::catalog::Found;
use crate::error::ApiError;
use crate::parse::ParsedName;

/// How long to wait to connect to Jellyfin.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a catalog request may take in all.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a stream may go without receiving anything before it is
/// abandoned. (A whole stream can take hours, so there is no total.)
const READ_TIMEOUT: Duration = Duration::from_secs(60);
/// Items per `GET /Items` page.
const PAGE_SIZE: u64 = 500;

/// Request headers passed on to Jellyfin when streaming.
const FORWARDED_REQUEST_HEADERS: [header::HeaderName; 5] = [
    header::RANGE,
    header::IF_RANGE,
    header::IF_NONE_MATCH,
    header::IF_MODIFIED_SINCE,
    header::ACCEPT,
];

/// Response headers passed back from Jellyfin when streaming.
const FORWARDED_RESPONSE_HEADERS: [header::HeaderName; 6] = [
    header::CONTENT_TYPE,
    header::CONTENT_LENGTH,
    header::CONTENT_RANGE,
    header::ACCEPT_RANGES,
    header::LAST_MODIFIED,
    header::ETAG,
];

/// A Jellyfin catalog request failed.
#[derive(Debug, thiserror::Error)]
pub enum JellyfinError {
    #[error("invalid Jellyfin URL {0:?}")]
    BadUrl(String),
    #[error("cannot set up the HTTP client: {0}")]
    Client(String),
    #[error("cannot reach Jellyfin: {0}")]
    Request(String),
    #[error("Jellyfin refused the API key ({0})")]
    Unauthorized(StatusCode),
    #[error("Jellyfin answered {status} for {url}")]
    Status { status: StatusCode, url: String },
    #[error("unexpected answer from Jellyfin for {url}: {message}")]
    Decode { url: String, message: String },
}

/// A connection to one Jellyfin server.
#[derive(Debug, Clone)]
pub struct JellyfinClient {
    source_id: String,
    base: Url,
    http: reqwest::Client,
    /// `Authorization: MediaBrowser ... Token="<api key>"`, marked
    /// sensitive.
    auth: HeaderValue,
    libraries: Option<Vec<String>>,
}

/// A library, as `GET /Library/VirtualFolders` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Library {
    pub name: String,
    /// Folders on the Jellyfin server the library is made of.
    #[serde(default)]
    pub locations: Vec<String>,
    /// The library's own item ID, the parent of everything in it.
    pub item_id: Option<String>,
}

/// One page of `GET /Items`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ItemsPage {
    #[serde(default)]
    items: Vec<JellyfinItem>,
    #[serde(default)]
    total_record_count: u64,
}

/// The fields of Jellyfin's `BaseItemDto` that Marathon uses.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct JellyfinItem {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    /// `Episode`, `Movie`, `Video`, ...
    #[serde(rename = "Type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub series_name: Option<String>,
    /// The season number, for an episode.
    #[serde(default)]
    pub parent_index_number: Option<u32>,
    /// The episode number, for an episode.
    #[serde(default)]
    pub index_number: Option<u32>,
    /// Duration in 100ns ticks.
    #[serde(default)]
    pub run_time_ticks: Option<i64>,
    /// The file's path on the Jellyfin server.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub media_sources: Vec<MediaSource>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct MediaSource {
    #[serde(default)]
    pub size: Option<i64>,
}

impl JellyfinClient {
    /// A client for the Jellyfin server at `url`. Makes no requests.
    pub fn new(
        source_id: &str,
        url: &str,
        api_key: &str,
        libraries: Option<Vec<String>>,
    ) -> Result<Self, JellyfinError> {
        let mut base = Url::parse(url).map_err(|_| JellyfinError::BadUrl(url.to_owned()))?;
        // Treat the base as a directory, so joining keeps a path prefix
        // (Jellyfin behind a reverse proxy at /jellyfin).
        if !base.path().ends_with('/') {
            let path = format!("{}/", base.path());
            base.set_path(&path);
        }
        let mut auth = HeaderValue::from_str(&format!(
            "MediaBrowser Client=\"Marathon\", Device=\"marathond\", DeviceId=\"marathond-{source_id}\", \
             Version=\"{}\", Token=\"{api_key}\"",
            env!("CARGO_PKG_VERSION")
        ))
        .map_err(|_| JellyfinError::Client("the API key contains invalid characters".to_owned()))?;
        auth.set_sensitive(true);
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .build()
            .map_err(|e| JellyfinError::Client(e.to_string()))?;
        Ok(Self {
            source_id: source_id.to_owned(),
            base,
            http,
            auth,
            libraries,
        })
    }

    /// `segments` appended to the base URL, percent-encoded.
    fn url(&self, segments: &[&str]) -> Url {
        let mut url = self.base.clone();
        // Cannot fail: `new` only accepts http(s) URLs, which can be a base.
        if let Ok(mut path) = url.path_segments_mut() {
            path.pop_if_empty();
            path.extend(segments);
        }
        url
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: Url) -> Result<T, JellyfinError> {
        let response = self
            .http
            .get(url.clone())
            .header(header::AUTHORIZATION, self.auth.clone())
            .header(header::ACCEPT, "application/json")
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|e| JellyfinError::Request(e.to_string()))?;
        let status = response.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(JellyfinError::Unauthorized(status));
        }
        if !status.is_success() {
            return Err(JellyfinError::Status {
                status,
                url: url.to_string(),
            });
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|e| JellyfinError::Request(e.to_string()))?;
        serde_json::from_slice(&bytes).map_err(|e| JellyfinError::Decode {
            url: url.to_string(),
            message: e.to_string(),
        })
    }

    /// The libraries to scan: all of them, or those named in config.
    pub async fn libraries(&self) -> Result<Vec<Library>, JellyfinError> {
        let all: Vec<Library> = self
            .get_json(self.url(&["Library", "VirtualFolders"]))
            .await?;
        Ok(match &self.libraries {
            None => all,
            Some(wanted) => {
                for name in wanted {
                    if !all.iter().any(|l| &l.name == name) {
                        tracing::warn!(source = %self.source_id, library = %name, "no such Jellyfin library");
                    }
                }
                all.into_iter()
                    .filter(|l| wanted.contains(&l.name))
                    .collect()
            }
        })
    }

    /// Every video in `library`.
    pub async fn library_items(
        &self,
        library: &Library,
    ) -> Result<Vec<JellyfinItem>, JellyfinError> {
        let Some(parent) = &library.item_id else {
            tracing::warn!(source = %self.source_id, library = %library.name, "library has no item ID; skipping it");
            return Ok(Vec::new());
        };
        let mut items = Vec::new();
        loop {
            let mut url = self.url(&["Items"]);
            url.query_pairs_mut()
                .append_pair("ParentId", parent)
                .append_pair("Recursive", "true")
                .append_pair("IncludeItemTypes", "Episode,Movie,Video,MusicVideo")
                .append_pair("ExcludeLocationTypes", "Virtual")
                .append_pair("Fields", "Path,MediaSources")
                .append_pair("SortBy", "SortName")
                .append_pair("StartIndex", &items.len().to_string())
                .append_pair("Limit", &PAGE_SIZE.to_string());
            let page: ItemsPage = self.get_json(url).await?;
            let got = page.items.len();
            items.extend(page.items);
            if got == 0 || items.len() as u64 >= page.total_record_count {
                return Ok(items);
            }
        }
    }

    /// Lists every video in the configured libraries.
    pub async fn scan(&self) -> Result<Vec<Found>, JellyfinError> {
        let mut found = Vec::new();
        let mut paths = std::collections::HashSet::new();
        for library in self.libraries().await? {
            let items = self.library_items(&library).await?;
            tracing::debug!(source = %self.source_id, library = %library.name, items = items.len(), "listed Jellyfin library");
            for item in &items {
                let mut entry = to_found(&library, item);
                // Two items can map to one path (several versions of a
                // film, or metadata fallbacks that coincide); paths must
                // be unique within a source.
                if !paths.insert(entry.path.clone()) {
                    entry.path = format!("{} [{}]", entry.path, item.id);
                    paths.insert(entry.path.clone());
                }
                found.push(entry);
            }
        }
        Ok(found)
    }

    /// Proxies a stream request for the Jellyfin item `jellyfin_id` (see
    /// the module docs).
    pub async fn stream(
        &self,
        jellyfin_id: &str,
        method: &Method,
        headers: &HeaderMap,
    ) -> Result<Response, ApiError> {
        let mut url = self.url(&["Videos", jellyfin_id, "stream"]);
        url.query_pairs_mut().append_pair("static", "true");
        let mut request = self
            .http
            .request(method.clone(), url)
            .header(header::AUTHORIZATION, self.auth.clone());
        for name in FORWARDED_REQUEST_HEADERS {
            if let Some(value) = headers.get(&name) {
                request = request.header(name, value.clone());
            }
        }
        let upstream = request.send().await.map_err(|e| {
            ApiError::Upstream(format!(
                "cannot reach Jellyfin source {}: {e}",
                self.source_id
            ))
        })?;
        let status = upstream.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(ApiError::Upstream(format!(
                "Jellyfin source {} refused the API key ({status})",
                self.source_id
            )));
        }
        if status == StatusCode::NOT_FOUND {
            return Err(ApiError::NotFound(format!(
                "the item is gone from Jellyfin source {}",
                self.source_id
            )));
        }
        if status.is_server_error() {
            return Err(ApiError::Upstream(format!(
                "Jellyfin source {} failed to stream the item ({status})",
                self.source_id
            )));
        }
        let mut response = Response::builder().status(status);
        for name in FORWARDED_RESPONSE_HEADERS {
            if let Some(value) = upstream.headers().get(&name) {
                response = response.header(name, value.clone());
            }
        }
        let body = if *method == Method::HEAD {
            Body::empty()
        } else {
            Body::from_stream(upstream.bytes_stream())
        };
        response
            .body(body)
            .map_err(|e| ApiError::Upstream(format!("cannot relay Jellyfin's answer: {e}")))
    }
}

/// Turns a Jellyfin item into a catalog entry. See the module docs for the
/// path.
pub fn to_found(library: &Library, item: &JellyfinItem) -> Found {
    let is_episode = item.kind.as_deref() == Some("Episode");
    let name = item
        .name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .or_else(|| item.path.as_deref().map(file_stem))
        .unwrap_or_else(|| item.id.clone());
    let relative = item
        .path
        .as_deref()
        .and_then(|p| relative_to(p, &library.locations))
        .unwrap_or_else(|| {
            let mut parts = Vec::new();
            if is_episode && let Some(series) = &item.series_name {
                parts.push(component(series));
                if let Some(season) = item.parent_index_number {
                    parts.push(format!("Season {season}"));
                }
            }
            parts.push(component(&name));
            parts.join("/")
        });
    Found {
        path: format!("{}/{relative}", component(&library.name)),
        size: item
            .media_sources
            .first()
            .and_then(|m| m.size)
            .and_then(|s| u64::try_from(s).ok())
            .unwrap_or(0),
        mtime_ms: 0,
        subtitles: Vec::new(),
        metadata: Some(ParsedName {
            show: item.series_name.clone().filter(|_| is_episode),
            season: item.parent_index_number.filter(|_| is_episode),
            episode: item.index_number.filter(|_| is_episode),
            title: name,
        }),
        external_id: Some(item.id.clone()),
        duration_ms: item
            .run_time_ticks
            .filter(|t| *t > 0)
            .map(|ticks| ticks / 10_000),
    }
}

/// `path` relative to whichever of `locations` contains it, `/`-separated.
/// Handles Windows paths from a Jellyfin server on Windows.
fn relative_to(path: &str, locations: &[String]) -> Option<String> {
    let path = path.replace('\\', "/");
    locations.iter().find_map(|location| {
        let location = location.replace('\\', "/");
        let rest = path.strip_prefix(location.trim_end_matches('/'))?;
        let rest = rest.strip_prefix('/')?;
        (!rest.is_empty()).then(|| rest.to_owned())
    })
}

/// A file name without its directory or extension.
fn file_stem(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_owned(),
        _ => name.to_owned(),
    }
}

/// A name made safe to be one path component.
fn component(name: &str) -> String {
    let cleaned = name.replace('/', "-").trim().to_owned();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "_".to_owned()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library() -> Library {
        Library {
            name: "TV Shows".into(),
            locations: vec!["/media/tv".into(), "/mnt/more-tv/".into()],
            item_id: Some("lib1".into()),
        }
    }

    fn episode(path: Option<&str>) -> JellyfinItem {
        JellyfinItem {
            id: "ep1".into(),
            name: Some("The Injury".into()),
            kind: Some("Episode".into()),
            series_name: Some("The Office (US)".into()),
            parent_index_number: Some(3),
            index_number: Some(14),
            run_time_ticks: Some(13_200_000_000),
            path: path.map(Into::into),
            media_sources: vec![MediaSource {
                size: Some(123_456),
            }],
        }
    }

    #[test]
    fn episodes_keep_their_folder_layout_and_metadata() {
        let found = to_found(
            &library(),
            &episode(Some(
                "/media/tv/The Office (US)/Season 3/The Office - S03E14.mkv",
            )),
        );
        assert_eq!(
            found.path,
            "TV Shows/The Office (US)/Season 3/The Office - S03E14.mkv"
        );
        let meta = found.metadata.unwrap();
        assert_eq!(meta.show.as_deref(), Some("The Office (US)"));
        assert_eq!((meta.season, meta.episode), (Some(3), Some(14)));
        assert_eq!(meta.title, "The Injury");
        // 13.2e9 ticks of 100ns = 22 minutes.
        assert_eq!(found.duration_ms, Some(1_320_000));
        assert_eq!(found.size, 123_456);
        assert_eq!(found.external_id.as_deref(), Some("ep1"));
    }

    #[test]
    fn paths_resolve_against_any_location_including_windows_ones() {
        let found = to_found(
            &library(),
            &episode(Some("/mnt/more-tv/Show/Season 1/a.mkv")),
        );
        assert_eq!(found.path, "TV Shows/Show/Season 1/a.mkv");
        let windows = Library {
            locations: vec![r"D:\Media\TV".into()],
            ..library()
        };
        let found = to_found(&windows, &episode(Some(r"D:\Media\TV\Show\Season 1\a.mkv")));
        assert_eq!(found.path, "TV Shows/Show/Season 1/a.mkv");
        // A location that is only a prefix of a folder name does not count.
        let found = to_found(&library(), &episode(Some("/media/tv-old/x.mkv")));
        assert_eq!(found.path, "TV Shows/The Office (US)/Season 3/The Injury");
    }

    #[test]
    fn items_without_a_known_file_get_a_path_from_metadata() {
        let found = to_found(&library(), &episode(None));
        assert_eq!(found.path, "TV Shows/The Office (US)/Season 3/The Injury");
        let movie = JellyfinItem {
            id: "m1".into(),
            name: Some("Heat / Director's Cut".into()),
            kind: Some("Movie".into()),
            // Episode fields on a non-episode are ignored.
            series_name: Some("Nope".into()),
            index_number: Some(2),
            ..JellyfinItem::default()
        };
        let found = to_found(
            &Library {
                name: "Films".into(),
                ..library()
            },
            &movie,
        );
        assert_eq!(found.path, "Films/Heat - Director's Cut");
        let meta = found.metadata.unwrap();
        assert_eq!((meta.show, meta.episode), (None, None));
        assert_eq!(found.duration_ms, None);
        assert_eq!(found.size, 0);
    }

    #[test]
    fn a_nameless_item_is_named_after_its_file() {
        let item = JellyfinItem {
            id: "v1".into(),
            kind: Some("Video".into()),
            path: Some("/elsewhere/clips/Why Doors Matter.webm".into()),
            ..JellyfinItem::default()
        };
        let found = to_found(&library(), &item);
        assert_eq!(found.path, "TV Shows/Why Doors Matter");
        assert_eq!(found.metadata.unwrap().title, "Why Doors Matter");
    }

    #[test]
    fn urls_keep_a_path_prefix_and_never_carry_the_key() {
        let client =
            JellyfinClient::new("jf", "https://example.com/jellyfin", "s3cret", None).unwrap();
        assert_eq!(
            client.url(&["Videos", "a b", "stream"]).as_str(),
            "https://example.com/jellyfin/Videos/a%20b/stream"
        );
        assert!(client.auth.is_sensitive());
        let debug = format!("{client:?}");
        assert!(!debug.contains("s3cret"), "{debug}");
    }
}
