//! Marathon server library: config, catalog, sources, database, HTTP routes,
//! and media streaming. Used by `marathond` and by the desktop client's
//! embedded mode.

pub mod catalog;
pub mod config;
pub mod db;
mod error;
pub mod parse;
pub mod scan;

pub use error::{ApiError, Error};
