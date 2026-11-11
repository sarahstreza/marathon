//! [`MpvPlayer`]: the tuner's [`Player`], backed by libmpv.

use marathon_client::player::{LoadRequest, Player, PlayerError, PlayerEvent, SubtitleTrack};
use marathon_mpv::{EndFileReason, Event, Mpv};

/// mpv options for the desktop client, set before mpv initializes.
pub fn mpv_options() -> Vec<(&'static str, &'static str)> {
    vec![
        // Render through the render API into our GL context.
        ("vo", "libmpv"),
        ("hwdec", "auto-safe"),
        // Stay alive with nothing loaded (between items, before tuning).
        ("idle", "yes"),
        // Marathon is the UI: no on-screen controller, bundled scripts,
        // or youtube-dl hook.
        ("load-scripts", "no"),
        ("ytdl", "no"),
        ("osd-level", "0"),
        // Sidecar subtitles come from the server (PLAN.md §2); mpv cannot
        // find them next to an HTTP URL anyway.
        ("sub-auto", "no"),
        ("network-timeout", "15"),
    ]
}

/// Makes mpv send `headers` with every HTTP request (streams and
/// subtitles), so the server's token and client ID reach it.
pub fn set_http_headers(mpv: &Mpv, headers: &[(String, String)]) -> marathon_mpv::Result<()> {
    for (name, value) in headers {
        // `append` adds one entry; setting the whole list would split
        // values on commas.
        mpv.command(&[
            "change-list",
            "http-header-fields",
            "append",
            &format!("{name}: {value}"),
        ])?;
    }
    Ok(())
}

/// Plays one item at a time in mpv, and turns mpv's events into the
/// tuner's [`PlayerEvent`]s.
///
/// Every load replaces mpv's playlist with a single entry. mpv numbers
/// entries with increasing IDs, so remembering the ID of the latest load
/// lets [`MpvPlayer::on_event`] ignore events from files that were
/// replaced (e.g. the end of the previous channel's item racing a channel
/// switch).
pub struct MpvPlayer {
    mpv: Mpv,
    /// Playlist entry ID of the latest load.
    entry: Option<i64>,
    /// Playlist entry ID mpv most recently started.
    started: Option<i64>,
    /// Subtitles to add once the latest load has opened.
    pending_subtitles: Vec<SubtitleTrack>,
}

impl MpvPlayer {
    pub fn new(mpv: Mpv) -> Self {
        Self {
            mpv,
            entry: None,
            started: None,
            pending_subtitles: Vec::new(),
        }
    }

    pub fn mpv(&self) -> &Mpv {
        &self.mpv
    }

    /// Handles an mpv event, returning what it means to the tuner, if
    /// anything.
    pub fn on_event(&mut self, event: &Event) -> Option<PlayerEvent> {
        match event {
            Event::StartFile { playlist_entry_id } => {
                self.started = Some(*playlist_entry_id);
                None
            }
            Event::FileLoaded => {
                // FILE_LOADED carries no entry ID; it belongs to the entry
                // that started last.
                if self.started.is_some() && self.started == self.entry {
                    self.add_subtitles();
                    Some(PlayerEvent::Started)
                } else {
                    None
                }
            }
            Event::EndFile {
                playlist_entry_id,
                reason,
            } => classify_end(self.entry, *playlist_entry_id, reason),
            _ => None,
        }
    }

    fn add_subtitles(&mut self) {
        for (i, sub) in std::mem::take(&mut self.pending_subtitles)
            .into_iter()
            .enumerate()
        {
            // Select the first sidecar track: having one is a sign the
            // viewer wants it.
            let flag = if i == 0 { "select" } else { "auto" };
            let lang = sub.language.as_deref().unwrap_or("");
            if let Err(err) = self
                .mpv
                .command(&["sub-add", &sub.url, flag, &sub.title, lang])
            {
                tracing::warn!(url = %sub.url, %err, "cannot add subtitle");
            }
        }
    }

    fn property_ms(&self, name: &str) -> Option<u64> {
        let seconds = self.mpv.get_property_double(name).ok()?;
        (seconds.is_finite() && seconds >= 0.0).then(|| (seconds * 1000.0).round() as u64)
    }
}

/// Decides what an `END_FILE` means: only the latest load's end counts.
fn classify_end(latest: Option<i64>, ended: i64, reason: &EndFileReason) -> Option<PlayerEvent> {
    if latest != Some(ended) {
        return None;
    }
    match reason {
        EndFileReason::Eof => Some(PlayerEvent::Ended),
        EndFileReason::Error(err) => Some(PlayerEvent::Failed(err.to_string())),
        // Stopped or replaced by us, quitting, or a redirect (whose entries
        // mpv plays next): nothing for the tuner to do.
        _ => None,
    }
}

impl Player for MpvPlayer {
    fn load(&mut self, request: &LoadRequest) -> Result<(), PlayerError> {
        let err = |e: marathon_mpv::Error| PlayerError(e.to_string());
        // `start` is a global option that applies to the next file loaded;
        // it is set before every load, so it never leaks between items.
        let start = format!("{:.3}", request.start_ms as f64 / 1000.0);
        self.mpv.set_property_string("start", &start).map_err(err)?;
        self.mpv.set_property_flag("pause", false).map_err(err)?;
        self.mpv
            .command(&["loadfile", &request.url, "replace"])
            .map_err(err)?;
        // `loadfile replace` has replaced the playlist with this one entry
        // by the time it returns, and only this thread changes the
        // playlist, so entry 0 is ours.
        self.entry = Some(self.mpv.get_property_i64("playlist/0/id").map_err(err)?);
        self.pending_subtitles = request.subtitles.clone();
        tracing::debug!(url = %request.url, start, entry = ?self.entry, "loading");
        Ok(())
    }

    fn stop(&mut self) {
        self.entry = None;
        if let Err(err) = self.mpv.command(&["stop"]) {
            tracing::warn!(%err, "cannot stop playback");
        }
    }

    fn set_paused(&mut self, paused: bool) {
        if let Err(err) = self.mpv.set_property_flag("pause", paused) {
            tracing::warn!(%err, "cannot set pause");
        }
    }

    fn is_paused(&self) -> bool {
        self.mpv.get_property_flag("pause").unwrap_or(false)
    }

    fn seek_relative(&mut self, seconds: i32) {
        if let Err(err) = self
            .mpv
            .command(&["seek", &seconds.to_string(), "relative"])
        {
            // Fails harmlessly when nothing is loaded yet.
            tracing::debug!(%err, "cannot seek");
        }
    }

    fn position_ms(&self) -> Option<u64> {
        self.property_ms("time-pos")
    }

    fn duration_ms(&self) -> Option<u64> {
        self.property_ms("duration")
    }

    fn toggle_subtitles(&mut self) {
        // Cycles through the subtitle tracks and "off"; with one track
        // (the usual sidecar case) that is a toggle.
        match self.mpv.command(&["cycle", "sub"]) {
            Ok(()) => {
                let sid = self.mpv.get_property_string("sid").unwrap_or_default();
                tracing::info!(track = %sid, "subtitles");
            }
            Err(err) => tracing::debug!(%err, "cannot cycle subtitles"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_latest_load_reports_its_end() {
        let eof = EndFileReason::Eof;
        assert_eq!(classify_end(Some(3), 3, &eof), Some(PlayerEvent::Ended));
        // The previous item ending (or being replaced) after a new load.
        assert_eq!(classify_end(Some(3), 2, &eof), None);
        assert_eq!(classify_end(None, 3, &eof), None);
    }

    #[test]
    fn end_reasons() {
        assert_eq!(classify_end(Some(1), 1, &EndFileReason::Stop), None);
        assert_eq!(classify_end(Some(1), 1, &EndFileReason::Quit), None);
        assert_eq!(classify_end(Some(1), 1, &EndFileReason::Redirect), None);
        let failed = EndFileReason::Error(marathon_mpv::Error::Create);
        assert!(matches!(
            classify_end(Some(1), 1, &failed),
            Some(PlayerEvent::Failed(_))
        ));
    }

    #[test]
    fn headers_are_appended_whole() {
        let (mpv, _events) = Mpv::new([("vo", "null"), ("ao", "null")]).unwrap();
        let headers = [
            ("X-Marathon-Client".to_owned(), "abc".to_owned()),
            ("authorization".to_owned(), "Bearer a,b".to_owned()),
        ];
        set_http_headers(&mpv, &headers).unwrap();
        // The string form joins entries with commas, so it cannot show
        // that "Bearer a,b" stayed one entry, but it does show both were
        // added after the (empty) default.
        assert_eq!(
            mpv.get_property_string("http-header-fields").unwrap(),
            "X-Marathon-Client: abc,authorization: Bearer a,b"
        );
    }

    #[test]
    fn options_are_accepted_by_mpv() {
        Mpv::new(mpv_options().into_iter().filter(|(k, _)| *k != "vo")).unwrap();
    }

    /// Drives a real (headless) mpv through the Player trait: a load at a
    /// start position plays to the end, and a replaced load is ignored.
    #[test]
    fn plays_through_mpv_and_reports_only_the_latest_end() {
        let (mpv, mut events) =
            Mpv::new([("vo", "null"), ("ao", "null"), ("idle", "yes")]).unwrap();
        let mut player = MpvPlayer::new(mpv);
        let clip = |d: &str| LoadRequest {
            url: format!("av://lavfi:testsrc=duration={d}:size=32x32:rate=25"),
            start_ms: 0,
            subtitles: Vec::new(),
        };
        player.load(&clip("30")).unwrap();
        // Replace it before it ends; then play a short clip from 0.5s.
        player
            .load(&LoadRequest {
                start_ms: 500,
                ..clip("1")
            })
            .unwrap();
        assert!(!player.is_paused());

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut reported = Vec::new();
        let mut saw_position = None;
        while std::time::Instant::now() < deadline {
            let Some(event) = events.wait(0.5) else {
                continue;
            };
            if event == Event::PlaybackRestart {
                saw_position = player.position_ms();
            }
            if let Some(e) = player.on_event(&event) {
                let done = e != PlayerEvent::Started;
                reported.push(e);
                if done {
                    break;
                }
            }
        }
        assert_eq!(reported, vec![PlayerEvent::Started, PlayerEvent::Ended]);
        let position = saw_position.expect("playback started");
        assert!((480..=1000).contains(&position), "{position}");
    }
}
