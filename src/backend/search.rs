//! Finding text in files.
//!
//! Everything here is pure: a pattern goes in, and lines of text go past. What
//! supplies the text is [`scan`](super::scan), which reads it out of the local
//! store — the split matters, because it is what makes the matching testable
//! without a browser underneath it.
//!
//! Three toggles, the same three every editor has: match case, whole word, and
//! regular expression. They compose into one [`Regex`], which is also how the
//! whole-word search behind Find References is built — a reference is a
//! case-sensitive, whole-word match on an identifier and nothing more clever
//! than that.

use std::path::{Path, PathBuf};

use regex::{Regex, RegexBuilder};

/// How many hits one search keeps. Past this it stops walking: nobody reads the
/// six hundredth result, and every one of them is a row to draw.
pub const MAX_HITS: usize = 500;

/// How much of a matching line to keep. Long enough to see the match in
/// context, short enough that one minified line cannot fill the panel.
const MAX_PREVIEW: usize = 240;

/// How many matches on one line to mark. The row shows the line once however
/// many times it matches.
const MAX_MARKS: usize = 12;

/// The three toggles, as the buttons in the search box set them.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Options {
    /// `Aa` — off means the search is case-insensitive.
    pub case: bool,
    /// `ab|` — the pattern has to be bounded by non-word characters.
    pub word: bool,
    /// `.*` — take the pattern as a regular expression rather than as text.
    pub regex: bool,
}

/// One matching line.
#[derive(Clone, PartialEq, Debug)]
pub struct Hit {
    /// Repo-relative.
    pub path: PathBuf,
    /// 1-based, the way every editor counts them.
    pub line: usize,
    /// The line, trimmed of its indentation and capped in length.
    pub text: String,
    /// Byte ranges within `text` that matched, so the row can pick them out.
    /// Empty when the match fell off the end of the preview.
    pub marks: Vec<(usize, usize)>,
}

/// A compiled pattern.
#[derive(Debug)]
pub struct Matcher {
    re: Regex,
}

/// Build the pattern the three toggles describe.
///
/// Case folding goes in as a builder flag rather than as `(?i)` in the pattern
/// text: inline, it would be part of a user-written regex, and a pattern
/// carrying its own `(?-i)` would silently win the argument with the button.
pub fn compile(pattern: &str, opts: Options) -> Result<Matcher, String> {
    if pattern.is_empty() {
        return Err("nothing to search for".to_string());
    }
    let body = if opts.regex {
        pattern.to_string()
    } else {
        regex::escape(pattern)
    };
    // Non-capturing, so `a|b` as a whole-word search means "the word a or the
    // word b" rather than "the word a, or b anywhere".
    let body = if opts.word {
        format!(r"\b(?:{body})\b")
    } else {
        body
    };
    RegexBuilder::new(&body)
        .case_insensitive(!opts.case)
        .size_limit(1 << 22)
        .build()
        .map(|re| Matcher { re })
        .map_err(|e| first_line(&e.to_string()))
}

/// A whole-word, case-sensitive search for one identifier — what Find
/// References runs.
pub fn references_to(name: &str) -> Result<Matcher, String> {
    compile(
        name,
        Options {
            case: true,
            word: true,
            regex: false,
        },
    )
}

/// regex's errors are several lines of diagram pointing at the offending
/// character, which is excellent in a terminal and does not fit in a text box.
fn first_line(err: &str) -> String {
    err.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("regex parse error"))
        .unwrap_or("not a valid regular expression")
        .to_string()
}

impl Matcher {
    /// Collect every matching line of one file into `out`.
    ///
    /// Returns false once `out` is full, which is the walk's signal to stop
    /// reading the rest of the repository.
    pub fn scan(&self, path: &Path, text: &str, out: &mut Vec<Hit>) -> bool {
        if out.len() >= MAX_HITS {
            return false;
        }
        for (i, line) in text.lines().enumerate() {
            // Cheap rejection first: `is_match` stops at the first hit, and
            // most lines of most files are not one.
            if !self.re.is_match(line) {
                continue;
            }
            out.push(self.hit(path, i + 1, line));
            if out.len() >= MAX_HITS {
                return false;
            }
        }
        true
    }

    fn hit(&self, path: &Path, line: usize, raw: &str) -> Hit {
        // The indentation is dropped so the previews line up with each other
        // rather than with the file they came from — and every match offset
        // moves left with it.
        let indent = raw.len() - raw.trim_start().len();
        let body = &raw[indent..];
        let text = clip(body);
        let marks = self
            .re
            .find_iter(body)
            .take(MAX_MARKS)
            // A match past the end of what is shown cannot be pointed at.
            .filter(|m| m.end() <= text.len() && m.start() < m.end())
            .map(|m| (m.start(), m.end()))
            .collect();
        Hit {
            path: path.to_path_buf(),
            line,
            text,
            marks,
        }
    }
}

/// The first [`MAX_PREVIEW`] characters, cut on a character rather than in the
/// middle of one.
fn clip(s: &str) -> String {
    match s.char_indices().nth(MAX_PREVIEW) {
        Some((at, _)) => s[..at].to_string(),
        None => s.to_string(),
    }
}

/// Whether a word is one worth looking a definition up for. Punctuation,
/// numbers and single letters are not.
pub fn is_identifier(word: &str) -> bool {
    let mut chars = word.chars();
    chars
        .next()
        .is_some_and(|c| c.is_alphabetic() || c == '_')
        && word.chars().count() > 1
        && word.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// Split `text` on whole-word occurrences of `name`, as (matched, run) pairs.
///
/// What the viewer paints occurrence highlights with. Same rule as Find
/// References, so what lights up in the file is what the panel would list.
pub fn split_word<'a>(text: &'a str, name: &str) -> Vec<(bool, &'a str)> {
    let mut out = Vec::new();
    if name.is_empty() || text.is_empty() {
        return out;
    }
    let mut at = 0;
    while let Some(found) = text[at..].find(name) {
        let start = at + found;
        let end = start + name.len();
        let bounded = !is_word_byte(text.as_bytes().get(start.wrapping_sub(1)).copied())
            && !is_word_byte(text.as_bytes().get(end).copied());
        if bounded {
            if start > at {
                out.push((false, &text[at..start]));
            }
            out.push((true, &text[start..end]));
            at = end;
        } else {
            // Not a word on its own — step past its first character and keep
            // looking, so `foo` inside `foofoo` cannot hide a later real one.
            let step = text[start..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| start + i)
                .unwrap_or(end);
            out.push((false, &text[at..step]));
            at = step;
        }
    }
    if at < text.len() {
        out.push((false, &text[at..]));
    }
    out
}

fn is_word_byte(b: Option<u8>) -> bool {
    // A multi-byte character's continuation bytes are all >= 0x80, which counts
    // as a word character here — `naïve_thing` is one identifier, not two.
    b.is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hits(pattern: &str, opts: Options, text: &str) -> Vec<Hit> {
        let m = compile(pattern, opts).expect("pattern should compile");
        let mut out = Vec::new();
        m.scan(Path::new("f.rs"), text, &mut out);
        out
    }

    const SAMPLE: &str = "let total = 1;\n    let Total = 2;\nfn subtotal() {}\n";

    #[test]
    fn plain_search_ignores_case_and_finds_substrings() {
        let found = hits("total", Options::default(), SAMPLE);
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].line, 1);
        // The indentation is gone, and the offsets moved with it.
        assert_eq!(found[1].text, "let Total = 2;");
        assert_eq!(found[1].marks, vec![(4, 9)]);
    }

    #[test]
    fn matching_case_narrows_it() {
        let opts = Options {
            case: true,
            ..Default::default()
        };
        let found = hits("total", opts, SAMPLE);
        assert_eq!(found.len(), 2, "`Total` is a different word now");
        assert_eq!(found[1].line, 3, "and `subtotal` still contains it");
    }

    #[test]
    fn whole_word_rules_out_the_substring() {
        let opts = Options {
            word: true,
            ..Default::default()
        };
        let found = hits("total", opts, SAMPLE);
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|h| h.line != 3));
    }

    #[test]
    fn a_plain_search_does_not_read_its_pattern_as_a_regex() {
        let found = hits("a.c", Options::default(), "abc\na.c\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text, "a.c");
    }

    #[test]
    fn a_regex_search_does() {
        let opts = Options {
            regex: true,
            ..Default::default()
        };
        let found = hits(r"fn \w+\(", opts, SAMPLE);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].line, 3);
    }

    #[test]
    fn a_broken_regex_is_reported_in_one_line() {
        let opts = Options {
            regex: true,
            ..Default::default()
        };
        let err = compile("foo(", opts).expect_err("unbalanced paren");
        assert!(!err.contains('\n'), "got: {err:?}");
        assert!(!err.is_empty());
    }

    #[test]
    fn every_match_on_a_line_is_marked_and_a_long_line_is_cut() {
        let found = hits("x", Options::default(), "x y x y x\n");
        assert_eq!(found[0].marks, vec![(0, 1), (4, 5), (8, 9)]);

        let long = format!("{}needle", "a".repeat(MAX_PREVIEW));
        let found = hits("needle", Options::default(), &long);
        assert_eq!(found.len(), 1, "the line still matched");
        assert_eq!(found[0].text.chars().count(), MAX_PREVIEW);
        assert!(found[0].marks.is_empty(), "but it is off the end of what is shown");
    }

    #[test]
    fn an_empty_pattern_is_not_a_search() {
        assert!(compile("", Options::default()).is_err());
    }

    #[test]
    fn whole_word_alternation_stays_whole() {
        let opts = Options {
            word: true,
            regex: true,
            ..Default::default()
        };
        let found = hits("foo|bar", opts, "foo\nbarn\nbar\n");
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|h| h.text != "barn"));
    }

    #[test]
    fn identifiers_are_what_a_definition_can_be_looked_up_for() {
        assert!(is_identifier("foo"));
        assert!(is_identifier("_private"));
        assert!(is_identifier("Thing2"));
        assert!(!is_identifier("x"), "one letter is not worth a lookup");
        assert!(!is_identifier("42"));
        assert!(!is_identifier("foo.bar"));
        assert!(!is_identifier(""));
    }

    #[test]
    fn occurrences_split_on_word_boundaries() {
        let got = split_word("let total = subtotal + total;", "total");
        let marked = got.iter().filter(|(m, _)| *m).count();
        assert_eq!(marked, 2, "`subtotal` is not an occurrence of `total`");
        // And the pieces put the line back together exactly.
        let back: String = got.iter().map(|(_, s)| *s).collect();
        assert_eq!(back, "let total = subtotal + total;");
    }

    #[test]
    fn a_word_touching_the_ends_of_a_line_still_counts() {
        let got = split_word("total", "total");
        assert_eq!(got, vec![(true, "total")]);
    }

    #[test]
    fn splitting_a_line_with_no_occurrence_leaves_it_whole() {
        assert_eq!(split_word("nothing here", "total"), vec![(false, "nothing here")]);
        assert_eq!(split_word("", "total"), Vec::new());
    }
}
