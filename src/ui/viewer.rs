use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use dioxus::prelude::*;

use std::collections::HashMap;

use crate::backend::difftool::{
    Block, Expansion, FileDiff, Line, LineKind, STEP, blocks, diff_file, stats, to_rows,
};
use crate::backend::highlight::{Span, highlight};
use crate::backend::images::{self, media_type};
use crate::backend::markdown;
use crate::backend::route;
use crate::backend::search::split_word;
use crate::backend::tree::ChangeKind;
use crate::backend::{FileContent, clip};

use super::app::{PrFileState, St, ViewMode, Workspace};
use super::compat;
use super::ide;
use super::imgcache::{all_settled, drawable, ensure_image};
use super::markdown::Target;
use super::prcache::ensure_path;

/// Files offered in Preview. The browser lays HTML out itself, so this is the
/// whole test — there is no renderer here with opinions of its own.
fn is_html(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm"))
}

const BIG_FILE_BYTES: usize = 400_000;
const BIG_FILE_LINES: usize = 6_000;

/// What one `src` in a previewed page turns out to be.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Pic {
    /// A file in this repository, to be carried into the document — there is no
    /// server under this app for the frame to have fetched it from.
    File(PathBuf),
    /// Something the browser resolves on its own: a picture on a host of its
    /// own, or one written into the page as data already.
    Elsewhere,
    /// A path leading somewhere this app cannot reach.
    Missing,
}

/// Read one `src` out of a previewed page, `from` being the page's own path —
/// a relative URL in an HTML file is relative to the file, not to the root.
fn pic_of(from: &Path, url: &str) -> Pic {
    let url = url.trim();
    // Before anything is resolved as a path, because none of these are one.
    // `data:` above all: a picture already written into the page is one that
    // draws, and treating it as a filename would blank it out.
    let scheme_like = url.contains("://")
        || url.starts_with("//")
        || ["data:", "blob:", "mailto:", "about:"]
            .iter()
            .any(|s| url.len() >= s.len() && url[..s.len()].eq_ignore_ascii_case(s));
    if scheme_like {
        return Pic::Elsewhere;
    }
    match super::markdown::resolve(from, url) {
        Target::File(path) if media_type(&path).is_some() => Pic::File(path),
        Target::Web(_) => Pic::Elsewhere,
        // A path that resolves to nothing drawable, or out of the repository
        // altogether. Left as written, the frame would resolve it against
        // *this* app's URL and fetch a 404 of ours.
        _ => Pic::Missing,
    }
}

#[derive(Clone, PartialEq)]
enum SourceLines {
    Colored(Vec<Vec<Span>>),
    Plain(Vec<String>),
}

/// What the viewer has to show right now. Every file arrives over the network
/// or off the browser's filesystem, hence `Loading`/`Failed`.
#[derive(Clone, PartialEq)]
enum Pane {
    Empty,
    Loading,
    Failed(String),
    Ready {
        rel: PathBuf,
        /// Left-hand side: the PR's merge base.
        old: Rc<FileContent>,
        /// Right-hand side: the PR's head commit.
        new: Rc<FileContent>,
    },
}

/// Clicking a line number picks that line out — which is what puts it in the
/// address bar, and so into the link the button beside it copies.
///
/// One listener on the document rather than a handler per line: a six thousand
/// line file is six thousand rows, and this is the difference between a
/// closure on each of them and none. It reads the anchor the row already
/// carries for jumping to, so the number it sends is the head commit's
/// numbering in every view — which is the numbering a link is written in.
const LINE_JS: &str = r#"
(function () {
  // A reload of this page's script should not leave the last listener behind.
  if (window.__pullspace_lines) window.__pullspace_lines();
  var on = function (e) {
    if (!e.target || !e.target.closest) return;
    var gutter = e.target.closest('.ln.lnk');
    if (!gutter) return;
    var row = gutter.closest('.cl, .scell');
    if (!row) return;
    var anchor = row.id || ((row.querySelector('.anchor') || {}).id || '');
    if (anchor.charAt(0) !== 'L') return;
    var line = parseInt(anchor.slice(1), 10);
    if (!line) return;
    e.preventDefault();
    dioxus.send(line);
  };
  document.addEventListener('click', on);
  window.__pullspace_lines = function () {
    document.removeEventListener('click', on);
  };
})();
"#;

/// Listen for line numbers being clicked, for as long as the app is up.
pub async fn lines(st: St) {
    let mut eval = document::eval(LINE_JS);
    while let Ok(line) = eval.recv::<usize>().await {
        st.mark_line(line);
    }
}

fn scroll_js(line: usize) -> String {
    format!(
        r#"(function(){{var n=0;function go(){{var e=document.getElementById('L{line}');if(e){{var t=e.classList.contains('anchor')?e.parentElement:e;t.scrollIntoView({{block:'center'}});t.classList.add('flash');setTimeout(function(){{t.classList.remove('flash')}},1400);}}else if(n++<25){{setTimeout(go,60);}}}}go();}})();"#
    )
}

#[component]
pub fn Viewer() -> Element {
    let st = use_context::<St>();

    // On a pull request the open file usually arrives already warmed, from the
    // background prefetch or the tree's hover handler. This covers the rest —
    // and is a no-op when the content is cached or on its way.
    use_effect(move || {
        let rel = st.open.read().clone();
        if let Some(rel) = rel {
            ensure_path(st, &rel);
        }
    });

    // Resolve the two sides of the open file out of the cache.
    let data = use_memo(move || {
        st.refresh_tick.read();
        let Some(rel) = st.open.read().clone() else {
            return Pane::Empty;
        };
        match st.pr_files.read().get(&rel) {
            None | Some(PrFileState::Loading) => Pane::Loading,
            Some(PrFileState::Failed(e)) => Pane::Failed(e.clone()),
            Some(PrFileState::Ready { base, head }) => Pane::Ready {
                rel,
                old: base.clone(),
                new: head.clone(),
            },
        }
    });

    // Jump-to-line requests (definitions, references, search hits, and a link
    // somebody was sent). The request is not consumed — see `St::scroll_to`;
    // this fires on the write, and what the signal holds afterwards is nobody's
    // business.
    //
    // It also fires when the file itself arrives, which is the case a link
    // opened cold: the line was asked for while the pane still said "Loading…",
    // and the retry loop in `scroll_js` gives up long before a fetch of a large
    // file comes back. `at_line` is what tells this from the leftovers of the
    // last jump — opening a file without one clears it.
    use_effect(move || {
        let _ = data.read();
        let want = *st.scroll_to.read();
        if let Some(line) = want
            && *st.at_line.peek() == want
        {
            document::eval(&scroll_js(line));
        }
    });

    let diff = use_memo(move || {
        let guard = data.read();
        let Pane::Ready { rel, old, new } = &*guard else {
            return None;
        };
        let status = st.statuses.read().get(rel).copied()?;
        // An added file has nothing to compare against, even if a path of the
        // same name exists in the base.
        let fresh = matches!(status, ChangeKind::Added);
        let old_text = if fresh { "" } else { old.text()? };
        Some(diff_file(old_text, new.text()?))
    });

    // A memo of its own rather than `st.prefs.read().theme` inside
    // `source_lines`: reading `prefs` there subscribes to all of it, and a
    // font-size nudge would re-highlight the whole file for nothing.
    let theme = use_memo(move || st.prefs.read().theme);
    let source_lines = use_memo(move || {
        // Syntax colours follow the app's theme, and the highlighter is asked
        // for them again when it moves. Subscribed to rather than used: what
        // the theme *is* lives in `backend::highlight`.
        theme.read();
        let guard = data.read();
        let Pane::Ready { rel, old, new } = &*guard else {
            return None;
        };
        // Borrowed, not cloned — `guard` holds the pane to the end of this.
        let text: &str = match new.as_ref() {
            FileContent::Text(t) => t,
            // Deleted: fall back to the old side so there is something to read.
            FileContent::Absent => match old.as_ref() {
                FileContent::Text(t) => t,
                _ => return None,
            },
            FileContent::Binary => return None,
        };
        let line_count = text.lines().count();
        if text.len() > BIG_FILE_BYTES || line_count > BIG_FILE_LINES {
            Some(SourceLines::Plain(text.lines().map(String::from).collect()))
        } else {
            Some(SourceLines::Colored(highlight(rel, text)))
        }
    });

    // Markdown, parsed only while it is what the preview mode is being asked
    // for. Cheap enough to do here — a README parses in well under the
    // millisecond a `spawn_blocking` round trip would cost.
    let doc = use_memo(move || {
        let wanted = *st.view_mode.read() == ViewMode::Preview;
        let guard = data.read();
        let Pane::Ready { rel, old, new } = &*guard else {
            return None;
        };
        if !wanted || !markdown::is_markdown(rel) {
            return None;
        }
        let text = match (new.as_ref(), old.as_ref()) {
            (FileContent::Text(t), _) => t,
            // Deleted: render the version that is going away.
            (FileContent::Absent, FileContent::Text(t)) => t,
            _ => return None,
        };
        Some(markdown::parse(text))
    });

    // The page to draw, and only while the preview is the mode being asked
    // for. Memoised on the file's own text, so a reload that changes it
    // redraws and one that does not costs nothing.
    let preview = use_memo(move || {
        let wanted = *st.view_mode.read() == ViewMode::Preview;
        match &*data.read() {
            Pane::Ready { rel, old, new } if wanted && is_html(rel) => {
                match (new.as_ref(), old.as_ref()) {
                    (FileContent::Text(t), _) => Some(t.clone()),
                    // Deleted by the PR: draw the version that is going away.
                    (FileContent::Absent, FileContent::Text(t)) => Some(t.clone()),
                    _ => None,
                }
            }
            _ => None,
        }
    });

    // Every picture the page being previewed asks for, and where in the file it
    // asks. Scanned once per page rather than once per picture arriving.
    let preview_pics = use_memo(move || match preview.read().as_deref() {
        Some(html) => images::image_refs(html),
        None => Vec::new(),
    });

    // Read them. A page carries its pictures inside itself here — see
    // `backend::images` — so this is what has to happen before the frame is
    // handed anything.
    use_effect(move || {
        let rel = st.open.read().clone().unwrap_or_default();
        for r in preview_pics.read().iter() {
            if let Pic::File(path) = pic_of(&rel, &r.url) {
                ensure_image(st, &path);
            }
        }
    });

    // The page as the frame gets it: every `src` naming a file in this
    // repository swapped for the file itself.
    //
    // Written out twice at most, however many pictures there are. A `srcdoc`
    // that changes reloads the frame it is on, so a page of six screenshots
    // arriving one at a time would load itself six times over in front of
    // whoever is reading it — hence the wait for the last of them to settle
    // before anything but the blanks is put in.
    let preview_html = use_memo(move || {
        let held = preview.read();
        let html = held.as_deref()?;
        let refs = preview_pics.read();
        if refs.is_empty() {
            return Some(html.to_string());
        }
        let rel = st.open.read().clone().unwrap_or_default();
        let wanted: Vec<PathBuf> = refs
            .iter()
            .filter_map(|r| match pic_of(&rel, &r.url) {
                Pic::File(path) => Some(path),
                _ => None,
            })
            .collect();
        let settled = all_settled(&st, &wanted);
        Some(images::rewrite(html, &refs, |r| {
            match pic_of(&rel, &r.url) {
                Pic::File(path) if settled => Some(
                    drawable(&st, &path)
                        .map_or_else(|| images::BLANK.to_string(), |uri| uri.to_string()),
                ),
                Pic::File(_) | Pic::Missing => Some(images::BLANK.to_string()),
                Pic::Elsewhere => None,
            }
        }))
    });

    // A new file starts at the top. The scroll container outlives the file in
    // it, so without this, opening something short after reading deep into
    // something long lands you at the bottom of it.
    //
    // Unless it was opened *at* somewhere, in which case the effect above is
    // already on its way there and this would fight it.
    use_effect(move || {
        let _ = st.open.read();
        if st.at_line.peek().is_some() {
            return;
        }
        document::eval("var e=document.querySelector('.codewrap'); if(e) e.scrollTop=0;");
    });

    // Markdown's fenced blocks are highlighted where they are drawn, so the
    // pane has to be redrawn when the theme moves — the memo above only covers
    // the source view.
    let _ = st.prefs.read().theme;

    let guard = data.read();
    // Loading and failure replace the file, not the window around it: a header
    // that blinks out and back every time an uncached file is clicked is worse
    // jank than the wait it is reporting.
    //
    // Only whether the new side is absent leaves this match, not the side
    // itself — the viewer renders often, and the one question the header asks
    // is not worth cloning a file's contents to answer.
    let (rel, new_absent, pending) = match &*guard {
        Pane::Empty => return rsx! { Welcome {} },
        Pane::Loading => (
            st.open.read().clone().unwrap_or_default(),
            true,
            Some(rsx! { div { class: "notice", "Loading…" } }),
        ),
        Pane::Failed(e) => (
            st.open.read().clone().unwrap_or_default(),
            true,
            Some(rsx! { div { class: "notice error", "{e}" } }),
        ),
        Pane::Ready { rel, new, .. } => (rel.clone(), **new == FileContent::Absent, None),
    };
    let settled = pending.is_none();

    let status = st.statuses.read().get(&rel).copied();
    let deleted_note = settled && new_absent && status == Some(ChangeKind::Deleted);
    let prose = markdown::is_markdown(&rel);
    let previewable = is_html(&rel) || prose;
    // Only changed files have a diff to show, and only HTML has a page to draw.
    let mode = match *st.view_mode.read() {
        ViewMode::Preview if !previewable => ViewMode::Source,
        ViewMode::Inline | ViewMode::Split if status.is_none() => ViewMode::Source,
        m => m,
    };

    let rel_str = rel.display().to_string();
    let badge = status.map(|s| (s.badge(), s.css()));
    let diff_stats = diff.read().as_ref().map(stats);
    // Which contracted stretches of this file have been opened up. Read here,
    // so that opening one redraws the diff — and only the diff: the comparison
    // itself is memoised on the file's contents and does not run again.
    let open_gaps = st.open_gaps();

    // The identifier picked out of the code, if one has been. Every line that
    // holds it gets it marked — which is why this is threaded all the way down
    // rather than done in CSS: only the lines that match pay for it.
    let mark = st.selected.read().clone();
    let mark = mark.as_deref();
    // The line a link points at, if one does. Every view that has line numbers
    // lights it up, and clicking a number is what puts it there.
    let at = *st.at_line.read();

    let body = match mode {
        ViewMode::Source => match source_lines.read().as_ref() {
            Some(SourceLines::Colored(lines)) => render_colored(lines, mark, at),
            Some(SourceLines::Plain(lines)) => render_plain(lines, mark, at),
            None => rsx! { div { class: "notice", "Binary file — no preview." } },
        },
        ViewMode::Inline => match diff.read().as_ref() {
            Some(d) if d.is_empty() => rsx! { div { class: "notice", "No differences." } },
            Some(d) => render_inline(d, &open_gaps, mark, at),
            None => rsx! { div { class: "notice", "Binary file — cannot diff." } },
        },
        ViewMode::Split => match diff.read().as_ref() {
            Some(d) if d.is_empty() => rsx! { div { class: "notice", "No differences." } },
            Some(d) => render_split(d, &open_gaps, mark, at),
            None => rsx! { div { class: "notice", "Binary file — cannot diff." } },
        },
        ViewMode::Preview if prose => match doc.read().as_ref() {
            Some(parsed) => super::markdown::render(st, &rel, parsed),
            None => rsx! { div { class: "notice", "Nothing to render — this file has no text." } },
        },
        ViewMode::Preview => match &*preview_html.read() {
            None => rsx! { div { class: "notice", "Nothing to draw — this file has no text." } },
            Some(html) => render_preview(html),
        },
    };

    let mut vm = st.view_mode;
    // A disabled button at 30% opacity with no explanation is a puzzle; the
    // tooltip is where the reason goes.
    let mode_btn =
        |label: &'static str, m: ViewMode, cur: ViewMode, enabled: bool, why: &'static str| {
            let cls = if m == cur && enabled {
                "modebtn on"
            } else {
                "modebtn"
            };
            rsx! {
                button {
                    class: cls,
                    title: "{why}",
                    disabled: !enabled,
                    onclick: move |_| vm.set(m),
                    "{label}"
                }
            }
        };
    // Nothing to diff or draw until the file itself has arrived.
    let diffable = settled && status.is_some();
    let diff_why = if diffable {
        "Show this file's changes"
    } else if settled {
        "This file has no changes to diff"
    } else {
        "Waiting for the file"
    };

    let (can_back, can_fwd) = (st.can_go_back(), st.can_go_forward());
    // Where this file stands among the ones the pull request changes, and
    // whether it is one of them at all — an unchanged file opened to read
    // around the change is not something to tick off a review.
    let step_at = st.file_at();
    let step_total = st.changed_files.read().len();
    let markable = st.viewed_key(&rel).is_some();
    let viewed = markable && st.is_viewed(&rel);
    let selected = st.selected.read().clone();
    // Only worth offering while there is something to undo, and only where the
    // thing it undoes can be seen.
    let can_reset = matches!(mode, ViewMode::Inline | ViewMode::Split) && !open_gaps.is_empty();

    rsx! {
        div { class: "viewer",
            div { class: "viewhdr",
                // Following a definition three files away is only worth doing
                // if getting back is one key. These are that key, drawn.
                button {
                    class: "iconbtn sm",
                    title: "Back  (⌘[)",
                    disabled: !can_back,
                    onclick: move |_| st.go_back(),
                    "‹"
                }
                button {
                    class: "iconbtn sm",
                    title: "Forward  (⌘])",
                    disabled: !can_fwd,
                    onclick: move |_| st.go_forward(),
                    "›"
                }
                span { class: "vpath", title: "{rel_str}", "{rel_str}" }
                if let Some((b, c)) = badge {
                    span { class: "badge {c}", "{b}" }
                }
                if deleted_note {
                    span { class: "delnote", "deleted — showing the old version" }
                }
                if let Some(ds) = diff_stats {
                    if diffable {
                        span { class: "dstat add", "+{ds.added}" }
                        span { class: "dstat del", "−{ds.removed}" }
                    }
                }
                span { class: "spacer" }
                if settled {
                    LinkButton { line: at }
                }
                if markable {
                    ViewedBox { rel, viewed }
                }
                if step_total > 0 {
                    FileStep { at: step_at, total: step_total }
                }
                if can_reset {
                    button {
                        class: "resetbtn",
                        title: "Contract every stretch that has been opened up",
                        onclick: move |_| st.contract_all_gaps(),
                        "Reset"
                    }
                }
                // One control, not four loose buttons: these are alternatives,
                // and a segmented group is what says so.
                div { class: "modegroup",
                    {mode_btn("Source", ViewMode::Source, mode, settled, "Show the file as it stands")}
                    {mode_btn("Inline", ViewMode::Inline, mode, diffable, diff_why)}
                    {mode_btn("Split", ViewMode::Split, mode, diffable, diff_why)}
                    if previewable {
                        {mode_btn(
                            "Preview",
                            ViewMode::Preview,
                            mode,
                            settled,
                            if prose { "Show this file as the prose it is" } else { "Draw this page as it would look" },
                        )}
                    }
                }
            }
            if let Some(name) = selected {
                SymBar { name }
            }
            div {
                class: "codewrap",
                // A browser already knows where a word starts and ends, and a
                // double-click is how it is asked. Wrapping every token of
                // every line in its own clickable span to find out the same
                // thing costs more than everything else the viewer does.
                ondoubleclick: move |_| ide::select_word(st),
                {pending.unwrap_or(body)}
            }
        }
    }
}

/// A link to what is on screen, on the clipboard.
///
/// The address bar already says it — an effect at the root keeps it in step
/// with the file and the line — so this is a button for not having to go up
/// there and select it. Which is also why it copies `location.href` rather than
/// building an address of its own: there is only one answer, and the bar is
/// already showing it.
#[component]
fn LinkButton(line: Option<usize>) -> Element {
    let mut copied = use_signal(|| false);
    let done = *copied.read();
    let why = match line {
        Some(n) => format!("Copy a link to line {n} of this file"),
        None => "Copy a link to this file — click a line number to point at a line".to_string(),
    };
    let cls = if done { "linkbtn done" } else { "linkbtn" };
    rsx! {
        button {
            class: cls,
            title: "{why}",
            onclick: move |_| {
                // Inside the click, not after an await: the permission to write
                // the clipboard is the gesture, and a gesture does not survive
                // being waited on.
                if let Some(url) = route::href() {
                    clip::copy(&url);
                }
                copied.set(true);
                spawn(async move {
                    compat::sleep(Duration::from_millis(1200)).await;
                    copied.set(false);
                });
            },
            if done { "Copied" } else { "Link" }
        }
    }
}

/// "I have read this one" — the box a review is actually kept in.
///
/// What it remembers is the file's *contents*, not its name: the mark is the
/// git blob hash, so a force-push that rebases the branch without touching this
/// file leaves it ticked, and one that rewrites it brings it back for another
/// look. That is the behaviour the content-addressed store on disk was already
/// giving the files themselves, applied to the reader's own place in the review.
#[component]
fn ViewedBox(rel: PathBuf, viewed: bool) -> Element {
    let st = use_context::<St>();
    let cls = if viewed { "viewedbox on" } else { "viewedbox" };
    let why = if viewed {
        "Read. Kept by content, so a push that rewrites this file unticks it"
    } else {
        "Mark this file as read — kept between visits, per pull request"
    };
    rsx! {
        label { class: "{cls}", title: "{why}",
            input {
                r#type: "checkbox",
                checked: viewed,
                onchange: move |_| st.toggle_viewed(&rel),
            }
            "Viewed"
        }
    }
}

/// Where you are in the changed files, and the two buttons that move.
///
/// A review is a list of files worked through in order, and the pair of these
/// is how it gets worked through: tick the box, press the arrow. They step in
/// the order the explorer lists them, folded directories included — a file is
/// no less changed for being out of sight.
#[component]
fn FileStep(at: Option<usize>, total: usize) -> Element {
    let st = use_context::<St>();
    let (can_prev, can_next) = match at {
        Some(i) => (i > 0, i + 1 < total),
        // Somewhere that is not one of them: both arrows enter the list, at the
        // end each is heading for.
        None => (true, true),
    };
    let label = match at {
        Some(i) => format!("{}/{}", i + 1, total),
        None => format!("–/{total}"),
    };
    let why = match at {
        Some(i) => format!("Changed file {} of {}", i + 1, total),
        None => format!(
            "{} changed file{} — this is not one of them",
            total,
            if total == 1 { "" } else { "s" }
        ),
    };
    rsx! {
        div { class: "stepgrp", title: "{why}",
            button {
                class: "stepbtn",
                title: "Previous changed file  (⌥↑)",
                disabled: !can_prev,
                onclick: move |_| st.step_file(false),
                "⌃"
            }
            span { class: "stepnum", "{label}" }
            button {
                class: "stepbtn",
                title: "Next changed file  (⌥↓)",
                disabled: !can_next,
                onclick: move |_| st.step_file(true),
                "⌄"
            }
        }
    }
}

/// What can be done with the identifier that has been picked out.
#[component]
fn SymBar(name: String) -> Element {
    let st = use_context::<St>();
    let mut sel = st.selected;
    let (def, peek, refs) = (name.clone(), name.clone(), name.clone());
    rsx! {
        div { class: "symbar",
            span { class: "symname", "{name}" }
            button {
                class: "symbtn",
                title: "Open where this is defined  (F12)",
                onclick: move |_| ide::goto_def(st, &def),
                "Go to Definition"
            }
            button {
                class: "symbtn",
                title: "Show the definition below, without leaving this file",
                onclick: move |_| ide::peek_def(st, &peek),
                "Peek"
            }
            button {
                class: "symbtn",
                title: "Every use of this name in the repository  (⇧F12)",
                onclick: move |_| ide::find_refs(st, &refs),
                "Find References"
            }
            span { class: "spacer" }
            button {
                class: "iconbtn",
                title: "Clear the selection  (Esc)",
                onclick: move |_| sel.set(None),
                "✕"
            }
        }
    }
}

/// No file open: something is on screen and no file in it has been picked yet
/// — a repository whose README could not be found, a commit, a merge. Say what
/// is open and where its files are. (Nothing open at all never lands here: the
/// landing page stands in for the whole IDE then.)
#[component]
fn Welcome() -> Element {
    let st = use_context::<St>();
    let showing = match &*st.workspace.read() {
        Workspace::Pr(pr) => Some((
            format!("{} #{}", pr.repo, pr.number),
            format!(
                "{} file{} changed — pick one on the left.",
                pr.files.len(),
                if pr.files.len() == 1 { "" } else { "s" },
            ),
        )),
        Workspace::Repo(view) => Some((
            format!("{} @ {}", view.repo, view.branch),
            "No README here — pick a file on the left.".to_string(),
        )),
        Workspace::Commit(view) => Some((
            format!("{} {}", view.repo, view.commit.short()),
            match view.files.len() {
                0 if view.merge => {
                    "A merge commit — GitHub lists no changed files for one.".to_string()
                }
                0 => "Nothing changed in this commit.".to_string(),
                1 => "1 file changed — pick it on the left.".to_string(),
                n => format!("{n} files changed — pick one on the left."),
            },
        )),
        _ => None,
    };
    let Some((title, hint)) = showing else {
        // Unreachable while the landing page owns the empty workspace, and
        // nothing worth drawing if that ever changes.
        return rsx! {
            div { class: "viewer" }
        };
    };
    rsx! {
        div { class: "viewer",
            div { class: "welcome",
                div { class: "welcome-title", "{title}" }
                div { class: "welcome-hint", "{hint}" }
            }
        }
    }
}

/// One run of text, with every whole-word occurrence of the selected
/// identifier picked out of it.
///
/// The `contains` short-circuit is the whole design: with nothing selected, or
/// on the overwhelming majority of lines that do not mention it, this is the
/// single span it always was. Only the handful of lines that actually match
/// pay for being split up.
fn marked(text: &str, color: Option<&str>, mark: Option<&str>) -> Element {
    let style = color.map(|c| format!("color:{c}")).unwrap_or_default();
    let Some(name) = mark.filter(|n| text.contains(*n)) else {
        return rsx! { span { style: "{style}", "{text}" } };
    };
    let parts = split_word(text, name);
    if !parts.iter().any(|(hit, _)| *hit) {
        return rsx! { span { style: "{style}", "{text}" } };
    }
    rsx! {
        for (i, (hit, part)) in parts.into_iter().enumerate() {
            if hit {
                span { key: "{i}", class: "occ", style: "{style}", "{part}" }
            } else {
                span { key: "{i}", style: "{style}", "{part}" }
            }
        }
    }
}

/// The class a row of code carries: the one the address bar is pointing at is
/// the one a link brought somebody to, and it stays lit for as long as it is
/// the answer.
fn row_class(no: usize, at: Option<usize>) -> &'static str {
    if at == Some(no) { "cl linked" } else { "cl" }
}

fn render_colored(lines: &[Vec<Span>], mark: Option<&str>, at: Option<usize>) -> Element {
    rsx! {
        div { class: "code",
            for (i, spans) in lines.iter().enumerate() {
                div { class: row_class(i + 1, at), id: "L{i + 1}",
                    span { class: "ln lnk", "{i + 1}" }
                    span { class: "lc",
                        for sp in spans {
                            {marked(&sp.text, Some(&sp.color), mark)}
                        }
                    }
                }
            }
        }
    }
}

/// The page itself, in a box that cannot reach out of itself.
///
/// The empty `sandbox` matters more here than the convenience does. This page
/// holds a GitHub token in local storage, and the file being previewed is
/// someone else's code from a pull request. An empty value denies everything
/// the attribute can deny — scripts, forms, navigation, popups, and above all
/// same-origin access, so the frame cannot read the storage it is sitting in.
fn render_preview(html: &str) -> Element {
    rsx! {
        div { class: "previewwrap",
            div { class: "previewbar",
                span {
                    class: "previewsafe",
                    title: "Rendered in a sandboxed frame — scripts, forms and navigation are all denied, and it cannot see this page",
                    "scripts disabled"
                }
                span {
                    class: "previewnote",
                    title: "The frame has no origin, so anything the page hosts elsewhere is fetched without credentials",
                    "sandboxed"
                }
            }
            iframe {
                class: "previewframe",
                // Not in dioxus 0.6's iframe element, so spelled as a raw
                // attribute. Dropping it is not an option — see above.
                "sandbox": "",
                // `srcdoc` keeps the page inline: nothing is served, so there
                // is no URL to leak and nothing to clean up afterwards.
                srcdoc: "{html}",
            }
        }
    }
}

fn render_plain(lines: &[String], mark: Option<&str>, at: Option<usize>) -> Element {
    rsx! {
        div { class: "code",
            for (i, text) in lines.iter().enumerate() {
                div { class: row_class(i + 1, at), id: "L{i + 1}",
                    span { class: "ln lnk", "{i + 1}" }
                    span { class: "lc", {marked(text, None, mark)} }
                }
            }
        }
    }
}

fn anchor(no: Option<usize>) -> Element {
    match no {
        Some(n) => rsx! { span { id: "L{n}", class: "anchor" } },
        None => rsx! {},
    }
}

fn num(no: Option<usize>) -> String {
    no.map(|n| n.to_string()).unwrap_or_default()
}

/// A diff line's word-level segments, with occurrences marked inside them.
///
/// Two kinds of emphasis end up on the same text here: `emph` is what the diff
/// changed, and `occ` is what the reader picked out. They are different
/// questions about the same line and both are worth being able to see, so they
/// nest rather than compete.
fn segs_rsx(l: &Line, mark: Option<&str>) -> Element {
    rsx! {
        for seg in l.segs.iter() {
            if seg.emph {
                span { class: "emph", {marked(&seg.text, None, mark)} }
            } else {
                {marked(&seg.text, None, mark)}
            }
        }
    }
}

fn inline_line(l: &Line, mark: Option<&str>, at: Option<usize>) -> Element {
    let (cls, sign) = match l.kind {
        LineKind::Ctx => ("cl", " "),
        LineKind::Add => ("cl dl-add", "+"),
        LineKind::Del => ("cl dl-del", "-"),
    };
    let cls = match l.new_no.is_some() && l.new_no == at {
        true => format!("{cls} linked"),
        false => cls.to_string(),
    };
    let old = num(l.old_no);
    let new = num(l.new_no);
    // Only the head commit's numbering is linkable: a line that has been
    // removed is not somewhere anyone can be sent to.
    let new_cls = if l.new_no.is_some() { "ln lnk" } else { "ln" };
    rsx! {
        div { class: "{cls}",
            {anchor(l.new_no)}
            span { class: "ln", "{old}" }
            span { class: "{new_cls}", "{new}" }
            span { class: "dsign", "{sign}" }
            span { class: "lc", {segs_rsx(l, mark)} }
        }
    }
}

/// The `@@ … @@` line, pinned to the top of the pane for as long as its own
/// hunk is on screen — halfway down a long one, knowing which it is is the
/// whole question. Its text is pinned to the left edge too, so scrolling
/// sideways through wide code does not take the answer with it.
fn hunk_header(header: &str) -> Element {
    rsx! {
        div { class: "hunkhdr",
            span { class: "hunkhdrtext", "{header}" }
        }
    }
}

/// The bar standing in for the unchanged lines a diff leaves out — and, once
/// they are showing, the handle that folds them away again.
///
/// Three ways to open one, because a gap can be four lines or four hundred:
/// a step off either end for reading outwards from a change, and the whole
/// thing for when the answer is somewhere else entirely. Every one of them is
/// undone by the same control, which is why it stays on the bar afterwards
/// rather than leaving the reader to find Reset.
#[component]
fn GapBar(index: usize, hidden: usize, shown: usize) -> Element {
    let st = use_context::<St>();
    let len = hidden + shown;
    if hidden == 0 {
        let lines = plural(shown);
        return rsx! {
            div { class: "gapbar open",
                div { class: "gapbarinner",
                    button {
                        class: "gapbtn",
                        title: "Contract these {shown} lines again",
                        onclick: move |_| st.contract_gap(index),
                        "⌃  hide {shown} unchanged {lines}"
                    }
                }
            }
        };
    }
    let lines = plural(hidden);
    // Below a step there is nothing for the step buttons to do that the whole
    // stretch does not already do in one click.
    let stepped = hidden > STEP;
    rsx! {
        div { class: "gapbar",
            div { class: "gapbarinner",
                if stepped {
                    button {
                        class: "gapbtn",
                        title: "Show {STEP} more lines, from the top of this stretch down",
                        onclick: move |_| st.expand_gap(index, STEP, true),
                        "⌄"
                    }
                    button {
                        class: "gapbtn",
                        title: "Show {STEP} more lines, from the bottom of this stretch up",
                        onclick: move |_| st.expand_gap(index, STEP, false),
                        "⌃"
                    }
                }
                button {
                    class: "gapbtn wide",
                    title: "Show all {hidden} of them",
                    onclick: move |_| st.expand_gap_fully(index, len),
                    "⋯  {hidden} unchanged {lines}"
                }
                if shown > 0 {
                    button {
                        class: "gapbtn",
                        title: "Contract this stretch again",
                        onclick: move |_| st.contract_gap(index),
                        "⤺"
                    }
                }
            }
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "line" } else { "lines" }
}

fn render_inline(
    diff: &FileDiff,
    open: &HashMap<usize, Expansion>,
    mark: Option<&str>,
    at: Option<usize>,
) -> Element {
    rsx! {
        div { class: "code inline",
            for block in blocks(diff, open) {
                {inline_block(diff, block, mark, at)}
            }
        }
    }
}

fn inline_block(diff: &FileDiff, block: Block, mark: Option<&str>, at: Option<usize>) -> Element {
    match block {
        Block::Gap {
            index,
            hidden,
            shown,
        } => rsx! {
            GapBar { key: "g{index}", index, hidden, shown }
        },
        Block::Lines { header, from, to } => rsx! {
            div { key: "l{from}", class: "hunk",
                if let Some(h) = header {
                    {hunk_header(&h)}
                }
                for l in diff.lines[from..to].iter() {
                    {inline_line(l, mark, at)}
                }
            }
        },
    }
}

fn split_cell(l: Option<&Line>, right: bool, mark: Option<&str>, at: Option<usize>) -> Element {
    match l {
        None => rsx! { div { class: "scell s-empty" } },
        Some(l) => {
            let cls = match l.kind {
                LineKind::Ctx => "scell",
                LineKind::Add => "scell dl-add",
                LineKind::Del => "scell dl-del",
            };
            let no = if right { num(l.new_no) } else { num(l.old_no) };
            // The head commit's side carries the anchors, so it is the side a
            // link can point into — and the side worth lighting up when one
            // does.
            let anchor_no = if right { l.new_no } else { None };
            let cls = match anchor_no.is_some() && anchor_no == at {
                true => format!("{cls} linked"),
                false => cls.to_string(),
            };
            let ln_cls = if anchor_no.is_some() { "ln lnk" } else { "ln" };
            rsx! {
                div { class: "{cls}",
                    {anchor(anchor_no)}
                    span { class: "{ln_cls}", "{no}" }
                    span { class: "lc", {segs_rsx(l, mark)} }
                }
            }
        }
    }
}

fn render_split(
    diff: &FileDiff,
    open: &HashMap<usize, Expansion>,
    mark: Option<&str>,
    at: Option<usize>,
) -> Element {
    rsx! {
        div { class: "code split",
            for block in blocks(diff, open) {
                {split_block(diff, block, mark, at)}
            }
        }
    }
}

fn split_block(diff: &FileDiff, block: Block, mark: Option<&str>, at: Option<usize>) -> Element {
    match block {
        Block::Gap {
            index,
            hidden,
            shown,
        } => rsx! {
            GapBar { key: "g{index}", index, hidden, shown }
        },
        Block::Lines { header, from, to } => rsx! {
            div { key: "l{from}", class: "hunk",
                if let Some(h) = header {
                    {hunk_header(&h)}
                }
                for row in to_rows(&diff.lines[from..to]) {
                    div { class: "srow",
                        {split_cell(row.left, false, mark, at)}
                        {split_cell(row.right, true, mark, at)}
                    }
                }
            }
        },
    }
}
