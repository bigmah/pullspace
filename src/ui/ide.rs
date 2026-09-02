//! The editor-shaped half of the app: search, definitions, references.
//!
//! All four actions are the same shape underneath — read every text file of
//! what is open, match something against it, put the results in the panel — and
//! the shape is worth naming, because the reading is the expensive part.
//! [`scan::walk`] does it once and keeps what it decoded, so the first search of
//! a session pays for the disk and the rest are string comparisons over memory.
//!
//! The symbol index is the same walk, run once in the background as soon as the
//! clone finishes. Go to Definition is therefore instant and Find References is
//! not, which is the right way round: a definition is one lookup in a table and
//! a reference could be anywhere.
//!
//! Nothing here can be sure it is still wanted by the time it finishes. A scan
//! walks thousands of files while somebody carries on clicking, so every task
//! carries the two counters it started under — the workspace's and the scan's —
//! and drops its results on the floor if either has moved on.

use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use dioxus::prelude::*;

use crate::backend::github::TreeEntry;
use crate::backend::highlight::{Span, highlight};
use crate::backend::scan::{self, Flow, Walked};
use crate::backend::search::{self, Hit, MAX_HITS, Matcher, Options};
use crate::backend::symbols::{self, Symbol};

use crate::backend::prefs::Prefs;

use super::app::St;
use super::compat;
use super::full;
use super::palette::{self, Pick};
use super::spaces;

/// How long to wait for the clone before indexing anyway. It is a backstop for
/// a clone that never reports rather than a deadline — the usual path is the
/// clone finishing and this waking up on the next poll.
const CLONE_WAIT: Duration = Duration::from_secs(90);
const POLL: Duration = Duration::from_millis(250);

/// Lines of context a peek shows above and below the definition itself.
const PEEK_BEFORE: usize = 1;
const PEEK_AFTER: usize = 14;

/// What the panel across the bottom of the code is showing.
#[derive(Clone, PartialEq)]
pub enum Panel {
    Hidden,
    /// A walk in progress. Named, because a slow search finishing after a
    /// second one started must not overwrite it — the title is half of how
    /// that is spotted.
    Working {
        title: String,
        done: usize,
        total: usize,
    },
    Results {
        title: String,
        hits: Vec<Hit>,
        /// Files the walk could not read, if any.
        note: Option<String>,
    },
    /// More than one thing is called this. Which one was meant is a question
    /// this index cannot answer, so it asks.
    Defs {
        name: String,
        syms: Vec<Symbol>,
    },
    /// A definition, read where it stands, without leaving the file.
    Peek {
        name: String,
        syms: Vec<Symbol>,
        /// Which of `syms` is on show.
        at: usize,
        body: Peek,
    },
    /// Asked for a definition before there was an index to look in.
    NotIndexed(String),
}

impl Panel {
    pub fn showing(&self) -> bool {
        !matches!(self, Panel::Hidden)
    }
}

#[derive(Clone, PartialEq)]
pub enum Peek {
    Loading,
    /// The file is named by the tree but is not in the local store.
    Missing,
    Ready {
        /// Line number of the first line shown, 1-based.
        start: usize,
        lines: Vec<Vec<Span>>,
    },
}

/// The symbol index for what is open.
#[derive(Clone)]
pub enum Index {
    /// Nothing to index: nothing open, or nowhere to read it from.
    Off,
    Building {
        done: usize,
        total: usize,
    },
    Ready {
        /// Shared rather than cloned — this is tens of thousands of entries,
        /// and every definition lookup would otherwise copy the lot.
        syms: Rc<Vec<Symbol>>,
    },
}

impl Index {
    /// The top bar's readout. `None` while there is nothing worth saying.
    pub fn label(&self) -> Option<String> {
        match self {
            Index::Off => None,
            Index::Building { done, total } => Some(format!("indexing {done}/{total}")),
            Index::Ready { syms } => Some(format!("{} symbols", syms.len())),
        }
    }
}

// ------------------------------------------------------------------ scanning

/// The files of the commit on show. Empty when nothing is open, or when
/// GitHub would not give up the tree.
fn head_files(st: &St) -> Vec<TreeEntry> {
    st.workspace
        .peek()
        .trees()
        .map(|(head, _)| head.files)
        .unwrap_or_default()
}

/// Why there is nothing to search, when there is nothing to search.
///
/// The search box wears this as its placeholder. A control that does nothing is
/// worse than one that is missing; a control that says why is neither.
///
/// It reads the workspace rather than peeking at it, which matters more than it
/// looks: the search box is drawn from this, and a box that does not subscribe
/// to what disables it is a box that stays disabled after a pull request opens.
pub fn why_not(st: &St) -> Option<&'static str> {
    let ws = st.workspace.read();
    if !ws.is_open() {
        return Some("View a repository or a pull request to search it");
    }
    (!ws.has_tree())
        .then_some("GitHub would not serve this repository's tree, so there is nothing to search")
}

/// Whether the task that started under these two counters is still the one
/// anybody is waiting for.
fn current(st: &St, mine: u32, seq: u32) -> bool {
    st.generation() == mine && *st.scan_seq.peek() == seq
}

/// Claim the panel: anything already walking for it stops writing to it.
///
/// Everything that puts something in the panel calls this, not only the walks.
/// A definition list is just as easily clobbered by a search that started
/// before it and finished after it.
fn claim(st: &St) -> u32 {
    let mut seq = st.scan_seq;
    let next = *seq.peek() + 1;
    seq.set(next);
    next
}

/// Put something in the panel, and stop whatever was walking for it.
fn show(st: &St, what: Panel) {
    claim(st);
    let mut panel = st.panel;
    panel.set(what);
}

/// Close the panel. What `✕` and Escape both call — and, like everything else
/// here, it takes the panel away from any walk still filling it.
/// Say that there is nothing picked out to look up — which is a better answer
/// than a key, or a command, that silently does nothing.
pub fn show_not_indexed(st: &St) {
    show(st, Panel::NotIndexed(String::new()));
}

pub fn close(st: St) {
    show(&st, Panel::Hidden);
}

/// Walk what is open, collecting matches, and put them in the panel.
async fn run(st: St, title: String, matcher: Matcher, mine: u32, seq: u32) {
    let files = head_files(&st);
    let mut hits: Vec<Hit> = Vec::new();
    let mut panel = st.panel;

    let walked = scan::walk(
        &files,
        |done, total| {
            if current(&st, mine, seq) {
                panel.set(Panel::Working {
                    title: title.clone(),
                    done,
                    total,
                });
            }
        },
        |path, text| {
            if !matcher.scan(path, text, &mut hits) {
                return Flow::Stop;
            }
            // Whatever else has happened, this scan is no longer the one the
            // panel belongs to — so there is no point reading the rest of the
            // repository for it.
            if current(&st, mine, seq) {
                Flow::Go
            } else {
                Flow::Stop
            }
        },
    )
    .await;

    if !current(&st, mine, seq) {
        return;
    }
    panel.set(Panel::Results {
        title: format!("{title} — {}", counted(&hits, &walked)),
        hits,
        note: walked.shortfall(),
    });
}

/// "412 hits in 38 files", and whether that was all of them.
fn counted(hits: &[Hit], walked: &Walked) -> String {
    if hits.is_empty() {
        return "no matches".to_string();
    }
    let mut files = 0;
    let mut last: Option<&Path> = None;
    for hit in hits {
        if last != Some(hit.path.as_path()) {
            files += 1;
            last = Some(hit.path.as_path());
        }
    }
    let capped = if hits.len() >= MAX_HITS || walked.stopped {
        " (first "
    } else {
        ""
    };
    format!(
        "{}{} line{} in {} file{}{}",
        capped,
        hits.len(),
        if hits.len() == 1 { "" } else { "s" },
        files,
        if files == 1 { "" } else { "s" },
        if capped.is_empty() { "" } else { ")" },
    )
}

/// Run the search box's contents over the repository.
pub fn search(st: St) {
    let query = st.search_text.peek().trim().to_string();
    let mut error = st.search_error;
    if query.is_empty() || why_not(&st).is_some() {
        return;
    }
    let opts = *st.search_opts.peek();
    let matcher = match search::compile(&query, opts) {
        Ok(m) => m,
        Err(e) => return error.set(Some(e)),
    };
    error.set(None);

    let title = format!("SEARCH  “{query}”");
    start(st, title, matcher);
}

/// Every whole-word use of an identifier, anywhere in the repository.
pub fn find_refs(st: St, name: &str) {
    if why_not(&st).is_some() {
        return;
    }
    let Ok(matcher) = search::references_to(name) else {
        return;
    };
    start(st, format!("REFERENCES  {name}"), matcher);
}

fn start(st: St, title: String, matcher: Matcher) {
    let mine = st.generation();
    let seq = claim(&st);
    let mut panel = st.panel;
    panel.set(Panel::Working {
        title: title.clone(),
        done: 0,
        total: 0,
    });
    // Root scope: the panel outlives whichever button or key started this, and
    // a walk takes long enough for that to matter.
    spawn_forever(run(st, title, matcher, mine, seq));
}

// --------------------------------------------------------------- definitions

/// Every definition of `name`, nearest to what is open first — or `None` when
/// there is no index yet to ask.
fn definitions(st: &St, name: &str) -> Option<Vec<Symbol>> {
    let near = st.open.peek().clone();
    match &*st.index.peek() {
        Index::Ready { syms } => Some(symbols::find_definitions(syms, name, near.as_deref())),
        _ => None,
    }
}

/// Go to the definition — or, when the index knows several things by that name,
/// offer them rather than picking one.
pub fn goto_def(st: St, name: &str) {
    let Some(found) = definitions(&st, name) else {
        return show(&st, Panel::NotIndexed(name.to_string()));
    };
    match found.len() {
        // A single answer is not a result to present, it is somewhere to be.
        1 => {
            let sym = &found[0];
            st.open_at(sym.path.to_path_buf(), sym.line);
            show(&st, Panel::Hidden);
        }
        _ => show(
            &st,
            Panel::Defs {
                name: name.to_string(),
                syms: found,
            },
        ),
    }
}

/// Show the definition without going to it — the whole point being that you
/// keep your place.
pub fn peek_def(st: St, name: &str) {
    let Some(found) = definitions(&st, name) else {
        return show(&st, Panel::NotIndexed(name.to_string()));
    };
    if found.is_empty() {
        return show(
            &st,
            Panel::Defs {
                name: name.to_string(),
                syms: found,
            },
        );
    }
    show(
        &st,
        Panel::Peek {
            name: name.to_string(),
            syms: found,
            at: 0,
            body: Peek::Loading,
        },
    );
    spawn_forever(load_peek(st, 0));
}

/// Step through the definitions a peek is showing, when there is more than one.
pub fn peek_step(st: St, delta: isize) {
    let mut panel = st.panel;
    let next = {
        let Panel::Peek { syms, at, .. } = &*panel.peek() else {
            return;
        };
        // Wrapping: with two definitions, "next" and "previous" are the same
        // move, and a disabled arrow at either end is a worse answer.
        let len = syms.len() as isize;
        ((*at as isize + delta).rem_euclid(len)) as usize
    };
    if let Panel::Peek { at, body, .. } = &mut *panel.write() {
        *at = next;
        *body = Peek::Loading;
    }
    spawn_forever(load_peek(st, next));
}

/// Read the lines around one definition and colour them.
async fn load_peek(st: St, at: usize) {
    let target = match &*st.panel.peek() {
        Panel::Peek { syms, .. } => syms.get(at).cloned(),
        _ => None,
    };
    let Some(sym) = target else { return };
    let Some(head) = st.workspace.peek().trees().map(|(head, _)| head) else {
        return;
    };

    let body = match scan::text_at(&head, &sym.path).await {
        Some(text) => {
            let start = sym.line.saturating_sub(PEEK_BEFORE).max(1);
            let window: Vec<&str> = text
                .lines()
                .skip(start - 1)
                .take(PEEK_BEFORE + 1 + PEEK_AFTER)
                .collect();
            // Highlighted as a fragment, which is what it is: a peek opening
            // halfway through a string literal colours the rest of itself
            // oddly, and reading the whole file to avoid that is not worth
            // doing for sixteen lines.
            Peek::Ready {
                start,
                lines: highlight(&sym.path, &window.join("\n")),
            }
        }
        None => Peek::Missing,
    };

    // Still the same peek, on the same definition? A second click while this
    // was reading means these lines belong to nothing on screen.
    let mut panel = st.panel;
    let stale = !matches!(&*panel.peek(), Panel::Peek { at: now, syms, .. }
        if *now == at && syms.get(at).map(|s| &s.path) == Some(&sym.path));
    if stale {
        return;
    }
    if let Panel::Peek { body: slot, .. } = &mut *panel.write() {
        *slot = body;
    }
}

// --------------------------------------------------------------- the index

/// Build the symbol index for whatever has just opened.
///
/// It waits for the clone first. Both go through the same filesystem, and this
/// one is reading the very files that one is still writing — starting early
/// would mean indexing a repository that is half here and then being wrong
/// about it for as long as it stays open.
pub async fn build_index(st: St, files: Vec<TreeEntry>) {
    let mine = st.generation();
    let mut index = st.index;
    if files.is_empty() {
        index.set(Index::Off);
        return;
    }
    index.set(Index::Building { done: 0, total: 0 });

    let mut waited = Duration::ZERO;
    while waited < CLONE_WAIT {
        if st.generation() != mine {
            return;
        }
        if st.cloning.peek().is_some_and(|at| at.finished()) {
            break;
        }
        compat::sleep(POLL).await;
        waited += POLL;
    }
    if st.generation() != mine {
        return;
    }

    let mut syms = Vec::new();
    scan::walk(
        &files,
        |done, total| {
            if st.generation() == mine {
                index.set(Index::Building { done, total });
            }
        },
        // Every text file, not only the ones with patterns to match: the walk
        // is what fills the text cache, and the first search after it should
        // not have to read the disk again for the files this skipped.
        |path, text| {
            symbols::scan_file(path, text, &mut syms);
            if st.generation() == mine {
                Flow::Go
            } else {
                Flow::Stop
            }
        },
    )
    .await;

    if st.generation() != mine {
        return;
    }
    index.set(Index::Ready {
        syms: Rc::new(symbols::sorted(syms)),
    });
}

// ------------------------------------------------------------------ the DOM

/// Take whatever the reader just double-clicked as the selected identifier.
///
/// Double-click, rather than a click handler on every token, because a browser
/// already knows where a word starts and ends — and wrapping thirty thousand
/// spans around the tokens of a long file to find out costs more than every
/// other thing the viewer does put together.
pub fn select_word(st: St) {
    spawn(async move {
        let mut eval = document::eval("dioxus.send(String(window.getSelection() ?? ''));");
        let Ok(word) = eval.recv::<String>().await else {
            return;
        };
        let word = word.trim();
        if search::is_identifier(word) {
            let mut sel = st.selected;
            sel.set(Some(word.to_string()));
        }
    });
}

/// Put the cursor in the search box, with whatever is in it selected — so the
/// shortcut is "search for something else" and not "search again".
pub fn focus_search() {
    document::eval("var e=document.querySelector('.searchbox'); if(e){e.focus();e.select();}");
}

/// Put the cursor in the find bar, with whatever is in it selected.
///
/// Only ever finds anything when the bar is already up: on the way up it is
/// the box's own `onmounted` that takes the focus, since there is nothing here
/// to focus yet. So this is what makes a second ⌘F mean "find something else"
/// rather than nothing at all.
pub fn focus_find() {
    document::eval("var e=document.querySelector('.findinbox'); if(e){e.focus();e.select();}");
}

/// Put the cursor in the explorer's filter box.
pub fn focus_filter() {
    document::eval("var e=document.querySelector('.findbox'); if(e){e.focus();e.select();}");
}

// ----------------------------------------------------------------- keyboard

/// Installed once, on the window, because the keys have to work wherever the
/// focus happens to be — and for most of this app's life that is the document
/// body, which is above every element a dioxus handler can be attached to.
const KEYS: &str = r#"
(function () {
  // A reload of this page's script should not leave the last listener behind.
  if (window.__pullspace_keys) window.__pullspace_keys();
  var onkey = function (e) {
    var mod = e.metaKey || e.ctrlKey;
    var key = (e.key || '').toLowerCase();
    var el = e.target || {};
    var tag = el.tagName || '';
    var typing = tag === 'INPUT' || tag === 'TEXTAREA' || el.isContentEditable;
    var what = null;
    // Fullscreen first, and both of its keys before find: F11 is what every
    // browser but Safari on a Mac calls this, ⌃⌘F is what a Mac calls it, and
    // the second arrives here as ⌘F with one more modifier held down.
    if (key === 'f11' || (e.ctrlKey && e.metaKey && key === 'f')) what = 'full';
    // Shift before its bare form every time: with Shift held the key is still
    // the same letter, so the wider match would swallow the narrower one.
    else if (mod && e.shiftKey && key === 'f') what = 'search';
    else if (mod && key === 'f') what = 'find';
    else if (mod && e.shiftKey && key === 'p') what = 'commands';
    else if (mod && key === 'p') what = 'gotofile';
    else if (mod && e.shiftKey && (key === 'o' || e.code === 'KeyO')) what = 'gotosym';
    else if (mod && e.shiftKey && (key === 'e' || e.code === 'KeyE')) what = 'filter';
    // Control and not Command, on every platform — which on Windows is the
    // same key as `mod` and is what the editor this is shaped like uses there
    // too.
    else if (e.ctrlKey && !e.metaKey && (key === 'g' || e.code === 'KeyG')) what = 'gotoline';
    else if (mod && key === ',') what = 'prefs';
    else if (key === 'f12') what = e.shiftKey ? 'refs' : 'def';
    else if (key === 'f7') what = e.shiftKey ? 'prevchange' : 'nextchange';
    else if (key === 'f3') what = e.shiftKey ? 'prevfind' : 'nextfind';
    else if (key === 'escape') what = 'escape';
    // The spaces go first, since every one of them is one of the keys below
    // with Shift held: Option and an arrow moves inside a space, Option, Shift
    // and an arrow moves between them.
    else if (e.altKey && e.shiftKey && key === 'arrowleft') what = 'prevspace';
    else if (e.altKey && e.shiftKey && key === 'arrowright') what = 'nextspace';
    else if (e.altKey && e.shiftKey && (key === 't' || e.code === 'KeyT')) what = 'newspace';
    else if (e.altKey && e.shiftKey && (key === 'w' || e.code === 'KeyW')) what = 'closespace';
    else if ((e.altKey && key === 'arrowleft') || (mod && key === '[')) what = 'back';
    else if ((e.altKey && key === 'arrowright') || (mod && key === ']')) what = 'forward';
    else if (e.altKey && key === 'arrowdown') what = 'nextfile';
    else if (e.altKey && key === 'arrowup') what = 'prevfile';
    // Not the browser's own close, which no page is allowed to take. By code
    // as well as by key: Option+W on a Mac arrives as '\u2211'.
    else if (e.altKey && (key === 'w' || e.code === 'KeyW')) what = 'closetab';
    // And its opposite. ⌘⇧T is what an editor calls this and what a browser
    // will not hand over, so it moves one modifier out of the way like the
    // rest of them — beside ⌥W, which is the key it undoes.
    else if (e.altKey && (key === 't' || e.code === 'KeyT')) what = 'reopen';
    else if (e.altKey && (key === 'z' || e.code === 'KeyZ')) what = 'wrap';
    if (!what) return;
    // Escape out of a text box is about the text box. Everything else it
    // could mean is one more press away.
    if (what === 'escape' && typing) { el.blur(); e.preventDefault(); return; }
    // And an arrow in one is about the caret, not about the review — nor is
    // a letter in one about the file behind it, and three of these are
    // letters: on a Mac ⌥W, ⌥T and ⌥Z type '∑', '†' and 'Ω'. The spaces are
    // not on this list: stepping to another review is worth doing from inside
    // the search box, and Option+Shift is not a chord anybody types text with.
    if (typing && (what === 'nextfile' || what === 'prevfile' || what === 'closetab'
                   || what === 'reopen' || what === 'wrap')) return;
    e.preventDefault();
    dioxus.send(what);
  };
  window.addEventListener('keydown', onkey);
  window.__pullspace_keys = function () {
    window.removeEventListener('keydown', onkey);
  };
})();
"#;

/// Listen for the shortcuts, for as long as the app is up.
pub async fn keys(st: St) {
    let mut eval = document::eval(KEYS);
    while let Ok(what) = eval.recv::<String>().await {
        let selected = st.selected.peek().clone();
        match (what.as_str(), selected) {
            ("full", _) => full::toggle(),
            // The two finds. In this file, and in every file — which is the
            // arrangement every editor has and the one this app did not: ⌘F
            // used to be the box in the top bar, and there was no way at all
            // to ask a question about the file on screen.
            ("find", _) => {
                st.toggle_find(true);
                focus_find();
            }
            ("search", _) => focus_search(),
            ("filter", _) => focus_filter(),
            ("gotofile", _) => palette::open_with(&st, Pick::File),
            ("commands", _) => palette::open_with(&st, Pick::Command),
            ("gotosym", _) => palette::open_with(&st, Pick::Symbol),
            ("gotoline", _) => palette::open_with(&st, Pick::Line),
            ("nextchange", _) => st.step_change(true),
            ("prevchange", _) => st.step_change(false),
            ("nextfind", _) => st.step_find(true),
            ("prevfind", _) => st.step_find(false),
            ("reopen", _) => st.reopen_tab(),
            ("wrap", _) => {
                let now = *st.prefs.peek();
                st.set_prefs(Prefs {
                    wrap: !now.wrap,
                    ..now
                });
            }
            ("prefs", _) => {
                let mut open = st.prefs_open;
                let showing = *open.peek();
                open.set(!showing);
            }
            ("def", Some(name)) => goto_def(st, &name),
            ("refs", Some(name)) => find_refs(st, &name),
            // Nothing picked out to look up. Saying so beats a key that
            // silently does nothing.
            ("def" | "refs", None) => show_not_indexed(&st),
            ("escape", _) => escape(st),
            ("back", _) => st.go_back(),
            ("forward", _) => st.go_forward(),
            // Along the review rather than back through it: the changed files,
            // in the order the explorer lists them.
            ("nextfile", _) => st.step_file(true),
            ("prevfile", _) => st.step_file(false),
            // Put down the file being read. What is beside it in the strip
            // takes its place.
            ("closetab", _) => st.close_open_tab(),
            // And the four that are about the space rather than about what is
            // inside it — see `super::spaces`.
            ("nextspace", _) => spaces::step(&st, true),
            ("prevspace", _) => spaces::step(&st, false),
            ("newspace", _) => spaces::open_new(&st),
            ("closespace", _) => {
                // Bound first, and not passed straight in as
                // `close(&st, *st.space.peek())`: the guard that `peek`
                // returns is a temporary of the whole statement, so written
                // that way it is still holding the signal while `close` runs
                // — and `close` moves to another space, which writes it.
                let here = *st.space.peek();
                spaces::close(&st, here);
            }
            _ => {}
        }
    }
}

/// Escape closes the panel if one is up, and otherwise puts down whatever
/// identifier is selected — the reverse of the order they were opened in.
///
/// Fullscreen is deliberately not on this list. A browser holding the screen
/// leaves it on Escape itself, and does so without the page ever seeing the
/// key — so taking the panel down would cost the reader nothing but a second
/// press, while claiming the key here would be claiming one that never arrives.
fn escape(st: St) {
    // Outermost first, which is the order they were opened in read backwards.
    // Only ever reached with the focus outside a text box — Escape inside one
    // is about that box and never arrives here, see `KEYS`.
    if *st.picker.peek() {
        return palette::shut(&st);
    }
    if *st.find_open.peek() {
        return st.toggle_find(false);
    }
    if st.panel.peek().showing() {
        return close(st);
    }
    let mut sel = st.selected;
    sel.set(None);
}

/// One toggle: what the button says, what it says on hover, and the field it
/// flips — which is what lets the three be drawn by one loop rather than three
/// near-identical blocks.
pub type Toggle = (&'static str, &'static str, fn(&mut Options) -> &mut bool);

/// The three toggles, as the buttons that set them.
pub const TOGGLES: [Toggle; 3] = [
    ("Aa", "Match case", |o| &mut o.case),
    ("ab", "Whole word", |o| &mut o.word),
    (".*", "Regular expression", |o| &mut o.regex),
];
