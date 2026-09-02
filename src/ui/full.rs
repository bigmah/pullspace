//! Fullscreen: the file, the whole screen, and nothing around it.
//!
//! One control doing two things, because they are one intention. The browser is
//! asked for the screen — its tabs and its address bar go — and the app puts
//! away its own frame with them: the top bar, the explorer, the conversation.
//! What is left is the file, the strip of what else is open, and the thin
//! header saying which file this is.
//!
//! The two halves are deliberately not tied together. The signal here is what
//! the layout reads, and it is set whether or not the request lands, so a
//! browser that will not give up the screen costs its own chrome rather than
//! the mode — see [`crate::backend::screen`], which is entitled to be refused.
//!
//! What is granted can be taken back without a word. Escape leaves fullscreen,
//! and so does the browser's own control for it, and the page sees neither as a
//! key or as a click; [`watch`] is the ear for that. Without it the frame would
//! stay put away with the window back to its ordinary size.

use dioxus::prelude::*;

use crate::backend::screen;

use super::app::St;

/// The browser's own idea of who has the screen, said out loud whenever it
/// changes.
///
/// On the document rather than on the app's root element: the event fires at
/// the element that went fullscreen, and both of the names below are needed for
/// the same reason the two request methods are.
const WATCH: &str = r#"
(function () {
  // A reload of this page's script should not leave the last listener behind.
  if (window.__pullspace_full) window.__pullspace_full();
  var on = function () {
    dioxus.send(!!(document.fullscreenElement || document.webkitFullscreenElement));
  };
  document.addEventListener('fullscreenchange', on);
  document.addEventListener('webkitfullscreenchange', on);
  window.__pullspace_full = function () {
    document.removeEventListener('fullscreenchange', on);
    document.removeEventListener('webkitfullscreenchange', on);
  };
})();
"#;

/// Follow it, for as long as the app is up.
pub async fn watch(st: St) {
    let mut eval = document::eval(WATCH);
    while let Ok(holding) = eval.recv::<bool>().await {
        // Only the leaving is news. Going in is this app's own doing, and the
        // signal is already set by the time the browser is asked — while
        // something else on the page can take the screen without it being
        // anything to do with the frame: a video in a previewed HTML file, say.
        let mut full = st.full;
        if !holding && *full.peek() {
            full.set(false);
        }
    }
}

/// Go in, or come back out. What the button and the key both call.
pub fn toggle(st: St) {
    let mut full = st.full;
    let want = !*full.peek();
    full.set(want);
    if want {
        screen::enter();
    } else {
        screen::leave();
    }
}

/// Come back out. Does nothing when it is already out, so Escape — and the two
/// shortcuts that need the frame back to have anywhere to put the cursor — can
/// call it without asking first.
pub fn leave(st: St) {
    let mut full = st.full;
    if !*full.peek() {
        return;
    }
    full.set(false);
    screen::leave();
}

/// The way out, drawn at the end of whichever header the middle pane has.
///
/// It draws nothing at all when there is nothing to leave, which is what lets
/// its three call sites be one line each rather than a condition apiece. Three,
/// because in fullscreen there is no one header to put it in: a file has one, a
/// description has another, and a commit with nothing picked out of it yet has
/// neither.
#[component]
pub fn LeaveFull() -> Element {
    let st = use_context::<St>();
    if !*st.full.read() {
        return rsx! {};
    }
    rsx! {
        button {
            class: "leavefull",
            title: "Leave fullscreen — the top bar, the explorer and the conversation come back  (Esc)",
            onclick: move |_| leave(st),
            span { class: "glyph", "\u{26f6}" }
            "leave"
        }
    }
}
