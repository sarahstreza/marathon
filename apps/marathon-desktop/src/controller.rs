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

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use marathon_api::Action;
use marathon_client::api::HttpApi;
use marathon_client::tuner::{Flow, SystemClock, Tuner};
use marathon_mpv::{Event, Events, Format, LogLevel, PropertyValue};

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

pub type DesktopTuner = Tuner<HttpApi, MpvPlayer, SystemClock>;

/// Runs the tuner until the viewer quits or the window goes away.
/// `start_channel` is passed to [`Tuner::start`].
pub fn run(
    mut tuner: DesktopTuner,
    mut events: Events,
    inbox: Receiver<Msg>,
    wake: Sender<Msg>,
    start_channel: Option<u32>,
) -> Exit {
    events.set_wakeup_callback(move || {
        // The receiver only goes away when this thread is exiting.
        let _ = wake.send(Msg::MpvWakeup);
    });
    if let Err(err) =
        tuner
            .player()
            .mpv()
            .observe_property("hwdec-current", Format::String, OBSERVE_HWDEC)
    {
        tracing::warn!(%err, "cannot observe hwdec-current");
    }

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
                if drain_events(&mut tuner, &mut events).is_break() {
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
fn drain_events(tuner: &mut DesktopTuner, events: &mut Events) -> std::ops::ControlFlow<()> {
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
