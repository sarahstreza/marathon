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

use marathon_api::{Capabilities, PROFILE_AUTO, PROFILE_ORIGINAL, ProfileKind, ProfileOption};

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

/// The bandwidth to plan with: `headroom_percent` of what was measured,
/// so a stream does not use the whole link.
pub(crate) fn budget_kbps(measured_kbps: u32, headroom_percent: u32) -> u32 {
    let kbps = u64::from(measured_kbps) * u64::from(headroom_percent) / 100;
    u32::try_from(kbps).unwrap_or(u32::MAX)
}

/// The profiles a client may choose from, for `POST /playback/profiles`.
///
/// `original` is always there. With transcoding available and a client
/// that plays HLS (which transcoded streams are), `auto` and the ladder
/// follow. `auto` is recommended once the client has measured its link;
/// before that, `original`, which is the behaviour a client gets when it
/// has chosen nothing.
pub(crate) fn options(
    caps: &Capabilities,
    measured_kbps: Option<u32>,
    ladder: &[Rung],
    headroom_percent: u32,
    transcoding: bool,
) -> Vec<ProfileOption> {
    let offers_transcodes = transcoding && caps.hls;
    let budget = measured_kbps.map(|m| budget_kbps(m, headroom_percent));
    let recommend_auto = offers_transcodes && measured_kbps.is_some();
    let mut options = vec![ProfileOption {
        id: PROFILE_ORIGINAL.to_owned(),
        name: "Original".to_owned(),
        kind: ProfileKind::Original,
        video_kbps: None,
        max_height: None,
        fits_bandwidth: true,
        recommended: !recommend_auto,
    }];
    if offers_transcodes {
        options.push(ProfileOption {
            id: PROFILE_AUTO.to_owned(),
            name: "Auto".to_owned(),
            kind: ProfileKind::Auto,
            video_kbps: None,
            max_height: None,
            fits_bandwidth: true,
            recommended: recommend_auto,
        });
        options.extend(ladder.iter().map(|rung| ProfileOption {
            id: rung.id.clone(),
            name: rung.name.clone(),
            kind: ProfileKind::Transcode,
            video_kbps: Some(rung.video_kbps),
            max_height: Some(rung.height),
            fits_bandwidth: budget.is_none_or(|b| rung.total_kbps() <= b),
            recommended: false,
        }));
    }
    options
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

    fn ids(options: &[ProfileOption]) -> Vec<&str> {
        options.iter().map(|o| o.id.as_str()).collect()
    }

    fn recommended(options: &[ProfileOption]) -> Vec<&str> {
        options
            .iter()
            .filter(|o| o.recommended)
            .map(|o| o.id.as_str())
            .collect()
    }

    #[test]
    fn options_list_original_auto_and_the_ladder() {
        let all = options(&broad(), Some(6_000), &ladder(), 60, true);
        assert_eq!(
            ids(&all),
            [
                "original", "auto", "1080p", "720p", "720p-low", "480p", "360p"
            ]
        );
        // 60% of 6 Mbit/s is 3.6: 720p (4.1) is too much, 720p-low (2.1) fits.
        let fits: Vec<_> = all
            .iter()
            .map(|o| (o.id.as_str(), o.fits_bandwidth))
            .collect();
        assert_eq!(
            fits,
            [
                ("original", true),
                ("auto", true),
                ("1080p", false),
                ("720p", false),
                ("720p-low", true),
                ("480p", true),
                ("360p", true)
            ]
        );
        assert_eq!(recommended(&all), ["auto"]);
    }

    #[test]
    fn without_a_measurement_original_is_recommended_and_everything_fits() {
        let all = options(&broad(), None, &ladder(), 60, true);
        assert!(all.iter().all(|o| o.fits_bandwidth));
        assert_eq!(recommended(&all), ["original"]);
    }

    #[test]
    fn only_original_when_there_is_no_transcoding_or_no_hls() {
        let off = options(&broad(), Some(6_000), &ladder(), 60, false);
        assert_eq!(ids(&off), ["original"]);
        assert_eq!(recommended(&off), ["original"]);
        let mut no_hls = broad();
        no_hls.hls = false;
        let on = options(&no_hls, Some(6_000), &ladder(), 60, true);
        assert_eq!(ids(&on), ["original"]);
    }

    #[test]
    fn headroom_scales_the_measurement() {
        assert_eq!(budget_kbps(10_000, 60), 6_000);
        assert_eq!(budget_kbps(u32::MAX, 100), u32::MAX);
        assert_eq!(budget_kbps(0, 60), 0);
    }

    #[test]
    fn files_without_video_are_always_direct() {
        let mut song = media("h264", 1080, 8, 200);
        song.video = None;
        assert_eq!(pick(&song, &tablet(), "720p", Some(10)), Decision::Direct);
    }
}
