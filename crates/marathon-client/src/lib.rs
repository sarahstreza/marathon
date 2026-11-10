//! Marathon client library: HTTP API client, the player-agnostic Tuner state
//! machine, the `Player` trait it drives, and the `Action` enum that all input
//! maps to.
//!
//! - [`api`]: the [`Api`](api::Api) calls a client makes, and
//!   [`HttpApi`](api::HttpApi) over HTTP.
//! - [`tuner`]: the [`Tuner`](tuner::Tuner) state machine (PLAN.md §9).
//! - [`player`]: the [`Player`](player::Player) trait the tuner drives.
//! - [`Action`] and [`keyboard`]: input.
//! - [`client_id`]: the per-installation client ID.

mod action;
pub mod api;
pub mod client_id;
pub mod keyboard;
pub mod player;
pub mod tuner;

pub use action::Action;
