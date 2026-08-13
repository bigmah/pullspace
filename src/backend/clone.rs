//! Pulling a whole commit down at once, and reading files back out of it.
//!
//! A page cannot run `git clone` — github.com's smart-HTTP endpoints send no
//! CORS headers, and codeload answers archive requests only to GitHub's own
//! origin, so neither the protocol nor the tarball is available from a browser
//! tab. What is available is one request per file from the CDN, unmetered and
//! answered to anybody. So that is what this does: the tree says which files a
//! commit is made of, [`blobs`] says which of those are already on disk, and
//! everything left over is fetched a dozen at a time.
//!
//! It costs a few seconds on the way in and it buys two things. Clicking
//! through the tree stops touching the network at all, and — because the store
//! is keyed by content — the next pull request on the same repository arrives
//! to find nearly all of itself already here.
//!
//! What it does not do is fetch everything unconditionally. Large files and the
//! obviously non-textual ones are left for the reader to ask for, since the
//! viewer would not draw them anyway; the budgets below are what keeps "clone
//! the repository" from meaning "download half a gigabyte of vendored
//! dependencies".

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use futures_util::stream::{self, StreamExt};

use super::FileContent;
use super::blobs;
use super::github::{self, FetchJob, RepoRef, Snapshot, TreeEntry};

/// How many files to fetch at once. The CDN is HTTP/2 and does not mind, but a
/// browser tab has other things to do with its connection and its main thread.
const CONCURRENCY: usize = 12;

/// Files bigger than this are left for whoever opens them. The viewer already
/// gives up highlighting at 400 KB, and a repository's weight is nearly always
/// a handful of outliers — lockfiles, vendored bundles, checked-in data.
const MAX_BLOB_BYTES: u64 = 1 << 20;

/// What one commit may add to the store up front.
const MAX_CLONE_BYTES: u64 = 96 << 20;

/// And how many files, for a repository that is mostly tiny ones.
const MAX_CLONE_FILES: usize = 20_000;

/// How often the caller hears about it, in files.
const REPORT_EVERY: usize = 8;

/// How often the clone stands back and lets the browser paint, in files. Often
/// enough that nothing feels stuck, rarely enough that the timer's own cost —
/// browsers clamp a chain of them to about 4ms apiece — stays out of the way.
const YIELD_EVERY: usize = 24;

/// How long to stand back for while a reader is waiting on the filesystem.
const YIELD_MS: u32 = 20;

/// How much of the hour's API allowance one clone may spend.
///
/// Only private repositories ever touch it — everything public comes off the
/// CDN for free — and a private repository bigger than this is one pullspace
/// covers in part rather than one it locks its reader out of GitHub for. The
/// files it does spend it on are the ones the pull request changes, which is
/// what [`plan`] puts first.
const MAX_API_BLOBS: usize = 1_500;

/// Give up on a clone once this many files have failed and failures are the
/// majority of what has come back. GitHub refusing, or an allowance spent, is
/// not a thing to keep asking about several thousand more times.
const GIVE_UP_AFTER: usize = 24;

/// Extensions the viewer would only ever call binary. Fetching them costs the
/// most bandwidth per unit of nothing to read.
const OPAQUE: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "ico", "webp", "avif", "tiff", "psd", "pdf", "zip", "gz",
    "tgz", "bz2", "xz", "zst", "7z", "rar", "jar", "war", "class", "so", "dylib", "dll", "exe",
    "bin", "dat", "wasm", "o", "a", "pyc", "pdb", "mp3", "wav", "ogg", "flac", "mp4", "mov", "avi",
    "mkv", "webm", "woff", "woff2", "ttf", "otf", "eot", "sqlite", "db", "pack", "idx",
];

/// Also what [`scan`](super::scan) skips: a file the clone would not fetch is
/// one the search has nothing to read.
pub fn is_opaque(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|e| OPAQUE.contains(&e.as_str()))
}

thread_local! {
    /// Repositories the CDN would not serve — private ones. Remembered so that
    /// only the first file of a private repository pays for finding out.
    static CDN_REFUSED: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
    /// Metered requests this clone has spent, against [`MAX_API_BLOBS`].
    static API_SPENT: Cell<usize> = const { Cell::new(0) };
    /// Reads somebody is waiting on, right now, with a cursor.
    static WAITING: Cell<usize> = const { Cell::new(0) };
}

/// Held for as long as a reader is waiting for a file.
///
/// The clone stands aside while one of these exists. Both go through the same
/// filesystem, and the filesystem serves them in the order they arrive: a clone
/// with twelve writes in the air and four thousand behind them will bury a
/// single read for as long as it takes to finish, which is a browser that has
/// stopped responding as far as anyone using it is concerned.
struct Waiting;

impl Waiting {
    fn new() -> Self {
        WAITING.with(|w| w.set(w.get() + 1));
        Waiting
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        WAITING.with(|w| w.set(w.get().saturating_sub(1)));
    }
}

/// Let the browser get on with whatever it was doing.
///
/// A timer rather than an `await` on more work of our own: yielding to the
/// microtask queue only lets the next thing we queued run, and it is the
/// event loop underneath — the one that paints, and delivers clicks — that has
/// to be given a turn.
async fn breathe(ms: u32) {
    gloo_timers::future::TimeoutFuture::new(ms).await;
}

fn cdn_serves(repo: &RepoRef) -> bool {
    CDN_REFUSED.with(|r| !r.borrow().contains(&repo.to_string()))
}

fn cdn_refused(repo: &RepoRef) {
    CDN_REFUSED.with(|r| {
        r.borrow_mut().insert(repo.to_string());
    });
}

// -------------------------------------------------------------------- plan

/// One file to fetch: what it hashes to, and the two names the two transports
/// know it by.
#[derive(Clone, PartialEq, Debug)]
pub struct Want {
    pub path: PathBuf,
    pub sha: String,
    /// The commit the CDN will serve `path` at.
    pub commit: String,
    pub size: u64,
}

/// How much of a commit is missing, before any of it is fetched.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct Plan {
    pub wanted: Vec<Want>,
    /// Files already on disk, from an earlier visit — the number worth having
    /// done all this for.
    pub cached: usize,
    /// Files deliberately left behind: too large, or nothing the viewer draws.
    pub skipped: usize,
    /// What `wanted` adds up to.
    pub bytes: u64,
}

#[derive(Default)]
struct Planner {
    plan: Plan,
    seen: HashSet<String>,
}

impl Planner {
    /// Consider one file. Returns false once the budget is gone, which is the
    /// signal to stop walking the tree.
    fn add(&mut self, path: &Path, sha: &str, commit: &str, size: u64) -> bool {
        // The same content under two names, or both sides of a file the pull
        // request does not change: one copy, one fetch.
        if !self.seen.insert(sha.to_string()) {
            return true;
        }
        if blobs::have(sha) {
            self.plan.cached += 1;
            return true;
        }
        if size > MAX_BLOB_BYTES || is_opaque(path) {
            self.plan.skipped += 1;
            return true;
        }
        if self.plan.wanted.len() >= MAX_CLONE_FILES || self.plan.bytes + size > MAX_CLONE_BYTES {
            self.plan.skipped += 1;
            return false;
        }
        self.plan.bytes += size;
        self.plan.wanted.push(Want {
            path: path.to_path_buf(),
            sha: sha.to_string(),
            commit: commit.to_string(),
            size,
        });
        true
    }
}

/// Work out what is missing.
///
/// Order matters: what the pull request changes comes first, both sides of it,
/// because that is what the reader opens while the rest is still arriving.
pub fn plan(head: &Snapshot, base: &Snapshot, changed: &[FetchJob]) -> Plan {
    let mut planner = Planner::default();

    for job in changed {
        if job.wants_head()
            && let Some(entry) = head.entry(&job.path)
            && !planner.add(&entry.path, &entry.sha, &head.commit, entry.size)
        {
            break;
        }
        if job.wants_base()
            && let Some(entry) = base.entry(&job.base_path)
            && !planner.add(&entry.path, &entry.sha, &base.commit, entry.size)
        {
            break;
        }
    }

    for entry in &head.files {
        if !planner.add(&entry.path, &entry.sha, &head.commit, entry.size) {
            break;
        }
    }
    planner.plan
}

// ------------------------------------------------------------------- fetch

/// Fetch one blob and keep it. The number is the bytes it turned out to be.
///
/// The CDN first, always: it is unmetered, so a signed-in reader cloning a
/// public repository spends none of their hourly allowance on it. A 404 there
/// means the repository is private, which is the API's job — and is remembered,
/// so the rest of the clone goes straight there.
async fn fetch(token: &str, repo: &RepoRef, want: &Want) -> Result<usize> {
    let mut reply = None;
    if cdn_serves(repo) {
        let (status, body) = github::raw_file(repo, &want.commit, &want.path).await?;
        if (200..300).contains(&status) {
            reply = Some(body);
        } else if status == 404 && !token.is_empty() {
            cdn_refused(repo);
        } else {
            bail!("HTTP {status} for {}", want.path.display());
        }
    }

    let body = match reply {
        Some(body) => body,
        None if token.is_empty() => bail!("{} is not public", repo),
        None => {
            // Every one of these is a request out of the hour's five thousand,
            // and being locked out of GitHub is a worse outcome than a clone
            // that only got most of the way.
            let spent = API_SPENT.with(|s| {
                s.set(s.get() + 1);
                s.get()
            });
            if spent > MAX_API_BLOBS {
                bail!("this clone's share of the API rate limit is spent");
            }
            let (status, body) = github::api_blob(token, repo, &want.sha).await?;
            if !(200..300).contains(&status) {
                bail!("HTTP {status} for {}", want.path.display());
            }
            body
        }
    };

    blobs::write(&want.sha, &body, want.size).await;
    Ok(body.len())
}

/// How far along a clone is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
    pub cached: usize,
    pub skipped: usize,
    /// Bytes actually downloaded so far, against what the tree said they would
    /// come to.
    pub bytes: u64,
    pub expected: u64,
    /// Files that would not come down. They are not retried here — opening one
    /// tries again, and says so if it fails again.
    pub failed: usize,
}

impl Progress {
    pub fn finished(&self) -> bool {
        self.done >= self.total
    }
}

/// Pull a commit into the local store, reporting as it goes.
///
/// `progress` is called once as soon as the size of the job is known — which is
/// the moment the reader finds out whether this is a first visit or a top-up —
/// and again after every file. `wanted` is checked between files: closing the
/// pull request stops the clone where it stands, and what has already been
/// written stays written.
pub async fn hydrate(
    token: String,
    head: Snapshot,
    base: Snapshot,
    changed: Vec<FetchJob>,
    mut progress: impl FnMut(Progress),
    wanted: impl Fn() -> bool,
) {
    if !blobs::open().await || head.is_empty() {
        return;
    }
    // Written before anything is fetched: an interrupted clone still leaves a
    // record of what this commit is made of, and re-writing it is what marks
    // the repository as recently read.
    blobs::save(&head).await;
    if !base.is_empty() {
        blobs::save(&base).await;
    }

    let repo = head.repo.clone();
    let Plan {
        wanted: files,
        cached,
        skipped,
        bytes: expected,
    } = plan(&head, &base, &changed);

    let mut at = Progress {
        total: files.len(),
        cached,
        skipped,
        expected,
        ..Default::default()
    };
    progress(at);
    if files.is_empty() {
        return;
    }
    API_SPENT.with(|s| s.set(0));

    let token = &token;
    let repo = &repo;
    let mut stream = stream::iter(
        files
            .into_iter()
            .map(|want| async move { fetch(token, repo, &want).await.map_err(|_| ()) }),
    )
    .buffer_unordered(CONCURRENCY);

    while let Some(result) = stream.next().await {
        at.done += 1;
        match result {
            Ok(bytes) => at.bytes += bytes as u64,
            Err(()) => at.failed += 1,
        }
        // Not every file: a repository is thousands of them, and each report
        // redraws the bar it is written on. Often enough to look like it is
        // moving is what a progress note is for.
        if at.done <= 2 || at.done.is_multiple_of(REPORT_EVERY) || at.finished() {
            progress(at);
        }
        if !wanted() {
            return;
        }
        // Whatever is going wrong is not going to stop going wrong over the
        // next four thousand requests. What is here is here; the rest is read
        // when somebody opens it, which is where the error will be visible.
        if at.failed >= GIVE_UP_AFTER && at.failed * 2 > at.done {
            at.total = at.done;
            progress(at);
            break;
        }
        // Somebody is looking at something. They are one file and this is
        // thousands, and theirs is the one with a cursor blinking at it.
        while WAITING.with(|w| w.get()) > 0 {
            breathe(YIELD_MS).await;
            if !wanted() {
                return;
            }
        }
        // And a turn for the event loop regardless, so the tree still scrolls
        // and the panes still drag while several thousand files come down.
        if at.done.is_multiple_of(YIELD_EVERY) {
            breathe(0).await;
        }
    }
    // A clone that downloaded nothing changed nothing, so there is nothing to
    // ask about and nothing to tidy up after.
    if at.bytes > 0 {
        blobs::keep().await;
        blobs::tidy(blobs::CACHE_BYTES).await;
    }
}

// -------------------------------------------------------------------- read

/// One side of a file: off the disk if it is there, off the network if it is
/// not — and kept on the way past, so the second reader of a file the clone
/// skipped does not fetch it again either.
async fn side(
    token: &str,
    repo: &RepoRef,
    commit: &str,
    path: &Path,
    blob: Option<&TreeEntry>,
) -> Result<FileContent> {
    let Some(blob) = blob else {
        // No tree to say what this hashes to, so there is nothing to file a
        // copy under. Read it and let it go.
        return github::file_at(token, repo, commit, path).await;
    };
    let sha = blob.sha.as_str();
    if let Some(bytes) = blobs::read(sha, blob.size).await {
        return Ok(FileContent::from_bytes(&bytes));
    }

    let mut reply = None;
    if cdn_serves(repo) {
        let (status, body) = github::raw_file(repo, commit, path).await?;
        match status {
            200..=299 => reply = Some(body),
            404 if !token.is_empty() => cdn_refused(repo),
            404 => return Ok(FileContent::Absent),
            _ => bail!("GitHub returned HTTP {status} for {}", path.display()),
        }
    }
    let body = match reply {
        Some(body) => body,
        None => {
            let (status, body) = github::api_blob(token, repo, sha).await?;
            if status == 404 {
                return Ok(FileContent::Absent);
            }
            if !(200..300).contains(&status) {
                bail!("GitHub returned HTTP {status} for {}", path.display());
            }
            body
        }
    };

    blobs::write(sha, &body, blob.size).await;
    Ok(FileContent::from_bytes(&body))
}

/// Whether both sides of a file could be read without asking GitHub for
/// anything.
///
/// What the tree's hover warming is for is hiding a request, and there is no
/// request to hide here — so hovering a repository that has been cloned is
/// free, rather than thousands of reads and decodes of files nobody opened.
pub fn is_local(job: &FetchJob) -> bool {
    let side = |wanted: bool, blob: Option<&TreeEntry>| match blob {
        _ if !wanted => true,
        Some(entry) => blobs::have(&entry.sha),
        // Nothing is known about this side, so nothing can be assumed of it.
        None => false,
    };
    side(job.wants_head(), job.head_blob.as_ref()) && side(job.wants_base(), job.base_blob.as_ref())
}

/// Both sides of one file, from the store where possible.
///
/// An added file has no base side and a deleted one has no head side; an
/// untouched file is never diffed, so its base side is not read either.
pub async fn read_pair(token: &str, job: &FetchJob) -> Result<(FileContent, FileContent)> {
    // Somebody is waiting on this one — held until both sides are in hand, so
    // a clone in progress gets out of the way for the whole read rather than
    // between its halves.
    let _waiting = Waiting::new();
    let base = if job.wants_base() {
        side(
            token,
            &job.repo,
            &job.base_sha,
            &job.base_path,
            job.base_blob.as_ref(),
        )
        .await?
    } else {
        FileContent::Absent
    };
    let head = if job.wants_head() {
        side(
            token,
            &job.repo,
            &job.head_sha,
            &job.path,
            job.head_blob.as_ref(),
        )
        .await?
    } else {
        FileContent::Absent
    };
    Ok((base, head))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, sha: &str, size: u64) -> TreeEntry {
        TreeEntry {
            path: PathBuf::from(path),
            sha: sha.to_string(),
            size,
        }
    }

    fn snapshot(files: Vec<TreeEntry>) -> Snapshot {
        let mut files = files;
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Snapshot {
            repo: RepoRef {
                owner: "o".into(),
                name: "n".into(),
            },
            commit: "head".into(),
            files,
            truncated: false,
        }
    }

    #[test]
    fn the_whole_tree_is_wanted() {
        let head = snapshot(vec![
            entry("src/main.rs", &"a".repeat(40), 100),
            entry("README.md", &"b".repeat(40), 200),
        ]);
        let plan = plan(&head, &Snapshot::default(), &[]);
        assert_eq!(plan.wanted.len(), 2);
        assert_eq!(plan.bytes, 300);
        // Every one of them is named by the commit the CDN will serve it at.
        assert!(plan.wanted.iter().all(|w| w.commit == "head"));
    }

    #[test]
    fn one_copy_of_a_file_that_appears_twice() {
        let sha = "c".repeat(40);
        let head = snapshot(vec![
            entry("a/LICENSE", &sha, 1000),
            entry("b/LICENSE", &sha, 1000),
        ]);
        let plan = plan(&head, &Snapshot::default(), &[]);
        assert_eq!(plan.wanted.len(), 1, "identical content is one blob");
        assert_eq!(plan.bytes, 1000);
    }

    #[test]
    fn what_the_viewer_would_not_draw_is_left_alone() {
        let head = snapshot(vec![
            entry("logo.png", &"d".repeat(40), 4_000),
            entry("vendor.min.js", &"e".repeat(40), MAX_BLOB_BYTES + 1),
            entry("src/lib.rs", &"f".repeat(40), 4_000),
        ]);
        let plan = plan(&head, &Snapshot::default(), &[]);
        assert_eq!(plan.wanted.len(), 1);
        assert_eq!(plan.wanted[0].path, PathBuf::from("src/lib.rs"));
        assert_eq!(plan.skipped, 2);
    }

    #[test]
    fn opaque_is_by_extension_whatever_the_case() {
        assert!(is_opaque(Path::new("a/b/photo.JPG")));
        assert!(is_opaque(Path::new("fonts/x.woff2")));
        assert!(!is_opaque(Path::new("src/main.rs")));
        assert!(!is_opaque(Path::new("Makefile")));
    }

    #[test]
    fn changed_files_are_fetched_first_and_from_both_commits() {
        let head = snapshot(vec![
            entry("a.rs", &"1".repeat(40), 10),
            entry("z.rs", &"2".repeat(40), 10),
        ]);
        let mut base = snapshot(vec![entry("z.rs", &"3".repeat(40), 10)]);
        base.commit = "base".to_string();

        let job = FetchJob {
            repo: head.repo.clone(),
            base_sha: "base".into(),
            head_sha: "head".into(),
            path: PathBuf::from("z.rs"),
            base_path: PathBuf::from("z.rs"),
            head_blob: Some(entry("z.rs", &"2".repeat(40), 10)),
            base_blob: Some(entry("z.rs", &"3".repeat(40), 10)),
            status: Some(crate::backend::tree::ChangeKind::Modified),
        };

        let plan = plan(&head, &base, &[job]);
        let order: Vec<_> = plan.wanted.iter().map(|w| w.sha.clone()).collect();
        assert_eq!(
            order,
            vec!["2".repeat(40), "3".repeat(40), "1".repeat(40)],
            "the changed file's two sides come before the rest of the tree"
        );
        // The base side is read from the base commit, not the head.
        assert_eq!(plan.wanted[1].commit, "base");
    }

    #[test]
    fn a_budget_that_runs_out_stops_the_walk() {
        // Each one is allowed on its own; together they are more repository
        // than anybody asked to have downloaded.
        let files = (0..200)
            .map(|i| entry(&format!("f{i:03}.rs"), &format!("{i:040}"), MAX_BLOB_BYTES))
            .collect();
        let plan = plan(&snapshot(files), &Snapshot::default(), &[]);
        assert!(plan.bytes <= MAX_CLONE_BYTES);
        assert_eq!(plan.wanted.len() as u64, MAX_CLONE_BYTES / MAX_BLOB_BYTES);
        assert!(plan.skipped >= 1, "and the rest are left for later");
    }
}
