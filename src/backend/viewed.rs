//! Which files of a pull request have been marked read, kept between visits.
//!
//! A mark is a **git blob hash**, not a path — the same forty characters the
//! local copy files that file's contents under. Which is what makes this behave
//! the way a reviewer would want it to across a force-push: a rebase that
//! leaves a file byte-identical leaves its blob alone, so it stays ticked;
//! anything actually rewritten hashes to something else, so its tick goes away
//! and the file comes back for a second look. Keyed by path, both cases would
//! be wrong.
//!
//! It lives in `localStorage` rather than the filesystem the blobs themselves
//! are in: it is a few kilobytes of forty-character strings, and it has to be
//! readable synchronously while the explorer is drawing rows.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::github::RepoRef;
use super::store;

/// How many pull requests to remember marks for. A reviewer's afternoon, not
/// their year — and a bound on a value that would otherwise only ever grow.
const MAX_PRS: usize = 24;

/// And how many marks to keep for one of them. Past a thousand ticked files
/// nobody is coming back to finish the review, and `localStorage` has 5 MB for
/// everything on this origin.
const MAX_MARKS: usize = 1000;

/// Everything remembered, oldest pull request first.
#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Book {
    prs: Vec<Entry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Entry {
    pr: String,
    marks: Vec<String>,
}

impl Book {
    fn marks(&self, pr: &str) -> HashSet<String> {
        self.prs
            .iter()
            .find(|e| e.pr == pr)
            .map(|e| e.marks.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Write one pull request's marks, as the most recently touched.
    ///
    /// Sorted rather than in the order they were ticked: it makes the stored
    /// text stable, so a review that ticks the same files in a different order
    /// does not rewrite the whole entry — and it makes the cap drop the same
    /// marks twice rather than whichever ones a hash set happened to yield.
    fn remember(&mut self, pr: &str, marks: &HashSet<String>) {
        self.prs.retain(|e| e.pr != pr);
        if !marks.is_empty() {
            let mut marks: Vec<String> = marks.iter().cloned().collect();
            marks.sort();
            marks.truncate(MAX_MARKS);
            self.prs.push(Entry {
                pr: pr.to_string(),
                marks,
            });
        }
        // The oldest go, which are the ones at the front.
        let over = self.prs.len().saturating_sub(MAX_PRS);
        self.prs.drain(..over);
    }
}

/// One pull request, as this remembers it.
pub fn pr_key(repo: &RepoRef, number: u64) -> String {
    format!("{repo}#{number}")
}

/// And one commit of it, read on its own. Apart from the pull request's own
/// marks on purpose — the same file read in one commit and read across the
/// whole branch are two different readings.
pub fn commit_key(repo: &RepoRef, sha: &str) -> String {
    format!("{repo}@{sha}")
}

/// And two refs held up against each other. Apart from either branch's own
/// marks, and from any pull request's: what has been read here is one
/// comparison, and it stops being that the moment either end moves.
pub fn compare_key(repo: &RepoRef, base: &str, head: &str) -> String {
    format!("{repo}@{base}...{head}")
}

fn book() -> Book {
    store::get(store::VIEWED)
        .and_then(|raw| serde_json::from_str::<Book>(&raw).ok())
        .unwrap_or_default()
}

/// What was ticked on this pull request last time it was open.
pub fn load(pr: &str) -> HashSet<String> {
    book().marks(pr)
}

/// Keep what is ticked now. Best effort, like everything else in storage — the
/// cost of losing it is ticking a few boxes again.
pub fn save(pr: &str, marks: &HashSet<String>) {
    let mut book = book();
    book.remember(pr, marks);
    if let Ok(body) = serde_json::to_string(&book) {
        store::set(store::VIEWED, &body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marks(of: &[&str]) -> HashSet<String> {
        of.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn what_was_ticked_comes_back() {
        let mut book = Book::default();
        book.remember("o/r#1", &marks(&["aaa", "bbb"]));
        assert_eq!(book.marks("o/r#1"), marks(&["aaa", "bbb"]));
        // And says nothing about a pull request it has never seen.
        assert!(book.marks("o/r#2").is_empty());
    }

    #[test]
    fn a_round_trip_through_json_keeps_it() {
        let mut book = Book::default();
        book.remember("o/r#1", &marks(&["aaa"]));
        let raw = serde_json::to_string(&book).unwrap();
        let back: Book = serde_json::from_str(&raw).unwrap();
        assert_eq!(back, book);
    }

    #[test]
    fn unticking_the_last_box_forgets_the_pull_request() {
        let mut book = Book::default();
        book.remember("o/r#1", &marks(&["aaa"]));
        book.remember("o/r#1", &HashSet::new());
        assert!(book.prs.is_empty(), "no empty entries left behind");
    }

    #[test]
    fn writing_one_again_does_not_leave_two() {
        let mut book = Book::default();
        book.remember("o/r#1", &marks(&["aaa"]));
        book.remember("o/r#1", &marks(&["aaa", "bbb"]));
        assert_eq!(book.prs.len(), 1);
        assert_eq!(book.marks("o/r#1"), marks(&["aaa", "bbb"]));
    }

    #[test]
    fn the_oldest_pull_requests_are_the_ones_dropped() {
        let mut book = Book::default();
        for i in 0..MAX_PRS + 5 {
            book.remember(&format!("o/r#{i}"), &marks(&["aaa"]));
        }
        assert_eq!(book.prs.len(), MAX_PRS);
        assert!(book.marks("o/r#0").is_empty(), "the first one went");
        assert!(!book.marks(&format!("o/r#{}", MAX_PRS + 4)).is_empty());

        // Touching one again makes it the most recent, so the next eviction
        // takes something else.
        book.remember("o/r#5", &marks(&["bbb"]));
        book.remember("o/r#999", &marks(&["ccc"]));
        assert_eq!(book.marks("o/r#5"), marks(&["bbb"]));
    }

    #[test]
    fn one_pull_request_cannot_grow_without_end() {
        let all: HashSet<String> = (0..MAX_MARKS + 50).map(|i| format!("sha{i:05}")).collect();
        let mut book = Book::default();
        book.remember("o/r#1", &all);
        assert_eq!(book.marks("o/r#1").len(), MAX_MARKS);
    }

    #[test]
    fn a_broken_entry_reads_as_nothing_ticked() {
        assert!(serde_json::from_str::<Book>("not json").is_err());
        let empty: Book = serde_json::from_str("{}").unwrap();
        assert!(empty.marks("o/r#1").is_empty());
    }
}
