//! Local input adapters (PLAN.md §9): the [`InputSource`] trait and
//! [`InputSources`], which runs any number of them at once.
//!
//! A source is started with an [`ActionSender`] into the action bus, runs on
//! its own thread or task, sends [`Action`](marathon_api::Action)s, and
//! stops cleanly when asked. Each concrete source lives behind a cargo
//! feature (`input-keyboard`, `input-mouse`, `input-gamepad`, later
//! `input-cec`, ...) so a
//! build includes only what its platform needs.

use crate::bus::{ActionBus, ActionSender};

/// An input source failed to start.
#[derive(Debug, thiserror::Error)]
#[error("input source {source_name}: {message}")]
pub struct InputError {
    pub source_name: String,
    pub message: String,
}

/// A local input adapter.
pub trait InputSource: Send {
    /// A short name, used to tag its actions on the bus (e.g. `keyboard`).
    fn name(&self) -> &str;
    /// Starts delivering actions to `actions`. Must not block: run the work
    /// on a thread or task. If the bus closes, the source should stop on
    /// its own.
    fn start(&mut self, actions: ActionSender) -> Result<(), InputError>;
    /// Stops delivering actions and releases resources. Called at most once
    /// after a successful `start`; must return promptly.
    fn stop(&mut self);
}

/// The running set of input sources. Stops them all when dropped.
#[derive(Default)]
pub struct InputSources {
    running: Vec<Box<dyn InputSource>>,
}

impl InputSources {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts `source` on `bus` and keeps it running.
    pub fn start(
        &mut self,
        mut source: Box<dyn InputSource>,
        bus: &ActionBus,
    ) -> Result<(), InputError> {
        let sender = bus.sender(source.name());
        source.start(sender)?;
        tracing::info!(source = source.name(), "input source started");
        self.running.push(source);
        Ok(())
    }

    /// Stops the source called `name`, if running. Others keep going.
    pub fn stop(&mut self, name: &str) -> bool {
        match self.running.iter().position(|s| s.name() == name) {
            Some(i) => {
                let mut source = self.running.remove(i);
                source.stop();
                tracing::info!(source = name, "input source stopped");
                true
            }
            None => false,
        }
    }

    /// Stops every source.
    pub fn stop_all(&mut self) {
        for mut source in self.running.drain(..).rev() {
            source.stop();
            tracing::info!(source = source.name(), "input source stopped");
        }
    }

    /// Names of the running sources, in start order.
    pub fn names(&self) -> Vec<&str> {
        self.running.iter().map(|s| s.name()).collect()
    }
}

impl Drop for InputSources {
    fn drop(&mut self) {
        self.stop_all();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    use marathon_api::Action;

    use super::*;
    use crate::bus::SourcedAction;

    /// Sends `action` every millisecond on its own thread until stopped.
    struct Ticker {
        name: &'static str,
        action: Action,
        stop: Arc<AtomicBool>,
        thread: Option<JoinHandle<()>>,
    }

    impl Ticker {
        fn new(name: &'static str, action: Action) -> Box<Self> {
            Box::new(Self {
                name,
                action,
                stop: Arc::default(),
                thread: None,
            })
        }
    }

    impl InputSource for Ticker {
        fn name(&self) -> &str {
            self.name
        }
        fn start(&mut self, actions: ActionSender) -> Result<(), InputError> {
            let (stop, action) = (self.stop.clone(), self.action.clone());
            self.thread = Some(std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    if actions.send(action.clone()).is_err() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            }));
            Ok(())
        }
        fn stop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(t) = self.thread.take() {
                t.join().unwrap();
            }
        }
    }

    /// Collects actions until both sources have been heard from, or panics.
    fn wait_for(rx: &std::sync::mpsc::Receiver<SourcedAction>, sources: &[&str]) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut heard = std::collections::HashSet::new();
        while heard.len() < sources.len() {
            let left = deadline.saturating_duration_since(Instant::now());
            let a = rx.recv_timeout(left).expect("timed out waiting for input");
            heard.insert(a.source.to_string());
        }
    }

    #[test]
    fn concurrent_sources_both_deliver_and_stopping_one_leaves_the_other() {
        let (bus, rx) = ActionBus::channel();
        let mut sources = InputSources::new();
        sources
            .start(Ticker::new("one", Action::ChannelUp), &bus)
            .unwrap();
        sources
            .start(Ticker::new("two", Action::ChannelDown), &bus)
            .unwrap();
        assert_eq!(sources.names(), ["one", "two"]);
        wait_for(&rx, &["one", "two"]);

        assert!(sources.stop("one"));
        assert!(!sources.stop("one"));
        assert_eq!(sources.names(), ["two"]);
        // Drain what "one" sent before stopping; from now on only "two".
        while rx.try_recv().is_ok() {}
        let later: Vec<_> = (0..20)
            .map(|_| rx.recv_timeout(Duration::from_secs(5)).unwrap())
            .collect();
        assert!(later.iter().all(|a| &*a.source == "two"));
        assert!(later.iter().all(|a| a.action == Action::ChannelDown));

        sources.stop_all();
        assert!(sources.names().is_empty());
    }

    #[test]
    fn sources_stop_on_their_own_when_the_bus_closes() {
        let (bus, rx) = ActionBus::channel();
        let mut sources = InputSources::new();
        sources
            .start(Ticker::new("one", Action::Quit), &bus)
            .unwrap();
        drop(rx);
        // stop() joins the thread; it must have exited (or exit promptly).
        sources.stop_all();
    }
}
