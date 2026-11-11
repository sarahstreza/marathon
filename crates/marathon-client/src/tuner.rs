//! The Tuner: the client's state machine for watching channels
//! (PLAN.md §9).
//!
//! ```text
//! Idle → Tuning(channel) → Playing(channel, item) ─ended→ Advancing → Playing
//!                               │
//!                               └─user switches channel→ (report progress) → Tuning(other)
//! ```
//!
//! The tuner is synchronous and player-agnostic. It is driven by four
//! inputs, all from one thread:
//!
//! - [`Tuner::start`] once, to tune the first channel;
//! - [`Tuner::handle_action`] for actions from the action bus;
//! - [`Tuner::handle_player_event`] when the player starts, finishes, or
//!   fails an item;
//! - [`Tuner::tick`] by [`Tuner::next_deadline`], for periodic progress
//!   reports.
//!
//! Tuning and Advancing are transient: they happen inside those calls,
//! which block on the [`Api`]. Between calls the tuner is [`State::Idle`]
//! or [`State::Playing`].
//!
//! Semantics implemented here:
//!
//! - PLAN.md §1: the resume rewind, the finished threshold on tune-in, and
//!   progress reporting (every interval while playing, and immediately on
//!   pause, channel change, and quit).
//! - PLAN.md §9: context-dependent actions (`Navigate`, `Select`, `Back`)
//!   are resolved by [`Navigation`]; what happens is published as
//!   [`Event`]s through the sink set with [`Tuner::with_event_sink`].
//! - PLAN.md §9 "Overlay UI": what the screen should show (the channel
//!   banner, the guide) is published as [`UiEvent`]s through the sink set
//!   with [`Tuner::with_ui_sink`]. A channel switch announces itself
//!   before any network call, so the banner can appear instantly.
//! - Standby (PLAN.md M7): [`Action::Standby`] pauses and reports progress,
//!   and [`Action::Wake`] resumes, but only if standby was what paused it.
//!   Anything that starts playback (play, tune, skip) also ends standby.
//! - "Keeps going": an item that fails to load or play is skipped with an
//!   [`Event::Error`]. After [`TunerConfig::max_consecutive_failures`]
//!   failures in a row the tuner stops retrying and says so, rather than
//!   looping forever.

use std::time::{Duration, Instant};

use marathon_api::{
    Action, AdvanceReason, AdvanceRequest, ChannelSummary, ErrorCode, Event, Now, PreviousRequest,
    ProgressReport,
};

use crate::api::{Api, ApiError};
use crate::navigation::{Navigation, Resolved};
use crate::player::{LoadRequest, Player, PlayerError, PlayerEvent, SubtitleTrack};
use crate::ui::{Banner, Guide, UiEvent};

/// Tuning knobs. [`TunerConfig::default`] has the values from PLAN.md §12.
#[derive(Debug, Clone, PartialEq)]
pub struct TunerConfig {
    /// How far before the saved position to resume.
    pub resume_rewind: Duration,
    /// How often to report progress while playing.
    pub progress_interval: Duration,
    /// An item is finished when the saved position is within
    /// `max(finished_min, finished_fraction × duration)` of its end.
    pub finished_min: Duration,
    pub finished_fraction: f64,
    /// How many items in a row may fail before the tuner stops skipping
    /// ahead and waits for the viewer.
    pub max_consecutive_failures: u32,
    /// This client's ID, used to find the channel it watched last.
    pub client_id: Option<String>,
}

impl Default for TunerConfig {
    fn default() -> Self {
        Self {
            resume_rewind: Duration::from_secs(5),
            progress_interval: Duration::from_secs(5),
            finished_min: Duration::from_secs(30),
            finished_fraction: 0.02,
            max_consecutive_failures: 5,
            client_id: None,
        }
    }
}

impl TunerConfig {
    /// Whether a saved `position_ms` counts as finished (PLAN.md §1):
    /// within `max(30s, 2% of duration)` of the end. An item with no known
    /// duration is never finished, and neither is one at position 0, which
    /// keeps clips shorter than the threshold from being skipped forever.
    pub fn is_finished(&self, position_ms: u64, duration_ms: Option<u64>) -> bool {
        let Some(duration) = duration_ms.filter(|d| *d > 0) else {
            return false;
        };
        if position_ms == 0 {
            return false;
        }
        let min = u64::try_from(self.finished_min.as_millis()).unwrap_or(u64::MAX);
        // f64 holds every realistic duration in ms exactly.
        let fraction = (duration as f64 * self.finished_fraction) as u64;
        position_ms.saturating_add(min.max(fraction)) >= duration
    }

    /// Where to start playing an item saved at `position_ms`.
    pub fn resume_at(&self, position_ms: u64) -> u64 {
        let rewind = u64::try_from(self.resume_rewind.as_millis()).unwrap_or(u64::MAX);
        position_ms.saturating_sub(rewind)
    }
}

/// A source of the current time, so tests can control it.
pub trait Clock {
    fn now(&self) -> Instant;
}

/// The real clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Why a tuner call failed. The tuner stays usable after any of these.
#[derive(Debug, thiserror::Error)]
pub enum TunerError {
    #[error(transparent)]
    Api(#[from] ApiError),
    #[error(transparent)]
    Player(#[from] PlayerError),
    #[error("the server has no channels with anything to play")]
    NoChannels,
    #[error("there is no channel {0}")]
    NoSuchChannel(String),
}

/// What the caller should do after an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    /// The viewer quit; progress has been reported. Shut down.
    Quit,
}

/// What the tuner is doing between calls.
#[derive(Debug, Clone, PartialEq)]
pub enum State {
    /// Nothing tuned yet, or the last tune failed with nothing playing.
    Idle,
    Playing(Box<Playing>),
}

/// A channel being watched.
#[derive(Debug, Clone, PartialEq)]
pub struct Playing {
    /// The server's answer for the channel; `now.item` is what the player
    /// has loaded (or failed to).
    pub now: Now,
    /// The saved position this item was resumed from, while playback is
    /// still inside the resume rewind (see [`Tuner::report_progress`]).
    resume_floor: Option<u64>,
    last_report: Instant,
}

/// Receives the tuner's [`Event`]s.
pub type EventSink = Box<dyn FnMut(Event) + Send>;

/// Receives the tuner's [`UiEvent`]s.
pub type UiSink = Box<dyn FnMut(UiEvent) + Send>;

/// The Tuner state machine. See the module docs.
pub struct Tuner<A, P, C = SystemClock> {
    api: A,
    player: P,
    clock: C,
    config: TunerConfig,
    channels: Vec<ChannelSummary>,
    state: State,
    navigation: Navigation,
    events: EventSink,
    ui: UiSink,
    /// Items in a row that failed to load or play.
    failures: u32,
    /// In standby: whether waking should resume playback (it was playing
    /// when standby began). `None` when not in standby.
    standby: Option<bool>,
}

impl<A: Api, P: Player, C: Clock> Tuner<A, P, C> {
    pub fn new(api: A, player: P, clock: C, config: TunerConfig) -> Self {
        Self {
            api,
            player,
            clock,
            config,
            channels: Vec::new(),
            state: State::Idle,
            navigation: Navigation::default(),
            events: Box::new(|event| tracing::debug!(?event, "event")),
            ui: Box::new(|cue| tracing::debug!(?cue, "ui")),
            failures: 0,
            standby: None,
        }
    }

    /// Sends the tuner's events to `sink` instead of the debug log.
    pub fn with_event_sink(mut self, sink: impl FnMut(Event) + Send + 'static) -> Self {
        self.events = Box::new(sink);
        self
    }

    /// Sends the tuner's overlay cues to `sink` instead of the debug log.
    pub fn with_ui_sink(mut self, sink: impl FnMut(UiEvent) + Send + 'static) -> Self {
        self.ui = Box::new(sink);
        self
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    /// What is playing, if anything.
    pub fn now(&self) -> Option<&Now> {
        match &self.state {
            State::Playing(p) => Some(&p.now),
            State::Idle => None,
        }
    }

    /// The channel list as last fetched, by number.
    pub fn channels(&self) -> &[ChannelSummary] {
        &self.channels
    }

    /// What is open on screen (the guide), for the overlay.
    pub fn navigation(&self) -> &Navigation {
        &self.navigation
    }

    pub fn player(&self) -> &P {
        &self.player
    }

    pub fn player_mut(&mut self) -> &mut P {
        &mut self.player
    }

    /// Whether the client is in standby.
    pub fn in_standby(&self) -> bool {
        self.standby.is_some()
    }

    fn emit(&mut self, event: Event) {
        (self.events)(event);
    }

    fn cue(&mut self, cue: UiEvent) {
        (self.ui)(cue);
    }

    /// Tells the overlay what is on now (e.g. after a switch failed, so the
    /// banner does not keep naming a channel that never tuned in).
    fn cue_current(&mut self, announce: bool) {
        let cue = match self.now() {
            Some(now) => UiEvent::Playing {
                banner: Banner::from_now(now),
                announce,
            },
            None => UiEvent::Stopped,
        };
        self.cue(cue);
    }

    /// Tells the overlay the guide's state: its rows and highlight, or
    /// that it is closed.
    fn cue_guide(&mut self) {
        let guide = self.navigation.highlighted().map(|highlighted| {
            let current = self.now().map(|n| n.channel_id.as_str());
            Guide::new(&self.channels, highlighted, current)
        });
        self.cue(UiEvent::Guide(guide));
    }

    /// Fetches the channel list and tunes the first channel: channel
    /// number `channel` if given, otherwise the one this client watched
    /// last (from the server's history), otherwise the lowest-numbered
    /// channel with something to play.
    pub fn start(&mut self, channel: Option<u32>) -> Result<(), TunerError> {
        self.refresh_channels()?;
        let target = match channel {
            Some(number) => self
                .channels
                .iter()
                .find(|c| c.number == number)
                .ok_or_else(|| TunerError::NoSuchChannel(number.to_string()))?,
            None => match self.last_watched() {
                Some(c) => c,
                None => self
                    .channels
                    .iter()
                    .find(|c| c.item_count > 0)
                    .ok_or(TunerError::NoChannels)?,
            },
        };
        let id = target.id.clone();
        self.tune(&id)
    }

    /// The channel this client was last tuned to, per the server's history.
    /// Falls back to the most recent channel of any client.
    fn last_watched(&self) -> Option<&ChannelSummary> {
        let events = self
            .api
            .history(200)
            .inspect_err(|err| tracing::warn!(%err, "cannot read history"))
            .ok()?;
        let watchable = |id: &Option<String>| {
            id.as_ref().and_then(|id| {
                self.channels
                    .iter()
                    .find(|c| &c.id == id && c.item_count > 0)
            })
        };
        let mine = events.iter().find(|e| {
            e.client_id.is_some()
                && e.client_id == self.config.client_id
                && watchable(&e.channel_id).is_some()
        });
        let any = || events.iter().find(|e| watchable(&e.channel_id).is_some());
        mine.or_else(any).and_then(|e| watchable(&e.channel_id))
    }

    /// Re-fetches the channel list.
    pub fn refresh_channels(&mut self) -> Result<(), TunerError> {
        let mut channels = self.api.channels()?;
        channels.sort_by_key(|c| c.number);
        self.channels = channels;
        Ok(())
    }

    /// Handles one action from the bus. Actions this client does not
    /// support yet are logged and ignored.
    pub fn handle_action(&mut self, action: Action) -> Result<Flow, TunerError> {
        match action {
            Action::ChannelUp => self.step_channel(1)?,
            Action::ChannelDown => self.step_channel(-1)?,
            Action::GoToChannel { number } => {
                self.go_to_channel(|c| c.number == number, || number.to_string())?
            }
            Action::GoToChannelId { id } => {
                self.go_to_channel(|c| c.id == id, || id.clone())?;
            }
            Action::TogglePause => {
                let paused = !self.player.is_paused();
                self.set_paused(paused);
            }
            Action::Play => self.set_paused(false),
            Action::Pause => self.set_paused(true),
            Action::SeekRelative { seconds } => {
                if let State::Playing(playing) = &mut self.state {
                    // An explicit seek is where the viewer wants to be,
                    // even if it is behind the resume point.
                    playing.resume_floor = None;
                    self.player.seek_relative(seconds);
                }
            }
            Action::NextItem => self.skip(Direction::Next)?,
            Action::PreviousItem => self.skip(Direction::Previous)?,
            Action::Navigate { direction } => {
                match self.navigation.navigate(direction, &self.channels) {
                    Resolved::Action(action) => return self.handle_action(action),
                    Resolved::GuideMoved => self.cue_guide(),
                    Resolved::Nothing => {}
                }
            }
            Action::Select => {
                let guide_was_open = self.navigation.guide_open();
                let resolved = self.navigation.select(&self.channels);
                if guide_was_open {
                    // Selecting closes the guide, before any tuning.
                    self.cue_guide();
                }
                match resolved {
                    Resolved::Action(action) => return self.handle_action(action),
                    Resolved::GuideMoved | Resolved::Nothing => {}
                }
            }
            Action::Back => {
                if self.navigation.close() {
                    tracing::debug!("guide closed");
                    self.cue_guide();
                } else {
                    self.cue(UiEvent::Hide);
                }
            }
            Action::ShowInfo => {
                self.log_info();
                self.cue(UiEvent::ShowInfo);
            }
            Action::ToggleGuide => {
                if self.navigation.close() {
                    tracing::debug!("guide closed");
                } else {
                    if let Err(err) = self.refresh_channels() {
                        tracing::warn!(%err, "cannot refresh channels");
                    }
                    let current = self.now().map(|n| n.channel_id.clone());
                    self.navigation
                        .open_guide(&self.channels, current.as_deref());
                    tracing::debug!("guide opened");
                }
                self.cue_guide();
            }
            Action::ToggleSubtitles => self.player.toggle_subtitles(),
            // Window business, routed there by the app; nothing to do here.
            Action::ToggleFullscreen => {}
            Action::Standby => self.enter_standby(),
            Action::Wake => self.wake(),
            Action::Quit => {
                self.report_progress();
                self.player.stop();
                return Ok(Flow::Quit);
            }
        }
        Ok(Flow::Continue)
    }

    /// Handles a report from the player.
    pub fn handle_player_event(&mut self, event: PlayerEvent) -> Result<(), TunerError> {
        match event {
            PlayerEvent::Started => {
                self.failures = 0;
                Ok(())
            }
            PlayerEvent::Ended => self.advance_on_end(),
            PlayerEvent::Failed(reason) => match self.after_failure(&reason)? {
                Some(next) => self.play(next, false),
                None => Ok(()),
            },
        }
    }

    /// Reports progress if the interval has passed. Call by
    /// [`Tuner::next_deadline`].
    pub fn tick(&mut self) {
        if let Some(deadline) = self.next_deadline()
            && self.clock.now() >= deadline
        {
            self.report_progress();
        }
    }

    /// When [`Tuner::tick`] next has work to do: the next periodic progress
    /// report, while something is playing and not paused.
    pub fn next_deadline(&self) -> Option<Instant> {
        match &self.state {
            State::Playing(p) if !self.player.is_paused() => {
                Some(p.last_report + self.config.progress_interval)
            }
            _ => None,
        }
    }

    /// Tells the server where playback is. Called on the interval, on
    /// pause, before switching channels, and on quit. Failures are logged,
    /// not returned: a missed report only loses a few seconds.
    ///
    /// While playback is still inside the resume rewind (behind the
    /// position it was resumed from), the resume position is reported
    /// instead, so flipping through channels does not walk each one back
    /// by the rewind every time.
    pub fn report_progress(&mut self) {
        let State::Playing(playing) = &mut self.state else {
            return;
        };
        playing.last_report = self.clock.now();
        let Some(mut position_ms) = self.player.position_ms() else {
            // Still opening the stream: nothing new to report, and
            // reporting 0 would lose the saved position.
            return;
        };
        if let Some(floor) = playing.resume_floor {
            if position_ms < floor {
                position_ms = floor;
            } else {
                playing.resume_floor = None;
            }
        }
        let report = ProgressReport {
            item_id: playing.now.item.id.clone(),
            position_ms,
            duration_ms: self.player.duration_ms(),
            paused: self.player.is_paused(),
        };
        match self.api.progress(&playing.now.channel_id, &report) {
            Ok(()) => {}
            Err(err) if err.code() == Some(ErrorCode::StaleItem) => {
                // Another client moved the channel on. Keep watching; when
                // this item ends, `advance` (idempotent) catches us up.
                tracing::info!(channel = %playing.now.channel_id, "channel moved on elsewhere");
            }
            Err(err) => tracing::warn!(%err, "cannot report progress"),
        }
    }

    /// Goes quiet (the TV switched away or went to standby): pauses,
    /// reports progress, and hides the overlay. Remembers whether it was
    /// playing, so [`Tuner::wake`] resumes only what standby paused.
    fn enter_standby(&mut self) {
        if self.standby.is_some() {
            return;
        }
        let was_playing = matches!(self.state, State::Playing(_)) && !self.player.is_paused();
        tracing::info!(was_playing, "standby");
        if was_playing {
            self.set_paused(true);
        } else {
            self.report_progress();
        }
        self.standby = Some(was_playing);
        self.emit(Event::Standby);
        if self.navigation.close() {
            self.cue_guide();
        }
        self.cue(UiEvent::Hide);
    }

    /// Comes back from standby, resuming playback if standby paused it.
    fn wake(&mut self) {
        let Some(resume) = self.standby.take() else {
            return;
        };
        tracing::info!(resume, "wake");
        self.emit(Event::Wake);
        if resume {
            self.set_paused(false);
        }
    }

    /// Something started playback while in standby (the viewer pressed
    /// play, tuned, or skipped): standby is over.
    fn leave_standby(&mut self) {
        if self.standby.take().is_some() {
            tracing::info!("playback resumed; leaving standby");
            self.emit(Event::Wake);
        }
    }

    fn set_paused(&mut self, paused: bool) {
        if !matches!(self.state, State::Playing(_)) || self.player.is_paused() == paused {
            return;
        }
        if !paused {
            self.leave_standby();
        }
        let State::Playing(playing) = &self.state else {
            return;
        };
        let (channel_id, item_id) = (playing.now.channel_id.clone(), playing.now.item.id.clone());
        self.player.set_paused(paused);
        tracing::info!(paused, "pause");
        self.report_progress();
        let position_ms = self.player.position_ms().unwrap_or_default();
        self.emit(if paused {
            Event::Paused {
                channel_id,
                item_id,
                position_ms,
            }
        } else {
            Event::Resumed {
                channel_id,
                item_id,
                position_ms,
            }
        });
    }

    /// Tunes to `channel_id`, reporting progress on the current channel
    /// first. If the saved position counts as finished, advances instead
    /// of resuming. On failure to reach the server, whatever was playing
    /// keeps playing.
    fn tune(&mut self, channel_id: &str) -> Result<(), TunerError> {
        // Announce the switch before anything that waits on the network.
        if let Some(channel) = self.channels.iter().find(|c| c.id == channel_id) {
            self.cue(UiEvent::Tuning(Banner::from_summary(channel)));
        }
        self.report_progress();
        let result = self.tune_inner(channel_id);
        if result.is_err() {
            self.cue_current(true);
        }
        result
    }

    fn tune_inner(&mut self, channel_id: &str) -> Result<(), TunerError> {
        let mut now = self.api.now(channel_id)?;
        if self
            .config
            .is_finished(now.position_ms, now.item.duration_ms)
        {
            tracing::info!(
                channel = now.channel_number,
                item = %now.item.title,
                "saved position is at the end; advancing"
            );
            now = self.api.advance(
                channel_id,
                &AdvanceRequest {
                    from_item_id: now.item.id.clone(),
                    reason: AdvanceReason::Ended,
                },
            )?;
        }
        tracing::info!(
            channel = now.channel_number,
            name = %now.channel_name,
            item = %describe(&now),
            position = %format_ms(now.position_ms),
            "tuned in"
        );
        self.emit(Event::ChannelChanged {
            channel_id: now.channel_id.clone(),
            channel_number: now.channel_number,
            channel_name: now.channel_name.clone(),
        });
        // A fresh choice by the viewer gets a fresh set of retries.
        self.failures = 0;
        self.play(now, true)
    }

    /// Loads `now` into the player, resuming with the rewind. If the player
    /// cannot load it, skips ahead (see [`Tuner::after_failure`]).
    /// `announce` says whether the overlay should show the banner for it.
    fn play(&mut self, mut now: Now, announce: bool) -> Result<(), TunerError> {
        loop {
            let start_ms = self.config.resume_at(now.position_ms);
            let loaded = self.player.load(&LoadRequest {
                url: now.stream_url.clone(),
                start_ms,
                subtitles: subtitle_tracks(&now.item),
            });
            let event = Event::NowPlaying {
                channel_id: now.channel_id.clone(),
                channel_number: now.channel_number,
                item_id: now.item.id.clone(),
                title: describe(&now),
                position_ms: start_ms,
            };
            self.state = State::Playing(Box::new(Playing {
                resume_floor: (now.position_ms > start_ms).then_some(now.position_ms),
                now,
                last_report: self.clock.now(),
            }));
            match loaded {
                Ok(()) => {
                    // A load starts playback, which ends standby.
                    self.leave_standby();
                    self.emit(event);
                    self.cue_current(announce);
                    return Ok(());
                }
                Err(err) => match self.after_failure(&err.to_string())? {
                    Some(next) => now = next,
                    None => return Ok(()),
                },
            }
        }
    }

    /// The current item failed to load or play: report it and skip ahead
    /// ("keeps going"). Returns the item to try next, or `None` once too
    /// many items in a row have failed, in which case playback stops and
    /// the viewer is told.
    fn after_failure(&mut self, reason: &str) -> Result<Option<Now>, TunerError> {
        let State::Playing(playing) = &self.state else {
            return Ok(None);
        };
        let now = &playing.now;
        let (channel_id, item_id) = (now.channel_id.clone(), now.item.id.clone());
        let message = format!(
            "cannot play {} on channel {}: {reason}",
            now.item.path, now.channel_number
        );
        let number = now.channel_number;
        self.failures += 1;
        tracing::warn!(%message, failures = self.failures, "skipping");
        self.emit(Event::Error { message });

        if self.failures >= self.config.max_consecutive_failures {
            let message = format!(
                "channel {number}: {} items in a row failed to play; stopped trying. \
                 Change channel or skip to try again",
                self.failures
            );
            tracing::error!(%message);
            self.emit(Event::Error { message });
            self.player.stop();
            self.cue(UiEvent::Stopped);
            return Ok(None);
        }
        let next = self.api.advance(
            &channel_id,
            &AdvanceRequest {
                from_item_id: item_id,
                reason: AdvanceReason::Skipped,
            },
        )?;
        tracing::info!(
            channel = next.channel_number,
            item = %describe(&next),
            "skipped ahead"
        );
        Ok(Some(next))
    }

    /// The player finished the current item: advance the channel. The
    /// prefetched next item is loaded before asking the server, so the gap
    /// between items is only the time to open the stream.
    fn advance_on_end(&mut self) -> Result<(), TunerError> {
        let State::Playing(playing) = &self.state else {
            return Ok(());
        };
        let channel_id = playing.now.channel_id.clone();
        let from_item_id = playing.now.item.id.clone();
        let ended = Event::ItemEnded {
            channel_id: channel_id.clone(),
            item_id: from_item_id.clone(),
        };
        let prefetch = playing.now.next.as_ref().map(|next| Now {
            item: next.item.clone(),
            position_ms: 0,
            stream_url: next.stream_url.clone(),
            next: None,
            ..playing.now.clone()
        });
        self.emit(ended);
        // It played, so the failure streak is over.
        self.failures = 0;
        let preloaded = match prefetch {
            Some(next) => {
                let loaded = self.player.load(&LoadRequest {
                    url: next.stream_url.clone(),
                    start_ms: 0,
                    subtitles: subtitle_tracks(&next.item),
                });
                match loaded {
                    Ok(()) => Some(next),
                    Err(err) => {
                        tracing::warn!(%err, "cannot preload the next item");
                        None
                    }
                }
            }
            None => None,
        };
        let request = AdvanceRequest {
            from_item_id,
            reason: AdvanceReason::Ended,
        };
        let now = match self.api.advance(&channel_id, &request) {
            Ok(now) => now,
            Err(err) => {
                // The next item is already playing. Track it, so progress
                // reports name it (the server rejects them as stale instead
                // of saving them against the finished item); the next
                // `advance` resynchronizes.
                if let Some(next) = preloaded {
                    self.state = State::Playing(Box::new(Playing {
                        now: next,
                        resume_floor: None,
                        last_report: self.clock.now(),
                    }));
                }
                return Err(err.into());
            }
        };
        tracing::info!(
            channel = now.channel_number,
            name = %now.channel_name,
            item = %describe(&now),
            "advanced"
        );
        // The server's answer wins. It differs from the prefetch only if
        // another client moved the channel meanwhile, in which case it may
        // also have a saved position worth resuming.
        let rewind_ms = u64::try_from(self.config.resume_rewind.as_millis()).unwrap_or(u64::MAX);
        if preloaded.map(|n| n.item.id).as_ref() == Some(&now.item.id)
            && now.position_ms <= rewind_ms
        {
            self.emit(Event::NowPlaying {
                channel_id: now.channel_id.clone(),
                channel_number: now.channel_number,
                item_id: now.item.id.clone(),
                title: describe(&now),
                position_ms: 0,
            });
            self.state = State::Playing(Box::new(Playing {
                now,
                resume_floor: None,
                last_report: self.clock.now(),
            }));
            self.cue_current(false);
            Ok(())
        } else {
            self.play(now, false)
        }
    }

    /// Skips forward or back within the current channel.
    fn skip(&mut self, direction: Direction) -> Result<(), TunerError> {
        let State::Playing(playing) = &self.state else {
            return Ok(());
        };
        let channel_id = playing.now.channel_id.clone();
        let from_item_id = playing.now.item.id.clone();
        let now = match direction {
            Direction::Next => self.api.advance(
                &channel_id,
                &AdvanceRequest {
                    from_item_id,
                    reason: AdvanceReason::Skipped,
                },
            )?,
            Direction::Previous => self
                .api
                .previous(&channel_id, &PreviousRequest { from_item_id })?,
        };
        tracing::info!(
            channel = now.channel_number,
            item = %describe(&now),
            ?direction,
            "skipped"
        );
        self.failures = 0;
        self.play(now, true)
    }

    /// Tunes `step` channels up or down from the current one, skipping
    /// channels with nothing to play and wrapping around.
    fn step_channel(&mut self, step: i32) -> Result<(), TunerError> {
        if self.channels.is_empty() {
            self.refresh_channels()?;
        }
        let watchable: Vec<&ChannelSummary> =
            self.channels.iter().filter(|c| c.item_count > 0).collect();
        if watchable.is_empty() {
            return Err(TunerError::NoChannels);
        }
        let current = self.now().map(|n| n.channel_number);
        let target = match current {
            None => watchable[0],
            Some(number) if step > 0 => watchable
                .iter()
                .find(|c| c.number > number)
                .unwrap_or(&watchable[0]),
            Some(number) => watchable
                .iter()
                .rev()
                .find(|c| c.number < number)
                .unwrap_or(&watchable[watchable.len() - 1]),
        };
        let id = target.id.clone();
        self.tune(&id)
    }

    /// Tunes to the channel matching `wanted`, refreshing the channel list
    /// once if none does. `name` describes the channel for errors.
    fn go_to_channel(
        &mut self,
        wanted: impl Fn(&ChannelSummary) -> bool,
        name: impl Fn() -> String,
    ) -> Result<(), TunerError> {
        let find =
            |channels: &[ChannelSummary]| channels.iter().find(|c| wanted(c)).map(|c| c.id.clone());
        let id = match find(&self.channels) {
            Some(id) => id,
            None => {
                // Channels may have been added to the config since.
                self.refresh_channels()?;
                find(&self.channels).ok_or_else(|| TunerError::NoSuchChannel(name()))?
            }
        };
        if self.now().map(|n| &n.channel_id) == Some(&id) {
            return Ok(());
        }
        self.tune(&id)
    }

    fn log_info(&self) {
        match self.now() {
            Some(now) => tracing::info!(
                channel = now.channel_number,
                name = %now.channel_name,
                item = %describe(now),
                position = %self.player.position_ms().map(format_ms).unwrap_or_default(),
                duration = %self.player.duration_ms().map(format_ms).unwrap_or_default(),
                paused = self.player.is_paused(),
                "now playing"
            ),
            None => tracing::info!("nothing playing"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Direction {
    Next,
    Previous,
}

fn subtitle_tracks(item: &marathon_api::Item) -> Vec<SubtitleTrack> {
    item.subtitles
        .iter()
        .map(|s| SubtitleTrack {
            url: s.url.clone(),
            title: s.label.clone(),
            language: s.language.clone(),
        })
        .collect()
}

/// A one-line description of the current item, e.g.
/// `Show · S03E14 · The Injury`.
pub fn describe(now: &Now) -> String {
    let item = &now.item;
    let mut parts = Vec::new();
    if let Some(show) = &item.show {
        parts.push(show.clone());
    }
    match (item.season, item.episode) {
        (Some(s), Some(e)) => parts.push(format!("S{s:02}E{e:02}")),
        (None, Some(e)) => parts.push(format!("E{e:02}")),
        _ => {}
    }
    parts.push(item.title.clone());
    parts.join(" · ")
}

/// Formats milliseconds as `h:mm:ss` or `m:ss`.
pub fn format_ms(ms: u64) -> String {
    let s = ms / 1000;
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

#[cfg(test)]
mod tests;
