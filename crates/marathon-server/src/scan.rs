//! Walking a filesystem source for video files and their sidecar subtitles.
//!
//! This is blocking I/O; call it from `tokio::task::spawn_blocking`. It only
//! reads the filesystem. Turning the result into database rows is the
//! catalog's job ([`crate::catalog`]).

use std::fs::{self, Metadata};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

/// File extensions (lowercase) treated as videos.
const VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "m4v", "mov", "avi", "webm", "ts", "m2ts", "mts", "wmv", "flv", "mpg", "mpeg",
    "ogv",
];

/// File extensions (lowercase) treated as sidecar subtitles. These are the
/// text formats mpv's `sub-add` loads from a single file; VobSub (`.idx` +
/// `.sub`) needs two files and is not supported.
const SUBTITLE_EXTENSIONS: &[&str] = &["srt", "vtt", "ass", "ssa"];

/// A video found by a scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedFile {
    /// Path relative to the source root, `/`-separated.
    pub path: String,
    pub size: u64,
    /// Modification time in milliseconds since the Unix epoch (0 if
    /// unavailable).
    pub mtime_ms: i64,
    /// Sidecar subtitles, sorted by file name so their indices are stable.
    pub subtitles: Vec<SidecarSubtitle>,
}

/// A subtitle file next to a video whose name starts with the video's name:
/// `Episode.srt`, `Episode.en.srt`, `Episode.en.forced.srt` for
/// `Episode.mkv`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidecarSubtitle {
    /// Path relative to the source root, `/`-separated.
    pub path: String,
    /// Language tag from the file name (`en`), if the first label part looks
    /// like one.
    pub language: Option<String>,
    /// Everything between the video's name and the extension (`en.forced`).
    pub label: String,
    /// Lowercase extension: `srt`, `vtt`, `ass`, or `ssa`.
    pub format: String,
}

/// Errors that stop a scan. Unreadable subdirectories and odd files are
/// logged and skipped instead.
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    /// The source root could not be listed (not mounted, no permission).
    #[error("cannot read source root {path}")]
    Root {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Lists every video under `root`, with sidecar subtitles attached.
///
/// Hidden files and directories (names starting with `.`) are skipped, as
/// are names that are not valid UTF-8 and release samples (see
/// [`is_sample`]). Symlinks to files are followed;
/// symlinks to directories are not, which rules out cycles. The result is
/// sorted by path.
pub fn scan_filesystem(root: &Path) -> Result<Vec<ScannedFile>, ScanError> {
    let mut found = Vec::new();
    let top = fs::read_dir(root).map_err(|source| ScanError::Root {
        path: root.to_owned(),
        source,
    })?;
    let mut pending = vec![(String::new(), top)];
    while let Some((rel_dir, entries)) = pending.pop() {
        let mut videos = Vec::new();
        let mut subtitles = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    tracing::warn!(dir = %root.join(&rel_dir).display(), %err, "skipping unreadable entry");
                    continue;
                }
            };
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                tracing::warn!(path = %entry.path().display(), "skipping file name that is not UTF-8");
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let rel = join(&rel_dir, &name);
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                match fs::read_dir(entry.path()) {
                    Ok(sub) => pending.push((rel, sub)),
                    Err(err) => {
                        tracing::warn!(path = %entry.path().display(), %err, "skipping unreadable directory");
                    }
                }
                continue;
            }
            // `fs::metadata` follows symlinks, so a link to a file counts as
            // that file; a link to a directory is not a file and is skipped.
            let meta = match fs::metadata(entry.path()) {
                Ok(meta) if meta.is_file() => meta,
                _ => continue,
            };
            match extension(&name) {
                Some(ext) if VIDEO_EXTENSIONS.contains(&ext.as_str()) && is_sample(&name) => {
                    tracing::debug!(path = %rel, "skipping release sample");
                }
                Some(ext) if VIDEO_EXTENSIONS.contains(&ext.as_str()) => {
                    videos.push((name, rel, meta));
                }
                Some(ext) if SUBTITLE_EXTENSIONS.contains(&ext.as_str()) => {
                    subtitles.push((name, rel, ext));
                }
                _ => {}
            }
        }
        found.extend(attach_subtitles(videos, subtitles));
    }
    found.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(found)
}

/// Pairs the videos and subtitle files of one directory.
fn attach_subtitles(
    videos: Vec<(String, String, Metadata)>,
    mut subtitles: Vec<(String, String, String)>,
) -> Vec<ScannedFile> {
    subtitles.sort();
    let mut files: Vec<(String, ScannedFile)> = videos
        .into_iter()
        .map(|(name, path, meta)| {
            let stem = stem(&name).to_owned();
            let file = ScannedFile {
                path,
                size: meta.len(),
                mtime_ms: mtime_ms(&meta),
                subtitles: Vec::new(),
            };
            (stem, file)
        })
        .collect();

    for (name, path, format) in subtitles {
        let sub_stem = stem(&name);
        // The longest matching video name wins, so `Ep 1.5.en.srt` goes to
        // `Ep 1.5.mkv` rather than `Ep 1.mkv`.
        let owner = files
            .iter_mut()
            .filter_map(|(video_stem, file)| {
                let label = if sub_stem == video_stem {
                    ""
                } else {
                    sub_stem
                        .strip_prefix(video_stem.as_str())?
                        .strip_prefix('.')?
                };
                Some((video_stem.len(), label, file))
            })
            .max_by_key(|(len, _, _)| *len);
        if let Some((_, label, file)) = owner {
            file.subtitles.push(SidecarSubtitle {
                language: language_tag(label),
                label: label.to_owned(),
                path,
                format,
            });
        }
    }
    files.into_iter().map(|(_, file)| file).collect()
}

/// The first `.`-separated part of a subtitle label, if it looks like a
/// language tag: two or three ASCII letters, optionally followed by a region
/// (`en`, `eng`, `pt-BR`, `en_US`).
fn language_tag(label: &str) -> Option<String> {
    let first = label.split('.').next()?;
    let (lang, region) = match first.split_once(['-', '_']) {
        Some((lang, region)) => (lang, Some(region)),
        None => (first, None),
    };
    let lang_ok = (2..=3).contains(&lang.len()) && lang.chars().all(|c| c.is_ascii_alphabetic());
    let region_ok = region
        .is_none_or(|r| (2..=4).contains(&r.len()) && r.chars().all(|c| c.is_ascii_alphanumeric()));
    (lang_ok && region_ok).then(|| first.to_owned())
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_owned()
    } else {
        format!("{dir}/{name}")
    }
}

/// The lowercase extension of a file name, if it has one.
fn extension(name: &str) -> Option<String> {
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty()).then(|| ext.to_ascii_lowercase())
}

/// Returns `true` for a release sample: a short preview clip that download
/// tools leave next to the real file, named like it plus `.sample`
/// (`Show - 1x01 - Pilot.sample.mkv`, also `-sample`, `_sample`, or just
/// `sample.mkv`). Playing one would put a duplicate, truncated episode on a
/// channel.
fn is_sample(name: &str) -> bool {
    let stem = stem(name).to_ascii_lowercase();
    stem == "sample"
        || stem
            .strip_suffix("sample")
            .is_some_and(|rest| rest.ends_with(['.', '-', '_']))
}

/// A file name without its extension.
fn stem(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => name,
    }
}

fn mtime_ms(meta: &Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(root: &Path, rel: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, rel.as_bytes()).unwrap();
    }

    #[test]
    fn finds_videos_and_attaches_sidecars() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(root, "Show/Season 1/Show - S01E01 - Pilot.mkv");
        touch(root, "Show/Season 1/Show - S01E01 - Pilot.en.srt");
        touch(root, "Show/Season 1/Show - S01E01 - Pilot.en.forced.SRT");
        touch(root, "Show/Season 1/Show - S01E02 - Next.mkv");
        touch(root, "Show/Season 1/Show - S01E02 - Next.ass");
        touch(root, "Show/Season 1/orphan.srt");
        touch(root, "Show/notes.txt");
        touch(root, "Show/.hidden.mkv");
        touch(root, "Show/Season 1/Show - S01E02 - Next.sample.mkv");
        touch(root, "Show/Season 1/sample.mkv");
        touch(root, ".trash/old.mkv");
        touch(root, "Top.MP4");

        let files = scan_filesystem(root).unwrap();
        let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "Show/Season 1/Show - S01E01 - Pilot.mkv",
                "Show/Season 1/Show - S01E02 - Next.mkv",
                "Top.MP4",
            ]
        );

        let pilot = &files[0];
        assert_eq!(pilot.size, pilot.path.len() as u64);
        assert!(pilot.mtime_ms > 0);
        assert_eq!(
            pilot.subtitles,
            [
                SidecarSubtitle {
                    path: "Show/Season 1/Show - S01E01 - Pilot.en.forced.SRT".into(),
                    language: Some("en".into()),
                    label: "en.forced".into(),
                    format: "srt".into(),
                },
                SidecarSubtitle {
                    path: "Show/Season 1/Show - S01E01 - Pilot.en.srt".into(),
                    language: Some("en".into()),
                    label: "en".into(),
                    format: "srt".into(),
                },
            ]
        );
        assert_eq!(files[1].subtitles.len(), 1);
        assert_eq!(files[1].subtitles[0].label, "");
        assert_eq!(files[1].subtitles[0].language, None);
        assert_eq!(files[1].subtitles[0].format, "ass");
    }

    #[test]
    fn subtitle_goes_to_longest_matching_video() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "Ep 1.mkv");
        touch(dir.path(), "Ep 1.5.mkv");
        touch(dir.path(), "Ep 1.5.en.srt");
        let files = scan_filesystem(dir.path()).unwrap();
        let half = files.iter().find(|f| f.path == "Ep 1.5.mkv").unwrap();
        assert_eq!(half.subtitles.len(), 1);
        let one = files.iter().find(|f| f.path == "Ep 1.mkv").unwrap();
        assert!(one.subtitles.is_empty());
    }

    #[test]
    fn missing_root_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = scan_filesystem(&dir.path().join("unmounted")).unwrap_err();
        assert!(matches!(err, ScanError::Root { .. }));
    }

    #[cfg(unix)]
    #[test]
    fn follows_file_symlinks_but_not_directory_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        touch(root, "real/a.mkv");
        std::os::unix::fs::symlink(root.join("real/a.mkv"), root.join("link.mkv")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("loop")).unwrap();
        let files = scan_filesystem(root).unwrap();
        let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["link.mkv", "real/a.mkv"]);
    }

    #[test]
    fn samples() {
        assert!(is_sample("Weeds - 8x01 - Messy.sample.mkv"));
        assert!(is_sample("show-SAMPLE.avi"));
        assert!(is_sample("Sample.mkv"));
        assert!(!is_sample("Free Sample.mkv"));
        assert!(!is_sample("Sampler.mkv"));
    }

    #[test]
    fn language_tags() {
        assert_eq!(language_tag("en").as_deref(), Some("en"));
        assert_eq!(language_tag("pt-BR.sdh").as_deref(), Some("pt-BR"));
        assert_eq!(language_tag("eng").as_deref(), Some("eng"));
        assert_eq!(language_tag("forced"), None);
        assert_eq!(language_tag(""), None);
    }
}
