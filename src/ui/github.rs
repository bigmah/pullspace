//! The GitHub overlay: sign in, pick a repository, pick a pull request.

use std::time::Duration;

use dioxus::prelude::*;

use crate::backend::auth::{
    open_browser, poll_device_flow, save_client_id, save_token, sign_out, start_device_flow,
    Token, TokenSource,
};
use crate::backend::github::{
    list_prs, load_pr, my_repos, parse_target, repo_tree, search_repos, viewer_login, PrSummary,
    RepoHit, RepoRef,
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

/// Put `text` on the clipboard, reporting whether it worked.
///
/// `navigator.clipboard` needs a secure context, which a desktop webview
/// serving from its own scheme is not always considered to be, so fall back to
/// the old selection-based command. The outcome comes back either way, because
/// a Copy button that says "Copied" without having copied anything is worse
/// than no button — the user only finds out at the point of pasting.
async fn copy_to_clipboard(text: &str) -> bool {
    let literal = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_string());
    let mut eval = document::eval(&format!(
        r#"(async function() {{
            const text = {literal};
            try {{
                if (window.isSecureContext && navigator.clipboard) {{
                    await navigator.clipboard.writeText(text);
                    dioxus.send(true);
                    return;
                }}
            }} catch (e) {{}}
            try {{
                const ta = document.createElement('textarea');
                ta.value = text;
                ta.setAttribute('readonly', '');
                ta.style.position = 'fixed';
                ta.style.opacity = '0';
                document.body.appendChild(ta);
                ta.select();
                ta.setSelectionRange(0, ta.value.length);
                const ok = document.execCommand('copy');
                document.body.removeChild(ta);
                dioxus.send(!!ok);
                return;
            }} catch (e) {{}}
            dioxus.send(false);
        }})();"#
    ));
    // Every path above reports back, but a script that fails to reach any of
    // them would leave the button waiting forever. Time out into the honest
    // answer instead.
    let answered = tokio::time::timeout(Duration::from_secs(3), eval.recv::<bool>()).await;
    matches!(answered, Ok(Ok(true)))
}

#[component]
fn DevicePrompt(user_code: String, verification_uri: String, note: String) -> Element {
    let st = use_context::<St>();
    let mut account = st.account;
    let uri = verification_uri.clone();
    let show_code = !user_code.is_empty();

    // None until the button is pressed; then whether it actually copied.
    let mut copied = use_signal(|| None::<bool>);
    let code = user_code.clone();
    let shortcut = if cfg!(target_os = "macos") { "⌘C" } else { "Ctrl+C" };
    let copy_label = match *copied.read() {
        None => "Copy".to_string(),
        Some(true) => "Copied ✓".to_string(),
        Some(false) => format!("Copy failed — select it and press {shortcut}"),
    };

    rsx! {
        div { class: "ghsection",
            if show_code {
                div { class: "ghlabel", "Enter this code at GitHub" }
                div { class: "ghcoderow",
                    div {
                        class: "ghcode",
                        // Clicking selects the whole code, so the keyboard
                        // route works even if the clipboard call does not.
                        title: "Click to select, or use Copy",
                        "{user_code}"
                    }
                    button {
                        class: "copybtn",
                        onclick: move |_| {
                            let text = code.clone();
                            async move {
                                let ok = copy_to_clipboard(&text).await;
                                copied.set(Some(ok));
                                if ok {
                                    // Back to "Copy" so a second copy still
                                    // looks like it did something.
                                    tokio::time::sleep(Duration::from_secs(2)).await;
                                    copied.set(None);
                                }
                            }
                        },
                        "{copy_label}"
                    }
                }
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
        RepoPicker {}
        div { class: "ghsection prsection",
            match prs {
                PrList::Idle => rsx! {
                    div { class: "ghnote", "Pick a repository to list its open pull requests." }
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

// ------------------------------------------------------------- repo picker

/// How many repositories to offer at once. Enough to recognise the one you
/// meant, few enough that the list stays a glance rather than a second search.
const SUGGESTION_LIMIT: u32 = 8;

/// Long enough that typing a repository name costs one request, not eight.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(250);

/// What the last lookup turned up, and what it was looking for.
///
/// The query travels with the results because the box runs ahead of them: it is
/// the only way the list can tell "nothing matches this" from "the answer for
/// what you have typed is still on its way".
#[derive(Clone, PartialEq)]
struct Suggestions {
    query: String,
    items: Vec<RepoHit>,
    error: Option<String>,
}

/// The suggestions on offer, read from outside the render pass.
fn suggested(found: &Resource<Option<Suggestions>>) -> Vec<RepoHit> {
    match &*found.peek() {
        Some(Some(s)) => s.items.clone(),
        _ => Vec::new(),
    }
}

/// Take a repository from the list: put its name in the box, close the list,
/// and show its open pull requests.
fn choose(st: St, mut open: Signal<bool>, repo: RepoRef) {
    let mut input = st.repo_input;
    input.set(repo.to_string());
    open.set(false);
    // Root scope: closing the list unmounts the row that was clicked.
    spawn_forever(load_repo_prs(st, repo));
}

/// Star counts run long, and the exact number is not what anyone reads.
fn stars_label(stars: u64) -> String {
    match stars {
        0 => String::new(),
        n if n < 1000 => n.to_string(),
        n if n < 100_000 => format!("{:.1}k", n as f64 / 1000.0),
        n => format!("{}k", n / 1000),
    }
}

/// The repository box: type a name and pick from what GitHub matches, or paste
/// an `owner/repo` or pull request link straight in.
///
/// Searching is what the box is for — a link is the fallback, not the price of
/// entry. With nothing typed it offers the account's own repositories, since
/// the pull requests you have been asked to look at are nearly always on one.
#[component]
fn RepoPicker() -> Element {
    let st = use_context::<St>();
    let mut repo_input = st.repo_input;
    let typed = repo_input.read().clone();

    // The list is only up while a repository is being chosen. Picking one, or
    // pressing Load, puts it away rather than leaving half the panel covered by
    // results nobody is reading any more.
    let mut open = use_signal(|| false);
    let mut highlight = use_signal(|| 0usize);

    let found = use_resource(move || {
        // Read the dependencies here, in the synchronous part: a change to
        // either cancels the lookup in flight and starts the next one.
        let showing = *open.read();
        let query = st.repo_input.read().trim().to_string();
        let token = st.api_token();
        async move {
            if !showing {
                return None;
            }
            // Debounce: the next keystroke drops this task where it stands, so
            // nothing reaches GitHub until the typing stops.
            tokio::time::sleep(SEARCH_DEBOUNCE).await;
            let q = query.clone();
            let hits = tokio::task::spawn_blocking(move || {
                if q.is_empty() {
                    my_repos(&token, SUGGESTION_LIMIT)
                } else {
                    search_repos(&token, &q, SUGGESTION_LIMIT)
                }
            })
            .await;
            Some(match hits {
                Ok(Ok(items)) => Suggestions {
                    query,
                    items,
                    error: None,
                },
                Ok(Err(e)) => Suggestions {
                    query,
                    items: Vec::new(),
                    error: Some(format!("{e:#}")),
                },
                Err(e) => Suggestions {
                    query,
                    items: Vec::new(),
                    error: Some(e.to_string()),
                },
            })
        }
    });

    let showing = *open.read();
    let current = found.cloned().flatten();
    let settled = current.as_ref().is_some_and(|s| s.query == typed.trim());
    let items = current.as_ref().map(|s| s.items.clone()).unwrap_or_default();
    let error = current.as_ref().and_then(|s| s.error.clone());
    let hi = (*highlight.read()).min(items.len().saturating_sub(1));

    rsx! {
        div { class: "ghsection",
            div { class: "ghlabel", "Repository or pull request" }
            div { class: "ghrow",
                input {
                    class: "ghinput",
                    r#type: "text",
                    placeholder: "search repositories · owner/repo · or a link to a pull request",
                    spellcheck: "false",
                    autocomplete: "off",
                    value: "{typed}",
                    onfocus: move |_| open.set(true),
                    oninput: move |e| {
                        repo_input.set(e.value());
                        open.set(true);
                        highlight.set(0);
                    },
                    onkeydown: move |e| {
                        // Whatever is on screen, rather than the render that
                        // produced this handler — the list moves under it.
                        let items = suggested(&found);
                        let at = (*highlight.peek()).min(items.len().saturating_sub(1));
                        match e.key() {
                            Key::ArrowDown if !items.is_empty() => {
                                e.prevent_default();
                                highlight.set((at + 1).min(items.len() - 1));
                            }
                            Key::ArrowUp if !items.is_empty() => {
                                e.prevent_default();
                                highlight.set(at.saturating_sub(1));
                            }
                            Key::Escape => open.set(false),
                            Key::Enter => match items.get(at) {
                                // What is highlighted in the list wins…
                                Some(hit) => choose(st, open, hit.repo.clone()),
                                // …but a pasted link, or a name typed out in
                                // full, never needed the list. Root scope so
                                // closing the panel mid-load does not strand
                                // it on "Loading…".
                                None => {
                                    open.set(false);
                                    spawn_forever(open_target(st));
                                }
                            },
                            _ => {}
                        }
                    },
                }
                button {
                    class: "primarybtn",
                    onclick: move |_| {
                        open.set(false);
                        spawn_forever(open_target(st));
                    },
                    "Load"
                }
            }
            if showing {
                if let Some(e) = error {
                    div { class: "gherror", "{e}" }
                } else if !items.is_empty() {
                    div { class: "repolist",
                        for (i , hit) in items.iter().enumerate() {
                            RepoRow { key: "{hit.repo}", hit: hit.clone(), active: i == hi, open }
                        }
                    }
                } else if !settled {
                    div { class: "ghnote", "Searching GitHub…" }
                } else if typed.trim().is_empty() {
                    div { class: "ghnote",
                        "Type a name to search GitHub, or paste a link to a pull request."
                    }
                } else {
                    div { class: "ghnote", "No repositories match “{typed}”." }
                }
            }
        }
    }
}

#[component]
fn RepoRow(hit: RepoHit, active: bool, open: Signal<bool>) -> Element {
    let st = use_context::<St>();
    let repo = hit.repo.clone();
    let class = if active { "repoitem on" } else { "repoitem" };
    let stars = stars_label(hit.stars);
    rsx! {
        div {
            class: "{class}",
            onclick: move |_| choose(st, open, repo.clone()),
            div { class: "repotop",
                span { class: "reponame", "{hit.repo}" }
                if hit.private {
                    span { class: "repotag", "private" }
                }
                if hit.fork {
                    span { class: "repotag", "fork" }
                }
                if hit.archived {
                    span { class: "repotag", "archived" }
                }
                if !stars.is_empty() {
                    span { class: "repostars", "★ {stars}" }
                }
            }
            // The date is the fallback rather than a second line: for the
            // account's own repositories — often private and undescribed — how
            // recently one moved is exactly what picks it out of the list.
            if !hit.description.is_empty() {
                div { class: "repodesc", "{hit.description}" }
            } else if !hit.pushed.is_empty() {
                div { class: "repodesc", "pushed {hit.pushed}" }
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
            "That is not a repository — pick one from the list, or paste a GitHub link."
                .to_string(),
        ));
        return;
    };
    if let Some(number) = number {
        return open_pr(st, repo, number).await;
    }
    load_repo_prs(st, repo).await;
}

/// Show a repository's open pull requests.
async fn load_repo_prs(st: St, repo: RepoRef) {
    let token = st.api_token();
    let mut prs = st.prs;
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
///
/// This is also what `⟳` runs on an open pull request: reloading one means
/// exactly this work again, since a push moves the head commit and everything
/// downstream — objects, tree, checkout, cached contents — hangs off it.
pub(super) async fn open_pr(st: St, repo: RepoRef, number: u64) {
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
