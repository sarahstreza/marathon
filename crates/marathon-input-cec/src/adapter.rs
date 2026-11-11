//! The boundary between Marathon and a CEC implementation: what an adapter
//! must do ([`CecAdapter`]) and what it reports ([`CecEvent`]). The real
//! one is [`LibCec`](crate::LibCec) (feature `libcec`); tests use a mock.

use std::sync::mpsc::Sender;

use crate::CecConfig;

/// Something an adapter observed on the CEC bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CecEvent {
    /// A remote key went down, or repeats while held. The code is a CEC
    /// user control code (see [`crate::keys::code`]).
    KeyPressed(u8),
    /// A remote key came up.
    KeyReleased(u8),
    /// The TV routed to this device's input (true) or away from it
    /// (false). libcec works this out from the routing messages on the
    /// bus (Set Stream Path, Routing Change, Active Source).
    Selected(bool),
    /// The TV announced it is going to standby.
    TvStandby,
    /// The adapter lost its connection (unplugged, or the bus went away).
    /// The source closes it and tries to open it again.
    ConnectionLost,
}

/// CEC logical address of the TV.
pub const TV: i32 = 0;
/// CEC opcode `<Standby>`.
pub const OPCODE_STANDBY: u8 = 0x36;

/// Interprets a CEC command from the bus: the TV saying `<Standby>`
/// (addressed to us or broadcast) means it is going to standby. Everything
/// else is libcec's business.
pub fn classify_command(initiator: i32, opcode: u8) -> Option<CecEvent> {
    (initiator == TV && opcode == OPCODE_STANDBY).then_some(CecEvent::TvStandby)
}

pub(crate) enum Msg {
    Event(CecEvent),
    Stop,
}

/// Where an adapter reports [`CecEvent`]s. Clone freely; safe to call from
/// any thread, including libcec's own, and never blocks.
#[derive(Clone)]
pub struct EventSender(pub(crate) Sender<Msg>);

impl EventSender {
    pub fn send(&self, event: CecEvent) {
        // Fails only once the source has stopped, when nobody cares.
        let _ = self.0.send(Msg::Event(event));
    }
}

impl std::fmt::Debug for EventSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventSender").finish_non_exhaustive()
    }
}

/// An adapter could not be opened.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct CecError(pub String);

/// A connection to the CEC bus.
pub trait CecAdapter: Send + 'static {
    /// Connects, registering on the bus as `config` says, and reports what
    /// happens to `events` until closed. May block for a few seconds while
    /// the adapter is probed. Called again after a failure or
    /// [`CecEvent::ConnectionLost`], so it must work more than once.
    ///
    /// Must not make Marathon the active source unless
    /// `config.claim_on_start`, and must never power the TV on or off.
    fn open(&mut self, config: &CecConfig, events: EventSender) -> Result<(), CecError>;
    /// Disconnects. Called only after a successful `open`.
    fn close(&mut self);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_tv_saying_standby_is_tv_standby() {
        assert_eq!(
            classify_command(TV, OPCODE_STANDBY),
            Some(CecEvent::TvStandby)
        );
        // Another device going to standby is not the TV.
        assert_eq!(classify_command(5, OPCODE_STANDBY), None);
        // Other messages from the TV.
        assert_eq!(classify_command(TV, 0x82), None);
        assert_eq!(classify_command(TV, 0x8F), None);
    }
}
