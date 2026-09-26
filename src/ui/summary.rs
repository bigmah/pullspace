//! Summaries, given the middle pane: the page an agent wrote about a directory,
//! or about a pull request, read beside the code it describes.
//!
//! What a summary *is* — where they live, how one says whether it is still
//! true, what its links mean — is [`crate::backend::summary`]. This is the
//! pane: the way back up the tree, whether the page is current, what the pull
//! request did to it, and the page itself in a frame whose links come back out
//! to the app.
//!
//! On a pull request a summary is two pages, not one: the directory as it was
//! explained before the change and as it is explained after. That comparison
//! — how the account of the code moved, rather than which lines did — is the
//! point of keeping summaries in the repository at all, so a page the pull
//! request rewrote opens with both sides up.

use std::path::{Path, PathBuf};

use dioxus::prelude::*;

use crate::backend::FileContent;
use crate::backend::auth::open_browser;
use crate::backend::github::{self, RepoRef, Snapshot};
use crate::backend::route::Place;
use crate::backend::summary::{self, Against, Fresh, Link, Meta};
use crate::backend::tree::ChangeKind;

use super::app::{PrFileState, St, Workspace};
use super::prcache::ensure_path;
use super::tabs::TabStrip;

pub use crate::backend::summary::page_of;

// ------------------------------------------------------------- where it is

/// The repository as it stands, and as it stood under the change — the two
/// trees a summary is held up to. Borrowed: these run to thousands of entries
/// and are consulted on every render of the pane.
fn trees(ws: &Workspace) -> Option<(&Snapshot, Option<&Snapshot>)> {
    match ws {
        Workspace::Empty => None,
        Workspace::Pr(pr) => Some((&pr.tree, Some(&pr.base_tree))),
        Workspace::Repo(view) => Some((&view.tree, None)),
        Workspace::Commit(view) => Some((&view.tree, Some(&view.base_tree))),
        Workspace::Compare(view) => Some((&view.tree, Some(&view.base_tree))),
    }
}

/// Where to start: the pull request's own guide when it brings one, the root
/// of the map otherwise, and the first page there is when the root has none.
pub fn home(st: &St) -> Option<PathBuf> {
    let ws = st.workspace.peek();
    let (head, _) = trees(&ws)?;
    let statuses = st.statuses.peek();
    let guide = summary::change_pages(head)
        .into_iter()
        .find(|p| statuses.contains_key(p));
    if guide.is_some() {
        return guide;
    }
    let root = summary::page_of(Path::new(""));
    if head.entry(&root).is_some() {
        return Some(root);
    }
    summary::summarized(head)
        .first()
        .map(|d| summary::page_of(d))
}

/// Whether there is anything to open — what the explorer's button and the
/// welcome page ask before offering to.
pub fn available(ws: &Workspace) -> bool {
    trees(ws)
        .is_some_and(|(head, _)| summary::has_map(head) || !summary::change_pages(head).is_empty())
}

/// Every directory with a page, for the explorer to mark.
pub fn summarized_dirs(ws: &Workspace) -> std::collections::HashSet<PathBuf> {
    trees(ws)
        .map(|(head, _)| summary::summarized(head).into_iter().collect())
        .unwrap_or_default()
}

/// One page further down the summary tree.
#[derive(Clone, PartialEq)]
struct Child {
    dir: PathBuf,
    /// How it reads from the page it is listed on: `backend`, or
    /// `src/backend` from a root that has no page for `src`.
    name: String,
    /// Whether the pull request touches anything under it.
    touched: bool,
}

/// Everything about the page on show that comes from the trees rather than
/// from the page.
#[derive(Clone, PartialEq)]
struct Situation {
    page: PathBuf,
    /// The directory described — `None` for a pull request's guide.
    dir: Option<PathBuf>,
    crumbs: Vec<(String, PathBuf, bool)>,
    children: Vec<Child>,
    /// What the change being read did to the page itself.
    status: Option<ChangeKind>,
    /// How many files under the directory the change touches, summaries aside.
    touched: usize,
    /// Guides the change brings, for the chip that goes to them.
    guides: Vec<PathBuf>,
    repo: Option<RepoRef>,
}

fn place_of(st: &St, page: &Path) -> Situation {
    let ws = st.workspace.read();
    let statuses = st.statuses.read();
    let dir = summary::dir_of(page);
    let under = |d: &Path, p: &Path| p.starts_with(d) && !p.starts_with(summary::ROOT);
    let (crumbs, children, guides) = match trees(&ws) {
        Some((head, _)) => {
            let crumbs = dir
                .as_deref()
                .map(|d| summary::crumbs(head, d))
                .unwrap_or_default();
            let children = dir
                .as_deref()
                .map(|d| {
                    summary::children(head, d)
                        .into_iter()
                        .map(|c| Child {
                            name: c.strip_prefix(d).unwrap_or(&c).display().to_string(),
                            touched: statuses.keys().any(|p| under(&c, p)),
                            dir: c,
                        })
                        .collect()
                })
                .unwrap_or_default();
            let guides = summary::change_pages(head)
                .into_iter()
                .filter(|p| statuses.contains_key(p))
                .collect();
            (crumbs, children, guides)
        }
        None => Default::default(),
    };
    Situation {
        page: page.to_path_buf(),
        touched: dir
            .as_deref()
            .map_or(0, |d| statuses.keys().filter(|p| under(d, p)).count()),
        status: statuses.get(page).copied(),
        dir,
        crumbs,
        children,
        guides,
        repo: ws.repo_ref().cloned(),
    }
}

// ------------------------------------------------------------- the frame

/// Two things the frame is handed per page load and nothing else knows: the
/// nonce that lets the link bridge run, and the token it signs its messages
/// with. Neither is secret from the page — the page cannot run a script to
/// read them — so all either has to be is unguessable by whoever wrote it.
fn secrets() -> &'static (String, String) {
    use std::sync::OnceLock;
    static SECRETS: OnceLock<(String, String)> = OnceLock::new();
    SECRETS.get_or_init(|| (random_hex(), random_hex()))
}

fn random_hex() -> String {
    #[cfg(target_arch = "wasm32")]
    {
        (0..4)
            .map(|_| format!("{:08x}", (js_sys::Math::random() * 4_294_967_296.0) as u32))
            .collect()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        "0123456789abcdef0123456789abcdef".to_string()
    }
}

/// Listen for a summary's links, for as long as the app is up.
///
/// Only from the frames this pane draws, and only signed: any page can post a
/// message to this one, and a link followed here opens things.
const LISTEN_JS: &str = r#"
(function () {
  if (window.__pullspace_summary) window.__pullspace_summary();
  var on = function (e) {
    var d = e.data;
    if (!d || d.pullspace !== TOKEN || typeof d.href !== 'string') return;
    var frames = document.querySelectorAll('iframe.summaryframe');
    for (var i = 0; i < frames.length; i++) {
      if (frames[i].contentWindow === e.source) { dioxus.send(d.href); return; }
    }
  };
  window.addEventListener('message', on);
  window.__pullspace_summary = function () { window.removeEventListener('message', on); };
})();
"#;

pub async fn links(st: St) {
    let (_, token) = secrets();
    let mut eval = document::eval(&LISTEN_JS.replace("TOKEN", &format!("'{token}'")));
    while let Ok(href) = eval.recv::<String>().await {
        follow(st, &href);
    }
}

/// Go where a summary's link points.
///
/// A directory is its summary when it has one, which is what makes a map a
/// map: the page for `src` links to `/src/backend/` and that is a click down
/// the tree. A file opens in the viewer, at the line when the link names one.
fn follow(st: St, href: &str) {
    let page = st.summary.peek().clone().unwrap_or_default();
    let base = summary::dir_of(&page).unwrap_or_default();
    match summary::resolve(&base, href) {
        Link::Web(url) => open_browser(&url),
        Link::Nowhere => {}
        Link::Path { path, line } => {
            let dir_page = summary::page_of(&path);
            let is_dir = path.as_os_str().is_empty() || st.has_dir(&path);
            if is_dir && st.has_file(&dir_page) {
                st.open_summary(dir_page);
            } else if summary::is_page(&path) && st.has_file(&path) {
                st.open_summary(path);
            } else if st.has_file(&path) || is_dir {
                st.open_place(Place { path, line });
            }
        }
    }
}

/// One side of a page, as the frame gets it — or why there is nothing to get.
#[derive(Clone, PartialEq)]
enum Side {
    Page { meta: Meta, doc: String },
    Missing,
    Binary,
}

/// `other` is the page's other side when the change rewrote it, so each side
/// can light what the other does not say.
fn side_of(content: &FileContent, theme: &str, other: Option<(&FileContent, Against)>) -> Side {
    match content {
        FileContent::Text(html) => {
            let (nonce, token) = secrets();
            let compare = other.and_then(|(o, side)| Some((o.text()?, side)));
            Side::Page {
                meta: summary::meta(html),
                doc: summary::frame_doc(html, theme, nonce, token, compare),
            }
        }
        FileContent::Absent => Side::Missing,
        FileContent::Binary => Side::Binary,
    }
}

#[derive(Clone, PartialEq)]
enum Pages {
    Loading,
    Failed(String),
    Ready { base: Side, head: Side },
}

/// Which side of a page the pull request rewrote is up.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Showing {
    Before,
    After,
    Both,
}

/// What came of asking which files moved since a page was written.
#[derive(Clone, PartialEq)]
enum Drift {
    Idle,
    Asking,
    Found(Vec<(PathBuf, ChangeKind)>),
    Failed(String),
}

// ------------------------------------------------------------- the pane

#[component]
pub fn SummaryPane() -> Element {
    let st = use_context::<St>();
    let mut showing = use_signal(|| Showing::Both);
    // Keyed by the page and the stamp it was asked about, so an answer never
    // outlives the question: another page, or a newer version of this one, is
    // a different question.
    let mut drift: Signal<(PathBuf, String, Drift)> =
        use_signal(|| (PathBuf::new(), String::new(), Drift::Idle));

    // Read it, both sides, the way the viewer reads any file.
    use_effect(move || {
        if let Some(page) = st.summary.read().clone() {
            ensure_path(st, &page);
        }
    });

    let theme = use_memo(move || st.prefs.read().css());
    let pages = use_memo(move || {
        let Some(page) = st.summary.read().clone() else {
            return Pages::Loading;
        };
        let theme = theme.read();
        match st.pr_files.read().get(&page) {
            None | Some(PrFileState::Loading) => Pages::Loading,
            Some(PrFileState::Failed(e)) => Pages::Failed(e.clone()),
            Some(PrFileState::Ready { base, head }) => {
                // Only a page the change rewrote has two sides to hold up
                // against each other; an untouched one reads its base as
                // absent.
                let both = base.text().is_some() && head.text().is_some();
                Pages::Ready {
                    base: side_of(base, &theme, both.then_some((&**head, Against::Before))),
                    head: side_of(head, &theme, both.then_some((&**base, Against::After))),
                }
            }
        }
    });
    let place = use_memo(move || {
        let page = st.summary.read().clone().unwrap_or_default();
        place_of(&st, &page)
    });

    let place = place.read().clone();
    let pages = pages.read().clone();
    let ws = st.workspace.read();
    let (head_tree, base_tree) = match trees(&ws) {
        Some((h, b)) => (Some(h), b),
        None => (None, None),
    };

    let guide = place.dir.is_none();
    let rewritten = matches!(
        place.status,
        Some(ChangeKind::Modified | ChangeKind::Renamed)
    );
    let (base, head) = match &pages {
        Pages::Ready { base, head } => (Some(base.clone()), Some(head.clone())),
        _ => (None, None),
    };
    let showing_now = if rewritten {
        *showing.read()
    } else {
        Showing::After
    };

    // How each side stands against the tree it belongs to.
    let fresh_of = |side: &Option<Side>, tree: Option<&Snapshot>| -> Option<Fresh> {
        let (Some(Side::Page { meta, .. }), Some(tree), Some(dir)) = (side, tree, &place.dir)
        else {
            return None;
        };
        Some(summary::freshness(meta, tree, dir))
    };
    let head_fresh = fresh_of(&head, head_tree);
    let stamp = match &head {
        Some(Side::Page { meta, .. }) => meta.tree.clone(),
        _ => None,
    };
    let written = match &head {
        Some(Side::Page { meta, .. }) => meta.written.clone(),
        _ => None,
    };
    let in_change = base_tree.is_some();
    drop(ws);

    // The directory moved on and the page did not: what exactly moved, read
    // from the tree the page was stamped with. Asked for, not fetched on
    // sight — it is a request against a budget of sixty an hour signed out.
    let ask_drift = {
        let place = place.clone();
        let stamp = stamp.clone();
        move |_| {
            let (Some(dir), Some(stamp), Some(repo)) =
                (place.dir.clone(), stamp.clone(), place.repo.clone())
            else {
                return;
            };
            let page = place.page.clone();
            drift.set((page.clone(), stamp.clone(), Drift::Asking));
            let token = st.api_token();
            spawn(async move {
                let answer = match github::repo_tree(&token, &repo, &stamp).await {
                    Ok(then) => {
                        let ws = st.workspace.peek();
                        match trees(&ws) {
                            Some((head, _)) => {
                                Drift::Found(summary::drift(&then.files, head, &dir))
                            }
                            None => Drift::Idle,
                        }
                    }
                    Err(_) => Drift::Failed(
                        "The tree this page was written against is not on GitHub — \
                         it was stamped before its commit was pushed."
                            .to_string(),
                    ),
                };
                if drift.peek().0 == page {
                    drift.set((page, stamp, answer));
                }
            });
        }
    };
    let drift_now = {
        let held = drift.read();
        match (
            held.0 == place.page,
            stamp.as_deref() == Some(held.1.as_str()),
        ) {
            (true, true) => held.2.clone(),
            _ => Drift::Idle,
        }
    };

    let kind = if guide { "Review guide" } else { "Summary" };
    let source_page = place.page.clone();
    let pull_touches_unrewritten =
        in_change && !guide && place.status.is_none() && place.touched > 0;

    let body = match &pages {
        Pages::Loading => rsx! { div { class: "notice", "Loading…" } },
        Pages::Failed(e) => rsx! { div { class: "notice error", "{e}" } },
        Pages::Ready { .. } => {
            let (show_base, show_head) = match showing_now {
                Showing::Before => (true, false),
                Showing::After => (false, true),
                Showing::Both => (true, true),
            };
            rsx! {
                div { class: "sumframes",
                    if show_base {
                        {frame(base.as_ref(), if show_head { Some("Before") } else { None }, "base")}
                    }
                    if show_head {
                        {frame(head.as_ref(), if show_base { Some("After") } else { None }, "head")}
                    }
                }
            }
        }
    };

    rsx! {
        div { class: "viewer",
            TabStrip {}
            div { class: "viewhdr sumhdr",
                span { class: "sumkind", "{kind}" }
                if guide {
                    span { class: "vpath", title: "{place.page.display()}",
                        "{place.page.file_stem().unwrap_or_default().to_string_lossy()}"
                    }
                } else {
                    nav { class: "crumbs",
                        for (i, (name, dir, has)) in place.crumbs.iter().cloned().enumerate() {
                            if i > 0 {
                                span { class: "crumbsep", "/" }
                            }
                            {
                                let last = i + 1 == place.crumbs.len();
                                let cls = match (last, has) {
                                    (true, _) => "crumb here",
                                    (false, true) => "crumb",
                                    (false, false) => "crumb bare",
                                };
                                rsx! {
                                    button {
                                        key: "{i}",
                                        class: cls,
                                        disabled: last || !has,
                                        title: if has { "Open this directory's summary" } else { "No summary for this directory" },
                                        onclick: move |_| st.open_summary(summary::page_of(&dir)),
                                        "{name}"
                                    }
                                }
                            }
                        }
                    }
                }
                match place.status {
                    Some(ChangeKind::Added) => rsx! { span { class: "sumpill added", title: "This pull request adds this page", "new in this PR" } },
                    Some(ChangeKind::Modified | ChangeKind::Renamed) => rsx! { span { class: "sumpill changed", title: "This pull request rewrites this page", "rewritten in this PR" } },
                    Some(ChangeKind::Deleted) => rsx! { span { class: "sumpill removed", title: "This pull request deletes this page", "removed in this PR" } },
                    None => rsx! {},
                }
                match head_fresh {
                    Some(Fresh::Current) => rsx! {
                        span {
                            class: "sumpill current",
                            title: "Written against this directory exactly as it is{written_note(&written)}",
                            "current"
                        }
                    },
                    Some(Fresh::Stale) => rsx! {
                        span {
                            class: "sumpill stale",
                            title: "Something under this directory has changed since this page was written{written_note(&written)}",
                            "out of date"
                        }
                    },
                    Some(Fresh::Unknown) => rsx! {
                        span {
                            class: "sumpill unknown",
                            title: "This page has no pullspace:tree stamp, or GitHub did not give the directory's hash",
                            "unstamped"
                        }
                    },
                    None => rsx! {},
                }
                span { class: "spacer" }
                if rewritten {
                    div { class: "modegroup",
                        for (label, m) in [("Before", Showing::Before), ("Both", Showing::Both), ("After", Showing::After)] {
                            button {
                                key: "{label}",
                                class: if showing_now == m { "modebtn on" } else { "modebtn" },
                                onclick: move |_| showing.set(m),
                                "{label}"
                            }
                        }
                    }
                }
                button {
                    class: "modebtn sumsrc",
                    title: "Open the page's HTML in the viewer — as a diff, when this pull request changes it",
                    onclick: move |_| st.open_file(source_page.clone()),
                    "Source"
                }
            }
            if !place.guides.is_empty() && !guide || !place.children.is_empty() || pull_touches_unrewritten || matches!(head_fresh, Some(Fresh::Stale)) {
                div { class: "sumstrip",
                    if !guide {
                        for g in place.guides.iter().cloned() {
                            button {
                                key: "{g.display()}",
                                class: "sumchip guide",
                                title: "This pull request's guide to itself",
                                // Its own clone: the key and the label read `g`
                                // too, and in a release build the closure can be
                                // built first (see the `Peek` row in prboard).
                                onclick: {
                                    let g = g.clone();
                                    move |_| st.open_summary(g.clone())
                                },
                                "Review guide: {g.file_stem().unwrap_or_default().to_string_lossy()}"
                            }
                        }
                    }
                    if pull_touches_unrewritten {
                        span {
                            class: "sumnote warn",
                            title: "The summary may no longer be true — the author's agent did not rewrite it",
                            "This PR changes {place.touched} file{plural(place.touched)} under here and leaves this page as it was"
                        }
                    }
                    if matches!(head_fresh, Some(Fresh::Stale)) && !pull_touches_unrewritten {
                        {drift_row(&drift_now, place.dir.as_deref().is_some_and(|d| !d.as_os_str().is_empty()), ask_drift, st)}
                    }
                    if !place.children.is_empty() {
                        span { class: "sumlabel", "Inside" }
                        for c in place.children.iter().cloned() {
                            button {
                                key: "{c.dir.display()}",
                                class: if c.touched { "sumchip touched" } else { "sumchip" },
                                title: if c.touched { "Summary — this pull request changes files under here" } else { "Summary" },
                                onclick: {
                                    let dir = c.dir.clone();
                                    move |_| st.open_summary(summary::page_of(&dir))
                                },
                                "{c.name}"
                                if c.touched {
                                    span { class: "sumdot" }
                                }
                            }
                        }
                    }
                }
            }
            {body}
        }
    }
}

fn written_note(written: &Option<String>) -> String {
    written
        .as_deref()
        .map(|w| format!(" — written {w}"))
        .unwrap_or_default()
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// What has moved under a stale page, or the button that asks.
fn drift_row(
    drift: &Drift,
    can_ask: bool,
    ask: impl FnMut(MouseEvent) + 'static,
    st: St,
) -> Element {
    match drift {
        Drift::Idle if can_ask => rsx! {
            button {
                class: "sumchip ask",
                title: "Read the tree this page was written against, and list what has changed since",
                onclick: ask,
                "What changed since it was written?"
            }
        },
        Drift::Idle => rsx! {
            span { class: "sumnote", "Something in the repository has changed since this page was written" }
        },
        Drift::Asking => {
            rsx! { span { class: "sumnote", "Reading the tree it was written against…" } }
        }
        Drift::Failed(e) => rsx! { span { class: "sumnote", "{e}" } },
        Drift::Found(moved) if moved.is_empty() => rsx! {
            span { class: "sumnote", "Only summaries have changed under here since it was written" }
        },
        Drift::Found(moved) => rsx! {
            span { class: "sumlabel", "Changed since" }
            for (path, kind) in moved.iter().take(40).cloned() {
                button {
                    key: "{path.display()}",
                    class: "sumchip file",
                    title: "{path.display()}",
                    onclick: {
                        let path = path.clone();
                        move |_| st.open_file(path.clone())
                    },
                    span { class: "badge {kind.css()}", "{kind.badge()}" }
                    "{path.file_name().unwrap_or_default().to_string_lossy()}"
                }
            }
            if moved.len() > 40 {
                span { class: "sumnote", "+{moved.len() - 40} more" }
            }
        },
    }
}

/// One side of a page, in its frame.
///
/// `allow-scripts` and nothing else. Without `allow-same-origin` the frame is
/// an origin of its own that no other page shares, so a script in it could
/// read nothing of this one — and the policy it is handed runs no script but
/// the bridge. See [`summary::frame_doc`].
fn frame(side: Option<&Side>, label: Option<&str>, which: &str) -> Element {
    let inner = match side {
        Some(Side::Page { doc, .. }) => rsx! {
            iframe {
                key: "{which}",
                class: "summaryframe",
                "sandbox": "allow-scripts",
                srcdoc: "{doc}",
            }
        },
        Some(Side::Missing) => rsx! {
            div { class: "notice",
                if which == "base" { "No page here before this change." } else { "This change removes the page." }
            }
        },
        Some(Side::Binary) => {
            rsx! { div { class: "notice", "Not a page — this file is not text." } }
        }
        None => rsx! {},
    };
    rsx! {
        div { class: "sumside {which}",
            if let Some(label) = label {
                div { class: "sumsidehdr", "{label}" }
            }
            {inner}
        }
    }
}
