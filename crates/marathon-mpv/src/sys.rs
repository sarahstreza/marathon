//! Raw declarations for the parts of libmpv's C API that Marathon uses.
//!
//! Written by hand from `mpv/client.h`, `mpv/render.h`, and
//! `mpv/render_gl.h` (client API 2.x) rather than generated with `bindgen`,
//! so building needs no libclang. The surface is small; when adding to it,
//! copy declarations from the headers exactly. C enums are `int`-sized on
//! every platform Marathon targets, so they are declared as `c_int`.
//!
//! `tests::event_layout_matches_libmpv` in `lib.rs` plays a generated clip
//! through these declarations, which catches layout mistakes in the event
//! structs.

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_double, c_int, c_ulong, c_void};

/// Opaque `mpv_handle`.
#[repr(C)]
pub struct mpv_handle {
    _private: [u8; 0],
}

/// Opaque `mpv_render_context`.
#[repr(C)]
pub struct mpv_render_context {
    _private: [u8; 0],
}

pub type mpv_format = c_int;
pub const MPV_FORMAT_STRING: mpv_format = 1;
pub const MPV_FORMAT_FLAG: mpv_format = 3;
pub const MPV_FORMAT_INT64: mpv_format = 4;
pub const MPV_FORMAT_DOUBLE: mpv_format = 5;

pub type mpv_event_id = c_int;
pub const MPV_EVENT_NONE: mpv_event_id = 0;
pub const MPV_EVENT_SHUTDOWN: mpv_event_id = 1;
pub const MPV_EVENT_LOG_MESSAGE: mpv_event_id = 2;
pub const MPV_EVENT_START_FILE: mpv_event_id = 6;
pub const MPV_EVENT_END_FILE: mpv_event_id = 7;
pub const MPV_EVENT_FILE_LOADED: mpv_event_id = 8;
pub const MPV_EVENT_PLAYBACK_RESTART: mpv_event_id = 21;
pub const MPV_EVENT_PROPERTY_CHANGE: mpv_event_id = 22;

pub type mpv_end_file_reason = c_int;
pub const MPV_END_FILE_REASON_EOF: mpv_end_file_reason = 0;
pub const MPV_END_FILE_REASON_STOP: mpv_end_file_reason = 2;
pub const MPV_END_FILE_REASON_QUIT: mpv_end_file_reason = 3;
pub const MPV_END_FILE_REASON_ERROR: mpv_end_file_reason = 4;
pub const MPV_END_FILE_REASON_REDIRECT: mpv_end_file_reason = 5;

pub type mpv_log_level = c_int;
pub const MPV_LOG_LEVEL_FATAL: mpv_log_level = 10;
pub const MPV_LOG_LEVEL_ERROR: mpv_log_level = 20;
pub const MPV_LOG_LEVEL_WARN: mpv_log_level = 30;
pub const MPV_LOG_LEVEL_INFO: mpv_log_level = 40;
pub const MPV_LOG_LEVEL_V: mpv_log_level = 50;

#[repr(C)]
pub struct mpv_event {
    pub event_id: mpv_event_id,
    pub error: c_int,
    pub reply_userdata: u64,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct mpv_event_property {
    pub name: *const c_char,
    pub format: mpv_format,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct mpv_event_log_message {
    pub prefix: *const c_char,
    pub level: *const c_char,
    pub text: *const c_char,
    pub log_level: mpv_log_level,
}

#[repr(C)]
pub struct mpv_event_start_file {
    pub playlist_entry_id: i64,
}

#[repr(C)]
pub struct mpv_event_end_file {
    pub reason: mpv_end_file_reason,
    pub error: c_int,
    pub playlist_entry_id: i64,
    pub playlist_insert_id: i64,
    pub playlist_insert_num_entries: c_int,
}

pub type mpv_render_param_type = c_int;
pub const MPV_RENDER_PARAM_INVALID: mpv_render_param_type = 0;
pub const MPV_RENDER_PARAM_API_TYPE: mpv_render_param_type = 1;
pub const MPV_RENDER_PARAM_OPENGL_INIT_PARAMS: mpv_render_param_type = 2;
pub const MPV_RENDER_PARAM_OPENGL_FBO: mpv_render_param_type = 3;
pub const MPV_RENDER_PARAM_FLIP_Y: mpv_render_param_type = 4;
pub const MPV_RENDER_PARAM_X11_DISPLAY: mpv_render_param_type = 8;
pub const MPV_RENDER_PARAM_WL_DISPLAY: mpv_render_param_type = 9;

/// `MPV_RENDER_API_TYPE_OPENGL`, NUL-terminated.
pub const MPV_RENDER_API_TYPE_OPENGL: &[u8] = b"opengl\0";

/// `MPV_RENDER_UPDATE_FRAME`: a new frame should be rendered.
pub const MPV_RENDER_UPDATE_FRAME: u64 = 1 << 0;

#[repr(C)]
pub struct mpv_render_param {
    pub type_: mpv_render_param_type,
    pub data: *mut c_void,
}

#[repr(C)]
pub struct mpv_opengl_init_params {
    pub get_proc_address:
        Option<unsafe extern "C" fn(ctx: *mut c_void, name: *const c_char) -> *mut c_void>,
    pub get_proc_address_ctx: *mut c_void,
}

#[repr(C)]
pub struct mpv_opengl_fbo {
    pub fbo: c_int,
    pub w: c_int,
    pub h: c_int,
    pub internal_format: c_int,
}

pub type mpv_callback = Option<unsafe extern "C" fn(d: *mut c_void)>;

// Linking: build.rs finds libmpv with pkg-config and emits the search path
// and `-lmpv`.
unsafe extern "C" {
    pub fn mpv_client_api_version() -> c_ulong;
    pub fn mpv_error_string(error: c_int) -> *const c_char;
    pub fn mpv_free(data: *mut c_void);

    pub fn mpv_create() -> *mut mpv_handle;
    pub fn mpv_initialize(ctx: *mut mpv_handle) -> c_int;
    pub fn mpv_terminate_destroy(ctx: *mut mpv_handle);

    pub fn mpv_set_option_string(
        ctx: *mut mpv_handle,
        name: *const c_char,
        data: *const c_char,
    ) -> c_int;
    pub fn mpv_command(ctx: *mut mpv_handle, args: *mut *const c_char) -> c_int;
    pub fn mpv_set_property(
        ctx: *mut mpv_handle,
        name: *const c_char,
        format: mpv_format,
        data: *mut c_void,
    ) -> c_int;
    pub fn mpv_set_property_string(
        ctx: *mut mpv_handle,
        name: *const c_char,
        data: *const c_char,
    ) -> c_int;
    pub fn mpv_get_property(
        ctx: *mut mpv_handle,
        name: *const c_char,
        format: mpv_format,
        data: *mut c_void,
    ) -> c_int;
    pub fn mpv_get_property_string(ctx: *mut mpv_handle, name: *const c_char) -> *mut c_char;
    pub fn mpv_observe_property(
        mpv: *mut mpv_handle,
        reply_userdata: u64,
        name: *const c_char,
        format: mpv_format,
    ) -> c_int;

    pub fn mpv_request_log_messages(ctx: *mut mpv_handle, min_level: *const c_char) -> c_int;
    pub fn mpv_wait_event(ctx: *mut mpv_handle, timeout: c_double) -> *mut mpv_event;
    pub fn mpv_wakeup(ctx: *mut mpv_handle);
    pub fn mpv_set_wakeup_callback(ctx: *mut mpv_handle, cb: mpv_callback, d: *mut c_void);

    pub fn mpv_render_context_create(
        res: *mut *mut mpv_render_context,
        mpv: *mut mpv_handle,
        params: *mut mpv_render_param,
    ) -> c_int;
    pub fn mpv_render_context_set_update_callback(
        ctx: *mut mpv_render_context,
        callback: mpv_callback,
        callback_ctx: *mut c_void,
    );
    pub fn mpv_render_context_update(ctx: *mut mpv_render_context) -> u64;
    pub fn mpv_render_context_render(
        ctx: *mut mpv_render_context,
        params: *mut mpv_render_param,
    ) -> c_int;
    pub fn mpv_render_context_report_swap(ctx: *mut mpv_render_context);
    pub fn mpv_render_context_free(ctx: *mut mpv_render_context);
}
