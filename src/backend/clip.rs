//! The clipboard.
//!
//! One call, and a best-effort one: the write needs a secure context and the
//! click that asked for it, and a browser is entitled to refuse either way.
//! Nothing here is load-bearing — the address bar says the same thing this
//! copies, so a refusal costs a ⌘C rather than the link.

use wasm_bindgen_futures::JsFuture;

/// Put some text on the clipboard.
///
/// Call it from a click, not from a task started by one: browsers grant this
/// on the user gesture that is still in progress, and an `await` in between
/// spends it.
pub fn copy(text: &str) {
    let Some(win) = web_sys::window() else {
        return;
    };
    let promise = win.navigator().clipboard().write_text(text);
    // Awaited only so that a refusal is a refusal rather than an unhandled
    // rejection in the console.
    wasm_bindgen_futures::spawn_local(async move {
        let _ = JsFuture::from(promise).await;
    });
}
