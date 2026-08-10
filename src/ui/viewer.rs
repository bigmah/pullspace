use dioxus::prelude::*;

use crate::backend::difftool::{diff_hunks, stats, to_rows, Hunk, Line, LineKind};
use crate::backend::gitio::{head_file, HeadFile};
use crate::backend::highlight::{highlight, Span};
use crate::backend::tree::ChangeKind;

use super::app::{St, ViewMode};

const BIG_FILE_BYTES: usize = 400_000;
const BIG_FILE_LINES: usize = 6_000;

#[derive(Clone, PartialEq)]
enum Loaded {
    Text(String),
    Binary,
    Missing,
}

#[derive(Clone, PartialEq)]
enum SourceLines {
    Colored(Vec<Vec<Span>>),
    Plain(Vec<String>),
}

fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|&b| b == 0)
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

    // Load worktree + HEAD content whenever the open file or refresh tick changes.
    let data = use_memo(move || {
        st.refresh_tick.read();
        let rel = st.open.read().clone()?;
        let root = st.root_path();
        let work = match std::fs::read(root.join(&rel)) {
            Ok(bytes) => {
                if looks_binary(&bytes) {
                    Loaded::Binary
                } else {
                    Loaded::Text(String::from_utf8_lossy(&bytes).into_owned())
                }
            }
            Err(_) => Loaded::Missing,
        };
        let head = head_file(&root, &rel);
        Some((rel, work, head))
    });

    let hunks = use_memo(move || {
        let guard = data.read();
        let (rel, work, head) = guard.as_ref()?;
        let status = st.statuses.read().get(rel).copied()?;
        let old = match status {
            ChangeKind::Untracked | ChangeKind::Added => String::new(),
            _ => match head {
                HeadFile::Text(t) => t.clone(),
                HeadFile::Binary => return None,
                HeadFile::Absent => String::new(),
            },
        };
        let new = match work {
            Loaded::Text(t) => t.clone(),
            Loaded::Missing => String::new(),
            Loaded::Binary => return None,
        };
        Some(diff_hunks(&old, &new))
    });

    let source_lines = use_memo(move || {
        let guard = data.read();
        let (rel, work, head) = guard.as_ref()?;
        let text = match work {
            Loaded::Text(t) => t.clone(),
            Loaded::Missing => match head {
                HeadFile::Text(t) => t.clone(),
                _ => return None,
            },
            Loaded::Binary => return None,
        };
        let line_count = text.lines().count();
        if text.len() > BIG_FILE_BYTES || line_count > BIG_FILE_LINES {
            Some(SourceLines::Plain(text.lines().map(String::from).collect()))
        } else {
            Some(SourceLines::Colored(highlight(rel, &text)))
        }
    });

    let guard = data.read();
    let Some((rel, work, _head)) = guard.as_ref() else {
        return rsx! {
            div { class: "viewer",
                div { class: "welcome",
                    div { class: "welcome-logo", "pullspace" }
                    div { class: "welcome-sub", "a lightweight diff viewer" }
                    div { class: "welcome-hint", "Pick a file on the left — changed files open as diffs." }
                    div { class: "welcome-hint", "Click an identifier for Go to Definition / Find References." }
                }
            }
        };
    };
    let rel = rel.clone();
    let status = st.statuses.read().get(&rel).copied();
    let deleted_note = matches!(work, Loaded::Missing) && status == Some(ChangeKind::Deleted);

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
            Some(SourceLines::Colored(lines)) => render_colored(st, lines),
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
                    span { class: "delnote", "deleted — showing HEAD" }
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
                SymBar { name }
            }
            div { class: "codewrap", {body} }
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

fn token_span(st: St, color: String, is_word: bool, tok: String) -> Element {
    if is_word && clickable(&tok) {
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

fn render_colored(st: St, lines: &[Vec<Span>]) -> Element {
    let lines = lines.to_vec();
    rsx! {
        div { class: "code",
            for (i, spans) in lines.into_iter().enumerate() {
                div { class: "cl", id: "L{i + 1}",
                    span { class: "ln", "{i + 1}" }
                    span { class: "lc",
                        for sp in spans {
                            for (is_word, tok) in tokenize(&sp.text) {
                                {token_span(st, sp.color.clone(), is_word, tok)}
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
