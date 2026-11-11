//! What Marathon does when the TV switches inputs or goes to standby
//! (PLAN.md M7, "polite input 4").
//!
//! - When the TV switches away from Marathon's input, or goes to standby:
//!   [`Action::Standby`], which pauses and saves the position.
//! - When the TV switches to Marathon's input: [`Action::Wake`], which
//!   resumes what standby paused, if [`Lifecycle::resume_on_select`] is set
//!   (the default). Without it, playback stays paused until the viewer
//!   presses play.
//!
//! Only changes count: libcec repeats itself (a TV may announce the same
//! routing twice), and a "you're active" at startup, when Marathon never
//! went quiet, needs no wake. Marathon never asks to become the active
//! source itself here; that is [`CecConfig::claim_on_start`]'s business,
//! and off by default.
//!
//! [`CecConfig::claim_on_start`]: crate::CecConfig::claim_on_start

use marathon_api::Action;

/// Something the TV did, as far as Marathon is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TvEvent {
    /// The TV routed to Marathon's input (true), or away from it (false).
    Selected(bool),
    /// The TV went to standby.
    Standby,
    /// The CEC connection was lost; whatever Marathon knew about the TV no
    /// longer holds.
    ConnectionLost,
}

/// The lifecycle state machine. See the module docs.
#[derive(Debug, Clone)]
pub struct Lifecycle {
    /// Whether switching back to Marathon's input resumes playback.
    pub resume_on_select: bool,
    /// Whether Marathon sent [`Action::Standby`] and has not been selected
    /// since.
    quiet: bool,
}

impl Lifecycle {
    pub fn new(resume_on_select: bool) -> Self {
        Self {
            resume_on_select,
            quiet: false,
        }
    }

    /// The action for `event`, if any.
    pub fn on(&mut self, event: TvEvent) -> Option<Action> {
        match event {
            TvEvent::Selected(true) => {
                let was_quiet = std::mem::replace(&mut self.quiet, false);
                // Even without resuming, being selected ends this quiet
                // spell, so the next switch away pauses again (the viewer
                // may have pressed play meanwhile).
                (was_quiet && self.resume_on_select).then_some(Action::Wake)
            }
            TvEvent::Selected(false) | TvEvent::Standby => {
                (!std::mem::replace(&mut self.quiet, true)).then_some(Action::Standby)
            }
            // Keep playing: a flaky adapter is no reason to stop. The
            // state is unknown until the TV next says something.
            TvEvent::ConnectionLost => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switching_away_pauses_and_back_resumes() {
        let mut life = Lifecycle::new(true);
        assert_eq!(life.on(TvEvent::Selected(false)), Some(Action::Standby));
        assert_eq!(life.on(TvEvent::Selected(true)), Some(Action::Wake));
        // TV standby, then turned on again on Marathon's input.
        assert_eq!(life.on(TvEvent::Standby), Some(Action::Standby));
        assert_eq!(life.on(TvEvent::Selected(true)), Some(Action::Wake));
    }

    #[test]
    fn being_selected_without_having_gone_quiet_does_nothing() {
        // E.g. at startup, with the TV already on Marathon's input.
        let mut life = Lifecycle::new(true);
        assert_eq!(life.on(TvEvent::Selected(true)), None);
        assert_eq!(life.on(TvEvent::Selected(true)), None);
    }

    #[test]
    fn repeats_are_ignored() {
        let mut life = Lifecycle::new(true);
        assert_eq!(life.on(TvEvent::Selected(false)), Some(Action::Standby));
        assert_eq!(life.on(TvEvent::Selected(false)), None);
        // Switched away, then the TV went to standby: still one standby.
        assert_eq!(life.on(TvEvent::Standby), None);
        assert_eq!(life.on(TvEvent::Selected(true)), Some(Action::Wake));
        assert_eq!(life.on(TvEvent::Selected(true)), None);
    }

    #[test]
    fn without_resume_on_select_switching_back_stays_paused() {
        let mut life = Lifecycle::new(false);
        assert_eq!(life.on(TvEvent::Selected(false)), Some(Action::Standby));
        assert_eq!(life.on(TvEvent::Selected(true)), None);
        // The next switch away still pauses (the viewer may have resumed).
        assert_eq!(life.on(TvEvent::Selected(false)), Some(Action::Standby));
    }

    #[test]
    fn losing_the_connection_changes_nothing() {
        let mut life = Lifecycle::new(true);
        assert_eq!(life.on(TvEvent::ConnectionLost), None);
        assert_eq!(life.on(TvEvent::Selected(false)), Some(Action::Standby));
        assert_eq!(life.on(TvEvent::ConnectionLost), None);
        assert_eq!(life.on(TvEvent::Selected(true)), Some(Action::Wake));
    }
}
