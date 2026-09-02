//! Fullscreen: the app, with the browser's own furniture out of the way.
//!
//! Nothing about the app changes. The top bar, the explorer, the code and the
//! conversation are all still there and all still the same shape — what goes is
//! everything *around* them: the tab strip, the address bar, the bookmarks. A
//! review is a thing to sit inside for an hour, and on a laptop that furniture
//! is a fifth of the screen spent on somewhere you are not.
//!
//! The signal here is a record of what the browser is doing rather than
//! something this app decides, which is why nothing writes it but [`watch`].
//! The screen can be given back without the page hearing a click — Escape
//! leaves fullscreen, and so does the browser's own control for it — and a
//! request can be refused outright, so a button that lit up because it had been
//! pressed would be telling you what it had asked for rather than what
//! happened.

use dioxus::prelude::*;

use crate::backend::screen;

use super::app::St;

/// The browser's own answer to who has the screen, said out loud whenever it
/// changes.
///
/// On the document rather than on the element that went fullscreen: the event
/// fires at that element, which is above where any handler in this tree could
/// be attached. Both names for the same reason `backend::screen` needs both.
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
        let mut full = st.full;
        if *full.peek() != holding {
            full.set(holding);
        }
    }
}

/// Ask for the screen, or hand it back. What the button and the key both call.
///
/// It asks the browser which way round it is rather than reading the signal:
/// the signal is a report of the last thing the browser said, and this is a
/// question about the answer to the last thing it was asked.
pub fn toggle() {
    if screen::holding() {
        screen::leave();
    } else {
        screen::enter();
    }
}
