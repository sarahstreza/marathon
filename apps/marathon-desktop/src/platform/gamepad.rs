//! Gamepad input through `gilrs`, as an
//! [`InputSource`](marathon_client::input::InputSource). The bindings live
//! in [`marathon_client::gamepad`]; this module only translates gilrs's
//! buttons and runs the polling thread.
//!
//! gilrs notices controllers being plugged in and unplugged by itself, so
//! hot-plugging needs nothing here beyond logging it. If gilrs cannot
//! start (no udev, no permission to read input devices), the app carries
//! on without gamepads.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use gilrs::ev::filter::{Filter, Repeat};
use gilrs::{Button as GilrsButton, EventType, Gilrs};
use marathon_client::bus::ActionSender;
use marathon_client::gamepad::{Button, map_button, repeats};
use marathon_client::input::{InputError, InputSource};

/// How often the thread checks whether it should stop.
const POLL: Duration = Duration::from_millis(100);

/// Held buttons that repeat (D-pad left/right, for seeking) do so after
/// this long, this often.
const REPEAT_AFTER: Duration = Duration::from_millis(500);
const REPEAT_EVERY: Duration = Duration::from_millis(200);

/// Our name for a gilrs button, if Marathon can use it.
pub fn translate(button: GilrsButton) -> Option<Button> {
    Some(match button {
        GilrsButton::South => Button::South,
        GilrsButton::East => Button::East,
        GilrsButton::North => Button::North,
        GilrsButton::West => Button::West,
        // gilrs's "trigger" is the shoulder button; "trigger 2" is the
        // trigger proper.
        GilrsButton::LeftTrigger => Button::LeftShoulder,
        GilrsButton::RightTrigger => Button::RightShoulder,
        GilrsButton::LeftTrigger2 => Button::LeftTrigger,
        GilrsButton::RightTrigger2 => Button::RightTrigger,
        GilrsButton::Select => Button::Select,
        GilrsButton::Start => Button::Start,
        GilrsButton::Mode => Button::Mode,
        GilrsButton::LeftThumb => Button::LeftStick,
        GilrsButton::RightThumb => Button::RightStick,
        GilrsButton::DPadUp => Button::DPadUp,
        GilrsButton::DPadDown => Button::DPadDown,
        GilrsButton::DPadLeft => Button::DPadLeft,
        GilrsButton::DPadRight => Button::DPadRight,
        GilrsButton::C | GilrsButton::Z | GilrsButton::Unknown => return None,
    })
}

/// The gamepad [`InputSource`].
#[derive(Default)]
pub struct GamepadSource {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl GamepadSource {
    pub fn new() -> Self {
        Self::default()
    }
}

fn run(stop: &AtomicBool, actions: &ActionSender) {
    let mut gilrs = match Gilrs::new() {
        Ok(gilrs) => gilrs,
        Err(err) => {
            tracing::warn!(%err, "gamepads unavailable");
            return;
        }
    };
    for (_, pad) in gilrs.gamepads() {
        tracing::info!(name = pad.name(), "gamepad connected");
    }
    let repeat = Repeat {
        after: REPEAT_AFTER,
        every: REPEAT_EVERY,
    };
    while !stop.load(Ordering::SeqCst) {
        // The repeat filter must also see the timeouts (None), which is
        // when it generates repeats.
        let event = gilrs.next_event_blocking(Some(POLL));
        let Some(event) = event.filter_ev(&repeat, &mut gilrs) else {
            continue;
        };
        let (button, repeated) = match event.event {
            EventType::ButtonPressed(button, _) => (button, false),
            EventType::ButtonRepeated(button, _) => (button, true),
            EventType::Connected => {
                let pad = gilrs.gamepad(event.id);
                tracing::info!(name = pad.name(), "gamepad connected");
                continue;
            }
            EventType::Disconnected => {
                let pad = gilrs.gamepad(event.id);
                tracing::info!(name = pad.name(), "gamepad disconnected");
                continue;
            }
            _ => continue,
        };
        let Some(button) = translate(button) else {
            continue;
        };
        if repeated && !repeats(button) {
            continue;
        }
        if let Some(action) = map_button(button)
            && actions.send(action).is_err()
        {
            return;
        }
    }
}

impl InputSource for GamepadSource {
    fn name(&self) -> &str {
        "gamepad"
    }

    fn start(&mut self, actions: ActionSender) -> Result<(), InputError> {
        if self.thread.is_some() {
            return Err(InputError {
                source_name: "gamepad".into(),
                message: "already started".into(),
            });
        }
        let stop = self.stop.clone();
        let thread = std::thread::Builder::new()
            .name("input-gamepad".into())
            .spawn(move || run(&stop, &actions))
            .map_err(|e| InputError {
                source_name: "gamepad".into(),
                message: e.to_string(),
            })?;
        self.thread = Some(thread);
        Ok(())
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("gamepad input thread panicked");
        }
    }
}

#[cfg(test)]
mod tests {
    use marathon_api::{Action, Direction};

    use super::*;

    /// Every gamepad binding in PLAN.md §9 (and M5's additions), from the
    /// gilrs button to the Action.
    #[test]
    fn every_binding_from_gilrs_to_action() {
        let nav = |direction| Action::Navigate { direction };
        let cases = [
            (GilrsButton::RightTrigger, Some(Action::ChannelUp)),
            (GilrsButton::LeftTrigger, Some(Action::ChannelDown)),
            (GilrsButton::DPadUp, Some(nav(Direction::Up))),
            (GilrsButton::DPadDown, Some(nav(Direction::Down))),
            (GilrsButton::DPadLeft, Some(nav(Direction::Left))),
            (GilrsButton::DPadRight, Some(nav(Direction::Right))),
            (GilrsButton::South, Some(Action::Select)),
            (GilrsButton::East, Some(Action::Back)),
            (GilrsButton::Start, Some(Action::TogglePause)),
            (GilrsButton::North, Some(Action::ToggleGuide)),
            (GilrsButton::West, Some(Action::ToggleSubtitles)),
            (GilrsButton::RightTrigger2, Some(Action::NextItem)),
            (GilrsButton::LeftTrigger2, Some(Action::PreviousItem)),
            (GilrsButton::Select, Some(Action::ShowInfo)),
            (GilrsButton::LeftThumb, Some(Action::ToggleFullscreen)),
            (GilrsButton::Mode, Some(Action::Quit)),
            (GilrsButton::RightThumb, None),
            (GilrsButton::C, None),
            (GilrsButton::Z, None),
            (GilrsButton::Unknown, None),
        ];
        for (button, expected) in cases {
            assert_eq!(
                translate(button).and_then(map_button),
                expected,
                "{button:?}"
            );
        }
    }

    #[test]
    fn every_one_of_our_buttons_has_a_gilrs_button() {
        let all = [
            GilrsButton::South,
            GilrsButton::East,
            GilrsButton::North,
            GilrsButton::West,
            GilrsButton::LeftTrigger,
            GilrsButton::LeftTrigger2,
            GilrsButton::RightTrigger,
            GilrsButton::RightTrigger2,
            GilrsButton::Select,
            GilrsButton::Start,
            GilrsButton::Mode,
            GilrsButton::LeftThumb,
            GilrsButton::RightThumb,
            GilrsButton::DPadUp,
            GilrsButton::DPadDown,
            GilrsButton::DPadLeft,
            GilrsButton::DPadRight,
        ];
        let ours: Vec<Button> = all.into_iter().filter_map(translate).collect();
        for button in Button::ALL {
            assert!(ours.contains(&button), "{button:?}");
        }
    }

    #[test]
    fn starts_and_stops_with_or_without_gamepads() {
        let (bus, _rx) = marathon_client::bus::ActionBus::channel();
        let mut source = GamepadSource::new();
        source.start(bus.sender("gamepad")).unwrap();
        assert!(source.start(bus.sender("gamepad")).is_err());
        source.stop();
    }
}
