//! What the [`Tuner`](crate::tuner::Tuner) tells the on-screen overlay
//! (PLAN.md §9 "Overlay UI"): when to show the channel banner, what it
//! says, and what the guide lists.
//!
//! These are local presentation cues, not the [`Event`](marathon_api::Event)s
//! that observers subscribe to: they carry exactly what a screen needs to
//! draw, including state the server never hears about (the guide's
//! highlight). The overlay itself, and how it looks, belong to the app.

use marathon_api::{ChannelSummary, Item, Now};

/// A cue for the overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiEvent {
    /// A channel switch has begun. Sent before the tuner asks the server
    /// for the channel or loads anything, so the banner can appear the
    /// instant the viewer acts (PLAN.md §9). `banner.title` is what the
    /// channel was last known to be on, and may change in the
    /// [`UiEvent::Playing`] that follows.
    Tuning(Banner),
    /// This is what is playing now: after a tune, a skip, an automatic
    /// advance, or a failed switch (which reverts to the old channel).
    /// `announce` says whether to show the banner; it is false when the
    /// channel just moved on by itself.
    Playing { banner: Banner, announce: bool },
    /// Nothing is playing any more (e.g. too many items failed).
    Stopped,
    /// The viewer asked what is on: show the banner.
    ShowInfo,
    /// The viewer pressed Back with nothing open that the tuner tracks:
    /// hide whatever is showing (the banner).
    Hide,
    /// The guide opened, its highlight moved, or (`None`) it closed.
    Guide(Option<Guide>),
}

/// What the channel banner says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Banner {
    pub channel_number: u32,
    pub channel_name: String,
    /// The item, e.g. `S03E14 · The Injury` (see [`item_title`]), if known.
    pub title: Option<String>,
    /// The show, if the item has one and it differs from the channel name.
    pub show: Option<String>,
}

impl Banner {
    pub fn from_now(now: &Now) -> Self {
        Self::new(now.channel_number, &now.channel_name, Some(&now.item))
    }

    pub fn from_summary(channel: &ChannelSummary) -> Self {
        Self::new(channel.number, &channel.name, channel.current.as_ref())
    }

    fn new(number: u32, name: &str, item: Option<&Item>) -> Self {
        Self {
            channel_number: number,
            channel_name: name.to_owned(),
            title: item.map(item_title),
            show: item
                .and_then(|i| i.show.clone())
                .filter(|show| show != name),
        }
    }
}

/// The guide: every channel and what it is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guide {
    pub rows: Vec<GuideRow>,
    /// Index into `rows` of the highlighted channel.
    pub highlighted: usize,
}

/// One channel in the guide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuideRow {
    pub number: u32,
    pub name: String,
    /// What the channel is on (see [`item_title`]); `None` if it has
    /// nothing to play.
    pub on: Option<String>,
    /// Whether this is the channel being watched.
    pub current: bool,
}

impl Guide {
    /// The guide for `channels`, marking `current` (a channel ID).
    pub fn new(channels: &[ChannelSummary], highlighted: usize, current: Option<&str>) -> Self {
        Self {
            rows: channels
                .iter()
                .map(|c| GuideRow {
                    number: c.number,
                    name: c.name.clone(),
                    on: c.current.as_ref().map(item_title),
                    current: Some(c.id.as_str()) == current,
                })
                .collect(),
            highlighted,
        }
    }
}

/// An item's title for the screen: `S03E14 · The Injury`, `E02 · Title`,
/// or just the title. (The show is left to the caller; on a channel named
/// after the show it is redundant.)
pub fn item_title(item: &Item) -> String {
    match (item.season, item.episode) {
        (Some(s), Some(e)) => format!("S{s:02}E{e:02} · {}", item.title),
        (None, Some(e)) => format!("E{e:02} · {}", item.title),
        _ => item.title.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(show: Option<&str>, season: Option<u32>, episode: Option<u32>) -> Item {
        Item {
            id: "i".into(),
            source_id: "s".into(),
            path: "p.mkv".into(),
            show: show.map(Into::into),
            season,
            episode,
            title: "The Injury".into(),
            duration_ms: None,
            subtitles: Vec::new(),
            missing: false,
        }
    }

    #[test]
    fn item_titles() {
        assert_eq!(
            item_title(&item(None, Some(3), Some(14))),
            "S03E14 · The Injury"
        );
        assert_eq!(item_title(&item(None, None, Some(2))), "E02 · The Injury");
        assert_eq!(item_title(&item(None, Some(1), None)), "The Injury");
    }

    #[test]
    fn banner_omits_a_show_named_like_the_channel() {
        let summary = |show| ChannelSummary {
            id: "office".into(),
            number: 1,
            name: "The Office".into(),
            item_count: 1,
            current: Some(item(Some(show), Some(3), Some(14))),
            position_ms: 0,
            hidden: false,
            resolution_path: Vec::new(),
        };
        let banner = Banner::from_summary(&summary("The Office"));
        assert_eq!(banner.show, None);
        assert_eq!(banner.title.as_deref(), Some("S03E14 · The Injury"));
        let banner = Banner::from_summary(&summary("Parks"));
        assert_eq!(banner.show.as_deref(), Some("Parks"));
    }
}
