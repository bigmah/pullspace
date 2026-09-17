//! The browser tab: what it is called.
//!
//! The name is worth setting. A review of four pull requests is four tabs, and
//! four tabs all called "pullspace" is a row of identical favicons to hunt
//! through.
//!
//! The icon is not set from here, but in the `index.html` beside `Cargo.toml`,
//! which `dx` builds the page from in place of its own. Safari looks for an
//! icon once, as the page finishes loading. That is before the wasm has run,
//! so a `<link rel="icon">` added from here is never asked for, and the tab
//! gets the 404 at `/favicon.ico` instead. Chrome picks up either.
//!
//! It is a telescope emoji set as SVG text, centred on both axes rather than
//! sat on a baseline, which lands it a different distance from the top in
//! every emoji font.

use dioxus::prelude::*;

use super::app::{St, Workspace};

/// What the tab is called: whatever is open, and the app after it.
fn title(ws: &Workspace) -> String {
    match ws {
        Workspace::Empty => "pullspace".to_string(),
        Workspace::Pr(pr) => format!("{} #{} · pullspace", pr.repo, pr.number),
        // The branch only when it is not the default one: four tabs on four
        // branches of one repository are four tabs to tell apart, and "the
        // repository" is what the default branch is usually called.
        Workspace::Repo(view) if view.default => format!("{} · pullspace", view.repo),
        Workspace::Repo(view) => format!("{} @ {} · pullspace", view.repo, view.branch),
        Workspace::Commit(view) => {
            format!("{} {} · pullspace", view.repo, view.commit.short())
        }
        Workspace::Compare(view) => {
            format!("{} {}...{} · pullspace", view.repo, view.base, view.head)
        }
    }
}

/// Draws nothing. The title is a head element — dioxus puts it in the document
/// rather than in the tree it is written in.
#[component]
pub fn Tab() -> Element {
    let st = use_context::<St>();
    // A space still on its way somewhere is named for where it is going. The
    // tab is the one part of the frame the browser keeps between the click and
    // the arrival — on a link opened in a background tab it is the *only*
    // part — so it should say what was clicked rather than the app's own name.
    let going = st
        .incoming
        .read()
        .as_ref()
        .filter(|_| !st.workspace.read().is_open())
        .and_then(|route| route.at.label());
    let name = match going {
        Some(going) => format!("{going} · pullspace"),
        None => title(&st.workspace.read()),
    };
    rsx! {
        document::Title { "{name}" }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tab_says_what_is_open() {
        assert_eq!(title(&Workspace::Empty), "pullspace");
    }
}
