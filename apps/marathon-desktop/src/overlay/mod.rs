//! The on-screen overlay (PLAN.md §9 "Overlay UI"): the channel banner,
//! the guide, and error toasts, drawn with egui over the video.
//!
//! The overlay is a model plus a drawing function, and knows nothing about
//! windows or GL. The [`Renderer`](crate::renderer::Renderer) paints it;
//! the platform layer asks it when to redraw ([`Overlay::wake`]).
//!
//! Its input arrives as [`OverlayMsg`]s from the tuner thread through an
//! [`OverlayFeed`], which also wakes the render thread:
//!
//! - the tuner's [`UiEvent`] cues (banner, guide);
//! - playback [`Progress`] observed from mpv, for the banner's progress
//!   bar, extrapolated between updates so it moves smoothly;
//! - error messages, shown as toasts.
//!
//! Only this module and [`draw`] are egui-specific; replacing the toolkit
//! (PLAN.md §12) means replacing them.

mod draw;
pub mod fade;

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use marathon_client::ui::{Banner, Guide, UiEvent};

use self::fade::{Fade, Timing, Wake};

/// How long the banner stays up after a tune or `ShowInfo`.
pub const BANNER_HOLD: Duration = Duration::from_secs(4);
/// How long an error toast stays up.
pub const TOAST_HOLD: Duration = Duration::from_secs(6);

/// The overlay's layout is in points on a screen this many points tall,
/// so it scales with the display and reads the same from across a room at
/// any resolution.
pub const SCREEN_HEIGHT_POINTS: f32 = 540.0;

/// Physical pixels per point for a drawable `height` pixels tall.
pub fn pixels_per_point(height: u32) -> f32 {
    (height as f32 / SCREEN_HEIGHT_POINTS).max(0.25)
}

/// egui's input for a frame on a drawable of `width` × `height` pixels.
/// The overlay takes no pointer or key input (that arrives as actions), so
/// only the screen size and scale matter.
pub fn raw_input(width: u32, height: u32, max_texture_side: usize) -> egui::RawInput {
    let ppp = pixels_per_point(height);
    let mut input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(width as f32, height as f32) / ppp,
        )),
        max_texture_side: Some(max_texture_side),
        ..Default::default()
    };
    input
        .viewports
        .entry(egui::ViewportId::ROOT)
        .or_default()
        .native_pixels_per_point = Some(ppp);
    input
}

/// Where playback is, as last observed.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Progress {
    pub position_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    pub paused: bool,
}

/// Input to the overlay.
#[derive(Debug, Clone, PartialEq)]
pub enum OverlayMsg {
    Ui(UiEvent),
    Progress(Progress),
    /// Something went wrong; tell the viewer briefly.
    Toast(String),
}

/// Sends messages to the overlay and wakes the render thread. Clone
/// freely.
#[derive(Clone)]
pub struct OverlayFeed {
    tx: Sender<OverlayMsg>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl OverlayFeed {
    pub fn send(&self, msg: OverlayMsg) {
        // Fails only once the overlay is gone, at shutdown.
        if self.tx.send(msg).is_ok() {
            (self.wake)();
        }
    }
}

/// The overlay's state. Lives on the render thread.
pub struct Overlay {
    inbox: Receiver<OverlayMsg>,
    /// The banner's contents; kept while it fades out.
    banner: Option<Banner>,
    /// A switch is under way and the new item is not playing yet.
    tuning: bool,
    banner_fade: Fade,
    /// The guide's contents; kept while it fades out.
    guide: Option<Guide>,
    guide_fade: Fade,
    toast: Option<String>,
    toast_fade: Fade,
    progress: Progress,
    /// When `progress` was observed.
    progress_at: Instant,
}

impl Overlay {
    /// The overlay, and the feed that drives it. `wake` is called (on the
    /// sending thread) after every message, and must get the render thread
    /// to call [`Overlay::pump`].
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> (Self, OverlayFeed) {
        let (tx, inbox) = mpsc::channel();
        let fade = Fade::new(Timing::STANDARD);
        let overlay = Self {
            inbox,
            banner: None,
            tuning: false,
            banner_fade: fade,
            guide: None,
            guide_fade: fade,
            toast: None,
            toast_fade: fade,
            progress: Progress::default(),
            progress_at: Instant::now(),
        };
        let feed = OverlayFeed {
            tx,
            wake: Arc::new(wake),
        };
        (overlay, feed)
    }

    /// Applies waiting messages. Returns whether the screen needs a redraw
    /// because of them.
    pub fn pump(&mut self, now: Instant) -> bool {
        let mut redraw = false;
        while let Ok(msg) = self.inbox.try_recv() {
            redraw |= self.apply(msg, now);
        }
        redraw
    }

    fn apply(&mut self, msg: OverlayMsg, now: Instant) -> bool {
        match msg {
            OverlayMsg::Ui(cue) => self.cue(cue, now),
            OverlayMsg::Progress(progress) => {
                self.progress = progress;
                self.progress_at = now;
                // Video frames redraw the banner while playing; this
                // matters when paused (e.g. a seek while paused).
                self.banner_fade.opacity(now) > 0.0
            }
            OverlayMsg::Toast(message) => {
                self.toast = Some(message);
                self.toast_fade.show(now, Some(TOAST_HOLD));
                true
            }
        }
    }

    fn cue(&mut self, cue: UiEvent, now: Instant) -> bool {
        match cue {
            UiEvent::Tuning(banner) => {
                self.banner = Some(banner);
                self.tuning = true;
                self.progress = Progress::default();
                self.show_banner(now);
            }
            UiEvent::Playing { banner, announce } => {
                let changed = self.banner.as_ref() != Some(&banner);
                if changed {
                    self.progress = Progress::default();
                }
                self.banner = Some(banner);
                self.tuning = false;
                if announce {
                    self.show_banner(now);
                }
            }
            UiEvent::Stopped => {
                self.tuning = false;
                self.progress = Progress::default();
            }
            UiEvent::ShowInfo => {
                if self.banner.is_some() && !self.guide_fade.is_shown(now) {
                    self.show_banner(now);
                }
            }
            UiEvent::Hide => {
                self.banner_fade.hide(now);
                self.toast_fade.hide(now);
            }
            UiEvent::Guide(Some(guide)) => {
                self.guide = Some(guide);
                if !self.guide_fade.is_shown(now) {
                    self.guide_fade.show(now, None);
                }
                // The guide says what everything is on; the banner would
                // only cover it.
                self.banner_fade.hide(now);
            }
            UiEvent::Guide(None) => self.guide_fade.hide(now),
        }
        true
    }

    fn show_banner(&mut self, now: Instant) {
        if !self.guide_fade.is_shown(now) {
            self.banner_fade.show(now, Some(BANNER_HOLD));
        }
    }

    /// Whether anything is (even faintly) on screen at `now`.
    pub fn visible(&self, now: Instant) -> bool {
        self.fades().iter().any(|f| f.opacity(now) > 0.0)
    }

    /// When the overlay next needs drawing, ignoring video frames.
    pub fn wake(&self, now: Instant) -> Wake {
        self.fades()
            .iter()
            .fold(Wake::Idle, |wake, f| wake.min(f.wake(now)))
    }

    fn fades(&self) -> [&Fade; 3] {
        [&self.banner_fade, &self.guide_fade, &self.toast_fade]
    }

    /// Playback position at `now`: the last observation, moved on by the
    /// time since if playing.
    fn position_at(&self, now: Instant) -> Option<u64> {
        let p = &self.progress;
        let mut position = p.position_ms?;
        if !p.paused {
            let elapsed = now.saturating_duration_since(self.progress_at);
            position += u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        }
        Some(match p.duration_ms {
            Some(d) => position.min(d),
            None => position,
        })
    }

    /// Draws the overlay as it is at `now`.
    pub fn draw(&self, ctx: &egui::Context, now: Instant) {
        draw::draw(self, ctx, now);
    }
}

#[cfg(test)]
mod tests {
    use marathon_client::ui::GuideRow;

    use super::*;

    fn banner(n: u32) -> Banner {
        Banner {
            channel_number: n,
            channel_name: format!("Channel {n}"),
            title: Some("S01E01 · Pilot".into()),
            show: None,
        }
    }

    fn guide() -> Guide {
        Guide {
            rows: vec![GuideRow {
                number: 1,
                name: "One".into(),
                on: None,
                current: true,
            }],
            highlighted: 0,
        }
    }

    fn overlay() -> (Overlay, OverlayFeed, Arc<std::sync::atomic::AtomicUsize>) {
        let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = wakes.clone();
        let (overlay, feed) = Overlay::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        (overlay, feed, wakes)
    }

    #[test]
    fn sending_wakes_the_render_thread() {
        let (mut overlay, feed, wakes) = overlay();
        feed.send(OverlayMsg::Ui(UiEvent::Tuning(banner(1))));
        assert_eq!(wakes.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(overlay.pump(Instant::now()));
    }

    #[test]
    fn tuning_shows_the_banner_at_once_and_it_goes_away_by_itself() {
        let (mut overlay, feed, _) = overlay();
        let t0 = Instant::now();
        assert!(!overlay.visible(t0));
        feed.send(OverlayMsg::Ui(UiEvent::Tuning(banner(2))));
        overlay.pump(t0);
        assert!(overlay.tuning);
        assert_eq!(overlay.wake(t0), Wake::Animating);
        assert!(overlay.visible(t0 + Duration::from_millis(1)));

        feed.send(OverlayMsg::Ui(UiEvent::Playing {
            banner: banner(2),
            announce: true,
        }));
        overlay.pump(t0 + Duration::from_millis(500));
        assert!(!overlay.tuning);
        let gone = t0 + Duration::from_millis(500) + BANNER_HOLD + Duration::from_secs(1);
        assert!(!overlay.visible(gone));
        assert_eq!(overlay.wake(gone), Wake::Idle);
    }

    #[test]
    fn automatic_advances_do_not_pop_the_banner_up() {
        let (mut overlay, feed, _) = overlay();
        let t0 = Instant::now();
        feed.send(OverlayMsg::Ui(UiEvent::Playing {
            banner: banner(1),
            announce: false,
        }));
        overlay.pump(t0);
        assert!(!overlay.visible(t0 + Duration::from_secs(1)));
        assert_eq!(overlay.banner, Some(banner(1)));
        // But info shows it.
        feed.send(OverlayMsg::Ui(UiEvent::ShowInfo));
        overlay.pump(t0);
        assert!(overlay.visible(t0 + Duration::from_millis(100)));
    }

    #[test]
    fn the_guide_stays_until_closed_and_hides_the_banner() {
        let (mut overlay, feed, _) = overlay();
        let t0 = Instant::now();
        feed.send(OverlayMsg::Ui(UiEvent::Tuning(banner(1))));
        feed.send(OverlayMsg::Ui(UiEvent::Guide(Some(guide()))));
        overlay.pump(t0);
        let later = t0 + Duration::from_secs(60);
        assert!(overlay.visible(later));
        assert_eq!(overlay.banner_fade.opacity(later), 0.0);
        // Info while the guide is open does not stack the banner on it.
        feed.send(OverlayMsg::Ui(UiEvent::ShowInfo));
        overlay.pump(later);
        assert_eq!(
            overlay.banner_fade.opacity(later + Duration::from_secs(1)),
            0.0
        );

        feed.send(OverlayMsg::Ui(UiEvent::Guide(None)));
        overlay.pump(later);
        assert!(overlay.guide.is_some(), "kept for the fade-out");
        assert!(!overlay.visible(later + Duration::from_secs(1)));
    }

    #[test]
    fn progress_moves_on_while_playing_and_stops_when_paused() {
        let (mut overlay, feed, _) = overlay();
        let t0 = Instant::now();
        let progress = |paused| Progress {
            position_ms: Some(10_000),
            duration_ms: Some(10_500),
            paused,
        };
        feed.send(OverlayMsg::Progress(progress(false)));
        overlay.pump(t0);
        assert_eq!(overlay.position_at(t0), Some(10_000));
        assert_eq!(
            overlay.position_at(t0 + Duration::from_millis(250)),
            Some(10_250)
        );
        assert_eq!(
            overlay.position_at(t0 + Duration::from_secs(5)),
            Some(10_500),
            "never past the end"
        );
        feed.send(OverlayMsg::Progress(progress(true)));
        overlay.pump(t0);
        assert_eq!(
            overlay.position_at(t0 + Duration::from_secs(5)),
            Some(10_000)
        );
    }

    #[test]
    fn a_new_channel_forgets_the_old_progress() {
        let (mut overlay, feed, _) = overlay();
        let t0 = Instant::now();
        feed.send(OverlayMsg::Progress(Progress {
            position_ms: Some(5_000),
            duration_ms: Some(60_000),
            paused: false,
        }));
        feed.send(OverlayMsg::Ui(UiEvent::Tuning(banner(3))));
        overlay.pump(t0);
        assert_eq!(overlay.position_at(t0), None);
    }

    #[test]
    fn toasts_show_and_back_hides_everything() {
        let (mut overlay, feed, _) = overlay();
        let t0 = Instant::now();
        feed.send(OverlayMsg::Toast("cannot play".into()));
        feed.send(OverlayMsg::Ui(UiEvent::Tuning(banner(1))));
        overlay.pump(t0);
        let t1 = t0 + Duration::from_secs(1);
        assert!(overlay.toast_fade.opacity(t1) > 0.0);
        feed.send(OverlayMsg::Ui(UiEvent::Hide));
        overlay.pump(t1);
        assert!(!overlay.visible(t1 + Duration::from_secs(1)));
    }

    /// egui draws text only with fonts, which it bundles only with its
    /// `default_fonts` feature. Without them it logs an error at runtime
    /// and the overlay has no text.
    #[test]
    fn egui_has_fonts() {
        let fonts = egui::FontDefinitions::default();
        assert!(!fonts.font_data.is_empty(), "enable egui's default_fonts");
    }

    /// Draws every part of the overlay with egui, headless.
    #[test]
    fn draws_headless() {
        let (mut overlay, feed, _) = overlay();
        let t0 = Instant::now();
        feed.send(OverlayMsg::Ui(UiEvent::Tuning(banner(1))));
        feed.send(OverlayMsg::Progress(Progress {
            position_ms: Some(1_000),
            duration_ms: Some(60_000),
            paused: true,
        }));
        feed.send(OverlayMsg::Toast("cannot play".into()));
        overlay.pump(t0);
        let now = t0 + Duration::from_secs(1);

        let ctx = egui::Context::default();
        let mut output = ctx.run_pass(raw_input(1920, 1080, 4096), |ctx| {
            overlay.draw(ctx, now);
        });
        // No GPU here to upload the font atlas to.
        output.textures_delta.clear();
        assert_eq!(output.pixels_per_point, 2.0);
        let primitives = ctx.tessellate(output.shapes, output.pixels_per_point);
        assert!(!primitives.is_empty());

        // And the guide.
        feed.send(OverlayMsg::Ui(UiEvent::Guide(Some(guide()))));
        overlay.pump(now);
        let mut output = ctx.run_pass(raw_input(1280, 720, 4096), |ctx| {
            overlay.draw(ctx, now + Duration::from_secs(1));
        });
        output.textures_delta.clear();
        assert!(
            !ctx.tessellate(output.shapes, output.pixels_per_point)
                .is_empty()
        );
    }

    #[test]
    fn scale_follows_the_drawable_height() {
        assert_eq!(pixels_per_point(1080), 2.0);
        assert_eq!(pixels_per_point(2160), 4.0);
        assert_eq!(pixels_per_point(0), 0.25);
    }
}
