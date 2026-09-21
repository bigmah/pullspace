//! The pull requests of a repository, given the page.
//!
//! Everything under the top bar, for as long as somebody is choosing what to
//! read next. It used to be a menu off the button that now opens this, and a
//! menu has room to say what a pull request is called. Choosing between them
//! takes more than that: whose it is, how old, whether it builds, what the
//! reviewers made of it, how big a read it is going to be — and, before
//! committing to the several requests that opening one costs, what it says it
//! does.
//!
//! So there are two halves. On the left, a row per pull request with all of
//! that on it. On the right, the description of whichever row the pointer or
//! the arrow keys are on, drawn as written — a peek, in the sense the code pane
//! means it: the thing itself, read without going to it.
//!
//! Where the rows get what they say is two places. Most of it arrives with the
//! list, which is one request however long the list is. The rest — checks,
//! reviews, sizes — is [`PrMore`], one more request that GitHub will only
//! answer for somebody signed in; the rows are complete without it and say more
//! with it.

use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use dioxus::prelude::*;

use crate::backend::auth::open_browser;
use crate::backend::github::{self, PR_PAGE, PrMore, PrStatus, PrSummary, RepoRef};
use crate::backend::markdown;

use super::app::{Account, Fetch, Got, MoreGot, St};
use super::compat;
use super::github::{
    GithubMark, PrStates, browse_repo, load_pr_more, load_repo_prs, open_pr, state_word,
};

/// A description's links are written from the root of the repository, the way
/// a comment's are.
const ROOT: &str = "";

/// How long the pointer rests on a row before the pane turns to it.
///
/// Long enough that crossing the list on the way to somewhere else does not
/// drag a dozen descriptions through the pane; short enough that stopping on a
/// row and being shown it read as the same moment.
const DWELL: Duration = Duration::from_millis(110);

/// How many labels a row has room for before the rest become a count.
const ROW_LABELS: usize = 3;

/// What the rows and the pane have in common: which row is being read out, and
/// what has been asked for.
///
/// Handed down as context rather than as props, because every row needs the
/// same five handles and none of them is a reason to draw a row again.
#[derive(Clone, Copy)]
struct Board {
    /// The row the pane is reading out, by pull request number — set by the
    /// pointer coming to rest on one, or by the arrow keys. `None` until either
    /// has happened, which [`peek_index`] has an answer for.
    at: Signal<Option<u64>>,
    /// The row the pointer is over, which becomes `at` if it stays there.
    hot: Signal<Option<u64>>,
    /// Where the pointer last really was. Scrolling a list under a pointer that
    /// has not moved still tells the rows they are being pointed at, and the
    /// arrow keys scroll the list — so without this the pointer takes the
    /// selection back from the keyboard on every press.
    mouse: Signal<(i32, i32)>,
    /// The pull request a row has been clicked to open, while it is on its way.
    opening: Signal<Option<u64>>,
    /// What has been typed into the filter.
    filter: Signal<String>,
    /// The rows on show: the list, narrowed by whatever is in the filter.
    shown: Memo<Vec<Rc<PrSummary>>>,
}

impl Board {
    /// The pointer is over a row, at `(x, y)`.
    fn moved(self, number: u64, x: f64, y: f64) {
        let here = (x as i32, y as i32);
        if *self.mouse.peek() == here {
            return;
        }
        let mut mouse = self.mouse;
        mouse.set(here);
        if *self.hot.peek() == Some(number) {
            return;
        }
        let mut hot = self.hot;
        hot.set(Some(number));
        let mut at = self.at;
        spawn(async move {
            compat::sleep(DWELL).await;
            // Still here? Then this is a row being looked at, not one being
            // crossed.
            if *hot.peek() == Some(number) && *at.peek() != Some(number) {
                at.set(Some(number));
            }
        });
    }

    /// The row being read out, as an index into the rows on show.
    fn index(self, st: &St) -> Option<usize> {
        let at = *self.at.peek();
        let fallback = fallback(reading(st, false), &self.filter.peek());
        let shown = self.shown.peek();
        peek_index(&shown, at, fallback)
    }

    /// Move the pane one row along, or to one end of the list.
    fn step(self, st: &St, to: Step) {
        let from = self.index(st);
        let target = {
            let shown = self.shown.peek();
            let last = shown.len().saturating_sub(1);
            let next = match (to, from) {
                (_, None) => 0,
                (Step::Next, Some(i)) => (i + 1).min(last),
                (Step::Prev, Some(i)) => i.saturating_sub(1),
            };
            shown.get(next).map(|pr| pr.number)
        };
        let Some(number) = target else { return };
        let mut at = self.at;
        at.set(Some(number));
        // The pointer has not moved, and what it is over is about to: whatever
        // it lands on is not being pointed at.
        let mut hot = self.hot;
        hot.set(None);
        document::eval(&format!(
            "var e=document.getElementById('prow-{number}');\
             if(e)e.scrollIntoView({{block:'nearest'}});"
        ));
    }

    /// Open the row being read out — what Enter does.
    fn open_peeked(self, st: St) {
        let target = self
            .index(&st)
            .and_then(|i| self.shown.peek().get(i).map(|pr| pr.number));
        if let Some(number) = target {
            self.open(st, number);
        }
    }

    /// Open a pull request off the board.
    fn open(self, st: St, number: u64) {
        let repo = st.prs.peek().as_ref().map(|l| l.repo.clone());
        let Some(repo) = repo else { return };
        // The one being read already is not a reload — `⟳` is, and it is on the
        // bar above. It is still somewhere to go, though: back to it.
        if reading(&st, false) == Some(number) {
            return shut(&st);
        }
        let mut opening = self.opening;
        opening.set(Some(number));
        // Root scope: opening one takes the board down around the row that was
        // clicked.
        spawn_forever(open_pr(st, repo, number));
    }
}

/// Where the arrow keys can send the pane.
#[derive(Clone, Copy)]
enum Step {
    Next,
    Prev,
}

/// Put the board away.
pub(super) fn shut(st: &St) {
    let mut board = st.pr_board;
    board.set(false);
}

/// The number of the pull request being read, when it is one of the listed
/// repository's — the row that is somewhere you already are.
///
/// The list is usually of the repository that is open, but the picker can have
/// moved it on to another without opening anything, and #12 over there is not
/// #12 here. `track` is whether to subscribe to the answer: yes while drawing,
/// no from inside a handler.
fn reading(st: &St, track: bool) -> Option<u64> {
    let (ws, prs) = if track {
        (st.workspace.read(), st.prs.read())
    } else {
        (st.workspace.peek(), st.prs.peek())
    };
    let listed = &prs.as_ref()?.repo;
    (ws.repo_ref() == Some(listed))
        .then(|| ws.pr_number())
        .flatten()
}

/// The row to read out when none has been picked: the pull request that is
/// open — until something is typed into the filter, and then the top of what it
/// found. Somebody who types `482` and presses Enter means the first thing that
/// came back, whatever else happens to be open and happens to match.
fn fallback(reading: Option<u64>, filter: &str) -> Option<u64> {
    reading.filter(|_| filter.trim().is_empty())
}

/// Put the caret in the filter, with whatever is in it selected.
fn focus_filter() {
    document::eval("var e=document.querySelector('.prfilter'); if(e){e.focus();e.select();}");
}

/// Hand the keys back to the board. After the tick, not during it: this is
/// asked for from inside a keydown that the window's own listener is about to
/// answer by blurring the box — see `ide::KEYS` — and a focus moved before that
/// is a focus it takes away again.
fn focus_board() {
    document::eval(
        "setTimeout(function(){var e=document.querySelector('.prboard'); if(e)e.focus();},0);",
    );
}

/// Which of the rows on show the pane reads out.
///
/// The one that was picked, for as long as it is still on show. Failing that —
/// nothing picked yet, or the filter has since hidden it — the pull request
/// that is open, because that is where the reader's attention already is; and
/// failing that the top of the list, so the pane is never an empty half of the
/// page beside a list with things in it.
fn peek_index(shown: &[Rc<PrSummary>], at: Option<u64>, reading: Option<u64>) -> Option<usize> {
    let find = |n: Option<u64>| n.and_then(|n| shown.iter().position(|pr| pr.number == n));
    find(at)
        .or_else(|| find(reading))
        .or(if shown.is_empty() { None } else { Some(0) })
}

/// Whether a pull request is one the filter is asking for.
///
/// Every word typed has to be somewhere in what the row says — its number, its
/// title, who wrote it, either branch, any label, where it has got to. Words
/// rather than one phrase, so `ada bug` finds ada's bug fixes without anybody
/// having to know which order a row says things in. `query` arrives lowercased.
fn matches(pr: &PrSummary, query: &str) -> bool {
    let mut words = query.split_whitespace().peekable();
    if words.peek().is_none() {
        return true;
    }
    let mut hay = format!(
        "#{} {} {} {} {} {}",
        pr.number,
        pr.title,
        pr.author,
        pr.head_label(),
        pr.base_ref,
        pr.status().label(),
    );
    for label in &pr.labels {
        hay.push(' ');
        hay.push_str(&label.name);
    }
    let hay = hay.to_lowercase();
    words.all(|w| hay.contains(w))
}

/// `1 file`, `8 files`.
fn count_of(n: u32, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

#[component]
pub fn PrBoard() -> Element {
    let st = use_context::<St>();
    let mut filter = use_signal(String::new);

    let shown = use_memo(move || {
        let query = filter.read().to_lowercase();
        st.prs
            .read()
            .as_ref()
            .map(|list| {
                list.items()
                    .iter()
                    .filter(|pr| matches(pr, &query))
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    });
    let board = use_context_provider(|| Board {
        at: Signal::new(None),
        hot: Signal::new(None),
        mouse: Signal::new((-1, -1)),
        opening: Signal::new(None),
        filter,
        shown,
    });
    // One reading of the clock for as long as the board is up: "3 days ago" is
    // not something that needs to tick, and a time that moved on every render
    // would be a reason to draw every row again on every render.
    let now = use_hook(github::now_secs);

    // The rest of what the rows say, the first time there is a list to say it
    // about and somebody to ask as. Spent on the board being looked at rather
    // than on the list being fetched, which happens for every review whether
    // anybody opens this or not.
    use_effect(move || {
        let signed_in = matches!(&*st.account.read(), Account::SignedIn { .. });
        let ask = st.prs.read().as_ref().and_then(|list| {
            let ready = matches!(list.got, Got::Ready(ref items) if !items.is_empty());
            (ready && list.more.got == MoreGot::Idle).then(|| (list.repo.clone(), list.state))
        });
        if let Some((repo, state)) = ask.filter(|_| signed_in) {
            spawn_forever(load_pr_more(st, repo, state));
        }
    });

    let reading = reading(&st, true);
    let signed_out = matches!(&*st.account.read(), Account::SignedOut);
    let fetch = st.fetch.read().clone();
    let query = filter.read().clone();

    let prs = st.prs.read();
    let Some(list) = prs.as_ref() else {
        // A board with no list behind it is one whose repository has not been
        // named yet, which the bar it hangs off does not allow. Put away rather
        // than drawn empty, in case it ever does.
        return rsx! {};
    };
    let repo = list.repo.clone();
    let again = repo.clone();
    let loading = matches!(list.got, Got::Loading) || list.more.got == MoreGot::Loading;
    let word = state_word(list.state);
    let total = list.items().len();
    let capped = total >= PR_PAGE;
    let more = list.more.known.clone();
    let more_note = match &list.more.got {
        MoreGot::Loading => rsx! {
            span { class: "prboardnote busy", "reading checks and reviews…" }
        },
        // What went wrong is the tooltip and the way to ask again is the
        // click: the rows are whole without it, so it is not worth a banner.
        MoreGot::Failed(e) => rsx! {
            button {
                class: "textlink prboardhint bad",
                title: "{e}",
                onclick: move |_| {
                    let mut prs = st.prs;
                    if let Some(list) = prs.write().as_mut() {
                        list.more.got = MoreGot::Idle;
                    }
                },
                "Checks and reviews did not load — try again"
            }
        },
        _ => rsx! {},
    };
    // Whether the four columns only `PrMore` can fill are worth their width.
    let plain = list.more.absent();

    let rows = shown.read();
    let at = *board.at.read();
    let peeked = peek_index(&rows, at, fallback(reading, &query)).map(|i| rows[i].clone());
    let peeked_number = peeked.as_ref().map(|pr| pr.number);
    // Only while something is actually being fetched: a load that failed is no
    // longer on its way, and its row should stop saying so.
    let opening = (*board.opening.read()).filter(|_| matches!(fetch, Fetch::Working(_)));

    let tally = match (&list.got, query.trim().is_empty()) {
        (Got::Ready(_), true) if capped => format!("the {PR_PAGE} most recently updated"),
        (Got::Ready(_), true) => format!("{total} {word}"),
        (Got::Ready(_), false) => format!("{} of {total} {word}", rows.len()),
        _ => String::new(),
    };

    let body = match &list.got {
        Got::Loading => rsx! {
            div { class: "prboardempty", "Loading pull requests…" }
        },
        Got::Failed(e) => {
            let again = repo.clone();
            rsx! {
                div { class: "prboardempty",
                    div { class: "gherror", "{e}" }
                    button {
                        class: "textlink",
                        // Root scope: the note this button is in is replaced
                        // by the load it starts.
                        onclick: move |_| {
                            spawn_forever(load_repo_prs(st, again.clone()));
                        },
                        "Try again"
                    }
                }
            }
        }
        Got::Ready(items) if items.is_empty() => rsx! {
            div { class: "prboardempty", "No {word}pull requests in {repo}." }
        },
        Got::Ready(_) if rows.is_empty() => rsx! {
            div { class: "prboardempty", "Nothing here matches “{query}”." }
        },
        Got::Ready(_) => rsx! {
            div { class: "prcols",
                span {}
                span { "pull request" }
                span { class: "prcell checks", title: "What ran against the head commit", "checks" }
                span { class: "prcell review", "review" }
                span { class: "prcell size", title: "Lines added and removed", "changes" }
                span { class: "prcell talk", title: "Comments, reviews and replies", "said" }
                span { class: "prcell when", title: "When it last changed", "updated" }
            }
            div {
                class: "prrows",
                // Gone from the list is gone from every row of it: coming back
                // to the same row is pointing at it again.
                onmouseleave: move |_| {
                    let mut hot = board.hot;
                    hot.set(None);
                },
                for pr in rows.iter() {
                    // The clone stays — see `github::PrListBody`, where the
                    // same one was seen to break another machine's build.
                    Row {
                        key: "{pr.number}",
                        pr: pr.clone(),
                        more: more.get(&pr.number).cloned(),
                        current: reading == Some(pr.number),
                        peeked: peeked_number == Some(pr.number),
                        opening: opening == Some(pr.number),
                        now,
                    }
                }
                // A page full is a page that may have had more behind it, and
                // a list quietly cut off reads as the whole of what there is.
                if capped {
                    div { class: "prboardnote foot",
                        "The {PR_PAGE} most recently updated. Older ones are on github.com."
                    }
                }
            }
        },
    };
    drop(rows);

    rsx! {
        div {
            class: if plain { "prboard plain" } else { "prboard" },
            tabindex: "-1",
            // The keys work from the moment it is up, without a click to say
            // which part of the page they are meant for.
            onmounted: move |e| {
                spawn(async move {
                    let _ = e.data().set_focus(true).await;
                });
            },
            // Escape is not on this list. It is one of the window's keys —
            // see `ide::escape` — because it has to work wherever the focus
            // has got to, and a key answered in two places is answered twice.
            onkeydown: move |e| {
                match e.key() {
                    Key::ArrowDown => board.step(&st, Step::Next),
                    Key::ArrowUp => board.step(&st, Step::Prev),
                    Key::Enter => board.open_peeked(st),
                    Key::Character(c) if c == "/" => focus_filter(),
                    Key::Character(c) if c == "j" => board.step(&st, Step::Next),
                    Key::Character(c) if c == "k" => board.step(&st, Step::Prev),
                    _ => return,
                }
                e.prevent_default();
            },
            div { class: "prboardhdr",
                span { class: "prboardtitle", "Pull requests" }
                span { class: "prboardrepo", title: "{repo}", "{repo}" }
                input {
                    class: "prfilter",
                    r#type: "text",
                    placeholder: "Filter — title, author, branch, label, #number",
                    title: "Every word has to be somewhere on the row. ↑ ↓ to move, Enter to open.",
                    spellcheck: "false",
                    autocomplete: "off",
                    value: "{query}",
                    oninput: move |e| {
                        filter.set(e.value());
                        // A different list: whatever was picked out of the last
                        // one is not what is wanted out of this one.
                        let mut at = board.at;
                        at.set(None);
                    },
                    // The arrows and Enter mean the same thing with the caret
                    // in here as without it, which is what makes "type a word,
                    // arrow down, Enter" one motion — so those go on up to the
                    // board. The letters do not: in here they are letters.
                    onkeydown: move |e| match e.key() {
                        Key::ArrowDown | Key::ArrowUp | Key::Enter => {}
                        // Out of the box, and first out of what is in it. The
                        // press after this one is the board's.
                        Key::Escape => {
                            filter.set(String::new());
                            focus_board();
                        }
                        _ => e.stop_propagation(),
                    },
                }
                span { class: "prboardtally", "{tally}" }
                {more_note}
                if signed_out && total > 0 {
                    SignInHint {}
                }
                span { class: "spacer" }
                PrStates {}
                // The list is fetched once and kept, which is right for a list
                // glanced at and wrong for one left up over lunch: a build
                // that was running then has finished since.
                button {
                    class: if loading { "iconbtn spin" } else { "iconbtn" },
                    title: "Fetch this list again — what has been pushed, reviewed or built since",
                    disabled: loading,
                    onclick: move |_| {
                        spawn_forever(load_repo_prs(st, again.clone()));
                    },
                    span { class: "glyph", "⟳" }
                }
                button {
                    class: "iconbtn",
                    title: "Back to what is open  (Esc)",
                    onclick: move |_| shut(&st),
                    "✕"
                }
            }
            if let Fetch::Failed(e) = &fetch {
                div { class: "gherror prboarderr", "{e}" }
            }
            div { class: "prboardmain",
                div { class: "prboardlist",
                    {body}
                    BrowseFoot { repo: repo.clone() }
                }
                // A list of one, for the key: a different pull request is a
                // different pane, which is what starts its description at the
                // top rather than wherever the last one was scrolled to.
                for pr in peeked {
                    Peek {
                        key: "{pr.number}",
                        more: more.get(&pr.number).cloned(),
                        current: reading == Some(pr.number),
                        opening: opening == Some(pr.number),
                        repo: repo.clone(),
                        pr,
                        now,
                    }
                }
            }
        }
    }
}

/// Why four of the columns are missing, to the one reader they are missing for.
#[component]
fn SignInHint() -> Element {
    let st = use_context::<St>();
    let mut gh_open = st.gh_open;
    rsx! {
        button {
            class: "textlink prboardhint",
            title: "GitHub only answers for checks, reviews and sizes when it knows who is asking. Everything else on this page works signed out.",
            onclick: move |_| gh_open.set(true),
            "Sign in to see checks, reviews and sizes"
        }
    }
}

/// Where a pull request has got to, as the mark at the head of its row.
#[component]
fn StatusMark(status: PrStatus) -> Element {
    let shape = match status {
        PrStatus::Open => rsx! {
            circle { cx: "8", cy: "8", r: "5.75" }
            circle { class: "fill", cx: "8", cy: "8", r: "2" }
        },
        PrStatus::Draft => rsx! {
            circle { cx: "8", cy: "8", r: "5.75", "stroke-dasharray": "2.6 2.4" }
        },
        PrStatus::Merged => rsx! {
            circle { cx: "4.75", cy: "3.75", r: "1.75" }
            circle { cx: "4.75", cy: "12.25", r: "1.75" }
            circle { cx: "11.75", cy: "8.5", r: "1.75" }
            path { d: "M4.75 5.5v5M4.75 5.5c0 3 2.2 3 5.25 3" }
        },
        PrStatus::Closed => rsx! {
            circle { cx: "8", cy: "8", r: "5.75" }
            path { d: "M6 6l4 4M10 6l-4 4" }
        },
    };
    rsx! {
        svg {
            class: "prmark {status.label()}",
            view_box: "0 0 16 16",
            "aria-hidden": "true",
            {shape}
        }
    }
}

/// A speech bubble, for the count of what has been said.
#[component]
fn TalkMark() -> Element {
    rsx! {
        svg { class: "prmark talk", view_box: "0 0 16 16", "aria-hidden": "true",
            path { d: "M2.75 3.25h10.5v7.5h-6l-2.75 2.5v-2.5h-1.75z" }
        }
    }
}

/// One label, in the colour its repository gave it.
///
/// The colour is a dot rather than the chip: a repository picks label colours
/// against GitHub's white page, and a good third of them are unreadable as text
/// on a dark one — or as a fill under it.
#[component]
fn LabelChip(name: String, color: String) -> Element {
    rsx! {
        span { class: "prlabel", title: "{name}",
            if !color.is_empty() {
                span { class: "prlabeldot", style: "background:{color}" }
            }
            "{name}"
        }
    }
}

/// Somebody's picture, or the space one would have taken.
#[component]
fn Avatar(src: String) -> Element {
    if src.is_empty() {
        return rsx! { span { class: "pravatar none" } };
    }
    // Small on the page, so asked for small: GitHub's avatars take a size.
    let sep = if src.contains('?') { '&' } else { '?' };
    rsx! {
        img {
            class: "pravatar",
            src: "{src}{sep}s=40",
            alt: "",
            "loading": "lazy",
            "referrerpolicy": "no-referrer",
        }
    }
}

/// What ran, as a mark and a number: how many failed if any did, how many are
/// still going if any are, and otherwise how many passed.
fn checks_cell(more: Option<&PrMore>) -> Element {
    let Some(tally) = more.and_then(|m| m.checks) else {
        return rsx! { span { class: "prcell checks" } };
    };
    let state = tally.state();
    let n = match state {
        github::CheckState::Failed => tally.failed,
        github::CheckState::Running => tally.running,
        github::CheckState::Passed => tally.passed,
        github::CheckState::Quiet => tally.quiet,
    };
    rsx! {
        span { class: "prcell checks", title: "{tally.phrase()}",
            span { class: "checkicon {state.tone()}", "{state.glyph()}" }
            span { class: "prcount", "{n}" }
        }
    }
}

#[component]
fn Row(
    pr: Rc<PrSummary>,
    more: Option<PrMore>,
    current: bool,
    peeked: bool,
    opening: bool,
    now: i64,
) -> Element {
    let st = use_context::<St>();
    let board = use_context::<Board>();
    let number = pr.number;
    let status = pr.status();

    let mut class = String::from("prow");
    let shut = matches!(status, PrStatus::Merged | PrStatus::Closed);
    for (on, word) in [
        (current, " on"),
        (peeked, " at"),
        (opening, " busy"),
        (shut, " shut"),
    ] {
        if on {
            class.push_str(word);
        }
    }
    let opened = github::ago(&pr.created_at, now);
    let updated = github::ago_short(&pr.updated_at, now);
    let updated_why = format!("Updated {}", github::ago(&pr.updated_at, now));
    let extra_labels = pr.labels.len().saturating_sub(ROW_LABELS);
    let more = more.as_ref();

    rsx! {
        div {
            id: "prow-{number}",
            class: "{class}",
            onmousemove: move |e| {
                let at = e.client_coordinates();
                board.moved(number, at.x, at.y);
            },
            onclick: move |_| board.open(st, number),
            span { class: "prcell mark", title: "{status.label()}", StatusMark { status } }
            div { class: "prowmain",
                div { class: "prowtop",
                    span { class: "prowtitle", "{pr.title}" }
                    span { class: "prlabels",
                        for label in pr.labels.iter().take(ROW_LABELS) {
                            LabelChip {
                                key: "{label.name}",
                                name: label.name.clone(),
                                color: label.color.clone(),
                            }
                        }
                        if extra_labels > 0 {
                            span { class: "prlabel", "+{extra_labels}" }
                        }
                    }
                    if opening {
                        span { class: "prhere busy", "opening…" }
                    } else if current {
                        span { class: "prhere", "reading" }
                    }
                    if more.is_some_and(|m| m.conflicts) {
                        span {
                            class: "prdraft closed",
                            title: "The base branch has moved under it: it cannot be merged as it stands",
                            "conflicts"
                        }
                    }
                }
                div { class: "prowmeta",
                    span { class: "prnum", "#{number}" }
                    Avatar { src: pr.avatar.clone() }
                    span { class: "prwho", "{pr.author}" }
                    if !opened.is_empty() {
                        span { class: "sep", "opened {opened}" }
                    }
                    span { class: "prbranches sep", title: "{pr.head_label()} → {pr.base_ref}",
                        "{pr.head_label()} → {pr.base_ref}"
                    }
                }
            }
            {checks_cell(more)}
            span { class: "prcell review",
                if let Some(review) = more.and_then(|m| m.review) {
                    span { class: "convkind {review.tone()}", "{review.label()}" }
                }
            }
            span { class: "prcell size",
                if let Some(m) = more {
                    span { class: "pradd", "+{m.additions}" }
                    span { class: "prdel", "−{m.deletions}" }
                }
            }
            span { class: "prcell talk",
                if let Some(m) = more.filter(|m| m.comments > 0) {
                    TalkMark {}
                    span { class: "prcount", "{m.comments}" }
                }
            }
            span { class: "prcell when", title: "{updated_why}", "{updated}" }
        }
    }
}

/// The pull request the pointer or the arrow keys are on, read out: everything
/// its row says, said in full, and under it the description as it was written.
#[component]
fn Peek(
    repo: RepoRef,
    pr: Rc<PrSummary>,
    more: Option<PrMore>,
    current: bool,
    opening: bool,
    now: i64,
) -> Element {
    let st = use_context::<St>();
    let board = use_context::<Board>();
    let number = pr.number;
    let status = pr.status();
    let url = pr.html_url.clone();

    // `#123` and `@name` in a description are references, and the repository
    // they refer to is the one listed — which need not be the one open.
    let body = pr.body.trim();
    let doc = markdown::parse_refs(body, &markdown::Refs::of(repo.to_string()));

    let opened = github::ago(&pr.created_at, now);
    let updated = github::ago(&pr.updated_at, now);
    let more = more.as_ref();
    let size = more.map(|m| {
        (
            format!("+{}", m.additions),
            format!("−{}", m.deletions),
            format!("in {}", count_of(m.files, "file")),
        )
    });
    let said = more
        .filter(|m| m.comments > 0)
        .map(|m| count_of(m.comments, "comment"));
    let checks = more.and_then(|m| m.checks);

    rsx! {
        div { class: "prpeek",
            div { class: "prpeekhdr",
                div { class: "prpeektop",
                    span { class: "prstatus {status.label()}",
                        StatusMark { status }
                        "{status.label()}"
                    }
                    span { class: "prnum", "#{number}" }
                    if let Some(review) = more.and_then(|m| m.review) {
                        span { class: "convkind {review.tone()}", "{review.label()}" }
                    }
                    if more.is_some_and(|m| m.conflicts) {
                        span { class: "prdraft closed", "conflicts" }
                    }
                    span { class: "spacer" }
                    if let Some(tally) = checks {
                        span { class: "prpeekchecks",
                            span { class: "checkicon {tally.state().tone()}", "{tally.state().glyph()}" }
                            "{tally.phrase()}"
                        }
                    }
                }
                div { class: "prpeektitle", "{pr.title}" }
                div { class: "prpeekmeta",
                    Avatar { src: pr.avatar.clone() }
                    span { class: "prwho", "{pr.author}" }
                    if !opened.is_empty() {
                        span { class: "sep", "opened {opened}" }
                    }
                    if !updated.is_empty() && updated != opened {
                        span { class: "sep", "updated {updated}" }
                    }
                }
                div { class: "prpeekmeta",
                    span { class: "prbranches", "{pr.head_label()} → {pr.base_ref}" }
                    if let Some((add, del, files)) = size {
                        span { class: "prsize sep",
                            span { class: "pradd", "{add}" }
                            span { class: "prdel", "{del}" }
                            "{files}"
                        }
                    }
                    if let Some(said) = said {
                        span { class: "sep", "{said}" }
                    }
                }
                if !pr.labels.is_empty() || !pr.reviewers.is_empty() {
                    div { class: "prpeekmeta wrap",
                        for label in pr.labels.iter() {
                            LabelChip {
                                key: "{label.name}",
                                name: label.name.clone(),
                                color: label.color.clone(),
                            }
                        }
                        if !pr.reviewers.is_empty() {
                            span {
                                class: "prwaiting",
                                title: "Asked for a review, and yet to give one",
                                "waiting on {pr.reviewers.join(\", \")}"
                            }
                        }
                    }
                }
            }
            div { class: "prpeekbody",
                if body.is_empty() {
                    div { class: "prpeeknone", "No description was written for this one." }
                } else {
                    if doc.raw_html {
                        span {
                            class: "convkind convhtml",
                            title: "This description contains HTML beyond the folds, line breaks and pictures this app reads. It is not drawn here; open it on github.com to read it.",
                            "html not drawn"
                        }
                    }
                    {super::markdown::render_body(st, Path::new(ROOT), &doc)}
                }
            }
            div { class: "prpeekfoot",
                button {
                    class: "primarybtn",
                    disabled: opening,
                    onclick: move |_| board.open(st, number),
                    if opening {
                        "Opening…"
                    } else if current {
                        "Back to it"
                    } else {
                        "Open"
                    }
                }
                button {
                    class: "iconbtn",
                    title: "Open #{number} on github.com",
                    onclick: move |_| open_browser(&url),
                    GithubMark {}
                }
                span { class: "spacer" }
                span { class: "prkeys",
                    span { class: "spkey", "↑" }
                    span { class: "spkey", "↓" }
                    span { "move" }
                    span { class: "spkey", "⏎" }
                    span { "open" }
                    span { class: "spkey", "/" }
                    span { "filter" }
                    span { class: "spkey", "esc" }
                    span { "close" }
                }
            }
        }
    }
}

/// The way out of the pull requests and into the repository they are against —
/// the row at the foot of the list, and the one that is already ticked when the
/// repository is what is open.
#[component]
fn BrowseFoot(repo: RepoRef) -> Element {
    let st = use_context::<St>();
    // The repository row is the one being read only when the repository itself
    // is what is open — a commit of it is somewhere else.
    let current = st
        .workspace
        .read()
        .repo()
        .is_some_and(|view| view.repo == repo);
    rsx! {
        div {
            class: if current { "prboardfoot on" } else { "prboardfoot" },
            title: if current { "Already open" } else { "Read {repo} at its default branch, with no pull request" },
            // Root scope: loading replaces the board this row is at the foot of.
            onclick: move |_| {
                if current {
                    shut(&st);
                } else {
                    spawn_forever(browse_repo(st, repo.clone()));
                }
            },
            span { class: "prboardfoot-label", "the repository itself" }
            if current {
                span { class: "prhere", "reading" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::github::Label;

    fn pr(number: u64, title: &str, author: &str) -> Rc<PrSummary> {
        Rc::new(PrSummary {
            number,
            title: title.to_string(),
            author: author.to_string(),
            avatar: String::new(),
            draft: false,
            state: "open".to_string(),
            merged: false,
            created_at: String::new(),
            updated_at: String::new(),
            html_url: String::new(),
            head_ref: format!("feat/{number}"),
            head_repo: None,
            base_ref: "main".to_string(),
            body: String::new(),
            labels: Vec::new(),
            reviewers: Vec::new(),
        })
    }

    /// The pane always has something to read out, and what it reads out is the
    /// row somebody chose for as long as that row is there to be chosen.
    #[test]
    fn the_pane_reads_out_the_row_picked_then_the_one_open_then_the_first() {
        let rows = vec![pr(9, "a", "x"), pr(7, "b", "x"), pr(3, "c", "x")];
        assert_eq!(
            peek_index(&rows, Some(3), Some(7)),
            Some(2),
            "the one picked"
        );
        assert_eq!(peek_index(&rows, None, Some(7)), Some(1), "the one open");
        assert_eq!(
            peek_index(&rows, None, None),
            Some(0),
            "the top of the list"
        );
        // Picked, and then filtered out from under the pick.
        assert_eq!(peek_index(&rows, Some(99), Some(7)), Some(1));
        assert_eq!(peek_index(&rows, Some(99), Some(98)), Some(0));
        assert_eq!(
            peek_index(&[], Some(3), Some(7)),
            None,
            "nothing to read out"
        );
        // With something typed, the top of what it found outranks what is open.
        assert_eq!(peek_index(&rows, None, fallback(Some(7), "fix")), Some(0));
        assert_eq!(peek_index(&rows, None, fallback(Some(7), "  ")), Some(1));
    }

    #[test]
    fn every_word_of_the_filter_has_to_be_somewhere_on_the_row() {
        let mut fix = (*pr(482, "Fix the wide-character crash", "ada")).clone();
        fix.labels = vec![Label {
            name: "Bug".to_string(),
            color: String::new(),
        }];
        fix.head_repo = Some(RepoRef {
            owner: "adafork".to_string(),
            name: "r".to_string(),
        });

        assert!(matches(&fix, ""), "no filter is no filter");
        assert!(matches(&fix, "   "));
        assert!(matches(&fix, "crash"));
        assert!(matches(&fix, "#482"));
        assert!(matches(&fix, "ada bug"), "any order, any part of the row");
        assert!(
            matches(&fix, "adafork:feat"),
            "a fork, as the row writes it"
        );
        assert!(matches(&fix, "main open"));
        assert!(!matches(&fix, "ada docs"), "every word, not any word");
        // A draft is found by being one.
        fix.draft = true;
        assert!(matches(&fix, "draft") && !matches(&fix, "open"));
    }
}
