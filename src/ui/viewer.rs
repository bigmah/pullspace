use std::path::PathBuf;

use dioxus::prelude::*;

use crate::backend::difftool::{diff_hunks, stats, to_rows, Hunk, Line, LineKind};
use crate::backend::gitio::{head_file, worktree_file};
use crate::backend::highlight::{highlight, Span};
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

    // In PR mode the open file usually arrives already warmed, from the
    // background prefetch or the tree's hover handler. This covers the rest —
    // and is a no-op when the content is cached or on its way.
    use_effect(move || {
        let rel = st.open.read().clone();
        if !st.workspace.read().is_pr() {
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
        if st.workspace.read().is_pr() {
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

    let guard = data.read();
    let (rel, new_side) = match &*guard {
        Pane::Empty => return rsx! { Welcome {} },
        Pane::Loading => {
            return rsx! {
                div { class: "viewer",
                    div { class: "notice", "Loading from GitHub…" }
                }
            }
        }
        Pane::Failed(e) => {
            let msg = e.clone();
            return rsx! {
                div { class: "viewer",
                    div { class: "notice error", "{msg}" }
                }
            };
        }
        Pane::Ready { rel, new, .. } => (rel.clone(), new.clone()),
    };

    let status = st.statuses.read().get(&rel).copied();
    let deleted_note = new_side == FileContent::Absent && status == Some(ChangeKind::Deleted);
    // Go to Definition / Find References run against whatever was indexed —
    // the local repository, or this pull request's own checkout.
    let symbols_enabled = st.scan_root.read().dir().is_some();

    // Only changed files have a diff to show.
    let mode = if status.is_none() {
        ViewMode::Source
    } else {
        *st.view_mode.read()
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
    };

    let mut vm = st.view_mode;
    let mode_btn = |label: &'static str, m: ViewMode, cur: ViewMode, enabled: bool| {
        let cls = if m == cur && enabled {
            "modebtn on"
        } else {
            "modebtn"
        };
        rsx! {
            button {
                class: cls,
                disabled: !enabled,
                onclick: move |_| vm.set(m),
                "{label}"
            }
        }
    };
    let diffable = status.is_some();

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
                {mode_btn("Source", ViewMode::Source, mode, true)}
                {mode_btn("Inline", ViewMode::Inline, mode, diffable)}
                {mode_btn("Split", ViewMode::Split, mode, diffable)}
            }
            if let Some(name) = selected {
                if symbols_enabled {
                    SymBar { name }
                }
            }
            div { class: "codewrap", {body} }
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
            button { class: "symbtn close", onclick: move |_| sel.set(None), "✕" }
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

fn render_inline(hunks: &[Hunk]) -> Element {
    let hunks = hunks.to_vec();
    rsx! {
        div { class: "code",
            for h in hunks {
                div { class: "hunkhdr", "{h.header}" }
                for l in h.lines.iter() {
                    {inline_line(l)}
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
                div { class: "hunkhdr", "{h.header}" }
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
