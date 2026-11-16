//! ffmpeg's command line for transcoding an item (PLAN.md M13).
//!
//! One builder serves every kind of output: the HLS sessions that stream a
//! transcode live ([`hls_args`]) and, later, the files made for offline
//! downloads (M14), which differ only in the output half. Everything here
//! is pure and unit tested; [`crate::transcode`] runs the result.
//!
//! What the arguments do, in short:
//!
//! - seek to the start segment with `-ss` before the input (fast), and
//!   throttle reading to real time after an initial burst, so an idle
//!   encode does not run far ahead of the viewer;
//! - scale to the output height, then tone map if the source is HDR (so the
//!   expensive float conversion runs on the output's pixels), then hand the
//!   frames to the encoder;
//! - force a keyframe every segment, with no extra scene-cut keyframes, so
//!   segment boundaries are the same however many times ffmpeg restarts;
//! - stereo AAC for the audio track a player would start with.

use std::ffi::OsString;
use std::path::Path;

use crate::audio::{Input, file_url};
use crate::encoder::{Capabilities, Encoder};
use crate::media::MediaInfo;

/// The audio bitrate of every transcode, in kbit/s: stereo AAC.
const AUDIO_KBPS: u32 = crate::profiles::AUDIO_KBPS;

/// How many seconds of media ffmpeg may read ahead at full speed when it
/// starts, before it is held to real time. It is what makes a seek into
/// an unencoded part respond quickly.
const READ_BURST_SECONDS: u32 = 60;

/// What to transcode, and to what.
pub(crate) struct Job<'a> {
    pub input: &'a Input,
    pub media: &'a MediaInfo,
    /// The encoder and tone mapping detection chose.
    pub encoder: Capabilities,
    pub vaapi_device: &'a Path,
    /// Output picture height, in pixels.
    pub height: u32,
    /// Target video bitrate, in kbit/s.
    pub video_kbps: u32,
    /// How many threads a software encoder may use, if bounded.
    pub threads: Option<u32>,
}

/// Where HLS output goes and how it is cut.
pub(crate) struct HlsOutput<'a> {
    /// The first segment to produce; the input is sought to its start.
    pub start_segment: u32,
    pub segment_seconds: u32,
    /// Segments are written here as `seg_NNNNN.ts`.
    pub dir: &'a Path,
}

/// The name of segment `n`'s file in a session's directory.
pub(crate) fn segment_file_name(n: u32) -> String {
    format!("seg_{n:05}.ts")
}

fn os(args: &[&str]) -> Vec<OsString> {
    args.iter().map(OsString::from).collect()
}

/// ffmpeg's arguments for producing HLS segments from `start_segment` on.
pub(crate) fn hls_args(job: &Job<'_>, out: &HlsOutput<'_>) -> Vec<OsString> {
    let start_seconds = out.start_segment * out.segment_seconds;
    let mut args = os(&["-nostdin", "-hide_banner", "-loglevel", "error", "-nostats"]);
    args.extend(device_args(job));
    args.extend(source_args(job, start_seconds, true));
    args.extend(codec_args(job, out.segment_seconds));
    args.extend(os(&[
        // The segments carry their place in the item, so a restart at
        // segment N produces timestamps that continue the playlist.
        "-output_ts_offset",
        &start_seconds.to_string(),
        "-f",
        "hls",
        "-hls_time",
        &out.segment_seconds.to_string(),
        "-hls_list_size",
        "0",
        "-hls_segment_type",
        "mpegts",
        // `temp_file` writes each segment under another name and renames it
        // when it is whole, so a segment that exists is a complete one.
        "-hls_flags",
        "independent_segments+temp_file",
        "-start_number",
        &out.start_segment.to_string(),
        "-hls_segment_filename",
    ]));
    args.push(out.dir.join("seg_%05d.ts").into());
    // ffmpeg insists on writing a playlist of its own; nobody reads it.
    args.push(out.dir.join("ffmpeg.m3u8").into());
    args
}

/// Device set-up, which has to come before the input: the encoder's, and
/// a tone mapping filter's.
fn device_args(job: &Job<'_>) -> Vec<OsString> {
    let mut args = job.encoder.encoder.device_args(job.vaapi_device);
    if hdr(job)
        && let Some(tonemap) = job.encoder.tonemap
    {
        args.extend(tonemap.device_args().into_iter().map(OsString::from));
    }
    args
}

/// The input half: seek, optionally throttle, and the input itself.
pub(crate) fn source_args(job: &Job<'_>, start_seconds: u32, throttle: bool) -> Vec<OsString> {
    let mut args = Vec::new();
    if start_seconds > 0 {
        args.extend(os(&["-ss", &start_seconds.to_string()]));
    }
    if throttle {
        args.extend(os(&[
            "-readrate",
            "1",
            "-readrate_initial_burst",
            &READ_BURST_SECONDS.to_string(),
        ]));
    }
    if let Input::Http { headers, .. } = job.input {
        args.extend(os(&["-headers"]));
        args.push(headers.into());
    }
    args.push("-i".into());
    args.push(match job.input {
        Input::File(path) => file_url(path),
        Input::Http { url, .. } => url.into(),
    });
    args
}

/// The stream selection, filters and codecs, which do not depend on where
/// the output goes. A keyframe is forced every `keyframe_seconds`.
pub(crate) fn codec_args(job: &Job<'_>, keyframe_seconds: u32) -> Vec<OsString> {
    let mut args = os(&["-map", "0:v:0"]);
    let audio = audio_track(job.media);
    if let Some(track) = audio {
        args.extend(os(&["-map", &format!("0:a:{track}")]));
    }
    // No subtitles or data: players get embedded subtitles another way.
    args.extend(os(&["-sn", "-dn", "-vf", &video_filter(job)]));
    args.extend(encoder_args(job));
    args.extend(os(&[
        "-force_key_frames",
        &format!("expr:gte(t,n_forced*{keyframe_seconds})"),
    ]));
    if audio.is_some() {
        args.extend(os(&[
            "-c:a",
            "aac",
            "-b:a",
            &format!("{AUDIO_KBPS}k"),
            "-ac",
            "2",
        ]));
    }
    args
}

fn hdr(job: &Job<'_>) -> bool {
    job.media.video.as_ref().is_some_and(|v| v.hdr.is_some())
}

/// The index, among the file's audio tracks, of the one to transcode: the
/// default track, else the first. `None` if there is no audio.
fn audio_track(media: &MediaInfo) -> Option<usize> {
    if media.audio.is_empty() {
        return None;
    }
    Some(media.audio.iter().position(|t| t.default).unwrap_or(0))
}

/// The `-vf` chain: scale, tone map if needed, and hand to the encoder.
fn video_filter(job: &Job<'_>) -> String {
    let mut chain = vec![format!("scale=-2:{}", job.height)];
    match (hdr(job), job.encoder.tonemap) {
        // The tone mapping ends in `yuv420p`.
        (true, Some(tonemap)) => chain.push(tonemap.filter().to_owned()),
        _ if job.encoder.encoder.upload_filter().is_none() => chain.push("format=yuv420p".into()),
        _ => {}
    }
    if let Some(upload) = job.encoder.encoder.upload_filter() {
        chain.push(upload.to_owned());
    }
    chain.join(",")
}

fn encoder_args(job: &Job<'_>) -> Vec<OsString> {
    let mut args = match job.encoder.encoder {
        // `veryfast` keeps a software encode well inside real time on a
        // modest CPU; no scene-cut keyframes, so segments stay aligned.
        Encoder::X264 => os(&[
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-profile:v",
            "high",
            "-sc_threshold",
            "0",
        ]),
        Encoder::OpenH264 => os(&["-c:v", "libopenh264"]),
        // No preset: the p1-p7 presets need a recent driver, and an older
        // card is exactly where this runs.
        Encoder::Nvenc => os(&["-c:v", "h264_nvenc", "-forced-idr", "1"]),
        Encoder::Qsv => os(&["-c:v", "h264_qsv"]),
        Encoder::Vaapi => os(&["-c:v", "h264_vaapi"]),
        Encoder::VideoToolbox => os(&["-c:v", "h264_videotoolbox"]),
    };
    if let (Encoder::X264, Some(threads)) = (job.encoder.encoder, job.threads) {
        args.extend(os(&["-threads", &threads.to_string()]));
    }
    let kbps = job.video_kbps;
    args.extend(os(&[
        "-b:v",
        &format!("{kbps}k"),
        "-maxrate",
        &format!("{kbps}k"),
        "-bufsize",
        &format!("{}k", kbps.saturating_mul(2)),
    ]));
    args
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::encoder::ToneMap;
    use crate::media::{AudioTrack, Hdr, VideoStream};

    fn media(hdr: Option<Hdr>, audio: Vec<AudioTrack>) -> MediaInfo {
        MediaInfo {
            container: "matroska,webm".into(),
            duration_ms: Some(60_000),
            bitrate_kbps: Some(8_000),
            video: Some(VideoStream {
                codec: "hevc".into(),
                profile: None,
                bit_depth: if hdr.is_some() { 10 } else { 8 },
                width: 3840,
                height: 2160,
                fps: Some(24.0),
                hdr,
            }),
            audio,
            subtitles: vec![],
        }
    }

    fn track(default: bool) -> AudioTrack {
        AudioTrack {
            codec: "eac3".into(),
            channels: 6,
            language: None,
            default,
        }
    }

    fn caps(encoder: Encoder, tonemap: Option<ToneMap>) -> Capabilities {
        Capabilities { encoder, tonemap }
    }

    /// Arguments as one string, for readable assertions.
    fn line(args: Vec<OsString>) -> String {
        args.into_iter()
            .map(|a| a.into_string().unwrap())
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn hls(job: &Job<'_>, start_segment: u32) -> String {
        line(hls_args(
            job,
            &HlsOutput {
                start_segment,
                segment_seconds: 6,
                dir: Path::new("/cache/s1"),
            },
        ))
    }

    fn job<'a>(
        input: &'a Input,
        media: &'a MediaInfo,
        encoder: Capabilities,
        vaapi_device: &'a Path,
    ) -> Job<'a> {
        Job {
            input,
            media,
            encoder,
            vaapi_device,
            height: 720,
            video_kbps: 4_000,
            threads: Some(4),
        }
    }

    #[test]
    fn a_software_sdr_transcode_from_the_start() {
        let input = Input::File(PathBuf::from("/m/a b.mkv"));
        let media = media(None, vec![track(true)]);
        let device = Path::new("/dev/dri/renderD128");
        let args = hls(&job(&input, &media, caps(Encoder::X264, None), device), 0);
        assert_eq!(
            args,
            "-nostdin -hide_banner -loglevel error -nostats \
             -readrate 1 -readrate_initial_burst 60 -i file:/m/a b.mkv \
             -map 0:v:0 -map 0:a:0 -sn -dn -vf scale=-2:720,format=yuv420p \
             -c:v libx264 -preset veryfast -profile:v high -sc_threshold 0 -threads 4 \
             -b:v 4000k -maxrate 4000k -bufsize 8000k \
             -force_key_frames expr:gte(t,n_forced*6) \
             -c:a aac -b:a 128k -ac 2 \
             -output_ts_offset 0 -f hls -hls_time 6 -hls_list_size 0 \
             -hls_segment_type mpegts -hls_flags independent_segments+temp_file \
             -start_number 0 -hls_segment_filename /cache/s1/seg_%05d.ts \
             /cache/s1/ffmpeg.m3u8"
        );
    }

    #[test]
    fn restarting_at_a_segment_seeks_and_keeps_the_timeline() {
        let input = Input::File(PathBuf::from("/m/a.mkv"));
        let media = media(None, vec![track(true)]);
        let device = Path::new("/dev/dri/renderD128");
        let args = hls(&job(&input, &media, caps(Encoder::X264, None), device), 5);
        // Segment 5 of 6 s segments starts 30 s in.
        assert!(args.contains("-ss 30 -readrate 1"), "{args}");
        assert!(args.contains("-output_ts_offset 30"), "{args}");
        assert!(args.contains("-start_number 5"), "{args}");
        // The seek comes before the input, the segment numbering after.
        assert!(args.find("-ss 30").unwrap() < args.find("-i file:").unwrap());
    }

    #[test]
    fn the_default_audio_track_is_the_one_transcoded() {
        let input = Input::File(PathBuf::from("/m/a.mkv"));
        let device = Path::new("/d");
        let second = media(None, vec![track(false), track(true)]);
        let args = hls(&job(&input, &second, caps(Encoder::X264, None), device), 0);
        assert!(args.contains("-map 0:v:0 -map 0:a:1 "), "{args}");
        let none = media(None, vec![]);
        let args = hls(&job(&input, &none, caps(Encoder::X264, None), device), 0);
        assert!(!args.contains("-map 0:a"), "{args}");
        assert!(!args.contains("-c:a"), "{args}");
    }

    #[test]
    fn hdr_is_tone_mapped_after_scaling() {
        let input = Input::File(PathBuf::from("/m/a.mkv"));
        let hdr = media(Some(Hdr::Pq), vec![track(true)]);
        let device = Path::new("/d");
        let args = hls(
            &job(
                &input,
                &hdr,
                caps(Encoder::X264, Some(ToneMap::Zscale)),
                device,
            ),
            0,
        );
        let vf = args
            .split(" -vf ")
            .nth(1)
            .unwrap()
            .split(" -c:v ")
            .next()
            .unwrap();
        assert!(vf.starts_with("scale=-2:720,zscale=t=linear"), "{vf}");
        assert!(vf.ends_with("format=yuv420p"), "{vf}");
        // Without a working tone mapping the picture is scaled and left.
        let args = hls(&job(&input, &hdr, caps(Encoder::X264, None), device), 0);
        assert!(args.contains("-vf scale=-2:720,format=yuv420p "), "{args}");
        // SDR never gets the tone mapping, even if one is available.
        let sdr = media(None, vec![track(true)]);
        let args = hls(
            &job(
                &input,
                &sdr,
                caps(Encoder::X264, Some(ToneMap::Zscale)),
                device,
            ),
            0,
        );
        assert!(!args.contains("zscale"), "{args}");
    }

    #[test]
    fn opencl_tone_mapping_opens_its_device_before_the_input() {
        let input = Input::File(PathBuf::from("/m/a.mkv"));
        let hdr = media(Some(Hdr::Hlg), vec![track(true)]);
        let args = hls(
            &job(
                &input,
                &hdr,
                caps(Encoder::X264, Some(ToneMap::OpenCl)),
                Path::new("/d"),
            ),
            0,
        );
        assert!(
            args.find("-init_hw_device opencl=ocl").unwrap() < args.find("-i file:").unwrap(),
            "{args}"
        );
    }

    #[test]
    fn vaapi_opens_its_device_and_uploads_frames() {
        let input = Input::File(PathBuf::from("/m/a.mkv"));
        let media = media(None, vec![track(true)]);
        let device = Path::new("/dev/dri/renderD129");
        let args = hls(&job(&input, &media, caps(Encoder::Vaapi, None), device), 0);
        assert!(
            args.contains("-vaapi_device /dev/dri/renderD129 -readrate"),
            "{args}"
        );
        assert!(
            args.contains("-vf scale=-2:720,format=nv12,hwupload -c:v h264_vaapi -b:v 4000k"),
            "{args}"
        );
        assert!(!args.contains("-threads"), "{args}");
    }

    #[test]
    fn nvenc_uses_no_preset() {
        let input = Input::File(PathBuf::from("/m/a.mkv"));
        let media = media(None, vec![track(true)]);
        let args = hls(
            &job(&input, &media, caps(Encoder::Nvenc, None), Path::new("/d")),
            0,
        );
        assert!(
            args.contains("-c:v h264_nvenc -forced-idr 1 -b:v"),
            "{args}"
        );
        assert!(!args.contains("-preset"), "{args}");
        assert!(!args.contains("-threads"), "{args}");
    }

    #[test]
    fn jellyfin_sends_its_headers_before_the_input() {
        let input = Input::Http {
            url: "http://jf/Videos/x/stream?static=true".into(),
            headers: "Authorization: k\r\n".into(),
        };
        let media = media(None, vec![track(true)]);
        let args = hls_args(
            &job(&input, &media, caps(Encoder::X264, None), Path::new("/d")),
            &HlsOutput {
                start_segment: 0,
                segment_seconds: 6,
                dir: Path::new("/c"),
            },
        );
        let args: Vec<_> = args.into_iter().map(|a| a.into_string().unwrap()).collect();
        let at = |s: &str| args.iter().position(|a| a == s).unwrap();
        assert_eq!(args[at("-headers") + 1], "Authorization: k\r\n");
        assert!(at("-headers") < at("-i"));
        assert_eq!(args[at("-i") + 1], "http://jf/Videos/x/stream?static=true");
    }

    #[test]
    fn segment_files_are_numbered_to_match_the_pattern() {
        assert_eq!(segment_file_name(0), "seg_00000.ts");
        assert_eq!(segment_file_name(123), "seg_00123.ts");
    }
}
