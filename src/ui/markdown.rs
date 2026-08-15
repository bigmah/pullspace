//! Markdown, drawn as elements of this app rather than as a page.
//!
//! Everything here builds nodes out of parsed text, so nothing a repository
//! contains is ever handed to the page as markup — see
//! [`crate::backend::markdown`] for why that matters. What it buys, over the
//! sandboxed frame an HTML file is previewed in, is a document that scrolls,
//! wraps to the pane, can be selected out of, and whose links work: an
//! outside one opens in the browser, and one pointing at a file in the
//! repository opens it in the viewer.
//!
//! Pictures are the one thing here that is fetched rather than parsed, and only
//! ever out of the repository being read — [`crate::backend::images`] is where
//! that line is drawn and why.

use std::path::{Path, PathBuf};

use dioxus::prelude::*;

use crate::backend::auth::open_browser;
use crate::backend::highlight::highlight_lang;
use crate::backend::images::media_type;
use crate::backend::markdown::{Block, Doc, Item, Span};
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
    /// An anchor, a mail link with nothing to open, or a path that climbs out
    /// of the repository. Left as text.
    Nowhere,
}

/// Read a link the way a reader would: as a page to open, or as another file
/// in the repository they are reading.
///
/// `from` is the document's own path, because a relative link is relative to
/// the file it is written in, not to the root.
pub(super) fn resolve(from: &Path, href: &str) -> Target {
    let href = href.trim();
    if href.is_empty() || href.starts_with('#') {
        return Target::Nowhere;
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

/// A whole document.
pub fn render(st: St, rel: &Path, doc: &Doc) -> Element {
    rsx! {
        div { class: "mdwrap",
            if doc.raw_html {
                div { class: "mdbar",
                    span {
                        class: "previewnote",
                        title: "Markdown may contain HTML — badge rows, centred banners. It is not drawn here, and Source view shows the file exactly as written.",
                        "raw HTML not drawn"
                    }
                }
            }
            div { class: "mddoc",
                if doc.blocks.is_empty() {
                    div { class: "mdempty", "This file has nothing in it." }
                }
                for b in doc.blocks.iter() {
                    {block(st, rel, b)}
                }
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
    rsx! {
        div { class: "mddoc mdsmall",
            for b in doc.blocks.iter() {
                {block(st, rel, b)}
            }
        }
    }
}

fn block(st: St, rel: &Path, b: &Block) -> Element {
    match b {
        Block::Heading { level, spans } => {
            let cls = format!("mdh mdh{}", (*level).clamp(1, 6));
            rsx! {
                div { class: "{cls}", {inline(st, rel, spans)} }
            }
        }
        Block::Para(spans) => rsx! {
            div { class: "mdp", {inline(st, rel, spans)} }
        },
        Block::Code { lang, text } => code(lang, text),
        Block::Quote(inner) => rsx! {
            div { class: "mdquote",
                for b in inner.iter() {
                    {block(st, rel, b)}
                }
            }
        },
        Block::List {
            ordered,
            start,
            items,
        } => list(st, rel, *ordered, *start, items),
        Block::Table { head, rows } => table(st, rel, head, rows),
        Block::Rule => rsx! {
            div { class: "mdrule" }
        },
    }
}

/// A fenced block, highlighted by the same engine as the source view — a
/// README's samples are code, and reading them as code is the point of them.
fn code(lang: &str, text: &str) -> Element {
    let lines = highlight_lang(lang, text);
    rsx! {
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

/// The marker down the left of an item: a bullet, its number, or its checkbox.
fn marker(ordered: bool, start: u64, index: usize, task: Option<bool>) -> String {
    match task {
        Some(true) => "☑".to_string(),
        Some(false) => "☐".to_string(),
        None if ordered => format!("{}.", start + index as u64),
        None => "•".to_string(),
    }
}

fn list(st: St, rel: &Path, ordered: bool, start: u64, items: &[Item]) -> Element {
    rsx! {
        div { class: "mdlist",
            for (i , item) in items.iter().enumerate() {
                div { class: "mditem",
                    span { class: "mdmark", "{marker(ordered, start, i, item.task)}" }
                    div { class: "mdbody",
                        for b in item.blocks.iter() {
                            {block(st, rel, b)}
                        }
                    }
                }
            }
        }
    }
}

fn table(st: St, rel: &Path, head: &[Vec<Span>], rows: &[Vec<Vec<Span>>]) -> Element {
    rsx! {
        // A wide table scrolls inside itself rather than stretching the pane.
        div { class: "mdtablewrap",
            table { class: "mdtable",
                if !head.is_empty() {
                    thead {
                        tr {
                            for cell in head.iter() {
                                th { {inline(st, rel, cell)} }
                            }
                        }
                    }
                }
                tbody {
                    for row in rows.iter() {
                        tr {
                            for cell in row.iter() {
                                td { {inline(st, rel, cell)} }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn inline(st: St, rel: &Path, spans: &[Span]) -> Element {
    rsx! {
        for s in spans.iter() {
            {run(st, rel, s)}
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

fn run(st: St, rel: &Path, s: &Span) -> Element {
    match s.image.as_deref() {
        Some(src) => picture(st, rel, s, src),
        None => text(st, rel, s, s.link.as_deref()),
    }
}

/// A picture: the file itself when it is one this repository holds, and the alt
/// text when it is not — a badge on somebody's CDN, a path that has moved, or
/// something no browser would draw.
///
/// The alt text is a link either way, so a picture that cannot be shown is
/// still a picture that can be looked at: at whatever the document wrapped it
/// in, or failing that at the picture itself.
fn picture(st: St, rel: &Path, s: &Span, src: &str) -> Element {
    if let Target::File(path) = resolve(rel, src)
        && media_type(&path).is_some()
        && st.has_file(&path)
    {
        // The enclosing link is resolved here: inside the component the
        // document's own path is no longer around to resolve it against.
        let link = s.link.as_deref().map(|href| resolve(rel, href));
        return rsx! {
            Figure { path, alt: s.text.clone(), link }
        };
    }
    let href = s.link.as_deref().or(Some(src));
    text(st, rel, s, href)
}

/// A run of text, with `href` as where clicking it goes.
fn text(st: St, rel: &Path, s: &Span, href: Option<&str>) -> Element {
    let target = href.map(|href| resolve(rel, href));
    let (title, click) = match &target {
        Some(Target::Web(url)) => (url.clone(), true),
        Some(Target::File(path)) => (format!("Open {}", path.display()), true),
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
    fn links_with_nowhere_to_go_are_left_as_text() {
        // An anchor within the page, which this viewer has no anchors for.
        assert_eq!(target("README.md", "#install"), "nowhere");
        assert_eq!(target("README.md", ""), "nowhere");
        // Out of the repository, either upwards or from the filesystem root.
        assert_eq!(target("README.md", "../secrets"), "nowhere");
        assert_eq!(target("README.md", "/etc/passwd"), "nowhere");
    }
}
