//! When to hide the mouse cursor (PLAN.md §9: hide after inactivity, show
//! on movement). Pure timing; the platform applies it to the window.

use std::time::{Duration, Instant};

/// How long the mouse must be still before the cursor hides.
pub const HIDE_AFTER: Duration = Duration::from_secs(3);

/// Tracks mouse activity. The cursor starts visible.
#[derive(Debug)]
pub struct CursorIdle {
    hidden: bool,
    hide_at: Instant,
}

impl CursorIdle {
    pub fn new(now: Instant) -> Self {
        Self {
            hidden: false,
            hide_at: now + HIDE_AFTER,
        }
    }

    /// The mouse moved (or was used). Returns true if the cursor was
    /// hidden and must be shown.
    pub fn moved(&mut self, now: Instant) -> bool {
        self.hide_at = now + HIDE_AFTER;
        std::mem::replace(&mut self.hidden, false)
    }

    /// Returns true if the cursor should be hidden now (once per idle
    /// spell).
    pub fn tick(&mut self, now: Instant) -> bool {
        if !self.hidden && now >= self.hide_at {
            self.hidden = true;
            true
        } else {
            false
        }
    }

    /// When [`CursorIdle::tick`] next has something to do.
    pub fn deadline(&self) -> Option<Instant> {
        (!self.hidden).then_some(self.hide_at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hides_after_inactivity_and_shows_on_movement() {
        let t0 = Instant::now();
        let mut cursor = CursorIdle::new(t0);
        assert_eq!(cursor.deadline(), Some(t0 + HIDE_AFTER));
        assert!(!cursor.tick(t0 + HIDE_AFTER / 2));

        // Movement postpones hiding, and needs no showing while visible.
        let t1 = t0 + HIDE_AFTER / 2;
        assert!(!cursor.moved(t1));
        assert!(!cursor.tick(t0 + HIDE_AFTER));
        assert!(cursor.tick(t1 + HIDE_AFTER));
        assert!(!cursor.tick(t1 + HIDE_AFTER * 2), "hides once");
        assert_eq!(cursor.deadline(), None, "nothing to wait for while hidden");

        let t2 = t1 + HIDE_AFTER * 3;
        assert!(cursor.moved(t2), "shown again");
        assert_eq!(cursor.deadline(), Some(t2 + HIDE_AFTER));
    }
}
