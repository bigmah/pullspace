//! Spaces: several pullspaces inside one browser tab.
//!
//! A review is never one pull request. It is this one, the two it depends on,
//! the branch you went to read for context and the commit somebody linked in a
//! comment — and until now each of those was a browser tab, with the same app
//! loaded five times over, five clones of the same repository, and five
//! identical favicons to hunt through.
//!
//! A **space** is a whole pullspace: what is open, the files open inside it,
//! where the reader is in each of them, what the explorer has unfolded, what
//! the panes are showing. The app holds several and shows one, and the switcher
//! behind the name in the corner moves between them.
//!
//! # How nothing is lost
//!
//! There is one set of signals — [`St`] — and every component in the app reads
//! it. So a space is not a second copy of the app: it is the *contents* of
//! those signals, lifted off and put down again. [`Held`] is that state, one
//! field per signal, and [`Held::swap`] exchanges it with what the signals are
//! carrying in a single pass. What comes back is the space being left, whole
//! and unexamined, and what goes in is the space being entered exactly as it
//! was set down.
//!
//! Two things do not live in a signal and so are dealt with by hand:
//!
//! - **Where each pane is scrolled to** is kept in the page — see
//!   [`super::tabs`], and the reason it is not a signal. It is keyed by the
//!   space it belongs to, so nothing has to be moved on a switch; the map just
//!   stops being asked about one space and starts being asked about another.
//! - **Work in flight** — a clone, an index walk, a fetch three requests deep —
//!   belongs to the space it was started in. The generation counter retires
//!   the first two (see [`St::generation`]), and everything that goes to GitHub
//!   on a reader's behalf holds a [`Claim`] on the space it was asked in and
//!   drops what it fetched rather than landing it in somebody else's window.
//!
//! # Between sessions
//!
//! The list survives a reload, in session storage: a reload is a mistake to be
//! made with twelve reviews open, not a reason to lose them. What is kept is
//! the *link* to each — a space read back off storage is a route and a label
//! until somebody opens it, and opening it is the same fetch a pasted link
//! would do. Session storage rather than local: a second window is a second
//! desk, not a second view of this one.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::backend::auth::open_tab;
use crate::backend::clone::Progress;
use crate::backend::difftool::Expansion;
use crate::backend::github::PrState;
use crate::backend::route::{self, Place, Route, Target};
use crate::backend::search::Options;
use crate::backend::store;
use crate::backend::tree::ChangeKind;

use super::app::{
    Annots, BranchList, CheckList, CommitList, ConvTab, Conversation, Fetch, ImgState, OpenTab,
    PrFileState, PrList, Reading, Spot, St, ViewMode, Workspace,
};
use super::github::open_route;
use super::ide::{Index, Panel};
use super::tabs;

/// Every per-space signal, declared once.
///
/// The list below is the whole definition of what a space *is*, and it is a
/// list rather than forty hand-written moves for one reason: a field stowed
/// and not restored — or restored and not stowed — is state that leaks from
/// one review into another, and there is no way to notice it except by using
/// the app for a week. Written this way the two directions are the same line
/// of code, so they cannot disagree.
macro_rules! held {
    ($( $(#[$why:meta])* $name:ident : $ty:ty = $fresh:expr ),* $(,)?) => {
        /// The contents of one space.
        pub struct Held {
            $( $(#[$why])* pub $name: $ty, )*
        }

        impl Held {
            /// A space with nothing opened in it. Also where [`super::app::App`]
            /// takes its starting values from, so the defaults are stated once.
            pub fn fresh() -> Held {
                Held { $( $name: $fresh, )* }
            }

            /// Put this state on the signals and take back what was there.
            ///
            /// A move in both directions: nothing here is cloned, however
            /// large it is, and a swap costs the same for a pull request with
            /// four hundred files as for an empty space.
            fn swap(self, st: &St) -> Held {
                Held {
                    $( $name: {
                        // Signals are `Copy`, and writing one takes a `&mut`
                        // to the handle rather than to what it holds.
                        let mut slot = st.$name;
                        std::mem::replace(&mut *slot.write(), self.$name)
                    }, )*
                }
            }
        }
    };
}

held! {
    // --- what is open, and what it says about the files in it ---
    workspace: Workspace = Workspace::Empty,
    /// Where this space is on its way to, while it is still on its way.
    ///
    /// An empty workspace is two different things, and this is what tells them
    /// apart: nothing opened, which is what the landing page is for, and
    /// something opening, which is a review three requests away. Kept per
    /// space because arriving is per space — a link opened in one is not a
    /// reason to cover the review in another.
    ///
    /// The route rather than a flag, because a space can be put down mid-
    /// arrival: the fetch is retired with it, and coming back has to start
    /// that fetch again rather than sit on a screen nothing is working for.
    incoming: Option<Route> = None,
    statuses: HashMap<PathBuf, ChangeKind> = HashMap::new(),
    pending: Option<Place> = None,

    // --- the middle pane ---
    open: Option<PathBuf> = None,
    reading: Option<Reading> = None,
    view_mode: ViewMode = ViewMode::Source,
    at_line: Option<usize> = None,
    /// A jump asked for, kept with the space that asked: coming back to a file
    /// entered at a line means coming back to that line.
    scroll_to: Option<usize> = None,
    anchor: Option<String> = None,
    tabs: Vec<OpenTab> = Vec::new(),
    trail: Vec<Spot> = Vec::new(),
    ahead: Vec<Spot> = Vec::new(),
    expansions: HashMap<PathBuf, HashMap<usize, Expansion>> = HashMap::new(),

    // --- the explorer ---
    expanded: HashMap<PathBuf, bool> = HashMap::new(),
    tree_seeded: bool = false,
    tree_filter: String = String::new(),
    changes_only: bool = false,
    viewed: HashSet<String> = HashSet::new(),
    changed_files: Vec<PathBuf> = Vec::new(),

    // --- the IDE ---
    selected: Option<String> = None,
    find_open: bool = false,
    find_text: String = String::new(),
    find_opts: Options = Options::default(),
    find_at: Option<usize> = None,
    find_lines: Vec<usize> = Vec::new(),
    change_lines: Vec<usize> = Vec::new(),
    closed: Vec<Spot> = Vec::new(),
    panel: Panel = Panel::Hidden,
    index: Index = Index::Off,
    search_text: String = String::new(),
    search_opts: Options = Options::default(),
    search_files: bool = false,
    search_error: Option<String> = None,

    // --- GitHub, and what has been read out of it ---
    gh_open: bool = false,
    repo_input: String = String::new(),
    prs: Option<PrList> = None,
    pr_state: PrState = PrState::default(),
    fetch: Fetch = Fetch::Idle,
    conv: Conversation = Conversation::Loading,
    conv_open: bool = true,
    conv_tab: ConvTab = ConvTab::default(),
    commits: CommitList = CommitList::Idle,
    branches: BranchList = BranchList::Idle,
    checks: CheckList = CheckList::Idle,
    annots: HashMap<u64, Annots> = HashMap::new(),

    // --- what has been read off the network or the disk ---
    pr_files: HashMap<PathBuf, PrFileState> = HashMap::new(),
    warm_order: VecDeque<PathBuf> = VecDeque::new(),
    images: HashMap<PathBuf, ImgState> = HashMap::new(),
    image_order: VecDeque<PathBuf> = VecDeque::new(),
    cloning: Option<Progress> = None,
}

/// What one space is: its state, or — before anybody has opened it in this
/// session — the link that will fetch it.
enum State {
    Live(Box<Held>),
    /// Read back off session storage. Nothing has been fetched for it yet, and
    /// the first switch to it is the same work a pasted link would do.
    Away(Route),
}

/// One space, as the switcher knows it.
pub struct Space {
    pub id: u32,
    /// What to call it. Kept beside the state rather than derived from it:
    /// drawing the menu would otherwise mean reading — and matching on — every
    /// open pull request in the app, several times a second while one is
    /// loading.
    pub card: Card,
    state: State,
}

impl Space {
    /// Whether this one has been opened in this session. The switcher says so:
    /// a row that has not is a link, and clicking it costs a fetch.
    pub fn here(&self) -> bool {
        matches!(self.state, State::Live(_))
    }

    /// Where this space points — its own link, for the address bar, for
    /// session storage, and for breaking it out into a browser tab.
    ///
    /// The active space is the exception: its state is on the signals rather
    /// than in here, so it is asked of those instead.
    fn route(&self, st: &St) -> Route {
        if *st.space.peek() == self.id {
            // Mid-arrival, where it is going is a better answer than the
            // nothing it has to show for itself — that is the link a reload
            // should bring it back to.
            return st.incoming.peek().clone().unwrap_or_else(|| st.route());
        }
        match &self.state {
            State::Away(route) => route.clone(),
            State::Live(held) => held.incoming.clone().unwrap_or_else(|| Route {
                at: held.workspace.target(),
                place: held.open.clone().map(|path| Place {
                    path,
                    line: held.at_line,
                }),
            }),
        }
    }
}

/// Which of the four things a space has open, for the chip that says so.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    Empty,
    Pr,
    Repo,
    Commit,
    Compare,
}

impl Kind {
    pub fn of(ws: &Workspace) -> Kind {
        match ws {
            Workspace::Empty => Kind::Empty,
            Workspace::Pr(_) => Kind::Pr,
            Workspace::Repo(_) => Kind::Repo,
            Workspace::Commit(_) => Kind::Commit,
            Workspace::Compare(_) => Kind::Compare,
        }
    }

    /// The same, for a space that is still on its way to one — the chip is up
    /// before the workspace is, so it is read off the link instead.
    pub fn going(at: &Target) -> Kind {
        match at {
            Target::Home => Kind::Empty,
            Target::Pr(..) => Kind::Pr,
            Target::Repo(..) | Target::Branch(..) => Kind::Repo,
            Target::Commit(..) => Kind::Commit,
            Target::Compare(..) => Kind::Compare,
        }
    }

    /// The word on the chip.
    pub fn word(self) -> &'static str {
        match self {
            Kind::Empty => "nothing open",
            Kind::Pr => "pull request",
            Kind::Repo => "browsing",
            Kind::Commit => "commit",
            Kind::Compare => "comparing",
        }
    }

    /// And its colour. A commit and a comparison share one: both are two
    /// commits held up against each other, which is what the colour means.
    pub fn css(self) -> &'static str {
        match self {
            Kind::Empty => "wschip local",
            Kind::Pr => "wschip pr",
            Kind::Repo => "wschip repo",
            Kind::Commit | Kind::Compare => "wschip cmp",
        }
    }
}

/// What the switcher draws for one space.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Card {
    pub kind: Kind,
    /// Its name: `owner/repo #12`, a repository, a short sha.
    pub lead: String,
    /// What that is called: the title, the branch, the subject line.
    pub trail: String,
    /// And where the reader was left inside it — the file in the middle pane,
    /// which is the one thing that says "this is the one I was in the middle
    /// of".
    pub note: String,
}

impl Card {
    /// A space with nothing open yet.
    pub fn blank() -> Card {
        Card {
            kind: Kind::Empty,
            lead: "New space".to_string(),
            trail: String::new(),
            note: String::new(),
        }
    }

    /// A space that is on its way somewhere, named for where it is going.
    ///
    /// Without this a space woken from a link reads "New space" for as long as
    /// it takes to fetch — and, since the list is written down as it changes,
    /// is *saved* as one. The label a reload comes back to would be the label
    /// of the second the reader spent switching into it.
    pub fn arriving(route: &Route) -> Card {
        Card {
            kind: Kind::going(&route.at),
            lead: route.at.label().unwrap_or_default(),
            // The title is on the pull request, which is the thing being
            // fetched. Until it lands there are only the two halves the link
            // itself carries.
            trail: String::new(),
            note: route
                .place
                .as_ref()
                .and_then(|place| place.path.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }
    }

    /// What a workspace is called — the same two halves the top bar's crumb
    /// puts either side of the title, so a row in the switcher reads as the
    /// bar of the space it opens.
    pub fn of(ws: &Workspace, open: Option<&Path>) -> Card {
        let (lead, trail) = match ws {
            Workspace::Empty => return Card::blank(),
            Workspace::Pr(p) => (format!("{} #{}", p.repo, p.number), p.title.clone()),
            Workspace::Repo(v) => (v.repo.to_string(), format!("@ {}", v.branch)),
            Workspace::Commit(v) => (
                v.commit.short().to_string(),
                format!("{} — {}", v.repo, v.commit.subject()),
            ),
            Workspace::Compare(v) => (format!("{}...{}", v.base, v.head), v.summary()),
        };
        Card {
            kind: Kind::of(ws),
            lead,
            trail,
            note: open
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }
    }
}

// ------------------------------------------------------------------ claims

/// A task's hold on the space it was started in.
///
/// Everything fetched from GitHub lands in signals that belong to one space,
/// and a fetch outlives the click that asked for it: opening a pull request is
/// three requests deep by the time it enters, and the reader can be two spaces
/// away by then. Without this, what came back would be written into whatever
/// space happens to be on screen — a pull request opening itself over somebody
/// else's review.
///
/// It is deliberately not the generation counter, which is the other guard in
/// this app. That one means "something newer has superseded you"; this one
/// means "you are in the wrong room", and the two answer differently when two
/// things are opened in quick succession in the *same* space.
#[derive(Clone, Copy)]
pub struct Claim {
    st: St,
    space: u32,
}

impl Claim {
    /// Take a claim on the space that is on screen *now*.
    ///
    /// Which means: before the first `await`. An async body does not begin
    /// until it is first polled, so a claim taken at the top of one is taken
    /// on the tick the task was spawned — which is the tick the reader
    /// clicked. Taken after a wait, it would claim whichever space the wait
    /// landed in, and answer yes to every question ever asked of it.
    pub fn new(st: St) -> Claim {
        let space = *st.space.peek();
        Claim { st, space }
    }

    /// Whether the space this was started in is still the one on screen.
    pub fn kept(&self) -> bool {
        *self.st.space.peek() == self.space
    }

    /// Say what is being fetched — in the space that asked, and nowhere else.
    pub fn working(&self, note: impl Into<String>) {
        self.say(Fetch::Working(note.into()));
    }

    /// And what went wrong, written the way every error in this app is.
    ///
    /// Also the end of an arrival, for a space that was still on its way to
    /// something: nothing is coming any more, so the picker comes back — with
    /// this error at the top of it, which is where the reader can do something
    /// about it.
    pub fn failed(&self, e: impl std::fmt::Display) {
        self.say(Fetch::Failed(format!("{e:#}")));
        if self.kept() {
            self.st.arrived();
        }
    }

    /// Nothing in flight any more.
    pub fn done(&self) {
        self.say(Fetch::Idle);
    }

    fn say(&self, what: Fetch) {
        if !self.kept() {
            return;
        }
        let mut fetch = self.st.fetch;
        fetch.set(what);
    }
}

// ------------------------------------------------------- moving between them

/// Show a space, putting down the one on screen.
///
/// The whole of a switch: the file being read notes where it was left, the two
/// states change places, the page is told which space's scroll offsets to
/// answer with, and everything in flight for the space being left is retired.
pub fn go_to(st: &St, id: u32) {
    if *st.space.peek() == id {
        return;
    }
    // Where the reader is standing in the open file, so its tab comes back to
    // that rather than to the top. The one piece of a space that is written
    // lazily, and this is the last moment to write it.
    st.stow();

    let now = *st.space.peek();
    // Out of the list, leaving a placeholder — what goes back in its slot is
    // the space being left, a few lines below.
    let mut spaces = st.spaces;
    let incoming = {
        let mut list = spaces.write();
        let Some(at) = list.iter().position(|s| s.id == id) else {
            return;
        };
        std::mem::replace(&mut list[at].state, State::Live(Box::new(Held::fresh())))
    };
    let (incoming, fetch) = match incoming {
        // A space put down in the middle of arriving somewhere: what it was
        // waiting on was retired when it was left, so coming back is where
        // that fetch starts again. Without this the screen that stands in for
        // the arrival would be up with nothing working behind it.
        State::Live(held) => {
            let again = held.incoming.clone().filter(|_| !held.workspace.is_open());
            (*held, again)
        }
        State::Away(route) => (Held::fresh(), Some(route)),
    };

    let outgoing = incoming.swap(st);
    {
        let mut list = spaces.write();
        if let Some(space) = list.iter_mut().find(|s| s.id == now) {
            space.state = State::Live(Box::new(outgoing));
        }
    }

    let mut here = st.space;
    here.set(id);
    tabs::use_space(id);
    // Everything cloning, indexing or warming for the space just left is
    // working for nobody. The effects that started it run again the moment its
    // workspace is back on the signals, so this stops that work rather than
    // losing it.
    st.bump_tick();

    match fetch {
        // Never opened in this session: it is a link, and opening a link is
        // what this app does. The address bar is left alone until it lands —
        // `St::enter` writes it, and writing it here would have the listener
        // on `hashchange` fetch the same thing a second time.
        Some(route) => {
            // What the space shows until it lands: where it is going, rather
            // than the front page of an app the reader is already inside.
            // Home is the exception — a saved space with nothing in it opens
            // on nothing, and nothing is what the landing page is for.
            st.arriving_at(&route);
            spawn_forever(open_route(*st, route));
        }
        // The effect in `App` follows the address bar for anything open. It
        // deliberately says nothing about home, so an empty space says it here.
        None => {
            if st.workspace.peek().target() == Target::Home {
                route::show(&Route::home());
            }
        }
    }
    save(st);
}

/// The next or previous space, wrapping — what `⌥⇧→` and `⌥⇧←` are.
pub fn step(st: &St, forward: bool) {
    let next = {
        let list = st.spaces.peek();
        if list.len() < 2 {
            return;
        }
        let at = list
            .iter()
            .position(|s| s.id == *st.space.peek())
            .unwrap_or(0);
        let by = if forward { 1 } else { list.len() - 1 };
        list[(at + by) % list.len()].id
    };
    go_to(st, next);
}

/// Open a space with nothing in it, beside the one on screen.
///
/// Next to it rather than at the end, because that is where a new one belongs:
/// the thing being opened is nearly always about the thing being read.
pub fn open_new(st: &St) {
    let id = next_id(st);
    let at = {
        let list = st.spaces.peek();
        list.iter()
            .position(|s| s.id == *st.space.peek())
            .map_or(list.len(), |at| at + 1)
    };
    let mut spaces = st.spaces;
    spaces.write().insert(
        at,
        Space {
            id,
            card: Card::blank(),
            state: State::Live(Box::new(Held::fresh())),
        },
    );
    go_to(st, id);
}

/// Put a space down. The one beside it takes over, as a closed tab's neighbour
/// does everywhere else.
///
/// Closing the last one leaves an empty space rather than nothing: there has to
/// be somewhere to be, and an empty space is the landing page.
pub fn close(st: &St, id: u32) {
    let (at, len) = {
        let list = st.spaces.peek();
        let Some(at) = list.iter().position(|s| s.id == id) else {
            return;
        };
        (at, list.len())
    };
    if len == 1 {
        // Nothing to hand over to. Emptying it in place is the same gesture the
        // top bar's ✕ makes, and it leaves the app somewhere it can be used.
        st.close_workspace();
        let mut spaces = st.spaces;
        let mut list = spaces.write();
        list[at].card = Card::blank();
        drop(list);
        tabs::shut(id);
        return save(st);
    }
    // Whatever slides into its place, or — closing the last of them — the one
    // to its left.
    if *st.space.peek() == id {
        let next = {
            let list = st.spaces.peek();
            list.get(at + 1).or_else(|| list.get(at - 1)).map(|s| s.id)
        };
        if let Some(next) = next {
            go_to(st, next);
        }
    }
    let mut spaces = st.spaces;
    spaces.write().retain(|s| s.id != id);
    tabs::shut(id);
    save(st);
}

/// Break a space out into a browser tab of its own.
///
/// It leaves this window: that is what breaking out means, and a space that
/// stayed behind as well would be the same review in two places, each with its
/// own idea of what has been read. The link is this same page with the space's
/// route on the end, so what opens is what was here.
///
/// Nothing is let go unless a window actually came back — a blocked popup must
/// not cost somebody a review.
pub fn break_out(st: &St, id: u32) {
    let url = {
        let list = st.spaces.peek();
        let Some(space) = list.iter().find(|s| s.id == id) else {
            return;
        };
        route::page_url(&space.route(st))
    };
    if open_tab(&url) {
        close(st, id);
    }
}

/// The next id. Counted from what is here rather than from a counter of its
/// own, so a list read back off storage cannot hand out an id it is already
/// using.
fn next_id(st: &St) -> u32 {
    st.spaces
        .peek()
        .iter()
        .map(|s| s.id)
        .max()
        .map_or(1, |n| n + 1)
}

/// Keep the active space's label in step with what it has open.
///
/// Called from an effect, so it runs whenever the workspace or the file being
/// read moves — and writes nothing when neither has changed anything the
/// switcher shows, since the menu subscribes to this list.
pub fn describe(st: &St, card: Card) {
    let id = *st.space.peek();
    let mut spaces = st.spaces;
    let mut list = spaces.write();
    let Some(space) = list.iter_mut().find(|s| s.id == id) else {
        return;
    };
    if space.card == card {
        return;
    }
    space.card = card;
}

// -------------------------------------------------------------- persistence

/// One space as session storage keeps it: what it is called, and the link that
/// re-opens it.
#[derive(Serialize, Deserialize)]
struct Row {
    id: u32,
    /// The route, written the way the address bar writes it.
    at: String,
    card: Card,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    /// Which one was on screen.
    on: u32,
    spaces: Vec<Row>,
}

/// Write the list down. Cheap enough to do on every file opened: it is a
/// dozen labels and a dozen links, and it is what makes a reload cost nothing.
pub fn save(st: &St) {
    let list = st.spaces.peek();
    let saved = Saved {
        on: *st.space.peek(),
        spaces: list
            .iter()
            .map(|s| Row {
                id: s.id,
                at: s.route(st).hash(),
                card: s.card.clone(),
            })
            .collect(),
    };
    if let Ok(body) = serde_json::to_string(&saved) {
        store::session_set(store::SPACES, &body);
    }
}

/// What this browser tab had open, or one empty space.
///
/// Every space but the active one comes back as a link — see [`State::Away`].
/// The active one comes back as a link too, and is opened by the same thing
/// that opens one on a fresh visit: the address bar, which is where it was
/// written on the way out.
pub fn load() -> (Vec<Space>, u32) {
    read(store::session_get(store::SPACES))
}

/// The half of [`load`] that does not touch storage.
///
/// A storage entry is data from outside the program — it can be half-written,
/// from an older version of this app, or hand-edited into nonsense — and none
/// of those are worth more than one empty space to whoever opens the tab.
fn read(raw: Option<String>) -> (Vec<Space>, u32) {
    let saved: Option<Saved> = raw
        .and_then(|raw| serde_json::from_str::<Saved>(&raw).ok())
        .filter(|s| !s.spaces.is_empty());
    let Some(saved) = saved else {
        return (
            vec![Space {
                id: 1,
                card: Card::blank(),
                state: State::Live(Box::new(Held::fresh())),
            }],
            1,
        );
    };
    let spaces: Vec<Space> = saved
        .spaces
        .into_iter()
        .map(|row| Space {
            id: row.id,
            card: row.card,
            state: State::Away(route::parse(&row.at)),
        })
        .collect();
    // The one that was on screen, and — for a storage entry that has been
    // edited into nonsense — the first one, which always exists.
    let on = spaces
        .iter()
        .find(|s| s.id == saved.on)
        .map_or(spaces[0].id, |s| s.id);
    (spaces, on)
}

/// The space the app starts in, ready to be written into.
///
/// Whatever it was last time is a link like any other, and it is the address
/// bar — not this — that says which link. So the space on screen starts empty
/// and `nav::landing` opens the fragment into it, exactly as it does on a
/// first visit.
pub fn wake(spaces: &mut [Space], on: u32) {
    if let Some(space) = spaces.iter_mut().find(|s| s.id == on) {
        space.state = State::Live(Box::new(Held::fresh()));
    }
}

// ------------------------------------------------------------- the switcher

/// The name in the corner, and everything else open behind it.
///
/// It is the app's own name because that is what a space is a whole one of —
/// and because the corner of the window is where every application keeps the
/// list of what it has open.
#[component]
pub fn SpaceSwitch() -> Element {
    let st = use_context::<St>();
    let mut open = use_signal(|| false);

    // A switch that has landed is a menu that has done what it was opened for.
    use_effect(move || {
        let _ = st.space.read();
        open.set(false);
    });

    let showing = *open.read();
    let here = *st.space.read();
    let count = st.spaces.read().len();
    let many = plural(count);
    // The rows only when there are rows to draw. This component re-renders
    // whenever a label moves, which is every time a file is opened in any of
    // them, and copying a dozen titles into a menu nobody has opened is work
    // for nothing.
    let rows: Vec<(u32, Card, bool)> = match showing {
        false => Vec::new(),
        true => st
            .spaces
            .read()
            .iter()
            .map(|s| (s.id, s.card.clone(), s.here()))
            .collect(),
    };

    let wrap = if showing { "spswitch on" } else { "spswitch" };

    rsx! {
        div {
            class: "{wrap}",
            // Escape, wherever the focus is inside here — which after the click
            // that opened the menu is the name itself, above the menu rather
            // than in it.
            onkeydown: move |e| {
                if e.key() == Key::Escape && *open.peek() {
                    e.stop_propagation();
                    open.set(false);
                }
            },
            button {
                class: "brand brandbtn",
                title: "{count} space{many} open  (⌥⇧← ⌥⇧→ to step between them, ⌥⇧T for a new one)",
                onclick: move |_| {
                    let shown = *open.peek();
                    open.set(!shown);
                },
                span { class: "brandname", "pullspace" }
                if count > 1 {
                    span { class: "spcount", "{count}" }
                }
                span { class: "prchev spchev", "▾" }
            }
            if showing {
                div {
                    class: "menuback",
                    onclick: move |_| open.set(false),
                }
                div { class: "spmenu",
                    div { class: "prmenuhdr",
                        span { class: "ghlabel", "SPACES" }
                        span { class: "spacer" }
                        button {
                            class: "textlink",
                            title: "Open an empty space  (⌥⇧T)",
                            onclick: move |_| open_new(&st),
                            span { class: "spplus", "+" }
                            "New space"
                        }
                    }
                    div { class: "spmenubody",
                        for (id , card , live) in rows.into_iter() {
                            SpaceRow {
                                key: "{id}",
                                id,
                                card: card.clone(),
                                live,
                                on: id == here,
                            }
                        }
                    }
                    div { class: "spmenufoot",
                        span { class: "spfoottext", "Everything stays exactly as you left it." }
                        span { class: "spacer" }
                        span { class: "spkeys",
                            span { class: "spkey", "⌥⇧←" }
                            span { class: "spkey", "⌥⇧→" }
                        }
                    }
                }
            }
        }
    }
}

/// "s", unless there is one of them.
fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// One space in the menu: what it has open, and the two things to do with it.
#[component]
fn SpaceRow(id: u32, card: Card, live: bool, on: bool) -> Element {
    let st = use_context::<St>();
    let cls = if on { "sprow on" } else { "sprow" };
    let chip = card.kind.css();
    let what = card.kind.word();
    let why = match (on, live) {
        (true, _) => "The space you are in".to_string(),
        (_, true) => format!("Go to {}", card.lead),
        // A row read back off session storage after a reload: the link is
        // here, what it points at is not.
        (_, false) => format!("Open {} — it has not been read yet", card.lead),
    };

    rsx! {
        div { class: "{cls}", title: "{why}",
            div {
                class: "sprowmain",
                onclick: move |_| go_to(&st, id),
                div { class: "sprowtop",
                    span { class: "{chip}", "{what}" }
                    span { class: "prnum", "{card.lead}" }
                    if !live {
                        span { class: "spaway", "link" }
                    }
                }
                if !card.trail.is_empty() {
                    div { class: "sprowtitle", "{card.trail}" }
                }
                if !card.note.is_empty() {
                    div { class: "sprownote", "reading {card.note}" }
                }
            }
            // Only where there is something to break out. An empty space has
            // no link of its own, and a browser tab on the landing page is not
            // what anybody is asking this button for.
            if card.kind != Kind::Empty {
                button {
                    class: "iconbtn sm",
                    title: "Open this in a browser tab of its own, and let go of it here",
                    onclick: move |e| {
                        e.stop_propagation();
                        break_out(&st, id);
                    },
                    "↗"
                }
            }
            button {
                class: "tabx",
                title: "Close this space  (⌥⇧W)",
                onclick: move |e| {
                    e.stop_propagation();
                    close(&st, id);
                },
                "×"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_space_starts_where_the_app_does() {
        let fresh = Held::fresh();
        // The three that are not simply empty, and are the ones a mismatch
        // with `App`'s own defaults would show up in.
        assert!(fresh.conv_open, "the conversation pane opens unfolded");
        assert!(matches!(fresh.conv, Conversation::Loading));
        assert!(matches!(fresh.workspace, Workspace::Empty));
        assert!(matches!(fresh.view_mode, ViewMode::Source));
    }

    /// An empty space says so rather than being a blank row: the menu is
    /// read at a glance, and a row with nothing on it looks like a fault.
    #[test]
    fn a_space_with_nothing_in_it_still_has_a_name() {
        let card = Card::of(&Workspace::Empty, Some(Path::new("a/b.rs")));
        assert_eq!(card.kind, Kind::Empty);
        assert_eq!(card.lead, "New space");
        // Nothing open means nothing being read, whatever else is passed.
        assert_eq!(card.note, "");
    }

    /// A space woken from a link is named for the link while it fetches — and
    /// since the list is written down as it changes, that is also the label a
    /// reload in the middle of one comes back to.
    #[test]
    fn a_space_on_its_way_somewhere_is_named_for_where_it_is_going() {
        let repo = crate::backend::github::RepoRef {
            owner: "bigmah".to_string(),
            name: "pullspace".to_string(),
        };
        let card = Card::arriving(&Route {
            at: Target::Pr(repo.clone(), 7),
            place: Some(Place {
                path: PathBuf::from("src/ui/app.rs"),
                line: Some(42),
            }),
        });
        assert_eq!(card.kind, Kind::Pr);
        assert_eq!(card.lead, "bigmah/pullspace #7");
        // The title is on the pull request, which has not arrived yet.
        assert_eq!(card.trail, "");
        assert_eq!(card.note, "app.rs");

        // A branch is a browse, and a link naming no file names no file.
        let card = Card::arriving(&Route::to(Target::Branch(repo, "dev".to_string())));
        assert_eq!(card.kind, Kind::Repo);
        assert_eq!(card.lead, "bigmah/pullspace @ dev");
        assert_eq!(card.note, "");
    }

    /// The list is written down as links, so what comes back is a link per
    /// space and nothing heavier.
    #[test]
    fn the_saved_shape_is_a_label_and_a_link() {
        let saved = Saved {
            on: 2,
            spaces: vec![Row {
                id: 2,
                at: "#/bigmah/pullspace/pull/7".to_string(),
                card: Card {
                    kind: Kind::Pr,
                    lead: "bigmah/pullspace #7".to_string(),
                    trail: "A change".to_string(),
                    note: "app.rs".to_string(),
                },
            }],
        };
        let raw = serde_json::to_string(&saved).unwrap();
        let back: Saved = serde_json::from_str(&raw).unwrap();
        assert_eq!(back.on, 2);
        assert_eq!(back.spaces[0].card.lead, "bigmah/pullspace #7");
        assert_eq!(
            route::parse(&back.spaces[0].at).at,
            Target::Pr(
                crate::backend::github::RepoRef {
                    owner: "bigmah".to_string(),
                    name: "pullspace".to_string(),
                },
                7
            )
        );
    }

    /// A session entry is data from outside the program. Whatever is wrong
    /// with it costs the spaces, not the tab.
    #[test]
    fn nonsense_in_storage_is_one_empty_space() {
        for raw in [
            None,
            Some(String::new()),
            Some("not json at all".to_string()),
            // Well-formed, and empty: a tab that had nothing open.
            Some(r#"{"on":3,"spaces":[]}"#.to_string()),
        ] {
            let (spaces, on) = read(raw);
            assert_eq!(spaces.len(), 1);
            assert_eq!(on, spaces[0].id);
            assert!(spaces[0].here(), "and it is one you can open something in");
        }
    }

    /// Every space but one comes back as a link, and the one that was on
    /// screen is named — or, when the entry no longer names one that exists,
    /// the first, since there always is one.
    #[test]
    fn a_saved_tab_comes_back_as_its_links() {
        let row = |id: u32, at: &str| {
            format!(
                r#"{{"id":{id},"at":"{at}","card":{{"kind":"Pr","lead":"o/r #{id}","trail":"t","note":""}}}}"#
            )
        };
        let raw = format!(
            r#"{{"on":5,"spaces":[{},{}]}}"#,
            row(4, "#/o/r/pull/4"),
            row(5, "#/o/r/pull/5")
        );
        let (spaces, on) = read(Some(raw.clone()));
        assert_eq!(spaces.len(), 2);
        assert_eq!(on, 5);
        assert!(
            spaces.iter().all(|s| !s.here()),
            "nothing has been fetched for any of them yet"
        );
        assert_eq!(spaces[0].card.lead, "o/r #4");

        // `wake` is what makes room for the address bar to open one into.
        let mut spaces = spaces;
        wake(&mut spaces, on);
        assert!(spaces[1].here());
        assert!(!spaces[0].here());

        // An `on` naming a space that is not in the list.
        let (spaces, on) = read(Some(raw.replace(r#""on":5"#, r#""on":99"#)));
        assert_eq!(on, spaces[0].id);
    }

    #[test]
    fn a_kind_carries_its_own_chip() {
        assert_eq!(Kind::Pr.css(), "wschip pr");
        // A commit and a comparison are the same claim about two commits.
        assert_eq!(Kind::Commit.css(), Kind::Compare.css());
        assert_eq!(Kind::Empty.word(), "nothing open");
    }

    #[test]
    fn one_space_is_singular() {
        assert_eq!(plural(1), "");
        assert_eq!(plural(0), "s");
        assert_eq!(plural(2), "s");
    }
}
