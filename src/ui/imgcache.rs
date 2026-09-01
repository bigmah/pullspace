//! The picture cache: files read as bytes rather than as text, and held as the
//! URLs a document draws them with.
//!
//! [`prcache`](super::prcache) beside it does the same job for the files being
//! read, and the two are deliberately separate. A picture is fetched because
//! something else — a README, a previewed page — mentioned it, never because
//! anybody opened it; it is kept encoded rather than decoded; and it is a third
//! larger in memory than on disk, which is why what is kept here is bounded in
//! bytes while the other is bounded in files.
//!
//! Under this is the same store as everything else: a screenshot read once is a
//! screenshot on disk, and the second pull request that shows it draws it
//! without a request.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use dioxus::prelude::*;

use crate::backend::clone;
use crate::backend::images::{MAX_IMAGE_BYTES, data_uri, media_type};

use super::app::{ImgState, St};
use super::spaces::Claim;

/// How many bytes of encoded pictures to keep. A README's worth of screenshots
/// several times over, and far short of what a browser tab should be spending
/// on documents nobody is looking at any more. Past it, the least recently
/// asked for go — and come back off the disk, not the network.
const MEMORY_BYTES: usize = 24 << 20;

/// Whether this path is already drawn or on its way. A `Failed` entry is not
/// claimed, so a document opened again retries.
fn claimed(st: &St, path: &Path) -> bool {
    matches!(
        st.images.peek().get(path),
        Some(ImgState::Loading | ImgState::Ready(_))
    )
}

fn settle(st: &St, path: &Path, state: ImgState) {
    let mut images = st.images;
    let mut order = st.image_order;
    images.write().insert(path.to_path_buf(), state);
    order.write().push_back(path.to_path_buf());

    // What is held, measured rather than tracked: the map is a few dozen
    // entries at the very most, and a running total is one more thing to get
    // wrong when an entry is replaced by a later read of the same file.
    let mut held: usize = images
        .peek()
        .values()
        .map(|s| match s {
            ImgState::Ready(uri) => uri.len(),
            _ => 0,
        })
        .sum();
    while held > MEMORY_BYTES {
        let Some(oldest) = order.write().pop_front() else {
            break;
        };
        // Its own entry, superseded by a later read of the same picture.
        if order.peek().contains(&oldest) {
            continue;
        }
        let gone = images.write().remove(&oldest);
        held -= match gone {
            Some(ImgState::Ready(uri)) => uri.len(),
            _ => 0,
        };
    }
}

/// Read a picture out of the repository, unless it is already in hand.
///
/// Everything that can be answered without a request is answered without one:
/// a file the tree has never heard of, a name no browser would draw, and one
/// the tree says is far too big to carry inside a document.
pub fn ensure_image(st: St, rel: &Path) {
    if claimed(&st, rel) {
        return;
    }
    let Some(mime) = media_type(rel) else {
        return fail(&st, rel, "not a picture".to_string());
    };
    if !st.has_file(rel) {
        return fail(&st, rel, "not in this repository".to_string());
    }
    let Some(job) = st.workspace.peek().job_for(rel) else {
        return;
    };
    // The tree already said how big it is, so the one that is too big to draw
    // costs nothing at all rather than a download and then a refusal.
    let size = job
        .head_blob
        .as_ref()
        .or(job.base_blob.as_ref())
        .map_or(0, |blob| blob.size);
    if size > MAX_IMAGE_BYTES {
        return fail(&st, rel, format!("{} is too large to draw", human(size)));
    }

    let mut images = st.images;
    let token = st.api_token();
    let claim = Claim::new(st);
    images.write().insert(rel.to_path_buf(), ImgState::Loading);

    // Root scope: a read has to outlive the document that asked for it, or
    // scrolling away mid-fetch would strand the entry on `Loading` forever.
    spawn_forever(async move {
        let path = job.path.clone();
        let state = match clone::read_bytes(&token, &job).await {
            // Whatever the tree said, this is what turned up.
            Ok(bytes) if bytes.len() as u64 > MAX_IMAGE_BYTES => ImgState::Failed(format!(
                "{} is too large to draw",
                human(bytes.len() as u64)
            )),
            Ok(bytes) => ImgState::Ready(Rc::from(data_uri(mime, &bytes).as_str())),
            Err(e) => ImgState::Failed(format!("{e:#}")),
        };
        // As in `prcache`: a picture that arrives after the reader has moved
        // to another space belongs to a repository that is no longer open.
        if claim.kept() {
            settle(&st, &path, state);
        }
    });
}

fn fail(st: &St, rel: &Path, why: String) {
    settle(st, rel, ImgState::Failed(why));
}

/// A size somebody reading an error is meant to understand.
fn human(bytes: u64) -> String {
    if bytes >= 1 << 20 {
        format!("{:.1} MB", bytes as f64 / (1 << 20) as f64)
    } else {
        format!("{} KB", bytes.div_ceil(1 << 10))
    }
}

/// The URL to draw this picture with, if it is here.
pub fn drawable(st: &St, rel: &Path) -> Option<Rc<str>> {
    match st.images.read().get(rel) {
        Some(ImgState::Ready(uri)) => Some(Rc::clone(uri)),
        _ => None,
    }
}

/// A picture that will not be drawn, and why not. `None` while it is still
/// being read, or once it is here.
pub fn refused(st: &St, rel: &Path) -> Option<String> {
    match st.images.read().get(rel) {
        Some(ImgState::Failed(why)) => Some(why.clone()),
        _ => None,
    }
}

/// Whether every picture in a list has settled one way or the other.
///
/// What the HTML preview waits for. Its pictures are carried inside the
/// document it is handed, so each one arriving would otherwise rebuild that
/// document and reload the frame around it — a page of six screenshots loading
/// itself six times over, in front of somebody trying to read it.
pub fn all_settled<'a>(st: &St, paths: impl IntoIterator<Item = &'a PathBuf>) -> bool {
    let images = st.images.read();
    paths
        .into_iter()
        .all(|p| !matches!(images.get(p), Some(ImgState::Loading)))
}
