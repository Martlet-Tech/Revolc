//! Shared logging (see `revolc-common`).
/// Legacy call site: the window process logs to `revolc.log`.
pub fn init() {
    revolc_common::logging::init("revolc");
}
