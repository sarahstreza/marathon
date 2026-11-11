//! Tuner scenarios against an in-memory fake server and a mock player
//! (PLAN.md M3). Both record into one shared journal, so tests can assert
//! the order of API calls and player commands.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use marathon_api::{EventKind, HistoryEntry, Item, NextItem, Subtitle};

use super::*;

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
}

impl FakeApi {
    fn log(&self, entry: Entry) {
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
            .map(|c| ChannelSummary {
                id: c.id.clone(),
                number: c.number,
                name: format!("Channel {}", c.number),
                item_count: c.items.len() as u32,
                current: c.items.get(c.current).cloned(),
                position_ms: c.position_ms,
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
    position_ms: Rc<Cell<Option<u64>>>,
    duration_ms: Rc<Cell<Option<u64>>>,
    paused: bool,
}

impl Player for MockPlayer {
    fn load(&mut self, request: &LoadRequest) -> Result<(), PlayerError> {
        self.journal.borrow_mut().push(Entry::Load(request.clone()));
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
                })
                .collect(),
            history: Vec::new(),
            fail_advance: false,
        };
        let server = Rc::new(RefCell::new(server));
        let journal = Journal::default();
        let position = Rc::new(Cell::new(None));
        let clock = Rc::new(Cell::new(Instant::now()));
        let player = MockPlayer {
            journal: journal.clone(),
            position_ms: position.clone(),
            duration_ms: Rc::new(Cell::new(Some(20 * MIN))),
            paused: false,
        };
        let api = FakeApi {
            server: server.clone(),
            journal: journal.clone(),
        };
        let config = TunerConfig {
            client_id: Some("me".into()),
            ..TunerConfig::default()
        };
        Self {
            tuner: Tuner::new(api, player, TestClock(clock.clone()), config),
            server,
            journal,
            position,
            clock,
        }
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
    h.action(Action::SeekRelative(-10));
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
    h.action(Action::GoToChannel(3));
    assert_eq!(h.tuner.now().unwrap().channel_id, "c3");
    h.take();
    // Same channel: nothing happens.
    h.action(Action::GoToChannel(3));
    assert_eq!(h.take(), vec![]);
    // Unknown channel: refreshes the list, then reports the error.
    let err = h.tuner.handle_action(Action::GoToChannel(9)).unwrap_err();
    assert!(matches!(err, TunerError::NoSuchChannel(9)));
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
