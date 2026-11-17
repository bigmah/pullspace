//! The clipboard.
//!
//! One call, and a best-effort one: the write needs a secure context and the
//! click that asked for it, and a browser is entitled to refuse either way.
//! Nothing here is load-bearing — the address bar says the same thing this
//! copies, so a refusal costs a ⌘C rather than the link.

use js_sys::{Array, Function, Reflect};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::Clipboard;

/// Put some text on the clipboard.
///
/// Call it from a click, not from a task started by one: browsers grant this
/// on the user gesture that is still in progress, and an `await` in between
/// spends it.
pub fn copy(text: &str) {
    let Some(win) = web_sys::window() else {
        return;
    };
    // Outside a secure context there is no `navigator.clipboard` at all, and
    // `web_sys` types it as always there: read by name and looked at first,
    // because calling into `undefined` throws and a throw stops the app.
    let clipboard = Reflect::get(&win.navigator(), &JsValue::from_str("clipboard"));
    let Some(clipboard) = clipboard.ok().filter(|c| c.is_object()) else {
        copy_selected(text);
        return;
    };
    let clipboard: Clipboard = clipboard.unchecked_into();
    let promise = clipboard.write_text(text);
    // Awaited only so that a refusal is a refusal rather than an unhandled
    // rejection in the console.
    wasm_bindgen_futures::spawn_local(async move {
        let _ = JsFuture::from(promise).await;
    });
}

/// The way that came before `navigator.clipboard`, and the only one a page
/// without it has: put the text in a field, select it, and have the document
/// copy its selection. That is pullspace embedded in a page read over plain
/// http — a phone on the same network as the machine serving it.
///
/// The field is off screen (`.clipfield`), read-only so that a phone does not
/// raise its keyboard for it, and gone again before anything is painted.
fn copy_selected(text: &str) -> Option<()> {
    let document = web_sys::window()?.document()?;
    let body = document.query_selector("body").ok()??;
    let focused = Reflect::get(&document, &JsValue::from_str("activeElement")).ok();
    let field = document.create_element("textarea").ok()?;
    field.set_class_name("clipfield");
    field.set_attribute("readonly", "").ok()?;
    field.set_text_content(Some(text));
    body.append_child(&field).ok()?;
    call(&field, "select", &[]);
    // `select` alone leaves a read-only field unselected on iOS.
    let end = JsValue::from_f64(text.encode_utf16().count() as f64);
    call(&field, "setSelectionRange", &[JsValue::from_f64(0.0), end]);
    call(&document, "execCommand", &[JsValue::from_str("copy")]);
    field.remove();
    // Selecting took the focus with it; whatever was clicked gets it back.
    if let Some(focused) = focused.filter(|f| f.is_object()) {
        call(&focused, "focus", &[]);
    }
    Some(())
}

/// Call a method by name, for the handful `web_sys` would want a feature each
/// for. `None` if it is not there or it threw.
fn call(target: &JsValue, method: &str, args: &[JsValue]) -> Option<JsValue> {
    let method: Function = Reflect::get(target, &JsValue::from_str(method))
        .ok()?
        .dyn_into()
        .ok()?;
    method.apply(target, &args.iter().collect::<Array>()).ok()
}
