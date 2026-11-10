//! Keyboard bindings (PLAN.md §9).
//!
//! The windowing layer translates its key events into [`Key`] and feeds
//! them to a [`Keyboard`], which returns [`Action`]s. Keeping the mapping
//! here, away from any windowing library, lets it be tested directly.
//!
//! | Key | Action |
//! |---|---|
//! | PgUp, Up | [`Action::ChannelUp`] |
//! | PgDn, Down | [`Action::ChannelDown`] |
//! | digits | [`Action::GoToChannel`], after a pause or Enter |
//! | Space | [`Action::TogglePause`] |
//! | Left / Right | [`Action::SeekRelative`] ∓10s |
//! | N / P | [`Action::NextItem`] / [`Action::PreviousItem`] |
//! | I | [`Action::ShowInfo`] |
//! | G | [`Action::ToggleGuide`] |
//! | S | [`Action::ToggleSubtitles`] |
//! | F | [`Action::ToggleFullscreen`] |
//! | Esc | [`Action::Back`] |
//! | Q | [`Action::Quit`] |

use std::time::{Duration, Instant};

use crate::Action;

/// Seconds that Left and Right seek by.
pub const SEEK_STEP_SECS: i32 = 10;

/// How long after the last digit a channel number is committed.
pub const DIGIT_TIMEOUT: Duration = Duration::from_millis(1500);

/// Longest channel number that can be typed.
const MAX_DIGITS: usize = 4;

/// A key press, independent of windowing library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    PageUp,
    PageDown,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Space,
    Enter,
    Escape,
    /// A printable character as typed (any case).
    Character(char),
}

/// Maps a key with a fixed meaning to its action. Digits and Enter are
/// handled by [`Keyboard`], since a channel number spans several presses.
pub fn map_key(key: Key) -> Option<Action> {
    Some(match key {
        Key::PageUp | Key::ArrowUp => Action::ChannelUp,
        Key::PageDown | Key::ArrowDown => Action::ChannelDown,
        Key::ArrowLeft => Action::SeekRelative(-SEEK_STEP_SECS),
        Key::ArrowRight => Action::SeekRelative(SEEK_STEP_SECS),
        Key::Space => Action::TogglePause,
        Key::Escape => Action::Back,
        Key::Enter => return None,
        Key::Character(c) => match c.to_ascii_lowercase() {
            'n' => Action::NextItem,
            'p' => Action::PreviousItem,
            'i' => Action::ShowInfo,
            'g' => Action::ToggleGuide,
            's' => Action::ToggleSubtitles,
            'f' => Action::ToggleFullscreen,
            'q' => Action::Quit,
            _ => return None,
        },
    })
}

/// Turns key presses into actions, collecting typed digits into a channel
/// number.
///
/// Digits are buffered; the number is committed as
/// [`Action::GoToChannel`] when Enter is pressed or [`DIGIT_TIMEOUT`]
/// passes without another digit (call [`Keyboard::tick`] by
/// [`Keyboard::deadline`]). Any other key discards pending digits.
#[derive(Debug, Default)]
pub struct Keyboard {
    digits: String,
    last_digit: Option<Instant>,
}

impl Keyboard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Handles one key press at time `now`.
    pub fn press(&mut self, key: Key, now: Instant) -> Option<Action> {
        if let Key::Character(c) = key
            && let Some(d) = c.to_digit(10)
        {
            if self.digits.len() < MAX_DIGITS {
                self.digits.push(char::from(b'0' + d as u8));
            }
            self.last_digit = Some(now);
            return None;
        }
        if key == Key::Enter {
            return self.commit();
        }
        self.clear();
        map_key(key)
    }

    /// When pending digits will be committed, if any are pending.
    pub fn deadline(&self) -> Option<Instant> {
        self.last_digit.map(|t| t + DIGIT_TIMEOUT)
    }

    /// Commits pending digits if their timeout has passed.
    pub fn tick(&mut self, now: Instant) -> Option<Action> {
        match self.deadline() {
            Some(deadline) if now >= deadline => self.commit(),
            _ => None,
        }
    }

    /// The digits typed so far, for display.
    pub fn pending_digits(&self) -> &str {
        &self.digits
    }

    fn commit(&mut self) -> Option<Action> {
        let number = self.digits.parse().ok();
        self.clear();
        number.map(Action::GoToChannel)
    }

    fn clear(&mut self) {
        self.digits.clear();
        self.last_digit = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(keys: &[Key]) -> Vec<Action> {
        let mut kb = Keyboard::new();
        let now = Instant::now();
        keys.iter().filter_map(|k| kb.press(*k, now)).collect()
    }

    /// Every single-key binding in PLAN.md §9, plus the additions
    /// documented on [`Action`].
    #[test]
    fn every_binding_maps_to_its_action() {
        let cases = [
            (Key::PageUp, Action::ChannelUp),
            (Key::ArrowUp, Action::ChannelUp),
            (Key::PageDown, Action::ChannelDown),
            (Key::ArrowDown, Action::ChannelDown),
            (Key::Space, Action::TogglePause),
            (Key::ArrowLeft, Action::SeekRelative(-10)),
            (Key::ArrowRight, Action::SeekRelative(10)),
            (Key::Character('n'), Action::NextItem),
            (Key::Character('N'), Action::NextItem),
            (Key::Character('p'), Action::PreviousItem),
            (Key::Character('P'), Action::PreviousItem),
            (Key::Character('i'), Action::ShowInfo),
            (Key::Character('I'), Action::ShowInfo),
            (Key::Character('g'), Action::ToggleGuide),
            (Key::Character('G'), Action::ToggleGuide),
            (Key::Escape, Action::Back),
            (Key::Character('q'), Action::Quit),
            (Key::Character('Q'), Action::Quit),
            (Key::Character('s'), Action::ToggleSubtitles),
            (Key::Character('f'), Action::ToggleFullscreen),
        ];
        for (key, action) in cases {
            assert_eq!(press(&[key]), vec![action], "{key:?}");
        }
    }

    #[test]
    fn unbound_keys_do_nothing() {
        for key in [Key::Character('x'), Key::Character('é'), Key::Enter] {
            assert_eq!(press(&[key]), vec![], "{key:?}");
        }
    }

    #[test]
    fn every_digit_enters_a_channel_number() {
        for d in 0..=9u32 {
            let c = char::from_digit(d, 10).unwrap();
            assert_eq!(
                press(&[Key::Character(c), Key::Enter]),
                vec![Action::GoToChannel(d)]
            );
        }
    }

    #[test]
    fn digits_accumulate_and_commit_on_enter() {
        let keys = [Key::Character('1'), Key::Character('2'), Key::Enter];
        assert_eq!(press(&keys), vec![Action::GoToChannel(12)]);
    }

    #[test]
    fn digits_commit_after_the_timeout() {
        let mut kb = Keyboard::new();
        let t0 = Instant::now();
        assert_eq!(kb.press(Key::Character('4'), t0), None);
        assert_eq!(kb.press(Key::Character('2'), t0 + DIGIT_TIMEOUT / 2), None);
        assert_eq!(kb.pending_digits(), "42");
        let deadline = kb.deadline().unwrap();
        assert_eq!(deadline, t0 + DIGIT_TIMEOUT / 2 + DIGIT_TIMEOUT);
        assert_eq!(kb.tick(deadline - Duration::from_millis(1)), None);
        assert_eq!(kb.tick(deadline), Some(Action::GoToChannel(42)));
        assert_eq!(kb.deadline(), None);
        assert_eq!(kb.tick(deadline + DIGIT_TIMEOUT), None);
    }

    #[test]
    fn other_keys_discard_pending_digits() {
        let keys = [Key::Character('7'), Key::Space, Key::Enter];
        assert_eq!(press(&keys), vec![Action::TogglePause]);
    }

    #[test]
    fn channel_numbers_are_capped_in_length() {
        let keys = [
            Key::Character('1'),
            Key::Character('2'),
            Key::Character('3'),
            Key::Character('4'),
            Key::Character('5'),
            Key::Enter,
        ];
        assert_eq!(press(&keys), vec![Action::GoToChannel(1234)]);
    }
}
