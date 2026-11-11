//! Channel playback state: what each channel is on, and moving it along.
//!
//! Every operation here takes the server-wide playback lock
//! ([`crate::app::Inner::playback`]) for its whole read-modify-write, so
//! concurrent requests see each other's effects in order.

use std::time::{SystemTime, UNIX_EPOCH};

use marathon_api::{
    AdvanceReason, ChannelRef, ChannelSummary, EventKind, Item, NextItem, Now, ProgressReport,
    Subtitle, paths,
};
use marathon_rules::{Cursor, ItemKey, Rule};

use crate::app::App;
use crate::db::{ChannelRow, ItemRow, NewEvent, StateRow};
use crate::error::ApiError;
use crate::scan::SidecarSubtitle;

/// A channel with its rule, playable items, and (ensured) state.
struct Loaded {
    channel: ChannelRow,
    rule: Rule,
    items: Vec<ItemRow>,
    keys: Vec<ItemKey>,
    state: StateRow,
    cursor: Cursor,
}

impl Loaded {
    fn current(&self) -> &ItemRow {
        // `ensure_state` only ever stores an item from `items`, and `items`
        // is non-empty, so the lookup succeeds; fall back to the first item
        // rather than panic if that invariant is ever broken.
        self.items
            .iter()
            .find(|i| Some(&i.id) == self.state.item_id.as_ref())
            .unwrap_or(&self.items[0])
    }

    fn item(&self, id: &str) -> Option<&ItemRow> {
        self.items.iter().find(|i| i.id == id)
    }
}

impl App {
    /// `GET /channels`: every channel with what it is on. Channels never
    /// tuned before get their starting state here.
    pub async fn list_channels(&self) -> Result<Vec<ChannelSummary>, ApiError> {
        let _guard = self.inner.playback.lock().await;
        let mut out = Vec::new();
        for channel in self.inner.db.channels().await? {
            let summary = match self.load(&channel.id).await {
                Ok(loaded) => ChannelSummary {
                    id: channel.id.clone(),
                    number: to_u32(channel.number),
                    name: channel.name.clone(),
                    item_count: u32::try_from(loaded.items.len()).unwrap_or(u32::MAX),
                    current: Some(to_api_item(loaded.current())?),
                    position_ms: to_u64(loaded.state.position_ms),
                    hidden: false,
                    resolution_path: vec![ChannelRef {
                        id: channel.id.clone(),
                        name: channel.name.clone(),
                    }],
                },
                Err(ApiError::ChannelEmpty(_)) => ChannelSummary {
                    id: channel.id.clone(),
                    number: to_u32(channel.number),
                    name: channel.name.clone(),
                    item_count: 0,
                    current: None,
                    position_ms: 0,
                    hidden: false,
                    resolution_path: Vec::new(),
                },
                Err(err) => return Err(err),
            };
            out.push(summary);
        }
        Ok(out)
    }

    /// `GET /channels/{id}/now`: tunes in. Records a `tuned` event.
    pub async fn now(&self, channel_id: &str, client_id: Option<&str>) -> Result<Now, ApiError> {
        let _guard = self.inner.playback.lock().await;
        let loaded = self.load(channel_id).await?;
        self.record(
            &loaded,
            client_id,
            &loaded.current().id,
            EventKind::Tuned,
            loaded.state.position_ms,
        )
        .await?;
        to_now(&loaded)
    }

    /// `POST /channels/{id}/progress`: saves the playback position.
    ///
    /// Rejects reports for anything but the current item with
    /// [`ApiError::StaleItem`], so a client still playing an old item cannot
    /// drag the channel's position around after another client advanced it.
    pub async fn progress(
        &self,
        channel_id: &str,
        report: &ProgressReport,
        client_id: Option<&str>,
    ) -> Result<(), ApiError> {
        let _guard = self.inner.playback.lock().await;
        let loaded = self.load(channel_id).await?;
        let current = loaded.current();
        if current.id != report.item_id {
            return Err(ApiError::StaleItem {
                channel: channel_id.to_owned(),
                reported: report.item_id.clone(),
                current: current.id.clone(),
            });
        }
        let position = to_i64(report.position_ms);
        let db = &self.inner.db;
        db.set_position(channel_id, position, now_ms()).await?;
        // A duration the catalog provided (Jellyfin's) is authoritative;
        // clients only fill in durations nobody else knows.
        let from_catalog = current.external_id.is_some() && current.duration_ms.is_some();
        if let Some(duration) = report.duration_ms.filter(|d| *d > 0)
            && !from_catalog
            && current.duration_ms != Some(to_i64(duration))
        {
            db.set_duration(&current.id, to_i64(duration)).await?;
        }
        let kind = if report.paused {
            EventKind::Paused
        } else if db.last_event_kind(channel_id).await?.as_deref()
            == Some(EventKind::Paused.as_str())
        {
            EventKind::Resumed
        } else {
            EventKind::Progress
        };
        self.record(&loaded, client_id, &current.id, kind, position)
            .await
    }

    /// `POST /channels/{id}/advance`. Idempotent: only moves the channel if
    /// `from_item_id` is its current item; otherwise returns where it is.
    pub async fn advance(
        &self,
        channel_id: &str,
        from_item_id: &str,
        reason: AdvanceReason,
        client_id: Option<&str>,
    ) -> Result<Now, ApiError> {
        let _guard = self.inner.playback.lock().await;
        let loaded = self.load(channel_id).await?;
        if loaded.current().id != from_item_id {
            return to_now(&loaded);
        }
        let Some(step) = marathon_rules::next(&loaded.rule, &loaded.keys, Some(&loaded.cursor))
        else {
            // `load` guarantees at least one item, and `next` only returns
            // `None` for zero items.
            return to_now(&loaded);
        };
        self.move_to(&loaded, step).await?;
        self.record(
            &loaded,
            client_id,
            from_item_id,
            reason.into(),
            loaded.state.position_ms,
        )
        .await?;
        to_now(&self.load(channel_id).await?)
    }

    /// `POST /channels/{id}/previous`. Like `advance`, only moves the
    /// channel if `from_item_id` is its current item. If the rule has
    /// nothing to go back to, the channel stays where it is.
    pub async fn previous(
        &self,
        channel_id: &str,
        from_item_id: &str,
        client_id: Option<&str>,
    ) -> Result<Now, ApiError> {
        let _guard = self.inner.playback.lock().await;
        let loaded = self.load(channel_id).await?;
        if loaded.current().id != from_item_id {
            return to_now(&loaded);
        }
        let Some(step) = marathon_rules::previous(&loaded.rule, &loaded.keys, &loaded.cursor)
        else {
            return to_now(&loaded);
        };
        self.move_to(&loaded, step).await?;
        self.record(
            &loaded,
            client_id,
            from_item_id,
            EventKind::Previous,
            loaded.state.position_ms,
        )
        .await?;
        to_now(&self.load(channel_id).await?)
    }

    /// `GET /items/{id}`.
    pub async fn item(&self, item_id: &str) -> Result<Item, ApiError> {
        let row = self
            .inner
            .db
            .item(item_id)
            .await?
            .ok_or_else(|| ApiError::NotFound(format!("no item {item_id}")))?;
        to_api_item(&row)
    }

    /// Loads a channel and makes sure it has a valid current item: a channel
    /// never tuned before starts at its rule's first item, and one whose
    /// current item has gone missing (or left the channel) moves on to the
    /// next. Must be called with the playback lock held.
    async fn load(&self, channel_id: &str) -> Result<Loaded, ApiError> {
        let db = &self.inner.db;
        let channel = db
            .channel(channel_id)
            .await?
            .ok_or_else(|| ApiError::NotFound(format!("no channel {channel_id}")))?;
        let rule: Rule = serde_json::from_str(&channel.rule_json)?;
        let items = db.channel_items(channel_id).await?;
        if items.is_empty() {
            return Err(ApiError::ChannelEmpty(channel_id.to_owned()));
        }
        let keys: Vec<ItemKey> = items.iter().map(to_key).collect();

        let saved = db.state(channel_id).await?;
        let saved_cursor = saved.as_ref().and_then(|s| {
            serde_json::from_str::<Cursor>(&s.cursor_json)
                .inspect_err(|err| {
                    tracing::warn!(channel = channel_id, %err, "discarding unreadable cursor");
                })
                .ok()
        });

        match (saved, saved_cursor) {
            (Some(state), Some(cursor))
                if state
                    .item_id
                    .as_ref()
                    .is_some_and(|id| items.iter().any(|i| &i.id == id)) =>
            {
                Ok(Loaded {
                    channel,
                    rule,
                    items,
                    keys,
                    state,
                    cursor,
                })
            }
            (saved, cursor) => {
                // First tune-in, an unreadable cursor, or the current item
                // is gone: step forward from wherever the cursor was.
                let cursor = cursor.unwrap_or_else(|| Cursor::seeded(random_seed()));
                let step = marathon_rules::next(&rule, &keys, Some(&cursor))
                    .ok_or_else(|| ApiError::ChannelEmpty(channel_id.to_owned()))?;
                if let Some(old) = saved.and_then(|s| s.item_id) {
                    tracing::info!(channel = channel_id, from = %old, to = %step.item.id,
                        "current item is no longer playable; moving on");
                }
                let state = StateRow {
                    channel_id: channel_id.to_owned(),
                    item_id: Some(step.item.id.clone()),
                    position_ms: 0,
                    cursor_json: serde_json::to_string(&step.cursor)?,
                    updated_at: now_ms(),
                };
                db.save_state(&state).await?;
                Ok(Loaded {
                    channel,
                    rule,
                    items,
                    keys,
                    state,
                    cursor: step.cursor,
                })
            }
        }
    }

    /// Moves a loaded channel to the start of `step`'s item.
    async fn move_to(&self, loaded: &Loaded, step: marathon_rules::Step) -> Result<(), ApiError> {
        let state = StateRow {
            channel_id: loaded.channel.id.clone(),
            item_id: Some(step.item.id),
            position_ms: 0,
            cursor_json: serde_json::to_string(&step.cursor)?,
            updated_at: now_ms(),
        };
        self.inner.db.save_state(&state).await?;
        Ok(())
    }

    async fn record(
        &self,
        loaded: &Loaded,
        client_id: Option<&str>,
        item_id: &str,
        kind: EventKind,
        position_ms: i64,
    ) -> Result<(), ApiError> {
        let event = NewEvent {
            ts: now_ms(),
            client_id,
            channel_id: &loaded.channel.id,
            item_id,
            kind: kind.as_str(),
            position_ms,
        };
        self.inner
            .db
            .insert_event(&uuid::Uuid::now_v7().to_string(), &event)
            .await?;
        Ok(())
    }
}

fn to_now(loaded: &Loaded) -> Result<Now, ApiError> {
    let current = loaded.current();
    let next = marathon_rules::next(&loaded.rule, &loaded.keys, Some(&loaded.cursor))
        .and_then(|step| loaded.item(&step.item.id))
        .map(|row| {
            Ok::<_, ApiError>(NextItem {
                item: to_api_item(row)?,
                stream_url: paths::stream(&row.id),
            })
        })
        .transpose()?;
    Ok(Now {
        channel_id: loaded.channel.id.clone(),
        channel_number: to_u32(loaded.channel.number),
        channel_name: loaded.channel.name.clone(),
        item: to_api_item(current)?,
        position_ms: to_u64(loaded.state.position_ms),
        stream_url: paths::stream(&current.id),
        next,
        resolution_path: vec![ChannelRef {
            id: loaded.channel.id.clone(),
            name: loaded.channel.name.clone(),
        }],
        warnings: Vec::new(),
    })
}

/// The rules engine's view of an item. The path is prefixed with the source
/// ID so that items from different sources in one channel stay distinct.
fn to_key(row: &ItemRow) -> ItemKey {
    ItemKey {
        id: row.id.clone(),
        path: format!("{}/{}", row.source_id, row.path),
        show: row.show.clone(),
        season: row.season.and_then(|s| u32::try_from(s).ok()),
        episode: row.episode.and_then(|e| u32::try_from(e).ok()),
    }
}

/// The sidecar subtitles stored for an item.
pub(crate) fn subtitles_of(row: &ItemRow) -> Result<Vec<SidecarSubtitle>, serde_json::Error> {
    serde_json::from_str(&row.subtitles_json)
}

pub(crate) fn to_api_item(row: &ItemRow) -> Result<Item, ApiError> {
    let subtitles = subtitles_of(row)?
        .into_iter()
        .zip(0u32..)
        .map(|(sub, index)| Subtitle {
            index,
            language: sub.language,
            label: sub.label,
            format: sub.format,
            url: paths::subtitle(&row.id, index),
        })
        .collect();
    Ok(Item {
        id: row.id.clone(),
        source_id: row.source_id.clone(),
        path: row.path.clone(),
        show: row.show.clone(),
        season: row.season.and_then(|s| u32::try_from(s).ok()),
        episode: row.episode.and_then(|e| u32::try_from(e).ok()),
        title: row.title.clone(),
        duration_ms: row.duration_ms.map(to_u64),
        subtitles,
        missing: row.missing,
    })
}

/// A seed for a channel's first cursor. The rules engine never picks seeds
/// itself; the server draws one from the OS RNG (via a v4 UUID) when a
/// channel is first tuned and stores it in the cursor.
fn random_seed() -> u64 {
    uuid::Uuid::new_v4().as_u64_pair().0
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

fn to_u64(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

fn to_u32(v: i64) -> u32 {
    u32::try_from(v).unwrap_or(0)
}
