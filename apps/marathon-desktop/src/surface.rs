//! The [`Surface`] abstraction (PLAN.md §9 "Keep winit at the edge"): what
//! the renderer needs from whatever owns the screen.
//!
//! Today the only implementation is the winit + glutin window in
//! [`crate::platform`]. A future appliance backend supplies a `Surface` from
//! GBM/EGL/KMS instead, without touching the renderer, tuner, or overlay.

use std::ffi::{CStr, c_void};
use std::ptr::NonNull;

/// Looks up an OpenGL function by name in the surface's context. Returns
/// NULL if unknown.
pub type GlLoader = Box<dyn Fn(&CStr) -> *const c_void>;

/// The native display connection behind a surface. mpv uses it for
/// hardware-decoding interop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeDisplay {
    None,
    /// An Xlib `Display*`.
    X11(NonNull<c_void>),
    /// A `wl_display*`.
    Wayland(NonNull<c_void>),
}

/// A drawable with a current OpenGL context.
///
/// Implementations keep their GL context current on the thread that uses
/// the surface, for as long as the surface lives.
pub trait Surface {
    /// Resolves GL functions for this surface's context. The loader stays
    /// valid as long as the surface does.
    fn gl_loader(&self) -> GlLoader;
    /// Size of the default framebuffer, in physical pixels.
    fn drawable_size(&self) -> (u32, u32);
    /// Physical pixels per logical pixel (for the overlay, M5).
    fn scale_factor(&self) -> f64;
    /// Shows the frame just drawn (swaps buffers, vsynced).
    fn present(&self) -> Result<(), String>;
    /// The native display, if there is one mpv can use.
    fn native_display(&self) -> NativeDisplay;
}
