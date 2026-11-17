//! pullspace, built, for embedding in another app's page — and the protocol that app
//! speaks to it.
//!
//! The bundle in `dist/` is pullspace's WebAssembly build, made by `build.sh` with its
//! base path set to [`BASE`]: a host serves [`file`] at those paths, from its own origin,
//! and opens `BASE?embed` in a frame of its page. The frame then asks its parent for the
//! files to review over `postMessage`, in the shapes in [`protocol`]; how the parent gets
//! them (from a git checkout, over a socket) is the host's business.
//!
//! ```text
//! frame → parent  {ps: 1, id, t: "snapshot", body: {}}            → {ps: 1, id, body: Snapshot}
//! frame → parent  {ps: 1, id, t: "blobs",    body: BlobsRequest}  → {ps: 1, id, body: BlobsReply}
//! frame → parent  {ps: 1, t: "ready"}                             (no answer: listening)
//! frame → parent  {ps: 1, t: "quote", body: Quote}                (no answer)
//! frame → parent  {ps: 1, t: "missing", body: Open}               (no answer)
//! parent → frame  {ps: 1, event: "changed"}                       (read the snapshot again)
//! parent → frame  {ps: 1, event: "open", body: Open}              (show this file)
//! ```
//!
//! A failed request is answered `{ps: 1, id, error: "…"}`. Messages only count between a
//! frame and its parent of the same origin. The frame hears events from `ready` on: a
//! host with one to send sooner keeps it until then.
//!
//! The frame is as wide as the host makes it. Narrow, it folds its explorer away behind
//! a button and shows a diff as one column, so a host need not keep it wide — beside a
//! conversation, say, or on a phone.

pub mod protocol;

/// Where the bundle expects to be served from. It was built for this path, so it is not a
/// choice a host gets to make.
pub const BASE: &str = "/pullspace/";

/// What the frame needs of its page's Content Security Policy: pullspace builds its UI
/// with `eval` and inline styles, and draws HTML documents in `srcdoc` frames with nonce'd
/// scripts of their own. It needs nothing beyond the origin it is served from.
pub const CSP: &str = "default-src 'self'; \
     script-src 'self' 'unsafe-inline' 'unsafe-eval' 'wasm-unsafe-eval'; \
     style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; \
     connect-src 'self'; frame-src 'self' data: blob:; object-src 'none'; base-uri 'none'; \
     form-action 'none'; frame-ancestors 'self'";

/// One file of the bundle, as it should be served.
#[derive(Clone, Copy, Debug)]
pub struct File {
    /// Under [`BASE`]: `""` for the page itself.
    pub name: &'static str,
    pub content_type: &'static str,
    /// `Content-Encoding` to send it with, when it is stored compressed.
    pub encoding: Option<&'static str>,
    /// Whether it may be cached for good. The page is not; the script and the module
    /// have a version on their URLs, so they are.
    pub immutable: bool,
    pub body: &'static [u8],
}

/// The pullspace commit the bundle was built from, as `build.sh` recorded it.
#[cfg(feature = "assets")]
pub const COMMIT: &str = include_str!("../dist/COMMIT");

/// Every file of the bundle.
#[cfg(feature = "assets")]
pub const FILES: &[File] = &[
    File {
        name: "",
        content_type: "text/html; charset=utf-8",
        encoding: None,
        immutable: false,
        body: include_bytes!("../dist/index.html"),
    },
    File {
        name: "pullspace.js",
        content_type: "text/javascript; charset=utf-8",
        encoding: None,
        immutable: true,
        body: include_bytes!("../dist/pullspace.js"),
    },
    File {
        name: "pullspace_bg.wasm",
        content_type: "application/wasm",
        encoding: Some("gzip"),
        immutable: true,
        body: include_bytes!("../dist/pullspace_bg.wasm.gz"),
    },
];

/// The file at `path` (a full path, starting with [`BASE`]), if the bundle has one there.
#[cfg(feature = "assets")]
pub fn file(path: &str) -> Option<&'static File> {
    let name = path.strip_prefix(BASE)?;
    let name = name.strip_prefix("index.html").unwrap_or(name);
    FILES.iter().find(|f| f.name == name)
}

/// Every path the bundle is served at, for a host that has to list them (a relay, say).
#[cfg(feature = "assets")]
pub fn paths() -> impl Iterator<Item = String> {
    FILES.iter().map(|f| format!("{BASE}{}", f.name))
}

#[cfg(all(test, feature = "assets"))]
mod tests {
    use super::*;

    #[test]
    fn the_bundle_is_whole_and_points_at_itself() {
        let page = std::str::from_utf8(file(BASE).unwrap().body).unwrap();
        let script = std::str::from_utf8(file("/pullspace/pullspace.js").unwrap().body).unwrap();
        // The page loads the script, and the script the module, by their versioned paths.
        assert!(page.contains("/pullspace/pullspace.js?v="), "{page}");
        assert!(script.contains("/pullspace/pullspace_bg.wasm?v="));
        let module = file("/pullspace/pullspace_bg.wasm").unwrap();
        assert_eq!(&module.body[..2], [0x1f, 0x8b], "gzipped");
        assert!(file("/pullspace/index.html").is_some());
        assert!(file("/pullspace/other").is_none() && file("/elsewhere").is_none());
        assert_eq!(paths().count(), FILES.len());
        assert!(!COMMIT.trim().is_empty());
    }
}
