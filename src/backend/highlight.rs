use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

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
static DARK: OnceLock<Theme> = OnceLock::new();
static LIGHT: OnceLock<Theme> = OnceLock::new();

/// Which of the two the code is being coloured with.
///
/// A plain flag rather than a signal, because this is read from inside
/// `highlight`, which is called from background walks as well as from the
/// viewer. It is written once, by the setter that changes the app's theme, and
/// before the signal that redraws anything — so the colours and the palette
/// they sit on can never disagree.
static ON_LIGHT: AtomicBool = AtomicBool::new(false);

/// Colour the code for a light page, or a dark one.
pub fn use_light(light: bool) {
    ON_LIGHT.store(light, Ordering::Relaxed);
}

fn syntaxes() -> &'static SyntaxSet {
    SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// A theme out of syntect's own defaults, or — if the build ever ships without
/// it — whatever is first, which is still a theme rather than a panic.
fn load(name: &str) -> Theme {
    let mut ts = ThemeSet::load_defaults();
    ts.themes
        .remove(name)
        .or_else(|| ts.themes.values().next().cloned())
        .unwrap_or_default()
}

/// Syntax colours follow the app's own. Dark syntax on a light page is not a
/// preference, it is unreadable — so this is not a separate setting.
fn theme() -> &'static Theme {
    if ON_LIGHT.load(Ordering::Relaxed) {
        LIGHT.get_or_init(|| load("InspiredGitHub"))
    } else {
        DARK.get_or_init(|| load("base16-ocean.dark"))
    }
}

/// What a line that could not be parsed is drawn in: the theme's own body text.
fn plain_color() -> String {
    let fg = theme()
        .settings
        .foreground
        .unwrap_or(syntect::highlighting::Color {
            r: 0xc0,
            g: 0xc5,
            b: 0xce,
            a: 0xff,
        });
    format!("#{:02x}{:02x}{:02x}", fg.r, fg.g, fg.b)
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
                color: plain_color(),
                text: line.trim_end_matches(['\n', '\r']).to_string(),
            }],
        };
        out.push(spans);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`load`] falls back rather than panicking on a name syntect does not
    /// have, which is the right behaviour and an invisible one — a renamed
    /// theme would quietly hand the light page its dark colours.
    #[test]
    fn both_themes_are_the_ones_asked_for() {
        let ts = ThemeSet::load_defaults();
        for name in ["base16-ocean.dark", "InspiredGitHub"] {
            assert!(ts.themes.contains_key(name), "syntect has no {name:?}");
        }
    }

    /// And they are actually different themes: one for reading on black, one
    /// for reading on white.
    #[test]
    fn the_two_themes_are_lit_the_other_way_up() {
        let dark = load("base16-ocean.dark");
        let light = load("InspiredGitHub");
        let brightness = |t: &Theme| {
            let c = t.settings.background.expect("a theme has a background");
            c.r as u16 + c.g as u16 + c.b as u16
        };
        assert!(
            brightness(&light) > brightness(&dark),
            "the light theme is not the lighter one"
        );
    }
}
