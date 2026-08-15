//! Markdown, parsed into blocks and runs of styled text.
//!
//! Deliberately not into HTML. A README is a file out of a repository, and
//! handing its markup to the page this app draws its own UI in would execute
//! it — next to a GitHub token in local storage. So the markup is turned into
//! a small tree the viewer renders with its own elements, and text is only
//! ever text.
//!
//! Which is also why raw HTML *inside* the markdown is dropped rather than
//! shown: a `<div align=center>` around a row of badges is invisible either
//! way, and the alternative is either executing it or printing the tags.
//! Source view is one button away and shows the file exactly as written.

use std::path::{Path, PathBuf};

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

/// How a run of text is set. Flat rather than nested: emphasis in a README
/// nests two deep at the very worst, and a stack buys nothing for it.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Style {
    pub strong: bool,
    pub em: bool,
    pub code: bool,
    pub strike: bool,
}

/// A run of text with everything the viewer needs to draw it.
#[derive(Clone, PartialEq, Debug)]
pub struct Span {
    pub text: String,
    pub style: Style,
    /// Where a click on this run goes — a URL, or a path inside the repository.
    pub link: Option<String>,
    /// The picture this run stands for, as the document named it. A path in the
    /// repository is drawn; anything hosted elsewhere is not fetched, and the
    /// text — the alt text — is what there is to show. See
    /// [`crate::backend::images`] for why that line is drawn where it is.
    pub image: Option<String>,
}

/// One entry in a list, which may hold blocks of its own — a nested list, or a
/// paragraph and a code sample under the same bullet.
#[derive(Clone, PartialEq, Debug)]
pub struct Item {
    /// `- [ ]` / `- [x]`, when the item is a checkbox.
    pub task: Option<bool>,
    pub blocks: Vec<Block>,
}

#[derive(Clone, PartialEq, Debug)]
pub enum Block {
    Heading {
        level: u8,
        spans: Vec<Span>,
    },
    Para(Vec<Span>),
    Code {
        lang: String,
        text: String,
    },
    Quote(Vec<Block>),
    List {
        ordered: bool,
        start: u64,
        items: Vec<Item>,
    },
    Table {
        head: Vec<Vec<Span>>,
        rows: Vec<Vec<Vec<Span>>>,
    },
    Rule,
}

/// True for the files worth offering a rendered view of.
pub fn is_markdown(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("md" | "markdown" | "mdown" | "mkd" | "mkdn")
    )
}

/// The repository's README, if it has one.
///
/// Only at the top level: `docs/README.md` is a file about the docs directory,
/// not the front page of the repository. A markdown one wins over a plain one,
/// since it is the one there is anything to render.
pub fn readme_of<'a>(paths: impl IntoIterator<Item = &'a Path>) -> Option<PathBuf> {
    let mut best: Option<(u8, PathBuf)> = None;
    for path in paths {
        if path.parent().is_some_and(|p| !p.as_os_str().is_empty()) {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if !stem.eq_ignore_ascii_case("readme") {
            continue;
        }
        let rank = match () {
            _ if is_markdown(path) => 0,
            // `README` with no extension at all — still prose, just unmarked.
            _ if path.extension().is_none() => 1,
            _ => 2,
        };
        if best.as_ref().is_none_or(|(seen, _)| rank < *seen) {
            best = Some((rank, path.to_path_buf()));
        }
    }
    best.map(|(_, p)| p)
}

/// A parsed document, and what parsing it had to leave out.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Doc {
    pub blocks: Vec<Block>,
    /// True when the source had raw HTML in it. Nothing is drawn for it, and a
    /// reader looking at a gap where a row of badges should be deserves to be
    /// told why rather than left wondering.
    pub raw_html: bool,
}

/// Parse markdown into blocks.
pub fn parse(text: &str) -> Doc {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut reader = Reader {
        events: Parser::new_ext(text, options),
        raw_html: false,
    };
    let blocks = reader.container(&mut None);
    Doc {
        blocks,
        raw_html: reader.raw_html,
    }
}

/// The inline events between one pair of block tags, accumulated into runs.
#[derive(Default)]
struct Inline {
    spans: Vec<Span>,
    style: Style,
    link: Option<String>,
    image: Option<String>,
    /// How many spans there were when the open image started, so that one with
    /// no alt text at all can still be given a run of its own to be drawn as.
    image_at: usize,
    /// Raw HTML passed through in the middle of the text.
    raw_html: bool,
}

impl Inline {
    fn push(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        // Emphasis inside a word arrives as several events with the same
        // styling; joining them keeps the element count near the word count.
        // Checked against the accumulator's own state, so the common merge
        // builds no span — and clones no link — only to throw it away.
        match self.spans.last_mut() {
            Some(prev)
                if prev.style == self.style
                    && prev.link == self.link
                    && prev.image == self.image =>
            {
                prev.text.push_str(text)
            }
            _ => self.spans.push(Span {
                text: text.to_string(),
                style: self.style,
                link: self.link.clone(),
                image: self.image.clone(),
            }),
        }
    }

    /// Fold one event in. `false` means this was the `End` that closes the
    /// block being read — a paragraph, a heading, a cell, a list item.
    fn step(&mut self, ev: Event) -> bool {
        match ev {
            Event::Text(t) => self.push(&t),
            Event::Code(t) => {
                let was = self.style.code;
                self.style.code = true;
                self.push(&t);
                self.style.code = was;
            }
            // A soft break is a line wrapped in the source, which is not a line
            // break in the output; a hard one is, and the renderer keeps it.
            Event::SoftBreak => self.push(" "),
            Event::HardBreak => self.push("\n"),
            Event::FootnoteReference(name) => self.push(&format!("[^{name}]")),
            Event::Start(Tag::Emphasis) => self.style.em = true,
            Event::Start(Tag::Strong) => self.style.strong = true,
            Event::Start(Tag::Strikethrough) => self.style.strike = true,
            Event::Start(Tag::Link { dest_url, .. }) => self.link = Some(dest_url.to_string()),
            // A badge is a link wrapped around an image, so the enclosing link
            // is left in place: whether the picture is drawn or stands as its
            // alt text, clicking it goes where the link goes.
            Event::Start(Tag::Image { dest_url, .. }) => {
                self.image = Some(dest_url.to_string());
                self.image_at = self.spans.len();
            }
            Event::End(TagEnd::Emphasis) => self.style.em = false,
            Event::End(TagEnd::Strong) => self.style.strong = false,
            Event::End(TagEnd::Strikethrough) => self.style.strike = false,
            Event::End(TagEnd::Link) => self.link = None,
            Event::End(TagEnd::Image) => {
                // `![](diff.png)` — a picture with nothing written for the case
                // where it cannot be shown. There is still a picture, so it
                // gets an empty run to be drawn as rather than disappearing.
                if self.spans.len() == self.image_at {
                    self.spans.push(Span {
                        text: String::new(),
                        style: self.style,
                        link: self.link.clone(),
                        image: self.image.take(),
                    });
                }
                self.image = None;
            }
            Event::End(_) => return false,
            Event::Html(_) | Event::InlineHtml(_) => self.raw_html = true,
            // Anything else that is not text.
            _ => {}
        }
        true
    }
}

/// True for the tags that open a block rather than style a run of text.
fn is_block(tag: &Tag) -> bool {
    !matches!(
        tag,
        Tag::Emphasis | Tag::Strong | Tag::Strikethrough | Tag::Link { .. } | Tag::Image { .. }
    )
}

struct Reader<'a> {
    events: Parser<'a>,
    raw_html: bool,
}

impl Reader<'_> {
    /// Everything inside one container — the document, a block quote, or a list
    /// item — up to the `End` that closes it.
    ///
    /// A tight list puts its text straight inside the item with no paragraph
    /// around it, so loose inline events are collected here too and flushed as
    /// a paragraph when a block interrupts them.
    fn container(&mut self, task: &mut Option<bool>) -> Vec<Block> {
        let mut out = Vec::new();
        let mut loose = Inline::default();
        while let Some(ev) = self.events.next() {
            match ev {
                Event::TaskListMarker(done) => *task = Some(done),
                Event::Rule => out.push(Block::Rule),
                Event::Start(tag) if is_block(&tag) => {
                    flush(&mut loose, &mut out);
                    out.extend(self.block(tag));
                }
                ev => {
                    if !loose.step(ev) {
                        break;
                    }
                }
            }
        }
        self.raw_html |= loose.raw_html;
        flush(&mut loose, &mut out);
        out
    }

    /// The block a start tag opens, having read it to its end. `None` for the
    /// ones with nothing to draw.
    fn block(&mut self, tag: Tag) -> Option<Block> {
        let html_block = matches!(tag, Tag::HtmlBlock);
        match tag {
            Tag::Paragraph => Some(Block::Para(self.inline())),
            Tag::Heading { level, .. } => Some(Block::Heading {
                level: level as u8,
                spans: self.inline(),
            }),
            Tag::CodeBlock(kind) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => {
                        // ```rust,ignore — the first word is the language.
                        info.split([',', ' '])
                            .next()
                            .unwrap_or_default()
                            .to_string()
                    }
                    CodeBlockKind::Indented => String::new(),
                };
                Some(Block::Code {
                    lang,
                    text: self.verbatim(),
                })
            }
            Tag::BlockQuote(_) => Some(Block::Quote(self.container(&mut None))),
            Tag::List(start) => Some(self.list(start)),
            Tag::Table(_) => Some(self.table()),
            // Raw HTML blocks, footnote definitions, front matter.
            _ => {
                self.raw_html |= html_block;
                self.skip();
                None
            }
        }
    }

    fn inline(&mut self) -> Vec<Span> {
        let mut acc = Inline::default();
        for ev in self.events.by_ref() {
            if !acc.step(ev) {
                break;
            }
        }
        self.raw_html |= acc.raw_html;
        acc.spans
    }

    /// A code block's text, which arrives as text events and nothing else.
    fn verbatim(&mut self) -> String {
        let mut out = String::new();
        for ev in self.events.by_ref() {
            match ev {
                Event::Text(t) => out.push_str(&t),
                Event::End(_) => break,
                _ => {}
            }
        }
        // Fenced blocks end with the newline before the closing fence, which
        // would otherwise draw an empty last line in every sample.
        out.truncate(out.trim_end_matches('\n').len());
        out
    }

    fn list(&mut self, start: Option<u64>) -> Block {
        let mut items = Vec::new();
        while let Some(ev) = self.events.next() {
            match ev {
                Event::Start(Tag::Item) => {
                    let mut task = None;
                    let blocks = self.container(&mut task);
                    items.push(Item { task, blocks });
                }
                Event::End(_) => break,
                _ => {}
            }
        }
        Block::List {
            ordered: start.is_some(),
            start: start.unwrap_or(1),
            items,
        }
    }

    fn table(&mut self) -> Block {
        let mut head = Vec::new();
        let mut rows = Vec::new();
        let mut cells = Vec::new();
        while let Some(ev) = self.events.next() {
            match ev {
                Event::Start(Tag::TableHead | Tag::TableRow) => cells = Vec::new(),
                Event::Start(Tag::TableCell) => cells.push(self.inline()),
                Event::End(TagEnd::TableHead) => head = std::mem::take(&mut cells),
                Event::End(TagEnd::TableRow) => rows.push(std::mem::take(&mut cells)),
                Event::End(TagEnd::Table) => break,
                _ => {}
            }
        }
        Block::Table { head, rows }
    }

    /// Consume a container whose contents are not drawn, however deeply nested.
    fn skip(&mut self) {
        let mut depth = 1usize;
        for ev in self.events.by_ref() {
            match ev {
                Event::Start(_) => depth += 1,
                Event::End(_) => {
                    depth -= 1;
                    if depth == 0 {
                        return;
                    }
                }
                _ => {}
            }
        }
    }
}

/// Turn whatever inline text has piled up into a paragraph. Whitespace alone is
/// the gap between two blocks, not a paragraph of its own — but a picture with
/// no alt text is nothing *written* and something to draw.
fn flush(acc: &mut Inline, out: &mut Vec<Block>) {
    let spans = std::mem::take(&mut acc.spans);
    if spans
        .iter()
        .any(|s| !s.text.trim().is_empty() || s.image.is_some())
    {
        out.push(Block::Para(spans));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The text of a run of spans, with the styling forgotten.
    fn plain(spans: &[Span]) -> String {
        spans.iter().map(|s| s.text.as_str()).collect()
    }

    /// The blocks alone, which is all most of these care about.
    fn doc(text: &str) -> Vec<Block> {
        parse(text).blocks
    }

    fn paras(blocks: &[Block]) -> Vec<String> {
        blocks
            .iter()
            .filter_map(|b| match b {
                Block::Para(spans) => Some(plain(spans)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn markdown_files_are_renderable() {
        assert!(is_markdown(Path::new("README.md")));
        assert!(is_markdown(Path::new("docs/GUIDE.Markdown")));
        assert!(!is_markdown(Path::new("src/main.rs")));
        assert!(!is_markdown(Path::new("README")));
    }

    #[test]
    fn the_readme_is_the_one_at_the_top() {
        let paths = [
            Path::new("src/lib.rs"),
            Path::new("docs/README.md"),
            Path::new("README.md"),
        ];
        assert_eq!(
            readme_of(paths),
            Some(PathBuf::from("README.md")),
            "a nested README is about its own directory"
        );

        // Markdown wins over a bare one, whatever order they arrive in.
        let both = [Path::new("README"), Path::new("readme.md")];
        assert_eq!(readme_of(both), Some(PathBuf::from("readme.md")));
        assert_eq!(
            readme_of([Path::new("README")]),
            Some(PathBuf::from("README"))
        );
        assert_eq!(readme_of([Path::new("src/main.rs")]), None);
    }

    #[test]
    fn headings_and_paragraphs_come_out_in_order() {
        let blocks = doc("# Title\n\nSome *words* here.\n\n## Next\n");
        assert_eq!(blocks.len(), 3);
        match &blocks[0] {
            Block::Heading { level, spans } => {
                assert_eq!(*level, 1);
                assert_eq!(plain(spans), "Title");
            }
            other => panic!("expected a heading, got {other:?}"),
        }
        assert_eq!(paras(&blocks), vec!["Some words here."]);
    }

    #[test]
    fn emphasis_is_carried_on_the_run_it_covers() {
        let blocks = doc("plain **bold** `code` ~~gone~~");
        let Block::Para(spans) = &blocks[0] else {
            panic!("expected a paragraph: {blocks:?}")
        };
        let styled = |text: &str| {
            spans
                .iter()
                .find(|s| s.text == text)
                .unwrap_or_else(|| panic!("no run {text:?} in {spans:?}"))
                .style
        };
        assert!(styled("bold").strong);
        assert!(styled("code").code);
        assert!(styled("gone").strike);
        assert_eq!(styled("plain "), Style::default());
    }

    #[test]
    fn nested_emphasis_closes_the_inner_one_only() {
        let blocks = doc("**bold *and italic* still bold**");
        let Block::Para(spans) = &blocks[0] else {
            panic!("expected a paragraph")
        };
        for s in spans {
            assert!(s.style.strong, "{s:?} lost its bold");
        }
        assert!(spans.iter().any(|s| s.style.em && s.text == "and italic"));
        assert!(spans.iter().any(|s| !s.style.em && s.text == " still bold"));
    }

    #[test]
    fn links_and_images_carry_their_target() {
        let blocks = doc("[docs](https://example.com) and ![a chart](chart.png)");
        let Block::Para(spans) = &blocks[0] else {
            panic!("expected a paragraph")
        };
        let link = spans.iter().find(|s| s.text == "docs").unwrap();
        assert_eq!(link.link.as_deref(), Some("https://example.com"));
        assert_eq!(link.image, None);

        let img = spans.iter().find(|s| s.text == "a chart").unwrap();
        assert_eq!(
            img.image.as_deref(),
            Some("chart.png"),
            "the picture is named, and the run holds its alt text"
        );
        assert_eq!(img.link, None);
    }

    #[test]
    fn a_picture_with_no_alt_text_is_still_a_picture() {
        // `![](diff.png)` produces no text event at all, so the run it is drawn
        // as has to be made where the image closes or it is lost entirely.
        let blocks = doc("![](diff.png)\n");
        let Block::Para(spans) = &blocks[0] else {
            panic!("expected a paragraph: {blocks:?}")
        };
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].image.as_deref(), Some("diff.png"));
        assert_eq!(spans[0].text, "");

        // The same inside a tight list item, which collects its text loosely.
        let blocks = doc("- ![](a.png)\n");
        let Block::List { items, .. } = &blocks[0] else {
            panic!("expected a list: {blocks:?}")
        };
        assert!(
            matches!(&items[0].blocks[..], [Block::Para(spans)]
                if spans.iter().any(|s| s.image.as_deref() == Some("a.png"))),
            "{:?}",
            items[0].blocks
        );
    }

    #[test]
    fn a_badge_keeps_the_link_it_is_wrapped_in() {
        // `[![alt](img)](target)` — clicking the picture, drawn or not, goes to
        // the target rather than to the picture.
        let blocks = doc("[![build](https://img.example/b.svg)](https://ci.example/job)");
        let Block::Para(spans) = &blocks[0] else {
            panic!("expected a paragraph")
        };
        let badge = spans.iter().find(|s| s.text == "build").unwrap();
        assert_eq!(badge.image.as_deref(), Some("https://img.example/b.svg"));
        assert_eq!(badge.link.as_deref(), Some("https://ci.example/job"));
    }

    #[test]
    fn fenced_code_keeps_its_language_and_its_lines() {
        let blocks = doc("```rust,ignore\nfn main() {}\n\nlet x = 1;\n```\n");
        match &blocks[0] {
            Block::Code { lang, text } => {
                assert_eq!(lang, "rust", "the first word of the info string");
                assert_eq!(text, "fn main() {}\n\nlet x = 1;");
            }
            other => panic!("expected code, got {other:?}"),
        }
    }

    #[test]
    fn tight_lists_keep_their_text_and_nesting() {
        let blocks = doc("- one\n- two\n  - nested\n");
        let Block::List { ordered, items, .. } = &blocks[0] else {
            panic!("expected a list: {blocks:?}")
        };
        assert!(!ordered);
        assert_eq!(items.len(), 2);
        assert_eq!(paras(&items[0].blocks), vec!["one"]);
        // The second item holds its own text *and* the list under it.
        assert_eq!(paras(&items[1].blocks), vec!["two"]);
        assert!(
            items[1]
                .blocks
                .iter()
                .any(|b| matches!(b, Block::List { .. })),
            "{:?}",
            items[1].blocks
        );
    }

    #[test]
    fn ordered_lists_start_where_they_say() {
        let blocks = doc("3. three\n4. four\n");
        let Block::List {
            ordered,
            start,
            items,
        } = &blocks[0]
        else {
            panic!("expected a list")
        };
        assert!(ordered);
        assert_eq!(*start, 3);
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn task_items_remember_whether_they_are_ticked() {
        let blocks = doc("- [x] done\n- [ ] todo\n");
        let Block::List { items, .. } = &blocks[0] else {
            panic!("expected a list")
        };
        assert_eq!(items[0].task, Some(true));
        assert_eq!(items[1].task, Some(false));
        assert_eq!(paras(&items[0].blocks), vec!["done"]);
    }

    #[test]
    fn tables_split_into_a_header_and_rows() {
        let blocks = doc("| a | b |\n| --- | --- |\n| 1 | 2 |\n| 3 | 4 |\n");
        let Block::Table { head, rows } = &blocks[0] else {
            panic!("expected a table: {blocks:?}")
        };
        assert_eq!(
            head.iter().map(|c| plain(c)).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[1].iter().map(|c| plain(c)).collect::<Vec<_>>(),
            ["3", "4"]
        );
    }

    #[test]
    fn quotes_and_rules_survive() {
        let blocks = doc("> quoted\n\n---\n");
        match &blocks[0] {
            Block::Quote(inner) => assert_eq!(paras(inner), vec!["quoted"]),
            other => panic!("expected a quote, got {other:?}"),
        }
        assert!(matches!(blocks[1], Block::Rule));
    }

    #[test]
    fn raw_html_is_dropped_rather_than_printed() {
        let blocks = doc("<div align=\"center\">\n<script>alert(1)</script>\n</div>\n\nafter\n");
        // Whatever else happens, the tags and the script are not text on screen.
        let text = paras(&blocks).join(" ");
        assert!(!text.contains("script"), "{text:?}");
        assert!(!text.contains("<div"), "{text:?}");
        assert_eq!(paras(&blocks), vec!["after"], "the rest still renders");
    }

    #[test]
    fn a_hard_break_is_kept_and_a_soft_one_is_a_space() {
        let blocks = doc("one\ntwo  \nthree\n");
        assert_eq!(paras(&blocks), vec!["one two\nthree"]);
    }

    #[test]
    fn an_empty_document_is_no_blocks_at_all() {
        assert!(doc("").is_empty());
        assert!(doc("   \n\n  \n").is_empty());
    }
}
