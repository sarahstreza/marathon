//! Choosing the video encoder for live transcoding (PLAN.md M13).
//!
//! ffmpeg may be built with several H.264 encoders, and listing them says
//! nothing about whether they work here: `h264_nvenc` is compiled into
//! most builds yet fails without an NVIDIA GPU. So detection runs a
//! one-second test encode with each candidate, in order of preference, and
//! takes the first that succeeds. The same is done for the HDR to SDR tone
//! mapping filter chains.
//!
//! The functions that build ffmpeg's arguments are pure and unit tested;
//! [`detect`] runs them against the real ffmpeg.

use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use crate::config::Hardware;

/// How long one test encode may take before the encoder counts as broken.
/// Hardware encoders can take a few seconds to initialise.
const TEST_TIMEOUT: Duration = Duration::from_secs(20);

/// An H.264 encoder, in the order detection prefers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Encoder {
    Nvenc,
    Qsv,
    Vaapi,
    VideoToolbox,
    X264,
    OpenH264,
}

impl Encoder {
    /// Every encoder, most preferred first.
    const PREFERENCE: [Encoder; 6] = [
        Encoder::Nvenc,
        Encoder::Qsv,
        Encoder::Vaapi,
        Encoder::VideoToolbox,
        Encoder::X264,
        Encoder::OpenH264,
    ];

    /// The encoders `hardware` allows, most preferred first.
    fn candidates(hardware: Hardware) -> &'static [Encoder] {
        match hardware {
            Hardware::Auto => &Self::PREFERENCE,
            Hardware::Nvenc => &[Encoder::Nvenc],
            Hardware::Qsv => &[Encoder::Qsv],
            Hardware::Vaapi => &[Encoder::Vaapi],
            Hardware::VideoToolbox => &[Encoder::VideoToolbox],
            Hardware::Software => &[Encoder::X264, Encoder::OpenH264],
        }
    }

    /// ffmpeg's name for the encoder (`-c:v`).
    pub(crate) fn name(self) -> &'static str {
        match self {
            Encoder::Nvenc => "h264_nvenc",
            Encoder::Qsv => "h264_qsv",
            Encoder::Vaapi => "h264_vaapi",
            Encoder::VideoToolbox => "h264_videotoolbox",
            Encoder::X264 => "libx264",
            Encoder::OpenH264 => "libopenh264",
        }
    }

    /// Whether the encoder runs on a GPU or other hardware.
    pub(crate) fn is_hardware(self) -> bool {
        !matches!(self, Encoder::X264 | Encoder::OpenH264)
    }
}

/// A way of mapping HDR video down to SDR, in the order detection prefers
/// them: the GPU filters first, the software one last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToneMap {
    OpenCl,
    Placebo,
    Zscale,
}

impl ToneMap {
    const PREFERENCE: [ToneMap; 3] = [ToneMap::OpenCl, ToneMap::Placebo, ToneMap::Zscale];

    /// ffmpeg arguments that apply the tone mapping to a test source and
    /// leave 8-bit `yuv420p`, as a real transcode needs.
    fn args(self) -> Vec<&'static str> {
        match self {
            ToneMap::OpenCl => vec![
                "-init_hw_device",
                "opencl=ocl",
                "-filter_hw_device",
                "ocl",
                "-vf",
                "format=p010,hwupload,tonemap_opencl=tonemap=bt2390:format=nv12,hwdownload,\
                 format=nv12",
            ],
            ToneMap::Placebo => vec![
                "-vf",
                "libplacebo=tonemapping=bt.2390:colorspace=bt709:color_primaries=bt709:\
                 color_trc=bt709:format=yuv420p",
            ],
            ToneMap::Zscale => vec![
                "-vf",
                "zscale=t=linear:npl=100,format=gbrpf32le,zscale=p=bt709,tonemap=hable,\
                 zscale=t=bt709:m=bt709:r=tv,format=yuv420p",
            ],
        }
    }
}

/// What detection found: the encoder transcodes use, and how HDR is
/// handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Capabilities {
    pub encoder: Encoder,
    /// `None`: no tone mapping works, so HDR items are transcoded as they
    /// are and look washed out.
    pub tonemap: Option<ToneMap>,
}

/// Arguments for a one-second test encode with `encoder` into the void.
fn test_encode_args(encoder: Encoder, vaapi_device: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["-hide_banner", "-loglevel", "error", "-nostdin"]
        .into_iter()
        .map(OsString::from)
        .collect();
    match encoder {
        Encoder::Vaapi => {
            args.extend(["-vaapi_device".into(), vaapi_device.into()]);
        }
        Encoder::Qsv => {
            args.extend(
                ["-init_hw_device", "qsv=hw", "-filter_hw_device", "hw"].map(OsString::from),
            );
        }
        _ => {}
    }
    // 256x144 is above the minimum size some hardware encoders accept.
    args.extend(["-f", "lavfi", "-i", "testsrc=s=256x144:r=10:d=1"].map(OsString::from));
    match encoder {
        Encoder::Vaapi => {
            args.extend(["-vf", "format=nv12,hwupload"].map(OsString::from));
        }
        Encoder::Qsv => {
            args.extend(["-vf", "format=nv12,hwupload=extra_hw_frames=64"].map(OsString::from));
        }
        _ => args.extend(["-pix_fmt", "yuv420p"].map(OsString::from)),
    }
    args.extend(["-c:v".into(), encoder.name().into()]);
    args.extend(["-f", "null", "-"].map(OsString::from));
    args
}

/// Arguments for a one-second test run of a tone mapping chain, which
/// encodes with software x264 since only the filters are on trial.
fn test_tonemap_args(tonemap: ToneMap) -> Vec<OsString> {
    let mut args: Vec<OsString> = ["-hide_banner", "-loglevel", "error", "-nostdin"]
        .into_iter()
        .map(OsString::from)
        .collect();
    // A 10-bit PQ source: the sort of video the chain exists for.
    args.extend(
        [
            "-f",
            "lavfi",
            "-i",
            "testsrc=s=256x144:r=10:d=1,format=yuv420p10le,\
             setparams=color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc",
        ]
        .map(OsString::from),
    );
    args.extend(tonemap.args().into_iter().map(OsString::from));
    args.extend(["-c:v", "libx264", "-f", "null", "-"].map(OsString::from));
    args
}

/// Whether ffmpeg runs `args` successfully within [`TEST_TIMEOUT`].
async fn succeeds(ffmpeg: &Path, args: Vec<OsString>) -> bool {
    let mut command = Command::new(ffmpeg);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    matches!(
        tokio::time::timeout(TEST_TIMEOUT, command.status()).await,
        Ok(Ok(status)) if status.success()
    )
}

/// Finds the encoder and tone mapping to use with `ffmpeg`, or `None` if
/// no allowed encoder works.
///
/// Logs each candidate that failed (at debug level, since failing is
/// normal for hardware the machine lacks) and the final choice.
pub(crate) async fn detect(
    ffmpeg: &Path,
    hardware: Hardware,
    vaapi_device: &Path,
) -> Option<Capabilities> {
    let mut chosen = None;
    for &encoder in Encoder::candidates(hardware) {
        if succeeds(ffmpeg, test_encode_args(encoder, vaapi_device)).await {
            chosen = Some(encoder);
            break;
        }
        tracing::debug!(encoder = encoder.name(), "encoder does not work here");
    }
    let encoder = chosen?;

    let mut tonemap = None;
    for candidate in ToneMap::PREFERENCE {
        if succeeds(ffmpeg, test_tonemap_args(candidate)).await {
            tonemap = Some(candidate);
            break;
        }
        tracing::debug!(tonemap = ?candidate, "tone mapping does not work here");
    }
    Some(Capabilities { encoder, tonemap })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: Vec<OsString>) -> String {
        args.into_iter()
            .map(|a| a.into_string().unwrap())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn auto_prefers_hardware_then_software() {
        let names: Vec<_> = Encoder::candidates(Hardware::Auto)
            .iter()
            .map(|e| e.name())
            .collect();
        assert_eq!(
            names,
            [
                "h264_nvenc",
                "h264_qsv",
                "h264_vaapi",
                "h264_videotoolbox",
                "libx264",
                "libopenh264"
            ]
        );
        assert_eq!(Encoder::candidates(Hardware::Nvenc), [Encoder::Nvenc]);
        assert_eq!(
            Encoder::candidates(Hardware::Software),
            [Encoder::X264, Encoder::OpenH264]
        );
        assert!(Encoder::Nvenc.is_hardware());
        assert!(!Encoder::X264.is_hardware());
    }

    #[test]
    fn vaapi_and_qsv_tests_set_up_their_devices() {
        let device = Path::new("/dev/dri/renderD129");
        let vaapi = strings(test_encode_args(Encoder::Vaapi, device));
        assert!(
            vaapi.contains("-vaapi_device /dev/dri/renderD129"),
            "{vaapi}"
        );
        assert!(vaapi.contains("hwupload"), "{vaapi}");
        assert!(vaapi.ends_with("-c:v h264_vaapi -f null -"), "{vaapi}");
        let qsv = strings(test_encode_args(Encoder::Qsv, device));
        assert!(qsv.contains("-init_hw_device qsv=hw"), "{qsv}");
        let nvenc = strings(test_encode_args(Encoder::Nvenc, device));
        assert!(!nvenc.contains("hwupload"), "{nvenc}");
        assert!(
            nvenc.contains("-pix_fmt yuv420p -c:v h264_nvenc"),
            "{nvenc}"
        );
    }

    #[test]
    fn tonemap_tests_start_from_a_pq_source() {
        for tonemap in ToneMap::PREFERENCE {
            let args = strings(test_tonemap_args(tonemap));
            assert!(args.contains("smpte2084"), "{args}");
            assert!(args.ends_with("-c:v libx264 -f null -"), "{args}");
        }
    }

    #[tokio::test]
    async fn no_ffmpeg_means_no_encoder() {
        let none = detect(
            Path::new("/nonexistent/ffmpeg"),
            Hardware::Auto,
            Path::new("/dev/dri/renderD128"),
        )
        .await;
        assert_eq!(none, None);
    }

    /// Needs a real ffmpeg with libx264 (or libopenh264); skipped without.
    #[tokio::test]
    async fn software_is_found_when_hardware_is_not_wanted() {
        let have = std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success());
        if !have {
            eprintln!("skipping: ffmpeg is not installed");
            return;
        }
        let found = detect(
            Path::new("ffmpeg"),
            Hardware::Software,
            Path::new("/dev/dri/renderD128"),
        )
        .await;
        let Some(found) = found else {
            eprintln!("skipping: this ffmpeg has no software H.264 encoder");
            return;
        };
        assert!(!found.encoder.is_hardware());
        // A hardware encoder that cannot work here is refused, not chosen:
        // there is no VAAPI device at this path.
        let vaapi = detect(
            Path::new("ffmpeg"),
            Hardware::Vaapi,
            Path::new("/nonexistent/renderD128"),
        )
        .await;
        assert_eq!(vaapi, None);
    }
}
