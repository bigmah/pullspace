use dioxus::prelude::*;

use crate::backend::auth::open_browser;

use super::app::{Account, PrList, PrSource, St, Workspace};
use super::github::{browse_repo, open_pr};
use super::prcache::warmed;

#[component]
pub fn TopBar() -> Element {
    let st = use_context::<St>();
    let mut search_text = st.search_text;
    let mut gh_open = st.gh_open;
    // Read (not peek) so the path re-renders after opening another repo.
    let root_str = st.root.read().to_string_lossy().into_owned();
    let index_label = match st.index.read().as_ref() {
        None => "indexing symbols…".to_string(),
        Some(idx) => format!("{} symbols", idx.len()),
    };

    let workspace = st.workspace.read().clone();
    let pr = workspace.pr().cloned();
    let browsing = workspace.repo().cloned();
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
        Workspace::Local => "Reload git status & file tree",
    };

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
        div { class: "topbar",
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
                "📂"
            }
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
                if let Some((done, total)) = warming {
                    span {
                        class: "prwarm",
                        title: "Caching the PR's changed files in the background",
                        "warming {done}/{total}"
                    }
                }
                if let Some(note) = reload_note.clone() {
                    span { class: "prwarm", "{note}" }
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
                    class: "linkbtn",
                    title: "Back to the local working tree",
                    onclick: move |_| st.leave_remote(),
                    "✕ close PR"
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
                if let Some(note) = reload_note.clone() {
                    span { class: "prwarm", "{note}" }
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
                    class: "linkbtn",
                    title: "Back to the local working tree",
                    onclick: move |_| st.leave_remote(),
                    "✕ close repo"
                }
            } else {
                span { class: "repopath", title: "{root_str}", "{root_str}" }
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
                class: "iconbtn",
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
                    _ => st.refresh(),
                },
                "⟳"
            }
        }
    }
}
