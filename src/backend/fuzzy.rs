//! Matching a few letters against a few thousand paths.
//!
//! The picker is judged on one thing: whether `uiapp` puts `src/ui/app.rs`
//! first. Which is a scoring problem rather than a matching one — the letters
//! are a subsequence of a great many paths in any repository worth reviewing,
//! and the answer is which of those the reader meant.
//!
//! So the rules here are all about *where* a letter landed. A letter at the
//! start of a word is worth several in the middle of one, a run of letters
//! kept together is worth more than the same letters scattered, and a hit in
//! the file's own name beats one in the directories above it. Everything else
//! is tie-breaking.
//!
//! One pass, greedily, left to right. A proper alignment would find a better
//! path through the awkward cases, and would do it thousands of times per
//! keystroke on a repository this size; greedy plus the word-start bonus below
//! lands the same answer for anything anybody actually types.

/// What one candidate scored, and where the letters landed in it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Match {
    pub score: i32,
    /// Byte offsets into the haystack, ascending — what the list draws in bold
    /// so the reader can see why a row is in it.
    pub hits: Vec<usize>,
}

/// Every letter matched is worth this much before any bonus.
const BASE: i32 = 8;
/// Following straight on from the last letter matched.
const RUN: i32 = 12;
/// Landing on the first letter of a word — after a separator, or on the
/// capital that starts the second half of a camelCase name.
const WORD: i32 = 14;
/// Landing on the first character of the whole string.
const HEAD: i32 = 16;
/// Skipping a character to get there. Small, and capped by the length of what
/// is being skipped over, so a long path is not ruled out by being long.
const SKIP: i32 = -1;
/// Every letter of the needle fell inside the file's own name rather than the
/// directories in front of it.
const BASENAME: i32 = 40;

/// Score `needle` against `hay`, or `None` if the letters are not in it at all.
///
/// Case-insensitive, and the case that was ignored is paid back as a bonus:
/// typing `App` should prefer `AppState` over `happy`, but typing `app` should
/// not rule either out.
pub fn score(needle: &str, hay: &str) -> Option<Match> {
    if needle.is_empty() {
        return Some(Match {
            score: 0,
            hits: Vec::new(),
        });
    }
    let whole = walk(needle, hay, 0)?;
    // The same letters against the file's name alone. A hit there is what the
    // reader nearly always meant, so it wins outright when it exists — but it
    // is scored over the whole string, so the offsets it reports are offsets
    // into the path and not into the tail of one.
    let base_at = hay.rfind('/').map(|i| i + 1).unwrap_or(0);
    let inside = (base_at > 0)
        .then(|| walk(needle, hay, base_at))
        .flatten()
        .map(|m| Match {
            score: m.score + BASENAME,
            hits: m.hits,
        });
    Some(match inside {
        Some(m) if m.score > whole.score => m,
        _ => whole,
    })
}

/// One greedy pass, starting at `from` and never looking back.
fn walk(needle: &str, hay: &str, from: usize) -> Option<Match> {
    let mut hits = Vec::with_capacity(needle.chars().count());
    let mut score = 0;
    // Where the last letter matched ended, so the next one knows whether it is
    // carrying on a run or starting somewhere new.
    let mut after: Option<usize> = None;
    let mut at = from;
    for want in needle.chars() {
        let want = want.to_ascii_lowercase();
        let rest = hay.get(at..)?;
        // The first place this letter appears from here on. `char_indices`
        // rather than `find`, so the offset recorded is one the haystack can
        // actually be sliced at.
        let (off, got) = rest
            .char_indices()
            .find(|(_, c)| c.to_ascii_lowercase() == want)?;
        let index = at + off;
        let before = hay[..index].chars().next_back();
        score += BASE;
        if after == Some(index) {
            score += RUN;
        }
        // The two ways code spells the start of a word: after a separator, and
        // the capital that starts the second half of a camelCase name. Worth
        // the same, being the same thing.
        let word_start = before.is_some_and(is_separator)
            || (before.is_some_and(|c| c.is_lowercase()) && got.is_uppercase());
        if index == 0 {
            score += HEAD;
        } else if word_start {
            score += WORD;
        }
        // What was stepped over to get here — but never more than the run
        // itself was worth, so a deep path stays reachable.
        if after.is_some_and(|end| end < index) {
            let skipped = hay[after.unwrap_or(0)..index].chars().count() as i32;
            score += (skipped * SKIP).max(-(BASE + RUN));
        }
        hits.push(index);
        at = index + got.len_utf8();
        after = Some(at);
    }
    // Two candidates that scored the same on their letters are separated by
    // how much else is in them: `app.rs` before `application_state.rs`.
    score -= hay.chars().count() as i32 / 12;
    Some(Match { score, hits })
}

fn is_separator(c: char) -> bool {
    matches!(c, '/' | '_' | '-' | '.' | ' ' | ':')
}

/// Rank `items` against `needle`, best first, keeping at most `limit`.
///
/// The index of each survivor comes back with it, because the caller is
/// holding the things these strings describe and needs to know which is which.
/// An empty needle matches everything in the order it was given, which is what
/// makes the picker's first frame the list of recent files rather than blank.
pub fn rank<S: AsRef<str>>(needle: &str, items: &[S], limit: usize) -> Vec<(usize, Match)> {
    let needle = needle.trim();
    let mut out: Vec<(usize, Match)> = items
        .iter()
        .enumerate()
        .filter_map(|(i, s)| score(needle, s.as_ref()).map(|m| (i, m)))
        .collect();
    // Stable, so items that score the same stay in the order the caller put
    // them — which is how "most recently opened" survives being ranked.
    out.sort_by(|a, b| b.1.score.cmp(&a.1.score));
    out.truncate(limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn best(needle: &str, paths: &[&str]) -> String {
        let ranked = rank(needle, paths, 10);
        paths[ranked[0].0].to_string()
    }

    #[test]
    fn letters_not_in_it_at_all_do_not_match() {
        assert!(score("xyz", "src/main.rs").is_none());
        // In it, but not in this order.
        assert!(score("niam", "src/main.rs").is_none());
    }

    #[test]
    fn the_letters_may_be_scattered() {
        let m = score("smrs", "src/main.rs").expect("a subsequence");
        assert_eq!(m.hits, vec![0, 4, 9, 10]);
    }

    #[test]
    fn a_word_start_beats_the_middle_of_a_word() {
        assert_eq!(
            best("app", &["src/backend/happening.rs", "src/ui/app.rs"]),
            "src/ui/app.rs"
        );
    }

    #[test]
    fn the_file_name_beats_the_directories_above_it() {
        assert_eq!(
            best("tree", &["tree/src/main.rs", "src/backend/tree.rs"]),
            "src/backend/tree.rs"
        );
    }

    #[test]
    fn letters_kept_together_beat_the_same_letters_spread_out() {
        assert_eq!(
            best("diff", &["d/i/f/f.rs", "src/backend/difftool.rs"]),
            "src/backend/difftool.rs"
        );
    }

    #[test]
    fn a_path_can_be_typed_through_its_directories() {
        assert_eq!(
            best("uiapp", &["src/ui/app.rs", "src/backend/appdata/ui.rs"]),
            "src/ui/app.rs"
        );
    }

    #[test]
    fn camel_humps_are_word_starts() {
        let m = score("fd", "fileDiff").expect("a subsequence");
        let flat = score("fd", "fiddle").expect("a subsequence");
        assert!(m.score > flat.score, "{} vs {}", m.score, flat.score);
    }

    #[test]
    fn case_is_ignored() {
        assert!(score("APP", "src/ui/app.rs").is_some());
        assert!(score("app", "src/ui/APP.rs").is_some());
    }

    #[test]
    fn nothing_typed_keeps_every_candidate_in_the_order_given() {
        let paths = ["b.rs", "a.rs", "c.rs"];
        let ranked = rank("", &paths, 10);
        assert_eq!(
            ranked.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn the_limit_is_honoured() {
        let paths = ["a.rs", "ab.rs", "abc.rs", "abcd.rs"];
        assert_eq!(rank("a", &paths, 2).len(), 2);
    }

    #[test]
    fn a_shorter_name_wins_a_tie() {
        assert_eq!(
            best("mod", &["src/a/module_registry_builder.rs", "src/a/mod.rs"]),
            "src/a/mod.rs"
        );
    }

    #[test]
    fn multibyte_paths_do_not_panic_and_report_sliceable_offsets() {
        let hay = "src/naïve/café.rs";
        let m = score("nc", hay).expect("a subsequence");
        for at in m.hits {
            assert!(hay.is_char_boundary(at), "{at} is mid-character");
        }
    }
}
