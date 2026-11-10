//! Pure programming-rules engine for Marathon channels.
//!
//! A rule decides which item a channel plays next. This crate must stay pure:
//! no I/O, no clocks, no global RNG, and no dependencies on other Marathon
//! crates. Randomness (added with the engine in M1) comes from a seed stored in
//! the cursor.
