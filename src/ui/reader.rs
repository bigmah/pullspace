//! A description, given the middle pane to be read in.
//!
//! A pull request's description is the one thing in a review that is *prose* —
//! written to be read start to finish, often at length, and usually the only
//! account of why any of the diff is the way it is. The column on the right is
//! 380 pixels wide and holds a conversation; a description of any size read
//! there is a ribbon of text with a scrollbar down it.
//!
//! So it can be opened here instead, where the code usually is: one column at
//! a measure, its headings down the side, and the file it took the pane from
//! still in the strip above waiting to be clicked back. Nothing about the
//! document changes on the way — the same parse and the same renderer as the
//! pane on the right, in a frame that gives it room. See
//! [`crate::backend::markdown`] for why none of it is ever handed to the page
//! as markup.

use std::path::Path;

use dioxus::prelude::*;

use crate::backend::auth::open_browser;
use crate::backend::markdown::{self, Entry, Refs};

use super::app::{Reading, St};

/// A comment's links are written from the root of the repository — there is no
/// file they are relative to, the way a README's are.
const ROOT: &str = "";

/// How far down the document the reader is, and which section they are in.
///
/// Both are read off one scroll container, and neither is worth a render: the
/// bar moves on every frame of a scroll, and the outline's mark moves on every
/// section boundary. So they are written straight onto the two elements that
/// show them, from a listener in the capture phase — a `scroll` event does not
/// bubble, and one listener there sees every one however often the document
/// under it is rebuilt.
const READ_JS: &str = r#"
(function () {
  // A reload of this page's script should not leave the last listener behind.
  if (window.__pullspace_read) window.__pullspace_read();
  var pending = false;
  function frame() {
    pending = false;
    var box = document.querySelector('.rdscroll');
    if (!box) return;
    var span = box.scrollHeight - box.clientHeight;
    var bar = document.querySelector('.rdprog');
    // A document that fits has been read to the end by virtue of being on
    // screen; a bar stuck at zero would say the opposite.
    if (bar) bar.style.width = (span > 8 ? Math.min(1, box.scrollTop / span) * 100 : 100).toFixed(2) + '%';
    var rows = document.querySelectorAll('.rdtocrow');
    if (!rows.length) return;
    // The section being read is the last heading above the fold — with a
    // little slack, so a heading level with the top of the pane counts as the
    // one you are in rather than the one you are about to reach.
    var heads = box.querySelectorAll('.mdh[id]');
    var top = box.getBoundingClientRect().top + 72;
    var at = heads.length ? heads[0].id : '';
    for (var i = 0; i < heads.length; i++) {
      if (heads[i].getBoundingClientRect().top > top) break;
      at = heads[i].id;
    }
    for (var j = 0; j < rows.length; j++) {
      rows[j].classList.toggle('on', rows[j].getAttribute('data-at') === at);
    }
  }
  var on = function (e) {
    var el = e.target;
    if (!el || !el.classList || !el.classList.contains('rdscroll')) return;
    if (pending) return;
    pending = true;
    requestAnimationFrame(frame);
  };
  document.addEventListener('scroll', on, true);
  window.__pullspace_read = function () {
    document.removeEventListener('scroll', on, true);
  };
  // The document is not in the page yet on the render that installs this.
  setTimeout(frame, 60);
  setTimeout(frame, 300);
})();
"#;

/// Put the reader back at the top. Opening a second description into a pane
/// scrolled two thousand pixels down would otherwise open it in the middle.
const TOP_JS: &str = "var e=document.querySelector('.rdscroll'); if(e) e.scrollTop=0;";

#[component]
pub fn Reader(doc: Reading) -> Element {
    let st = use_context::<St>();

    // `#123` and `@name` in a description are references, and this is what
    // they are references to. Peeked: which repository is open cannot change
    // without the description going with it.
    let refs = st
        .workspace
        .peek()
        .repo_ref()
        .map(|r| Refs::of(r.to_string()))
        .unwrap_or_default();
    // Parsed on the render, not memoised: the props are compared before this
    // component is re-run at all, so this happens once per document opened.
    let parsed = markdown::parse_refs(&doc.body, &refs);
    let outline = markdown::outline(&parsed.blocks);

    // A description opened into a pane scrolled two thousand pixels down
    // would otherwise open in the middle of itself.
    use_effect(use_reactive!(|doc| {
        // The document itself is not wanted here — only the fact that it has
        // changed, which is what this effect is subscribed to.
        let _ = &doc;
        document::eval(TOP_JS);
    }));

    // And the bar and the outline's mark are worked out again whenever what
    // they measure has moved: a new document, the contents coming or going,
    // the column widening. All three change how long the document is without
    // anybody scrolling, which is the one thing the listener never sees.
    use_effect(use_reactive!(|doc| {
        let _ = &doc;
        let _ = st.read_toc.read();
        let _ = st.read_wide.read();
        document::eval(READ_JS);
    }));

    let mut wide = st.read_wide;
    let mut toc = st.read_toc;
    // Nothing to draw a contents from is nothing to offer one for: a
    // description with no headings would get an empty column down its side.
    let has_toc = outline.len() > 1;
    let showing_toc = has_toc && *toc.read();
    let url = doc.url.clone();
    let linkable = !url.is_empty();

    rsx! {
        div { class: "reader",
            div { class: "viewhdr rdhdr",
                span { class: "rdtitle", title: "{doc.title}", "{doc.title}" }
                if !doc.meta.is_empty() {
                    span { class: "rdmeta", "{doc.meta}" }
                }
                span { class: "spacer" }
                if has_toc {
                    button {
                        class: if showing_toc { "modebtn on" } else { "modebtn" },
                        title: "Show this description's headings down the side",
                        onclick: move |_| toc.toggle(),
                        "Contents"
                    }
                }
                button {
                    class: if *wide.read() { "modebtn on" } else { "modebtn" },
                    title: "Set the text to the full width of the pane rather than to a reading measure",
                    onclick: move |_| wide.toggle(),
                    "Wide"
                }
                if linkable {
                    button {
                        class: "iconbtn sm",
                        title: "Open on github.com",
                        onclick: move |_| open_browser(&url),
                        "\u{2197}"
                    }
                }
                button {
                    class: "iconbtn sm",
                    title: "Close, and give the pane back to the file  (\u{2325}W)",
                    onclick: move |_| st.stop_reading(),
                    "\u{00d7}"
                }
            }
            // How far down it the reader is. A fat description is a scrollbar
            // that barely moves, and this is the one honest answer to "how
            // much of this is left".
            div { class: "rdtrack",
                div { class: "rdprog" }
            }
            div { class: "rdmain",
                if showing_toc {
                    Contents { entries: outline.clone() }
                }
                div { class: "rdscroll",
                    div { class: if *wide.read() { "rdcol wide" } else { "rdcol" },
                        if parsed.raw_html {
                            div { class: "rdnote",
                                title: "GitHub descriptions can carry HTML this app does not draw — a centred banner, a table written in tags. It is not executed here, and the description on github.com is one click away.",
                                "some HTML in this description is not drawn"
                            }
                        }
                        if parsed.blocks.is_empty() {
                            div { class: "mdempty", "Nothing written here." }
                        }
                        {super::markdown::render_page(st, Path::new(ROOT), &parsed)}
                    }
                }
            }
        }
    }
}

/// The headings, down the side.
///
/// Indented by level and capped at three deep: a contents that mirrors every
/// `####` in a long description is a second document to read rather than a way
/// around the first.
#[component]
fn Contents(entries: Vec<Entry>) -> Element {
    let st = use_context::<St>();
    let deepest = entries.iter().map(|e| e.level).min().unwrap_or(1);
    rsx! {
        nav { class: "rdtoc",
            div { class: "side-title rdtochd", "CONTENTS" }
            div { class: "rdtoclist",
                for e in entries.iter().filter(|e| e.level < deepest + 3) {
                    {
                        let id = e.id.clone();
                        let depth = (e.level - deepest).min(2);
                        rsx! {
                            div {
                                key: "{e.id}",
                                class: "rdtocrow d{depth}",
                                "data-at": "mdx-{e.id}",
                                title: "{e.text}",
                                onclick: move |_| super::markdown::jump_to(st, &id),
                                "{e.text}"
                            }
                        }
                    }
                }
            }
        }
    }
}
