//! libmpv's OpenGL render API: mpv draws video frames into a framebuffer
//! the application owns, so the application can draw its own UI on top.
//!
//! The usual loop (see mpv's `render.h` for the full contract):
//!
//! 1. [`RenderContext::set_update_callback`] registers a function mpv calls
//!    from its own threads when there may be a new frame. It must only wake
//!    the render thread, never call mpv.
//! 2. On the render thread, [`RenderContext::update`] says whether a new
//!    frame is due; if so, [`RenderContext::render`] draws it.
//! 3. After swapping buffers, [`RenderContext::report_swap`].

use std::ffi::{CStr, c_char, c_int, c_void};
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::Arc;

use crate::{Callback, Handle, Mpv, Result, call_callback, check, sys};

/// Looks up an OpenGL function by name, as the windowing library's
/// `get_proc_address` does. Returns NULL if unknown.
pub type GetProcAddress = dyn Fn(&CStr) -> *const c_void;

/// How to set up an OpenGL render context.
pub struct GlParams {
    /// Resolves OpenGL function pointers for the current context.
    pub get_proc_address: Box<GetProcAddress>,
    /// The X11 `Display*` the GL context belongs to, if it is an X11
    /// context. mpv uses it for some hardware decoding interop.
    pub x11_display: Option<NonNull<c_void>>,
    /// The Wayland `wl_display*`, if the window is a Wayland surface. mpv
    /// uses it for hardware decoding interop.
    pub wayland_display: Option<NonNull<c_void>>,
}

/// Renders mpv's video into OpenGL framebuffers.
///
/// Not `Send`: every method, and dropping it, must happen on the thread
/// where the GL context it was created with is current.
pub struct RenderContext {
    ctx: NonNull<sys::mpv_render_context>,
    /// Keeps the mpv core alive: libmpv requires the render context to be
    /// freed before the core is destroyed.
    _handle: Arc<Handle>,
    /// Boxed `GetProcAddress`; libmpv holds a pointer to it.
    _get_proc_address: Box<Box<GetProcAddress>>,
    update_callback: Option<Box<Callback>>,
    _not_send: PhantomData<*const ()>,
}

unsafe extern "C" fn get_proc_address_shim(ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    if ctx.is_null() || name.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: `ctx` is the `Box<GetProcAddress>` owned by the render
    // context being created; `name` is a NUL-terminated GL function name.
    let (lookup, name) = unsafe { (&*(ctx as *const Box<GetProcAddress>), CStr::from_ptr(name)) };
    catch_unwind(AssertUnwindSafe(|| lookup(name)))
        .unwrap_or(std::ptr::null())
        .cast_mut()
}

impl RenderContext {
    /// Creates an OpenGL render context for `mpv`. The player must have
    /// been created with `vo=libmpv`.
    ///
    /// # Safety
    ///
    /// The OpenGL context that `params.get_proc_address` resolves against
    /// must be current on the calling thread now, and whenever any method
    /// of the returned value is called or it is dropped. Display pointers in
    /// `params` must stay valid for the life of the returned value.
    pub unsafe fn new_gl(mpv: &Mpv, params: GlParams) -> Result<Self> {
        let lookup: Box<Box<GetProcAddress>> = Box::new(params.get_proc_address);
        let mut init = sys::mpv_opengl_init_params {
            get_proc_address: Some(get_proc_address_shim),
            get_proc_address_ctx: (&raw const *lookup).cast_mut().cast(),
        };
        let mut raw = vec![
            sys::mpv_render_param {
                type_: sys::MPV_RENDER_PARAM_API_TYPE,
                data: sys::MPV_RENDER_API_TYPE_OPENGL.as_ptr().cast_mut().cast(),
            },
            sys::mpv_render_param {
                type_: sys::MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
                data: (&raw mut init).cast(),
            },
        ];
        if let Some(display) = params.x11_display {
            raw.push(sys::mpv_render_param {
                type_: sys::MPV_RENDER_PARAM_X11_DISPLAY,
                data: display.as_ptr(),
            });
        }
        if let Some(display) = params.wayland_display {
            raw.push(sys::mpv_render_param {
                type_: sys::MPV_RENDER_PARAM_WL_DISPLAY,
                data: display.as_ptr(),
            });
        }
        raw.push(sys::mpv_render_param {
            type_: sys::MPV_RENDER_PARAM_INVALID,
            data: std::ptr::null_mut(),
        });

        let mut ctx: *mut sys::mpv_render_context = std::ptr::null_mut();
        // SAFETY: the parameter array is terminated and every pointer in it
        // is valid for the call (libmpv copies what it keeps, except the
        // get_proc_address context, which `lookup` keeps alive). The GL
        // context is current per this function's contract.
        check(unsafe {
            sys::mpv_render_context_create(&raw mut ctx, mpv.ctx(), raw.as_mut_ptr())
        })?;
        let ctx = NonNull::new(ctx).ok_or(crate::Error::Create)?;
        Ok(Self {
            ctx,
            _handle: mpv.handle.clone(),
            _get_proc_address: lookup,
            update_callback: None,
            _not_send: PhantomData,
        })
    }

    /// Sets the function mpv calls (from any thread) when a new frame may
    /// be ready or the video needs redrawing. It must not call mpv; it
    /// should wake the render thread, which then calls [`Self::update`].
    /// Replaces any previous callback.
    pub fn set_update_callback(&mut self, callback: impl Fn() + Send + Sync + 'static) {
        let boxed: Box<Callback> = Box::new(Box::new(callback));
        let data = (&raw const *boxed).cast_mut().cast::<c_void>();
        // SAFETY: `data` stays alive in `self.update_callback` until it is
        // replaced (after libmpv has switched to the new pointer) or the
        // context is freed.
        unsafe {
            sys::mpv_render_context_set_update_callback(
                self.ctx.as_ptr(),
                Some(call_callback),
                data,
            );
        }
        self.update_callback = Some(boxed);
    }

    /// Call after the update callback fired. Returns true if a new frame
    /// should be rendered with [`Self::render`].
    pub fn update(&self) -> bool {
        // SAFETY: valid context, called on the render thread.
        let flags = unsafe { sys::mpv_render_context_update(self.ctx.as_ptr()) };
        flags & sys::MPV_RENDER_UPDATE_FRAME != 0
    }

    /// Draws the current video frame into framebuffer `fbo` (0 is the
    /// window's default framebuffer) of `width` x `height` pixels.
    /// `flip_y` should be true when drawing to the default framebuffer.
    ///
    /// May block until the frame's display time, depending on mpv's
    /// `video-timing-offset`.
    pub fn render(&self, fbo: i32, width: i32, height: i32, flip_y: bool) -> Result<()> {
        let mut target = sys::mpv_opengl_fbo {
            fbo,
            w: width,
            h: height,
            internal_format: 0,
        };
        let mut flip: c_int = flip_y.into();
        let mut params = [
            sys::mpv_render_param {
                type_: sys::MPV_RENDER_PARAM_OPENGL_FBO,
                data: (&raw mut target).cast(),
            },
            sys::mpv_render_param {
                type_: sys::MPV_RENDER_PARAM_FLIP_Y,
                data: (&raw mut flip).cast(),
            },
            sys::mpv_render_param {
                type_: sys::MPV_RENDER_PARAM_INVALID,
                data: std::ptr::null_mut(),
            },
        ];
        // SAFETY: terminated parameter array of valid pointers; GL context
        // current per `new_gl`'s contract.
        check(unsafe { sys::mpv_render_context_render(self.ctx.as_ptr(), params.as_mut_ptr()) })
    }

    /// Tells mpv a frame was just presented (after swapping buffers). Helps
    /// mpv's frame timing; call it after every swap or never.
    pub fn report_swap(&self) {
        // SAFETY: valid context.
        unsafe { sys::mpv_render_context_report_swap(self.ctx.as_ptr()) }
    }
}

impl Drop for RenderContext {
    fn drop(&mut self) {
        // SAFETY: valid context, freed once, with the GL context current
        // per `new_gl`'s contract. After this returns mpv makes no more
        // update callbacks, so the boxed callback may be dropped (as a
        // field) afterwards. `_handle` is dropped after this too, so the
        // core outlives the render context.
        unsafe { sys::mpv_render_context_free(self.ctx.as_ptr()) }
    }
}
