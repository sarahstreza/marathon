//! What the server knows about an item's media (PLAN.md M13): its
//! container, duration, bitrate and streams, as ffprobe reports them.
//!
//! [`MediaInfo`] is what gets stored in `items.media_json`, so its JSON
//! shape is a persistence format: add fields with `#[serde(default)]`,
//! never rename or remove them. [`MediaInfo::from_ffprobe`] is pure (it
//! parses ffprobe's `-print_format json` output) so it is tested without
//! ffmpeg; running ffprobe lives with the probing code.
//!
//! This is deliberately a small summary of what profile selection and
//! transcoding need, not a mirror of ffprobe's output.

use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use marathon_api::Now;
use serde::{Deserialize, Serialize};
use tokio::process::Command;

use crate::app::App;
use crate::audio::{Input, file_url};
use crate::http::safe_join;
use crate::sources::Source;

/// How long probing one item may take. ffprobe reads only the container's
/// headers, so this is generous even for a network share.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// The facts about one media file that decide how it can be streamed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct MediaInfo {
    /// ffprobe's `format_name`, e.g. `matroska,webm` or `mov,mp4,m4a,3gp,3g2,mj2`.
    pub container: String,
    pub duration_ms: Option<u64>,
    /// Whole-file bitrate (video, audio, and everything else), in kbit/s.
    pub bitrate_kbps: Option<u32>,
    /// The first real video stream (cover art does not count). `None` for
    /// an audio-only file.
    pub video: Option<VideoStream>,
    /// Audio tracks, in file order.
    pub audio: Vec<AudioTrack>,
    /// Subtitle tracks, in file order.
    pub subtitles: Vec<SubtitleStream>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct VideoStream {
    /// ffprobe's codec name: `h264`, `hevc`, `av1`, `vp9`, `mpeg2video`...
    pub codec: String,
    /// e.g. `High`, `Main 10`. Not all codecs have one.
    pub profile: Option<String>,
    /// Bits per colour channel: 8, 10 or 12.
    pub bit_depth: u8,
    pub width: u32,
    pub height: u32,
    pub fps: Option<f32>,
    /// Set when the stream is HDR and needs tone mapping to look right on
    /// an SDR output.
    pub hdr: Option<Hdr>,
}

/// The HDR transfer function of a video stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Hdr {
    /// SMPTE ST 2084, used by HDR10 and Dolby Vision.
    Pq,
    /// ARIB STD-B67, hybrid log-gamma.
    Hlg,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct AudioTrack {
    pub codec: String,
    pub channels: u32,
    /// ISO 639 language tag from the file, if any.
    pub language: Option<String>,
    /// Whether the file marks this as the default track.
    pub default: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct SubtitleStream {
    pub codec: String,
    pub language: Option<String>,
    /// Text subtitles can be converted to WebVTT; image subtitles (PGS,
    /// VobSub) cannot.
    pub text: bool,
}

/// Subtitle codecs that are text, by ffprobe name.
const TEXT_SUBTITLE_CODECS: &[&str] = &[
    "subrip",
    "srt",
    "ass",
    "ssa",
    "webvtt",
    "mov_text",
    "text",
    "subviewer",
    "microdvd",
];

impl MediaInfo {
    /// Parses ffprobe's JSON (`-print_format json -show_format
    /// -show_streams`).
    pub(crate) fn from_ffprobe(json: &str) -> Result<Self, serde_json::Error> {
        let probe: Probe = serde_json::from_str(json)?;
        let format = probe.format.unwrap_or_default();

        let video = probe
            .streams
            .iter()
            .find(|s| s.codec_type == "video" && !s.disposition.attached_pic)
            .and_then(VideoStream::from_stream);
        let audio = probe
            .streams
            .iter()
            .filter(|s| s.codec_type == "audio")
            .map(|s| AudioTrack {
                codec: s.codec_name.clone().unwrap_or_default(),
                channels: s.channels.unwrap_or(0),
                language: s.language(),
                default: s.disposition.default,
            })
            .collect();
        let subtitles = probe
            .streams
            .iter()
            .filter(|s| s.codec_type == "subtitle")
            .map(|s| {
                let codec = s.codec_name.clone().unwrap_or_default();
                SubtitleStream {
                    text: TEXT_SUBTITLE_CODECS.contains(&codec.as_str()),
                    codec,
                    language: s.language(),
                }
            })
            .collect();

        Ok(Self {
            container: format.format_name.unwrap_or_default(),
            duration_ms: format.duration.as_deref().and_then(seconds_to_ms),
            bitrate_kbps: format
                .bit_rate
                .as_deref()
                .and_then(|b| b.parse::<u64>().ok())
                .map(|bits| u32::try_from(bits / 1000).unwrap_or(u32::MAX)),
            video,
            audio,
            subtitles,
        })
    }
}

impl VideoStream {
    fn from_stream(s: &Stream) -> Option<Self> {
        Some(Self {
            codec: s.codec_name.clone()?,
            profile: s.profile.clone(),
            bit_depth: s.bit_depth(),
            width: s.width?,
            height: s.height?,
            fps: s.avg_frame_rate.as_deref().and_then(parse_fraction),
            hdr: match s.color_transfer.as_deref() {
                Some("smpte2084") => Some(Hdr::Pq),
                Some("arib-std-b67") => Some(Hdr::Hlg),
                _ => None,
            },
        })
    }
}

/// `"1234.567"` seconds as whole milliseconds; `None` if it is not a
/// positive, finite number (ffprobe says `N/A` for unknown).
fn seconds_to_ms(text: &str) -> Option<u64> {
    let seconds: f64 = text.trim().parse().ok()?;
    // Positive and finite, so the cast cannot wrap; it saturates for
    // absurd values.
    (seconds.is_finite() && seconds > 0.0).then(|| (seconds * 1000.0).round() as u64)
}

/// `"24000/1001"` as a number; `None` for `0/0` and other nonsense.
fn parse_fraction(text: &str) -> Option<f32> {
    let (num, den) = text.split_once('/')?;
    let (num, den): (f32, f32) = (num.parse().ok()?, den.parse().ok()?);
    (den > 0.0 && num > 0.0).then(|| num / den)
}

// --- ffprobe's JSON ---------------------------------------------------------

#[derive(Deserialize)]
struct Probe {
    #[serde(default)]
    streams: Vec<Stream>,
    format: Option<Format>,
}

#[derive(Deserialize, Default)]
struct Format {
    format_name: Option<String>,
    // ffprobe prints numbers as strings here.
    duration: Option<String>,
    bit_rate: Option<String>,
}

#[derive(Deserialize)]
struct Stream {
    #[serde(default)]
    codec_type: String,
    codec_name: Option<String>,
    profile: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    pix_fmt: Option<String>,
    bits_per_raw_sample: Option<String>,
    avg_frame_rate: Option<String>,
    color_transfer: Option<String>,
    channels: Option<u32>,
    #[serde(default)]
    tags: Tags,
    #[serde(default)]
    disposition: Disposition,
}

#[derive(Deserialize, Default)]
struct Tags {
    language: Option<String>,
}

#[derive(Deserialize, Default)]
struct Disposition {
    #[serde(default, deserialize_with = "int_flag")]
    default: bool,
    #[serde(default, deserialize_with = "int_flag")]
    attached_pic: bool,
}

/// ffprobe writes dispositions as `0` / `1`.
fn int_flag<'de, D: serde::Deserializer<'de>>(de: D) -> Result<bool, D::Error> {
    Ok(u8::deserialize(de)? != 0)
}

impl Stream {
    /// The language tag, unless the file leaves it undetermined.
    fn language(&self) -> Option<String> {
        self.tags
            .language
            .clone()
            .filter(|l| !l.is_empty() && l != "und")
    }

    /// Bits per colour channel: the stream's own claim if it makes one,
    /// else read from the pixel format name (`yuv420p10le`, `p010le`).
    fn bit_depth(&self) -> u8 {
        if let Some(bits) = self
            .bits_per_raw_sample
            .as_deref()
            .and_then(|b| b.parse::<u8>().ok())
            .filter(|b| *b > 0)
        {
            return bits;
        }
        match self.pix_fmt.as_deref() {
            Some(f) if f.contains("p010") || f.contains("10le") || f.contains("10be") => 10,
            Some(f) if f.contains("12le") || f.contains("12be") => 12,
            _ => 8,
        }
    }
}

// --- Probing ---------------------------------------------------------------

/// ffprobe's arguments for printing `input`'s format and streams as JSON.
fn ffprobe_args(input: &Input) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["-v", "error", "-print_format", "json"]
        .into_iter()
        .map(OsString::from)
        .collect();
    args.push("-show_format".into());
    args.push("-show_streams".into());
    if let Input::Http { headers, .. } = input {
        args.push("-headers".into());
        args.push(headers.into());
    }
    args.push(match input {
        Input::File(path) => file_url(path),
        Input::Http { url, .. } => url.into(),
    });
    args
}

/// Runs ffprobe on `input`.
async fn probe(ffprobe: &Path, input: &Input) -> Result<MediaInfo, String> {
    let mut command = Command::new(ffprobe);
    command
        .args(ffprobe_args(input))
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let output = match tokio::time::timeout(PROBE_TIMEOUT, command.output()).await {
        Err(_) => return Err(format!("ffprobe took over {PROBE_TIMEOUT:?}")),
        Ok(Err(err)) => return Err(format!("cannot run ffprobe: {err}")),
        Ok(Ok(output)) => output,
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "ffprobe failed ({}): {}",
            output.status,
            stderr.trim()
        ));
    }
    MediaInfo::from_ffprobe(&String::from_utf8_lossy(&output.stdout))
        .map_err(|err| format!("cannot read ffprobe's output: {err}"))
}

impl App {
    /// What is known about an item's media, probing it (once) if need be.
    ///
    /// The result is stored in `items.media_json` and served from there
    /// until a rescan sees the file change. A probe that fails is
    /// remembered in memory, so a broken file is not retried on every
    /// tune-in. `None` when the server has no ffprobe, the item is
    /// missing, or probing failed.
    ///
    /// Probing also fills in a missing duration.
    pub(crate) async fn media_info(&self, item_id: &str) -> Option<MediaInfo> {
        let audio = self.inner.audio.as_ref()?;
        let ffprobe = audio.ffprobe.as_ref()?;
        let row = self.db().item(item_id).await.ok().flatten()?;
        if let Some(json) = self.db().media_json(item_id).await.ok().flatten() {
            match serde_json::from_str(&json) {
                Ok(info) => return Some(info),
                Err(err) => {
                    tracing::warn!(item = item_id, %err, "stored media info unreadable; probing again");
                }
            }
        }
        if audio.is_unprobeable(item_id) || row.missing {
            return None;
        }
        let input = match self.source(&row.source_id)? {
            Source::Filesystem { root } => Input::File(safe_join(root, &row.path)?),
            Source::Jellyfin(client) => {
                let (url, headers) = client.ffmpeg_input(row.external_id.as_deref()?);
                Input::Http { url, headers }
            }
        };
        match probe(ffprobe, &input).await {
            Ok(info) => {
                tracing::debug!(item = item_id, "probed media");
                match serde_json::to_string(&info) {
                    Ok(json) => {
                        if let Err(err) = self.db().set_media_json(item_id, &json).await {
                            tracing::warn!(item = item_id, %err, "cannot store probed media info");
                        }
                    }
                    Err(err) => tracing::warn!(item = item_id, %err, "cannot encode media info"),
                }
                if row.duration_ms.is_none()
                    && let Some(duration_ms) = info.duration_ms
                    && let Err(err) = self
                        .db()
                        .set_duration(item_id, i64::try_from(duration_ms).unwrap_or(i64::MAX))
                        .await
                {
                    tracing::warn!(item = item_id, %err, "cannot store probed duration");
                }
                Some(info)
            }
            Err(reason) => {
                tracing::info!(item = item_id, path = %row.path, %reason,
                    "cannot probe the item; it stays without media info");
                audio.mark_unprobeable(item_id);
                None
            }
        }
    }

    /// Fills in `now`'s missing durations by probing, when ffprobe is
    /// available (see `audio`'s module docs). The current item is probed
    /// before returning, so the client gets its duration; the next item is
    /// probed in the background, ready for when the channel gets to it.
    ///
    /// Never fails: an item that cannot be probed keeps no duration.
    pub(crate) async fn with_probed_media(&self, mut now: Now) -> Now {
        if now.item.duration_ms.is_none() {
            now.item.duration_ms = self
                .media_info(&now.item.id)
                .await
                .and_then(|info| info.duration_ms);
        }
        if let Some(next) = &mut now.next
            && next.item.id == now.item.id
        {
            // A one-item channel: its next item is this one again.
            next.item.duration_ms = now.item.duration_ms;
        }
        if let Some(next) = &now.next
            && next.item.duration_ms.is_none()
        {
            let app = self.clone();
            let id = next.item.id.clone();
            tokio::spawn(async move { app.media_info(&id).await });
        }
        now
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An MKV like a typical rip: 10-bit HDR10 HEVC, two audio tracks,
    /// text and image subtitles, and a cover image.
    const HDR_MKV: &str = r#"{
      "streams": [
        {"codec_type": "video", "codec_name": "hevc", "profile": "Main 10",
         "width": 3840, "height": 2160, "pix_fmt": "yuv420p10le",
         "avg_frame_rate": "24000/1001", "color_transfer": "smpte2084",
         "disposition": {"default": 1, "attached_pic": 0}},
        {"codec_type": "audio", "codec_name": "eac3", "channels": 6,
         "tags": {"language": "eng"}, "disposition": {"default": 1}},
        {"codec_type": "audio", "codec_name": "aac", "channels": 2,
         "tags": {"language": "und"}, "disposition": {"default": 0}},
        {"codec_type": "subtitle", "codec_name": "subrip",
         "tags": {"language": "eng"}, "disposition": {"default": 0}},
        {"codec_type": "subtitle", "codec_name": "hdmv_pgs_subtitle",
         "tags": {"language": "jpn"}, "disposition": {"default": 0}},
        {"codec_type": "video", "codec_name": "mjpeg", "width": 600, "height": 900,
         "avg_frame_rate": "0/0", "disposition": {"attached_pic": 1}}
      ],
      "format": {"format_name": "matroska,webm", "duration": "5025.512000",
                 "bit_rate": "18250123"}
    }"#;

    #[test]
    fn parses_an_hdr_mkv() {
        let info = MediaInfo::from_ffprobe(HDR_MKV).unwrap();
        assert_eq!(info.container, "matroska,webm");
        assert_eq!(info.duration_ms, Some(5_025_512));
        assert_eq!(info.bitrate_kbps, Some(18_250));

        let video = info.video.unwrap();
        assert_eq!(video.codec, "hevc");
        assert_eq!(video.profile.as_deref(), Some("Main 10"));
        assert_eq!((video.width, video.height), (3840, 2160));
        assert_eq!(video.bit_depth, 10);
        assert_eq!(video.hdr, Some(Hdr::Pq));
        assert!((video.fps.unwrap() - 23.976).abs() < 0.001);

        assert_eq!(info.audio.len(), 2);
        assert_eq!(info.audio[0].codec, "eac3");
        assert_eq!(info.audio[0].channels, 6);
        assert_eq!(info.audio[0].language.as_deref(), Some("eng"));
        assert!(info.audio[0].default);
        assert_eq!(info.audio[1].language, None, "`und` is no language");
        assert!(!info.audio[1].default);

        assert_eq!(info.subtitles.len(), 2);
        assert!(info.subtitles[0].text);
        assert!(!info.subtitles[1].text, "PGS is an image");
    }

    #[test]
    fn cover_art_is_not_the_video_stream() {
        let json = r#"{"streams": [
            {"codec_type": "video", "codec_name": "mjpeg", "width": 500, "height": 500,
             "disposition": {"attached_pic": 1}},
            {"codec_type": "audio", "codec_name": "mp3", "channels": 2}
          ], "format": {"format_name": "mp3", "duration": "180.0"}}"#;
        let info = MediaInfo::from_ffprobe(json).unwrap();
        assert_eq!(info.video, None);
        assert_eq!(info.audio.len(), 1);
    }

    #[test]
    fn bit_depth_comes_from_the_stream_or_its_pixel_format() {
        let stream = |pix_fmt: &str, raw: Option<&str>| Stream {
            codec_type: "video".into(),
            codec_name: Some("h264".into()),
            profile: None,
            width: Some(1),
            height: Some(1),
            pix_fmt: Some(pix_fmt.into()),
            bits_per_raw_sample: raw.map(Into::into),
            avg_frame_rate: None,
            color_transfer: None,
            channels: None,
            tags: Tags::default(),
            disposition: Disposition::default(),
        };
        assert_eq!(stream("yuv420p", None).bit_depth(), 8);
        assert_eq!(stream("yuv420p10le", None).bit_depth(), 10);
        assert_eq!(stream("p010le", None).bit_depth(), 10);
        assert_eq!(stream("yuv444p12le", None).bit_depth(), 12);
        assert_eq!(stream("yuv420p", Some("10")).bit_depth(), 10);
        assert_eq!(stream("yuv420p10le", Some("0")).bit_depth(), 10);
    }

    #[test]
    fn hlg_is_hdr_and_bt709_is_not() {
        let video = |transfer: &str| {
            format!(
                r#"{{"streams": [{{"codec_type": "video", "codec_name": "hevc",
                "width": 1920, "height": 1080, "color_transfer": "{transfer}"}}],
                "format": {{"format_name": "mov,mp4"}}}}"#
            )
        };
        let hlg = MediaInfo::from_ffprobe(&video("arib-std-b67")).unwrap();
        assert_eq!(hlg.video.unwrap().hdr, Some(Hdr::Hlg));
        let sdr = MediaInfo::from_ffprobe(&video("bt709")).unwrap();
        assert_eq!(sdr.video.unwrap().hdr, None);
    }

    #[test]
    fn missing_and_unknown_values_are_none() {
        let json = r#"{"streams": [], "format": {"format_name": "x", "duration": "N/A",
                       "bit_rate": "N/A"}}"#;
        let info = MediaInfo::from_ffprobe(json).unwrap();
        assert_eq!(info.duration_ms, None);
        assert_eq!(info.bitrate_kbps, None);
        assert_eq!(info.video, None);
        assert!(info.audio.is_empty());
        assert!(MediaInfo::from_ffprobe("not json").is_err());
    }

    #[test]
    fn survives_a_json_round_trip() {
        let info = MediaInfo::from_ffprobe(HDR_MKV).unwrap();
        let json = serde_json::to_string(&info).unwrap();
        assert_eq!(serde_json::from_str::<MediaInfo>(&json).unwrap(), info);
    }

    fn strings(args: Vec<OsString>) -> Vec<String> {
        args.into_iter().map(|a| a.into_string().unwrap()).collect()
    }

    #[test]
    fn ffprobe_args_for_a_file_keep_odd_names_as_files() {
        let args = strings(ffprobe_args(&Input::File("/m/-x: http.mkv".into())));
        assert_eq!(
            args.join(" "),
            "-v error -print_format json -show_format -show_streams file:/m/-x: http.mkv"
        );
    }

    #[test]
    fn ffprobe_args_for_jellyfin_send_headers_before_the_url() {
        let args = strings(ffprobe_args(&Input::Http {
            url: "http://jf/Videos/x/stream?static=true".into(),
            headers: "Authorization: k\r\n".into(),
        }));
        let at = |s: &str| args.iter().position(|a| a == s).unwrap();
        assert_eq!(args[at("-headers") + 1], "Authorization: k\r\n");
        assert_eq!(
            args.last().unwrap(),
            "http://jf/Videos/x/stream?static=true"
        );
    }
}
