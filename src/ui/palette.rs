//! The picker: one box, five questions.
//!
//! `⌘P` opens a file by typing three letters of its name, and that is the one
//! people reach for. The other four are the same box with a character in front
//! of what is typed — `>` for a command, `@` for a definition in the file being
//! read, `#` for one anywhere in the repository, `:` for a line number — which
//! is github.dev's arrangement, and worth copying exactly: the prefixes are
//! muscle memory for anybody who has used the editor this app is shaped like,
//! and one overlay with four modes is one overlay to get right.
//!
//! The command list is the other half of the point. Every keyboard shortcut in
//! this app is written down in the README and nowhere else, which means it is
//! written down nowhere anybody is looking. Here they are next to the thing
//! they do, in a list that can be searched for a word — so the way to find out
//! that Option and an arrow steps through the changed files is to open this and
//! type "changed".

use std::path::PathBuf;
use std::rc::Rc;

use dioxus::prelude::*;

use crate::backend::symbols::Symbol;
use crate::backend::{clip, fuzzy, route};

use super::app::{St, ViewMode};
use super::ide::Index;
use super::{filetree, full, ide, spaces};

/// How many rows the list draws. Nobody reads past the first screen of a
/// picker — they type another letter — and the count of what was left out is
/// shown, so the list is never quietly short.
const ROWS: usize = 50;

/// What the picker is being asked, which is decided by the first character of
/// what has been typed rather than by how it was opened. Typing over the
/// prefix changes the question, which is what makes one box four.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Pick {
    File,
    Command,
    Symbol,
    Repo,
    Line,
}

impl Pick {
    /// The character that asks for this one.
    pub const fn prefix(self) -> &'static str {
        match self {
            Pick::File => "",
            Pick::Command => ">",
            Pick::Symbol => "@",
            Pick::Repo => "#",
            Pick::Line => ":",
        }
    }

    /// Read the question off the front of what has been typed, and hand back
    /// the rest of it.
    pub fn of(text: &str) -> (Pick, &str) {
        match text.as_bytes().first() {
            Some(b'>') => (Pick::Command, &text[1..]),
            Some(b'@') => (Pick::Symbol, &text[1..]),
            Some(b'#') => (Pick::Repo, &text[1..]),
            Some(b':') => (Pick::Line, &text[1..]),
            _ => (Pick::File, text),
        }
    }

    fn placeholder(self) -> &'static str {
        match self {
            Pick::File => {
                "Go to file — or > for a command, @ for a definition here, # for one anywhere, : for a line"
            }
            Pick::Command => "Type the name of a command",
            Pick::Symbol => "Go to a definition in this file",
            Pick::Repo => "Go to a definition anywhere in this repository",
            Pick::Line => "Go to line",
        }
    }
}

/// One thing the picker can do that is not going somewhere.
///
/// The keys beside them are the ones already installed on the window — see
/// [`super::ide`] — written here so that the list is where somebody finds out
/// about them.
pub struct Cmd {
    pub id: &'static str,
    pub name: &'static str,
    pub keys: &'static str,
}

const fn cmd(id: &'static str, name: &'static str, keys: &'static str) -> Cmd {
    Cmd { id, name, keys }
}

pub const CMDS: &[Cmd] = &[
    cmd("gotofile", "Go to File…", "⌘P"),
    cmd("gotosym", "Go to Symbol in File…", "⌘⇧O"),
    cmd("gotorepo", "Go to Symbol in Repository…", "⌘T"),
    cmd("gotoline", "Go to Line…", "⌃G"),
    cmd("find", "Find in File", "⌘F"),
    cmd("search", "Search in Repository", "⌘⇧F"),
    cmd("searchnames", "Find Files by Name in Repository", ""),
    cmd("filter", "Filter Files in Explorer", "⌘⇧E"),
    cmd("nextchange", "Go to Next Change", "F7"),
    cmd("prevchange", "Go to Previous Change", "⇧F7"),
    cmd("nextfile", "Go to Next Changed File", "⌥↓"),
    cmd("prevfile", "Go to Previous Changed File", "⌥↑"),
    cmd("def", "Go to Definition", "F12"),
    cmd("refs", "Find References", "⇧F12"),
    cmd("viewsource", "View: Source", ""),
    cmd("viewinline", "View: Inline Diff", ""),
    cmd("viewsplit", "View: Split Diff", ""),
    cmd("viewpreview", "View: Preview", ""),
    cmd("wrap", "Toggle Word Wrap", "⌥Z"),
    cmd("ws", "Toggle Ignore Whitespace in Diffs", ""),
    cmd("changedonly", "Toggle Changed Files Only", ""),
    cmd("collapse", "Collapse Folders in Explorer", ""),
    cmd("reveal", "Reveal Open File in Explorer", ""),
    cmd("outline", "Toggle Outline", ""),
    cmd("viewed", "Toggle Viewed on This File", ""),
    cmd("resetgaps", "Contract Every Opened Stretch", ""),
    cmd("copypath", "Copy Path of This File", ""),
    cmd("copylink", "Copy Link to This Line", ""),
    cmd("closefile", "Close File", "⌥W"),
    cmd("reopen", "Reopen Closed File", "⌥T"),
    cmd("back", "Go Back", "⌘["),
    cmd("forward", "Go Forward", "⌘]"),
    cmd("panel", "Close the Panel Below the Code", "Esc"),
    cmd("conv", "Toggle the Conversation Pane", ""),
    cmd("newspace", "New Space", "⌥⇧T"),
    cmd("closespace", "Close Space", "⌥⇧W"),
    cmd("nextspace", "Next Space", "⌥⇧→"),
    cmd("prevspace", "Previous Space", "⌥⇧←"),
    cmd("full", "Toggle Full Screen", "F11"),
    cmd("prefs", "Appearance…", "⌘,"),
    cmd("close", "Close What Is Open", ""),
];

/// Do one of them.
///
/// Called straight out of the click or the Enter that asked for it, and not
/// from a task started by one: the clipboard is granted on the gesture that is
/// still in progress, and an `await` in between spends it.
pub fn run(st: St, id: &str) {
    let open = st.open.peek().clone();
    match id {
        "gotofile" => return open_with(&st, Pick::File),
        "gotosym" => return open_with(&st, Pick::Symbol),
        "gotorepo" => return open_with(&st, Pick::Repo),
        "gotoline" => return open_with(&st, Pick::Line),
        "find" => {
            st.toggle_find(true);
            ide::focus_find();
        }
        "search" => ide::focus_search(),
        // The same box, asked the other question. Set first and focused after,
        // so the placeholder already says which question it is.
        "searchnames" => {
            let mut names = st.search_files;
            names.set(true);
            ide::focus_search();
        }
        "filter" => ide::focus_filter(),
        "nextchange" => st.step_change(true),
        "prevchange" => st.step_change(false),
        "nextfile" => st.step_file(true),
        "prevfile" => st.step_file(false),
        "def" | "refs" => {
            // Only ever something to do with a word picked out of the code, and
            // the panel says so itself when there is not one.
            match st.selected.peek().clone() {
                Some(name) if id == "def" => ide::goto_def(st, &name),
                Some(name) => ide::find_refs(st, &name),
                None => ide::show_not_indexed(&st),
            }
        }
        "viewsource" => st.view_mode.clone().set(ViewMode::Source),
        "viewinline" => st.view_mode.clone().set(ViewMode::Inline),
        "viewsplit" => st.view_mode.clone().set(ViewMode::Split),
        "viewpreview" => st.view_mode.clone().set(ViewMode::Preview),
        "wrap" => {
            let now = *st.prefs.peek();
            st.set_prefs(crate::backend::prefs::Prefs {
                wrap: !now.wrap,
                ..now
            });
        }
        "ws" => {
            let now = *st.prefs.peek();
            st.set_prefs(crate::backend::prefs::Prefs {
                ignore_ws: !now.ignore_ws,
                ..now
            });
        }
        "changedonly" => {
            let mut only = st.changes_only;
            let now = *only.peek();
            only.set(!now);
            st.reset_tree_folds();
        }
        "collapse" => st.collapse_tree(),
        "reveal" => filetree::reveal(&st),
        "outline" => {
            let mut open = st.outline_open;
            let now = *open.peek();
            open.set(!now);
        }
        "viewed" => {
            if let Some(rel) = open.as_deref() {
                st.toggle_viewed(rel);
            }
        }
        "resetgaps" => st.contract_all_gaps(),
        "copypath" => {
            if let Some(rel) = open.as_deref() {
                clip::copy(&rel.display().to_string());
            }
        }
        // The address bar is already showing the answer — see the Link button
        // in `super::viewer`, which copies the same thing for the same reason.
        "copylink" => {
            if let Some(url) = route::href() {
                clip::copy(&url);
            }
        }
        "closefile" => st.close_open_tab(),
        "reopen" => st.reopen_tab(),
        "back" => st.go_back(),
        "forward" => st.go_forward(),
        "panel" => ide::close(st),
        "conv" => {
            let mut conv = st.conv_open;
            let now = *conv.peek();
            conv.set(!now);
        }
        "newspace" => spaces::open_new(&st),
        // Bound first — see the same call in `super::ide`, and why.
        "closespace" => {
            let here = *st.space.peek();
            spaces::close(&st, here);
        }
        "nextspace" => spaces::step(&st, true),
        "prevspace" => spaces::step(&st, false),
        "full" => full::toggle(),
        "prefs" => {
            let mut panel = st.prefs_open;
            let now = *panel.peek();
            panel.set(!now);
        }
        "close" => st.close_workspace(),
        _ => {}
    }
    shut(&st);
}

/// Put the picker up, asking one of the five questions.
pub fn open_with(st: &St, pick: Pick) {
    let mut text = st.picker_text;
    text.set(pick.prefix().to_string());
    let mut at = st.picker_at;
    at.set(0);
    let mut up = st.picker;
    up.set(true);
}

/// And take it down.
pub fn shut(st: &St) {
    let mut up = st.picker;
    if *up.peek() {
        up.set(false);
    }
}

/// What one row of the list does when it is chosen.
#[derive(Clone, PartialEq)]
enum Act {
    Open(PathBuf),
    At(PathBuf, usize),
    Line(usize),
    Run(&'static str),
    /// Nothing to do — an explanation standing where the rows would be.
    Nothing,
}

/// One row as drawn.
#[derive(Clone, PartialEq)]
struct Row {
    label: String,
    /// Which characters of `label` the typing landed on, for the emphasis that
    /// says why this row is in the list.
    hits: Vec<usize>,
    /// The quieter half: a path, a line, the key that does the same thing.
    detail: String,
    /// A word or two saying what kind of thing this is.
    tag: String,
    act: Act,
}

/// A file's name and the directories in front of it, which is how the rows are
/// drawn: the name is what was typed, the path is which one it is.
fn split_path(path: &str) -> (String, String) {
    match path.rfind('/') {
        Some(cut) => (path[cut + 1..].to_string(), path[..cut].to_string()),
        None => (path.to_string(), String::new()),
    }
}

/// `main.rs:120` — the line, taken off the end of a file query.
///
/// github.dev takes it, and it is the difference between two trips through the
/// picker and one when a stack trace is what you are holding.
fn trailing_line(query: &str) -> (&str, Option<usize>) {
    match query.rsplit_once(':') {
        Some((head, tail)) if !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()) => {
            (head, tail.parse().ok())
        }
        _ => (query, None),
    }
}

#[component]
pub fn Picker() -> Element {
    let st = use_context::<St>();
    let all_paths = use_context::<Memo<Rc<Vec<String>>>>();
    let file_syms = use_context::<Memo<Rc<Vec<Symbol>>>>();
    let mut text = st.picker_text;
    let mut at = st.picker_at;

    // Every path in the repository, with the ones already in hand at the front
    // — the files open, then the files the change touches. With nothing typed
    // that ordering *is* the answer, and once something is typed the score
    // decides and this only breaks ties.
    let pool = use_memo(move || {
        let all = all_paths.read();
        let mut out: Vec<String> = Vec::with_capacity(all.len());
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut first = |p: String, out: &mut Vec<String>| {
            if seen.insert(p.clone()) {
                out.push(p);
            }
        };
        for rel in st.recent_paths() {
            first(rel.display().to_string(), &mut out);
        }
        for rel in st.changed_files.read().iter() {
            first(rel.display().to_string(), &mut out);
        }
        for p in all.iter() {
            first(p.clone(), &mut out);
        }
        Rc::new(out)
    });

    let rows = use_memo(move || {
        let typed = text.read().clone();
        let (pick, query) = Pick::of(&typed);
        let query = query.trim();
        match pick {
            Pick::Command => {
                let names: Vec<&str> = CMDS.iter().map(|c| c.name).collect();
                fuzzy::rank(query, &names, ROWS)
                    .into_iter()
                    .map(|(i, m)| Row {
                        label: CMDS[i].name.to_string(),
                        hits: m.hits,
                        detail: CMDS[i].keys.to_string(),
                        tag: String::new(),
                        act: Act::Run(CMDS[i].id),
                    })
                    .collect()
            }
            Pick::Line => {
                let Ok(line) = query.parse::<usize>() else {
                    return vec![Row {
                        label: "Type a line number".to_string(),
                        hits: Vec::new(),
                        detail: String::new(),
                        tag: String::new(),
                        act: Act::Nothing,
                    }];
                };
                vec![Row {
                    label: format!("Go to line {line}"),
                    hits: Vec::new(),
                    detail: String::new(),
                    tag: String::new(),
                    act: Act::Line(line.max(1)),
                }]
            }
            Pick::Symbol | Pick::Repo => {
                let held = file_syms.read();
                let index = st.index.read();
                let repo_syms = match &*index {
                    Index::Ready { syms } => Some(Rc::clone(syms)),
                    _ => None,
                };
                let syms: &[Symbol] = match pick {
                    Pick::Repo => match repo_syms.as_deref() {
                        Some(s) => s,
                        // Nothing to look in yet. Say which, rather than
                        // showing an empty list that looks like "no such
                        // thing".
                        None => {
                            return vec![Row {
                                label: "The definitions are still being read".to_string(),
                                hits: Vec::new(),
                                detail: index.label().unwrap_or_default(),
                                tag: String::new(),
                                act: Act::Nothing,
                            }];
                        }
                    },
                    _ => &held,
                };
                let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
                let ranked = fuzzy::rank(query, &names, ROWS);
                // In a file, with nothing typed, the list is the file's own
                // outline and reads best in the order it is written in; ranked
                // by score once something is typed.
                ranked
                    .into_iter()
                    .map(|(i, m)| {
                        let s = &syms[i];
                        let detail = if pick == Pick::Repo {
                            format!("{}:{}", s.path.display(), s.line)
                        } else {
                            s.line.to_string()
                        };
                        Row {
                            label: s.name.clone(),
                            hits: m.hits,
                            detail,
                            tag: s.kind.to_string(),
                            act: Act::At(s.path.to_path_buf(), s.line),
                        }
                    })
                    .collect()
            }
            Pick::File => {
                let (query, line) = trailing_line(query);
                let held = pool.read();
                fuzzy::rank(query, &held, ROWS)
                    .into_iter()
                    .map(|(i, m)| {
                        let path = &held[i];
                        let (name, dir) = split_path(path);
                        // The offsets came back against the whole path; the
                        // label is only the name, so the ones inside it move
                        // left and the ones above it are not drawn.
                        let cut = path.len() - name.len();
                        let hits = m
                            .hits
                            .iter()
                            .filter(|h| **h >= cut)
                            .map(|h| h - cut)
                            .collect();
                        Row {
                            label: name,
                            hits,
                            detail: dir,
                            tag: String::new(),
                            act: match line {
                                Some(n) => Act::At(PathBuf::from(path), n),
                                None => Act::Open(PathBuf::from(path)),
                            },
                        }
                    })
                    .collect()
            }
        }
    });

    let count = rows.read().len();
    let here = (*at.read()).min(count.saturating_sub(1));
    let (pick, _) = Pick::of(&text.read().clone());
    let placeholder = pick.placeholder();

    let choose = move |row: Row| match row.act {
        Act::Open(path) => {
            st.open_file(path);
            shut(&st);
        }
        Act::At(path, line) => {
            // Somewhere else, or a line of what is already open — which are
            // two different moves, and only one of them should disturb the
            // view the reader has set up.
            if st.open.peek().as_deref() == Some(path.as_path()) {
                st.jump_line(line);
            } else {
                st.open_at(path, line);
            }
            shut(&st);
        }
        Act::Line(line) => {
            st.jump_line(line);
            shut(&st);
        }
        Act::Run(id) => run(st, id),
        Act::Nothing => {}
    };

    rsx! {
        div {
            class: "pickmask",
            // A click anywhere off the box is the other way to mean Escape.
            onclick: move |_| shut(&st),
            div {
                class: "pickbox",
                // The mask is what closes it, and the box is on top of it.
                onclick: move |e| e.stop_propagation(),
                input {
                    class: "pickinput",
                    r#type: "text",
                    placeholder: "{placeholder}",
                    spellcheck: "false",
                    autocomplete: "off",
                    autocapitalize: "off",
                    value: "{text}",
                    onmounted: move |e| async move {
                        let _ = e.set_focus(true).await;
                    },
                    oninput: move |e| {
                        text.set(e.value());
                        // A different list: whatever row four was is not what
                        // row four is now.
                        at.set(0);
                    },
                    onkeydown: move |e| {
                        // What is on screen, rather than what was on screen
                        // when this handler was made — the list moves under it.
                        let held = rows.read();
                        let n = held.len();
                        let here = (*at.peek()).min(n.saturating_sub(1));
                        match e.key() {
                            Key::ArrowDown if n > 0 => {
                                e.prevent_default();
                                at.set((here + 1) % n);
                            }
                            Key::ArrowUp if n > 0 => {
                                e.prevent_default();
                                at.set((here + n - 1) % n);
                            }
                            Key::Escape => {
                                e.prevent_default();
                                shut(&st);
                            }
                            Key::Enter => {
                                e.prevent_default();
                                if let Some(row) = held.get(here).cloned() {
                                    drop(held);
                                    choose(row);
                                }
                            }
                            _ => {}
                        }
                    },
                }
                div { class: "picklist",
                    for (i, row) in rows.read().iter().enumerate() {
                        {
                            let row = row.clone();
                            let chosen = i == here;
                            let cls = if chosen { "pickrow on" } else { "pickrow" };
                            rsx! {
                                div {
                                    // Keyed by position and not by the label:
                                    // whether the key string is built before
                                    // or after `row` moves into the closure
                                    // below is decided by the macro
                                    // expansion, and a key over a `Copy` value
                                    // cannot be on the wrong side of it. The
                                    // list is rebuilt wholesale on every
                                    // keystroke anyway, so position is the
                                    // identity that means anything here.
                                    key: "{i}",
                                    class: cls,
                                    // Not a click: the mouse button going down
                                    // takes the focus off the box, and the row
                                    // under the pointer is gone by the time a
                                    // click would land.
                                    onmousedown: move |e| {
                                        e.prevent_default();
                                        choose(row.clone());
                                    },
                                    onmouseenter: move |_| at.set(i),
                                    if !row.tag.is_empty() {
                                        span { class: "picktag", "{row.tag}" }
                                    }
                                    span { class: "picklabel", {lit(&row.label, &row.hits)} }
                                    if !row.detail.is_empty() {
                                        span { class: "pickdetail", "{row.detail}" }
                                    }
                                }
                            }
                        }
                    }
                    if count == 0 {
                        div { class: "pickempty", "Nothing matches" }
                    }
                }
            }
        }
    }
}

/// The label, with the characters that were typed picked out of it.
///
/// Runs rather than characters: a span per letter of a path is a great many
/// spans for a list that is redrawn on every keystroke, and the letters that
/// matched are nearly always next to each other.
fn lit(text: &str, hits: &[usize]) -> Element {
    if hits.is_empty() {
        return rsx! { "{text}" };
    }
    let mut runs: Vec<(String, bool)> = Vec::new();
    let mut cursor = 0;
    for &at in hits {
        // An offset that is not a character boundary — or is behind us — came
        // from somewhere that has trimmed the string this was scored against.
        // Drawing the label whole beats panicking on it.
        if at < cursor || at >= text.len() || !text.is_char_boundary(at) {
            continue;
        }
        let Some(c) = text[at..].chars().next() else {
            continue;
        };
        let end = at + c.len_utf8();
        if at > cursor {
            runs.push((text[cursor..at].to_string(), false));
        }
        match runs.last_mut() {
            Some((run, true)) => run.push(c),
            _ => runs.push((c.to_string(), true)),
        }
        cursor = end;
    }
    if cursor < text.len() {
        runs.push((text[cursor..].to_string(), false));
    }
    rsx! {
        for (i, (run, hit)) in runs.into_iter().enumerate() {
            if hit {
                b { key: "{i}", class: "pickhit", "{run}" }
            } else {
                span { key: "{i}", "{run}" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_character_asks_the_question() {
        assert_eq!(Pick::of("app"), (Pick::File, "app"));
        assert_eq!(Pick::of(">wrap"), (Pick::Command, "wrap"));
        assert_eq!(Pick::of("@render"), (Pick::Symbol, "render"));
        assert_eq!(Pick::of("#render"), (Pick::Repo, "render"));
        assert_eq!(Pick::of(":120"), (Pick::Line, "120"));
        assert_eq!(Pick::of(""), (Pick::File, ""));
    }

    #[test]
    fn a_prefix_on_its_own_is_still_that_question() {
        assert_eq!(Pick::of(">"), (Pick::Command, ""));
    }

    #[test]
    fn a_line_can_be_typed_onto_the_end_of_a_file() {
        assert_eq!(trailing_line("main.rs:120"), ("main.rs", Some(120)));
        assert_eq!(trailing_line("main.rs"), ("main.rs", None));
        // A colon with nothing after it is somebody halfway through typing.
        assert_eq!(trailing_line("main.rs:"), ("main.rs:", None));
        assert_eq!(trailing_line("main.rs:abc"), ("main.rs:abc", None));
    }

    #[test]
    fn a_row_is_named_by_the_file_and_placed_by_the_path() {
        assert_eq!(
            split_path("src/ui/app.rs"),
            ("app.rs".to_string(), "src/ui".to_string())
        );
        assert_eq!(
            split_path("README.md"),
            ("README.md".to_string(), String::new())
        );
    }

    #[test]
    fn every_command_is_named_once_and_has_somewhere_to_go() {
        let mut ids: Vec<&str> = CMDS.iter().map(|c| c.id).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), before, "two commands share an id");

        let mut names: Vec<&str> = CMDS.iter().map(|c| c.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before, "two commands share a name");
    }

    #[test]
    fn the_commands_are_findable_by_a_word_in_the_middle_of_them() {
        let names: Vec<&str> = CMDS.iter().map(|c| c.name).collect();
        let ranked = fuzzy::rank("wrap", &names, 5);
        assert_eq!(names[ranked[0].0], "Toggle Word Wrap");

        let ranked = fuzzy::rank("changed", &names, 5);
        assert!(
            names[ranked[0].0].contains("Changed File"),
            "got {}",
            names[ranked[0].0]
        );
    }
}
