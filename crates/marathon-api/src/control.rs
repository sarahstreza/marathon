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
    /// Go quiet (e.g. the TV switched to another input): pause, save the
    /// position, and hide the overlay, ready to wake.
    Standby,
    /// Come back from standby, resuming playback if standby paused it.
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

impl Action {
    /// One value of every variant (and every [`Direction`]), for tests,
    /// documentation, and anything that must cover the whole set, such as
    /// checking that each action can be relayed. Exhaustive by
    /// construction: adding a variant without listing it here fails to
    /// compile.
    pub fn examples() -> Vec<Action> {
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
        // Not a runtime check: the exhaustive `match` is what makes a new
        // variant a compile error here until it is added to the list.
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

    /// The variant's name as it appears in JSON (`channel_up`).
    pub fn name(&self) -> &'static str {
        match self {
            Action::ChannelUp => "channel_up",
            Action::ChannelDown => "channel_down",
            Action::GoToChannel { .. } => "go_to_channel",
            Action::GoToChannelId { .. } => "go_to_channel_id",
            Action::TogglePause => "toggle_pause",
            Action::Play => "play",
            Action::Pause => "pause",
            Action::SeekRelative { .. } => "seek_relative",
            Action::NextItem => "next_item",
            Action::PreviousItem => "previous_item",
            Action::Navigate { .. } => "navigate",
            Action::Select => "select",
            Action::Back => "back",
            Action::ShowInfo => "show_info",
            Action::ToggleGuide => "toggle_guide",
            Action::ToggleSubtitles => "toggle_subtitles",
            Action::ToggleFullscreen => "toggle_fullscreen",
            Action::Standby => "standby",
            Action::Wake => "wake",
            Action::Quit => "quit",
        }
    }
}

impl Event {
    /// One value of every variant; see [`Action::examples`].
    pub fn examples() -> Vec<Event> {
        let all = vec![
            Event::NowPlaying {
                channel_id: "office".into(),
                channel_number: 1,
                item_id: "0190a1b2-0000-7000-8000-000000000001".into(),
                title: "The Office · S03E14 · The Injury".into(),
                position_ms: 5000,
            },
            Event::ChannelChanged {
                channel_id: "office".into(),
                channel_number: 1,
                channel_name: "The Office".into(),
            },
            Event::Paused {
                channel_id: "office".into(),
                item_id: "0190a1b2-0000-7000-8000-000000000001".into(),
                position_ms: 65_000,
            },
            Event::Resumed {
                channel_id: "office".into(),
                item_id: "0190a1b2-0000-7000-8000-000000000001".into(),
                position_ms: 65_000,
            },
            Event::ItemEnded {
                channel_id: "office".into(),
                item_id: "0190a1b2-0000-7000-8000-000000000001".into(),
            },
            Event::Standby,
            Event::Wake,
            Event::Error {
                message: "cannot play TV/a.mkv on channel 1: file not found".into(),
            },
        ];
        // As in `Action::examples`: a compile-time completeness check.
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

    /// The variant's name as it appears in JSON (`now_playing`), also used
    /// as the Server-Sent Events `event:` field by `GET /events`.
    pub fn name(&self) -> &'static str {
        match self {
            Event::NowPlaying { .. } => "now_playing",
            Event::ChannelChanged { .. } => "channel_changed",
            Event::Paused { .. } => "paused",
            Event::Resumed { .. } => "resumed",
            Event::ItemEnded { .. } => "item_ended",
            Event::Standby => "standby",
            Event::Wake => "wake",
            Event::Error { .. } => "error",
        }
    }
}

/// An [`Event`] as relayed by the server on `GET /events`: the event's own
/// fields, plus which client sent it and when the server received it.
///
/// ```
/// # use marathon_api::{Event, RelayedEvent};
/// let relayed = RelayedEvent { client_id: "c1".into(), ts_ms: 1, event: Event::Standby };
/// assert_eq!(
///     serde_json::to_string(&relayed).unwrap(),
///     r#"{"client_id":"c1","ts_ms":1,"event":"standby"}"#
/// );
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RelayedEvent {
    /// The `X-Marathon-Client` ID of the client the event came from.
    pub client_id: String,
    /// When the server received it, in milliseconds since the Unix epoch.
    pub ts_ms: i64,
    #[serde(flatten)]
    pub event: Event,
}

/// What a connected client is playing, as far as its events tell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ClientPlaying {
    pub channel_id: String,
    pub channel_number: u32,
    pub item_id: String,
    /// Human-readable description, e.g. `Show · S03E14 · The Injury`.
    pub title: String,
}

/// One entry of `GET /clients`: a client connected to the control channel
/// (`/clients/connect`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ClientInfo {
    /// The client's `X-Marathon-Client` ID; use it in
    /// `POST /clients/{id}/actions`.
    pub id: String,
    /// When this connection was opened, in milliseconds since the Unix
    /// epoch.
    pub connected_at_ms: i64,
    /// When the client last connected or sent an event. The most recent
    /// one is the `default` client.
    pub last_active_ms: i64,
    /// Whether this is the client `default` currently refers to.
    pub is_default: bool,
    /// What it is playing; `None` before its first `now_playing` event, or
    /// while in standby.
    pub playing: Option<ClientPlaying>,
    /// Whether playback is paused.
    pub paused: bool,
    /// Whether the client is in standby.
    pub standby: bool,
}

/// Response to `POST /clients/{id}/actions`: the action was handed to the
/// client's control channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ActionAccepted {
    /// The client the action went to (useful when the request said
    /// `default`).
    pub client_id: String,
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn is_snake_case(s: &str) -> bool {
        !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c == '_')
    }

    #[test]
    fn every_action_round_trips_with_a_snake_case_tag() {
        for action in Action::examples() {
            let value = serde_json::to_value(&action).unwrap();
            let tag = value["action"].as_str().unwrap();
            assert!(is_snake_case(tag), "{tag}");
            assert_eq!(serde_json::from_value::<Action>(value).unwrap(), action);
        }
    }

    #[test]
    fn every_event_round_trips_with_a_snake_case_tag() {
        for event in Event::examples() {
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
    fn names_match_the_json_tags() {
        for action in Action::examples() {
            assert_eq!(
                serde_json::to_value(&action).unwrap()["action"],
                action.name()
            );
        }
        for event in Event::examples() {
            assert_eq!(serde_json::to_value(&event).unwrap()["event"], event.name());
        }
    }

    #[test]
    fn relayed_events_flatten_and_round_trip() {
        for event in Event::examples() {
            let relayed = RelayedEvent {
                client_id: "c".into(),
                ts_ms: 42,
                event: event.clone(),
            };
            let value = serde_json::to_value(&relayed).unwrap();
            assert_eq!(value["client_id"], "c");
            assert_eq!(value["event"], event.name());
            assert_eq!(
                serde_json::from_value::<RelayedEvent>(value).unwrap(),
                relayed
            );
        }
    }

    #[test]
    fn unknown_actions_are_rejected_not_misread() {
        assert!(serde_json::from_str::<Action>(r#"{"action": "self_destruct"}"#).is_err());
        assert!(serde_json::from_str::<Action>(r#"{"action": "go_to_channel"}"#).is_err());
    }
}
