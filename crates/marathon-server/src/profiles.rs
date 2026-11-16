//! Quality profiles and the choice between the original file and a
//! transcode (PLAN.md M13).
//!
//! A client's profile is `original` (always stream the file as it is),
//! `auto` (original when the client can play it and the link can carry it,
//! otherwise the best rung that fits), or one rung of the quality ladder
//! (`720p`...). [`decide`] turns a profile into a [`Decision`] for one item.
//! It is pure, so the policy is tested as a table without ffmpeg.

// `decide` is called once `now` consults a client's profile, a later
// commit, which removes this allow.
#![allow(dead_code)]

use marathon_api::{Capabilities, PROFILE_AUTO, PROFILE_ORIGINAL};

use crate::config::TranscodeConfig;
use crate::media::MediaInfo;

/// The audio share of a transcoded stream, in kbit/s: stereo AAC.
pub(crate) const AUDIO_KBPS: u32 = 128;

/// One rung of the quality ladder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rung {
    pub id: String,
    pub name: String,
    /// The tallest picture of the rung, in pixels.
    pub height: u32,
    /// Target video bitrate, in kbit/s.
    pub video_kbps: u32,
}

impl Rung {
    fn new(id: &str, height: u32, video_kbps: u32) -> Self {
        Self {
            id: id.to_owned(),
            name: rung_name(height, video_kbps),
            height,
            video_kbps,
        }
    }

    /// What the stream takes in all, audio included.
    pub(crate) fn total_kbps(&self) -> u32 {
        self.video_kbps + AUDIO_KBPS
    }
}

fn rung_name(height: u32, video_kbps: u32) -> String {
    if video_kbps >= 1000 && video_kbps.is_multiple_of(100) {
        format!("{height}p · {} Mbps", video_kbps / 1000)
    } else if video_kbps >= 1000 {
        format!("{height}p · {:.1} Mbps", f64::from(video_kbps) / 1000.0)
    } else {
        format!("{height}p · {video_kbps} kbps")
    }
}

/// The quality ladder, best first: `[[transcode.profile]]` if the config
/// has any, else the built-in one.
pub(crate) fn ladder(config: &TranscodeConfig) -> Vec<Rung> {
    let mut rungs: Vec<Rung> = if config.profile.is_empty() {
        vec![
            Rung::new("1080p", 1080, 8000),
            Rung::new("720p", 720, 4000),
            Rung::new("720p-low", 720, 2000),
            Rung::new("480p", 480, 1000),
            Rung::new("360p", 360, 500),
        ]
    } else {
        config
            .profile
            .iter()
            .map(|p| Rung {
                id: p.id.clone(),
                name: p
                    .name
                    .clone()
                    .unwrap_or_else(|| rung_name(p.height, p.video_kbps)),
                height: p.height,
                video_kbps: p.video_kbps,
            })
            .collect()
    };
    // Best first: by bitrate, then height (the order is what `auto` walks).
    rungs.sort_by_key(|r| std::cmp::Reverse((r.video_kbps, r.height)));
    rungs
}

/// What a client asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Choice {
    Original,
    Auto,
    /// A rung of the ladder, by ID.
    Rung(String),
}

impl Choice {
    pub(crate) fn parse(id: &str) -> Self {
        match id {
            PROFILE_ORIGINAL => Choice::Original,
            PROFILE_AUTO => Choice::Auto,
            other => Choice::Rung(other.to_owned()),
        }
    }
}

/// How to stream one item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Stream the original file (`/stream`).
    Direct,
    /// Transcode it.
    Transcode {
        /// The rung's ID.
        profile: String,
        /// Output picture height: the rung's, or the source's if smaller
        /// (never scaled up).
        height: u32,
        /// Target video bitrate, in kbit/s: the rung's, or the source's
        /// own if lower (no point spending more than the original has).
        video_kbps: u32,
    },
}

/// Whether the client can play `media`'s original file as it is.
pub(crate) fn direct_play_ok(media: &MediaInfo, caps: &Capabilities) -> bool {
    let Some(video) = &media.video else {
        return true;
    };
    if !caps.containers.is_empty()
        && !media
            .container
            .split(',')
            .any(|name| caps.containers.iter().any(|c| c == name))
    {
        return false;
    }
    let decodes_video = caps.video.iter().any(|support| {
        support.codec == video.codec
            && video.height <= support.max_height
            && (video.bit_depth <= 8 || support.ten_bit)
    });
    if !decodes_video {
        return false;
    }
    // The track a player starts with: the file's default, else the first.
    let track = media
        .audio
        .iter()
        .find(|t| t.default)
        .or_else(|| media.audio.first());
    match track {
        None => true,
        Some(track) => {
            caps.audio_codecs.contains(&track.codec)
                && (caps.max_audio_channels == 0 || track.channels <= caps.max_audio_channels)
        }
    }
}

/// Chooses how to stream `media` to a client with `caps` that asked for
/// `choice`. `budget_kbps` is the bandwidth to plan with (the measured
/// throughput less headroom), if the client measured one.
///
/// Only call this when transcoding is available; otherwise the answer is
/// always [`Decision::Direct`].
pub(crate) fn decide(
    media: &MediaInfo,
    caps: &Capabilities,
    choice: &Choice,
    ladder: &[Rung],
    budget_kbps: Option<u32>,
) -> Decision {
    let Some(video) = &media.video else {
        return Decision::Direct;
    };
    let transcode = |rung: &Rung| Decision::Transcode {
        profile: rung.id.clone(),
        height: rung.height.min(video.height),
        video_kbps: media
            .bitrate_kbps
            .map_or(rung.video_kbps, |own| rung.video_kbps.min(own)),
    };
    match choice {
        Choice::Original => Decision::Direct,
        Choice::Rung(id) => {
            let Some(rung) = ladder.iter().find(|r| r.id == *id) else {
                // A profile the ladder no longer has: play it as it is.
                return Decision::Direct;
            };
            let already_smaller = video.height <= rung.height
                && media
                    .bitrate_kbps
                    .is_some_and(|own| own <= rung.total_kbps());
            if direct_play_ok(media, caps) && already_smaller {
                Decision::Direct
            } else {
                transcode(rung)
            }
        }
        Choice::Auto => {
            let fits = |kbps: u32| budget_kbps.is_none_or(|budget| kbps <= budget);
            if direct_play_ok(media, caps) && media.bitrate_kbps.is_none_or(fits) {
                return Decision::Direct;
            }
            // No taller than the picture there is, or the screen there is.
            let tallest = video.height.min(caps.screen_height.unwrap_or(u32::MAX));
            let allowed: Vec<&Rung> = ladder.iter().filter(|r| r.height <= tallest).collect();
            let rung = allowed
                .iter()
                .find(|r| fits(r.total_kbps()))
                .or(allowed.last())
                // Smaller than every rung: the lowest one, scaled down to
                // the source's own height by `transcode`.
                .copied()
                .or(ladder.last());
            rung.map_or(Decision::Direct, transcode)
        }
    }
}

#[cfg(test)]
mod tests {
    use marathon_api::VideoCodecSupport;

    use super::*;
    use crate::media::{AudioTrack, Hdr, MediaInfo, VideoStream};

    fn media(codec: &str, height: u32, bit_depth: u8, kbps: u32) -> MediaInfo {
        MediaInfo {
            container: "matroska,webm".into(),
            duration_ms: Some(1_380_000),
            bitrate_kbps: Some(kbps),
            video: Some(VideoStream {
                codec: codec.into(),
                profile: None,
                bit_depth,
                width: height * 16 / 9,
                height,
                fps: Some(24.0),
                hdr: None,
            }),
            audio: vec![AudioTrack {
                codec: "aac".into(),
                channels: 2,
                language: None,
                default: true,
            }],
            subtitles: vec![],
        }
    }

    /// A client like the desktop's: decodes nearly everything.
    fn broad() -> Capabilities {
        let codec = |name: &str, ten_bit| VideoCodecSupport {
            codec: name.into(),
            max_height: 2160,
            ten_bit,
        };
        Capabilities {
            video: vec![codec("h264", true), codec("hevc", true), codec("av1", true)],
            audio_codecs: ["aac", "ac3", "eac3", "opus"].map(String::from).to_vec(),
            containers: vec![],
            max_audio_channels: 0,
            hls: true,
            screen_height: None,
        }
    }

    /// A client like an older tablet: H.264 8-bit to 1080p, stereo AAC.
    fn tablet() -> Capabilities {
        Capabilities {
            video: vec![VideoCodecSupport {
                codec: "h264".into(),
                max_height: 1080,
                ten_bit: false,
            }],
            audio_codecs: vec!["aac".into()],
            containers: vec!["mov".into()],
            max_audio_channels: 2,
            hls: true,
            screen_height: Some(768),
        }
    }

    fn ladder() -> Vec<Rung> {
        super::ladder(&TranscodeConfig::default())
    }

    fn transcode(profile: &str, height: u32, video_kbps: u32) -> Decision {
        Decision::Transcode {
            profile: profile.into(),
            height,
            video_kbps,
        }
    }

    fn pick(media: &MediaInfo, caps: &Capabilities, choice: &str, budget: Option<u32>) -> Decision {
        decide(media, caps, &Choice::parse(choice), &ladder(), budget)
    }

    #[test]
    fn the_built_in_ladder_is_best_first() {
        let ids: Vec<_> = ladder().into_iter().map(|r| r.id).collect();
        assert_eq!(ids, ["1080p", "720p", "720p-low", "480p", "360p"]);
        assert_eq!(ladder()[1].name, "720p · 4 Mbps");
        assert_eq!(ladder()[4].name, "360p · 500 kbps");
    }

    #[test]
    fn configured_profiles_replace_the_ladder() {
        let config = TranscodeConfig {
            profile: vec![
                crate::config::ProfileConfig {
                    id: "small".into(),
                    name: None,
                    height: 360,
                    video_kbps: 600,
                },
                crate::config::ProfileConfig {
                    id: "big".into(),
                    name: Some("Big one".into()),
                    height: 1080,
                    video_kbps: 6500,
                },
            ],
            ..TranscodeConfig::default()
        };
        let rungs = super::ladder(&config);
        assert_eq!(rungs[0].id, "big");
        assert_eq!(rungs[0].name, "Big one");
        assert_eq!(rungs[1].name, "360p · 600 kbps");
    }

    #[test]
    fn original_is_always_direct() {
        let hevc = media("hevc", 2160, 10, 40_000);
        assert_eq!(
            pick(&hevc, &tablet(), "original", Some(500)),
            Decision::Direct
        );
    }

    #[test]
    fn auto_plays_a_compatible_file_that_fits_directly() {
        let ok = media("h264", 1080, 8, 5_000);
        assert_eq!(pick(&ok, &broad(), "auto", Some(8_000)), Decision::Direct);
        // No measurement yet: nothing to say it will not fit.
        assert_eq!(pick(&ok, &broad(), "auto", None), Decision::Direct);
    }

    #[test]
    fn auto_transcodes_a_file_that_is_too_big_for_the_link() {
        let big = media("h264", 1080, 8, 20_000);
        // 5 Mbit/s of budget: 1080p (8128) and 720p (4128) → 720p fits.
        assert_eq!(
            pick(&big, &broad(), "auto", Some(5_000)),
            transcode("720p", 720, 4_000)
        );
        // 1.2 Mbit/s: only 480p (1128) fits.
        assert_eq!(
            pick(&big, &broad(), "auto", Some(1_200)),
            transcode("480p", 480, 1_000)
        );
        // Less than the lowest rung: the lowest rung anyway.
        assert_eq!(
            pick(&big, &broad(), "auto", Some(100)),
            transcode("360p", 360, 500)
        );
    }

    #[test]
    fn auto_transcodes_what_the_client_cannot_decode() {
        let hevc10 = media("hevc", 1080, 10, 6_000);
        let on_tablet = pick(&hevc10, &tablet(), "auto", Some(50_000));
        // Screen is 768 tall: the 1080p rung is out, 720p is the best.
        assert_eq!(on_tablet, transcode("720p", 720, 4_000));
        // The same file is fine for a client with HEVC.
        assert_eq!(
            pick(&hevc10, &broad(), "auto", Some(50_000)),
            Decision::Direct
        );
        // MKV on a client that opens only MOV.
        let mkv_h264 = media("h264", 720, 8, 3_000);
        assert_eq!(
            pick(&mkv_h264, &tablet(), "auto", Some(50_000)),
            transcode("720p", 720, 3_000)
        );
    }

    #[test]
    fn auto_never_upscales_or_spends_more_than_the_source_has() {
        let small = media("mpeg2video", 480, 8, 1_800);
        // Source is 480 tall: the best rung no taller is 480p; its
        // bitrate is capped at what the source has.
        assert_eq!(
            pick(&small, &broad(), "auto", Some(50_000)),
            transcode("480p", 480, 1_000)
        );
        // Smaller than every rung: the lowest, at the source's height.
        let tiny = media("mpeg2video", 240, 8, 400);
        assert_eq!(
            pick(&tiny, &broad(), "auto", Some(50_000)),
            transcode("360p", 240, 400)
        );
    }

    #[test]
    fn a_fixed_rung_plays_directly_only_when_the_source_is_already_smaller() {
        let small = media("h264", 480, 8, 900);
        assert_eq!(pick(&small, &broad(), "720p", None), Decision::Direct);
        // Bigger than the rung: transcode.
        let big = media("h264", 1080, 8, 9_000);
        assert_eq!(
            pick(&big, &broad(), "720p", None),
            transcode("720p", 720, 4_000)
        );
        // Small, but not playable: transcode (at its own height).
        let hevc = media("hevc", 480, 8, 900);
        assert_eq!(
            pick(&hevc, &tablet(), "720p", None),
            transcode("720p", 480, 900)
        );
        // Bitrate unknown: cannot say it is smaller.
        let mut unknown = media("h264", 480, 8, 0);
        unknown.bitrate_kbps = None;
        assert_eq!(
            pick(&unknown, &broad(), "720p", None),
            transcode("720p", 480, 4_000)
        );
        // A rung that has since left the ladder.
        assert_eq!(pick(&big, &broad(), "gone", None), Decision::Direct);
    }

    #[test]
    fn hdr_is_not_a_reason_to_transcode_by_itself() {
        let mut hdr = media("hevc", 2160, 10, 30_000);
        hdr.video.as_mut().unwrap().hdr = Some(Hdr::Pq);
        assert_eq!(
            pick(&hdr, &broad(), "auto", Some(100_000)),
            Decision::Direct
        );
    }

    #[test]
    fn audio_that_the_client_cannot_play_forces_a_transcode() {
        let mut dts = media("h264", 1080, 8, 5_000);
        dts.audio[0].codec = "dts".into();
        assert!(matches!(
            pick(&dts, &broad(), "auto", Some(50_000)),
            Decision::Transcode { .. }
        ));
        // 5.1 AAC on a stereo-only client.
        let mut surround = media("h264", 1080, 8, 5_000);
        surround.audio[0].channels = 6;
        let mut stereo = broad();
        stereo.max_audio_channels = 2;
        assert!(matches!(
            pick(&surround, &stereo, "auto", Some(50_000)),
            Decision::Transcode { .. }
        ));
    }

    #[test]
    fn the_default_audio_track_is_the_one_that_counts() {
        let mut two = media("h264", 1080, 8, 5_000);
        two.audio = vec![
            AudioTrack {
                codec: "dts".into(),
                channels: 6,
                language: None,
                default: false,
            },
            AudioTrack {
                codec: "aac".into(),
                channels: 2,
                language: None,
                default: true,
            },
        ];
        assert_eq!(pick(&two, &broad(), "auto", None), Decision::Direct);
    }

    #[test]
    fn files_without_video_are_always_direct() {
        let mut song = media("h264", 1080, 8, 200);
        song.video = None;
        assert_eq!(pick(&song, &tablet(), "720p", Some(10)), Decision::Direct);
    }
}
