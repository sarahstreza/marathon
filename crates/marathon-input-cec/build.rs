//! With the `libcec` feature: compiles `src/libcec/shim.c` against the
//! installed libcec headers and links libcec, both found with
//! `pkg-config`.
//!
//! The shim is why there are no hand-maintained struct layouts here:
//! libcec's configuration struct grows with every major version, and C
//! code compiled against the local headers always gets it right. Install
//! libcec's development package (`libcec-devel` on Fedora, `libcec-dev` and
//! `libp8-platform-dev` on Debian/Ubuntu) or `brew install libcec` on macOS.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(feature = "libcec")]
    libcec();
}

#[cfg(feature = "libcec")]
fn libcec() {
    println!("cargo:rerun-if-changed=src/libcec/shim.c");
    let probe = |link: bool| {
        pkg_config::Config::new()
            .atleast_version("4.0.0")
            .cargo_metadata(link)
            .probe("libcec")
    };
    // Headers first; the link flags are printed after the shim's, so the
    // linker sees libcec after the code that uses it.
    let lib = match probe(false) {
        Ok(lib) => lib,
        Err(err) => {
            // `cargo:warning` is the only way a build script can explain
            // itself; panicking would hide the hint behind a backtrace.
            println!("cargo:warning=libcec (4.0 or newer) not found via pkg-config: {err}");
            println!(
                "cargo:warning=install libcec's development package (libcec-devel; libcec-dev and libp8-platform-dev; or `brew install libcec`), or build without the `input-cec` feature"
            );
            std::process::exit(1);
        }
    };
    cc::Build::new()
        .file("src/libcec/shim.c")
        .includes(&lib.include_paths)
        .warnings(true)
        .compile("marathon_cec_shim");
    if let Err(err) = probe(true) {
        println!("cargo:warning=cannot link libcec: {err}");
        std::process::exit(1);
    }
}
