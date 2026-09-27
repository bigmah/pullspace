//! Writing back to a pull request: a comment on a line of the diff, a reply to
//! one, the discussion, and the review that says what the reader made of it.
//!
//! Line comments are written into the diff, under the line they are about, and
//! are held back by default — kept in [`drafts`] until the review they belong
//! to is submitted from the foot of the conversation, when they go out together
//! in one request the way they do on github.com. "Comment now" is the other
//! way, for the one note that is not worth a review.
//!
//! One write is in flight at a time, app-wide ([`St::writing`]). A review is a
//! thing somebody does once and then waits for, and a second button pressed
//! while the first is still out is much more likely to be a double click than
//! a second thought.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use dioxus::prelude::*;

use crate::backend::auth::open_browser;
use crate::backend::difftool::{FileDiff, Line, LineKind};
use crate::backend::drafts::{self, Note, Pending};
use crate::backend::github::{self, Comment, CommentKind, LineNote, RepoRef, Side, Verdict};
use crate::backend::viewed;

use super::app::{Account, Conversation, St};
use super::conversation::{Body, day_of, reload};
use super::github::GithubMark;

/// A comment being written on one line of the open pull request's diff.
#[derive(Clone, PartialEq, Debug)]
pub struct Composing {
    /// Which pull request ([`viewed::pr_key`]). The same path can be open in
    /// another space, on another pull request, and this is not about that one.
    pub pr: String,
    pub path: PathBuf,
    pub side: Side,
    pub line: usize,
    /// Answering the thread with this root, rather than starting one.
    pub reply: Option<u64>,
    /// Rewriting this held note, rather than adding one.
    pub editing: Option<u64>,
    /// What the box opens with: empty, or the note being rewritten.
    pub text: String,
}

/// Which box a write was sent from — so that the reason it failed is shown in
/// that box and not in every one of them.
#[derive(Clone, PartialEq, Debug)]
pub enum Origin {
    Review(String),
    Line(Composing),
}

#[derive(Clone, PartialEq, Debug)]
pub enum Writing {
    Idle,
    Busy(Origin),
    Failed(Origin, String),
}

impl Writing {
    fn busy(&self) -> bool {
        matches!(self, Writing::Busy(_))
    }

    fn busy_on(&self, from: &Origin) -> bool {
        matches!(self, Writing::Busy(f) if f == from)
    }

    fn failed_on(&self, from: &Origin) -> Option<String> {
        match self {
            Writing::Failed(f, e) if f == from => Some(e.clone()),
            _ => None,
        }
    }
}

/// What the open file's diff needs to know to offer comments and show them.
#[derive(Clone, PartialEq, Default)]
pub struct Talk {
    /// Whether a `+` goes beside the lines: a pull request's own diff (not one
    /// of its commits, whose numbering GitHub would not recognise), and
    /// somebody signed in to write as.
    pub on: bool,
    pub path: PathBuf,
    /// The lines with something under them: a thread, a held note, or a box
    /// being written in.
    pub spots: HashSet<(Side, usize)>,
}

impl Talk {
    /// Where a comment on this line would go, if one can go on it at all.
    ///
    /// Only on the lines GitHub's own diff has. A stretch opened out of a gap
    /// is lines GitHub never showed anyone, and it refuses a comment on them.
    pub fn add_for(&self, diff: &FileDiff, index: usize, l: &Line) -> Option<(Side, usize)> {
        if !self.on || in_gap(diff, index) {
            return None;
        }
        match l.kind {
            LineKind::Del => l.old_no.map(|n| (Side::Left, n)),
            LineKind::Add | LineKind::Ctx => l.new_no.map(|n| (Side::Right, n)),
        }
    }

    /// Whatever hangs under a line, on either of its numberings.
    pub fn under(&self, l: &Line) -> Element {
        let at: Vec<(Side, usize)> = [
            l.new_no.map(|n| (Side::Right, n)),
            l.old_no.map(|n| (Side::Left, n)),
        ]
        .into_iter()
        .flatten()
        .filter(|s| self.spots.contains(s))
        .collect();
        if at.is_empty() {
            return rsx! {};
        }
        rsx! {
            for (side , line) in at {
                LineThread {
                    key: "{side:?}{line}",
                    path: self.path.clone(),
                    side,
                    line,
                }
            }
        }
    }
}

fn in_gap(diff: &FileDiff, index: usize) -> bool {
    diff.gaps
        .iter()
        .any(|g| index >= g.at && index < g.at + g.len)
}

/// The `+` itself — drawn only where [`Talk::add_for`] says, and answered by
/// one listener on the page rather than a handler per row.
pub fn add_button(spot: Option<(Side, usize)>) -> Element {
    let Some((side, line)) = spot else {
        return rsx! {};
    };
    let s = match side {
        Side::Left => "L",
        Side::Right => "R",
    };
    rsx! {
        span {
            class: "cadd",
            "data-side": s,
            "data-no": "{line}",
            title: "Comment on this line",
            "+"
        }
    }
}

/// Which pull request is open, and at which head — `None` for anything else,
/// including one of its commits.
fn open_pr(st: &St) -> Option<(RepoRef, u64, String)> {
    let held = st.workspace.peek();
    let pr = held.pr()?;
    Some((pr.repo.clone(), pr.number, pr.head_sha.clone()))
}

fn signed_in(st: &St) -> bool {
    matches!(&*st.account.read(), Account::SignedIn { .. })
}

/// What the viewer hands [`Talk`] for the file that is open. Read inside a
/// memo, so that the review's summary being typed into — which is in the same
/// signal as the held notes — does not redraw a diff whose lines it changes
/// nothing about.
pub fn talk(st: St) -> Talk {
    let Some(path) = st.open.read().clone() else {
        return Talk::default();
    };
    let key = {
        let held = st.workspace.read();
        let Some(pr) = held.pr() else {
            return Talk::default();
        };
        viewed::pr_key(&pr.repo, pr.number)
    };
    let mut spots = HashSet::new();
    if let Conversation::Ready(thread) = &*st.conv.read() {
        for c in thread.comments.iter().filter(|c| on_line(c, &path)) {
            if let Some(line) = c.line {
                spots.insert((c.side.unwrap_or(Side::Right), line));
            }
        }
    }
    if let Some(pending) = st.drafts.read().get(&key) {
        let here = path.to_string_lossy();
        for n in pending.notes.iter().filter(|n| n.path == here) {
            spots.insert((n.side, n.line));
        }
    }
    if let Some(c) = &*st.composing.read()
        && c.pr == key
        && c.path == path
    {
        spots.insert((c.side, c.line));
    }
    Talk {
        on: signed_in(&st),
        path,
        spots,
    }
}

/// A line comment still standing on a line of this file's diff. An outdated
/// one is about a diff that no longer exists, and stays in the conversation.
fn on_line(c: &Comment, path: &Path) -> bool {
    c.kind == CommentKind::Inline && !c.outdated && c.path.as_deref() == Some(path)
}

// ------------------------------------------------------------ the listener

/// A click on a `+`: which side, which line. The file is whichever is open.
const NOTE_JS: &str = r#"
(function () {
  if (window.__pullspace_adds) window.__pullspace_adds();
  var on = function (e) {
    if (!e.target || !e.target.closest) return;
    var b = e.target.closest('.cadd');
    if (!b) return;
    var side = b.getAttribute('data-side');
    var line = parseInt(b.getAttribute('data-no'), 10);
    if (!line || (side !== 'L' && side !== 'R')) return;
    e.preventDefault();
    e.stopPropagation();
    dioxus.send([side, line]);
  };
  document.addEventListener('click', on);
  window.__pullspace_adds = function () {
    document.removeEventListener('click', on);
  };
})();
"#;

/// Listen for `+` being clicked, for as long as the app is up.
pub async fn adds(st: St) {
    let mut eval = document::eval(NOTE_JS);
    while let Ok((side, line)) = eval.recv::<(String, usize)>().await {
        let side = if side == "L" { Side::Left } else { Side::Right };
        st.compose(side, line, None);
    }
}

impl St {
    /// Open a box for writing on `line` of the file on screen — a new thread,
    /// or a reply to `reply`.
    pub fn compose(&self, side: Side, line: usize, reply: Option<u64>) {
        let Some((repo, number, _)) = open_pr(self) else {
            return;
        };
        let Some(path) = self.open.peek().clone() else {
            return;
        };
        let mut composing = self.composing;
        composing.set(Some(Composing {
            pr: viewed::pr_key(&repo, number),
            path,
            side,
            line,
            reply,
            editing: None,
            text: String::new(),
        }));
    }

    /// Change the review being written on `pr`, and keep the change.
    fn edit_pending(&self, pr: &str, change: impl FnOnce(&mut Pending)) {
        let mut drafts = self.drafts;
        let mut held = drafts.write();
        let pending = held.entry(pr.to_string()).or_default();
        change(pending);
        drafts::save(pr, pending);
    }
}

// ------------------------------------------------------------- the writes

/// Hold a line comment for the review.
fn hold(st: St, at: &Composing, body: String) {
    // The head it was read at, so that it lands where it was written even if
    // the pull request moves on before the review goes.
    let commit = open_pr(&st).map(|(_, _, head)| head).unwrap_or_default();
    let path = at.path.to_string_lossy().to_string();
    let (side, line, editing) = (at.side, at.line, at.editing);
    st.edit_pending(&at.pr, move |p| {
        match editing.and_then(|id| p.notes.iter_mut().find(|n| n.id == id)) {
            Some(note) => note.body = body,
            None => {
                let id = p.next_id();
                p.notes.push(Note {
                    id,
                    commit,
                    path,
                    line,
                    side,
                    body,
                });
            }
        }
    });
    let mut composing = st.composing;
    composing.set(None);
}

/// Send what is in a line's box now — a reply, or a comment of its own.
async fn send_line(st: St, at: Composing, body: String) {
    let Some((repo, number, head)) = open_pr(&st) else {
        return;
    };
    let from = Origin::Line(at.clone());
    let mut writing = st.writing;
    writing.set(Writing::Busy(from.clone()));
    let token = st.api_token();
    let got = match at.reply {
        Some(root) => github::reply_to(&token, &repo, number, root, &body).await,
        None => {
            let note = LineNote {
                path: at.path.to_string_lossy().to_string(),
                line: at.line,
                side: at.side,
                body,
            };
            github::post_line_comment(&token, &repo, number, &head, &note).await
        }
    };
    match got {
        Ok(()) => {
            writing.set(Writing::Idle);
            // Sent, so no longer held — when it was a held note being sent.
            if let Some(id) = at.editing {
                st.edit_pending(&at.pr, |p| p.notes.retain(|n| n.id != id));
            }
            let mut composing = st.composing;
            let still = composing.peek().as_ref() == Some(&at);
            if still {
                composing.set(None);
            }
            reload(st, repo, number).await;
        }
        Err(e) => writing.set(Writing::Failed(from, format!("{e:#}"))),
    }
}

/// Send the review — or, with no verdict and nothing held, a plain comment on
/// the discussion.
async fn send_review(st: St, repo: RepoRef, number: u64, verdict: Option<Verdict>) {
    let pr = viewed::pr_key(&repo, number);
    let from = Origin::Review(pr.clone());
    let pending = st.drafts.peek().get(&pr).cloned().unwrap_or_default();
    let mut writing = st.writing;
    writing.set(Writing::Busy(from.clone()));
    let token = st.api_token();
    let body = pending.body.trim();
    let got = match verdict {
        None if pending.notes.is_empty() => github::post_comment(&token, &repo, number, body).await,
        verdict => {
            let notes: Vec<LineNote> = pending.notes.iter().map(Note::to_send).collect();
            github::submit_review(
                &token,
                &repo,
                number,
                pending.commit(),
                verdict.unwrap_or(Verdict::Comment),
                body,
                &notes,
            )
            .await
        }
    };
    match got {
        Ok(()) => {
            writing.set(Writing::Idle);
            // Gone to GitHub, so gone from here — but only what was sent. A
            // note held from another tab while this was out is not in it.
            let sent: HashSet<u64> = pending.notes.iter().map(|n| n.id).collect();
            let text = pending.body.clone();
            st.edit_pending(&pr, move |p| {
                p.notes.retain(|n| !sent.contains(&n.id));
                if p.body == text {
                    p.body.clear();
                }
            });
            reload(st, repo, number).await;
        }
        Err(e) => writing.set(Writing::Failed(from, format!("{e:#}"))),
    }
}

// ------------------------------------------------------------ in the diff

/// Everything under one line of the diff: the threads on it, the notes held
/// for the review, and the box being written in.
#[component]
fn LineThread(path: PathBuf, side: Side, line: usize) -> Element {
    let st = use_context::<St>();
    let Some((repo, number, _)) = open_pr(&st) else {
        return rsx! {};
    };
    let pr = viewed::pr_key(&repo, number);

    // The threads, each in the order it was written, in the order they began.
    let mut threads: Vec<(u64, Vec<Comment>)> = Vec::new();
    if let Conversation::Ready(thread) = &*st.conv.read() {
        for c in thread.comments.iter().filter(|c| {
            on_line(c, &path) && c.line == Some(line) && c.side.unwrap_or(Side::Right) == side
        }) {
            let root = c.thread_root();
            match threads.iter_mut().find(|(r, _)| *r == root) {
                Some((_, list)) => list.push(c.clone()),
                None => threads.push((root, vec![c.clone()])),
            }
        }
    }
    let here = path.to_string_lossy().to_string();
    let composing = st
        .composing
        .read()
        .clone()
        .filter(|c| c.pr == pr && c.path == path && c.side == side && c.line == line);
    let editing = composing.as_ref().and_then(|c| c.editing);
    let notes: Vec<Note> = st
        .drafts
        .read()
        .get(&pr)
        .map(|p| {
            p.notes
                .iter()
                .filter(|n| n.path == here && n.side == side && n.line == line)
                .filter(|n| Some(n.id) != editing)
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let can_write = signed_in(&st);
    let fresh = composing.clone().filter(|c| c.reply.is_none());

    rsx! {
        div { class: "lthread",
            for (root , list) in threads {
                div { key: "t{root}", class: "lthreadgroup",
                    for c in list {
                        // Cloned, not moved: the key reads it too, and a release
                        // build formats the key after the props have taken it.
                        LineComment { key: "{c.id}", c: c.clone() }
                    }
                    match composing.clone().filter(|c| c.reply == Some(root)) {
                        Some(at) => rsx! { NoteBox { at } },
                        None if can_write => rsx! {
                            div { class: "lreplyrow",
                                button {
                                    class: "lreply",
                                    onclick: move |_| st.compose(side, line, Some(root)),
                                    "Reply…"
                                }
                            }
                        },
                        None => rsx! {},
                    }
                }
            }
            for n in notes {
                HeldNote { key: "n{n.id}", pr: pr.clone(), n: n.clone() }
            }
            if let Some(at) = fresh {
                NoteBox { at }
            }
        }
    }
}

#[component]
fn LineComment(c: Comment) -> Element {
    let day = day_of(&c.created_at);
    let url = c.html_url.clone();
    rsx! {
        div { class: "lcomment",
            div { class: "convmeta",
                span { class: "convwho", "{c.author}" }
                if !day.is_empty() {
                    span { class: "convdate", "{day}" }
                }
                span { class: "spacer" }
                if !url.is_empty() {
                    button {
                        class: "iconbtn sm",
                        title: "Open on github.com",
                        onclick: move |_| open_browser(&url),
                        GithubMark {}
                    }
                }
            }
            if !c.body.is_empty() {
                Body { text: c.body.clone() }
            }
        }
    }
}

/// A line comment held for the review: not sent, and said so.
#[component]
fn HeldNote(pr: String, n: Note) -> Element {
    let st = use_context::<St>();
    let (pr_edit, pr_drop) = (pr.clone(), pr);
    let id = n.id;
    let redo = n.clone();
    rsx! {
        div { class: "lcomment held",
            div { class: "convmeta",
                span { class: "convkind pending", "pending" }
                span { class: "lheldnote", "goes out with your review" }
                span { class: "spacer" }
                button {
                    class: "textlink",
                    onclick: move |_| {
                        let mut composing = st.composing;
                        composing.set(Some(Composing {
                            pr: pr_edit.clone(),
                            path: PathBuf::from(&redo.path),
                            side: redo.side,
                            line: redo.line,
                            reply: None,
                            editing: Some(redo.id),
                            text: redo.body.clone(),
                        }));
                    },
                    "Edit"
                }
                button {
                    class: "textlink",
                    onclick: move |_| st.edit_pending(&pr_drop, |p| p.notes.retain(|n| n.id != id)),
                    "Delete"
                }
            }
            Body { text: n.body.clone() }
        }
    }
}

/// The box a line comment is written in.
#[component]
fn NoteBox(at: Composing) -> Element {
    let st = use_context::<St>();
    let mut text = use_signal(|| at.text.clone());
    let from = Origin::Line(at.clone());
    let writing = st.writing.read().clone();
    let busy = writing.busy_on(&from);
    let blocked = writing.busy();
    let error = writing.failed_on(&from);
    let empty = text.read().trim().is_empty();
    let reply = at.reply.is_some();
    let editing = at.editing.is_some();
    let signed = signed_in(&st);

    let (at_hold, at_send, at_key) = (at.clone(), at.clone(), at.clone());
    let cancel = move |_| {
        let mut composing = st.composing;
        composing.set(None);
    };
    // The button ⌘Enter means: whatever the primary one is.
    let primary = move |at: Composing| {
        let body = text.peek().trim().to_string();
        if body.is_empty() || st.writing.peek().busy() {
            return;
        }
        if at.reply.is_some() {
            spawn_forever(send_line(st, at, body));
        } else {
            hold(st, &at, body);
        }
    };

    if !signed {
        return rsx! {
            div { class: "lcompose",
                SignInNote { what: "comment on this line" }
            }
        };
    }

    rsx! {
        div { class: "lcompose",
            textarea {
                class: "reviewtext",
                rows: "3",
                placeholder: if reply { "Reply…" } else { "Leave a comment on this line" },
                readonly: busy,
                value: "{text}",
                onmounted: move |e| async move {
                    let _ = e.set_focus(true).await;
                },
                oninput: move |e| text.set(e.value()),
                onkeydown: move |e| {
                    let m = e.modifiers();
                    if e.key() == Key::Enter && (m.meta() || m.ctrl()) {
                        e.prevent_default();
                        primary(at_key.clone());
                    } else if e.key() == Key::Escape {
                        e.stop_propagation();
                        let mut composing = st.composing;
                        composing.set(None);
                    }
                },
            }
            if let Some(e) = error {
                div { class: "gherror", "{e}" }
            }
            div { class: "reviewrow",
                span { class: "spacer" }
                button { class: "ghostbtn", onclick: cancel, disabled: busy, "Cancel" }
                if reply {
                    button {
                        class: "primarybtn",
                        disabled: empty || blocked,
                        title: "Post this reply now (⌘Enter)",
                        onclick: move |_| primary(at_hold.clone()),
                        if busy { "Replying…" } else { "Reply" }
                    }
                } else {
                    button {
                        class: "ghostbtn",
                        disabled: empty || blocked,
                        title: "Post this comment on its own, now, without waiting for the review",
                        onclick: move |_| {
                            let body = text.peek().trim().to_string();
                            spawn_forever(send_line(st, at_send.clone(), body));
                        },
                        if busy { "Posting…" } else { "Comment now" }
                    }
                    button {
                        class: "primarybtn",
                        disabled: empty || blocked,
                        title: "Hold this for your review, and send them all together from the conversation (⌘Enter)",
                        onclick: move |_| primary(at_hold.clone()),
                        if editing { "Save" } else { "Add to review" }
                    }
                }
            }
        }
    }
}

#[component]
fn SignInNote(what: &'static str) -> Element {
    let st = use_context::<St>();
    rsx! {
        div { class: "reviewsignin",
            "Sign in with a GitHub token to {what}. "
            button {
                class: "textlink",
                onclick: move |_| {
                    let mut gh = st.gh_open;
                    gh.set(true);
                },
                "Sign in"
            }
        }
    }
}

// ------------------------------------------------------ the conversation

/// The foot of the conversation: the discussion's comment box, and the review
/// the held line comments go out with.
#[component]
pub fn ReviewBox(repo: RepoRef, number: u64, author: String) -> Element {
    let st = use_context::<St>();
    let pr = viewed::pr_key(&repo, number);
    if !signed_in(&st) {
        return rsx! {
            div { class: "reviewbox",
                SignInNote { what: "comment on or review this pull request" }
            }
        };
    }
    let pending = st.drafts.read().get(&pr).cloned().unwrap_or_default();
    let from = Origin::Review(pr.clone());
    let writing = st.writing.read().clone();
    let busy = writing.busy_on(&from);
    let blocked = writing.busy();
    let error = writing.failed_on(&from);
    let wrote = !pending.body.trim().is_empty();
    let held = pending.notes.len();
    // GitHub will not take either verdict from the author, and says so in a
    // 422 — better said on the button, before anybody writes a paragraph.
    let own = !author.is_empty() && st.viewer() == author;
    let own_why = "GitHub does not let you approve, or request changes on, your own pull request";

    let pr_body = pr.clone();
    let send = move |verdict: Option<Verdict>| {
        spawn_forever(send_review(st, repo.clone(), number, verdict));
    };
    let comment_why = match held {
        0 => "Add this to the discussion".to_string(),
        n => format!(
            "Submit a review with the {n} held line comment{} and this summary, neither approving nor asking for changes",
            if n == 1 { "" } else { "s" }
        ),
    };

    rsx! {
        div { class: "reviewbox",
            div { class: "reviewhdr",
                span { "Your review" }
                if held > 0 {
                    span { class: "convkind pending",
                        "{held} pending"
                    }
                }
            }
            if held > 0 {
                div { class: "reviewnotes",
                    for n in pending.notes.iter().cloned() {
                        PendingRow { key: "{n.id}", pr: pr.clone(), n: n.clone() }
                    }
                }
            }
            textarea {
                class: "reviewtext",
                rows: "4",
                readonly: busy,
                placeholder: if held > 0 { "Summary (optional)" } else { "Leave a comment" },
                value: "{pending.body}",
                oninput: move |e| {
                    let v = e.value();
                    st.edit_pending(&pr_body, move |p| p.body = v);
                },
            }
            if let Some(e) = error {
                div { class: "gherror", "{e}" }
            }
            div { class: "reviewrow",
                button {
                    class: "ghostbtn",
                    disabled: blocked || (!wrote && held == 0),
                    title: "{comment_why}",
                    onclick: {
                        let send = send.clone();
                        move |_| send(None)
                    },
                    if busy { "Sending…" } else { "Comment" }
                }
                span { class: "spacer" }
                button {
                    class: "ghostbtn bad",
                    // GitHub asks for a reason with a request for changes; the
                    // held line comments count as one.
                    disabled: blocked || own || (!wrote && held == 0),
                    title: if own { own_why } else { "Submit, asking for changes before this merges" },
                    onclick: {
                        let send = send.clone();
                        move |_| send(Some(Verdict::RequestChanges))
                    },
                    "Request changes"
                }
                button {
                    class: "primarybtn",
                    disabled: blocked || own,
                    title: if own { own_why } else { "Submit, approving this pull request" },
                    onclick: move |_| send(Some(Verdict::Approve)),
                    "Approve"
                }
            }
        }
    }
}

/// One held note, listed under the review it will go out with.
#[component]
fn PendingRow(pr: String, n: Note) -> Element {
    let st = use_context::<St>();
    let loc = format!("{}:{}", n.path, n.line);
    let path = PathBuf::from(&n.path);
    let (line, side, id) = (n.line, n.side, n.id);
    let first = n.body.lines().next().unwrap_or_default().to_string();
    rsx! {
        div { class: "reviewnote",
            span {
                class: "convloc",
                title: "Open {loc}",
                onclick: move |_| {
                    // The head's numbering is the one a file opens at; a line
                    // only the base has is found by opening the file.
                    match side {
                        Side::Right => st.open_at(path.clone(), line),
                        Side::Left => st.open_file(path.clone()),
                    }
                },
                "{loc}"
            }
            span { class: "reviewnotetext", "{first}" }
            button {
                class: "iconbtn sm",
                title: "Delete this comment",
                onclick: move |_| st.edit_pending(&pr, |p| p.notes.retain(|n| n.id != id)),
                "×"
            }
        }
    }
}
