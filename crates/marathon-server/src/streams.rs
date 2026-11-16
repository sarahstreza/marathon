//! Choosing how each item reaches a client (PLAN.md M13): the original
//! file, or a transcode, according to the client's stored playback
//! settings ([`crate::client_playback`]).
//!
//! `now`, `advance` and `previous` build their answer with the original
//! file's URL and then pass it through [`App::with_streams`], which swaps
//! in a transcode's playlist where the client's profile calls for one, and
//! says which it chose in `stream`. A client that has stored nothing keeps
//! getting the original everywhere, so older clients see no change.
//!
//! Whenever a transcode was asked for but cannot be given (the server is
//! full or has no transcoding, the client cannot play HLS, or the item
//! cannot be probed), the original is served and `warnings` says why, so
//! playback never stops over it.

use marathon_api::{
    Capabilities, NextItem, Now, PROFILE_ORIGINAL, PlaybackSettings, StreamInfo, StreamKind, paths,
};

use crate::app::App;
use crate::profiles::{self, Choice, Decision};

/// What one item gets: its URL, and what that is.
struct Streaming {
    url: String,
    info: StreamInfo,
    /// Why a transcode was wanted but not given, if so.
    warning: Option<String>,
}

fn direct(item_id: &str, warning: Option<String>) -> Streaming {
    Streaming {
        url: paths::stream(item_id),
        info: StreamInfo {
            kind: StreamKind::Direct,
            profile: None,
            summary: "Original".to_owned(),
        },
        warning,
    }
}

impl App {
    /// Chooses the stream for `now`'s item and its next item, given
    /// `client_id`'s stored settings (the original files for a client that
    /// has none, or none was named).
    pub(crate) async fn with_streams(&self, mut now: Now, client_id: Option<&str>) -> Now {
        let settings = match client_id {
            Some(id) => self.playback_settings(id).await.unwrap_or_else(|err| {
                tracing::warn!(client = id, %err, "cannot read playback settings");
                PlaybackSettings::default()
            }),
            None => PlaybackSettings::default(),
        };
        let (current, next) = tokio::join!(self.stream_for(&now.item.id, &settings), async {
            match &now.next {
                Some(next) => Some(self.stream_for(&next.item.id, &settings).await),
                None => None,
            }
        });
        now.stream_url = current.url;
        now.stream = Some(current.info);
        now.warnings.extend(current.warning);
        if let (Some(slot), Some(next)) = (&mut now.next, next) {
            apply(slot, next);
        }
        now
    }

    /// The stream for one item.
    async fn stream_for(&self, item_id: &str, settings: &PlaybackSettings) -> Streaming {
        if settings.profile == PROFILE_ORIGINAL {
            return direct(item_id, None);
        }
        let Some(transcode) = self.inner.transcode.as_ref() else {
            // `auto` on a server that cannot transcode is just the
            // original; only a named quality is worth a warning.
            let warning = (settings.profile != marathon_api::PROFILE_AUTO).then(|| {
                format!(
                    "this server cannot transcode, so {} plays as the original",
                    settings.profile
                )
            });
            return direct(item_id, warning);
        };
        let Capabilities { hls, .. } = &settings.capabilities;
        if !hls {
            return direct(
                item_id,
                Some(
                    "this client cannot play HLS, so transcodes are off; playing the original"
                        .to_owned(),
                ),
            );
        }
        let Some(media) = self.media_info(item_id).await else {
            return direct(
                item_id,
                Some(
                    "cannot read the item's media to choose a quality; playing the original"
                        .to_owned(),
                ),
            );
        };

        let ladder = profiles::ladder(&transcode.config);
        let budget = settings
            .measured_kbps
            .map(|m| profiles::budget_kbps(m, transcode.config.auto_headroom_percent));
        let decision = profiles::decide(
            &media,
            &settings.capabilities,
            &Choice::parse(&settings.profile),
            &ladder,
            budget,
        );
        match decision {
            Decision::Direct => direct(item_id, None),
            Decision::Transcode {
                profile,
                height,
                video_kbps,
            } => {
                if !transcode.registry.has_room() {
                    return direct(
                        item_id,
                        Some("all transcoding sessions are busy; playing the original".to_owned()),
                    );
                }
                Streaming {
                    url: paths::transcode(item_id, &profile),
                    info: StreamInfo {
                        kind: StreamKind::Transcode,
                        summary: format!(
                            "Transcoding · {}",
                            profiles::rung_name(height, video_kbps)
                        ),
                        profile: Some(profile),
                    },
                    warning: None,
                }
            }
        }
    }
}

/// Puts a chosen stream into `next`. Its warning is not repeated: the
/// client reports the current item's, and the next one is chosen again
/// (with fresh information) when the channel gets to it.
fn apply(next: &mut NextItem, chosen: Streaming) {
    next.stream_url = chosen.url;
    next.stream = Some(chosen.info);
}
