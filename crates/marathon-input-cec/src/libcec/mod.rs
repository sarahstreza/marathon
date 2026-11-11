//! [`LibCec`]: the real [`CecAdapter`], through the system libcec
//! (feature `libcec`).
//!
//! libcec is reached through `shim.c`, compiled by `build.rs` against the
//! installed headers (which also links libcec), so any libcec from 4.x on works without
//! version-specific struct layouts here. The shim's interface is the two
//! functions and the callback table declared below.
//!
//! libcec calls back on its own threads. The callbacks only translate and
//! forward to the source's [`EventSender`], which never blocks or panics.

use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::ptr::NonNull;
use std::time::Duration;

use crate::CecConfig;
use crate::adapter::{CecAdapter, CecError, CecEvent, EventSender, classify_command};

/// Must match `struct marathon_cec_callbacks` in shim.c.
#[repr(C)]
struct Callbacks {
    key: unsafe extern "C" fn(user: *mut c_void, keycode: c_int, duration: c_uint),
    source: unsafe extern "C" fn(user: *mut c_void, activated: c_int),
    command: unsafe extern "C" fn(
        user: *mut c_void,
        initiator: c_int,
        destination: c_int,
        opcode: c_int,
    ),
    alert: unsafe extern "C" fn(user: *mut c_void, alert: c_int),
    log: unsafe extern "C" fn(user: *mut c_void, level: c_int, message: *const c_char),
    user: *mut c_void,
}

// Alert codes from shim.c.
const ALERT_CONNECTION_LOST: c_int = 1;
const ALERT_PERMISSION_ERROR: c_int = 2;
const ALERT_PORT_BUSY: c_int = 3;

// libcec's `cec_log_level`.
const LOG_ERROR: c_int = 1;
const LOG_WARNING: c_int = 2;
const LOG_NOTICE: c_int = 4;

unsafe extern "C" {
    fn marathon_cec_open(
        osd_name: *const c_char,
        claim_on_start: c_int,
        hdmi_port: c_int,
        port: *const c_char,
        timeout_ms: u32,
        callbacks: *const Callbacks,
        error: *mut c_char,
        error_len: usize,
    ) -> *mut c_void;
    fn marathon_cec_close(handle: *mut c_void);
}

/// Recovers the event sender from a callback's `user` pointer.
///
/// # Safety
///
/// `user` must be the pointer [`LibCec::open`] registered, which stays
/// valid until `marathon_cec_close` returns.
unsafe fn sender<'a>(user: *mut c_void) -> &'a EventSender {
    // SAFETY: per this function's contract.
    unsafe { &*user.cast::<EventSender>() }
}

unsafe extern "C" fn on_key(user: *mut c_void, keycode: c_int, duration: c_uint) {
    // SAFETY: libcec passes back the `user` we registered.
    let events = unsafe { sender(user) };
    // CEC user control codes are one byte; libcec reports anything else
    // as "unknown" (-1 or 0xFF), which maps to nothing.
    let Ok(key) = u8::try_from(keycode) else {
        return;
    };
    // libcec reports a press (and each repeat while held) with duration 0,
    // and the release with how long the key was down.
    events.send(if duration == 0 {
        CecEvent::KeyPressed(key)
    } else {
        CecEvent::KeyReleased(key)
    });
}

unsafe extern "C" fn on_source(user: *mut c_void, activated: c_int) {
    // SAFETY: as in `on_key`.
    let events = unsafe { sender(user) };
    events.send(CecEvent::Selected(activated != 0));
}

unsafe extern "C" fn on_command(user: *mut c_void, initiator: c_int, _: c_int, opcode: c_int) {
    // SAFETY: as in `on_key`.
    let events = unsafe { sender(user) };
    if let Ok(opcode) = u8::try_from(opcode)
        && let Some(event) = classify_command(initiator, opcode)
    {
        events.send(event);
    }
}

unsafe extern "C" fn on_alert(user: *mut c_void, alert: c_int) {
    // SAFETY: as in `on_key`.
    let events = unsafe { sender(user) };
    match alert {
        ALERT_CONNECTION_LOST => events.send(CecEvent::ConnectionLost),
        ALERT_PERMISSION_ERROR => {
            tracing::warn!("no permission to use the CEC adapter (check its device's group)")
        }
        ALERT_PORT_BUSY => tracing::warn!("the CEC adapter is in use by another program"),
        _ => tracing::debug!(alert, "libcec alert"),
    }
}

unsafe extern "C" fn on_log(_: *mut c_void, level: c_int, message: *const c_char) {
    if message.is_null() {
        return;
    }
    // SAFETY: libcec passes a NUL-terminated string valid for the call.
    let message = unsafe { CStr::from_ptr(message) }.to_string_lossy();
    match level {
        LOG_ERROR => tracing::warn!(target: "libcec", "{message}"),
        LOG_WARNING | LOG_NOTICE => tracing::debug!(target: "libcec", "{message}"),
        _ => tracing::trace!(target: "libcec", "{message}"),
    }
}

/// How long libcec may take to open an adapter (its own default).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// A libcec connection. See the module docs.
#[derive(Debug)]
pub struct LibCec {
    handle: Option<NonNull<c_void>>,
    /// The callbacks' `user` pointer; freed after the connection closes.
    events: Option<Box<EventSender>>,
    connect_timeout: Duration,
}

impl Default for LibCec {
    fn default() -> Self {
        Self {
            handle: None,
            events: None,
            connect_timeout: CONNECT_TIMEOUT,
        }
    }
}

// SAFETY: the handle is only used from the thread that owns the `LibCec`
// (open and close); libcec's own threads only see the boxed sender, which
// is `Sync`.
unsafe impl Send for LibCec {}

impl LibCec {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets how long opening an adapter may take (default 10s).
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }
}

impl CecAdapter for LibCec {
    fn open(&mut self, config: &CecConfig, events: EventSender) -> Result<(), CecError> {
        if self.handle.is_some() {
            self.close();
        }
        let text = |s: &str, what: &str| {
            CString::new(s).map_err(|_| CecError(format!("{what} contains a NUL byte")))
        };
        let name = text(&config.osd_name, "the OSD name")?;
        let port = config
            .adapter
            .as_deref()
            .map(|p| text(p, "the adapter path"))
            .transpose()?;
        let events = Box::new(events);
        let callbacks = Callbacks {
            key: on_key,
            source: on_source,
            command: on_command,
            alert: on_alert,
            log: on_log,
            user: std::ptr::from_ref::<EventSender>(&*events)
                .cast_mut()
                .cast(),
        };
        let mut error = [0 as c_char; 256];
        // SAFETY: every pointer is valid for the call; the shim copies the
        // callback table, and `events` (the `user` pointer) is kept alive
        // in `self` until after `marathon_cec_close`.
        let handle = unsafe {
            marathon_cec_open(
                name.as_ptr(),
                c_int::from(config.claim_on_start),
                c_int::from(config.hdmi_port.unwrap_or(0)),
                port.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()),
                // At least 1ms: 0 would mean libcec's default.
                u32::try_from(self.connect_timeout.as_millis())
                    .unwrap_or(u32::MAX)
                    .max(1),
                &callbacks,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        match NonNull::new(handle) {
            Some(handle) => {
                self.handle = Some(handle);
                self.events = Some(events);
                Ok(())
            }
            None => {
                // SAFETY: the shim always NUL-terminates `error` (it is
                // zeroed, and vsnprintf terminates what it writes).
                let message = unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy();
                Err(CecError(message.into_owned()))
            }
        }
    }

    fn close(&mut self) {
        if let Some(handle) = self.handle.take() {
            // SAFETY: `handle` came from `marathon_cec_open` and is closed
            // once. No callbacks run after this returns.
            unsafe { marathon_cec_close(handle.as_ptr()) };
        }
        self.events = None;
    }
}

impl Drop for LibCec {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;
    use crate::adapter::Msg;

    /// The callbacks translate libcec's reports, called the way the shim
    /// calls them.
    #[test]
    fn callbacks_translate_libcec_reports() {
        let (tx, rx) = mpsc::channel();
        let events = Box::new(EventSender(tx));
        let user = std::ptr::from_ref::<EventSender>(&*events)
            .cast_mut()
            .cast();
        // SAFETY: `user` points at a live EventSender.
        unsafe {
            on_key(user, 0x30, 0);
            on_key(user, 0x30, 120);
            on_key(user, -1, 0);
            on_source(user, 1);
            on_source(user, 0);
            on_command(user, 0, 15, 0x36);
            on_command(user, 0, 4, 0x82);
            on_alert(user, ALERT_CONNECTION_LOST);
            on_alert(user, ALERT_PORT_BUSY);
            on_log(user, LOG_ERROR, c"hello".as_ptr());
            on_log(user, LOG_ERROR, std::ptr::null());
        }
        drop(events);
        let got: Vec<CecEvent> = rx
            .try_iter()
            .map(|m| match m {
                Msg::Event(e) => e,
                Msg::Stop => panic!("unexpected stop"),
            })
            .collect();
        assert_eq!(
            got,
            vec![
                CecEvent::KeyPressed(0x30),
                CecEvent::KeyReleased(0x30),
                CecEvent::Selected(true),
                CecEvent::Selected(false),
                CecEvent::TvStandby,
                CecEvent::ConnectionLost,
            ]
        );
    }

    /// With no CEC hardware (as in CI), opening fails cleanly with a
    /// message rather than crashing, and can be retried.
    #[test]
    fn opening_without_an_adapter_fails_cleanly() {
        let (tx, _rx) = mpsc::channel();
        let mut cec = LibCec::new().with_connect_timeout(Duration::from_millis(200));
        let config = CecConfig {
            adapter: Some("/dev/marathon-no-such-cec-adapter".into()),
            ..CecConfig::default()
        };
        for _ in 0..2 {
            let err = cec.open(&config, EventSender(tx.clone())).unwrap_err();
            assert!(!err.0.is_empty());
        }
        cec.close();
    }
}
