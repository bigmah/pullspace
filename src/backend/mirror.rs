//! Reviewing a pull request out of a real repository on disk.
//!
//! Fetching one PR ref into a clone that already exists costs about a second
//! and no meaningful disk, after which every file read is local instead of an
//! HTTP round trip. Repositories the user does not already have are mirrored
//! under the cache directory instead, bounded by a total size cap.
//!
//! [`materialize`] then writes the pull request's head commit out as ordinary
//! files. That is what lets search, the symbol index and anything else that
//! walks a directory treat a pull request exactly like a local checkout —
//! they take a path, and a checkout is a path.
//!
//! Network git goes through the `git` CLI rather than git2, because this
//! crate's git2 is built without the `https` feature — enabling it would pull
//! in `openssl-sys` and its build-time headaches, and anyone reviewing pull
//! requests already has `git`. Reading objects still uses git2.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::github::{repo_from_remote, RepoRef};
use super::FileContent;

/// Everything we write lives under this namespace, so borrowing someone's
/// clone can never collide with their own branches or tags. We never touch
/// HEAD, the index, or the working tree.
const REF_NS: &str = "refs/pullspace";

/// Total ceiling for mirrored repositories and checked-out commits. Override
/// with `PULLSPACE_CACHE_MAX_GB`. Clones the user already had are never counted
/// against this, and never evicted — they are not ours to delete.
const DEFAULT_CAP_GB: u64 = 5;

/// Checkouts kept per repository. Reviewing tends to circle a handful of pull
/// requests in the same repository, so keeping a few means switching back is
/// free; keeping every commit ever reviewed would not be.
const CHECKOUTS_PER_REPO: usize = 3;

/// GitHub treats owner and repo names case-insensitively.
fn same_repo(a: &RepoRef, b: &RepoRef) -> bool {
    a.owner.eq_ignore_ascii_case(&b.owner) && a.name.eq_ignore_ascii_case(&b.name)
}

pub fn cache_root() -> Option<PathBuf> {
    if let Ok(x) = std::env::var("XDG_CACHE_HOME")
        && !x.is_empty()
    {
        return Some(PathBuf::from(x).join("pullspace"));
    }
    #[cfg(windows)]
    {
        dirs::cache_dir().map(|d| d.join("pullspace"))
    }
    #[cfg(not(windows))]
    {
        dirs::home_dir().map(|h| h.join(".cache").join("pullspace"))
    }
}

/// Owner and repo names are restricted to `[A-Za-z0-9._-]` by GitHub, but this
/// is building a filesystem path from remote input, so be sure.
fn safe(component: &str) -> String {
    component
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect()
}

/// Directory name for a repository. Lower-cased for the same reason
/// [`same_repo`] ignores case: GitHub does, so `DioxusLabs/dioxus` and
/// `dioxuslabs/dioxus` must land on one directory — and on the one index key
/// that [`dir_for`] maps back to it.
fn slug(repo: &RepoRef) -> String {
    format!("{}__{}", safe(&repo.owner), safe(&repo.name)).to_lowercase()
}

fn mirror_dir(repo: &RepoRef) -> Option<PathBuf> {
    cache_root().map(|r| r.join("repos").join(format!("{}.git", slug(repo))))
}

fn clone_url(repo: &RepoRef) -> String {
    format!("https://github.com/{}/{}.git", repo.owner, repo.name)
}

fn checkouts_root(repo: &RepoRef) -> Option<PathBuf> {
    cache_root().map(|r| r.join("checkouts").join(slug(repo)))
}

/// Where one commit's files live. Keyed by commit, so pushing to a pull
/// request under review produces a new checkout rather than a stale one.
fn checkout_dir(repo: &RepoRef, sha: &str) -> Option<PathBuf> {
    checkouts_root(repo).map(|r| r.join(safe(sha)))
}

// --------------------------------------------------------------- git runner

/// Run git, returning stdout.
///
/// The token is handed over in the environment and referenced by name, so it
/// never appears in the process's arguments where any other process could read
/// it out of `ps`.
fn git(args: &[&str], token: &str) -> Result<String> {
    let mut cmd = Command::new("git");
    // A GUI app must never block on an interactive credential prompt.
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    if !token.is_empty() {
        cmd.env("PULLSPACE_GIT_AUTH", format!("Authorization: Bearer {token}"));
        cmd.arg("--config-env=http.extraHeader=PULLSPACE_GIT_AUTH");
    }
    cmd.args(args);

    let out = cmd
        .output()
        .context("running `git` — is it installed and on PATH?")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("git {}: {}", args.join(" "), err.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

// ------------------------------------------------------------ local lookup

/// Is the repository at `root` a clone of `repo`?
pub fn is_clone_of(root: &Path, repo: &RepoRef) -> bool {
    let Ok(r) = git2::Repository::discover(root) else {
        return false;
    };
    let Ok(names) = r.remotes() else {
        return false;
    };
    names
        .iter()
        .filter_map(|n| n.ok().flatten())
        .filter_map(|n| r.find_remote(n).ok())
        .filter_map(|rm| rm.url().ok().and_then(repo_from_remote))
        .any(|found: RepoRef| same_repo(&found, repo))
}

/// The `.git` directory backing a working copy.
fn git_dir_of(root: &Path) -> Result<PathBuf> {
    let repo = git2::Repository::discover(root)?;
    Ok(repo.path().to_path_buf())
}

fn has_commit(git_dir: &Path, sha: &str) -> bool {
    let Ok(repo) = git2::Repository::open(git_dir) else {
        return false;
    };
    let Ok(oid) = git2::Oid::from_str(sha) else {
        return false;
    };
    repo.find_commit(oid).is_ok()
}

/// Where a pull request's objects can be read from.
#[derive(Clone, PartialEq, Debug)]
pub struct LocalRepo {
    pub git_dir: PathBuf,
    /// True when this is a clone the user already had, rather than our mirror.
    pub borrowed: bool,
}

/// A ref on the remote, and the leaf under [`REF_NS`] it is parked at locally.
struct Wanted {
    src: String,
    leaf: String,
}

/// A pull request's head. Fetching it also brings its whole ancestry, which
/// includes the merge base — so one fetch makes both sides of the diff readable.
fn pr_ref(repo: &RepoRef, number: u64) -> Wanted {
    Wanted {
        src: format!("refs/pull/{number}/head"),
        leaf: format!("{}/{}/pr/{number}", safe(&repo.owner), safe(&repo.name)),
    }
}

/// A branch, for browsing a repository with no pull request involved.
fn branch_ref(repo: &RepoRef, branch: &str) -> Wanted {
    Wanted {
        src: format!("refs/heads/{branch}"),
        leaf: format!(
            "{}/{}/branch/{}",
            safe(&repo.owner),
            safe(&repo.name),
            safe(branch),
        ),
    }
}

/// Make a pull request's `sha` readable from a repository on disk.
pub fn prepare(
    repo: &RepoRef,
    number: u64,
    sha: &str,
    token: &str,
    own_clone: Option<&Path>,
) -> Result<LocalRepo> {
    prepare_ref(repo, &pr_ref(repo, number), sha, token, own_clone)
}

/// [`prepare`] for a branch tip: what opening a repository on its own needs.
pub fn prepare_branch(
    repo: &RepoRef,
    branch: &str,
    sha: &str,
    token: &str,
    own_clone: Option<&Path>,
) -> Result<LocalRepo> {
    prepare_ref(repo, &branch_ref(repo, branch), sha, token, own_clone)
}

/// Make `sha` readable from a repository on disk, fetching `want` if it is not
/// there already. Prefers a clone the user already has.
fn prepare_ref(
    repo: &RepoRef,
    want: &Wanted,
    sha: &str,
    token: &str,
    own_clone: Option<&Path>,
) -> Result<LocalRepo> {
    if let Some(root) = own_clone
        && is_clone_of(root, repo)
        && let Ok(git_dir) = git_dir_of(root)
    {
        ensure_commit(&git_dir, repo, want, sha, token)?;
        return Ok(LocalRepo {
            git_dir,
            borrowed: true,
        });
    }

    let dir = mirror_dir(repo).ok_or_else(|| anyhow!("no cache directory available"))?;
    if !dir.join("HEAD").exists() {
        // Make room before taking more, not after.
        evict_to_fit(&[mirror_key(repo)]);
        clone_mirror(repo, &dir, token)?;
    }
    ensure_commit(&dir, repo, want, sha, token)?;
    touch(repo, &dir);
    Ok(LocalRepo {
        git_dir: dir,
        borrowed: false,
    })
}

fn clone_mirror(repo: &RepoRef, dir: &Path, token: &str) -> Result<()> {
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let dir_s = dir.to_string_lossy().into_owned();
    let url = clone_url(repo);
    let res = git(&["clone", "--bare", "--quiet", &url, &dir_s], token);
    if res.is_err() {
        // Never leave a half-written mirror behind to be trusted later.
        let _ = std::fs::remove_dir_all(dir);
    }
    res.map(|_| ())
}

fn ensure_commit(
    git_dir: &Path,
    repo: &RepoRef,
    want: &Wanted,
    sha: &str,
    token: &str,
) -> Result<()> {
    if has_commit(git_dir, sha) {
        return Ok(());
    }
    let dir_s = git_dir.to_string_lossy().into_owned();
    let url = clone_url(repo);
    let refspec = format!("+{}:{REF_NS}/{}", want.src, want.leaf);
    git(
        &["-C", &dir_s, "fetch", "--no-tags", "--quiet", &url, &refspec],
        token,
    )?;
    if !has_commit(git_dir, sha) {
        bail!("fetched {} but commit {sha} is still missing", want.src);
    }
    Ok(())
}

// ---------------------------------------------------------------- reading

/// One side of a file, read straight from the object database.
pub fn blob_at(git_dir: &Path, sha: &str, rel: &Path) -> Result<FileContent> {
    let repo = git2::Repository::open(git_dir)?;
    let commit = repo.find_commit(git2::Oid::from_str(sha)?)?;
    let tree = commit.tree()?;
    // Not in this commit — expected for the base side of an added file.
    let Ok(entry) = tree.get_path(rel) else {
        return Ok(FileContent::Absent);
    };
    let obj = entry.to_object(&repo)?;
    let Some(blob) = obj.as_blob() else {
        return Ok(FileContent::Absent);
    };
    Ok(if blob.is_binary() {
        FileContent::Binary
    } else {
        FileContent::Text(String::from_utf8_lossy(blob.content()).into_owned())
    })
}

/// Every file in a tree, with the blob to read it from.
///
/// Blobs only: `tree` entries are directories, and `commit` entries are
/// submodules, whose objects live in another repository entirely. Both the
/// explorer's file list and the checkout are built from this, so they cannot
/// disagree about what the repository contains.
fn blob_entries(tree: &git2::Tree) -> Vec<(String, git2::Oid)> {
    let mut out = Vec::new();
    let _ = tree.walk(git2::TreeWalkMode::PreOrder, |dir, entry| {
        if entry.kind() == Some(git2::ObjectType::Blob)
            && let Ok(name) = entry.name()
        {
            out.push((format!("{dir}{name}"), entry.id()));
        }
        git2::TreeWalkResult::Ok
    });
    out
}

/// Every file in the repository at `sha`. Unlike the API's tree endpoint this
/// has no size limit, so large repositories list in full.
pub fn tree_paths(git_dir: &Path, sha: &str) -> Result<Vec<PathBuf>> {
    let repo = git2::Repository::open(git_dir)?;
    let commit = repo.find_commit(git2::Oid::from_str(sha)?)?;
    Ok(blob_entries(&commit.tree()?)
        .into_iter()
        .map(|(path, _)| PathBuf::from(path))
        .collect())
}

// ------------------------------------------------------------- materialising

/// Join a path that came out of the object database onto `base`, refusing
/// anything that could climb out of it. Git will not put `..` or an absolute
/// path in a tree, but this writes data fetched from a remote onto disk.
fn safe_join(base: &Path, rel: &str) -> Option<PathBuf> {
    let mut out = base.to_path_buf();
    for part in rel.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.contains('\\') {
            return None;
        }
        out.push(part);
    }
    Some(out)
}

/// Write the tree at `sha` into `dest` as ordinary files.
///
/// The blobs are written out directly rather than through libgit2's checkout,
/// which insists on resolving submodules and needs a working tree to do it —
/// so it refuses on a bare mirror, which is precisely what a repository the
/// user does not already have gets. Nothing here needs submodules resolved:
/// they are separate repositories whose objects a mirror does not have, and a
/// pull request diffs the gitlink, never their contents. They are skipped.
///
/// Writing blobs also means no index is opened at all, so pointing this at a
/// clone the user already had cannot disturb it, and the bytes on disk are the
/// blob's own — no filters — matching what the diff views show. The executable
/// bit is not reproduced; nothing that reads a checkout cares.
///
/// Written to a sibling temporary directory and renamed into place, so an
/// interrupted checkout is never left behind looking complete.
fn checkout_into(git_dir: &Path, sha: &str, dest: &Path) -> Result<()> {
    let (parent, name) = dest
        .parent()
        .zip(dest.file_name())
        .ok_or_else(|| anyhow!("{} is not a usable checkout path", dest.display()))?;
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".{}.partial", name.to_string_lossy()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;

    let write_all = || -> Result<()> {
        let repo = git2::Repository::open(git_dir)?;
        let commit = repo.find_commit(git2::Oid::from_str(sha)?)?;
        for (rel, oid) in blob_entries(&commit.tree()?) {
            let Some(path) = safe_join(&tmp, &rel) else {
                continue;
            };
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let blob = repo.find_blob(oid)?;
            std::fs::write(&path, blob.content())
                .with_context(|| format!("writing {rel}"))?;
        }
        Ok(())
    };
    if let Err(e) = write_all() {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e).with_context(|| format!("checking out {sha}"));
    }
    if let Err(e) = std::fs::rename(&tmp, dest) {
        let _ = std::fs::remove_dir_all(&tmp);
        // Losing the race with another checkout of the same commit is fine —
        // the directory it left is the one we were about to write.
        if !dest.is_dir() {
            return Err(e).with_context(|| format!("moving the checkout into {}", dest.display()));
        }
    }
    Ok(())
}

/// A directory holding the repository at `sha`, checked out on demand.
///
/// Everything that walks a working tree — search, the symbol index — takes a
/// path, so handing it this makes a pull request behave like a local
/// repository. Repeat visits reuse the checkout.
pub fn materialize(git_dir: &Path, repo: &RepoRef, sha: &str) -> Result<PathBuf> {
    let dest = checkout_dir(repo, sha).ok_or_else(|| anyhow!("no cache directory available"))?;
    let key = checkout_key(repo, sha);
    if dest.is_dir() {
        touch_checkout(&key, &dest);
        return Ok(dest);
    }
    // Make room before taking more, but never at the expense of the mirror
    // this checkout is about to come out of.
    evict_to_fit(&[key.clone(), mirror_key(repo)]);
    checkout_into(git_dir, sha, &dest)?;
    touch_checkout(&key, &dest);
    prune_checkouts(repo);
    Ok(dest)
}

// ------------------------------------------------------------ cache budget

#[derive(Default, Serialize, Deserialize)]
struct Index {
    /// Bare mirrors, keyed `owner/repo`.
    #[serde(default)]
    repos: HashMap<String, Entry>,
    /// Checked-out commits, keyed `owner/repo@sha`. Counted against the same
    /// cap as the mirrors — a checkout of a large repository is not small.
    #[serde(default)]
    checkouts: HashMap<String, Entry>,
}

impl Index {
    fn total(&self) -> u64 {
        self.repos
            .values()
            .chain(self.checkouts.values())
            .map(|e| e.bytes)
            .sum()
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    last_used: u64,
    bytes: u64,
}

fn mirror_key(repo: &RepoRef) -> String {
    repo.to_string().to_lowercase()
}

fn checkout_key(repo: &RepoRef, sha: &str) -> String {
    format!("{}@{}", mirror_key(repo), sha)
}

/// The directory an index key stands for — the inverse of the two above.
fn dir_for(key: &str) -> Option<PathBuf> {
    let (repo_part, sha) = match key.split_once('@') {
        Some((repo, sha)) => (repo, Some(sha)),
        None => (key, None),
    };
    let (owner, name) = repo_part.split_once('/')?;
    let repo = RepoRef {
        owner: owner.to_string(),
        name: name.to_string(),
    };
    match sha {
        Some(sha) => checkout_dir(&repo, sha),
        None => mirror_dir(&repo),
    }
}

fn index_path() -> Option<PathBuf> {
    cache_root().map(|r| r.join("index.json"))
}

fn load_index() -> Index {
    index_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_index(idx: &Index) {
    let Some(path) = index_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(body) = serde_json::to_string_pretty(idx) {
        let _ = std::fs::write(path, body);
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map(|m| m.len()).unwrap_or(0),
            _ => 0,
        })
        .sum()
}

fn cap_bytes() -> u64 {
    let gb = std::env::var("PULLSPACE_CACHE_MAX_GB")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_CAP_GB);
    gb * 1024 * 1024 * 1024
}

fn record(map: &mut HashMap<String, Entry>, key: String, dir: &Path) {
    map.insert(
        key,
        Entry {
            last_used: now(),
            bytes: dir_size(dir),
        },
    );
}

fn touch(repo: &RepoRef, dir: &Path) {
    let mut idx = load_index();
    record(&mut idx.repos, mirror_key(repo), dir);
    save_index(&idx);
}

fn touch_checkout(key: &str, dir: &Path) {
    let mut idx = load_index();
    match idx.checkouts.get_mut(key) {
        // A checkout is one fixed commit, so its size cannot have changed —
        // reopening a pull request should not re-walk the whole directory to
        // learn that. Only the recency moves.
        Some(entry) => entry.last_used = now(),
        None => record(&mut idx.checkouts, key.to_string(), dir),
    }
    save_index(&idx);
}

/// Drop least-recently-used entries until the cache is under its cap.
///
/// Checkouts go before mirrors: replacing a checkout is a local operation
/// against the mirror beside it, where replacing a mirror is a download.
/// Anything in `protect` is left alone — that is what the caller is about to
/// use, and evicting it would be undone immediately.
fn evict_to_fit(protect: &[String]) {
    let cap = cap_bytes();
    let mut idx = load_index();
    let mut total = idx.total();
    if total <= cap {
        return;
    }

    // (is_mirror, last_used) orders checkouts ahead of mirrors, each oldest
    // first.
    let mut victims: Vec<(bool, u64, String, u64)> = idx
        .checkouts
        .iter()
        .map(|(k, e)| (false, e.last_used, k.clone(), e.bytes))
        .chain(idx.repos.iter().map(|(k, e)| (true, e.last_used, k.clone(), e.bytes)))
        .filter(|(_, _, key, _)| !protect.contains(key))
        .collect();
    victims.sort_by_key(|(is_mirror, last_used, _, _)| (*is_mirror, *last_used));

    for (is_mirror, _, key, bytes) in victims {
        if total <= cap {
            break;
        }
        if let Some(dir) = dir_for(&key) {
            let _ = std::fs::remove_dir_all(&dir);
        }
        if is_mirror {
            idx.repos.remove(&key);
        } else {
            idx.checkouts.remove(&key);
        }
        total = total.saturating_sub(bytes);
    }
    save_index(&idx);
}

/// Keep only the most recently used checkouts of one repository, so circling a
/// few pull requests is free while a long review session is still bounded.
fn prune_checkouts(repo: &RepoRef) {
    let prefix = format!("{}@", mirror_key(repo));
    let mut idx = load_index();
    let mut mine: Vec<(String, u64)> = idx
        .checkouts
        .iter()
        .filter(|(k, _)| k.starts_with(&prefix))
        .map(|(k, e)| (k.clone(), e.last_used))
        .collect();
    if mine.len() <= CHECKOUTS_PER_REPO {
        return;
    }
    mine.sort_by(|a, b| b.1.cmp(&a.1)); // newest first
    for (key, _) in mine.split_off(CHECKOUTS_PER_REPO) {
        if let Some(dir) = dir_for(&key) {
            let _ = std::fs::remove_dir_all(&dir);
        }
        idx.checkouts.remove(&key);
    }
    save_index(&idx);
}

/// What the cache is holding, for the readout in the GitHub panel.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct CacheStats {
    pub bytes: u64,
    pub repos: usize,
    pub checkouts: usize,
}

impl CacheStats {
    pub fn is_empty(&self) -> bool {
        self.repos == 0 && self.checkouts == 0
    }
}

pub fn cache_stats() -> CacheStats {
    let idx = load_index();
    CacheStats {
        bytes: idx.total(),
        repos: idx.repos.len(),
        checkouts: idx.checkouts.len(),
    }
}

/// Delete every mirrored repository and checkout. Clones the user already had
/// are never part of the cache, so this cannot touch their work.
pub fn clear_cache() -> Result<()> {
    let Some(root) = cache_root() else {
        return Ok(());
    };
    for (dir, what) in [
        (root.join("repos"), "mirrored repositories"),
        (root.join("checkouts"), "checked-out commits"),
    ] {
        if dir.exists() {
            std::fs::remove_dir_all(&dir).with_context(|| format!("removing {what}"))?;
        }
    }
    if let Some(p) = index_path() {
        let _ = std::fs::remove_file(p);
    }
    Ok(())
}

/// Human-readable size, for the cache readout.
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(owner: &str, name: &str) -> RepoRef {
        RepoRef {
            owner: owner.to_string(),
            name: name.to_string(),
        }
    }

    #[test]
    fn repo_matching_ignores_case() {
        assert!(same_repo(&repo("DioxusLabs", "dioxus"), &repo("dioxuslabs", "Dioxus")));
        assert!(!same_repo(&repo("a", "b"), &repo("a", "c")));
    }

    #[test]
    fn path_components_cannot_escape_the_cache() {
        // Nothing from a remote name may introduce a separator or traversal.
        assert_eq!(safe("../../etc"), "------etc");
        assert_eq!(safe("owner/name"), "owner-name");
        assert_eq!(safe("normal-repo_1.0"), "normal-repo_1-0");
    }

    #[test]
    fn mirror_paths_are_scoped_per_repo() {
        let a = mirror_dir(&repo("o", "a"));
        let b = mirror_dir(&repo("o", "b"));
        assert_ne!(a, b);
        if let Some(p) = a {
            assert!(p.to_string_lossy().ends_with("o__a.git"));
        }
    }

    #[test]
    fn clone_urls_are_https() {
        assert_eq!(
            clone_url(&repo("DioxusLabs", "dioxus")),
            "https://github.com/DioxusLabs/dioxus.git"
        );
    }

    #[test]
    fn our_refs_are_namespaced() {
        // A borrowed clone's own branches must be untouchable.
        assert!(REF_NS.starts_with("refs/"));
        assert!(!REF_NS.starts_with("refs/heads"));
        assert!(!REF_NS.starts_with("refs/remotes"));
        assert!(!REF_NS.starts_with("refs/tags"));
    }

    #[test]
    fn pull_requests_and_branches_land_on_different_refs() {
        let r = repo("Owner", "Name");
        let pr = pr_ref(&r, 7);
        let branch = branch_ref(&r, "main");
        assert_eq!(pr.src, "refs/pull/7/head");
        assert_eq!(branch.src, "refs/heads/main");
        // Parking a branch must not overwrite a pull request of the same repo.
        assert_ne!(pr.leaf, branch.leaf);
        // A branch name with a slash is fetched as written, but flattened on
        // the way into our namespace so it cannot add levels of its own.
        let nested = branch_ref(&r, "feat/thing");
        assert_eq!(nested.src, "refs/heads/feat/thing");
        assert_eq!(nested.leaf, "Owner/Name/branch/feat-thing");
    }

    #[test]
    fn missing_commit_is_not_a_panic() {
        assert!(!has_commit(Path::new("/nonexistent"), "deadbeef"));
    }

    #[test]
    fn index_keys_round_trip_to_directories() {
        let r = repo("Owner", "Name");
        assert_eq!(dir_for(&mirror_key(&r)), mirror_dir(&r));
        assert_eq!(dir_for(&checkout_key(&r, "abc123")), checkout_dir(&r, "abc123"));
        // A checkout and its mirror are distinct entries.
        assert_ne!(mirror_key(&r), checkout_key(&r, "abc123"));
        assert_eq!(dir_for("nonsense"), None);
    }

    /// Checks out this very repository, which is the cheapest real git
    /// repository to hand — and proves the source is left alone.
    #[test]
    fn checkout_materialises_files_without_touching_the_source() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let repo = git2::Repository::discover(root).expect("pullspace is a git repository");
        let git_dir = repo.path().to_path_buf();
        let head = repo
            .head()
            .and_then(|h| h.peel_to_commit())
            .expect("HEAD commit")
            .id()
            .to_string();

        let index_before = std::fs::metadata(git_dir.join("index")).and_then(|m| m.modified());
        let dest = std::env::temp_dir().join(format!("pullspace-checkout-test-{head}"));
        let _ = std::fs::remove_dir_all(&dest);

        checkout_into(&git_dir, &head, &dest).expect("checkout");
        assert!(dest.join("Cargo.toml").is_file(), "tracked files are written");
        assert!(dest.join("src/main.rs").is_file());
        // Plain files, so the ignore walker sees a directory like any other.
        assert!(!dest.join(".git").exists(), "no repository inside the checkout");
        // And nothing was left half-written next to it.
        assert!(!dest.with_file_name(format!(".{head}.partial")).exists());

        let index_after = std::fs::metadata(git_dir.join("index")).and_then(|m| m.modified());
        assert_eq!(
            index_before.ok(),
            index_after.ok(),
            "the source repository's index must not be written"
        );

        // Checking out again over an existing directory is a no-op, not an error.
        checkout_into(&git_dir, &head, &dest).expect("second checkout");
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn checkout_paths_cannot_escape_the_destination() {
        let base = Path::new("/tmp/dest");
        assert_eq!(safe_join(base, "src/main.rs"), Some(base.join("src/main.rs")));
        assert_eq!(safe_join(base, "../../etc/passwd"), None);
        assert_eq!(safe_join(base, "a/../b"), None);
        assert_eq!(safe_join(base, "a//b"), None);
        assert_eq!(safe_join(base, r"a\b"), None);
    }

    /// The failure this was written for: libgit2's own checkout refuses a tree
    /// containing a submodule unless the repository has a working tree, and a
    /// bare mirror — what every repository the user does not already have gets
    /// — never does. Blobs are written directly so a submodule is simply not
    /// its business.
    #[test]
    fn trees_with_submodules_check_out_of_a_bare_mirror() {
        let dir = std::env::temp_dir().join("pullspace-submodule-test.git");
        let _ = std::fs::remove_dir_all(&dir);
        let repo = git2::Repository::init_bare(&dir).expect("bare repository");
        let sig = git2::Signature::now("pullspace", "test@example.invalid").unwrap();

        let readme = repo.blob(b"# hello\n").unwrap();
        let modules = repo
            .blob(b"[submodule \"vendor\"]\n\tpath = vendor\n\turl = ../other\n")
            .unwrap();

        // Something for the gitlink to point at. The point is that whatever it
        // names, its *contents* are not in this repository — as they are not
        // in a real mirror.
        let empty = repo.treebuilder(None).unwrap();
        let empty_tree = repo.find_tree(empty.write().unwrap()).unwrap();
        let submodule = repo
            .commit(None, &sig, &sig, "submodule", &empty_tree, &[])
            .unwrap();

        let mut tb = repo.treebuilder(None).unwrap();
        let blob = i32::from(git2::FileMode::Blob);
        tb.insert("README.md", readme, blob).unwrap();
        tb.insert(".gitmodules", modules, blob).unwrap();
        tb.insert("vendor", submodule, i32::from(git2::FileMode::Commit))
            .unwrap();
        let tree = repo.find_tree(tb.write().unwrap()).unwrap();
        let head = repo
            .commit(None, &sig, &sig, "root", &tree, &[])
            .unwrap()
            .to_string();

        let dest = std::env::temp_dir().join("pullspace-submodule-checkout");
        let _ = std::fs::remove_dir_all(&dest);
        checkout_into(&dir, &head, &dest).expect("a submodule must not stop the checkout");
        assert!(dest.join("README.md").is_file(), "the repository's own files");
        assert!(
            !dest.join("vendor").exists(),
            "the submodule is skipped, not faked up as an empty directory"
        );
        // And the explorer's file list agrees about what is in there.
        let listed = tree_paths(&dir, &head).unwrap();
        assert!(listed.contains(&PathBuf::from("README.md")));
        assert!(!listed.iter().any(|p| p == Path::new("vendor")));

        let _ = std::fs::remove_dir_all(&dest);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A repository the user does not already have is mirrored bare, which has
    /// no working directory of its own to check out into.
    #[test]
    fn bare_mirrors_can_be_checked_out() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let repo = git2::Repository::discover(root).expect("pullspace is a git repository");
        let head = repo
            .head()
            .and_then(|h| h.peel_to_commit())
            .expect("HEAD commit")
            .id()
            .to_string();

        // Cloned from the local path, so this stays off the network.
        let bare = std::env::temp_dir().join("pullspace-bare-mirror-test.git");
        let _ = std::fs::remove_dir_all(&bare);
        let out = Command::new("git")
            .args([
                "clone",
                "--bare",
                "--quiet",
                &root.to_string_lossy(),
                &bare.to_string_lossy(),
            ])
            .output()
            .expect("running git");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

        let dest = std::env::temp_dir().join("pullspace-bare-checkout-test");
        let _ = std::fs::remove_dir_all(&dest);
        checkout_into(&bare, &head, &dest).expect("checking out of a bare mirror");
        assert!(dest.join("Cargo.toml").is_file(), "bare mirrors materialise too");

        let _ = std::fs::remove_dir_all(&dest);
        let _ = std::fs::remove_dir_all(&bare);
    }

    /// The point of checking out at all: the tools that walk a working tree
    /// cannot tell a checkout from the repository it came from.
    #[test]
    fn a_checkout_is_searchable_and_indexable() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let repo = git2::Repository::discover(root).expect("pullspace is a git repository");
        let head = repo
            .head()
            .and_then(|h| h.peel_to_commit())
            .expect("HEAD commit")
            .id()
            .to_string();
        let dest = std::env::temp_dir().join(format!("pullspace-scan-test-{head}"));
        let _ = std::fs::remove_dir_all(&dest);
        checkout_into(repo.path(), &head, &dest).expect("checkout");

        let re = crate::backend::search::text_query("fn main").expect("query");
        let hits = crate::backend::search::search_repo(&dest, &re);
        assert!(
            hits.iter().any(|h| h.path == Path::new("src/main.rs")),
            "search finds main() in the checkout: {hits:?}",
            hits = hits.iter().map(|h| h.path.display().to_string()).collect::<Vec<_>>(),
        );

        let index = crate::backend::symbols::build_index(&dest);
        let defs = crate::backend::symbols::find_definitions(&index, "main");
        assert!(!defs.is_empty(), "the symbol index sees the checkout's code");
        // Paths are repo-relative, so a hit maps straight onto the PR's tree.
        assert!(defs.iter().all(|d| d.path.is_relative()));

        let _ = std::fs::remove_dir_all(&dest);
    }
}
