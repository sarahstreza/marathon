//! Where actions go, and the tuner thread.
//!
//! [`route`] splits actions from the bus: window actions go to the
//! platform, everything else to the tuner thread.
//!
//! The tuner thread owns the [`Tuner`] and mpv's event queue, so API calls
//! (which block) never stall the render thread. It waits on one channel
//! for [`Msg`]s: the start signal, actions from the bus, and wakeups from
//! mpv's event callback. Between messages it wakes for the tuner's
//! periodic progress reports.
//!
//! It also feeds the overlay: the tuner's cues and error events, and the
//! playback position, duration, and pause state that mpv reports, so the
//! render thread never has to query mpv (which can block while mpv is
//! busy opening a stream).

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use marathon_api::Action;
use marathon_client::api::HttpApi;
use marathon_client::tuner::{Flow, SystemClock, Tuner};
use marathon_mpv::{Event, Events, Format, LogLevel, PropertyValue};

use crate::overlay::{OverlayFeed, OverlayMsg, Progress};
use crate::player::MpvPlayer;

/// Where an action is handled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// By the window: fullscreen, or quitting (which shuts everything down
    /// in order, including the tuner).
    Window(WindowCommand),
    /// By the tuner thread.
    Tuner(Action),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowCommand {
    ToggleFullscreen,
    Quit,
}

/// Decides where an action from the bus goes.
pub fn route(action: Action) -> Route {
    match action {
        Action::ToggleFullscreen => Route::Window(WindowCommand::ToggleFullscreen),
        Action::Quit => Route::Window(WindowCommand::Quit),
        other => Route::Tuner(other),
    }
}

/// Messages to the tuner thread.
#[derive(Debug)]
pub enum Msg {
    /// The window and mpv's render context exist: tune in. mpv cannot open
    /// a video output (`vo=libmpv`) before then.
    Start,
    Action(Action),
    /// mpv has events waiting.
    MpvWakeup,
}

/// Why the tuner thread stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The viewer quit; progress was reported.
    Quit,
    /// mpv shut down, or the window went away.
    Gone,
}

/// Property observation IDs.
const OBSERVE_HWDEC: u64 = 1;
const OBSERVE_TIME_POS: u64 = 2;
const OBSERVE_DURATION: u64 = 3;
const OBSERVE_PAUSE: u64 = 4;

/// How often, at most, steady playback progress is passed to the overlay,
/// which extrapolates in between.
const PROGRESS_EVERY: Duration = Duration::from_millis(500);

/// Passes mpv's playback state to the overlay: every change of duration,
/// pause, or availability, and jumps (seeks), but steady progress only
/// every [`PROGRESS_EVERY`], since mpv reports `time-pos` every frame.
#[derive(Debug)]
pub struct ProgressForwarder {
    current: Progress,
    sent: Option<(Progress, Instant)>,
}

impl ProgressForwarder {
    pub fn new() -> Self {
        Self {
            current: Progress::default(),
            sent: None,
        }
    }

    /// Applies an observed property change. Returns the progress to send,
    /// if it should be sent now.
    pub fn observe(&mut self, id: u64, value: &PropertyValue, now: Instant) -> Option<Progress> {
        let ms = |value: &PropertyValue| match value {
            PropertyValue::Double(s) if s.is_finite() && *s >= 0.0 => {
                Some((s * 1000.0).round() as u64)
            }
            _ => None,
        };
        match id {
            OBSERVE_TIME_POS => self.current.position_ms = ms(value),
            OBSERVE_DURATION => self.current.duration_ms = ms(value),
            OBSERVE_PAUSE => self.current.paused = matches!(value, PropertyValue::Flag(true)),
            _ => return None,
        }
        let due = match self.sent {
            None => true,
            Some((sent, at)) => {
                let cur = self.current;
                let expected = sent.position_ms.map(|p| {
                    let elapsed = if sent.paused {
                        0
                    } else {
                        u64::try_from(now.saturating_duration_since(at).as_millis())
                            .unwrap_or(u64::MAX)
                    };
                    p.saturating_add(elapsed)
                });
                let jumped = match (cur.position_ms, expected) {
                    (Some(p), Some(e)) => p.abs_diff(e) > 1000,
                    (a, b) => a.is_some() != b.is_some(),
                };
                cur.duration_ms != sent.duration_ms
                    || cur.paused != sent.paused
                    || jumped
                    || now >= at + PROGRESS_EVERY
            }
        };
        if due && self.sent.map(|(p, _)| p) != Some(self.current) {
            self.sent = Some((self.current, now));
            Some(self.current)
        } else {
            None
        }
    }
}

pub type DesktopTuner = Tuner<HttpApi, MpvPlayer, SystemClock>;

/// Runs the tuner until the viewer quits or the window goes away.
/// `start_channel` is passed to [`Tuner::start`].
pub fn run(
    mut tuner: DesktopTuner,
    mut events: Events,
    inbox: Receiver<Msg>,
    wake: Sender<Msg>,
    overlay: OverlayFeed,
    start_channel: Option<u32>,
) -> Exit {
    events.set_wakeup_callback(move || {
        // The receiver only goes away when this thread is exiting.
        let _ = wake.send(Msg::MpvWakeup);
    });
    for (name, format, id) in [
        ("hwdec-current", Format::String, OBSERVE_HWDEC),
        ("time-pos", Format::Double, OBSERVE_TIME_POS),
        ("duration", Format::Double, OBSERVE_DURATION),
        ("pause", Format::Flag, OBSERVE_PAUSE),
    ] {
        if let Err(err) = tuner.player().mpv().observe_property(name, format, id) {
            tracing::warn!(%err, "cannot observe {name}");
        }
    }
    let mut progress = ProgressForwarder::new();

    loop {
        let timeout = tuner
            .next_deadline()
            .map(|d| d.saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::from_secs(3600));
        match inbox.recv_timeout(timeout) {
            Ok(Msg::Start) => {
                if let Err(err) = tuner.start(start_channel) {
                    tracing::error!(%err, "cannot tune in");
                }
            }
            Ok(Msg::Action(action)) => {
                let what = format!("{action:?}");
                match tuner.handle_action(action) {
                    Ok(Flow::Quit) => return Exit::Quit,
                    Ok(Flow::Continue) => {}
                    Err(err) => tracing::warn!(action = what, %err, "action failed"),
                }
            }
            Ok(Msg::MpvWakeup) => {
                if drain_events(&mut tuner, &mut events, &mut progress, &overlay).is_break() {
                    return Exit::Gone;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                // The window is gone without a Quit; save our place anyway.
                tuner.report_progress();
                return Exit::Gone;
            }
        }
        tuner.tick();
    }
}

/// Handles every queued mpv event. Breaks if mpv is shutting down.
fn drain_events(
    tuner: &mut DesktopTuner,
    events: &mut Events,
    progress: &mut ProgressForwarder,
    overlay: &OverlayFeed,
) -> std::ops::ControlFlow<()> {
    while let Some(event) = events.wait(0.0) {
        match &event {
            Event::Shutdown => return std::ops::ControlFlow::Break(()),
            Event::LogMessage {
                prefix,
                level,
                text,
            } => log_mpv(prefix, *level, text.trim_end()),
            Event::PropertyChange {
                id: OBSERVE_HWDEC,
                value,
                ..
            } => {
                if let PropertyValue::String(hwdec) = value
                    && !hwdec.is_empty()
                {
                    tracing::info!(hwdec_current = %hwdec, "hardware decoding (\"no\" means software)");
                }
            }
            Event::PropertyChange { id, value, .. } => {
                if let Some(p) = progress.observe(*id, value, Instant::now()) {
                    overlay.send(OverlayMsg::Progress(p));
                }
            }
            _ => {}
        }
        if let Some(player_event) = tuner.player_mut().on_event(&event)
            && let Err(err) = tuner.handle_player_event(player_event)
        {
            tracing::warn!(%err, "cannot handle player event");
        }
    }
    std::ops::ControlFlow::Continue(())
}

/// Forwards an mpv log line to `tracing` under the `mpv` target. mpv's
/// informational chatter goes to debug.
fn log_mpv(prefix: &str, level: LogLevel, text: &str) {
    match level {
        LogLevel::Fatal | LogLevel::Error => tracing::error!(target: "mpv", "[{prefix}] {text}"),
        LogLevel::Warn => tracing::warn!(target: "mpv", "[{prefix}] {text}"),
        LogLevel::Info => tracing::debug!(target: "mpv", "[{prefix}] {text}"),
        LogLevel::Verbose | LogLevel::Debug => {
            tracing::trace!(target: "mpv", "[{prefix}] {text}")
        }
    }
}

#[cfg(test)]
mod tests {
    use marathon_api::Direction;

    use super::*;

    #[test]
    fn progress_is_forwarded_on_changes_and_throttled_while_steady() {
        let mut fwd = ProgressForwarder::new();
        let t0 = Instant::now();
        let pos = |s: f64| PropertyValue::Double(s);
        let at = |ms| t0 + Duration::from_millis(ms);

        let first = fwd.observe(OBSERVE_TIME_POS, &pos(10.0), t0).unwrap();
        assert_eq!(first.position_ms, Some(10_000));
        let with_duration = fwd.observe(OBSERVE_DURATION, &pos(60.0), at(1)).unwrap();
        assert_eq!(with_duration.duration_ms, Some(60_000));

        // Steady playback: held back until the interval passes.
        assert_eq!(fwd.observe(OBSERVE_TIME_POS, &pos(10.04), at(40)), None);
        assert_eq!(fwd.observe(OBSERVE_TIME_POS, &pos(10.3), at(300)), None);
        let steady = fwd.observe(OBSERVE_TIME_POS, &pos(10.5), at(501)).unwrap();
        assert_eq!(steady.position_ms, Some(10_500));

        // A seek is sent at once.
        let seek = fwd.observe(OBSERVE_TIME_POS, &pos(40.0), at(540)).unwrap();
        assert_eq!(seek.position_ms, Some(40_000));
        // So is pausing, and while paused the position is not expected to
        // move.
        let pause = PropertyValue::Flag(true);
        assert!(fwd.observe(OBSERVE_PAUSE, &pause, at(600)).unwrap().paused);
        assert_eq!(fwd.observe(OBSERVE_TIME_POS, &pos(40.0), at(5000)), None);
        // An unloaded file (no position) is sent at once.
        let gone = fwd.observe(OBSERVE_TIME_POS, &PropertyValue::None, at(5001));
        assert_eq!(gone.unwrap().position_ms, None);
        // Unrelated properties are not progress.
        assert_eq!(fwd.observe(OBSERVE_HWDEC, &pos(1.0), at(6000)), None);
    }

    #[test]
    fn window_actions_go_to_the_window_and_the_rest_to_the_tuner() {
        assert_eq!(
            route(Action::ToggleFullscreen),
            Route::Window(WindowCommand::ToggleFullscreen)
        );
        assert_eq!(route(Action::Quit), Route::Window(WindowCommand::Quit));
        for action in [
            Action::ChannelUp,
            Action::Navigate {
                direction: Direction::Up,
            },
            Action::Standby,
            Action::GoToChannel { number: 3 },
        ] {
            assert_eq!(route(action.clone()), Route::Tuner(action));
        }
    }
}
