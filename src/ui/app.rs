use std::collections::HashMap;
use std::path::PathBuf;

use dioxus::prelude::*;

use crate::backend::gitio::{discover_root, load_statuses};
use crate::backend::search::{search_repo, text_query, word_query, Hit};
use crate::backend::symbols::{build_index, find_definitions, Symbol};
use crate::backend::tree::{build_tree, filter_changed, ChangeKind, FileNode};

use super::bottom::Bottom;
use super::filetree::FileTreePane;
use super::topbar::TopBar;
use super::viewer::Viewer;

static CSS: &str = include_str!("../../assets/style.css");

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Source,
    Inline,
    Split,
}

#[derive(Clone, PartialEq)]
pub enum BottomPanel {
    Hidden,
    Search {
        query: String,
        hits: Vec<Hit>,
    },
    Refs {
        name: String,
        hits: Vec<Hit>,
    },
    Defs {
        name: String,
        syms: Vec<Symbol>,
        indexed: bool,
    },
}

/// All app state, shared through context. Every field is a Copy signal.
#[derive(Clone, Copy)]
pub struct St {
    pub root: Signal<PathBuf>,
    pub statuses: Signal<HashMap<PathBuf, ChangeKind>>,
    pub open: Signal<Option<PathBuf>>,
    pub view_mode: Signal<ViewMode>,
    pub bottom: Signal<BottomPanel>,
    pub selected: Signal<Option<String>>,
    pub pending_scroll: Signal<Option<usize>>,
    pub index: Signal<Option<Vec<Symbol>>>,
    pub expanded: Signal<HashMap<PathBuf, bool>>,
    pub changes_only: Signal<bool>,
    pub refresh_tick: Signal<u32>,
    pub search_text: Signal<String>,
}

impl St {
    pub fn root_path(&self) -> PathBuf {
        self.root.peek().clone()
    }

    pub fn refresh(&self) {
        let root = self.root_path();
        let mut statuses = self.statuses;
        statuses.set(load_statuses(&root));
        let mut tick = self.refresh_tick;
        let v = *tick.peek();
        tick.set(v + 1);
    }

    /// Open a file from the tree: changed files land in split-diff view.
    pub fn open_file(&self, rel: PathBuf) {
        let changed = self.statuses.peek().get(&rel).is_some();
        let mut vm = self.view_mode;
        vm.set(if changed { ViewMode::Split } else { ViewMode::Source });
        let mut sel = self.selected;
        sel.set(None);
        let mut open = self.open;
        open.set(Some(rel));
    }

    /// Jump to a specific line (search result, definition, reference).
    pub fn open_at(&self, rel: PathBuf, line: usize) {
        let mut vm = self.view_mode;
        vm.set(ViewMode::Source);
        let mut sel = self.selected;
        sel.set(None);
        let mut open = self.open;
        open.set(Some(rel));
        let mut ps = self.pending_scroll;
        ps.set(Some(line));
    }

    pub fn do_search(&self) {
        let q = self.search_text.peek().clone();
        let Some(re) = text_query(&q) else { return };
        let hits = search_repo(&self.root_path(), &re);
        let mut b = self.bottom;
        b.set(BottomPanel::Search { query: q, hits });
    }

    pub fn goto_def(&self, name: &str) {
        let (syms, indexed) = {
            let idx = self.index.peek();
            match idx.as_ref() {
                Some(i) => (find_definitions(i, name), true),
                None => (Vec::new(), false),
            }
        };
        if syms.len() == 1 {
            let s = &syms[0];
            self.open_at(s.path.clone(), s.line);
        } else {
            let mut b = self.bottom;
            b.set(BottomPanel::Defs {
                name: name.to_string(),
                syms,
                indexed,
            });
        }
    }

    pub fn find_refs(&self, name: &str) {
        let Some(re) = word_query(name) else { return };
        let hits = search_repo(&self.root_path(), &re);
        let mut b = self.bottom;
        b.set(BottomPanel::Refs {
            name: name.to_string(),
            hits,
        });
    }
}

#[component]
pub fn App() -> Element {
    let st = use_context_provider(|| {
        let positional: Vec<String> = std::env::args()
            .skip(1)
            .filter(|a| !a.starts_with("--"))
            .collect();
        let arg = positional
            .first()
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let root = discover_root(&arg).unwrap_or(arg);
        let statuses = load_statuses(&root);

        // Optional second positional arg: open this file (repo-relative) on launch.
        let open = positional.get(1).map(PathBuf::from);
        let mode_flag = std::env::args().find_map(|a| {
            a.strip_prefix("--mode=").map(|m| match m {
                "inline" => ViewMode::Inline,
                "split" => ViewMode::Split,
                _ => ViewMode::Source,
            })
        });
        let view_mode = mode_flag.unwrap_or_else(|| {
            match open.as_ref().map(|p| statuses.contains_key(p)) {
                Some(true) => ViewMode::Split,
                _ => ViewMode::Source,
            }
        });

        St {
            root: Signal::new(root),
            statuses: Signal::new(statuses),
            open: Signal::new(open),
            view_mode: Signal::new(view_mode),
            bottom: Signal::new(BottomPanel::Hidden),
            selected: Signal::new(None),
            pending_scroll: Signal::new(None),
            index: Signal::new(None),
            expanded: Signal::new(HashMap::new()),
            changes_only: Signal::new(false),
            refresh_tick: Signal::new(0),
            search_text: Signal::new(String::new()),
        }
    });

    let tree: Memo<Option<FileNode>> = use_memo(move || {
        st.refresh_tick.read();
        let statuses = st.statuses.read().clone();
        let root = build_tree(&st.root.peek(), &statuses);
        if *st.changes_only.read() {
            filter_changed(&root)
        } else {
            Some(root)
        }
    });
    use_context_provider(|| tree);

    // Build the symbol index off the UI thread once at startup.
    use_future(move || async move {
        let root = st.root_path();
        let idx = tokio::task::spawn_blocking(move || build_index(&root))
            .await
            .unwrap_or_default();
        let mut sig = st.index;
        sig.set(Some(idx));
    });

    rsx! {
        style { dangerous_inner_html: CSS }
        div { class: "app",
            TopBar {}
            div { class: "main",
                FileTreePane {}
                div { class: "rightcol",
                    Viewer {}
                    Bottom {}
                }
            }
        }
    }
}
