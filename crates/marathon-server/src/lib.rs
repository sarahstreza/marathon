//! Marathon server library: config, catalog, sources, database, HTTP routes,
//! and media streaming. Used by `marathond` and by the desktop client's
//! embedded mode.

mod app;
pub mod catalog;
pub mod config;
pub mod db;
mod error;
pub mod parse;
mod playback;
pub mod scan;

pub use app::{App, DATABASE_FILE, StartError};
pub use error::{ApiError, Error};
