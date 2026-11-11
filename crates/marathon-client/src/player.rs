//! The [`Player`] trait: what the [`Tuner`](crate::tuner::Tuner) needs from a
//! video player. The desktop client implements it with libmpv; tests use a
//! mock.

/// A sidecar subtitle to load alongside a video.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitleTrack {
    /// Absolute URL of the subtitle file.
    pub url: String,
    /// Track title to show, e.g. `en.forced`. May be empty.
    pub title: String,
    pub language: Option<String>,
}

/// What to play.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadRequest {
    /// Absolute URL of the video stream.
    pub url: String,
    /// Where to start, in milliseconds.
    pub start_ms: u64,
    /// Sidecar subtitles to add once the video has loaded.
    pub subtitles: Vec<SubtitleTrack>,
}

/// A player failure that is not about one particular file.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("player error: {0}")]
pub struct PlayerError(pub String);

/// Something the player reports back to the tuner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerEvent {
    /// The most recently loaded file opened and started playing. Ends a
    /// streak of failures.
    Started,
    /// The most recently loaded file played to its end. Must not be sent
    /// for files that were replaced or stopped, nor for any file but the
    /// latest loaded one.
    Ended,
    /// The most recently loaded file could not be opened or played. The
    /// tuner skips ahead.
    Failed(String),
}

/// A video player the tuner drives. Implementations play one file at a
/// time; [`Player::load`] replaces whatever is playing and starts playback
/// (unpaused).
pub trait Player {
    /// Replaces the current file with `request` and plays it from
    /// `request.start_ms`.
    fn load(&mut self, request: &LoadRequest) -> Result<(), PlayerError>;
    /// Stops playback and unloads the file.
    fn stop(&mut self);
    fn set_paused(&mut self, paused: bool);
    fn is_paused(&self) -> bool;
    /// Seeks by `seconds` (negative is backwards) from the current position.
    fn seek_relative(&mut self, seconds: i32);
    /// Playback position in the current file, or `None` if nothing is
    /// loaded yet (e.g. still opening the stream).
    fn position_ms(&self) -> Option<u64>;
    /// Duration of the current file, if known.
    fn duration_ms(&self) -> Option<u64>;
    /// Turns subtitles on or off.
    fn toggle_subtitles(&mut self);
}
