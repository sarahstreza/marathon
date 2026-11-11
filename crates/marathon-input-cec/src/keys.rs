//! TV remote keys to [`Action`]s (PLAN.md M7).
//!
//! The TV forwards remote presses over CEC as "user control" codes, from
//! the CEC specification (libcec's `cec_user_control_code`). Which keys a
//! TV forwards varies by make; the D-pad, OK, Back, and the play controls
//! are nearly universal, channel and number keys common, Info and Guide
//! less so.
//!
//! | Key | Action |
//! |---|---|
//! | D-pad | [`Action::Navigate`] (channel up/down and seek with nothing open) |
//! | OK / Select | [`Action::Select`] |
//! | Back / Return / Exit | [`Action::Back`] |
//! | Channel +/- | [`Action::ChannelUp`] / [`Action::ChannelDown`] |
//! | 0–9 | a channel number, committed by Enter or after a pause |
//! | Play / Pause / Play-pause | [`Action::Play`] / [`Action::Pause`] / [`Action::TogglePause`] |
//! | Stop | [`Action::Pause`] (a channel never stops) |
//! | Rewind / Fast forward | [`Action::SeekRelative`] ∓10s |
//! | Skip back / Skip forward | [`Action::PreviousItem`] / [`Action::NextItem`] |
//! | Info / Display | [`Action::ShowInfo`] |
//! | Guide / Menu / Contents / Channel list | [`Action::ToggleGuide`] |
//! | Red / Green / Yellow / Blue | whatever [`ColourButtons`] says (nothing by default) |
//!
//! As with the keyboard, only seeking repeats while a key is held.

use std::time::Instant;

use marathon_api::{Action, Direction};
use marathon_client::digits::ChannelNumberEntry;

/// CEC user control codes (CEC 1.4 table 27), as libcec reports them.
pub mod code {
    pub const SELECT: u8 = 0x00;
    pub const UP: u8 = 0x01;
    pub const DOWN: u8 = 0x02;
    pub const LEFT: u8 = 0x03;
    pub const RIGHT: u8 = 0x04;
    pub const ROOT_MENU: u8 = 0x09;
    pub const SETUP_MENU: u8 = 0x0A;
    pub const CONTENTS_MENU: u8 = 0x0B;
    pub const EXIT: u8 = 0x0D;
    pub const NUMBER_0: u8 = 0x20;
    pub const NUMBER_9: u8 = 0x29;
    pub const ENTER: u8 = 0x2B;
    pub const CHANNEL_UP: u8 = 0x30;
    pub const CHANNEL_DOWN: u8 = 0x31;
    pub const PREVIOUS_CHANNEL: u8 = 0x32;
    pub const DISPLAY_INFORMATION: u8 = 0x35;
    pub const PLAY: u8 = 0x44;
    pub const STOP: u8 = 0x45;
    pub const PAUSE: u8 = 0x46;
    pub const REWIND: u8 = 0x48;
    pub const FAST_FORWARD: u8 = 0x49;
    pub const FORWARD: u8 = 0x4B;
    pub const BACKWARD: u8 = 0x4C;
    pub const ELECTRONIC_PROGRAM_GUIDE: u8 = 0x53;
    pub const PLAY_FUNCTION: u8 = 0x60;
    pub const PAUSE_PLAY_FUNCTION: u8 = 0x61;
    pub const STOP_FUNCTION: u8 = 0x64;
    pub const F1_BLUE: u8 = 0x71;
    pub const F2_RED: u8 = 0x72;
    pub const F3_GREEN: u8 = 0x73;
    pub const F4_YELLOW: u8 = 0x74;
    /// Samsung's Return key (libcec's `AN_RETURN`).
    pub const AN_RETURN: u8 = 0x91;
    /// Samsung's channel list key (libcec's `AN_CHANNELS_LIST`).
    pub const AN_CHANNELS_LIST: u8 = 0x96;
}

/// How far Rewind and Fast forward seek.
const SEEK_SECONDS: i32 = 10;

/// A colour button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Colour {
    Red,
    Green,
    Yellow,
    Blue,
}

impl Colour {
    pub const ALL: [Colour; 4] = [Colour::Red, Colour::Green, Colour::Yellow, Colour::Blue];

    pub fn name(self) -> &'static str {
        match self {
            Colour::Red => "red",
            Colour::Green => "green",
            Colour::Yellow => "yellow",
            Colour::Blue => "blue",
        }
    }
}

/// What the colour buttons do. Nothing, unless configured.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColourButtons {
    pub red: Option<Action>,
    pub green: Option<Action>,
    pub yellow: Option<Action>,
    pub blue: Option<Action>,
}

impl ColourButtons {
    pub fn get(&self, colour: Colour) -> Option<&Action> {
        match colour {
            Colour::Red => self.red.as_ref(),
            Colour::Green => self.green.as_ref(),
            Colour::Yellow => self.yellow.as_ref(),
            Colour::Blue => self.blue.as_ref(),
        }
    }

    pub fn set(&mut self, colour: Colour, action: Option<Action>) {
        let slot = match colour {
            Colour::Red => &mut self.red,
            Colour::Green => &mut self.green,
            Colour::Yellow => &mut self.yellow,
            Colour::Blue => &mut self.blue,
        };
        *slot = action;
    }
}

/// Parses an action as the user writes it in configuration: an action
/// name for actions without data (`toggle_guide`), or the action's JSON
/// (`{"action":"go_to_channel","number":4}`), as in the HTTP API.
pub fn parse_action(text: &str) -> Result<Action, String> {
    let text = text.trim();
    let value = if text.starts_with('{') {
        serde_json::from_str(text).map_err(|e| e.to_string())?
    } else {
        serde_json::json!({ "action": text })
    };
    serde_json::from_value(value).map_err(|e| format!("{text:?} is not an action: {e}"))
}

/// What a key means, before digit entry and colour mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Meaning {
    Action(Action),
    Digit(u8),
    Enter,
    Colour(Colour),
}

/// The fixed meaning of a key code, if it has one.
pub fn meaning(key: u8) -> Option<Meaning> {
    use code::*;
    let action = |a| Some(Meaning::Action(a));
    let navigate = |direction| action(Action::Navigate { direction });
    match key {
        SELECT => action(Action::Select),
        UP => navigate(Direction::Up),
        DOWN => navigate(Direction::Down),
        LEFT => navigate(Direction::Left),
        RIGHT => navigate(Direction::Right),
        EXIT | AN_RETURN => action(Action::Back),
        NUMBER_0..=NUMBER_9 => Some(Meaning::Digit(key - NUMBER_0)),
        ENTER => Some(Meaning::Enter),
        CHANNEL_UP => action(Action::ChannelUp),
        CHANNEL_DOWN => action(Action::ChannelDown),
        DISPLAY_INFORMATION => action(Action::ShowInfo),
        ELECTRONIC_PROGRAM_GUIDE | ROOT_MENU | CONTENTS_MENU | AN_CHANNELS_LIST => {
            action(Action::ToggleGuide)
        }
        PLAY | PLAY_FUNCTION => action(Action::Play),
        PAUSE | STOP | STOP_FUNCTION => action(Action::Pause),
        PAUSE_PLAY_FUNCTION => action(Action::TogglePause),
        REWIND => action(Action::SeekRelative {
            seconds: -SEEK_SECONDS,
        }),
        FAST_FORWARD => action(Action::SeekRelative {
            seconds: SEEK_SECONDS,
        }),
        FORWARD => action(Action::NextItem),
        BACKWARD => action(Action::PreviousItem),
        F2_RED => Some(Meaning::Colour(Colour::Red)),
        F3_GREEN => Some(Meaning::Colour(Colour::Green)),
        F4_YELLOW => Some(Meaning::Colour(Colour::Yellow)),
        F1_BLUE => Some(Meaning::Colour(Colour::Blue)),
        _ => None,
    }
}

/// Whether holding the key should repeat it: only seeking does.
pub fn repeats(key: u8) -> bool {
    use code::*;
    matches!(key, LEFT | RIGHT | REWIND | FAST_FORWARD)
}

/// Turns presses and releases from the TV into actions: the key map,
/// channel number entry, colour buttons, and held keys.
///
/// libcec reports a held key as repeated presses with no release in
/// between; a press of the key already held is a repeat.
#[derive(Debug, Default)]
pub struct Remote {
    colours: ColourButtons,
    digits: ChannelNumberEntry,
    held: Option<u8>,
}

impl Remote {
    pub fn new(colours: ColourButtons) -> Self {
        Self {
            colours,
            ..Self::default()
        }
    }

    /// A key went down (or repeated) at `now`.
    pub fn press(&mut self, key: u8, now: Instant) -> Option<Action> {
        let repeat = self.held == Some(key);
        self.held = Some(key);
        if repeat && !repeats(key) {
            return None;
        }
        match meaning(key) {
            Some(Meaning::Digit(d)) => {
                self.digits.digit(d, now);
                None
            }
            Some(Meaning::Enter) if !self.digits.is_empty() => self.digits.commit(),
            Some(Meaning::Enter) => Some(Action::Select),
            Some(Meaning::Action(action)) => {
                self.digits.clear();
                Some(action)
            }
            Some(Meaning::Colour(colour)) => {
                self.digits.clear();
                self.colours.get(colour).cloned()
            }
            None => {
                tracing::debug!(key = format!("{key:#04x}"), "unmapped remote key");
                None
            }
        }
    }

    /// A key came up.
    pub fn release(&mut self, key: u8) {
        if self.held == Some(key) {
            self.held = None;
        }
    }

    /// Forgets held keys (e.g. after the adapter reconnects).
    pub fn reset(&mut self) {
        self.held = None;
        self.digits.clear();
    }

    /// When typed digits will be committed, if any are pending.
    pub fn deadline(&self) -> Option<Instant> {
        self.digits.deadline()
    }

    /// Commits typed digits if their timeout has passed.
    pub fn tick(&mut self, now: Instant) -> Option<Action> {
        self.digits.tick(now)
    }
}

#[cfg(test)]
mod tests {
    use marathon_client::digits::DIGIT_TIMEOUT;

    use super::code::*;
    use super::*;

    fn press_release(remote: &mut Remote, keys: &[u8]) -> Vec<Action> {
        let now = Instant::now();
        keys.iter()
            .filter_map(|k| {
                let action = remote.press(*k, now);
                remote.release(*k);
                action
            })
            .collect()
    }

    fn tap(keys: &[u8]) -> Vec<Action> {
        press_release(&mut Remote::default(), keys)
    }

    fn nav(direction: Direction) -> Action {
        Action::Navigate { direction }
    }

    /// Every key binding from the module docs (PLAN.md M7).
    #[test]
    fn every_binding_maps_to_its_action() {
        let cases = [
            (UP, nav(Direction::Up)),
            (DOWN, nav(Direction::Down)),
            (LEFT, nav(Direction::Left)),
            (RIGHT, nav(Direction::Right)),
            (SELECT, Action::Select),
            (EXIT, Action::Back),
            (AN_RETURN, Action::Back),
            (CHANNEL_UP, Action::ChannelUp),
            (CHANNEL_DOWN, Action::ChannelDown),
            (PLAY, Action::Play),
            (PLAY_FUNCTION, Action::Play),
            (PAUSE, Action::Pause),
            (STOP, Action::Pause),
            (STOP_FUNCTION, Action::Pause),
            (PAUSE_PLAY_FUNCTION, Action::TogglePause),
            (REWIND, Action::SeekRelative { seconds: -10 }),
            (FAST_FORWARD, Action::SeekRelative { seconds: 10 }),
            (FORWARD, Action::NextItem),
            (BACKWARD, Action::PreviousItem),
            (DISPLAY_INFORMATION, Action::ShowInfo),
            (ELECTRONIC_PROGRAM_GUIDE, Action::ToggleGuide),
            (ROOT_MENU, Action::ToggleGuide),
            (CONTENTS_MENU, Action::ToggleGuide),
            (AN_CHANNELS_LIST, Action::ToggleGuide),
            // Enter with no digits typed selects, like OK.
            (ENTER, Action::Select),
        ];
        for (key, action) in cases {
            assert_eq!(tap(&[key]), vec![action], "{key:#04x}");
        }
    }

    #[test]
    fn unmapped_keys_and_unconfigured_colours_do_nothing() {
        for key in [
            SETUP_MENU,
            PREVIOUS_CHANNEL,
            F1_BLUE,
            F2_RED,
            F3_GREEN,
            F4_YELLOW,
            0x7F,
            0xFF,
        ] {
            assert_eq!(tap(&[key]), vec![], "{key:#04x}");
        }
    }

    #[test]
    fn colour_buttons_do_what_they_are_configured_to() {
        let mut colours = ColourButtons::default();
        colours.set(Colour::Red, Some(Action::ToggleSubtitles));
        colours.set(Colour::Blue, Some(Action::GoToChannel { number: 7 }));
        let mut remote = Remote::new(colours);
        assert_eq!(
            press_release(&mut remote, &[F2_RED, F3_GREEN, F4_YELLOW, F1_BLUE]),
            vec![Action::ToggleSubtitles, Action::GoToChannel { number: 7 }]
        );
    }

    #[test]
    fn digits_enter_a_channel_number() {
        for d in 0..=9u8 {
            assert_eq!(
                tap(&[NUMBER_0 + d, ENTER]),
                vec![Action::GoToChannel {
                    number: u32::from(d)
                }]
            );
        }
        assert_eq!(
            tap(&[NUMBER_0 + 1, NUMBER_0 + 2, ENTER]),
            vec![Action::GoToChannel { number: 12 }]
        );
        // Any other key discards them.
        assert_eq!(
            tap(&[NUMBER_0 + 4, CHANNEL_UP, ENTER]),
            vec![Action::ChannelUp, Action::Select]
        );
    }

    #[test]
    fn digits_commit_after_a_pause() {
        let mut remote = Remote::default();
        let t0 = Instant::now();
        assert_eq!(remote.press(NUMBER_0 + 3, t0), None);
        remote.release(NUMBER_0 + 3);
        let deadline = remote.deadline().unwrap();
        assert_eq!(deadline, t0 + DIGIT_TIMEOUT);
        assert_eq!(
            remote.tick(deadline),
            Some(Action::GoToChannel { number: 3 })
        );
        assert_eq!(remote.deadline(), None);
    }

    #[test]
    fn only_seeking_repeats_while_held() {
        let now = Instant::now();
        let mut remote = Remote::default();
        // Held Right: every repeat seeks.
        let seeks: Vec<_> = (0..3).filter_map(|_| remote.press(RIGHT, now)).collect();
        assert_eq!(seeks, vec![nav(Direction::Right); 3]);
        remote.release(RIGHT);
        // Held channel up: once.
        let flips: Vec<_> = (0..3)
            .filter_map(|_| remote.press(CHANNEL_UP, now))
            .collect();
        assert_eq!(flips, vec![Action::ChannelUp]);
        remote.release(CHANNEL_UP);
        // Released and pressed again: a new press.
        assert_eq!(remote.press(CHANNEL_UP, now), Some(Action::ChannelUp));
        // A different key while one is held is a new press too.
        assert_eq!(remote.press(CHANNEL_DOWN, now), Some(Action::ChannelDown));
        // Releasing a key that is not held changes nothing.
        remote.release(CHANNEL_UP);
        assert_eq!(remote.press(CHANNEL_DOWN, now), None);
        // A reset forgets the held key.
        remote.reset();
        assert_eq!(remote.press(CHANNEL_DOWN, now), Some(Action::ChannelDown));
    }

    #[test]
    fn actions_parse_from_names_or_json() {
        assert_eq!(parse_action("toggle_guide"), Ok(Action::ToggleGuide));
        assert_eq!(parse_action(" show_info "), Ok(Action::ShowInfo));
        assert_eq!(
            parse_action(r#"{"action":"go_to_channel","number":4}"#),
            Ok(Action::GoToChannel { number: 4 })
        );
        assert!(parse_action("go_to_channel").is_err());
        assert!(parse_action("self_destruct").is_err());
        assert!(parse_action("{not json").is_err());
    }
}
