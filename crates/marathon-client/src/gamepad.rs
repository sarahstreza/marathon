//! Gamepad bindings (PLAN.md §9). Behind the `input-gamepad` feature.
//!
//! Only the mapping lives here, over a [`Button`] type of our own; the
//! gamepad library (`gilrs`) stays in the app's platform layer, which
//! translates its buttons into these and runs the
//! [`InputSource`](crate::input::InputSource). Names follow the usual
//! layout-neutral convention: South is A on Xbox and Cross on PlayStation.
//!
//! | Button | Action |
//! |---|---|
//! | Right / left shoulder | [`Action::ChannelUp`] / [`Action::ChannelDown`] |
//! | D-pad | [`Action::Navigate`] |
//! | South (A) | [`Action::Select`] (shows info with nothing open) |
//! | East (B) | [`Action::Back`] |
//! | Start | [`Action::TogglePause`] |
//! | North (Y) | [`Action::ToggleGuide`] |
//! | West (X) | [`Action::ToggleSubtitles`] |
//! | Right / left trigger | [`Action::NextItem`] / [`Action::PreviousItem`] |
//! | Select (Back/View/Share) | [`Action::ShowInfo`] |
//! | Left stick click | [`Action::ToggleFullscreen`] |
//! | Mode (Xbox/PS/Home) | [`Action::Quit`] |
//!
//! Beyond §9's shoulders, D-pad, A, B, and Start, the rest give a
//! controller every action the keyboard has (M5). Typing a channel number
//! has no button; the guide covers it.

use marathon_api::{Action, Direction};

/// A gamepad button, independent of gamepad library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Button {
    South,
    East,
    North,
    West,
    LeftShoulder,
    RightShoulder,
    LeftTrigger,
    RightTrigger,
    Select,
    Start,
    Mode,
    LeftStick,
    RightStick,
    DPadUp,
    DPadDown,
    DPadLeft,
    DPadRight,
}

impl Button {
    /// Every button, for exhaustive tests.
    pub const ALL: [Button; 17] = [
        Button::South,
        Button::East,
        Button::North,
        Button::West,
        Button::LeftShoulder,
        Button::RightShoulder,
        Button::LeftTrigger,
        Button::RightTrigger,
        Button::Select,
        Button::Start,
        Button::Mode,
        Button::LeftStick,
        Button::RightStick,
        Button::DPadUp,
        Button::DPadDown,
        Button::DPadLeft,
        Button::DPadRight,
    ];
}

/// The action for a button press, if it is bound.
pub fn map_button(button: Button) -> Option<Action> {
    let navigate = |direction| Action::Navigate { direction };
    Some(match button {
        Button::RightShoulder => Action::ChannelUp,
        Button::LeftShoulder => Action::ChannelDown,
        Button::DPadUp => navigate(Direction::Up),
        Button::DPadDown => navigate(Direction::Down),
        Button::DPadLeft => navigate(Direction::Left),
        Button::DPadRight => navigate(Direction::Right),
        Button::South => Action::Select,
        Button::East => Action::Back,
        Button::Start => Action::TogglePause,
        Button::North => Action::ToggleGuide,
        Button::West => Action::ToggleSubtitles,
        Button::RightTrigger => Action::NextItem,
        Button::LeftTrigger => Action::PreviousItem,
        Button::Select => Action::ShowInfo,
        Button::LeftStick => Action::ToggleFullscreen,
        Button::Mode => Action::Quit,
        Button::RightStick => return None,
    })
}

/// Whether holding the button should repeat its action. As with the
/// keyboard, only seeking does: a held Up would flip channel after channel.
pub fn repeats(button: Button) -> bool {
    matches!(button, Button::DPadLeft | Button::DPadRight)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every gamepad binding: PLAN.md §9's, plus the ones that cover the
    /// rest of the keyboard's actions.
    #[test]
    fn every_binding_maps_to_its_action() {
        let nav = |direction| Action::Navigate { direction };
        let cases = [
            (Button::RightShoulder, Some(Action::ChannelUp)),
            (Button::LeftShoulder, Some(Action::ChannelDown)),
            (Button::DPadUp, Some(nav(Direction::Up))),
            (Button::DPadDown, Some(nav(Direction::Down))),
            (Button::DPadLeft, Some(nav(Direction::Left))),
            (Button::DPadRight, Some(nav(Direction::Right))),
            (Button::South, Some(Action::Select)),
            (Button::East, Some(Action::Back)),
            (Button::Start, Some(Action::TogglePause)),
            (Button::North, Some(Action::ToggleGuide)),
            (Button::West, Some(Action::ToggleSubtitles)),
            (Button::RightTrigger, Some(Action::NextItem)),
            (Button::LeftTrigger, Some(Action::PreviousItem)),
            (Button::Select, Some(Action::ShowInfo)),
            (Button::LeftStick, Some(Action::ToggleFullscreen)),
            (Button::Mode, Some(Action::Quit)),
            (Button::RightStick, None),
        ];
        assert_eq!(cases.len(), Button::ALL.len(), "a button is untested");
        for (button, action) in cases {
            assert_eq!(map_button(button), action, "{button:?}");
        }
    }

    /// Every action a key can produce, a button can too (apart from typed
    /// channel numbers, which the guide stands in for).
    #[cfg(feature = "input-keyboard")]
    #[test]
    fn a_controller_can_do_everything_the_keyboard_can() {
        use crate::keyboard::{Key, map_key};
        let buttons: Vec<Action> = Button::ALL.into_iter().filter_map(map_button).collect();
        let named = [
            Key::PageUp,
            Key::PageDown,
            Key::ArrowUp,
            Key::ArrowDown,
            Key::ArrowLeft,
            Key::ArrowRight,
            Key::Space,
            Key::Enter,
            Key::Escape,
        ];
        let printable = (' '..='~').map(Key::Character);
        for key in named.into_iter().chain(printable) {
            if let Some(action) = map_key(key) {
                assert!(buttons.contains(&action), "no button for {action:?}");
            }
        }
    }

    #[test]
    fn only_seeking_repeats() {
        for button in Button::ALL {
            let seek = matches!(button, Button::DPadLeft | Button::DPadRight);
            assert_eq!(repeats(button), seek, "{button:?}");
        }
    }
}
