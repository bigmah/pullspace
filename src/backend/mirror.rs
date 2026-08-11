//! Reviewing a pull request out of a real repository on disk.
//!
//! Fetching one PR ref into a clone that already exists costs about a second
//! and no meaningful disk, after which every file read is local instead of an
//! HTTP round trip. Repositories the user does not already have are mirrored
//! under the cache directory instead, bounded by a total size cap.
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

/// Total ceiling for mirrored repositories. Override with
/// `PULLSPACE_CACHE_MAX_GB`. Clones the user already had are never counted
/// against this, and never evicted — they are not ours to delete.
const DEFAULT_CAP_GB: u64 = 5;

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

fn mirror_dir(repo: &RepoRef) -> Option<PathBuf> {
    cache_root().map(|r| {
        r.join("repos")
            .join(format!("{}__{}.git", safe(&repo.owner), safe(&repo.name)))
    })
}

fn clone_url(repo: &RepoRef) -> String {
    format!("https://github.com/{}/{}.git", repo.owner, repo.name)
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

/// Make `sha` readable from a repository on disk.
///
/// Prefers a clone the user already has. Fetching `pull/N/head` also brings
/// its whole ancestry, which includes the merge base — so one fetch makes both
/// sides of the diff readable.
pub fn prepare(
    repo: &RepoRef,
    number: u64,
    sha: &str,
    token: &str,
    own_clone: Option<&Path>,
) -> Result<LocalRepo> {
    if let Some(root) = own_clone
        && is_clone_of(root, repo)
        && let Ok(git_dir) = git_dir_of(root)
    {
        ensure_commit(&git_dir, repo, number, sha, token)?;
        return Ok(LocalRepo {
            git_dir,
            borrowed: true,
        });
    }

    let dir = mirror_dir(repo).ok_or_else(|| anyhow!("no cache directory available"))?;
    if !dir.join("HEAD").exists() {
        // Make room before taking more, not after.
        evict_to_fit();
        clone_mirror(repo, &dir, token)?;
    }
    ensure_commit(&dir, repo, number, sha, token)?;
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
    number: u64,
    sha: &str,
    token: &str,
) -> Result<()> {
    if has_commit(git_dir, sha) {
        return Ok(());
    }
    let dir_s = git_dir.to_string_lossy().into_owned();
    let url = clone_url(repo);
    let refspec = format!(
        "+refs/pull/{number}/head:{REF_NS}/{}/{}/pr/{number}",
        safe(&repo.owner),
        safe(&repo.name),
    );
    git(
        &["-C", &dir_s, "fetch", "--no-tags", "--quiet", &url, &refspec],
        token,
    )?;
    if !has_commit(git_dir, sha) {
        bail!("fetched pull request #{number} but commit {sha} is still missing");
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

/// Every file in the repository at `sha`. Unlike the API's tree endpoint this
/// has no size limit, so large repositories list in full.
pub fn tree_paths(git_dir: &Path, sha: &str) -> Result<Vec<PathBuf>> {
    let repo = git2::Repository::open(git_dir)?;
    let commit = repo.find_commit(git2::Oid::from_str(sha)?)?;
    let mut out = Vec::new();
    commit
        .tree()?
        .walk(git2::TreeWalkMode::PreOrder, |dir, entry| {
            if entry.kind() == Some(git2::ObjectType::Blob)
                && let Ok(name) = entry.name()
            {
                out.push(PathBuf::from(format!("{dir}{name}")));
            }
            git2::TreeWalkResult::Ok
        })?;
    Ok(out)
}

// ------------------------------------------------------------ cache budget

#[derive(Default, Serialize, Deserialize)]
struct Index {
    #[serde(default)]
    repos: HashMap<String, Entry>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    last_used: u64,
    bytes: u64,
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

fn touch(repo: &RepoRef, dir: &Path) {
    let mut idx = load_index();
    idx.repos.insert(
        repo.to_string().to_lowercase(),
        Entry {
            last_used: now(),
            bytes: dir_size(dir),
        },
    );
    save_index(&idx);
}

/// Drop least-recently-used mirrors until the cache is under its cap.
fn evict_to_fit() {
    let cap = cap_bytes();
    let mut idx = load_index();
    let mut total: u64 = idx.repos.values().map(|e| e.bytes).sum();
    if total <= cap {
        return;
    }

    let mut by_age: Vec<(String, Entry)> =
        idx.repos.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    by_age.sort_by_key(|(_, e)| e.last_used);

    for (key, entry) in by_age {
        if total <= cap {
            break;
        }
        if let Some((owner, name)) = key.split_once('/') {
            let target = RepoRef {
                owner: owner.to_string(),
                name: name.to_string(),
            };
            if let Some(dir) = mirror_dir(&target) {
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
        idx.repos.remove(&key);
        total = total.saturating_sub(entry.bytes);
    }
    save_index(&idx);
}

/// Total size of the mirror cache, and how many repositories it holds.
pub fn cache_stats() -> (u64, usize) {
    let idx = load_index();
    (idx.repos.values().map(|e| e.bytes).sum(), idx.repos.len())
}

/// Delete every mirrored repository. Clones the user already had are never
/// part of the cache, so this cannot touch their work.
pub fn clear_cache() -> Result<()> {
    let Some(root) = cache_root() else {
        return Ok(());
    };
    let repos = root.join("repos");
    if repos.exists() {
        std::fs::remove_dir_all(&repos).context("removing mirrored repositories")?;
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
    fn missing_commit_is_not_a_panic() {
        assert!(!has_commit(Path::new("/nonexistent"), "deadbeef"));
    }
}
