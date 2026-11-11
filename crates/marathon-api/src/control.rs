//! The control plane (PLAN.md §9 "Input and control"): [`Action`]s that
//! every input adapter emits, and [`Event`]s that observers subscribe to.
//!
//! Both live here, not in the client, because the server relays them
//! (remote adapters, `GET /events`). Both serialize as internally tagged
//! JSON with snake_case names:
//!
//! ```
//! # use marathon_api::{Action, Direction};
//! let json = serde_json::to_string(&Action::GoToChannel { number: 4 }).unwrap();
//! assert_eq!(json, r#"{"action":"go_to_channel","number":4}"#);
//! let nav: Action = serde_json::from_str(r#"{"action":"navigate","direction":"up"}"#).unwrap();
//! assert_eq!(nav, Action::Navigate { direction: Direction::Up });
//! ```

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A direction for [`Action::Navigate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// Something a viewer (or a program acting for one) asked for. Actions are
/// semantic, not device-shaped: a TV remote's D-pad, a gamepad, and arrow
/// keys all produce [`Action::Navigate`], and the client decides what it
/// means in context.
///
/// Clients log and ignore actions they do not support yet.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// Tune to the next channel up (higher number, wrapping).
    ChannelUp,
    /// Tune to the next channel down (lower number, wrapping).
    ChannelDown,
    /// Tune to the channel with this number.
    GoToChannel {
        number: u32,
    },
    /// Tune to the channel with this ID.
    GoToChannelId {
        id: String,
    },
    TogglePause,
    /// Resume playback (no-op if playing).
    Play,
    /// Pause playback (no-op if paused).
    Pause,
    /// Seek by this many seconds; negative is backwards.
    SeekRelative {
        seconds: i32,
    },
    /// Skip to the channel's next item.
    NextItem,
    /// Step back to the channel's previous item.
    PreviousItem,
    /// Move in a direction. With nothing open, Up/Down change channel and
    /// Left/Right seek; with the guide open, they move its selection.
    Navigate {
        direction: Direction,
    },
    /// Choose the highlighted thing (a channel in the guide), or show info
    /// when nothing is open.
    Select,
    /// Close whatever is open.
    Back,
    /// Show what is playing.
    ShowInfo,
    /// Open or close the channel guide.
    ToggleGuide,
    /// Cycle subtitle tracks (on/off with one track).
    ToggleSubtitles,
    /// Switch between fullscreen and windowed.
    ToggleFullscreen,
    /// Go quiet: stop playback and blank the screen, ready to wake.
    Standby,
    /// Come back from standby.
    Wake,
    /// Save the position and exit.
    Quit,
}

/// Something that happened on a client, for observers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// An item started playing (after a tune, an advance, or a skip).
    NowPlaying {
        channel_id: String,
        channel_number: u32,
        item_id: String,
        /// Human-readable description, e.g. `Show · S03E14 · The Injury`.
        title: String,
        /// Where playback started within the item.
        position_ms: u64,
    },
    /// The client tuned to a different channel.
    ChannelChanged {
        channel_id: String,
        channel_number: u32,
        channel_name: String,
    },
    Paused {
        channel_id: String,
        item_id: String,
        position_ms: u64,
    },
    Resumed {
        channel_id: String,
        item_id: String,
        position_ms: u64,
    },
    /// An item played to its end.
    ItemEnded {
        channel_id: String,
        item_id: String,
    },
    Standby,
    Wake,
    /// Something went wrong; playback carries on where it can.
    Error {
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    /// One of each action. The `match` makes this fail to compile when a
    /// variant is added without being listed.
    fn every_action() -> Vec<Action> {
        let all = vec![
            Action::ChannelUp,
            Action::ChannelDown,
            Action::GoToChannel { number: 4 },
            Action::GoToChannelId {
                id: "office".into(),
            },
            Action::TogglePause,
            Action::Play,
            Action::Pause,
            Action::SeekRelative { seconds: -10 },
            Action::NextItem,
            Action::PreviousItem,
            Action::Navigate {
                direction: Direction::Up,
            },
            Action::Navigate {
                direction: Direction::Down,
            },
            Action::Navigate {
                direction: Direction::Left,
            },
            Action::Navigate {
                direction: Direction::Right,
            },
            Action::Select,
            Action::Back,
            Action::ShowInfo,
            Action::ToggleGuide,
            Action::ToggleSubtitles,
            Action::ToggleFullscreen,
            Action::Standby,
            Action::Wake,
            Action::Quit,
        ];
        for a in &all {
            match a {
                Action::ChannelUp
                | Action::ChannelDown
                | Action::GoToChannel { .. }
                | Action::GoToChannelId { .. }
                | Action::TogglePause
                | Action::Play
                | Action::Pause
                | Action::SeekRelative { .. }
                | Action::NextItem
                | Action::PreviousItem
                | Action::Navigate { .. }
                | Action::Select
                | Action::Back
                | Action::ShowInfo
                | Action::ToggleGuide
                | Action::ToggleSubtitles
                | Action::ToggleFullscreen
                | Action::Standby
                | Action::Wake
                | Action::Quit => {}
            }
        }
        all
    }

    fn every_event() -> Vec<Event> {
        let all = vec![
            Event::NowPlaying {
                channel_id: "c".into(),
                channel_number: 1,
                item_id: "i".into(),
                title: "T".into(),
                position_ms: 5,
            },
            Event::ChannelChanged {
                channel_id: "c".into(),
                channel_number: 1,
                channel_name: "C".into(),
            },
            Event::Paused {
                channel_id: "c".into(),
                item_id: "i".into(),
                position_ms: 5,
            },
            Event::Resumed {
                channel_id: "c".into(),
                item_id: "i".into(),
                position_ms: 5,
            },
            Event::ItemEnded {
                channel_id: "c".into(),
                item_id: "i".into(),
            },
            Event::Standby,
            Event::Wake,
            Event::Error {
                message: "m".into(),
            },
        ];
        for e in &all {
            match e {
                Event::NowPlaying { .. }
                | Event::ChannelChanged { .. }
                | Event::Paused { .. }
                | Event::Resumed { .. }
                | Event::ItemEnded { .. }
                | Event::Standby
                | Event::Wake
                | Event::Error { .. } => {}
            }
        }
        all
    }

    fn is_snake_case(s: &str) -> bool {
        !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c == '_')
    }

    #[test]
    fn every_action_round_trips_with_a_snake_case_tag() {
        for action in every_action() {
            let value = serde_json::to_value(&action).unwrap();
            let tag = value["action"].as_str().unwrap();
            assert!(is_snake_case(tag), "{tag}");
            assert_eq!(serde_json::from_value::<Action>(value).unwrap(), action);
        }
    }

    #[test]
    fn every_event_round_trips_with_a_snake_case_tag() {
        for event in every_event() {
            let value = serde_json::to_value(&event).unwrap();
            let tag = value["event"].as_str().unwrap();
            assert!(is_snake_case(tag), "{tag}");
            assert_eq!(serde_json::from_value::<Event>(value).unwrap(), event);
        }
    }

    #[test]
    fn action_json_shapes() {
        let shape = |a: Action| serde_json::to_value(a).unwrap();
        assert_eq!(shape(Action::ChannelUp), json!({"action": "channel_up"}));
        assert_eq!(
            shape(Action::GoToChannel { number: 4 }),
            json!({"action": "go_to_channel", "number": 4})
        );
        assert_eq!(
            shape(Action::Navigate {
                direction: Direction::Left
            }),
            json!({"action": "navigate", "direction": "left"})
        );
        assert_eq!(
            shape(Action::SeekRelative { seconds: -10 }),
            json!({"action": "seek_relative", "seconds": -10})
        );
    }

    #[test]
    fn event_json_shape() {
        let value: Value = serde_json::to_value(Event::Error {
            message: "boom".into(),
        })
        .unwrap();
        assert_eq!(value, json!({"event": "error", "message": "boom"}));
    }

    #[test]
    fn unknown_actions_are_rejected_not_misread() {
        assert!(serde_json::from_str::<Action>(r#"{"action": "self_destruct"}"#).is_err());
        assert!(serde_json::from_str::<Action>(r#"{"action": "go_to_channel"}"#).is_err());
    }
}
