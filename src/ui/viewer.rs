use std::path::PathBuf;

use dioxus::prelude::*;

use crate::backend::difftool::{diff_hunks, stats, to_rows, Hunk, Line, LineKind};
use crate::backend::gitio::{head_file, worktree_file};
use crate::backend::highlight::{highlight, Span};
use crate::backend::htmlview;
use crate::backend::tree::ChangeKind;
use crate::backend::FileContent;

use super::app::{PrFileState, St, ViewMode};
use super::prcache::ensure_path;

const BIG_FILE_BYTES: usize = 400_000;
const BIG_FILE_LINES: usize = 6_000;

#[derive(Clone, PartialEq)]
enum SourceLines {
    Colored(Vec<Vec<Span>>),
    Plain(Vec<String>),
}

/// A rendered HTML page, ready to hand to an `<img>`.
#[derive(Clone, PartialEq)]
struct Shot {
    uri: String,
    /// The width to show it at — half what it was drawn at.
    css_width: u32,
    clipped: bool,
    blocked: usize,
}

/// What the viewer has to show right now. Local files resolve synchronously;
/// pull request files arrive over the network, hence `Loading`/`Failed`.
#[derive(Clone, PartialEq)]
enum Pane {
    Empty,
    Loading,
    Failed(String),
    Ready {
        rel: PathBuf,
        /// Left-hand side: HEAD, or the PR's merge base.
        old: FileContent,
        /// Right-hand side: the working tree, or the PR's head commit.
        new: FileContent,
    },
}

fn scroll_js(line: usize) -> String {
    format!(
        r#"(function(){{var n=0;function go(){{var e=document.getElementById('L{line}');if(e){{var t=e.classList.contains('anchor')?e.parentElement:e;t.scrollIntoView({{block:'center'}});t.classList.add('flash');setTimeout(function(){{t.classList.remove('flash')}},1400);}}else if(n++<25){{setTimeout(go,60);}}}}go();}})();"#
    )
}

#[component]
pub fn Viewer() -> Element {
    let st = use_context::<St>();

    // Jump-to-line requests (definitions, references, search hits).
    use_effect(move || {
        let mut ps = st.pending_scroll;
        let target = *ps.read();
        if let Some(line) = target {
            document::eval(&scroll_js(line));
            ps.set(None);
        }
    });

    // On a pull request the open file usually arrives already warmed, from the
    // background prefetch or the tree's hover handler. This covers the rest —
    // and is a no-op when the content is cached or on its way.
    use_effect(move || {
        let rel = st.open.read().clone();
        if !st.workspace.read().is_remote() {
            return;
        }
        if let Some(rel) = rel {
            ensure_path(st, &rel);
        }
    });

    // Resolve the two sides of the open file, from disk or from the PR cache.
    let data = use_memo(move || {
        st.refresh_tick.read();
        let Some(rel) = st.open.read().clone() else {
            return Pane::Empty;
        };
        if st.workspace.read().is_remote() {
            return match st.pr_files.read().get(&rel) {
                None | Some(PrFileState::Loading) => Pane::Loading,
                Some(PrFileState::Failed(e)) => Pane::Failed(e.clone()),
                Some(PrFileState::Ready { base, head }) => Pane::Ready {
                    rel,
                    old: base.clone(),
                    new: head.clone(),
                },
            };
        }
        let root = st.root_path();
        Pane::Ready {
            old: head_file(&root, &rel),
            new: worktree_file(&root, &rel),
            rel,
        }
    });

    let hunks = use_memo(move || {
        let guard = data.read();
        let Pane::Ready { rel, old, new } = &*guard else {
            return None;
        };
        let status = st.statuses.read().get(rel).copied()?;
        // A file that is new on this side has nothing to compare against, even
        // if a path of the same name exists in the base.
        let old_text = match status {
            ChangeKind::Untracked | ChangeKind::Added => "",
            _ => old.text()?,
        };
        Some(diff_hunks(old_text, new.text()?))
    });

    let source_lines = use_memo(move || {
        let guard = data.read();
        let Pane::Ready { rel, old, new } = &*guard else {
            return None;
        };
        let text = match new {
            FileContent::Text(t) => t.clone(),
            // Deleted: fall back to the old side so there is something to read.
            FileContent::Absent => match old {
                FileContent::Text(t) => t.clone(),
                _ => return None,
            },
            FileContent::Binary => return None,
        };
        let line_count = text.lines().count();
        if text.len() > BIG_FILE_BYTES || line_count > BIG_FILE_LINES {
            Some(SourceLines::Plain(text.lines().map(String::from).collect()))
        } else {
            Some(SourceLines::Colored(highlight(rel, &text)))
        }
    });

    // Draw the page, off the UI thread and only while the preview is the mode
    // being asked for. Keyed on the file's own text, so a reload that changes
    // it redraws and one that does not costs nothing.
    let preview = use_resource(move || {
        let wanted = *st.view_mode.read() == ViewMode::Preview;
        let source = match &*data.read() {
            Pane::Ready { rel, old, new } if wanted && htmlview::is_html(rel) => match (new, old) {
                (FileContent::Text(t), _) => Some(t.clone()),
                // Deleted by the PR: draw the version that is going away.
                (FileContent::Absent, FileContent::Text(t)) => Some(t.clone()),
                _ => None,
            },
            _ => None,
        };
        async move {
            let text = source?;
            let done = tokio::task::spawn_blocking(move || htmlview::render(&text)).await;
            Some(match done {
                Ok(Ok(p)) => Ok(Shot {
                    uri: htmlview::data_uri(&p.png),
                    css_width: p.css_size().0,
                    clipped: p.clipped,
                    blocked: p.blocked,
                }),
                Ok(Err(e)) => Err(format!("{e:#}")),
                // Blitz is a young engine. A page that trips it takes the
                // blocking task down, not the app — so say so and carry on.
                Err(e) if e.is_panic() => {
                    Err("This page could not be laid out — its source is still readable in Source view.".to_string())
                }
                Err(e) => Err(e.to_string()),
            })
        }
    });

    // A new file starts at the top. The scroll container outlives the file in
    // it, so without this, opening something short after reading deep into
    // something long lands you at the bottom of it.
    use_effect(move || {
        let _ = st.open.read();
        if st.pending_scroll.peek().is_some() {
            return;
        }
        document::eval("var e=document.querySelector('.codewrap'); if(e) e.scrollTop=0;");
    });

    let guard = data.read();
    // Loading and failure replace the file, not the window around it: a header
    // that blinks out and back every time an uncached file is clicked is worse
    // jank than the wait it is reporting.
    let (rel, new_side, pending) = match &*guard {
        Pane::Empty => return rsx! { Welcome {} },
        Pane::Loading => (
            st.open.read().clone().unwrap_or_default(),
            FileContent::Absent,
            Some(rsx! { div { class: "notice", "Loading from GitHub…" } }),
        ),
        Pane::Failed(e) => (
            st.open.read().clone().unwrap_or_default(),
            FileContent::Absent,
            Some(rsx! { div { class: "notice error", "{e}" } }),
        ),
        Pane::Ready { rel, new, .. } => (rel.clone(), new.clone(), None),
    };
    let settled = pending.is_none();

    let status = st.statuses.read().get(&rel).copied();
    let deleted_note =
        settled && new_side == FileContent::Absent && status == Some(ChangeKind::Deleted);
    // Go to Definition / Find References run against whatever was indexed —
    // the local repository, or this pull request's own checkout.
    let symbols_enabled = st.scan_root.read().dir().is_some();

    let previewable = htmlview::is_html(&rel);
    // Only changed files have a diff to show, and only HTML has a page to draw.
    let mode = match *st.view_mode.read() {
        ViewMode::Preview if !previewable => ViewMode::Source,
        ViewMode::Inline | ViewMode::Split if status.is_none() => ViewMode::Source,
        m => m,
    };

    let rel_str = rel.display().to_string();
    let badge = status.map(|s| (s.badge(), s.css()));
    let diff_stats = hunks.read().as_ref().map(|h| stats(h));

    let selected = st.selected.read().clone();

    let body = match mode {
        ViewMode::Source => match source_lines.read().as_ref() {
            Some(SourceLines::Colored(lines)) => render_colored(st, lines, symbols_enabled),
            Some(SourceLines::Plain(lines)) => render_plain(lines),
            None => rsx! { div { class: "notice", "Binary file — no preview." } },
        },
        ViewMode::Inline => match hunks.read().as_ref() {
            Some(h) if h.is_empty() => rsx! { div { class: "notice", "No differences." } },
            Some(h) => render_inline(h),
            None => rsx! { div { class: "notice", "Binary file — cannot diff." } },
        },
        ViewMode::Split => match hunks.read().as_ref() {
            Some(h) if h.is_empty() => rsx! { div { class: "notice", "No differences." } },
            Some(h) => render_split(h),
            None => rsx! { div { class: "notice", "Binary file — cannot diff." } },
        },
        ViewMode::Preview => match &*preview.read() {
            None => rsx! { div { class: "notice", "Drawing the page…" } },
            Some(None) => rsx! { div { class: "notice", "Nothing to draw — this file has no text." } },
            Some(Some(Err(e))) => rsx! { div { class: "notice error", "{e}" } },
            Some(Some(Ok(shot))) => render_preview(shot),
        },
    };

    let mut vm = st.view_mode;
    // A disabled button at 30% opacity with no explanation is a puzzle; the
    // tooltip is where the reason goes.
    let mode_btn = |label: &'static str,
                    m: ViewMode,
                    cur: ViewMode,
                    enabled: bool,
                    why: &'static str| {
        let cls = if m == cur && enabled {
            "modebtn on"
        } else {
            "modebtn"
        };
        rsx! {
            button {
                class: cls,
                title: "{why}",
                disabled: !enabled,
                onclick: move |_| vm.set(m),
                "{label}"
            }
        }
    };
    // Nothing to diff or draw until the file itself has arrived.
    let diffable = settled && status.is_some();
    let diff_why = if diffable {
        "Show this file's changes"
    } else if settled {
        "This file has no changes to diff"
    } else {
        "Waiting for the file"
    };

    rsx! {
        div { class: "viewer",
            div { class: "viewhdr",
                span { class: "vpath", title: "{rel_str}", "{rel_str}" }
                if let Some((b, c)) = badge {
                    span { class: "badge {c}", "{b}" }
                }
                if deleted_note {
                    span { class: "delnote", "deleted — showing the old version" }
                }
                if let Some(ds) = diff_stats {
                    if diffable {
                        span { class: "dstat add", "+{ds.added}" }
                        span { class: "dstat del", "−{ds.removed}" }
                    }
                }
                span { class: "spacer" }
                // One control, not four loose buttons: these are alternatives,
                // and a segmented group is what says so.
                div { class: "modegroup",
                    {mode_btn("Source", ViewMode::Source, mode, settled, "Show the file as it stands")}
                    {mode_btn("Inline", ViewMode::Inline, mode, diffable, diff_why)}
                    {mode_btn("Split", ViewMode::Split, mode, diffable, diff_why)}
                    if previewable {
                        {mode_btn("Preview", ViewMode::Preview, mode, settled, "Draw this page as it would look")}
                    }
                }
            }
            if let Some(name) = selected {
                if symbols_enabled {
                    SymBar { name }
                }
            }
            div { class: "codewrap", {pending.unwrap_or(body)} }
        }
    }
}

#[component]
fn Welcome() -> Element {
    let st = use_context::<St>();
    let mut gh_open = st.gh_open;
    rsx! {
        div { class: "viewer",
            div { class: "welcome",
                div { class: "welcome-logo", "pullspace" }
                div { class: "welcome-sub", "a lightweight diff viewer" }
                div { class: "welcome-hint", "Pick a file on the left — changed files open as diffs." }
                div { class: "welcome-hint", "Click an identifier for Go to Definition / Find References." }
                button {
                    class: "primarybtn",
                    onclick: move |_| gh_open.set(true),
                    "Review a GitHub pull request"
                }
            }
        }
    }
}

#[component]
fn SymBar(name: String) -> Element {
    let st = use_context::<St>();
    let n1 = name.clone();
    let n2 = name.clone();
    let mut sel = st.selected;
    rsx! {
        div { class: "symbar",
            span { class: "symname", "{name}" }
            button { class: "symbtn", onclick: move |_| st.goto_def(&n1), "Go to Definition" }
            button { class: "symbtn", onclick: move |_| st.find_refs(&n2), "Find References" }
            span { class: "spacer" }
            button {
                class: "iconbtn",
                title: "Clear the selection",
                onclick: move |_| sel.set(None),
                "✕"
            }
        }
    }
}

/// Split text into identifier / non-identifier runs.
fn tokenize(s: &str) -> Vec<(bool, String)> {
    let mut out: Vec<(bool, String)> = Vec::new();
    let mut cur = String::new();
    let mut cur_word = false;
    for ch in s.chars() {
        let is_word = ch.is_alphanumeric() || ch == '_';
        if !cur.is_empty() && is_word != cur_word {
            out.push((cur_word, std::mem::take(&mut cur)));
        }
        cur_word = is_word;
        cur.push(ch);
    }
    if !cur.is_empty() {
        out.push((cur_word, cur));
    }
    out
}

fn clickable(tok: &str) -> bool {
    tok.len() > 1 && tok.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')
}

fn token_span(st: St, color: String, is_word: bool, tok: String, enabled: bool) -> Element {
    if enabled && is_word && clickable(&tok) {
        let t2 = tok.clone();
        let mut sel = st.selected;
        rsx! {
            span {
                class: "id",
                style: "color:{color}",
                onclick: move |_| sel.set(Some(t2.clone())),
                "{tok}"
            }
        }
    } else {
        rsx! {
            span { style: "color:{color}", "{tok}" }
        }
    }
}

fn render_colored(st: St, lines: &[Vec<Span>], symbols_enabled: bool) -> Element {
    let lines = lines.to_vec();
    rsx! {
        div { class: "code",
            for (i, spans) in lines.into_iter().enumerate() {
                div { class: "cl", id: "L{i + 1}",
                    span { class: "ln", "{i + 1}" }
                    span { class: "lc",
                        for sp in spans {
                            for (is_word, tok) in tokenize(&sp.text) {
                                {token_span(st, sp.color.clone(), is_word, tok, symbols_enabled)}
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The page as a picture, over a bar saying what it is and is not.
///
/// "scripts disabled" is not a disclaimer to be buried: this is a drawing of
/// the page, not the page running, and anyone judging a change by it should
/// know that up front.
fn render_preview(shot: &Shot) -> Element {
    let uri = shot.uri.clone();
    let blocked = shot.blocked;
    let clipped = shot.clipped;
    rsx! {
        div { class: "previewwrap",
            div { class: "previewbar",
                span {
                    class: "previewsafe",
                    title: "Drawn by Blitz, which has no JavaScript engine — nothing in this page can run",
                    "scripts disabled"
                }
                if blocked > 0 {
                    span {
                        class: "previewnote",
                        title: "Stylesheets, images and fonts hosted elsewhere are not fetched",
                        "{blocked} remote resource{plural(blocked)} blocked"
                    }
                }
                if clipped {
                    span { class: "previewnote", "page too tall — cut off below" }
                }
            }
            img {
                class: "previewimg",
                style: "width:{shot.css_width}px",
                src: "{uri}",
            }
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn render_plain(lines: &[String]) -> Element {
    let lines = lines.to_vec();
    rsx! {
        div { class: "code",
            for (i, text) in lines.into_iter().enumerate() {
                div { class: "cl", id: "L{i + 1}",
                    span { class: "ln", "{i + 1}" }
                    span { class: "lc", "{text}" }
                }
            }
        }
    }
}

fn anchor(no: Option<usize>) -> Element {
    match no {
        Some(n) => rsx! { span { id: "L{n}", class: "anchor" } },
        None => rsx! {},
    }
}

fn num(no: Option<usize>) -> String {
    no.map(|n| n.to_string()).unwrap_or_default()
}

fn segs_rsx(l: &Line) -> Element {
    let segs = l.segs.clone();
    rsx! {
        for seg in segs {
            if seg.emph {
                span { class: "emph", "{seg.text}" }
            } else {
                span { "{seg.text}" }
            }
        }
    }
}

fn inline_line(l: &Line) -> Element {
    let (cls, sign) = match l.kind {
        LineKind::Ctx => ("cl", " "),
        LineKind::Add => ("cl dl-add", "+"),
        LineKind::Del => ("cl dl-del", "-"),
    };
    let old = num(l.old_no);
    let new = num(l.new_no);
    rsx! {
        div { class: "{cls}",
            {anchor(l.new_no)}
            span { class: "ln", "{old}" }
            span { class: "ln", "{new}" }
            span { class: "dsign", "{sign}" }
            span { class: "lc", {segs_rsx(l)} }
        }
    }
}

/// The `@@ … @@` line, pinned to the top of the pane for as long as its own
/// hunk is on screen — halfway down a long one, knowing which it is is the
/// whole question. Its text is pinned to the left edge too, so scrolling
/// sideways through wide code does not take the answer with it.
fn hunk_header(header: &str) -> Element {
    let header = header.to_string();
    rsx! {
        div { class: "hunkhdr",
            span { class: "hunkhdrtext", "{header}" }
        }
    }
}

fn render_inline(hunks: &[Hunk]) -> Element {
    let hunks = hunks.to_vec();
    rsx! {
        div { class: "code inline",
            for h in hunks {
                div { class: "hunk",
                    {hunk_header(&h.header)}
                    for l in h.lines.iter() {
                        {inline_line(l)}
                    }
                }
            }
        }
    }
}

fn split_cell(l: Option<&Line>, right: bool) -> Element {
    match l {
        None => rsx! { div { class: "scell s-empty" } },
        Some(l) => {
            let cls = match l.kind {
                LineKind::Ctx => "scell",
                LineKind::Add => "scell dl-add",
                LineKind::Del => "scell dl-del",
            };
            let no = if right { num(l.new_no) } else { num(l.old_no) };
            let anchor_no = if right { l.new_no } else { None };
            rsx! {
                div { class: "{cls}",
                    {anchor(anchor_no)}
                    span { class: "ln", "{no}" }
                    span { class: "lc", {segs_rsx(l)} }
                }
            }
        }
    }
}

fn render_split(hunks: &[Hunk]) -> Element {
    let hunks = hunks.to_vec();
    rsx! {
        div { class: "code split",
            for h in hunks {
                div { class: "hunk",
                    {hunk_header(&h.header)}
                    for row in to_rows(&h) {
                        div { class: "srow",
                            {split_cell(row.left.as_ref(), false)}
                            {split_cell(row.right.as_ref(), true)}
                        }
                    }
                }
            }
        }
    }
}
