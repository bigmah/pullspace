use std::path::PathBuf;

use dioxus::prelude::*;

use crate::backend::tree::{
    matching_rows, seed_expansion, visible_rows, FileNode, Row, RowKind,
};

use super::app::St;
use super::panes::{Edge, Splitter};
use super::prcache::ensure_hover;

/// How many matches the filter draws. Typing one letter in a large repository
/// matches thousands of files, and nobody scrolls past the first screen of a
/// filter — they type another letter. The count of what was left out is shown,
/// so the list is never quietly short.
const MATCH_LIMIT: usize = 400;

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

/// Scroll the open file's row back into view.
///
/// A file can be opened from somewhere other than the tree — a search hit, a
/// link in a README, a jump to a definition — and land on a row scrolled miles
/// off. `nearest` moves the list only when it has to, so clicking a row that is
/// already on screen never twitches it.
///
/// A frame late on purpose: the rows this is looking for are written by the
/// same mutation batch that triggered it.
const REVEAL_JS: &str = r#"
    requestAnimationFrame(function () {
        const row = document.querySelector('.tree .row.active');
        if (row) row.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
    });
"#;

/// The rows to draw, and how many matches were cut off the end of them.
#[derive(Clone, PartialEq)]
struct Listing {
    rows: Vec<Row>,
    hidden: usize,
}

#[component]
pub fn FileTreePane() -> Element {
    let st = use_context::<St>();
    let tree = use_context::<Memo<Option<FileNode>>>();
    let mut expanded = st.expanded;
    let mut query = st.tree_filter;
    let mut changes_only = st.changes_only;

    // Fetch whatever the pointer comes to rest on, so opening it is instant.
    // Mounted once, and the listener is on the document, so it goes on working
    // across every rebuild of the tree under it.
    use_future(move || async move {
        let mut hover = document::eval(HOVER_JS);
        while let Ok(path) = hover.recv::<String>().await {
            ensure_hover(st, &PathBuf::from(path));
        }
    });

    // Settle every directory's open/closed state the first time it is seen, and
    // never revisit it — see `seed_expansion`.
    use_effect(move || {
        let held = tree.read();
        let Some(root) = held.as_ref() else { return };
        let mut seeded = expanded.peek().clone();
        let before = seeded.len();
        let first = !*st.tree_seeded.peek();
        seed_expansion(root, first, &mut seeded);
        // Only when something new turned up: an unconditional write would
        // re-render the tree on every refresh that changed nothing.
        if seeded.len() != before {
            expanded.set(seeded);
        }
        if first {
            let mut flag = st.tree_seeded;
            flag.set(true);
        }
    });

    // Open the way to whatever is being read, so a file reached from a search
    // result or a link is where the tree says it is rather than shut inside
    // three folders.
    use_effect(move || {
        let open = st.open.read().clone();
        let Some(path) = open else { return };
        let mut reveal: Vec<PathBuf> = Vec::new();
        {
            let known = expanded.peek();
            let mut dir = path.parent();
            while let Some(d) = dir {
                if known.get(d).copied() != Some(true) {
                    reveal.push(d.to_path_buf());
                }
                dir = d.parent();
            }
        }
        if !reveal.is_empty() {
            let mut w = expanded.write();
            for dir in reveal {
                w.insert(dir, true);
            }
        }
        spawn(async move {
            let _ = document::eval(REVEAL_JS).await;
        });
    });

    let listing = use_memo(move || {
        let needle = query.read().trim().to_string();
        let held = tree.read();
        let Some(root) = held.as_ref() else {
            return Listing { rows: Vec::new(), hidden: 0 };
        };
        if needle.is_empty() {
            return Listing {
                rows: visible_rows(root, &expanded.read()),
                hidden: 0,
            };
        }
        let (rows, total) = matching_rows(root, &needle, MATCH_LIMIT);
        Listing {
            hidden: total - rows.len(),
            rows,
        }
    });

    let filtering = !query.read().trim().is_empty();
    let showing_all = !changes_only();
    let filter_cls = if showing_all {
        "iconbtn sm filter"
    } else {
        "iconbtn sm filter on"
    };
    // A toggle that describes the same thing in both states says nothing about
    // which one it is in.
    let filter_title = if showing_all {
        "Show changed files only"
    } else {
        "Showing changed files only — click to show every file"
    };
    let changed = st.statuses.read().len();
    let changed_title = match changed {
        1 => "1 changed file".to_string(),
        n => format!("{n} changed files"),
    };
    // Why the list is empty is not the same question every time, and "no
    // changed files" under a filter that matched nothing is an answer to a
    // question nobody asked.
    let empty = if !listing.read().rows.is_empty() {
        None
    } else if filtering {
        Some(format!("No file matches “{}”", query.read().trim()))
    } else if !showing_all {
        Some("No changed files".to_string())
    } else {
        Some("Nothing to show".to_string())
    };

    rsx! {
        div { class: "sidebar",
            div { class: "side-hdr",
                span { class: "side-title", "EXPLORER" }
                if changed > 0 {
                    span {
                        class: "side-count",
                        title: "{changed_title}",
                        "{changed}"
                    }
                }
                button {
                    class: filter_cls,
                    title: filter_title,
                    onclick: move |_| {
                        let v = *changes_only.peek();
                        changes_only.set(!v);
                        // The two modes are different trees, and carrying one's
                        // folds over to the other lands you in a changed-files
                        // view with the changed files folded away. Forgotten,
                        // the way down to every change opens again.
                        st.reset_tree_folds();
                    },
                    "Δ"
                }
            }
            div { class: "tree-find",
                input {
                    class: "findbox",
                    r#type: "text",
                    placeholder: "Filter files…",
                    spellcheck: "false",
                    autocomplete: "off",
                    value: "{query}",
                    oninput: move |e| query.set(e.value()),
                    onkeydown: move |e| {
                        if e.key() == Key::Escape {
                            query.set(String::new());
                        }
                    },
                }
                if filtering {
                    button {
                        class: "findclear",
                        title: "Clear the filter (Esc)",
                        onclick: move |_| query.set(String::new()),
                        "✕"
                    }
                }
            }
            div {
                // Filtered, the list is no longer a tree: no folds, no indents,
                // and the guides that explain both would be drawing structure
                // that is not there.
                class: if filtering { "tree flat" } else { "tree" },
                for row in listing.read().rows.iter() {
                    TreeRow { key: "{row.path.display()}", row: row.clone() }
                }
                if let Some(note) = empty {
                    div { class: "tree-empty", "{note}" }
                }
                if listing.read().hidden > 0 {
                    div { class: "tree-more", "+{listing.read().hidden} more — keep typing" }
                }
            }
        }
        // The divider belongs to the pane it moves, so nothing above has to
        // know which panes happen to be on screen.
        Splitter { edge: Edge::Sidebar }
    }
}

#[component]
fn TreeRow(row: Row) -> Element {
    let st = use_context::<St>();
    let mut expanded = st.expanded;
    // The guides run down the chevrons of every directory above this row, and
    // the stylesheet draws them from the count alone.
    let indent = format!(
        "padding-left:{}px;--lvl:{}",
        row.depth * 14 + 8,
        row.depth.saturating_sub(1)
    );

    match row.kind {
        RowKind::Dir { open, changed } => {
            let path = row.path.clone();
            let cls = match (open, row.depth) {
                (true, 0) => "row dir root open",
                (false, 0) => "row dir root",
                (true, _) => "row dir open",
                (false, _) => "row dir",
            };
            rsx! {
                div {
                    class: cls,
                    style: "{indent}",
                    onclick: move |_| {
                        expanded.write().insert(path.clone(), !open);
                    },
                    span { class: "arrow" }
                    span { class: "dirname", "{row.name}" }
                    // Only while it is closed. Open, the badges below say the
                    // same thing in more detail, and a count beside them is one
                    // more thing on screen saying nothing new.
                    if !open && changed > 0 {
                        span { class: "dcount", "{changed}" }
                    }
                }
            }
        }
        RowKind::File { status, hint } => {
            let active = st.open.read().as_deref() == Some(row.path.as_path());
            let cls = if active { "row file active" } else { "row file" };
            let path = row.path.clone();
            let badge = status.map(|s| (s.badge(), s.css()));
            let name_cls = match status {
                Some(s) => format!("fname {}", s.css()),
                None => "fname".to_string(),
            };
            let path_attr = row.path.display().to_string();
            rsx! {
                div {
                    class: cls,
                    style: "{indent}",
                    // What the hover listener reads to start the fetch on the way to
                    // the click. An attribute rather than an `onmouseenter`, because
                    // a handler here would put a blocking round trip to Rust in the
                    // way of the highlight — see `HOVER_JS`.
                    "data-path": "{path_attr}",
                    onclick: move |_| st.open_file(path.clone()),
                    span { class: "arrow" }
                    span { class: "{name_cls}", "{row.name}" }
                    if let Some(dir) = hint {
                        span { class: "fpath", "{dir}" }
                    }
                    if let Some((b, c)) = badge {
                        span { class: "badge {c}", "{b}" }
                    }
                }
            }
        }
    }
}
