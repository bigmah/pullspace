//! The frame that stands in for a space while it is on its way somewhere.
//!
//! Every review this app opens is three or four requests deep, and until the
//! last of them lands there is no workspace to draw. What went up in the
//! meantime was the landing page — the app's front page, with its name, its
//! pitch and its picker — which is the right answer to "nothing is open" and
//! the wrong one to "the pull request you clicked is loading". A reader who
//! followed a link, switched spaces or came in from the extension has already
//! chosen; being shown the front page on the way past is being shown a page
//! they did not ask for, and being shown it for half a second is a flash of
//! somewhere else between them and where they were going.
//!
//! So an empty space that is going somewhere shows this instead: the name of
//! the place, and what is being fetched to get there. It is deliberately the
//! same column, at the same height, in the same colours as the page it
//! replaces and the top bar it becomes — nothing here moves when the workspace
//! lands, because there is nothing here that the workspace does not also have.

use dioxus::prelude::*;

use crate::backend::route::Route;

use super::app::{Fetch, St};
use super::spaces::SpaceSwitch;

/// What the reader can do while a space arrives, other than wait: leave.
///
/// The switcher is the way to the other reviews, and the way back out of the
/// arrival is to stop it — which is `close_workspace`, the same thing closing
/// a review is. It matters most for the case nothing else covers: a request
/// that never answers at all, which no error will ever arrive to clear.
#[component]
pub fn Opening(route: Route) -> Element {
    let st = use_context::<St>();
    // The place, named the way the switcher's card names it. `Home` is never
    // stored here — see `St::arriving_at` — so this is a name every time.
    let name = route.at.label().unwrap_or_default();
    // And the file inside it, for a link that pointed at one: the reason the
    // link was sent is usually the file, not the pull request around it.
    let file = route
        .place
        .as_ref()
        .and_then(|place| place.path.file_name())
        .map(|n| n.to_string_lossy().into_owned());
    // Which of the several requests is out. `Failed` puts the landing page
    // back rather than showing here — see `Claim::failed` — so the only two
    // states this ever draws are "working" and the moment before the first
    // request has been sent.
    let note = match &*st.fetch.read() {
        Fetch::Working(note) => note.clone(),
        Fetch::Idle | Fetch::Failed(_) => "Connecting to GitHub…".to_string(),
    };

    rsx! {
        div { class: "opening",
            div { class: "landing-spaces", SpaceSwitch {} }
            div { class: "opening-col",
                div { class: "opening-brand", "pullspace" }
                div { class: "opening-name", "{name}" }
                if let Some(file) = file {
                    div { class: "opening-file", "{file}" }
                }
                div { class: "opening-bar" }
                div { class: "opening-note", "{note}" }
                button {
                    class: "opening-stop",
                    title: "Stop opening this and go back to the picker",
                    onclick: move |_| st.close_workspace(),
                    "Cancel"
                }
            }
        }
    }
}
