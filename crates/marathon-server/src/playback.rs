//! Channel playback state: what each channel is on, and moving it along.
//!
//! Every operation here takes the server-wide playback lock
//! ([`crate::app::Inner::playback`]) for its whole read-modify-write, so
//! concurrent requests see each other's effects in order.
//!
//! The work of resolving a channel (through any child channels) to an item
//! and moving it along is done by [`crate::nested::Resolver`] on data
//! loaded here; this module loads that data, saves what changed, records
//! events, and builds API responses.

use std::collections::{HashMap, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

use marathon_api::{
    AdvanceReason, ChannelRef, ChannelSummary, EventKind, Item, NextItem, Now, PlannedItem,
    ProgressReport, Subtitle, paths,
};

use crate::app::App;
use crate::db::{ItemRow, NewEvent, StateRow};
use crate::error::ApiError;
use crate::nested::{
    ChannelData, Data, MAX_SKIPPED_CHILDREN, NodeState, Resolved, Resolver, Row, Unresolved,
    duration_of,
};
use crate::scan::SidecarSubtitle;

/// Default and maximum number of items in a plan.
const PLAN_DEFAULT_ITEMS: u32 = 10;
const PLAN_MAX_ITEMS: u32 = 500;

/// How far ahead to plan: `GET /channels/{id}/plan`'s query.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlanLimit {
    /// At most this many items, including the current one (default 10, or
    /// 500 when `hours` is given; at most 500).
    pub items: Option<u32>,
    /// Stop at the first item starting this many hours or more ahead.
    pub hours: Option<u32>,
}

impl App {
    /// `GET /channels`: channels with what they are on, in guide order (by
    /// number). Hidden channels are left out unless `include_hidden`, and
    /// then listed after the rest. Channels never tuned before get their
    /// starting state here.
    pub async fn list_channels(
        &self,
        include_hidden: bool,
    ) -> Result<Vec<ChannelSummary>, ApiError> {
        let _guard = self.inner.playback.lock().await;
        let mut channels: Vec<_> = self
            .config()
            .channels
            .iter()
            .filter(|c| include_hidden || !c.hidden)
            .collect();
        channels.sort_by(|a, b| {
            (a.hidden, a.number.unwrap_or(0), &a.id).cmp(&(b.hidden, b.number.unwrap_or(0), &b.id))
        });
        let mut out = Vec::new();
        for channel in channels {
            let id = channel.id.as_str();
            let (data, rows) = self.load_data(id).await?;
            let mut resolver = Resolver::new(&data, rows, random_seed);
            let resolved = resolver.ensure(id, id, None);
            let item_count = u32::try_from(resolver.leaf_items(id).len()).unwrap_or(u32::MAX);
            let warnings = resolver.take_warnings();
            self.save_rows(resolver.into_rows()).await?;
            let mut summary = ChannelSummary {
                id: id.to_owned(),
                number: channel.number.unwrap_or(0),
                name: channel.name.clone(),
                item_count,
                current: None,
                position_ms: 0,
                hidden: channel.hidden,
                resolution_path: Vec::new(),
            };
            match resolved {
                Ok(resolved) => {
                    summary.current = Some(to_api_item(&resolved.item)?);
                    summary.position_ms = to_u64(resolved.position_ms);
                    summary.resolution_path = channel_refs(&data, &resolved);
                }
                Err(err) => {
                    tracing::debug!(channel = id, ?err, ?warnings, "channel has nothing to play");
                }
            }
            out.push(summary);
        }
        Ok(out)
    }

    /// `GET /channels/{id}/now`: tunes in. Records a `tuned` event.
    pub async fn now(&self, channel_id: &str, client_id: Option<&str>) -> Result<Now, ApiError> {
        let _guard = self.inner.playback.lock().await;
        let (data, rows) = self.load_data(channel_id).await?;
        let mut resolver = Resolver::new(&data, rows, random_seed);
        let resolved = self.resolve(&mut resolver, channel_id).await?;
        let next = predict_next(&resolver, channel_id);
        let warnings = resolver.take_warnings();
        self.save_rows(resolver.into_rows()).await?;
        self.record(
            channel_id,
            &resolved,
            client_id,
            EventKind::Tuned,
            resolved.position_ms,
        )
        .await?;
        to_now(&data, channel_id, &resolved, next.as_ref(), warnings)
    }

    /// `POST /channels/{id}/progress`: saves the playback position. For a
    /// channel playing a child channel, the position is saved where the
    /// item's own channel keeps it (the child's state, if shared).
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
        let (data, rows) = self.load_data(channel_id).await?;
        let mut resolver = Resolver::new(&data, rows, random_seed);
        let resolved = self.resolve(&mut resolver, channel_id).await?;
        let current = &resolved.item;
        if current.id != report.item_id {
            self.save_rows(resolver.into_rows()).await?;
            return Err(ApiError::StaleItem {
                channel: channel_id.to_owned(),
                reported: report.item_id.clone(),
                current: current.id.clone(),
            });
        }
        let position = to_i64(report.position_ms);
        resolver.set_position(resolved.owner(), position);
        self.save_rows(resolver.into_rows()).await?;
        let db = &self.inner.db;
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
        self.record(channel_id, &resolved, client_id, kind, position)
            .await
    }

    /// `POST /channels/{id}/advance`. Idempotent: only moves the channel if
    /// `from_item_id` is its current item; otherwise returns where it is.
    /// A channel playing a child channel moves the child on, then picks its
    /// next entry.
    pub async fn advance(
        &self,
        channel_id: &str,
        from_item_id: &str,
        reason: AdvanceReason,
        client_id: Option<&str>,
    ) -> Result<Now, ApiError> {
        self.step(channel_id, from_item_id, client_id, reason.into())
            .await
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
        self.step(channel_id, from_item_id, client_id, EventKind::Previous)
            .await
    }

    /// `advance` (for `Ended` and `Skipped`) and `previous`.
    async fn step(
        &self,
        channel_id: &str,
        from_item_id: &str,
        client_id: Option<&str>,
        kind: EventKind,
    ) -> Result<Now, ApiError> {
        let _guard = self.inner.playback.lock().await;
        let (data, rows) = self.load_data(channel_id).await?;
        let mut resolver = Resolver::new(&data, rows, random_seed);
        let from = self.resolve(&mut resolver, channel_id).await?;
        let moved = from.item.id == from_item_id
            && match kind {
                EventKind::Previous => resolver.previous(channel_id, channel_id),
                _ => {
                    resolver.advance(channel_id, channel_id);
                    true
                }
            };
        let resolved = if moved {
            self.resolve(&mut resolver, channel_id).await?
        } else {
            from.clone()
        };
        let next = predict_next(&resolver, channel_id);
        let warnings = resolver.take_warnings();
        self.save_rows(resolver.into_rows()).await?;
        if moved {
            self.record(channel_id, &from, client_id, kind, from.position_ms)
                .await?;
        }
        to_now(&data, channel_id, &resolved, next.as_ref(), warnings)
    }

    /// `GET /channels/{id}/plan`: what the channel will play, starting with
    /// the current item, if it plays on without interruption. Computed by
    /// stepping copies of the channel's state (and its children's)
    /// exactly as playback would, so playing the channel produces the
    /// same sequence, as long as nothing else moves it or its children
    /// meanwhile. Records no event.
    pub async fn plan(
        &self,
        channel_id: &str,
        limit: PlanLimit,
    ) -> Result<Vec<PlannedItem>, ApiError> {
        let _guard = self.inner.playback.lock().await;
        let (data, rows) = self.load_data(channel_id).await?;
        let mut resolver = Resolver::new(&data, rows, random_seed);
        let mut current = self.resolve(&mut resolver, channel_id).await?;
        // Starting a channel never tuned is saved, as `GET /channels`
        // does; everything after that is a dry run.
        let mut dry = resolver.dry_run();
        self.save_rows(resolver.into_rows()).await?;

        let max_items = limit
            .items
            .unwrap_or(if limit.hours.is_some() {
                PLAN_MAX_ITEMS
            } else {
                PLAN_DEFAULT_ITEMS
            })
            .clamp(1, PLAN_MAX_ITEMS);
        let horizon = limit.hours.map(|h| u64::from(h) * 3_600_000);
        let mut starts_in: u64 = 0;
        let mut position = to_u64(current.position_ms);
        let mut out = Vec::new();
        loop {
            out.push(PlannedItem {
                item: to_api_item(&current.item)?,
                resolution_path: channel_refs(&data, &current),
                starts_in_ms: starts_in,
            });
            starts_in += duration_of(&current.item).saturating_sub(position);
            position = 0;
            if out.len() >= max_items as usize || horizon.is_some_and(|h| starts_in >= h) {
                break;
            }
            dry.advance(channel_id, channel_id);
            match dry.ensure(channel_id, channel_id, None) {
                Ok(next) => current = next,
                Err(err) => {
                    tracing::debug!(channel = channel_id, ?err, "plan stops early");
                    break;
                }
            }
        }
        Ok(out)
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

    /// Resolves a channel to its current item, saving whatever progress
    /// the resolver made (such as stepping past empty children) even if it
    /// fails.
    async fn resolve(
        &self,
        resolver: &mut Resolver<'_>,
        channel_id: &str,
    ) -> Result<Resolved, ApiError> {
        match resolver.ensure(channel_id, channel_id, None) {
            Ok(resolved) => Ok(resolved),
            Err(err) => {
                let warnings = resolver.take_warnings();
                self.save_rows(resolver.clone().into_rows()).await?;
                Err(match err {
                    Unresolved::Empty(_) if warnings.is_empty() => {
                        ApiError::ChannelEmpty(channel_id.to_owned())
                    }
                    Unresolved::Empty(_) => ApiError::NothingToPlay {
                        channel: channel_id.to_owned(),
                        detail: warnings.join("; "),
                    },
                    Unresolved::TooManySkipped => ApiError::NothingToPlay {
                        channel: channel_id.to_owned(),
                        detail: format!(
                            "stopped after skipping {MAX_SKIPPED_CHILDREN} child channels \
                             with nothing to play ({})",
                            warnings.join("; ")
                        ),
                    },
                })
            }
        }
    }

    /// Loads a channel and every channel nested in it: definitions from
    /// config, items and state rows from the database. Must be called with
    /// the playback lock held.
    async fn load_data(&self, channel_id: &str) -> Result<(Data, HashMap<String, Row>), ApiError> {
        let config = self.config();
        if config.channel(channel_id).is_none() {
            return Err(ApiError::NotFound(format!("no channel {channel_id}")));
        }
        let db = &self.inner.db;
        let mut data = Data::default();
        let mut rows = HashMap::new();
        let mut queue = VecDeque::from([channel_id.to_owned()]);
        while let Some(id) = queue.pop_front() {
            if data.channels.contains_key(&id) {
                continue;
            }
            // Config validation guarantees every child exists; skip any
            // that does not rather than fail.
            let Some(channel) = config.channel(&id) else {
                continue;
            };
            queue.extend(channel.children().map(|c| c.channel.clone()));
            let members = db.channel_items(&id).await?;
            if let Some(state) = db.state(&id).await? {
                rows.insert(id.clone(), row_from(&state));
            }
            let positions = members
                .iter()
                .filter_map(|m| Some((m.item.id.clone(), u32::try_from(m.position?).ok()?)))
                .collect();
            let items = members.into_iter().map(|m| m.item).collect();
            data.channels.insert(
                id,
                ChannelData {
                    config: channel.clone(),
                    items,
                    positions,
                },
            );
        }
        Ok((data, rows))
    }

    /// Saves the rows a resolver changed.
    async fn save_rows(&self, rows: HashMap<String, Row>) -> Result<(), ApiError> {
        let now = now_ms();
        for (channel_id, row) in rows {
            let (true, Some(own)) = (row.dirty, row.own) else {
                continue;
            };
            let state = StateRow {
                channel_id,
                item_id: own.item_id,
                position_ms: own.position_ms,
                cursor_json: serde_json::to_string(&own.cursor)?,
                updated_at: now,
                children_json: serde_json::to_string(&row.children)?,
            };
            self.inner.db.save_state(&state).await?;
        }
        Ok(())
    }

    async fn record(
        &self,
        channel_id: &str,
        resolved: &Resolved,
        client_id: Option<&str>,
        kind: EventKind,
        position_ms: i64,
    ) -> Result<(), ApiError> {
        let path: Vec<&str> = resolved
            .chain
            .iter()
            .map(|h| h.channel_id.as_str())
            .collect();
        let path_json = serde_json::to_string(&path)?;
        let event = NewEvent {
            ts: now_ms(),
            client_id,
            channel_id,
            resolved_channel_id: &resolved.owner().channel_id,
            resolution_path_json: &path_json,
            item_id: &resolved.item.id,
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

/// Reads a `channel_state` row. An unreadable cursor or children object is
/// discarded with a warning, and the channel starts over (keeping nothing
/// it cannot read).
fn row_from(state: &StateRow) -> Row {
    let own = serde_json::from_str(&state.cursor_json)
        .inspect_err(|err| {
            tracing::warn!(channel = %state.channel_id, %err, "discarding unreadable cursor");
        })
        .ok()
        .map(|cursor| NodeState {
            item_id: state.item_id.clone(),
            position_ms: state.position_ms,
            cursor,
        });
    let children = serde_json::from_str(&state.children_json)
        .inspect_err(|err| {
            tracing::warn!(channel = %state.channel_id, %err,
                "discarding unreadable child channel states");
        })
        .unwrap_or_default();
    Row {
        own,
        children,
        dirty: false,
    }
}

/// What the channel will play after its current item, worked out on a dry
/// run of the resolver.
fn predict_next(resolver: &Resolver<'_>, channel_id: &str) -> Option<Resolved> {
    let mut dry = resolver.dry_run();
    dry.advance(channel_id, channel_id);
    dry.ensure(channel_id, channel_id, None).ok()
}

/// The resolution path, with names.
fn channel_refs(data: &Data, resolved: &Resolved) -> Vec<ChannelRef> {
    resolved
        .chain
        .iter()
        .map(|hop| ChannelRef {
            id: hop.channel_id.clone(),
            name: data
                .channels
                .get(&hop.channel_id)
                .map(|c| c.config.name.clone())
                .unwrap_or_default(),
        })
        .collect()
}

fn to_now(
    data: &Data,
    channel_id: &str,
    resolved: &Resolved,
    next: Option<&Resolved>,
    warnings: Vec<String>,
) -> Result<Now, ApiError> {
    let channel = &data
        .channels
        .get(channel_id)
        .ok_or_else(|| ApiError::NotFound(format!("no channel {channel_id}")))?
        .config;
    let next = next
        .map(|next| {
            Ok::<_, ApiError>(NextItem {
                item: to_api_item(&next.item)?,
                stream_url: paths::stream(&next.item.id),
            })
        })
        .transpose()?;
    Ok(Now {
        channel_id: channel_id.to_owned(),
        channel_number: channel.number.unwrap_or(0),
        channel_name: channel.name.clone(),
        item: to_api_item(&resolved.item)?,
        position_ms: to_u64(resolved.position_ms),
        stream_url: paths::stream(&resolved.item.id),
        next,
        resolution_path: channel_refs(data, resolved),
        warnings,
    })
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
