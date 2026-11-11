//! Resolving context-dependent actions (PLAN.md §9): what
//! [`Action::Navigate`], [`Action::Select`], and [`Action::Back`] mean
//! depends on what is open.
//!
//! - Nothing open: Up/Down change channel, Left/Right seek ∓10s, Select
//!   shows info.
//! - Guide open: Up/Down move the highlighted channel (wrapping),
//!   Left/Right do nothing, Select tunes to the highlighted channel and
//!   closes the guide, Back closes it.
//!
//! This is UI state, kept apart from the tuner's playback logic. The tuner
//! tells the overlay about it with [`UiEvent::Guide`](crate::ui::UiEvent::Guide).

use marathon_api::{Action, ChannelSummary, Direction};

/// Seconds that Left and Right seek by with nothing open.
pub const SEEK_STEP_SECS: i32 = 10;

/// What is open on screen, for resolving navigation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Navigation {
    /// Index of the highlighted channel, if the guide is open.
    guide: Option<usize>,
}

/// What a navigation action turned out to mean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// Carry out this (context-free) action.
    Action(Action),
    /// The guide's highlight moved; redraw it.
    GuideMoved,
    /// Nothing to do in this context.
    Nothing,
}

impl Navigation {
    pub fn guide_open(&self) -> bool {
        self.guide.is_some()
    }

    /// Index into `channels` of the highlighted channel, if the guide is
    /// open and the index is valid.
    pub fn highlighted(&self) -> Option<usize> {
        self.guide
    }

    /// Opens the guide highlighting `current` (a channel ID), or the first
    /// channel.
    pub fn open_guide(&mut self, channels: &[ChannelSummary], current: Option<&str>) {
        let index = current
            .and_then(|id| channels.iter().position(|c| c.id == id))
            .unwrap_or(0);
        self.guide = Some(index);
    }

    /// Closes whatever is open. Returns whether anything was.
    pub fn close(&mut self) -> bool {
        self.guide.take().is_some()
    }

    /// Resolves [`Action::Navigate`].
    pub fn navigate(&mut self, direction: Direction, channels: &[ChannelSummary]) -> Resolved {
        match (&mut self.guide, direction) {
            (None, Direction::Up) => Resolved::Action(Action::ChannelUp),
            (None, Direction::Down) => Resolved::Action(Action::ChannelDown),
            (None, Direction::Left) => Resolved::Action(Action::SeekRelative {
                seconds: -SEEK_STEP_SECS,
            }),
            (None, Direction::Right) => Resolved::Action(Action::SeekRelative {
                seconds: SEEK_STEP_SECS,
            }),
            (Some(_), _) if channels.is_empty() => Resolved::Nothing,
            // The channel list may have shrunk since the guide opened, so
            // an out-of-range highlight is treated as the last channel.
            (Some(i), Direction::Up) => {
                let last = channels.len() - 1;
                *i = if *i == 0 || *i > last { last } else { *i - 1 };
                Resolved::GuideMoved
            }
            (Some(i), Direction::Down) => {
                *i = if *i >= channels.len() - 1 { 0 } else { *i + 1 };
                Resolved::GuideMoved
            }
            (Some(_), Direction::Left | Direction::Right) => Resolved::Nothing,
        }
    }

    /// Resolves [`Action::Select`].
    pub fn select(&mut self, channels: &[ChannelSummary]) -> Resolved {
        match self.guide.take() {
            Some(i) => match channels.get(i) {
                Some(c) => Resolved::Action(Action::GoToChannelId { id: c.id.clone() }),
                None => Resolved::Nothing,
            },
            None => Resolved::Action(Action::ShowInfo),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channels(n: u32) -> Vec<ChannelSummary> {
        (1..=n)
            .map(|i| ChannelSummary {
                id: format!("c{i}"),
                number: i,
                name: format!("Channel {i}"),
                item_count: 1,
                current: None,
                position_ms: 0,
                hidden: false,
                resolution_path: Vec::new(),
            })
            .collect()
    }

    #[test]
    fn with_nothing_open_up_down_change_channel_and_left_right_seek() {
        let mut nav = Navigation::default();
        let ch = channels(3);
        let resolve = |nav: &mut Navigation, d| nav.navigate(d, &ch);
        assert_eq!(
            resolve(&mut nav, Direction::Up),
            Resolved::Action(Action::ChannelUp)
        );
        assert_eq!(
            resolve(&mut nav, Direction::Down),
            Resolved::Action(Action::ChannelDown)
        );
        assert_eq!(
            resolve(&mut nav, Direction::Left),
            Resolved::Action(Action::SeekRelative { seconds: -10 })
        );
        assert_eq!(
            resolve(&mut nav, Direction::Right),
            Resolved::Action(Action::SeekRelative { seconds: 10 })
        );
        assert_eq!(nav.select(&ch), Resolved::Action(Action::ShowInfo));
        assert!(!nav.close());
    }

    #[test]
    fn with_the_guide_open_arrows_move_the_selection() {
        let ch = channels(3);
        let mut nav = Navigation::default();
        nav.open_guide(&ch, Some("c2"));
        assert_eq!(nav.highlighted(), Some(1));

        assert_eq!(nav.navigate(Direction::Down, &ch), Resolved::GuideMoved);
        assert_eq!(nav.highlighted(), Some(2));
        assert_eq!(nav.navigate(Direction::Down, &ch), Resolved::GuideMoved);
        assert_eq!(nav.highlighted(), Some(0), "wraps at the bottom");
        assert_eq!(nav.navigate(Direction::Up, &ch), Resolved::GuideMoved);
        assert_eq!(nav.highlighted(), Some(2), "wraps at the top");
        assert_eq!(nav.navigate(Direction::Left, &ch), Resolved::Nothing);
        assert_eq!(nav.navigate(Direction::Right, &ch), Resolved::Nothing);
        assert_eq!(nav.highlighted(), Some(2));

        assert_eq!(
            nav.select(&ch),
            Resolved::Action(Action::GoToChannelId { id: "c3".into() })
        );
        assert!(!nav.guide_open(), "selecting closes the guide");
    }

    #[test]
    fn back_closes_the_guide() {
        let ch = channels(2);
        let mut nav = Navigation::default();
        nav.open_guide(&ch, None);
        assert_eq!(nav.highlighted(), Some(0));
        assert!(nav.close());
        assert!(!nav.guide_open());
        assert_eq!(
            nav.navigate(Direction::Up, &ch),
            Resolved::Action(Action::ChannelUp)
        );
    }

    #[test]
    fn an_empty_guide_never_panics() {
        let mut nav = Navigation::default();
        nav.open_guide(&[], None);
        for d in [
            Direction::Up,
            Direction::Down,
            Direction::Left,
            Direction::Right,
        ] {
            assert_eq!(nav.navigate(d, &[]), Resolved::Nothing);
        }
        assert_eq!(nav.select(&[]), Resolved::Nothing);
    }
}
