//! The refs of what is open, on the top bar — and the way to every other pair
//! of them.
//!
//! Everything this app opens on a repository is one or two refs of it: a
//! branch being read, two branches held up against each other, a pull request
//! from one into another. So that is how the bar draws it — a chip per ref,
//! each with the repository's branches behind it, and a ✕ on each of a pair
//! that drops that one and reads the other. Reading a single branch, the second
//! chip is an empty slot: `+ compare branch`.
//!
//! One shape for all three, so that going from a pull request to a comparison
//! to a branch and back is a few clicks on the same two chips rather than three
//! trips through three different menus.

use dioxus::prelude::*;

use crate::backend::github::{Branch, CommitFrom, RepoRef, short_sha};

use super::app::{BranchList, Fetch, St, Workspace};
use super::conversation::{
    Found, Shown, load_branches, load_more_branches, shown_branches, use_branch_lookup,
};
use super::github::{browse_branch, browse_repo, open_commit, open_compare, open_pr};

/// Somewhere a click on the bar goes.
#[derive(Clone, PartialEq, Debug)]
pub enum Go {
    /// A repository at whatever its default branch is now.
    Repo(RepoRef),
    /// A branch, at the commit its tip is already known to be at, when it is.
    Branch(RepoRef, String, Option<String>),
    /// Base, then head.
    Compare(RepoRef, String, String),
    Pr(RepoRef, u64),
    Commit(RepoRef, String, CommitFrom),
}

impl Go {
    /// Whatever is open, fetched again — which is what `⟳` does.
    pub fn reload(ws: &Workspace) -> Option<Go> {
        Some(match ws {
            Workspace::Empty => return None,
            Workspace::Pr(p) => Go::Pr(p.repo.clone(), p.number),
            // The default branch is looked up again rather than taken as read:
            // it is a setting on the repository, and reloading is when a change
            // to one shows up.
            Workspace::Repo(v) if v.default => Go::Repo(v.repo.clone()),
            // No sha: what the branch points at now is the thing being asked
            // for.
            Workspace::Repo(v) => Go::Branch(v.repo.clone(), v.branch.clone(), None),
            Workspace::Commit(v) => {
                Go::Commit(v.repo.clone(), v.commit.sha.clone(), v.from.clone())
            }
            Workspace::Compare(v) => Go::Compare(v.repo.clone(), v.base.clone(), v.head.clone()),
        })
    }

    /// Go there. In the root scope, because what loads replaces the bar that
    /// was clicked — and the load outlives the render that started it.
    pub fn run(self, st: St) {
        match self {
            Go::Repo(repo) => spawn_forever(browse_repo(st, repo)),
            Go::Branch(repo, branch, sha) => spawn_forever(browse_branch(st, repo, branch, sha)),
            Go::Compare(repo, base, head) => spawn_forever(open_compare(st, repo, base, head)),
            Go::Pr(repo, number) => spawn_forever(open_pr(st, repo, number)),
            Go::Commit(repo, sha, from) => spawn_forever(open_commit(st, repo, sha, from)),
        };
    }
}

/// What a row of a chip's branch list does with the branch it is.
#[derive(Clone, PartialEq, Debug)]
enum Pick {
    /// Read it, in place of the branch being read.
    Read { at: String },
    /// Make it the base, against `head` — which `head_is` names.
    Base {
        at: String,
        head: String,
        head_is: &'static str,
    },
    /// Hold it up against `base`, in place of whatever was there (`at`), if
    /// anything was.
    Head {
        at: Option<String>,
        base: String,
        base_is: &'static str,
    },
}

impl Pick {
    /// The branch this list is choosing a replacement for. Its row is marked,
    /// and is not somewhere to go.
    fn at(&self) -> Option<&str> {
        match self {
            Pick::Read { at } | Pick::Base { at, .. } => Some(at),
            Pick::Head { at, .. } => at.as_deref(),
        }
    }

    /// The ref on the other side, and the word for it there. Comparing a branch
    /// with itself has nothing in the answer, so its row is not a choice either.
    fn other(&self) -> Option<(&str, &'static str)> {
        match self {
            Pick::Read { .. } => None,
            Pick::Base { head, head_is, .. } => Some((head, head_is)),
            Pick::Head { base, base_is, .. } => Some((base, base_is)),
        }
    }

    /// Whether a row for `name` can be picked.
    fn allows(&self, name: &str) -> bool {
        self.at() != Some(name) && self.other().is_none_or(|(other, _)| other != name)
    }

    /// Where picking `b` goes.
    fn go(&self, repo: &RepoRef, b: &Branch) -> Go {
        match self {
            Pick::Read { .. } => Go::Branch(repo.clone(), b.name.clone(), Some(b.sha.clone())),
            Pick::Base { head, .. } => Go::Compare(repo.clone(), b.name.clone(), head.clone()),
            Pick::Head { base, .. } => Go::Compare(repo.clone(), base.clone(), b.name.clone()),
        }
    }
}

/// One chip: a ref of what is open, and what can be done to it.
#[derive(Clone, PartialEq, Debug)]
struct Side {
    /// What the ref is to what is open: `branch`, `base`, `compare`, `into`,
    /// `from`, `commit`.
    label: &'static str,
    name: String,
    why: String,
    /// What its list does, when it has one. A commit does not: it is not one of
    /// a list of alternatives the way a branch is.
    pick: Option<Pick>,
    /// The line at the top of that list, saying what picking a row will do.
    note: String,
    /// What ✕ does, and the words for it — only where there is something left
    /// once this side is gone.
    close: Option<(Go, String)>,
}

/// The chips for what is open.
enum Shape {
    /// A branch being read, and the empty slot for a second one.
    One { side: Side, add: Side },
    /// Two refs, and between them the way to swap them where they can be.
    Two {
        left: Side,
        swap: Option<(Go, String)>,
        right: Side,
    },
    /// A commit, on its own.
    Commit(Side),
}

/// What follows the chips: what is open as more than its refs.
struct Trail {
    /// A pull request's number.
    lead: Option<String>,
    text: String,
    /// Short and worth keeping whole — the ahead/behind count of a comparison —
    /// rather than a title to cut down to fit.
    whole: bool,
}

fn shape_of(ws: &Workspace) -> Option<(RepoRef, Shape, Trail)> {
    Some(match ws {
        Workspace::Empty => return None,
        Workspace::Repo(v) => {
            let (repo, branch) = (&v.repo, &v.branch);
            let side = Side {
                label: "branch",
                name: branch.clone(),
                why: format!("Reading {repo} at {branch}\nPick another branch to read"),
                pick: Some(Pick::Read { at: branch.clone() }),
                note: format!("Read another branch of {repo}"),
                close: None,
            };
            let add = Side {
                label: "compare",
                name: String::new(),
                why: format!(
                    "Compare {branch} with another branch — what the other one has that {branch} does not"
                ),
                pick: Some(Pick::Head {
                    at: None,
                    base: branch.clone(),
                    base_is: "base",
                }),
                note: format!("Compare against {branch}"),
                close: None,
            };
            let trail = Trail {
                lead: None,
                text: String::new(),
                whole: false,
            };
            (repo.clone(), Shape::One { side, add }, trail)
        }
        Workspace::Compare(v) => {
            let (repo, base, head) = (&v.repo, &v.base, &v.head);
            let left = Side {
                label: "base",
                name: base.clone(),
                why: format!("Comparing into {base}\nPick another base for {head}"),
                pick: Some(Pick::Base {
                    at: base.clone(),
                    head: head.clone(),
                    head_is: "compare",
                }),
                note: format!("A new base for {head}"),
                close: Some((
                    Go::Branch(repo.clone(), head.clone(), Some(v.head_sha.clone())),
                    format!("Stop comparing — read {head} on its own"),
                )),
            };
            let right = Side {
                label: "compare",
                name: head.clone(),
                why: format!("What {head} has that {base} does not\nPick another branch to compare"),
                pick: Some(Pick::Head {
                    at: Some(head.clone()),
                    base: base.clone(),
                    base_is: "base",
                }),
                note: format!("Compare against {base}"),
                close: Some((
                    Go::Branch(
                        repo.clone(),
                        base.clone(),
                        (!v.base_tip.is_empty()).then(|| v.base_tip.clone()),
                    ),
                    format!("Stop comparing — read {base} on its own"),
                )),
            };
            // Which of two branches is the base is a thing to get wrong, and
            // the answer is one click rather than two trips through the lists.
            let swap = Some((
                Go::Compare(repo.clone(), head.clone(), base.clone()),
                format!("Swap them — compare {base} into {head}"),
            ));
            let trail = Trail {
                lead: None,
                text: v.summary(),
                whole: true,
            };
            (repo.clone(), Shape::Two { left, swap, right }, trail)
        }
        Workspace::Pr(p) => {
            let repo = &p.repo;
            let base = &p.base_ref;
            let head = p.head_label();
            let leaves = format!("Leaves #{} for a comparison", p.number);
            let left = Side {
                label: "into",
                name: base.clone(),
                why: format!(
                    "#{} merges into {base}\nPick another base to compare {head} against",
                    p.number
                ),
                pick: Some(Pick::Base {
                    at: base.clone(),
                    head: head.clone(),
                    head_is: "from",
                }),
                note: format!("A new base for {head}. {leaves}."),
                // The branch where it is: a fork's branch is not a branch of
                // this repository, and reading it here would be a 404.
                close: Some((
                    Go::Branch(
                        p.head_repo.clone().unwrap_or_else(|| repo.clone()),
                        p.head_ref.clone(),
                        Some(p.head_sha.clone()),
                    ),
                    format!("Leave the pull request — read {head} on its own"),
                )),
            };
            let right = Side {
                label: "from",
                name: head.clone(),
                why: format!(
                    "#{} brings in {head}\nPick another branch to compare against {base}",
                    p.number
                ),
                pick: Some(Pick::Head {
                    at: Some(head.clone()),
                    base: base.clone(),
                    base_is: "into",
                }),
                note: format!("Compare against {base}. {leaves}."),
                // No sha: `base_sha` is the merge base, not the tip.
                close: Some((
                    Go::Branch(repo.clone(), base.clone(), None),
                    format!("Leave the pull request — read {base} on its own"),
                )),
            };
            let trail = Trail {
                lead: Some(format!("#{}", p.number)),
                text: p.title.clone(),
                whole: false,
            };
            (
                repo.clone(),
                Shape::Two {
                    left,
                    swap: None,
                    right,
                },
                trail,
            )
        }
        Workspace::Commit(v) => {
            let repo = &v.repo;
            // ✕ is a step back to what it was opened out of, as it is on the
            // chips of a pair: what is left without the commit.
            let (back, back_why) = match &v.from {
                CommitFrom::Pr(pr) => (
                    Go::Pr(repo.clone(), pr.number),
                    format!("Back to #{}", pr.number),
                ),
                CommitFrom::Branch(branch) => (
                    Go::Branch(repo.clone(), branch.clone(), None),
                    format!("Back to {branch}"),
                ),
                CommitFrom::Compare(base, head) => (
                    Go::Compare(repo.clone(), base.clone(), head.clone()),
                    format!("Back to {base}...{head}"),
                ),
                CommitFrom::Alone => (Go::Repo(repo.clone()), format!("Close it — read {repo}")),
            };
            let of = match (v.pr(), v.branch(), v.compare()) {
                (Some(pr), ..) => format!("One commit of #{}", pr.number),
                (_, Some(branch), _) => format!("One commit of {branch}"),
                (.., Some((base, head))) => format!("One commit of {base}...{head}"),
                _ => "Diffed against the commit before it".to_string(),
            };
            let side = Side {
                label: "commit",
                name: v.commit.short().to_string(),
                why: format!("{} — {}\n{of}", v.commit.sha, v.commit.subject()),
                pick: None,
                note: String::new(),
                close: Some((back, back_why)),
            };
            let trail = Trail {
                lead: None,
                text: v.commit.subject().to_string(),
                whole: false,
            };
            (repo.clone(), Shape::Commit(side), trail)
        }
    })
}

/// The chips, and what follows them.
#[component]
pub fn Refs() -> Element {
    let st = use_context::<St>();
    // Only what the chips say leaves the guard: the workspace carries two tree
    // snapshots, and this is re-rendered whenever it changes.
    let Some((repo, shape, trail)) = shape_of(&st.workspace.read()) else {
        return rsx! {};
    };

    rsx! {
        div { class: "refs",
            match shape {
                Shape::One { side, add: slot } => rsx! {
                    RefSpot { repo: repo.clone(), side, add: false }
                    RefSpot { repo: repo.clone(), side: slot, add: true }
                },
                Shape::Two { left, swap, right } => rsx! {
                    RefSpot { repo: repo.clone(), side: left, add: false }
                    if let Some((go, why)) = swap {
                        button {
                            class: "refsep",
                            title: "{why}",
                            onclick: move |_| go.clone().run(st),
                            span { class: "dir", "←" }
                            span { class: "swap", "⇄" }
                        }
                    } else {
                        span { class: "refsep", title: "Merges into the branch on the left",
                            span { class: "dir", "←" }
                        }
                    }
                    RefSpot { repo: repo.clone(), side: right, add: false }
                },
                Shape::Commit(side) => rsx! {
                    RefSpot { repo: repo.clone(), side, add: false }
                },
            }
        }
        if !trail.text.is_empty() {
            div {
                class: if trail.whole { "reftrail whole" } else { "reftrail" },
                title: "{trail.text}",
                if let Some(lead) = trail.lead {
                    span { class: "prnum", "{lead}" }
                }
                span { class: "reftext", "{trail.text}" }
            }
        }
    }
}

/// One chip, and the branch list behind it.
#[component]
fn RefSpot(repo: RepoRef, side: Side, add: bool) -> Element {
    let st = use_context::<St>();
    let mut open = use_signal(|| false);

    // The branches are fetched on being asked for rather than with what is
    // open. Idle is the whole of the test: a list already on its way, or
    // already here, is *this* list, since everything that asks asks for the
    // branches of the same repository, and all of it is emptied when that
    // changes.
    use_effect(move || {
        if !*open.read() {
            return;
        }
        let repo = st.workspace.read().repo_ref().cloned();
        let Some(repo) = repo else {
            return;
        };
        if !matches!(*st.branches.peek(), BranchList::Idle) {
            return;
        }
        spawn_forever(load_branches(st, repo));
    });

    // Something opened is a list that has done what it was opened for. On the
    // workspace rather than on the click, so the list stays up — with the row
    // saying it is on its way — for as long as the fetching takes.
    use_effect(move || {
        let _ = st.workspace.read();
        open.set(false);
    });

    let showing = *open.read();
    let wrap = if showing { "refspot on" } else { "refspot" };
    let Side {
        label,
        name,
        why,
        pick,
        note,
        close,
    } = side;
    let pickable = pick.is_some();
    let toggle = move |_: MouseEvent| {
        let now = *open.peek();
        open.set(!now);
    };

    rsx! {
        div {
            class: "{wrap}",
            // Escape, wherever the focus is inside here — the filter box in the
            // list, or the chip above it.
            onkeydown: move |e| {
                if e.key() == Key::Escape && *open.peek() {
                    e.stop_propagation();
                    open.set(false);
                }
            },
            if add {
                button { class: "refadd", title: "{why}", onclick: toggle,
                    span { class: "refplus", "+" }
                    "compare branch"
                }
            } else {
                div { class: "refchip",
                    if pickable {
                        button { class: "refmain", title: "{why}", onclick: toggle,
                            span { class: "reflabel", "{label}" }
                            span { class: "refname", "{name}" }
                            span { class: "prchev", "▾" }
                        }
                    } else {
                        div { class: "refmain still", title: "{why}",
                            span { class: "reflabel", "{label}" }
                            span { class: "refname", "{name}" }
                        }
                    }
                    if let Some((go, close_why)) = close {
                        button {
                            class: "refx",
                            title: "{close_why}",
                            onclick: move |_| go.clone().run(st),
                            "✕"
                        }
                    }
                }
            }
            if showing {
                if let Some(pick) = pick {
                    // Everything else on the page, for as long as the list is
                    // up: a click anywhere out here puts it away.
                    div {
                        class: "menuback",
                        onclick: move |_| open.set(false),
                    }
                    BranchPicker { repo, pick, note }
                }
            }
        }
    }
}

/// The rows a picker is showing, read from outside the render pass — the list
/// moves under the keyboard as pages and lookups arrive.
fn rows_now(st: St, filter: Signal<String>, found: Resource<Option<Found>>) -> Vec<Branch> {
    let typed = filter.peek().trim().to_string();
    let held = st.branches.peek();
    let Some(list) = held.items() else {
        return Vec::new();
    };
    let answer = (*found.peek()).clone().flatten();
    shown_branches(list, &typed, answer).rows
}

/// The row the keyboard is on: the one it was moved to, or else the first that
/// can be picked — so that typing a name and pressing Enter opens it, even with
/// the branch already open sitting at the top of the list.
fn resting(rows: &[Branch], pick: &Pick, moved: Option<usize>) -> Option<usize> {
    moved
        .filter(|i| rows.get(*i).is_some_and(|b| pick.allows(&b.name)))
        .or_else(|| rows.iter().position(|b| pick.allows(&b.name)))
}

/// The next row that can be picked, one way or the other from `from`.
fn step(rows: &[Branch], pick: &Pick, from: Option<usize>, down: bool) -> Option<usize> {
    let Some(from) = from else {
        return resting(rows, pick, None);
    };
    let next = if down {
        (from + 1..rows.len()).find(|i| pick.allows(&rows[*i].name))
    } else {
        (0..from).rev().find(|i| pick.allows(&rows[*i].name))
    };
    Some(next.unwrap_or(from))
}

/// A repository's branches, to pick one of for a chip.
///
/// Typed into rather than read down: it opens with the box focused, filters as
/// it is typed into, and asks GitHub for the branches past the listed ones —
/// the same lookup as the pane on the right. ↑ ↓ and Enter choose without the
/// mouse.
#[component]
fn BranchPicker(repo: RepoRef, pick: Pick, note: String) -> Element {
    let st = use_context::<St>();
    let mut filter = use_signal(String::new);
    // The row the arrow keys or the pointer last put the highlight on.
    let mut moved = use_signal(|| None::<usize>);
    // The branch picked, for as long as what it opens is on its way.
    let mut going = use_signal(|| None::<String>);
    let found = use_branch_lookup(filter);

    let typed = filter.read().trim().to_string();
    let failed = match (&*going.read(), &*st.fetch.read()) {
        (Some(_), Fetch::Failed(e)) => Some(e.clone()),
        _ => None,
    };

    let held = st.branches.read();
    let (list, waiting) = match &*held {
        BranchList::Ready(list) => (Some(list), false),
        BranchList::More(list) => (Some(list), true),
        _ => (None, false),
    };
    let Shown {
        rows,
        error,
        looking,
    } = match list {
        Some(list) => shown_branches(list, &typed, found.cloned().flatten()),
        None => Shown {
            rows: Vec::new(),
            error: None,
            looking: false,
        },
    };
    let truncated = list.is_some_and(|l| l.truncated);
    let listed = list.map_or(0, |l| l.items.len());
    let hi = resting(&rows, &pick, *moved.read());
    let status = match &*held {
        BranchList::Idle | BranchList::Loading => Some("Loading the branches…".to_string()),
        BranchList::Failed(e) => Some(e.clone()),
        _ if looking => Some(format!("Looking for “{typed}” on GitHub…")),
        _ if rows.is_empty() && typed.is_empty() => Some(format!("{repo} has no branches.")),
        _ if rows.is_empty() => Some(format!("No branch of {repo} matches “{typed}”.")),
        _ => None,
    };
    let bad = matches!(&*held, BranchList::Failed(_));
    drop(held);

    let keys_pick = pick.clone();
    let keys_repo = repo.clone();
    let more_repo = repo.clone();

    rsx! {
        div { class: "prmenu refmenu",
            div { class: "refhdr",
                input {
                    class: "ghinput",
                    r#type: "text",
                    placeholder: "Find a branch…",
                    title: if truncated { "Filters the branches listed here, and asks GitHub for the ones past them" } else { "Filters the branches of {repo}" },
                    spellcheck: "false",
                    autocomplete: "off",
                    value: "{filter}",
                    // The list is opened to pick something out of it, so it
                    // opens ready to be typed into.
                    onmounted: move |e| async move {
                        let _ = e.set_focus(true).await;
                    },
                    oninput: move |e| {
                        filter.set(e.value());
                        moved.set(None);
                    },
                    onkeydown: move |e| {
                        let rows = rows_now(st, filter, found);
                        let at = resting(&rows, &keys_pick, *moved.peek());
                        match e.key() {
                            Key::ArrowDown | Key::ArrowUp => {
                                e.prevent_default();
                                let down = e.key() == Key::ArrowDown;
                                moved.set(step(&rows, &keys_pick, at, down));
                            }
                            Key::Enter => {
                                if let Some(b) = at.and_then(|i| rows.get(i)) {
                                    going.set(Some(b.name.clone()));
                                    keys_pick.go(&keys_repo, b).run(st);
                                }
                            }
                            _ => {}
                        }
                    },
                }
                div { class: "refnote", "{note}" }
            }
            div { class: "refbody",
                if let Some(e) = failed {
                    div { class: "gherror", "{e}" }
                }
                for (i , b) in rows.iter().enumerate() {
                    {
                        let is_at = pick.at() == Some(b.name.as_str());
                        let other = pick
                            .other()
                            .filter(|(other, _)| *other == b.name)
                            .map(|(_, word)| word);
                        let can = !is_at && other.is_none();
                        let opening = going.read().as_deref() == Some(b.name.as_str());
                        let class = match (is_at, can, hi == Some(i)) {
                            (true, ..) => "refrow on",
                            (_, false, _) => "refrow off",
                            (_, _, true) => "refrow hi",
                            _ => "refrow",
                        };
                        let tag = match (opening, is_at, other) {
                            (true, ..) => "opening…",
                            (_, true, _) => "current",
                            (_, _, Some(word)) => word,
                            _ => "",
                        };
                        let why = match (is_at, other) {
                            (true, _) => "Already open".to_string(),
                            (_, Some(word)) => format!("Already the {word} side"),
                            _ => b.name.clone(),
                        };
                        let go = pick.go(&repo, b);
                        let name = b.name.clone();
                        rsx! {
                            div {
                                key: "{b.name}",
                                class,
                                title: "{why}",
                                onmouseenter: move |_| {
                                    if can {
                                        moved.set(Some(i));
                                    }
                                },
                                onclick: move |_| {
                                    if can {
                                        going.set(Some(name.clone()));
                                        go.clone().run(st);
                                    }
                                },
                                span { class: "branchname", "{b.name}" }
                                if b.protected {
                                    span {
                                        class: "reftag",
                                        title: "GitHub refuses pushes straight at this branch",
                                        "protected"
                                    }
                                }
                                span { class: "spacer" }
                                if !tag.is_empty() {
                                    span { class: "reftag", "{tag}" }
                                }
                                span { class: "refsha", title: "{b.sha}", "{short_sha(&b.sha)}" }
                            }
                        }
                    }
                }
                if let Some(e) = error {
                    div { class: "gherror", "{e}" }
                }
                if let Some(text) = status {
                    div { class: if bad { "gherror" } else { "panel-empty" }, "{text}" }
                }
                // Past the listed branches GitHub's index matches from the
                // start of the name, letter for letter — which is the one
                // thing that turns a wrong-looking "nothing" into a usable one.
                if rows.is_empty() && truncated && !typed.is_empty() && !looking {
                    div { class: "panel-empty",
                        "Past the first {listed}, GitHub looks a branch up by what it starts "
                        "with, exactly as written — try the beginning of the name."
                    }
                }
                if truncated {
                    if waiting {
                        div { class: "panel-empty", "Loading more branches…" }
                    } else {
                        button {
                            class: "convolder",
                            title: "Read more of the branches of {repo} from GitHub",
                            onclick: move |_| {
                                spawn_forever(load_more_branches(st, more_repo.clone()));
                            },
                            "Show more branches"
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn branch(name: &str) -> Branch {
        Branch {
            name: name.to_string(),
            sha: format!("{name}-sha"),
            protected: false,
        }
    }

    fn repo() -> RepoRef {
        RepoRef {
            owner: "o".to_string(),
            name: "r".to_string(),
        }
    }

    #[test]
    fn neither_side_of_a_pair_is_somewhere_to_pick() {
        let pick = Pick::Head {
            at: Some("feat".to_string()),
            base: "main".to_string(),
            base_is: "base",
        };
        assert!(!pick.allows("feat"), "already the head");
        assert!(!pick.allows("main"), "comparing main with itself");
        assert!(pick.allows("other"));

        // An empty slot has nothing to replace, only the base to avoid.
        let add = Pick::Head {
            at: None,
            base: "main".to_string(),
            base_is: "base",
        };
        assert!(add.allows("feat") && !add.allows("main"));
    }

    #[test]
    fn a_pick_keeps_the_side_it_is_not_changing() {
        let (r, b) = (repo(), branch("next"));
        let base = Pick::Base {
            at: "main".to_string(),
            head: "o2:feat".to_string(),
            head_is: "from",
        };
        assert_eq!(
            base.go(&r, &b),
            Go::Compare(r.clone(), "next".to_string(), "o2:feat".to_string())
        );
        let head = Pick::Head {
            at: None,
            base: "main".to_string(),
            base_is: "base",
        };
        assert_eq!(
            head.go(&r, &b),
            Go::Compare(r.clone(), "main".to_string(), "next".to_string())
        );
        // Reading a branch carries its tip, so opening it costs no lookup.
        let read = Pick::Read {
            at: "main".to_string(),
        };
        assert_eq!(
            read.go(&r, &b),
            Go::Branch(r, "next".to_string(), Some("next-sha".to_string()))
        );
    }

    #[test]
    fn the_keyboard_rests_on_the_first_branch_it_can_open() {
        let rows = [branch("main"), branch("a"), branch("feat"), branch("b")];
        let pick = Pick::Head {
            at: Some("feat".to_string()),
            base: "main".to_string(),
            base_is: "base",
        };
        // `main` is the base, so Enter with nothing moved opens `a`.
        assert_eq!(resting(&rows, &pick, None), Some(1));
        // Down steps over `feat`, which is already open, and stops at the end.
        assert_eq!(step(&rows, &pick, Some(1), true), Some(3));
        assert_eq!(step(&rows, &pick, Some(3), true), Some(3));
        // Up does not land on `main`.
        assert_eq!(step(&rows, &pick, Some(1), false), Some(1));
        // A highlight left on a row that has since become unpickable falls
        // back rather than opening nothing.
        assert_eq!(resting(&rows, &pick, Some(2)), Some(1));
        // And a list with nothing to pick has nowhere to rest.
        assert_eq!(resting(&rows[..1], &pick, None), None);
    }
}
