use std::collections::HashMap;
use std::path::PathBuf;

use dioxus::prelude::*;

use crate::backend::auth::{Token, TokenSource};
use crate::backend::github::{statuses_of, PrDetail, PrSummary, RepoRef, RepoView, Thread};
use crate::backend::gitio::{discover_root, load_statuses};
use crate::backend::search::{search_repo, text_query, word_query, Hit};
use crate::backend::symbols::{build_index, find_definitions, Symbol};
use crate::backend::tree::{build_tree, build_tree_from_paths, filter_changed, ChangeKind, FileNode};
use crate::backend::FileContent;

use super::bottom::Bottom;
use super::conversation::ConvPane;
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
    /// An HTML file drawn as the page it describes. Offered for HTML only.
    Preview,
}

#[derive(Clone, PartialEq)]
pub enum BottomPanel {
    Hidden,
    /// Dispatched, not yet finished. Searching walks a whole directory off the
    /// UI thread — on a pull request just checked out, that is a real
    /// repository's worth of files, not a handful.
    Working {
        title: String,
    },
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

/// What the explorer and viewer are showing: the local working tree, a pull
/// request fetched from GitHub, or a GitHub repository browsed on its own.
#[derive(Clone, PartialEq)]
pub enum Workspace {
    Local,
    Pr(Box<PrDetail>),
    /// A repository with no pull request in view — because it has none open,
    /// or because reading the code is the point.
    Repo(Box<RepoView>),
}

impl Workspace {
    pub fn pr(&self) -> Option<&PrDetail> {
        match self {
            Workspace::Pr(pr) => Some(pr),
            _ => None,
        }
    }

    pub fn repo(&self) -> Option<&RepoView> {
        match self {
            Workspace::Repo(view) => Some(view),
            _ => None,
        }
    }

    /// Anything read from GitHub rather than from the working tree. The two
    /// remote cases share a file cache, a source and a scan root, so most of
    /// the app only needs to know which side of this line it is on.
    pub fn is_remote(&self) -> bool {
        !matches!(self, Workspace::Local)
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
    /// Carries what is happening — opening a PR can involve a clone.
    Loading(String),
    Ready { repo: RepoRef, items: Vec<PrSummary> },
    Failed(String),
}

/// Where search and the symbol index look — and, when they cannot look
/// anywhere, why not.
///
/// The reason travels with the state rather than being dropped, because the
/// only thing worse than a pull request that cannot be searched is one that
/// cannot be searched without saying so.
#[derive(Clone, PartialEq)]
pub enum ScanRoot {
    /// A directory to walk: the repository, or a pull request checked out.
    Dir(PathBuf),
    /// Nothing on disk, and what to tell the user about it.
    Unavailable(String),
}

impl ScanRoot {
    pub fn dir(&self) -> Option<&PathBuf> {
        match self {
            ScanRoot::Dir(p) => Some(p),
            ScanRoot::Unavailable(_) => None,
        }
    }

    pub fn why(&self) -> Option<&str> {
        match self {
            ScanRoot::Unavailable(reason) => Some(reason),
            ScanRoot::Dir(_) => None,
        }
    }
}

/// Where the open pull request's file contents are read from.
#[derive(Clone, PartialEq)]
pub enum PrSource {
    /// One HTTP request per file.
    Api,
    /// A git repository on disk — reads are local and effectively instant.
    Local {
        git_dir: PathBuf,
        /// True when this is a clone the user already had.
        borrowed: bool,
    },
}

/// What the conversation pane has to show for the open pull request.
///
/// The description arrives with the pull request itself, so this is only about
/// the comments — the pane has something to read either way.
#[derive(Clone, PartialEq)]
pub enum Conversation {
    Loading,
    Ready(Box<Thread>),
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
    /// What search and the symbol index walk: the repository itself, or a pull
    /// request's head commit checked out on disk. Never the user's own
    /// repository while a pull request is open — that would answer a question
    /// they did not ask.
    pub scan_root: Signal<ScanRoot>,
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
    pub pr_source: Signal<PrSource>,
    /// The open PR's comments. Folded away rather than unmounted, so the pane
    /// comes back instantly and without a second trip to GitHub.
    pub conv: Signal<Conversation>,
    pub conv_open: Signal<bool>,
}

impl St {
    pub fn root_path(&self) -> PathBuf {
        self.root.peek().clone()
    }

    pub fn token_value(&self) -> Option<String> {
        self.token.peek().as_ref().map(|t| t.value.clone())
    }

    /// The credential to send to GitHub, empty when signed out. GitHub serves
    /// public repositories anonymously (at a much lower rate limit), so being
    /// signed out limits what you can reach rather than blocking the app.
    pub fn api_token(&self) -> String {
        self.token_value().unwrap_or_default()
    }

    pub fn refresh(&self) {
        // What came from GitHub is a fixed snapshot; reloading git status would
        // replace its file list with the local working tree's.
        if self.workspace.peek().is_remote() {
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
    /// selection, panels, the tree's expansion state, and the symbol index.
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
        // The results are gone, so the term that produced them has to go too:
        // left behind it reads as a search still in effect, and it covers the
        // placeholder that explains when a pull request cannot be searched.
        let mut search_text = self.search_text;
        search_text.set(String::new());
    }

    /// Point search and the symbol index at another directory, dropping the
    /// index built for the last one.
    ///
    /// A no-op when the directory is unchanged: reloading a pull request whose
    /// head has not moved lands on the same checkout, and should not throw away
    /// a perfectly good index to rebuild it identically.
    fn set_scan_root(&self, next: ScanRoot) {
        if *self.scan_root.peek() == next {
            return;
        }
        let mut scan = self.scan_root;
        scan.set(next);
        // Dropped so the top bar shows "indexing…" while the new one builds.
        let mut index = self.index;
        index.set(None);
    }

    /// Point the app at another repository, clearing everything tied to the old
    /// one. `path` may be any directory inside the repo; a non-repo directory is
    /// browsable too, just without statuses or diffs.
    pub fn open_repo(&self, path: PathBuf) {
        let root = discover_root(&path).unwrap_or(path);
        if root == *self.root.peek() && !self.workspace.peek().is_remote() {
            self.refresh();
            return;
        }
        let mut r = self.root;
        r.set(root.clone());

        self.leave_remote_state();
        self.clear_view();

        self.refresh();
    }

    fn leave_remote_state(&self) {
        let mut ws = self.workspace;
        ws.set(Workspace::Local);
        let mut cache = self.pr_files;
        cache.set(HashMap::new());
        // Back to walking the repository on disk.
        self.set_scan_root(ScanRoot::Dir(self.root_path()));
    }

    /// Show a pull request instead of the working tree. `statuses` becomes the
    /// PR's change list, so the tree, badges and viewer need no special casing.
    ///
    /// `checkout` is the PR's head commit as files on disk, when one could be
    /// made; it takes the place of the working tree for search and the symbol
    /// index, so those work on a pull request exactly as they do locally.
    pub fn enter_pr(&self, pr: PrDetail, source: PrSource, checkout: ScanRoot) {
        let reload = self
            .workspace
            .peek()
            .pr()
            .is_some_and(|open| open.repo == pr.repo && open.number == pr.number);
        self.enter_remote(Workspace::Pr(Box::new(pr)), reload, source, checkout);
    }

    /// Show a repository on its own — no pull request, so no changed files and
    /// no diffs, just the code at `view.head_sha`.
    pub fn enter_repo(&self, view: RepoView, source: PrSource, checkout: ScanRoot) {
        let reload = self
            .workspace
            .peek()
            .repo()
            .is_some_and(|open| open.repo == view.repo);
        self.enter_remote(Workspace::Repo(Box::new(view)), reload, source, checkout);
    }

    /// Swap in something fetched from GitHub, whichever kind it is.
    ///
    /// `reload` means the same thing is already open, so the file being read
    /// and the tree as it was expanded are kept — resetting the view out from
    /// under someone who asked for fresh data is not what they asked for. Only
    /// what describes the old commit is dropped.
    fn enter_remote(&self, ws: Workspace, reload: bool, source: PrSource, checkout: ScanRoot) {
        if reload {
            let mut sel = self.selected;
            sel.set(None);
            let mut bottom = self.bottom;
            bottom.set(BottomPanel::Hidden);
        } else {
            self.clear_view();
        }
        // Contents are keyed by path, not commit, so they have to go either
        // way: this is where a reload picks up what was pushed.
        let mut cache = self.pr_files;
        cache.set(HashMap::new());
        // Dropped here rather than when the new one lands, so the pane never
        // shows the last pull request's comments under this one's title.
        let mut conv = self.conv;
        conv.set(Conversation::Loading);
        let mut src = self.pr_source;
        src.set(source);
        self.set_scan_root(checkout);
        let mut statuses = self.statuses;
        // A repository browsed on its own has nothing changed in it.
        statuses.set(ws.pr().map(|pr| statuses_of(&pr.files)).unwrap_or_default());
        let mut w = self.workspace;
        w.set(ws);
        let mut gh = self.gh_open;
        gh.set(false);
        let mut tick = self.refresh_tick;
        let v = *tick.peek();
        tick.set(v + 1);
    }

    /// Back to the local working tree.
    pub fn leave_remote(&self) {
        self.leave_remote_state();
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
        // No scan root means a pull request with no files on disk — nothing to
        // walk. The top bar disables the box in that case.
        let Some(root) = self.scan_root.peek().dir().cloned() else {
            return;
        };
        let q = self.search_text.peek().clone();
        let Some(re) = text_query(&q) else { return };
        let title = format!("SEARCH  “{q}”");
        self.run_scan(title, move || search_repo(&root, &re), move |hits| {
            BottomPanel::Search { query: q, hits }
        });
    }

    /// Walk the tree off the UI thread, showing the panel as busy meanwhile.
    ///
    /// The title doubles as the claim on the panel: a result is only applied
    /// while the panel is still waiting for *this* scan, so a slow search
    /// cannot land on top of the faster one that replaced it.
    fn run_scan(
        &self,
        title: String,
        scan: impl FnOnce() -> Vec<Hit> + Send + 'static,
        done: impl FnOnce(Vec<Hit>) -> BottomPanel + 'static,
    ) {
        let st = *self;
        let mut bottom = self.bottom;
        bottom.set(BottomPanel::Working {
            title: title.clone(),
        });
        spawn_forever(async move {
            let hits = tokio::task::spawn_blocking(scan).await.unwrap_or_default();
            let mut bottom = st.bottom;
            let still_ours = matches!(
                &*bottom.peek(),
                BottomPanel::Working { title: t } if *t == title
            );
            if still_ours {
                bottom.set(done(hits));
            }
        });
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
        let Some(root) = self.scan_root.peek().dir().cloned() else {
            return;
        };
        let Some(re) = word_query(name) else { return };
        let name = name.to_string();
        let title = format!("REFERENCES  {name}");
        self.run_scan(title, move || search_repo(&root, &re), move |hits| {
            BottomPanel::Refs { name, hits }
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
                "preview" => ViewMode::Preview,
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
            root: Signal::new(root.clone()),
            scan_root: Signal::new(ScanRoot::Dir(root)),
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
            pr_source: Signal::new(PrSource::Api),
            conv: Signal::new(Conversation::Loading),
            conv_open: Signal::new(true),
        }
    });

    let tree: Memo<Option<FileNode>> = use_memo(move || {
        st.refresh_tick.read();
        let statuses = st.statuses.read().clone();
        let root = match &*st.workspace.read() {
            Workspace::Pr(pr) => build_tree_from_paths(
                &format!("{} #{}", pr.repo, pr.number),
                pr.tree.iter().map(|p| p.as_path()),
                &statuses,
            ),
            Workspace::Repo(view) => build_tree_from_paths(
                &format!("{} @ {}", view.repo, view.branch),
                view.tree.iter().map(|p| p.as_path()),
                &statuses,
            ),
            Workspace::Local => build_tree(&st.root.read(), &statuses),
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

    // Warm the cache for a pull request's changed files as soon as it opens,
    // so clicking through the review does not wait on the network.
    use_effect(move || {
        let job = st
            .workspace
            .read()
            .pr()
            .map(|pr| (pr.number, super::prcache::changed_jobs(pr)));
        let Some((number, jobs)) = job else { return };
        spawn_forever(super::prcache::prefetch(st, number, jobs, st.api_token()));
    });

    // Pull the conversation as soon as a pull request opens — three requests,
    // next to the tree and every changed file, and the pane is the first thing
    // read on a review. Re-runs on `⟳`, which is how a reply written since
    // shows up.
    use_effect(move || {
        let target = st
            .workspace
            .read()
            .pr()
            .map(|pr| (pr.repo.clone(), pr.number));
        let Some((repo, number)) = target else { return };
        spawn_forever(super::conversation::load(st, repo, number));
    });

    // Build the symbol index off the UI thread, for whatever is being browsed
    // — the local repository, or a pull request checked out on disk. Reading
    // `scan_root` here is what re-triggers it; an in-flight build for the
    // previous one is cancelled.
    let _ = use_resource(move || {
        let root = st.scan_root.read().dir().cloned();
        async move {
            // Nothing on disk to index. `index` stays None, and the top bar
            // hides the readout rather than counting symbols that are not there.
            let Some(root) = root else { return };
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
                ConvPane {}
            }
            if *st.gh_open.read() {
                GhPanel {}
            }
        }
    }
}
