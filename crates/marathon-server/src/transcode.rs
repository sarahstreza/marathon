//! Live transcoding (PLAN.md M13): the server's ability to re-encode an
//! item at a lower quality, which depends on what ffmpeg can do on the
//! machine it runs on.
//!
//! Like audio streams, transcoding is optional. [`Transcode::detect`] runs
//! when the server starts: it checks that ffmpeg and ffprobe work and picks
//! an encoder (see [`crate::encoder`]). Without a working H.264 encoder, or
//! with `[transcode] enabled = false`, the server runs without
//! transcoding and every client plays the original files.
//!
//! What is served is an HLS VOD playlist ([`crate::hls`]) and its segments,
//! encoded as they are asked for by a [`session`](crate::session). The two
//! methods here, [`App::transcode_playlist`] and [`App::transcode_segment`],
//! are what the routes call.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use uuid::Uuid;

use crate::app::App;
use crate::audio::Input;
use crate::config::TranscodeConfig;
use crate::encoder;
use crate::error::ApiError;
use crate::hls;
use crate::profiles;
use crate::session::{Plan, Registry, Settings};

/// Transcoding, when it is available.
#[derive(Debug)]
pub(crate) struct Transcode {
    /// The ffprobe program, which runs: items cannot be transcoded
    /// without probing.
    pub(crate) ffprobe: PathBuf,
    pub(crate) config: TranscodeConfig,
    pub(crate) registry: Arc<Registry>,
}

impl Transcode {
    /// Picks the encoder for `config` (the `[transcode]` table, `None`
    /// when the config has none) and returns the transcoding settings, or
    /// `None` if transcoding is off or unusable. Session files go under
    /// `data_dir` unless the config names another cache directory.
    ///
    /// Never fails: without a working encoder the server just has no
    /// transcoding. If `[transcode]` was configured, that is logged as a
    /// warning; without it, ffmpeg was only looked for, so it is logged as
    /// info.
    pub(crate) async fn detect(config: Option<&TranscodeConfig>, data_dir: &Path) -> Option<Self> {
        let explicit = config.is_some();
        let config = config.cloned().unwrap_or_default();
        if !config.enabled {
            tracing::info!("transcoding is turned off (transcode.enabled = false)");
            return None;
        }
        let Some(capabilities) =
            encoder::detect(&config.ffmpeg, config.hardware, &config.vaapi_device).await
        else {
            let ffmpeg = config.ffmpeg.display();
            if explicit {
                tracing::warn!(%ffmpeg, hardware = ?config.hardware,
                    "no H.264 encoder works with this ffmpeg; transcoding is off");
            } else {
                tracing::info!(%ffmpeg, "no working ffmpeg H.264 encoder found; transcoding is off");
            }
            return None;
        };
        if let Err(reason) = crate::audio::runs(&config.ffprobe).await {
            tracing::warn!(ffprobe = %config.ffprobe.display(), %reason,
                "ffprobe is not usable, so items cannot be probed; transcoding is off");
            return None;
        }
        tracing::info!(
            encoder = capabilities.encoder.name(),
            hardware = capabilities.encoder.is_hardware(),
            tonemap = ?capabilities.tonemap,
            max_sessions = config.max_sessions,
            "transcoding is on"
        );
        if capabilities.tonemap.is_none() {
            tracing::warn!(
                "no HDR tone mapping works with this ffmpeg; HDR video will look washed out \
                 when transcoded"
            );
        }

        let max_sessions = usize::try_from(config.max_sessions).unwrap_or(usize::MAX);
        // Share the CPU between the sessions that may run at once, so one
        // cannot take it all, but give each at least two threads.
        let cores = std::thread::available_parallelism().map_or(2, usize::from);
        let threads = u32::try_from((cores / max_sessions.max(1)).max(2)).unwrap_or(u32::MAX);
        let root = config
            .cache_dir
            .clone()
            .unwrap_or_else(|| data_dir.to_owned())
            .join("transcode");
        let registry = match Registry::new(
            root.clone(),
            Settings {
                ffmpeg: config.ffmpeg.clone(),
                encoder: capabilities,
                vaapi_device: config.vaapi_device.clone(),
                segment_seconds: config.segment_seconds,
                threads: Some(threads),
            },
            max_sessions,
            Duration::from_secs(u64::from(config.idle_timeout_s)),
        ) {
            Ok(registry) => Arc::new(registry),
            Err(err) => {
                tracing::warn!(dir = %root.display(), %err,
                    "cannot set up the transcoding cache directory; transcoding is off");
                return None;
            }
        };
        registry.spawn_reaper();
        Some(Self {
            ffprobe: config.ffprobe.clone(),
            config,
            registry,
        })
    }
}

impl App {
    fn transcoding(&self) -> Result<&Transcode, ApiError> {
        self.inner.transcode.as_ref().ok_or_else(|| {
            ApiError::Unavailable(
                "this server has no transcoding: it needs ffmpeg with an H.264 encoder, \
                 and transcode.enabled not set to false"
                    .to_owned(),
            )
        })
    }

    /// Works out what to transcode for `item_id` at `profile`: the media,
    /// where to read it, and the size to encode it at.
    async fn transcode_plan(
        &self,
        transcode: &Transcode,
        item_id: &str,
        profile: &str,
    ) -> Result<Plan, ApiError> {
        let not_found = || ApiError::NotFound(format!("no item {item_id}"));
        let row = self.db().item(item_id).await?.ok_or_else(not_found)?;
        if row.missing {
            return Err(not_found());
        }
        let ladder = profiles::ladder(&transcode.config);
        let rung = ladder
            .iter()
            .find(|r| r.id == profile)
            .ok_or_else(|| ApiError::NotFound(format!("no transcoding profile {profile:?}")))?;
        let media = self.media_info(item_id).await.ok_or_else(|| {
            ApiError::Upstream(format!("cannot read item {item_id}'s media with ffprobe"))
        })?;
        let source_height = media
            .video
            .as_ref()
            .ok_or_else(|| ApiError::BadRequest(format!("item {item_id} has no video")))?
            .height;
        let duration_ms = media
            .duration_ms
            .or_else(|| row.duration_ms.and_then(|d| u64::try_from(d).ok()))
            .filter(|d| *d > 0)
            .ok_or_else(|| ApiError::Upstream(format!("item {item_id} has no known duration")))?;
        let input = self.input_for_row(&row).ok_or_else(not_found)?;
        if let Input::File(path) = &input
            && !tokio::fs::try_exists(path).await.unwrap_or(false)
        {
            return Err(ApiError::NotFound(format!(
                "item {item_id}'s file is missing"
            )));
        }
        let (height, video_kbps) = profiles::output_for(rung, &media, source_height);
        Ok(Plan {
            input,
            media,
            duration_ms,
            height,
            video_kbps,
        })
    }

    /// `GET /transcode/{item}/{profile}/index.m3u8`: the playlist of the
    /// whole item, with the ID of the session its segments will belong to.
    /// Nothing is encoded, and no session exists, until a segment is asked
    /// for; the playlist only refuses if the server is already full.
    pub(crate) async fn transcode_playlist(
        &self,
        item_id: &str,
        profile: &str,
    ) -> Result<Response, ApiError> {
        let transcode = self.transcoding()?;
        let plan = self.transcode_plan(transcode, item_id, profile).await?;
        let duration_ms = plan.duration_ms;
        if !transcode.registry.has_room() {
            return Err(crate::session::too_many_sessions());
        }
        let id = Uuid::now_v7();
        let body = hls::playlist(
            duration_ms,
            transcode.registry.segment_seconds(),
            &id.to_string(),
        );
        Ok((
            [
                (
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/vnd.apple.mpegurl"),
                ),
                (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            ],
            body,
        )
            .into_response())
    }

    /// `GET /transcode/{item}/{profile}/{n}.ts?s=`: segment `n` of the
    /// session `session`, encoding it if need be. A session the server no
    /// longer has is made again, so a player that paused past the idle
    /// timeout carries on.
    pub(crate) async fn transcode_segment(
        &self,
        item_id: &str,
        profile: &str,
        n: u32,
        session: Option<&str>,
    ) -> Result<Response, ApiError> {
        let transcode = self.transcoding()?;
        let id = session
            .ok_or_else(|| ApiError::BadRequest("the segment URL needs the `s` session".into()))?;
        let id = Uuid::parse_str(id)
            .map_err(|_| ApiError::BadRequest(format!("{id:?} is not a session ID")))?;
        let session = match transcode.registry.get(&id) {
            Some(session) => session,
            None => {
                let plan = self.transcode_plan(transcode, item_id, profile).await?;
                transcode.registry.create(id, item_id, profile, plan)?
            }
        };
        if session.item_id != item_id || session.profile != profile {
            return Err(ApiError::NotFound(
                "that session is for another item or profile".to_owned(),
            ));
        }
        let bytes = session.segment(n).await?;
        Ok((
            [
                (header::CONTENT_TYPE, HeaderValue::from_static("video/mp2t")),
                (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            ],
            bytes,
        )
            .into_response())
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    #[tokio::test]
    async fn turned_off_or_missing_ffmpeg_means_no_transcoding() {
        let dir = tempfile::tempdir().unwrap();
        let off = TranscodeConfig {
            enabled: false,
            ..TranscodeConfig::default()
        };
        assert!(Transcode::detect(Some(&off), dir.path()).await.is_none());
        let missing = TranscodeConfig {
            ffmpeg: PathBuf::from("/nonexistent/ffmpeg"),
            ..TranscodeConfig::default()
        };
        assert!(
            Transcode::detect(Some(&missing), dir.path())
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn without_ffprobe_there_is_no_transcoding() {
        let dir = tempfile::tempdir().unwrap();
        let config = TranscodeConfig {
            ffprobe: PathBuf::from("/nonexistent/ffprobe"),
            hardware: crate::config::Hardware::Software,
            ..TranscodeConfig::default()
        };
        assert!(
            Transcode::detect(Some(&config), Path::new(dir.path()))
                .await
                .is_none()
        );
    }
}
