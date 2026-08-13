//! The file cache: the one place that decides when a file's two sides are
//! read, so clicking through a review never waits on the network.
//!
//! There are two layers under this. The repository itself is on disk, in the
//! browser's own filesystem — [`clone`](crate::backend::clone) puts it there in
//! one go when a pull request opens, and it survives the tab being closed.
//! What lives here is the decoded form of the handful of files somebody has
//! actually looked at, since decoding forty thousand files into strings is not
//! something to do to a browser tab.
//!
//! Three callers funnel through [`ensure_file`] — the clone's stragglers, the
//! tree's hover handler, and the viewer opening a file — and it is cheap to
//! call repeatedly, so none of them need to know about the others.

use std::path::Path;

use dioxus::prelude::*;

use crate::backend::github::{FetchJob, PrDetail, Snapshot};
use crate::backend::{blobs, clone};

use super::app::{PrFileState, St};

/// How many files to keep decoded in memory. Enough that going back and forth
/// across a review costs nothing; few enough that a browsed repository cannot
/// quietly become a hundred megabytes of strings. Past it, the least recently
/// read are dropped — and come back off the disk, not the network.
const MEMORY_FILES: usize = 128;

/// How many files the memory-only fallback reads at once. GitHub throttles
/// bursts of concurrent requests, so this stays deliberately modest.
const WARM_CONCURRENCY: usize = 6;

fn settle(
    result: anyhow::Result<(crate::backend::FileContent, crate::backend::FileContent)>,
) -> PrFileState {
    match result {
        Ok((base, head)) => PrFileState::Ready { base, head },
        Err(e) => PrFileState::Failed(format!("{e:#}")),
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

/// Put a file's content in memory, and drop the oldest if the shelf is full.
///
/// Never the file being read: it is the one thing on screen, and re-reading it
/// to draw it would flicker the viewer back to `Loading`.
fn remember(st: &St, path: &Path, state: PrFileState) {
    let mut cache = st.pr_files;
    let mut order = st.warm_order;
    cache.write().insert(path.to_path_buf(), state);

    let mut order = order.write();
    order.push_back(path.to_path_buf());
    while order.len() > MEMORY_FILES {
        let Some(oldest) = order.pop_front() else { break };
        // Its own entry, superseded by a later read of the same file.
        if order.contains(&oldest) || st.open.peek().as_deref() == Some(oldest.as_path()) {
            continue;
        }
        cache.write().remove(&oldest);
    }
}

/// Start reading a file unless it is already in hand or on its way.
///
/// Dioxus writes signals only from the UI thread, so the check and the claim
/// below cannot interleave with another caller — `Loading` therefore always
/// means genuinely in flight, and no file is ever read twice at once.
pub fn ensure_file(st: St, job: FetchJob) {
    if claimed(&st, &job.path) {
        return;
    }
    let mut cache = st.pr_files;
    let token = st.api_token();
    cache.write().insert(job.path.clone(), PrFileState::Loading);

    // Root scope: a read has to outlive the tree row or viewer that triggered
    // it, or unmounting would strand the entry on `Loading` forever.
    spawn_forever(async move {
        let path = job.path.clone();
        let state = settle(clone::read_pair(&token, &job).await);
        remember(&st, &path, state);
    });
}

/// Everything needed to read one path of whatever is open.
fn job_for(st: &St, rel: &Path) -> Option<FetchJob> {
    let ws = st.workspace.peek();
    match (ws.pr(), ws.repo()) {
        (Some(pr), _) => Some(FetchJob::new(pr, rel)),
        (_, Some(view)) => Some(FetchJob::browsing(view, rel)),
        _ => None,
    }
}

/// [`ensure_file`] for callers that only have a path.
pub fn ensure_path(st: St, rel: &Path) {
    // Before building a job, not after: the job below clones the pull request's
    // identifiers and scans its file list to find this path, and the common case
    // — a file already in hand or on its way — needs none of that. `ensure_file`
    // makes the same check for callers who arrive holding a job already.
    if claimed(&st, rel) {
        return;
    }
    let Some(job) = job_for(&st, rel) else { return };
    ensure_file(st, job);
}

/// Warm the file the pointer has come to rest on — but only one that would
/// otherwise have to be fetched.
///
/// A file already in the local store opens in about a millisecond, so reading
/// it early saves nothing and is far from free: every row the pointer settles
/// on would be a filesystem read, a whole file decoded into a string, and a
/// write to the cache that makes the viewer re-examine what it has drawn.
/// Doing that for every row somebody moves across is how browsing a repository
/// turns into watching one.
pub fn ensure_hover(st: St, rel: &Path) {
    if claimed(&st, rel) {
        return;
    }
    let Some(job) = job_for(&st, rel) else { return };
    if clone::is_local(&job) {
        return;
    }
    ensure_file(st, job);
}

/// One job per file the PR changes — what the clone fetches before it fetches
/// anything else, since it is what the review is about.
pub fn changed_jobs(pr: &PrDetail) -> Vec<FetchJob> {
    pr.files.iter().map(|f| FetchJob::new(pr, &f.path)).collect()
}

/// The fallback for a browser with nowhere to keep anything: read the pull
/// request's own files, a few at a time, and hold them in memory.
///
/// Only those. Without a store there is nothing to gain from the rest of the
/// repository and a whole repository's worth of strings to lose by it.
async fn warm(st: St, changed: Vec<FetchJob>, token: String) {
    let mine = st.generation();
    for chunk in changed.chunks(WARM_CONCURRENCY) {
        if st.generation() != mine {
            return;
        }
        // Claim the whole chunk first, then run it together — `join_all` polls
        // every future at once, so the chunk is genuinely in flight together.
        let mut started = Vec::new();
        for job in chunk {
            if claimed(&st, &job.path) {
                continue;
            }
            let mut cache = st.pr_files;
            cache.write().insert(job.path.clone(), PrFileState::Loading);
            let token = token.clone();
            let job = job.clone();
            started.push(async move { (job.path.clone(), clone::read_pair(&token, &job).await) });
        }
        // Collect first, *then* write per result. Writing the signal inside the
        // joined futures would hold a borrow across an await, and the viewer
        // reading the cache mid-fetch would panic.
        for (path, result) in futures_util::future::join_all(started).await {
            remember(&st, &path, settle(result));
        }
    }
}

/// Clone what is open into the browser's filesystem, keeping the top bar's
/// readout up to date, and stopping the moment something else is opened.
pub async fn clone_repo(st: St, head: Snapshot, base: Snapshot, changed: Vec<FetchJob>) {
    let mut status = st.cloning;
    let token = st.api_token();
    // No filesystem to clone into — a private window, or a browser too old for
    // it. Warm what the review is actually about, into memory, which is what
    // this did before there was anywhere to put a repository.
    if !blobs::open().await {
        return warm(st, changed, token).await;
    }
    // Whatever is opened next retires this one — including the same pull
    // request opened again by `⟳`, which wants the clone that follows its new
    // head commit and not the one still finishing the old.
    let mine = st.generation();
    clone::hydrate(
        token,
        head,
        base,
        changed,
        move |at| {
            if st.generation() == mine {
                status.set(Some(at));
            }
        },
        move || st.generation() == mine,
    )
    .await;
}
