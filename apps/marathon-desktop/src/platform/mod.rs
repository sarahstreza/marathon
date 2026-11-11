//! The desktop platform layer: the only place `winit`, `glutin`, and
//! `raw-window-handle` are used (PLAN.md §2 decision 7, §9 "Keep winit at
//! the edge"; enforced by `scripts/check-seams.sh`).
//!
//! It owns the event loop and the window, gives the
//! [`Renderer`](crate::renderer::Renderer) a [`Surface`](crate::surface::Surface),
//! turns key and mouse events into the input sources' own types, and hides
//! the cursor when the mouse is idle. The rest of the app talks to it only
//! through [`PlatformHandle`] and [`Hooks`].
//!
//! The loop:
//!
//! - mpv's frame callback posts [`UserEvent::Frame`] through the event
//!   loop proxy; the handler asks the renderer whether a frame is due
//!   and, if so, requests a redraw. mpv is never called from the callback.
//! - Overlay messages post [`UserEvent::Overlay`]; the handler applies
//!   them and redraws if they changed what is on screen.
//! - A redraw draws video and overlay and presents (vsynced).
//! - Between events the loop sleeps (`ControlFlow::Wait`, or `WaitUntil`
//!   the next overlay fade or cursor hide). Only while an overlay fade is
//!   running does it redraw continuously, at the display's refresh rate.

mod cursor;
mod gl;
mod keys;

use std::time::Instant;

use marathon_client::keyboard::KeyFeed;
use marathon_client::mouse::{MouseFeed, MouseInput};
use marathon_mpv::Mpv;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::WindowId;

use self::cursor::CursorIdle;
use self::gl::GlWindowSurface;
use crate::overlay::Overlay;
use crate::overlay::fade::Wake;
use crate::renderer::Renderer;

/// Events posted to the event loop from other threads.
#[derive(Debug, Clone, Copy)]
enum UserEvent {
    /// mpv may have a new frame.
    Frame,
    /// The overlay has messages waiting.
    Overlay,
    ToggleFullscreen,
    Quit,
}

/// The app's side of the platform: called on the main thread.
pub trait Hooks {
    /// The window and renderer exist; playback may start.
    fn ready(&mut self);
    /// The app is closing (quit action, window closed, or player gone).
    /// Called once, before the renderer is torn down; may block briefly to
    /// save state.
    fn shutdown(&mut self);
}

/// How to open the window.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    pub fullscreen: bool,
}

/// A handle other threads use to reach the window. Clone freely.
#[derive(Clone)]
pub struct PlatformHandle(EventLoopProxy<UserEvent>);

impl PlatformHandle {
    pub fn toggle_fullscreen(&self) {
        // Fails only once the event loop has exited.
        let _ = self.0.send_event(UserEvent::ToggleFullscreen);
    }

    /// Asks the app to shut down (see [`Hooks::shutdown`]).
    pub fn quit(&self) {
        let _ = self.0.send_event(UserEvent::Quit);
    }

    /// Tells the window the overlay has messages to apply.
    pub fn overlay_changed(&self) {
        let _ = self.0.send_event(UserEvent::Overlay);
    }
}

/// Where the platform sends local input.
pub struct InputFeeds {
    pub keys: KeyFeed,
    pub mouse: MouseFeed,
}

/// The event loop, before it runs.
pub struct Platform {
    event_loop: EventLoop<UserEvent>,
}

impl Platform {
    pub fn new() -> anyhow::Result<Self> {
        let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
        event_loop.set_control_flow(ControlFlow::Wait);
        Ok(Self { event_loop })
    }

    pub fn handle(&self) -> PlatformHandle {
        PlatformHandle(self.event_loop.create_proxy())
    }

    /// Opens the window, renders `mpv` and `overlay` into it, feeds input
    /// to `input`, and runs until the app shuts down. Must run on the main
    /// thread.
    pub fn run(
        self,
        options: Options,
        mpv: Mpv,
        overlay: Overlay,
        input: InputFeeds,
        hooks: impl Hooks + 'static,
    ) -> anyhow::Result<()> {
        let mut app = App {
            options,
            mpv,
            overlay,
            input,
            cursor: CursorIdle::new(Instant::now()),
            hooks: Box::new(hooks),
            proxy: self.event_loop.create_proxy(),
            graphics: None,
            shut_down: false,
            error: None,
        };
        self.event_loop.run_app(&mut app)?;
        match app.error.take() {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }
}

/// Drop order matters: the renderer (mpv's render context) must go while
/// the GL context, owned by the surface, is still alive and current.
struct Graphics {
    renderer: Renderer,
    surface: GlWindowSurface,
}

struct App {
    options: Options,
    mpv: Mpv,
    overlay: Overlay,
    input: InputFeeds,
    cursor: CursorIdle,
    hooks: Box<dyn Hooks>,
    proxy: EventLoopProxy<UserEvent>,
    graphics: Option<Graphics>,
    shut_down: bool,
    error: Option<anyhow::Error>,
}

impl App {
    fn create_graphics(&self, event_loop: &ActiveEventLoop) -> anyhow::Result<Graphics> {
        let surface = GlWindowSurface::create(event_loop, self.options.fullscreen)?;
        let proxy = self.proxy.clone();
        // SAFETY: the surface's GL context is current on this (the main)
        // thread for the life of the app, and `Graphics` drops the renderer
        // before the surface.
        let renderer = unsafe {
            Renderer::new(&surface, &self.mpv, move || {
                let _ = proxy.send_event(UserEvent::Frame);
            })
        }?;
        Ok(Graphics { renderer, surface })
    }

    /// The mouse was moved or used: show the cursor.
    fn mouse_active(&mut self) {
        if self.cursor.moved(Instant::now())
            && let Some(g) = &self.graphics
        {
            g.surface.window().set_cursor_visible(true);
        }
    }

    fn shut_down(&mut self, event_loop: &ActiveEventLoop) {
        if !self.shut_down {
            self.shut_down = true;
            tracing::info!("quitting");
            self.hooks.shutdown();
        }
        self.graphics = None;
        event_loop.exit();
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.graphics.is_some() || self.shut_down {
            return;
        }
        match self.create_graphics(event_loop) {
            Ok(g) => {
                g.surface.window().request_redraw();
                self.graphics = Some(g);
                self.hooks.ready();
            }
            Err(err) => {
                self.error = Some(err.context("cannot set up the window and OpenGL"));
                self.shut_down(event_loop);
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Frame => {
                if let Some(g) = &self.graphics
                    && g.renderer.frame_due()
                {
                    g.surface.window().request_redraw();
                }
            }
            UserEvent::Overlay => {
                if self.overlay.pump(Instant::now())
                    && let Some(g) = &self.graphics
                {
                    g.surface.window().request_redraw();
                }
            }
            UserEvent::ToggleFullscreen => {
                if let Some(g) = &self.graphics {
                    g.surface.toggle_fullscreen();
                }
            }
            UserEvent::Quit => self.shut_down(event_loop),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.shut_down(event_loop),
            WindowEvent::Resized(size) => {
                if let Some(g) = &self.graphics {
                    g.surface.resize(size.width, size.height);
                    g.surface.window().request_redraw();
                }
            }
            WindowEvent::RedrawRequested => {
                if let Some(g) = &mut self.graphics {
                    g.renderer.draw(&g.surface, &self.overlay, Instant::now());
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if let Some(key) = keys::translate(&event.logical_key) {
                    self.input.keys.press(key, event.repeat);
                }
            }
            WindowEvent::CursorMoved { .. } => self.mouse_active(),
            WindowEvent::MouseWheel { delta, .. } => {
                self.mouse_active();
                self.input.mouse.input(match delta {
                    MouseScrollDelta::LineDelta(_, y) => MouseInput::WheelLines(y.into()),
                    MouseScrollDelta::PixelDelta(p) => MouseInput::WheelPixels(p.y),
                });
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button,
                ..
            } => {
                self.mouse_active();
                if button == MouseButton::Left {
                    self.input.mouse.input(MouseInput::Click);
                }
            }
            _ => {}
        }
    }

    /// Runs after each batch of events: keeps an overlay fade animating,
    /// hides an idle cursor, and decides how long to sleep.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(g) = &self.graphics else {
            return;
        };
        let now = Instant::now();
        if self.cursor.tick(now) {
            g.surface.window().set_cursor_visible(false);
        }
        let wake = self.overlay.wake(now);
        // Redraw while a fade runs, and once more after the overlay has
        // gone, to clear its last faint frame.
        if wake == Wake::Animating || (g.renderer.overlay_on_screen() && !self.overlay.visible(now))
        {
            g.surface.window().request_redraw();
        }
        let next = match wake {
            Wake::At(t) => Some(t),
            Wake::Animating | Wake::Idle => None,
        };
        let deadline = match (next, self.cursor.deadline()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        event_loop.set_control_flow(match deadline {
            Some(t) => ControlFlow::WaitUntil(t),
            None => ControlFlow::Wait,
        });
    }
}
