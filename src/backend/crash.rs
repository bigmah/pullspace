//! The page, once the program under it has stopped.
//!
//! A panic in a browser is not an error anything can catch. wasm32 does not
//! unwind: the panic runs its hook, the module traps where it stood, and
//! nothing after that runs — no `Claim::failed`, no error on the landing page,
//! no next frame. What stays up is the last frame painted, which is usually the
//! Opening screen, its bar sweeping forever over a request that finished long
//! ago. That reads as a slow network rather than a broken app, and the one
//! place the truth is written down is the console.
//!
//! So the way down paints one more thing: a screen of its own over all of it,
//! saying that the app stopped, what it stopped on, and the two ways out.
//!
//! All of it is page script, installed while the module is still well. A panic
//! reaches it from the hook, which runs before the trap; everything else that
//! stops the module reaches it without the module at all, and has to. The stack
//! running out is the case that decides it: it leaves Rust's stack pointer past
//! the end of its stack, so the next call into the module — a listener written
//! in Rust, say — traps as well, before it has drawn a thing.

use std::panic;

use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;

/// Put the screen under every way the module can stop.
///
/// Call it after `dioxus::launch`, which on the web returns as soon as the app
/// is scheduled, before it has drawn anything. Launching installs the logger's
/// hook — the one that writes the panic to the console — and a hook set before
/// that would be replaced by it rather than run beside it.
pub fn catch() {
    let install = js_sys::Function::new_with_args("css", CRASH_JS);
    if install
        .call1(&JsValue::UNDEFINED, &JsValue::from_str(STYLE))
        .is_err()
    {
        return;
    }
    let before = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        before(info);
        show(&info.to_string());
    }));
}

/// Put the screen up with this on it, from Rust — the panic hook, while the
/// module can still make a call.
fn show(what: &str) {
    let Some(win) = web_sys::window() else {
        return;
    };
    if let Ok(paint) = js_sys::Reflect::get(&win, &JsValue::from_str(PAINT))
        && let Some(paint) = paint.dyn_ref::<js_sys::Function>()
    {
        let _ = paint.call1(&JsValue::NULL, &JsValue::from_str(what));
    }
}

/// Where [`CRASH_JS`] leaves the function that paints the screen.
const PAINT: &str = "__pullspace_crash";

/// The screen, and the two listeners that put it up for whatever never reaches
/// the panic hook: an allocation the memory cannot hold, which aborts without
/// one, and the stack running out. Both arrive as something thrown out of
/// whatever called in — a click's handler, a task — which the page reports as
/// an uncaught error or an unhandled rejection.
///
/// A trap says what it is by its type. The stack running out does not always:
/// a browser may throw its own `RangeError` from whichever frame it ran out in.
/// So the rest are told by their stacks, where every engine writes a wasm frame
/// as `wasm-function[n]` — anything thrown through the module has left it in
/// the middle of something, and the next call in finds it that way. An error
/// in page script alone, like one of the `document::eval` snippets throwing,
/// has none and is left to the console.
///
/// Every piece of text goes in as text, never as markup: the message can quote
/// whatever was being read when it stopped, which is somebody else's pull
/// request.
const CRASH_JS: &str = r#"
var shown = false;
function make(tag, cls, text) {
  var el = document.createElement(tag);
  if (cls) el.className = cls;
  if (text) el.textContent = text;
  return el;
}
function paint(what) {
  // The first failure is the one worth reading; any after it are usually the
  // dead app being clicked on.
  if (shown) return;
  shown = true;
  var screen = make('div');
  screen.id = 'crash';
  screen.setAttribute('role', 'alertdialog');
  screen.setAttribute('aria-modal', 'true');
  screen.setAttribute('aria-labelledby', 'crash-title');
  var title = make('div', 'title', 'Something went wrong');
  title.id = 'crash-title';
  var reload = make('button', '', 'Reload');
  reload.title = 'Load the page again and retry what was open';
  reload.onclick = function () { location.reload(); };
  // The page with nothing after its path. What was open lives in the fragment
  // — or, from the extension, in `?url=` — so reloading can walk straight back
  // into whatever stopped it; this is the landing page instead.
  var home = make('a', '', 'Start over');
  home.href = location.pathname;
  home.title = 'Go back to the picker';
  var ways = make('div', 'ways');
  ways.append(reload, home);
  var col = make('div', 'col');
  col.append(
    make('div', 'brand', 'pullspace'),
    title,
    make('div', 'note', 'pullspace hit a bug and had to stop. Reloading starts it again.'),
    make('pre', '', String(what)),
    ways
  );
  screen.append(make('style', '', css), col);
  // The dead app, out of reach: nothing under the screen can be tabbed to,
  // clicked, or read out, since none of it will ever answer again.
  var app = document.getElementById('main');
  if (app) app.setAttribute('inert', '');
  document.body.append(screen);
  // So that Enter reloads.
  reload.focus();
}
function fatal(thrown) {
  return thrown instanceof WebAssembly.RuntimeError
    || (thrown != null && typeof thrown.stack === 'string'
        && thrown.stack.indexOf('wasm-function[') >= 0);
}
addEventListener('error', function (e) {
  if (fatal(e.error)) paint(e.error);
});
addEventListener('unhandledrejection', function (e) {
  if (fatal(e.reason)) paint(e.reason);
});
window.__pullspace_crash = paint;
"#;

/// The screen's own stylesheet, which it carries with it: the app's may never
/// have been put on the page. Every colour is the app's variable where there is
/// one, so a light theme stays light, with the dark default behind it.
const STYLE: &str = r#"
#crash {
  position: fixed; inset: 0; z-index: 2147483647;
  display: flex; justify-content: center; overflow: auto;
  background: var(--bg, #17181d); color: var(--fg, #d4d8e0);
  font: 13px/1.5 var(--sans, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif);
  -webkit-font-smoothing: antialiased;
}
#crash .col {
  width: 560px; max-width: calc(100vw - 32px); padding: 22vh 0 48px;
  display: flex; flex-direction: column; align-items: center; text-align: center;
}
#crash .brand {
  font: 700 11px var(--mono, ui-monospace, Menlo, monospace);
  letter-spacing: 1.4px; text-transform: uppercase; color: var(--fg-faint, #848a9c);
}
#crash .title { margin-top: 12px; font-size: 17px; font-weight: 600; color: var(--fg-bright, #e8ecf4); }
#crash .note { margin-top: 6px; font-size: 12.5px; color: var(--fg-dim, #9aa0b0); }
#crash pre {
  align-self: stretch; margin: 18px 0 0; padding: 8px 10px; max-height: 40vh; overflow: auto;
  text-align: left; white-space: pre-wrap; overflow-wrap: anywhere;
  font: 12px/1.5 var(--mono, ui-monospace, Menlo, monospace);
  color: var(--deleted, #e06c75); background: var(--del-bg, rgba(224, 108, 117, 0.13));
  border: 1px solid rgba(224, 108, 117, 0.3); border-radius: var(--r, 7px);
  -webkit-user-select: text; user-select: text;
}
#crash .ways { margin-top: 18px; display: flex; align-items: center; gap: 18px; }
#crash button {
  height: 32px; padding: 0 15px; cursor: pointer;
  background: var(--accent, #5c9cf5); border: 1px solid var(--accent, #5c9cf5);
  border-radius: var(--r, 7px); color: var(--on-accent, #0f1116);
  font: 600 12px var(--sans, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif);
}
#crash button:hover { filter: brightness(1.12); }
#crash a { font-size: 11.5px; color: var(--fg-faint, #848a9c); text-decoration: none; }
#crash a:hover { color: var(--fg-dim, #9aa0b0); text-decoration: underline; }
"#;
