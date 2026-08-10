use dioxus::prelude::*;

use crate::backend::tree::FileNode;

use super::app::St;

#[component]
pub fn FileTreePane() -> Element {
    let st = use_context::<St>();
    let tree = use_context::<Memo<Option<FileNode>>>();
    let mut changes_only = st.changes_only;
    let filter_cls = if changes_only() {
        "iconbtn filter on"
    } else {
        "iconbtn filter"
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
                    title: "Show changed files only",
                    onclick: move |_| {
                        let v = *changes_only.peek();
                        changes_only.set(!v);
                    },
                    "Δ"
                }
            }
            div { class: "tree", {body} }
        }
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
        rsx! {
            div {
                class: cls,
                style: "{indent}",
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
