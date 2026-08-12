//! The conversation pane: a pull request's description, the discussion on it,
//! and the comments left on lines of the diff.
//!
//! Bodies are markdown, and they are shown as the markdown they are. Rendering
//! them would mean putting text written by someone else into this webview as
//! HTML, which is not a trade worth making for a few bold headings.

use dioxus::prelude::*;

use crate::backend::auth::open_browser;
use crate::backend::github::{pr_comments, Comment, CommentKind, PrDetail, RepoRef};

use super::app::{Conversation, St};
use super::panes::{Edge, Splitter};

/// Fetch the conversation for a pull request, unless it has been closed or
/// swapped out from under us in the meantime.
pub(super) async fn load(st: St, repo: RepoRef, number: u64) {
    let mut conv = st.conv;
    conv.set(Conversation::Loading);
    let token = st.api_token();
    let target = repo.clone();
    let got = tokio::task::spawn_blocking(move || pr_comments(&token, &target, number)).await;

    let still_open = st
        .workspace
        .peek()
        .pr()
        .is_some_and(|pr| pr.repo == repo && pr.number == number);
    if !still_open {
        return;
    }
    conv.set(match got {
        Ok(Ok(thread)) => Conversation::Ready(Box::new(thread)),
        Ok(Err(e)) => Conversation::Failed(format!("{e:#}")),
        Err(e) => Conversation::Failed(e.to_string()),
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
    let Some(pr) = st.workspace.read().pr().cloned() else {
        return rsx! {};
    };
    let mut open = st.conv_open;
    let conv = st.conv.read().clone();

    let count = match &conv {
        Conversation::Ready(thread) => thread.comments.len(),
        _ => 0,
    };

    if !*open.read() {
        return rsx! {
            div {
                class: "convrail",
                title: "Show the pull request conversation",
                onclick: move |_| open.set(true),
                span { class: "convrail-chev", "‹" }
                div { class: "convrail-label",
                    "CONVERSATION"
                    if count > 0 {
                        span { class: "convrail-count", "{count}" }
                    }
                }
            }
        };
    }

    let repo = pr.repo.clone();
    let number = pr.number;
    let reloading = conv == Conversation::Loading;

    let body = match conv {
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

    rsx! {
        Splitter { edge: Edge::Conv }
        div { class: "convpane",
            div { class: "conv-hdr",
                span { class: "side-title", "CONVERSATION" }
                if count > 0 {
                    span { class: "convcount", "{count}" }
                }
                span { class: "spacer" }
                button {
                    class: reload_cls,
                    title: "Reload the conversation from GitHub",
                    disabled: reloading,
                    // Root scope: this button is re-rendered by the load it starts.
                    onclick: move |_| {
                        spawn_forever(load(st, repo.clone(), number));
                    },
                    span { class: "glyph", "⟳" }
                }
                button {
                    class: "iconbtn",
                    title: "Hide the conversation",
                    onclick: move |_| open.set(false),
                    "›"
                }
            }
            div { class: "conv-body",
                Description { pr }
                {body}
            }
        }
    }
}

#[component]
fn Description(pr: PrDetail) -> Element {
    let body = pr.body.trim().to_string();
    let url = pr.html_url.clone();
    rsx! {
        div { class: "convitem convdesc",
            div { class: "convmeta",
                span { class: "convwho", "{pr.author}" }
                span { class: "convkind", "opened #{pr.number}" }
                if pr.draft {
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
            div { class: "convtitle", "{pr.title}" }
            if body.is_empty() {
                div { class: "convnone", "No description." }
            } else {
                div { class: "convtext", "{body}" }
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
                div { class: "convtext", "{c.body}" }
            }
        }
    }
}
