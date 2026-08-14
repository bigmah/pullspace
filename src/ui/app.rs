use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;

// std's Instant panics on wasm32-unknown-unknown.
use web_time::Instant;

use dioxus::prelude::*;

use crate::backend::auth::Token;
use crate::backend::clone::Progress;
use crate::backend::difftool::Expansion;
use crate::backend::github::{
    CommitView, Commits, FetchJob, PrDetail, PrHeader, PrState, PrSummary, RepoRef, RepoView,
    Snapshot, Thread, statuses_of,
};
use crate::backend::highlight;
use crate::backend::markdown;
use crate::backend::prefs::{self, Prefs};
use crate::backend::route::{self, Place, Route, Target};
use crate::backend::search::Options;
use crate::backend::tree::{
    ChangeKind, FileNode, build_tree_from_paths, changed_paths, filter_changed,
};
use crate::backend::{FileContent, blobs, layout, viewed};

use super::bottom::Bottom;
use super::conversation::ConvPane;
use super::filetree::FileTreePane;
use super::github::GhPanel;
use super::ide::{Index, Panel};
use super::page::Tab;
use super::panes::{self, Drag, DragMask, Edge};
use super::prefs::PrefsPanel;
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

/// What the explorer and viewer are showing.
///
/// Everything here comes from GitHub, which is what makes the two cases so
/// nearly alike: a pull request is a repository with some files marked as
/// changed, and browsing is the same thing with nothing marked.
#[derive(Clone, PartialEq)]
pub enum Workspace {
    /// Nothing opened yet. The picker is up, because choosing something is the
    /// only thing there is to do.
    Empty,
    Pr(Box<PrDetail>),
    /// A repository with no pull request in view — because it has none open,
    /// or because reading the code is the point.
    Repo(Box<RepoView>),
    /// One commit of it, diffed against the commit before it. Usually opened
    /// out of the pull request it belongs to, which rides along inside so that
    /// the conversation beside it stays whole.
    Commit(Box<CommitView>),
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

    pub fn commit(&self) -> Option<&CommitView> {
        match self {
            Workspace::Commit(view) => Some(view),
            _ => None,
        }
    }

    pub fn is_open(&self) -> bool {
        !matches!(self, Workspace::Empty)
    }

    /// Which repository this is, whichever of the three it is — what the pull
    /// request list beside it is a list for.
    pub fn repo_ref(&self) -> Option<&RepoRef> {
        match self {
            Workspace::Empty => None,
            Workspace::Pr(pr) => Some(&pr.repo),
            Workspace::Repo(view) => Some(&view.repo),
            Workspace::Commit(view) => Some(&view.repo),
        }
    }

    /// The open pull request's number, when one is open — what the switcher
    /// marks as the one you are already reading.
    ///
    /// A commit of a pull request is deliberately not it: the pull request is
    /// what that row would take you to, and it is somewhere else from here.
    pub fn pr_number(&self) -> Option<u64> {
        self.pr().map(|pr| pr.number)
    }

    /// The pull request whatever is open belongs to — the one that is open, or
    /// the one a commit was opened out of.
    ///
    /// This is what the conversation pane is about, and reading one commit of a
    /// pull request does not change the answer.
    pub fn header(&self) -> Option<PrHeader> {
        match self {
            Workspace::Pr(pr) => Some(pr.header()),
            Workspace::Commit(view) => view.pr.clone(),
            _ => None,
        }
    }

    /// The same as a pair of identifiers, for the two loads that hang off it.
    pub fn review_key(&self) -> Option<(RepoRef, u64)> {
        match self {
            Workspace::Pr(pr) => Some((pr.repo.clone(), pr.number)),
            Workspace::Commit(view) => {
                Some((view.repo.clone(), view.pr.as_ref().map(|p| p.number)?))
            }
            _ => None,
        }
    }

    /// Whether there is a file list to walk.
    ///
    /// What search and the symbol index both need, and what a pull request
    /// whose tree GitHub would not serve does not have. Cheap on purpose —
    /// [`trees`](Self::trees) answers the same question by cloning several
    /// thousand entries, which is not something to do on every render of the
    /// top bar.
    pub fn has_tree(&self) -> bool {
        match self {
            Workspace::Empty => false,
            Workspace::Pr(pr) => !pr.tree.is_empty(),
            Workspace::Repo(view) => !view.tree.is_empty(),
            Workspace::Commit(view) => !view.tree.is_empty(),
        }
    }

    /// What this workspace says is changed, and how — the explorer's badges,
    /// and what the viewer opens as a diff rather than as source.
    pub fn statuses(&self) -> HashMap<PathBuf, ChangeKind> {
        match self {
            Workspace::Pr(pr) => statuses_of(&pr.files),
            Workspace::Commit(view) => statuses_of(&view.files),
            // Nothing is changed in a repository being read on its own.
            _ => HashMap::new(),
        }
    }

    /// One job per changed file — what the clone fetches before anything else,
    /// since it is what is being read.
    pub fn changed_jobs(&self) -> Vec<FetchJob> {
        match self {
            Workspace::Pr(pr) => pr
                .files
                .iter()
                .map(|f| FetchJob::for_changed(pr, f))
                .collect(),
            Workspace::Commit(view) => view
                .files
                .iter()
                .map(|f| FetchJob::for_commit_change(view, f))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Everything needed to read one path of what is open, changed or not.
    pub fn job_for(&self, rel: &Path) -> Option<FetchJob> {
        match self {
            Workspace::Empty => None,
            Workspace::Pr(pr) => Some(FetchJob::new(pr, rel)),
            Workspace::Repo(view) => Some(FetchJob::browsing(view, rel)),
            Workspace::Commit(view) => Some(FetchJob::in_commit(view, rel)),
        }
    }

    /// This, as the half of a link that names what is open — what the address
    /// bar is made to say, and what a link pasted into it opens. The file being
    /// read is the other half: see [`St::route`].
    pub fn target(&self) -> Target {
        match self {
            Workspace::Empty => Target::Home,
            Workspace::Pr(pr) => Target::Pr(pr.repo.clone(), pr.number),
            Workspace::Repo(view) => Target::Repo(view.repo.clone()),
            Workspace::Commit(view) => Target::Commit(view.repo.clone(), view.commit.sha.clone()),
        }
    }

    /// The repository at the commit on show, and the one under it every diff is
    /// read against — which a repository browsed on its own does not have.
    pub fn trees(&self) -> Option<(Snapshot, Snapshot)> {
        match self {
            Workspace::Empty => None,
            Workspace::Pr(pr) => Some((pr.tree.clone(), pr.base_tree.clone())),
            Workspace::Repo(view) => Some((view.tree.clone(), Snapshot::default())),
            Workspace::Commit(view) => Some((view.tree.clone(), view.base_tree.clone())),
        }
    }
}

/// Sign-in status, as far as the UI needs to know. The token itself lives in a
/// separate signal so it is never part of anything rendered.
#[derive(Clone, PartialEq)]
pub enum Account {
    /// Startup: looking for a saved token.
    Checking,
    SignedOut,
    SignedIn {
        login: String,
    },
    Failed(String),
}

/// The pull requests of one repository: which of them were asked for, and how
/// the asking went.
///
/// Kept for as long as that repository is open rather than cleared once
/// something has been picked out of it — it is what the switcher in the top bar
/// switches between, and re-fetching it to open the menu would make a list you
/// glance at cost a request.
#[derive(Clone, PartialEq)]
pub struct PrList {
    pub repo: RepoRef,
    pub state: PrState,
    pub got: Got,
}

/// How a list of pull requests is getting on.
#[derive(Clone, PartialEq)]
pub enum Got {
    Loading,
    Ready(Vec<PrSummary>),
    Failed(String),
}

impl PrList {
    /// The pull requests themselves, empty until they are here.
    pub fn items(&self) -> &[PrSummary] {
        match &self.got {
            Got::Ready(items) => items,
            _ => &[],
        }
    }

    /// Whether this is the answer to a question already being asked, so that
    /// opening a pull request from a list does not go and fetch that same list
    /// again underneath it.
    pub fn covers(&self, repo: &RepoRef, state: PrState) -> bool {
        self.repo == *repo && self.state == state
    }
}

/// What the app is fetching from GitHub, for the one line that says so.
///
/// Separate from [`PrList`] because the two outlive each other in both
/// directions: a pull request opens while its repository's list stays on the
/// shelf, and a list is fetched in the background of a review that is already
/// on screen.
#[derive(Clone, PartialEq, Default)]
pub enum Fetch {
    #[default]
    Idle,
    Working(String),
    Failed(String),
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

/// The open pull request's commits — what its branch is made of.
///
/// Idle until somebody asks, unlike the conversation: the commits are one more
/// request, and a review that never opens the tab should not spend it.
#[derive(Clone, PartialEq)]
pub enum CommitList {
    Idle,
    Loading,
    Ready(Box<Commits>),
    Failed(String),
}

/// Which half of the conversation pane is on show.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum ConvTab {
    #[default]
    Talk,
    Commits,
}

/// Somewhere the reader has been: a file, how it was being shown, and the line
/// they were taken to if they were taken to one.
///
/// What Back and Forward are made of. Jumping to a definition three files away
/// is only worth doing if getting back is one key, and getting back means the
/// file *as it was* — a diff you were reading does not come back as source.
#[derive(Clone, PartialEq)]
pub struct Spot {
    pub path: PathBuf,
    pub mode: ViewMode,
    pub line: Option<usize>,
}

/// How far back the trail is kept. Long enough to cover an afternoon of
/// following definitions around; short enough not to be a memory leak with a
/// nice name.
const TRAIL: usize = 60;

/// Both sides of one file, fetched on demand.
#[derive(Clone, PartialEq)]
pub enum PrFileState {
    Loading,
    Ready {
        base: FileContent,
        head: FileContent,
    },
    Failed(String),
}

/// All app state, shared through context. Every field is a Copy signal, and
/// every one of them is owned by [`ScopeId::ROOT`] — see [`root`].
#[derive(Clone, Copy)]
pub struct St {
    /// Which files the open pull request changes, and how. Empty when browsing
    /// a repository on its own — nothing there is changed.
    pub statuses: Signal<HashMap<PathBuf, ChangeKind>>,
    pub open: Signal<Option<PathBuf>>,
    pub view_mode: Signal<ViewMode>,
    /// A request to put a line on screen: written by everything that jumps —
    /// a comment on the diff, a search hit, a definition.
    ///
    /// Set, never cleared. Clearing it was a write to the signal its own
    /// effect is subscribed to, which dioxus quite rightly calls a loop; and
    /// clearing was only ever needed to tell "opened at a line" from "opened",
    /// which is what `at_line` says now. So the effect fires on each write and
    /// the value left behind means nothing.
    pub scroll_to: Signal<Option<usize>>,
    /// The line the open file was entered at, if it was entered at one — a
    /// fact about where the reader is rather than a request to go anywhere.
    ///
    /// Two things read it: Back, which comes back to the definition you were
    /// reading rather than to the top of the file it was in, and the viewer,
    /// which only scrolls a newly opened file to the top when it was not
    /// opened at a line in the first place.
    pub at_line: Signal<Option<usize>>,
    /// Which directories of the explorer are open. Every directory has an entry
    /// from the moment it is first drawn, so what is open is a record of what
    /// was clicked rather than something re-derived from what has changed —
    /// see `backend::tree::seed_expansion`.
    pub expanded: Signal<HashMap<PathBuf, bool>>,
    /// Whether `expanded` has been filled in for what is on screen now. False
    /// means the next tree drawn is a new one, and gets the arrival view: the
    /// root open, and the way down to every change open with it.
    pub tree_seeded: Signal<bool>,
    /// Which contracted stretches of a diff have been opened up, and by how
    /// much — per file, and keyed within a file by the gap's position in
    /// `FileDiff::gaps`.
    ///
    /// Absent means contracted, which is how every file starts and what Reset
    /// puts it back to. Kept per file rather than for whichever one is open,
    /// so reading a second file and coming back does not undo the reading of
    /// the first.
    pub expansions: Signal<HashMap<PathBuf, HashMap<usize, Expansion>>>,
    /// The explorer's filter box: a substring of the path, narrowing the tree
    /// to a flat list of matches.
    pub tree_filter: Signal<String>,
    pub changes_only: Signal<bool>,
    /// Which of the open pull request's files have been marked read, by blob
    /// hash — see [`viewed`](crate::backend::viewed). Read back off storage
    /// when a pull request opens, and written whenever a box is ticked.
    pub viewed: Signal<HashSet<String>>,
    /// The changed files, in the order the explorer draws them: what next and
    /// previous changed file step through.
    ///
    /// Derived from the tree rather than held as truth — it is here, and not a
    /// memo beside it, because the keyboard reaches state and not context.
    pub changed_files: Signal<Vec<PathBuf>>,
    pub refresh_tick: Signal<u32>,
    /// Where the reader has been, and — after a Back — where they were before
    /// they went back. Emptied whenever somewhere new is opened, which is what
    /// every editor's Forward does.
    pub trail: Signal<Vec<Spot>>,
    pub ahead: Signal<Vec<Spot>>,

    // --- the IDE ---
    /// The identifier picked out of the code, if one has been. What Go to
    /// Definition and Find References act on, and what the viewer highlights
    /// every occurrence of.
    pub selected: Signal<Option<String>>,
    /// What the panel across the bottom of the code is showing.
    pub panel: Signal<Panel>,
    /// Every definition in the repository, once the background walk has read
    /// them all.
    pub index: Signal<Index>,
    /// Moved on by anything that supersedes a walk in progress, so a slow
    /// search cannot land on top of the one started after it.
    pub scan_seq: Signal<u32>,
    /// The search box, and its three toggles. Nothing to do with `tree_filter`,
    /// which narrows the explorer by path — this one reads inside files.
    pub search_text: Signal<String>,
    pub search_opts: Signal<Options>,
    /// What was wrong with the pattern, when it is a regular expression and it
    /// is wrong.
    pub search_error: Signal<Option<String>>,

    // --- GitHub ---
    /// Never rendered; `account` is what the UI reads.
    pub token: Signal<Option<Token>>,
    pub account: Signal<Account>,
    pub gh_open: Signal<bool>,
    pub repo_input: Signal<String>,
    /// The pull requests of whatever repository is open — `None` before any
    /// repository has been named. Both the picker and the top bar's switcher
    /// read this one list.
    pub prs: Signal<Option<PrList>>,
    /// Which of them to ask for: open, closed, or the lot.
    pub pr_state: Signal<PrState>,
    /// What is being fetched from GitHub, if anything, and what went wrong the
    /// last time something was.
    pub fetch: Signal<Fetch>,
    pub workspace: Signal<Workspace>,
    /// Per-file base/head content for what is open — decoded, and only for
    /// files somebody has actually looked at. The repository itself lives on
    /// disk; this is the shelf by the desk, not the library.
    pub pr_files: Signal<HashMap<PathBuf, PrFileState>>,
    /// What is in `pr_files`, oldest first, so the shelf can be kept to a size.
    pub warm_order: Signal<VecDeque<PathBuf>>,
    /// How the background clone of what is open is getting on. `None` before
    /// one has started, or where there is no filesystem to clone into.
    pub cloning: Signal<Option<Progress>>,
    /// Moved on whenever the local store changes underneath what the panel is
    /// saying about it — which today means somebody cleared it.
    ///
    /// It lives here, at the root, rather than in the panel that reads it.
    /// Clearing a store means deleting every file in it, which takes long
    /// enough that the panel can be closed while it runs — and a signal owned
    /// by a closed panel is one that has been dropped by the time the work
    /// finishes reporting back to it.
    pub store_gen: Signal<u32>,
    /// The open PR's comments. Folded away rather than unmounted, so the pane
    /// comes back instantly and without a second trip to GitHub.
    pub conv: Signal<Conversation>,
    pub conv_open: Signal<bool>,
    /// And its commits, which the same pane shows in place of them.
    pub commits: Signal<CommitList>,
    pub conv_tab: Signal<ConvTab>,

    /// A file — and perhaps a line of it — named by a link that arrived before
    /// there was anything open to find it in. Set on the way to a fetch and
    /// taken by [`St::enter`] once the tree is there to look it up in.
    pub pending: Signal<Option<Place>>,

    // --- appearance ---
    /// Theme, accent, font, size. Read at the top of the tree and published to
    /// the page as a stylesheet — see [`Prefs::css`].
    pub prefs: Signal<Prefs>,
    pub prefs_open: Signal<bool>,

    // --- layout ---
    /// Pane sizes in CSS pixels, published to the stylesheet as custom
    /// properties. Read at the very top of the tree and nowhere else, so
    /// dragging a divider re-renders one small template.
    pub side_w: Signal<f64>,
    pub conv_w: Signal<f64>,
    pub bottom_h: Signal<f64>,
    /// The area the two panes share, as last measured. `(0, 0)` until the
    /// first report from the resize observer.
    pub main_size: Signal<(f64, f64)>,
    /// The divider in hand, while one is.
    pub drag: Signal<Option<Drag>>,
    /// The last divider pressed and when — how a double-click is spotted.
    pub last_grab: Signal<Option<(Edge, Instant)>>,
}

impl St {
    pub fn token_value(&self) -> Option<String> {
        self.token.peek().as_ref().map(|t| t.value.clone())
    }

    /// The credential to send to GitHub, empty when signed out. GitHub serves
    /// public repositories anonymously (at a much lower rate limit), so being
    /// signed out limits what you can reach rather than blocking the app.
    pub fn api_token(&self) -> String {
        self.token_value().unwrap_or_default()
    }

    /// Whose token it is, empty when signed out. What tells "list this
    /// account's repositories" from "list mine" — only one of those two can
    /// see anything private.
    pub fn viewer(&self) -> String {
        match &*self.account.peek() {
            Account::SignedIn { login } => login.clone(),
            _ => String::new(),
        }
    }

    fn bump_tick(&self) {
        let mut tick = self.refresh_tick;
        let v = *tick.peek();
        tick.set(v + 1);
    }

    /// Change how the app looks, and keep it that way.
    ///
    /// The syntax theme is told first, and told outside the signal system on
    /// purpose: everything that re-colours code does so in response to the
    /// write below, and a highlighter still set to the old theme would hand
    /// back dark keywords for a light page.
    pub fn set_prefs(&self, next: Prefs) {
        highlight::use_light(next.theme.is_light());
        let mut prefs = self.prefs;
        prefs.set(next);
        prefs::save(next);
    }

    /// Say that the local store is not what it was.
    pub fn store_changed(&self) {
        let mut seen = self.store_gen;
        let v = *seen.peek();
        seen.set(v + 1);
    }

    /// Which workspace this is, counted rather than named.
    ///
    /// Opening or closing anything moves it on, so a background task that
    /// started under the last one can tell that it is working for nobody —
    /// including on a reload, where the pull request is the same one and the
    /// clone in flight is still the wrong one to be finishing.
    pub fn generation(&self) -> u32 {
        *self.refresh_tick.peek()
    }

    /// Clear everything tied to whatever was open: the file, the scroll, the
    /// trail through it, and the tree's expansion state.
    fn clear_view(&self) {
        let mut open = self.open;
        open.set(None);
        let mut ps = self.scroll_to;
        ps.set(None);
        self.reset_tree_folds();
        // Narrowing the explorer was about the files that were in it. Left
        // behind, it hides most of whatever is being opened instead.
        let mut tree_filter = self.tree_filter;
        tree_filter.set(String::new());
        self.clear_ide();
        let mut trail = self.trail;
        trail.set(Vec::new());
        let mut ahead = self.ahead;
        ahead.set(Vec::new());
    }

    /// Put down the results of reading the last thing.
    ///
    /// Every one of these is about files that are no longer on screen: hits in
    /// them, definitions of them, an identifier picked out of one. The search
    /// text itself stays — searching two pull requests for the same thing is a
    /// reasonable morning.
    fn clear_ide(&self) {
        let mut panel = self.panel;
        panel.set(Panel::Hidden);
        let mut sel = self.selected;
        sel.set(None);
        let mut index = self.index;
        index.set(Index::Off);
        let mut err = self.search_error;
        err.set(None);
    }

    /// Forget which directories were open, so the next tree drawn gets the
    /// arrival view rather than the last one's folds.
    ///
    /// For a different tree, not a changed one: a file appearing or a status
    /// moving must leave the explorer exactly where the reader put it.
    pub fn reset_tree_folds(&self) {
        let mut expanded = self.expanded;
        expanded.set(HashMap::new());
        let mut seeded = self.tree_seeded;
        seeded.set(false);
    }

    /// Show a pull request. `statuses` becomes the PR's change list, so the
    /// tree, badges and viewer need no special casing.
    pub fn enter_pr(&self, pr: PrDetail) {
        let reload = self
            .workspace
            .peek()
            .pr()
            .is_some_and(|open| open.repo == pr.repo && open.number == pr.number);
        self.enter(Workspace::Pr(Box::new(pr)), reload);
    }

    /// Show a repository on its own — no pull request, so no changed files and
    /// no diffs, just the code at `view.head_sha`.
    ///
    /// It opens on the README, drawn rather than as source. A repository with
    /// no pull request in it is being opened to be read, and the front page is
    /// what it is written to be read from — an empty pane with a button in it
    /// is not the answer to "show me this repository".
    pub fn enter_repo(&self, view: RepoView) {
        let reload = self
            .workspace
            .peek()
            .repo()
            .is_some_and(|open| open.repo == view.repo);
        let readme = markdown::readme_of(view.tree.paths());
        let linked = self.enter(Workspace::Repo(Box::new(view)), reload);
        // Not on a reload: that keeps whatever was being read, and coming back
        // to the README is a click on the tree away. Nor when the link that
        // brought us here named a file — that file is the front page it asked
        // for.
        if !reload
            && !linked
            && let Some(readme) = readme
        {
            self.open_file(readme);
        }
    }

    /// Show one commit, diffed against the commit before it.
    ///
    /// The pull request it came out of stays beside it — the conversation, the
    /// description and the list of commits are all facts about the pull
    /// request, and reading one of its commits does not change any of them.
    pub fn enter_commit(&self, view: CommitView) {
        let reload = self
            .workspace
            .peek()
            .commit()
            .is_some_and(|open| open.repo == view.repo && open.commit.sha == view.commit.sha);
        self.enter(Workspace::Commit(Box::new(view)), reload);
    }

    /// Swap in something fetched from GitHub.
    ///
    /// `reload` means the same thing is already open, so the file being read
    /// and the tree as it was expanded are kept — resetting the view out from
    /// under someone who asked for fresh data is not what they asked for. Only
    /// what describes the old commit is dropped.
    ///
    /// Answers whether a file named by the link that brought us here was
    /// opened, which is the one thing the caller may still have to decide about
    /// — see [`St::enter_repo`].
    fn enter(&self, ws: Workspace, reload: bool) -> bool {
        if !reload {
            self.clear_view();
        } else {
            // The same pull request at a different commit. What was read about
            // the last one — the index above all — is not about this one.
            self.clear_ide();
        }
        // Contents are keyed by path, not commit, so they have to go either
        // way: this is where a reload picks up what was pushed.
        self.forget_contents();
        let mut cloning = self.cloning;
        cloning.set(None);
        // The conversation and the commit list belong to a pull request, not to
        // whichever of its commits is on screen — so stepping from a pull
        // request into one of its commits, or from one commit to the next,
        // keeps both rather than fetching them again.
        //
        // Dropped here rather than when the new ones land, so the pane never
        // shows the last pull request's comments under this one's title.
        //
        // Not on a reload, though: `⟳` means fetch all of it again, and the
        // conversation and the commits are part of what may have moved.
        let same_review = !reload
            && self
                .workspace
                .peek()
                .review_key()
                .is_some_and(|was| Some(was) == ws.review_key());
        if !same_review {
            let mut conv = self.conv;
            conv.set(Conversation::Loading);
            // Idle rather than empty: it is what has the pane go and fetch the
            // commits the moment somebody looks at the tab, and — on a reload —
            // what picks up whatever was just pushed. Before the workspace is
            // written, since that write is what the fetch hangs off.
            let mut commits = self.commits;
            commits.set(CommitList::Idle);
        }
        let mut statuses = self.statuses;
        statuses.set(ws.statuses());
        // What was ticked here last time. On a reload as well: a mark is about
        // a blob, so the ones that survived the push are still the answer, and
        // the ones that did not have quietly stopped matching anything.
        let mut marks = self.viewed;
        marks.set(
            marks_key(&ws)
                .map(|key| viewed::load(&key))
                .unwrap_or_default(),
        );
        let link = Route::to(ws.target());
        let mut w = self.workspace;
        w.set(ws);
        // The address bar follows whatever is open, so every review has a link:
        // one to send to somebody, and one this tab comes back to on reload.
        //
        // After the workspace and not before it. Writing the bar raises a
        // `hashchange`, which is how Back reaches us — and the listener tells
        // one of those from the other by asking what is already open.
        route::show(&link);
        let mut gh = self.gh_open;
        gh.set(false);
        self.bump_tick();
        // And now that there is a tree to look a path up in, whatever the link
        // named inside it.
        self.take_pending()
    }

    /// Open the file a link named, if it named one and it is still there.
    ///
    /// A path that has moved since the link was written is not an error worth
    /// showing: the pull request it points into is open, which is most of what
    /// was being asked for.
    fn take_pending(&self) -> bool {
        let mut pending = self.pending;
        let Some(place) = pending.write().take() else {
            return false;
        };
        self.open_place(place)
    }

    /// Go where a link points, inside what is already open.
    pub fn open_place(&self, place: Place) -> bool {
        if !self.has_file(&place.path) {
            return false;
        }
        match place.line {
            Some(line) => self.open_at(place.path, line),
            None => self.open_file(place.path),
        }
        true
    }

    /// Where the reader is, as a link — what is open, the file being read, and
    /// the line they picked out of it.
    ///
    /// Reactive: this is what the address bar is kept in step with, so it has
    /// to be re-answered when any of the three moves.
    pub fn route(&self) -> Route {
        Route {
            at: self.workspace.read().target(),
            place: self.open.read().clone().map(|path| Place {
                path,
                line: *self.at_line.read(),
            }),
        }
    }

    /// Pick a line out of the open file, or put it down again.
    ///
    /// It is not a jump — the reader is looking at the line already. All it
    /// changes is what the address bar says, which is what makes the link in it
    /// a link to *here*.
    pub fn mark_line(&self, line: usize) {
        if self.open.peek().is_none() {
            return;
        }
        let mut at = self.at_line;
        let now = *at.peek();
        at.set((now != Some(line)).then_some(line));
    }

    /// Close whatever is open, back to nothing — which puts the picker up,
    /// since that is the only thing left to do.
    pub fn close_workspace(&self) {
        let mut w = self.workspace;
        w.set(Workspace::Empty);
        let mut commits = self.commits;
        commits.set(CommitList::Idle);
        self.forget_contents();
        let mut cloning = self.cloning;
        cloning.set(None);
        let mut statuses = self.statuses;
        statuses.set(HashMap::new());
        let mut marks = self.viewed;
        marks.set(HashSet::new());
        self.clear_view();
        // Closing is a place to come back to as much as opening is: without
        // this the address bar would go on naming a pull request that is no
        // longer on screen, and reloading would open it again.
        route::show(&Route::home());
        let mut gh = self.gh_open;
        gh.set(true);
        self.bump_tick();
    }

    /// Open a file from the tree: changed files land in split-diff view, and
    /// prose lands rendered — markdown is written to be read, and the source of
    /// it is one button away.
    pub fn open_file(&self, rel: PathBuf) {
        self.mark(&rel);
        let changed = self.statuses.peek().get(&rel).is_some();
        let mut vm = self.view_mode;
        vm.set(match () {
            _ if changed => ViewMode::Split,
            _ if markdown::is_markdown(&rel) => ViewMode::Preview,
            _ => ViewMode::Source,
        });
        let mut open = self.open;
        open.set(Some(rel));
        let mut at = self.at_line;
        at.set(None);
    }

    // ----------------------------------------------------- marking files read

    /// What a changed file of what is open is remembered as, or `None` when it
    /// is not one — an unchanged file, or a repository being browsed. Ticking a
    /// box against those would be a note about nothing: a review is over the
    /// files that changed.
    pub fn viewed_key(&self, rel: &Path) -> Option<String> {
        // The status map first, because this is asked once per row of the
        // explorer and answering it is a hash lookup where the workspace would
        // be a scan of every changed file.
        if !self.statuses.peek().contains_key(rel) {
            return None;
        }
        match &*self.workspace.peek() {
            Workspace::Pr(pr) => Some(pr.blob_key(rel).into_owned()),
            Workspace::Commit(view) => Some(view.blob_key(rel).into_owned()),
            _ => None,
        }
    }

    /// Whether this file has been marked read. A reactive read: ticking one box
    /// redraws its row in the explorer and the header above the code.
    pub fn is_viewed(&self, rel: &Path) -> bool {
        // Nothing ticked is the common case and it costs nothing to answer, but
        // subscribe first — a component that skipped the read would not redraw
        // when the first box was ticked.
        let marks = self.viewed.read();
        !marks.is_empty() && self.viewed_key(rel).is_some_and(|key| marks.contains(&key))
    }

    /// Tick or untick one file, and keep it.
    pub fn toggle_viewed(&self, rel: &Path) {
        let Some(key) = self.viewed_key(rel) else {
            return;
        };
        let mut marks = self.viewed;
        {
            let mut held = marks.write();
            // Removing tells us whether it was there, so this is one lookup
            // rather than a `contains` and then a branch.
            if !held.remove(&key) {
                held.insert(key);
            }
        }
        let held = self.workspace.peek();
        if let Some(key) = marks_key(&held) {
            viewed::save(&key, &marks.peek());
        }
    }

    /// How many of the changed files have been marked read — the explorer's
    /// progress readout.
    pub fn viewed_count(&self) -> usize {
        let marks = self.viewed.read();
        if marks.is_empty() {
            return 0;
        }
        match &*self.workspace.read() {
            Workspace::Pr(pr) => pr
                .files
                .iter()
                .filter(|f| marks.contains(pr.blob_key_of(f).as_ref()))
                .count(),
            Workspace::Commit(view) => view
                .files
                .iter()
                .filter(|f| marks.contains(view.blob_key_of(f).as_ref()))
                .count(),
            _ => 0,
        }
    }

    // --------------------------------------------- stepping through the diff

    /// Where the open file stands among the changed ones, when it is one of
    /// them. `None` while reading something the pull request does not touch —
    /// which is where a definition three files away lands you.
    pub fn file_at(&self) -> Option<usize> {
        let open = self.open.read();
        let here = open.as_deref()?;
        self.changed_files.read().iter().position(|p| p == here)
    }

    /// The next or previous changed file, in the order the explorer draws them.
    ///
    /// From somewhere that is not one of them — an unchanged file opened to
    /// read around the change — it enters the list at the end you are heading
    /// for rather than doing nothing.
    pub fn step_file(&self, forward: bool) {
        let files = self.changed_files.peek();
        let target = match (self.file_at(), forward) {
            (Some(at), true) => files.get(at + 1),
            (Some(0), false) => None,
            (Some(at), false) => files.get(at - 1),
            (None, true) => files.first(),
            (None, false) => files.last(),
        };
        let Some(next) = target.cloned() else { return };
        drop(files);
        self.open_file(next);
    }

    // -------------------------------------------- opening a diff back up

    /// Show more of one contracted stretch of the open file's diff.
    ///
    /// `from_top` takes the lines nearest the code above the gap, so the
    /// stretch grows downwards out of it; otherwise they come off the bottom
    /// and it grows up towards the change below. Overshooting the end of the
    /// gap is harmless — `difftool::blocks` will not show more than is there.
    pub fn expand_gap(&self, gap: usize, by: usize, from_top: bool) {
        let Some(rel) = self.open.peek().clone() else {
            return;
        };
        let mut expansions = self.expansions;
        let mut w = expansions.write();
        let e = w.entry(rel).or_default().entry(gap).or_default();
        if from_top {
            e.top += by;
        } else {
            e.bottom += by;
        }
    }

    /// Show all of one contracted stretch. Taken off the bottom, which is what
    /// puts the bar that folds it away again at the head of what it opened.
    pub fn expand_gap_fully(&self, gap: usize, len: usize) {
        let Some(rel) = self.open.peek().clone() else {
            return;
        };
        let mut expansions = self.expansions;
        expansions.write().entry(rel).or_default().insert(
            gap,
            Expansion {
                top: 0,
                bottom: len,
            },
        );
    }

    /// Fold one stretch back up.
    pub fn contract_gap(&self, gap: usize) {
        let Some(rel) = self.open.peek().clone() else {
            return;
        };
        let mut expansions = self.expansions;
        let mut w = expansions.write();
        let Some(file) = w.get_mut(&rel) else { return };
        file.remove(&gap);
        // Nothing open in it is the same as never having been touched, and is
        // what `has_expansions` — and so the Reset button — is asking.
        if file.is_empty() {
            w.remove(&rel);
        }
    }

    /// Put the open file's diff back the way it arrived: every stretch of it
    /// contracted again, in one click.
    pub fn contract_all_gaps(&self) {
        let Some(rel) = self.open.peek().clone() else {
            return;
        };
        let mut expansions = self.expansions;
        expansions.write().remove(&rel);
    }

    /// Which stretches of the open file are open, if any.
    pub fn open_gaps(&self) -> HashMap<usize, Expansion> {
        let Some(rel) = self.open.read().clone() else {
            return HashMap::new();
        };
        self.expansions
            .read()
            .get(&rel)
            .cloned()
            .unwrap_or_default()
    }

    /// Whether a repo-relative path is one of the files on show. What a link in
    /// a README is checked against before it is followed, so a stale one is
    /// left alone rather than opening an error where the document was.
    pub fn has_file(&self, rel: &Path) -> bool {
        match &*self.workspace.peek() {
            Workspace::Empty => false,
            // A pull request whose tree could not be read still knows the files
            // it changes.
            Workspace::Pr(pr) => {
                pr.tree.entry(rel).is_some() || pr.files.iter().any(|f| f.path == rel)
            }
            Workspace::Repo(view) => view.tree.entry(rel).is_some(),
            Workspace::Commit(view) => {
                view.tree.entry(rel).is_some() || view.files.iter().any(|f| f.path == rel)
            }
        }
    }

    /// Drop every decoded file held in memory.
    ///
    /// Only the decoded copies: what was cloned stays on disk, so the files
    /// dropped here come back off the filesystem rather than off the network.
    fn forget_contents(&self) {
        let mut cache = self.pr_files;
        cache.set(HashMap::new());
        let mut order = self.warm_order;
        order.set(VecDeque::new());
        // Which stretches of a diff were opened up is a fact about that diff,
        // and every diff here is about to be built again out of whatever
        // arrives. Held on to, the positions would refer to gaps in a
        // comparison that no longer exists.
        let mut expansions = self.expansions;
        expansions.set(HashMap::new());
    }

    /// Jump to a specific line — what a comment on the diff, a search hit and a
    /// definition all link to.
    pub fn open_at(&self, rel: PathBuf, line: usize) {
        self.mark(&rel);
        let mut vm = self.view_mode;
        vm.set(ViewMode::Source);
        let mut open = self.open;
        open.set(Some(rel));
        let mut ps = self.scroll_to;
        ps.set(Some(line));
        let mut at = self.at_line;
        at.set(Some(line));
    }

    // ------------------------------------------------------------ the trail

    /// Where the reader is standing, as somewhere to come back to.
    fn here(&self) -> Option<Spot> {
        Some(Spot {
            path: self.open.peek().clone()?,
            mode: *self.view_mode.peek(),
            line: *self.at_line.peek(),
        })
    }

    /// Note where we were, on the way to somewhere else.
    ///
    /// Only when the file changes. A jump within the file you are already
    /// reading leaves no mark, because a Back that lands on the same file at
    /// the top of it looks like a button that did nothing.
    fn mark(&self, going_to: &Path) {
        let Some(here) = self.here().filter(|h| h.path != going_to) else {
            return;
        };
        let mut trail = self.trail;
        {
            let mut trail = trail.write();
            trail.push(here);
            if trail.len() > TRAIL {
                trail.remove(0);
            }
        }
        // Going somewhere new is what ends the branch you had gone back along.
        let mut ahead = self.ahead;
        if !ahead.peek().is_empty() {
            ahead.set(Vec::new());
        }
    }

    pub fn can_go_back(&self) -> bool {
        !self.trail.read().is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.ahead.read().is_empty()
    }

    pub fn go_back(&self) {
        let mut trail = self.trail;
        let Some(spot) = trail.write().pop() else {
            return;
        };
        if let Some(here) = self.here() {
            let mut ahead = self.ahead;
            ahead.write().push(here);
        }
        self.land(spot);
    }

    pub fn go_forward(&self) {
        let mut ahead = self.ahead;
        let Some(spot) = ahead.write().pop() else {
            return;
        };
        if let Some(here) = self.here() {
            let mut trail = self.trail;
            trail.write().push(here);
        }
        self.land(spot);
    }

    /// Show a spot as it was — and without marking the trail, since moving
    /// along it is not the same as leaving it.
    fn land(&self, spot: Spot) {
        let mut vm = self.view_mode;
        vm.set(spot.mode);
        let mut open = self.open;
        open.set(Some(spot.path));
        let mut ps = self.scroll_to;
        ps.set(spot.line);
        let mut at = self.at_line;
        at.set(spot.line);
    }
}

/// Where the ticks against this workspace's changed files are kept.
///
/// A commit keeps its own, apart from the pull request it belongs to: reading
/// one commit's version of a file is not the same claim as having read what the
/// whole pull request does to it. `None` for a repository being browsed, which
/// has nothing changed to tick.
fn marks_key(ws: &Workspace) -> Option<String> {
    match ws {
        Workspace::Pr(pr) => Some(viewed::pr_key(&pr.repo, pr.number)),
        Workspace::Commit(view) => Some(viewed::commit_key(&view.repo, &view.commit.sha)),
        _ => None,
    }
}

/// Hold a piece of state in the root scope rather than in `App`.
///
/// `App` is not the root scope. Dioxus wraps whatever is launched in an error
/// boundary and a suspense boundary, so the component below is `ScopeId(3)`,
/// three deep — while `spawn_forever`, which is how every fetch here outlives
/// the row or panel that started it, runs its tasks in `ScopeId::ROOT`.
///
/// A signal owned by `App` and written from one of those tasks is therefore
/// being used *outside* the scope that owns it, which dioxus-signals warns
/// about on every single write — and warns about for a reason: a task that
/// outlives its scope is a task holding a dropped value. Creating the state
/// here puts it in the same scope as the tasks that write it, which is the
/// lifetime it actually has.
fn root<T: 'static>(value: T) -> Signal<T> {
    Signal::new_in_scope(value, ScopeId::ROOT)
}

#[component]
pub fn App() -> Element {
    let st = use_context_provider(|| {
        // The panes come back the width they were left, which is the whole
        // point of being able to drag them.
        let saved = layout::load();
        // And so does the look of the place. Before the first render, so the
        // app is never briefly drawn in a theme nobody chose.
        let look = prefs::load();
        highlight::use_light(look.theme.is_light());
        St {
            statuses: root(HashMap::new()),
            open: root(None),
            view_mode: root(ViewMode::Source),
            scroll_to: root(None),
            at_line: root(None),
            expanded: root(HashMap::new()),
            expansions: root(HashMap::new()),
            tree_seeded: root(false),
            tree_filter: root(String::new()),
            changes_only: root(false),
            viewed: root(HashSet::new()),
            changed_files: root(Vec::new()),
            refresh_tick: root(0),
            trail: root(Vec::new()),
            ahead: root(Vec::new()),

            selected: root(None),
            panel: root(Panel::Hidden),
            index: root(Index::Off),
            scan_seq: root(0),
            search_text: root(String::new()),
            search_opts: root(Options::default()),
            search_error: root(None),

            token: root(None),
            account: root(Account::Checking),
            // Nothing is open at launch, so the picker is where to start.
            gh_open: root(true),
            repo_input: root(String::new()),
            prs: root(None),
            pr_state: root(PrState::default()),
            fetch: root(Fetch::Idle),
            workspace: root(Workspace::Empty),
            pr_files: root(HashMap::new()),
            warm_order: root(VecDeque::new()),
            cloning: root(None),
            store_gen: root(0),
            conv: root(Conversation::Loading),
            conv_open: root(true),
            commits: root(CommitList::Idle),
            conv_tab: root(ConvTab::default()),

            pending: root(None),

            prefs: root(look),
            prefs_open: root(false),

            side_w: root(saved.side_w),
            conv_w: root(saved.conv_w),
            bottom_h: root(saved.bottom_h),
            main_size: root((0.0, 0.0)),
            drag: root(None),
            last_grab: root(None),
        }
    });

    // The whole tree, built once per workspace or refresh — and shared from
    // behind an `Rc`, so the memo below can hand it on without copying it.
    let full_tree: Memo<Option<Rc<FileNode>>> = use_memo(move || {
        st.refresh_tick.read();
        let statuses = st.statuses.read();
        Some(Rc::new(match &*st.workspace.read() {
            Workspace::Empty => return None,
            Workspace::Pr(pr) => build_tree_from_paths(
                &format!("{} #{}", pr.repo, pr.number),
                pr.tree.paths(),
                &statuses,
            ),
            Workspace::Repo(view) => build_tree_from_paths(
                &format!("{} @ {}", view.repo, view.branch),
                view.tree.paths(),
                &statuses,
            ),
            Workspace::Commit(view) => build_tree_from_paths(
                &format!("{} @ {}", view.repo, view.commit.short()),
                view.tree.paths(),
                &statuses,
            ),
        }))
    });
    // The Δ filter, split out so flipping the toggle re-answers the cheap
    // question — which nodes hold changes — without walking every path in the
    // repository again.
    let tree: Memo<Option<Rc<FileNode>>> = use_memo(move || {
        let held = full_tree.read();
        let root = held.as_ref()?;
        if *st.changes_only.read() {
            filter_changed(root).map(Rc::new)
        } else {
            Some(Rc::clone(root))
        }
    });
    use_context_provider(|| tree);

    // The changed files in the order they are drawn, for next/previous file to
    // walk. Derived here rather than where it is used because both users are a
    // long way from this memo: the viewer's stepper is three components down,
    // and the keyboard has state and no context at all.
    use_effect(move || {
        let order = tree
            .read()
            .as_ref()
            .map(|root| changed_paths(root))
            .unwrap_or_default();
        let mut changed_files = st.changed_files;
        // The Δ filter rebuilds the tree without changing which files are
        // changed or what order they come in; an unconditional write would
        // redraw the viewer's header for nothing.
        if *changed_files.peek() != order {
            changed_files.set(order);
        }
    });

    // The address bar, both ways: what a link opens, and — from `St::enter` —
    // what is written back into it once something is open.
    use_future(move || super::nav::watch(st));
    use_future(move || super::nav::landing(st));

    // And the third thing the bar has to follow: the file being read, and the
    // line picked out of it. Written in place rather than pushed — see
    // `route::replace` — so that Back walks the pull requests somebody opened
    // and not every file they clicked inside one.
    use_effect(move || {
        let here = st.route();
        // Nothing open writes `#/` already, from `close_workspace`. Doing it
        // again here would fight the picker on the way in.
        if here.at != Target::Home {
            route::replace(&here);
        }
    });

    // Pick up a saved token once at startup. Verifying it costs one API call
    // and tells us the login.
    use_future(move || async move {
        let Some(tok) = crate::backend::auth::find_token() else {
            let mut acct = st.account;
            acct.set(Account::SignedOut);
            return;
        };
        let login = crate::backend::github::viewer_login(&tok.value).await.ok();

        let mut acct = st.account;
        match login {
            Some(login) => {
                let mut t = st.token;
                t.set(Some(tok));
                acct.set(Account::SignedIn { login });
            }
            // A stale token is the same as none, but say so rather than
            // silently showing a signed-out chip.
            None => acct.set(Account::Failed(
                "The saved token was rejected by GitHub.".to_string(),
            )),
        }
    });

    // Open the local filesystem and read its index once, before anything is
    // opened — every later decision about what to fetch is a question it
    // answers, and it has to be able to answer without waiting.
    use_future(|| async move {
        blobs::open().await;
    });

    // Pull the whole repository down as soon as something opens, so clicking
    // through it never touches the network again. Most of it is usually here
    // already — the store is keyed by content, and a repository read last week
    // has not changed much since.
    use_effect(move || {
        let opened = {
            let held = st.workspace.read();
            held.trees()
                .map(|(head, base)| (head, base, held.changed_jobs()))
        };
        let Some((head, base, changed)) = opened else {
            return;
        };
        spawn_forever(super::prcache::clone_repo(st, head, base, changed));
    });

    // Read every definition in whatever has opened, so that clicking one
    // identifier and asking where it comes from is a lookup rather than a
    // wait. It goes second: the walk it does is over the files the clone
    // above is still fetching, and it holds off until that has finished.
    use_effect(move || {
        let files = st
            .workspace
            .read()
            .trees()
            .map(|(head, _)| head.files)
            .unwrap_or_default();
        spawn_forever(super::ide::build_index(st, files));
    });

    // The keyboard, for as long as the app is up. It has to be installed on
    // the window rather than on an element here — for most of this app's life
    // the focus is on the document body, which is above everything a handler
    // in this tree could be attached to.
    use_future(move || super::ide::keys(st));

    // And the other thing listened for on the document: a click on a line
    // number, which is how a line gets picked out to link to.
    use_future(move || super::viewer::lines(st));

    // Pull the conversation as soon as a pull request opens — three requests,
    // next to the tree and every changed file, and the pane is the first thing
    // read on a review. Re-runs on `⟳`, which is how a reply written since
    // shows up.
    use_effect(move || {
        let Some((repo, number)) = st.workspace.read().review_key() else {
            return;
        };
        // `enter` decides whether this pull request's conversation is still the
        // one on screen, and says so by leaving it on `Loading` or not —
        // stepping into one of its commits keeps it, `⟳` does not. Peeked, so
        // the fetch below cannot start itself again.
        if !matches!(*st.conv.peek(), Conversation::Loading) {
            return;
        }
        spawn_forever(super::conversation::load(st, repo, number));
    });

    // The pull requests of whatever repository is open, kept alongside it —
    // which is what makes the switcher in the top bar a list to swap through
    // rather than a button that goes and fetches one. It follows the repository
    // and the open/closed/all toggle, and nothing else: opening a second pull
    // request on the same repository is a list already in hand.
    use_effect(move || {
        let state = *st.pr_state.read();
        // Whatever is open — or, with nothing open, whatever the picker last
        // named, so that flipping open/closed/all in front of a list refetches
        // that list rather than waiting for something to be opened.
        let repo = st
            .workspace
            .read()
            .repo_ref()
            .cloned()
            .or_else(|| st.prs.peek().as_ref().map(|l| l.repo.clone()));
        let Some(repo) = repo else { return };
        if st
            .prs
            .peek()
            .as_ref()
            .is_some_and(|l| l.covers(&repo, state))
        {
            return;
        }
        spawn_forever(super::github::load_repo_prs(st, repo));
    });

    // And the open pull request's commits, once somebody looks at the tab they
    // are under. One request, spent on being asked for rather than on every
    // review that never opens it.
    use_effect(move || {
        let asked = *st.conv_tab.read() == ConvTab::Commits;
        let target = st.workspace.read().review_key();
        let Some((repo, number)) = target.filter(|_| asked) else {
            return;
        };
        if !matches!(*st.commits.peek(), CommitList::Idle) {
            return;
        }
        spawn_forever(super::conversation::load_commits(st, repo, number));
    });

    // Unfolding the conversation claims 380px the explorer may currently be
    // standing on, and opening a pull request is what puts it there at all.
    // Either way the dividers' limits have moved, so the panes are brought
    // back inside them. Everything `refit` touches it peeks, so this watches
    // the two signals named here and nothing else.
    use_effect(move || {
        let _ = st.conv_open.read();
        let _ = st.workspace.read();
        panes::refit(&st);
    });

    // The one place the pane sizes are read. Whole pixels: a divider parked on
    // a half pixel blurs the hairline it draws, and the extra precision is not
    // something anyone is dragging for.
    let panes = format!(
        "--side-w:{:.0}px;--conv-w:{:.0}px;--bottom-h:{:.0}px",
        *st.side_w.read(),
        *st.conv_w.read(),
        *st.bottom_h.read(),
    );

    // The chosen palette, font and size, as the stylesheet that applies them.
    // After the app's own, so it wins — and nowhere near it, so the defaults
    // stay readable as the defaults.
    let look = st.prefs.read().css();

    rsx! {
        style { dangerous_inner_html: CSS }
        style { dangerous_inner_html: look }
        Tab {}
        div { class: "app", style: "{panes}",
            TopBar {}
            div {
                class: "main",
                // What the dividers are allowed to give away — and, when the
                // window has just given away some of it, the moment the panes
                // are moved back inside what is left. The panes are `flex:
                // none`, so this is the only thing that resizes them, which is
                // what makes the sizes here the sizes on screen.
                onresize: move |e| {
                    if let Ok(size) = e.get_content_box_size() {
                        let mut area = st.main_size;
                        area.set((size.width, size.height));
                        panes::refit(&st);
                    }
                },
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
            if *st.prefs_open.read() {
                PrefsPanel {}
            }
            DragMask {}
        }
    }
}
