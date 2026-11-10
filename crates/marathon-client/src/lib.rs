//! Marathon client library: HTTP API client, the player-agnostic Tuner state
//! machine, the `Player` trait it drives, and the `Action` enum that all input
//! maps to.

mod action;
pub mod api;
pub mod client_id;
pub mod keyboard;

pub use action::Action;
