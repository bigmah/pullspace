//! The conversation pane: a pull request's description, the discussion on it,
//! and the comments left on lines of the diff.
//!
//! Bodies are markdown, and they are drawn as the markdown they are — through
//! the same renderer the README goes through, which builds elements of this app
//! out of parsed text and never hands the page a line of somebody else's
//! markup. See [`crate::backend::markdown`] for why that distinction is the
//! whole design: a review is read next to a GitHub token in local storage.

use std::path::Path;

use dioxus::prelude::*;

use crate::backend::auth::open_browser;
use crate::backend::github::{
    self, Annotation, Branch, Check, Comment, CommentKind, CommitFrom, CommitSummary, PrHeader,
    RepoRef, short_sha,
};
use crate::backend::markdown;

use super::app::{
    Annots, BranchList, CheckList, CommitList, CommitSource, ConvTab, Conversation, Fetch, Reading,
    St,
};
use super::panes::{Edge, Splitter};

/// A comment's links are written from the root of the repository — there is no
/// file they are relative to, the way a README's are.
const ROOT: &str = "";

/// How much writing makes a comment worth offering the middle pane for. About
/// two screenfuls of the column on the right.
const LONG: usize = 900;

/// Fetch the conversation for a pull request, unless it has been closed or
/// swapped out from under us in the meantime.
pub(super) async fn load(st: St, repo: RepoRef, number: u64) {
    let mut conv = st.conv;
    conv.set(Conversation::Loading);
    let token = st.api_token();
    let got = github::pr_comments(&token, &repo, number).await;

    let still_open = st
        .workspace
        .peek()
        .pr()
        .is_some_and(|pr| pr.repo == repo && pr.number == number);
    if !still_open {
        return;
    }
    conv.set(match got {
        Ok(thread) => Conversation::Ready(Box::new(thread)),
        Err(e) => Conversation::Failed(format!("{e:#}")),
    });
}

/// Fetch the commits beside what is open — everything on a pull request, or the
/// first page of a branch's history. Asked for rather than fetched with the
/// rest of it — see [`CommitList`].
pub(super) async fn load_commits(st: St, source: CommitSource) {
    let mut commits = st.commits;
    commits.set(CommitList::Loading);
    let token = st.api_token();
    let got = match &source {
        CommitSource::Pr(repo, number) => github::pr_commits(&token, repo, *number).await,
        CommitSource::Branch(repo, branch) => github::branch_commits(&token, repo, branch, 1).await,
        CommitSource::Compare(repo, base, head) => {
            github::compare_commits(&token, repo, base, head, 1).await
        }
    };

    // Something else opened while this was in flight: what came back is the
    // answer to a question nobody is asking any more.
    if st.workspace.peek().commits_key() != Some(source) {
        return;
    }
    commits.set(match got {
        Ok(commits) => CommitList::Ready(Box::new(commits)),
        Err(e) => CommitList::Failed(format!("{e:#}")),
    });
}

/// One more page of a branch's history, on to the end of what is already there.
///
/// A branch does not end the way a pull request does, so its commits arrive a
/// page at a time and this is the ask for the next one. What is already read
/// stays on screen while it comes, and stays there if it never does: losing a
/// hundred rows that were fetched and read to a request that failed would be
/// the wrong way round.
pub(super) async fn load_older(st: St, source: CommitSource) {
    // A pull request's commits arrive whole; there is no next page to ask for.
    if !source.is_paged() {
        return;
    }
    let mut commits = st.commits;
    // Only from a settled page of this branch that says there is more behind
    // it — which is also what keeps a double click from asking twice.
    let held = match &*commits.peek() {
        CommitList::Ready(held) if held.truncated && held.pages > 0 => held.clone(),
        _ => return,
    };
    if st.workspace.peek().commits_key().as_ref() != Some(&source) {
        return;
    }
    let page = held.pages + 1;
    commits.set(CommitList::More(held));

    let token = st.api_token();
    let got = match &source {
        CommitSource::Branch(repo, branch) => {
            github::branch_commits(&token, repo, branch, page).await
        }
        CommitSource::Compare(repo, base, head) => {
            github::compare_commits(&token, repo, base, head, page).await
        }
        // Guarded above; a pull request never reaches here.
        CommitSource::Pr(..) => return,
    };

    if st.workspace.peek().commits_key() != Some(source) {
        return;
    }
    // And the list is still the one this page was asked for — `⟳` pressed
    // while it was in flight has already replaced it.
    let base = match &*commits.peek() {
        CommitList::More(base) if base.pages + 1 == page => base.clone(),
        _ => return,
    };
    match got {
        Ok(next) => {
            let mut all = base;
            all.items.extend(next.items);
            all.truncated = next.truncated;
            all.total = next.total;
            all.pages = page;
            commits.set(CommitList::Ready(all));
        }
        // The rows already read are worth more than the error is: they go back
        // up as they were, and the reason goes to the one line that reports
        // what GitHub would not do.
        Err(e) => {
            commits.set(CommitList::Ready(base));
            let mut fetch = st.fetch;
            fetch.set(Fetch::Failed(format!("{e:#}")));
        }
    }
}

/// Fetch the branches of the repository whatever is open belongs to.
///
/// Keyed by the repository and nothing finer: a pull request, a commit and a
/// branch of one are all inside it, so the list survives stepping between them
/// and is fetched again only when the repository itself changes.
pub(super) async fn load_branches(st: St, repo: RepoRef) {
    let mut branches = st.branches;
    branches.set(BranchList::Loading);
    let token = st.api_token();
    let got = github::list_branches(&token, &repo).await;

    let still_open = st.workspace.peek().repo_ref() == Some(&repo);
    if !still_open {
        return;
    }
    branches.set(match got {
        Ok(list) => BranchList::Ready(Box::new(list)),
        Err(e) => BranchList::Failed(format!("{e:#}")),
    });
}

/// Fetch what ran against the commit on screen. Asked for rather than fetched
/// with the pull request — see [`CheckList`].
pub(super) async fn load_checks(st: St, repo: RepoRef, sha: String) {
    let mut checks = st.checks;
    checks.set(CheckList::Loading);
    let token = st.api_token();
    let got = github::commit_checks(&token, &repo, &sha).await;

    // The same commit, still open. A pull request that was pushed to while this
    // was in flight is a different set of checks.
    let still_open = st
        .workspace
        .peek()
        .checks_key()
        .is_some_and(|(now, at)| now == repo && at == sha);
    if !still_open {
        return;
    }
    // What each check marked up was read out of the answer this one replaces.
    // `⟳` is pressed precisely when something has moved — a check that was
    // running has finished, and has things to say that it did not have before.
    st.forget_annotations();
    checks.set(match got {
        Ok(checks) => CheckList::Ready(Box::new(checks)),
        Err(e) => CheckList::Failed(format!("{e:#}")),
    });
}

/// Fetch what one check marked up, the first time somebody opens it.
///
/// One request per check, and one only: the entry is claimed before the request
/// goes out, so a row opened and closed and opened again asks once. What is
/// held is dropped wholesale when the checks change — see
/// `St::forget_annotations` — which is also how this notices that its answer is
/// about a commit nobody is reading any more.
pub(super) async fn load_annotations(st: St, check: u64) {
    let mut annots = st.annots;
    if annots.peek().contains_key(&check) {
        return;
    }
    let Some(repo) = st.workspace.peek().repo_ref().cloned() else {
        return;
    };
    annots.write().insert(check, Annots::Loading);
    let token = st.api_token();
    let got = github::check_annotations(&token, &repo, check).await;

    let mut held = annots.write();
    // Gone from under us: these are somebody else's check runs now.
    if !held.contains_key(&check) {
        return;
    }
    held.insert(
        check,
        match got {
            Ok(list) => Annots::Ready(list),
            Err(e) => Annots::Failed(format!("{e:#}")),
        },
    );
}

/// The date alone. The time of day is not what anyone is reading a comment
/// list for, and it costs the width of the author's name.
fn day_of(timestamp: &str) -> String {
    timestamp.chars().take(10).collect()
}

fn kind_label(c: &Comment) -> &str {
    match c.kind {
        CommentKind::Discussion => "commented",
        CommentKind::Inline => "line note",
        CommentKind::Review if c.verdict.is_empty() => "reviewed",
        CommentKind::Review => &c.verdict,
    }
}

/// Approval and rejection are the two things worth spotting from across the
/// pane; everything else is just a label.
fn kind_class(c: &Comment) -> &'static str {
    match c.verdict.as_str() {
        "approved" => "convkind ok",
        "changes requested" => "convkind bad",
        _ => "convkind",
    }
}

#[component]
pub fn ConvPane() -> Element {
    let st = use_context::<St>();
    // Only the handful of fields the pane draws leave the guard — the whole
    // workspace carries two tree snapshots, which is not something to clone on
    // every fold and unfold of the pane.
    let held = st.workspace.read();
    // Nothing open has no pane: the landing page is up in place of the lot.
    let Some(repo) = held.repo_ref().cloned() else {
        return rsx! {};
    };
    // The pull request that is open — or the one a commit was opened out of,
    // since the conversation is a fact about the pull request and not about
    // whichever of its commits is on screen. `None` while a repository is being
    // read on its own, which is where the branches take its place.
    let desc = held.header();
    // Which list of commits belongs here, and where each row leads.
    let source = held.commits_key();
    // Which commit and which branch are being read, so their rows are marked
    // rather than offered as somewhere to go.
    let at_commit = held.commit().map(|v| v.commit.sha.clone());
    let at_branch = held
        .repo()
        .map(|v| v.branch.clone())
        .or_else(|| held.commit().and_then(|v| v.branch().map(str::to_string)));
    // What a comparison started from the branch list would be against, and
    // which branch is already the right-hand side of one. Reading a branch, it
    // is that branch; reading a comparison, it is the base — so walking down
    // the list holds a repository's branches up against the same one in turn,
    // which is what anybody comparing branches is doing.
    let (against, compared) = match held.compare() {
        Some(v) => (Some(v.base.clone()), Some(v.head.clone())),
        None => (at_branch.clone(), None),
    };
    drop(held);
    let mut open = st.conv_open;
    let mut tab = st.conv_tab;
    let conv = st.conv.read();

    // The heading this pane leads with is whichever of the two the thing on
    // screen has: a pull request has a conversation, and a repository read on
    // its own has branches. Never both — see [`ConvTab`]. Which one is *shown*
    // is read the same way the effects that fetch these lists read it, so that
    // what is on screen and what is being fetched cannot disagree.
    let showing = st.conv_showing();

    let count = match &*conv {
        Conversation::Ready(thread) => thread.comments.len(),
        _ => 0,
    };
    let branches = match &*st.branches.read() {
        BranchList::Ready(list) => Some(list.items.len()),
        _ => None,
    };
    let commits = st.commits.read().items().map(|c| c.items.len());
    // The checks count carries the verdict with it: a red 14 beside the heading
    // is the answer to the question the tab is there for, before it is opened.
    let (checks, checks_tone, checks_why) = match &*st.checks.read() {
        CheckList::Ready(checks) => (
            Some(checks.items.len()),
            checks.state().tone(),
            format!("The checks on this commit — {}", checks.tally().phrase()),
        ),
        _ => (
            None,
            "",
            "What ran against this commit, and how it went".to_string(),
        ),
    };

    // Folded away, the rail says what is behind it — which is whichever of the
    // headings was last being read, not always the first.
    let (rail_label, rail_count, rail_why) = match showing {
        ConvTab::Talk => (
            "CONVERSATION",
            count,
            "Show the pull request conversation".to_string(),
        ),
        ConvTab::Branches => (
            "BRANCHES",
            branches.unwrap_or_default(),
            format!("Show the branches of {repo}"),
        ),
        ConvTab::Commits => (
            "COMMITS",
            commits.unwrap_or_default(),
            "Show the commits behind what is open".to_string(),
        ),
        ConvTab::Checks => (
            "CHECKS",
            checks.unwrap_or_default(),
            "Show what ran against this commit".to_string(),
        ),
    };

    if !*open.read() {
        return rsx! {
            div {
                class: "convrail",
                title: "{rail_why}",
                onclick: move |_| open.set(true),
                span { class: "convrail-chev", "‹" }
                div { class: "convrail-label",
                    "{rail_label}"
                    if rail_count > 0 {
                        span { class: "convrail-count", "{rail_count}" }
                    }
                }
            }
        };
    }

    let reloading = match showing {
        ConvTab::Talk => matches!(&*conv, Conversation::Loading),
        ConvTab::Branches => matches!(&*st.branches.read(), BranchList::Loading),
        ConvTab::Commits => matches!(
            &*st.commits.read(),
            CommitList::Loading | CommitList::More(_)
        ),
        ConvTab::Checks => matches!(&*st.checks.read(), CheckList::Loading),
    };

    let talk = match &*conv {
        Conversation::Loading => rsx! {
            div { class: "panel-empty", "Loading the conversation…" }
        },
        Conversation::Failed(e) => rsx! {
            div { class: "gherror", "{e}" }
        },
        Conversation::Ready(thread) if thread.comments.is_empty() => rsx! {
            div { class: "panel-empty", "No comments yet." }
        },
        Conversation::Ready(thread) => rsx! {
            for (i , c) in thread.comments.iter().enumerate() {
                CommentRow { key: "{i}", c: c.clone() }
            }
            if thread.truncated {
                div { class: "panel-empty",
                    "This conversation is longer than pullspace loads — open it on github.com to read the rest."
                }
            }
        },
    };

    let reload_cls = if reloading { "iconbtn spin" } else { "iconbtn" };
    let reload_why = match showing {
        ConvTab::Talk => "Reload the conversation from GitHub",
        ConvTab::Branches => "Reload the branches from GitHub",
        ConvTab::Commits => "Reload the commits from GitHub",
        // The one of the four most worth pressing twice: a build that was
        // running a minute ago has usually finished by now.
        ConvTab::Checks => "Reload the checks from GitHub",
    };
    // What the commits below lead into: the pull request they are on, or the
    // branch they are the history of. It travels with each commit that is
    // opened, so the list stays beside it — see [`CommitFrom`].
    let from = match (&source, &desc) {
        (Some(CommitSource::Pr(..)), Some(header)) => CommitFrom::Pr(header.clone()),
        (Some(CommitSource::Branch(_, branch)), _) => CommitFrom::Branch(branch.clone()),
        (Some(CommitSource::Compare(_, base, head)), _) => {
            CommitFrom::Compare(base.clone(), head.clone())
        }
        _ => CommitFrom::Alone,
    };
    // Which commit the checks are about, for the button that fetches them
    // again. Peeked: the header is redrawn by every one of the lists as it
    // lands, and this is only read when something is clicked.
    let at_sha = st.workspace.peek().checks_key().map(|(_, sha)| sha);
    let number = desc.as_ref().map(|d| d.number).unwrap_or_default();
    let listing = source.clone();
    let repo_for_reload = repo.clone();

    rsx! {
        Splitter { edge: Edge::Conv }
        div { class: "convpane",
            div { class: "conv-hdr",
                // Two things are worth knowing about a pull request besides its
                // diff: what people said about it, and what it is made of. A
                // repository read on its own has no discussion, and what takes
                // its place is the thing it does have: branches.
                if desc.is_some() {
                    PaneTab {
                        label: "CONVERSATION",
                        on: showing == ConvTab::Talk,
                        count: (count > 0).then_some(count),
                        tone: "",
                        why: "The description, the discussion and the notes left on lines of the diff"
                            .to_string(),
                        onpick: move |_| tab.set(ConvTab::Talk),
                    }
                } else {
                    PaneTab {
                        label: "BRANCHES",
                        on: showing == ConvTab::Branches,
                        count: branches,
                        tone: "",
                        why: format!("Every branch of {repo} — open one to read the code on it"),
                        onpick: move |_| tab.set(ConvTab::Branches),
                    }
                }
                if source.is_some() {
                    PaneTab {
                        label: "COMMITS",
                        on: showing == ConvTab::Commits,
                        count: commits,
                        tone: "",
                        why: match &source {
                            Some(CommitSource::Branch(_, branch)) => {
                                format!("The commits on {branch}, newest first")
                            }
                            _ => "Every commit on this pull request, oldest first".to_string(),
                        },
                        onpick: move |_| tab.set(ConvTab::Commits),
                    }
                }
                PaneTab {
                    label: "CHECKS",
                    on: showing == ConvTab::Checks,
                    count: checks,
                    tone: checks_tone,
                    why: checks_why,
                    onpick: move |_| tab.set(ConvTab::Checks),
                }
                span { class: "spacer" }
                button {
                    class: reload_cls,
                    title: "{reload_why}",
                    disabled: reloading,
                    // Root scope: this button is re-rendered by the load it starts.
                    onclick: move |_| match showing {
                        ConvTab::Talk => {
                            spawn_forever(load(st, repo_for_reload.clone(), number));
                        }
                        ConvTab::Branches => {
                            spawn_forever(load_branches(st, repo_for_reload.clone()));
                        }
                        ConvTab::Commits => {
                            if let Some(source) = listing.clone() {
                                spawn_forever(load_commits(st, source));
                            }
                        }
                        ConvTab::Checks => {
                            if let Some(sha) = at_sha.clone() {
                                spawn_forever(load_checks(st, repo_for_reload.clone(), sha));
                            }
                        }
                    },
                    span { class: "glyph", "⟳" }
                }
                button {
                    class: "iconbtn",
                    title: "Hide this pane",
                    onclick: move |_| open.set(false),
                    "›"
                }
            }
            div { class: "conv-body", "data-keep": "conv",
                match showing {
                    ConvTab::Talk => rsx! {
                        if let Some(desc) = desc.clone() {
                            Description { desc }
                        }
                        {talk}
                    },
                    ConvTab::Branches => rsx! {
                        BranchesBody {
                            repo: repo.clone(),
                            at: at_branch.clone(),
                            against: against.clone(),
                            compared: compared.clone(),
                        }
                    },
                    ConvTab::Commits => rsx! {
                        if let Some(source) = source.clone() {
                            CommitsBody { source, from: from.clone(), at: at_commit.clone() }
                        }
                    },
                    ConvTab::Checks => rsx! {
                        ChecksBody {}
                    },
                }
            }
        }
    }
}

/// One of the pane's three headings, as the button that turns it on.
#[component]
fn PaneTab(
    label: &'static str,
    on: bool,
    /// Shown beside the label once it is known. `None` while it is not — an
    /// unread list is not a list of none.
    count: Option<usize>,
    /// What colour to say the count in: `ok`, `bad`, `run`, or empty for the
    /// plain one. It is how the checks tab answers its own question without
    /// being opened; the other two are counting comments, which are not good
    /// news or bad news.
    tone: &'static str,
    why: String,
    onpick: EventHandler<()>,
) -> Element {
    let class = if on { "convtab on" } else { "convtab" };
    rsx! {
        button {
            class: "{class}",
            title: "{why}",
            onclick: move |_| onpick.call(()),
            span { class: "side-title", "{label}" }
            if let Some(count) = count {
                span { class: "convcount {tone}", "{count}" }
            }
        }
    }
}

/// What is behind the code on screen: the commits, and each one a way into its
/// own diff.
///
/// Two lists in one, because they are read the same way: everything on a pull
/// request, oldest first — the order it was written in — or a branch's history,
/// newest first, which is the order `git log` writes it in and the order
/// anybody reads back through one.
#[component]
fn CommitsBody(source: CommitSource, from: CommitFrom, at: Option<String>) -> Element {
    let st = use_context::<St>();
    let held = st.commits.read();
    let branch = source.branch().is_some();
    let comparing = matches!(source, CommitSource::Compare(..));

    let (commits, waiting) = match &*held {
        // Idle only ever lasts as long as it takes the effect in `App` to see
        // that this tab is up — see the comment there.
        CommitList::Idle | CommitList::Loading => {
            return rsx! {
                div { class: "panel-empty", "Loading the commits…" }
            };
        }
        CommitList::Failed(e) => {
            return rsx! {
                div { class: "gherror", "{e}" }
            };
        }
        CommitList::Ready(commits) if commits.items.is_empty() => {
            return rsx! {
                div { class: "panel-empty",
                    if comparing {
                        "Nothing between these two — the right-hand side adds no commits."
                    } else if branch {
                        "No commits on this branch."
                    } else {
                        "No commits on this pull request."
                    }
                }
            };
        }
        CommitList::Ready(commits) => (commits, false),
        CommitList::More(commits) => (commits, true),
    };
    // More behind what is here — which on a pull request is the end of what
    // GitHub will say, and on the two that arrive a page at a time is one more
    // request away.
    let more = commits.truncated;
    let paged = source.is_paged();
    // Which way that page goes: back through a branch's history, or on through
    // the commits of a comparison, which are read oldest first.
    let older = source.older_first();
    // How many there are in all, where GitHub said — a hundred rows under a
    // comparison of six thousand commits should not read as the whole of it.
    let total = commits.total as usize;
    let shown = commits.items.len();
    let listing = source.clone();

    rsx! {
        for c in commits.items.iter() {
            CommitRow {
                key: "{c.sha}",
                c: c.clone(),
                repo: source.repo().clone(),
                from: from.clone(),
                current: at.as_deref() == Some(c.sha.as_str()),
            }
        }
        if total > shown {
            div { class: "panel-empty", "{shown} of {total} commits." }
        }
        if more && !paged {
            div { class: "panel-empty",
                "GitHub lists at most 250 commits on a pull request — open it on github.com to read the rest."
            }
        } else if waiting {
            div { class: "panel-empty", "Loading more commits…" }
        } else if more {
            button {
                class: "convolder",
                title: if older { "Read another {github::HISTORY_PAGE} commits back along this branch" } else { "Read the next {github::HISTORY_PAGE} commits between these two" },
                // Root scope: the page lands on a pane that may have been
                // scrolled, folded away or stepped out of in the meantime.
                onclick: move |_| {
                    spawn_forever(load_older(st, listing.clone()));
                },
                if older { "Show older commits" } else { "Show more commits" }
            }
        }
    }
}

/// One commit: what it is called, and — on a click — what it changed.
///
/// Opening one puts its own diff in the panes, against the commit before it,
/// while this list stays where it is: reading a branch commit by commit is
/// clicking down the list, and the row you are on is marked as you go.
#[component]
fn CommitRow(c: CommitSummary, repo: RepoRef, from: CommitFrom, current: bool) -> Element {
    let st = use_context::<St>();
    // A message with more than a subject line to it has the rest behind the
    // chevron. Most have nothing under the fold, and a control that unfolds
    // nothing is one that should not have been there.
    let mut unfolded = use_signal(|| false);
    let body = c.body().to_string();
    let more = !body.is_empty();
    let showing = more && *unfolded.read();
    let day = day_of(&c.date);
    let url = c.html_url.clone();
    let has_link = !url.is_empty();
    let sha = c.sha.clone();
    let class = if current {
        "convitem convcommit on"
    } else {
        "convitem convcommit"
    };

    rsx! {
        div {
            class: "{class}",
            title: if current { "Already open" } else { "Open this commit's diff" },
            // Root scope: opening one replaces the panes this row is beside,
            // and the load outlives the render that started it.
            onclick: move |_| {
                if !current {
                    spawn_forever(
                        super::github::open_commit(st, repo.clone(), sha.clone(), from.clone()),
                    );
                }
            },
            div { class: "convmeta",
                span { class: "convwho", "{c.author}" }
                span {
                    class: "convsha",
                    title: "{c.sha}",
                    "{c.short()}"
                }
                if !day.is_empty() {
                    span { class: "convdate", "{day}" }
                }
                // No pill saying which one this is: the accent edge and the
                // lit row say it, as they do for the open file in the
                // explorer — and this pane is narrow enough that a pill here
                // costs the author's name.
                span { class: "spacer" }
                if more {
                    button {
                        class: "iconbtn sm",
                        title: if showing { "Hide the rest of the message" } else { "Show the rest of the message" },
                        onclick: move |e| {
                            // The row under this button opens the commit; this
                            // one only unfolds what it says.
                            e.stop_propagation();
                            let now = *unfolded.peek();
                            unfolded.set(!now);
                        },
                        span { class: "convmore", if showing { "⌃" } else { "⌄" } }
                    }
                }
                if has_link {
                    button {
                        class: "iconbtn sm",
                        title: "Open this commit on github.com",
                        onclick: move |e| {
                            e.stop_propagation();
                            open_browser(&url);
                        },
                        "↗"
                    }
                }
            }
            div { class: "convtitle", "{c.subject()}" }
            if showing {
                div { class: "convcommitbody", "{body}" }
            }
        }
    }
}

/// Below which a list of branches is a list, and above which it is a haystack.
const BRANCH_FILTER_AT: usize = 8;

/// What one lookup on GitHub turned up, and what it was looking for.
///
/// The query travels with the answer because the box runs ahead of it: it is
/// the only way the list can tell "there is no such branch" from "the answer
/// for what you have typed is still on its way".
#[derive(Clone, PartialEq)]
struct Found {
    query: String,
    got: Result<Vec<Branch>, String>,
}

/// The rows to draw: what the list in hand matches, and after it whatever
/// GitHub found that was not in the list at all.
///
/// Nothing is drawn twice — a branch inside the first pages *and* returned by
/// the lookup is the row that was already there, which is the one that knows
/// whether it is protected.
fn merged(local: Vec<Branch>, found: &[Branch]) -> Vec<Branch> {
    let mut out = local;
    for b in found {
        if !out.iter().any(|had| had.name == b.name) {
            out.push(b.clone());
        }
    }
    out
}

/// Every branch of the repository, and each one a way into the code on it.
///
/// The whole repository, not a diff: opening a branch is browsing it at its
/// tip, with the file being read carried across wherever that branch still has
/// it — see [`St::enter_repo`](super::app::St::enter_repo). What the branch is
/// made of is the tab beside this one.
///
/// The box over the list is a filter while the list is all there is, and a
/// search once it is not: past a few hundred branches GitHub's list is a
/// beginning rather than the whole, and what is typed goes to GitHub as well —
/// see [`github::matching_branches`].
#[component]
fn BranchesBody(
    repo: RepoRef,
    at: Option<String>,
    /// The branch a comparison started from a row here would be against — the
    /// one being read, or the base of the comparison already open.
    against: Option<String>,
    /// And the branch that is already the right-hand side of one.
    compared: Option<String>,
) -> Element {
    let st = use_context::<St>();
    let mut filter = use_signal(String::new);

    // Before the early returns below: a hook skipped on one render and run on
    // the next is a hook dioxus counts wrong.
    let found = use_resource(move || {
        // Read the dependencies here, in the synchronous part, so that a
        // keystroke cancels the lookup in flight and starts the next one.
        let typed = filter.read().trim().to_string();
        let token = st.api_token();
        let repo = st.workspace.read().repo_ref().cloned();
        // Only where the list is not the whole of what the repository has.
        // Where it is, filtering it is the complete answer and a request would
        // buy nothing.
        let cut_short = matches!(&*st.branches.read(), BranchList::Ready(list) if list.truncated);
        async move {
            let repo = repo?;
            if typed.is_empty() || !cut_short {
                return None;
            }
            // The next keystroke drops this task where it stands, so nothing
            // reaches GitHub until the typing stops.
            super::compat::sleep(super::github::SEARCH_DEBOUNCE).await;
            Some(Found {
                got: github::matching_branches(&token, &repo, &typed)
                    .await
                    .map_err(|e| format!("{e:#}")),
                query: typed,
            })
        }
    });

    let held = st.branches.read();
    let list = match &*held {
        // Idle only ever lasts as long as it takes the effect in `App` to see
        // that this tab is up — the same as the commits beside it.
        BranchList::Idle | BranchList::Loading => {
            return rsx! {
                div { class: "panel-empty", "Loading the branches…" }
            };
        }
        BranchList::Failed(e) => {
            return rsx! {
                div { class: "gherror", "{e}" }
            };
        }
        BranchList::Ready(list) if list.items.is_empty() => {
            return rsx! {
                div { class: "panel-empty", "{repo} has no branches." }
            };
        }
        BranchList::Ready(list) => list,
    };

    // By name is the order GitHub answers in, and on a repository with two
    // hundred branches it is the wrong one for finding the one you want. The
    // box is the answer to that, and it is not worth its line on a repository
    // with four.
    let searching = list.items.len() > BRANCH_FILTER_AT;
    let typed = filter.read().trim().to_string();
    let needle = typed.to_lowercase();
    let here: Vec<Branch> = list
        .items
        .iter()
        .filter(|b| needle.is_empty() || b.name.to_lowercase().contains(&needle))
        .cloned()
        .collect();

    // And what GitHub had to say about the rest of them, when there is a rest
    // and somebody has typed something to look for in it.
    let asked = list.truncated && !typed.is_empty();
    let answer = found.cloned().flatten().filter(|f| f.query == typed);
    let looking = asked && answer.is_none();
    let (shown, error) = match answer.as_ref().map(|f| &f.got) {
        Some(Ok(hits)) => (merged(here, hits), None),
        Some(Err(e)) => (here, Some(e.clone())),
        None => (here, None),
    };

    rsx! {
        if searching {
            div { class: "branchfind",
                input {
                    class: "ghinput",
                    r#type: "text",
                    placeholder: if list.truncated { "Find a branch…".to_string() } else { format!("Filter {} branches…", list.items.len()) },
                    title: if list.truncated { "Filters the branches listed here, and asks GitHub for the ones past them" } else { "Filters the branches listed here" },
                    spellcheck: "false",
                    autocomplete: "off",
                    value: "{filter}",
                    oninput: move |e| filter.set(e.value()),
                }
            }
        }
        for b in shown.iter() {
            BranchRow {
                key: "{b.name}",
                b: b.clone(),
                repo: repo.clone(),
                current: at.as_deref() == Some(b.name.as_str()),
                against: against.clone().filter(|left| *left != b.name),
                compared: compared.as_deref() == Some(b.name.as_str()),
            }
        }
        if let Some(e) = error {
            div { class: "gherror", "{e}" }
        }
        if looking {
            div { class: "panel-empty", "Looking for “{typed}” on GitHub…" }
        } else if shown.is_empty() {
            div { class: "panel-empty", "No branch of {repo} matches “{typed}”." }
            // The one thing that turns a wrong-looking answer into a usable
            // one. Past the listed branches this is GitHub's ref index
            // answering, and it matches from the start of the name and
            // letter for letter — so a fragment from the middle, or the right
            // name in the wrong case, finds nothing however many branches
            // there are. The rows above are filtered here instead, which is
            // why the same box can be kinder about both.
            if list.truncated {
                div { class: "panel-empty",
                    "Past the first {list.items.len()}, GitHub looks a branch up by what it starts "
                    "with, exactly as written — try the beginning of the name, capitals and all."
                }
            }
        } else if list.truncated && typed.is_empty() {
            div { class: "panel-empty",
                "The first {list.items.len()} branches of {repo}. Type to look up the rest on GitHub."
            }
        }
    }
}

/// One branch: what it is called, what is at the end of it, and — on a click —
/// the repository as that branch has it.
///
/// `⇄` beside it is the other thing to do with a branch: hold it up against
/// whichever one is being read. It is on the row rather than in a form of its
/// own because that is the question — *this* branch against the one I am on —
/// and a pair of dropdowns would be a longer way of saying it.
#[component]
fn BranchRow(
    b: Branch,
    repo: RepoRef,
    current: bool,
    /// The left-hand side a comparison from this row would have, when there is
    /// one and it is not this branch: comparing a branch with itself is a
    /// question with nothing in the answer.
    against: Option<String>,
    /// Whether this row is already the right-hand side of the comparison on
    /// screen.
    compared: bool,
) -> Element {
    let st = use_context::<St>();
    let name = b.name.clone();
    let sha = b.sha.clone();
    // One clone each for the two things a row does — browse it, or hold it up
    // against the branch being read — since each moves what it needs into its
    // own handler.
    let compare_to = b.name.clone();
    let in_repo = repo.clone();
    let class = if current {
        "convitem convbranch on"
    } else {
        "convitem convbranch"
    };

    rsx! {
        div {
            class: "{class}",
            title: if current { "Already open" } else { "Read {repo} at {b.name}" },
            // Root scope: opening one replaces the panes this row is beside.
            onclick: move |_| {
                if !current {
                    spawn_forever(
                        super::github::browse_branch(
                            st,
                            repo.clone(),
                            name.clone(),
                            Some(sha.clone()),
                        ),
                    );
                }
            },
            div { class: "convmeta",
                span { class: "branchname", "{b.name}" }
                if b.protected {
                    span {
                        class: "convkind",
                        title: "GitHub refuses pushes straight at this branch",
                        "protected"
                    }
                }
                span { class: "spacer" }
                if let Some(left) = against {
                    button {
                        class: if compared { "iconbtn sm on" } else { "iconbtn sm" },
                        title: if compared { "Already comparing {left}...{b.name}" } else { "Compare {left}...{b.name} — what {b.name} has that {left} does not" },
                        onclick: move |e| {
                            // The row under this button opens the branch; this
                            // one holds it up against the one being read.
                            e.stop_propagation();
                            if !compared {
                                spawn_forever(
                                    super::github::open_compare(
                                        st,
                                        in_repo.clone(),
                                        left.clone(),
                                        compare_to.clone(),
                                    ),
                                );
                            }
                        },
                        span { class: "glyph", "⇄" }
                    }
                }
                span {
                    class: "convsha",
                    title: "{b.sha}",
                    "{short_sha(&b.sha)}"
                }
            }
        }
    }
}

/// What ran against the commit on screen: the tests, the linters, the deploy
/// previews — each with the mark it earned.
///
/// A check belongs to a commit, so the list says which commit it is about. On a
/// pull request that is its head; on one of its commits, that commit — and the
/// seven characters at the top are what tells the reader which question they
/// are looking at the answer to.
#[component]
fn ChecksBody() -> Element {
    let st = use_context::<St>();
    let held = st.checks.read();

    match &*held {
        // Idle only ever lasts as long as it takes the effect in `App` to see
        // that this tab is up — the same as the commits beside it.
        CheckList::Idle | CheckList::Loading => rsx! {
            div { class: "panel-empty", "Loading the checks…" }
        },
        CheckList::Failed(e) => rsx! {
            div { class: "gherror", "{e}" }
        },
        CheckList::Ready(checks) if checks.items.is_empty() => rsx! {
            div { class: "panel-empty",
                "Nothing has run against this commit — no checks, and no statuses posted."
            }
        },
        CheckList::Ready(checks) => {
            let short: String = checks.sha.chars().take(7).collect();
            let state = checks.state();
            rsx! {
                div { class: "checkroll",
                    span { class: "checkicon {state.tone()}", "{state.glyph()}" }
                    span { class: "checktally", "{checks.tally().phrase()}" }
                    span { class: "spacer" }
                    span {
                        class: "convsha",
                        title: "{checks.sha}",
                        "{short}"
                    }
                }
                for (i , c) in checks.items.iter().enumerate() {
                    CheckRow { key: "{i}-{c.name}", c: c.clone() }
                }
                if checks.truncated {
                    div { class: "panel-empty",
                        "This commit has more checks than pullspace lists — open it on github.com to see the rest."
                    }
                }
            }
        }
    }
}

/// One check: what it is called, how it went, and — on a click — everything it
/// wrote down.
///
/// What it wrote is two things, and they arrive at different times. The report
/// came with the list and is markdown the check composed for a person to read;
/// the annotations are one more request, made the first time this row is
/// opened, and are the half that names files and lines.
#[component]
fn CheckRow(c: Check) -> Element {
    let st = use_context::<St>();
    let url = c.html_url.clone();
    let has_link = !url.is_empty();
    // Anything to open the row for? Most checks pass and say nothing, and a
    // row that unfolds nothing is a row that should not have been a control.
    let more = c.has_detail();
    let mut unfolded = use_signal(|| false);
    let showing = more && *unfolded.read();

    // The service that ran it, how long it took, and how many lines it marked
    // up — and nothing where there is nothing to say, rather than an empty
    // line under the name.
    let marked = match c.annotations {
        0 => String::new(),
        1 => "1 annotation".to_string(),
        n => format!("{n} annotations"),
    };
    let aside = [c.source.as_str(), c.took.as_str(), marked.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    let tone = c.state.tone();
    let class = match (more, showing) {
        (true, true) => "convitem convcheck can open",
        (true, false) => "convitem convcheck can",
        _ => "convitem convcheck",
    };
    let why = match (more, showing) {
        (true, true) => "Fold this check away",
        (true, false) => "Show what this check wrote",
        // Said rather than left blank: a row that does not open when its
        // neighbours do is worth a word.
        _ => "This check left nothing behind — ↗ opens its log on github.com",
    };
    let id = c.id;
    let wanted = c.annotations > 0;

    rsx! {
        div {
            class: "{class}",
            title: "{why}",
            onclick: move |_| {
                if !more {
                    return;
                }
                let now = *unfolded.peek();
                unfolded.set(!now);
                // Asked for on the way open, and only once — `load_annotations`
                // drops the second ask. Root scope: the fetch outlives a row
                // folded away again while it is in flight.
                if !now && wanted {
                    spawn_forever(load_annotations(st, id));
                }
            },
            div { class: "convmeta",
                span {
                    class: "checkicon {tone}",
                    title: "{c.label}",
                    "{c.state.glyph()}"
                }
                span { class: "checkname", "{c.name}" }
                span { class: "spacer" }
                span { class: "convkind {tone}", "{c.label}" }
                if more {
                    span { class: "convmore", if showing { "⌃" } else { "⌄" } }
                }
                if has_link {
                    button {
                        class: "iconbtn sm",
                        title: "Open this check on github.com — the whole log is there",
                        onclick: move |e| {
                            // The row unfolds what GitHub already told us; this
                            // leaves for the part it did not.
                            e.stop_propagation();
                            open_browser(&url);
                        },
                        "↗"
                    }
                }
            }
            if !aside.is_empty() {
                div { class: "checkaside", "{aside}" }
            }
            if !c.summary.is_empty() {
                div { class: "checksummary", "{c.summary}" }
            }
            if showing {
                CheckDetail { c: c.clone() }
            }
        }
    }
}

/// What one check wrote: its report, and every line of the code it marked up.
///
/// Split out of the row so that opening one check does not re-render the other
/// forty — and so that the annotations landing re-renders only the row that
/// asked for them.
#[component]
fn CheckDetail(c: Check) -> Element {
    let st = use_context::<St>();
    let held = st.annots.read();

    rsx! {
        div {
            class: "checkdetail",
            // The detail is the row's own; clicking inside it — a link in the
            // report, a line to jump to — must not fold it away again.
            onclick: move |e| e.stop_propagation(),
            if !c.report.is_empty() {
                div { class: "checkreport", Body { text: c.report.clone() } }
            }
            match held.get(&c.id) {
                Some(Annots::Loading) => rsx! {
                    div { class: "panel-empty", "Loading what it marked up…" }
                },
                Some(Annots::Failed(e)) => rsx! {
                    div { class: "gherror", "{e}" }
                },
                Some(Annots::Ready(list)) if list.is_empty() => rsx! {
                    // It counted them and then had none: they expire, and an
                    // old check run is a common thing to be reading.
                    div { class: "panel-empty", "GitHub no longer has the lines this check marked." }
                },
                Some(Annots::Ready(list)) => rsx! {
                    for (i , a) in list.iter().enumerate() {
                        AnnotationRow { key: "{i}", a: a.clone() }
                    }
                },
                None => rsx! {},
            }
        }
    }
}

/// One line of the code a check had something to say about — and the way to
/// that line.
///
/// This is the whole reason the checks are worth having in here rather than in
/// the other tab: the file it names is in the explorer, so a failing assertion
/// is one click from the code that failed it.
#[component]
fn AnnotationRow(a: Annotation) -> Element {
    let st = use_context::<St>();
    let path = a.path.clone();
    // A check can mark up a file that is not in what is on screen — a
    // workflow file on a commit whose tree would not load, or a path from
    // somewhere else in the build. Named either way; only openable when it is
    // there to open.
    let here = st.has_file(&path);
    let lines = if a.end_line > a.line {
        format!("{}–{}", a.line, a.end_line)
    } else {
        a.line.to_string()
    };
    let loc = format!("{}:{lines}", a.path.display());
    let line = a.line;
    let tone = a.level.tone();

    rsx! {
        div { class: "annrow",
            div { class: "annhead",
                span {
                    class: "checkicon {tone}",
                    title: "{a.level.label()}",
                    "{a.level.glyph()}"
                }
                if here {
                    div {
                        class: "convloc",
                        title: "Open {loc}",
                        onclick: move |_| st.open_at(path.clone(), line),
                        "{loc}"
                    }
                } else {
                    div {
                        class: "convloc off",
                        title: "This file is not in the commit on screen",
                        "{loc}"
                    }
                }
            }
            if !a.title.is_empty() {
                div { class: "anntitle", "{a.title}" }
            }
            if !a.message.is_empty() {
                div { class: "annmsg", "{a.message}" }
            }
            if !a.raw_details.is_empty() {
                div { class: "annraw", "{a.raw_details}" }
            }
        }
    }
}

/// A body, drawn — and the note that says what drawing it had to leave out.
///
/// Parsed here rather than memoised: a comment is a few hundred bytes, and the
/// row it is in only re-renders when the comment itself changes.
#[component]
fn Body(text: String) -> Element {
    let st = use_context::<St>();
    // `#123` and `@name` in a review are references, and this is the
    // repository they refer to.
    let doc = markdown::parse_refs(&text, &refs_of(&st));
    rsx! {
        if doc.raw_html {
            HtmlNote {}
        }
        {super::markdown::render_body(st, Path::new(ROOT), &doc)}
    }
}

/// Which repository the bodies in this pane were written against.
fn refs_of(st: &St) -> markdown::Refs {
    st.workspace
        .peek()
        .repo_ref()
        .map(|r| markdown::Refs::of(r.to_string()))
        .unwrap_or_default()
}

/// The button that hands a body to the middle pane.
///
/// A pull request's description is prose, and prose in a 380px column is a
/// ribbon. This is the way out of it — and it is a button rather than the
/// default because the column is the right place for the short ones, which is
/// most of them.
#[component]
fn ReadHere(doc: Reading) -> Element {
    let st = use_context::<St>();
    rsx! {
        button {
            class: "iconbtn sm",
            title: "Read this in the middle pane",
            onclick: move |_| st.read_doc(doc.clone()),
            "\u{2922}"
        }
    }
}

/// Said once, in one place: raw HTML in a comment is not drawn, and a reader
/// looking at a gap where a table or a `<details>` should be is owed the reason.
#[component]
fn HtmlNote() -> Element {
    rsx! {
        span {
            class: "convkind convhtml",
            title: "This comment contains HTML beyond the folds, line breaks and pictures this app reads — a table written in tags, a centred banner. It is not drawn here; open the comment on github.com to read it.",
            "html not drawn"
        }
    }
}

#[component]
fn Description(desc: PrHeader) -> Element {
    let st = use_context::<St>();
    let url = desc.html_url.clone();
    let body = desc.body.trim().to_string();
    let written = !body.is_empty();
    let doc = Reading {
        title: desc.title.clone(),
        meta: format!("#{} \u{00b7} {}", desc.number, desc.author),
        body,
        url: desc.html_url.clone(),
    };
    rsx! {
        div { class: "convitem convdesc",
            div { class: "convmeta",
                span { class: "convwho", "{desc.author}" }
                span { class: "convkind", "opened #{desc.number}" }
                if desc.draft {
                    span { class: "prdraft", "draft" }
                }
                span { class: "spacer" }
                if written {
                    ReadHere { doc: doc.clone() }
                }
                button {
                    class: "iconbtn",
                    title: "Open on github.com",
                    onclick: move |_| open_browser(&url),
                    "↗"
                }
            }
            div {
                class: if written { "convtitle readable" } else { "convtitle" },
                title: if written { "Read this in the middle pane" } else { "" },
                onclick: move |_| {
                    if written {
                        st.read_doc(doc.clone());
                    }
                },
                "{desc.title}"
            }
            // Trailing blank lines are common in a template-filled description
            // and would be drawn as empty space at the foot of the card.
            if written {
                Body { text: desc.body.trim().to_string() }
            } else {
                div { class: "convnone", "No description." }
            }
        }
    }
}

#[component]
fn CommentRow(c: Comment) -> Element {
    let st = use_context::<St>();
    // A line comment names a file, and that file is in the explorer — so the
    // location is the way from the comment to the code it is about.
    let target = c.path.clone().map(|p| (p, c.line.unwrap_or(1)));
    let loc = target.as_ref().map(|(p, line)| match c.line {
        Some(_) => format!("{}:{line}", p.display()),
        // A comment whose lines no longer exist in the head commit; the file
        // is still the right place to land.
        None => p.display().to_string(),
    });
    let day = day_of(&c.created_at);
    let url = c.html_url.clone();
    let has_link = !url.is_empty();
    let kind_cls = kind_class(&c);
    let label = kind_label(&c).to_string();
    // A review that runs to pages is prose too, and the offer to read it in
    // the pane belongs on it for the same reason it belongs on a description.
    // Only on the long ones: a button on all forty rows of a busy pull request
    // is a column of buttons.
    let long = c.body.len() > LONG;
    let doc = long.then(|| Reading {
        title: format!("{} \u{00b7} {}", c.author, kind_label(&c)),
        meta: day.clone(),
        body: c.body.trim().to_string(),
        url: c.html_url.clone(),
    });

    rsx! {
        div { class: "convitem",
            div { class: "convmeta",
                span { class: "convwho", "{c.author}" }
                span { class: "{kind_cls}", "{label}" }
                if !day.is_empty() {
                    span { class: "convdate", "{day}" }
                }
                span { class: "spacer" }
                if let Some(doc) = doc {
                    ReadHere { doc }
                }
                if has_link {
                    button {
                        class: "iconbtn sm",
                        title: "Open on github.com",
                        onclick: move |_| open_browser(&url),
                        "↗"
                    }
                }
            }
            if let (Some(loc), Some((path, line))) = (loc, target) {
                div {
                    class: "convloc",
                    title: "Open {loc}",
                    onclick: move |_| st.open_at(path.clone(), line),
                    "{loc}"
                }
            }
            if !c.body.is_empty() {
                Body { text: c.body.clone() }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn branch(name: &str, protected: bool) -> Branch {
        Branch {
            name: name.to_string(),
            sha: format!("sha-of-{name}"),
            protected,
        }
    }

    /// The list stops a few hundred branches in, so what GitHub finds past it
    /// is added rather than swapped in — and a branch in both is the row that
    /// was already there, which is the one that knows it is protected.
    #[test]
    fn what_github_finds_is_added_to_what_was_already_listed() {
        let here = vec![branch("main", true), branch("feat/a", false)];
        let found = [branch("main", false), branch("zygotic-cicada", false)];
        let rows = merged(here, &found);
        let names: Vec<&str> = rows.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, ["main", "feat/a", "zygotic-cicada"]);
        assert!(rows[0].protected, "the listed row is the one that was kept");
    }

    #[test]
    fn nothing_found_leaves_the_list_as_it_was() {
        let here = vec![branch("main", true)];
        assert_eq!(merged(here.clone(), &[]), here);
    }
}
