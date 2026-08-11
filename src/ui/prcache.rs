//! The pull request file cache: the one place that decides when a file's two
//! sides get fetched, so clicking through a review never waits on the network.
//!
//! Three callers funnel through [`ensure_file`] — the background warm-up, the
//! tree's hover handler, and the viewer opening a file — and it is cheap to
//! call repeatedly, so none of them need to know about the others.

use std::path::{Path, PathBuf};

use dioxus::prelude::*;

use crate::backend::github::{file_at, find_file, PrDetail, RepoRef};
use crate::backend::mirror;
use crate::backend::tree::ChangeKind;
use crate::backend::FileContent;

use super::app::{PrFileState, PrSource, St};

/// How many files to pull at once while warming. GitHub throttles bursts of
/// concurrent requests, so this stays deliberately modest.
const PREFETCH_CONCURRENCY: usize = 6;

/// Everything needed to fetch one file, lifted out of app state so the fetch
/// does not borrow a signal across an await.
#[derive(Clone, PartialEq)]
pub struct FetchJob {
    repo: RepoRef,
    base_sha: String,
    head_sha: String,
    pub path: PathBuf,
    /// Differs from `path` for renames.
    base_path: PathBuf,
    /// `None` for a file the PR does not touch — browsable, but not a diff.
    status: Option<ChangeKind>,
}

impl FetchJob {
    /// For any path in the PR's repository, changed or not.
    pub fn new(pr: &PrDetail, rel: &Path) -> Self {
        // Unchanged files are in the repo tree but not the PR's file list.
        let f = find_file(&pr.files, rel);
        FetchJob {
            repo: pr.repo.clone(),
            base_sha: pr.base_sha.clone(),
            head_sha: pr.head_sha.clone(),
            path: rel.to_path_buf(),
            base_path: f.map_or_else(|| rel.to_path_buf(), |f| f.base_path().clone()),
            status: f.map(|f| f.status),
        }
    }
}

/// Both sides of one file. Blocking — callers run it off the UI thread.
///
/// A local source reads straight out of the object database, which is roughly
/// two orders of magnitude faster than the per-file HTTP request the API path
/// needs.
fn fetch_pair(
    token: &str,
    source: &PrSource,
    job: &FetchJob,
) -> anyhow::Result<(FileContent, FileContent)> {
    let read = |sha: &str, path: &Path| match source {
        PrSource::Local { git_dir, .. } => mirror::blob_at(git_dir, sha, path),
        PrSource::Api => file_at(token, &job.repo, sha, path),
    };
    // An added file has no base side, a deleted one has no head side, and an
    // untouched file is never diffed — so in each case skip a read that would
    // be wasted or 404 anyway.
    let base = match job.status {
        None | Some(ChangeKind::Added) => FileContent::Absent,
        Some(_) => read(&job.base_sha, &job.base_path)?,
    };
    let head = if job.status == Some(ChangeKind::Deleted) {
        FileContent::Absent
    } else {
        read(&job.head_sha, &job.path)?
    };
    Ok((base, head))
}

type Fetched = Result<anyhow::Result<(FileContent, FileContent)>, tokio::task::JoinError>;

fn settle(result: Fetched) -> PrFileState {
    match result {
        Ok(Ok((base, head))) => PrFileState::Ready { base, head },
        Ok(Err(e)) => PrFileState::Failed(format!("{e:#}")),
        Err(e) => PrFileState::Failed(format!("Fetch failed: {e}")),
    }
}

/// Whether this path already has content or a fetch in flight. A `Failed`
/// entry is deliberately not claimed, so reopening a file retries it.
fn claimed(st: &St, path: &Path) -> bool {
    matches!(
        st.pr_files.peek().get(path),
        Some(PrFileState::Loading | PrFileState::Ready { .. })
    )
}

/// Start fetching a file unless it is already cached or on its way.
///
/// Dioxus writes signals only from the UI thread, so the check and the claim
/// below cannot interleave with another caller — `Loading` therefore always
/// means genuinely in flight, and no file is ever fetched twice.
pub fn ensure_file(st: St, job: FetchJob) {
    if claimed(&st, &job.path) {
        return;
    }
    let mut cache = st.pr_files;
    let token = st.api_token();
    let source = st.pr_source.peek().clone();
    cache.write().insert(job.path.clone(), PrFileState::Loading);

    // Root scope: a fetch has to outlive the tree row or viewer that triggered
    // it, or unmounting would strand the entry on `Loading` forever.
    spawn_forever(async move {
        let path = job.path.clone();
        let state =
            settle(tokio::task::spawn_blocking(move || fetch_pair(&token, &source, &job)).await);
        let mut cache = st.pr_files;
        cache.write().insert(path, state);
    });
}

/// [`ensure_file`] for callers that only have a path. A no-op outside PR mode.
pub fn ensure_path(st: St, rel: &Path) {
    let job = {
        let ws = st.workspace.peek();
        let Some(pr) = ws.pr() else { return };
        FetchJob::new(pr, rel)
    };
    ensure_file(st, job);
}

/// One job per file the PR changes, for [`prefetch`].
pub fn changed_jobs(pr: &PrDetail) -> Vec<FetchJob> {
    pr.files.iter().map(|f| FetchJob::new(pr, &f.path)).collect()
}

/// Warm every file the PR changes, a few at a time, so clicking through a
/// review is instant.
///
/// Only the changed files: a repository can hold 60k others, and fetching
/// those up front would exhaust the API rate limit for no benefit. They are
/// covered by hover prefetching instead.
pub async fn prefetch(st: St, pr_number: u64, jobs: Vec<FetchJob>, token: String) {
    let mut cache = st.pr_files;
    let source = st.pr_source.peek().clone();

    for chunk in jobs.chunks(PREFETCH_CONCURRENCY) {
        // Stop as soon as the user leaves this pull request.
        if st.workspace.peek().pr().map(|p| p.number) != Some(pr_number) {
            return;
        }

        // Claim the whole chunk first, then await it — `spawn_blocking` starts
        // running on call, so the chunk is genuinely in flight together.
        let mut started = Vec::new();
        for job in chunk {
            if claimed(&st, &job.path) {
                continue;
            }
            cache.write().insert(job.path.clone(), PrFileState::Loading);
            let token = token.clone();
            let source = source.clone();
            let job = job.clone();
            let path = job.path.clone();
            started.push((
                path,
                tokio::task::spawn_blocking(move || fetch_pair(&token, &source, &job)),
            ));
        }
        for (path, handle) in started {
            // Await first, *then* take the write guard. Inlining these would
            // hold a borrow of the signal across the await, and the viewer
            // reading the cache mid-fetch would panic the task.
            let state = settle(handle.await);
            cache.write().insert(path, state);
        }
    }
}

/// How many of the PR's changed files are cached, for the progress readout.
/// Reads reactively so the top bar ticks upward on its own.
pub fn warmed(st: &St, pr: &PrDetail) -> usize {
    let cache = st.pr_files.read();
    pr.files
        .iter()
        .filter(|f| matches!(cache.get(&f.path), Some(PrFileState::Ready { .. })))
        .count()
}
