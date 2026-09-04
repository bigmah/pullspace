//! The panel across the bottom of the code: search hits, references,
//! definitions, and a peek at one.
//!
//! Four things in one panel rather than four panels, because they are all the
//! same answer to the same question — "where else is this?" — and because the
//! room they take is room the file is not getting. It opens when something puts
//! something in it and closes on `✕` or Escape.

use std::path::PathBuf;

use dioxus::prelude::*;

use crate::backend::highlight::Span;
use crate::backend::scan;
use crate::backend::search::{FileHit, Hit};
use crate::backend::symbols::Symbol;

use super::app::St;
use super::ide::{self, Panel, Peek};
use super::panes::{Edge, Splitter};

#[component]
pub fn Bottom() -> Element {
    let st = use_context::<St>();

    let (title, extra, body) = match &*st.panel.read() {
        Panel::Hidden => return rsx! {},
        Panel::Working { title, done, total } => {
            let note = if *total == 0 {
                "Reading…".to_string()
            } else {
                format!("{done}/{total} files")
            };
            (
                title.clone(),
                Some(note),
                rsx! { div { class: "panel-empty", "Searching…" } },
            )
        }
        Panel::Results { title, hits, note } => (title.clone(), note.clone(), hit_list(st, hits)),
        Panel::Files { title, hits } => (title.clone(), None, file_list(st, hits)),
        Panel::Defs { name, syms } => (
            format!("DEFINITIONS  {name} — {}", syms.len()),
            None,
            if syms.is_empty() {
                rsx! {
                    div { class: "panel-empty",
                        "No definition of “{name}” found. It may be defined in a language this index does not read, or in a file too large to have been downloaded."
                    }
                }
            } else {
                sym_list(st, syms)
            },
        ),
        Panel::Peek {
            name,
            syms,
            at,
            body,
        } => {
            let sym = syms.get(*at);
            let where_ = sym
                .map(|s| scan::at_line(&s.path, s.line))
                .unwrap_or_default();
            (
                format!("DEFINITION  {name} — {where_}"),
                (syms.len() > 1).then(|| format!("{} of {}", at + 1, syms.len())),
                peek_body(st, syms, *at, body),
            )
        }
        Panel::NotIndexed(name) => (
            "DEFINITION".to_string(),
            None,
            rsx! {
                div { class: "panel-empty",
                    if name.is_empty() {
                        "Nothing is selected. Double-click an identifier in the code first."
                    } else {
                        "The symbol index is still being built — try “{name}” again in a moment."
                    }
                }
            },
        ),
    };

    let stepping = matches!(&*st.panel.read(), Panel::Peek { syms, .. } if syms.len() > 1);

    rsx! {
        Splitter { edge: Edge::Bottom }
        div { class: "bottom",
            div { class: "panel-hdr",
                span { class: "panel-title", "{title}" }
                if let Some(extra) = extra {
                    span { class: "panel-note", "{extra}" }
                }
                span { class: "spacer" }
                if stepping {
                    button {
                        class: "iconbtn sm",
                        title: "The previous definition of this name",
                        onclick: move |_| ide::peek_step(st, -1),
                        "‹"
                    }
                    button {
                        class: "iconbtn sm",
                        title: "The next definition of this name",
                        onclick: move |_| ide::peek_step(st, 1),
                        "›"
                    }
                }
                button {
                    class: "iconbtn",
                    title: "Close this panel  (Esc)",
                    onclick: move |_| ide::close(st),
                    "✕"
                }
            }
            div { class: "panel-body", "data-keep": "panel", {body} }
        }
    }
}

fn hit_list(st: St, hits: &[Hit]) -> Element {
    if hits.is_empty() {
        return rsx! { div { class: "panel-empty", "No results." } };
    }
    rsx! {
        for (i, hit) in hits.iter().enumerate() {
            {hit_row(st, i, hit)}
        }
    }
}

fn hit_row(st: St, i: usize, hit: &Hit) -> Element {
    let loc = scan::at_line(&hit.path, hit.line);
    let path = hit.path.clone();
    let line = hit.line;
    rsx! {
        div {
            key: "{i}",
            class: "hitrow",
            title: "{loc}",
            onclick: move |_| st.open_at(path.clone(), line),
            span { class: "hitloc", "{loc}" }
            span { class: "hittext", {marked(&hit.text, &hit.marks)} }
        }
    }
}

fn file_list(st: St, hits: &[FileHit]) -> Element {
    if hits.is_empty() {
        return rsx! {
            div { class: "panel-empty",
                "No file of this repository is named that. The pattern is matched against the whole path, so a folder name finds what is inside it."
            }
        };
    }
    rsx! {
        for (i , hit) in hits.iter().enumerate() {
            {file_row(st, i, hit)}
        }
    }
}

/// One matching file: its name, then the path it matched on with the matching
/// parts picked out — the same two columns a line hit has, saying the same two
/// things in the same order.
fn file_row(st: St, i: usize, hit: &FileHit) -> Element {
    let path = hit.path.clone();
    let name = hit
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| hit.text.clone());
    rsx! {
        div {
            key: "{i}",
            class: "hitrow",
            title: "{hit.text}",
            onclick: move |_| st.open_file(path.clone()),
            span { class: "hitloc", "{name}" }
            span { class: "hittext", {marked(&hit.text, &hit.marks)} }
        }
    }
}

/// The matching line, with the parts that matched picked out.
///
/// The ranges are byte offsets into `text` and they arrive sorted and
/// non-overlapping — [`Matcher::scan`](crate::backend::search::Matcher) reads
/// them straight off `find_iter`. Anything else is ignored rather than
/// trusted: this is one row of a search panel, not somewhere to panic.
fn marked(text: &str, marks: &[(usize, usize)]) -> Element {
    let mut parts: Vec<(bool, &str)> = Vec::new();
    let mut at = 0;
    for &(start, end) in marks {
        if start < at
            || end > text.len()
            || !text.is_char_boundary(start)
            || !text.is_char_boundary(end)
        {
            continue;
        }
        if start > at {
            parts.push((false, &text[at..start]));
        }
        parts.push((true, &text[start..end]));
        at = end;
    }
    if at < text.len() {
        parts.push((false, &text[at..]));
    }
    rsx! {
        for (i, (hit, part)) in parts.into_iter().enumerate() {
            if hit {
                span { key: "{i}", class: "hitmark", "{part}" }
            } else {
                span { key: "{i}", "{part}" }
            }
        }
    }
}

fn sym_list(st: St, syms: &[Symbol]) -> Element {
    rsx! {
        for (i, sym) in syms.iter().enumerate() {
            {sym_row(st, i, sym)}
        }
    }
}

fn sym_row(st: St, i: usize, sym: &Symbol) -> Element {
    let loc = scan::at_line(&sym.path, sym.line);
    let path: PathBuf = sym.path.to_path_buf();
    let line = sym.line;
    rsx! {
        div {
            key: "{i}",
            class: "hitrow",
            title: "{loc}",
            onclick: move |_| st.open_at(path.clone(), line),
            span { class: "symkind", "{sym.kind}" }
            span { class: "hitloc", "{loc}" }
            span { class: "hittext", "{sym.preview}" }
        }
    }
}

/// The definition itself, drawn as code, with the defining line marked.
fn peek_body(st: St, syms: &[Symbol], at: usize, body: &Peek) -> Element {
    let Some(sym) = syms.get(at) else {
        return rsx! { div { class: "panel-empty", "Nothing to show." } };
    };
    match body {
        Peek::Loading => rsx! { div { class: "panel-empty", "Reading…" } },
        Peek::Missing => rsx! {
            div { class: "panel-empty",
                "{sym.path.display()} is not in this browser's copy of the repository — it was too large to download, or the clone did not reach it."
            }
        },
        Peek::Ready { start, lines } => {
            let path = sym.path.to_path_buf();
            let line = sym.line;
            let start = *start;
            rsx! {
                div { class: "peek",
                    div { class: "code", {peek_lines(lines, start, line)} }
                    div { class: "peekfoot",
                        button {
                            class: "symbtn",
                            onclick: move |_| st.open_at(path.clone(), line),
                            "Open this file"
                        }
                    }
                }
            }
        }
    }
}

fn peek_lines(lines: &[Vec<Span>], start: usize, defining: usize) -> Element {
    rsx! {
        for (i, spans) in lines.iter().enumerate() {
            {
                let no = start + i;
                let cls = if no == defining { "cl peekdef" } else { "cl" };
                rsx! {
                    div { key: "{no}", class: "{cls}",
                        span { class: "ln", "{no}" }
                        span { class: "lc",
                            for (j, sp) in spans.iter().enumerate() {
                                span { key: "{j}", style: "color:{sp.color}", "{sp.text}" }
                            }
                        }
                    }
                }
            }
        }
    }
}
