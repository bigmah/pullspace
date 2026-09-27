//! A review being written: the line comments held back to go out together, and
//! the summary that will go with them. Kept between visits.
//!
//! GitHub has a pending review of its own, but the REST API can only create
//! one whole — adding a comment to it afterwards is a GraphQL mutation per
//! comment, and a draft kept on GitHub is a draft that a closed tab leaves
//! behind on somebody else's pull request. So a review is written here, and is
//! sent as one request when it is submitted.
//!
//! In `localStorage` beside the viewed marks, and for the same reasons: it is a
//! few kilobytes, and losing a half-written review to a reload is exactly the
//! kind of thing that makes somebody stop writing them anywhere but github.com.

use serde::{Deserialize, Serialize};

use super::github::{LineNote, Side};
use super::store;

/// How many pull requests to hold drafts for. Reviews get abandoned; this is
/// what stops the abandoned ones accumulating for ever.
const MAX_PRS: usize = 24;

/// One line comment, written and not yet sent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    /// Unique within its review — what Edit and Delete point at.
    pub id: u64,
    /// The head commit the line was counted in. A line number is only a place
    /// in the diff it was read from.
    pub commit: String,
    pub path: String,
    pub line: usize,
    pub side: Side,
    pub body: String,
}

impl Note {
    pub fn to_send(&self) -> LineNote {
        LineNote {
            path: self.path.clone(),
            line: self.line,
            side: self.side,
            body: self.body.clone(),
        }
    }
}

/// Everything written towards one review of one pull request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Pending {
    /// The summary: the box at the foot of the conversation.
    pub body: String,
    pub notes: Vec<Note>,
}

impl Pending {
    pub fn is_empty(&self) -> bool {
        self.body.trim().is_empty() && self.notes.is_empty()
    }

    /// An id no note in here has yet.
    pub fn next_id(&self) -> u64 {
        self.notes.iter().map(|n| n.id).max().unwrap_or(0) + 1
    }

    /// The commit to submit the notes against: theirs, when they agree.
    ///
    /// They disagree only when the pull request was pushed to part-way through
    /// the review. There is no right answer then — a review has one commit —
    /// and leaving it out lets GitHub use the head, where most of them still
    /// land.
    pub fn commit(&self) -> Option<&str> {
        let first = self.notes.first()?.commit.as_str();
        self.notes
            .iter()
            .all(|n| n.commit == first)
            .then_some(first)
    }
}

#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Book {
    /// Oldest pull request first.
    prs: Vec<(String, Pending)>,
}

impl Book {
    fn get(&self, pr: &str) -> Pending {
        self.prs
            .iter()
            .find(|(k, _)| k == pr)
            .map(|(_, p)| p.clone())
            .unwrap_or_default()
    }

    /// Write one pull request's draft, as the most recently touched — or drop
    /// it, once there is nothing in it.
    fn put(&mut self, pr: &str, pending: &Pending) {
        self.prs.retain(|(k, _)| k != pr);
        if !pending.is_empty() {
            self.prs.push((pr.to_string(), pending.clone()));
        }
        let over = self.prs.len().saturating_sub(MAX_PRS);
        self.prs.drain(..over);
    }
}

fn book() -> Book {
    store::get(store::DRAFTS)
        .and_then(|raw| serde_json::from_str::<Book>(&raw).ok())
        .unwrap_or_default()
}

/// What was being written on this pull request (a
/// [`viewed::pr_key`](super::viewed::pr_key)).
pub fn load(pr: &str) -> Pending {
    book().get(pr)
}

/// Keep it. Read, changed and written back whole each time rather than held,
/// so two windows writing reviews of two pull requests do not each put back a
/// book without the other's.
pub fn save(pr: &str, pending: &Pending) {
    let mut book = book();
    book.put(pr, pending);
    if let Ok(body) = serde_json::to_string(&book) {
        store::set(store::DRAFTS, &body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(id: u64, commit: &str) -> Note {
        Note {
            id,
            commit: commit.to_string(),
            path: "a.rs".to_string(),
            line: 3,
            side: Side::Right,
            body: "hm".to_string(),
        }
    }

    #[test]
    fn a_draft_round_trips_and_an_empty_one_is_forgotten() {
        let mut book = Book::default();
        let p = Pending {
            body: "looks fine".to_string(),
            notes: vec![note(1, "abc")],
        };
        book.put("o/r#1", &p);
        let back: Book = serde_json::from_str(&serde_json::to_string(&book).unwrap()).unwrap();
        assert_eq!(back.get("o/r#1"), p);

        book.put("o/r#1", &Pending::default());
        assert!(book.prs.is_empty());
    }

    #[test]
    fn the_oldest_drafts_are_the_ones_dropped() {
        let mut book = Book::default();
        let p = Pending {
            body: "x".to_string(),
            notes: vec![],
        };
        for i in 0..MAX_PRS + 3 {
            book.put(&format!("o/r#{i}"), &p);
        }
        assert_eq!(book.prs.len(), MAX_PRS);
        assert!(book.get("o/r#0").is_empty());
        assert!(!book.get(&format!("o/r#{}", MAX_PRS + 2)).is_empty());
    }

    #[test]
    fn notes_written_across_a_push_leave_the_commit_to_github() {
        let mut p = Pending::default();
        assert_eq!(p.commit(), None);
        p.notes.push(note(1, "abc"));
        p.notes.push(note(2, "abc"));
        assert_eq!(p.commit(), Some("abc"));
        assert_eq!(p.next_id(), 3);
        p.notes.push(note(3, "def"));
        assert_eq!(p.commit(), None);
    }
}
