//! Translates winit key events into [`marathon_client::keyboard::Key`]s.
//! The bindings themselves live in `marathon_client::keyboard`.

use marathon_client::keyboard::Key;
use winit::keyboard::{Key as WinitKey, NamedKey};

/// The key a winit logical key stands for, if Marathon binds it.
pub fn translate(key: &WinitKey) -> Option<Key> {
    Some(match key {
        WinitKey::Named(named) => match named {
            NamedKey::PageUp => Key::PageUp,
            NamedKey::PageDown => Key::PageDown,
            NamedKey::ArrowUp => Key::ArrowUp,
            NamedKey::ArrowDown => Key::ArrowDown,
            NamedKey::ArrowLeft => Key::ArrowLeft,
            NamedKey::ArrowRight => Key::ArrowRight,
            NamedKey::Space => Key::Space,
            NamedKey::Enter => Key::Enter,
            NamedKey::Escape => Key::Escape,
            _ => return None,
        },
        WinitKey::Character(text) => {
            let mut chars = text.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Key::Character(c),
                _ => return None,
            }
        }
        _ => return None,
    })
}

/// Whether holding the key down should repeat its action. Seeking does;
/// flipping channels or toggling things on auto-repeat would be chaos.
pub fn repeats(key: Key) -> bool {
    matches!(key, Key::ArrowLeft | Key::ArrowRight)
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use marathon_client::Action;
    use marathon_client::keyboard::Keyboard;

    use super::*;

    fn action(key: WinitKey) -> Option<Action> {
        Keyboard::new().press(translate(&key)?, Instant::now())
    }

    fn ch(s: &str) -> WinitKey {
        WinitKey::Character(s.into())
    }

    /// Every keyboard binding in PLAN.md §9, from the winit key event to
    /// the Action.
    #[test]
    fn every_binding_from_winit_to_action() {
        let cases = [
            (WinitKey::Named(NamedKey::PageUp), Action::ChannelUp),
            (WinitKey::Named(NamedKey::ArrowUp), Action::ChannelUp),
            (WinitKey::Named(NamedKey::PageDown), Action::ChannelDown),
            (WinitKey::Named(NamedKey::ArrowDown), Action::ChannelDown),
            (WinitKey::Named(NamedKey::Space), Action::TogglePause),
            (
                WinitKey::Named(NamedKey::ArrowLeft),
                Action::SeekRelative(-10),
            ),
            (
                WinitKey::Named(NamedKey::ArrowRight),
                Action::SeekRelative(10),
            ),
            (ch("n"), Action::NextItem),
            (ch("N"), Action::NextItem),
            (ch("p"), Action::PreviousItem),
            (ch("i"), Action::ShowInfo),
            (ch("g"), Action::ToggleGuide),
            (WinitKey::Named(NamedKey::Escape), Action::Back),
            (ch("q"), Action::Quit),
            (ch("s"), Action::ToggleSubtitles),
            (ch("f"), Action::ToggleFullscreen),
        ];
        for (key, expected) in cases {
            assert_eq!(action(key.clone()), Some(expected), "{key:?}");
        }
    }

    #[test]
    fn digits_then_enter_go_to_a_channel() {
        let mut kb = Keyboard::new();
        let now = Instant::now();
        for key in [ch("1"), ch("2")] {
            assert_eq!(kb.press(translate(&key).unwrap(), now), None);
        }
        let enter = translate(&WinitKey::Named(NamedKey::Enter)).unwrap();
        assert_eq!(kb.press(enter, now), Some(Action::GoToChannel(12)));
    }

    #[test]
    fn unbound_keys_translate_to_nothing() {
        assert_eq!(translate(&WinitKey::Named(NamedKey::F1)), None);
        assert_eq!(translate(&ch("ab")), None);
        assert_eq!(action(ch("x")), None);
    }

    #[test]
    fn only_seeking_repeats() {
        assert!(repeats(Key::ArrowLeft));
        assert!(repeats(Key::ArrowRight));
        assert!(!repeats(Key::ArrowUp));
        assert!(!repeats(Key::Character('n')));
    }
}
