//! Thumbnails: one frame of an item as a small JPEG, made by ffmpeg on
//! request (`GET /thumbnail/{item_id}?at_ms=&width=`).
//!
//! They let a client that isn't showing the video (the Android client in
//! audio-only mode, its lock screen, and Android Auto) show roughly what's
//! on, refreshing a picture now and then instead of streaming video.
//!
//! Thumbnails use the same ffmpeg as audio streams, so they are offered
//! exactly when audio streams are (`now` carries an `audio_url`); without
//! it the route answers `503 unavailable`. ffmpeg seeks in the input
//! (`-ss` before `-i`), so a frame from the middle of a local file takes a
//! fraction of a second; from a Jellyfin stream it takes longer.
//! [`MAX_RUNNING`] limit how many run at once, separately from audio
//! streams, so a burst of thumbnails can't starve someone's listening.

use std::ffi::OsString;
use std::process::Stdio;
use std::time::Duration;

use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use tokio::process::Command;

use crate::audio::{Audio, Input, file_url};
use crate::error::ApiError;

/// How many thumbnails ffmpeg may make at once.
pub(crate) const MAX_RUNNING: usize = 2;

/// How long one thumbnail may take, seeking included.
const TIMEOUT: Duration = Duration::from_secs(20);

/// The width a thumbnail gets when the request doesn't say.
pub(crate) const DEFAULT_WIDTH: u32 = 480;

/// The widths a request may ask for. Wider than this is a video, not a
/// thumbnail.
pub(crate) const MIN_WIDTH: u32 = 32;
pub(crate) const MAX_WIDTH: u32 = 1280;

/// The most bytes of JPEG accepted from ffmpeg (a 1280-wide frame is
/// far smaller).
const MAX_BYTES: usize = 4 * 1024 * 1024;

impl Audio {
    /// Makes a `width`-pixel-wide JPEG of `input`'s frame at `at_ms`.
    ///
    /// Fails with `503 unavailable` when [`MAX_RUNNING`] are already being
    /// made, and `502 upstream` when ffmpeg produces no picture (the item
    /// has no video, `at_ms` is past its end, or its source is
    /// unreadable).
    pub(crate) async fn thumbnail(
        &self,
        input: Input,
        at_ms: u64,
        width: u32,
    ) -> Result<Response, ApiError> {
        let _permit = self.thumbnails.try_acquire().map_err(|_| {
            ApiError::Unavailable("too many thumbnails are being made; try again later".to_owned())
        })?;
        let mut command = Command::new(&self.ffmpeg);
        command
            .args(ffmpeg_args(&input, at_ms, width))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let output = match tokio::time::timeout(TIMEOUT, command.output()).await {
            Err(_) => {
                return Err(ApiError::Upstream(format!(
                    "ffmpeg made no thumbnail within {TIMEOUT:?}"
                )));
            }
            Ok(Err(err)) => return Err(ApiError::Internal(err.into())),
            Ok(Ok(output)) => output,
        };
        if !output.status.success() || output.stdout.is_empty() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = stderr.trim();
            tracing::info!(status = %output.status, ffmpeg = detail, at_ms, "no thumbnail");
            return Err(ApiError::Upstream(format!(
                "ffmpeg made no thumbnail at {at_ms} ms: {}",
                if detail.is_empty() {
                    "no picture there (no video, or past the end)"
                } else {
                    detail
                }
            )));
        }
        if output.stdout.len() > MAX_BYTES {
            return Err(ApiError::Upstream(format!(
                "ffmpeg's thumbnail is over {MAX_BYTES} bytes"
            )));
        }
        Ok((
            [
                (header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg")),
                // The same item, time, and width always make the same picture.
                (
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("private, max-age=86400"),
                ),
            ],
            output.stdout,
        )
            .into_response())
    }
}

/// ffmpeg's arguments for a `width`-wide JPEG of `input` at `at_ms`.
pub(crate) fn ffmpeg_args(input: &Input, at_ms: u64, width: u32) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["-nostdin", "-hide_banner", "-loglevel", "error"]
        .into_iter()
        .map(OsString::from)
        .collect();
    if let Input::Http { headers, .. } = input {
        args.push("-headers".into());
        args.push(headers.into());
    }
    // Before `-i`: a fast seek in the input, to the keyframe before
    // `at_ms` and then decoding up to it.
    args.push("-ss".into());
    args.push(format!("{}.{:03}", at_ms / 1000, at_ms % 1000).into());
    args.push("-i".into());
    args.push(match input {
        Input::File(path) => file_url(path),
        Input::Http { url, .. } => url.into(),
    });
    args.push("-map".into());
    args.push("0:v:0".into());
    args.push("-an".into());
    args.push("-sn".into());
    args.push("-dn".into());
    args.push("-frames:v".into());
    args.push("1".into());
    // Scale to `width` by the display aspect ratio (so anamorphic video
    // isn't squashed), with an even height, and square pixels.
    args.push("-vf".into());
    args.push(format!("scale={width}:trunc({width}/dar/2)*2,setsar=1").into());
    for arg in ["-c:v", "mjpeg", "-q:v", "5", "-f", "image2pipe", "pipe:1"] {
        args.push(arg.into());
    }
    args
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn strings(args: Vec<OsString>) -> Vec<String> {
        args.into_iter().map(|a| a.into_string().unwrap()).collect()
    }

    #[test]
    fn ffmpeg_args_for_a_file() {
        let args = ffmpeg_args(&Input::File(PathBuf::from("/m/a b.mkv")), 61_005, 320);
        assert_eq!(
            strings(args).join(" "),
            "-nostdin -hide_banner -loglevel error -ss 61.005 -i file:/m/a b.mkv \
             -map 0:v:0 -an -sn -dn -frames:v 1 \
             -vf scale=320:trunc(320/dar/2)*2,setsar=1 \
             -c:v mjpeg -q:v 5 -f image2pipe pipe:1"
        );
    }

    #[test]
    fn ffmpeg_args_for_jellyfin_send_headers_before_the_input() {
        let input = Input::Http {
            url: "http://jf/Videos/x/stream?static=true".to_owned(),
            headers: "Authorization: MediaBrowser Token=\"k\"\r\n".to_owned(),
        };
        let args = strings(ffmpeg_args(&input, 0, 480));
        let at = |s: &str| args.iter().position(|a| a == s).unwrap();
        assert!(at("-headers") < at("-i"));
        assert!(at("-ss") < at("-i"));
        assert_eq!(args[at("-ss") + 1], "0.000");
        assert_eq!(args[at("-i") + 1], "http://jf/Videos/x/stream?static=true");
    }
}
