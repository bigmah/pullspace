use std::collections::HashMap;
use std::path::PathBuf;

use dioxus::prelude::*;

use crate::backend::auth::{Token, TokenSource};
use crate::backend::github::{statuses_of, PrDetail, PrSummary, RepoRef};
use crate::backend::gitio::{discover_root, load_statuses};
use crate::backend::search::{search_repo, text_query, word_query, Hit};
use crate::backend::symbols::{build_index, find_definitions, Symbol};
use crate::backend::tree::{build_tree, build_tree_from_paths, filter_changed, ChangeKind, FileNode};
use crate::backend::FileContent;

use super::bottom::Bottom;
use super::filetree::FileTreePane;
use super::github::GhPanel;
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

/// What the explorer and viewer are showing: the local working tree, or a pull
/// request fetched from GitHub.
#[derive(Clone, PartialEq)]
pub enum Workspace {
    Local,
    Pr(Box<PrDetail>),
}

impl Workspace {
    pub fn pr(&self) -> Option<&PrDetail> {
        match self {
            Workspace::Pr(pr) => Some(pr),
            Workspace::Local => None,
        }
    }

    pub fn is_pr(&self) -> bool {
        matches!(self, Workspace::Pr(_))
    }
}

/// Sign-in status, as far as the UI needs to know. The token itself lives in a
/// separate signal so it is never part of anything rendered.
#[derive(Clone, PartialEq)]
pub enum Account {
    /// Startup: looking for a usable token.
    Checking,
    SignedOut,
    /// Device flow in progress — the user is entering `user_code` on GitHub.
    Connecting {
        user_code: String,
        verification_uri: String,
        note: String,
    },
    SignedIn {
        login: String,
        source: TokenSource,
    },
    Failed(String),
}

/// Pull request list for the repo currently typed into the picker.
#[derive(Clone, PartialEq)]
pub enum PrList {
    Idle,
    Loading,
    Ready { repo: RepoRef, items: Vec<PrSummary> },
    Failed(String),
}

/// Both sides of one file in a pull request, fetched on demand.
#[derive(Clone, PartialEq)]
pub enum PrFileState {
    Loading,
    Ready { base: FileContent, head: FileContent },
    Failed(String),
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

    // --- GitHub ---
    /// Never rendered; `account` is what the UI reads.
    pub token: Signal<Option<Token>>,
    pub account: Signal<Account>,
    pub gh_open: Signal<bool>,
    pub client_id_input: Signal<String>,
    pub repo_input: Signal<String>,
    pub prs: Signal<PrList>,
    pub workspace: Signal<Workspace>,
    /// Per-file base/head content for the open PR.
    pub pr_files: Signal<HashMap<PathBuf, PrFileState>>,
}

impl St {
    pub fn root_path(&self) -> PathBuf {
        self.root.peek().clone()
    }

    pub fn token_value(&self) -> Option<String> {
        self.token.peek().as_ref().map(|t| t.value.clone())
    }

    pub fn refresh(&self) {
        // A pull request is a fixed snapshot; reloading git status would
        // replace its file list with the local working tree's.
        if self.workspace.peek().is_pr() {
            return;
        }
        let root = self.root_path();
        let mut statuses = self.statuses;
        statuses.set(load_statuses(&root));
        let mut tick = self.refresh_tick;
        let v = *tick.peek();
        tick.set(v + 1);
    }

    /// Clear everything tied to whatever was open: the file, the symbol
    /// selection, panels, and the tree's expansion state.
    fn clear_view(&self) {
        let mut open = self.open;
        open.set(None);
        let mut sel = self.selected;
        sel.set(None);
        let mut ps = self.pending_scroll;
        ps.set(None);
        let mut bottom = self.bottom;
        bottom.set(BottomPanel::Hidden);
        let mut expanded = self.expanded;
        expanded.set(HashMap::new());
    }

    /// Point the app at another repository, clearing everything tied to the old
    /// one. `path` may be any directory inside the repo; a non-repo directory is
    /// browsable too, just without statuses or diffs.
    pub fn open_repo(&self, path: PathBuf) {
        let root = discover_root(&path).unwrap_or(path);
        if root == *self.root.peek() && !self.workspace.peek().is_pr() {
            self.refresh();
            return;
        }
        let mut r = self.root;
        r.set(root.clone());

        self.leave_pr_state();
        self.clear_view();
        let mut search_text = self.search_text;
        search_text.set(String::new());
        // Dropped so the top bar shows "indexing…" while the new index builds.
        let mut index = self.index;
        index.set(None);

        self.refresh();
    }

    fn leave_pr_state(&self) {
        let mut ws = self.workspace;
        ws.set(Workspace::Local);
        let mut cache = self.pr_files;
        cache.set(HashMap::new());
    }

    /// Show a pull request instead of the working tree. `statuses` becomes the
    /// PR's change list, so the tree, badges and viewer need no special casing.
    pub fn enter_pr(&self, pr: PrDetail) {
        self.clear_view();
        let mut cache = self.pr_files;
        cache.set(HashMap::new());
        let mut statuses = self.statuses;
        statuses.set(statuses_of(&pr.files));
        let mut ws = self.workspace;
        ws.set(Workspace::Pr(Box::new(pr)));
        let mut gh = self.gh_open;
        gh.set(false);
        let mut tick = self.refresh_tick;
        let v = *tick.peek();
        tick.set(v + 1);
    }

    /// Back to the local working tree.
    pub fn leave_pr(&self) {
        self.leave_pr_state();
        self.clear_view();
        self.refresh();
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
        // Search walks the working tree, which a PR's files are not part of.
        if self.workspace.peek().is_pr() {
            return;
        }
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

        // Pre-fill the PR picker from the clone's own remote when there is one.
        let repo_hint = crate::backend::github::repo_from_local(&root)
            .map(|r| r.to_string())
            .unwrap_or_default();

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

            token: Signal::new(None),
            account: Signal::new(Account::Checking),
            gh_open: Signal::new(false),
            client_id_input: Signal::new(
                crate::backend::auth::client_id().unwrap_or_default(),
            ),
            repo_input: Signal::new(repo_hint),
            prs: Signal::new(PrList::Idle),
            workspace: Signal::new(Workspace::Local),
            pr_files: Signal::new(HashMap::new()),
        }
    });

    let tree: Memo<Option<FileNode>> = use_memo(move || {
        st.refresh_tick.read();
        let statuses = st.statuses.read().clone();
        let root = match st.workspace.read().pr() {
            Some(pr) => build_tree_from_paths(
                &format!("{} #{}", pr.repo, pr.number),
                pr.tree.iter().map(|p| p.as_path()),
                &statuses,
            ),
            None => build_tree(&st.root.read(), &statuses),
        };
        if *st.changes_only.read() {
            filter_changed(&root)
        } else {
            Some(root)
        }
    });
    use_context_provider(|| tree);

    // Resolve a token once at startup: a stored sign-in, else GITHUB_TOKEN,
    // else the gh CLI. Verifying it costs one API call and tells us the login.
    use_future(move || async move {
        let found = tokio::task::spawn_blocking(crate::backend::auth::find_token)
            .await
            .ok()
            .flatten();
        let Some(tok) = found else {
            let mut acct = st.account;
            acct.set(Account::SignedOut);
            return;
        };
        let value = tok.value.clone();
        let source = tok.source;
        let login = tokio::task::spawn_blocking(move || {
            crate::backend::github::viewer_login(&value)
        })
        .await
        .ok()
        .and_then(|r| r.ok());

        let mut acct = st.account;
        match login {
            Some(login) => {
                let mut t = st.token;
                t.set(Some(tok));
                acct.set(Account::SignedIn { login, source });
            }
            // A stale token is the same as none, but say so rather than
            // silently showing a signed-out chip.
            None => acct.set(Account::Failed(format!(
                "The {} token was rejected by GitHub.",
                source.label()
            ))),
        }
    });

    // Build the symbol index off the UI thread, once at startup and again for
    // each repo opened afterwards. Reading `root` here is what re-triggers it;
    // an in-flight build for the previous repo is cancelled.
    let _ = use_resource(move || {
        let root = st.root.read().clone();
        async move {
            let idx = tokio::task::spawn_blocking(move || build_index(&root))
                .await
                .unwrap_or_default();
            let mut sig = st.index;
            sig.set(Some(idx));
        }
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
            if *st.gh_open.read() {
                GhPanel {}
            }
        }
    }
}
