//! Markdown, drawn as elements of this app rather than as a page.
//!
//! Everything here builds nodes out of parsed text, so nothing a repository
//! contains is ever handed to the page as markup — see
//! [`crate::backend::markdown`] for why that matters. What it buys, over the
//! sandboxed frame an HTML file is previewed in, is a document that scrolls,
//! wraps to the pane, can be selected out of, and whose links work: an
//! outside one opens in the browser, one pointing at a file in the repository
//! opens it in the viewer, and one pointing at a heading goes to it.
//!
//! Pictures are the one thing here that is fetched rather than parsed, and only
//! ever out of the repository being read — [`crate::backend::images`] is where
//! that line is drawn and why.

use std::path::{Path, PathBuf};

use dioxus::prelude::*;

use crate::backend::auth::open_browser;
use crate::backend::clip;
use crate::backend::highlight::highlight_lang;
use crate::backend::images::media_type;
use crate::backend::markdown::{self, Alert, Block, Doc, Item, Span};
use crate::backend::route::decoded;

use super::app::St;
use super::imgcache::{drawable, ensure_image, refused};

/// Where a link in a document goes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum Target {
    /// Out of the app, to a browser.
    Web(String),
    /// A file in this repository, relative to its root.
    File(PathBuf),
    /// A heading in the document being read — what the table of contents at
    /// the top of a long description is made of.
    Anchor(String),
    /// A mail link with nothing to open, or a path that climbs out of the
    /// repository. Left as text.
    Nowhere,
}

/// Read a link the way a reader would: as a page to open, another file in the
/// repository they are reading, or a place further down what they are reading.
///
/// `from` is the document's own path, because a relative link is relative to
/// the file it is written in, not to the root.
pub(super) fn resolve(from: &Path, href: &str) -> Target {
    let href = href.trim();
    if href.is_empty() {
        return Target::Nowhere;
    }
    if let Some(anchor) = href.strip_prefix('#') {
        let anchor = anchor.trim();
        return match anchor.is_empty() {
            true => Target::Nowhere,
            false => Target::Anchor(decoded(anchor)),
        };
    }
    // Anything with a scheme is somebody else's to open — http, mailto, and
    // whatever else a README has been known to link to.
    if href.contains("://") || href.starts_with("mailto:") {
        return Target::Web(href.to_string());
    }
    // A link into a file, minus the part naming a place inside it.
    let path = href.split(['#', '?']).next().unwrap_or_default();
    if path.is_empty() || path.starts_with('/') {
        return Target::Nowhere;
    }
    let mut out = PathBuf::new();
    for part in from.parent().into_iter().flat_map(|p| p.components()) {
        out.push(part);
    }
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                // `../..` from the top is a link out of the repository, which
                // is not something this viewer can show.
                if !out.pop() {
                    return Target::Nowhere;
                }
            }
            other => out.push(decoded(other)),
        }
    }
    if out.as_os_str().is_empty() {
        Target::Nowhere
    } else {
        Target::File(out)
    }
}

/// Everything one drawing of a document needs that is the same all the way
/// down it: the app, the document's own path, and whether its headings are
/// places that can be linked to.
///
/// `Copy`, because it is threaded through every block and every run — the
/// alternative is four arguments doing the same job.
#[derive(Clone, Copy)]
pub(super) struct Ctx<'a> {
    st: St,
    rel: &'a Path,
    /// Headings carry an `id` and links to them work. False in the pane on
    /// the right, where a dozen comments are drawn one under another and the
    /// same heading can be in three of them — an `id` has to be the only one
    /// in the page to be worth anything.
    anchors: bool,
}

/// The prefix on every heading's `id`, so that a slug from a document cannot
/// collide with an element this app named for its own reasons.
const ANCHOR: &str = "mdx-";

/// Put a heading of the document on screen. Called by a link to it, which is
/// the only thing that ever asks.
///
/// Two halves, because the heading asked for may not be in the page yet: the
/// signal is what a folded `<details>` holding it watches to unfold itself,
/// and the retry is what waits for the render that follows. Twelve tries at
/// 40ms — long enough for a fold to open, short enough that a link to a
/// heading that is simply not there gives up rather than hunting for ever.
pub(super) fn jump_to(st: St, id: &str) {
    let mut want = st.anchor;
    want.set(Some(id.to_string()));
    let at = serde_json::to_string(&format!("{ANCHOR}{id}")).unwrap_or_default();
    document::eval(&format!(
        "(function(){{var n=0;function go(){{\
         var e=document.getElementById({at});\
         if(!e){{if(n++<12)setTimeout(go,40);return;}}\
         e.scrollIntoView({{block:'start'}});e.classList.add('mdflash');\
         setTimeout(function(){{e.classList.remove('mdflash')}},1200);}}go();}})();"
    ));
}

/// A whole document, as the page — a README, a file opened in Preview.
pub fn render(st: St, rel: &Path, doc: &Doc) -> Element {
    let ctx = Ctx {
        st,
        rel,
        anchors: true,
    };
    rsx! {
        div { class: "mdwrap",
            if doc.raw_html {
                div { class: "mdbar",
                    span {
                        class: "previewnote",
                        title: "Markdown may contain HTML beyond the folds, line breaks and pictures this app reads — badge rows, centred banners. It is not drawn here, and Source view shows the file exactly as written.",
                        "raw HTML not drawn"
                    }
                }
            }
            div { class: "mddoc",
                if doc.blocks.is_empty() {
                    div { class: "mdempty", "This file has nothing in it." }
                }
                {blocks(ctx, &doc.blocks)}
            }
        }
    }
}

/// A document drawn inside something that has a frame of its own already — a
/// comment, a pull request's description — rather than as the page.
///
/// The same nodes as [`render`] and the same guarantee behind them: text out of
/// somebody else's pull request becomes elements of this app, never markup. All
/// that changes is the chrome, which the pane around it is already providing —
/// no width cap, no bar above it, and type scaled to the column it sits in.
pub fn render_body(st: St, rel: &Path, doc: &Doc) -> Element {
    let ctx = Ctx {
        st,
        rel,
        anchors: false,
    };
    rsx! {
        div { class: "mddoc mdsmall", {blocks(ctx, &doc.blocks)} }
    }
}

/// The same document again, given the middle pane to be read in: the measure,
/// the type and the space of something written to be read start to finish.
///
/// The class is the whole difference. Which is the point of it being one — a
/// description read in the pane on the right and the same description read
/// here have to be the same document, not two renderers that agree today.
pub fn render_page(st: St, rel: &Path, doc: &Doc) -> Element {
    let ctx = Ctx {
        st,
        rel,
        anchors: true,
    };
    rsx! {
        div { class: "mddoc mdread", {blocks(ctx, &doc.blocks)} }
    }
}

pub(super) fn blocks(ctx: Ctx, list: &[Block]) -> Element {
    rsx! {
        for b in list.iter() {
            {block(ctx, b)}
        }
    }
}

fn block(ctx: Ctx, b: &Block) -> Element {
    match b {
        Block::Heading { level, id, spans } => heading(ctx, *level, id, spans),
        Block::Para(spans) => rsx! {
            div { class: "mdp", {inline(ctx, spans)} }
        },
        Block::Code { lang, text } => rsx! {
            Code { lang: lang.clone(), text: text.clone() }
        },
        Block::Quote {
            alert,
            blocks: inner,
        } => quote(ctx, *alert, inner),
        Block::Details {
            summary,
            open,
            blocks: inner,
        } => {
            // Keyed by what it is called, so that drawing a second document
            // into this slot does not hand its first fold the state of the
            // last one's. The label is built before the props move — see the
            // note on `.clone()` and `key` in `github::PrRow`.
            let label: String = summary.iter().map(|s| s.text.as_str()).collect();
            rsx! {
                Fold {
                    key: "{label}",
                    summary: summary.clone(),
                    open: *open,
                    // What a link into this fold would be asking for.
                    ids: markdown::outline(inner).into_iter().map(|e| e.id).collect(),
                    blocks: inner.clone(),
                    rel: ctx.rel.to_path_buf(),
                    anchors: ctx.anchors,
                }
            }
        }
        Block::List {
            ordered,
            start,
            items,
        } => list_of(ctx, *ordered, *start, items),
        Block::Table { head, rows } => table(ctx, head, rows),
        Block::Rule => rsx! {
            div { class: "mdrule" }
        },
    }
}

/// A heading, and — where headings are places — the link that reaches it.
///
/// The `¶` beside it is the same affordance every documentation site has: a
/// mark that appears on hover and copies a link to the section under it. Here
/// there is no address to copy, so it scrolls to itself instead, which is what
/// makes a table of contents written in the description work at both ends.
fn heading(ctx: Ctx, level: u8, id: &str, spans: &[Span]) -> Element {
    let cls = format!("mdh mdh{}", level.clamp(1, 6));
    if !ctx.anchors || id.is_empty() {
        return rsx! {
            div { class: "{cls}", {inline(ctx, spans)} }
        };
    }
    let anchor = id.to_string();
    let st = ctx.st;
    rsx! {
        div { class: "{cls}", id: "{ANCHOR}{id}",
            {inline(ctx, spans)}
            span {
                class: "mdanchor",
                title: "Go to this section",
                onclick: move |_| jump_to(st, &anchor),
                "#"
            }
        }
    }
}

/// A quote — or, when it opens with `> [!WARNING]`, the callout that is.
fn quote(ctx: Ctx, alert: Option<Alert>, inner: &[Block]) -> Element {
    let Some(alert) = alert else {
        return rsx! {
            div { class: "mdquote", {blocks(ctx, inner)} }
        };
    };
    rsx! {
        div { class: "mdalert {alert.css()}",
            div { class: "mdalerthd",
                span { class: "mdalertmark", "{alert.glyph()}" }
                "{alert.label()}"
            }
            {blocks(ctx, inner)}
        }
    }
}

/// A fenced block, highlighted by the same engine as the source view — a
/// README's samples are code, and reading them as code is the point of them.
///
/// A component for the copy button, which has to remember that it was pressed.
/// Worth the state: half the fenced blocks in a pull request's description are
/// a command somebody is meant to run.
#[component]
fn Code(lang: String, text: String) -> Element {
    let mut copied = use_signal(|| false);
    let lines = highlight_lang(&lang, &text);
    let label = lang.trim();
    let body = text.clone();
    rsx! {
        div { class: "mdcodewrap",
            div { class: "mdcodebar",
                if !label.is_empty() {
                    span { class: "mdlang", "{label}" }
                }
                span { class: "spacer" }
                button {
                    class: if *copied.read() { "mdcopy done" } else { "mdcopy" },
                    title: "Copy this block",
                    onclick: move |_| {
                        clip::copy(&body);
                        copied.set(true);
                    },
                    if *copied.read() { "Copied" } else { "Copy" }
                }
            }
            div { class: "mdcode",
                for spans in lines {
                    div { class: "mdcl",
                        for sp in spans {
                            span { style: "color:{sp.color}", "{sp.text}" }
                        }
                    }
                }
            }
        }
    }
}

/// A `<details>`, folded until it is asked for.
///
/// A component, because whether it is open is a fact about this fold on screen
/// and nothing else — and because the blocks inside a folded one are not drawn
/// at all until it is opened, which is most of what makes a description with
/// six of them cheap to put on screen.
#[component]
fn Fold(
    summary: Vec<Span>,
    open: bool,
    /// The headings inside it. A contents that lists a section nobody can
    /// reach is a contents that lies, so a link to one of these unfolds this.
    ids: Vec<String>,
    blocks: Vec<Block>,
    rel: PathBuf,
    anchors: bool,
) -> Element {
    let st = use_context::<St>();
    let mut shown = use_signal(|| open);
    use_effect(use_reactive!(|ids| {
        if let Some(id) = st.anchor.read().clone()
            && ids.contains(&id)
        {
            shown.set(true);
        }
    }));
    let on = *shown.read();
    let ctx = Ctx {
        st,
        rel: &rel,
        anchors,
    };
    let count = blocks.len();
    rsx! {
        div { class: if on { "mdfold open" } else { "mdfold" },
            div {
                class: "mdfoldhd",
                title: if on { "Fold this away" } else { "Unfold" },
                onclick: move |_| shown.toggle(),
                span { class: "mdfoldmark", "\u{203a}" }
                if summary.is_empty() {
                    span { class: "mdt", "Details" }
                } else {
                    {inline(ctx, &summary)}
                }
                if !on && count > 0 {
                    span { class: "mdfoldcount", "{count}" }
                }
            }
            if on {
                div { class: "mdfoldbody", {super::markdown::blocks(ctx, &blocks)} }
            }
        }
    }
}

/// The marker down the left of an item: a bullet, its number, or its checkbox.
fn marker(ordered: bool, start: u64, index: usize, task: Option<bool>) -> String {
    match task {
        Some(true) => "\u{2713}".to_string(),
        Some(false) => String::new(),
        None if ordered => format!("{}.", start + index as u64),
        None => "\u{2022}".to_string(),
    }
}

fn list_of(ctx: Ctx, ordered: bool, start: u64, items: &[Item]) -> Element {
    rsx! {
        div { class: if items.iter().any(|i| i.task.is_some()) { "mdlist mdtasks" } else { "mdlist" },
            for (i , item) in items.iter().enumerate() {
                div { class: if item.task == Some(true) { "mditem done" } else { "mditem" },
                    span {
                        class: match item.task {
                            Some(true) => "mdmark mdbox on",
                            Some(false) => "mdmark mdbox",
                            None => "mdmark",
                        },
                        "{marker(ordered, start, i, item.task)}"
                    }
                    div { class: "mdbody", {blocks(ctx, &item.blocks)} }
                }
            }
        }
    }
}

fn table(ctx: Ctx, head: &[Vec<Span>], rows: &[Vec<Vec<Span>>]) -> Element {
    rsx! {
        // A wide table scrolls inside itself rather than stretching the pane.
        div { class: "mdtablewrap",
            table { class: "mdtable",
                if !head.is_empty() {
                    thead {
                        tr {
                            for cell in head.iter() {
                                th { {inline(ctx, cell)} }
                            }
                        }
                    }
                }
                tbody {
                    for row in rows.iter() {
                        tr {
                            for cell in row.iter() {
                                td { {inline(ctx, cell)} }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn inline(ctx: Ctx, spans: &[Span]) -> Element {
    rsx! {
        for s in spans.iter() {
            {run(ctx, s)}
        }
    }
}

fn classes(s: &Span, link: bool) -> String {
    let mut cls = String::from("mdt");
    for (on, name) in [
        (s.style.strong, " mdb"),
        (s.style.em, " mdi"),
        (s.style.code, " mdc"),
        (s.style.strike, " mds"),
        (s.image.is_some(), " mdimg"),
        (link, " mda"),
    ] {
        if on {
            cls.push_str(name);
        }
    }
    cls
}

fn run(ctx: Ctx, s: &Span) -> Element {
    match s.image.as_deref() {
        Some(src) => picture(ctx, s, src),
        None => text(ctx, s, s.link.as_deref()),
    }
}

/// A picture: the file itself when it is one this repository holds, and the alt
/// text when it is not — a badge on somebody's CDN, a path that has moved, or
/// something no browser would draw.
///
/// The alt text is a link either way, so a picture that cannot be shown is
/// still a picture that can be looked at: at whatever the document wrapped it
/// in, or failing that at the picture itself.
fn picture(ctx: Ctx, s: &Span, src: &str) -> Element {
    if let Target::File(path) = resolve(ctx.rel, src)
        && media_type(&path).is_some()
        && ctx.st.has_file(&path)
    {
        // The enclosing link is resolved here: inside the component the
        // document's own path is no longer around to resolve it against.
        let link = s.link.as_deref().map(|href| resolve(ctx.rel, href));
        return rsx! {
            Figure { path, alt: s.text.clone(), link }
        };
    }
    let href = s.link.as_deref().or(Some(src));
    text(ctx, s, href)
}

/// A run of text, with `href` as where clicking it goes.
fn text(ctx: Ctx, s: &Span, href: Option<&str>) -> Element {
    let st = ctx.st;
    let target = href.map(|href| resolve(ctx.rel, href));
    let (title, click) = match &target {
        Some(Target::Web(url)) => (url.clone(), true),
        Some(Target::File(path)) => (format!("Open {}", path.display()), true),
        // Only where headings are places to go. Elsewhere it is a link to
        // nothing, and drawing it as one is a promise the pane cannot keep.
        Some(Target::Anchor(id)) if ctx.anchors => (format!("Go to {id}"), true),
        _ => (String::new(), false),
    };
    let cls = classes(s, click);
    if !click {
        return rsx! {
            span { class: "{cls}", "{s.text}" }
        };
    }
    rsx! {
        span {
            class: "{cls}",
            title: "{title}",
            onclick: move |_| match &target {
                Some(Target::Web(url)) => open_browser(url),
                // A link to a file that is not in this repository — a path that
                // has moved, or one that only exists in another checkout —
                // stays put rather than opening an error where the document was.
                Some(Target::File(path)) if st.has_file(path) => st.open_file(path.clone()),
                Some(Target::Anchor(id)) => jump_to(st, id),
                _ => {}
            },
            "{s.text}"
        }
    }
}

/// One picture out of the repository, read on demand and drawn where it was
/// written.
///
/// A component rather than a function because it is the only thing in this file
/// that has to *ask* for something: the read is started from an effect, since
/// starting it while the document is being drawn is a write to the state the
/// drawing is reading.
#[component]
fn Figure(path: PathBuf, alt: String, link: Option<Target>) -> Element {
    let st = use_context::<St>();
    // `use_reactive` because `path` is a prop rather than a signal: a plain
    // effect is built once and would keep asking for the first picture this
    // slot ever held, which is the wrong file the moment another document is
    // drawn in the same place.
    use_effect(use_reactive!(|path| ensure_image(st, &path)));

    // A picture wrapped in a link goes where the link goes — a badge, or a
    // screenshot standing in for the page it was taken of.
    let click = move |_| match &link {
        Some(Target::Web(url)) => open_browser(url),
        Some(Target::File(to)) if st.has_file(to) => st.open_file(to.clone()),
        _ => {}
    };

    let name = path.display().to_string();
    if let Some(uri) = drawable(&st, &path) {
        return rsx! {
            img {
                class: "mdimage",
                src: "{uri}",
                alt: "{alt}",
                title: "{name}",
                onclick: click,
            }
        };
    }
    // Not here yet, or never coming. Empty alt text is a picture the document
    // did not describe, which is most of them — its own name is better than a
    // blank space to look at while it is read.
    let why = refused(&st, &path);
    let label = if alt.trim().is_empty() { &name } else { &alt };
    let title = match &why {
        Some(why) => format!("{name} — {why}"),
        None => format!("{name} — reading…"),
    };
    rsx! {
        span {
            class: if why.is_some() { "mdt mdimg" } else { "mdt mdimg mdimgwait" },
            title: "{title}",
            "{label}"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(from: &str, href: &str) -> String {
        match resolve(Path::new(from), href) {
            Target::Web(url) => format!("web {url}"),
            Target::File(path) => format!("file {}", path.display()),
            Target::Anchor(id) => format!("anchor {id}"),
            Target::Nowhere => "nowhere".to_string(),
        }
    }

    #[test]
    fn outside_links_go_to_the_browser() {
        assert_eq!(
            target("README.md", "https://example.com/x"),
            "web https://example.com/x"
        );
        assert_eq!(target("README.md", "mailto:a@b.c"), "web mailto:a@b.c");
    }

    #[test]
    fn relative_links_resolve_against_the_document() {
        assert_eq!(target("README.md", "src/main.rs"), "file src/main.rs");
        assert_eq!(target("docs/guide.md", "api.md"), "file docs/api.md");
        assert_eq!(target("docs/a/b.md", "../c.md"), "file docs/c.md");
        assert_eq!(target("docs/guide.md", "./api.md"), "file docs/api.md");
        // The part that names a place inside the file is not part of its name.
        assert_eq!(target("README.md", "docs/api.md#usage"), "file docs/api.md");
        assert_eq!(target("README.md", "a%20b.md"), "file a b.md");
    }

    #[test]
    fn a_link_to_a_heading_is_a_place_in_the_document() {
        assert_eq!(target("README.md", "#install"), "anchor install");
        // A table of contents copied off github.com is written escaped.
        assert_eq!(target("README.md", "#why%20not"), "anchor why not");
    }

    #[test]
    fn links_with_nowhere_to_go_are_left_as_text() {
        assert_eq!(target("README.md", ""), "nowhere");
        // A `#` with nothing after it names no section.
        assert_eq!(target("README.md", "# "), "nowhere");
        // Out of the repository, either upwards or from the filesystem root.
        assert_eq!(target("README.md", "../secrets"), "nowhere");
        assert_eq!(target("README.md", "/etc/passwd"), "nowhere");
    }
}
