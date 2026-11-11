//! Mouse input (PLAN.md §9 bindings), as an
//! [`InputSource`](crate::input::InputSource). Behind the `input-mouse`
//! feature.
//!
//! | Input | Action |
//! |---|---|
//! | wheel up / down | [`Action::ChannelUp`] / [`Action::ChannelDown`] |
//! | left click | [`Action::Select`] |
//!
//! (Moving the mouse shows the cursor; that is the window's business, not
//! an action.)
//!
//! As with the keyboard, the windowing layer translates its events into
//! [`MouseInput`] and pushes them into a [`MouseFeed`]. The mapping needs
//! no timers, so the source runs no thread: the feed applies it on the
//! caller's thread and sends straight to the bus.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use marathon_api::Action;

use crate::bus::ActionSender;
use crate::input::{InputError, InputSource};

/// Wheel travel in pixels (from touchpads and smooth-scrolling mice) that
/// counts as one notch.
pub const PIXELS_PER_NOTCH: f64 = 50.0;

/// After the wheel changes channel, further scrolling is ignored for this
/// long. A touchpad flick or a free-spinning wheel would otherwise queue a
/// burst of channel changes.
pub const WHEEL_COOLDOWN: Duration = Duration::from_millis(150);

/// Mouse input, independent of windowing library.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MouseInput {
    /// The wheel turned by this many notches; positive is up (away from
    /// the viewer).
    WheelLines(f64),
    /// Smooth scrolling by this many pixels; positive is up.
    WheelPixels(f64),
    /// The primary button was pressed.
    Click,
}

/// Turns mouse input into actions, adding up partial wheel travel.
#[derive(Debug, Default)]
pub struct Mouse {
    /// Notches of travel not yet acted on; the sign is the direction.
    travel: f64,
    last_step: Option<Instant>,
}

impl Mouse {
    pub fn new() -> Self {
        Self::default()
    }

    /// Handles one input at time `now`.
    pub fn input(&mut self, input: MouseInput, now: Instant) -> Option<Action> {
        let notches = match input {
            MouseInput::Click => return Some(Action::Select),
            MouseInput::WheelLines(n) => n,
            MouseInput::WheelPixels(px) => px / PIXELS_PER_NOTCH,
        };
        if !notches.is_finite() || notches == 0.0 {
            return None;
        }
        if self.last_step.is_some_and(|t| now < t + WHEEL_COOLDOWN) {
            self.travel = 0.0;
            return None;
        }
        // Reversing direction starts over.
        if self.travel * notches < 0.0 {
            self.travel = 0.0;
        }
        self.travel += notches;
        if self.travel.abs() < 1.0 {
            return None;
        }
        let up = self.travel > 0.0;
        self.travel = 0.0;
        self.last_step = Some(now);
        Some(if up {
            Action::ChannelUp
        } else {
            Action::ChannelDown
        })
    }
}

type Shared = Arc<Mutex<Option<(Mouse, ActionSender)>>>;

/// Where the windowing layer pushes mouse input. Clone freely; input sent
/// while the mouse source is not running is dropped.
#[derive(Clone)]
pub struct MouseFeed(Shared);

impl MouseFeed {
    pub fn input(&self, input: MouseInput) {
        let mut guard = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let Some((mouse, sender)) = guard.as_mut() else {
            return;
        };
        if let Some(action) = mouse.input(input, Instant::now())
            && sender.send(action).is_err()
        {
            // The bus is closed: stop, as an input source should.
            *guard = None;
        }
    }
}

/// The mouse [`InputSource`].
pub struct MouseSource(Shared);

impl MouseSource {
    /// The source, and the feed the windowing layer pushes input into.
    pub fn new() -> (Self, MouseFeed) {
        let shared = Shared::default();
        (Self(shared.clone()), MouseFeed(shared))
    }
}

impl InputSource for MouseSource {
    fn name(&self) -> &str {
        "mouse"
    }

    fn start(&mut self, actions: ActionSender) -> Result<(), InputError> {
        let mut guard = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if guard.is_some() {
            return Err(InputError {
                source_name: "mouse".into(),
                message: "already started".into(),
            });
        }
        *guard = Some((Mouse::new(), actions));
        Ok(())
    }

    fn stop(&mut self) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::ActionBus;

    #[test]
    fn wheel_notches_change_channel_and_click_selects() {
        let mut mouse = Mouse::new();
        let t = Instant::now();
        assert_eq!(
            mouse.input(MouseInput::WheelLines(1.0), t),
            Some(Action::ChannelUp)
        );
        let t = t + WHEEL_COOLDOWN;
        assert_eq!(
            mouse.input(MouseInput::WheelLines(-1.0), t),
            Some(Action::ChannelDown)
        );
        assert_eq!(mouse.input(MouseInput::Click, t), Some(Action::Select));
    }

    #[test]
    fn smooth_scrolling_adds_up_to_a_notch() {
        let mut mouse = Mouse::new();
        let t = Instant::now();
        let step = MouseInput::WheelPixels(PIXELS_PER_NOTCH / 2.0);
        assert_eq!(mouse.input(step, t), None);
        assert_eq!(mouse.input(step, t), Some(Action::ChannelUp));
        // Reversing direction discards partial travel.
        let t = t + WHEEL_COOLDOWN;
        assert_eq!(mouse.input(step, t), None);
        let back = MouseInput::WheelPixels(-PIXELS_PER_NOTCH * 0.6);
        assert_eq!(mouse.input(back, t), None);
        assert_eq!(mouse.input(back, t), Some(Action::ChannelDown));
    }

    #[test]
    fn a_flick_changes_channel_once() {
        let mut mouse = Mouse::new();
        let t = Instant::now();
        let flick = MouseInput::WheelPixels(PIXELS_PER_NOTCH * 3.0);
        assert_eq!(mouse.input(flick, t), Some(Action::ChannelUp));
        for ms in [10, 50, 100, 149] {
            let later = t + Duration::from_millis(ms);
            assert_eq!(mouse.input(flick, later), None, "{ms}ms");
        }
        assert_eq!(
            mouse.input(flick, t + WHEEL_COOLDOWN),
            Some(Action::ChannelUp)
        );
    }

    #[test]
    fn nonsense_wheel_values_are_ignored() {
        let mut mouse = Mouse::new();
        let t = Instant::now();
        for n in [0.0, f64::NAN, f64::INFINITY] {
            assert_eq!(mouse.input(MouseInput::WheelLines(n), t), None);
        }
    }

    #[test]
    fn source_delivers_only_while_running() {
        let (bus, rx) = ActionBus::channel();
        let (mut source, feed) = MouseSource::new();
        feed.input(MouseInput::Click);
        assert!(rx.try_recv().is_err(), "not started yet");

        source.start(bus.sender("mouse")).unwrap();
        assert!(source.start(bus.sender("mouse")).is_err());
        feed.input(MouseInput::Click);
        let got = rx.try_recv().unwrap();
        assert_eq!((&*got.source, got.action), ("mouse", Action::Select));

        source.stop();
        feed.input(MouseInput::Click);
        assert!(rx.try_recv().is_err(), "stopped");
    }
}
