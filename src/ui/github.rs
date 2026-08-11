//! The GitHub overlay: sign in, pick a repository, pick a pull request.

use std::time::Duration;

use dioxus::prelude::*;

use crate::backend::auth::{
    open_browser, poll_device_flow, save_client_id, save_token, sign_out, start_device_flow,
    Token, TokenSource,
};
use crate::backend::github::{
    list_prs, load_pr, parse_target, repo_tree, viewer_login, PrSummary, RepoRef,
};
use crate::backend::mirror;

use super::app::{Account, PrList, PrSource, ScanRoot, St};

/// Docs for creating the OAuth app this needs.
const NEW_APP_URL: &str = "https://github.com/settings/applications/new";

#[component]
pub fn GhPanel() -> Element {
    let st = use_context::<St>();
    let mut gh_open = st.gh_open;
    let account = st.account.read().clone();

    rsx! {
        div {
            class: "ghoverlay",
            // Click-away closes, but only on the backdrop itself.
            onclick: move |_| gh_open.set(false),
            div {
                class: "ghpanel",
                onclick: move |e| e.stop_propagation(),
                div { class: "ghhdr",
                    span { class: "ghtitle", "GitHub" }
                    span { class: "spacer" }
                    button { class: "iconbtn", onclick: move |_| gh_open.set(false), "✕" }
                }
                div { class: "ghbody",
                    match account {
                        Account::Checking => rsx! {
                            div { class: "ghnote", "Looking for a saved sign-in…" }
                        },
                        Account::SignedOut => rsx! { SignIn { error: None } },
                        Account::Failed(e) => rsx! { SignIn { error: Some(e) } },
                        Account::Connecting { user_code, verification_uri, note } => rsx! {
                            DevicePrompt { user_code, verification_uri, note }
                        },
                        Account::SignedIn { login, source } => rsx! {
                            SignedIn { login, source }
                        },
                    }
                    CacheFooter {}
                }
            }
        }
    }
}

/// Mirrors and checkouts are the one thing pullspace keeps on disk that the
/// user might want back, so show the size and offer to drop it.
#[component]
fn CacheFooter() -> Element {
    let mut cleared = use_signal(|| false);
    let stats = use_memo(move || {
        cleared.read();
        mirror::cache_stats()
    })();

    if stats.is_empty() {
        return rsx! {};
    }
    let mut parts = Vec::new();
    if stats.repos > 0 {
        parts.push(format!(
            "Mirrored {} {}",
            stats.repos,
            if stats.repos == 1 { "repository" } else { "repositories" },
        ));
    }
    if stats.checkouts > 0 {
        parts.push(format!(
            "{} {}",
            stats.checkouts,
            if stats.checkouts == 1 { "checkout" } else { "checkouts" },
        ));
    }
    parts.push(mirror::human_bytes(stats.bytes));
    let label = parts.join(" · ");
    rsx! {
        div { class: "ghsection ghcache",
            span { class: "ghnote", "{label}" }
            span { class: "spacer" }
            button {
                class: "linkbtn",
                onclick: move |_| {
                    let _ = mirror::clear_cache();
                    let v = *cleared.peek();
                    cleared.set(!v);
                },
                "Clear"
            }
        }
    }
}

// ------------------------------------------------------------------ sign in

#[component]
fn SignIn(error: Option<String>) -> Element {
    let st = use_context::<St>();
    let mut client_id_input = st.client_id_input;
    let id_value = client_id_input.read().clone();
    let ready = !id_value.trim().is_empty();

    rsx! {
        if let Some(e) = error {
            div { class: "gherror", "{e}" }
        }
        div { class: "ghsection",
            div { class: "ghlabel", "OAuth client ID" }
            input {
                class: "ghinput",
                r#type: "text",
                placeholder: "Ov23li…",
                spellcheck: "false",
                value: "{id_value}",
                oninput: move |e| client_id_input.set(e.value()),
            }
            div { class: "ghhelp",
                "pullspace signs in with the OAuth device flow, so it never needs a client secret. "
                "Register an OAuth app once, tick "
                b { "Enable Device Flow" }
                ", and paste its client ID here — it is saved to ~/.config/pullspace/config.json."
            }
            button {
                class: "linkbtn",
                onclick: move |_| open_browser(NEW_APP_URL),
                "Register an OAuth app on GitHub →"
            }
        }
        div { class: "ghsection",
            button {
                class: "primarybtn",
                disabled: !ready,
                onclick: move |_| {
                    let id = st.client_id_input.peek().trim().to_string();
                    if id.is_empty() {
                        return;
                    }
                    let _ = save_client_id(&id);
                    // Root scope, not this component's. `device_flow` replaces
                    // SignIn with DevicePrompt on its very first step, and
                    // Dioxus cancels a task when the component that spawned it
                    // unmounts — so a plain `spawn` here kills the sign-in
                    // before it ever reaches the network.
                    spawn_forever(device_flow(st, id));
                },
                "Sign in with GitHub"
            }
            div { class: "ghhelp",
                "Already have a token? pullspace also picks up "
                code { "GITHUB_TOKEN" }
                " or an authenticated "
                code { "gh" }
                " CLI automatically — restart the app after running "
                code { "gh auth login" }
                "."
            }
        }
    }
}

#[component]
fn DevicePrompt(user_code: String, verification_uri: String, note: String) -> Element {
    let st = use_context::<St>();
    let mut account = st.account;
    let uri = verification_uri.clone();
    let show_code = !user_code.is_empty();

    rsx! {
        div { class: "ghsection",
            if show_code {
                div { class: "ghlabel", "Enter this code at GitHub" }
                div { class: "ghcode", "{user_code}" }
                button {
                    class: "primarybtn",
                    onclick: move |_| open_browser(&uri),
                    "Open {verification_uri}"
                }
            }
            div { class: "ghnote", "{note}" }
            button {
                class: "linkbtn",
                onclick: move |_| account.set(Account::SignedOut),
                "Cancel"
            }
        }
    }
}

/// Drive the device flow to completion, reporting each step through `account`.
async fn device_flow(st: St, client_id: String) {
    let mut account = st.account;
    account.set(Account::Connecting {
        user_code: String::new(),
        verification_uri: String::new(),
        note: "Asking GitHub for a code…".to_string(),
    });

    let cid = client_id.clone();
    let device = match tokio::task::spawn_blocking(move || start_device_flow(&cid)).await {
        Ok(Ok(d)) => d,
        Ok(Err(e)) => return account.set(Account::Failed(format!("{e:#}"))),
        Err(e) => return account.set(Account::Failed(e.to_string())),
    };

    account.set(Account::Connecting {
        user_code: device.user_code.clone(),
        verification_uri: device.verification_uri.clone(),
        note: "Waiting for you to approve the app on GitHub…".to_string(),
    });
    open_browser(&device.verification_uri);

    // GitHub rejects polls faster than `interval`, and asks for +5s each time
    // it says slow_down.
    let mut interval = device.interval.max(5);
    let mut waited = 0u64;
    loop {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        waited += interval;
        if waited >= device.expires_in {
            return account.set(Account::Failed(
                "That code expired before it was approved. Try again.".to_string(),
            ));
        }

        // Bail out if the user cancelled while we were sleeping.
        if !matches!(*st.account.peek(), Account::Connecting { .. }) {
            return;
        }

        let cid = client_id.clone();
        let code = device.device_code.clone();
        match tokio::task::spawn_blocking(move || poll_device_flow(&cid, &code)).await {
            Ok(Ok(crate::backend::auth::PollOutcome::Pending)) => continue,
            Ok(Ok(crate::backend::auth::PollOutcome::SlowDown { interval: bump })) => {
                interval += bump;
            }
            Ok(Ok(crate::backend::auth::PollOutcome::Token(token))) => {
                return finish_sign_in(st, token).await;
            }
            Ok(Ok(crate::backend::auth::PollOutcome::Denied)) => {
                return account.set(Account::Failed("Sign-in was denied on GitHub.".to_string()));
            }
            Ok(Ok(crate::backend::auth::PollOutcome::Expired)) => {
                return account.set(Account::Failed(
                    "That code expired before it was approved. Try again.".to_string(),
                ));
            }
            Ok(Err(e)) => return account.set(Account::Failed(format!("{e:#}"))),
            Err(e) => return account.set(Account::Failed(e.to_string())),
        }
    }
}

/// Persist the token and confirm which account it belongs to.
async fn finish_sign_in(st: St, token: String) {
    let mut account = st.account;
    let stored = token.clone();
    let saved = tokio::task::spawn_blocking(move || save_token(&stored)).await;
    if let Ok(Err(e)) = saved {
        // Not fatal — the token still works for this session.
        account.set(Account::Failed(format!("Signed in, but could not save: {e:#}")));
    }

    let probe = token.clone();
    let login = tokio::task::spawn_blocking(move || viewer_login(&probe)).await;
    match login {
        Ok(Ok(login)) => {
            let mut t = st.token;
            t.set(Some(Token {
                value: token,
                source: TokenSource::Stored,
            }));
            account.set(Account::SignedIn {
                login,
                source: TokenSource::Stored,
            });
        }
        Ok(Err(e)) => account.set(Account::Failed(format!("{e:#}"))),
        Err(e) => account.set(Account::Failed(e.to_string())),
    }
}

// ------------------------------------------------------------- repo & PR list

#[component]
fn SignedIn(login: String, source: TokenSource) -> Element {
    let st = use_context::<St>();
    let mut repo_input = st.repo_input;
    let repo_value = repo_input.read().clone();
    let prs = st.prs.read().clone();
    let revocable = source.revocable();

    rsx! {
        div { class: "ghsection ghaccount",
            span { class: "ghwho", "Signed in as " b { "{login}" } }
            span { class: "ghsrc", "{source.label()}" }
            span { class: "spacer" }
            if revocable {
                button {
                    class: "linkbtn",
                    onclick: move |_| { spawn_forever(do_sign_out(st)); },
                    "Sign out"
                }
            }
        }
        div { class: "ghsection",
            div { class: "ghlabel", "Repository or pull request" }
            div { class: "ghrow",
                input {
                    class: "ghinput",
                    r#type: "text",
                    placeholder: "owner/repo  ·  or a link to a pull request",
                    spellcheck: "false",
                    value: "{repo_value}",
                    oninput: move |e| repo_input.set(e.value()),
                    // Root scope so closing the panel mid-load does not strand
                    // the list on "Loading…".
                    onkeydown: move |e| {
                        if e.key() == Key::Enter {
                            spawn_forever(open_target(st));
                        }
                    },
                }
                button {
                    class: "primarybtn",
                    onclick: move |_| { spawn_forever(open_target(st)); },
                    "Load"
                }
            }
        }
        div { class: "ghsection prsection",
            match prs {
                PrList::Idle => rsx! {
                    div { class: "ghnote", "Enter a repository to list its open pull requests." }
                },
                PrList::Loading(note) => rsx! { div { class: "ghnote", "{note}" } },
                PrList::Failed(e) => rsx! { div { class: "gherror", "{e}" } },
                PrList::Ready { repo, items } if items.is_empty() => rsx! {
                    div { class: "ghnote", "No open pull requests in {repo}." }
                },
                PrList::Ready { repo, items } => rsx! {
                    div { class: "ghlabel", "{items.len()} open in {repo}" }
                    div { class: "prlist",
                        for pr in items {
                            PrRow { key: "{pr.number}", repo: repo.clone(), pr: pr.clone() }
                        }
                    }
                },
            }
        }
    }
}

#[component]
fn PrRow(repo: RepoRef, pr: PrSummary) -> Element {
    let st = use_context::<St>();
    let number = pr.number;
    let target = repo.clone();
    rsx! {
        div {
            class: "pritem",
            // Root scope: switching the list to `Loading` unmounts this row.
            onclick: move |_| { spawn_forever(open_pr(st, target.clone(), number)); },
            div { class: "prtop",
                span { class: "prnum", "#{pr.number}" }
                span { class: "prtitle", "{pr.title}" }
                if pr.draft {
                    span { class: "prdraft", "draft" }
                }
            }
            div { class: "prmeta", "{pr.author} · {pr.head_ref} → {pr.base_ref}" }
        }
    }
}

/// Forget the stored token. Leaves `$GITHUB_TOKEN` and `gh` alone; the next
/// startup will pick those up again.
async fn do_sign_out(st: St) {
    let _ = tokio::task::spawn_blocking(sign_out).await;
    let mut t = st.token;
    t.set(None);
    let mut a = st.account;
    a.set(Account::SignedOut);
    let mut p = st.prs;
    p.set(PrList::Idle);
}

/// Load whatever the user typed: a repo lists its PRs, a PR link opens it.
async fn open_target(st: St) {
    let raw = st.repo_input.peek().clone();
    let mut prs = st.prs;
    let Some((repo, number)) = parse_target(&raw) else {
        prs.set(PrList::Failed(
            "Enter owner/repo, or paste a GitHub URL.".to_string(),
        ));
        return;
    };
    if let Some(number) = number {
        return open_pr(st, repo, number).await;
    }

    let token = st.api_token();
    prs.set(PrList::Loading("Loading pull requests…".to_string()));
    let target = repo.clone();
    match tokio::task::spawn_blocking(move || list_prs(&token, &target)).await {
        Ok(Ok(items)) => prs.set(PrList::Ready { repo, items }),
        Ok(Err(e)) => prs.set(PrList::Failed(format!("{e:#}"))),
        Err(e) => prs.set(PrList::Failed(e.to_string())),
    }
}

/// Open a PR: metadata from the API, then contents from a local clone if one
/// can be had, falling back to the API.
///
/// The source is settled *before* the file tree is loaded, because a local
/// repository can list its own tree instantly — asking the API first would
/// download up to 20 MB of JSON we are about to throw away.
async fn open_pr(st: St, repo: RepoRef, number: u64) {
    let token = st.api_token();
    let mut prs = st.prs;

    prs.set(PrList::Loading("Loading pull request…".to_string()));
    let target = repo.clone();
    let tok = token.clone();
    let mut detail = match tokio::task::spawn_blocking(move || load_pr(&tok, &target, number)).await
    {
        Ok(Ok(d)) => d,
        Ok(Err(e)) => return prs.set(PrList::Failed(format!("{e:#}"))),
        Err(e) => return prs.set(PrList::Failed(e.to_string())),
    };

    prs.set(PrList::Loading(
        "Syncing the repository (first time may take a moment)…".to_string(),
    ));
    let target = repo.clone();
    let head = detail.head_sha.clone();
    let own = st.root_path();
    let tok = token.clone();
    let local = tokio::task::spawn_blocking(move || {
        mirror::prepare(&target, number, &head, &tok, Some(&own))
    })
    .await;

    let source = match local {
        Ok(Ok(repo_on_disk)) => PrSource::Local {
            git_dir: repo_on_disk.git_dir,
            borrowed: repo_on_disk.borrowed,
        },
        // Falling back is not an error worth stopping for — the API path works,
        // it is just slower per file.
        _ => PrSource::Api,
    };

    prs.set(PrList::Loading("Reading the file tree…".to_string()));
    let head = detail.head_sha.clone();
    let target = repo.clone();
    let src = source.clone();
    let tree = tokio::task::spawn_blocking(move || match &src {
        PrSource::Local { git_dir, .. } => {
            mirror::tree_paths(git_dir, &head).map(|p| (p, false)).ok()
        }
        PrSource::Api => repo_tree(&token, &target, &head).ok(),
    })
    .await;

    if let Ok(Some((paths, truncated))) = tree {
        detail.tree = paths;
        detail.tree_truncated = truncated;
    }

    // Check the head commit out as ordinary files, so search, Go to Definition
    // and Find References work on a pull request exactly as they do on a local
    // repository — they walk a directory, and this is one.
    let checkout = match &source {
        PrSource::Local { git_dir, .. } => {
            prs.set(PrList::Loading(
                "Checking out the pull request…".to_string(),
            ));
            let git_dir = git_dir.clone();
            let target = repo.clone();
            let head = detail.head_sha.clone();
            match tokio::task::spawn_blocking(move || mirror::materialize(&git_dir, &target, &head))
                .await
            {
                Ok(Ok(dir)) => ScanRoot::Dir(dir),
                // The pull request is perfectly reviewable without a checkout,
                // so this is not worth refusing to open it over — but it is
                // worth saying, or search just looks broken.
                Ok(Err(e)) => ScanRoot::Unavailable(format!(
                    "Search is off: this pull request could not be checked out — {e:#}"
                )),
                Err(e) => ScanRoot::Unavailable(format!(
                    "Search is off: the checkout did not finish — {e}"
                )),
            }
        }
        // Nothing on disk to check out of. The pull request still opens; the
        // features that need a directory are the ones that go quiet.
        PrSource::Api => ScanRoot::Unavailable(
            "Search needs a local copy — this pull request is read over the GitHub API".to_string(),
        ),
    };

    prs.set(PrList::Idle);
    st.enter_pr(detail, source, checkout);
}
