//! Keyboard input (PLAN.md §9 bindings), as an
//! [`InputSource`](crate::input::InputSource). Behind the `input-keyboard`
//! feature.
//!
//! The windowing layer translates its key events into [`Key`] and pushes
//! them into a [`KeyFeed`]; the [`KeyboardSource`] thread turns them into
//! actions on the bus. Keeping the bindings here, away from any windowing
//! library, keeps them testable and reusable by other backends.
//!
//! | Key | Action |
//! |---|---|
//! | arrows | [`Action::Navigate`] (channel up/down and seek ∓10s with nothing open) |
//! | PgUp / PgDn | [`Action::ChannelUp`] / [`Action::ChannelDown`] |
//! | digits | [`Action::GoToChannel`], after a pause or Enter |
//! | Enter | [`Action::Select`] (or commits typed digits) |
//! | Space | [`Action::TogglePause`] |
//! | N / P | [`Action::NextItem`] / [`Action::PreviousItem`] |
//! | I | [`Action::ShowInfo`] |
//! | G | [`Action::ToggleGuide`] |
//! | S | [`Action::ToggleSubtitles`] |
//! | F | [`Action::ToggleFullscreen`] |
//! | Esc | [`Action::Back`] |
//! | Q | [`Action::Quit`] |

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::Instant;

use marathon_api::{Action, Direction};

use crate::bus::ActionSender;
use crate::digits::ChannelNumberEntry;
pub use crate::digits::DIGIT_TIMEOUT;
use crate::input::{InputError, InputSource};

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
    let navigate = |direction| Action::Navigate { direction };
    Some(match key {
        Key::PageUp => Action::ChannelUp,
        Key::PageDown => Action::ChannelDown,
        Key::ArrowUp => navigate(Direction::Up),
        Key::ArrowDown => navigate(Direction::Down),
        Key::ArrowLeft => navigate(Direction::Left),
        Key::ArrowRight => navigate(Direction::Right),
        Key::Space => Action::TogglePause,
        Key::Escape => Action::Back,
        Key::Enter => Action::Select,
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

/// Whether holding the key down should repeat its action. Seeking does;
/// flipping channels or toggling things on auto-repeat would be chaos.
pub fn repeats(key: Key) -> bool {
    matches!(key, Key::ArrowLeft | Key::ArrowRight)
}

/// Turns key presses into actions, collecting typed digits into a channel
/// number.
///
/// Digits are buffered; the number is committed as
/// [`Action::GoToChannel`] when Enter is pressed or [`DIGIT_TIMEOUT`]
/// passes without another digit (call [`Keyboard::tick`] by
/// [`Keyboard::deadline`]). Any other key discards pending digits. (See
/// [`crate::digits`].)
#[derive(Debug, Default)]
pub struct Keyboard {
    digits: ChannelNumberEntry,
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
            // A decimal digit is 0..=9, so it fits in a u8.
            self.digits.digit(d as u8, now);
            return None;
        }
        if key == Key::Enter && !self.digits.is_empty() {
            return self.digits.commit();
        }
        self.digits.clear();
        map_key(key)
    }

    /// When pending digits will be committed, if any are pending.
    pub fn deadline(&self) -> Option<Instant> {
        self.digits.deadline()
    }

    /// Commits pending digits if their timeout has passed.
    pub fn tick(&mut self, now: Instant) -> Option<Action> {
        self.digits.tick(now)
    }

    /// The digits typed so far, for display.
    pub fn pending_digits(&self) -> &str {
        self.digits.pending()
    }
}

enum KeyMsg {
    Press { key: Key, repeat: bool },
    Stop,
}

/// Where the windowing layer pushes key presses. Clone freely; presses
/// sent while the keyboard source is not running are dropped.
#[derive(Clone)]
pub struct KeyFeed(Sender<KeyMsg>);

impl KeyFeed {
    /// A key went down. `repeat` is true for auto-repeat while held.
    pub fn press(&self, key: Key, repeat: bool) {
        let _ = self.0.send(KeyMsg::Press { key, repeat });
    }
}

/// The keyboard [`InputSource`]. Runs a thread that applies the bindings
/// and the digit-entry timeout.
pub struct KeyboardSource {
    feed: Sender<KeyMsg>,
    inbox: Option<Receiver<KeyMsg>>,
    thread: Option<JoinHandle<()>>,
}

impl KeyboardSource {
    /// The source, and the feed the windowing layer pushes keys into.
    pub fn new() -> (Self, KeyFeed) {
        let (tx, rx) = mpsc::channel();
        let source = Self {
            feed: tx.clone(),
            inbox: Some(rx),
            thread: None,
        };
        (source, KeyFeed(tx))
    }
}

fn run(inbox: Receiver<KeyMsg>, actions: ActionSender) {
    let mut keyboard = Keyboard::new();
    loop {
        let msg = match keyboard.deadline() {
            Some(deadline) => {
                inbox.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            }
            None => inbox.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        let action = match msg {
            Ok(KeyMsg::Press { key, repeat }) => {
                if repeat && !repeats(key) {
                    continue;
                }
                let action = keyboard.press(key, Instant::now());
                if action.is_none() && !keyboard.pending_digits().is_empty() {
                    tracing::info!(digits = keyboard.pending_digits(), "channel number");
                }
                action
            }
            Err(RecvTimeoutError::Timeout) => keyboard.tick(Instant::now()),
            Ok(KeyMsg::Stop) | Err(RecvTimeoutError::Disconnected) => return,
        };
        if let Some(action) = action
            && actions.send(action).is_err()
        {
            return;
        }
    }
}

impl InputSource for KeyboardSource {
    fn name(&self) -> &str {
        "keyboard"
    }

    fn start(&mut self, actions: ActionSender) -> Result<(), InputError> {
        let error = |message: String| InputError {
            source_name: "keyboard".into(),
            message,
        };
        let inbox = self
            .inbox
            .take()
            .ok_or_else(|| error("already started".into()))?;
        let thread = std::thread::Builder::new()
            .name("input-keyboard".into())
            .spawn(move || run(inbox, actions))
            .map_err(|e| error(e.to_string()))?;
        self.thread = Some(thread);
        Ok(())
    }

    fn stop(&mut self) {
        let _ = self.feed.send(KeyMsg::Stop);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("keyboard input thread panicked");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::bus::ActionBus;

    fn press(keys: &[Key]) -> Vec<Action> {
        let mut kb = Keyboard::new();
        let now = Instant::now();
        keys.iter().filter_map(|k| kb.press(*k, now)).collect()
    }

    fn nav(direction: Direction) -> Action {
        Action::Navigate { direction }
    }

    /// Every single-key binding in PLAN.md §9, plus S and F.
    #[test]
    fn every_binding_maps_to_its_action() {
        let cases = [
            (Key::ArrowUp, nav(Direction::Up)),
            (Key::ArrowDown, nav(Direction::Down)),
            (Key::ArrowLeft, nav(Direction::Left)),
            (Key::ArrowRight, nav(Direction::Right)),
            (Key::PageUp, Action::ChannelUp),
            (Key::PageDown, Action::ChannelDown),
            (Key::Enter, Action::Select),
            (Key::Space, Action::TogglePause),
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
        for key in [Key::Character('x'), Key::Character('é')] {
            assert_eq!(press(&[key]), vec![], "{key:?}");
        }
    }

    #[test]
    fn every_digit_enters_a_channel_number() {
        for d in 0..=9u32 {
            let c = char::from_digit(d, 10).unwrap();
            assert_eq!(
                press(&[Key::Character(c), Key::Enter]),
                vec![Action::GoToChannel { number: d }]
            );
        }
    }

    #[test]
    fn digits_accumulate_and_commit_on_enter() {
        let keys = [Key::Character('1'), Key::Character('2'), Key::Enter];
        assert_eq!(press(&keys), vec![Action::GoToChannel { number: 12 }]);
        // With no digits pending, Enter selects.
        assert_eq!(press(&[Key::Enter]), vec![Action::Select]);
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
        assert_eq!(kb.tick(deadline), Some(Action::GoToChannel { number: 42 }));
        assert_eq!(kb.deadline(), None);
        assert_eq!(kb.tick(deadline + DIGIT_TIMEOUT), None);
    }

    #[test]
    fn other_keys_discard_pending_digits() {
        let keys = [Key::Character('7'), Key::Space, Key::Enter];
        assert_eq!(press(&keys), vec![Action::TogglePause, Action::Select]);
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
        assert_eq!(press(&keys), vec![Action::GoToChannel { number: 1234 }]);
    }

    #[test]
    fn only_seeking_repeats() {
        assert!(repeats(Key::ArrowLeft));
        assert!(repeats(Key::ArrowRight));
        assert!(!repeats(Key::ArrowUp));
        assert!(!repeats(Key::PageUp));
        assert!(!repeats(Key::Character('n')));
    }

    #[test]
    fn source_delivers_to_the_bus_and_commits_digits_on_its_own() {
        let (bus, rx) = ActionBus::channel();
        let (mut source, feed) = KeyboardSource::new();
        source.start(bus.sender("keyboard")).unwrap();
        let recv = || rx.recv_timeout(Duration::from_secs(5)).unwrap();

        feed.press(Key::PageUp, false);
        let got = recv();
        assert_eq!((&*got.source, got.action), ("keyboard", Action::ChannelUp));

        // Auto-repeat only passes for seek keys.
        feed.press(Key::PageUp, true);
        feed.press(Key::ArrowRight, true);
        assert_eq!(recv().action, nav(Direction::Right));

        // A typed number commits after the timeout without further input.
        feed.press(Key::Character('7'), false);
        assert_eq!(
            rx.recv_timeout(DIGIT_TIMEOUT * 3).unwrap().action,
            Action::GoToChannel { number: 7 }
        );

        source.stop();
        feed.press(Key::PageUp, false); // dropped, not an error
        assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());
    }
}
