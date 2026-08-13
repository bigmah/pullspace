//! The GitHub overlay: pick a repository, pick a pull request, and — if a
//! private repository is wanted — sign in.
//!
//! The picker is up in every state, signed in or not. GitHub serves public
//! repositories to anyone, `api.github.com` answers cross-origin, and file
//! bytes come off the CDN unmetered — so a token buys private repositories and
//! a rate limit of 5000 an hour instead of 60, and nothing else. Asking for one
//! before showing the box would be charging entry to a public building.

use std::collections::HashMap;
use std::time::Duration;

use dioxus::prelude::*;

use crate::backend::auth::{self, open_browser, Token};
use crate::backend::blobs;
use crate::backend::github::{self, parse_target, PrSummary, RepoHit, RepoRef, RepoView};

use super::app::{Account, PrList, St};
use super::compat;
use super::topbar::size_label;

#[component]
pub fn GhPanel() -> Element {
    let st = use_context::<St>();
    let mut gh_open = st.gh_open;
    let account = st.account.read().clone();
    // Whether the reader has asked for the token form, or asked for it to go
    // away. `None` until they say either, which is when [`form_open`] answers
    // for them.
    let token_form = use_signal(|| None::<bool>);
    let showing = form_open(&token_form, &account);

    rsx! {
        div {
            class: "ghoverlay",
            // Click-away closes, but only on the backdrop itself.
            onclick: move |_| gh_open.set(false),
            // Escape closes it too, which is the other half of what anyone
            // expects from something that covers the app. Keydowns reach here
            // by bubbling out of whichever field has focus.
            tabindex: "-1",
            onkeydown: move |e| {
                if e.key() == Key::Escape {
                    gh_open.set(false);
                }
            },
            div {
                class: "ghpanel",
                onclick: move |e| e.stop_propagation(),
                div { class: "ghhdr",
                    span { class: "ghtitle", "GitHub" }
                    span { class: "spacer" }
                    button {
                        class: "iconbtn",
                        title: "Close",
                        onclick: move |_| gh_open.set(false),
                        "✕"
                    }
                }
                div { class: "ghbody",
                    match account.clone() {
                        Account::Checking => rsx! {
                            div { class: "ghnote", "Looking for a saved sign-in…" }
                        },
                        Account::SignedOut => rsx! {
                            Anonymous { form: token_form, showing, error: None }
                        },
                        Account::Failed(e) => rsx! {
                            Anonymous { form: token_form, showing, error: Some(e) }
                        },
                        Account::SignedIn { login } => rsx! { SignedIn { login } },
                    }
                    // Everything below is the same whether or not there is a
                    // token — it is only *which* repositories answer that
                    // changes. Held back while the saved token is being
                    // checked, so a search started in that half-second does not
                    // go out anonymously and come back rate-limited.
                    if !matches!(account, Account::Checking) {
                        // Whichever box the reader came to type in gets the
                        // caret. The token form is only up because something
                        // wants a token, so when it is up, it is that one.
                        RepoPicker { autofocus: !showing }
                        PrSection {}
                    }
                    LocalCopy {}
                }
            }
        }
    }
}

// --------------------------------------------------------------- local copy

/// What is on the disk, and the one button for getting rid of it.
///
/// Worth a panel of its own because it is the one thing pullspace leaves
/// behind. Everything else about the app is a page that forgets you: this keeps
/// entire repositories, and somebody who wants that space back should not have
/// to go looking through browser settings for it.
#[component]
fn LocalCopy() -> Element {
    let st = use_context::<St>();
    // Re-read after a clear, and when a clone finishes — but not while one is
    // running, since asking the browser how full it is on every file it writes
    // would be the most expensive thing on the page.
    let settled = use_memo(move || st.cloning.read().is_none_or(|at| at.finished()));
    let stored = use_resource(move || async move {
        st.store_gen.read();
        settled.read();
        (blobs::stored(), blobs::usage().await)
    });

    let (files, used) = stored.cloned().unwrap_or((0, None));
    let size = match used {
        Some((used, quota)) if quota > 0.0 => format!(
            " · {} of {} used",
            size_label(used as u64),
            size_label(quota as u64)
        ),
        _ => String::new(),
    };

    rsx! {
        div { class: "ghsection",
            div { class: "ghrow",
                div { class: "ghlabel", "Local copy" }
                span { class: "spacer" }
                if files > 0 {
                    button {
                        class: "linkbtn",
                        title: "Delete every repository kept in this browser",
                        // Root scope: emptying the store means deleting every
                        // file in it, and closing the panel part way through
                        // must not leave that half done.
                        onclick: move |_| {
                            spawn_forever(async move {
                                blobs::clear().await;
                                // The decoded copies search reads from are a
                                // copy of what has just been deleted.
                                crate::backend::scan::forget();
                                st.store_changed();
                            });
                        },
                        "Clear"
                    }
                }
            }
            div { class: "ghhelp",
                if files == 0 {
                    "Repositories you open are kept in this browser's own filesystem, so the \
                     next pull request on one of them opens without downloading it again."
                } else {
                    "{files} files kept{size} — so a repository opened before comes back \
                     instantly, and a pull request on it downloads only what changed."
                }
            }
        }
    }
}

// ------------------------------------------------------------------ sign in

/// Where to make the token pullspace asks for.
const NEW_TOKEN_URL: &str = "https://github.com/settings/personal-access-tokens/new";

/// Whether the token form is up.
///
/// What the reader last asked for, and — until they ask for anything — whether
/// there is a token that needs fixing. It cannot be settled at mount: the panel
/// is up from the first frame, while the saved token is still being checked, so
/// a rejection arrives after whatever was decided on the way in.
fn form_open(form: &Signal<Option<bool>>, account: &Account) -> bool {
    form.read()
        .unwrap_or(matches!(account, Account::Failed(_)))
}

/// Signed out: say what that costs, and offer the token form to anyone it costs
/// something.
///
/// It costs two things and no others — private repositories, and the difference
/// between sixty requests an hour and five thousand — so both are named here,
/// next to the button that fixes them. The picker below this is live either way.
#[component]
fn Anonymous(form: Signal<Option<bool>>, showing: bool, error: Option<String>) -> Element {
    let mut form = form;

    rsx! {
        div { class: "ghsection ghaccount",
            span { class: "ghwho", "Browsing " b { "anonymously" } }
            span { class: "spacer" }
            button {
                class: "linkbtn",
                onclick: move |_| form.set(Some(!showing)),
                if showing { "Cancel" } else { "Add a token" }
            }
        }
        if let Some(e) = error {
            div { class: "gherror", "{e}" }
        }
        if showing {
            SignIn {}
        } else {
            div { class: "ghsection",
                div { class: "ghhelp",
                    "Public repositories and their pull requests need no sign-in. A token adds "
                    "private repositories, and raises GitHub's limit of 60 API requests an hour "
                    "to 5000 — file contents come off the CDN and count against neither."
                }
            }
        }
    }
}

/// Sign-in for the static build: paste a token.
///
/// The device flow is not an option here and no amount of work would make it
/// one — GitHub's OAuth endpoints send no CORS headers, so no page may call
/// them, and the web flow needs a secret that only a server could hold. Every
/// alternative ends at infrastructure that would have to be run by someone and
/// trusted with everyone's tokens.
///
/// So: paste one. It is stored on this origin and sent only to
/// api.github.com — a shorter path than any OAuth flow would give it, and one
/// you can check for yourself in the network tab.
#[component]
fn SignIn() -> Element {
    let st = use_context::<St>();
    let mut typed = use_signal(String::new);
    let value = typed.read().clone();
    let ready = !value.trim().is_empty();

    rsx! {
        div { class: "ghsection",
            div { class: "ghlabel", "GitHub token" }
            input {
                class: "ghinput",
                // A bearer token is a password, and shoulder-surfing is real.
                r#type: "password",
                placeholder: "github_pat_… or ghp_…",
                spellcheck: "false",
                autocomplete: "off",
                value: "{value}",
                // This form is only on screen because somebody asked for it,
                // and what they asked for is somewhere to paste.
                onmounted: move |e| async move {
                    let _ = e.set_focus(true).await;
                },
                oninput: move |e| typed.set(e.value()),
                // Pasting then hitting return is the whole interaction.
                onkeydown: move |e| {
                    if e.key() == Key::Enter {
                        let token = typed.peek().trim().to_string();
                        if !token.is_empty() {
                            spawn_forever(use_pasted_token(st, token));
                        }
                    }
                },
            }
            div { class: "ghhelp",
                "A fine-grained token needs read access to "
                b { "Contents" }
                ", "
                b { "Pull requests" }
                " and "
                b { "Metadata" }
                ". A classic token needs "
                code { "repo" }
                "."
            }
            button {
                class: "linkbtn",
                onclick: move |_| open_browser(NEW_TOKEN_URL),
                "Create a token on GitHub →"
            }
        }
        div { class: "ghsection",
            button {
                class: "primarybtn",
                disabled: !ready,
                onclick: move |_| {
                    let token = typed.peek().trim().to_string();
                    if token.is_empty() {
                        return;
                    }
                    // Root scope, not this component's: signing in unmounts
                    // SignIn, and Dioxus cancels a task when the component that
                    // spawned it goes away.
                    spawn_forever(use_pasted_token(st, token));
                },
                "Save token"
            }
            div { class: "ghhelp",
                "Kept in this browser's local storage, on this site only. It is sent to "
                code { "api.github.com" }
                " and nowhere else — there is no pullspace server to send it to."
            }
        }
    }
}

/// Verify a pasted token before keeping it — a typo should not be something
/// you have to sign out of.
async fn use_pasted_token(st: St, token: String) {
    let mut account = st.account;
    account.set(Account::Checking);
    match github::viewer_login(&token).await {
        Ok(login) => {
            auth::save_token(&token);
            let mut t = st.token;
            t.set(Some(Token { value: token }));
            account.set(Account::SignedIn { login });
        }
        Err(e) => account.set(Account::Failed(format!("{e:#}"))),
    }
}

#[component]
fn SignedIn(login: String) -> Element {
    let st = use_context::<St>();

    rsx! {
        div { class: "ghsection ghaccount",
            span { class: "ghwho", "Signed in as " b { "{login}" } }
            span { class: "spacer" }
            button {
                class: "linkbtn",
                onclick: move |_| do_sign_out(st),
                "Sign out"
            }
        }
    }
}

/// The open pull requests of whatever repository the picker last loaded.
///
/// Signed in or not: a public repository lists its pull requests to anyone, and
/// this pane is the same one either way.
#[component]
fn PrSection() -> Element {
    let st = use_context::<St>();
    let prs = st.prs.read().clone();

    rsx! {
        div { class: "ghsection prsection",
            match prs {
                PrList::Idle => rsx! {
                    div { class: "ghnote", "Pick a repository to list its open pull requests." }
                },
                PrList::Loading(note) => rsx! { div { class: "ghnote", "{note}" } },
                PrList::Failed(e) => rsx! { div { class: "gherror", "{e}" } },
                PrList::Ready { repo, items } if items.is_empty() => rsx! {
                    div { class: "ghnote", "No open pull requests in {repo}." }
                    BrowseRow { repo: repo.clone(), no_prs: true }
                },
                PrList::Ready { repo, items } => rsx! {
                    div { class: "ghlabel", "{items.len()} open in {repo}" }
                    div { class: "prlist",
                        for pr in items {
                            PrRow { key: "{pr.number}", repo: repo.clone(), pr: pr.clone() }
                        }
                    }
                    BrowseRow { repo: repo.clone(), no_prs: false }
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
///
/// Search is metered by the minute rather than by the hour — ten of them to an
/// anonymous caller — so this is the difference between a name typed with a
/// pause in it costing one request and costing four.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(350);

/// Below this, a query is not worth a request: one letter matches most of
/// GitHub, and nobody reads what comes back.
const MIN_QUERY: usize = 2;

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
/// the pull requests you are asked to review are nearly always on one; signed
/// out there is no such account, so it waits to be typed into instead.
#[component]
fn RepoPicker(autofocus: bool) -> Element {
    let st = use_context::<St>();
    let mut repo_input = st.repo_input;
    let typed = repo_input.read().clone();

    // The list is only up while a repository is being chosen. Picking one, or
    // pressing Load, puts it away rather than leaving half the panel covered by
    // results nobody is reading any more.
    let mut open = use_signal(|| false);
    let mut highlight = use_signal(|| 0usize);
    // Every query this picker has already had an answer for.
    //
    // Typing a name is not a straight line — it is typed, over-typed, and
    // backspaced through — and every step back over ground already covered was
    // a fresh request for an answer we have had once. On the minute-long search
    // budget that is most of what runs it out. Cleared with the panel, which is
    // as long as any of it is worth trusting.
    let mut seen = use_signal(HashMap::<String, Vec<RepoHit>>::new);

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
            let ready = |items| {
                Some(Suggestions {
                    query: query.clone(),
                    items,
                    error: None,
                })
            };
            // An answer already in hand: no wait, and no request.
            if let Some(items) = seen.peek().get(&query).cloned() {
                return ready(items);
            }
            // One letter is not a search, and neither is half of one.
            if !query.is_empty() && query.chars().count() < MIN_QUERY {
                return ready(Vec::new());
            }
            // Debounce: the next keystroke drops this task where it stands, so
            // nothing reaches GitHub until the typing stops.
            compat::sleep(SEARCH_DEBOUNCE).await;
            let q = query.clone();
            let hits = if q.is_empty() {
                github::my_repos(&token, SUGGESTION_LIMIT).await.map_err(|e| format!("{e:#}"))
            } else {
                github::search_repos(&token, &q, SUGGESTION_LIMIT).await.map_err(|e| format!("{e:#}"))
            };
            Some(match hits {
                Ok(items) => {
                    seen.write().insert(query.clone(), items.clone());
                    Suggestions {
                        query,
                        items,
                        error: None,
                    }
                }
                Err(e) => Suggestions {
                    query,
                    items: Vec::new(),
                    error: Some(e),
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
                    // The panel is opened to look something up, so it opens
                    // ready to be typed into.
                    onmounted: move |e| async move {
                        if autofocus {
                            let _ = e.set_focus(true).await;
                        }
                    },
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
                            // The list first, the panel second — and only one
                            // of them per press, so Escape out of the
                            // suggestions does not take the panel with it.
                            Key::Escape => {
                                if *open.peek() {
                                    e.stop_propagation();
                                    open.set(false);
                                }
                            }
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
                            RepoRow {
                                key: "{hit.repo}",
                                hit: hit.clone(),
                                active: i == hi,
                                open,
                                index: i,
                                highlight,
                            }
                        }
                    }
                } else if !settled {
                    div { class: "ghnote", "Searching GitHub…" }
                } else if typed.trim().chars().count() < MIN_QUERY {
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
fn RepoRow(
    hit: RepoHit,
    active: bool,
    open: Signal<bool>,
    index: usize,
    highlight: Signal<usize>,
) -> Element {
    let st = use_context::<St>();
    let repo = hit.repo.clone();
    let class = if active { "repoitem on" } else { "repoitem" };
    let stars = stars_label(hit.stars);
    let mut highlight = highlight;
    rsx! {
        div {
            class: "{class}",
            onclick: move |_| choose(st, open, repo.clone()),
            // Move the selection to whatever the pointer is on, so the row
            // under the cursor and the row Enter will open are always the same
            // one.
            onmouseenter: move |_| highlight.set(index),
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

/// Open the repository itself, at its default branch.
///
/// A repository with no open pull requests is still worth reading, and so is
/// the code around the one you are reviewing — so this is offered either way:
/// as the only way in when the list is empty, and as a quieter link when the
/// pull requests are what you probably came for.
#[component]
fn BrowseRow(repo: RepoRef, no_prs: bool) -> Element {
    let st = use_context::<St>();
    let class = if no_prs { "primarybtn" } else { "linkbtn" };
    let label = if no_prs {
        format!("Browse {repo}")
    } else {
        "Browse the whole repository →".to_string()
    };
    rsx! {
        button {
            class,
            title: "View {repo} at its default branch, with no pull request",
            // Root scope: loading replaces the list this button lives in.
            onclick: move |_| {
                spawn_forever(browse_repo(st, repo.clone()));
            },
            "{label}"
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

/// Forget the stored token.
fn do_sign_out(st: St) {
    auth::sign_out();
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
    match github::list_prs(&token, &repo).await {
        Ok(items) => prs.set(PrList::Ready { repo, items }),
        Err(e) => prs.set(PrList::Failed(format!("{e:#}"))),
    }
}

/// Open a repository with no pull request involved: its default branch, at the
/// commit its tip points to.
///
/// `⟳` runs this again on an open repository, which is how a browse picks up
/// commits pushed since — the branch tip moves, and everything downstream of it
/// is keyed by commit.
pub(super) async fn browse_repo(st: St, repo: RepoRef) {
    let token = st.api_token();
    let mut prs = st.prs;

    prs.set(PrList::Loading("Reading the repository…".to_string()));
    let head = match github::repo_head(&token, &repo).await {
        Ok(h) => h,
        Err(e) => return prs.set(PrList::Failed(format!("{e:#}"))),
    };

    prs.set(PrList::Loading("Reading the file tree…".to_string()));
    // A pull request with no readable tree still has its changed files to show.
    // A repository has nothing at all, so this is where it stops — with the
    // list still on screen, rather than on an explorer that looks empty.
    let Some(tree) = tree_at(&token, &repo, &head.sha).await else {
        return prs.set(PrList::Failed(format!(
            "Could not read the file list for {repo}."
        )));
    };

    prs.set(PrList::Idle);
    st.enter_repo(RepoView {
        repo,
        branch: head.branch,
        head_sha: head.sha,
        tree,
    });
}

/// Which files a commit is made of.
///
/// Off the disk when that commit has been read before — a commit's tree never
/// changes, so a stored one is not stale, and skipping the request is what
/// makes reopening a pull request instant rather than merely fast. It also
/// keeps a reader who is out of API requests in business.
async fn tree_at(token: &str, repo: &RepoRef, sha: &str) -> Option<github::Snapshot> {
    if let Some(stored) = blobs::load(repo, sha).await {
        return Some(stored);
    }
    github::repo_tree(token, repo, sha).await.ok()
}

/// Open a pull request: its metadata and changed files, then the repository
/// tree at its head so the explorer shows the whole thing rather than only what
/// changed.
///
/// This is also what `⟳` runs on an open pull request: reloading one means
/// exactly this work again, since a push moves the head commit and everything
/// downstream of it is keyed by that commit.
pub(super) async fn open_pr(st: St, repo: RepoRef, number: u64) {
    let token = st.api_token();
    let mut prs = st.prs;

    prs.set(PrList::Loading("Loading pull request…".to_string()));
    let mut detail = match github::load_pr(&token, &repo, number).await {
        Ok(d) => d,
        Err(e) => return prs.set(PrList::Failed(format!("{e:#}"))),
    };

    prs.set(PrList::Loading("Reading the file tree…".to_string()));
    // A tree that will not load costs the explorer its unchanged files, which
    // is a smaller loss than refusing to open the review at all.
    if let Some(tree) = tree_at(&token, &repo, &detail.head_sha).await {
        detail.tree = tree;
    }
    // And the merge base's, which is what the left-hand side of every diff is
    // read from. Only the changed files are wanted out of it, but knowing what
    // they hash to is what lets them come off the disk: the base commit is
    // usually a branch tip that has been read before.
    if let Some(base) = tree_at(&token, &repo, &detail.base_sha).await {
        detail.base_tree = base;
    }

    prs.set(PrList::Idle);
    st.enter_pr(detail);
}
