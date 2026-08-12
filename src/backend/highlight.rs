use std::path::Path;
use std::sync::OnceLock;

use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};
use syntect::util::LinesWithEndings;

#[derive(Clone, PartialEq)]
pub struct Span {
    pub color: String, // css hex color
    pub text: String,
}

static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
static THEME: OnceLock<Theme> = OnceLock::new();

fn syntaxes() -> &'static SyntaxSet {
    SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme() -> &'static Theme {
    THEME.get_or_init(|| {
        let ts = ThemeSet::load_defaults();
        ts.themes["base16-ocean.dark"].clone()
    })
}

/// Syntax-highlight a whole file into per-line colored spans.
/// Trailing newlines are stripped from the emitted spans.
pub fn highlight(path: &Path, content: &str) -> Vec<Vec<Span>> {
    let ss = syntaxes();
    let syntax = path
        .extension()
        .and_then(|e| e.to_str())
        .and_then(|e| ss.find_syntax_by_extension(e))
        .or_else(|| {
            content
                .lines()
                .next()
                .and_then(|l| ss.find_syntax_by_first_line(l))
        })
        .unwrap_or_else(|| ss.find_syntax_plain_text());
    run(syntax, content)
}

/// The same, for a fenced code block in markdown — where the language arrives
/// as the word after the fence (`rust`, `sh`, `jsx`) rather than as a path.
/// An unknown or missing one is plain text, which is what it looks like anyway.
pub fn highlight_lang(lang: &str, content: &str) -> Vec<Vec<Span>> {
    let ss = syntaxes();
    let syntax = ss
        .find_syntax_by_token(lang)
        .unwrap_or_else(|| ss.find_syntax_plain_text());
    run(syntax, content)
}

fn run(syntax: &SyntaxReference, content: &str) -> Vec<Vec<Span>> {
    let ss = syntaxes();
    let mut hl = HighlightLines::new(syntax, theme());
    let mut out = Vec::new();
    for line in LinesWithEndings::from(content) {
        let spans = match hl.highlight_line(line, ss) {
            Ok(ranges) => ranges
                .into_iter()
                .filter_map(|(style, text)| {
                    let text = text.trim_end_matches(['\n', '\r']);
                    if text.is_empty() {
                        return None;
                    }
                    let fg = style.foreground;
                    Some(Span {
                        color: format!("#{:02x}{:02x}{:02x}", fg.r, fg.g, fg.b),
                        text: text.to_string(),
                    })
                })
                .collect(),
            Err(_) => vec![Span {
                color: "#c0c5ce".to_string(),
                text: line.trim_end_matches(['\n', '\r']).to_string(),
            }],
        };
        out.push(spans);
    }
    out
}
