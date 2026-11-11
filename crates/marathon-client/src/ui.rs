//! What the [`Tuner`](crate::tuner::Tuner) tells the on-screen overlay
//! (PLAN.md §9 "Overlay UI"): when to show the channel banner, what it
//! says, and what the guide lists.
//!
//! These are local presentation cues, not the [`Event`](marathon_api::Event)s
//! that observers subscribe to: they carry exactly what a screen needs to
//! draw, including state the server never hears about (the guide's
//! highlight). The overlay itself, and how it looks, belong to the app.

use marathon_api::{ChannelRef, ChannelSummary, Item, Now};

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
    /// The show, if the item has one and it differs from the name of the
    /// channel it comes from.
    pub show: Option<String>,
    /// The child channels the item comes through, for a channel with
    /// nested channels (PLAN.md M11), e.g. `Contrapoints`; `None` for an
    /// ordinary channel. See [`Banner::item_line`].
    pub via: Option<String>,
}

impl Banner {
    pub fn from_now(now: &Now) -> Self {
        Self::new(
            now.channel_number,
            &now.channel_name,
            &now.resolution_path,
            Some(&now.item),
        )
    }

    pub fn from_summary(channel: &ChannelSummary) -> Self {
        Self::new(
            channel.number,
            &channel.name,
            &channel.resolution_path,
            channel.current.as_ref(),
        )
    }

    fn new(number: u32, name: &str, path: &[ChannelRef], item: Option<&Item>) -> Self {
        // The channel the item belongs to: a show named like it is
        // redundant.
        let owner = path.last().map_or(name, |c| c.name.as_str());
        Self {
            channel_number: number,
            channel_name: name.to_owned(),
            title: item.map(item_title),
            show: item
                .and_then(|i| i.show.clone())
                .filter(|show| show != owner),
            via: via(path),
        }
    }

    /// The line under the channel name: `Contrapoints: Show · S01E02 ·
    /// Envy`, with whichever parts are known. Together with the channel
    /// name above it, a nested channel reads "Video Essayists ·
    /// Contrapoints: Envy".
    pub fn item_line(&self) -> String {
        let item = match (&self.show, &self.title) {
            (Some(show), Some(title)) => format!("{show} · {title}"),
            (None, Some(title)) => title.clone(),
            (Some(show), None) => show.clone(),
            (None, None) => String::new(),
        };
        match &self.via {
            Some(via) if item.is_empty() => via.clone(),
            Some(via) => format!("{via}: {item}"),
            None => item,
        }
    }
}

/// The names of the child channels in a resolution path (everything after
/// the tuned channel), joined with ` · `; `None` if there are none.
fn via(path: &[ChannelRef]) -> Option<String> {
    let names: Vec<&str> = path.iter().skip(1).map(|c| c.name.as_str()).collect();
    (!names.is_empty()).then(|| names.join(" · "))
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
    /// What the channel is on (see [`item_title`]), after the child
    /// channels it comes through if any (`Contrapoints: Envy`); `None` if
    /// it has nothing to play.
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
                    on: c
                        .current
                        .as_ref()
                        .map(|item| match via(&c.resolution_path) {
                            Some(via) => format!("{via}: {}", item_title(item)),
                            None => item_title(item),
                        }),
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
        assert_eq!(banner.via, None);
        assert_eq!(banner.item_line(), "Parks · S03E14 · The Injury");
        assert_eq!(banner.show.as_deref(), Some("Parks"));
    }

    fn path(names: &[(&str, &str)]) -> Vec<ChannelRef> {
        names
            .iter()
            .map(|(id, name)| ChannelRef {
                id: (*id).into(),
                name: (*name).into(),
            })
            .collect()
    }

    #[test]
    fn nested_channels_show_the_child_before_the_title() {
        let mut envy = item(Some("Contrapoints"), None, None);
        envy.title = "Envy".into();
        let summary = ChannelSummary {
            id: "essayists".into(),
            number: 10,
            name: "Video Essayists".into(),
            item_count: 3,
            current: Some(envy),
            position_ms: 0,
            hidden: false,
            resolution_path: path(&[
                ("essayists", "Video Essayists"),
                ("contrapoints", "Contrapoints"),
            ]),
        };
        let banner = Banner::from_summary(&summary);
        assert_eq!(banner.channel_name, "Video Essayists");
        // The show is named like the child channel, so it is left out.
        assert_eq!(banner.show, None);
        assert_eq!(banner.via.as_deref(), Some("Contrapoints"));
        assert_eq!(
            format!("{} · {}", banner.channel_name, banner.item_line()),
            "Video Essayists · Contrapoints: Envy"
        );

        let guide = Guide::new(std::slice::from_ref(&summary), 0, None);
        assert_eq!(guide.rows[0].on.as_deref(), Some("Contrapoints: Envy"));

        // Deeper nesting lists every child.
        let mut deep = summary;
        deep.resolution_path = path(&[("a", "A"), ("b", "B"), ("c", "Contrapoints")]);
        assert_eq!(
            Banner::from_summary(&deep).item_line(),
            "B · Contrapoints: Envy"
        );
    }
}
