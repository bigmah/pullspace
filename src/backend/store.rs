//! Local storage: where a page keeps what a desktop app would put in
//! `~/.config`.
//!
//! It is per-origin, which is the whole security story of this build — the
//! token is readable by the page it was typed into and by nothing else.
//!
//! Every operation is best effort. Storage can be full, disabled, or refused
//! outright in a private window, and none of those are worth interrupting
//! anyone over: the cost is retyping a token, not losing work.

const PREFIX: &str = "pullspace.";

/// The GitHub token, as pasted into the sign-in form.
pub const TOKEN: &str = "token";
/// Pane sizes, as JSON.
pub const LAYOUT: &str = "layout";
/// Theme, accent, font and code size, as JSON.
pub const PREFS: &str = "prefs";
/// Which files of which pull requests have been marked read, as JSON.
pub const VIEWED: &str = "viewed";

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

pub fn get(key: &str) -> Option<String> {
    let value = storage()?
        .get_item(&format!("{PREFIX}{key}"))
        .ok()
        .flatten()?;
    // An empty string is how a cleared field reads back; treat it as absent so
    // callers do not have to.
    (!value.is_empty()).then_some(value)
}

pub fn set(key: &str, value: &str) {
    if let Some(s) = storage() {
        let _ = s.set_item(&format!("{PREFIX}{key}"), value);
    }
}

pub fn remove(key: &str) {
    if let Some(s) = storage() {
        let _ = s.remove_item(&format!("{PREFIX}{key}"));
    }
}
