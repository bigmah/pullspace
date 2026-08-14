//! The conversation pane: a pull request's description, the discussion on it,
//! and the comments left on lines of the diff.
//!
//! Bodies are markdown, and they are drawn as the markdown they are — through
//! the same renderer the README goes through, which builds elements of this app
//! out of parsed text and never hands the page a line of somebody else's
//! markup. See [`crate::backend::markdown`] for why that distinction is the
//! whole design: a review is read next to a GitHub token in local storage.

use std::path::Path;

use dioxus::prelude::*;

use crate::backend::auth::open_browser;
use crate::backend::github::{self, Comment, CommentKind, CommitSummary, RepoRef};
use crate::backend::markdown;

use super::app::{CommitList, ConvTab, Conversation, St};
use super::panes::{Edge, Splitter};

/// A comment's links are written from the root of the repository — there is no
/// file they are relative to, the way a README's are.
const ROOT: &str = "";

/// Fetch the conversation for a pull request, unless it has been closed or
/// swapped out from under us in the meantime.
pub(super) async fn load(st: St, repo: RepoRef, number: u64) {
    let mut conv = st.conv;
    conv.set(Conversation::Loading);
    let token = st.api_token();
    let got = github::pr_comments(&token, &repo, number).await;

    let still_open = st
        .workspace
        .peek()
        .pr()
        .is_some_and(|pr| pr.repo == repo && pr.number == number);
    if !still_open {
        return;
    }
    conv.set(match got {
        Ok(thread) => Conversation::Ready(Box::new(thread)),
        Err(e) => Conversation::Failed(format!("{e:#}")),
    });
}

/// Fetch the commits of a pull request. Asked for rather than fetched with the
/// rest of it — see [`CommitList`].
pub(super) async fn load_commits(st: St, repo: RepoRef, number: u64) {
    let mut commits = st.commits;
    commits.set(CommitList::Loading);
    let token = st.api_token();
    let got = github::pr_commits(&token, &repo, number).await;

    let still_open = st
        .workspace
        .peek()
        .pr()
        .is_some_and(|pr| pr.repo == repo && pr.number == number);
    if !still_open {
        return;
    }
    commits.set(match got {
        Ok(commits) => CommitList::Ready(Box::new(commits)),
        Err(e) => CommitList::Failed(format!("{e:#}")),
    });
}

/// The date alone. The time of day is not what anyone is reading a comment
/// list for, and it costs the width of the author's name.
fn day_of(timestamp: &str) -> String {
    timestamp.chars().take(10).collect()
}

fn kind_label(c: &Comment) -> &str {
    match c.kind {
        CommentKind::Discussion => "commented",
        CommentKind::Inline => "line note",
        CommentKind::Review if c.verdict.is_empty() => "reviewed",
        CommentKind::Review => &c.verdict,
    }
}

/// Approval and rejection are the two things worth spotting from across the
/// pane; everything else is just a label.
fn kind_class(c: &Comment) -> &'static str {
    match c.verdict.as_str() {
        "approved" => "convkind ok",
        "changes requested" => "convkind bad",
        _ => "convkind",
    }
}

#[component]
pub fn ConvPane() -> Element {
    let st = use_context::<St>();
    // A repository browsed on its own has no conversation to show, and the
    // local working tree certainly does not.
    //
    // Only the handful of fields the pane draws leave the guard — the whole
    // `PrDetail` carries two tree snapshots, which is not something to clone
    // on every fold and unfold of the pane.
    let held = st.workspace.read();
    let Some(pr) = held.pr() else {
        return rsx! {};
    };
    let desc = Desc {
        author: pr.author.clone(),
        number: pr.number,
        draft: pr.draft,
        title: pr.title.clone(),
        body: pr.body.trim().to_string(),
        html_url: pr.html_url.clone(),
    };
    let repo = pr.repo.clone();
    let number = pr.number;
    drop(held);
    let mut open = st.conv_open;
    let mut tab = st.conv_tab;
    let showing = *tab.read();
    let conv = st.conv.read();

    let count = match &*conv {
        Conversation::Ready(thread) => thread.comments.len(),
        _ => 0,
    };
    let commits = match &*st.commits.read() {
        CommitList::Ready(commits) => Some(commits.items.len()),
        _ => None,
    };

    // Folded away, the rail says what is behind it — which is whichever of the
    // two was last being read, not always the conversation.
    let (rail_label, rail_count, rail_why) = match showing {
        ConvTab::Talk => ("CONVERSATION", count, "Show the pull request conversation"),
        ConvTab::Commits => (
            "COMMITS",
            commits.unwrap_or_default(),
            "Show the commits on this pull request",
        ),
    };

    if !*open.read() {
        return rsx! {
            div {
                class: "convrail",
                title: "{rail_why}",
                onclick: move |_| open.set(true),
                span { class: "convrail-chev", "‹" }
                div { class: "convrail-label",
                    "{rail_label}"
                    if rail_count > 0 {
                        span { class: "convrail-count", "{rail_count}" }
                    }
                }
            }
        };
    }

    let reloading = match showing {
        ConvTab::Talk => matches!(&*conv, Conversation::Loading),
        ConvTab::Commits => matches!(&*st.commits.read(), CommitList::Loading),
    };

    let talk = match &*conv {
        Conversation::Loading => rsx! {
            div { class: "panel-empty", "Loading the conversation…" }
        },
        Conversation::Failed(e) => rsx! {
            div { class: "gherror", "{e}" }
        },
        Conversation::Ready(thread) if thread.comments.is_empty() => rsx! {
            div { class: "panel-empty", "No comments yet." }
        },
        Conversation::Ready(thread) => rsx! {
            for (i , c) in thread.comments.iter().enumerate() {
                CommentRow { key: "{i}", c: c.clone() }
            }
            if thread.truncated {
                div { class: "panel-empty",
                    "This conversation is longer than pullspace loads — open it on github.com to read the rest."
                }
            }
        },
    };

    let reload_cls = if reloading { "iconbtn spin" } else { "iconbtn" };
    let reload_why = match showing {
        ConvTab::Talk => "Reload the conversation from GitHub",
        ConvTab::Commits => "Reload the commits from GitHub",
    };

    rsx! {
        Splitter { edge: Edge::Conv }
        div { class: "convpane",
            div { class: "conv-hdr",
                // Two things are worth knowing about a pull request besides its
                // diff: what people said about it, and what it is made of.
                PaneTab {
                    label: "CONVERSATION",
                    on: showing == ConvTab::Talk,
                    count: (count > 0).then_some(count),
                    why: "The description, the discussion and the notes left on lines of the diff",
                    onpick: move |_| tab.set(ConvTab::Talk),
                }
                PaneTab {
                    label: "COMMITS",
                    on: showing == ConvTab::Commits,
                    count: commits,
                    why: "Every commit on this pull request, oldest first",
                    onpick: move |_| tab.set(ConvTab::Commits),
                }
                span { class: "spacer" }
                button {
                    class: reload_cls,
                    title: "{reload_why}",
                    disabled: reloading,
                    // Root scope: this button is re-rendered by the load it starts.
                    onclick: move |_| match showing {
                        ConvTab::Talk => {
                            spawn_forever(load(st, repo.clone(), number));
                        }
                        ConvTab::Commits => {
                            spawn_forever(load_commits(st, repo.clone(), number));
                        }
                    },
                    span { class: "glyph", "⟳" }
                }
                button {
                    class: "iconbtn",
                    title: "Hide this pane",
                    onclick: move |_| open.set(false),
                    "›"
                }
            }
            div { class: "conv-body",
                match showing {
                    ConvTab::Talk => rsx! {
                        Description { desc }
                        {talk}
                    },
                    ConvTab::Commits => rsx! { CommitsBody {} },
                }
            }
        }
    }
}

/// One of the pane's two headings, as the button that turns it on.
#[component]
fn PaneTab(
    label: &'static str,
    on: bool,
    /// Shown beside the label once it is known. `None` while it is not — an
    /// unread list is not a list of none.
    count: Option<usize>,
    why: &'static str,
    onpick: EventHandler<()>,
) -> Element {
    let class = if on { "convtab on" } else { "convtab" };
    rsx! {
        button {
            class: "{class}",
            title: "{why}",
            onclick: move |_| onpick.call(()),
            span { class: "side-title", "{label}" }
            if let Some(count) = count {
                span { class: "convcount", "{count}" }
            }
        }
    }
}

/// What the pull request is made of: its commits, oldest first, the way they
/// were written.
#[component]
fn CommitsBody() -> Element {
    let st = use_context::<St>();
    let held = st.commits.read();

    match &*held {
        // Idle only ever lasts as long as it takes the effect in `App` to see
        // that this tab is up — see the comment there.
        CommitList::Idle | CommitList::Loading => rsx! {
            div { class: "panel-empty", "Loading the commits…" }
        },
        CommitList::Failed(e) => rsx! {
            div { class: "gherror", "{e}" }
        },
        CommitList::Ready(commits) if commits.items.is_empty() => rsx! {
            div { class: "panel-empty", "No commits on this pull request." }
        },
        CommitList::Ready(commits) => rsx! {
            for c in commits.items.iter() {
                CommitRow { key: "{c.sha}", c: c.clone() }
            }
            if commits.truncated {
                div { class: "panel-empty",
                    "GitHub lists at most 250 commits on a pull request — open it on github.com to read the rest."
                }
            }
        },
    }
}

#[component]
fn CommitRow(c: CommitSummary) -> Element {
    // A message with more than a subject line to it opens on a click. Most have
    // nothing under the fold, and a row that unfolds to nothing is a row that
    // should not have looked as though it would.
    let mut unfolded = use_signal(|| false);
    let body = c.body().to_string();
    let more = !body.is_empty();
    let showing = more && *unfolded.read();
    let day = day_of(&c.date);
    let url = c.html_url.clone();
    let has_link = !url.is_empty();
    let class = if more {
        "convitem convcommit more"
    } else {
        "convitem convcommit"
    };

    rsx! {
        div {
            class: "{class}",
            onclick: move |_| {
                if more {
                    let now = *unfolded.peek();
                    unfolded.set(!now);
                }
            },
            div { class: "convmeta",
                span { class: "convwho", "{c.author}" }
                span {
                    class: "convsha",
                    title: "{c.sha}",
                    "{c.short()}"
                }
                if !day.is_empty() {
                    span { class: "convdate", "{day}" }
                }
                span { class: "spacer" }
                if has_link {
                    button {
                        class: "iconbtn sm",
                        title: "Open this commit on github.com",
                        onclick: move |e| {
                            // The row underneath this button folds; the button
                            // does not fold anything.
                            e.stop_propagation();
                            open_browser(&url);
                        },
                        "↗"
                    }
                }
            }
            div { class: "convtitle",
                "{c.subject()}"
                if more {
                    span { class: "convmore", if showing { " ⌃" } else { " ⌄" } }
                }
            }
            if showing {
                div { class: "convcommitbody", "{body}" }
            }
        }
    }
}

/// What the description card needs of a pull request, and nothing else —
/// small enough that the prop comparison a re-render starts with is a few
/// string compares rather than a walk of two tree snapshots.
#[derive(Clone, PartialEq)]
struct Desc {
    author: String,
    number: u64,
    draft: bool,
    title: String,
    /// Already trimmed.
    body: String,
    html_url: String,
}

/// A body, drawn — and the note that says what drawing it had to leave out.
///
/// Parsed here rather than memoised: a comment is a few hundred bytes, and the
/// row it is in only re-renders when the comment itself changes.
#[component]
fn Body(text: String) -> Element {
    let st = use_context::<St>();
    let doc = markdown::parse(&text);
    rsx! {
        if doc.raw_html {
            HtmlNote {}
        }
        {super::markdown::render_body(st, Path::new(ROOT), &doc)}
    }
}

/// Said once, in one place: raw HTML in a comment is not drawn, and a reader
/// looking at a gap where a table or a `<details>` should be is owed the reason.
#[component]
fn HtmlNote() -> Element {
    rsx! {
        span {
            class: "convkind convhtml",
            title: "This comment contains HTML — a collapsed section, a table, an image. It is not drawn here; open the comment on github.com to read it.",
            "html not drawn"
        }
    }
}

#[component]
fn Description(desc: Desc) -> Element {
    let url = desc.html_url.clone();
    rsx! {
        div { class: "convitem convdesc",
            div { class: "convmeta",
                span { class: "convwho", "{desc.author}" }
                span { class: "convkind", "opened #{desc.number}" }
                if desc.draft {
                    span { class: "prdraft", "draft" }
                }
                span { class: "spacer" }
                button {
                    class: "iconbtn",
                    title: "Open on github.com",
                    onclick: move |_| open_browser(&url),
                    "↗"
                }
            }
            div { class: "convtitle", "{desc.title}" }
            if desc.body.is_empty() {
                div { class: "convnone", "No description." }
            } else {
                Body { text: desc.body.clone() }
            }
        }
    }
}

#[component]
fn CommentRow(c: Comment) -> Element {
    let st = use_context::<St>();
    // A line comment names a file, and that file is in the explorer — so the
    // location is the way from the comment to the code it is about.
    let target = c.path.clone().map(|p| (p, c.line.unwrap_or(1)));
    let loc = target.as_ref().map(|(p, line)| match c.line {
        Some(_) => format!("{}:{line}", p.display()),
        // A comment whose lines no longer exist in the head commit; the file
        // is still the right place to land.
        None => p.display().to_string(),
    });
    let day = day_of(&c.created_at);
    let url = c.html_url.clone();
    let has_link = !url.is_empty();
    let kind_cls = kind_class(&c);
    let label = kind_label(&c).to_string();

    rsx! {
        div { class: "convitem",
            div { class: "convmeta",
                span { class: "convwho", "{c.author}" }
                span { class: "{kind_cls}", "{label}" }
                if !day.is_empty() {
                    span { class: "convdate", "{day}" }
                }
                span { class: "spacer" }
                if has_link {
                    button {
                        class: "iconbtn sm",
                        title: "Open on github.com",
                        onclick: move |_| open_browser(&url),
                        "↗"
                    }
                }
            }
            if let (Some(loc), Some((path, line))) = (loc, target) {
                div {
                    class: "convloc",
                    title: "Open {loc}",
                    onclick: move |_| st.open_at(path.clone(), line),
                    "{loc}"
                }
            }
            if !c.body.is_empty() {
                Body { text: c.body.clone() }
            }
        }
    }
}
