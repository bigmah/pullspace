//! pullspace inside another page, reviewing the files that page hands it — see
//! [`crate::backend::host`].
//!
//! Embedded, there is no GitHub, no address bar to follow and no spaces: one
//! comparison is open from start to finish, read again whenever the host says
//! its files moved.
//!
//! And the frame is whatever size the host gives it — half a window beside a
//! conversation, or the whole of a phone. So this is also where the app learns
//! to be narrow: the explorer folds away behind a button, and a diff that has
//! no room for two columns is drawn as one.

use std::path::{Path, PathBuf};

use dioxus::prelude::*;
use pullspace_embed::protocol;
use serde::Deserialize;

use crate::backend::host;
use crate::backend::tree::changed_paths;

use super::app::{Fetch, PrFileState, St, ViewMode};

/// At this width and under, the explorer no longer fits beside the code: it
/// comes out over it instead, from the button at the head of the bar.
///
/// The stylesheet says the same number in its `max-width` query. Change one
/// and change the other.
pub const NARROW: f64 = 720.0;

/// The narrowest pane a side-by-side diff is worth drawing in: two gutters and
/// some forty characters a side. Under it, one column says the same thing
/// without a scrollbar under each half.
const SPLIT_FITS: f64 = 700.0;

/// The host's events, for as long as the app is up. Only from the parent, and
/// only from a page of this one's origin: anything else can post here too.
///
/// It says `ready` once it is listening. An event posted before that is one
/// nobody heard, and a host with something to say first — open this file —
/// has no other way to know when it can.
const EVENTS_JS: &str = r#"
(function () {
  if (window.__pullspace_host) window.__pullspace_host();
  var on = function (e) {
    if (e.source !== window.parent || e.origin !== location.origin) return;
    var d = e.data;
    if (d && d.ps === 1 && typeof d.event === 'string') {
      dioxus.send([d.event, d.body === undefined ? null : d.body]);
    }
  };
  window.addEventListener('message', on);
  window.__pullspace_host = function () { window.removeEventListener('message', on); };
  if (window.parent && window.parent !== window) {
    window.parent.postMessage({ ps: 1, t: 'ready' }, location.origin);
  }
})();
"#;

/// What is selected in the code pane, and the lines it runs between — read off
/// the anchors the rows already carry, so they are the head commit's numbers
/// in every view.
const SELECTION_JS: &str = r#"
var sel = window.getSelection();
if (!sel || sel.isCollapsed || !sel.rangeCount) return null;
var range = sel.getRangeAt(0);
var pane = document.querySelector('.codewrap');
if (!pane || !pane.contains(range.commonAncestorContainer)) return null;
var text = sel.toString();
if (!text.trim()) return null;
var line = function (node) {
  var el = node.nodeType === 1 ? node : node.parentElement;
  var row = el && el.closest ? el.closest('.cl, .scell') : null;
  if (!row) return null;
  var id = row.id || ((row.querySelector('.anchor') || {}).id || '');
  return id.charAt(0) === 'L' ? (parseInt(id.slice(1), 10) || null) : null;
};
return { text: text, start: line(range.startContainer), end: line(range.endContainer) };
"#;

#[derive(Deserialize)]
struct Selected {
    text: String,
    start: Option<usize>,
    end: Option<usize>,
}

impl St {
    /// Whether the frame is too narrow to keep the explorer beside the code.
    ///
    /// Reactive, and read through a memo wherever it is read: the size it
    /// comes from moves on every frame of a resize, and the answer moves once.
    pub fn narrow(&self) -> bool {
        let (w, _) = *self.main_size.read();
        host::embedded() && w > 0.0 && w <= NARROW
    }

    /// Whether the code pane has the width for a diff's two sides.
    ///
    /// Only ever no when embedded: on its own, pullspace has the window, and
    /// how wide that is is the reader's own business.
    pub fn split_fits(&self) -> bool {
        if !host::embedded() {
            return true;
        }
        let (w, _) = *self.main_size.read();
        if w <= 0.0 {
            return true;
        }
        let beside = w > NARROW && !*self.side_shut.read();
        let side = if beside { *self.side_w.read() } else { 0.0 };
        w - side >= SPLIT_FITS
    }

    /// The button at the head of the bar: the explorer, away and back.
    ///
    /// Two different things by width, and so two flags. Beside the code it is
    /// there until it is shut; over the code it is shut until it is asked for.
    pub fn toggle_side(&self) {
        let narrow = {
            let (w, _) = *self.main_size.peek();
            w > 0.0 && w <= NARROW
        };
        let mut flag = if narrow { self.drawer } else { self.side_shut };
        let now = *flag.peek();
        flag.set(!now);
    }

    /// Put the explorer away again, where it is lying over the code — which
    /// is what picking a file out of it is asking for.
    pub fn close_drawer(&self) {
        let mut drawer = self.drawer;
        if *drawer.peek() {
            drawer.set(false);
        }
    }

    /// The file a path from outside names, if it names one.
    ///
    /// The host knows the path the way its own tools wrote it: relative to the
    /// repository, absolute, or from a directory inside it. So what is opened
    /// is the file here whose path is the end of the one given, or — for a
    /// path given short — the only file here that ends with it.
    fn resolve(&self, given: &str) -> Option<PathBuf> {
        let given = given.trim().replace('\\', "/");
        let given = given.trim_start_matches("./");
        if given.is_empty() {
            return None;
        }
        let held = self.workspace.peek();
        let view = held.compare()?;
        // The working tree first; a file the change deletes is only in the
        // other one.
        for tree in [&view.tree, &view.base_tree] {
            if let Some(found) = name_file(tree.paths(), given) {
                return Some(found);
            }
        }
        None
    }

    /// Show the file the host asked for. `false` when it named nothing here.
    fn open_for_host(&self, want: &protocol::Open) -> bool {
        let Some(path) = self.resolve(&want.path) else {
            return false;
        };
        self.close_drawer();
        match want.line {
            Some(line) if !want.diff => self.open_at(path, line),
            _ => {
                // As a click on its row would: its changes if it has any,
                // drawn or as source if it has none.
                self.open_file(path);
                if let Some(line) = want.line {
                    self.jump_line(line);
                }
            }
        }
        true
    }
}

/// Which of `paths` the string names — see [`St::resolve`].
fn name_file<'a>(paths: impl Iterator<Item = &'a Path>, given: &str) -> Option<PathBuf> {
    let mut longest: Option<&Path> = None;
    let mut ending: Option<&Path> = None;
    let mut several = false;
    for path in paths {
        let Some(text) = path.to_str() else { continue };
        if text == given {
            return Some(path.to_path_buf());
        }
        // The given path is longer: an absolute one, with this on the end.
        if given.len() > text.len()
            && given.ends_with(text)
            && given.as_bytes()[given.len() - text.len() - 1] == b'/'
            && longest.is_none_or(|l| l.as_os_str().len() < text.len())
        {
            longest = Some(path);
        }
        // Or shorter: written from inside a directory of this repository.
        if text.len() > given.len()
            && text.ends_with(given)
            && text.as_bytes()[text.len() - given.len() - 1] == b'/'
        {
            several |= ending.is_some();
            ending = Some(path);
        }
    }
    longest
        .or(ending.filter(|_| !several))
        .map(Path::to_path_buf)
}

/// Open what the host has, open it again each time it says it changed, and
/// show whichever file it asks for.
pub async fn run(st: St) {
    // Listening before the first read, so a change made while it is out is
    // not missed.
    let mut events = document::eval(EVENTS_JS);
    load(st).await;
    while let Ok((event, body)) = events.recv::<(String, serde_json::Value)>().await {
        match event.as_str() {
            protocol::CHANGED => load(st).await,
            protocol::OPEN => {
                let Ok(want) = serde_json::from_value::<protocol::Open>(body.clone()) else {
                    continue;
                };
                // Asked for before there was anything to look it up in: read
                // the workspace first, which is what it was waiting on.
                if !st.workspace.peek().is_open() {
                    load(st).await;
                }
                if !st.open_for_host(&want) {
                    host::post(protocol::MISSING, body);
                }
            }
            _ => {}
        }
    }
}

/// Read both sides from the host and show them.
pub async fn load(st: St) {
    let mut fetch = st.fetch;
    fetch.set(Fetch::Working("reading the workspace".to_string()));
    let snap = match host::snapshot().await {
        Ok(snap) => snap,
        Err(e) => {
            fetch.set(Fetch::Failed(format!("{e:#}")));
            return;
        }
    };
    fetch.set(Fetch::Idle);
    let view = host::compare_view(snap);
    let arriving = {
        let held = st.workspace.peek();
        match held.compare() {
            // Nothing moved: a reload would only throw away what is decoded.
            Some(open) if open.tree == view.tree && open.base_tree == view.base_tree => return,
            Some(_) => false,
            None => true,
        }
    };
    let changed = !view.files.is_empty();
    st.enter_compare(view);
    if !arriving {
        return;
    }
    // The first time, what changed is what there is to review: the explorer
    // opens on those files alone, and the code on the first of them — the
    // first as the explorer lists them, which is the order the stepper walks.
    // With nothing changed there is only the repository to read, so it is the
    // whole of that instead.
    let mut only = st.changes_only;
    only.set(changed);
    let first = {
        let held = st.workspace.peek();
        held.compare().and_then(|view| {
            let tree = crate::backend::tree::build_tree_from_paths(
                "",
                view.tree.paths(),
                &st.statuses.peek(),
            );
            let order = changed_paths(&tree);
            // Past any that only moved, where there is another: a file with
            // nothing changed in it is an empty pane to arrive at.
            order
                .iter()
                .find(|path| held.moved_from(path).is_none())
                .or(order.first())
                .cloned()
        })
    };
    if let Some(path) = first {
        st.open_file(path);
    }
}

/// What the reader is asking the host about, quoted for it.
///
/// Whatever is most specific: the code they selected, or the line they picked
/// by its number, or — with neither — the file itself, which is a question too.
pub async fn quote(st: St) {
    let Some(path) = st.open.peek().clone() else {
        return;
    };
    let selected = document::eval(SELECTION_JS)
        .join::<Option<Selected>>()
        .await
        .ok()
        .flatten();
    let picked = *st.at_line.peek();
    let (line, end, text) = match selected {
        Some(sel) => {
            let (a, b) = match (sel.start, sel.end) {
                (Some(a), Some(b)) => (Some(a.min(b)), Some(a.max(b)).filter(|_| a != b)),
                (a, b) => (a.or(b), None),
            };
            (a, b, Some(sel.text))
        }
        None => (picked, None, picked.and_then(|n| line_text(&st, &path, n))),
    };
    let quote = protocol::Quote {
        path: path.display().to_string(),
        line,
        end,
        text,
    };
    if let Ok(body) = serde_json::to_value(quote) {
        host::post(protocol::QUOTE, body);
    }
}

/// One line of the open file, as the side being shown has it.
fn line_text(st: &St, path: &Path, line: usize) -> Option<String> {
    match st.pr_files.peek().get(path) {
        Some(PrFileState::Ready { base, head }) => head
            .text()
            .filter(|t| !t.is_empty())
            .or_else(|| base.text())
            .and_then(|t| t.lines().nth(line.saturating_sub(1)))
            .map(str::to_string),
        _ => None,
    }
}

/// The view a diff is drawn in, given the one that was asked for: side by side
/// only where there is the width for two sides.
pub fn fitting(mode: ViewMode, fits: bool) -> ViewMode {
    match mode {
        ViewMode::Split if !fits => ViewMode::Inline,
        m => m,
    }
}

/// What stands in for the app until the host has answered.
#[component]
pub fn Waiting() -> Element {
    let st = use_context::<St>();
    let failed = match &*st.fetch.read() {
        Fetch::Failed(e) => Some(e.clone()),
        _ => None,
    };
    rsx! {
        div { class: "opening",
            div { class: "opening-col",
                div { class: "opening-brand", "pullspace" }
                if let Some(e) = failed {
                    div { class: "opening-note", "Couldn't read the workspace: {e}" }
                    button {
                        class: "opening-stop",
                        onclick: move |_| {
                            spawn(load(st));
                        },
                        "Try again"
                    }
                } else {
                    div { class: "opening-bar" }
                    div { class: "opening-note", "Reading the workspace…" }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(given: &str) -> Option<String> {
        let paths = [
            "README.md",
            "src/web/app.js",
            "src/web/files.rs",
            "web/client/src/app.js",
            "docs/web.md",
        ];
        name_file(paths.iter().map(Path::new), given).map(|p| p.display().to_string())
    }

    /// A host says a path the way its tools wrote it, which is rarely the way
    /// the repository lists it.
    #[test]
    fn a_path_from_outside_names_the_file_it_ends_in() {
        assert_eq!(
            named("src/web/files.rs").as_deref(),
            Some("src/web/files.rs")
        );
        // Absolute: the longest file that is its tail, not the first.
        assert_eq!(
            named("/Users/t/dev/tamale/src/web/app.js").as_deref(),
            Some("src/web/app.js")
        );
        assert_eq!(
            named("/Users/t/dev/tamale/web/client/src/app.js").as_deref(),
            Some("web/client/src/app.js")
        );
        // Short, from inside a directory: only when one file ends that way.
        assert_eq!(named("files.rs").as_deref(), Some("src/web/files.rs"));
        assert_eq!(named("app.js"), None, "two files end in it");
        // And never half a name.
        assert_eq!(named("iles.rs"), None);
        assert_eq!(named("b/web.md"), None);
        assert_eq!(named(""), None);
    }

    #[test]
    fn a_diff_is_side_by_side_only_where_two_sides_fit() {
        assert_eq!(fitting(ViewMode::Split, true), ViewMode::Split);
        assert_eq!(fitting(ViewMode::Split, false), ViewMode::Inline);
        assert_eq!(fitting(ViewMode::Source, false), ViewMode::Source);
    }
}
