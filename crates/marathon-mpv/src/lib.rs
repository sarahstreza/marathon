//! Safe wrapper over libmpv, including the OpenGL render API used by the
//! desktop client.
//!
//! The wrapper covers only what Marathon needs:
//!
//! - [`Mpv`]: a cheap-to-clone, thread-safe handle for commands and
//!   properties. libmpv's client API is thread-safe, so any thread may use
//!   it.
//! - [`Events`]: the single reader of the handle's event queue. libmpv only
//!   allows one thread at a time to wait for events, which is why this is a
//!   separate, non-`Clone` value.
//! - [`render::RenderContext`]: draws video into the caller's OpenGL
//!   framebuffer.
//!
//! [`Mpv::new`] returns the first two together. The mpv core is destroyed
//! when the last of them (including any render context) is dropped.
//!
//! Bindings to libmpv are hand-written in [`sys`] (see its docs for why);
//! `build.rs` links the system libmpv found by `pkg-config`.

pub mod render;
mod sys;

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex, PoisonError};

/// Errors from libmpv or from preparing arguments for it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// libmpv returned an error code.
    #[error("mpv: {message} ({code})")]
    Mpv {
        /// The `mpv_error` value, always negative.
        code: i32,
        /// libmpv's description of `code`.
        message: String,
    },
    /// `mpv_create` returned NULL (out of memory, or libmpv refused, e.g.
    /// because the process locale is not "C" for numbers).
    #[error("mpv_create failed")]
    Create,
    /// An argument contained a NUL byte and cannot be passed to C.
    #[error("argument contains a NUL byte: {0:?}")]
    Nul(String),
}

impl Error {
    fn from_code(code: c_int) -> Self {
        // SAFETY: mpv_error_string accepts any value and returns a pointer
        // to a static string.
        let message = unsafe { CStr::from_ptr(sys::mpv_error_string(code)) }
            .to_string_lossy()
            .into_owned();
        Error::Mpv { code, message }
    }

    /// The libmpv error code, if this is an [`Error::Mpv`].
    pub fn code(&self) -> Option<i32> {
        match self {
            Error::Mpv { code, .. } => Some(*code),
            _ => None,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// `MPV_ERROR_PROPERTY_UNAVAILABLE`: the property exists but has no value
/// right now (e.g. `duration` with nothing loaded).
pub const ERROR_PROPERTY_UNAVAILABLE: i32 = -10;

fn check(code: c_int) -> Result<()> {
    if code < 0 {
        Err(Error::from_code(code))
    } else {
        Ok(())
    }
}

fn cstring(s: &str) -> Result<CString> {
    CString::new(s).map_err(|_| Error::Nul(s.to_owned()))
}

/// Copies a C string owned by libmpv. NULL becomes the empty string.
///
/// # Safety
/// `p` must be NULL or point to a NUL-terminated string valid for the
/// duration of the call.
unsafe fn owned_str(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        // SAFETY: non-null and valid per this function's contract.
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

/// The libmpv client API version this process is linked against, as
/// `(major, minor)`.
pub fn client_api_version() -> (u32, u32) {
    // SAFETY: no preconditions.
    let v = unsafe { sys::mpv_client_api_version() };
    ((v >> 16) as u32 & 0xffff, v as u32 & 0xffff)
}

type Callback = Box<dyn Fn() + Send + Sync + 'static>;

/// Calls a boxed [`Callback`] from C. Panics are caught: unwinding into
/// libmpv would abort the process.
unsafe extern "C" fn call_callback(data: *mut c_void) {
    if data.is_null() {
        return;
    }
    // SAFETY: `data` is the `*const Callback` registered alongside this
    // function, and its owner keeps it alive until the registration is
    // removed or the mpv object is destroyed.
    let callback = unsafe { &*(data as *const Callback) };
    let _ = catch_unwind(AssertUnwindSafe(callback));
}

/// Owns the `mpv_handle`. Shared by [`Mpv`], [`Events`], and render
/// contexts; the core is destroyed when the last owner drops.
struct Handle {
    ctx: NonNull<sys::mpv_handle>,
    /// The wakeup callback registered with libmpv, kept alive until it is
    /// replaced or the handle is destroyed.
    wakeup: Mutex<Option<Box<Callback>>>,
}

// SAFETY: libmpv's client API is thread-safe ("all functions are
// thread-safe" in client.h), with the single exception of waiting for
// events, which only `Events` does and which requires `&mut Events`.
unsafe impl Send for Handle {}
// SAFETY: as above.
unsafe impl Sync for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: we own the handle and nothing else references it any
        // more. After this returns libmpv calls no callbacks, so the boxed
        // wakeup callback can be dropped afterwards (as a field).
        unsafe { sys::mpv_terminate_destroy(self.ctx.as_ptr()) };
    }
}

/// Value formats for [`Mpv::observe_property`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    String,
    Flag,
    Int64,
    Double,
}

impl Format {
    fn raw(self) -> sys::mpv_format {
        match self {
            Format::String => sys::MPV_FORMAT_STRING,
            Format::Flag => sys::MPV_FORMAT_FLAG,
            Format::Int64 => sys::MPV_FORMAT_INT64,
            Format::Double => sys::MPV_FORMAT_DOUBLE,
        }
    }
}

/// A property value delivered by [`Event::PropertyChange`].
#[derive(Debug, Clone, PartialEq)]
pub enum PropertyValue {
    /// The property is unavailable (e.g. `duration` with nothing loaded).
    None,
    String(String),
    Flag(bool),
    Int64(i64),
    Double(f64),
}

/// Why a file stopped playing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndFileReason {
    /// Played to the end.
    Eof,
    /// Stopped by a command, including being replaced by `loadfile`.
    Stop,
    /// The player is shutting down.
    Quit,
    /// Failed to open or play.
    Error(Error),
    /// The file was a playlist or redirect and was replaced by its entries.
    Redirect,
    /// A reason this wrapper does not know.
    Unknown(i32),
}

/// Severity of an [`Event::LogMessage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Fatal,
    Error,
    Warn,
    Info,
    Verbose,
    Debug,
}

/// An event from libmpv. Strings are copied out of libmpv's buffers.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// The player is quitting; stop using the handle.
    Shutdown,
    /// A log line, if requested with [`Mpv::request_log_messages`].
    LogMessage {
        prefix: String,
        level: LogLevel,
        /// The text, usually ending in a newline.
        text: String,
    },
    /// A playlist entry started loading.
    StartFile { playlist_entry_id: i64 },
    /// The current file was opened and playback is about to begin.
    FileLoaded,
    /// A playlist entry stopped (for any reason; see `reason`).
    EndFile {
        playlist_entry_id: i64,
        reason: EndFileReason,
    },
    /// Playback started, or resumed after a seek.
    PlaybackRestart,
    /// An observed property changed. `id` is the value passed to
    /// [`Mpv::observe_property`].
    PropertyChange {
        id: u64,
        name: String,
        value: PropertyValue,
    },
    /// Any other event, by its `mpv_event_id`.
    Other(i32),
}

/// A handle to an mpv player. Cheap to clone; usable from any thread.
#[derive(Clone)]
pub struct Mpv {
    handle: Arc<Handle>,
}

impl fmt::Debug for Mpv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Mpv").finish_non_exhaustive()
    }
}

/// The event queue of an [`Mpv`] handle. There is exactly one per handle.
pub struct Events {
    handle: Arc<Handle>,
}

impl fmt::Debug for Events {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Events").finish_non_exhaustive()
    }
}

impl Mpv {
    /// Creates and initializes a player. `options` are set before
    /// initialization, as on mpv's command line without the leading `--`
    /// (e.g. `("vo", "libmpv")`). Some options only take effect this way.
    ///
    /// libmpv does not read the user's `mpv.conf` unless `config=yes` is
    /// passed.
    pub fn new<'a>(options: impl IntoIterator<Item = (&'a str, &'a str)>) -> Result<(Mpv, Events)> {
        // SAFETY: no preconditions.
        let raw = unsafe { sys::mpv_create() };
        let ctx = NonNull::new(raw).ok_or(Error::Create)?;
        // From here on `handle` owns the context and destroys it on error.
        let handle = Arc::new(Handle {
            ctx,
            wakeup: Mutex::new(None),
        });
        for (name, value) in options {
            let (name_c, value_c) = (cstring(name)?, cstring(value)?);
            // SAFETY: valid handle and NUL-terminated strings.
            check(unsafe {
                sys::mpv_set_option_string(ctx.as_ptr(), name_c.as_ptr(), value_c.as_ptr())
            })
            .map_err(|err| match err {
                Error::Mpv { code, message } => Error::Mpv {
                    code,
                    message: format!("{message} (option {name}={value})"),
                },
                other => other,
            })?;
        }
        // SAFETY: valid, not yet initialized handle.
        check(unsafe { sys::mpv_initialize(ctx.as_ptr()) })?;
        Ok((
            Mpv {
                handle: handle.clone(),
            },
            Events { handle },
        ))
    }

    fn ctx(&self) -> *mut sys::mpv_handle {
        self.handle.ctx.as_ptr()
    }

    /// Runs a command synchronously, e.g. `["loadfile", url, "replace"]`.
    pub fn command(&self, args: &[&str]) -> Result<()> {
        let owned = args
            .iter()
            .map(|a| cstring(a))
            .collect::<Result<Vec<_>>>()?;
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|a| a.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        // SAFETY: NULL-terminated array of valid C strings that outlive the
        // call; libmpv does not write through the pointer.
        check(unsafe { sys::mpv_command(self.ctx(), ptrs.as_mut_ptr()) })
    }

    /// Sets a property from its string form, e.g. `("pause", "yes")`.
    pub fn set_property_string(&self, name: &str, value: &str) -> Result<()> {
        let (name, value) = (cstring(name)?, cstring(value)?);
        // SAFETY: valid handle and C strings.
        check(unsafe { sys::mpv_set_property_string(self.ctx(), name.as_ptr(), value.as_ptr()) })
    }

    /// Sets a flag (boolean) property.
    pub fn set_property_flag(&self, name: &str, value: bool) -> Result<()> {
        let name = cstring(name)?;
        let mut flag: c_int = value.into();
        // SAFETY: MPV_FORMAT_FLAG takes a pointer to an int.
        check(unsafe {
            sys::mpv_set_property(
                self.ctx(),
                name.as_ptr(),
                sys::MPV_FORMAT_FLAG,
                (&raw mut flag).cast(),
            )
        })
    }

    /// Sets a floating-point property.
    pub fn set_property_double(&self, name: &str, value: f64) -> Result<()> {
        let name = cstring(name)?;
        let mut v = value;
        // SAFETY: MPV_FORMAT_DOUBLE takes a pointer to a double.
        check(unsafe {
            sys::mpv_set_property(
                self.ctx(),
                name.as_ptr(),
                sys::MPV_FORMAT_DOUBLE,
                (&raw mut v).cast(),
            )
        })
    }

    /// Reads a property in its string form.
    pub fn get_property_string(&self, name: &str) -> Result<String> {
        let name_c = cstring(name)?;
        // SAFETY: valid handle and C string. The result is NULL on error,
        // otherwise a string we must release with mpv_free.
        let raw = unsafe { sys::mpv_get_property_string(self.ctx(), name_c.as_ptr()) };
        if raw.is_null() {
            // mpv_get_property_string does not report why; ask again with
            // the typed API to get an error code.
            let mut out: *mut c_char = std::ptr::null_mut();
            // SAFETY: MPV_FORMAT_STRING writes a `char *` to `out`.
            let code = unsafe {
                sys::mpv_get_property(
                    self.ctx(),
                    name_c.as_ptr(),
                    sys::MPV_FORMAT_STRING,
                    (&raw mut out).cast(),
                )
            };
            if !out.is_null() {
                // SAFETY: allocated by libmpv for us.
                unsafe { sys::mpv_free(out.cast()) };
            }
            check(code)?;
            return Err(Error::from_code(ERROR_PROPERTY_UNAVAILABLE));
        }
        // SAFETY: non-null, NUL-terminated, owned by us until mpv_free.
        let s = unsafe { owned_str(raw) };
        // SAFETY: allocated by libmpv for us.
        unsafe { sys::mpv_free(raw.cast()) };
        Ok(s)
    }

    /// Reads a flag (boolean) property.
    pub fn get_property_flag(&self, name: &str) -> Result<bool> {
        let name = cstring(name)?;
        let mut flag: c_int = 0;
        // SAFETY: MPV_FORMAT_FLAG writes an int.
        check(unsafe {
            sys::mpv_get_property(
                self.ctx(),
                name.as_ptr(),
                sys::MPV_FORMAT_FLAG,
                (&raw mut flag).cast(),
            )
        })?;
        Ok(flag != 0)
    }

    /// Reads a floating-point property.
    pub fn get_property_double(&self, name: &str) -> Result<f64> {
        let name = cstring(name)?;
        let mut v: f64 = 0.0;
        // SAFETY: MPV_FORMAT_DOUBLE writes a double.
        check(unsafe {
            sys::mpv_get_property(
                self.ctx(),
                name.as_ptr(),
                sys::MPV_FORMAT_DOUBLE,
                (&raw mut v).cast(),
            )
        })?;
        Ok(v)
    }

    /// Reads an integer property.
    pub fn get_property_i64(&self, name: &str) -> Result<i64> {
        let name = cstring(name)?;
        let mut v: i64 = 0;
        // SAFETY: MPV_FORMAT_INT64 writes an int64_t.
        check(unsafe {
            sys::mpv_get_property(
                self.ctx(),
                name.as_ptr(),
                sys::MPV_FORMAT_INT64,
                (&raw mut v).cast(),
            )
        })?;
        Ok(v)
    }

    /// Asks for [`Event::PropertyChange`] events whenever `name` changes,
    /// tagged with `id`. One event is sent right away with the current
    /// value.
    pub fn observe_property(&self, name: &str, format: Format, id: u64) -> Result<()> {
        let name = cstring(name)?;
        // SAFETY: valid handle and C string.
        check(unsafe { sys::mpv_observe_property(self.ctx(), id, name.as_ptr(), format.raw()) })
    }

    /// Asks for [`Event::LogMessage`] events at `min_level` and above
    /// (`"warn"`, `"info"`, `"v"`, ...; `"no"` turns them off).
    pub fn request_log_messages(&self, min_level: &str) -> Result<()> {
        let level = cstring(min_level)?;
        // SAFETY: valid handle and C string.
        check(unsafe { sys::mpv_request_log_messages(self.ctx(), level.as_ptr()) })
    }

    /// Wakes a thread blocked in [`Events::wait`].
    pub fn wakeup(&self) {
        // SAFETY: valid handle.
        unsafe { sys::mpv_wakeup(self.ctx()) }
    }
}

impl Events {
    /// Sets a function libmpv calls (from its own threads) when new events
    /// may be waiting. It must return quickly and must not call back into
    /// mpv; typically it posts a message to the thread that calls
    /// [`Events::wait`]. Replaces any previous callback.
    pub fn set_wakeup_callback(&mut self, callback: impl Fn() + Send + Sync + 'static) {
        let boxed: Box<Callback> = Box::new(Box::new(callback));
        let data = (&raw const *boxed).cast_mut().cast::<c_void>();
        let mut slot = self
            .handle
            .wakeup
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // SAFETY: `data` points to a `Callback` that stays alive in `slot`
        // until it is replaced (after libmpv has switched to the new one;
        // libmpv swaps callbacks under its own lock) or the handle is
        // destroyed.
        unsafe {
            sys::mpv_set_wakeup_callback(self.handle.ctx.as_ptr(), Some(call_callback), data);
        }
        *slot = Some(boxed);
    }

    /// Waits up to `timeout` seconds for the next event (`0.0` polls,
    /// negative waits forever). Returns `None` on timeout.
    pub fn wait(&mut self, timeout: f64) -> Option<Event> {
        // SAFETY: valid handle; `&mut self` makes this the only waiter. The
        // returned event is valid until the next wait on this handle, and
        // we copy everything out of it before returning.
        let event = unsafe { &*sys::mpv_wait_event(self.handle.ctx.as_ptr(), timeout) };
        // SAFETY: as above; `data` matches `event_id` per client.h.
        unsafe { convert_event(event) }
    }
}

/// Converts a libmpv event into an owned [`Event`].
///
/// # Safety
/// `event` must come from `mpv_wait_event` and still be valid.
unsafe fn convert_event(event: &sys::mpv_event) -> Option<Event> {
    Some(match event.event_id {
        sys::MPV_EVENT_NONE => return None,
        sys::MPV_EVENT_SHUTDOWN => Event::Shutdown,
        sys::MPV_EVENT_LOG_MESSAGE => {
            // SAFETY: LOG_MESSAGE carries an mpv_event_log_message.
            let msg = unsafe { &*(event.data as *const sys::mpv_event_log_message) };
            let level = match msg.log_level {
                sys::MPV_LOG_LEVEL_FATAL => LogLevel::Fatal,
                sys::MPV_LOG_LEVEL_ERROR => LogLevel::Error,
                sys::MPV_LOG_LEVEL_WARN => LogLevel::Warn,
                sys::MPV_LOG_LEVEL_INFO => LogLevel::Info,
                sys::MPV_LOG_LEVEL_V => LogLevel::Verbose,
                _ => LogLevel::Debug,
            };
            Event::LogMessage {
                // SAFETY: strings valid for the event's lifetime.
                prefix: unsafe { owned_str(msg.prefix) },
                level,
                // SAFETY: as above.
                text: unsafe { owned_str(msg.text) },
            }
        }
        sys::MPV_EVENT_START_FILE => {
            // SAFETY: START_FILE carries an mpv_event_start_file.
            let data = unsafe { &*(event.data as *const sys::mpv_event_start_file) };
            Event::StartFile {
                playlist_entry_id: data.playlist_entry_id,
            }
        }
        sys::MPV_EVENT_FILE_LOADED => Event::FileLoaded,
        sys::MPV_EVENT_END_FILE => {
            // SAFETY: END_FILE carries an mpv_event_end_file.
            let data = unsafe { &*(event.data as *const sys::mpv_event_end_file) };
            let reason = match data.reason {
                sys::MPV_END_FILE_REASON_EOF => EndFileReason::Eof,
                sys::MPV_END_FILE_REASON_STOP => EndFileReason::Stop,
                sys::MPV_END_FILE_REASON_QUIT => EndFileReason::Quit,
                sys::MPV_END_FILE_REASON_ERROR => {
                    EndFileReason::Error(Error::from_code(data.error))
                }
                sys::MPV_END_FILE_REASON_REDIRECT => EndFileReason::Redirect,
                other => EndFileReason::Unknown(other),
            };
            Event::EndFile {
                playlist_entry_id: data.playlist_entry_id,
                reason,
            }
        }
        sys::MPV_EVENT_PLAYBACK_RESTART => Event::PlaybackRestart,
        sys::MPV_EVENT_PROPERTY_CHANGE => {
            // SAFETY: PROPERTY_CHANGE carries an mpv_event_property.
            let prop = unsafe { &*(event.data as *const sys::mpv_event_property) };
            // SAFETY: `data` points to a value of `format` (or is unused
            // for MPV_FORMAT_NONE).
            let value = unsafe {
                match prop.format {
                    sys::MPV_FORMAT_STRING => {
                        PropertyValue::String(owned_str(*(prop.data as *const *const c_char)))
                    }
                    sys::MPV_FORMAT_FLAG => PropertyValue::Flag(*(prop.data as *const c_int) != 0),
                    sys::MPV_FORMAT_INT64 => PropertyValue::Int64(*(prop.data as *const i64)),
                    sys::MPV_FORMAT_DOUBLE => PropertyValue::Double(*(prop.data as *const f64)),
                    _ => PropertyValue::None,
                }
            };
            Event::PropertyChange {
                id: event.reply_userdata,
                // SAFETY: valid for the event's lifetime.
                name: unsafe { owned_str(prop.name) },
                value,
            }
        }
        other => Event::Other(other),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// A headless player: no video or audio output.
    fn headless() -> (Mpv, Events) {
        Mpv::new([("vo", "null"), ("ao", "null"), ("idle", "yes")]).unwrap()
    }

    /// Collects events until `stop` matches one or the deadline passes.
    fn collect_until(events: &mut Events, stop: impl Fn(&Event) -> bool) -> Vec<Event> {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut seen = Vec::new();
        while Instant::now() < deadline {
            if let Some(event) = events.wait(0.5) {
                let done = stop(&event);
                seen.push(event);
                if done {
                    return seen;
                }
            }
        }
        panic!("timed out; events so far: {seen:#?}");
    }

    #[test]
    fn links_a_client_api_2() {
        assert!(client_api_version().0 >= 2, "{:?}", client_api_version());
    }

    #[test]
    fn properties_round_trip_and_errors_carry_codes() {
        let (mpv, _events) = headless();
        mpv.set_property_flag("pause", true).unwrap();
        assert!(mpv.get_property_flag("pause").unwrap());
        assert_eq!(mpv.get_property_string("pause").unwrap(), "yes");
        mpv.set_property_string("pause", "no").unwrap();
        assert!(!mpv.get_property_flag("pause").unwrap());
        mpv.set_property_double("volume", 42.5).unwrap();
        assert_eq!(mpv.get_property_double("volume").unwrap(), 42.5);
        assert_eq!(mpv.get_property_i64("volume").unwrap(), 42);

        let err = mpv.get_property_string("no-such-property").unwrap_err();
        assert_eq!(err.code(), Some(-8), "{err}"); // MPV_ERROR_PROPERTY_NOT_FOUND
        let err = mpv.get_property_double("duration").unwrap_err();
        assert_eq!(err.code(), Some(ERROR_PROPERTY_UNAVAILABLE), "{err}");
        assert!(matches!(mpv.command(&["a\0b"]), Err(Error::Nul(_))));
        assert!(mpv.command(&["no-such-command"]).is_err());
    }

    #[test]
    fn bad_option_is_reported() {
        let err = Mpv::new([("no-such-option", "1")]).unwrap_err();
        assert!(err.to_string().contains("no-such-option"), "{err}");
    }

    /// Plays a clip generated by FFmpeg's lavfi through the hand-written
    /// event structs, checking that ids, reasons, and property values come
    /// out intact.
    #[test]
    fn event_layout_matches_libmpv() {
        let (mpv, mut events) = headless();
        mpv.observe_property("duration", Format::Double, 7).unwrap();
        mpv.observe_property("pause", Format::Flag, 8).unwrap();
        mpv.observe_property("path", Format::String, 9).unwrap();

        let url = "av://lavfi:testsrc=duration=0.5:size=32x32:rate=10";
        mpv.command(&["loadfile", url, "replace"]).unwrap();
        let entry: i64 = mpv
            .get_property_string("playlist/0/id")
            .unwrap()
            .parse()
            .unwrap();

        let seen = collect_until(&mut events, |e| matches!(e, Event::EndFile { .. }));
        assert!(seen.contains(&Event::StartFile {
            playlist_entry_id: entry
        }));
        assert!(seen.contains(&Event::FileLoaded));
        assert_eq!(
            seen.last(),
            Some(&Event::EndFile {
                playlist_entry_id: entry,
                reason: EndFileReason::Eof
            })
        );
        let has = |id, check: &dyn Fn(&PropertyValue) -> bool| {
            seen.iter().any(|e| {
                matches!(e, Event::PropertyChange { id: i, value, .. } if *i == id && check(value))
            })
        };
        // lavfi reports the last frame's timestamp as the duration.
        assert!(has(
            7,
            &|v| matches!(v, PropertyValue::Double(d) if *d > 0.3 && *d <= 0.5)
        ));
        assert!(has(8, &|v| *v == PropertyValue::Flag(false)));
        assert!(has(9, &|v| *v == PropertyValue::String(url.to_owned())));
        assert!(seen.iter().any(|e| matches!(
            e,
            Event::PropertyChange { name, .. } if name == "duration"
        )));
    }

    #[test]
    fn load_failure_ends_with_error() {
        let (mpv, mut events) = headless();
        mpv.command(&["loadfile", "/nonexistent/marathon-test.mkv"])
            .unwrap();
        let seen = collect_until(&mut events, |e| matches!(e, Event::EndFile { .. }));
        assert!(matches!(
            seen.last(),
            Some(Event::EndFile {
                reason: EndFileReason::Error(_),
                ..
            })
        ));
    }

    #[test]
    fn wakeup_callback_fires_and_log_messages_arrive() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let (mpv, mut events) = headless();
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        events.set_wakeup_callback(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        mpv.request_log_messages("info").unwrap();
        mpv.command(&["loadfile", "av://lavfi:testsrc=duration=0.2"])
            .unwrap();
        let seen = collect_until(&mut events, |e| matches!(e, Event::EndFile { .. }));
        assert!(calls.load(Ordering::SeqCst) > 0);
        assert!(
            seen.iter().any(|e| matches!(e, Event::LogMessage { .. })),
            "{seen:#?}"
        );
        // Dropping both halves destroys the core; the callback must not be
        // called after its box is freed (checked by sanitizers / no crash).
        drop(mpv);
        drop(events);
    }
}
