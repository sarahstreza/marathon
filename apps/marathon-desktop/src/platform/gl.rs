//! A winit window with a glutin OpenGL context, as a [`Surface`].

use std::num::NonZeroU32;
use std::panic::{AssertUnwindSafe, catch_unwind};

use anyhow::{Context as _, anyhow};
use glutin::config::{Config, ConfigTemplateBuilder, GlConfig};
use glutin::context::{
    ContextApi, ContextAttributesBuilder, NotCurrentGlContext, PossiblyCurrentContext,
};
use glutin::display::{Display, GetGlDisplay, GlDisplay};
use glutin::surface::{GlSurface as _, Surface as GlutinSurface, SwapInterval, WindowSurface};
use glutin_winit::{ApiPreference, DisplayBuilder, GlWindow};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle};
use winit::dpi::LogicalSize;
use winit::event_loop::ActiveEventLoop;
use winit::window::{Fullscreen, Window};

use crate::surface::{GlLoader, NativeDisplay, Surface};

/// The window and its GL context, current on the main thread. Fields drop
/// in declaration order: the GL surface and context before the window.
pub struct GlWindowSurface {
    surface: GlutinSurface<WindowSurface>,
    context: PossiblyCurrentContext,
    display: Display,
    window: Window,
}

impl GlWindowSurface {
    /// Opens the window and makes a GL context current for it.
    pub fn create(event_loop: &ActiveEventLoop, fullscreen: bool) -> anyhow::Result<Self> {
        let attributes = Window::default_attributes()
            .with_title("Marathon")
            .with_inner_size(LogicalSize::new(1280.0, 720.0))
            .with_fullscreen(fullscreen.then_some(Fullscreen::Borderless(None)));
        // EGL first: mpv's VA-API interop for hardware decoding needs EGL
        // (on X11 too; GLX interop is gone). macOS uses CGL regardless.
        let builder = DisplayBuilder::new()
            .with_preference(ApiPreference::PreferEgl)
            .with_window_attributes(Some(attributes));
        // glutin-winit's config picker must return a config, so an empty
        // list can only be signalled by panicking; turn that into an error.
        let built = catch_unwind(AssertUnwindSafe(|| {
            builder.build(event_loop, ConfigTemplateBuilder::new(), pick_config)
        }))
        .map_err(|_| anyhow!("no usable OpenGL configuration"))?;
        let (window, config) = built.map_err(|e| anyhow!("cannot create GL display: {e}"))?;
        let window = window.context("no window was created")?;

        let raw_window = window.window_handle()?.as_raw();
        let display = config.display();
        let attrs = ContextAttributesBuilder::new().build(Some(raw_window));
        let gles = ContextAttributesBuilder::new()
            .with_context_api(ContextApi::Gles(None))
            .build(Some(raw_window));
        // SAFETY: `raw_window` belongs to `window`, which outlives the
        // context (see the field order).
        let context = unsafe { display.create_context(&config, &attrs) }
            .or_else(|_| unsafe { display.create_context(&config, &gles) })
            .context("cannot create an OpenGL context")?;

        let surface_attrs = window.build_surface_attributes(Default::default())?;
        // SAFETY: as above, the window outlives the surface.
        let surface = unsafe { display.create_window_surface(&config, &surface_attrs) }
            .context("cannot create a GL surface")?;
        let context = context.make_current(&surface)?;
        if let Err(err) = surface.set_swap_interval(&context, SwapInterval::Wait(NonZeroU32::MIN)) {
            tracing::warn!(%err, "cannot enable vsync");
        }
        Ok(Self {
            surface,
            context,
            display,
            window,
        })
    }

    pub fn window(&self) -> &Window {
        &self.window
    }

    /// Resizes the GL surface to the window's new size.
    pub fn resize(&self, width: u32, height: u32) {
        if let (Some(w), Some(h)) = (NonZeroU32::new(width), NonZeroU32::new(height)) {
            self.surface.resize(&self.context, w, h);
        }
    }

    pub fn toggle_fullscreen(&self) {
        let fullscreen = match self.window.fullscreen() {
            Some(_) => None,
            None => Some(Fullscreen::Borderless(None)),
        };
        self.window.set_fullscreen(fullscreen);
    }
}

impl Surface for GlWindowSurface {
    fn gl_loader(&self) -> GlLoader {
        let display = self.display.clone();
        Box::new(move |name| display.get_proc_address(name))
    }

    fn drawable_size(&self) -> (u32, u32) {
        let size = self.window.inner_size();
        (size.width, size.height)
    }

    fn scale_factor(&self) -> f64 {
        self.window.scale_factor()
    }

    fn present(&self) -> Result<(), String> {
        self.surface
            .swap_buffers(&self.context)
            .map_err(|e| e.to_string())
    }

    fn native_display(&self) -> NativeDisplay {
        match self.window.display_handle().map(|h| h.as_raw()) {
            Ok(RawDisplayHandle::Xlib(h)) => {
                h.display.map_or(NativeDisplay::None, NativeDisplay::X11)
            }
            Ok(RawDisplayHandle::Wayland(h)) => NativeDisplay::Wayland(h.display),
            _ => NativeDisplay::None,
        }
    }
}

/// Prefers configs without multisampling (video needs none).
fn pick_config(configs: Box<dyn Iterator<Item = Config> + '_>) -> Config {
    // See `GlWindowSurface::create`: an empty iterator panics here and
    // becomes an error there.
    #[allow(clippy::expect_used)]
    configs
        .min_by_key(|c| c.num_samples())
        .expect("the GL display offered no configs")
}
