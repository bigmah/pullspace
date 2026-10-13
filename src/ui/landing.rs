//! The front page: what an empty workspace shows instead of the IDE.
//!
//! Opening something is the only thing there is to do from here, and the
//! quickest way to open something is the link already on the clipboard — the
//! pull request somebody asked you to look at arrives as a URL, not as a name
//! to go looking for. So the page is one big box to paste it into, which says
//! what it has recognised as you type and opens a pasted link without waiting
//! for Enter. Searching GitHub is still here, one click down, for whoever has
//! no link in hand. A page rather than a modal, because the modal's old
//! backdrop was an empty IDE: a frame around nothing, dressed as an app that
//! had failed to load.

use std::time::Duration;

use dioxus::prelude::*;

use crate::backend::github::{RepoRef, parse_commit_target, parse_target, strip_host};
use crate::backend::route::{self, Link, Target};

use super::app::{Account, St};
use super::compat;
use super::github::{
    GithubMark, PickerBody, PickerFoot, PrSection, browse_repo, form_open, open_target,
};
use super::spaces::SpaceSwitch;

/// What pullspace is, in the three claims that pick it out: the shape of the
/// review, the conversation beside it, and the absence of a server.
const POINTS: [(&str, &str, &str); 3] = [
    (
        "⌥",
        "Review like code",
        "The whole repository in an IDE-style file tree, with syntax-highlighted \
         diffs — split or unified — plus search and go-to-definition.",
    ),
    (
        "❝",
        "The whole story",
        "The description, discussion and review comments beside the code they are \
         about, and every commit of the branch readable one at a time.",
    ),
    (
        "⇄",
        "No server, no account",
        "A static page: your browser talks to GitHub directly, and keeps what it \
         opens so the next visit is instant. Public repositories need no sign-in.",
    ),
];

/// Links to click for whoever arrived with nothing on the clipboard. This
/// app's own repository, because it is the one every copy of pullspace can
/// vouch for — and reading its own pull requests is the demo.
const EXAMPLES: [(&str, &str); 3] = [
    (
        "A pull request",
        "https://github.com/bigmah/pullspace/pull/5",
    ),
    (
        "A commit",
        "https://github.com/bigmah/pullspace/commit/f956aa9",
    ),
    ("A whole repo", "https://github.com/bigmah/pullspace"),
];

/// How often to look in on the saved token before opening a link pasted while
/// it is still being checked — the same wait `nav::landing` makes for a link
/// arrived on.
const POLL: Duration = Duration::from_millis(50);

/// What a piece of pasted text will open, read the way [`open_target`] will
/// read it — in the same order, so the line under the box cannot promise one
/// thing and Enter do another.
#[derive(Clone, Debug, PartialEq)]
enum Seen {
    Pr(RepoRef, u64),
    Commit(RepoRef, String),
    Compare(RepoRef, String, String),
    Branch(RepoRef, String),
    /// A github.com `blob/…` or `tree/…` link, whose ref and path only the
    /// repository can split.
    Path(RepoRef),
    Repo(RepoRef),
}

impl Seen {
    /// The kind, as the chip under the box names it.
    fn kind(&self) -> &'static str {
        match self {
            Seen::Pr(..) => "Pull request",
            Seen::Commit(..) => "Commit",
            Seen::Compare(..) => "Comparison",
            Seen::Branch(..) => "Branch",
            Seen::Path(..) => "File",
            Seen::Repo(..) => "Repository",
        }
    }

    /// Which one, in a line.
    fn what(&self) -> String {
        match self {
            Seen::Pr(repo, n) => format!("{repo} #{n}"),
            Seen::Commit(repo, sha) => format!("{repo} @ {}", &sha[..sha.len().min(7)]),
            Seen::Compare(repo, base, head) => format!("{repo} · {base}…{head}"),
            Seen::Branch(repo, branch) => format!("{repo} @ {branch}"),
            Seen::Path(repo) | Seen::Repo(repo) => repo.to_string(),
        }
    }
}

fn recognise(raw: &str) -> Option<Seen> {
    let raw = raw.trim();
    // A URL off some other site: `parse_target` would read its host as an
    // owner, and promising `https:/example.com` as a repository is no help.
    if raw.contains("://") && strip_host(raw).is_none() {
        return None;
    }
    if let Some(link) = route::github_link(raw) {
        return Some(match link {
            Link::At(route) => match route.at {
                Target::Pr(repo, n) => Seen::Pr(repo, n),
                Target::Commit(repo, sha) => Seen::Commit(repo, sha),
                Target::Compare(repo, base, head) => Seen::Compare(repo, base, head),
                Target::Branch(repo, branch) => Seen::Branch(repo, branch),
                Target::Repo(repo) => Seen::Repo(repo),
                Target::Home => return None,
            },
            Link::Ref { repo, .. } => Seen::Path(repo),
        });
    }
    if let Some((repo, sha)) = parse_commit_target(raw) {
        return Some(Seen::Commit(repo, sha));
    }
    if let Some((repo, base, head)) = route::compare_target(raw) {
        return Some(Seen::Compare(repo, base, head));
    }
    if let Some((repo, branch)) = route::branch_target(raw) {
        return Some(Seen::Branch(repo, branch));
    }
    match parse_target(raw)? {
        (repo, Some(n)) => Some(Seen::Pr(repo, n)),
        (repo, None) => Some(Seen::Repo(repo)),
    }
}

/// Whether an edit to the box looks like a paste of a whole link rather than
/// typing: it went from empty to a full URL in one step. That is what opens
/// without waiting for Enter — a URL is never typed out a letter at a time, and
/// the person who pasted one has already said what they want.
fn pasted_link(before: &str, after: &str) -> bool {
    before.trim().is_empty() && after.trim().starts_with("http") && recognise(after).is_some()
}

/// Open what is in the box — once the saved sign-in has been checked, so a
/// link to a private pull request pasted in the first half-second does not go
/// out anonymously and come back 404.
fn open_typed(st: St) {
    // Root scope: what loads replaces the page this was started from.
    spawn_forever(async move {
        while matches!(*st.account.peek(), Account::Checking) {
            compat::sleep(POLL).await;
        }
        open_target(st).await;
    });
}

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
    // Paste first; search on request. The two boxes share `repo_input`, so
    // whatever was typed into one is waiting in the other.
    let mut searching = use_signal(|| false);

    rsx! {
        div { class: "landing",
            // The backdrop: a glow of the accent and a fading grid, drawn
            // behind everything and taking no clicks.
            div { class: "landing-glow", "aria-hidden": "true" }
            // The two controls of the top bar that are about the app rather
            // than about something being open, which nothing is. The switcher
            // matters more here than anywhere: an empty space is one somebody
            // has just opened, and the way back to the review they opened it
            // beside has to be on the page they are looking at.
            div { class: "landing-spaces", SpaceSwitch {} }
            button {
                class: "iconbtn lg landing-prefs",
                title: "Appearance — theme, accent, font  (⌘,)",
                onclick: move |_| prefs_open.set(true),
                span { class: "glyph", "◐" }
            }
            div { class: "landing-col",
                div { class: "landing-hero",
                    div { class: "landing-logo", "pullspace" }
                    h1 { class: "landing-head",
                        "Paste a pull request."
                        br {}
                        span { class: "landing-head-dim", "Read it like code." }
                    }
                    div { class: "landing-tag",
                        "Drop any GitHub link below — a PR, a commit, a branch, a file — "
                        "and it opens in a full IDE, right here in your browser."
                    }
                }
                if *searching.read() {
                    div { class: "landing-search",
                        PickerBody { autofocus: !showing }
                    }
                    button {
                        class: "landing-switch",
                        onclick: move |_| searching.set(false),
                        "← Back to pasting a link"
                    }
                } else {
                    PasteBar { autofocus: !showing, searching }
                    PrSection {}
                    div { class: "landing-or",
                        span { "No link handy?" }
                        button {
                            class: "landing-switch",
                            onclick: move |_| searching.set(true),
                            "Search GitHub instead"
                        }
                        span { class: "landing-or-dot", "·" }
                        button {
                            class: "landing-switch",
                            title: "Open bigmah/pullspace at its default branch — this app, reading its own source",
                            onclick: move |_| {
                                let repo = RepoRef {
                                    owner: "bigmah".to_string(),
                                    name: "pullspace".to_string(),
                                };
                                // The picker's box follows, as it does for a
                                // link arrived on: the panel opened later
                                // should offer this repository's pull requests
                                // rather than a blank box.
                                let mut input = st.repo_input;
                                input.set(repo.to_string());
                                // Root scope: what loads replaces this page.
                                spawn_forever(browse_repo(st, repo));
                            },
                            "Explore pullspace's own source"
                        }
                    }
                }
                div { class: "landing-points",
                    for (icon , title , body) in POINTS {
                        div { key: "{title}", class: "landing-point",
                            div { class: "landing-point-icon", "{icon}" }
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

/// The box a link goes in, what it makes of the link, and links to try.
///
/// What is typed and is not a link is most likely a name, and a name is what
/// search is for — so Enter on one goes there, with the name already in the
/// box, rather than failing to open it.
#[component]
fn PasteBar(autofocus: bool, searching: Signal<bool>) -> Element {
    let st = use_context::<St>();
    let mut searching = searching;
    let mut repo_input = st.repo_input;
    let typed = repo_input.read().clone();
    let seen = recognise(&typed);
    let blank = typed.trim().is_empty();
    let class = if seen.is_some() {
        "landing-paste ok"
    } else {
        "landing-paste"
    };

    rsx! {
        div { class: "{class}",
            GithubMark {}
            input {
                class: "landing-paste-input",
                r#type: "text",
                placeholder: "https://github.com/owner/repo/pull/123",
                spellcheck: "false",
                autocomplete: "off",
                autocapitalize: "off",
                "aria-label": "GitHub link to open",
                value: "{typed}",
                // The page is here to be pasted into, so it arrives ready to
                // take a ⌘V.
                onmounted: move |e| async move {
                    if autofocus {
                        let _ = e.set_focus(true).await;
                    }
                },
                oninput: move |e| {
                    let before = repo_input.peek().clone();
                    let after = e.value();
                    let go = pasted_link(&before, &after);
                    repo_input.set(after);
                    if go {
                        open_typed(st);
                    }
                },
                onkeydown: move |e| {
                    let typed = repo_input.peek().clone();
                    if e.key() != Key::Enter || typed.trim().is_empty() {
                        return;
                    }
                    if recognise(&typed).is_some() {
                        open_typed(st);
                    } else {
                        searching.set(true);
                    }
                },
            }
            button {
                class: "landing-paste-go",
                disabled: blank,
                title: "Open (⏎)",
                onclick: move |_| {
                    if recognise(&repo_input.peek()).is_some() {
                        open_typed(st);
                    } else {
                        searching.set(true);
                    }
                },
                "Open"
                span { class: "landing-kbd", "⏎" }
            }
        }
        div { class: "landing-seen",
            match (seen, blank) {
                (Some(s), _) => rsx! {
                    span { class: "landing-chip", "{s.kind()}" }
                    span { class: "landing-seen-what", "{s.what()}" }
                    span { class: "landing-seen-hint", "press ⏎ to open" }
                },
                (None, false) => rsx! {
                    span { class: "landing-seen-hint", "Not a link —" }
                    button {
                        class: "landing-switch",
                        onclick: move |_| searching.set(true),
                        "search GitHub for “{typed.trim()}” ⏎"
                    }
                },
                (None, true) => rsx! {
                    span { class: "landing-seen-hint", "Or try one:" }
                    for (label , url) in EXAMPLES {
                        button {
                            key: "{url}",
                            class: "landing-example",
                            title: "{url}",
                            onclick: move |_| {
                                repo_input.set(url.to_string());
                                open_typed(st);
                            },
                            "{label}"
                        }
                    }
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> RepoRef {
        RepoRef {
            owner: "bigmah".to_string(),
            name: "pullspace".to_string(),
        }
    }

    #[test]
    fn a_pull_request_link_is_a_pull_request() {
        assert_eq!(
            recognise("https://github.com/bigmah/pullspace/pull/5"),
            Some(Seen::Pr(repo(), 5))
        );
        assert_eq!(
            recognise("bigmah/pullspace/pull/5"),
            Some(Seen::Pr(repo(), 5))
        );
    }

    #[test]
    fn the_other_kinds_are_told_apart() {
        assert!(matches!(
            recognise("https://github.com/bigmah/pullspace/commit/f956aa9"),
            Some(Seen::Commit(..))
        ));
        assert!(matches!(
            recognise("https://github.com/bigmah/pullspace/compare/main...feat/x"),
            Some(Seen::Compare(..))
        ));
        assert!(matches!(
            recognise("https://github.com/bigmah/pullspace/blob/main/src/main.rs"),
            Some(Seen::Path(..))
        ));
        assert_eq!(
            recognise("https://github.com/bigmah/pullspace"),
            Some(Seen::Repo(repo()))
        );
    }

    #[test]
    fn a_word_is_not_a_link() {
        assert_eq!(recognise("dioxus"), None);
        assert_eq!(recognise(""), None);
    }

    #[test]
    fn only_a_whole_url_landing_in_an_empty_box_opens_by_itself() {
        let url = "https://github.com/bigmah/pullspace/pull/5";
        assert!(pasted_link("", url));
        assert!(!pasted_link("h", url));
        assert!(!pasted_link("", "bigmah/pullspace"));
        assert!(!pasted_link("", "https://example.com"));
    }

    #[test]
    fn every_example_is_recognised() {
        for (_, url) in EXAMPLES {
            assert!(recognise(url).is_some(), "{url}");
        }
    }
}
