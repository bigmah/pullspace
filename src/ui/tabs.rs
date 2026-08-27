//! The strip of open files above the code.
//!
//! A tab is a file somebody has not finished with. Following a definition
//! three files away used to cost you the file it was called from: the viewer
//! showed one thing, and Back was the only way home. Here what is being left
//! stays open beside what is being opened, and comes back as it was — the same
//! view, the same line picked out of it, and scrolled to the same place.
//!
//! The first two of those are the tab's own business and are kept with it, in
//! [`OpenTab`](super::app::OpenTab). The third is this module's, and it is kept
//! in the page rather than in Rust: where a file is scrolled to changes with
//! every notch of the wheel, and a signal written sixty times a second is sixty
//! renders of everything that reads it. The offsets live in a JS object
//! instead, filed under the path the scroll container says it is showing — so
//! an offset cannot be recorded against the wrong file, however a render and a
//! scroll event happen to interleave.

use std::path::{Path, PathBuf};

use dioxus::html::input_data::MouseButton;
use dioxus::prelude::*;

use crate::backend::tree::ChangeKind;

use super::app::St;

/// Where each open file is scrolled to, kept in the page.
///
/// Installed once, for as long as the app is up. The listener is in the capture
/// phase because a `scroll` event does not bubble — one listener there sees
/// every one, which is the difference between this and a handler on a container
/// that is rebuilt every time a file opens.
const TOPS_JS: &str = r#"
(function () {
  // A reload of this page's script should not leave the last listener behind.
  if (window.__pullspace_tops) window.__pullspace_tops();
  var at = {};
  // A scroll the app itself asked for is not a fact about where anybody was
  // reading. It arrives a frame or so after the assignment that caused it, and
  // while a file is still on its way here it is a clamped 0 — which would
  // overwrite the very offset being restored.
  var hush = 0;
  var on = function (e) {
    var el = e.target;
    if (!el || !el.classList || !el.classList.contains('codewrap')) return;
    if (Date.now() < hush) return;
    var path = el.getAttribute('data-path');
    if (path) at[path] = el.scrollTop;
  };
  document.addEventListener('scroll', on, true);
  window.__pullspace_at = function (path) { return at[path]; };
  window.__pullspace_put = function (el, top) {
    hush = Date.now() + 200;
    el.scrollTop = top;
  };
  window.__pullspace_drop = function (path) {
    if (path === null) { at = {}; } else { delete at[path]; }
  };
  window.__pullspace_tops = function () {
    document.removeEventListener('scroll', on, true);
  };
})();
"#;

/// Start remembering. Called once, from the root.
pub fn watch() {
    document::eval(TOPS_JS);
}

/// A string as JavaScript source. Paths are not expected to hold a quote, but
/// what goes into a `document::eval` is source code and has to be written as
/// such — and JSON's string is a JS string.
fn quoted(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| String::from("\"\""))
}

/// Forget where one file was scrolled to — its tab has been closed, and the
/// next time it opens it opens fresh.
pub fn forget(rel: &Path) {
    document::eval(&format!(
        "if (window.__pullspace_drop) window.__pullspace_drop({});",
        quoted(&rel.display().to_string())
    ));
}

/// Forget all of them, which is what closing a pull request does.
pub fn forget_all() {
    document::eval("if (window.__pullspace_drop) window.__pullspace_drop(null);");
}

/// Put a file back where it was left — or at the top of it, the first time.
///
/// The file may not be on screen yet: it can still be coming off the network,
/// and a pane with nothing in it cannot be scrolled anywhere. So this keeps
/// asking until the pane is tall enough to take the offset it is being handed.
/// It gives up the moment the pane is showing a different file, and the moment
/// the reader scrolls it themselves — a restore that fought the wheel would be
/// worse than one that never happened.
pub fn restore_js(rel: &Path) -> String {
    let path = quoted(&rel.display().to_string());
    format!(
        r#"(function(){{
  if (!window.__pullspace_put) return;
  var path = {path};
  var want = window.__pullspace_at(path);
  if (want === undefined) want = 0;
  var n = 0, last = -1;
  function go() {{
    var e = document.querySelector('.codewrap');
    if (!e || e.getAttribute('data-path') !== path) return;
    if (last >= 0 && Math.abs(e.scrollTop - last) > 1) return;
    window.__pullspace_put(e, want);
    last = e.scrollTop;
    if (last < want - 1 && n++ < 40) setTimeout(go, 75);
  }}
  go();
}})();"#
    )
}

/// The last `n` components of a path, written out.
fn tail(path: &Path, n: usize) -> String {
    let parts: Vec<_> = path.components().collect();
    let from = parts.len().saturating_sub(n);
    parts[from..]
        .iter()
        .collect::<PathBuf>()
        .display()
        .to_string()
}

/// What one tab is called.
///
/// The file's name, because that is what anybody looking for it is looking for
/// — and, where two open files are both called `mod.rs`, as much of the path in
/// front of it as it takes to tell this one from the others.
fn label(path: &Path, others: &[&Path]) -> String {
    let depth = path.components().count();
    for n in 1..=depth {
        let mine = tail(path, n);
        if others.iter().all(|o| tail(o, n) != mine) {
            return mine;
        }
    }
    path.display().to_string()
}

/// A changed file's colour, so that the strip says the same thing about a file
/// as the row in the explorer it was opened from.
fn tint(kind: Option<ChangeKind>) -> &'static str {
    match kind {
        Some(k) => k.css(),
        None => "",
    }
}

#[component]
pub fn TabStrip() -> Element {
    let st = use_context::<St>();

    // Whatever is on screen has to be a tab that can be seen. Back, ⌥↓ and a
    // definition three files away all land on tabs that were never clicked,
    // and any of them can be off the end of a strip that has scrolled.
    use_effect(move || {
        let _ = st.open.read();
        let _ = st.reading.read();
        document::eval(
            "var e=document.querySelector('.tab.on');\
             if(e) e.scrollIntoView({block:'nearest',inline:'nearest'});",
        );
    });

    let tabs = st.tabs.read();
    // What is being read in the pane, when it is prose rather than a file. It
    // gets a tab of its own at the head of the strip: it is a thing that is
    // open, and the strip is what says what is open.
    let doc = st.reading.read().as_ref().map(|d| d.title.clone());
    if tabs.is_empty() && doc.is_none() {
        return rsx! {};
    }
    // A file's tab is the one being read only while nothing else has the pane.
    let here = match doc.is_some() {
        true => None,
        false => st.open.read().clone(),
    };
    let statuses = st.statuses.read();
    let paths: Vec<&Path> = tabs.iter().map(|t| t.at.path.as_path()).collect();

    rsx! {
        div { class: "tabstrip",
            if let Some(title) = doc {
                div {
                    class: "tab doc on",
                    title: "{title}",
                    span { class: "tname", "{title}" }
                    button {
                        class: "tabx",
                        title: "Close  (\u{2325}W)",
                        onclick: move |e| {
                            e.stop_propagation();
                            st.stop_reading();
                        },
                        "\u{00d7}"
                    }
                }
            }
            for (i, tab) in tabs.iter().enumerate() {
                {
                    let path = tab.at.path.clone();
                    let full = path.display().to_string();
                    let rivals: Vec<&Path> = paths
                        .iter()
                        .enumerate()
                        .filter(|(j, _)| *j != i)
                        .map(|(_, p)| *p)
                        .collect();
                    let name = label(&path, &rivals);
                    let on = here.as_deref() == Some(path.as_path());
                    let cls = if on { "tab on" } else { "tab" };
                    let name_cls = format!("tname {}", tint(statuses.get(&path).copied()));
                    // One clone each: the row's handlers and the close
                    // button's are three separate closures, and every one of
                    // them outlives this iteration.
                    let (go, aux, shut) = (path.clone(), path.clone(), path.clone());
                    rsx! {
                        div {
                            key: "{full}",
                            class: "{cls}",
                            title: "{full}",
                            onclick: move |_| st.open_file(go.clone()),
                            // A middle click closes, as it does on every other
                            // strip of tabs anybody has used. The press is
                            // what the browser would otherwise turn into its
                            // own scroll gesture.
                            onmousedown: move |e| {
                                if e.trigger_button() == Some(MouseButton::Auxiliary) {
                                    e.prevent_default();
                                }
                            },
                            onmouseup: move |e| {
                                if e.trigger_button() == Some(MouseButton::Auxiliary) {
                                    st.close_tab(&aux);
                                }
                            },
                            span { class: "{name_cls}", "{name}" }
                            button {
                                class: "tabx",
                                title: "Close  (⌥W)",
                                onclick: move |e| {
                                    // The tab under it would otherwise open
                                    // the file on the way to closing it.
                                    e.stop_propagation();
                                    st.close_tab(&shut);
                                },
                                "×"
                            }
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

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn a_name_that_stands_alone_is_the_whole_label() {
        let others = [p("src/ui/viewer.rs")];
        let others: Vec<&Path> = others.iter().map(|p| p.as_path()).collect();
        assert_eq!(label(&p("src/ui/app.rs"), &others), "app.rs");
    }

    #[test]
    fn a_clash_takes_the_directory_with_it() {
        let others = [p("src/backend/mod.rs")];
        let others: Vec<&Path> = others.iter().map(|p| p.as_path()).collect();
        assert_eq!(label(&p("src/ui/mod.rs"), &others), "ui/mod.rs");
    }

    #[test]
    fn and_keeps_taking_until_it_is_told_apart() {
        let others = [p("b/ui/mod.rs"), p("src/backend/mod.rs")];
        let others: Vec<&Path> = others.iter().map(|p| p.as_path()).collect();
        assert_eq!(label(&p("a/ui/mod.rs"), &others), "a/ui/mod.rs");
    }

    #[test]
    fn a_file_at_the_root_has_nothing_to_take() {
        let others = [p("README.md")];
        let others: Vec<&Path> = others.iter().map(|p| p.as_path()).collect();
        // Nothing distinguishes them, and the label is still the file: two
        // tabs on one path is not a state the strip can be in.
        assert_eq!(label(&p("README.md"), &others), "README.md");
    }
}
