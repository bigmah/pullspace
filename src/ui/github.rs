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

use crate::backend::auth::{self, Token, open_browser};
use crate::backend::blobs;
use crate::backend::github::{
    self, OwnerHit, PR_PAGE, PrHeader, PrState, PrSummary, RepoHit, RepoRef, RepoView,
    parse_commit_target, parse_owner, parse_target,
};

use super::app::{Account, Fetch, Got, PrList, St};
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
    form.read().unwrap_or(matches!(account, Account::Failed(_)))
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

/// The pull requests of whatever repository is open, or was last named in the
/// picker.
///
/// Signed in or not: a public repository lists its pull requests to anyone, and
/// this pane is the same one either way.
#[component]
fn PrSection() -> Element {
    let st = use_context::<St>();
    let held = st.prs.read().clone();
    // What is on screen already, so the row for it is marked rather than
    // offered as somewhere to go.
    let current = st.workspace.read().pr_number();
    let fetching = st.fetch.read().clone();

    let Some(list) = held else {
        return rsx! {
            div { class: "ghsection prsection",
                if let Fetch::Failed(e) = fetching {
                    div { class: "gherror", "{e}" }
                }
                div { class: "ghnote", "Pick a repository to list its pull requests." }
            }
        };
    };
    let repo = list.repo.clone();
    let count = list.items().len();
    let word = state_word(list.state);
    let ready = matches!(list.got, Got::Ready(_));

    rsx! {
        div { class: "ghsection prsection",
            // A failed load is reported here rather than nowhere: the panel is
            // where the thing that failed was asked for.
            if let Fetch::Failed(e) = fetching {
                div { class: "gherror", "{e}" }
            }
            div { class: "ghrow",
                if ready && count > 0 {
                    div { class: "ghlabel", "{count} {word}in {repo}" }
                } else {
                    div { class: "ghlabel", "Pull requests in {repo}" }
                }
                span { class: "spacer" }
                PrStates {}
            }
            PrListBody { current }
            BrowseRow { repo, no_prs: ready && count == 0 }
        }
    }
}

/// Open · closed · all — which of a repository's pull requests the list is a
/// list of.
///
/// One toggle, read by the picker and by the top bar's switcher alike, because
/// there is one list and both of them are looking at it.
#[component]
pub(super) fn PrStates() -> Element {
    let st = use_context::<St>();
    let mut pr_state = st.pr_state;
    let now = *pr_state.read();

    rsx! {
        div { class: "prstates",
            for state in PrState::EVERY {
                button {
                    key: "{state.label()}",
                    class: if state == now { "pstate on" } else { "pstate" },
                    title: "{state.why()}",
                    onclick: move |_| pr_state.set(state),
                    "{state.label()}"
                }
            }
        }
    }
}

/// The adjective in "3 open pull requests" — with the space it needs, since the
/// list of everything has no adjective at all.
fn state_word(state: PrState) -> &'static str {
    match state {
        PrState::Open => "open ",
        PrState::Closed => "closed ",
        PrState::All => "",
    }
}

/// A repository's pull requests, however the fetching of them went.
///
/// The same rows in the panel and in the top bar's switcher, reading the same
/// signal — so swapping through pull requests and picking one out of the picker
/// are two ways at one list rather than two lists.
#[component]
pub(super) fn PrListBody(current: Option<u64>) -> Element {
    let st = use_context::<St>();
    let held = st.prs.read().clone();
    let Some(list) = held else {
        return rsx! {};
    };
    let repo = list.repo.clone();
    let word = state_word(list.state);

    match &list.got {
        Got::Loading => rsx! {
            div { class: "ghnote", "Loading pull requests…" }
        },
        Got::Failed(e) => rsx! {
            div { class: "gherror", "{e}" }
            button {
                class: "linkbtn",
                // Root scope: the note this button is in is replaced by the
                // load it starts.
                onclick: move |_| {
                    spawn_forever(load_repo_prs(st, repo.clone()));
                },
                "Try again"
            }
        },
        Got::Ready(items) if items.is_empty() => rsx! {
            div { class: "ghnote", "No {word}pull requests in {repo}." }
        },
        Got::Ready(items) => rsx! {
            div { class: "prlist",
                for pr in items.iter() {
                    // The clone looks removable — clippy says so — but the key
                    // reads `pr` and whether that read lands before or after
                    // the move into props is the rsx macro's business, not
                    // ours. Seen to fail on another machine; keep the clone.
                    PrRow {
                        key: "{pr.number}",
                        repo: repo.clone(),
                        pr: pr.clone(),
                        current: current == Some(pr.number),
                    }
                }
            }
            // A page full is a page that may have had more behind it, and a
            // list quietly cut off reads as the whole of what there is.
            if items.len() >= PR_PAGE {
                div { class: "ghnote",
                    "The {PR_PAGE} most recently updated. Older ones are on github.com."
                }
            }
        },
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

/// How much of one account's repositories to hold. A page, the largest GitHub
/// gives — fetched once, so that typing inside an account narrows what is
/// already here rather than costing a request per keystroke.
const OWNER_PAGE: u32 = 100;

/// One question for the picker to answer: what is typed, and who is asking.
///
/// All three are read in the render pass and carried into the lookup rather
/// than read again once it gets there. A lookup runs after a debounce and a
/// round trip, and the box may well have moved on by then — what comes back
/// should be an answer to the question that was asked.
#[derive(Clone)]
struct Ask {
    query: String,
    token: String,
    /// The signed-in login, empty when signed out.
    viewer: String,
}

/// What the last lookup turned up, and what it was looking for.
///
/// The query travels with the results because the box runs ahead of them: it is
/// the only way the list can tell "nothing matches this" from "the answer for
/// what you have typed is still on its way".
#[derive(Clone, PartialEq)]
struct Suggestions {
    query: String,
    /// The account the query names, when it names one. Offered above the
    /// repositories, since "everything DioxusLabs owns" is a different answer
    /// from "repositories with dioxus in the name".
    owner: Option<OwnerHit>,
    /// Whose repositories these are, when the query is scoped to one account.
    scope: Option<String>,
    items: Vec<RepoHit>,
    error: Option<String>,
}

impl Suggestions {
    fn for_query(query: String) -> Self {
        Suggestions {
            query,
            owner: None,
            scope: None,
            items: Vec::new(),
            error: None,
        }
    }

    /// The list as it is drawn and as the arrow keys walk it — one sequence, so
    /// that what is highlighted and what Enter opens cannot disagree.
    fn rows(&self) -> Vec<Row> {
        self.owner
            .iter()
            .cloned()
            .map(Row::Owner)
            .chain(self.items.iter().cloned().map(Row::Repo))
            .collect()
    }
}

/// One row of the picker's list.
#[derive(Clone, PartialEq)]
enum Row {
    /// An account. Opens up what it owns rather than loading anything.
    Owner(OwnerHit),
    Repo(RepoHit),
}

/// The suggestions on offer, read from outside the render pass.
fn suggested(found: &Resource<Option<Suggestions>>) -> Vec<Row> {
    match &*found.peek() {
        Some(Some(s)) => s.rows(),
        _ => Vec::new(),
    }
}

/// The account a scoped query names, and whatever is typed after the slash.
///
/// `dioxuslabs/` is everything that account owns; `dioxuslabs/d` narrows it.
/// A second slash means a URL or a link to a pull request, and neither of those
/// is a filter — they go the ordinary way, through [`parse_target`].
fn owner_scope(q: &str) -> Option<(String, String)> {
    let (owner, rest) = q.split_once('/')?;
    if rest.contains('/') {
        return None;
    }
    Some((parse_owner(owner)?, rest.trim().to_string()))
}

/// The repositories of one account whose names contain `frag`.
fn narrow(all: &[RepoHit], frag: &str) -> Vec<RepoHit> {
    let needle = frag.to_lowercase();
    let mut hits: Vec<RepoHit> = all
        .iter()
        .filter(|h| h.repo.name.to_lowercase().contains(&needle))
        .cloned()
        .collect();
    // What starts with it first, what merely contains it after — and within
    // each, the order GitHub sent them in: most recently pushed.
    hits.sort_by_key(|h| !h.repo.name.to_lowercase().starts_with(&needle));
    hits.truncate(SUGGESTION_LIMIT as usize);
    hits
}

/// What to offer for one query, and what to spend finding it out.
///
/// Four answers, because there are four things a person types into this box:
/// nothing, which offers their own repositories; an account, which offers
/// everything it owns; `owner/…`, which narrows that list as you type; and
/// free text, which is a search of GitHub at large.
async fn suggest(
    ask: Ask,
    mut seen: Signal<HashMap<String, Suggestions>>,
    owned: Signal<HashMap<String, Vec<RepoHit>>>,
) -> Suggestions {
    let q = ask.query.clone();
    let scope = owner_scope(&q);

    // An answer already in hand: no wait, and no request.
    if let Some(done) = seen.peek().get(&q) {
        return done.clone();
    }
    // Nor is one needed to narrow a list already fetched, which is what makes
    // typing inside an account cost nothing at all. Narrowing to nothing falls
    // through to a search instead: a page of an account's repositories is not
    // always all of them.
    if let Some((owner, frag)) = &scope {
        let held = owned.peek().get(&owner.to_lowercase()).cloned();
        if let Some(all) = held {
            let items = narrow(&all, frag);
            if !items.is_empty() || frag.is_empty() {
                return Suggestions {
                    scope: Some(owner.clone()),
                    items,
                    ..Suggestions::for_query(q)
                };
            }
        }
    }
    // One letter is not a search, and neither is half of one. A scoped query is
    // exempt: `k/` names an account rather than guessing at one.
    if scope.is_none() && !q.is_empty() && q.chars().count() < MIN_QUERY {
        return Suggestions::for_query(q);
    }
    // Debounce: the next keystroke drops this task where it stands, so nothing
    // reaches GitHub until the typing stops.
    compat::sleep(SEARCH_DEBOUNCE).await;

    let out = match &scope {
        Some((owner, frag)) => scoped(&ask, owner, frag, owned).await,
        None if q.is_empty() => mine(&ask).await,
        None => searched(&ask).await,
    };
    // Errors are not answers. A budget that has since refilled should not go on
    // being reported for the rest of the session.
    if out.error.is_none() {
        seen.write().insert(q, out.clone());
    }
    out
}

/// Nothing typed: the signed-in account's own repositories.
async fn mine(ask: &Ask) -> Suggestions {
    let base = Suggestions::for_query(ask.query.clone());
    match github::my_repos(&ask.token, SUGGESTION_LIMIT).await {
        Ok(items) => Suggestions { items, ..base },
        Err(e) => Suggestions {
            error: Some(format!("{e:#}")),
            ..base
        },
    }
}

/// Free text: what GitHub's index ranks for it, and — when the text is shaped
/// like a login — the account of that name.
///
/// Both at once, so the account row arrives with the repositories rather than
/// after them. They are separate budgets as well as separate requests, which is
/// why one running out does not take the other's answer down with it.
async fn searched(ask: &Ask) -> Suggestions {
    let base = Suggestions::for_query(ask.query.clone());
    let account = async {
        match parse_owner(&ask.query) {
            Some(login) => github::lookup_owner(&ask.token, &login).await,
            None => None,
        }
    };
    let (owner, repos) = futures_util::future::join(
        account,
        github::search_repos(&ask.token, &ask.query, SUGGESTION_LIMIT),
    )
    .await;
    match repos {
        Ok(items) => Suggestions {
            owner,
            items,
            ..base
        },
        Err(e) => Suggestions {
            owner,
            error: Some(format!("{e:#}")),
            ..base
        },
    }
}

/// One account's repositories, narrowed by whatever follows the slash.
///
/// The list comes down once and every keystroke after that filters what is
/// already here — which is the point of scoping at all. `dioxuslabs/d` is a
/// question about one account, and the search index answers a different one.
async fn scoped(
    ask: &Ask,
    owner: &str,
    frag: &str,
    mut owned: Signal<HashMap<String, Vec<RepoHit>>>,
) -> Suggestions {
    let base = Suggestions::for_query(ask.query.clone());
    let all = match github::owner_repos(&ask.token, &ask.viewer, owner, OWNER_PAGE).await {
        Ok(all) => all,
        Err(e) => {
            return Suggestions {
                error: Some(format!("{e:#}")),
                ..base
            };
        }
    };
    owned.write().insert(owner.to_lowercase(), all.clone());

    let items = narrow(&all, frag);
    // Past the page, or renamed, or private to a token that cannot see it:
    // whatever the reason a name typed in full is not in the list, it is still
    // worth looking up directly — which is what a plain search does with it.
    if items.is_empty() && !frag.is_empty() {
        return searched(ask).await;
    }
    Suggestions {
        scope: Some(owner.to_string()),
        items,
        ..base
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

/// Step into an account: the box becomes `login/`, and the list under it
/// becomes that account's repositories.
///
/// It rewrites what is typed rather than keeping the account somewhere of its
/// own, so backspacing over the slash is how you leave again. Nothing to
/// explain, and nothing that can get out of step with the box.
fn drill(st: St, mut highlight: Signal<usize>, login: &str) {
    let mut input = st.repo_input;
    input.set(format!("{login}/"));
    highlight.set(0);
}

/// Enter, or Load: open whatever is typed.
///
/// Unless what is typed is an account, which has nothing to open — a person is
/// not a pull request. Stepping into it is what was meant, and it is what the
/// box does with `torvalds` typed in and nothing highlighted.
fn go(st: St, mut open: Signal<bool>, highlight: Signal<usize>) {
    let raw = st.repo_input.peek().clone();
    if parse_target(&raw).is_none()
        && let Some(login) = parse_owner(&raw)
    {
        open.set(true);
        return drill(st, highlight, &login);
    }
    open.set(false);
    // Root scope so closing the panel mid-load does not strand it on
    // "Loading…".
    spawn_forever(open_target(st));
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
/// entry. Three things can be searched for, because three things are how people
/// remember where code lives: the repository, the organisation that keeps it,
/// or the person who wrote it. An account found this way is a row like any
/// other, and opening it lists what it owns.
///
/// With nothing typed it offers the account's own repositories, since the pull
/// requests you are asked to review are nearly always on one; signed out there
/// is no such account, so it waits to be typed into instead.
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
    let seen = use_signal(HashMap::<String, Suggestions>::new);
    // And every account whose repositories have been listed, by login. Keyed
    // apart from the queries above because one fetch answers all of them:
    // `dioxuslabs/`, `dioxuslabs/d` and `dioxuslabs/dio` are one list, filtered
    // three ways.
    let owned = use_signal(HashMap::<String, Vec<RepoHit>>::new);

    let found = use_resource(move || {
        // Read the dependencies here, in the synchronous part: a change to
        // either cancels the lookup in flight and starts the next one.
        let showing = *open.read();
        let ask = Ask {
            query: st.repo_input.read().trim().to_string(),
            token: st.api_token(),
            viewer: st.viewer(),
        };
        async move {
            if !showing {
                return None;
            }
            Some(suggest(ask, seen, owned).await)
        }
    });

    let showing = *open.read();
    let current = found.cloned().flatten();
    let settled = current.as_ref().is_some_and(|s| s.query == typed.trim());
    let rows = current.as_ref().map(|s| s.rows()).unwrap_or_default();
    let scope = current.as_ref().and_then(|s| s.scope.clone());
    let error = current.as_ref().and_then(|s| s.error.clone());
    let hi = (*highlight.read()).min(rows.len().saturating_sub(1));

    rsx! {
        div { class: "ghsection",
            div { class: "ghlabel", "Repository, owner, pull request or commit" }
            div { class: "ghrow",
                input {
                    class: "ghinput",
                    r#type: "text",
                    placeholder: "search repos, orgs and users · owner/repo · or a link to a pull request or commit",
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
                        let rows = suggested(&found);
                        let at = (*highlight.peek()).min(rows.len().saturating_sub(1));
                        match e.key() {
                            Key::ArrowDown if !rows.is_empty() => {
                                e.prevent_default();
                                highlight.set((at + 1).min(rows.len() - 1));
                            }
                            Key::ArrowUp if !rows.is_empty() => {
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
                            Key::Enter => match rows.get(at) {
                                // What is highlighted in the list wins…
                                Some(Row::Repo(hit)) => choose(st, open, hit.repo.clone()),
                                Some(Row::Owner(o)) => drill(st, highlight, &o.login),
                                // …but a pasted link, or a name typed out in
                                // full, never needed the list.
                                None => go(st, open, highlight),
                            },
                            _ => {}
                        }
                    },
                }
                button {
                    class: "primarybtn",
                    onclick: move |_| go(st, open, highlight),
                    "Load"
                }
            }
            if showing {
                // Shown above the list rather than instead of it: a spent
                // search budget can arrive alongside an account that was found
                // on a different one, and dropping either would be a lie.
                if let Some(e) = error {
                    div { class: "gherror", "{e}" }
                }
                if !rows.is_empty() {
                    if let Some(owner) = scope.clone() {
                        div { class: "ghlabel", "Repositories in {owner}" }
                    }
                    div { class: "repolist",
                        for (i , row) in rows.iter().enumerate() {
                            match row {
                                Row::Owner(o) => rsx! {
                                    OwnerRow {
                                        key: "owner:{o.login}",
                                        owner: o.clone(),
                                        active: i == hi,
                                        index: i,
                                        highlight,
                                    }
                                },
                                Row::Repo(hit) => rsx! {
                                    RepoRow {
                                        key: "repo:{hit.repo}",
                                        hit: hit.clone(),
                                        active: i == hi,
                                        open,
                                        index: i,
                                        highlight,
                                    }
                                },
                            }
                        }
                    }
                } else if !settled {
                    div { class: "ghnote", "Searching GitHub…" }
                } else if let Some(owner) = scope {
                    div { class: "ghnote", "{owner} has no repositories to show." }
                } else if typed.trim().chars().count() < MIN_QUERY {
                    div { class: "ghnote",
                        "Type a repository, organisation or user name to search GitHub, \
                         or paste a link to a pull request or a commit."
                    }
                } else {
                    div { class: "ghnote", "Nothing on GitHub matches “{typed}”." }
                }
            }
        }
    }
}

/// An account: the row that opens up what it owns.
///
/// It sits above the repositories rather than among them because it is a
/// different kind of answer — a place to look rather than a thing to open — and
/// it reads as one: the login with the slash already on the end, which is
/// exactly what clicking it types.
#[component]
fn OwnerRow(owner: OwnerHit, active: bool, index: usize, highlight: Signal<usize>) -> Element {
    let st = use_context::<St>();
    let class = if active {
        "repoitem account on"
    } else {
        "repoitem account"
    };
    let login = owner.login.clone();
    let mut highlight = highlight;
    rsx! {
        div {
            class: "{class}",
            // Keeps the caret in the box. Stepping into an account only
            // rewrites what is typed, and the next thing anyone does is type
            // the rest of it.
            onmousedown: move |e| e.prevent_default(),
            onclick: move |_| drill(st, highlight, &login),
            onmouseenter: move |_| highlight.set(index),
            div { class: "repotop",
                span { class: "reponame", "{owner.login}/" }
                span { class: "repotag", if owner.org { "org" } else { "user" } }
                if owner.public_repos > 0 {
                    span { class: "repostars", "{owner.public_repos} public" }
                }
            }
            div { class: "repodesc", "{owner_blurb(&owner)}" }
        }
    }
}

/// The line under an account's login: who they are, and what the row does.
fn owner_blurb(owner: &OwnerHit) -> String {
    let what = if owner.org {
        "this organisation"
    } else {
        "this account"
    };
    if owner.name.is_empty() {
        format!("Browse the repositories {what} owns")
    } else {
        format!("{} — browse what {what} owns", owner.name)
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
fn PrRow(repo: RepoRef, pr: PrSummary, current: bool) -> Element {
    let st = use_context::<St>();
    let number = pr.number;
    let target = repo;
    let class = if current { "pritem on" } else { "pritem" };
    // The date alone. Which afternoon somebody last pushed to a branch is what
    // picks a pull request out of a list; the time of day is not.
    let day: String = pr.updated_at.chars().take(10).collect();
    rsx! {
        div {
            class: "{class}",
            title: if current { "Already open" } else { "Open #{number}" },
            // Root scope: opening one replaces the list this row is drawn in.
            onclick: move |_| {
                // Clicking the pull request you are reading is not a reload —
                // `⟳` is, and it is two inches away.
                if !current {
                    spawn_forever(open_pr(st, target.clone(), number));
                }
            },
            div { class: "prtop",
                span { class: "prnum", "#{pr.number}" }
                span { class: "prtitle", "{pr.title}" }
                if current {
                    span { class: "prhere", "reading" }
                }
                if pr.draft {
                    span { class: "prdraft", "draft" }
                }
                if pr.merged {
                    span { class: "prdraft merged", "merged" }
                } else if !pr.is_open() {
                    span { class: "prdraft closed", "closed" }
                }
            }
            div { class: "prmeta",
                "{pr.author} · {pr.head_ref} → {pr.base_ref}"
                if !day.is_empty() {
                    " · {day}"
                }
            }
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
    // What that account could see is not what the next caller can.
    let mut p = st.prs;
    p.set(None);
}

/// Load whatever the user typed: a repo lists its PRs, a PR link opens it, and
/// a link to a commit opens that commit's diff.
async fn open_target(st: St) {
    let raw = st.repo_input.peek().clone();
    // Before the repository: `owner/repo/commit/<sha>` names a repository as
    // well, and answering with that one would drop the half that was pasted.
    if let Some((repo, sha)) = parse_commit_target(&raw) {
        return open_commit(st, repo, sha, None).await;
    }
    let Some((repo, number)) = parse_target(&raw) else {
        let mut fetch = st.fetch;
        fetch.set(Fetch::Failed(
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

/// Show a repository's pull requests, in whichever state is toggled on.
///
/// It is the picker's answer to a repository being chosen, and it is also what
/// runs by itself whenever something opens — see the effect in
/// [`App`](super::app::App) — so that the switcher in the top bar always has a
/// list to switch through.
pub(super) async fn load_repo_prs(st: St, repo: RepoRef) {
    let token = st.api_token();
    let state = *st.pr_state.peek();
    let mut prs = st.prs;
    prs.set(Some(PrList {
        repo: repo.clone(),
        state,
        got: Got::Loading,
    }));

    let got = match github::list_prs(&token, &repo, state).await {
        Ok(items) => Got::Ready(items),
        Err(e) => Got::Failed(format!("{e:#}")),
    };
    // A repository opened, or the toggle flipped, while this was in flight: the
    // answer is to a question nobody is asking any more.
    let wanted = prs.peek().as_ref().is_some_and(|l| l.covers(&repo, state));
    if wanted {
        prs.set(Some(PrList { repo, state, got }));
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
    let mut fetch = st.fetch;

    fetch.set(Fetch::Working("Reading the repository…".to_string()));
    let head = match github::repo_head(&token, &repo).await {
        Ok(h) => h,
        Err(e) => return fetch.set(Fetch::Failed(format!("{e:#}"))),
    };

    fetch.set(Fetch::Working("Reading the file tree…".to_string()));
    // A pull request with no readable tree still has its changed files to show.
    // A repository has nothing at all, so this is where it stops — with the
    // list still on screen, rather than on an explorer that looks empty.
    let Some(tree) = tree_at(&token, &repo, &head.sha).await else {
        return fetch.set(Fetch::Failed(format!(
            "Could not read the file list for {repo}."
        )));
    };

    fetch.set(Fetch::Idle);
    st.enter_repo(RepoView {
        repo,
        branch: head.branch,
        head_sha: head.sha,
        tree,
    });
}

/// Open one commit: what it changed, diffed against the commit before it, with
/// the repository around it as of that commit.
///
/// The same three steps a pull request takes — metadata, then the tree at each
/// side — because from the explorer down it is the same thing: two commits and
/// a list of what differs between them.
///
/// `pr` is the pull request it was opened out of, when it was opened out of
/// one. It travels no further than the conversation pane, which goes on showing
/// that pull request's description and discussion while one of its commits is
/// being read.
pub(super) async fn open_commit(st: St, repo: RepoRef, sha: String, pr: Option<PrHeader>) {
    let token = st.api_token();
    let mut fetch = st.fetch;

    fetch.set(Fetch::Working("Loading commit…".to_string()));
    let mut view = match github::load_commit(&token, &repo, &sha).await {
        Ok(v) => v,
        Err(e) => return fetch.set(Fetch::Failed(format!("{e:#}"))),
    };
    view.pr = pr;

    fetch.set(Fetch::Working("Reading the file tree…".to_string()));
    // As for a pull request: a tree that will not load costs the explorer its
    // unchanged files, which is a smaller loss than refusing to open the diff.
    if let Some(tree) = tree_at(&token, &repo, &view.commit.sha).await {
        view.tree = tree;
    }
    // The parent's, which every left-hand side is read from. A root commit has
    // no parent and no left-hand side to read.
    if !view.parent_sha.is_empty()
        && let Some(base) = tree_at(&token, &repo, &view.parent_sha).await
    {
        view.base_tree = base;
    }

    fetch.set(Fetch::Idle);
    st.enter_commit(view);
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
    let mut fetch = st.fetch;

    fetch.set(Fetch::Working("Loading pull request…".to_string()));
    let mut detail = match github::load_pr(&token, &repo, number).await {
        Ok(d) => d,
        Err(e) => return fetch.set(Fetch::Failed(format!("{e:#}"))),
    };

    fetch.set(Fetch::Working("Reading the file tree…".to_string()));
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

    fetch.set(Fetch::Idle);
    st.enter_pr(detail);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(full: &str) -> RepoHit {
        let (owner, name) = full.split_once('/').unwrap();
        RepoHit {
            repo: RepoRef {
                owner: owner.to_string(),
                name: name.to_string(),
            },
            description: String::new(),
            private: false,
            fork: false,
            archived: false,
            stars: 0,
            pushed: String::new(),
        }
    }

    #[test]
    fn a_trailing_slash_scopes_the_query_to_one_account() {
        assert_eq!(
            owner_scope("DioxusLabs/"),
            Some(("DioxusLabs".to_string(), String::new()))
        );
        assert_eq!(
            owner_scope("DioxusLabs/dio"),
            Some(("DioxusLabs".to_string(), "dio".to_string()))
        );
    }

    #[test]
    fn a_link_is_not_a_filter() {
        // Two slashes: a URL, or a pull request. Both go the ordinary way.
        assert_eq!(owner_scope("https://github.com/rust-lang/rust"), None);
        assert_eq!(owner_scope("rust-lang/rust/pull/12345"), None);
        // And free text is not scoped at all.
        assert_eq!(owner_scope("diff viewer"), None);
    }

    #[test]
    fn narrowing_matches_anywhere_but_ranks_the_start_first() {
        let all = [hit("o/serde"), hit("o/dioxus-cli"), hit("o/dioxus")];
        let got = narrow(&all, "dioxus");
        let names: Vec<_> = got.iter().map(|h| h.repo.name.clone()).collect();
        assert_eq!(names, ["dioxus-cli", "dioxus"], "serde does not contain it");

        // Case is not something anyone types carefully into a filter.
        assert_eq!(narrow(&all, "DIOX").len(), 2);
        // A substring that starts nothing still matches.
        assert_eq!(narrow(&all, "-cli").len(), 1);
    }

    #[test]
    fn an_empty_filter_keeps_the_order_github_sent() {
        let all = [hit("o/newest"), hit("o/older"), hit("o/oldest")];
        let names: Vec<_> = narrow(&all, "")
            .iter()
            .map(|h| h.repo.name.clone())
            .collect();
        assert_eq!(names, ["newest", "older", "oldest"]);
    }

    #[test]
    fn narrowing_stops_at_what_the_list_can_show() {
        let all: Vec<RepoHit> = (0..40).map(|i| hit(&format!("o/thing{i}"))).collect();
        assert_eq!(narrow(&all, "thing").len(), SUGGESTION_LIMIT as usize);
    }

    #[test]
    fn the_rows_put_the_account_above_its_repositories() {
        let found = Suggestions {
            owner: Some(OwnerHit {
                login: "DioxusLabs".to_string(),
                org: true,
                name: String::new(),
                public_repos: 42,
            }),
            items: vec![hit("DioxusLabs/dioxus")],
            ..Suggestions::for_query("dioxuslabs".to_string())
        };
        assert!(matches!(
            found.rows().as_slice(),
            [Row::Owner(_), Row::Repo(_)]
        ));
    }
}
