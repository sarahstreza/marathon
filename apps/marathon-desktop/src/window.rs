//! The window and render loop (PLAN.md §9): one winit window, one glutin
//! OpenGL context, and mpv drawing into it through the render API.
//!
//! - mpv's render update callback posts [`UserEvent::MpvFrame`] through an
//!   `EventLoopProxy`; the handler asks mpv whether a frame is due and, if
//!   so, requests a redraw. mpv is never called from the callback itself.
//! - A redraw renders mpv's frame into the default framebuffer, swaps
//!   (vsynced), and reports the swap to mpv.
//! - The loop waits (`ControlFlow::Wait`) unless a typed channel number is
//!   pending, so it does not spin.
//!
//! Input becomes [`Action`]s via `marathon_client::keyboard`. Fullscreen
//! is handled here; every other action goes to the tuner thread.

use std::ffi::c_void;
use std::num::NonZeroU32;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;
use std::time::Instant;

use anyhow::{Context as _, anyhow};
use glow::HasContext;
use glutin::config::{Config, ConfigTemplateBuilder, GlConfig};
use glutin::context::{
    ContextApi, ContextAttributesBuilder, NotCurrentGlContext, PossiblyCurrentContext,
};
use glutin::display::{GetGlDisplay, GlDisplay};
use glutin::surface::{GlSurface, Surface, SwapInterval, WindowSurface};
use glutin_winit::{ApiPreference, DisplayBuilder, GlWindow};
use marathon_client::Action;
use marathon_client::keyboard::Keyboard;
use marathon_mpv::Mpv;
use marathon_mpv::render::{GlParams, RenderContext};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::window::{Fullscreen, Window, WindowId};

use crate::controller::{Exit, Msg};
use crate::input;

/// Events posted to the event loop from other threads.
#[derive(Debug, Clone, Copy)]
pub enum UserEvent {
    /// mpv may have a new frame (from mpv's render update callback).
    MpvFrame,
    /// The tuner thread stopped on its own (mpv shut down).
    TunerStopped,
}

/// Window and GL state, created once the event loop is running. Fields
/// drop in declaration order: mpv's render context must go first, while
/// the GL context is still alive and current.
struct Graphics {
    render: RenderContext,
    gl: glow::Context,
    surface: Surface<WindowSurface>,
    context: PossiblyCurrentContext,
    window: Window,
}

pub struct App {
    mpv: Mpv,
    proxy: EventLoopProxy<UserEvent>,
    tuner: Sender<Msg>,
    tuner_thread: Option<JoinHandle<Exit>>,
    start_fullscreen: bool,
    graphics: Option<Graphics>,
    keyboard: Keyboard,
    /// Set if setting up graphics failed; returned from `main`.
    pub error: Option<anyhow::Error>,
}

impl App {
    pub fn new(
        mpv: Mpv,
        proxy: EventLoopProxy<UserEvent>,
        tuner: Sender<Msg>,
        tuner_thread: JoinHandle<Exit>,
        fullscreen: bool,
    ) -> Self {
        Self {
            mpv,
            proxy,
            tuner,
            tuner_thread: Some(tuner_thread),
            start_fullscreen: fullscreen,
            graphics: None,
            keyboard: Keyboard::new(),
            error: None,
        }
    }

    fn dispatch(&mut self, event_loop: &ActiveEventLoop, action: Action) {
        match action {
            Action::ToggleFullscreen => {
                if let Some(g) = &self.graphics {
                    let fullscreen = match g.window.fullscreen() {
                        Some(_) => None,
                        None => Some(Fullscreen::Borderless(None)),
                    };
                    g.window.set_fullscreen(fullscreen);
                }
            }
            Action::Quit => self.quit(event_loop),
            other => {
                tracing::debug!(action = ?other, "input");
                if self.tuner.send(Msg::Action(other)).is_err() {
                    tracing::warn!("tuner thread is gone");
                    event_loop.exit();
                }
            }
        }
    }

    /// Asks the tuner to report progress and stop, waits for it, then
    /// tears down graphics and leaves the event loop.
    fn quit(&mut self, event_loop: &ActiveEventLoop) {
        tracing::info!("quitting");
        let _ = self.tuner.send(Msg::Action(Action::Quit));
        if let Some(thread) = self.tuner_thread.take() {
            match thread.join() {
                Ok(exit) => tracing::debug!(?exit, "tuner thread stopped"),
                Err(_) => tracing::error!("tuner thread panicked"),
            }
        }
        // Drop the render context now, on this thread with GL current.
        self.graphics = None;
        event_loop.exit();
    }

    fn redraw(&mut self) {
        let Some(g) = &self.graphics else {
            return;
        };
        let size = g.window.inner_size();
        let (w, h) = (
            i32::try_from(size.width).unwrap_or(i32::MAX),
            i32::try_from(size.height).unwrap_or(i32::MAX),
        );
        // SAFETY: plain GL calls on the current context.
        unsafe {
            g.gl.viewport(0, 0, w, h);
            g.gl.clear_color(0.0, 0.0, 0.0, 1.0);
            g.gl.clear(glow::COLOR_BUFFER_BIT);
        }
        if let Err(err) = g.render.render(0, w, h, true) {
            tracing::warn!(%err, "mpv render failed");
        }
        if let Err(err) = g.surface.swap_buffers(&g.context) {
            tracing::warn!(%err, "swap failed");
        }
        g.render.report_swap();
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.graphics.is_some() {
            return;
        }
        match create_graphics(
            event_loop,
            &self.mpv,
            self.proxy.clone(),
            self.start_fullscreen,
        ) {
            Ok(g) => {
                g.window.request_redraw();
                self.graphics = Some(g);
                let _ = self.tuner.send(Msg::Start);
            }
            Err(err) => {
                self.error = Some(err.context("cannot set up the window and OpenGL"));
                self.quit(event_loop);
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::MpvFrame => {
                if let Some(g) = &self.graphics
                    && g.render.update()
                {
                    g.window.request_redraw();
                }
            }
            UserEvent::TunerStopped => {
                if self.tuner_thread.is_some() {
                    tracing::info!("player stopped; exiting");
                    self.quit(event_loop);
                }
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.quit(event_loop),
            WindowEvent::Resized(size) => {
                if let Some(g) = &self.graphics
                    && let (Some(w), Some(h)) =
                        (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
                {
                    g.surface.resize(&g.context, w, h);
                    g.window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let Some(key) = input::translate(&event.logical_key) else {
                    return;
                };
                if event.repeat && !input::repeats(key) {
                    return;
                }
                if let Some(action) = self.keyboard.press(key, Instant::now()) {
                    self.dispatch(event_loop, action);
                } else if !self.keyboard.pending_digits().is_empty() {
                    tracing::info!(digits = self.keyboard.pending_digits(), "channel number");
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(action) = self.keyboard.tick(Instant::now()) {
            self.dispatch(event_loop, action);
        }
        event_loop.set_control_flow(match self.keyboard.deadline() {
            Some(deadline) => ControlFlow::WaitUntil(deadline),
            None => ControlFlow::Wait,
        });
    }
}

/// Opens the window, creates a GL context for it, and an mpv render
/// context drawing into it.
fn create_graphics(
    event_loop: &ActiveEventLoop,
    mpv: &Mpv,
    proxy: EventLoopProxy<UserEvent>,
    fullscreen: bool,
) -> anyhow::Result<Graphics> {
    let attributes = Window::default_attributes()
        .with_title("Marathon")
        .with_inner_size(LogicalSize::new(1280.0, 720.0))
        .with_fullscreen(fullscreen.then_some(Fullscreen::Borderless(None)));
    // EGL first: mpv's VA-API interop for hardware decoding needs EGL
    // (on X11 too; GLX interop is gone). macOS uses CGL regardless.
    let builder = DisplayBuilder::new()
        .with_preference(ApiPreference::PreferEgl)
        .with_window_attributes(Some(attributes));
    // glutin-winit's config picker must return a config, so an empty list
    // can only be signalled by panicking; turn that into an error.
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
    // SAFETY: `raw_window` belongs to `window`, which outlives the context
    // (see `Graphics`'s field order).
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

    // SAFETY: the context is current on this thread.
    let gl =
        unsafe { glow::Context::from_loader_function_cstr(|name| display.get_proc_address(name)) };

    let (x11_display, wayland_display) = native_display(&window);
    let lookup_display = display.clone();
    let params = GlParams {
        get_proc_address: Box::new(move |name| lookup_display.get_proc_address(name)),
        x11_display,
        wayland_display,
    };
    // SAFETY: the GL context is current on this (the main) thread and
    // stays current for the life of the app; `Graphics` drops the render
    // context before the GL context. The display pointers come from
    // `window`, which also outlives it.
    let mut render = unsafe { RenderContext::new_gl(mpv, params) }
        .context("cannot create mpv's render context")?;
    render.set_update_callback(move || {
        // Fails only once the event loop has exited.
        let _ = proxy.send_event(UserEvent::MpvFrame);
    });
    tracing::info!(
        gl = ?gl.version(),
        // SAFETY: a plain GL query on the current context.
        renderer = unsafe { gl.get_parameter_string(glow::RENDERER) },
        size = ?window.inner_size(),
        scale = window.scale_factor(),
        "window ready"
    );
    Ok(Graphics {
        render,
        gl,
        surface,
        context,
        window,
    })
}

/// Prefers configs without multisampling (video needs none), then the
/// first offered.
fn pick_config(configs: Box<dyn Iterator<Item = Config> + '_>) -> Config {
    // See `create_graphics`: an empty iterator panics here and becomes an
    // error there.
    #[allow(clippy::expect_used)]
    configs
        .min_by_key(|c| c.num_samples())
        .expect("the GL display offered no configs")
}

/// The window system's display connection, which mpv uses for hardware
/// decoding interop.
fn native_display(window: &Window) -> (Option<NonNull<c_void>>, Option<NonNull<c_void>>) {
    match window.display_handle().map(|h| h.as_raw()) {
        Ok(RawDisplayHandle::Xlib(h)) => (h.display, None),
        Ok(RawDisplayHandle::Wayland(h)) => (None, Some(h.display)),
        _ => (None, None),
    }
}
