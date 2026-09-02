//! The screen, asked for and given back.
//!
//! Two names for one thing: `requestFullscreen` is what every current browser
//! calls it, and `webkitRequestFullscreen` is what Safari called it until 16.4.
//! They are reached by name rather than through the typed binding so that the
//! second can stand in for the first where the first is not there — and so that
//! the promise the first answers with has somewhere to land.
//!
//! Nothing here is load-bearing. A browser is entitled to refuse the screen and
//! some of them always do: an iPhone has fullscreen for a video and for nothing
//! else, and a page inside a frame needs a permission it may not have been
//! given. A refusal costs the browser's own chrome rather than the mode — see
//! [`crate::ui`], where the app's frame is put away whether this lands or not.

use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;

/// Ask for the screen.
///
/// Call it from the click or the key that asked for it, not from a task
/// started by one: a browser grants this on the user gesture still in
/// progress, and an `await` in between spends it. The same rule as
/// [`super::clip::copy`].
pub fn enter() {
    let Some(root) = web_sys::window()
        .and_then(|win| win.document())
        .and_then(|doc| doc.document_element())
    else {
        return;
    };
    let root = JsValue::from(root);
    if !call(&root, "requestFullscreen") {
        call(&root, "webkitRequestFullscreen");
    }
}

/// Give it back.
///
/// Asked for only when it was granted: `exitFullscreen` on a document that has
/// no screen to give up is a rejected promise rather than a no-op, and coming
/// out of the app's own mode without the browser's is the ordinary case
/// wherever the request above was refused.
pub fn leave() {
    let Some(doc) = web_sys::window().and_then(|win| win.document()) else {
        return;
    };
    let doc = JsValue::from(doc);
    if !holding(&doc) {
        return;
    }
    if !call(&doc, "exitFullscreen") {
        call(&doc, "webkitExitFullscreen");
    }
}

/// Whether the browser is holding the screen for this page.
fn holding(doc: &JsValue) -> bool {
    ["fullscreenElement", "webkitFullscreenElement"]
        .iter()
        .any(
            |name| match js_sys::Reflect::get(doc, &JsValue::from_str(name)) {
                Ok(found) => !found.is_null() && !found.is_undefined(),
                Err(_) => false,
            },
        )
}

/// Call one of the four methods above, if this browser has it. `false` means it
/// does not, which is the cue to try the other name for the same thing.
fn call(on: &JsValue, name: &str) -> bool {
    let Ok(found) = js_sys::Reflect::get(on, &JsValue::from_str(name)) else {
        return false;
    };
    let Some(method) = found.dyn_ref::<js_sys::Function>() else {
        return false;
    };
    let Ok(answer) = method.call0(on) else {
        return false;
    };
    // The unprefixed pair answer with a promise. Awaited only so that a refusal
    // is a refusal rather than an unhandled rejection in the console — nothing
    // here is waiting on the outcome, the layout has already moved.
    if let Ok(promise) = answer.dyn_into::<js_sys::Promise>() {
        wasm_bindgen_futures::spawn_local(async move {
            let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
        });
    }
    true
}
