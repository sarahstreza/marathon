//! Typing a channel number on a number pad, shared by every input with
//! digit keys (keyboard, TV remote).
//!
//! Digits are buffered; the number is committed as
//! [`Action::GoToChannel`] when the input's Enter key is pressed
//! ([`ChannelNumberEntry::commit`]) or [`DIGIT_TIMEOUT`] passes without
//! another digit (call [`ChannelNumberEntry::tick`] by
//! [`ChannelNumberEntry::deadline`]). Inputs discard pending digits on any
//! other key ([`ChannelNumberEntry::clear`]).

use std::time::{Duration, Instant};

use marathon_api::Action;

/// How long after the last digit a channel number is committed.
pub const DIGIT_TIMEOUT: Duration = Duration::from_millis(1500);

/// Longest channel number that can be typed.
const MAX_DIGITS: usize = 4;

/// Digits typed so far. See the module docs.
#[derive(Debug, Default)]
pub struct ChannelNumberEntry {
    digits: String,
    last_digit: Option<Instant>,
}

impl ChannelNumberEntry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds digit `d` (0–9) typed at `now`. Digits past the fourth are
    /// ignored, but still restart the timeout.
    pub fn digit(&mut self, d: u8, now: Instant) {
        if d <= 9 && self.digits.len() < MAX_DIGITS {
            self.digits.push(char::from(b'0' + d));
        }
        self.last_digit = Some(now);
    }

    /// Commits the pending number, if any.
    pub fn commit(&mut self) -> Option<Action> {
        let number = self.digits.parse().ok();
        self.clear();
        number.map(|number| Action::GoToChannel { number })
    }

    /// Discards pending digits.
    pub fn clear(&mut self) {
        self.digits.clear();
        self.last_digit = None;
    }

    /// When pending digits will be committed, if any are pending.
    pub fn deadline(&self) -> Option<Instant> {
        self.last_digit.map(|t| t + DIGIT_TIMEOUT)
    }

    /// Commits pending digits if their timeout has passed.
    pub fn tick(&mut self, now: Instant) -> Option<Action> {
        match self.deadline() {
            Some(deadline) if now >= deadline => self.commit(),
            _ => None,
        }
    }

    /// The digits typed so far, for display.
    pub fn pending(&self) -> &str {
        &self.digits
    }

    pub fn is_empty(&self) -> bool {
        self.digits.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_accumulate_commit_and_cap() {
        let now = Instant::now();
        let mut entry = ChannelNumberEntry::new();
        assert!(entry.is_empty());
        assert_eq!(entry.commit(), None);
        for d in [1, 2, 3, 4, 5] {
            entry.digit(d, now);
        }
        assert_eq!(entry.pending(), "1234");
        assert_eq!(entry.commit(), Some(Action::GoToChannel { number: 1234 }));
        assert!(entry.is_empty());
        assert_eq!(entry.deadline(), None);
    }

    #[test]
    fn the_timeout_commits_and_clear_discards() {
        let t0 = Instant::now();
        let mut entry = ChannelNumberEntry::new();
        entry.digit(4, t0);
        entry.digit(2, t0 + DIGIT_TIMEOUT / 2);
        let deadline = entry.deadline().unwrap();
        assert_eq!(deadline, t0 + DIGIT_TIMEOUT / 2 + DIGIT_TIMEOUT);
        assert_eq!(entry.tick(deadline - Duration::from_millis(1)), None);
        assert_eq!(
            entry.tick(deadline),
            Some(Action::GoToChannel { number: 42 })
        );
        entry.digit(7, t0);
        entry.clear();
        assert_eq!(entry.tick(t0 + DIGIT_TIMEOUT * 2), None);
    }
}
