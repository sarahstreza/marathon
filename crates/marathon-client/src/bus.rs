//! The action bus (PLAN.md §9 "Input and control"): every input adapter
//! sends [`Action`]s into it, and the client consumes them in one place.
//!
//! The bus is a cheap, clonable handle around a *sink* function chosen by
//! the app. [`ActionBus::channel`] is the plain version, delivering into an
//! `mpsc` receiver; the desktop client uses [`ActionBus::new`] with a sink
//! that routes window actions (fullscreen, quit) to the window and the rest
//! to its tuner thread.
//!
//! Conflicts are resolved by order of arrival: last action wins, and no
//! source can block another.

use std::fmt;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};

use marathon_api::Action;

/// An action and the input source it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcedAction {
    /// The source's name, e.g. `keyboard`.
    pub source: Arc<str>,
    pub action: Action,
}

/// The bus's consumer has gone away; the source should stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the action bus is closed")]
pub struct BusClosed;

type Sink = dyn Fn(SourcedAction) -> Result<(), BusClosed> + Send + Sync;

/// Where input sources send actions. Clone freely.
#[derive(Clone)]
pub struct ActionBus {
    sink: Arc<Sink>,
}

impl fmt::Debug for ActionBus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ActionBus").finish_non_exhaustive()
    }
}

impl ActionBus {
    /// A bus that hands every action to `sink`. The sink is called on the
    /// sending source's thread, so it should only forward, not do work.
    pub fn new(
        sink: impl Fn(SourcedAction) -> Result<(), BusClosed> + Send + Sync + 'static,
    ) -> Self {
        Self {
            sink: Arc::new(sink),
        }
    }

    /// A bus delivering into a channel. Closed once the receiver is
    /// dropped.
    pub fn channel() -> (Self, Receiver<SourcedAction>) {
        let (tx, rx) = mpsc::channel();
        (Self::new(move |a| tx.send(a).map_err(|_| BusClosed)), rx)
    }

    /// A sender for the input source called `source`.
    pub fn sender(&self, source: &str) -> ActionSender {
        ActionSender {
            source: source.into(),
            sink: self.sink.clone(),
        }
    }
}

/// An input source's handle on the bus. Clone freely.
#[derive(Clone)]
pub struct ActionSender {
    source: Arc<str>,
    sink: Arc<Sink>,
}

impl fmt::Debug for ActionSender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ActionSender")
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

impl ActionSender {
    pub fn send(&self, action: Action) -> Result<(), BusClosed> {
        tracing::debug!(source = %self.source, ?action, "input");
        (self.sink)(SourcedAction {
            source: self.source.clone(),
            action,
        })
    }

    /// The name of the source this sender belongs to.
    pub fn source(&self) -> &str {
        &self.source
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_arrive_tagged_with_their_source() {
        let (bus, rx) = ActionBus::channel();
        bus.sender("a").send(Action::ChannelUp).unwrap();
        bus.sender("b").send(Action::Quit).unwrap();
        let got: Vec<_> = rx.try_iter().map(|s| (s.source, s.action)).collect();
        assert_eq!(
            got,
            vec![("a".into(), Action::ChannelUp), ("b".into(), Action::Quit)]
        );
    }

    #[test]
    fn closed_once_the_receiver_is_gone() {
        let (bus, rx) = ActionBus::channel();
        let sender = bus.sender("a");
        drop(rx);
        assert_eq!(sender.send(Action::ChannelUp), Err(BusClosed));
    }
}
