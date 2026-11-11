//! HDMI-CEC input for Marathon (PLAN.md §9, M7): the TV's own remote
//! controls Marathon, and switching the TV's input pauses and resumes it.
//!
//! [`CecSource`] is an [`InputSource`](marathon_client::input::InputSource)
//! like the keyboard. It runs a [`CecAdapter`] and turns what happens on
//! the CEC bus into actions:
//!
//! - [`keys`]: remote keys to actions (D-pad, OK, Back, channel, digits,
//!   play controls, Info/Guide, user-mappable colour buttons).
//! - [`lifecycle`]: the TV switching away or going to standby to
//!   [`Action::Standby`](marathon_api::Action::Standby), and switching
//!   back to [`Action::Wake`](marathon_api::Action::Wake).
//!
//! Marathon is a polite guest on input 4. With the default [`CecConfig`]
//! it registers as a playback device named "Marathon", never asks to
//! become the active source (so turning the TV on never pulls it away
//! from input 1), and never powers the TV on or off.
//!
//! All of this is independent of libcec and hardware, and tested against a
//! mock adapter.

pub mod adapter;
pub mod keys;
pub mod lifecycle;
pub mod source;

use std::time::Duration;

pub use adapter::{CecAdapter, CecError, CecEvent, EventSender};
pub use keys::{ColourButtons, parse_action};
pub use source::CecSource;

/// How Marathon appears and behaves on the CEC bus.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CecConfig {
    /// The name the TV shows for Marathon's input. libcec truncates it to
    /// fit CEC's OSD names (14 characters).
    pub osd_name: String,
    /// The adapter to use (e.g. `/dev/cec0` or a Pulse-Eight's serial
    /// port); `None` uses the first one found.
    pub adapter: Option<String>,
    /// The TV's HDMI input Marathon is plugged into, if libcec cannot
    /// detect it (some adapters cannot read the physical address).
    pub hdmi_port: Option<u8>,
    /// Become the active source (switching the TV to Marathon, and turning
    /// it on) when the adapter opens. Off by default: Marathon is input 4
    /// and must not take over the TV (PLAN.md §12).
    pub claim_on_start: bool,
    /// Resume playback when the TV switches to Marathon's input. On by
    /// default.
    pub resume_on_select: bool,
    /// What the colour buttons do.
    pub colours: ColourButtons,
    /// How often to retry opening the adapter while it is missing.
    pub retry_every: Duration,
}

impl Default for CecConfig {
    fn default() -> Self {
        Self {
            osd_name: "Marathon".to_owned(),
            adapter: None,
            hdmi_port: None,
            claim_on_start: false,
            resume_on_select: true,
            colours: ColourButtons::default(),
            retry_every: Duration::from_secs(5),
        }
    }
}
