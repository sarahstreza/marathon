//! Tuner scenarios against an in-memory fake server and a mock player
//! (PLAN.md M3). Both record into one shared journal, so tests can assert
//! the order of API calls and player commands.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use marathon_api::{ChannelRef, Direction, EventKind, HistoryEntry, Item, NextItem, Subtitle};

use super::*;
use crate::ui::GuideRow;

const MIN: u64 = 60_000;

/// One API call or player command, in the order it happened.
#[derive(Debug, Clone, PartialEq)]
enum Entry {
    Channels,
    Now(String),
    Progress(String, ProgressReport),
    Advance(String, String, AdvanceReason),
    Previous(String, String),
    History,
    Load(LoadRequest),
    Stop,
    Pause(bool),
    Seek(i32),
    ToggleSubtitles,
}

type Journal = Rc<RefCell<Vec<Entry>>>;

fn item(id: &str, duration_ms: Option<u64>) -> Item {
    Item {
        id: id.to_owned(),
        source_id: "lib".into(),
        path: format!("{id}.mkv"),
        show: None,
        season: None,
        episode: None,
        title: id.to_owned(),
        duration_ms,
        subtitles: Vec::new(),
        missing: false,
    }
}

fn url(id: &str) -> String {
    format!("http://server/api/v1/stream/{id}")
}

struct FakeChannel {
    id: String,
    number: u32,
    items: Vec<Item>,
    current: usize,
    position_ms: u64,
    /// Hidden channels are left out of `channels`, as the server does.
    hidden: bool,
    /// Child channels the items come through (PLAN.md M11).
    via: Vec<ChannelRef>,
    /// What `now` and `advance` report the server skipped.
    warnings: Vec<String>,
}

/// Mimics the server: ordered channels that loop, idempotent advance and
/// previous, and stale progress rejected with 409.
#[derive(Default)]
struct FakeServer {
    channels: Vec<FakeChannel>,
    history: Vec<HistoryEntry>,
    /// Makes `advance` fail as if the server were unreachable.
    fail_advance: bool,
}

impl FakeServer {
    fn channel(&mut self, id: &str) -> Result<&mut FakeChannel, ApiError> {
        self.channels
            .iter_mut()
            .find(|c| c.id == id)
            .ok_or_else(|| ApiError::Server {
                status: 404,
                code: Some(ErrorCode::NotFound),
                message: format!("no channel {id}"),
            })
    }
}

impl FakeChannel {
    fn now(&self) -> Now {
        let item = self.items[self.current].clone();
        let next = self.items[(self.current + 1) % self.items.len()].clone();
        Now {
            channel_id: self.id.clone(),
            channel_number: self.number,
            channel_name: format!("Channel {}", self.number),
            stream_url: url(&item.id),
            position_ms: self.position_ms,
            item,
            next: Some(NextItem {
                stream_url: url(&next.id),
                item: next,
            }),
            resolution_path: std::iter::once(ChannelRef {
                id: self.id.clone(),
                name: format!("Channel {}", self.number),
            })
            .chain(self.via.iter().cloned())
            .collect(),
            warnings: self.warnings.clone(),
        }
    }

    fn current_id(&self) -> &str {
        &self.items[self.current].id
    }
}

#[derive(Clone)]
struct FakeApi {
    server: Rc<RefCell<FakeServer>>,
    journal: Journal,
    /// API calls so far; shared with the UI sink, which must be `Send`.
    calls: Arc<AtomicUsize>,
}

impl FakeApi {
    fn log(&self, entry: Entry) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.journal.borrow_mut().push(entry);
    }
}

impl Api for FakeApi {
    fn channels(&self) -> Result<Vec<ChannelSummary>, ApiError> {
        self.log(Entry::Channels);
        Ok(self
            .server
            .borrow()
            .channels
            .iter()
            .filter(|c| !c.hidden)
            .map(|c| ChannelSummary {
                id: c.id.clone(),
                number: c.number,
                name: format!("Channel {}", c.number),
                item_count: c.items.len() as u32,
                current: c.items.get(c.current).cloned(),
                position_ms: c.position_ms,
                hidden: false,
                resolution_path: Vec::new(),
            })
            .collect())
    }

    fn now(&self, channel_id: &str) -> Result<Now, ApiError> {
        self.log(Entry::Now(channel_id.into()));
        let mut server = self.server.borrow_mut();
        let c = server.channel(channel_id)?;
        if c.items.is_empty() {
            return Err(ApiError::Server {
                status: 409,
                code: Some(ErrorCode::ChannelEmpty),
                message: "empty".into(),
            });
        }
        Ok(c.now())
    }

    fn progress(&self, channel_id: &str, report: &ProgressReport) -> Result<(), ApiError> {
        self.log(Entry::Progress(channel_id.into(), report.clone()));
        let mut server = self.server.borrow_mut();
        let c = server.channel(channel_id)?;
        if c.current_id() != report.item_id {
            return Err(ApiError::Server {
                status: 409,
                code: Some(ErrorCode::StaleItem),
                message: "stale".into(),
            });
        }
        c.position_ms = report.position_ms;
        Ok(())
    }

    fn advance(&self, channel_id: &str, request: &AdvanceRequest) -> Result<Now, ApiError> {
        self.log(Entry::Advance(
            channel_id.into(),
            request.from_item_id.clone(),
            request.reason,
        ));
        let mut server = self.server.borrow_mut();
        if server.fail_advance {
            return Err(ApiError::Transport("connection refused".into()));
        }
        let c = server.channel(channel_id)?;
        if c.current_id() == request.from_item_id {
            c.current = (c.current + 1) % c.items.len();
            c.position_ms = 0;
        }
        Ok(c.now())
    }

    fn previous(&self, channel_id: &str, request: &PreviousRequest) -> Result<Now, ApiError> {
        self.log(Entry::Previous(
            channel_id.into(),
            request.from_item_id.clone(),
        ));
        let mut server = self.server.borrow_mut();
        let c = server.channel(channel_id)?;
        if c.current_id() == request.from_item_id && c.current > 0 {
            c.current -= 1;
            c.position_ms = 0;
        }
        Ok(c.now())
    }

    fn history(&self, limit: u32) -> Result<Vec<HistoryEntry>, ApiError> {
        self.log(Entry::History);
        let server = self.server.borrow();
        Ok(server
            .history
            .iter()
            .take(limit as usize)
            .cloned()
            .collect())
    }
}

/// Records commands; position, duration, and pause state are set by tests.
struct MockPlayer {
    journal: Journal,
    /// Item IDs whose loads fail.
    broken: Rc<RefCell<HashSet<String>>>,
    position_ms: Rc<Cell<Option<u64>>>,
    duration_ms: Rc<Cell<Option<u64>>>,
    paused: bool,
}

impl Player for MockPlayer {
    fn load(&mut self, request: &LoadRequest) -> Result<(), PlayerError> {
        self.journal.borrow_mut().push(Entry::Load(request.clone()));
        if self.broken.borrow().iter().any(|id| request.url == url(id)) {
            self.position_ms.set(None);
            return Err(PlayerError("no such file".into()));
        }
        self.paused = false;
        self.position_ms.set(Some(request.start_ms));
        Ok(())
    }
    fn stop(&mut self) {
        self.journal.borrow_mut().push(Entry::Stop);
    }
    fn set_paused(&mut self, paused: bool) {
        self.journal.borrow_mut().push(Entry::Pause(paused));
        self.paused = paused;
    }
    fn is_paused(&self) -> bool {
        self.paused
    }
    fn seek_relative(&mut self, seconds: i32) {
        self.journal.borrow_mut().push(Entry::Seek(seconds));
        if let Some(p) = self.position_ms.get() {
            let ms = i64::try_from(p).unwrap() + i64::from(seconds) * 1000;
            self.position_ms
                .set(Some(u64::try_from(ms.max(0)).unwrap()));
        }
    }
    fn position_ms(&self) -> Option<u64> {
        self.position_ms.get()
    }
    fn duration_ms(&self) -> Option<u64> {
        self.duration_ms.get()
    }
    fn toggle_subtitles(&mut self) {
        self.journal.borrow_mut().push(Entry::ToggleSubtitles);
    }
}

#[derive(Clone)]
struct TestClock(Rc<Cell<Instant>>);

impl Clock for TestClock {
    fn now(&self) -> Instant {
        self.0.get()
    }
}

/// A tuner over the fake server, plus handles to steer and inspect it.
struct Harness {
    tuner: Tuner<FakeApi, MockPlayer, TestClock>,
    server: Rc<RefCell<FakeServer>>,
    journal: Journal,
    position: Rc<Cell<Option<u64>>>,
    clock: Rc<Cell<Instant>>,
    broken: Rc<RefCell<HashSet<String>>>,
    events: Arc<Mutex<Vec<Event>>>,
    /// UI cues, each with the number of API calls made before it.
    ui: Arc<Mutex<Vec<(usize, UiEvent)>>>,
}

impl Harness {
    /// Channels 1..=n, each with `items` items (`c<n>-<i>`) of 20 minutes.
    fn new(channels: u32, items: usize) -> Self {
        let server = FakeServer {
            channels: (1..=channels)
                .map(|n| FakeChannel {
                    id: format!("c{n}"),
                    number: n,
                    items: (0..items)
                        .map(|i| item(&format!("c{n}-{i}"), Some(20 * MIN)))
                        .collect(),
                    current: 0,
                    position_ms: 0,
                    hidden: false,
                    via: Vec::new(),
                    warnings: Vec::new(),
                })
                .collect(),
            history: Vec::new(),
            fail_advance: false,
        };
        let server = Rc::new(RefCell::new(server));
        let journal = Journal::default();
        let position = Rc::new(Cell::new(None));
        let clock = Rc::new(Cell::new(Instant::now()));
        let broken = Rc::new(RefCell::new(HashSet::new()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let player = MockPlayer {
            journal: journal.clone(),
            broken: broken.clone(),
            position_ms: position.clone(),
            duration_ms: Rc::new(Cell::new(Some(20 * MIN))),
            paused: false,
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let api = FakeApi {
            server: server.clone(),
            journal: journal.clone(),
            calls: calls.clone(),
        };
        let config = TunerConfig {
            client_id: Some("me".into()),
            ..TunerConfig::default()
        };
        let sink = events.clone();
        let ui = Arc::new(Mutex::new(Vec::new()));
        let ui_sink = ui.clone();
        let tuner = Tuner::new(api, player, TestClock(clock.clone()), config)
            .with_event_sink(move |e| sink.lock().unwrap().push(e))
            .with_ui_sink(move |cue| {
                let calls = calls.load(Ordering::SeqCst);
                ui_sink.lock().unwrap().push((calls, cue));
            });
        Self {
            tuner,
            server,
            journal,
            position,
            clock,
            broken,
            events,
            ui,
        }
    }

    /// Takes the UI cues so far.
    fn take_ui(&self) -> Vec<UiEvent> {
        std::mem::take(&mut *self.ui.lock().unwrap())
            .into_iter()
            .map(|(_, cue)| cue)
            .collect()
    }

    fn set_saved(&self, channel: &str, current: usize, position_ms: u64) {
        let mut server = self.server.borrow_mut();
        let c = server.channel(channel).unwrap();
        c.current = current;
        c.position_ms = position_ms;
    }

    fn saved(&self, channel: &str) -> (String, u64) {
        let mut server = self.server.borrow_mut();
        let c = server.channel(channel).unwrap();
        (c.current_id().to_owned(), c.position_ms)
    }

    fn advance_clock(&self, by: Duration) {
        self.clock.set(self.clock.get() + by);
    }

    /// Takes the events emitted so far.
    fn take_events(&self) -> Vec<Event> {
        std::mem::take(&mut *self.events.lock().unwrap())
    }

    /// Takes the journal so far.
    fn take(&self) -> Vec<Entry> {
        std::mem::take(&mut *self.journal.borrow_mut())
    }

    fn action(&mut self, action: Action) -> Flow {
        self.tuner.handle_action(action).unwrap()
    }

    fn playing_item(&self) -> &str {
        &self.tuner.now().unwrap().item.id
    }

    fn progress_reports(entries: &[Entry]) -> Vec<(String, ProgressReport)> {
        entries
            .iter()
            .filter_map(|e| match e {
                Entry::Progress(c, r) => Some((c.clone(), r.clone())),
                _ => None,
            })
            .collect()
    }
}

fn load(id: &str, start_ms: u64) -> Entry {
    Entry::Load(LoadRequest {
        url: url(id),
        start_ms,
        subtitles: Vec::new(),
    })
}

#[test]
fn tune_plays_at_saved_position_minus_rewind() {
    let mut h = Harness::new(2, 3);
    h.set_saved("c1", 1, 10 * MIN);
    h.tuner.start(Some(1)).unwrap();
    let entries = h.take();
    assert!(entries.contains(&Entry::Now("c1".into())));
    assert_eq!(entries.last(), Some(&load("c1-1", 10 * MIN - 5_000)));
    assert_eq!(h.playing_item(), "c1-1");
}

#[test]
fn rewind_never_goes_before_the_start() {
    let mut h = Harness::new(1, 2);
    h.set_saved("c1", 0, 3_000);
    h.tuner.start(Some(1)).unwrap();
    assert_eq!(h.take().last(), Some(&load("c1-0", 0)));
}

#[test]
fn tune_in_within_finished_threshold_advances_instead_of_resuming() {
    let mut h = Harness::new(1, 3);
    // 20-minute item: threshold is max(30s, 24s) = 30s. 20s left.
    h.set_saved("c1", 0, 20 * MIN - 20_000);
    h.tuner.start(Some(1)).unwrap();
    let entries = h.take();
    let advance = Entry::Advance("c1".into(), "c1-0".into(), AdvanceReason::Ended);
    assert!(entries.contains(&advance), "{entries:?}");
    assert_eq!(entries.last(), Some(&load("c1-1", 0)));
    assert_eq!(h.saved("c1"), ("c1-1".into(), 0));
}

#[test]
fn tune_in_just_outside_finished_threshold_resumes() {
    let mut h = Harness::new(1, 3);
    h.set_saved("c1", 0, 20 * MIN - 31_000);
    h.tuner.start(Some(1)).unwrap();
    assert_eq!(h.take().last(), Some(&load("c1-0", 20 * MIN - 36_000)));
}

#[test]
fn finished_threshold_is_the_larger_of_30s_and_2_percent() {
    let config = TunerConfig::default();
    let hour = 60 * MIN;
    // 2% of an hour is 72s.
    assert!(config.is_finished(hour - 72_000, Some(hour)));
    assert!(!config.is_finished(hour - 73_000, Some(hour)));
    // For 10 minutes 2% is 12s, so 30s applies.
    assert!(config.is_finished(10 * MIN - 30_000, Some(10 * MIN)));
    assert!(!config.is_finished(10 * MIN - 31_000, Some(10 * MIN)));
    // Past the end counts too.
    assert!(config.is_finished(hour + 1, Some(hour)));
    // Unknown duration, or not started: never finished, even for clips
    // shorter than the threshold.
    assert!(!config.is_finished(hour, None));
    assert!(!config.is_finished(0, Some(6_000)));
    assert!(config.is_finished(1, Some(6_000)));
}

#[test]
fn end_of_file_advances_and_plays_the_next_item() {
    let mut h = Harness::new(1, 3);
    h.tuner.start(Some(1)).unwrap();
    h.take();
    h.tuner.handle_player_event(PlayerEvent::Ended).unwrap();
    let entries = h.take();
    // The prefetched next item is loaded first, then the server is told.
    assert_eq!(
        entries,
        vec![
            load("c1-1", 0),
            Entry::Advance("c1".into(), "c1-0".into(), AdvanceReason::Ended),
        ]
    );
    assert_eq!(h.playing_item(), "c1-1");
    assert_eq!(h.saved("c1").0, "c1-1");

    // And again, using the new `next`.
    h.tuner.handle_player_event(PlayerEvent::Ended).unwrap();
    assert_eq!(h.take()[0], load("c1-2", 0));
    assert_eq!(h.playing_item(), "c1-2");
}

#[test]
fn end_of_file_follows_the_server_if_another_client_moved_the_channel() {
    let mut h = Harness::new(1, 4);
    h.tuner.start(Some(1)).unwrap();
    // Another client advanced twice and watched a minute of item 2.
    h.set_saved("c1", 2, MIN);
    h.take();
    h.tuner.handle_player_event(PlayerEvent::Ended).unwrap();
    let entries = h.take();
    assert_eq!(entries[0], load("c1-1", 0)); // the stale prefetch
    assert_eq!(entries.last(), Some(&load("c1-2", MIN - 5_000)));
    assert_eq!(h.playing_item(), "c1-2");
}

#[test]
fn failed_advance_at_end_does_not_save_progress_against_the_old_item() {
    let mut h = Harness::new(1, 3);
    h.set_saved("c1", 0, 19 * MIN);
    h.tuner.start(Some(1)).unwrap();
    h.server.borrow_mut().fail_advance = true;
    let err = h.tuner.handle_player_event(PlayerEvent::Ended).unwrap_err();
    assert!(matches!(err, TunerError::Api(ApiError::Transport(_))));
    // The prefetched item plays and is what the tuner tracks...
    assert_eq!(h.playing_item(), "c1-1");
    // ...so its progress is rejected as stale rather than overwriting the
    // finished item's position.
    h.position.set(Some(MIN));
    h.action(Action::Quit);
    assert_eq!(h.saved("c1"), ("c1-0".into(), 19 * MIN));

    // Once the server is back, the next end resynchronizes.
    h.server.borrow_mut().fail_advance = false;
    h.tuner.handle_player_event(PlayerEvent::Ended).unwrap();
    assert_eq!(h.playing_item(), "c1-0");
}

#[test]
fn channel_switch_reports_progress_before_tuning_away() {
    let mut h = Harness::new(3, 2);
    h.tuner.start(Some(1)).unwrap();
    h.position.set(Some(7 * MIN));
    h.take();

    assert_eq!(h.action(Action::ChannelUp), Flow::Continue);
    let entries = h.take();
    let report = Entry::Progress(
        "c1".into(),
        ProgressReport {
            item_id: "c1-0".into(),
            position_ms: 7 * MIN,
            duration_ms: Some(20 * MIN),
            paused: false,
        },
    );
    let report_at = entries.iter().position(|e| *e == report).unwrap();
    let now_at = entries
        .iter()
        .position(|e| *e == Entry::Now("c2".into()))
        .unwrap();
    assert!(report_at < now_at, "{entries:?}");
    assert_eq!(entries.last(), Some(&load("c2-0", 0)));
    assert_eq!(h.saved("c1"), ("c1-0".into(), 7 * MIN));

    // Coming back resumes where we left off, minus the rewind.
    h.action(Action::ChannelDown);
    assert_eq!(h.take().last(), Some(&load("c1-0", 7 * MIN - 5_000)));
}

#[test]
fn progress_is_reported_on_the_interval() {
    let mut h = Harness::new(1, 2);
    h.tuner.start(Some(1)).unwrap();
    h.take();
    let interval = Duration::from_secs(5);
    assert_eq!(
        h.tuner.next_deadline(),
        Some(h.clock.get() + interval),
        "first report is due one interval after tuning"
    );

    h.advance_clock(interval - Duration::from_millis(1));
    h.position.set(Some(4_999));
    h.tuner.tick();
    assert_eq!(h.take(), vec![], "not due yet");

    h.advance_clock(Duration::from_millis(1));
    h.position.set(Some(5_000));
    h.tuner.tick();
    let reports = Harness::progress_reports(&h.take());
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].1.position_ms, 5_000);
    assert!(!reports[0].1.paused);
    assert_eq!(h.tuner.next_deadline(), Some(h.clock.get() + interval));

    h.advance_clock(interval);
    h.position.set(Some(10_000));
    h.tuner.tick();
    let reports = Harness::progress_reports(&h.take());
    assert_eq!(reports.len(), 1);
    assert_eq!(h.saved("c1").1, 10_000);
}

#[test]
fn progress_is_reported_immediately_on_pause_and_unpause() {
    let mut h = Harness::new(1, 2);
    h.tuner.start(Some(1)).unwrap();
    h.position.set(Some(MIN));
    h.take();

    h.action(Action::TogglePause);
    let entries = h.take();
    assert_eq!(entries[0], Entry::Pause(true));
    let reports = Harness::progress_reports(&entries);
    assert_eq!(reports.len(), 1);
    assert!(reports[0].1.paused);
    assert_eq!(reports[0].1.position_ms, MIN);

    // No periodic reports while paused.
    assert_eq!(h.tuner.next_deadline(), None);
    h.advance_clock(Duration::from_secs(60));
    h.tuner.tick();
    assert_eq!(h.take(), vec![]);

    h.action(Action::TogglePause);
    let entries = h.take();
    assert_eq!(entries[0], Entry::Pause(false));
    let reports = Harness::progress_reports(&entries);
    assert_eq!(reports.len(), 1);
    assert!(!reports[0].1.paused);
}

#[test]
fn progress_is_reported_on_quit() {
    let mut h = Harness::new(1, 2);
    h.tuner.start(Some(1)).unwrap();
    h.position.set(Some(3 * MIN));
    h.take();
    assert_eq!(h.action(Action::Quit), Flow::Quit);
    let entries = h.take();
    let reports = Harness::progress_reports(&entries);
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].1.position_ms, 3 * MIN);
    assert_eq!(entries.last(), Some(&Entry::Stop));
    assert_eq!(h.saved("c1").1, 3 * MIN);
}

#[test]
fn quick_flips_do_not_walk_the_position_back() {
    let mut h = Harness::new(2, 2);
    h.set_saved("c1", 0, 10 * MIN);
    h.tuner.start(Some(1)).unwrap();
    // Two seconds after resuming 5s early: still behind the saved spot.
    h.position.set(Some(10 * MIN - 3_000));
    h.action(Action::ChannelUp);
    assert_eq!(h.saved("c1").1, 10 * MIN);

    // Once playback passes the saved position, the real position counts.
    h.action(Action::ChannelDown);
    h.position.set(Some(10 * MIN + 2_000));
    h.action(Action::ChannelUp);
    assert_eq!(h.saved("c1").1, 10 * MIN + 2_000);
}

#[test]
fn seeking_back_is_respected_even_inside_the_rewind() {
    let mut h = Harness::new(1, 2);
    h.set_saved("c1", 0, 10 * MIN);
    h.tuner.start(Some(1)).unwrap();
    h.action(Action::SeekRelative { seconds: -10 });
    h.action(Action::Quit);
    assert_eq!(h.saved("c1").1, 10 * MIN - 15_000);
}

#[test]
fn position_unknown_while_loading_is_not_reported_as_zero() {
    let mut h = Harness::new(2, 2);
    h.set_saved("c1", 0, 10 * MIN);
    h.tuner.start(Some(1)).unwrap();
    h.position.set(None);
    h.action(Action::ChannelUp);
    assert_eq!(h.saved("c1").1, 10 * MIN);
}

#[test]
fn next_and_previous_items() {
    let mut h = Harness::new(1, 3);
    h.tuner.start(Some(1)).unwrap();
    h.take();
    h.action(Action::NextItem);
    let entries = h.take();
    assert_eq!(
        entries[0],
        Entry::Advance("c1".into(), "c1-0".into(), AdvanceReason::Skipped)
    );
    assert_eq!(entries.last(), Some(&load("c1-1", 0)));

    h.action(Action::PreviousItem);
    let entries = h.take();
    assert_eq!(entries[0], Entry::Previous("c1".into(), "c1-1".into()));
    assert_eq!(entries.last(), Some(&load("c1-0", 0)));
    assert_eq!(h.playing_item(), "c1-0");
}

#[test]
fn stale_progress_is_ignored_and_end_catches_up() {
    let mut h = Harness::new(1, 4);
    h.tuner.start(Some(1)).unwrap();
    // Another client moves the channel to item 2.
    h.set_saved("c1", 2, 0);
    h.position.set(Some(MIN));
    h.advance_clock(Duration::from_secs(5));
    h.tuner.tick(); // rejected as stale; must not error or stop playback
    assert_eq!(h.playing_item(), "c1-0");
    assert_eq!(h.saved("c1"), ("c1-2".into(), 0));
    h.tuner.handle_player_event(PlayerEvent::Ended).unwrap();
    assert_eq!(h.playing_item(), "c1-2");
}

#[test]
fn channel_up_and_down_wrap_and_skip_empty_channels() {
    let mut h = Harness::new(4, 2);
    h.server.borrow_mut().channels[2].items.clear(); // channel 3 is empty
    h.tuner.start(Some(1)).unwrap();
    let mut seen = Vec::new();
    for _ in 0..4 {
        h.action(Action::ChannelUp);
        seen.push(h.tuner.now().unwrap().channel_number);
    }
    assert_eq!(seen, vec![2, 4, 1, 2]);
    seen.clear();
    for _ in 0..3 {
        h.action(Action::ChannelDown);
        seen.push(h.tuner.now().unwrap().channel_number);
    }
    assert_eq!(seen, vec![1, 4, 2]);
}

#[test]
fn go_to_channel_by_number() {
    let mut h = Harness::new(3, 2);
    h.tuner.start(Some(1)).unwrap();
    h.action(Action::GoToChannel { number: 3 });
    assert_eq!(h.tuner.now().unwrap().channel_id, "c3");
    h.take();
    // Same channel: nothing happens.
    h.action(Action::GoToChannel { number: 3 });
    assert_eq!(h.take(), vec![]);
    // Unknown channel: refreshes the list, then reports the error.
    let err = h
        .tuner
        .handle_action(Action::GoToChannel { number: 9 })
        .unwrap_err();
    assert!(matches!(err, TunerError::NoSuchChannel(ref n) if n == "9"));
    assert_eq!(h.take(), vec![Entry::Channels]);
    assert_eq!(h.tuner.now().unwrap().channel_id, "c3");
}

#[test]
fn start_resumes_the_channel_this_client_watched_last() {
    let mut h = Harness::new(3, 2);
    let event = |client: &str, channel: &str| HistoryEntry {
        id: "e".into(),
        ts_ms: 0,
        client_id: Some(client.into()),
        channel_id: Some(channel.into()),
        resolved_channel_id: None,
        resolution_path: Vec::new(),
        item_id: None,
        kind: EventKind::Progress,
        position_ms: None,
    };
    // Newest first: someone else on c3, then us on c2.
    h.server.borrow_mut().history = vec![event("other", "c3"), event("me", "c2")];
    h.tuner.start(None).unwrap();
    assert_eq!(h.tuner.now().unwrap().channel_id, "c2");

    // With no history of our own, the most recent channel of anyone.
    let mut h = Harness::new(3, 2);
    h.server.borrow_mut().history = vec![event("other", "c3")];
    h.tuner.start(None).unwrap();
    assert_eq!(h.tuner.now().unwrap().channel_id, "c3");

    // With no history at all, the first channel.
    let mut h = Harness::new(3, 2);
    h.tuner.start(None).unwrap();
    assert_eq!(h.tuner.now().unwrap().channel_id, "c1");
}

#[test]
fn failed_tune_keeps_the_current_channel_playing() {
    let mut h = Harness::new(2, 2);
    h.tuner.start(Some(1)).unwrap();
    h.server.borrow_mut().channels[1].items.clear();
    // The cached list still says channel 2 has items; the server says no.
    let err = h.tuner.handle_action(Action::ChannelUp).unwrap_err();
    assert_eq!(err.to_string(), "server returned 409: empty");
    assert_eq!(h.tuner.now().unwrap().channel_id, "c1");
}

#[test]
fn subtitles_are_passed_to_the_player() {
    let mut h = Harness::new(1, 2);
    h.server.borrow_mut().channels[0].items[0].subtitles = vec![Subtitle {
        index: 0,
        language: Some("en".into()),
        label: "en".into(),
        format: "srt".into(),
        url: "http://server/api/v1/subtitles/c1-0/0".into(),
    }];
    h.tuner.start(Some(1)).unwrap();
    let Some(Entry::Load(request)) = h.take().pop() else {
        panic!("expected a load");
    };
    assert_eq!(
        request.subtitles,
        vec![SubtitleTrack {
            url: "http://server/api/v1/subtitles/c1-0/0".into(),
            title: "en".into(),
            language: Some("en".into()),
        }]
    );
    h.action(Action::ToggleSubtitles);
    assert_eq!(h.take(), vec![Entry::ToggleSubtitles]);
}

#[test]
fn describe_and_format() {
    let mut h = Harness::new(1, 1);
    h.tuner.start(Some(1)).unwrap();
    let mut now = h.tuner.now().unwrap().clone();
    now.item.show = Some("The Office".into());
    now.item.season = Some(3);
    now.item.episode = Some(14);
    now.item.title = "The Injury".into();
    assert_eq!(describe(&now), "The Office · S03E14 · The Injury");
    assert_eq!(format_ms(65_000), "1:05");
    assert_eq!(format_ms(3_725_000), "1:02:05");
}

fn errors(events: &[Event]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Error { message } => Some(message.as_str()),
            _ => None,
        })
        .collect()
}

fn advances(entries: &[Entry], reason: AdvanceReason) -> usize {
    entries
        .iter()
        .filter(|e| matches!(e, Entry::Advance(_, _, r) if *r == reason))
        .count()
}

#[test]
fn an_item_that_fails_to_load_is_skipped_with_an_error_event() {
    let mut h = Harness::new(1, 4);
    h.broken.borrow_mut().insert("c1-0".into());
    h.tuner.start(Some(1)).unwrap();
    let entries = h.take();
    assert!(entries.contains(&Entry::Advance(
        "c1".into(),
        "c1-0".into(),
        AdvanceReason::Skipped
    )));
    assert_eq!(entries.last(), Some(&load("c1-1", 0)));
    assert_eq!(h.playing_item(), "c1-1");
    let events = h.take_events();
    let errs = errors(&events);
    assert_eq!(errs.len(), 1);
    assert!(errs[0].contains("c1-0.mkv"), "{}", errs[0]);
    assert!(matches!(
        events.last(),
        Some(Event::NowPlaying { item_id, .. }) if item_id == "c1-1"
    ));
}

#[test]
fn an_item_that_fails_while_playing_is_skipped() {
    let mut h = Harness::new(1, 4);
    h.tuner.start(Some(1)).unwrap();
    h.take();
    h.take_events();
    // e.g. the file was deleted: mpv opens the stream, then gets a 404.
    h.tuner
        .handle_player_event(PlayerEvent::Failed("HTTP 404".into()))
        .unwrap();
    let entries = h.take();
    assert_eq!(advances(&entries, AdvanceReason::Skipped), 1);
    assert_eq!(h.playing_item(), "c1-1");
    assert_eq!(errors(&h.take_events()).len(), 1);
}

#[test]
fn stops_retrying_after_too_many_failures_in_a_row() {
    let mut h = Harness::new(1, 8);
    for i in 0..8 {
        h.broken.borrow_mut().insert(format!("c1-{i}"));
    }
    h.tuner.start(Some(1)).unwrap(); // does not loop forever, does not error
    let entries = h.take();
    let loads = entries
        .iter()
        .filter(|e| matches!(e, Entry::Load(_)))
        .count();
    assert_eq!(loads, 5, "default max_consecutive_failures");
    assert_eq!(advances(&entries, AdvanceReason::Skipped), 4);
    assert_eq!(entries.last(), Some(&Entry::Stop));
    let events = h.take_events();
    let errs = errors(&events);
    assert_eq!(errs.len(), 6, "{errs:?}");
    assert!(errs[5].contains("stopped trying"), "{}", errs[5]);

    // Nothing else happens on its own...
    h.advance_clock(Duration::from_secs(60));
    h.tuner.tick();
    assert!(h.take().iter().all(|e| !matches!(e, Entry::Load(_))));
    // ...but the viewer can try again, with a fresh set of retries.
    h.broken.borrow_mut().clear();
    h.action(Action::NextItem);
    assert!(matches!(h.take().last(), Some(Entry::Load(_))));
}

#[test]
fn a_started_item_ends_the_failure_streak() {
    let mut h = Harness::new(1, 10);
    let config_max = TunerConfig::default().max_consecutive_failures;
    // Four bad items, a good one, then four more bad: never gives up.
    for i in [0, 1, 2, 3, 5, 6, 7, 8] {
        h.broken.borrow_mut().insert(format!("c1-{i}"));
    }
    h.tuner.start(Some(1)).unwrap();
    assert_eq!(h.playing_item(), "c1-4");
    h.tuner.handle_player_event(PlayerEvent::Started).unwrap();
    h.tuner.handle_player_event(PlayerEvent::Ended).unwrap();
    assert_eq!(h.playing_item(), "c1-9");
    let errs = h.take_events();
    assert_eq!(errors(&errs).len(), 8);
    assert!(errors(&errs).iter().all(|m| !m.contains("stopped trying")));
    assert_eq!(config_max, 5);
}

#[test]
fn window_actions_are_ignored() {
    let mut h = Harness::new(1, 2);
    h.tuner.start(Some(1)).unwrap();
    h.take();
    assert_eq!(h.action(Action::ToggleFullscreen), Flow::Continue);
    assert_eq!(h.take(), vec![]);
    assert_eq!(h.playing_item(), "c1-0");
}

#[test]
fn standby_pauses_and_reports_and_wake_resumes() {
    let mut h = Harness::new(1, 2);
    h.tuner.start(Some(1)).unwrap();
    h.position.set(Some(3 * MIN));
    h.take();
    h.take_events();

    h.action(Action::Standby);
    assert!(h.tuner.in_standby());
    let entries = h.take();
    assert_eq!(entries[0], Entry::Pause(true));
    let reports = Harness::progress_reports(&entries);
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].1.position_ms, 3 * MIN);
    assert!(reports[0].1.paused);
    assert_eq!(h.saved("c1"), ("c1-0".into(), 3 * MIN));
    let events = h.take_events();
    assert!(matches!(events[0], Event::Paused { .. }), "{events:?}");
    assert_eq!(events[1], Event::Standby);
    // Paused: no periodic reports while in standby.
    assert_eq!(h.tuner.next_deadline(), None);

    // Standby again changes nothing.
    h.action(Action::Standby);
    assert_eq!(h.take(), vec![]);
    assert_eq!(h.take_events(), vec![]);

    h.action(Action::Wake);
    assert!(!h.tuner.in_standby());
    assert_eq!(h.take()[0], Entry::Pause(false));
    let events = h.take_events();
    assert_eq!(events[0], Event::Wake);
    assert!(matches!(events[1], Event::Resumed { .. }), "{events:?}");
    // Waking when awake changes nothing.
    h.action(Action::Wake);
    assert_eq!(h.take(), vec![]);
    assert_eq!(h.take_events(), vec![]);
}

#[test]
fn wake_keeps_a_pause_the_viewer_chose() {
    let mut h = Harness::new(1, 2);
    h.tuner.start(Some(1)).unwrap();
    h.position.set(Some(MIN));
    h.action(Action::Pause);
    h.take();
    h.take_events();

    h.action(Action::Standby);
    // Already paused: just a progress report.
    let entries = h.take();
    assert!(!entries.contains(&Entry::Pause(true)), "{entries:?}");
    assert_eq!(Harness::progress_reports(&entries).len(), 1);
    h.action(Action::Wake);
    assert_eq!(h.take(), vec![]);
    assert!(h.tuner.player().is_paused());
    assert_eq!(h.take_events(), vec![Event::Standby, Event::Wake]);
}

#[test]
fn playing_anything_ends_standby() {
    let mut h = Harness::new(2, 2);
    h.tuner.start(Some(1)).unwrap();
    h.position.set(Some(MIN));

    // Pressing play.
    h.action(Action::Standby);
    h.take_events();
    h.action(Action::TogglePause);
    assert!(!h.tuner.in_standby());
    let events = h.take_events();
    assert_eq!(events[0], Event::Wake, "{events:?}");
    assert!(matches!(events[1], Event::Resumed { .. }));

    // Changing channel.
    h.action(Action::Standby);
    h.take_events();
    h.action(Action::ChannelUp);
    assert!(!h.tuner.in_standby());
    assert!(!h.tuner.player().is_paused());
    let events = h.take_events();
    assert!(events.contains(&Event::Wake), "{events:?}");

    // Skipping.
    h.action(Action::Standby);
    h.take_events();
    h.action(Action::NextItem);
    assert!(!h.tuner.in_standby());
    assert!(h.take_events().contains(&Event::Wake));

    // A later wake has nothing to do.
    h.take();
    h.action(Action::Wake);
    assert_eq!(h.take(), vec![]);
    assert_eq!(h.take_events(), vec![]);
}

#[test]
fn standby_hides_the_overlay_and_closes_the_guide() {
    let mut h = Harness::new(2, 2);
    h.tuner.start(Some(1)).unwrap();
    h.action(Action::ToggleGuide);
    h.take_ui();
    h.action(Action::Standby);
    assert_eq!(h.take_ui(), vec![UiEvent::Guide(None), UiEvent::Hide]);
    assert!(!h.tuner.navigation().guide_open());
}

#[test]
fn standby_with_nothing_playing_does_not_panic() {
    let mut h = Harness::new(1, 2);
    h.action(Action::Standby);
    assert!(h.tuner.in_standby());
    h.action(Action::Wake);
    assert!(!h.tuner.in_standby());
    assert_eq!(h.take_events(), vec![Event::Standby, Event::Wake]);
}

#[test]
fn actions_before_anything_plays_do_not_panic() {
    let mut h = Harness::new(2, 2);
    for action in [
        Action::TogglePause,
        Action::Play,
        Action::Pause,
        Action::SeekRelative { seconds: 10 },
        Action::NextItem,
        Action::PreviousItem,
        Action::Select,
        Action::Back,
        Action::ShowInfo,
        Action::ToggleSubtitles,
        Action::Navigate {
            direction: Direction::Left,
        },
    ] {
        h.tuner.handle_action(action).unwrap();
    }
    assert_eq!(h.tuner.state(), &State::Idle);
}

#[test]
fn navigate_without_the_guide_changes_channel_and_seeks() {
    let mut h = Harness::new(3, 2);
    h.tuner.start(Some(1)).unwrap();
    h.take();
    let nav = |direction| Action::Navigate { direction };
    h.action(nav(Direction::Up));
    assert_eq!(h.tuner.now().unwrap().channel_number, 2);
    h.action(nav(Direction::Down));
    assert_eq!(h.tuner.now().unwrap().channel_number, 1);
    h.take();
    h.action(nav(Direction::Right));
    h.action(nav(Direction::Left));
    assert_eq!(h.take(), vec![Entry::Seek(10), Entry::Seek(-10)]);
}

#[test]
fn navigate_with_the_guide_moves_the_selection_and_select_tunes() {
    let mut h = Harness::new(3, 2);
    h.tuner.start(Some(1)).unwrap();
    h.action(Action::ToggleGuide);
    assert!(h.tuner.navigation().guide_open());
    h.take();

    let nav = |direction| Action::Navigate { direction };
    h.action(nav(Direction::Down));
    h.action(nav(Direction::Down));
    h.action(nav(Direction::Left));
    // Moving the highlight neither tunes nor seeks.
    assert_eq!(h.take(), vec![]);
    assert_eq!(h.tuner.now().unwrap().channel_number, 1);
    assert_eq!(h.tuner.navigation().highlighted(), Some(2));

    h.action(Action::Select);
    assert_eq!(h.tuner.now().unwrap().channel_id, "c3");
    assert!(!h.tuner.navigation().guide_open());

    // Back closes the guide; arrows change channel again.
    h.action(Action::ToggleGuide);
    h.action(Action::Back);
    h.action(nav(Direction::Up));
    assert_eq!(h.tuner.now().unwrap().channel_number, 1);
}

#[test]
fn select_with_nothing_open_shows_info() {
    let mut h = Harness::new(1, 2);
    h.tuner.start(Some(1)).unwrap();
    h.take();
    h.action(Action::Select);
    assert_eq!(h.take(), vec![], "info is logged, nothing else");
}

#[test]
fn go_to_channel_by_id() {
    let mut h = Harness::new(3, 2);
    h.tuner.start(Some(1)).unwrap();
    h.action(Action::GoToChannelId { id: "c2".into() });
    assert_eq!(h.tuner.now().unwrap().channel_number, 2);
    let err = h
        .tuner
        .handle_action(Action::GoToChannelId { id: "nope".into() })
        .unwrap_err();
    assert!(matches!(err, TunerError::NoSuchChannel(ref n) if n == "nope"));
}

#[test]
fn hidden_channels_are_skipped_by_flipping_and_number_but_reachable_by_id() {
    let mut h = Harness::new(3, 2);
    h.server.borrow_mut().channels[1].hidden = true;
    h.tuner.start(Some(1)).unwrap();
    h.action(Action::ChannelUp);
    assert_eq!(h.tuner.now().unwrap().channel_id, "c3");
    h.action(Action::ChannelDown);
    assert_eq!(h.tuner.now().unwrap().channel_id, "c1");
    let err = h
        .tuner
        .handle_action(Action::GoToChannel { number: 2 })
        .unwrap_err();
    assert!(matches!(err, TunerError::NoSuchChannel(ref n) if n == "2"));
    assert_eq!(h.tuner.now().unwrap().channel_id, "c1");

    // By ID, the hidden channel is reachable.
    h.action(Action::GoToChannelId { id: "c2".into() });
    assert_eq!(h.tuner.now().unwrap().channel_id, "c2");
    // Flipping from it goes to its visible neighbours.
    h.action(Action::ChannelUp);
    assert_eq!(h.tuner.now().unwrap().channel_id, "c3");
    // And the guide lists only visible channels.
    h.action(Action::ToggleGuide);
    let guide = h
        .take_ui()
        .into_iter()
        .rev()
        .find_map(|cue| match cue {
            UiEvent::Guide(Some(guide)) => Some(guide),
            _ => None,
        })
        .unwrap();
    let numbers: Vec<u32> = guide.rows.iter().map(|r| r.number).collect();
    assert_eq!(numbers, [1, 3]);
}

#[test]
fn nested_channels_report_their_resolution_path() {
    let mut h = Harness::new(2, 2);
    h.server.borrow_mut().channels[0].via = vec![ChannelRef {
        id: "child".into(),
        name: "Child".into(),
    }];
    h.tuner.start(Some(1)).unwrap();
    let events = h.take_events();
    let path = vec!["c1".to_owned(), "child".to_owned()];
    assert!(
        events.iter().any(|e| matches!(e,
            Event::ChannelChanged { resolution_path, .. } if *resolution_path == path)),
        "{events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(e,
            Event::NowPlaying { resolution_path, .. } if *resolution_path == path)),
        "{events:?}"
    );
    let banner = h
        .take_ui()
        .into_iter()
        .find_map(|cue| match cue {
            UiEvent::Playing { banner, .. } => Some(banner),
            _ => None,
        })
        .unwrap();
    assert_eq!(banner.item_line(), "Child: c1-0");
}

#[test]
fn what_the_server_skipped_is_reported_as_errors() {
    let mut h = Harness::new(2, 2);
    let warning = "channel \"empty\" has nothing to play; channel \"c1\" skipped it";
    h.server.borrow_mut().channels[0].warnings = vec![warning.into()];
    h.tuner.start(Some(1)).unwrap();
    let events = h.take_events();
    let error = events
        .iter()
        .position(|e| matches!(e, Event::Error { message } if message == warning));
    let playing = events
        .iter()
        .position(|e| matches!(e, Event::NowPlaying { .. }));
    assert!(error.is_some() && error < playing, "{events:?}");
    // Playback carries on.
    assert_eq!(h.tuner.now().unwrap().item.id, "c1-0");

    // Advancing at the end of an item reports them too.
    h.tuner.handle_player_event(PlayerEvent::Ended).unwrap();
    let events = h.take_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Error { message } if message == warning)),
        "{events:?}"
    );
}

#[test]
fn play_and_pause_are_idempotent_and_emit_events() {
    let mut h = Harness::new(1, 2);
    h.tuner.start(Some(1)).unwrap();
    h.position.set(Some(MIN));
    h.take();
    h.take_events();

    h.action(Action::Play); // already playing
    assert_eq!(h.take(), vec![]);
    h.action(Action::Pause);
    h.action(Action::Pause);
    let entries = h.take();
    assert_eq!(entries[0], Entry::Pause(true));
    assert_eq!(Harness::progress_reports(&entries).len(), 1);
    h.action(Action::Play);
    assert_eq!(h.take()[0], Entry::Pause(false));
    let paused = |e: &Event| matches!(e, Event::Paused { position_ms, .. } if *position_ms == MIN);
    let resumed = |e: &Event| matches!(e, Event::Resumed { .. });
    let events = h.take_events();
    assert_eq!(events.len(), 2, "{events:?}");
    assert!(paused(&events[0]) && resumed(&events[1]));
}

#[test]
fn tuning_and_advancing_emit_events() {
    let mut h = Harness::new(2, 3);
    h.tuner.start(Some(1)).unwrap();
    let events = h.take_events();
    assert!(matches!(
        &events[..],
        [
            Event::ChannelChanged { channel_id, .. },
            Event::NowPlaying { item_id, .. },
        ] if channel_id == "c1" && item_id == "c1-0"
    ));
    h.tuner.handle_player_event(PlayerEvent::Ended).unwrap();
    let events = h.take_events();
    assert!(matches!(
        &events[..],
        [
            Event::ItemEnded { item_id: ended, .. },
            Event::NowPlaying { item_id: next, .. },
        ] if ended == "c1-0" && next == "c1-1"
    ));
}

fn banner(number: u32, title: &str) -> Banner {
    Banner {
        channel_number: number,
        channel_name: format!("Channel {number}"),
        title: Some(title.into()),
        show: None,
        via: None,
    }
}

#[test]
fn the_banner_is_cued_before_the_switch_touches_the_network() {
    let mut h = Harness::new(3, 2);
    h.set_saved("c2", 1, MIN);
    h.tuner.start(Some(1)).unwrap();
    h.take_ui();
    h.take();
    let calls = h.tuner.api.calls.clone();
    let calls_before = calls.load(Ordering::SeqCst);

    h.action(Action::ChannelUp);
    let cues = std::mem::take(&mut *h.ui.lock().unwrap());
    // The first cue names the target channel and what it was last on, and
    // came before any API call for the switch (progress, now).
    assert_eq!(cues[0], (calls_before, UiEvent::Tuning(banner(2, "c2-1"))));
    assert!(calls.load(Ordering::SeqCst) > calls_before);
    assert_eq!(
        cues[1].1,
        UiEvent::Playing {
            banner: banner(2, "c2-1"),
            announce: true
        }
    );
    assert_eq!(cues.len(), 2, "{cues:?}");
}

#[test]
fn a_failed_switch_cues_the_channel_still_playing() {
    let mut h = Harness::new(2, 2);
    h.tuner.start(Some(1)).unwrap();
    h.server.borrow_mut().channels[1].items.clear();
    h.take_ui();
    assert!(h.tuner.handle_action(Action::ChannelUp).is_err());
    assert_eq!(
        h.take_ui(),
        vec![
            UiEvent::Tuning(banner(2, "c2-0")),
            UiEvent::Playing {
                banner: banner(1, "c1-0"),
                announce: true
            },
        ]
    );
}

#[test]
fn automatic_advances_update_the_banner_without_announcing() {
    let mut h = Harness::new(1, 3);
    h.tuner.start(Some(1)).unwrap();
    h.take_ui();
    h.tuner.handle_player_event(PlayerEvent::Ended).unwrap();
    assert_eq!(
        h.take_ui(),
        vec![UiEvent::Playing {
            banner: banner(1, "c1-1"),
            announce: false
        }]
    );
    // A skip is the viewer's doing, so it is announced.
    h.action(Action::NextItem);
    assert_eq!(
        h.take_ui(),
        vec![UiEvent::Playing {
            banner: banner(1, "c1-2"),
            announce: true
        }]
    );
}

#[test]
fn info_back_and_the_guide_are_cued() {
    let mut h = Harness::new(3, 2);
    h.tuner.start(Some(2)).unwrap();
    h.take_ui();

    h.action(Action::ShowInfo);
    h.action(Action::Back);
    assert_eq!(h.take_ui(), vec![UiEvent::ShowInfo, UiEvent::Hide]);
    h.action(Action::Select);
    assert_eq!(h.take_ui(), vec![UiEvent::ShowInfo]);

    let row = |n: u32, current| GuideRow {
        number: n,
        name: format!("Channel {n}"),
        on: Some(format!("c{n}-0")),
        current,
    };
    let rows = vec![row(1, false), row(2, true), row(3, false)];
    let guide = |highlighted| {
        UiEvent::Guide(Some(Guide {
            rows: rows.clone(),
            highlighted,
        }))
    };
    h.action(Action::ToggleGuide);
    h.action(Action::Navigate {
        direction: Direction::Down,
    });
    h.action(Action::Back);
    assert_eq!(h.take_ui(), vec![guide(1), guide(2), UiEvent::Guide(None)]);

    // Selecting closes the guide before the switch is announced.
    h.action(Action::ToggleGuide);
    h.action(Action::Navigate {
        direction: Direction::Up,
    });
    h.take_ui();
    h.action(Action::Select);
    let cues = h.take_ui();
    assert_eq!(cues[0], UiEvent::Guide(None));
    assert_eq!(cues[1], UiEvent::Tuning(banner(1, "c1-0")));
}

#[test]
fn giving_up_after_failures_cues_stopped() {
    let mut h = Harness::new(1, 2);
    h.broken
        .borrow_mut()
        .extend(["c1-0".to_owned(), "c1-1".to_owned()]);
    h.tuner.start(Some(1)).unwrap();
    assert_eq!(h.take_ui().last(), Some(&UiEvent::Stopped));
}
