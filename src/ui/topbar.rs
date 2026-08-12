use std::time::Duration;

use dioxus::prelude::*;

use crate::backend::auth::open_browser;
use crate::backend::branches::head_of;

use super::app::{Account, PrList, PrSource, St, Workspace};
use super::branches::BranchChip;
use super::github::{browse_repo, open_pr};
use super::prcache::warmed;

/// How far apart two branches are, in words rather than in a count with an
/// `(s)` stuck on the end of it.
fn commits(n: usize) -> String {
    format!("{n} commit{}", if n == 1 { "" } else { "s" })
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// "it" or "them", for a count that reads as one thing or several.
fn these(n: usize) -> &'static str {
    if n == 1 { "it" } else { "them" }
}

#[component]
pub fn TopBar() -> Element {
    let st = use_context::<St>();
    let mut search_text = st.search_text;
    let mut gh_open = st.gh_open;
    // Read (not peek) so the path re-renders after opening another repo.
    let root_str = st.root.read().to_string_lossy().into_owned();
    // Short, because it shares a fixed width with the count that replaces it —
    // a readout that resizes shoves the four controls to its right.
    let index_label = match st.index.read().as_ref() {
        None => "indexing…".to_string(),
        Some(idx) => format!("{} symbols", idx.len()),
    };

    let workspace = st.workspace.read().clone();
    let local = matches!(workspace, Workspace::Local);
    // What is on screen, said in the same place whatever it is. Without it the
    // working tree is only ever implied — by a path standing where a pull
    // request's title would be, which is not something anybody reads as "these
    // are your own uncommitted changes".
    // pullspace opens plain directories too, and one that is not a repository
    // has no HEAD to be clean against and nothing staged either.
    let in_repo = use_memo(move || {
        st.refresh_tick.read();
        st.root.read();
        head_of(&st.root_path()).is_some()
    });
    let (ws_label, ws_cls, ws_why) = match &workspace {
        Workspace::Local if !in_repo() => (
            "files",
            "wschip local",
            "A directory on disk that is not a git repository — so nothing here is marked as changed",
        ),
        Workspace::Local => (
            "local",
            "wschip local",
            "Your own working tree — uncommitted changes on disk. Nothing here came from GitHub.",
        ),
        Workspace::Pr(_) => (
            "pull request",
            "wschip pr",
            "A pull request fetched from GitHub — the files as it changes them, not your working tree",
        ),
        Workspace::Repo(_) => (
            "browsing",
            "wschip repo",
            "A GitHub repository being read — no pull request, so nothing is marked as changed",
        ),
        Workspace::Compare(_) => (
            "compare",
            "wschip cmp",
            "Two branches on disk held against each other — not your working tree, and not a pull request",
        ),
    };

    // The working tree's two halves, counted where the question "am I looking
    // at what I have added, or at everything?" is asked.
    let (staged, unstaged) = if local {
        let stages = st.stages.read();
        (
            stages.values().filter(|s| s.is_staged()).count(),
            stages.values().filter(|s| s.is_unstaged()).count(),
        )
    } else {
        (0, 0)
    };
    let clean = local && in_repo() && staged == 0 && unstaged == 0;

    let pr = workspace.pr().cloned();
    let browsing = workspace.repo().cloned();
    let comparing = workspace.compare().cloned();
    let pr_target = workspace.pr().map(|p| (p.repo.clone(), p.number));
    let repo_target = workspace.repo().map(|v| v.repo.clone());

    // Reloading from GitHub can fetch objects, so it has to say so; reloading
    // the local tree is synchronous and needs no notice.
    let (reload_note, reload_error) = match &*st.prs.read() {
        PrList::Loading(note) => (Some(note.clone()), None),
        PrList::Failed(e) => (None, Some(e.clone())),
        _ => (None, None),
    };
    let reloading = reload_note.is_some();
    let refresh_title = match &workspace {
        Workspace::Pr(_) => "Reload this pull request from GitHub",
        Workspace::Repo(_) => "Reload this repository from GitHub",
        Workspace::Compare(_) => "Compare these two branches again",
        Workspace::Local => "Reload git status & file tree",
    };
    // Reloading the local working tree is synchronous, so it is over before
    // the button could report that it had started. This holds the spin open
    // for long enough to be read as "it did something" rather than as a blink.
    let mut nudged = use_signal(|| false);
    let spinning = reloading || *nudged.read();
    let refresh_cls = if spinning {
        "iconbtn lg spin"
    } else {
        "iconbtn lg"
    };
    // Opening a pull request can mean a clone. The progress line along the
    // bottom of the bar is the one piece of that visible with the GitHub panel
    // closed — which is where `⟳` reloads from.
    let bar_cls = if reloading { "topbar busy" } else { "topbar" };

    // Where file contents come from — worth showing, since it is the
    // difference between instant and a round trip per file.
    let source = workspace.is_remote().then(|| match &*st.pr_source.read() {
        PrSource::Local { borrowed: true, .. } => (
            "local",
            "Reading from the clone you already had — no network per file",
        ),
        PrSource::Local { borrowed: false, .. } => (
            "mirrored",
            "Reading from pullspace's cached mirror — no network per file",
        ),
        PrSource::Api => (
            "api",
            "Reading over the GitHub API — one request per file",
        ),
    });

    // Background warm-up progress, shown only while it is still running.
    let warming = workspace.pr().and_then(|pr| {
        let done = warmed(&st, pr);
        (done < pr.files.len()).then_some((done, pr.files.len()))
    });

    // One note at a time. A reload in flight is the more urgent of the two,
    // and two of these pulsing out of phase in the corner of the eye is one
    // more than anybody needs.
    let status = match (reload_note, warming) {
        (Some(note), _) => Some((note, String::new())),
        (None, Some((done, total))) => Some((
            format!("warming {done}/{total}"),
            "Caching the PR's changed files in the background".to_string(),
        )),
        _ => None,
    };

    // Say so when the explorer is showing less than the whole repository,
    // rather than letting a missing file look like it does not exist.
    let partial_tree = (
        "partial tree",
        "This repository is past GitHub's tree limit, so some files are missing from the explorer",
    );
    let warn = match &workspace {
        Workspace::Pr(pr) => {
            if pr.truncated {
                Some((
                    "truncated",
                    "This PR changes more files than GitHub will list; only the first 3000 were loaded",
                ))
            } else if pr.tree_truncated {
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
        Workspace::Repo(view) => view.tree_truncated.then_some(partial_tree),
        Workspace::Compare(view) => view.unrelated.then_some((
            "unrelated",
            "These two branches share no history, so there is no merge base — this is a diff of their tips",
        )),
        Workspace::Local => None,
    };

    // Search and the symbol index walk a directory. Locally that is the
    // repository; for a pull request it is the checkout made when it opened.
    // When there isn't one, the state carries the reason — show it rather than
    // leaving a dead search box to be puzzled over.
    let scan_why = st.scan_root.read().why().map(str::to_string);
    let scannable = scan_why.is_none();
    let search_placeholder = scan_why
        .clone()
        .unwrap_or_else(|| "Search in files…  (Enter)".to_string());
    // The placeholder only shows while the box is empty; the tooltip always does.
    let search_title = scan_why.unwrap_or_default();

    let account_label = match &*st.account.read() {
        Account::Checking => "checking…".to_string(),
        Account::SignedOut => "Sign in".to_string(),
        Account::Connecting { .. } => "signing in…".to_string(),
        Account::Failed(_) => "sign-in failed".to_string(),
        Account::SignedIn { login, .. } => login.clone(),
    };
    let account_cls = match &*st.account.read() {
        Account::SignedIn { .. } => "ghchip on",
        Account::Failed(_) => "ghchip bad",
        _ => "ghchip",
    };

    rsx! {
        div { class: "{bar_cls}",
            span { class: "brand", "pullspace" }
            button {
                class: "iconbtn",
                title: "Open another repository…",
                onclick: move |_| async move {
                    let start = st.root_path();
                    let picked = rfd::AsyncFileDialog::new()
                        .set_title("Open repository")
                        .set_directory(&start)
                        .pick_folder()
                        .await;
                    if let Some(dir) = picked {
                        st.open_repo(dir.path().to_path_buf());
                    }
                },
                // Drawn rather than typed. Every other icon in the chrome is a
                // one-weight outline glyph; 📂 is full colour and fills its
                // whole em box, which is what made the ones beside it look
                // like specks.
                svg {
                    width: "16",
                    height: "16",
                    view_box: "0 0 16 16",
                    fill: "none",
                    stroke: "currentColor",
                    stroke_width: "1.3",
                    stroke_linejoin: "round",
                    path {
                        d: "M2 4.5A1.5 1.5 0 0 1 3.5 3h2.4a1 1 0 0 1 .8.4L7.8 5h4.7A1.5 1.5 0 0 1 14 6.5v5A1.5 1.5 0 0 1 12.5 13h-9A1.5 1.5 0 0 1 2 11.5v-7Z",
                    }
                }
            }
            span { class: "{ws_cls}", title: "{ws_why}", "{ws_label}" }
            if let Some(pr) = pr {
                span {
                    class: "prcrumb",
                    title: "{pr.repo} #{pr.number} — {pr.title}",
                    span { class: "prnum", "{pr.repo} #{pr.number}" }
                    span { class: "prcrumbtitle", "{pr.title}" }
                }
                if let Some((label, why)) = source {
                    span { class: "prsrc", title: "{why}", "{label}" }
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
                    onclick: move |_| open_browser(&pr.html_url),
                    "↗"
                }
                button {
                    class: "closebtn",
                    title: "Back to the local working tree",
                    onclick: move |_| st.back_to_worktree(),
                    span { class: "closex", "✕" }
                    "close PR"
                }
            } else if let Some(view) = browsing {
                span {
                    class: "prcrumb",
                    title: "{view.repo} at {view.branch} — no pull request, just the code",
                    span { class: "prnum", "{view.repo}" }
                    span { class: "prcrumbtitle", "@ {view.branch}" }
                }
                if let Some((label, why)) = source {
                    span { class: "prsrc", title: "{why}", "{label}" }
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
                    onclick: {
                        let url = view.html_url();
                        move |_| open_browser(&url)
                    },
                    "↗"
                }
                button {
                    class: "closebtn",
                    title: "Back to the local working tree",
                    onclick: move |_| st.back_to_worktree(),
                    span { class: "closex", "✕" }
                    "close repo"
                }
            } else if let Some(view) = comparing {
                span {
                    class: "prcrumb",
                    title: "{view.head} is {commits(view.ahead)} ahead of {view.base} and {commits(view.behind)} behind — diffed against the merge base of the two",
                    span { class: "prnum", "{view.base} … {view.head}" }
                    span { class: "prcrumbtitle", "{view.files.len()} changed" }
                }
                if let Some((label, why)) = warn {
                    span { class: "prwarn", title: "{why}", "{label}" }
                }
                BranchChip {}
                button {
                    class: "closebtn",
                    title: "Back to the local working tree",
                    onclick: move |_| st.back_to_worktree(),
                    span { class: "closex", "✕" }
                    "close compare"
                }
            } else {
                span { class: "repopath", title: "{root_str}", "{root_str}" }
                BranchChip {}
                // Both halves of `git add`, so that which one the viewer is
                // showing is a choice made in front of these numbers rather
                // than behind them.
                if staged > 0 {
                    span {
                        class: "stgcount stg",
                        title: "{staged} file{plural(staged)} added — a commit now would record {these(staged)}",
                        "{staged} staged"
                    }
                }
                if unstaged > 0 {
                    span {
                        class: "stgcount unstg",
                        title: "{unstaged} file{plural(unstaged)} changed on disk and not added — a commit now would leave {these(unstaged)} behind",
                        "{unstaged} unstaged"
                    }
                }
                if clean {
                    span {
                        class: "stgcount clean",
                        title: "The working tree matches HEAD — nothing to commit",
                        "clean"
                    }
                }
            }
            input {
                class: "searchbox",
                r#type: "text",
                placeholder: "{search_placeholder}",
                title: "{search_title}",
                spellcheck: "false",
                disabled: !scannable,
                value: "{search_text}",
                oninput: move |e| search_text.set(e.value()),
                onkeydown: move |e| {
                    if e.key() == Key::Enter {
                        st.do_search();
                    }
                },
            }
            if scannable {
                span { class: "idxstate", "{index_label}" }
            }
            button {
                class: account_cls,
                title: "GitHub pull requests",
                onclick: move |_| gh_open.set(true),
                "{account_label}"
            }
            button {
                class: refresh_cls,
                title: "{refresh_title}",
                disabled: reloading,
                onclick: move |_| match (pr_target.clone(), repo_target.clone()) {
                    // Root scope: reloading replaces the workspace, and this
                    // button is re-rendered underneath the task that did it.
                    (Some((repo, number)), _) => {
                        spawn_forever(open_pr(st, repo, number));
                    }
                    (_, Some(repo)) => {
                        spawn_forever(browse_repo(st, repo));
                    }
                    _ => {
                        st.refresh();
                        nudged.set(true);
                        spawn(async move {
                            tokio::time::sleep(Duration::from_millis(550)).await;
                            nudged.set(false);
                        });
                    }
                },
                // The glyph turns, not the button: a spinning hover square is
                // not what anyone means by "it is working".
                span { class: "glyph", "⟳" }
            }
        }
    }
}
