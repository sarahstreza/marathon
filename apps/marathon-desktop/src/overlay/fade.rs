//! Fade timing for overlay elements: opacity as a function of time,
//! independent of rendering (PLAN.md §9: fade in ~150ms, hold ~4s, fade out
//! ~300ms, eased).
//!
//! A [`Fade`] is shown, optionally for a limited hold, and hidden. Showing
//! again while visible or fading out continues from the current opacity
//! rather than popping, and only extends the hold of a fully visible
//! element.

use std::time::{Duration, Instant};

/// How long fading and holding take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    pub fade_in: Duration,
    pub fade_out: Duration,
}

impl Timing {
    /// PLAN.md §9's banner timing.
    pub const STANDARD: Timing = Timing {
        fade_in: Duration::from_millis(150),
        fade_out: Duration::from_millis(300),
    };
}

/// Whether, and how soon, a fade needs drawing again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    /// Opacity is changing: draw every frame.
    Animating,
    /// Opacity is steady until this time.
    At(Instant),
    /// Opacity is steady until something calls `show` or `hide`.
    Idle,
}

impl Wake {
    /// The more urgent of two wakes.
    pub fn min(self, other: Wake) -> Wake {
        match (self, other) {
            (Wake::Animating, _) | (_, Wake::Animating) => Wake::Animating,
            (Wake::At(a), Wake::At(b)) => Wake::At(a.min(b)),
            (Wake::At(t), Wake::Idle) | (Wake::Idle, Wake::At(t)) => Wake::At(t),
            (Wake::Idle, Wake::Idle) => Wake::Idle,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Shown {
    /// When the fade-in started, the opacity it started from, and how long
    /// it takes (less than the full fade-in when starting part way up).
    at: Instant,
    from: f32,
    fade_in: Duration,
    /// When the fade-out starts, if it is scheduled, and the opacity it
    /// starts from.
    hide_at: Option<Instant>,
    hide_from: f32,
}

/// One overlay element's fade.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fade {
    timing: Timing,
    shown: Option<Shown>,
}

impl Fade {
    pub fn new(timing: Timing) -> Self {
        Self {
            timing,
            shown: None,
        }
    }

    /// Shows the element at `now`: fades it in (from wherever it is), then
    /// holds it for `hold`, or until [`Fade::hide`] if `hold` is `None`.
    pub fn show(&mut self, now: Instant, hold: Option<Duration>) {
        let current = self.opacity(now).clamp(0.0, 1.0);
        // From part way up, the fade-in takes its share of the full time,
        // so it moves at the usual speed.
        let fade_in = scale(self.timing.fade_in, 1.0 - current);
        self.shown = Some(Shown {
            at: now,
            from: current,
            fade_in,
            hide_at: hold.map(|hold| now + fade_in + hold),
            hide_from: 1.0,
        });
    }

    /// Starts fading out at `now`, from the current opacity.
    pub fn hide(&mut self, now: Instant) {
        let current = self.opacity(now);
        match &mut self.shown {
            Some(_) if current <= 0.0 => self.shown = None,
            // Not already fading out: start now.
            Some(shown) if shown.hide_at.is_none_or(|h| h > now) => {
                shown.hide_at = Some(now);
                shown.hide_from = current;
            }
            Some(_) | None => {}
        }
    }

    /// Whether the element is shown and not fading out (it may be fading
    /// in).
    pub fn is_shown(&self, now: Instant) -> bool {
        self.shown
            .is_some_and(|s| s.hide_at.is_none_or(|h| now < h))
    }

    /// Opacity at `now`, from 0 (invisible) to 1.
    pub fn opacity(&self, now: Instant) -> f32 {
        let Some(s) = self.shown else {
            return 0.0;
        };
        if let Some(hide_at) = s.hide_at
            && now >= hide_at
        {
            let t = fraction(now - hide_at, self.timing.fade_out);
            return s.hide_from * (1.0 - ease_in_out(t));
        }
        let t = fraction(now.saturating_duration_since(s.at), s.fade_in);
        s.from + (1.0 - s.from) * ease_out(t)
    }

    /// When this element next needs drawing.
    pub fn wake(&self, now: Instant) -> Wake {
        let Some(s) = self.shown else {
            return Wake::Idle;
        };
        match s.hide_at {
            Some(h) if now >= h => {
                if now < h + self.timing.fade_out {
                    Wake::Animating
                } else {
                    Wake::Idle
                }
            }
            _ if now < s.at + s.fade_in => Wake::Animating,
            Some(h) => Wake::At(h),
            None => Wake::Idle,
        }
    }
}

/// `duration × factor`, rounded to the nanosecond (so a factor of 1 is
/// exact, unlike `Duration::mul_f32`).
fn scale(duration: Duration, factor: f32) -> Duration {
    let nanos = duration.as_nanos() as f64 * f64::from(factor.clamp(0.0, 1.0));
    Duration::from_nanos(nanos.round() as u64)
}

/// `elapsed / total`, clamped to 0..=1 (1 if `total` is zero).
fn fraction(elapsed: Duration, total: Duration) -> f32 {
    if total.is_zero() {
        1.0
    } else {
        (elapsed.as_secs_f32() / total.as_secs_f32()).clamp(0.0, 1.0)
    }
}

/// Cubic ease-out: fast start, gentle landing.
fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t).powi(3)
}

/// Cubic ease-in-out.
fn ease_in_out(t: f32) -> f32 {
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOLD: Duration = Duration::from_secs(4);

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn hidden_until_shown() {
        let fade = Fade::new(Timing::STANDARD);
        let t = Instant::now();
        assert_eq!(fade.opacity(t), 0.0);
        assert_eq!(fade.wake(t), Wake::Idle);
        assert!(!fade.is_shown(t));
    }

    #[test]
    fn fades_in_holds_and_fades_out() {
        let mut fade = Fade::new(Timing::STANDARD);
        let t0 = Instant::now();
        fade.show(t0, Some(HOLD));

        assert_eq!(fade.opacity(t0), 0.0);
        assert_eq!(fade.wake(t0), Wake::Animating);
        let mid_in = fade.opacity(t0 + ms(75));
        // Eased out: past halfway at half time.
        assert!(mid_in > 0.5 && mid_in < 1.0, "{mid_in}");
        assert_eq!(fade.opacity(t0 + ms(150)), 1.0);

        // Holding: steady, and asleep until the fade-out.
        let out_at = t0 + ms(150) + HOLD;
        assert_eq!(fade.opacity(t0 + ms(2000)), 1.0);
        assert_eq!(fade.wake(t0 + ms(2000)), Wake::At(out_at));
        assert!(fade.is_shown(t0 + ms(2000)));

        assert_eq!(fade.opacity(out_at), 1.0);
        assert_eq!(fade.wake(out_at), Wake::Animating);
        assert!(!fade.is_shown(out_at));
        assert!(close(fade.opacity(out_at + ms(150)), 0.5));
        assert_eq!(fade.opacity(out_at + ms(300)), 0.0);
        assert_eq!(fade.wake(out_at + ms(300)), Wake::Idle);
    }

    #[test]
    fn opacity_is_monotonic_through_each_phase() {
        let mut fade = Fade::new(Timing::STANDARD);
        let t0 = Instant::now();
        fade.show(t0, Some(HOLD));
        let samples: Vec<f32> = (0..=150).map(|i| fade.opacity(t0 + ms(i))).collect();
        assert!(samples.windows(2).all(|w| w[0] <= w[1]), "fade-in rises");
        let out_at = t0 + ms(150) + HOLD;
        let samples: Vec<f32> = (0..=300).map(|i| fade.opacity(out_at + ms(i))).collect();
        assert!(samples.windows(2).all(|w| w[0] >= w[1]), "fade-out falls");
    }

    #[test]
    fn showing_again_while_visible_extends_the_hold_without_a_flicker() {
        let mut fade = Fade::new(Timing::STANDARD);
        let t0 = Instant::now();
        fade.show(t0, Some(HOLD));
        let t1 = t0 + ms(3000);
        fade.show(t1, Some(HOLD));
        assert_eq!(fade.opacity(t1), 1.0);
        assert_eq!(fade.wake(t1), Wake::At(t1 + HOLD));
    }

    #[test]
    fn showing_during_the_fade_out_fades_back_in_from_there() {
        let mut fade = Fade::new(Timing::STANDARD);
        let t0 = Instant::now();
        fade.show(t0, Some(ms(0)));
        let t1 = t0 + ms(150) + ms(150); // halfway out
        let mid = fade.opacity(t1);
        assert!(close(mid, 0.5), "{mid}");
        fade.show(t1, Some(HOLD));
        assert!(close(fade.opacity(t1), mid), "no jump");
        assert!(fade.opacity(t1 + ms(30)) > mid);
        // The rest of the fade-in takes the remaining share of 150ms.
        assert!(fade.opacity(t1 + ms(70)) < 1.0);
        assert_eq!(fade.opacity(t1 + ms(76)), 1.0);
        let Wake::At(out_at) = fade.wake(t1 + ms(100)) else {
            panic!("should be holding");
        };
        let expected = t1 + ms(75) + HOLD;
        assert!(out_at.max(expected) - out_at.min(expected) < ms(1));
    }

    #[test]
    fn held_until_hidden() {
        let mut fade = Fade::new(Timing::STANDARD);
        let t0 = Instant::now();
        fade.show(t0, None);
        let later = t0 + Duration::from_secs(600);
        assert_eq!(fade.opacity(later), 1.0);
        assert_eq!(fade.wake(later), Wake::Idle);
        fade.hide(later);
        assert_eq!(fade.wake(later), Wake::Animating);
        assert_eq!(fade.opacity(later + ms(300)), 0.0);
    }

    #[test]
    fn hiding_during_the_fade_in_fades_out_from_there() {
        let mut fade = Fade::new(Timing::STANDARD);
        let t0 = Instant::now();
        fade.show(t0, None);
        let t1 = t0 + ms(50);
        let partial = fade.opacity(t1);
        fade.hide(t1);
        assert!(close(fade.opacity(t1), partial));
        assert!(fade.opacity(t1 + ms(150)) < partial);
        assert_eq!(fade.opacity(t1 + ms(300)), 0.0);
        // Hiding again does not restart the fade-out.
        fade.hide(t1 + ms(100));
        assert_eq!(fade.opacity(t1 + ms(300)), 0.0);
    }

    #[test]
    fn hiding_something_hidden_is_a_no_op() {
        let mut fade = Fade::new(Timing::STANDARD);
        let t0 = Instant::now();
        fade.hide(t0);
        assert_eq!(fade.wake(t0), Wake::Idle);
        assert_eq!(fade.opacity(t0), 0.0);
    }

    #[test]
    fn wakes_combine_to_the_most_urgent() {
        let t = Instant::now();
        let later = t + ms(10);
        assert_eq!(Wake::Idle.min(Wake::At(t)), Wake::At(t));
        assert_eq!(Wake::At(later).min(Wake::At(t)), Wake::At(t));
        assert_eq!(Wake::At(t).min(Wake::Animating), Wake::Animating);
        assert_eq!(Wake::Idle.min(Wake::Idle), Wake::Idle);
    }
}
