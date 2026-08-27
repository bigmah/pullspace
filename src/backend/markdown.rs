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
//!
//! Four kinds of HTML are the exception, because a pull request's description
//! is full of them and dropping them loses the shape of what was written: a
//! `<details>` and its `<summary>`, a `<br>`, an `<img>`, and a comment left
//! behind by the template the description was filled into. Every one of those
//! becomes a node of this tree like everything else — read out of the markup,
//! never handed to the page as markup.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use pulldown_cmark::{BlockQuoteKind, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

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

/// What a `> [!NOTE]` block says it is. GitHub's five, which are the five a
/// pull request's description is written with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Alert {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

impl Alert {
    /// The heading drawn above the block, as GitHub writes it.
    pub fn label(self) -> &'static str {
        match self {
            Alert::Note => "Note",
            Alert::Tip => "Tip",
            Alert::Important => "Important",
            Alert::Warning => "Warning",
            Alert::Caution => "Caution",
        }
    }

    /// The mark beside the heading. Text rather than an icon font: this app
    /// ships one wasm file and no assets beside it.
    pub fn glyph(self) -> &'static str {
        match self {
            Alert::Note => "i",
            Alert::Tip => "\u{2726}",
            Alert::Important => "!",
            Alert::Warning => "\u{25b2}",
            Alert::Caution => "\u{2715}",
        }
    }

    /// The class that carries its colour, which the stylesheet names once.
    pub fn css(self) -> &'static str {
        match self {
            Alert::Note => "mdnote",
            Alert::Tip => "mdtip",
            Alert::Important => "mdimportant",
            Alert::Warning => "mdwarning",
            Alert::Caution => "mdcaution",
        }
    }

    fn of(kind: BlockQuoteKind) -> Self {
        match kind {
            BlockQuoteKind::Note => Alert::Note,
            BlockQuoteKind::Tip => Alert::Tip,
            BlockQuoteKind::Important => Alert::Important,
            BlockQuoteKind::Warning => Alert::Warning,
            BlockQuoteKind::Caution => Alert::Caution,
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Block {
    Heading {
        level: u8,
        /// What a link to this section points at — GitHub's own slug, so that
        /// a table of contents written for github.com works here too. Unique
        /// within the document: two "Testing" headings are `testing` and
        /// `testing-1`, which is the rule github.com follows.
        id: String,
        spans: Vec<Span>,
    },
    Para(Vec<Span>),
    Code {
        lang: String,
        text: String,
    },
    Quote {
        /// `> [!WARNING]` and the four beside it, when the quote is one of
        /// them. A plain quote is `None`.
        alert: Option<Alert>,
        blocks: Vec<Block>,
    },
    /// A `<details>` — the folded half of a long description: a test plan, a
    /// stack trace, the screenshots.
    Details {
        /// What the fold is labelled, from its `<summary>`.
        summary: Vec<Span>,
        /// `<details open>` — the author wanted this one unfolded.
        open: bool,
        blocks: Vec<Block>,
    },
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

/// One line of a document's outline: a heading, and the link that reaches it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    pub level: u8,
    pub text: String,
    pub id: String,
}

/// Every heading in a document, in the order they are read.
///
/// What the reader draws down the side of a long description, and the reason
/// headings carry a slug at all. Quotes and folds are walked into — a section
/// inside a `<details>` is still a section — but lists and tables are not:
/// their headings are cells, not places.
pub fn outline(blocks: &[Block]) -> Vec<Entry> {
    let mut out = Vec::new();
    walk(blocks, &mut out);
    out
}

fn walk(blocks: &[Block], out: &mut Vec<Entry>) {
    for b in blocks {
        match b {
            Block::Heading { level, id, spans } => {
                let text = plain_text(spans);
                if !text.is_empty() {
                    out.push(Entry {
                        level: *level,
                        text,
                        id: id.clone(),
                    });
                }
            }
            Block::Quote { blocks, .. } | Block::Details { blocks, .. } => walk(blocks, out),
            _ => {}
        }
    }
}

/// A run of spans with the styling forgotten — what a heading is called.
fn plain_text(spans: &[Span]) -> String {
    let mut out = String::new();
    for s in spans {
        out.push_str(&s.text);
    }
    out.trim().to_string()
}

/// GitHub's heading slug: lowercased, punctuation dropped, spaces hyphenated.
///
/// Written to match github.com rather than to be pretty, because the links it
/// has to answer were written against github.com — a description with its own
/// table of contents in it is the whole reason this exists.
fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            ' ' | '\t' | '\n' => out.push('-'),
            '-' | '_' => out.push(c),
            c if c.is_alphanumeric() => out.extend(c.to_lowercase()),
            _ => {}
        }
    }
    out
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
    /// True when the source had raw HTML in it that nothing was drawn for. A
    /// reader looking at a gap where a row of badges should be deserves to be
    /// told why rather than left wondering — and, just as much, *not* to be
    /// told when the HTML was a `<details>` that is on screen or a comment
    /// left in a template that was never meant to be.
    pub raw_html: bool,
}

/// What the shorthand in a body refers to.
///
/// `#123` and `@name` are references on github.com and nothing at all in a
/// README — one is a fragment, the other an npm scope or an email. So they are
/// only read as references for text that came from a repository's own pull
/// requests, and this is what says which repository that was.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Refs {
    /// `owner/name`, when there is one.
    pub repo: Option<String>,
}

impl Refs {
    pub fn of(repo: impl Into<String>) -> Self {
        Refs {
            repo: Some(repo.into()),
        }
    }
}

/// Parse markdown into blocks.
pub fn parse(text: &str) -> Doc {
    parse_refs(text, &Refs::default())
}

/// The same, for a body written against a repository — a pull request's
/// description, a review comment — where `#123` and `@name` mean something.
pub fn parse_refs(text: &str, refs: &Refs) -> Doc {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        // `> [!NOTE]` and the four beside it.
        | Options::ENABLE_GFM;
    let mut reader = Reader {
        events: Parser::new_ext(text, options),
        raw_html: false,
        refs: refs.clone(),
        slugs: HashMap::new(),
    };
    let blocks = reader.container(&mut None, None);
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
    /// Raw HTML passed through in the middle of the text that nothing was
    /// drawn for.
    raw_html: bool,
    refs: Refs,
}

impl Inline {
    fn new(refs: &Refs) -> Self {
        Inline {
            refs: refs.clone(),
            ..Inline::default()
        }
    }

    /// The runs read so far, with whatever was written as text and meant as a
    /// link turned into one: a bare URL, and — in a body from a repository's
    /// own GitHub — a `#123` or an `@name`.
    ///
    /// Done here, once a whole run of inline events has been folded in, rather
    /// than as each one arrives. The parser splits its text at every character
    /// that could have opened emphasis, so `.../Foo_(bar)` reaches [`push`] in
    /// two pieces and is only one URL again after they have been joined.
    ///
    /// [`push`]: Self::push
    fn take(&mut self) -> Vec<Span> {
        let spans = std::mem::take(&mut self.spans);
        linkify(spans, &self.refs)
    }

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

    /// A tag in the middle of the text. Three of them are drawn; the rest is
    /// noted as dropped — see this module's header for where that line is.
    fn html(&mut self, raw: &str) {
        let text = raw.trim();
        match tag_of(text) {
            // A `<br>` is the one piece of markup that is unambiguously a
            // thing on the page rather than a thing around it.
            Some("br") => self.push("\n"),
            Some("img") => match attr(text, "src") {
                Some(src) => {
                    let alt = attr(text, "alt").unwrap_or_default();
                    self.spans.push(Span {
                        text: alt,
                        style: self.style,
                        link: self.link.clone(),
                        image: Some(src),
                    });
                }
                None => self.raw_html = true,
            },
            // The instructions in a pull request template, left in by whoever
            // filled it out. Nothing was meant to be seen, so nothing is
            // missing and there is nothing to warn about.
            _ if is_comment(text) => {}
            _ => self.raw_html = true,
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
            Event::Html(t) | Event::InlineHtml(t) => self.html(&t),
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

/// What reading one block turned up. Mostly a block; the two that are not are
/// the halves of a `<details>`, which is written as markup around markdown
/// rather than as one thing the parser hands over whole.
enum Piece {
    Block(Block),
    /// The label of the fold this container is the inside of.
    Summary(Vec<Span>),
    /// `</details>` — the end of it.
    Close,
    /// Read, and nothing to draw.
    None,
}

struct Reader<'a> {
    events: Parser<'a>,
    raw_html: bool,
    refs: Refs,
    /// Every heading slug handed out so far, and how many times. Two sections
    /// called "Testing" are `testing` and `testing-1` — github.com's rule, and
    /// therefore the rule a link written for github.com expects.
    slugs: HashMap<String, usize>,
}

impl Reader<'_> {
    /// Everything inside one container — the document, a block quote, a list
    /// item, or the inside of a fold — up to the `End` that closes it.
    ///
    /// A tight list puts its text straight inside the item with no paragraph
    /// around it, so loose inline events are collected here too and flushed as
    /// a paragraph when a block interrupts them.
    ///
    /// `summary` is where a `<summary>` found in here goes, and `Some` is also
    /// what says this container is the inside of a fold — the one place a
    /// `</details>` is the end of something rather than stray markup.
    fn container(
        &mut self,
        task: &mut Option<bool>,
        summary: Option<&mut Vec<Span>>,
    ) -> Vec<Block> {
        let mut out = Vec::new();
        let mut summary = summary;
        let mut loose = Inline::new(&self.refs);
        while let Some(ev) = self.events.next() {
            match ev {
                Event::TaskListMarker(done) => *task = Some(done),
                Event::Rule => out.push(Block::Rule),
                Event::Start(tag) if is_block(&tag) => {
                    flush(&mut loose, &mut out);
                    match self.block(tag) {
                        Piece::Block(b) => out.push(b),
                        Piece::Summary(spans) => match summary.as_deref_mut() {
                            // A second one is somebody else's fold, misnested.
                            Some(slot) if slot.is_empty() => *slot = spans,
                            _ => {}
                        },
                        Piece::Close if summary.is_some() => break,
                        _ => {}
                    }
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

    /// The block a start tag opens, having read it to its end.
    fn block(&mut self, tag: Tag) -> Piece {
        match tag {
            Tag::Paragraph => Piece::Block(Block::Para(self.inline())),
            Tag::Heading { level, .. } => {
                let spans = self.inline();
                let id = self.slug_for(&plain_text(&spans));
                Piece::Block(Block::Heading {
                    level: level as u8,
                    id,
                    spans,
                })
            }
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
                Piece::Block(Block::Code {
                    lang,
                    text: self.verbatim(),
                })
            }
            Tag::BlockQuote(kind) => Piece::Block(Block::Quote {
                alert: kind.map(Alert::of),
                blocks: self.container(&mut None, None),
            }),
            Tag::List(start) => Piece::Block(self.list(start)),
            Tag::Table(_) => Piece::Block(self.table()),
            Tag::HtmlBlock => self.html_block(),
            // Footnote definitions, front matter, definition lists.
            _ => {
                self.skip();
                Piece::None
            }
        }
    }

    /// A run of markup on lines of its own.
    ///
    /// A `<details>` opens a fold and everything up to the `</details>` goes
    /// inside it; a `<summary>` names the fold it is in; a comment is dropped
    /// silently. Everything else is noted as not drawn.
    fn html_block(&mut self) -> Piece {
        let raw = self.verbatim_html();
        let text = raw.trim();
        if is_comment(text) {
            return Piece::None;
        }
        match tag_of(text) {
            Some("/details") => Piece::Close,
            Some("summary") => match inner(text, "summary") {
                Some(label) => Piece::Summary(inline_of(&label, &self.refs)),
                None => Piece::None,
            },
            Some("details") => Piece::Block(self.details(text)),
            // A picture on a line of its own — a screenshot in a description,
            // sized with an attribute markdown has no way of writing.
            Some("img") => match self.pictures(text) {
                Some(spans) => Piece::Block(Block::Para(spans)),
                None => {
                    self.raw_html = true;
                    Piece::None
                }
            },
            _ => {
                self.raw_html = true;
                Piece::None
            }
        }
    }

    /// A fold, from the `<details>` that opens it.
    ///
    /// The `<summary>` is usually on the line under it and so in the same run
    /// of markup; when it is not, the container inside reports the one it
    /// finds. A run holding its own `</details>` is the whole fold written
    /// without a blank line in it — which is markdown nobody's renderer looks
    /// inside, so what is between the tags is taken as the text it is.
    fn details(&mut self, text: &str) -> Block {
        let open = attr(text, "open").is_some();
        let mut summary = inner(text, "summary")
            .map(|label| inline_of(&label, &self.refs))
            .unwrap_or_default();
        if let Some(rest) = after_close(text, "details") {
            let body = strip_tags(&rest);
            let blocks = if body.trim().is_empty() {
                Vec::new()
            } else {
                vec![Block::Para(inline_of(&body, &self.refs))]
            };
            return Block::Details {
                summary,
                open,
                blocks,
            };
        }
        let blocks = self.container(&mut None, Some(&mut summary));
        Block::Details {
            summary,
            open,
            blocks,
        }
    }

    /// Every `<img>` in a run of markup, as the runs they are drawn as.
    /// `None` when there was something else in there too.
    fn pictures(&mut self, text: &str) -> Option<Vec<Span>> {
        let mut out = Vec::new();
        for tag in text.split_inclusive('>') {
            let tag = tag.trim();
            if tag.is_empty() {
                continue;
            }
            if tag_of(tag) != Some("img") {
                return None;
            }
            out.push(Span {
                text: attr(tag, "alt").unwrap_or_default(),
                style: Style::default(),
                link: None,
                image: Some(attr(tag, "src")?),
            });
        }
        (!out.is_empty()).then_some(out)
    }

    /// A slug nothing else in this document has.
    fn slug_for(&mut self, text: &str) -> String {
        let base = slug(text);
        let seen = self.slugs.entry(base.clone()).or_insert(0);
        *seen += 1;
        match *seen {
            1 => base,
            n => format!("{base}-{}", n - 1),
        }
    }

    fn inline(&mut self) -> Vec<Span> {
        let mut acc = Inline::new(&self.refs);
        for ev in self.events.by_ref() {
            if !acc.step(ev) {
                break;
            }
        }
        self.raw_html |= acc.raw_html;
        acc.take()
    }

    /// The markup of one HTML block, which arrives as html events and nothing
    /// else.
    fn verbatim_html(&mut self) -> String {
        let mut out = String::new();
        for ev in self.events.by_ref() {
            match ev {
                Event::Html(t) | Event::Text(t) => out.push_str(&t),
                Event::End(_) => break,
                _ => {}
            }
        }
        out
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
                    let blocks = self.container(&mut task, None);
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

// ------------------------------------------------------------- a little HTML

/// The name of the tag a run of markup opens, lowercased — `"br"`, `"img"`,
/// `"details"`, and `"/details"` for a closing one. `None` when it does not
/// begin with a tag at all.
fn tag_of(text: &str) -> Option<&'static str> {
    const TAGS: [&str; 5] = ["br", "img", "details", "summary", "/details"];
    let rest = text.strip_prefix('<')?;
    let name = rest
        .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .map(|i| &rest[..i.max(1)])
        .unwrap_or(rest);
    // `</details>` — the slash is part of the name, so it is measured from the
    // character after it.
    let name = match rest.strip_prefix('/') {
        Some(tail) => {
            let n = tail
                .find(|c: char| c.is_whitespace() || c == '>')
                .unwrap_or(tail.len());
            &rest[..n + 1]
        }
        None => name,
    };
    TAGS.into_iter().find(|t| t.eq_ignore_ascii_case(name))
}

/// Whether a run of markup is nothing but comments — the instructions in a
/// pull request template, left in by whoever filled it out.
fn is_comment(text: &str) -> bool {
    let mut rest = text.trim();
    if rest.is_empty() {
        return false;
    }
    while let Some(open) = rest.strip_prefix("<!--") {
        match open.find("-->") {
            Some(end) => rest = open[end + 3..].trim_start(),
            None => return true,
        }
    }
    rest.is_empty()
}

/// The value of one attribute of the first tag in a run of markup. Quoted or
/// bare; `Some("")` for one written with no value at all, as `open` is.
fn attr(text: &str, name: &str) -> Option<String> {
    // The tag itself and nothing after it. `<details>` and its `<summary>`
    // arrive as one run of markup, and a summary reading "open the box" is not
    // a `<details open>`.
    let text = &text[..text.find('>').map(|i| i + 1).unwrap_or(text.len())];
    let lower = text.to_ascii_lowercase();
    let mut from = 0;
    loop {
        let at = from + lower[from..].find(name)?;
        from = at + name.len();
        // A whole word: `src` is not the tail of `data-src`, and the character
        // after it is the `=` or the space that ends it.
        let before_ok = at == 0
            || !lower[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '-' || c == '_');
        if !before_ok {
            continue;
        }
        let rest = lower[from..].trim_start();
        if !rest.starts_with('=') {
            // `<details open>` — an attribute that is its own value.
            if rest.starts_with('>') || rest.starts_with(char::is_whitespace) || rest.is_empty() {
                return Some(String::new());
            }
            continue;
        }
        let eq = from + lower[from..].find('=')? + 1;
        let value = text[eq..].trim_start();
        let (quote, value) = match value.chars().next()? {
            q @ ('"' | '\'') => (Some(q), &value[1..]),
            _ => (None, value),
        };
        let end = match quote {
            Some(q) => value.find(q)?,
            None => value
                .find(|c: char| c.is_whitespace() || c == '>')
                .unwrap_or(value.len()),
        };
        return Some(unescape(&value[..end]));
    }
}

/// What one pair of tags encloses, from the first `<name>` in a run of markup.
fn inner(text: &str, name: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let open = lower.find(&format!("<{name}"))?;
    let from = open + text[open..].find('>')? + 1;
    let end = lower[from..].find(&format!("</{name}"))?;
    Some(text[from..from + end].to_string())
}

/// Whatever a run of markup has after the tag that closes `name`.
fn after_close(text: &str, name: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let close = lower.find(&format!("</{name}"))?;
    let from = close + text[close..].find('>')?;
    // What is *before* it, minus the summary that named the fold.
    let body = match lower.find("</summary>") {
        Some(after) if after < close => &text[after + "</summary>".len()..close],
        _ => match text[..close].find('>') {
            Some(i) => &text[i + 1..close],
            None => "",
        },
    };
    let _ = from;
    Some(body.to_string())
}

/// Markup with the tags taken out and the entities put back — the last resort
/// for a fold written with no blank line in it, where what is between the tags
/// is text and nothing else was going to draw it.
fn strip_tags(text: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for c in text.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            c if depth == 0 => out.push(c),
            _ => {}
        }
    }
    unescape(out.trim())
}

/// The five entities markup written by hand actually uses.
fn unescape(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

/// A scrap of markdown as the runs it is set in — a `<summary>`'s label, which
/// is written as prose and often has a `**bold**` or a `` `path` `` in it.
fn inline_of(text: &str, refs: &Refs) -> Vec<Span> {
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    match parse_refs(text, refs).blocks.into_iter().next() {
        Some(Block::Para(spans)) | Some(Block::Heading { spans, .. }) => spans,
        _ => vec![Span {
            text: text.to_string(),
            style: Style::default(),
            link: None,
            image: None,
        }],
    }
}

// ------------------------------------------------------------- autolinking

/// Split every run that is plain text where there is a link written in it.
///
/// A run that is already a link, a picture's alt text or a piece of code is
/// left alone: those are three ways of saying "this text is not prose".
fn linkify(spans: Vec<Span>, refs: &Refs) -> Vec<Span> {
    if !spans
        .iter()
        .any(|s| s.link.is_none() && s.image.is_none() && !s.style.code)
    {
        return spans;
    }
    let mut out = Vec::with_capacity(spans.len());
    for s in spans {
        let found = match s.link.is_none() && s.image.is_none() && !s.style.code {
            true => links_in(&s.text, refs),
            false => Vec::new(),
        };
        if found.is_empty() {
            out.push(s);
            continue;
        }
        let mut at = 0;
        let piece = |text: &str, link: Option<String>| Span {
            text: text.to_string(),
            style: s.style,
            link,
            image: None,
        };
        for (start, end, url) in found {
            if start > at {
                out.push(piece(&s.text[at..start], None));
            }
            out.push(piece(&s.text[start..end], Some(url)));
            at = end;
        }
        if at < s.text.len() {
            out.push(piece(&s.text[at..], None));
        }
    }
    out
}

/// Where in a run of plain text there is something written as text and meant
/// as a link, as `(start, end, url)` in the order it is written.
fn links_in(text: &str, refs: &Refs) -> Vec<(usize, usize, String)> {
    // Nothing to find is the common case, and it costs a byte scan to know.
    if !text.contains("://")
        && !text.contains("www.")
        && !(refs.repo.is_some() && (text.contains('#') || text.contains('@')))
    {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut prev = ' ';
    let mut chars = text.char_indices();
    while let Some((i, c)) = chars.next() {
        // A `#` inside a word is a fragment or a colour, and a `www.` inside
        // one is the tail of something longer.
        let edge = !(prev.is_alphanumeric() || prev == '_' || prev == '-' || prev == '/');
        prev = c;
        if !edge {
            continue;
        }
        let rest = &text[i..];
        let found = url_at(rest).or_else(|| {
            refs.repo
                .as_deref()
                .and_then(|repo| reference_at(rest, repo))
        });
        let Some((len, url)) = found else { continue };
        out.push((i, i + len, url));
        // The scan resumes after the link, not inside it.
        for (j, c) in chars.by_ref() {
            prev = c;
            if j + c.len_utf8() >= i + len {
                break;
            }
        }
    }
    out
}

/// A bare URL at the start of `rest`, as its length and where it goes.
fn url_at(rest: &str) -> Option<(usize, String)> {
    let starts = |p: &str| rest.len() > p.len() && rest[..p.len()].eq_ignore_ascii_case(p);
    let bare = match () {
        _ if starts("https://") || starts("http://") => false,
        // `www.example.com`, written without a scheme, as half the internet
        // writes it.
        _ if starts("www.") => true,
        _ => return None,
    };
    let end = rest
        .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '`' | '\\'))
        .unwrap_or(rest.len());
    let end = trimmed(&rest[..end]);
    // A scheme and nothing after it is not a link, and neither is `www.`.
    let least = if bare {
        "www.".len()
    } else {
        rest.find("//")? + 2
    };
    if end <= least {
        return None;
    }
    let url = &rest[..end];
    Some((
        end,
        if bare {
            format!("https://{url}")
        } else {
            url.to_string()
        },
    ))
}

/// How much of a URL is the URL. A link at the end of a sentence takes the
/// full stop with it otherwise, and one in brackets takes the bracket.
fn trimmed(url: &str) -> usize {
    let mut end = url.len();
    while let Some(c) = url[..end].chars().next_back() {
        let cut = match c {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' | '*' | '_' | '~' => true,
            ')' | ']' | '}' => {
                let open = match c {
                    ')' => '(',
                    ']' => '[',
                    _ => '{',
                };
                // A closer the URL opened itself is part of it — which is what
                // a wikipedia link is made of.
                url[..end].matches(c).count() > url[..end].matches(open).count()
            }
            _ => false,
        };
        if !cut {
            break;
        }
        end -= c.len_utf8();
    }
    end
}

/// `#123` or `@name` at the start of `rest`, read against the repository the
/// body was written in.
fn reference_at(rest: &str, repo: &str) -> Option<(usize, String)> {
    let after = |len: usize| {
        rest[len..]
            .chars()
            .next()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '-' || c == '/'))
    };
    match rest.chars().next()? {
        '#' => {
            let digits = rest[1..]
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len() - 1);
            // Long enough to be a number, short enough to be an issue.
            if !(1..=9).contains(&digits) || !after(1 + digits) {
                return None;
            }
            // `/issues/` answers for pull requests too — github.com redirects.
            Some((
                1 + digits,
                format!("https://github.com/{repo}/issues/{}", &rest[1..1 + digits]),
            ))
        }
        '@' => {
            let name = rest[1..]
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
                .unwrap_or(rest.len() - 1);
            let handle = &rest[1..1 + name];
            // GitHub's own rule for a login, which is what keeps an email
            // address and a rust lifetime out of this.
            let ok = (1..=39).contains(&name)
                && !handle.starts_with('-')
                && !handle.ends_with('-')
                && after(1 + name);
            ok.then(|| (1 + name, format!("https://github.com/{handle}")))
        }
        _ => None,
    }
}

/// Turn whatever inline text has piled up into a paragraph. Whitespace alone is
/// the gap between two blocks, not a paragraph of its own — but a picture with
/// no alt text is nothing *written* and something to draw.
fn flush(acc: &mut Inline, out: &mut Vec<Block>) {
    let spans = acc.take();
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
            Block::Heading { level, id, spans } => {
                assert_eq!(*level, 1);
                assert_eq!(plain(spans), "Title");
                assert_eq!(id, "title");
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
            Block::Quote { alert, blocks } => {
                assert_eq!(*alert, None);
                assert_eq!(paras(blocks), vec!["quoted"]);
            }
            other => panic!("expected a quote, got {other:?}"),
        }
        assert!(matches!(blocks[1], Block::Rule));
    }

    #[test]
    fn alerts_are_the_quotes_that_say_they_are_one() {
        let blocks = doc("> [!WARNING]\n> Do not.\n");
        match &blocks[0] {
            Block::Quote { alert, blocks } => {
                assert_eq!(*alert, Some(Alert::Warning));
                assert_eq!(paras(blocks), vec!["Do not."]);
            }
            other => panic!("expected a quote, got {other:?}"),
        }
        // And the marker is not left in the text of the first line.
        for kind in ["NOTE", "TIP", "IMPORTANT", "CAUTION"] {
            let blocks = doc(&format!("> [!{kind}]\n> body\n"));
            let Block::Quote { alert, blocks } = &blocks[0] else {
                panic!("expected a quote for {kind}")
            };
            assert!(alert.is_some(), "{kind} was read as a plain quote");
            assert_eq!(paras(blocks), vec!["body"]);
        }
    }

    #[test]
    fn headings_are_slugged_the_way_github_slugs_them() {
        let blocks = doc("## Why not `serde`?\n\n### Testing\n\n### Testing\n");
        let ids: Vec<&str> = blocks
            .iter()
            .filter_map(|b| match b {
                Block::Heading { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        // Punctuation dropped, spaces hyphenated, and the second "Testing"
        // takes the suffix — which is the rule a link written on github.com
        // was written against.
        assert_eq!(ids, ["why-not-serde", "testing", "testing-1"]);
    }

    #[test]
    fn the_outline_is_every_heading_in_reading_order() {
        let text = "# Top\n\ntext\n\n## One\n\n<details>\n<summary>More</summary>\n\n### Deep\n\n</details>\n";
        let out = outline(&doc(text));
        let seen: Vec<(u8, &str)> = out.iter().map(|e| (e.level, e.text.as_str())).collect();
        assert_eq!(seen, [(1, "Top"), (2, "One"), (3, "Deep")]);
        assert_eq!(out[1].id, "one");
    }

    #[test]
    fn a_fold_holds_the_blocks_between_its_tags() {
        let text = "<details>\n<summary>Test plan</summary>\n\nRan it.\n\n</details>\n\nafter\n";
        let parsed = parse(text);
        let Block::Details {
            summary,
            open,
            blocks,
        } = &parsed.blocks[0]
        else {
            panic!("expected a fold: {:?}", parsed.blocks)
        };
        assert_eq!(plain(summary), "Test plan");
        assert!(!open);
        assert_eq!(paras(blocks), vec!["Ran it."]);
        // What follows the fold is outside it, and drawing the fold is not
        // "raw HTML not drawn".
        assert_eq!(paras(&parsed.blocks), vec!["after"]);
        assert!(!parsed.raw_html, "the fold was drawn, so nothing was lost");
    }

    #[test]
    fn a_fold_written_on_one_line_does_not_swallow_the_document() {
        // No blank line in it, so markdown never looks inside — which means
        // the text between the tags is text, and the `</details>` is here
        // rather than somewhere further down.
        let text = "<details><summary>Logs</summary>it crashed</details>\n\nafter\n";
        let parsed = parse(text);
        let Block::Details {
            summary, blocks, ..
        } = &parsed.blocks[0]
        else {
            panic!("expected a fold: {:?}", parsed.blocks)
        };
        assert_eq!(plain(summary), "Logs");
        assert_eq!(paras(blocks), vec!["it crashed"]);
        assert_eq!(paras(&parsed.blocks), vec!["after"], "the rest survives");
    }

    #[test]
    fn a_summary_on_a_line_of_its_own_still_names_its_fold() {
        // A blank line after the `<details>` puts the summary in a run of
        // markup of its own, which reaches the fold from the inside.
        let text = "<details>\n\n<summary>Logs</summary>\n\nbody\n\n</details>\n";
        let Block::Details {
            summary, blocks, ..
        } = &parse(text).blocks[0]
        else {
            panic!("expected a fold")
        };
        assert_eq!(plain(summary), "Logs");
        assert_eq!(paras(blocks), vec!["body"]);
    }

    #[test]
    fn folds_nest() {
        let text = "<details>\n<summary>Outer</summary>\n\n<details>\n<summary>Inner</summary>\n\ndeep\n\n</details>\n\nshallow\n\n</details>\n\nafter\n";
        let parsed = parse(text);
        let Block::Details {
            summary, blocks, ..
        } = &parsed.blocks[0]
        else {
            panic!("expected a fold: {:?}", parsed.blocks)
        };
        assert_eq!(plain(summary), "Outer");
        assert_eq!(paras(blocks), vec!["shallow"]);
        let Some(Block::Details {
            summary: inner,
            blocks: deep,
            ..
        }) = blocks.iter().find(|b| matches!(b, Block::Details { .. }))
        else {
            panic!("the inner fold is gone: {blocks:?}")
        };
        assert_eq!(plain(inner), "Inner");
        assert_eq!(paras(deep), vec!["deep"]);
        // And the outer one closed where it said it did.
        assert_eq!(paras(&parsed.blocks), vec!["after"]);
    }

    #[test]
    fn a_fold_that_says_it_is_open_says_so() {
        // And one that only has the word in its summary does not.
        let shut = "<details>\n<summary>How to open the box</summary>\n\nbody\n\n</details>\n";
        let Block::Details { open, .. } = &parse(shut).blocks[0] else {
            panic!("expected a fold")
        };
        assert!(!open, "a summary reading `open` is not an open attribute");

        let text = "<details open>\n<summary>Shown</summary>\n\nbody\n\n</details>\n";
        let Block::Details { open, .. } = &parse(text).blocks[0] else {
            panic!("expected a fold")
        };
        assert!(open);
    }

    #[test]
    fn a_template_s_own_instructions_are_not_a_gap_to_explain() {
        // Every pull request opened from a template has these in it, and
        // warning about markup that was never going to be seen is noise.
        let parsed = parse("<!-- Describe your change -->\n\nI changed it.\n");
        assert_eq!(paras(&parsed.blocks), vec!["I changed it."]);
        assert!(!parsed.raw_html, "a comment is not a missing badge row");
        // Whereas markup that really was dropped still says so.
        assert!(parse("<div align=center>x</div>\n").raw_html);
    }

    #[test]
    fn a_line_break_and_a_picture_written_as_html_are_drawn() {
        let parsed = parse("one<br>two\n");
        assert_eq!(paras(&parsed.blocks), vec!["one\ntwo"]);
        assert!(!parsed.raw_html);

        let parsed = parse("<img width=\"600\" src=\"shot.png\" alt=\"the pane\">\n");
        let Block::Para(spans) = &parsed.blocks[0] else {
            panic!("expected a paragraph: {:?}", parsed.blocks)
        };
        assert_eq!(spans[0].image.as_deref(), Some("shot.png"));
        assert_eq!(spans[0].text, "the pane");
        assert!(!parsed.raw_html);
    }

    #[test]
    fn a_bare_url_is_a_link_and_the_full_stop_after_it_is_not() {
        let Block::Para(spans) = &doc("see https://example.com/a_b. ok")[0] else {
            panic!("expected a paragraph")
        };
        let link = spans.iter().find(|s| s.link.is_some()).unwrap();
        assert_eq!(link.text, "https://example.com/a_b");
        assert_eq!(link.link.as_deref(), Some("https://example.com/a_b"));
        assert_eq!(plain(spans), "see https://example.com/a_b. ok");

        // A closer the URL opened is part of it; one it did not is not.
        let one = |text: &str| {
            let Block::Para(spans) = &doc(text)[0] else {
                panic!("expected a paragraph")
            };
            spans.iter().find_map(|s| s.link.clone())
        };
        assert_eq!(
            one("(https://en.wikipedia.org/wiki/Foo_(bar))").as_deref(),
            Some("https://en.wikipedia.org/wiki/Foo_(bar)")
        );
        assert_eq!(
            one("www.example.com works").as_deref(),
            Some("https://www.example.com"),
            "written without a scheme, as half the internet writes it"
        );
        // Not inside code, and not a second link over a link.
        assert_eq!(one("`https://example.com`"), None);
        assert_eq!(
            one("[docs](https://a.example) and https://b.example").as_deref(),
            Some("https://a.example"),
            "the written link comes first and keeps its own target"
        );
    }

    #[test]
    fn issue_and_user_references_are_links_only_where_they_mean_something() {
        let refs = Refs::of("bigmah/pullspace");
        let links = |text: &str, refs: &Refs| {
            let blocks = parse_refs(text, refs).blocks;
            let Block::Para(spans) = &blocks[0] else {
                panic!("expected a paragraph: {blocks:?}")
            };
            spans
                .iter()
                .filter_map(|s| s.link.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            links("fixes #123, thanks @octocat", &refs),
            [
                "https://github.com/bigmah/pullspace/issues/123",
                "https://github.com/octocat"
            ]
        );
        // A README has no pull requests behind it: there, the same text is a
        // fragment and an npm scope.
        assert!(links("fixes #123, thanks @octocat", &Refs::default()).is_empty());
        // And neither is a colour or the middle of a word.
        assert!(links("#fff and a#1 and me@example.com", &refs).is_empty());
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
