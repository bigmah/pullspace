use std::time::Duration;

use dioxus::prelude::*;

use crate::backend::auth::open_browser;

use super::app::{Account, PrList, St, Workspace};
use super::compat;
use super::github::{browse_repo, open_pr};
use super::ide;

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

    // One read of the workspace, and only the crumbs leave it. The whole
    // workspace — two tree snapshots and every changed file — is not something
    // to clone here: this bar re-renders on every clone progress tick.
    let workspace = st.workspace.read();
    // What is on screen, said in the same place whatever it is.
    let (ws_label, ws_cls, ws_why) = match &*workspace {
        Workspace::Empty => (
            "nothing open",
            "wschip local",
            "View a repository or a pull request from the GitHub panel",
        ),
        Workspace::Pr(_) => (
            "pull request",
            "wschip pr",
            "A pull request fetched from GitHub — the files as it changes them",
        ),
        Workspace::Repo(_) => (
            "browsing",
            "wschip repo",
            "A GitHub repository being read — no pull request, so nothing is marked as changed",
        ),
    };

    let pr = workspace.pr().map(|p| {
        (
            format!("{} #{}", p.repo, p.number),
            p.title.clone(),
            p.html_url.clone(),
        )
    });
    let browsing = workspace
        .repo()
        .map(|v| (v.repo.to_string(), v.branch.clone(), v.html_url()));
    let pr_target = workspace.pr().map(|p| (p.repo.clone(), p.number));
    let repo_target = workspace.repo().map(|v| v.repo.clone());
    let ws_open = workspace.is_open();

    let (reload_note, reload_error) = match &*st.prs.read() {
        PrList::Loading(note) => (Some(note.clone()), None),
        PrList::Failed(e) => (None, Some(e.clone())),
        _ => (None, None),
    };
    let reloading = reload_note.is_some();
    let refresh_title = match &*workspace {
        Workspace::Pr(_) => "Reload this pull request from GitHub",
        Workspace::Repo(_) => "Reload this repository from GitHub",
        Workspace::Empty => "Nothing open to reload",
    };
    let refresh_cls = if reloading {
        "iconbtn lg spin"
    } else {
        "iconbtn lg"
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
        Workspace::Empty => None,
    };

    let account_label = match &*st.account.read() {
        Account::Checking => "checking…".to_string(),
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
            span { class: "brand", "pullspace" }
            span { class: "{ws_cls}", title: "{ws_why}", "{ws_label}" }
            if let Some((prnum, title, url)) = pr {
                span {
                    class: "prcrumb",
                    title: "{prnum} — {title}",
                    span { class: "prnum", "{prnum}" }
                    span { class: "prcrumbtitle", "{title}" }
                }
                if let Some((label, why)) = warn {
                    span { class: "prwarn", title: "{why}", "{label}" }
                }
                if let Some((note, why)) = status.clone() {
                    span { class: "prwarm", title: "{why}", "{note}" }
                }
                if let Some(e) = reload_error.clone() {
                    span { class: "prwarn", title: "{e}", "reload failed" }
                }
                button {
                    class: "iconbtn",
                    title: "Open on github.com",
                    onclick: move |_| open_browser(&url),
                    "↗"
                }
                button {
                    class: "closebtn",
                    title: "Close this pull request",
                    onclick: move |_| st.close_workspace(),
                    span { class: "closex", "✕" }
                    "close PR"
                }
            } else if let Some((repo, branch, url)) = browsing {
                span {
                    class: "prcrumb",
                    title: "{repo} at {branch} — no pull request, just the code",
                    span { class: "prnum", "{repo}" }
                    span { class: "prcrumbtitle", "@ {branch}" }
                }
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
                    "↗"
                }
                button {
                    class: "closebtn",
                    title: "Close this repository",
                    onclick: move |_| st.close_workspace(),
                    span { class: "closex", "✕" }
                    "close repo"
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
            RefreshButton {
                pr_target,
                repo_target,
                reloading,
                refresh_title,
                refresh_cls,
            }
        }
    }
}

/// Search across every file of what is open, and the readout for the index
/// being built alongside it.
///
/// It takes the place of the bar's spacer rather than sitting next to one: the
/// box grows into whatever the crumbs on the left have not used, up to a width
/// past which a search field is just a long empty rectangle.
#[component]
fn SearchBox() -> Element {
    let st = use_context::<St>();
    let mut text = st.search_text;
    let mut opts = st.search_opts;

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
    let error = st.search_error.read().clone();
    let cls = if error.is_some() {
        "searchbox bad"
    } else {
        "searchbox"
    };
    let why = error.unwrap_or_else(|| "Search every file of this repository  (⌘F)".to_string());
    let index_label = st.index.read().label();

    rsx! {
        div { class: "searchgrp",
            input {
                class: "{cls}",
                r#type: "text",
                placeholder: "Search in files…  (Enter)",
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

/// `⟳`. Reloading means fetching the pull request again: a push moves the head
/// commit, and the changed-file list, the tree and the cached contents all hang
/// off it.
#[component]
fn RefreshButton(
    pr_target: Option<(crate::backend::github::RepoRef, u64)>,
    repo_target: Option<crate::backend::github::RepoRef>,
    reloading: bool,
    refresh_title: &'static str,
    refresh_cls: &'static str,
) -> Element {
    let st = use_context::<St>();
    // Nothing open is nothing to reload, but a dead button that does not say so
    // is a puzzle — hold the spin briefly so the click is acknowledged.
    let mut nudged = use_signal(|| false);
    let spinning = reloading || *nudged.read();
    let cls = if spinning {
        "iconbtn lg spin"
    } else {
        refresh_cls
    };

    rsx! {
        button {
            class: cls,
            title: "{refresh_title}",
            disabled: reloading,
            onclick: move |_| match (pr_target.clone(), repo_target.clone()) {
                // Root scope: reloading replaces the workspace, and this button
                // is re-rendered underneath the task that did it.
                (Some((repo, number)), _) => {
                    spawn_forever(open_pr(st, repo, number));
                }
                (_, Some(repo)) => {
                    spawn_forever(browse_repo(st, repo));
                }
                _ => {
                    nudged.set(true);
                    spawn(async move {
                        compat::sleep(Duration::from_millis(550)).await;
                        nudged.set(false);
                    });
                }
            },
            // The glyph turns, not the button: a spinning hover square is not
            // what anyone means by "it is working".
            span { class: "glyph", "⟳" }
        }
    }
}
