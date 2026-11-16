//! A client's stored playback settings (PLAN.md M13): what it can play and
//! which quality it chose. The server keeps them so that the calls a client
//! already makes (`now`, `advance`, `previous`) can answer with a URL for
//! the right quality, without the client sending its capabilities each time.

use marathon_api::{DEFAULT_CLIENT, PROFILE_AUTO, PROFILE_ORIGINAL, PlaybackSettings};

use crate::app::App;
use crate::error::ApiError;
use crate::playback::now_ms;
use crate::profiles;

impl App {
    /// The ladder's profile IDs, from the `[transcode]` table (or its
    /// defaults when the server has none).
    fn ladder_ids(&self) -> Vec<String> {
        let config = self.config().transcode.clone().unwrap_or_default();
        profiles::ladder(&config)
            .into_iter()
            .map(|r| r.id)
            .collect()
    }

    /// The settings stored for `client_id`: what it last `PUT`, or the
    /// defaults (the original files) for a client that has stored none.
    pub async fn playback_settings(&self, client_id: &str) -> Result<PlaybackSettings, ApiError> {
        let Some((capabilities, profile, measured_kbps)) =
            self.db().client_playback(client_id).await?
        else {
            return Ok(PlaybackSettings::default());
        };
        let capabilities = serde_json::from_str(&capabilities).unwrap_or_else(|err| {
            tracing::warn!(client = client_id, %err,
                "discarding unreadable stored capabilities");
            Default::default()
        });
        Ok(PlaybackSettings {
            capabilities,
            profile,
            measured_kbps: measured_kbps.and_then(|k| u32::try_from(k).ok()),
        })
    }

    /// Stores `settings` for `client_id`, replacing what it had.
    ///
    /// `400` for an empty or reserved client ID, or a profile that is not
    /// `original`, `auto`, or in the ladder. Storing a profile the server
    /// cannot serve (`720p` on a server without transcoding) is fine: it
    /// plays the original until the server can do better.
    pub async fn set_playback_settings(
        &self,
        client_id: &str,
        settings: PlaybackSettings,
    ) -> Result<PlaybackSettings, ApiError> {
        if client_id.is_empty() || client_id == DEFAULT_CLIENT {
            return Err(ApiError::BadRequest(format!(
                "{client_id:?} cannot be a client ID"
            )));
        }
        let ladder = self.ladder_ids();
        let known = settings.profile == PROFILE_ORIGINAL
            || settings.profile == PROFILE_AUTO
            || ladder.contains(&settings.profile);
        if !known {
            let mut valid = vec![PROFILE_ORIGINAL.to_owned(), PROFILE_AUTO.to_owned()];
            valid.extend(ladder);
            return Err(ApiError::BadRequest(format!(
                "unknown profile {:?}; this server has {}",
                settings.profile,
                valid.join(", ")
            )));
        }
        self.db()
            .set_client_playback(
                client_id,
                &serde_json::to_string(&settings.capabilities)?,
                &settings.profile,
                settings.measured_kbps.map(i64::from),
                now_ms(),
            )
            .await?;
        Ok(settings)
    }
}
