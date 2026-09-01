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
/// The spaces this browser tab has open, as JSON — see [`crate::ui::spaces`].
/// Session-scoped, unlike everything above it: spaces belong to the window
/// they were opened in, and a second window is a second set of them.
pub const SPACES: &str = "spaces";

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

/// The other one: cleared when the browser tab closes, and — unlike local
/// storage — not shared with any other tab on this origin.
///
/// Which is exactly the lifetime a set of spaces has. Reloading is a mistake
/// somebody makes with twelve reviews open, and it should cost nothing;
/// opening pullspace in a second window is asking for a second desk, not for
/// this one's contents to arrive in it and be fought over by two tabs writing
/// the same key.
fn session() -> Option<web_sys::Storage> {
    web_sys::window()?.session_storage().ok().flatten()
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

/// [`get`], out of the session rather than out of the origin.
pub fn session_get(key: &str) -> Option<String> {
    let value = session()?
        .get_item(&format!("{PREFIX}{key}"))
        .ok()
        .flatten()?;
    (!value.is_empty()).then_some(value)
}

/// [`set`], likewise.
pub fn session_set(key: &str, value: &str) {
    if let Some(s) = session() {
        let _ = s.set_item(&format!("{PREFIX}{key}"), value);
    }
}
