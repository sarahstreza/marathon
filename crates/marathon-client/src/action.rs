//! The [`Action`] enum that every input method maps to (PLAN.md §9).

/// Something the viewer asked for. Keyboard, gamepad, and (later) remote
/// input all map to these, so every control method drives the same
/// behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// Tune to the next channel up (higher number, wrapping).
    ChannelUp,
    /// Tune to the next channel down (lower number, wrapping).
    ChannelDown,
    /// Tune to the channel with this number.
    GoToChannel(u32),
    TogglePause,
    /// Seek by this many seconds (negative is backwards).
    SeekRelative(i32),
    /// Skip to the channel's next item.
    NextItem,
    /// Step back to the channel's previous item.
    PreviousItem,
    /// Show what is playing (the channel banner, once there is an overlay).
    ShowInfo,
    /// Show or hide the channel guide.
    ToggleGuide,
    /// Close whatever is open.
    Back,
    /// Turn sidecar or embedded subtitles on or off. Not in PLAN.md §9's
    /// original list; added for M3's subtitle check.
    ToggleSubtitles,
    /// Switch between fullscreen and windowed. Handled by the window, not
    /// the tuner. Not in PLAN.md §9's original list; added for M3's
    /// fullscreen check.
    ToggleFullscreen,
    /// Report progress and exit.
    Quit,
}
