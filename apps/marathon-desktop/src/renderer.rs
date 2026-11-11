//! Draws mpv's video (and, from M5, the overlay) into any [`Surface`].
//! Knows nothing about windows or input.

use anyhow::Context as _;
use glow::HasContext;
use marathon_mpv::Mpv;
use marathon_mpv::render::{GlParams, RenderContext};

use crate::surface::{NativeDisplay, Surface};

/// Renders frames. Field order matters: mpv's render context is dropped
/// before the GL function table.
pub struct Renderer {
    mpv: RenderContext,
    gl: glow::Context,
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
        let gl = unsafe { glow::Context::from_loader_function_cstr(|name| loader(name)) };
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
        tracing::info!(
            gl = ?gl.version(),
            // SAFETY: a plain GL query on the current context.
            renderer = unsafe { gl.get_parameter_string(glow::RENDERER) },
            size = ?surface.drawable_size(),
            scale = surface.scale_factor(),
            "renderer ready"
        );
        Ok(Self { mpv: render, gl })
    }

    /// Call after `on_frame` fired: whether a new frame should be drawn.
    pub fn frame_due(&self) -> bool {
        self.mpv.update()
    }

    /// Draws the current frame into `surface` and presents it.
    pub fn draw(&self, surface: &dyn Surface) {
        let (w, h) = surface.drawable_size();
        let (w, h) = (
            i32::try_from(w).unwrap_or(i32::MAX),
            i32::try_from(h).unwrap_or(i32::MAX),
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
        if let Err(err) = surface.present() {
            tracing::warn!(%err, "present failed");
        }
        self.mpv.report_swap();
    }
}
