//! Transcoding sessions (PLAN.md M13): one per player that is watching a
//! transcode.
//!
//! A session owns a directory of HLS segments and, at most, one running
//! ffmpeg that is filling it. The playlist lists the whole item
//! ([`crate::hls`]), so a player may ask for any segment at any time:
//!
//! - one already on disk is served at once;
//! - one that the running ffmpeg is about to produce is waited for;
//! - otherwise ffmpeg is restarted at that segment. Segments are a fixed
//!   length with a keyframe at each boundary ([`crate::encode`]), so a
//!   restarted ffmpeg produces exactly the segments the old one would have.
//!
//! ffmpeg reads at real time after a burst, so a paused or finished viewer
//! leaves it at most a minute or so ahead. A session nobody has asked
//! anything of for `idle_timeout` is dropped by [`Registry::reap`]: its
//! ffmpeg is killed and its directory removed. Dropping a session also
//! frees its slot, of which there are only `max_sessions`.
//!
//! A session is made by the first segment request that carries its ID, not
//! by the playlist, which only mints the ID: players (mpv's youtube-dl hook
//! among them) fetch a playlist more than once, and none of that should cost
//! a slot. The same path makes a session again when a request names one the
//! server no longer has (it expired while the viewer was paused, or the
//! server restarted), so playback carries on.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

use crate::audio::Input;
use crate::encode::{self, HlsOutput, Job};
use crate::encoder::Capabilities;
use crate::error::ApiError;
use crate::hls;
use crate::media::MediaInfo;

/// How many segments past the furthest one asked for a running ffmpeg is
/// trusted to reach soon. A request beyond that restarts it at the request
/// rather than waiting for it to catch up.
const LOOKAHEAD_SEGMENTS: u32 = 3;

/// How long a segment request waits for ffmpeg to produce its segment.
const SEGMENT_WAIT: Duration = Duration::from_secs(45);

/// How often a waiting request looks for its segment.
const POLL: Duration = Duration::from_millis(50);

/// How much of the item behind the viewer is kept on disk, in seconds.
const KEEP_SECONDS: u32 = 300;

/// How often idle sessions are looked for.
const REAP_EVERY: Duration = Duration::from_secs(10);

/// ffmpeg's error lines kept, to say why it stopped.
const STDERR_LINES: usize = 20;

/// What every session shares.
#[derive(Debug)]
pub(crate) struct Settings {
    pub ffmpeg: PathBuf,
    pub encoder: Capabilities,
    pub vaapi_device: PathBuf,
    pub segment_seconds: u32,
    /// Threads a software encoder may use.
    pub threads: Option<u32>,
}

/// What a session transcodes, fixed when it is made.
#[derive(Debug, Clone)]
pub(crate) struct Plan {
    pub input: Input,
    pub media: MediaInfo,
    pub duration_ms: u64,
    /// Output picture height and video bitrate (kbit/s).
    pub height: u32,
    pub video_kbps: u32,
}

/// All the sessions, and the slots they take.
pub(crate) struct Registry {
    /// The directory sessions make theirs in: owned by the server, emptied
    /// at startup.
    root: PathBuf,
    settings: Arc<Settings>,
    permits: Arc<Semaphore>,
    idle_timeout: Duration,
    sessions: Mutex<HashMap<Uuid, Arc<Session>>>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("root", &self.root)
            .field("sessions", &self.len())
            .finish()
    }
}

impl Registry {
    /// Makes a registry whose sessions live under `root`, which is emptied
    /// first: nothing from before a restart can still be running.
    ///
    /// `root` must be a directory this server owns (the `transcode` folder
    /// inside the cache directory), since it is removed wholesale.
    pub(crate) fn new(
        root: PathBuf,
        settings: Settings,
        max_sessions: usize,
        idle_timeout: Duration,
    ) -> std::io::Result<Self> {
        match std::fs::remove_dir_all(&root) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        std::fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            settings: Arc::new(settings),
            permits: Arc::new(Semaphore::new(max_sessions)),
            idle_timeout,
            sessions: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn segment_seconds(&self) -> u32 {
        self.settings.segment_seconds
    }

    /// Whether a new session could be made now.
    pub(crate) fn has_room(&self) -> bool {
        self.permits.available_permits() > 0
    }

    pub(crate) fn len(&self) -> usize {
        self.lock().len()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, Arc<Session>>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn get(&self, id: &Uuid) -> Option<Arc<Session>> {
        self.lock().get(id).cloned()
    }

    /// Makes the session `id`, or returns it if it exists already.
    ///
    /// Fails with `503` when `max_sessions` are in use.
    pub(crate) fn create(
        &self,
        id: Uuid,
        item_id: &str,
        profile: &str,
        plan: Plan,
    ) -> Result<Arc<Session>, ApiError> {
        let mut sessions = self.lock();
        if let Some(existing) = sessions.get(&id) {
            return Ok(existing.clone());
        }
        let permit = self
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| too_many_sessions())?;
        let dir = self.root.join(id.to_string());
        std::fs::create_dir_all(&dir).map_err(|err| ApiError::Internal(err.into()))?;
        let session = Arc::new(Session {
            id,
            item_id: item_id.to_owned(),
            profile: profile.to_owned(),
            plan,
            settings: self.settings.clone(),
            dir,
            last_used: Mutex::new(Instant::now()),
            state: tokio::sync::Mutex::new(State::default()),
            _permit: permit,
        });
        sessions.insert(id, session.clone());
        Ok(session)
    }

    /// Drops the sessions nobody has used since `idle_timeout` before `now`
    /// (and that no request is using), which kills their ffmpeg, removes
    /// their directories, and frees their slots. Returns how many.
    pub(crate) fn reap(&self, now: Instant) -> usize {
        let mut sessions = self.lock();
        let expired: Vec<Uuid> = sessions
            .iter()
            .filter(|(_, s)| {
                Arc::strong_count(s) == 1 && now.duration_since(s.last_used()) >= self.idle_timeout
            })
            .map(|(id, _)| *id)
            .collect();
        for id in &expired {
            sessions.remove(id);
        }
        expired.len()
    }

    /// Starts the task that reaps idle sessions every few seconds, until
    /// the registry is gone.
    pub(crate) fn spawn_reaper(self: &Arc<Self>) {
        let registry = Arc::downgrade(self);
        tokio::spawn(reap_loop(registry));
    }
}

/// The error for a session the server has no room for.
pub(crate) fn too_many_sessions() -> ApiError {
    ApiError::Unavailable(
        "too many transcoding sessions are running; try again later (transcode.max_sessions)"
            .to_owned(),
    )
}

async fn reap_loop(registry: Weak<Registry>) {
    let mut tick = tokio::time::interval(REAP_EVERY);
    loop {
        tick.tick().await;
        let Some(registry) = registry.upgrade() else {
            return;
        };
        let reaped = registry.reap(Instant::now());
        if reaped > 0 {
            tracing::debug!(reaped, "stopped idle transcoding sessions");
        }
    }
}

/// One player's transcode of one item at one profile.
pub(crate) struct Session {
    pub(crate) id: Uuid,
    pub(crate) item_id: String,
    pub(crate) profile: String,
    plan: Plan,
    settings: Arc<Settings>,
    dir: PathBuf,
    last_used: Mutex<Instant>,
    state: tokio::sync::Mutex<State>,
    /// The slot this session takes, given back when it is dropped.
    _permit: OwnedSemaphorePermit,
}

impl Drop for Session {
    /// ffmpeg is killed as its handle drops (`kill_on_drop`); the segments
    /// go with the directory.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[derive(Default)]
struct State {
    run: Option<Run>,
    /// Segments below this one have been deleted (or never existed since the
    /// last restart), so it is where the next cleaning starts.
    cleaned_below: u32,
}

/// A running (or finished) ffmpeg.
struct Run {
    child: Child,
    /// The segment it started at.
    start: u32,
    /// The furthest segment asked for since it started.
    furthest: u32,
    /// Its last error lines.
    stderr: Arc<Mutex<VecDeque<String>>>,
}

impl Run {
    fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.child.try_wait().unwrap_or(None)
    }

    fn tail(&self) -> String {
        let lines = self.stderr.lock().unwrap_or_else(PoisonError::into_inner);
        let text: Vec<&str> = lines.iter().map(String::as_str).collect();
        if text.is_empty() {
            "no error given".to_owned()
        } else {
            text.join("; ")
        }
    }
}

/// What [`needs_restart`] needs to know about a run.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RunView {
    pub start: u32,
    pub furthest: u32,
    pub exited: bool,
}

/// Whether segment `n`, which is not on disk, needs ffmpeg started (again)
/// at it, rather than waiting for the run there is.
pub(crate) fn needs_restart(run: Option<RunView>, n: u32, keep_segments: u32) -> bool {
    match run {
        None => true,
        Some(run) => {
            run.exited
                // Behind where it started: it will never produce it.
                || n < run.start
                // Far ahead: it would take too long to get there.
                || n > run.furthest.saturating_add(LOOKAHEAD_SEGMENTS)
                // Behind the viewer by more than is kept: deleted.
                || n.saturating_add(keep_segments) < run.furthest
        }
    }
}

impl Session {
    fn touch(&self) {
        *self
            .last_used
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Instant::now();
    }

    fn last_used(&self) -> Instant {
        *self
            .last_used
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn keep_segments(&self) -> u32 {
        KEEP_SECONDS.div_ceil(self.settings.segment_seconds)
    }

    fn segment_path(&self, n: u32) -> PathBuf {
        self.dir.join(encode::segment_file_name(n))
    }

    async fn read_segment(&self, n: u32) -> Option<Vec<u8>> {
        tokio::fs::read(self.segment_path(n)).await.ok()
    }

    /// The bytes of segment `n`, encoding it first if need be.
    ///
    /// `404` past the end of the item; `502` if ffmpeg fails or is too slow.
    pub(crate) async fn segment(&self, n: u32) -> Result<Vec<u8>, ApiError> {
        let count = hls::segment_count(self.plan.duration_ms, self.settings.segment_seconds);
        if n >= count {
            return Err(ApiError::NotFound(format!(
                "item {} has no segment {n} (it has {count})",
                self.item_id
            )));
        }
        self.touch();
        if let Some(bytes) = self.read_segment(n).await {
            self.served(n).await;
            return Ok(bytes);
        }

        {
            let mut state = self.state.lock().await;
            let view = state.run.as_mut().map(|run| RunView {
                start: run.start,
                furthest: run.furthest,
                exited: run.exited().is_some(),
            });
            if needs_restart(view, n, self.keep_segments()) {
                self.start(&mut state, n).await?;
            }
        }

        let deadline = Instant::now() + SEGMENT_WAIT;
        loop {
            tokio::time::sleep(POLL).await;
            self.touch();
            if let Some(bytes) = self.read_segment(n).await {
                self.served(n).await;
                return Ok(bytes);
            }
            let stopped = {
                let mut state = self.state.lock().await;
                state
                    .run
                    .as_mut()
                    .and_then(|run| run.exited().map(|status| (status, run.tail())))
            };
            if let Some((status, tail)) = stopped {
                // It may have written this segment just before it exited.
                if let Some(bytes) = self.read_segment(n).await {
                    self.served(n).await;
                    return Ok(bytes);
                }
                return Err(ApiError::Upstream(format!(
                    "ffmpeg stopped ({status}) before segment {n}: {tail}"
                )));
            }
            if Instant::now() >= deadline {
                return Err(ApiError::Upstream(format!(
                    "ffmpeg did not produce segment {n} within {SEGMENT_WAIT:?}"
                )));
            }
        }
    }

    /// Notes that segment `n` was served: it is the furthest, perhaps, and
    /// segments too far behind it are deleted.
    async fn served(&self, n: u32) {
        let mut state = self.state.lock().await;
        let Some(run) = state.run.as_mut() else {
            return;
        };
        run.furthest = run.furthest.max(n);
        let delete_below = run.furthest.saturating_sub(self.keep_segments());
        while state.cleaned_below < delete_below {
            let _ = tokio::fs::remove_file(self.segment_path(state.cleaned_below)).await;
            state.cleaned_below += 1;
        }
    }

    /// Stops any ffmpeg and starts one at segment `n`.
    async fn start(&self, state: &mut State, n: u32) -> Result<(), ApiError> {
        if let Some(mut old) = state.run.take() {
            let _ = old.child.start_kill();
            let _ = old.child.wait().await;
        }
        let job = Job {
            input: &self.plan.input,
            media: &self.plan.media,
            encoder: self.settings.encoder,
            vaapi_device: &self.settings.vaapi_device,
            height: self.plan.height,
            video_kbps: self.plan.video_kbps,
            threads: self.settings.threads,
        };
        let args = encode::hls_args(
            &job,
            &HlsOutput {
                start_segment: n,
                segment_seconds: self.settings.segment_seconds,
                dir: &self.dir,
            },
        );
        let mut child = Command::new(&self.settings.ffmpeg)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|err| ApiError::Internal(err.into()))?;
        let stderr = Arc::new(Mutex::new(VecDeque::new()));
        if let Some(pipe) = child.stderr.take() {
            tokio::spawn(collect_stderr(pipe, stderr.clone()));
        }
        tracing::info!(session = %self.id, item = %self.item_id, profile = %self.profile,
            start_segment = n, encoder = self.settings.encoder.encoder.name(),
            "started ffmpeg");
        state.cleaned_below = state.cleaned_below.min(n);
        state.run = Some(Run {
            child,
            start: n,
            furthest: n,
            stderr,
        });
        Ok(())
    }

    /// The segment the running ffmpeg started at, for tests.
    #[cfg(test)]
    pub(crate) async fn running_from(&self) -> Option<u32> {
        self.state.lock().await.run.as_ref().map(|run| run.start)
    }
}

/// Keeps the last [`STDERR_LINES`] lines of ffmpeg's error output.
async fn collect_stderr(pipe: tokio::process::ChildStderr, into: Arc<Mutex<VecDeque<String>>>) {
    let mut lines = BufReader::new(pipe).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let mut kept = into.lock().unwrap_or_else(PoisonError::into_inner);
        if kept.len() == STDERR_LINES {
            kept.pop_front();
        }
        kept.push_back(line);
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::config::Hardware;
    use crate::encoder::{self, Encoder};
    use crate::media::{AudioTrack, VideoStream};

    fn view(start: u32, furthest: u32, exited: bool) -> Option<RunView> {
        Some(RunView {
            start,
            furthest,
            exited,
        })
    }

    #[test]
    fn restarts_only_when_waiting_would_not_do() {
        // 50 segments are kept.
        let keep = 50;
        // Nothing running.
        assert!(needs_restart(None, 0, keep));
        // The next segment or two: wait for them.
        assert!(!needs_restart(view(0, 4, false), 5, keep));
        assert!(!needs_restart(view(0, 4, false), 7, keep));
        // Further ahead than the lookahead: restart there.
        assert!(needs_restart(view(0, 4, false), 8, keep));
        // Behind where it started: it never will.
        assert!(needs_restart(view(10, 12, false), 9, keep));
        assert!(!needs_restart(view(10, 12, false), 10, keep));
        // Behind the viewer by more than is kept: deleted.
        assert!(!needs_restart(view(0, 60, false), 10, keep));
        assert!(needs_restart(view(0, 60, false), 9, keep));
        // A run that has stopped produces nothing more.
        assert!(needs_restart(view(0, 4, true), 5, keep));
        // No overflow at the edges.
        assert!(!needs_restart(view(0, u32::MAX, false), u32::MAX, keep));
    }

    // --- With a real ffmpeg ---------------------------------------------

    fn have(program: &str) -> bool {
        std::process::Command::new(program)
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    /// A tiny test clip `seconds` long, with audio.
    fn clip(dir: &Path, seconds: u32) -> PathBuf {
        let out = dir.join("clip.mkv");
        let status = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
            .arg(format!("testsrc2=s=160x90:r=10:d={seconds}"))
            .args(["-f", "lavfi", "-i"])
            .arg(format!("sine=frequency=440:duration={seconds}"))
            .args(["-c:v", "ffv1", "-c:a", "flac", "-shortest"])
            .arg(&out)
            .status()
            .unwrap();
        assert!(status.success(), "ffmpeg could not make the test clip");
        out
    }

    fn plan(file: PathBuf, duration_ms: u64) -> Plan {
        Plan {
            input: Input::File(file),
            media: MediaInfo {
                container: "matroska,webm".into(),
                duration_ms: Some(duration_ms),
                bitrate_kbps: None,
                video: Some(VideoStream {
                    codec: "ffv1".into(),
                    profile: None,
                    bit_depth: 8,
                    width: 160,
                    height: 90,
                    fps: Some(10.0),
                    hdr: None,
                }),
                audio: vec![AudioTrack {
                    codec: "flac".into(),
                    channels: 1,
                    language: None,
                    default: true,
                }],
                subtitles: vec![],
            },
            duration_ms,
            height: 90,
            video_kbps: 200,
        }
    }

    fn settings(ffmpeg: &str, encoder: Encoder) -> Settings {
        Settings {
            ffmpeg: PathBuf::from(ffmpeg),
            encoder: Capabilities {
                encoder,
                tonemap: None,
            },
            vaapi_device: PathBuf::from("/dev/dri/renderD128"),
            segment_seconds: 2,
            threads: Some(2),
        }
    }

    fn registry(root: &Path, ffmpeg: &str, max: usize, encoder: Encoder) -> Registry {
        Registry::new(
            root.join("transcode"),
            settings(ffmpeg, encoder),
            max,
            Duration::from_secs(120),
        )
        .unwrap()
    }

    /// The software encoder this machine's ffmpeg has (libx264, or
    /// libopenh264 where that is all there is), or `None`, with a note on
    /// why, if ffmpeg, ffprobe, or an encoder is missing.
    async fn software_encoder() -> Option<Encoder> {
        if !have("ffmpeg") || !have("ffprobe") {
            eprintln!("skipping: ffmpeg or ffprobe is not installed");
            return None;
        }
        let found = encoder::detect(
            Path::new("ffmpeg"),
            Hardware::Software,
            Path::new("/dev/dri/renderD128"),
        )
        .await;
        if found.is_none() {
            eprintln!("skipping: this ffmpeg has no software H.264 encoder");
        }
        found.map(|f| f.encoder)
    }

    /// `ffprobe`'s `key=value` lines for a segment's bytes.
    fn probe(dir: &Path, bytes: &[u8]) -> String {
        let file = dir.join("probe.ts");
        std::fs::write(&file, bytes).unwrap();
        let out = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-show_entries"])
            .arg("format=start_time:stream=codec_name")
            .args(["-of", "default=noprint_wrappers=1"])
            .arg(&file)
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap()
    }

    fn start_time(probe: &str) -> f64 {
        probe
            .lines()
            .find_map(|l| l.strip_prefix("start_time="))
            .unwrap()
            .parse()
            .unwrap()
    }

    #[tokio::test]
    async fn a_session_serves_h264_and_aac_segments_from_the_start() {
        let Some(encoder) = software_encoder().await else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let file = clip(dir.path(), 7);
        let registry = registry(dir.path(), "ffmpeg", 2, encoder);
        let session = registry
            .create(Uuid::now_v7(), "item", "720p", plan(file, 7_000))
            .unwrap();

        let first = session.segment(0).await.unwrap();
        let probed = probe(dir.path(), &first);
        assert!(probed.contains("codec_name=h264"), "{probed}");
        assert!(probed.contains("codec_name=aac"), "{probed}");
        assert!(start_time(&probed) < 3.0, "{probed}");
        assert_eq!(session.running_from().await, Some(0));

        // The next segment comes from the same run; the last is the short
        // 1 s one.
        session.segment(1).await.unwrap();
        session.segment(3).await.unwrap();
        assert_eq!(session.running_from().await, Some(0));
        let past_the_end = session.segment(4).await.unwrap_err();
        assert!(matches!(past_the_end, ApiError::NotFound(_)));
    }

    #[tokio::test]
    async fn a_request_far_ahead_restarts_ffmpeg_there() {
        let Some(encoder) = software_encoder().await else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        // Ten 2 s segments.
        let file = clip(dir.path(), 20);
        let registry = registry(dir.path(), "ffmpeg", 2, encoder);
        let session = registry
            .create(Uuid::now_v7(), "item", "720p", plan(file, 20_000))
            .unwrap();
        session.segment(0).await.unwrap();

        let far = session.segment(8).await.unwrap();
        assert_eq!(session.running_from().await, Some(8));
        // Segment 8 starts 16 s in (plus the muxer's constant offset), so
        // the timeline continues from where the playlist says.
        let start = start_time(&probe(dir.path(), &far));
        assert!((16.0..18.5).contains(&start), "{start}");

        // Seeking back behind where this run started restarts there too
        // (the first run may or may not have got as far as segment 2 before
        // it was stopped, so make sure it is not on disk).
        let _ = std::fs::remove_file(session.segment_path(2));
        session.segment(2).await.unwrap();
        assert_eq!(session.running_from().await, Some(2));
    }

    #[tokio::test]
    async fn ffmpeg_failing_is_a_bad_gateway_with_its_reason() {
        if !have("ffmpeg") {
            eprintln!("skipping: ffmpeg is not installed");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let registry = registry(dir.path(), "ffmpeg", 2, Encoder::X264);
        let missing = dir.path().join("nonexistent.mkv");
        let session = registry
            .create(Uuid::now_v7(), "item", "720p", plan(missing, 6_000))
            .unwrap();
        let err = session.segment(0).await.unwrap_err();
        let ApiError::Upstream(message) = err else {
            panic!("expected a bad gateway");
        };
        assert!(message.contains("before segment 0"), "{message}");
    }

    // --- With a stand-in for ffmpeg --------------------------------------

    /// A script that writes segment 0 into the output directory (the
    /// directory of its last argument), records its process ID there, and
    /// then idles like a long encode.
    #[cfg(unix)]
    fn idling_ffmpeg(dir: &Path) -> String {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("fake-ffmpeg");
        std::fs::write(
            &script,
            "#!/bin/sh\nfor last; do :; done\nout=$(dirname \"$last\")\n\
             echo $$ > \"$out/pid\"\necho data > \"$out/tmp\"\n\
             mv \"$out/tmp\" \"$out/seg_00000.ts\"\nexec sleep 1000\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script.display().to_string()
    }

    #[cfg(unix)]
    async fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        for _ in 0..100 {
            if done() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("timed out waiting for {what}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_idle_session_is_reaped_with_its_ffmpeg_and_directory() {
        let dir = tempfile::tempdir().unwrap();
        let registry = registry(dir.path(), &idling_ffmpeg(dir.path()), 1, Encoder::X264);
        let id = Uuid::now_v7();
        let session = registry
            .create(id, "item", "720p", plan(PathBuf::from("/x.mkv"), 6_000))
            .unwrap();
        assert_eq!(session.segment(0).await.unwrap(), b"data\n");
        let session_dir = dir.path().join("transcode").join(id.to_string());
        let pid: u32 = std::fs::read_to_string(session_dir.join("pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let alive = || Path::new(&format!("/proc/{pid}")).exists();
        assert!(alive());
        assert!(!registry.has_room(), "its slot is taken");

        // Still in use (a request holds it): not reaped however idle.
        let later = Instant::now() + Duration::from_secs(600);
        assert_eq!(registry.reap(later), 0);
        drop(session);
        // Not idle for long enough yet.
        assert_eq!(registry.reap(Instant::now()), 0);
        assert_eq!(registry.len(), 1);

        assert_eq!(registry.reap(later), 1);
        assert_eq!(registry.len(), 0);
        assert!(registry.has_room(), "its slot is free again");
        assert!(!session_dir.exists(), "its directory is gone");
        wait_until("ffmpeg to be killed", || !alive()).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn only_max_sessions_run_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let registry = registry(dir.path(), "ffmpeg", 2, Encoder::X264);
        let make = |id: Uuid| registry.create(id, "item", "720p", plan(PathBuf::from("/x"), 6_000));
        let (a, b) = (Uuid::now_v7(), Uuid::now_v7());
        let first = make(a).unwrap();
        make(b).unwrap();
        let refused = make(Uuid::now_v7()).err().expect("a third is refused");
        assert!(matches!(refused, ApiError::Unavailable(_)));
        // Asking for one that exists is not a new session.
        assert!(Arc::ptr_eq(&make(a).unwrap(), &first));
        drop(first);
        registry.reap(Instant::now() + Duration::from_secs(600));
        assert!(make(Uuid::now_v7()).is_ok());
    }

    #[test]
    fn a_new_registry_empties_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("transcode");
        std::fs::create_dir_all(root.join("leftover-session")).unwrap();
        std::fs::write(root.join("leftover-session/seg_00000.ts"), "old").unwrap();
        let _registry = Registry::new(
            root.clone(),
            settings("ffmpeg", Encoder::X264),
            1,
            Duration::from_secs(60),
        )
        .unwrap();
        assert!(root.is_dir());
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    }
}
