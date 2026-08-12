use dioxus::prelude::*;

use crate::backend::search::{Hit, MAX_HITS};
use crate::backend::symbols::Symbol;

use super::app::{BottomPanel, St};
use super::panes::{Edge, Splitter};

#[component]
pub fn Bottom() -> Element {
    let st = use_context::<St>();
    let panel = st.bottom.read().clone();
    let mut bottom = st.bottom;

    let (title, body) = match panel {
        BottomPanel::Hidden => return rsx! {},
        BottomPanel::Working { title } => (
            title,
            rsx! { div { class: "panel-empty", "Searching…" } },
        ),
        BottomPanel::Search { query, hits } => {
            let capped = hits.len() >= MAX_HITS;
            let title = format!(
                "SEARCH  “{query}” — {} hit{}{}",
                hits.len(),
                if hits.len() == 1 { "" } else { "s" },
                if capped { " (capped)" } else { "" },
            );
            (title, hit_list(st, hits))
        }
        BottomPanel::Refs { name, hits } => {
            let title = format!(
                "REFERENCES  {name} — {} hit{}",
                hits.len(),
                if hits.len() == 1 { "" } else { "s" },
            );
            (title, hit_list(st, hits))
        }
        BottomPanel::Defs {
            name,
            syms,
            indexed,
        } => {
            let title = format!("DEFINITIONS  {name} — {}", syms.len());
            let body = if !indexed {
                rsx! { div { class: "panel-empty", "Symbol index is still building — try again in a moment." } }
            } else if syms.is_empty() {
                rsx! { div { class: "panel-empty", "No definition found for “{name}”." } }
            } else {
                sym_list(st, syms)
            };
            (title, body)
        }
    };

    rsx! {
        Splitter { edge: Edge::Bottom }
        div { class: "bottom",
            div { class: "panel-hdr",
                span { class: "panel-title", "{title}" }
                span { class: "spacer" }
                button {
                    class: "iconbtn",
                    title: "Close this panel",
                    onclick: move |_| bottom.set(BottomPanel::Hidden),
                    "✕"
                }
            }
            div { class: "panel-body", {body} }
        }
    }
}

fn hit_list(st: St, hits: Vec<Hit>) -> Element {
    if hits.is_empty() {
        return rsx! { div { class: "panel-empty", "No results." } };
    }
    rsx! {
        for (i, hit) in hits.into_iter().enumerate() {
            {hit_row(st, i, hit)}
        }
    }
}

fn hit_row(st: St, i: usize, hit: Hit) -> Element {
    let loc = format!("{}:{}", hit.path.display(), hit.line);
    let path = hit.path.clone();
    let line = hit.line;
    rsx! {
        div {
            key: "{i}",
            class: "hitrow",
            onclick: move |_| st.open_at(path.clone(), line),
            span { class: "hitloc", "{loc}" }
            span { class: "hittext", "{hit.text}" }
        }
    }
}

fn sym_list(st: St, syms: Vec<Symbol>) -> Element {
    rsx! {
        for (i, sym) in syms.into_iter().enumerate() {
            {sym_row(st, i, sym)}
        }
    }
}

fn sym_row(st: St, i: usize, sym: Symbol) -> Element {
    let loc = format!("{}:{}", sym.path.display(), sym.line);
    let path = sym.path.clone();
    let line = sym.line;
    rsx! {
        div {
            key: "{i}",
            class: "hitrow",
            onclick: move |_| st.open_at(path.clone(), line),
            span { class: "symkind", "{sym.kind}" }
            span { class: "hitloc", "{loc}" }
            span { class: "hittext", "{sym.preview}" }
        }
    }
}
