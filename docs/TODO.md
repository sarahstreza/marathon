# TODO

Known follow-ups that aren't tied to a milestone yet. Code that has a
`TODO` comment points here.

## Replace the libcec C shim with Pulse-Eight's `libcec` crate

PLAN.md named `cec-rs` for HDMI-CEC, but that crate is **deprecated**: its
README says new code should use Pulse-Eight's official
[`libcec` crate](https://crates.io/crates/libcec) instead. That crate only
builds against libcec **8.x**, and when M7 was built only Homebrew shipped
8 (Ubuntu 24.04 has 6.0, Fedora 44 has 7.1, Raspberry Pi OS isn't on 8
either).

So `marathon-input-cec` reaches libcec through its own small C shim
(`crates/marathon-input-cec/src/libcec/shim.c`), compiled against
whatever headers are installed. It works with libcec 4 and newer, but it
is our code to maintain.

**When to do it:** once the platforms we build on (Ubuntu CI, Fedora,
Raspberry Pi OS) ship libcec 8. Then swap `LibCec`'s internals for the
official crate. Keep the `CecAdapter` trait and everything above it as
they are, and keep the polite settings the shim applies (no active-source
claim unless asked, no wake/power-off devices, no combo key). Also update
PLAN.md §3 and §12, and drop `cc` from the workspace if nothing else uses
it.
