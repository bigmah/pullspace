//! Reading a whole commit back out of the local store, file by file.
//!
//! Search and the symbol index both need every text file of what is open. On a
//! machine that is a directory walk; here there is no directory and no walk.
//! What there is instead is the [`Snapshot`] — the commit's file list, straight
//! from GitHub's tree API — and [`blobs`], which holds the bytes of everything
//! the clone managed to fetch, keyed by content. So a "walk" is: take the file
//! list, ask the store for each blob, and skip what is not there.
//!
//! Skipping is the part worth being honest about. A clone leaves out anything
//! large or obviously not text, and can be interrupted or refused outright, so
//! a scan covers most of a repository rather than all of it. Every walk reports
//! what it could not read, and every caller says so — a search that quietly
//! misses a file is worse than one that admits to it.
//!
//! # The cache
//!
//! Reading four thousand files out of OPFS and decoding them takes a second or
//! two, which is fine once and irritating on every search. So decoded text is
//! kept, up to [`TEXT_CACHE_BYTES`], and — because the store is keyed by
//! content and so is this — it stays valid across reloads, across pull
//! requests, and across two branches of the same repository. The first scan
//! pays for the disk; the ones after it are a string search over memory.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::rc::Rc;

use futures_util::stream::{self, StreamExt};

use super::blobs;
use super::clone::is_opaque;
use super::github::{Snapshot, TreeEntry};
use super::looks_binary;

/// How many blobs to have in the air at once. OPFS serves these one at a time
/// underneath, but the handle lookups and the decoding overlap.
const READ_CONCURRENCY: usize = 8;

/// Files above this are not searched. A single minified bundle can be most of a
/// repository's bytes and none of its code, and the line it matches on is four
/// hundred kilobytes wide.
pub const MAX_SCAN_BYTES: u64 = 2 << 20;

/// How much decoded text to keep between scans. Most repositories worth reading
/// fit inside this whole, which is what makes a second search instant; the ones
/// that do not lose their oldest files and re-read them off disk.
const TEXT_CACHE_BYTES: usize = 48 << 20;

/// How often a walk stands back and lets the browser paint, in files.
const YIELD_EVERY: usize = 96;

/// How often the caller hears how far along it is, in files.
const REPORT_EVERY: usize = 64;

thread_local! {
    static TEXTS: RefCell<Cache> = RefCell::new(Cache::default());
}

/// Decoded file text, keyed by the blob's SHA.
///
/// `None` against a key means "read, and it turned out to be binary" — worth
/// remembering, since it saves reading the same bytes again to reach the same
/// conclusion, and a hash cannot change its mind about being binary.
#[derive(Default)]
struct Cache {
    map: HashMap<String, Option<Rc<str>>>,
    /// Insertion order, for dropping the oldest.
    order: VecDeque<String>,
    bytes: usize,
}

fn cached(sha: &str) -> Option<Option<Rc<str>>> {
    TEXTS.with(|c| c.borrow().map.get(sha).cloned())
}

fn remember(sha: &str, text: Option<Rc<str>>) {
    TEXTS.with(|c| {
        let mut c = c.borrow_mut();
        let size = text.as_ref().map(|t| t.len()).unwrap_or(0);
        match c.map.insert(sha.to_string(), text) {
            // Replaced: two concurrent reads of the same blob, the second
            // landing on the first. The queue entry is already there, and the
            // bytes the first put in have to come back out or the cache
            // believes itself fuller than it is.
            Some(old) => c.bytes = c.bytes.saturating_sub(old.map_or(0, |t| t.len())),
            None => c.order.push_back(sha.to_string()),
        }
        c.bytes += size;
        while c.bytes > TEXT_CACHE_BYTES {
            let Some(oldest) = c.order.pop_front() else {
                break;
            };
            if let Some(Some(gone)) = c.map.remove(&oldest) {
                c.bytes = c.bytes.saturating_sub(gone.len());
            }
        }
    });
}

/// Drop everything held in memory. What clearing the store calls, since the
/// files it is a copy of have just been deleted.
pub fn forget() {
    TEXTS.with(|c| *c.borrow_mut() = Cache::default());
}

/// What came of asking for one file.
enum Read {
    Text(Rc<str>),
    /// Read, and not text. Nothing to search, and nothing wrong either.
    Binary,
    /// Not in the local store. The clone left it out, or never got to it.
    Absent,
}

/// One file's text: out of memory if it is there, off the disk if it is not,
/// and absent if it is neither — never off the network. Everything here is a
/// bulk read of what has already been fetched.
async fn read_text(entry: &TreeEntry) -> Read {
    if let Some(hit) = cached(&entry.sha) {
        return match hit {
            Some(text) => Read::Text(text),
            None => Read::Binary,
        };
    }
    let Some(bytes) = blobs::read(&entry.sha, entry.size).await else {
        // Deliberately not cached: a miss is about this store at this moment,
        // and the next clone may well fill it in.
        return Read::Absent;
    };
    let text: Option<Rc<str>> =
        (!looks_binary(&bytes)).then(|| Rc::from(String::from_utf8_lossy(&bytes).as_ref()));
    remember(&entry.sha, text.clone());
    match text {
        Some(text) => Read::Text(text),
        None => Read::Binary,
    }
}

/// One named file's text, for whoever wants to look at just the one — the
/// definition a peek is showing, say.
pub async fn text_at(snapshot: &Snapshot, path: &Path) -> Option<Rc<str>> {
    match read_text(snapshot.entry(path)?).await {
        Read::Text(text) => Some(text),
        _ => None,
    }
}

/// Whether a walk should keep going.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Go,
    Stop,
}

/// What a walk covered, and what it did not.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Walked {
    /// Text files actually read and handed to the visitor.
    pub read: usize,
    /// Named by the tree but not in the local store: too large for the clone,
    /// nothing it would fetch, or a fetch that failed.
    pub missing: usize,
    /// Read, and not text.
    pub binary: usize,
    /// The visitor asked to stop — so `read` is not the whole repository even
    /// counting the misses.
    pub stopped: bool,
}

impl Walked {
    /// What to say about the files this did not look inside. `None` when it
    /// looked at all of them, which is the common case and needs no note.
    pub fn shortfall(&self) -> Option<String> {
        (self.missing > 0).then(|| {
            format!(
                "{} file{} not searched — not downloaded",
                self.missing,
                if self.missing == 1 { "" } else { "s" },
            )
        })
    }
}

/// Which of a commit's files are worth trying to read at all.
///
/// Answered without touching the disk: the size comes from the tree and
/// [`blobs::have`] is a set lookup. Doing it up front is what lets a walk
/// report a total before it starts, so the progress note counts towards
/// something real.
fn candidates(files: &[TreeEntry]) -> (Vec<&TreeEntry>, usize) {
    let mut wanted = Vec::new();
    let mut missing = 0;
    for entry in files {
        if entry.size > MAX_SCAN_BYTES || is_opaque(&entry.path) {
            missing += 1;
        } else if blobs::have(&entry.sha) {
            wanted.push(entry);
        } else {
            missing += 1;
        }
    }
    (wanted, missing)
}

/// Hand every readable text file of a commit to `visit`, in the order the tree
/// lists them.
///
/// `progress` is called with (files read, files to read) as it goes, often
/// enough to look like it is moving. `visit` returning [`Flow::Stop`] ends the
/// walk — which is how a search that has filled its result cap stops paying for
/// the rest of the repository.
pub async fn walk(
    files: &[TreeEntry],
    mut progress: impl FnMut(usize, usize),
    mut visit: impl FnMut(&Path, &str) -> Flow,
) -> Walked {
    let mut at = Walked::default();
    if !blobs::open().await {
        at.missing = files.len();
        return at;
    }
    let (wanted, missing) = candidates(files);
    at.missing = missing;
    let total = wanted.len();
    progress(0, total);

    // Read ahead of the visitor, several at a time, but hand the results over
    // in tree order: search results that come back in whatever order the
    // filesystem felt like are results nobody can scan down.
    let mut reads = stream::iter(wanted.into_iter().map(|entry| async move {
        let text = read_text(entry).await;
        (&entry.path, text)
    }))
    .buffered(READ_CONCURRENCY);

    let mut seen = 0usize;
    while let Some((path, text)) = reads.next().await {
        seen += 1;
        match text {
            Read::Text(text) => {
                at.read += 1;
                if visit(path, &text) == Flow::Stop {
                    at.stopped = true;
                    break;
                }
            }
            Read::Binary => at.binary += 1,
            // On disk a moment ago and not now — swept by another tab, or a
            // short write thrown out on the way past. Either way it is a file
            // this scan did not cover.
            Read::Absent => at.missing += 1,
        }
        if seen.is_multiple_of(REPORT_EVERY) {
            progress(seen, total);
        }
        // A repository is thousands of files and the tab has a cursor blinking
        // in it. Nothing here is in a hurry.
        if seen.is_multiple_of(YIELD_EVERY) {
            super::breathe(0).await;
        }
    }
    progress(at.read, total);
    at
}

/// Repo-relative path of a file, as the panels print it. Windows separators
/// never appear — every path here came from GitHub — so this is just the
/// display form, in one place.
pub fn show(path: &Path) -> String {
    path.display().to_string()
}

/// A path and a line, as a location string.
pub fn at_line(path: &Path, line: usize) -> String {
    format!("{}:{}", show(path), line)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn a_walk_without_a_store_reads_nothing_and_says_so() {
        // No browser, so no filesystem — and every file of the commit counts as
        // one this scan did not cover rather than one it silently dropped.
        let files = vec![
            TreeEntry {
                path: PathBuf::from("a.rs"),
                sha: "a".repeat(40),
                size: 10,
            },
            TreeEntry {
                path: PathBuf::from("b.rs"),
                sha: "b".repeat(40),
                size: 10,
            },
        ];
        let (wanted, missing) = candidates(&files);
        assert!(wanted.is_empty());
        assert_eq!(missing, 2);
    }

    #[test]
    fn what_is_too_big_or_not_text_is_not_a_candidate() {
        let files = vec![
            TreeEntry {
                path: PathBuf::from("logo.png"),
                sha: "c".repeat(40),
                size: 10,
            },
            TreeEntry {
                path: PathBuf::from("huge.json"),
                sha: "d".repeat(40),
                size: MAX_SCAN_BYTES + 1,
            },
        ];
        let (wanted, missing) = candidates(&files);
        assert!(wanted.is_empty());
        assert_eq!(missing, 2);
    }

    #[test]
    fn a_shortfall_is_only_reported_when_there_is_one() {
        assert_eq!(Walked::default().shortfall(), None);
        let short = Walked {
            missing: 1,
            ..Default::default()
        };
        assert_eq!(
            short.shortfall().as_deref(),
            Some("1 file not searched — not downloaded"),
        );
    }

    #[test]
    fn locations_read_the_way_an_editor_prints_them() {
        assert_eq!(at_line(Path::new("src/main.rs"), 42), "src/main.rs:42");
    }
}
