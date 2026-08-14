//! The front page: what an empty workspace shows instead of the IDE.
//!
//! Opening something is the only thing there is to do from here, so the page
//! *is* the picker — the same search box and pull request list the overlay
//! carries — with the app's name over it and, for whoever has not seen it
//! before, three lines on what this is. A page rather than a modal, because
//! the modal's old backdrop was an empty IDE: a frame around nothing, dressed
//! as an app that had failed to load.

use dioxus::prelude::*;

use super::app::St;
use super::github::{PickerBody, PickerFoot, form_open};

/// What pullspace is, in the three claims that pick it out: the shape of the
/// review, the conversation beside it, and the absence of a server.
const POINTS: [(&str, &str); 3] = [
    (
        "Review like code",
        "The whole repository in an IDE-style file tree, with syntax-highlighted \
         diffs — split or unified — plus search and go-to-definition.",
    ),
    (
        "The whole story",
        "The description, discussion and review comments beside the code they are \
         about, and every commit of the branch readable one at a time.",
    ),
    (
        "No server, no account",
        "A static page: your browser talks to GitHub directly, and keeps what it \
         opens so the next visit is instant. Public repositories need no sign-in.",
    ),
];

#[component]
pub fn Landing() -> Element {
    let st = use_context::<St>();
    let mut prefs_open = st.prefs_open;
    let account = st.account.read().clone();
    // Whether the reader has asked for the token form — this page's copy of
    // the flag the overlay keeps, which can be a copy because the two frames
    // are never up at once.
    let token_form = use_signal(|| None::<bool>);
    let showing = form_open(&token_form, &account);

    rsx! {
        div { class: "landing",
            // The one control of the top bar that is about the app rather than
            // about something being open, which nothing is.
            button {
                class: "iconbtn lg landing-prefs",
                title: "Appearance — theme, accent, font  (⌘,)",
                onclick: move |_| prefs_open.set(true),
                span { class: "glyph", "◐" }
            }
            div { class: "landing-col",
                div { class: "landing-hero",
                    div { class: "landing-logo", "pullspace" }
                    div { class: "landing-tag", "Read pull requests the way you read code." }
                }
                PickerBody { autofocus: !showing }
                div { class: "landing-points",
                    for (title , body) in POINTS {
                        div { key: "{title}", class: "landing-point",
                            div { class: "landing-point-title", "{title}" }
                            div { class: "landing-point-body", "{body}" }
                        }
                    }
                }
                PickerFoot { form: token_form, showing }
            }
        }
    }
}
