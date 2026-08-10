use dioxus::prelude::*;

use crate::backend::auth::open_browser;

use super::app::{Account, St};

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
    let in_pr = pr.is_some();

    // Search and the symbol index walk the working tree, which a PR is not
    // part of — say so rather than returning confusing local results.
    let (search_placeholder, search_disabled) = if in_pr {
        ("Search is unavailable while viewing a pull request", true)
    } else {
        ("Search in files…  (Enter)", false)
    };

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
                if pr.truncated {
                    span { class: "prwarn", title: "Only the first 3000 files were loaded", "truncated" }
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
                    onclick: move |_| st.leave_pr(),
                    "✕ close PR"
                }
            } else {
                span { class: "repopath", title: "{root_str}", "{root_str}" }
            }
            input {
                class: "searchbox",
                r#type: "text",
                placeholder: "{search_placeholder}",
                spellcheck: "false",
                disabled: search_disabled,
                value: "{search_text}",
                oninput: move |e| search_text.set(e.value()),
                onkeydown: move |e| {
                    if e.key() == Key::Enter {
                        st.do_search();
                    }
                },
            }
            if !in_pr {
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
                title: "Reload git status & file tree",
                disabled: in_pr,
                onclick: move |_| st.refresh(),
                "⟳"
            }
        }
    }
}
