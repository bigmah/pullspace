use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;

// std's Instant panics on wasm32-unknown-unknown.
use web_time::Instant;

use dioxus::prelude::*;

use crate::backend::auth::Token;
use crate::backend::clone::Progress;
use crate::backend::difftool::Expansion;
use crate::backend::drafts::Pending;
use crate::backend::github::{
    Annotation, Branches, Checks, CommitFrom, CommitView, Commits, CompareView, FetchJob, PrDetail,
    PrHeader, PrMore, PrState, PrSummary, RepoRef, RepoView, Snapshot, Thread, statuses_of,
};
use crate::backend::highlight;
use crate::backend::markdown;
use crate::backend::prefs::{self, Prefs};
use crate::backend::route::{self, Place, Route, Target};
use crate::backend::search::Options;
use crate::backend::symbols::{self, Symbol};
use crate::backend::tree::{
    ChangeKind, FileNode, build_tree_from_paths, changed_paths, filter_changed,
};
use crate::backend::{FileContent, blobs, layout, viewed};

use super::bottom::Bottom;
use super::conversation::ConvPane;
use super::filetree::FileTreePane;
use super::github::GhPanel;
use super::ide::{Index, Panel};
use super::landing::Landing;
use super::opening::Opening;
use super::page::Tab;
use super::palette::Picker;
use super::panes::{self, Drag, DragMask, Edge};
use super::prboard::PrBoard;
use super::prefs::PrefsPanel;
use super::review::{Composing, Writing};
use super::spaces::{self, Card, Held, Space};
use super::tabs;
use super::topbar::TopBar;
use super::viewer::Viewer;

mod gaps;
mod history;
mod marks;
mod stepping;
mod workspace;

static CSS: &str = include_str!("../../../assets/style.css");

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
    /// Nothing opened yet. The landing page is up in place of the IDE, because
    /// choosing something is the only thing there is to do.
    Empty,
    Pr(Box<PrDetail>),
    /// A repository with no pull request in view — because it has none open,
    /// or because reading the code is the point.
    Repo(Box<RepoView>),
    /// One commit of it, diffed against the commit before it. Usually opened
    /// out of the pull request it belongs to, which rides along inside so that
    /// the conversation beside it stays whole.
    Commit(Box<CommitView>),
    /// Two of its refs held up against each other — the pull request somebody
    /// has not opened yet.
    Compare(Box<CompareView>),
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

    pub fn compare(&self) -> Option<&CompareView> {
        match self {
            Workspace::Compare(view) => Some(view),
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
            Workspace::Compare(view) => Some(&view.repo),
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
            Workspace::Commit(view) => view.pr().cloned(),
            _ => None,
        }
    }

    /// Whether there is a pull request behind what is open — the one open, or
    /// the one a commit was read out of.
    ///
    /// What decides whether the pane on the right leads with a conversation or
    /// with the branches. Cheap on purpose: [`header`](Self::header) answers
    /// the same question by cloning half a pull request.
    pub fn has_review(&self) -> bool {
        match self {
            Workspace::Pr(_) => true,
            Workspace::Commit(view) => view.pr().is_some(),
            _ => false,
        }
    }

    /// Which list of commits belongs beside what is open: everything on a pull
    /// request, or the history of a branch.
    ///
    /// `None` for a commit reached by a link of its own, which has neither
    /// around it — and for nothing being open at all.
    pub fn commits_key(&self) -> Option<CommitSource> {
        match self {
            Workspace::Empty => None,
            Workspace::Pr(pr) => Some(CommitSource::Pr(pr.repo.clone(), pr.number)),
            Workspace::Repo(view) => {
                Some(CommitSource::Branch(view.repo.clone(), view.branch.clone()))
            }
            Workspace::Compare(view) => Some(CommitSource::Compare(
                view.repo.clone(),
                view.base.clone(),
                view.head.clone(),
            )),
            Workspace::Commit(view) => match &view.from {
                CommitFrom::Alone => None,
                CommitFrom::Pr(pr) => Some(CommitSource::Pr(view.repo.clone(), pr.number)),
                CommitFrom::Branch(branch) => {
                    Some(CommitSource::Branch(view.repo.clone(), branch.clone()))
                }
                CommitFrom::Compare(base, head) => Some(CommitSource::Compare(
                    view.repo.clone(),
                    base.clone(),
                    head.clone(),
                )),
            },
        }
    }

    /// The commit whose checks the pane shows: the head of the pull request,
    /// the tip of the branch being browsed, or whichever single commit is being
    /// read.
    ///
    /// A check is a fact about a commit and not about the thing it was reached
    /// through — so stepping into one of them asks a different question and
    /// gets a different answer. `None` only with nothing open.
    pub fn checks_key(&self) -> Option<(RepoRef, String)> {
        match self {
            Workspace::Empty => None,
            Workspace::Pr(pr) => Some((pr.repo.clone(), pr.head_sha.clone())),
            Workspace::Repo(view) => Some((view.repo.clone(), view.head_sha.clone())),
            Workspace::Commit(view) => Some((view.repo.clone(), view.commit.sha.clone())),
            Workspace::Compare(view) => Some((view.repo.clone(), view.head_sha.clone())),
        }
    }

    /// The same as a pair of identifiers, for the two loads that hang off it.
    pub fn review_key(&self) -> Option<(RepoRef, u64)> {
        match self {
            Workspace::Pr(pr) => Some((pr.repo.clone(), pr.number)),
            Workspace::Commit(view) => Some((view.repo.clone(), view.pr()?.number)),
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
            Workspace::Compare(view) => !view.tree.is_empty(),
        }
    }

    /// What this workspace says is changed, and how — the explorer's badges,
    /// and what the viewer opens as a diff rather than as source.
    pub fn statuses(&self) -> HashMap<PathBuf, ChangeKind> {
        match self {
            Workspace::Pr(pr) => statuses_of(&pr.files),
            Workspace::Commit(view) => statuses_of(&view.files),
            Workspace::Compare(view) => statuses_of(&view.files),
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
            Workspace::Compare(view) => view
                .files
                .iter()
                .map(|f| FetchJob::for_compare_change(view, f))
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
            Workspace::Compare(view) => Some(FetchJob::in_compare(view, rel)),
        }
    }

    /// This, as the half of a link that names what is open — what the address
    /// bar is made to say, and what a link pasted into it opens. The file being
    /// read is the other half: see [`St::route`].
    pub fn target(&self) -> Target {
        match self {
            Workspace::Empty => Target::Home,
            Workspace::Pr(pr) => Target::Pr(pr.repo.clone(), pr.number),
            // A link with no branch in it is the repository at whatever its
            // default branch is now, which is the right link for the one and
            // the wrong link for any other.
            Workspace::Repo(view) if view.default => Target::Repo(view.repo.clone()),
            Workspace::Repo(view) => Target::Branch(view.repo.clone(), view.branch.clone()),
            Workspace::Commit(view) => Target::Commit(view.repo.clone(), view.commit.sha.clone()),
            Workspace::Compare(view) => {
                Target::Compare(view.repo.clone(), view.base.clone(), view.head.clone())
            }
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
            Workspace::Compare(view) => Some((view.tree.clone(), view.base_tree.clone())),
        }
    }
}

/// Sign-in status, as far as the UI needs to know. The token itself lives in a
/// separate signal so it is never part of anything rendered.
#[derive(Clone, PartialEq)]
pub enum Account {
    /// Startup: looking for a saved token.
    Checking,
    /// A pasted token, on its way to GitHub to be checked.
    ///
    /// Kept apart from [`Account::Checking`] because the two mean different
    /// things to the picker: that one is the whole panel waiting to learn what
    /// it may ask for, this one is one form waiting on one answer, with the
    /// panel live around it.
    Verifying,
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
    /// What the list itself does not say about the pull requests in it. Part
    /// of the list rather than beside it, so that it cannot outlive the list it
    /// is about: a new list arrives with nothing known, however much was known
    /// about the last one.
    pub more: More,
}

/// How a list of pull requests is getting on.
///
/// Each behind an `Rc`, because a pull request now brings its description with
/// it and a list of them is handed around — to the rows, to the pane that reads
/// one out — far more often than it changes.
#[derive(Clone, PartialEq)]
pub enum Got {
    Loading,
    Ready(Vec<Rc<PrSummary>>),
    Failed(String),
}

/// The checks, reviews and sizes of a list's pull requests — see
/// [`PrMore`] for why they are not simply part of it.
///
/// They arrive a page at a time, from the top of the list down, so what is
/// known and how the asking is going are two things: a row near the top has its
/// checks while the rows under it are still waiting for theirs, and a page that
/// fails takes nothing back from the pages before it.
#[derive(Clone, PartialEq, Default)]
pub struct More {
    /// By pull request number.
    pub known: Rc<HashMap<u64, PrMore>>,
    pub got: MoreGot,
}

#[derive(Clone, PartialEq, Default)]
pub enum MoreGot {
    /// Not asked for. Nobody has looked at the board, or nobody is signed in to
    /// ask as.
    #[default]
    Idle,
    Loading,
    Done,
    Failed(String),
}

impl More {
    /// Whether there is nothing to show for it and nothing on its way — which
    /// is when the board does without the columns it fills.
    pub fn absent(&self) -> bool {
        self.known.is_empty() && matches!(self.got, MoreGot::Idle | MoreGot::Failed(_))
    }
}

impl PrList {
    /// The pull requests themselves, empty until they are here.
    pub fn items(&self) -> &[Rc<PrSummary>] {
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

/// Where the commits in the pane come from — the two lists worth reading one
/// commit at a time.
///
/// It is what the list is keyed by rather than a label on it: stepping into one
/// of the commits keeps the list it was clicked in, and the way that is decided
/// is by asking whether the answer to this has changed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CommitSource {
    /// Every commit on a pull request, oldest first — the order they were
    /// written in.
    Pr(RepoRef, u64),
    /// A branch's history, newest first, a page at a time.
    Branch(RepoRef, String),
    /// What lies between two refs, oldest first — the same list a pull request
    /// would have, for the pull request nobody has opened.
    Compare(RepoRef, String, String),
}

impl CommitSource {
    pub fn repo(&self) -> &RepoRef {
        match self {
            CommitSource::Pr(repo, _)
            | CommitSource::Branch(repo, _)
            | CommitSource::Compare(repo, ..) => repo,
        }
    }

    /// The branch this is the history of, when it is one.
    pub fn branch(&self) -> Option<&str> {
        match self {
            CommitSource::Branch(_, name) => Some(name),
            _ => None,
        }
    }

    /// Whether more of this list is a request away rather than the end of what
    /// GitHub will say — which is true of the two that arrive a page at a time
    /// and not of a pull request's, which arrives whole.
    pub fn is_paged(&self) -> bool {
        !matches!(self, CommitSource::Pr(..))
    }

    /// Which way the page after this one goes. A branch is read newest first,
    /// so the next page is older; a comparison is read oldest first, so the
    /// next page is the commits after the ones on screen.
    pub fn older_first(&self) -> bool {
        matches!(self, CommitSource::Branch(..))
    }
}

/// The commits beside what is open — a pull request's, or a branch's.
///
/// Idle until somebody asks, unlike the conversation: the commits are one more
/// request, and a review that never opens the tab should not spend it.
#[derive(Clone, PartialEq)]
pub enum CommitList {
    Idle,
    Loading,
    Ready(Box<Commits>),
    /// A page of history in hand and an older one on its way — the list stays
    /// on screen while it comes, since it is what was being read.
    More(Box<Commits>),
    Failed(String),
}

impl CommitList {
    /// The commits it holds, if it holds any yet.
    pub fn items(&self) -> Option<&Commits> {
        match self {
            CommitList::Ready(c) | CommitList::More(c) => Some(c),
            _ => None,
        }
    }
}

/// The branches of the repository whatever is open belongs to.
///
/// Asked for on the same terms as the commits: one request, and only once
/// somebody opens the tab. Kept for as long as that repository is, since
/// stepping through its branches is exactly what the list is for.
#[derive(Clone, PartialEq)]
pub enum BranchList {
    Idle,
    Loading,
    Ready(Box<Branches>),
    /// The pages read so far in hand and the next one on its way — the list
    /// stays on screen while it comes, since it is what was being read down.
    More(Box<Branches>),
    Failed(String),
}

impl BranchList {
    /// The branches it holds, if it holds any yet.
    pub fn items(&self) -> Option<&Branches> {
        match self {
            BranchList::Ready(b) | BranchList::More(b) => Some(b),
            _ => None,
        }
    }
}

/// What ran against the commit on screen — and how it went.
///
/// Asked for rather than fetched with the pull request, like the commits: it is
/// two more requests, and it is about a commit rather than about the review, so
/// stepping into one of them asks again.
#[derive(Clone, PartialEq)]
pub enum CheckList {
    Idle,
    Loading,
    Ready(Box<Checks>),
    Failed(String),
}

/// What one check marked up in the code, once somebody has opened that check.
///
/// One entry per check run, keyed by its id — so a row opened, closed and
/// opened again costs one request rather than three. Emptied whenever the
/// checks themselves are, since the ids belong to those.
#[derive(Clone, PartialEq)]
pub enum Annots {
    Loading,
    Ready(Vec<Annotation>),
    Failed(String),
}

/// Which list the pane on the right is showing.
///
/// Three at a time, not four: [`Talk`](Self::Talk) is the heading of something
/// with a pull request behind it and [`Branches`](Self::Branches) the heading
/// of something without one, so the two are never offered together — see
/// `ConvPane`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ConvTab {
    #[default]
    Talk,
    Branches,
    Commits,
    Checks,
}

/// Which of the pane's headings is actually on show.
///
/// The one somebody last picked, unless what is open does not have it: a
/// repository read on its own has no conversation, and a commit reached by a
/// link of its own has no list of commits behind it. Both fall back to the
/// heading that leads the pane for what *is* open.
///
/// A free function over plain values because two things have to agree about it
/// — the pane that draws the list, and the effect that goes and fetches what
/// the list is of. A tab shown by one and not noticed by the other is a pane
/// that says "Loading…" for ever.
pub fn showing_tab(tab: ConvTab, ws: &Workspace) -> ConvTab {
    let first = if ws.has_review() {
        ConvTab::Talk
    } else {
        ConvTab::Branches
    };
    match tab {
        ConvTab::Talk | ConvTab::Branches => first,
        ConvTab::Commits if ws.commits_key().is_none() => first,
        other => other,
    }
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

/// A body being read in the middle pane rather than in the column on the
/// right: a pull request's description, or a comment long enough that a 380px
/// column was the wrong shape for it.
///
/// The text and not the parse: what it takes to draw this is a few hundred
/// microseconds, and holding the parse here would mean holding one copy of
/// every description ever opened for as long as the app is up.
#[derive(Clone, PartialEq)]
pub struct Reading {
    /// What the strip above the code calls it, and the heading it is read
    /// under.
    pub title: String,
    /// The line under the title: who wrote it, and what it is.
    pub meta: String,
    pub body: String,
    /// Where it is on github.com, for the button that opens it there.
    pub url: String,
}

/// A file held open in the strip above the code.
///
/// The [`Spot`] inside it is the same three things the trail is made of, for
/// the same reason: coming back to a tab has to be coming back to what was
/// being read there, not to the top of that file in whatever view something
/// else was last in. Where it was scrolled to belongs with these and is
/// deliberately not here — see [`super::tabs`] for where it is instead, and
/// why.
#[derive(Clone, PartialEq)]
pub struct OpenTab {
    pub at: Spot,
    /// When this tab was last looked at, counted rather than clocked. What
    /// decides which one is let go when the strip is full.
    used: u32,
}

/// How many files the strip holds before the one looked at longest ago is let
/// go. Enough for everything one change touches, and few enough that a tab is
/// still wide enough to carry a name.
const TABS: usize = 12;

/// How many closed files are remembered for the key that picks one back up.
const REOPEN: usize = 10;

/// Both sides of one file, fetched on demand.
///
/// The sides are `Rc` because every write to the cache re-runs the viewer's
/// memo over the open file: shared, that is a pointer bump and a pointer
/// compare, not a copy of the file.
#[derive(Clone, PartialEq)]
pub enum PrFileState {
    Loading,
    Ready {
        base: Rc<FileContent>,
        head: Rc<FileContent>,
    },
    Failed(String),
}

/// One picture out of the repository, on its way to being drawn.
///
/// `Ready` holds the whole file as a `data:` URL — see
/// [`crate::backend::images`] for why it has to be the whole file. Shared
/// behind an `Rc`, because a README redrawn for a theme change would otherwise
/// copy every screenshot in it.
#[derive(Clone, PartialEq)]
pub enum ImgState {
    Loading,
    Ready(Rc<str>),
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
    /// The description or comment being read in the middle pane, when one is.
    ///
    /// Beside `open` rather than instead of it: what it takes the pane over
    /// from is still open, still in the strip, and comes back as it was the
    /// moment its tab is clicked.
    pub reading: Signal<Option<Reading>>,
    /// The summary page last opened — a `.pullspace/` page, see
    /// [`crate::backend::summary`] — which keeps a tab at the head of the strip
    /// until it is closed, so following a link out of it into the code is one
    /// click from coming back.
    pub summary: Signal<Option<PathBuf>>,
    /// Whether that page has the pane, rather than the file underneath it.
    pub summary_on: Signal<bool>,
    /// Whether the reader is set to the full width of the pane rather than to
    /// a measure. A fact about how somebody likes to read, so it outlives the
    /// document it was set on.
    pub read_wide: Signal<bool>,
    /// Whether the reader draws its contents down the side. On by default,
    /// and worth turning off for a description with three headings in it.
    pub read_toc: Signal<bool>,
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
    /// The heading a link has just asked for, so that a `<details>` holding
    /// it can unfold itself on the way there.
    ///
    /// Set, never cleared — like `scroll_to`, what reads this is listening for
    /// the write and what the signal holds afterwards means nothing. Asking
    /// twice for the same heading has to fire twice, and a value that was
    /// cleared could not say the difference between the two.
    pub anchor: Signal<Option<String>>,
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
    /// The files being held open, in the order the strip draws them. The one
    /// on screen is whichever of them `open` names — there is no second place
    /// recording which tab is current, because that is what `open` already is.
    pub tabs: Signal<Vec<OpenTab>>,

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
    /// The find bar over the open file: whether it is up, what is in it, and
    /// which of its hits is the current one.
    ///
    /// Its own text and its own toggles, kept apart from the search box in the
    /// top bar. They are two different questions — "where is this in this
    /// file" and "where is this in this repository" — and typing one into the
    /// other is how an editor loses somebody's place.
    pub find_open: Signal<bool>,
    pub find_text: Signal<String>,
    pub find_opts: Signal<Options>,
    /// Which hit Enter is standing on, as an index into the lines matched.
    pub find_at: Signal<Option<usize>>,
    /// The lines of the open file the find bar is matching, in order. Derived
    /// by the viewer, for the same reason as `change_lines`: the keyboard has
    /// no file to look in.
    pub find_lines: Signal<Vec<usize>>,
    /// Where each run of changes in the open file begins, as lines of the new
    /// side — what F7 steps through. Derived by the viewer, which is the only
    /// thing here holding the diff.
    pub change_lines: Signal<Vec<usize>>,
    /// The files put down, most recent last, for the key that picks one back
    /// up. Closing a tab by mistake should not cost the file.
    pub closed: Signal<Vec<Spot>>,
    /// The first line still on screen in the code pane, as the page reports it
    /// — what the header pinned above the code is worked out from.
    pub top_line: Signal<Option<usize>>,
    /// The picker: whether it is up, what has been typed into it, and which
    /// row the arrow keys are on. One overlay for files, commands, symbols and
    /// line numbers — see [`super::palette`].
    pub picker: Signal<bool>,
    pub picker_text: Signal<String>,
    pub picker_at: Signal<usize>,
    /// Whether the explorer is showing what the open file defines.
    pub outline_open: Signal<bool>,
    pub search_text: Signal<String>,
    pub search_opts: Signal<Options>,
    /// Whether the box is looking for files by name rather than for text
    /// inside them — the fourth toggle beside the three that shape the
    /// pattern. Per space, like the pattern itself.
    pub search_files: Signal<bool>,
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
    /// Whether that list has the page to itself — see [`super::prboard`].
    pub pr_board: Signal<bool>,
    /// What is being fetched from GitHub, if anything, and what went wrong the
    /// last time something was.
    pub fetch: Signal<Fetch>,
    pub workspace: Signal<Workspace>,
    /// Where this space is going, while it is still on its way there — see
    /// [`spaces::Held`]. An empty workspace with this set is an arrival in
    /// progress rather than an app with nothing in it, and that is the
    /// difference between the landing page and the frame that stands in for
    /// what is coming.
    pub incoming: Signal<Option<Route>>,
    /// Per-file base/head content for what is open — decoded, and only for
    /// files somebody has actually looked at. The repository itself lives on
    /// disk; this is the shelf by the desk, not the library.
    pub pr_files: Signal<HashMap<PathBuf, PrFileState>>,
    /// What is in `pr_files`, oldest first, so the shelf can be kept to a size.
    pub warm_order: Signal<VecDeque<PathBuf>>,
    /// The pictures a document has asked for, as URLs it can be drawn with.
    /// Kept apart from `pr_files` because they are a different shape — bytes
    /// rather than text — and a different size: see [`super::imgcache`], which
    /// is the only thing that writes either of these two.
    pub images: Signal<HashMap<PathBuf, ImgState>>,
    /// What is in `images`, oldest first.
    pub image_order: Signal<VecDeque<PathBuf>>,
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
    /// And its commits, which the same pane shows in place of them — or the
    /// history of the branch being browsed, which is the same list of a
    /// different thing.
    pub commits: Signal<CommitList>,
    /// And the branches of the repository all of it is inside.
    pub branches: Signal<BranchList>,
    /// And what ran against the commit on screen — the third of the three.
    pub checks: Signal<CheckList>,
    /// What each opened check marked up, by check run id.
    pub annots: Signal<HashMap<u64, Annots>>,
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
    /// The left-hand side's share of a side-by-side diff, from 0 to 1.
    pub split: Signal<f64>,
    /// How wide that diff was last drawn, which is what turns a drag in
    /// pixels into a change of share. `0` until it is first drawn.
    pub split_w: Signal<f64>,
    /// The area the two panes share, as last measured. `(0, 0)` until the
    /// first report from the resize observer.
    pub main_size: Signal<(f64, f64)>,
    /// Whether the browser is holding the screen for this page — see
    /// [`super::full`], which is the only thing that writes it.
    ///
    /// A report rather than a setting: nothing in the app reads it but the
    /// button that asks for it, because nothing in the app looks any different
    /// in fullscreen. Not a per-space thing either, for the same reason the
    /// theme is not: the window is the window, and stepping to the review
    /// beside this one is no reason to hand the screen back.
    pub full: Signal<bool>,
    /// The divider in hand, while one is.
    pub drag: Signal<Option<Drag>>,
    /// The last divider pressed and when — how a double-click is spotted.
    pub last_grab: Signal<Option<(Edge, Instant)>>,

    // --- the spaces ---
    /// Every pullspace open in this browser tab, in the order the switcher
    /// draws them. All of them but the one on screen keep their state in here;
    /// the one on screen keeps it on the signals above — see
    /// [`super::spaces`].
    pub spaces: Signal<Vec<Space>>,
    /// Which of them that is.
    ///
    /// Not part of a space's state, for the obvious reason, and not restored
    /// by a switch either: it is what a switch *changes*, and it is what
    /// everything fetching on a reader's behalf checks before it writes what
    /// it fetched — see [`spaces::Claim`].
    pub space: Signal<u32>,

    // --- writing back ---
    /// Reviews being written, by pull request ([`viewed::pr_key`]) — a copy of
    /// what [`drafts`](crate::backend::drafts) keeps, filled in as each pull
    /// request is opened. Keyed rather than per space, so it is global: two
    /// spaces on one pull request are writing one review.
    pub drafts: Signal<HashMap<String, Pending>>,
    /// The line a comment is being written on, if one is — see
    /// [`super::review`].
    pub composing: Signal<Option<Composing>>,
    /// The one write to GitHub in flight, or the last one that failed.
    pub writing: Signal<Writing>,
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

    /// [`showing_tab`], for the two signals it is about. A reactive read of
    /// both: the pane redraws and the fetches re-run when either moves.
    pub fn conv_showing(&self) -> ConvTab {
        let held = self.workspace.read();
        showing_tab(*self.conv_tab.read(), &held)
    }

    pub(super) fn bump_tick(&self) {
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
        Workspace::Compare(view) => Some(viewed::compare_key(&view.repo, &view.base, &view.head)),
        _ => None,
    }
}

/// One tree entry's path, as the string the picker matches against.
fn display(p: &Path) -> String {
    p.display().to_string()
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
        // And so do the spaces, if this tab has been here before. Every one of
        // them comes back as a link; the one that was on screen is opened by
        // the address bar, which is where it was written on the way out —
        // unless the address bar is a link handed over from outside, which
        // gets a space of its own rather than the one that was on screen.
        let (mut spaces, mut on) = spaces::load();
        if let Some(link) = route::handed() {
            on = spaces::make_room(&mut spaces, on, &link);
        }
        spaces::wake(&mut spaces, on);
        // The state of the space the app starts in. Every per-space signal
        // below is seeded from it, so what an empty space holds is stated in
        // one place — see `spaces::Held`.
        let h = Held::fresh();
        St {
            statuses: root(h.statuses),
            open: root(h.open),
            reading: root(h.reading),
            summary: root(h.summary),
            summary_on: root(h.summary_on),
            read_wide: root(false),
            read_toc: root(true),
            view_mode: root(h.view_mode),
            scroll_to: root(h.scroll_to),
            at_line: root(h.at_line),
            anchor: root(h.anchor),
            expanded: root(h.expanded),
            expansions: root(h.expansions),
            tree_seeded: root(h.tree_seeded),
            tree_filter: root(h.tree_filter),
            changes_only: root(h.changes_only),
            viewed: root(h.viewed),
            changed_files: root(h.changed_files),
            refresh_tick: root(0),
            trail: root(h.trail),
            ahead: root(h.ahead),
            tabs: root(h.tabs),

            selected: root(h.selected),
            panel: root(h.panel),
            index: root(h.index),
            scan_seq: root(0),
            find_open: root(h.find_open),
            find_text: root(h.find_text),
            find_opts: root(h.find_opts),
            find_at: root(h.find_at),
            find_lines: root(h.find_lines),
            change_lines: root(h.change_lines),
            closed: root(h.closed),
            // Not per space: it is a fact about the pane the reader is looking
            // at this instant, and the pane is scrolled back to where it was
            // on the way into a space anyway.
            top_line: root(None),
            picker: root(false),
            picker_text: root(String::new()),
            picker_at: root(0),
            outline_open: root(false),
            search_text: root(h.search_text),
            search_opts: root(h.search_opts),
            search_files: root(h.search_files),
            search_error: root(h.search_error),

            token: root(None),
            account: root(Account::Checking),
            // The overlay is asked for, never assumed: with nothing open the
            // landing page is the picker, and it is up because the workspace
            // is empty rather than because this is set.
            gh_open: root(h.gh_open),
            repo_input: root(h.repo_input),
            prs: root(h.prs),
            pr_state: root(h.pr_state),
            pr_board: root(h.pr_board),
            fetch: root(h.fetch),
            workspace: root(h.workspace),
            // The one field not taken from `h`, because the answer is not in
            // the app: a tab opened on a link is a tab that is already going
            // somewhere, and this is read before the first frame so that the
            // front page is never drawn over the top of it. The load itself
            // is `nav::landing`, several ticks behind.
            incoming: root(route::arriving()),
            pr_files: root(h.pr_files),
            warm_order: root(h.warm_order),
            images: root(h.images),
            image_order: root(h.image_order),
            cloning: root(h.cloning),
            store_gen: root(0),
            conv: root(h.conv),
            conv_open: root(h.conv_open),
            commits: root(h.commits),
            branches: root(h.branches),
            checks: root(h.checks),
            annots: root(h.annots),
            conv_tab: root(h.conv_tab),

            pending: root(h.pending),

            prefs: root(look),
            prefs_open: root(false),

            side_w: root(saved.side_w),
            conv_w: root(saved.conv_w),
            bottom_h: root(saved.bottom_h),
            split: root(saved.split),
            split_w: root(0.0),
            main_size: root((0.0, 0.0)),
            full: root(false),
            drag: root(None),
            last_grab: root(None),

            spaces: root(spaces),
            space: root(on),
            drafts: root(HashMap::new()),
            composing: root(None),
            writing: root(Writing::Idle),
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
            Workspace::Compare(view) => build_tree_from_paths(
                &format!("{} {}...{}", view.repo, view.base, view.head),
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

    // Every path in what is open, as the strings the picker matches against.
    //
    // Only while the picker is up. It is a few thousand short allocations on a
    // real repository, which is nothing once — and would be nothing worth
    // doing at all on every click that opens a file.
    let all_paths: Memo<Rc<Vec<String>>> = use_memo(move || {
        if !*st.picker.read() {
            return Rc::new(Vec::new());
        }
        st.refresh_tick.read();
        let held = st.workspace.read();
        let paths = match &*held {
            Workspace::Empty => Vec::new(),
            Workspace::Pr(pr) => pr.tree.paths().map(display).collect(),
            Workspace::Repo(view) => view.tree.paths().map(display).collect(),
            Workspace::Commit(view) => view.tree.paths().map(display).collect(),
            Workspace::Compare(view) => view.tree.paths().map(display).collect(),
        };
        Rc::new(paths)
    });
    use_context_provider(|| all_paths);

    // What the open file defines, in the order it defines it. Three things ask
    // — the header pinned over the code, the explorer's outline, and the
    // picker — and all three want the same answer, so it is worked out once.
    let file_syms: Memo<Rc<Vec<Symbol>>> = use_memo(move || {
        st.refresh_tick.read();
        let Some(rel) = st.open.read().clone() else {
            return Rc::new(Vec::new());
        };
        let files = st.pr_files.read();
        let Some(PrFileState::Ready { base, head }) = files.get(&rel) else {
            return Rc::new(Vec::new());
        };
        // The new side, or — for a file the change deletes — the old one,
        // which is the version the viewer is showing.
        let text = match (head.as_ref(), base.as_ref()) {
            (FileContent::Text(t), _) => t,
            (FileContent::Absent, FileContent::Text(t)) => t,
            _ => return Rc::new(Vec::new()),
        };
        let mut out = Vec::new();
        symbols::scan_file(&rel, text, &mut out);
        Rc::new(out)
    });
    use_context_provider(|| file_syms);

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
        // This runs again whenever the workspace is written, and coming back
        // to a space writes it. A clone that has already finished is the clone
        // for what is open — `St::enter` is what clears the progress, so
        // anything genuinely new still starts one.
        if st
            .cloning
            .peek()
            .is_some_and(|at| at.total > 0 && at.finished())
        {
            return;
        }
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
        // And as above: an index already built is the index for what is open,
        // and coming back to a space is not a reason to read forty thousand
        // files again. `St::clear_ide` drops it when the commit moves.
        if matches!(*st.index.peek(), Index::Ready { .. }) {
            return;
        }
        spawn_forever(super::ide::build_index(st, files));
    });

    // The keyboard, for as long as the app is up. It has to be installed on
    // the window rather than on an element here — for most of this app's life
    // the focus is on the document body, which is above everything a handler
    // in this tree could be attached to.
    use_future(move || super::ide::keys(st));
    // And the one thing about fullscreen the app cannot hear as a click: the
    // browser handing the screen back on Escape, or on its own control for it.
    use_future(move || super::full::watch(st));

    // And the other thing listened for on the document: a click on a line
    // number, which is how a line gets picked out to link to.
    use_future(move || super::viewer::lines(st));
    // And the `+` beside a line of a pull request's diff, which starts a comment
    // on it — see `review::NOTE_JS`.
    use_future(move || super::review::adds(st));
    // A summary's links, which come out of its frame as messages — see
    // `super::summary`.
    use_future(move || super::summary::links(st));

    // And which line is at the top of the code pane, for the header pinned
    // over it.
    use_future(move || super::viewer::tops(st));

    // The third: where each open file is scrolled to, which the page keeps on
    // our behalf — a wheel notch is not worth a render. See `super::tabs`.
    // Told which space it is keeping them for, since the page outlives every
    // one of them.
    use_effect(move || {
        tabs::watch();
        tabs::use_space(*st.space.peek());
    });

    // What the switcher calls the space on screen, kept in step with what it
    // has open and where the reader is in it — and the whole list written down
    // as either moves, so a reload comes back to the same dozen reviews rather
    // than to one.
    use_effect(move || {
        let open = st.open.read();
        let ws = st.workspace.read();
        let card = match st.incoming.read().as_ref() {
            // Still arriving: the link is all there is to go on, and it is
            // better than the "New space" an empty workspace would be called.
            Some(route) if !ws.is_open() => Card::arriving(route),
            _ => Card::of(&ws, open.as_deref()),
        };
        spaces::describe(&st, card);
        spaces::save(&st);
    });

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

    // The review being written on whichever pull request is open, off disk the
    // first time it is — so a reload, or coming back to it tomorrow, finds the
    // line comments where they were left.
    use_effect(move || {
        let Some((repo, number)) = st.workspace.read().review_key() else {
            return;
        };
        let key = viewed::pr_key(&repo, number);
        if st.drafts.peek().contains_key(&key) {
            return;
        }
        let saved = crate::backend::drafts::load(&key);
        let mut drafts = st.drafts;
        drafts.write().insert(key, saved);
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

    // And the commits beside whatever is open — a pull request's, or the
    // history of the branch being browsed — once somebody looks at the tab they
    // are under. One request, spent on being asked for rather than on every
    // review that never opens it.
    use_effect(move || {
        let asked = st.conv_showing() == ConvTab::Commits;
        let source = st.workspace.read().commits_key();
        let Some(source) = source.filter(|_| asked) else {
            return;
        };
        if !matches!(*st.commits.peek(), CommitList::Idle) {
            return;
        }
        spawn_forever(super::conversation::load_commits(st, source));
    });

    // And the branches of the repository all of it is inside, on the same
    // terms. They belong to the repository rather than to what is open in it,
    // so this is the one of the three that survives opening a commit.
    use_effect(move || {
        let asked = st.conv_showing() == ConvTab::Branches;
        let repo = st.workspace.read().repo_ref().cloned();
        let Some(repo) = repo.filter(|_| asked) else {
            return;
        };
        if !matches!(*st.branches.peek(), BranchList::Idle) {
            return;
        }
        spawn_forever(super::conversation::load_branches(st, repo));
    });

    // And what ran against the commit on screen, on the same terms: two
    // requests, spent when somebody asks whether the build is green.
    use_effect(move || {
        let asked = st.conv_showing() == ConvTab::Checks;
        let target = st.workspace.read().checks_key();
        let Some((repo, sha)) = target.filter(|_| asked) else {
            return;
        };
        if !matches!(*st.checks.peek(), CheckList::Idle) {
            return;
        }
        spawn_forever(super::conversation::load_checks(st, repo, sha));
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
        "--side-w:{:.0}px;--conv-w:{:.0}px;--bottom-h:{:.0}px;--split:{:.4}",
        *st.side_w.read(),
        *st.conv_w.read(),
        *st.bottom_h.read(),
        *st.split.read(),
    );

    // The chosen palette, font and size, as the stylesheet that applies them.
    // After the app's own, so it wins — and nowhere near it, so the defaults
    // stay readable as the defaults. Memoised because `App` re-renders on
    // every divider-drag frame, and thirty format'd properties per mousemove
    // is not what a drag should cost.
    let look = use_memo(move || st.prefs.read().css());

    rsx! {
        style { dangerous_inner_html: CSS }
        style { dangerous_inner_html: "{look}" }
        Tab {}
        div { class: "app", style: "{panes}",
            // The IDE is for something being open. With nothing open it would
            // be three empty panes under a modal, so the landing page stands
            // in for the lot — including the top bar, whose every control is
            // about what is open.
            if st.workspace.read().is_open() {
                TopBar {}
                div {
                    class: "main",
                    // What the dividers are allowed to give away — and, when
                    // the window has just given away some of it, the moment
                    // the panes are moved back inside what is left. The panes
                    // are `flex: none`, so this is the only thing that resizes
                    // them, which is what makes the sizes here the sizes on
                    // screen.
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
                // Under the panels and over the panes: everything but the bar
                // it was opened from.
                if *st.pr_board.read() {
                    PrBoard {}
                }
                if *st.gh_open.read() {
                    GhPanel {}
                }
            } else if let Some(route) = st.incoming.read().clone() {
                // Empty, but not for long: something is being fetched into
                // this space. The front page is not what somebody who followed
                // a link is waiting to read, so this says where they are going
                // instead — see `super::opening`.
                Opening { route }
            } else {
                Landing {}
            }
            if *st.prefs_open.read() {
                PrefsPanel {}
            }
            // Above everything, including the panels: it is the way to the
            // things they are for.
            if *st.picker.read() {
                Picker {}
            }
            DragMask {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::github::{CommitSummary, Snapshot};

    fn repo() -> RepoRef {
        RepoRef {
            owner: "bigmah".to_string(),
            name: "pullspace".to_string(),
        }
    }

    fn browsing(branch: &str, default: bool) -> Workspace {
        Workspace::Repo(Box::new(RepoView {
            repo: repo(),
            branch: branch.to_string(),
            default,
            head_sha: "tip".to_string(),
            tree: Snapshot::default(),
        }))
    }

    fn one_commit(from: CommitFrom) -> Workspace {
        Workspace::Commit(Box::new(CommitView {
            repo: repo(),
            commit: CommitSummary {
                sha: "abc1234def".to_string(),
                message: "a change".to_string(),
                author: "ada".to_string(),
                date: String::new(),
                html_url: String::new(),
            },
            parent_sha: "bbb2222".to_string(),
            merge: false,
            files: Vec::new(),
            truncated: false,
            tree: Snapshot::default(),
            base_tree: Snapshot::default(),
            from,
        }))
    }

    /// The list a commit was clicked in stays beside it — which is what makes
    /// reading a branch, or a pull request, a matter of clicking down that list.
    #[test]
    fn a_commit_is_read_beside_whatever_it_was_opened_out_of() {
        let header = PrHeader {
            number: 7,
            ..PrHeader::default()
        };
        assert_eq!(
            one_commit(CommitFrom::Pr(header)).commits_key(),
            Some(CommitSource::Pr(repo(), 7))
        );
        assert_eq!(
            one_commit(CommitFrom::Branch("dev".to_string())).commits_key(),
            Some(CommitSource::Branch(repo(), "dev".to_string()))
        );
        // Reached by a link of its own: nothing around it to go on showing.
        assert_eq!(one_commit(CommitFrom::Alone).commits_key(), None);
        // And a branch being browsed is its own history.
        assert_eq!(
            browsing("dev", false).commits_key(),
            Some(CommitSource::Branch(repo(), "dev".to_string()))
        );
    }

    fn comparing(base: &str, head: &str) -> Workspace {
        Workspace::Compare(Box::new(crate::backend::github::CompareView {
            repo: repo(),
            base: base.to_string(),
            head: head.to_string(),
            base_sha: "merge-base".to_string(),
            head_sha: "tip-of-head".to_string(),
            base_tip: "tip-of-base".to_string(),
            status: "ahead".to_string(),
            ahead: 5,
            behind: 0,
            files: Vec::new(),
            truncated: false,
            tree: Snapshot::default(),
            base_tree: Snapshot::default(),
            commits: Commits::default(),
            html_url: String::new(),
        }))
    }

    /// A comparison is a pull request nobody opened: the same two trees, the
    /// same list of what differs, and a link of its own to send somebody.
    #[test]
    fn a_comparison_is_read_the_way_a_pull_request_is() {
        let ws = comparing("main", "feat/thing");
        assert_eq!(
            ws.target(),
            Target::Compare(repo(), "main".to_string(), "feat/thing".to_string())
        );
        // Its commits are the ones between the two…
        assert_eq!(
            ws.commits_key(),
            Some(CommitSource::Compare(
                repo(),
                "main".to_string(),
                "feat/thing".to_string()
            ))
        );
        // …and its checks are about the commit on screen, which is the head.
        assert_eq!(ws.checks_key(), Some((repo(), "tip-of-head".to_string())));
        // No pull request behind it, so the pane leads with the branches.
        assert!(!ws.has_review());
        assert_eq!(showing_tab(ConvTab::Talk, &ws), ConvTab::Branches);
    }

    /// The two lists that arrive a page at a time go opposite ways, and the
    /// button that asks for the next page has to say which.
    #[test]
    fn only_the_paged_lists_have_a_next_page() {
        let branch = CommitSource::Branch(repo(), "dev".to_string());
        let compare = CommitSource::Compare(repo(), "main".to_string(), "dev".to_string());
        let pr = CommitSource::Pr(repo(), 7);

        assert!(branch.is_paged() && compare.is_paged());
        assert!(!pr.is_paged(), "a pull request's commits arrive whole");
        // A branch is read newest first, so the page after it is older; a
        // comparison is read oldest first, so it is newer.
        assert!(branch.older_first());
        assert!(!compare.older_first());
    }

    /// A link with no branch written in it opens whatever the default branch is
    /// *now*, which is the right answer for that branch and the wrong one for
    /// every other.
    #[test]
    fn a_branch_that_is_not_the_default_one_says_so_in_the_link() {
        assert_eq!(browsing("main", true).target(), Target::Repo(repo()));
        assert_eq!(
            browsing("feat/thing", false).target(),
            Target::Branch(repo(), "feat/thing".to_string())
        );
    }

    /// The pane leads with the conversation where there is one and with the
    /// branches where there is not — and the tab last picked is kept across the
    /// switch wherever the new thing has it.
    #[test]
    fn the_pane_falls_back_to_the_heading_the_thing_on_screen_leads_with() {
        // A repository read on its own: the default tab is a conversation it
        // does not have.
        assert_eq!(
            showing_tab(ConvTab::Talk, &browsing("dev", false)),
            ConvTab::Branches
        );
        // And the other way about, for somebody who came from one.
        let pr = one_commit(CommitFrom::Pr(PrHeader {
            number: 7,
            ..PrHeader::default()
        }));
        assert_eq!(showing_tab(ConvTab::Branches, &pr), ConvTab::Talk);
        // A commit reached by a link of its own has no list of commits behind
        // it, so that heading falls back too.
        assert_eq!(
            showing_tab(ConvTab::Commits, &one_commit(CommitFrom::Alone)),
            ConvTab::Branches
        );
        // What is kept: a heading both of them have.
        assert_eq!(
            showing_tab(ConvTab::Commits, &browsing("dev", false)),
            ConvTab::Commits
        );
        assert_eq!(showing_tab(ConvTab::Checks, &pr), ConvTab::Checks);
    }

    /// A check is a fact about a commit, so the question is asked of whichever
    /// commit is on screen — the tip of the branch being read, or the one
    /// stepped into.
    #[test]
    fn checks_are_asked_of_the_commit_on_screen() {
        assert_eq!(
            browsing("dev", false).checks_key(),
            Some((repo(), "tip".to_string()))
        );
        assert_eq!(
            one_commit(CommitFrom::Alone).checks_key(),
            Some((repo(), "abc1234def".to_string()))
        );
        assert_eq!(Workspace::Empty.checks_key(), None);
    }
}
