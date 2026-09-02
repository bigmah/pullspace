use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use dioxus::prelude::*;

use std::collections::HashMap;

use crate::backend::difftool::{
    Block, Expansion, FileDiff, Line, LineKind, Mark, STEP, blocks, change_lines, diff_file,
    overview, stats, to_rows,
};
use crate::backend::highlight::{Span, highlight};
use crate::backend::images::{self, media_type};
use crate::backend::markdown;
use crate::backend::prefs::Prefs;
use crate::backend::route;
use crate::backend::search::{self, Matcher, split_word};
use crate::backend::symbols::{Symbol, enclosing};
use crate::backend::tree::ChangeKind;
use crate::backend::{FileContent, clip};

use super::app::{PrFileState, Reading, St, ViewMode, Workspace};
use super::compat;
use super::ide;
use super::imgcache::{all_settled, drawable, ensure_image};
use super::markdown::Target;
use super::prcache::ensure_path;
use super::reader::Reader;
use super::tabs::{self, TabStrip};

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

impl SourceLines {
    fn len(&self) -> usize {
        match self {
            SourceLines::Colored(l) => l.len(),
            SourceLines::Plain(l) => l.len(),
        }
    }
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
        // Read on its own rather than out of `prefs`, so that a font-size
        // nudge does not re-run the comparison of every open file.
        let ignore_ws = st.prefs.read().ignore_ws;
        let guard = data.read();
        let Pane::Ready { rel, old, new } = &*guard else {
            return None;
        };
        let status = st.statuses.read().get(rel).copied()?;
        // An added file has nothing to compare against, even if a path of the
        // same name exists in the base.
        let fresh = matches!(status, ChangeKind::Added);
        let old_text = if fresh { "" } else { old.text()? };
        Some(diff_file(old_text, new.text()?, ignore_ws))
    });

    // The pattern in the find bar, compiled once rather than per line.
    //
    // A signal written by an effect rather than a memo: a compiled regex has
    // no equality worth the name, and a memo would have to compare two of them
    // to decide whether anything downstream should redraw.
    let mut matcher: Signal<Option<Rc<Matcher>>> = use_signal(|| None);
    use_effect(move || {
        let up = *st.find_open.read();
        let text = st.find_text.read().clone();
        let opts = *st.find_opts.read();
        let next = match up && !text.trim().is_empty() {
            true => search::compile(&text, opts).ok().map(Rc::new),
            false => None,
        };
        matcher.set(next);
    });

    // Which lines that pattern is on, for the count in the bar and for the
    // keys that step between them. Against the file as it stands, whichever
    // view is drawing it — the anchors every jump lands on are the new side's
    // numbering, so that is the only numbering a hit can be reported in.
    use_effect(move || {
        let held = matcher.read();
        let lines = match (held.as_deref(), &*data.read()) {
            (Some(m), Pane::Ready { old, new, .. }) => match (new.as_ref(), old.as_ref()) {
                (FileContent::Text(t), _) => m.lines(t),
                (FileContent::Absent, FileContent::Text(t)) => m.lines(t),
                _ => Vec::new(),
            },
            _ => Vec::new(),
        };
        let mut found = st.find_lines;
        if *found.peek() != lines {
            // A different set of hits: the one being stood on is not the same
            // one it was, and pretending otherwise steps from nowhere.
            let mut at = st.find_at;
            if at.peek().is_some() {
                at.set(None);
            }
            found.set(lines);
        }
    });

    // Where the changes are, for F7. Derived here because this is the only
    // place holding the comparison, and read by the keyboard, which is not a
    // component and has no context at all.
    use_effect(move || {
        let lines = diff.read().as_ref().map(change_lines).unwrap_or_default();
        let mut steps = st.change_lines;
        if *steps.peek() != lines {
            steps.set(lines);
        }
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

    // A file arrives where it was left — and, the first time, at the top of
    // itself. The scroll container outlives the file in it, so without this,
    // opening something short after reading deep into something long lands you
    // at the bottom of it.
    //
    // Unless it was opened *at* somewhere, in which case the effect above is
    // already on its way there and this would fight it.
    //
    // The pane is read as well as the file: something still coming off the
    // network has no height to be scrolled through, and this runs again when
    // it lands.
    use_effect(move || {
        let _ = data.read();
        let Some(rel) = st.open.read().clone() else {
            return;
        };
        if st.at_line.peek().is_some() {
            return;
        }
        document::eval(&tabs::restore_js(&rel));
    });

    // Markdown's fenced blocks are highlighted where they are drawn, so the
    // pane has to be redrawn when the theme moves — the memo above only covers
    // the source view.
    let _ = st.prefs.read().theme;

    // A description being read has the pane. The file it took it from is
    // untouched underneath — still open, still in the strip, and one click on
    // its tab from having it back.
    if let Some(doc) = st.reading.read().clone() {
        return rsx! {
            div { class: "viewer",
                TabStrip {}
                Reader { doc }
            }
        };
    }

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
    // The same path again, as the key its scroll offset is filed under.
    let keep = tabs::file_key(&rel);
    let badge = status.map(|s| (s.badge(), s.css()));
    let diff_stats = diff.read().as_ref().map(stats);
    // Which contracted stretches of this file have been opened up. Read here,
    // so that opening one redraws the diff — and only the diff: the comparison
    // itself is memoised on the file's contents and does not run again.
    let open_gaps = st.open_gaps();

    // What gets picked out of the code: the identifier double-clicked, and
    // whatever the find bar is matching. Threaded all the way down rather than
    // done in CSS, because only the lines that match should pay for it.
    let word = st.selected.read().clone();
    let held_matcher = matcher.read();
    // Which hit Enter is standing on, as a line of the file.
    let find_now = st
        .find_at
        .read()
        .and_then(|i| st.find_lines.read().get(i).copied());
    let marks = Marks {
        word: word.as_deref(),
        find: held_matcher.as_deref(),
        at: find_now,
    };
    // The line a link points at, if one does. Every view that has line numbers
    // lights it up, and clicking a number is what puts it there.
    let at = *st.at_line.read();

    let body = match mode {
        ViewMode::Source => match source_lines.read().as_ref() {
            Some(SourceLines::Colored(lines)) => render_colored(lines, &marks, at),
            Some(SourceLines::Plain(lines)) => render_plain(lines, &marks, at),
            None => rsx! { div { class: "notice", "Binary file — no preview." } },
        },
        ViewMode::Inline => match diff.read().as_ref() {
            Some(d) if d.is_empty() => rsx! { div { class: "notice", "No differences." } },
            Some(d) => render_inline(d, &open_gaps, &marks, at),
            None => rsx! { div { class: "notice", "Binary file — cannot diff." } },
        },
        ViewMode::Split => match diff.read().as_ref() {
            Some(d) if d.is_empty() => rsx! { div { class: "notice", "No differences." } },
            Some(d) => render_split(d, &open_gaps, &marks, at),
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

    let find_up = *st.find_open.read();
    let ignore_ws = st.prefs.read().ignore_ws;
    // Not over a rendered page: there are no lines in one to be inside of, and
    // nothing to draw a change against.
    let sticky_on = settled && !matches!(mode, ViewMode::Preview);
    let wrap = st.prefs.read().wrap;
    let code_cls = if wrap { "codewrap wrap" } else { "codewrap" };
    // The strip beside the scrollbar. In a diff it is where the changes are;
    // reading a whole file it is where the find bar's hits are, which is the
    // only thing there is to map. (Not both at once: a hit is a line of the
    // file and a band is a row of the laid-out diff, and the two only agree
    // where nothing is folded.)
    let bands: Vec<Band> = match mode {
        ViewMode::Inline | ViewMode::Split => diff
            .read()
            .as_ref()
            .map(|d| {
                overview(d, &open_gaps, mode == ViewMode::Split)
                    .into_iter()
                    .map(Band::change)
                    .collect()
            })
            .unwrap_or_default(),
        ViewMode::Source => {
            let total = source_lines.read().as_ref().map_or(0, SourceLines::len);
            hit_bands(&st.find_lines.read(), total)
        }
        ViewMode::Preview => Vec::new(),
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
            // Every file that has been opened and not put down, so that
            // following a definition three files away costs nothing but the
            // click it takes to come back.
            TabStrip {}
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
                // Beside the diff it changes rather than buried in a menu:
                // this one alters what the reader is being shown, and a diff
                // that is quietly hiding lines is worse than a noisy one.
                if diffable {
                    button {
                        class: if ignore_ws { "sopt on" } else { "sopt" },
                        title: if ignore_ws {
                            "Not showing blocks that only changed indentation — click to show them"
                        } else {
                            "Ignore blocks that only changed indentation"
                        },
                        onclick: move |_| {
                            let now = *st.prefs.peek();
                            st.set_prefs(Prefs { ignore_ws: !now.ignore_ws, ..now });
                        },
                        "ws"
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
            // The scroller, and the three things that stand over it: the find
            // bar, the header saying what the top of the pane is inside, and
            // the strip beside the scrollbar saying where the changes are.
            div { class: "codearea",
                if find_up {
                    FindBar {}
                }
                if sticky_on {
                    StickyBar {}
                }
                if !bands.is_empty() {
                    Ruler { bands }
                }
                div {
                    class: "{code_cls}",
                    // Which file is in it, for the page's own record of where
                    // each one is scrolled to — see `super::tabs`. On the
                    // element rather than passed to the script, so that a
                    // scroll event is always filed under whatever is actually
                    // on screen.
                    "data-path": "{rel_str}",
                    "data-keep": "{keep}",
                    // A browser already knows where a word starts and ends,
                    // and a double-click is how it is asked. Wrapping every
                    // token of every line in its own clickable span to find
                    // out the same thing costs more than everything else the
                    // viewer does.
                    ondoubleclick: move |_| ide::select_word(st),
                    {pending.unwrap_or(body)}
                }
            }
        }
    }
}

// ------------------------------------------------- the strip by the scrollbar

/// One band on it: where something is in the whole laid-out view, as a
/// fraction of it.
#[derive(Clone, PartialEq)]
struct Band {
    at: f32,
    len: f32,
    cls: &'static str,
}

impl Band {
    fn change(m: Mark) -> Band {
        Band {
            at: m.at,
            len: m.len,
            cls: match m.kind {
                LineKind::Add => "add",
                LineKind::Del => "del",
                LineKind::Ctx => "ctx",
            },
        }
    }
}

/// Where the find bar's hits are in a file being read whole.
fn hit_bands(lines: &[usize], total: usize) -> Vec<Band> {
    if total == 0 {
        return Vec::new();
    }
    lines
        .iter()
        .map(|&l| Band {
            at: (l.saturating_sub(1)) as f32 / total as f32,
            len: 1.0 / total as f32,
            cls: "hit",
        })
        .collect()
}

/// A map of the file, a few pixels wide, beside the scrollbar that scrolls it.
///
/// The useful half of a minimap and none of the rest: what a reader wants off
/// the side of a long diff is *where the changes are*, not four hundred lines
/// of unreadable grey. It is inert — the scrollbar next to it is the control.
#[component]
fn Ruler(bands: Vec<Band>) -> Element {
    rsx! {
        div { class: "ruler", "aria-hidden": "true",
            for (i, b) in bands.iter().enumerate() {
                div {
                    key: "{i}",
                    class: "rband {b.cls}",
                    style: "top:{pct(b.at)}%;height:{pct(b.len)}%",
                }
            }
        }
    }
}

/// A fraction as a percentage, at a precision worth writing down.
fn pct(f: f32) -> String {
    format!("{:.3}", (f * 100.0).clamp(0.0, 100.0))
}

// ---------------------------------------------- the header over the code

/// How many levels of the chain are pinned. Three is an `impl` inside a `mod`
/// with a method in it, which is as deep as anything anybody reads gets before
/// the header is taller than the code under it.
const STICKY_DEPTH: usize = 3;

/// What the top of the pane is inside, pinned above it.
///
/// Halfway down a four-hundred-line file — or, worse, landed in the middle of
/// one by a link to a line — the question is always "what am I looking at",
/// and the answer has scrolled off the top. The `@@` header of a hunk already
/// pins itself; this is the same idea taken up a level, to the definitions the
/// hunk is written inside.
#[component]
fn StickyBar() -> Element {
    let st = use_context::<St>();
    let syms = use_context::<Memo<Rc<Vec<Symbol>>>>();
    let Some(top) = *st.top_line.read() else {
        return rsx! {};
    };
    let held = syms.read();
    let chain = enclosing(&held, top);
    if chain.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "stickybar",
            for (depth, sym) in chain.iter().take(STICKY_DEPTH).enumerate() {
                {
                    let line = sym.line;
                    let indent = 10 + depth * 12;
                    rsx! {
                        button {
                            key: "{depth}",
                            class: "stickyrow",
                            style: "padding-left:{indent}px",
                            title: "Go to line {line}",
                            onclick: move |_| st.jump_line(line),
                            span { class: "stickykind", "{sym.kind}" }
                            span { class: "stickytext", "{sym.preview}" }
                        }
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------- find, in this file

/// The bar over the code: where is this, in the file I am reading.
///
/// The box in the top bar searches the whole repository, which is the other
/// question and the one this app could already answer. The browser's own ⌘F
/// cannot stand in for this one: half of a contracted diff is not in the page
/// to be found, and what it did find it would scroll to without this app ever
/// knowing where the reader had gone.
#[component]
fn FindBar() -> Element {
    let st = use_context::<St>();
    let mut text = st.find_text;
    let mut opts = st.find_opts;
    let typed = text.read().clone();
    let count = st.find_lines.read().len();
    let at = *st.find_at.read();
    let label = match (typed.trim().is_empty(), count, at) {
        (true, ..) => String::new(),
        (_, 0, _) => "No results".to_string(),
        (_, n, Some(i)) => format!("{} of {}", i + 1, n),
        (_, n, None) => format!("{n} found"),
    };
    let bad = !typed.trim().is_empty() && count == 0;
    let cls = if bad { "findinbox bad" } else { "findinbox" };
    let now = *opts.read();
    rsx! {
        div { class: "findbar",
            input {
                class: "{cls}",
                r#type: "text",
                placeholder: "Find in this file",
                spellcheck: "false",
                autocomplete: "off",
                value: "{typed}",
                onmounted: move |e| async move {
                    let _ = e.set_focus(true).await;
                },
                oninput: move |e| {
                    text.set(e.value());
                    // A new pattern is a new set of hits, and nothing is being
                    // stood on until Enter says so.
                    let mut at = st.find_at;
                    if at.peek().is_some() {
                        at.set(None);
                    }
                },
                onkeydown: move |e| match e.key() {
                    Key::Enter => {
                        e.prevent_default();
                        st.step_find(!e.modifiers().shift());
                    }
                    // The box has the focus, so the window's own Escape only
                    // blurs it — see `ide::KEYS`. Closing is this.
                    Key::Escape => {
                        e.prevent_default();
                        st.toggle_find(false);
                    }
                    _ => {}
                },
            }
            span { class: "findcount", "{label}" }
            for (glyph, why, field) in ide::TOGGLES {
                {
                    let mut probe = now;
                    let cls = if *field(&mut probe) { "sopt on" } else { "sopt" };
                    rsx! {
                        button {
                            key: "{glyph}",
                            class: "{cls}",
                            title: "{why}",
                            onclick: move |_| {
                                let mut next = *opts.peek();
                                let slot = field(&mut next);
                                *slot = !*slot;
                                opts.set(next);
                            },
                            "{glyph}"
                        }
                    }
                }
            }
            button {
                class: "iconbtn sm",
                title: "Previous match  (⇧⏎, ⇧F3)",
                disabled: count == 0,
                onclick: move |_| st.step_find(false),
                "‹"
            }
            button {
                class: "iconbtn sm",
                title: "Next match  (⏎, F3)",
                disabled: count == 0,
                onclick: move |_| st.step_find(true),
                "›"
            }
            button {
                class: "iconbtn sm",
                title: "Close  (Esc)",
                onclick: move |_| st.toggle_find(false),
                "✕"
            }
        }
    }
}

// ------------------------------------------- what the top of the pane is on

/// Report the line at the top of the code pane, so the header above it can say
/// what that line is inside.
///
/// One listener, in the page, on the capture phase — scroll events do not
/// bubble, so this is the only way to hear one from whichever element happens
/// to be scrolling. It answers at most once a frame and only when the answer
/// has actually changed, which on a long smooth scroll is a few dozen times
/// rather than a few thousand.
///
/// `elementFromPoint` rather than a walk over the rows: the page already knows
/// what is at a coordinate, and asking it is one call where measuring five
/// thousand rows is five thousand.
const TOP_JS: &str = r#"
(function () {
  if (window.__pullspace_top) window.__pullspace_top();
  var last = null;
  var frame = 0;
  var read = function () {
    frame = 0;
    var wrap = document.querySelector('.codewrap');
    if (!wrap) { if (last !== 0) { last = 0; dioxus.send(0); } return; }
    var box = wrap.getBoundingClientRect();
    // Past whatever is already pinned over the top of it, so the answer is a
    // line of code and not the header describing one.
    var bar = document.querySelector('.stickybar');
    var skip = bar ? bar.getBoundingClientRect().height : 0;
    var no = 0;
    // A hunk's `@@` header pins itself too and carries no line, so a couple of
    // steps down are tried before giving up on the question.
    for (var i = 0; i < 4 && !no; i++) {
      var el = document.elementFromPoint(box.left + 24, box.top + skip + 6 + i * 14);
      var row = el && el.closest ? el.closest('[data-line]:not([data-line=""])') : null;
      var got = row ? parseInt(row.getAttribute('data-line'), 10) : 0;
      if (got > 0) no = got;
    }
    if (no === last) return;
    last = no;
    dioxus.send(no);
  };
  var onscroll = function () {
    if (frame) return;
    frame = requestAnimationFrame(read);
  };
  document.addEventListener('scroll', onscroll, true);
  // And once now, for a file that arrives already scrolled to where it was
  // left rather than to the top of itself.
  setTimeout(read, 120);
  window.__pullspace_top = function () {
    document.removeEventListener('scroll', onscroll, true);
  };
})();
"#;

/// Listen for it, for as long as the app is up.
pub async fn tops(st: St) {
    let mut eval = document::eval(TOP_JS);
    while let Ok(no) = eval.recv::<usize>().await {
        let mut top = st.top_line;
        let next = (no > 0).then_some(no);
        if *top.peek() != next {
            top.set(next);
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
    // With nothing picked yet, the pane is empty and the description is the
    // thing there is most reason to read first. So the offer is here as well
    // as in the conversation: this is the moment it is wanted.
    let desc = st
        .workspace
        .read()
        .header()
        .filter(|h| !h.body.trim().is_empty());
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
                if let Some(desc) = desc {
                    button {
                        class: "welcome-read",
                        title: "Read the description here, in the pane the code goes in",
                        onclick: move |_| {
                            st.read_doc(Reading {
                                title: desc.title.clone(),
                                meta: format!("#{} \u{00b7} {}", desc.number, desc.author),
                                body: desc.body.trim().to_string(),
                                url: desc.html_url.clone(),
                            })
                        },
                        "Read the description"
                    }
                }
            }
        }
    }
}

/// What gets picked out of the code as it is drawn.
///
/// Two questions at once, and they are different questions: `word` is the
/// identifier somebody double-clicked, marked wherever it appears; `find` is
/// the pattern in the bar over the file, which is being stepped through. Both
/// can be on, and where they land on the same text the find wins — it is the
/// one with a cursor in it.
#[derive(Clone, Copy, Default)]
struct Marks<'a> {
    word: Option<&'a str>,
    find: Option<&'a Matcher>,
    /// The line the find is standing on, so that one match out of two hundred
    /// can be told apart from the rest.
    at: Option<usize>,
}

impl Marks<'_> {
    /// Where the find matches on one line, worked out over the whole of it.
    ///
    /// The whole line and not each coloured span of it, because a search for
    /// `fn main` lies across two spans and matching them one at a time would
    /// quietly find nothing. It costs joining the line back up, which is why
    /// it only happens while the bar is up.
    fn ranges<'a>(&self, parts: impl Iterator<Item = &'a str>) -> Vec<(usize, usize)> {
        let Some(find) = self.find else {
            return Vec::new();
        };
        let parts: Vec<&str> = parts.collect();
        match parts.as_slice() {
            [] => Vec::new(),
            // A line the highlighter did not cut up — which is most lines of a
            // diff — is matched where it stands, without being copied to be
            // put back together.
            [one] => find.ranges(one),
            many => find.ranges(&many.concat()),
        }
    }
}

/// One line's worth of the above: the ranges settled, and whether this is the
/// line the find is standing on.
#[derive(Clone, Copy)]
struct Line1<'a> {
    word: Option<&'a str>,
    ranges: &'a [(usize, usize)],
    now: bool,
}

/// One run of text, with everything that has been asked for picked out of it.
///
/// `at` is where this run starts in its line, because the find's ranges are
/// offsets into the whole line — see [`Marks::ranges`].
///
/// The short-circuits are the whole design: with nothing selected and no find
/// up, or on the overwhelming majority of lines that match neither, this is
/// the single span it always was. Only the handful of lines that actually
/// match pay for being split up.
fn marked(text: &str, at: usize, color: Option<&str>, m: Line1<'_>) -> Element {
    let style = color.map(|c| format!("color:{c}")).unwrap_or_default();
    if m.ranges.is_empty() {
        return occurrences(text, &style, m.word);
    }
    let hit_cls = if m.now { "fhit now" } else { "fhit" };
    rsx! {
        for (i, (piece, hit)) in search::cut(text, at, m.ranges).into_iter().enumerate() {
            if hit {
                span { key: "{i}", class: "{hit_cls}", style: "{style}", "{piece}" }
            } else {
                span { key: "{i}", {occurrences(piece, &style, m.word)} }
            }
        }
    }
}

/// The other half: whole-word occurrences of the selected identifier.
fn occurrences(text: &str, style: &str, word: Option<&str>) -> Element {
    let Some(name) = word.filter(|n| text.contains(*n)) else {
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

fn render_colored(lines: &[Vec<Span>], m: &Marks<'_>, at: Option<usize>) -> Element {
    rsx! {
        div { class: "code",
            for (i, spans) in lines.iter().enumerate() {
                {
                    let no = i + 1;
                    let ranges = m.ranges(spans.iter().map(|s| s.text.as_str()));
                    let one = Line1 { word: m.word, ranges: &ranges, now: m.at == Some(no) };
                    // Where each span starts in the line, so the find's
                    // ranges — which are offsets into the whole of it — land
                    // in the right place.
                    let mut off = 0;
                    rsx! {
                        div { class: row_class(no, at), id: "L{no}", "data-line": "{no}",
                            span { class: "ln lnk", "{no}" }
                            span { class: "lc",
                                for sp in spans {
                                    {
                                        let starts = off;
                                        off += sp.text.len();
                                        marked(&sp.text, starts, Some(&sp.color), one)
                                    }
                                }
                            }
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

fn render_plain(lines: &[String], m: &Marks<'_>, at: Option<usize>) -> Element {
    rsx! {
        div { class: "code",
            for (i, text) in lines.iter().enumerate() {
                {
                    let no = i + 1;
                    let ranges = m.ranges(std::iter::once(text.as_str()));
                    let one = Line1 { word: m.word, ranges: &ranges, now: m.at == Some(no) };
                    rsx! {
                        div { class: row_class(no, at), id: "L{no}", "data-line": "{no}",
                            span { class: "ln lnk", "{no}" }
                            span { class: "lc", {marked(text, 0, None, one)} }
                        }
                    }
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
fn segs_rsx(l: &Line, m: &Marks<'_>) -> Element {
    let ranges = m.ranges(l.segs.iter().map(|s| s.text.as_str()));
    let one = Line1 {
        word: m.word,
        ranges: &ranges,
        now: l.new_no.is_some() && l.new_no == m.at,
    };
    let mut off = 0;
    rsx! {
        for (i, seg) in l.segs.iter().enumerate() {
            {
                let starts = off;
                off += seg.text.len();
                if seg.emph {
                    rsx! { span { key: "{i}", class: "emph", {marked(&seg.text, starts, None, one)} } }
                } else {
                    marked(&seg.text, starts, None, one)
                }
            }
        }
    }
}

fn inline_line(l: &Line, m: &Marks<'_>, at: Option<usize>) -> Element {
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
    // The head commit's numbering is what the pinned header and the scroll
    // report are worked out from; a removed line is not a line of the file the
    // reader is standing in.
    let line_attr = l.new_no.map(|n| n.to_string()).unwrap_or_default();
    rsx! {
        div { class: "{cls}", "data-line": "{line_attr}",
            {anchor(l.new_no)}
            span { class: "ln", "{old}" }
            span { class: "{new_cls}", "{new}" }
            span { class: "dsign", "{sign}" }
            span { class: "lc", {segs_rsx(l, m)} }
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
    m: &Marks<'_>,
    at: Option<usize>,
) -> Element {
    rsx! {
        div { class: "code inline",
            for block in blocks(diff, open) {
                {inline_block(diff, block, m, at)}
            }
        }
    }
}

fn inline_block(diff: &FileDiff, block: Block, m: &Marks<'_>, at: Option<usize>) -> Element {
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
                    {inline_line(l, m, at)}
                }
            }
        },
    }
}

fn split_cell(l: Option<&Line>, right: bool, m: &Marks<'_>, at: Option<usize>) -> Element {
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
                    span { class: "lc", {segs_rsx(l, m)} }
                }
            }
        }
    }
}

fn render_split(
    diff: &FileDiff,
    open: &HashMap<usize, Expansion>,
    m: &Marks<'_>,
    at: Option<usize>,
) -> Element {
    rsx! {
        div { class: "code split",
            for block in blocks(diff, open) {
                {split_block(diff, block, m, at)}
            }
        }
    }
}

fn split_block(diff: &FileDiff, block: Block, m: &Marks<'_>, at: Option<usize>) -> Element {
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
                    // The row and not the cells: the left one is the base
                    // side and has no line of the file being read, so a probe
                    // landing in it would find an empty answer and stop
                    // looking. See `TOP_JS`.
                    div {
                        class: "srow",
                        "data-line": "{row.right.and_then(|l| l.new_no).map(|n| n.to_string()).unwrap_or_default()}",
                        {split_cell(row.left, false, m, at)}
                        {split_cell(row.right, true, m, at)}
                    }
                }
            }
        },
    }
}
