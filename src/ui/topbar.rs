use dioxus::prelude::*;

use crate::backend::auth::open_browser;
use crate::backend::github::{CommitFrom, RepoRef};

use super::app::{Account, Fetch, St, Workspace};
use super::github::{
    PrListBody, PrStates, browse_branch, browse_repo, open_commit, open_compare, open_pr,
};
use super::ide;

/// What the bar says is open, and what it offers to do about it.
///
/// One shape for all three, because the row is the same row: what this is
/// called, what it is, a link to it on github.com, and the way out of it.
struct Crumb {
    /// The name — a pull request's number, a repository, a commit's sha.
    lead: String,
    /// And what it is called: the title, the branch, the subject line.
    trail: String,
    why: String,
    url: String,
    close: &'static str,
    close_why: &'static str,
}

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
        Workspace::Commit(_) => (
            "commit",
            "wschip cmp",
            "One commit, diffed against the commit before it",
        ),
        Workspace::Compare(_) => (
            "comparing",
            "wschip cmp",
            "Two refs held up against each other — what the one on the right adds to the one on the left",
        ),
    };

    // What is open, said the same way whichever of the three it is: a name, the
    // thing it is called, somewhere on github.com, and the way out of it.
    let crumb = match &*workspace {
        Workspace::Empty => None,
        Workspace::Pr(p) => Some(Crumb {
            lead: format!("{} #{}", p.repo, p.number),
            trail: p.title.clone(),
            why: format!(
                "{} #{} — {}\nSwitch to another pull request on this repository",
                p.repo, p.number, p.title
            ),
            url: p.html_url.clone(),
            close: "close PR",
            close_why: "Close this pull request",
        }),
        Workspace::Repo(v) => Some(Crumb {
            lead: v.repo.to_string(),
            trail: format!("@ {}", v.branch),
            why: format!(
                "{} at {} — no pull request, just the code\nOpen one of this repository's pull requests. Its branches are in the pane on the right.",
                v.repo, v.branch
            ),
            url: v.html_url(),
            close: "close repo",
            close_why: "Close this repository",
        }),
        // The sha is the name of a commit and the subject is what it is for.
        Workspace::Commit(v) => Some(Crumb {
            lead: v.commit.short().to_string(),
            trail: v.commit.subject().to_string(),
            why: match (v.pr(), v.branch()) {
                (Some(pr), _) => format!(
                    "{} — {}\nOne commit of #{}. Its other commits are in the pane on the right; the pull requests of {} are in here.",
                    v.commit.short(),
                    v.commit.subject(),
                    pr.number,
                    v.repo,
                ),
                (None, Some(branch)) => format!(
                    "{} — {}\nOne commit of {branch}. The rest of that branch is in the pane on the right.",
                    v.commit.short(),
                    v.commit.subject(),
                ),
                (None, None) if v.compare().is_some() => {
                    let (base, head) = v.compare().unwrap_or_default();
                    format!(
                        "{} — {}\nOne commit of {base}...{head}. The rest of that comparison is in the pane on the right.",
                        v.commit.short(),
                        v.commit.subject(),
                    )
                }
                (None, None) => format!(
                    "{} — {}\nOne commit of {}, diffed against the commit before it",
                    v.commit.short(),
                    v.commit.subject(),
                    v.repo,
                ),
            },
            url: v.html_url(),
            close: "close commit",
            close_why: "Close this commit",
        }),
        // The two names are what this is; how they stand is what it found.
        Workspace::Compare(v) => Some(Crumb {
            lead: format!("{}...{}", v.base, v.head),
            trail: v.summary(),
            why: format!(
                "{}...{} — {}\nWhat {} has that {} does not, diffed against where the two last agreed. The branches of {} are in the pane on the right.",
                v.base,
                v.head,
                v.summary(),
                v.head,
                v.base,
                v.repo,
            ),
            url: v.html_url(),
            close: "close compare",
            close_why: "Close this comparison",
        }),
    };
    let pr_target = workspace.pr().map(|p| (p.repo.clone(), p.number));
    // The branch as well as the repository: `⟳` on a branch means that branch
    // as it is now, not whatever the default one has moved to.
    let repo_target = workspace
        .repo()
        .map(|v| (v.repo.clone(), v.branch.clone(), v.default));
    let commit_target = workspace
        .commit()
        .map(|v| (v.repo.clone(), v.commit.sha.clone(), v.from.clone()));
    // Both ways round: which of two branches is the base is a thing to get
    // wrong, and the answer is one click rather than a trip back through the
    // branch list.
    let compare_target = workspace
        .compare()
        .map(|v| (v.repo.clone(), v.base.clone(), v.head.clone()));
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
            span { class: "brand", "pullspace" }
            span { class: "{ws_cls}", title: "{ws_why}", "{ws_label}" }
            if let Some(crumb) = crumb {
                PrSwitch {
                    lead: crumb.lead,
                    trail: crumb.trail,
                    why: crumb.why,
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
                if let Some((repo, base, head)) = compare_target {
                    button {
                        class: "iconbtn",
                        title: "Compare them the other way round — {head}...{base}",
                        // Root scope: the swap replaces the bar this button is in.
                        onclick: move |_| {
                            spawn_forever(
                                open_compare(st, repo.clone(), head.clone(), base.clone()),
                            );
                        },
                        span { class: "glyph", "⇄" }
                    }
                }
                button {
                    class: "iconbtn",
                    title: "Open on github.com",
                    onclick: move |_| open_browser(&crumb.url),
                    "↗"
                }
                button {
                    class: "closebtn",
                    title: "{crumb.close_why}",
                    onclick: move |_| st.close_workspace(),
                    span { class: "closex", "✕" }
                    "{crumb.close}"
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
            RefreshButton {
                pr_target,
                repo_target,
                commit_target,
                reloading,
                refresh_title,
            }
        }
    }
}

/// The crumb that says what is open — and, behind it, everything else that
/// could be.
///
/// A review is rarely one pull request. The list of them is already here, kept
/// alongside whatever is open (see the effect in [`App`](super::app::App)), so
/// swapping is a click on the thing you are already looking at rather than a
/// trip back through the picker. The repository itself is at the foot of the
/// list, which is the way out of a pull request and into the code around it.
#[component]
fn PrSwitch(lead: String, trail: String, why: String) -> Element {
    let st = use_context::<St>();
    let mut open = use_signal(|| false);

    let current = st.workspace.read().pr_number();
    let repo = st.workspace.read().repo_ref().cloned();
    // The repository row is the one being read only when the repository itself
    // is what is open — a commit of it is somewhere else.
    let browsing = st.workspace.read().repo().is_some();
    // Whose pull requests these are. Usually the repository that is open, but
    // the picker can have moved on to another one without opening it — and a
    // list that says whose it is cannot be read as the wrong repository's.
    let listed = st.prs.read().as_ref().map(|l| l.repo.to_string());

    // A swap that has landed is a menu that has done what it was opened for.
    // On the workspace rather than on the click, so the menu stays up — with
    // the row still under the pointer — for as long as the loading takes.
    use_effect(move || {
        let _ = st.workspace.read();
        open.set(false);
    });

    rsx! {
        div {
            class: "prswitch",
            // Escape, wherever the focus is inside here — which after the click
            // that opened the menu is the crumb, above the menu rather than in
            // it, so the handler goes on the pair of them.
            onkeydown: move |e| {
                if e.key() == Key::Escape && *open.peek() {
                    e.stop_propagation();
                    open.set(false);
                }
            },
            button {
                class: "prcrumb prcrumbbtn",
                title: "{why}",
                onclick: move |_| {
                    let showing = *open.peek();
                    open.set(!showing);
                },
                span { class: "prnum", "{lead}" }
                span { class: "prcrumbtitle", "{trail}" }
                span { class: "prchev", "▾" }
            }
            if *open.read() {
                // Everything else on the page, for as long as the menu is up:
                // a click anywhere out here puts it away, which is the one
                // thing every menu does.
                div {
                    class: "menuback",
                    onclick: move |_| open.set(false),
                }
                div { class: "prmenu",
                    div { class: "prmenuhdr",
                        if let Some(listed) = listed {
                            span { class: "ghlabel", "{listed}" }
                        }
                        span { class: "spacer" }
                        PrStates {}
                    }
                    div { class: "prmenubody", PrListBody { current } }
                    if let Some(repo) = repo {
                        BrowseFoot { repo, current: browsing }
                    }
                }
            }
        }
    }
}

/// The way out of a pull request and into the repository it is against — the
/// row at the foot of the switcher, and the one that is already ticked when the
/// repository is what is open.
#[component]
fn BrowseFoot(repo: RepoRef, current: bool) -> Element {
    let st = use_context::<St>();
    let class = if current {
        "prmenufoot on"
    } else {
        "prmenufoot"
    };
    rsx! {
        div {
            class: "{class}",
            title: if current { "Already open" } else { "Read {repo} at its default branch, with no pull request" },
            // Root scope: loading replaces the bar this row hangs off.
            onclick: move |_| {
                if !current {
                    spawn_forever(browse_repo(st, repo.clone()));
                }
            },
            span { class: "prmenufoot-label", "the repository itself" }
            if current {
                span { class: "prhere", "reading" }
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
    pr_target: Option<(RepoRef, u64)>,
    repo_target: Option<(RepoRef, String, bool)>,
    commit_target: Option<(RepoRef, String, CommitFrom)>,
    reloading: bool,
    refresh_title: &'static str,
) -> Element {
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
            disabled: reloading,
            onclick: move |_| match (
                pr_target.clone(),
                repo_target.clone(),
                commit_target.clone(),
            ) {
                // Root scope: reloading replaces the workspace, and this button
                // is re-rendered underneath the task that did it.
                (Some((repo, number)), ..) => {
                    spawn_forever(open_pr(st, repo, number));
                }
                // The default branch is looked up again rather than taken as
                // read: it is a setting on the repository, and reloading is
                // when a change to one shows up.
                (_, Some((repo, _, true)), _) => {
                    spawn_forever(browse_repo(st, repo));
                }
                (_, Some((repo, branch, false)), _) => {
                    spawn_forever(browse_branch(st, repo, branch, None));
                }
                (.., Some((repo, sha, from))) => {
                    spawn_forever(open_commit(st, repo, sha, from));
                }
                // The bar only exists while something is open, so one of the
                // three targets above is always there to take the click.
                _ => {}
            },
            // The glyph turns, not the button: a spinning hover square is not
            // what anyone means by "it is working".
            span { class: "glyph", "⟳" }
        }
    }
}
