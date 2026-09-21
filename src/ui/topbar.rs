use dioxus::prelude::*;

use crate::backend::auth::open_browser;
use crate::backend::github::{PR_PAGE, PrState};

use super::app::{Account, Fetch, St, Workspace};
use super::full;
use super::github::GithubMark;
use super::ide;
use super::refbar::{Go, Refs};
use super::spaces::{Kind, SpaceSwitch};

/// Bytes, in the units anybody would say them in.
pub fn size_label(bytes: u64) -> String {
    match bytes {
        n if n < 1024 => format!("{n} B"),
        n if n < 1 << 20 => format!("{:.0} KB", n as f64 / 1024.0),
        n if n < 1 << 30 => format!("{:.1} MB", n as f64 / (1 << 20) as f64),
        n => format!("{:.1} GB", n as f64 / (1 << 30) as f64),
    }
}

#[component]
pub fn TopBar() -> Element {
    let st = use_context::<St>();
    let mut gh_open = st.gh_open;
    let mut prefs_open = st.prefs_open;
    let full_on = *st.full.read();

    // One read of the workspace, and only the crumbs leave it. The whole
    // workspace — two tree snapshots and every changed file — is not something
    // to clone here: this bar re-renders on every clone progress tick.
    let workspace = st.workspace.read();
    // What is on screen, said in the same place whatever it is. The word and
    // the colour come from `spaces::Kind`, so the chip on this bar and the one
    // on a row of the space switcher say the same thing about the same thing.
    let kind = Kind::of(&workspace);
    let (ws_label, ws_cls) = (kind.word(), kind.css());
    let ws_why = match &*workspace {
        Workspace::Empty => "View a repository or a pull request from the GitHub panel",
        Workspace::Pr(_) => "A pull request fetched from GitHub — the files as it changes them",
        Workspace::Repo(_) => {
            "A GitHub repository being read — no pull request, so nothing is marked as changed"
        }
        Workspace::Commit(_) => "One commit, diffed against the commit before it",
        Workspace::Compare(_) => {
            "Two refs held up against each other — what the one on the right adds to the one on the left"
        }
    };

    // Which repository, and where what is open is on github.com. The rest of
    // what is open — its branches, its title — is `Refs`, which reads the
    // workspace for itself.
    let open = workspace.repo_ref().map(|repo| {
        let url = match &*workspace {
            Workspace::Pr(p) => p.html_url.clone(),
            Workspace::Repo(v) => v.html_url(),
            Workspace::Commit(v) => v.html_url(),
            Workspace::Compare(v) => v.html_url(),
            Workspace::Empty => String::new(),
        };
        (repo.to_string(), url)
    });
    let reload = Go::reload(&workspace);
    let ws_open = workspace.is_open();

    let (reload_note, reload_error) = match &*st.fetch.read() {
        Fetch::Working(note) => (Some(note.clone()), None),
        Fetch::Failed(e) => (None, Some(e.clone())),
        Fetch::Idle => (None, None),
    };
    let reloading = reload_note.is_some();
    let refresh_title = match &*workspace {
        Workspace::Pr(_) => "Reload this pull request from GitHub",
        Workspace::Repo(_) => "Reload this repository from GitHub",
        Workspace::Commit(_) => "Reload this commit from GitHub",
        Workspace::Compare(_) => "Compare these two again — either end may have moved",
        Workspace::Empty => "Nothing open to reload",
    };
    // The progress line along the bottom of the bar is the one piece of a
    // reload visible with the GitHub panel closed — which is where `⟳` reloads
    // from.
    let bar_cls = if reloading { "topbar busy" } else { "topbar" };

    // How the clone is getting on, while it still is. Once everything is on
    // disk there is nothing to report and the note goes away.
    let cloning = (*st.cloning.read()).filter(|at| !at.finished()).map(|at| {
        let note = format!("cloning {}/{}", at.done, at.total);
        let why = format!(
            "Reading {} into this browser's filesystem, so the rest of this \
                 repository opens without waiting. {} {} already here.",
            size_label(at.expected),
            at.cached,
            if at.cached == 1 {
                "file was"
            } else {
                "files were"
            },
        );
        (note, why)
    });

    // One note at a time. A reload in flight is the more urgent of the two,
    // and two of these pulsing out of phase in the corner of the eye is one
    // more than anybody needs.
    let status = match (reload_note, cloning) {
        (Some(note), _) => Some((note, String::new())),
        (None, Some(note)) => Some(note),
        _ => None,
    };

    // Say so when the explorer is showing less than the whole repository,
    // rather than letting a missing file look like it does not exist.
    let partial_tree = (
        "partial tree",
        "This repository is past GitHub's tree limit, so some files are missing from the explorer",
    );
    let warn = match &*workspace {
        Workspace::Pr(pr) => {
            if pr.truncated {
                Some((
                    "truncated",
                    "This PR changes more files than GitHub will list; only the first 3000 were loaded",
                ))
            } else if pr.tree.truncated {
                Some(partial_tree)
            } else if pr.tree.is_empty() {
                Some((
                    "changed files only",
                    "The repository tree could not be read, so the explorer lists only the files this PR changes",
                ))
            } else {
                None
            }
        }
        Workspace::Repo(view) => view.tree.truncated.then_some(partial_tree),
        Workspace::Commit(view) => {
            if view.truncated {
                Some((
                    "truncated",
                    "This commit touches more files than GitHub will list; only the first 300 were loaded",
                ))
            } else if view.merge && view.files.is_empty() {
                Some((
                    "merge commit",
                    "GitHub lists no changed files for a merge commit — what it brought in is in the commits it merged",
                ))
            } else if view.tree.truncated {
                Some(partial_tree)
            } else if view.tree.is_empty() {
                Some((
                    "changed files only",
                    "The repository tree could not be read, so the explorer lists only the files this commit changes",
                ))
            } else {
                None
            }
        }
        // Nothing between them is not a failure to show anything: it is the
        // answer, and which of the two reasons it is is the thing worth saying.
        Workspace::Compare(view) => {
            if view.truncated {
                Some((
                    "truncated",
                    "This comparison covers more files than GitHub will list; only the first 300 were loaded",
                ))
            } else if view.is_empty() && view.behind > 0 {
                Some((
                    "nothing ahead",
                    "Everything on the right is already on the left — it is behind, not ahead",
                ))
            } else if view.is_empty() {
                Some(("identical", "These two are the same commit"))
            } else if view.tree.truncated {
                Some(partial_tree)
            } else if view.tree.is_empty() {
                Some((
                    "changed files only",
                    "The repository tree could not be read, so the explorer lists only the files that differ",
                ))
            } else {
                None
            }
        }
        Workspace::Empty => None,
    };

    let account_label = match &*st.account.read() {
        Account::Checking | Account::Verifying => "checking…".to_string(),
        Account::SignedOut => "Sign in".to_string(),
        Account::Failed(_) => "sign-in failed".to_string(),
        Account::SignedIn { login } => login.clone(),
    };
    let account_cls = match &*st.account.read() {
        Account::SignedIn { .. } => "ghchip on",
        Account::Failed(_) => "ghchip bad",
        _ => "ghchip",
    };

    // Anonymous browsing works, and works well for public repositories — but
    // the sixty-an-hour limit is the thing people hit without knowing why, so
    // it is named here rather than in an error later.
    let anon = matches!(&*st.account.read(), Account::SignedOut) && ws_open;
    drop(workspace);

    rsx! {
        div { class: "{bar_cls}",
            SpaceSwitch {}
            span { class: "{ws_cls}", title: "{ws_why}", "{ws_label}" }
            if let Some((repo, url)) = open {
                span { class: "refrepo", title: "{repo}", "{repo}" }
                PrSwitch {}
                Refs {}
                if let Some((label, why)) = warn {
                    span { class: "prwarn", title: "{why}", "{label}" }
                }
                if let Some((note, why)) = status {
                    span { class: "prwarm", title: "{why}", "{note}" }
                }
                if let Some(e) = reload_error {
                    span { class: "prwarn", title: "{e}", "reload failed" }
                }
                button {
                    class: "iconbtn",
                    title: "Open on github.com",
                    onclick: move |_| open_browser(&url),
                    GithubMark {}
                }
            }
            SearchBox {}
            if anon {
                span {
                    class: "prsrc",
                    title: "Anonymous requests are limited to 60 an hour. File contents are read from raw.githubusercontent.com and do not count against it; listing pull requests and trees does.",
                    "anonymous"
                }
            }
            button {
                class: account_cls,
                title: "GitHub pull requests",
                onclick: move |_| gh_open.set(true),
                "{account_label}"
            }
            button {
                class: "iconbtn lg",
                title: "Appearance — theme, accent, font  (⌘,)",
                onclick: move |_| prefs_open.set(true),
                span { class: "glyph", "◐" }
            }
            // Lit from what the browser says rather than from having been
            // pressed: the screen can be handed back on a key this page never
            // sees, and a request for it can be refused. See `super::full`.
            button {
                class: if full_on { "iconbtn lg on" } else { "iconbtn lg" },
                title: if full_on {
                    "Leave fullscreen — the browser's tabs and address bar come back  (Esc)"
                } else {
                    "Fullscreen — the whole app, with the browser's tabs and address bar out of the way  (F11)"
                },
                onclick: move |_| full::toggle(),
                span { class: "glyph", "\u{26f6}" }
            }
            RefreshButton { reload, reloading, refresh_title }
        }
    }
}

/// The way to the pull requests of the repository that is open.
///
/// A review is rarely one pull request. The list of them is already here, kept
/// alongside whatever is open (see the effect in [`App`](super::app::App)), so
/// swapping is one click from anywhere rather than a trip back through the
/// picker. Always the same word in the same place: what is open is said by the
/// chip before it and the branches after it, and a button whose label changed
/// with every pull request is a button that has to be found again every time.
///
/// What it opens is [`PrBoard`](super::prboard::PrBoard) — the page under this
/// bar, given over to the list. It used to be a menu hanging off this button,
/// and a menu is as wide as a menu: a title, a name and a date, which is enough
/// to find a pull request you already know and not enough to choose between the
/// ones you do not.
#[component]
fn PrSwitch() -> Element {
    let st = use_context::<St>();
    let mut board = st.pr_board;
    let showing = *board.read();

    // How many are open, once that is known — which is what says whether the
    // button is worth pressing. Only ever that: the list behind it can be
    // toggled to the closed ones, or moved on to another repository by the
    // picker, and a number on this bar about either would be read as this one.
    let count = {
        let held = st.prs.read();
        let ws = st.workspace.read();
        held.as_ref()
            .filter(|l| l.state == PrState::Open && ws.repo_ref() == Some(&l.repo))
            .map(|l| l.items().len())
            .filter(|n| *n > 0)
            .map(|n| match n {
                n if n >= PR_PAGE => format!("{PR_PAGE}+"),
                n => n.to_string(),
            })
    };
    let why = match st.workspace.read().repo_ref() {
        Some(repo) if showing => {
            format!("Back to what is open  (Esc) — the pull requests of {repo}")
        }
        Some(repo) => format!("The pull requests of {repo}"),
        None => "Pull requests".to_string(),
    };

    rsx! {
        button {
            class: if showing { "barbtn on" } else { "barbtn" },
            title: "{why}",
            onclick: move |_| {
                let showing = *board.peek();
                board.set(!showing);
            },
            "Pull requests"
            if let Some(count) = count {
                span { class: "barcount", title: "Open pull requests", "{count}" }
            }
        }
    }
}

/// Search across every file of what is open, and the readout for the index
/// being built alongside it.
///
/// Two questions rather than one: what is written in the files, and what the
/// files are called. `name` beside the box is which of the two — because they
/// are the same pattern, the same three toggles and the same panel underneath,
/// and a second box for the second question would be a second box to find.
///
/// It takes the place of the bar's spacer rather than sitting next to one: the
/// box grows into whatever the crumbs on the left have not used, up to a width
/// past which a search field is just a long empty rectangle.
#[component]
fn SearchBox() -> Element {
    let st = use_context::<St>();
    let mut text = st.search_text;
    let mut opts = st.search_opts;
    let mut by_name = st.search_files;

    // Search walks the files of the commit on show. When there is no commit on
    // show, or GitHub would not say what is in it, the box carries the reason
    // rather than being a control that does nothing.
    if let Some(reason) = ide::why_not(&st) {
        // No spacer beside it: `.searchbox` carries `margin-left: auto` of its
        // own, and two flexible things in a row only fight over the gap.
        return rsx! {
            input {
                class: "searchbox",
                r#type: "text",
                disabled: true,
                placeholder: "{reason}",
                title: "{reason}",
                value: "",
            }
        };
    }

    let now = *st.search_opts.read();
    let names = *by_name.read();
    let error = st.search_error.read().clone();
    let cls = if error.is_some() {
        "searchbox bad"
    } else {
        "searchbox"
    };
    let why = error.unwrap_or_else(|| {
        if names {
            "Find a file of this repository by its name  (⌘⇧F)".to_string()
        } else {
            "Search every file of this repository  (⌘⇧F)".to_string()
        }
    });
    let index_label = st.index.read().label();

    rsx! {
        div { class: "searchgrp",
            input {
                class: "{cls}",
                r#type: "text",
                placeholder: if names { "Find a file by name…  (Enter)" } else { "Search in files…  (Enter)" },
                title: "{why}",
                spellcheck: "false",
                autocomplete: "off",
                value: "{text}",
                oninput: move |e| {
                    text.set(e.value());
                    // The complaint was about the pattern as it was; it is not
                    // about this one until this one has been tried.
                    let mut err = st.search_error;
                    if err.peek().is_some() {
                        err.set(None);
                    }
                },
                onkeydown: move |e| {
                    if e.key() == Key::Enter {
                        ide::search(st);
                    }
                },
            }
            button {
                class: if names { "sopt wide on" } else { "sopt wide" },
                title: if names {
                    "Looking for files by name. Press again to search inside them instead"
                } else {
                    "Look for files by name rather than for text inside them — the pattern is matched against the whole path"
                },
                // Read out of the signal rather than off the render that drew
                // this, the way the three beside it do: a second press before
                // the redraw would otherwise set what was already set.
                onclick: move |_| {
                    let now = *by_name.peek();
                    by_name.set(!now);
                    // The two questions have different answers, and the one on
                    // screen is the answer to the question that is no longer
                    // being asked. Ask the new one rather than leave it there.
                    if !st.search_text.peek().trim().is_empty() {
                        ide::search(st);
                    }
                },
                "name"
            }
            for (label, why, field) in ide::TOGGLES {
                {
                    // `field` picks one of the three out of a copy, which is
                    // what lets the three buttons be one loop rather than
                    // three near-identical blocks.
                    let mut probe = now;
                    let cls = if *field(&mut probe) { "sopt on" } else { "sopt" };
                    rsx! {
                        button {
                            key: "{label}",
                            class: "{cls}",
                            title: "{why}",
                            onclick: move |_| {
                                let mut next = *opts.peek();
                                let slot = field(&mut next);
                                *slot = !*slot;
                                opts.set(next);
                                // Re-run rather than wait to be asked again:
                                // the toggles are only ever pressed to see
                                // what they do to the results.
                                if !st.search_text.peek().trim().is_empty() {
                                    ide::search(st);
                                }
                            },
                            "{label}"
                        }
                    }
                }
            }
            if let Some(label) = index_label {
                span {
                    class: "idxstate",
                    title: "Definitions read out of this repository, for Go to Definition. Built once, in the background, after the download finishes.",
                    "{label}"
                }
            }
        }
    }
}

/// `⟳`. Reloading means fetching what is open again: a push moves the head
/// commit, and the changed-file list, the tree and the cached contents all hang
/// off it.
#[component]
fn RefreshButton(reload: Option<Go>, reloading: bool, refresh_title: &'static str) -> Element {
    let st = use_context::<St>();
    let cls = if reloading {
        "iconbtn lg spin"
    } else {
        "iconbtn lg"
    };

    rsx! {
        button {
            class: cls,
            title: "{refresh_title}",
            disabled: reloading || reload.is_none(),
            onclick: move |_| {
                if let Some(go) = reload.clone() {
                    go.run(st);
                }
            },
            // The glyph turns, not the button: a spinning hover square is not
            // what anyone means by "it is working".
            span { class: "glyph", "⟳" }
        }
    }
}
