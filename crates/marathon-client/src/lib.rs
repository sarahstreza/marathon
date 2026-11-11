//! Marathon client library: HTTP API client, the player-agnostic Tuner state
//! machine, the `Player` and `InputSource` traits, and the action bus.
//!
//! - [`api`]: the [`Api`](api::Api) calls a client makes, and
//!   [`HttpApi`](api::HttpApi) over HTTP.
//! - [`tuner`]: the [`Tuner`](tuner::Tuner) state machine (PLAN.md §9).
//! - [`player`]: the [`Player`](player::Player) trait the tuner drives.
//! - [`bus`], [`input`]: the action bus and the
//!   [`InputSource`](input::InputSource) adapters that feed it.
//! - [`navigation`]: resolving `Navigate`/`Select`/`Back` by context.
//! - [`ui`]: the cues the tuner gives the on-screen overlay.
//! - [`keyboard`] (feature `input-keyboard`): keyboard bindings.
//! - [`client_id`]: the per-installation client ID.
//!
//! The [`Action`](marathon_api::Action) and [`Event`](marathon_api::Event)
//! types live in `marathon-api`, because the server relays them.
//!
//! This crate must not depend on windowing or device libraries (`winit`,
//! `glutin`, `gilrs`); `scripts/check-seams.sh` enforces it.

pub mod api;
pub mod bus;
pub mod client_id;
pub mod input;
#[cfg(feature = "input-keyboard")]
pub mod keyboard;
pub mod navigation;
pub mod player;
pub mod tuner;
pub mod ui;
