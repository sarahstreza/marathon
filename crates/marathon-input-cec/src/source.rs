//! [`CecSource`]: the CEC [`InputSource`]. It owns a [`CecAdapter`] on its
//! own thread, turns what the adapter reports into actions (remote keys
//! through [`Remote`], TV input switching through [`Lifecycle`]), and keeps
//! the adapter open: if it cannot be opened, or the connection is lost
//! (cable or USB adapter unplugged), it tries again every
//! [`CecConfig::retry_every`] until it can.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::Instant;

use marathon_client::bus::ActionSender;
use marathon_client::input::{InputError, InputSource};

use crate::CecConfig;
use crate::adapter::{CecAdapter, CecEvent, EventSender, Msg};
use crate::keys::Remote;
use crate::lifecycle::{Lifecycle, TvEvent};

/// The name the source's actions carry on the bus.
pub const SOURCE_NAME: &str = "cec";

/// The CEC input source. See the module docs.
pub struct CecSource<A: CecAdapter> {
    adapter: Option<A>,
    config: CecConfig,
    tx: Sender<Msg>,
    inbox: Option<Receiver<Msg>>,
    thread: Option<JoinHandle<()>>,
}

impl<A: CecAdapter> CecSource<A> {
    pub fn new(adapter: A, config: CecConfig) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            adapter: Some(adapter),
            config,
            tx,
            inbox: Some(rx),
            thread: None,
        }
    }
}

impl<A: CecAdapter> InputSource for CecSource<A> {
    fn name(&self) -> &str {
        SOURCE_NAME
    }

    fn start(&mut self, actions: ActionSender) -> Result<(), InputError> {
        let error = |message: String| InputError {
            source_name: SOURCE_NAME.to_owned(),
            message,
        };
        let (Some(adapter), Some(inbox)) = (self.adapter.take(), self.inbox.take()) else {
            return Err(error("already started".to_owned()));
        };
        let config = self.config.clone();
        let events = EventSender(self.tx.clone());
        let thread = std::thread::Builder::new()
            .name("input-cec".into())
            .spawn(move || run(adapter, config, inbox, events, actions))
            .map_err(|e| error(e.to_string()))?;
        self.thread = Some(thread);
        Ok(())
    }

    /// Stops and closes the adapter. If the adapter is in the middle of
    /// opening, waits for that to finish first (a few seconds at most).
    fn stop(&mut self) {
        let _ = self.tx.send(Msg::Stop);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("CEC input thread panicked");
        }
    }
}

impl<A: CecAdapter> Drop for CecSource<A> {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run<A: CecAdapter>(
    mut adapter: A,
    config: CecConfig,
    inbox: Receiver<Msg>,
    events: EventSender,
    actions: ActionSender,
) {
    let mut remote = Remote::new(config.colours.clone());
    let mut lifecycle = Lifecycle::new(config.resume_on_select);
    let mut open = false;
    let mut failures = 0u32;
    // When to (re)try opening the adapter; `None` while it is open.
    let mut retry_at = Some(Instant::now());

    loop {
        if let Some(at) = retry_at
            && Instant::now() >= at
        {
            match adapter.open(&config, events.clone()) {
                Ok(()) => {
                    tracing::info!(name = %config.osd_name, "CEC connected");
                    open = true;
                    failures = 0;
                    retry_at = None;
                }
                Err(err) => {
                    failures += 1;
                    // Say so once; a missing adapter should not fill the
                    // log.
                    if failures == 1 {
                        tracing::warn!(
                            %err,
                            retry_every = ?config.retry_every,
                            "CEC unavailable; will keep trying"
                        );
                    } else {
                        tracing::debug!(%err, failures, "CEC still unavailable");
                    }
                    retry_at = Some(Instant::now() + config.retry_every);
                }
            }
        }

        let deadline = [remote.deadline(), retry_at].into_iter().flatten().min();
        let message = match deadline {
            Some(deadline) => {
                inbox.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            }
            None => inbox.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        let action = match message {
            Ok(Msg::Event(event)) => match event {
                CecEvent::KeyPressed(key) => remote.press(key, Instant::now()),
                CecEvent::KeyReleased(key) => {
                    remote.release(key);
                    None
                }
                CecEvent::Selected(selected) => {
                    tracing::info!(selected, "TV switched input");
                    lifecycle.on(TvEvent::Selected(selected))
                }
                CecEvent::TvStandby => {
                    tracing::info!("TV going to standby");
                    lifecycle.on(TvEvent::Standby)
                }
                CecEvent::ConnectionLost => {
                    if open {
                        tracing::warn!("CEC connection lost; reconnecting");
                        adapter.close();
                        open = false;
                        retry_at = Some(Instant::now() + config.retry_every);
                    }
                    remote.reset();
                    lifecycle.on(TvEvent::ConnectionLost)
                }
            },
            Err(RecvTimeoutError::Timeout) => remote.tick(Instant::now()),
            Ok(Msg::Stop) | Err(RecvTimeoutError::Disconnected) => break,
        };
        if let Some(action) = action {
            tracing::debug!(action = action.name(), "CEC action");
            if actions.send(action).is_err() {
                break;
            }
        }
    }
    if open {
        adapter.close();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use marathon_api::{Action, Direction};
    use marathon_client::bus::{ActionBus, SourcedAction};

    use super::*;
    use crate::adapter::CecError;
    use crate::keys::code;

    /// What the mock adapter was asked to do, and its line to the source.
    #[derive(Default)]
    struct MockState {
        opens: u32,
        closes: u32,
        /// Opens to fail before one succeeds.
        fail_opens: u32,
        events: Option<EventSender>,
        /// The config the last open was given.
        config: Option<CecConfig>,
    }

    #[derive(Clone, Default)]
    struct MockAdapter(Arc<Mutex<MockState>>);

    impl MockAdapter {
        fn emit(&self, event: CecEvent) {
            let sender = self.0.lock().unwrap().events.clone();
            sender.expect("adapter is open").send(event);
        }

        fn counts(&self) -> (u32, u32) {
            let state = self.0.lock().unwrap();
            (state.opens, state.closes)
        }

        /// Waits until the adapter has been opened `n` times.
        fn wait_for_opens(&self, n: u32) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.counts().0 < n || self.0.lock().unwrap().events.is_none() {
                assert!(Instant::now() < deadline, "adapter never opened");
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    impl CecAdapter for MockAdapter {
        fn open(&mut self, config: &CecConfig, events: EventSender) -> Result<(), CecError> {
            let mut state = self.0.lock().unwrap();
            state.opens += 1;
            state.config = Some(config.clone());
            if state.fail_opens > 0 {
                state.fail_opens -= 1;
                return Err(CecError("no CEC adapter found".into()));
            }
            state.events = Some(events);
            Ok(())
        }

        fn close(&mut self) {
            let mut state = self.0.lock().unwrap();
            state.closes += 1;
            state.events = None;
        }
    }

    fn config() -> CecConfig {
        CecConfig {
            retry_every: Duration::from_millis(20),
            ..CecConfig::default()
        }
    }

    fn start(
        adapter: &MockAdapter,
        config: CecConfig,
    ) -> (CecSource<MockAdapter>, Receiver<SourcedAction>) {
        let (bus, rx) = ActionBus::channel();
        let mut source = CecSource::new(adapter.clone(), config);
        source.start(bus.sender(SOURCE_NAME)).unwrap();
        (source, rx)
    }

    fn next(rx: &Receiver<SourcedAction>) -> Action {
        let sourced = rx.recv_timeout(Duration::from_secs(5)).expect("an action");
        assert_eq!(&*sourced.source, "cec");
        sourced.action
    }

    fn nothing(rx: &Receiver<SourcedAction>) {
        if let Ok(a) = rx.recv_timeout(Duration::from_millis(50)) {
            panic!("unexpected {a:?}");
        }
    }

    #[test]
    fn remote_keys_become_actions() {
        let adapter = MockAdapter::default();
        let (mut source, rx) = start(&adapter, config());
        adapter.wait_for_opens(1);

        adapter.emit(CecEvent::KeyPressed(code::CHANNEL_UP));
        adapter.emit(CecEvent::KeyReleased(code::CHANNEL_UP));
        assert_eq!(next(&rx), Action::ChannelUp);
        // Held D-pad down: one press, however long the TV repeats it.
        for _ in 0..3 {
            adapter.emit(CecEvent::KeyPressed(code::DOWN));
        }
        adapter.emit(CecEvent::KeyReleased(code::DOWN));
        assert_eq!(
            next(&rx),
            Action::Navigate {
                direction: Direction::Down
            }
        );
        nothing(&rx);
        // A typed number commits on its own after the pause.
        adapter.emit(CecEvent::KeyPressed(code::NUMBER_0 + 5));
        adapter.emit(CecEvent::KeyReleased(code::NUMBER_0 + 5));
        assert_eq!(next(&rx), Action::GoToChannel { number: 5 });

        source.stop();
        assert_eq!(adapter.counts(), (1, 1), "opened once, closed on stop");
    }

    #[test]
    fn the_tv_switching_inputs_pauses_and_resumes() {
        let adapter = MockAdapter::default();
        let (_source, rx) = start(&adapter, config());
        adapter.wait_for_opens(1);

        // Already on Marathon's input at startup: nothing to do.
        adapter.emit(CecEvent::Selected(true));
        nothing(&rx);
        // The viewer switches to the games console...
        adapter.emit(CecEvent::Selected(false));
        assert_eq!(next(&rx), Action::Standby);
        // ...and back.
        adapter.emit(CecEvent::Selected(true));
        assert_eq!(next(&rx), Action::Wake);
        // The TV goes to standby; libcec then also reports the routing
        // change, which must not pause twice.
        adapter.emit(CecEvent::TvStandby);
        adapter.emit(CecEvent::Selected(false));
        assert_eq!(next(&rx), Action::Standby);
        nothing(&rx);
        // Turned back on, on Marathon's input.
        adapter.emit(CecEvent::Selected(true));
        assert_eq!(next(&rx), Action::Wake);
    }

    #[test]
    fn resume_on_select_can_be_turned_off() {
        let adapter = MockAdapter::default();
        let (_source, rx) = start(
            &adapter,
            CecConfig {
                resume_on_select: false,
                ..config()
            },
        );
        adapter.wait_for_opens(1);
        adapter.emit(CecEvent::Selected(false));
        assert_eq!(next(&rx), Action::Standby);
        adapter.emit(CecEvent::Selected(true));
        nothing(&rx);
    }

    #[test]
    fn keeps_trying_until_an_adapter_turns_up() {
        let adapter = MockAdapter::default();
        adapter.0.lock().unwrap().fail_opens = 3;
        let (_source, rx) = start(&adapter, config());
        adapter.wait_for_opens(4);
        adapter.emit(CecEvent::KeyPressed(code::SELECT));
        assert_eq!(next(&rx), Action::Select);
        assert_eq!(adapter.counts(), (4, 0));
    }

    #[test]
    fn reconnects_after_the_connection_is_lost() {
        let adapter = MockAdapter::default();
        let (_source, rx) = start(&adapter, config());
        adapter.wait_for_opens(1);
        // A key held when the cable is pulled is forgotten.
        adapter.emit(CecEvent::KeyPressed(code::CHANNEL_DOWN));
        assert_eq!(next(&rx), Action::ChannelDown);
        adapter.emit(CecEvent::ConnectionLost);
        adapter.wait_for_opens(2);
        assert_eq!(adapter.counts(), (2, 1));
        // Playback was not interrupted.
        nothing(&rx);
        adapter.emit(CecEvent::KeyPressed(code::CHANNEL_DOWN));
        assert_eq!(next(&rx), Action::ChannelDown);
    }

    #[test]
    fn the_adapter_gets_the_polite_config() {
        let adapter = MockAdapter::default();
        let (_source, _rx) = start(&adapter, config());
        adapter.wait_for_opens(1);
        let given = adapter.0.lock().unwrap().config.clone().unwrap();
        assert_eq!(given.osd_name, "Marathon");
        assert!(!given.claim_on_start);
        assert!(given.resume_on_select);
    }

    #[test]
    fn stops_promptly_and_only_starts_once() {
        let adapter = MockAdapter::default();
        adapter.0.lock().unwrap().fail_opens = u32::MAX;
        let (bus, _rx) = ActionBus::channel();
        let mut source = CecSource::new(adapter.clone(), config());
        source.start(bus.sender(SOURCE_NAME)).unwrap();
        assert!(source.start(bus.sender(SOURCE_NAME)).is_err());
        std::thread::sleep(Duration::from_millis(50));
        let started = Instant::now();
        source.stop();
        assert!(started.elapsed() < Duration::from_secs(1));
        // Never opened, so never closed.
        assert_eq!(adapter.counts().1, 0);
    }
}
