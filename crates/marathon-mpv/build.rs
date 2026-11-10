//! Links the system libmpv, found with `pkg-config`.
//!
//! Install it with the distro's mpv development package (`mpv-devel` on
//! Fedora, `libmpv-dev` on Debian/Ubuntu) or `brew install mpv` on macOS.
//! The render API and the event fields this crate reads need client API
//! 2.0 (mpv 0.35) or newer; `mpv.pc` carries the client API version.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if let Err(err) = pkg_config::Config::new()
        .atleast_version("2.0")
        .probe("mpv")
    {
        // `cargo:warning` is the only way a build script can explain
        // itself; panicking would hide the hint behind a backtrace.
        println!("cargo:warning=libmpv (client API >= 2.0) not found via pkg-config: {err}");
        println!(
            "cargo:warning=install mpv's development package (mpv-devel, libmpv-dev, or `brew install mpv`)"
        );
        std::process::exit(1);
    }
}
