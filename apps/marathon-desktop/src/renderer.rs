//! Draws mpv's video and the overlay into any [`Surface`]. Knows nothing
//! about windows or input.
//!
//! Each frame (PLAN.md §9 "Render loop"): mpv renders the video into the
//! default framebuffer, egui paints the overlay on top if any of it is
//! visible, the surface presents, and mpv is told about the swap. With
//! the overlay hidden, egui does no work at all, so plain playback costs
//! what mpv alone does.

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context as _, anyhow};
use glow::HasContext;
use marathon_mpv::Mpv;
use marathon_mpv::render::{GlParams, RenderContext};

use crate::overlay::{self, Overlay};
use crate::surface::{NativeDisplay, Surface};

/// Renders frames. Field order matters: mpv's render context is dropped
/// before the GL function table (and [`Drop`] frees the overlay's GL
/// objects before either).
pub struct Renderer {
    mpv: RenderContext,
    egui: egui::Context,
    painter: egui_glow::Painter,
    /// Whether the last frame drawn included the overlay.
    overlay_on_screen: bool,
    gl: Arc<glow::Context>,
}

impl Renderer {
    /// Sets up rendering into `surface`. `on_frame` is called from mpv's
    /// threads whenever a new frame may be due; it must only wake the
    /// render thread, which then calls [`Renderer::frame_due`].
    ///
    /// # Safety
    ///
    /// `surface`'s GL context must be current on this thread, now and for
    /// every later call on the returned renderer, including dropping it,
    /// and the surface must outlive the renderer.
    pub unsafe fn new(
        surface: &dyn Surface,
        mpv: &Mpv,
        on_frame: impl Fn() + Send + Sync + 'static,
    ) -> anyhow::Result<Self> {
        let loader = surface.gl_loader();
        // SAFETY: the context is current (this function's contract).
        let gl = Arc::new(unsafe { glow::Context::from_loader_function_cstr(|name| loader(name)) });
        let (x11_display, wayland_display) = match surface.native_display() {
            NativeDisplay::X11(d) => (Some(d), None),
            NativeDisplay::Wayland(d) => (None, Some(d)),
            NativeDisplay::None => (None, None),
        };
        let params = GlParams {
            get_proc_address: surface.gl_loader(),
            x11_display,
            wayland_display,
        };
        // SAFETY: forwarded from this function's contract; the display
        // pointers belong to the surface, which outlives the renderer.
        let mut render = unsafe { RenderContext::new_gl(mpv, params) }
            .context("cannot create mpv's render context")?;
        render.set_update_callback(on_frame);
        let painter = egui_glow::Painter::new(gl.clone(), "", None, true)
            .map_err(|e| anyhow!("cannot set up the overlay painter: {e}"))?;
        tracing::info!(
            gl = ?gl.version(),
            // SAFETY: a plain GL query on the current context.
            renderer = unsafe { gl.get_parameter_string(glow::RENDERER) },
            size = ?surface.drawable_size(),
            scale = surface.scale_factor(),
            "renderer ready"
        );
        Ok(Self {
            mpv: render,
            egui: egui::Context::default(),
            painter,
            overlay_on_screen: false,
            gl,
        })
    }

    /// Call after `on_frame` fired: whether a new frame should be drawn.
    pub fn frame_due(&self) -> bool {
        self.mpv.update()
    }

    /// Whether the last frame drawn showed (some of) the overlay. If the
    /// overlay has since become invisible, one more frame is needed to
    /// clear it.
    pub fn overlay_on_screen(&self) -> bool {
        self.overlay_on_screen
    }

    /// Draws the current video frame and, if visible, the overlay as of
    /// `now` into `surface`, and presents it.
    pub fn draw(&mut self, surface: &dyn Surface, overlay: &Overlay, now: Instant) {
        let (width, height) = surface.drawable_size();
        let (w, h) = (
            i32::try_from(width).unwrap_or(i32::MAX),
            i32::try_from(height).unwrap_or(i32::MAX),
        );
        // SAFETY: plain GL calls on the current context.
        unsafe {
            self.gl.viewport(0, 0, w, h);
            self.gl.clear_color(0.0, 0.0, 0.0, 1.0);
            self.gl.clear(glow::COLOR_BUFFER_BIT);
        }
        if let Err(err) = self.mpv.render(0, w, h, true) {
            tracing::warn!(%err, "mpv render failed");
        }
        self.overlay_on_screen = overlay.visible(now);
        if self.overlay_on_screen {
            self.paint_overlay(width, height, overlay, now);
        }
        if let Err(err) = surface.present() {
            tracing::warn!(%err, "present failed");
        }
        self.mpv.report_swap();
    }

    fn paint_overlay(&mut self, width: u32, height: u32, overlay: &Overlay, now: Instant) {
        let input = overlay::raw_input(width, height, self.painter.max_texture_side());
        let output = self.egui.run_pass(input, |ctx| overlay.draw(ctx, now));
        let primitives = self.egui.tessellate(output.shapes, output.pixels_per_point);
        let mut textures = output.textures_delta;
        self.painter.paint_and_update_textures(
            [width, height],
            output.pixels_per_point,
            &primitives,
            &mut textures,
        );
        // egui leaves scissoring and blending on; give mpv back the default
        // state it expects for the next frame.
        // SAFETY: plain GL calls on the current context.
        unsafe {
            self.gl.disable(glow::SCISSOR_TEST);
            self.gl.disable(glow::BLEND);
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        // Frees the overlay's GL objects while the context is still current
        // (this type's contract).
        self.painter.destroy();
    }
}
