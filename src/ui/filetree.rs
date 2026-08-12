use std::path::PathBuf;

use dioxus::prelude::*;

use crate::backend::tree::FileNode;

use super::app::St;
use super::panes::{Edge, Splitter};
use super::prcache::ensure_path;

/// One listener for the whole tree, living in the webview, reporting the row the
/// pointer has settled on.
///
/// A Dioxus handler cannot do this job. Every event one receives is delivered
/// over a *synchronous* request that stops the webview — and painting with it —
/// until Rust has answered. Hung off every row, that is a stall per row crossed,
/// which is the one thing a hover highlight cannot afford. This costs nothing to
/// cross, says nothing until the pointer stops, and what it does say goes back
/// asynchronously.
const HOVER_JS: &str = r#"
    let timer = null;
    let last = null;
    document.addEventListener('mouseover', function (e) {
        const row = e.target.closest('.row.file[data-path]');
        const path = row ? row.getAttribute('data-path') : null;
        if (path === last) return;
        last = path;
        clearTimeout(timer);
        if (path === null) return;
        // Long enough that sweeping past a row on the way somewhere else is
        // free, short enough to still be ahead of the click it precedes.
        timer = setTimeout(function () { dioxus.send(path); }, 80);
    });
"#;

#[component]
pub fn FileTreePane() -> Element {
    let st = use_context::<St>();

    // Fetch whatever the pointer comes to rest on, so opening it is instant.
    // Mounted once, and the listener is on the document, so it goes on working
    // across every rebuild of the tree under it.
    use_future(move || async move {
        let mut hover = document::eval(HOVER_JS);
        while let Ok(path) = hover.recv::<String>().await {
            ensure_path(st, &PathBuf::from(path));
        }
    });

    let tree = use_context::<Memo<Option<FileNode>>>();
    let mut changes_only = st.changes_only;
    let showing_all = !changes_only();
    let filter_cls = if showing_all {
        "iconbtn filter"
    } else {
        "iconbtn filter on"
    };
    // A toggle that describes the same thing in both states says nothing about
    // which one it is in.
    let filter_title = if showing_all {
        "Show changed files only"
    } else {
        "Showing changed files only — click to show every file"
    };

    let body = match tree.read().as_ref() {
        Some(root) => rsx! {
            TreeNode { node: root.clone(), depth: 0 }
        },
        None => rsx! {
            div { class: "tree-empty", "No changed files" }
        },
    };

    rsx! {
        div { class: "sidebar",
            div { class: "side-hdr",
                span { class: "side-title", "EXPLORER" }
                button {
                    class: filter_cls,
                    title: filter_title,
                    onclick: move |_| {
                        let v = *changes_only.peek();
                        changes_only.set(!v);
                    },
                    "Δ"
                }
            }
            div { class: "tree", {body} }
        }
        // The divider belongs to the pane it moves, so nothing above has to
        // know which panes happen to be on screen.
        Splitter { edge: Edge::Sidebar }
    }
}

#[component]
fn TreeNode(node: FileNode, depth: usize) -> Element {
    let st = use_context::<St>();
    let indent = format!("padding-left:{}px", depth * 14 + 8);

    if node.is_dir {
        let default_open = depth == 0 || node.contains_changes;
        let is_open = st
            .expanded
            .read()
            .get(&node.path)
            .copied()
            .unwrap_or(default_open);
        let arrow = if is_open { "▾" } else { "▸" };
        let path_key = node.path.clone();
        let mut expanded = st.expanded;
        rsx! {
            div {
                class: "row dir",
                style: "{indent}",
                onclick: move |_| {
                    let cur = expanded
                        .peek()
                        .get(&path_key)
                        .copied()
                        .unwrap_or(default_open);
                    expanded.write().insert(path_key.clone(), !cur);
                },
                span { class: "arrow", "{arrow}" }
                span { class: "dirname", "{node.name}" }
                if node.contains_changes {
                    span { class: "chdot", "●" }
                }
            }
            if is_open {
                for child in node.children.iter() {
                    TreeNode {
                        key: "{child.path.display()}",
                        node: child.clone(),
                        depth: depth + 1,
                    }
                }
            }
        }
    } else {
        let active = st.open.read().as_deref() == Some(node.path.as_path());
        let cls = if active { "row file active" } else { "row file" };
        let path_key = node.path.clone();
        let badge = node.status.map(|s| (s.badge(), s.css()));
        let name_cls = match node.status {
            Some(s) => format!("fname {}", s.css()),
            None => "fname".to_string(),
        };
        let path_attr = node.path.display().to_string();
        rsx! {
            div {
                class: cls,
                style: "{indent}",
                // What the hover listener reads to start the fetch on the way to
                // the click. An attribute rather than an `onmouseenter`, because
                // a handler here would put a blocking round trip to Rust in the
                // way of the highlight — see `HOVER_JS`.
                "data-path": "{path_attr}",
                onclick: move |_| st.open_file(path_key.clone()),
                span { class: "arrow", "" }
                span { class: "{name_cls}", "{node.name}" }
                if let Some((b, c)) = badge {
                    span { class: "badge {c}", "{b}" }
                }
            }
        }
    }
}
