//! Markdown, drawn as elements of this app rather than as a page.
//!
//! Everything here builds nodes out of parsed text, so nothing a repository
//! contains is ever handed to the page as markup — see
//! [`crate::backend::markdown`] for why that matters. What it buys, over the
//! sandboxed frame an HTML file is previewed in, is a document that scrolls,
//! wraps to the pane, can be selected out of, and whose links work: an
//! outside one opens in the browser, and one pointing at a file in the
//! repository opens it in the viewer.

use std::path::{Path, PathBuf};

use dioxus::prelude::*;

use crate::backend::auth::open_browser;
use crate::backend::highlight::highlight_lang;
use crate::backend::markdown::{Block, Doc, Item, Span};

use super::app::St;

/// Where a link in a document goes.
enum Target {
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
fn resolve(from: &Path, href: &str) -> Target {
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
            other => out.push(percent_decoded(other)),
        }
    }
    if out.as_os_str().is_empty() {
        Target::Nowhere
    } else {
        Target::File(out)
    }
}

/// `docs/getting%20started.md` is a path with a space in it. Only the escapes
/// are undone — everything else in the segment is already the name it is.
fn percent_decoded(seg: &str) -> String {
    if !seg.contains('%') {
        return seg.to_string();
    }
    let bytes = seg.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = (i + 2 < bytes.len())
            .then(|| std::str::from_utf8(&bytes[i + 1..i + 3]).ok())
            .flatten()
            .filter(|_| bytes[i] == b'%')
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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
        (s.image, " mdimg"),
        (link, " mda"),
    ] {
        if on {
            cls.push_str(name);
        }
    }
    cls
}

fn run(st: St, rel: &Path, s: &Span) -> Element {
    let target = s.link.as_deref().map(|href| resolve(rel, href));
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

    #[test]
    fn percent_escapes_are_undone_and_the_rest_is_left_alone() {
        assert_eq!(percent_decoded("plain.md"), "plain.md");
        assert_eq!(percent_decoded("a%20b"), "a b");
        assert_eq!(percent_decoded("100%"), "100%");
        assert_eq!(percent_decoded("%zz"), "%zz");
    }
}
