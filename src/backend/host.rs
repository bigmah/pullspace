//! The page pullspace is embedded in, when it is one: where the files come
//! from instead of GitHub.
//!
//! Opened as `…/?embed` inside a frame, pullspace reviews whatever its host
//! page hands it — a local checkout's `HEAD` against its working tree, say —
//! and never talks to GitHub at all. The host answers two questions over
//! `postMessage`, and every other part of the app (the store, search, symbols,
//! the diff) works as it does on a pull request, because the answers are the
//! same shape GitHub's are: a [`Snapshot`] per side, and blobs by git hash.
//!
//! The protocol, its message types and the built bundle a host serves are the
//! `pullspace-embed` crate (`crates/embed`), which is what a host depends on.
//!
//! Like `http.rs`, this is a way out of the tab, so it is kept as thin as one:
//! the JavaScript only moves messages, and everything worth testing — turning
//! two file lists into a comparison — is plain Rust below.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;

use super::github::{Commits, CompareView, PrFile, RepoRef, Snapshot, TreeEntry};
use super::tree::ChangeKind;

use pullspace_embed::protocol::{self, BlobsRequest, Want};
/// The protocol's types, under the names this module has always used for them.
pub use pullspace_embed::protocol::{Side as HostSide, Snapshot as HostSnapshot};

/// The owner every embedded repository is filed under, so its snapshots can
/// never be mistaken for a GitHub repository's.
pub const OWNER: &str = "local";

/// Whether this page is embedded in a host that supplies the files.
pub fn embedded() -> bool {
    thread_local! {
        static EMBEDDED: bool = js::embedded();
    }
    EMBEDDED.with(|e| *e)
}

/// Both sides, as the host has them now.
pub async fn snapshot() -> Result<HostSnapshot> {
    let reply = js::request(protocol::SNAPSHOT, "{}").await?;
    Ok(serde_json::from_str(&reply)?)
}

/// The bytes of whichever of these blobs the host has. A blob it does not have
/// is simply missing from the answer.
pub async fn blobs(wants: &[(String, PathBuf)]) -> Result<Vec<(String, Vec<u8>)>> {
    let request = BlobsRequest {
        wants: wants
            .iter()
            .map(|(sha, path)| Want {
                sha: sha.clone(),
                path: path.to_string_lossy().replace('\\', "/"),
            })
            .collect(),
    };
    js::blobs(&serde_json::to_string(&request)?).await
}

/// Tell the host something, with no answer expected.
pub fn post(t: &str, body: serde_json::Value) {
    js::post(t, &body.to_string());
}

/// Two sides as the comparison the rest of the app knows how to read.
///
/// The changes are worked out here rather than asked for: a file is added,
/// deleted or modified by whether its path and hash are on each side, and a
/// deleted file whose exact contents were added somewhere else was moved.
pub fn compare_view(snap: HostSnapshot) -> CompareView {
    let repo = RepoRef {
        owner: OWNER.to_string(),
        name: if snap.name.is_empty() {
            "workspace".to_string()
        } else {
            snap.name
        },
    };
    let tree = snapshot_of(&repo, &snap.head);
    let base_tree = snapshot_of(&repo, &snap.base);
    let files = changes(&base_tree, &tree);
    CompareView {
        repo,
        base: snap.base.label,
        head: snap.head.label,
        base_sha: base_tree.commit.clone(),
        head_sha: tree.commit.clone(),
        base_tip: base_tree.commit.clone(),
        status: if files.is_empty() {
            "identical"
        } else {
            "ahead"
        }
        .to_string(),
        ahead: u32::from(!files.is_empty()),
        behind: 0,
        files,
        truncated: false,
        tree,
        base_tree,
        commits: Commits::default(),
        html_url: String::new(),
    }
}

/// One side as a snapshot: sorted, with anything that is not a plain relative
/// path left out, since a path is what the explorer and the store trust.
fn snapshot_of(repo: &RepoRef, side: &HostSide) -> Snapshot {
    let mut files: Vec<TreeEntry> = side
        .files
        .iter()
        .filter(|f| sane_path(&f.path) && !f.sha.is_empty())
        .map(|f| TreeEntry {
            path: PathBuf::from(&f.path),
            sha: f.sha.to_ascii_lowercase(),
            size: f.size,
        })
        .collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files.dedup_by(|a, b| a.path == b.path);
    Snapshot {
        repo: repo.clone(),
        commit: side.commit.clone(),
        files,
        dirs: Vec::new(),
        truncated: false,
    }
}

fn sane_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

/// What differs between two snapshots, sorted by path.
fn changes(base: &Snapshot, head: &Snapshot) -> Vec<PrFile> {
    let mut added = Vec::new();
    let mut files = Vec::new();
    for entry in &head.files {
        match base.entry(&entry.path) {
            None => added.push(entry),
            Some(old) if old.sha != entry.sha => files.push(PrFile {
                path: entry.path.clone(),
                previous_path: None,
                status: ChangeKind::Modified,
            }),
            Some(_) => {}
        }
    }
    let deleted: Vec<&TreeEntry> = base
        .files
        .iter()
        .filter(|e| head.entry(&e.path).is_none())
        .collect();

    // Moves: the same contents gone from one path and new at another. Only
    // exact ones — a diff of a file against itself under a new name is the
    // one rename worth calling one without reading anything.
    let mut gone: HashMap<&str, Vec<&TreeEntry>> = HashMap::new();
    for entry in &deleted {
        gone.entry(entry.sha.as_str()).or_default().push(entry);
    }
    let mut moved_from = std::collections::HashSet::new();
    for entry in added {
        let from = gone.get_mut(entry.sha.as_str()).and_then(Vec::pop);
        match from {
            Some(old) => {
                moved_from.insert(old.path.clone());
                files.push(PrFile {
                    path: entry.path.clone(),
                    previous_path: Some(old.path.clone()),
                    status: ChangeKind::Renamed,
                });
            }
            None => files.push(PrFile {
                path: entry.path.clone(),
                previous_path: None,
                status: ChangeKind::Added,
            }),
        }
    }
    for entry in deleted {
        if !moved_from.contains(&entry.path) {
            files.push(PrFile {
                path: entry.path.clone(),
                previous_path: None,
                status: ChangeKind::Deleted,
            });
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files
}

#[cfg(target_arch = "wasm32")]
mod js {
    use anyhow::{Result, anyhow};
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::JsFuture;

    #[wasm_bindgen(inline_js = r#"
const waiting = new Map();
let next = 1;
const framed = () => window.parent && window.parent !== window;
window.addEventListener('message', (ev) => {
  if (!framed() || ev.source !== window.parent || ev.origin !== location.origin) return;
  const m = ev.data;
  if (!m || typeof m !== 'object' || m.ps !== 1 || typeof m.id !== 'number') return;
  const w = waiting.get(m.id);
  if (!w) return;
  waiting.delete(m.id);
  if (m.error) w.reject(new Error(String(m.error)));
  else w.resolve(m.body);
});
function ask(t, body) {
  return new Promise((resolve, reject) => {
    if (!framed()) return reject(new Error('pullspace is not embedded'));
    const id = next++;
    waiting.set(id, { resolve, reject });
    window.parent.postMessage({ ps: 1, id, t, body }, location.origin);
  });
}
function decode(b64) {
  if (Uint8Array.fromBase64) return Uint8Array.fromBase64(b64);
  const s = atob(b64);
  const out = new Uint8Array(s.length);
  for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i);
  return out;
}
export function ps_embedded() {
  return framed() && new URLSearchParams(location.search).has('embed');
}
export function ps_request(t, body) {
  return ask(t, JSON.parse(body)).then((r) => JSON.stringify(r ?? null));
}
export function ps_blobs(request) {
  return ask('blobs', JSON.parse(request)).then((r) => {
    const out = [];
    for (const [sha, b64] of Object.entries(r?.blobs ?? {})) {
      if (typeof b64 === 'string') out.push([sha, decode(b64)]);
    }
    return out;
  });
}
export function ps_post(t, body) {
  if (framed()) window.parent.postMessage({ ps: 1, t, body: JSON.parse(body) }, location.origin);
}
"#)]
    extern "C" {
        fn ps_embedded() -> bool;
        fn ps_request(t: &str, body: &str) -> js_sys::Promise;
        fn ps_blobs(wants: &str) -> js_sys::Promise;
        fn ps_post(t: &str, body: &str);
    }

    fn error(e: JsValue) -> anyhow::Error {
        let text = js_sys::Reflect::get(&e, &"message".into())
            .ok()
            .and_then(|m| m.as_string())
            .or_else(|| e.as_string())
            .unwrap_or_else(|| "the host did not answer".to_string());
        anyhow!(text)
    }

    pub fn embedded() -> bool {
        ps_embedded()
    }

    pub async fn request(t: &str, body: &str) -> Result<String> {
        let reply = JsFuture::from(ps_request(t, body)).await.map_err(error)?;
        reply
            .as_string()
            .ok_or_else(|| anyhow!("the host's answer was not text"))
    }

    pub async fn blobs(wants: &str) -> Result<Vec<(String, Vec<u8>)>> {
        let reply = JsFuture::from(ps_blobs(wants)).await.map_err(error)?;
        let mut out = Vec::new();
        for pair in js_sys::Array::from(&reply).iter() {
            let pair = js_sys::Array::from(&pair);
            let Some(sha) = pair.get(0).as_string() else {
                continue;
            };
            let bytes = js_sys::Uint8Array::new(&pair.get(1)).to_vec();
            out.push((sha, bytes));
        }
        Ok(out)
    }

    pub fn post(t: &str, body: &str) {
        ps_post(t, body);
    }
}

/// Off the browser — `cargo test` — there is never a host.
#[cfg(not(target_arch = "wasm32"))]
mod js {
    use anyhow::{Result, bail};

    pub fn embedded() -> bool {
        false
    }

    pub async fn request(_: &str, _: &str) -> Result<String> {
        bail!("no host outside a browser")
    }

    pub async fn blobs(_: &str) -> Result<Vec<(String, Vec<u8>)>> {
        bail!("no host outside a browser")
    }

    pub fn post(_: &str, _: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use pullspace_embed::protocol::Entry as HostEntry;

    fn entry(path: &str, sha: char, size: u64) -> HostEntry {
        HostEntry {
            path: path.to_string(),
            sha: sha.to_string().repeat(40),
            size,
        }
    }

    fn side(label: &str, commit: &str, files: Vec<HostEntry>) -> HostSide {
        HostSide {
            label: label.to_string(),
            commit: commit.to_string(),
            files,
        }
    }

    fn statuses(view: &CompareView) -> Vec<(String, ChangeKind, Option<String>)> {
        view.files
            .iter()
            .map(|f| {
                (
                    f.path.display().to_string(),
                    f.status,
                    f.previous_path.as_ref().map(|p| p.display().to_string()),
                )
            })
            .collect()
    }

    #[test]
    fn a_working_tree_is_read_as_a_comparison_against_head() {
        let view = compare_view(HostSnapshot {
            name: "tamale".into(),
            base: side(
                "HEAD",
                &"f".repeat(40),
                vec![
                    entry("src/a.rs", 'a', 10),
                    entry("src/gone.rs", 'b', 20),
                    entry("old/name.rs", 'c', 30),
                    entry("same.txt", 'd', 5),
                ],
            ),
            head: side(
                "working tree",
                "worktree",
                vec![
                    entry("src/a.rs", 'e', 12),
                    entry("new/name.rs", 'c', 30),
                    entry("same.txt", 'd', 5),
                    entry("src/new.rs", '1', 7),
                ],
            ),
        });
        assert_eq!(view.repo.owner, OWNER);
        assert_eq!(view.repo.name, "tamale");
        assert_eq!(
            (view.base.as_str(), view.head.as_str()),
            ("HEAD", "working tree")
        );
        assert_eq!(view.head_sha, "worktree");
        assert_eq!(
            statuses(&view),
            vec![
                (
                    "new/name.rs".into(),
                    ChangeKind::Renamed,
                    Some("old/name.rs".into())
                ),
                ("src/a.rs".into(), ChangeKind::Modified, None),
                ("src/gone.rs".into(), ChangeKind::Deleted, None),
                ("src/new.rs".into(), ChangeKind::Added, None),
            ]
        );
        assert!(!view.is_empty());
        // Both trees are whole, so the explorer shows the checkout and not
        // just the changes.
        assert_eq!(view.tree.files.len(), 4);
        assert!(view.tree.entry(std::path::Path::new("same.txt")).is_some());
    }

    #[test]
    fn nothing_changed_is_an_identical_comparison() {
        let files = vec![entry("a", 'a', 1)];
        let view = compare_view(HostSnapshot {
            name: String::new(),
            base: side("HEAD", "x", files.clone()),
            head: side("working tree", "y", files),
        });
        assert!(view.files.is_empty());
        assert!(view.is_empty());
        assert_eq!(view.repo.name, "workspace");
    }

    #[test]
    fn paths_that_climb_out_are_dropped_and_the_rest_are_sorted() {
        let view = compare_view(HostSnapshot {
            name: "x".into(),
            base: HostSide::default(),
            head: side(
                "working tree",
                "w",
                vec![
                    entry("z.rs", 'a', 1),
                    entry("../etc/passwd", 'b', 1),
                    entry("/abs", 'c', 1),
                    entry("a//b", 'd', 1),
                    entry("a.rs", 'e', 1),
                ],
            ),
        });
        let paths: Vec<_> = view.tree.paths().map(|p| p.display().to_string()).collect();
        assert_eq!(paths, vec!["a.rs", "z.rs"]);
        assert_eq!(
            view.files.len(),
            2,
            "everything is new against an empty base"
        );
    }

    #[test]
    fn the_host_answer_parses() {
        let text = r#"{"name":"r","base":{"label":"HEAD","commit":"c","files":[{"path":"a","sha":"ab","size":3}]},
                       "head":{"label":"working tree","commit":"w","files":[]}}"#;
        let snap: HostSnapshot = serde_json::from_str(text).unwrap();
        assert_eq!(snap.base.files[0].size, 3);
    }
}
