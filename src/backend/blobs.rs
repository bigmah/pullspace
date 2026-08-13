//! The local copy of everything that has been read, kept in the browser's
//! filesystem and keyed the way git keys it: by the SHA-1 of the contents.
//!
//! Two directories under [`opfs`](super::opfs)'s root:
//!
//! ```text
//! blob/<xx>/<40 hex>   one file's bytes, exactly as GitHub served them
//! snap/<owner>~<name>~<commit>.json   which paths that commit is made of
//! ```
//!
//! Naming blobs by content rather than by commit is the whole reason this is
//! worth keeping between visits. A pull request's head commit did not exist the
//! last time you were here, but nearly every file in it did, and each one hashes
//! to what it hashed to then — so opening a second pull request on a repository
//! already read downloads the handful of files that actually differ and reads
//! the other few thousand off disk.
//!
//! The manifests are maps, not promises: one may name blobs that were never
//! fetched, because a clone was interrupted or skipped a file for being large.
//! Every read checks, and a miss is a fetch.
//!
//! Nothing here is load-bearing. A browser without OPFS, a private window, or a
//! disk with nothing left on it all end at "not cached", which costs speed and
//! nothing else.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};

use web_sys::FileSystemDirectoryHandle;

use super::github::{RepoRef, Snapshot};
use super::opfs;

/// Unique blob bytes to keep before the oldest repositories are dropped.
///
/// Counted from the manifests rather than measured, so it is the size of what
/// is worth keeping rather than of what is on disk — the sweep that enforces it
/// deletes the difference.
pub const CACHE_BYTES: u64 = 1 << 30;

/// Blobs are spread over 256 directories by the first byte of their hash.
/// One directory holding every file of every repository ever opened is a
/// listing no filesystem enjoys.
const SHARD: usize = 2;

/// Written at startup, and read there first: it says both that this browser can
/// be written to at all and that what is already here was written by code that
/// agrees with this code about how to do it.
const MARKER: &str = "store";

/// Bump this and every stored file is thrown away on the next load.
///
/// What it is for: a bug in the writing is not a bug the reading can see. Blobs
/// written before 0.2 could be silently zero-filled — the bytes went to the
/// browser as a view into wasm memory that could be detached before it read
/// them — and a file of zeros is indistinguishable from a genuinely binary one.
/// Nothing here can tell the two apart after the fact, so the store from before
/// the fix is not one to keep.
const VERSION: &[u8] = b"0.2";

struct Store {
    blobs: FileSystemDirectoryHandle,
    snaps: FileSystemDirectoryHandle,
    /// Shard directories, once opened. A blob write would otherwise cost a
    /// directory lookup as well.
    shards: HashMap<String, FileSystemDirectoryHandle>,
    /// Which blobs are on disk. Built once by listing, kept current by
    /// [`write`] — the clone asks this thousands of times while planning, and
    /// it has to answer without awaiting.
    have: HashSet<String>,
}

thread_local! {
    static STORE: RefCell<Option<Store>> = const { RefCell::new(None) };
    /// Set once OPFS has been asked for and refused, so a browser without it
    /// is not asked again on every file.
    static UNAVAILABLE: RefCell<bool> = const { RefCell::new(false) };
    /// Whether the browser has been asked to hold on to all this.
    static ASKED_TO_KEEP: RefCell<bool> = const { RefCell::new(false) };
    /// Set while the index is being read, so it is only ever read once.
    static OPENING: Cell<bool> = const { Cell::new(false) };
}

fn with<T>(f: impl FnOnce(&mut Store) -> T) -> Option<T> {
    STORE.with(|s| s.borrow_mut().as_mut().map(f))
}

/// The forty hex characters a git blob is named by. Anything else is not
/// something to build a filename out of.
fn is_sha(sha: &str) -> bool {
    sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Open the store and read its index, once per page.
///
/// Everything else here calls this first and it is nearly always a flag check.
/// The once it is not, a second caller arriving mid-open waits for the first
/// rather than listing the whole store alongside it — and a clone that started
/// before the index was ready has to wait for the real answer, because "not
/// cached yet" would send it to the network for a repository already on disk.
pub async fn open() -> bool {
    if with(|_| ()).is_some() {
        return true;
    }
    if UNAVAILABLE.with(|u| *u.borrow()) {
        return false;
    }
    if OPENING.with(|o| o.replace(true)) {
        while OPENING.with(|o| o.get()) {
            gloo_timers::future::TimeoutFuture::new(15).await;
        }
        return with(|_| ()).is_some();
    }
    let opened = build().await;
    OPENING.with(|o| o.set(false));
    opened
}

async fn build() -> bool {
    let Some(root) = opfs::root().await else {
        UNAVAILABLE.with(|u| *u.borrow_mut() = true);
        return false;
    };
    // Anything written by an older pullspace goes, before it is read from and
    // believed.
    if opfs::read(&root, MARKER).await.as_deref() != Some(VERSION) {
        opfs::remove_all(&root, "blob").await;
        opfs::remove_all(&root, "snap").await;
    }
    let (Some(blobs), Some(snaps)) = (opfs::dir(&root, "blob").await, opfs::dir(&root, "snap").await)
    else {
        UNAVAILABLE.with(|u| *u.borrow_mut() = true);
        return false;
    };
    // Written after the clearing above, so a wipe interrupted half way is one
    // that happens again rather than one that is assumed to have finished. It
    // doubles as the writability test: reading a directory is not the same as
    // being able to fill it, and Safari before 17 has OPFS but no
    // `createWritable`. Either way the answer has to be no *now*, before a
    // clone downloads a repository it has nowhere to put.
    if !opfs::write(&root, MARKER, VERSION).await {
        UNAVAILABLE.with(|u| *u.borrow_mut() = true);
        return false;
    }

    let mut have = HashSet::new();
    for prefix in opfs::list(&blobs).await {
        let Some(shard) = opfs::dir(&blobs, &prefix).await else {
            continue;
        };
        have.extend(opfs::list(&shard).await.into_iter().filter(|n| is_sha(n)));
    }

    STORE.with(|s| {
        let mut slot = s.borrow_mut();
        match slot.as_mut() {
            // Two callers opened at once. Merging rather than replacing keeps
            // whatever the other one has written in the meantime.
            Some(existing) => existing.have.extend(have),
            None => {
                *slot = Some(Store {
                    blobs,
                    snaps,
                    shards: HashMap::new(),
                    have,
                })
            }
        }
    });
    true
}

/// Whether a blob is already on disk. Answers without awaiting, because the
/// clone asks once per file in the repository before fetching anything.
pub fn have(sha: &str) -> bool {
    with(|s| s.have.contains(sha)).unwrap_or(false)
}

/// How many blobs are stored, for the readout.
pub fn stored() -> usize {
    with(|s| s.have.len()).unwrap_or(0)
}

async fn shard_of(sha: &str) -> Option<FileSystemDirectoryHandle> {
    let prefix = sha.get(..SHARD)?.to_string();
    if let Some(handle) = with(|s| s.shards.get(&prefix).cloned()).flatten() {
        return Some(handle);
    }
    let blobs = with(|s| s.blobs.clone())?;
    let handle = opfs::dir(&blobs, &prefix).await?;
    with(|s| s.shards.insert(prefix, handle.clone()));
    Some(handle)
}

/// Whether a blob is the length the tree said it would be.
///
/// The only check available: the store is keyed by a hash of git's making, and
/// nothing here computes SHA-1. It is enough for the failure that matters,
/// which is a write that landed short or did not land at all. `0` means the
/// tree did not say, and nothing is claimed either way.
fn right_length(bytes: &[u8], expect: u64) -> bool {
    expect == 0 || bytes.len() as u64 == expect
}

/// One blob's bytes, if they are here and they are all here.
pub async fn read(sha: &str, expect: u64) -> Option<Vec<u8>> {
    if !is_sha(sha) || !open().await || !have(sha) {
        return None;
    }
    let shard = shard_of(sha).await?;
    let bytes = opfs::read(&shard, sha)
        .await
        .filter(|b| right_length(b, expect));
    // Either the index said it was here and the filesystem disagrees — a sweep
    // from another tab, or storage reclaimed under us — or what is there is
    // not the length it should be, which is a write that went wrong. Both are
    // cache misses, and both are cleared out here rather than read again
    // tomorrow: a wrong file served as content is a file that looks empty, or
    // binary, for as long as it sits there.
    if bytes.is_none() {
        with(|s| s.have.remove(sha));
        opfs::remove(&shard, sha).await;
    }
    bytes
}

/// Keep a blob. Named by content, so writing one twice is writing the same
/// bytes to the same place.
pub async fn write(sha: &str, bytes: &[u8], expect: u64) -> bool {
    if !is_sha(sha) || !right_length(bytes, expect) || !open().await {
        return false;
    }
    let Some(shard) = shard_of(sha).await else {
        return false;
    };
    if !opfs::write(&shard, sha, bytes).await {
        return false;
    }
    with(|s| s.have.insert(sha.to_string()));
    true
}

// ---------------------------------------------------------------- manifests

/// Record which blobs a commit is made of.
///
/// Written before its files are fetched, not after: an interrupted clone still
/// leaves something that says what was wanted, and the rewrite is what dates
/// the repository as recently used for [`sweep`].
pub async fn save(snapshot: &Snapshot) -> bool {
    if snapshot.is_empty() {
        return false;
    }
    let Some(snaps) = with(|s| s.snaps.clone()) else {
        return false;
    };
    let Ok(json) = serde_json::to_vec(snapshot) else {
        return false;
    };
    opfs::write(&snaps, &snapshot.key(), &json).await
}

/// A commit's file list as it was read last time, so a repository opened again
/// at the same commit needs no tree request at all.
pub async fn load(repo: &RepoRef, commit: &str) -> Option<Snapshot> {
    open().await;
    let snaps = with(|s| s.snaps.clone())?;
    let key = Snapshot {
        repo: repo.clone(),
        commit: commit.to_string(),
        ..Default::default()
    }
    .key();
    let bytes = opfs::read(&snaps, &key).await?;
    let snapshot: Snapshot = serde_json::from_slice(&bytes).ok()?;
    (!snapshot.is_empty()).then_some(snapshot)
}

// ------------------------------------------------------------------ tidying

/// Ask the browser not to reclaim this, once, and only once there is something
/// worth keeping.
///
/// Timing rather than politeness: Chrome decides for itself, but Firefox puts
/// the question to the reader, and a permission prompt on a page that has not
/// been asked to do anything yet is a page nobody trusts. After a repository
/// has been downloaded, it is at least a question about something.
pub async fn keep() {
    if ASKED_TO_KEEP.with(|a| std::mem::replace(&mut *a.borrow_mut(), true)) {
        return;
    }
    opfs::persist().await;
}

/// Tidy up, but only when there is a reason to.
///
/// The check is one call to the browser; the sweep behind it walks every file
/// in the store, which is not something to do after a clone that fitted
/// comfortably.
pub async fn tidy(limit: u64) {
    let over = usage().await.is_some_and(|(used, _)| used as u64 > limit);
    if over {
        sweep(limit).await;
    }
}

/// Drop the least recently opened repositories until what is left fits in
/// `limit` bytes, then delete every blob nothing left refers to.
///
/// Recency is the manifest's own modification time, which [`save`] refreshes
/// each time a commit is opened — so what goes is what has not been looked at,
/// not what was downloaded longest ago.
pub async fn sweep(limit: u64) {
    let Some((blobs, snaps)) = with(|s| (s.blobs.clone(), s.snaps.clone())) else {
        return;
    };

    let mut dated = Vec::new();
    for name in opfs::list(&snaps).await {
        let when = opfs::modified(&snaps, &name).await.unwrap_or(0.0);
        dated.push((when, name));
    }
    breathe().await;
    // Newest first: the budget is spent on what was read most recently.
    dated.sort_by(|a, b| b.0.total_cmp(&a.0));

    let mut keep: HashSet<String> = HashSet::new();
    let mut bytes: u64 = 0;
    for (_, name) in dated {
        let snapshot = opfs::read(&snaps, &name)
            .await
            .and_then(|raw| serde_json::from_slice::<Snapshot>(&raw).ok());
        let Some(snapshot) = snapshot.filter(|_| bytes < limit) else {
            // Over budget, or unreadable. Either way it stops being a reason to
            // keep anything.
            opfs::remove(&snaps, &name).await;
            continue;
        };
        for file in snapshot.files {
            if keep.insert(file.sha) {
                bytes += file.size;
            }
        }
        breathe().await;
    }

    for prefix in opfs::list(&blobs).await {
        let Some(shard) = opfs::dir(&blobs, &prefix).await else {
            continue;
        };
        for sha in opfs::list(&shard).await {
            if !keep.contains(&sha) {
                opfs::remove(&shard, &sha).await;
                with(|s| s.have.remove(&sha));
            }
        }
        breathe().await;
    }
}

/// Stand back and let the event loop have a turn.
///
/// A sweep walks every file of every repository ever opened, and it does it
/// while somebody is reading one of them. Nothing here is in a hurry; the
/// browser it runs in is.
async fn breathe() {
    gloo_timers::future::TimeoutFuture::new(0).await;
}

/// Delete every stored file. What the button in the GitHub panel calls — the
/// only thing here anyone should have to think about.
pub async fn clear() -> bool {
    let Some(root) = opfs::root().await else {
        return false;
    };
    let gone = opfs::remove_all(&root, "blob").await && opfs::remove_all(&root, "snap").await;
    // The handles just deleted are no use to anyone; the next call re-opens.
    STORE.with(|s| *s.borrow_mut() = None);
    open().await;
    gone
}

/// Bytes this origin is using, and what it is allowed. Straight from the
/// browser, for the readout in the panel.
pub async fn usage() -> Option<(f64, f64)> {
    opfs::usage().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_blob_hashes_become_filenames() {
        assert!(is_sha("5f2f0b6c3d9a1e8b7c4d2a0f9e8d7c6b5a4f3e2d"));
        assert!(!is_sha("short"));
        assert!(!is_sha("../../etc/passwd"));
        // Right length, wrong alphabet.
        assert!(!is_sha("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"));
    }

    #[test]
    fn a_blob_that_is_not_its_stated_length_is_not_a_blob() {
        assert!(right_length(b"hello", 5));
        // Short: a write that landed badly, which would read back as an empty
        // file or a binary one rather than as the miss it actually is.
        assert!(!right_length(b"hel", 5));
        assert!(!right_length(b"", 5));
        assert!(!right_length(b"hello world", 5));
        // An empty file really is empty.
        assert!(right_length(b"", 0));
        // GitHub did not say, so nothing is claimed either way.
        assert!(right_length(b"anything at all", 0));
    }

    /// Without a browser there is no filesystem, and every call has to say so
    /// rather than panic — which is also what makes `cargo test` possible.
    #[test]
    fn there_is_no_store_outside_a_browser() {
        assert!(!have("5f2f0b6c3d9a1e8b7c4d2a0f9e8d7c6b5a4f3e2d"));
        assert_eq!(stored(), 0);
    }

    #[test]
    fn manifests_are_named_by_repository_and_commit() {
        let snapshot = Snapshot {
            repo: RepoRef {
                owner: "DioxusLabs".into(),
                name: "dioxus".into(),
            },
            commit: "abc123".into(),
            ..Default::default()
        };
        assert_eq!(snapshot.key(), "DioxusLabs~dioxus~abc123.json");
    }
}
