//! Request/response types for the Marathon HTTP API (`/api/v1`), shared by the
//! server and Rust clients. The OpenAPI document is derived from these types.
//!
//! This crate must not depend on any other Marathon crate.
//!
//! # Conventions
//!
//! - Times and positions are integer milliseconds.
//! - URLs in responses (`stream_url`, subtitle `url`) are absolute paths such
//!   as `/api/v1/stream/<id>`. Clients resolve them against the server's base
//!   URL; the server does not know which host name a client used to reach it.
//! - Errors are returned as [`ErrorBody`] with a non-2xx status.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

mod control;

pub use control::{
    Action, ActionAccepted, ClientInfo, ClientPlaying, Direction, Event, RelayedEvent,
};

/// The client ID that `POST /clients/{id}/actions` resolves to the most
/// recently active connected client.
pub const DEFAULT_CLIENT: &str = "default";

/// Path prefix of every API route.
pub const API_PREFIX: &str = "/api/v1";

/// Version of the API served under [`API_PREFIX`].
pub const API_VERSION: u32 = 1;

/// Header a client uses to identify itself: a UUID generated once per client
/// installation and stored locally. Recorded in the event log.
pub const CLIENT_ID_HEADER: &str = "X-Marathon-Client";

/// `GET /health`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Health {
    /// Always `"ok"` when the server can answer.
    pub status: String,
}

/// `GET /version`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct VersionInfo {
    /// API version; see [`API_VERSION`].
    pub api_version: u32,
    /// Version of the server software.
    pub server_version: String,
}

/// A sidecar subtitle file attached to an item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Subtitle {
    /// Index used in the subtitle URL.
    pub index: u32,
    /// Language tag taken from the file name (`en` in `Episode.en.srt`), if
    /// any.
    pub language: Option<String>,
    /// Everything between the video's name and the extension (`en.forced` in
    /// `Episode.en.forced.srt`), suitable as a track title. Empty if none.
    pub label: String,
    /// File format, the lowercase extension: `srt`, `vtt`, `ass`, or `ssa`.
    pub format: String,
    /// Where to fetch the file (an absolute path on the server).
    pub url: String,
}

/// One playable video.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Item {
    /// Stable server-assigned ID.
    pub id: String,
    /// ID of the configured source the item comes from.
    pub source_id: String,
    /// Path within the source, `/`-separated.
    pub path: String,
    /// Parsed show name, if any.
    pub show: Option<String>,
    /// Parsed season number, if any.
    pub season: Option<u32>,
    /// Parsed episode number, if any.
    pub episode: Option<u32>,
    /// Episode title if one was parsed, otherwise the file name without its
    /// extension.
    pub title: String,
    /// Duration, once a client or catalog provider has reported it.
    pub duration_ms: Option<u64>,
    /// Sidecar subtitles. Subtitles embedded in the video are not listed.
    pub subtitles: Vec<Subtitle>,
    /// True if the file was not found by the most recent scan.
    pub missing: bool,
}

/// A channel named in a resolution path (PLAN.md M11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ChannelRef {
    pub id: String,
    pub name: String,
}

/// A channel as listed by `GET /channels`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ChannelSummary {
    pub id: String,
    /// The channel number. `0` for a hidden channel configured without
    /// one.
    pub number: u32,
    pub name: String,
    /// Number of playable (not missing) items in the channel, counting
    /// those of its child channels (PLAN.md M11).
    pub item_count: u32,
    /// What the channel is on, or `None` if it has no playable items.
    pub current: Option<Item>,
    /// Saved playback position within `current`.
    pub position_ms: u64,
    /// Whether the channel is hidden: left out of the guide and channel
    /// up/down, and listed only with `?include_hidden=true`. A hidden
    /// channel can still be tuned by ID.
    #[serde(default)]
    pub hidden: bool,
    /// The channels `current` is resolved through: this channel first,
    /// then any nested child channels. Just this channel for an ordinary
    /// channel; empty if it has nothing to play.
    #[serde(default)]
    pub resolution_path: Vec<ChannelRef>,
}

/// An item to prefetch: what the channel will play after the current item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct NextItem {
    pub item: Item,
    pub stream_url: String,
}

/// What a channel is playing: the response to `GET /channels/{id}/now`,
/// `advance`, and `previous`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Now {
    pub channel_id: String,
    pub channel_number: u32,
    pub channel_name: String,
    /// The current item.
    pub item: Item,
    /// Saved playback position within `item`. This is the raw saved
    /// position; clients apply the resume rewind and the finished threshold
    /// (PLAN.md §1) themselves.
    pub position_ms: u64,
    /// Where to stream `item` from.
    pub stream_url: String,
    /// The item the channel will advance to, for prefetching. Computed
    /// without moving the channel. `None` only if it cannot be determined.
    pub next: Option<NextItem>,
    /// The channels `item` was resolved through (PLAN.md M11): this
    /// channel first, then each nested child channel, ending with the one
    /// `item` belongs to. Just this channel for an ordinary channel.
    #[serde(default)]
    pub resolution_path: Vec<ChannelRef>,
    /// Problems the server stepped past to find `item`, such as a child
    /// channel with nothing to play. Clients report each as an `error`
    /// event; playback carries on.
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// One item of `GET /channels/{id}/plan`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PlannedItem {
    pub item: Item,
    /// The channels `item` would be resolved through, as in [`Now`].
    pub resolution_path: Vec<ChannelRef>,
    /// How far ahead of now the item would start, if the channel plays
    /// without interruption from its saved position. Items whose
    /// duration is unknown count as [`PLAN_UNKNOWN_DURATION_MS`].
    pub starts_in_ms: u64,
}

/// What `GET /channels/{id}/plan` assumes an item of unknown duration
/// lasts: 30 minutes.
pub const PLAN_UNKNOWN_DURATION_MS: u64 = 30 * 60 * 1000;

/// `POST /channels/{id}/progress`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProgressReport {
    /// The item the client is playing. Must be the channel's current item;
    /// otherwise the report is stale and rejected with `409`.
    pub item_id: String,
    pub position_ms: u64,
    /// The item's duration, if the player knows it.
    #[serde(default)]
    pub duration_ms: Option<u64>,
    /// Whether playback is paused.
    #[serde(default)]
    pub paused: bool,
}

/// Why a channel is advancing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum AdvanceReason {
    /// The player reached the end of the item.
    Ended,
    /// The viewer skipped to the next item.
    Skipped,
}

/// `POST /channels/{id}/advance`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdvanceRequest {
    /// The item the client is advancing from. If it is not the channel's
    /// current item, the channel has already moved on and the request
    /// returns the current [`Now`] without advancing again.
    pub from_item_id: String,
    pub reason: AdvanceReason,
}

/// `POST /channels/{id}/previous`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PreviousRequest {
    /// The item the client is stepping back from. Like `advance`, the
    /// request only moves the channel if this is its current item.
    pub from_item_id: String,
}

/// Kinds of playback event in the history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum EventKind {
    /// A client tuned to the channel (`GET now`).
    Tuned,
    /// A periodic progress report.
    Progress,
    /// The item played to the end and the channel advanced.
    Ended,
    /// The viewer skipped the item and the channel advanced.
    Skipped,
    /// The viewer stepped back to the previous item.
    Previous,
    /// A progress report with playback paused.
    Paused,
    /// The first unpaused progress report after a pause.
    Resumed,
}

impl EventKind {
    /// The lowercase name used in JSON and in the database.
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Tuned => "tuned",
            EventKind::Progress => "progress",
            EventKind::Ended => "ended",
            EventKind::Skipped => "skipped",
            EventKind::Previous => "previous",
            EventKind::Paused => "paused",
            EventKind::Resumed => "resumed",
        }
    }

    /// Parses the name returned by [`EventKind::as_str`].
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "tuned" => EventKind::Tuned,
            "progress" => EventKind::Progress,
            "ended" => EventKind::Ended,
            "skipped" => EventKind::Skipped,
            "previous" => EventKind::Previous,
            "paused" => EventKind::Paused,
            "resumed" => EventKind::Resumed,
            _ => return None,
        })
    }
}

impl From<AdvanceReason> for EventKind {
    fn from(reason: AdvanceReason) -> Self {
        match reason {
            AdvanceReason::Ended => EventKind::Ended,
            AdvanceReason::Skipped => EventKind::Skipped,
        }
    }
}

/// One entry of `GET /history`. (Not to be confused with [`Event`], what
/// clients publish for observers.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HistoryEntry {
    pub id: String,
    /// When the event was recorded, in milliseconds since the Unix epoch.
    pub ts_ms: i64,
    /// The reporting client's `X-Marathon-Client` header, if it sent one.
    pub client_id: Option<String>,
    /// The channel the client was tuned to.
    pub channel_id: Option<String>,
    /// The channel `item_id` belongs to: `channel_id` itself, or for a
    /// channel with nested child channels (PLAN.md M11) the child that
    /// supplied the item. `None` for events recorded before nesting
    /// existed.
    #[serde(default)]
    pub resolved_channel_id: Option<String>,
    /// The channels the item was resolved through, `channel_id` first
    /// (see [`Now::resolution_path`]). Empty for events recorded before
    /// nesting existed.
    #[serde(default)]
    pub resolution_path: Vec<String>,
    pub item_id: Option<String>,
    pub kind: EventKind,
    /// Playback position within the item when the event happened.
    pub position_ms: Option<u64>,
}

/// `POST /library/rescan`: what the scan found.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ScanReport {
    /// Number of sources scanned successfully.
    pub sources_scanned: u32,
    /// Items found by this scan.
    pub items_present: u32,
    /// Items seen for the first time.
    pub added: u32,
    /// Known items whose file or parsed metadata changed.
    pub updated: u32,
    /// Items that were present before and are now missing.
    pub newly_missing: u32,
    /// Items that were missing and are back.
    pub restored: u32,
    /// Sources that could not be scanned, with the reason. Their items are
    /// left as they were.
    pub errors: Vec<String>,
}

/// Machine-readable error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The request was malformed.
    BadRequest,
    /// No or wrong bearer token.
    Unauthorized,
    /// The channel, item, or file does not exist.
    NotFound,
    /// The channel has no playable items.
    ChannelEmpty,
    /// A progress report named an item that is not the channel's current
    /// item (another client moved the channel on). Re-fetch `now`.
    StaleItem,
    /// A source the server depends on (a Jellyfin server) failed or could
    /// not be reached (status 502).
    Upstream,
    /// Something went wrong on the server.
    Internal,
}

/// Body of every error response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ErrorBody {
    pub code: ErrorCode,
    /// Human-readable description.
    pub message: String,
}

/// Paths of API routes, relative to the server root. Used by the server to
/// build URLs in responses and by clients to build requests.
pub mod paths {
    use super::API_PREFIX;

    /// `GET`: media bytes of an item.
    pub fn stream(item_id: &str) -> String {
        format!("{API_PREFIX}/stream/{item_id}")
    }

    /// `GET`: the `index`th sidecar subtitle of an item.
    pub fn subtitle(item_id: &str, index: u32) -> String {
        format!("{API_PREFIX}/subtitles/{item_id}/{index}")
    }

    /// `GET` (WebSocket): a client's control channel.
    pub fn clients_connect() -> String {
        format!("{API_PREFIX}/clients/connect")
    }

    /// `POST`: relay an action to a client (or to `default`).
    pub fn client_actions(client_id: &str) -> String {
        format!("{API_PREFIX}/clients/{client_id}/actions")
    }

    /// `GET`: the Server-Sent Events stream.
    pub fn events() -> String {
        format!("{API_PREFIX}/events")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_kind_names_round_trip() {
        for kind in [
            EventKind::Tuned,
            EventKind::Progress,
            EventKind::Ended,
            EventKind::Skipped,
            EventKind::Previous,
            EventKind::Paused,
            EventKind::Resumed,
        ] {
            assert_eq!(EventKind::parse(kind.as_str()), Some(kind));
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
        }
        assert_eq!(EventKind::parse("bogus"), None);
    }

    #[test]
    fn progress_report_optional_fields_default() {
        let report: ProgressReport =
            serde_json::from_str(r#"{"item_id": "a", "position_ms": 5}"#).unwrap();
        assert_eq!(report.duration_ms, None);
        assert!(!report.paused);
    }

    #[test]
    fn advance_reason_is_lowercase() {
        let req: AdvanceRequest =
            serde_json::from_str(r#"{"from_item_id": "a", "reason": "skipped"}"#).unwrap();
        assert_eq!(req.reason, AdvanceReason::Skipped);
    }
}
