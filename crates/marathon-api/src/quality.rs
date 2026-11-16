//! Quality profiles and client capabilities (PLAN.md M13): what a client can
//! play, and the choices the server offers it as a result.
//!
//! Codec and container names are ffprobe's (`h264`, `hevc`, `av1`, `vp9`,
//! `aac`, `ac3`, `eac3`, `opus`, `matroska`, `mov`), which is how the server
//! describes its media, so the two sides compare names directly.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// The profile that streams the original file: today's `/stream`.
pub const PROFILE_ORIGINAL: &str = "original";

/// The profile that picks the original or a transcode per item, to fit what
/// the client can play and its measured bandwidth.
pub const PROFILE_AUTO: &str = "auto";

/// One video codec a client can decode, and how well.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct VideoCodecSupport {
    /// ffprobe's codec name, e.g. `h264`.
    pub codec: String,
    /// The tallest picture the client decodes in this codec, in pixels.
    pub max_height: u32,
    /// Whether it decodes 10-bit video in this codec.
    #[serde(default)]
    pub ten_bit: bool,
}

/// What a client can play. Sent with `PUT /clients/{id}/playback` and
/// `POST /playback/profiles`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Capabilities {
    /// Video codecs the client decodes. Empty means none: every video would
    /// be transcoded.
    #[serde(default)]
    pub video: Vec<VideoCodecSupport>,
    /// Audio codecs the client decodes. Empty means none.
    #[serde(default)]
    pub audio_codecs: Vec<String>,
    /// Container formats the client opens, by ffprobe's first name
    /// (`matroska`, `mov`). Empty means any.
    #[serde(default)]
    pub containers: Vec<String>,
    /// The most audio channels the client plays without downmixing. `0`
    /// means no limit.
    #[serde(default)]
    pub max_audio_channels: u32,
    /// Whether the client plays HLS (needed for transcoded streams).
    #[serde(default)]
    pub hls: bool,
    /// The height of the client's screen, in pixels, if it knows: the
    /// server never sends more picture than this.
    #[serde(default)]
    pub screen_height: Option<u32>,
}

fn original() -> String {
    PROFILE_ORIGINAL.to_owned()
}

/// A client's playback settings: what `GET` and `PUT
/// /clients/{id}/playback` carry. The server uses them to decide, for each
/// item, between the original file and a transcode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PlaybackSettings {
    /// What the client can play.
    #[serde(default)]
    pub capabilities: Capabilities,
    /// The quality the client chose: [`PROFILE_ORIGINAL`] (the default),
    /// [`PROFILE_AUTO`], or the ID of a transcoding profile from `POST
    /// /playback/profiles`.
    #[serde(default = "original")]
    pub profile: String,
    /// The client's last measured throughput, in kbit/s, for `auto`.
    #[serde(default)]
    pub measured_kbps: Option<u32>,
}

impl Default for PlaybackSettings {
    /// A client that has said nothing: it plays the original files.
    fn default() -> Self {
        Self {
            capabilities: Capabilities::default(),
            profile: original(),
            measured_kbps: None,
        }
    }
}

/// `POST /playback/profiles`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProfilesRequest {
    pub capabilities: Capabilities,
    /// The client's measured throughput from `GET /bandwidth`, in kbit/s.
    #[serde(default)]
    pub measured_kbps: Option<u32>,
}

/// What kind of stream a profile gives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProfileKind {
    /// The original file, untouched.
    Original,
    /// Original or transcoded, chosen per item.
    Auto,
    /// Always transcoded to this quality (unless the original is already
    /// compatible and smaller).
    Transcode,
}

/// A quality a client may choose, from `POST /playback/profiles`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProfileOption {
    /// The ID to use as the client's profile, e.g. `720p`.
    pub id: String,
    /// A name to show, e.g. `720p · 4 Mbps`.
    pub name: String,
    pub kind: ProfileKind,
    /// Target video bitrate, in kbit/s. `None` for `original` and `auto`.
    pub video_kbps: Option<u32>,
    /// Tallest picture, in pixels. `None` for `original` and `auto`.
    pub max_height: Option<u32>,
    /// Whether the client's measured bandwidth (if it gave one) is enough
    /// for this profile with headroom. Always true without a measurement.
    pub fits_bandwidth: bool,
    /// The one the server suggests: `auto` when there is a measurement,
    /// else `original`.
    pub recommended: bool,
}
