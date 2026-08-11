//! Minimal GitHub REST client: enough to list pull requests and pull the two
//! sides of a file so the existing diff engine can render them.
//!
//! Everything here is blocking and side-effect free apart from the network —
//! callers run it off the UI thread.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use super::tree::ChangeKind;
use super::FileContent;

const API: &str = "https://api.github.com";
const API_VERSION: &str = "2022-11-28";
/// ureq defaults to 10 MB, which a recursive tree blows past on large repos —
/// rust-lang/rust alone answers with ~20 MB. Bounded, but generously.
const MAX_RESPONSE_BYTES: u64 = 64 * 1024 * 1024;
/// GitHub itself stops at 3000 files per PR; 100 per page.
const MAX_FILE_PAGES: u32 = 30;

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(30)))
        .user_agent("pullspace")
        .build()
        .into()
}

/// Percent-encode one path segment. Avoids a dependency for the handful of
/// characters that actually show up in repo paths.
fn encode_segment(seg: &str) -> String {
    let mut out = String::with_capacity(seg.len());
    for b in seg.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn encode_path(path: &str) -> String {
    path.split('/')
        .map(encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

// -------------------------------------------------------------- repo target

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RepoRef {
    pub owner: String,
    pub name: String,
}

impl std::fmt::Display for RepoRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

/// Accepts what a person is likely to paste: `owner/repo`, a browser URL, an
/// SSH remote, or a link to a specific pull request.
pub fn parse_target(input: &str) -> Option<(RepoRef, Option<u64>)> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }

    // Strip scheme / host / SSH prefix down to the `owner/repo/...` tail.
    let rest = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .map(|r| r.trim_start_matches("www."))
        .and_then(|r| r.strip_prefix("github.com/"))
        .or_else(|| s.strip_prefix("git@github.com:"))
        .or_else(|| s.strip_prefix("github.com/"))
        .unwrap_or(s);

    let mut parts = rest.split('/').filter(|p| !p.is_empty());
    let owner = parts.next()?.to_string();
    let name = parts.next()?.trim_end_matches(".git").to_string();
    if owner.is_empty() || name.is_empty() {
        return None;
    }

    // `.../pull/123` (or `/pulls/123`) opens that PR directly.
    let number = match (parts.next(), parts.next()) {
        (Some("pull" | "pulls"), Some(n)) => n.parse::<u64>().ok(),
        _ => None,
    };
    Some((RepoRef { owner, name }, number))
}

/// Pull an `owner/repo` out of a git remote URL, so a local clone can offer
/// its own PRs without the user typing anything.
pub fn repo_from_remote(url: &str) -> Option<RepoRef> {
    parse_target(url).map(|(r, _)| r)
}

// ------------------------------------------------------------------ request

fn get_raw(token: &str, url: &str, accept: &str) -> Result<(u16, Vec<u8>)> {
    let mut req = agent()
        .get(url)
        .header("Accept", accept)
        .header("X-GitHub-Api-Version", API_VERSION);
    // An empty token means anonymous: public repos still work, at GitHub's
    // much lower unauthenticated rate limit.
    if !token.is_empty() {
        req = req.header("Authorization", &format!("Bearer {token}"));
    }
    let mut res = req.call().with_context(|| format!("GET {url}"))?;

    let status = res.status().as_u16();
    let rate_remaining = res
        .headers()
        .get("x-ratelimit-remaining")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_string());
    let body = res
        .body_mut()
        .with_config()
        .limit(MAX_RESPONSE_BYTES)
        .read_to_vec()?;

    if status == 401 {
        bail!("GitHub rejected the token (401). Sign in again.");
    }
    if status == 403 && rate_remaining.as_deref() == Some("0") {
        bail!("GitHub API rate limit exceeded. Try again shortly.");
    }
    if status == 403 {
        bail!("GitHub denied access (403). The token may lack the `repo` scope.");
    }
    Ok((status, body))
}

fn get_json<T: serde::de::DeserializeOwned>(token: &str, url: &str) -> Result<T> {
    let (status, body) = get_raw(token, url, "application/vnd.github+json")?;
    if status == 404 {
        bail!(
            "Not found (404). Check the name — and if it is a private repository, \
             sign in to an account that can see it."
        );
    }
    if !(200..300).contains(&status) {
        bail!("GitHub returned HTTP {status}");
    }
    serde_json::from_slice(&body).with_context(|| format!("parsing response from {url}"))
}

// ------------------------------------------------------------------- models

#[derive(Deserialize)]
struct User {
    login: String,
}

/// Verify a token and get the account it belongs to.
pub fn viewer_login(token: &str) -> Result<String> {
    let user: User = get_json(token, &format!("{API}/user"))?;
    Ok(user.login)
}

// ----------------------------------------------------------- finding a repo

#[derive(Deserialize)]
struct RawRepo {
    full_name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    private: bool,
    #[serde(default)]
    fork: bool,
    #[serde(default)]
    archived: bool,
    #[serde(default)]
    stargazers_count: u64,
    #[serde(default)]
    pushed_at: Option<String>,
    #[serde(default)]
    default_branch: String,
}

#[derive(Deserialize)]
struct RawSearch {
    #[serde(default)]
    items: Vec<RawRepo>,
}

/// A repository offered as a suggestion in the picker.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RepoHit {
    pub repo: RepoRef,
    pub description: String,
    pub private: bool,
    pub fork: bool,
    pub archived: bool,
    pub stars: u64,
    /// `YYYY-MM-DD` of the last push, empty when GitHub did not say.
    pub pushed: String,
}

/// `full_name` is the only field we cannot do without — everything else is
/// decoration, so a response missing it is the one that gets dropped.
fn hit_of(raw: RawRepo) -> Option<RepoHit> {
    let (owner, name) = raw.full_name.split_once('/')?;
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    Some(RepoHit {
        repo: RepoRef {
            owner: owner.to_string(),
            name: name.to_string(),
        },
        description: raw.description.unwrap_or_default(),
        private: raw.private,
        fork: raw.fork,
        archived: raw.archived,
        stars: raw.stargazers_count,
        // The rest of the timestamp is the time of day, which says nothing
        // useful about how current a repository is.
        pushed: raw
            .pushed_at
            .map(|d| d.chars().take(10).collect())
            .unwrap_or_default(),
    })
}

/// Repositories matching free text, best match first — so a repository can be
/// found by name rather than pasted as a link.
///
/// An exact `owner/name` is looked up directly as well and pinned to the top:
/// search runs off an index that a brand-new, renamed or private repository may
/// not be in yet, and the name typed in full is not a guess to be ranked.
pub fn search_repos(token: &str, query: &str, limit: u32) -> Result<Vec<RepoHit>> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    if let Some((repo, _)) = parse_target(q) {
        let url = format!(
            "{API}/repos/{}/{}",
            encode_segment(&repo.owner),
            encode_segment(&repo.name),
        );
        // A 404 here is the ordinary case — half of a repository name typed so
        // far is not a repository — so the error is the answer, not a failure.
        if let Ok(raw) = get_json::<RawRepo>(token, &url) {
            out.extend(hit_of(raw));
        }
    }
    // A complete name that resolves is the answer. Searching for it as well
    // would bury it under near-misses, and a pasted URL would go to the index
    // as `https github com owner name pull 3`.
    if !out.is_empty() && q.contains('/') {
        return Ok(out);
    }

    // GitHub's index does not read `owner/name` as a path, so the slash is only
    // noise in the query that reaches it.
    let text = q.replace('/', " ");
    let url = format!(
        "{API}/search/repositories?q={}&per_page={limit}",
        encode_segment(text.trim()),
    );
    match get_json::<RawSearch>(token, &url) {
        Ok(raw) => {
            for hit in raw.items.into_iter().filter_map(hit_of) {
                if !out.iter().any(|h: &RepoHit| h.repo == hit.repo) {
                    out.push(hit);
                }
            }
        }
        // Search has its own, much smaller rate limit than the rest of the API.
        // Spending it should not cost us a repository already in hand.
        Err(e) if out.is_empty() => return Err(e),
        Err(_) => {}
    }
    out.truncate(limit as usize);
    Ok(out)
}

/// The signed-in account's repositories, most recently pushed first — what the
/// picker offers before anything is typed, since the pull requests you are
/// asked to review are nearly always on one of them.
pub fn my_repos(token: &str, limit: u32) -> Result<Vec<RepoHit>> {
    if token.is_empty() {
        return Ok(Vec::new());
    }
    let url = format!(
        "{API}/user/repos?sort=pushed&direction=desc&per_page={limit}\
         &affiliation=owner,collaborator,organization_member"
    );
    let raw: Vec<RawRepo> = get_json(token, &url)?;
    Ok(raw.into_iter().filter_map(hit_of).collect())
}

/// A repository's default branch and the commit at its tip.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RepoHead {
    pub branch: String,
    pub sha: String,
}

/// Where "just open the repository" points: the tip of the default branch.
///
/// Two requests, because the repository record names the branch but not its
/// head. A repository with no commits has nothing to browse, and says so rather
/// than surfacing a bare 404.
pub fn repo_head(token: &str, repo: &RepoRef) -> Result<RepoHead> {
    let owner = encode_segment(&repo.owner);
    let name = encode_segment(&repo.name);

    let raw: RawRepo = get_json(token, &format!("{API}/repos/{owner}/{name}"))?;
    let branch = if raw.default_branch.is_empty() {
        // Every non-empty repository has one; fall back to the symbolic name
        // rather than refusing over a field GitHub is expected to send.
        "HEAD".to_string()
    } else {
        raw.default_branch
    };

    let commit: RawCommit = get_json(
        token,
        &format!(
            "{API}/repos/{owner}/{name}/commits/{}",
            encode_segment(&branch)
        ),
    )
    .with_context(|| format!("reading the tip of {branch} — {repo} may have no commits yet"))?;

    Ok(RepoHead {
        branch,
        sha: commit.sha,
    })
}

/// A repository being browsed on its own, with no pull request in the picture.
///
/// The same shape the explorer already reads out of a [`PrDetail`], minus
/// everything that only a pull request has: nothing is changed, so there is no
/// diff, no base side and no changed-file list.
#[derive(Clone, PartialEq, Debug)]
pub struct RepoView {
    pub repo: RepoRef,
    /// The branch `head_sha` came from, for the breadcrumb.
    pub branch: String,
    pub head_sha: String,
    /// Every file in the repository at `head_sha`.
    pub tree: Vec<PathBuf>,
    /// True if GitHub returned only part of the tree.
    pub tree_truncated: bool,
}

impl RepoView {
    /// Where this is on github.com.
    pub fn html_url(&self) -> String {
        format!(
            "https://github.com/{}/{}/tree/{}",
            self.repo.owner, self.repo.name, self.branch
        )
    }
}

#[derive(Deserialize)]
struct RawRef {
    #[serde(rename = "ref")]
    name: String,
    sha: String,
}

#[derive(Deserialize)]
struct RawPr {
    number: u64,
    title: String,
    #[serde(default)]
    user: Option<User>,
    #[serde(default)]
    draft: bool,
    state: String,
    updated_at: String,
    html_url: String,
    head: RawRef,
    base: RawRef,
}

#[derive(Clone, PartialEq, Debug)]
pub struct PrSummary {
    pub number: u64,
    pub title: String,
    pub author: String,
    pub draft: bool,
    pub updated_at: String,
    pub head_ref: String,
    pub base_ref: String,
}

fn author_of(user: &Option<User>) -> String {
    user.as_ref()
        .map(|u| u.login.clone())
        .unwrap_or_else(|| "ghost".to_string())
}

/// Open pull requests, most recently updated first.
pub fn list_prs(token: &str, repo: &RepoRef) -> Result<Vec<PrSummary>> {
    let url = format!(
        "{API}/repos/{}/{}/pulls?state=open&sort=updated&direction=desc&per_page=50",
        encode_segment(&repo.owner),
        encode_segment(&repo.name),
    );
    let raw: Vec<RawPr> = get_json(token, &url)?;
    Ok(raw
        .into_iter()
        .map(|p| PrSummary {
            number: p.number,
            title: p.title,
            author: author_of(&p.user),
            draft: p.draft,
            updated_at: p.updated_at,
            head_ref: p.head.name,
            base_ref: p.base.name,
        })
        .collect())
}

#[derive(Deserialize)]
struct RawFile {
    filename: String,
    status: String,
    #[serde(default)]
    additions: usize,
    #[serde(default)]
    deletions: usize,
    #[serde(default)]
    previous_filename: Option<String>,
}

fn change_kind(status: &str) -> ChangeKind {
    match status {
        "added" | "copied" => ChangeKind::Added,
        "removed" => ChangeKind::Deleted,
        "renamed" => ChangeKind::Renamed,
        // "modified", "changed", "unchanged" and anything new GitHub adds.
        _ => ChangeKind::Modified,
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct PrFile {
    pub path: PathBuf,
    /// Set for renames — the path to read on the base side.
    pub previous_path: Option<PathBuf>,
    pub status: ChangeKind,
    pub additions: usize,
    pub deletions: usize,
}

impl PrFile {
    /// Where this file lived before the change.
    pub fn base_path(&self) -> &PathBuf {
        self.previous_path.as_ref().unwrap_or(&self.path)
    }
}

#[derive(Deserialize)]
struct RawTree {
    #[serde(default)]
    tree: Vec<RawTreeEntry>,
    /// GitHub sets this when the repo exceeds its tree limits.
    #[serde(default)]
    truncated: bool,
}

#[derive(Deserialize)]
struct RawTreeEntry {
    path: String,
    #[serde(rename = "type")]
    kind: String,
}

/// Every file in the repository as of `sha`, in one request, so a pull request
/// can be browsed like a checkout rather than just a list of changes.
///
/// The bool is GitHub's `truncated` flag — true past 100k entries / 7 MB, where
/// it silently returns a partial tree.
pub fn repo_tree(token: &str, repo: &RepoRef, sha: &str) -> Result<(Vec<PathBuf>, bool)> {
    let url = format!(
        "{API}/repos/{}/{}/git/trees/{}?recursive=1",
        encode_segment(&repo.owner),
        encode_segment(&repo.name),
        encode_segment(sha),
    );
    let raw: RawTree = get_json(token, &url)?;
    let paths = raw
        .tree
        .into_iter()
        // "tree" entries are directories and "commit" entries are submodules;
        // the file tree is rebuilt from the blob paths alone.
        .filter(|e| e.kind == "blob")
        .map(|e| PathBuf::from(e.path))
        .collect();
    Ok((paths, raw.truncated))
}

#[derive(Deserialize)]
struct RawCompare {
    merge_base_commit: RawCommit,
}

#[derive(Deserialize)]
struct RawCommit {
    sha: String,
}

#[derive(Clone, PartialEq, Debug)]
pub struct PrDetail {
    pub repo: RepoRef,
    pub number: u64,
    pub title: String,
    pub author: String,
    pub state: String,
    pub draft: bool,
    pub html_url: String,
    pub head_ref: String,
    pub base_ref: String,
    /// The merge base — the commit GitHub's "Files changed" tab diffs against.
    pub base_sha: String,
    pub head_sha: String,
    pub files: Vec<PrFile>,
    /// True if the PR has more files than we fetched.
    pub truncated: bool,
    /// Every file in the repo at `head_sha`, so the explorer can show the whole
    /// tree and not just what changed. Filled in by the caller — from a local
    /// clone when there is one, else [`repo_tree`] — and empty when neither
    /// worked, which degrades to a changed-files-only explorer.
    pub tree: Vec<PathBuf>,
    /// True if GitHub returned only part of the repository tree.
    pub tree_truncated: bool,
}

/// Load a PR: metadata, the merge base, the changed-file list, and the full
/// repository tree at the PR's head.
///
/// The merge base matters — diffing against `base.sha` would show every commit
/// that landed on the base branch since the PR was opened as part of the PR.
pub fn load_pr(token: &str, repo: &RepoRef, number: u64) -> Result<PrDetail> {
    let owner = encode_segment(&repo.owner);
    let name = encode_segment(&repo.name);

    let pr: RawPr = get_json(token, &format!("{API}/repos/{owner}/{name}/pulls/{number}"))?;

    let compare: RawCompare = get_json(
        token,
        &format!(
            "{API}/repos/{owner}/{name}/compare/{}...{}",
            pr.base.sha, pr.head.sha
        ),
    )
    .with_context(|| format!("resolving the merge base for #{number}"))?;

    let mut files = Vec::new();
    let mut truncated = false;
    for page in 1..=MAX_FILE_PAGES {
        let url =
            format!("{API}/repos/{owner}/{name}/pulls/{number}/files?per_page=100&page={page}");
        let raw: Vec<RawFile> = get_json(token, &url)?;
        let full_page = raw.len() == 100;
        files.extend(raw.into_iter().map(|f| PrFile {
            path: PathBuf::from(&f.filename),
            previous_path: f.previous_filename.map(PathBuf::from),
            status: change_kind(&f.status),
            additions: f.additions,
            deletions: f.deletions,
        }));
        if !full_page {
            break;
        }
        if page == MAX_FILE_PAGES {
            truncated = true;
        }
    }

    Ok(PrDetail {
        repo: repo.clone(),
        number: pr.number,
        title: pr.title,
        author: author_of(&pr.user),
        state: pr.state,
        draft: pr.draft,
        html_url: pr.html_url,
        head_ref: pr.head.name,
        base_ref: pr.base.name,
        base_sha: compare.merge_base_commit.sha,
        head_sha: pr.head.sha,
        files,
        truncated,
        tree: Vec::new(),
        tree_truncated: false,
    })
}

/// One side of a file, at a specific commit. A 404 means the file does not
/// exist there — expected for the base side of an added file.
pub fn file_at(token: &str, repo: &RepoRef, sha: &str, path: &std::path::Path) -> Result<FileContent> {
    let rel = path.to_string_lossy().replace('\\', "/");
    let url = format!(
        "{API}/repos/{}/{}/contents/{}?ref={}",
        encode_segment(&repo.owner),
        encode_segment(&repo.name),
        encode_path(&rel),
        encode_segment(sha),
    );
    // The `raw` media type returns file bytes instead of base64-in-JSON.
    let (status, body) = get_raw(token, &url, "application/vnd.github.raw")?;
    if status == 404 {
        return Ok(FileContent::Absent);
    }
    if !(200..300).contains(&status) {
        bail!("GitHub returned HTTP {status} for {rel}");
    }
    Ok(FileContent::from_bytes(&body))
}

/// The `owner/repo` for a local clone's `origin` (or the only remote), if it
/// points at GitHub.
pub fn repo_from_local(root: &std::path::Path) -> Option<RepoRef> {
    let repo = git2::Repository::discover(root).ok()?;
    let names = repo.remotes().ok()?;
    // `remotes()` yields Result<Option<&str>> per entry; keep only real names.
    let list: Vec<String> = names
        .iter()
        .filter_map(|r| r.ok().flatten())
        .map(String::from)
        .collect();
    let preferred = list
        .iter()
        .find(|n| n.as_str() == "origin")
        .or_else(|| list.first())?;
    let remote = repo.find_remote(preferred.as_str()).ok()?;
    let url = remote.url().ok()?;
    if !url.contains("github.com") {
        return None;
    }
    repo_from_remote(url)
}

/// Turn a PR's file list into the status map the tree and viewer already speak.
pub fn statuses_of(files: &[PrFile]) -> std::collections::HashMap<PathBuf, ChangeKind> {
    files.iter().map(|f| (f.path.clone(), f.status)).collect()
}

pub fn find_file<'a>(files: &'a [PrFile], path: &std::path::Path) -> Option<&'a PrFile> {
    files.iter().find(|f| f.path == path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_owner_repo() {
        let (r, n) = parse_target("rust-lang/rust").unwrap();
        assert_eq!(r.owner, "rust-lang");
        assert_eq!(r.name, "rust");
        assert_eq!(n, None);
    }

    #[test]
    fn parses_browser_url() {
        let (r, n) = parse_target("https://github.com/DioxusLabs/dioxus").unwrap();
        assert_eq!(r.to_string(), "DioxusLabs/dioxus");
        assert_eq!(n, None);
    }

    #[test]
    fn parses_pull_request_url() {
        let (r, n) = parse_target("https://github.com/rust-lang/rust/pull/12345").unwrap();
        assert_eq!(r.to_string(), "rust-lang/rust");
        assert_eq!(n, Some(12345));
    }

    #[test]
    fn parses_ssh_remote() {
        let (r, _) = parse_target("git@github.com:owner/name.git").unwrap();
        assert_eq!(r.to_string(), "owner/name");
    }

    #[test]
    fn parses_https_remote_with_git_suffix() {
        let r = repo_from_remote("https://github.com/owner/name.git").unwrap();
        assert_eq!(r.to_string(), "owner/name");
    }

    #[test]
    fn rejects_junk() {
        assert!(parse_target("").is_none());
        assert!(parse_target("   ").is_none());
        assert!(parse_target("just-an-owner").is_none());
    }

    #[test]
    fn search_hits_split_the_full_name() {
        let raw: RawRepo = serde_json::from_str(
            r#"{"full_name":"DioxusLabs/dioxus","description":"Fullstack UI",
                "private":false,"fork":false,"archived":false,
                "stargazers_count":21000,"pushed_at":"2026-08-01T12:33:04Z"}"#,
        )
        .unwrap();
        let hit = hit_of(raw).unwrap();
        assert_eq!(hit.repo.to_string(), "DioxusLabs/dioxus");
        assert_eq!(hit.stars, 21000);
        // The time of day is dropped; the date is what the row shows.
        assert_eq!(hit.pushed, "2026-08-01");
    }

    #[test]
    fn search_hits_survive_a_bare_response() {
        // Everything but `full_name` is optional, and a hit without one is not
        // a repository we could open.
        let raw: RawRepo = serde_json::from_str(r#"{"full_name":"owner/name"}"#).unwrap();
        let hit = hit_of(raw).unwrap();
        assert_eq!(hit.repo.to_string(), "owner/name");
        assert!(hit.description.is_empty());
        assert!(hit.pushed.is_empty());

        let orphan: RawRepo = serde_json::from_str(r#"{"full_name":"no-slash"}"#).unwrap();
        assert!(hit_of(orphan).is_none());
    }

    #[test]
    fn encodes_awkward_paths() {
        assert_eq!(encode_path("src/main.rs"), "src/main.rs");
        assert_eq!(encode_path("a b/c#d.rs"), "a%20b/c%23d.rs");
        // Separators survive; segment contents do not.
        assert_eq!(encode_path("dir/sub dir/f.rs"), "dir/sub%20dir/f.rs");
    }

    #[test]
    fn maps_github_statuses() {
        assert_eq!(change_kind("added"), ChangeKind::Added);
        assert_eq!(change_kind("removed"), ChangeKind::Deleted);
        assert_eq!(change_kind("renamed"), ChangeKind::Renamed);
        assert_eq!(change_kind("modified"), ChangeKind::Modified);
        assert_eq!(change_kind("changed"), ChangeKind::Modified);
    }

    #[test]
    fn renamed_files_read_the_old_path_on_the_base_side() {
        let f = PrFile {
            path: PathBuf::from("new.rs"),
            previous_path: Some(PathBuf::from("old.rs")),
            status: ChangeKind::Renamed,
            additions: 1,
            deletions: 1,
        };
        assert_eq!(f.base_path(), &PathBuf::from("old.rs"));

        let plain = PrFile {
            path: PathBuf::from("same.rs"),
            previous_path: None,
            status: ChangeKind::Modified,
            additions: 0,
            deletions: 0,
        };
        assert_eq!(plain.base_path(), &PathBuf::from("same.rs"));
    }

    /// Talks to github.com, so it is not part of the normal run:
    /// `cargo test -- --ignored live_pr_round_trip --nocapture`.
    /// Anonymous, so it is subject to the 60 req/hour unauthenticated limit.
    #[test]
    #[ignore = "hits the network"]
    fn live_pr_round_trip() {
        // A long-merged PR, so the shape of the response is stable.
        let repo = RepoRef {
            owner: "DioxusLabs".to_string(),
            name: "dioxus".to_string(),
        };
        let pr = load_pr("", &repo, 1).expect("load PR");
        assert_eq!(pr.number, 1);
        assert!(!pr.base_sha.is_empty(), "merge base resolved");
        assert!(!pr.files.is_empty(), "PR has files");

        // `load_pr` deliberately leaves the tree to the caller, so that a
        // local clone can supply it instead of downloading it.
        assert!(pr.tree.is_empty(), "tree is the caller's job");

        // The repo tree must be a superset of the changed files (deletions
        // aside), or the explorer would be missing files the PR touches.
        let (tree, _) = repo_tree("", &repo, &pr.head_sha).expect("repo tree");
        assert!(tree.len() > pr.files.len(), "tree is the whole repo");
        for f in &pr.files {
            if f.status != ChangeKind::Deleted {
                assert!(tree.contains(&f.path), "{:?} missing from tree", f.path);
            }
        }

        let f = &pr.files[0];
        let head = file_at("", &repo, &pr.head_sha, &f.path).expect("head side");
        assert!(
            matches!(head, FileContent::Text(_) | FileContent::Binary),
            "head content present"
        );
        let base = file_at("", &repo, &pr.base_sha, f.base_path()).expect("base side");
        // Absent is legitimate here — it just means the file was added.
        let _ = base;
    }

    /// Also hits the network: `cargo test -- --ignored live_repo_head`.
    ///
    /// The path a repository with no open pull requests takes: resolve the
    /// default branch, then list it.
    #[test]
    #[ignore = "hits the network"]
    fn live_repo_head() {
        let repo = RepoRef {
            owner: "DioxusLabs".to_string(),
            name: "dioxus".to_string(),
        };
        let head = repo_head("", &repo).expect("default branch");
        assert!(!head.branch.is_empty(), "GitHub names the default branch");
        assert_eq!(head.sha.len(), 40, "a full commit sha: {}", head.sha);

        let (tree, _) = repo_tree("", &repo, &head.sha).expect("repo tree");
        assert!(tree.len() > 1, "the whole repository, not one file");
        assert!(tree.contains(&PathBuf::from("Cargo.toml")));
    }

    /// Also hits the network: `cargo test -- --ignored live_repo_search`.
    #[test]
    #[ignore = "hits the network"]
    fn live_repo_search() {
        // A name, not a link — the whole point of the picker's search box.
        let hits = search_repos("", "dioxus", 8).expect("search");
        assert!(!hits.is_empty(), "a common name finds something");
        assert!(
            hits.iter().any(|h| h.repo.name.contains("dioxus")),
            "results are actually about the query: {hits:?}"
        );

        // A complete name resolves exactly, and answers with itself alone.
        let exact = search_repos("", "DioxusLabs/dioxus", 8).expect("exact");
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].repo.to_string(), "DioxusLabs/dioxus");
    }

    #[test]
    fn file_list_becomes_a_status_map() {
        let files = vec![
            PrFile {
                path: PathBuf::from("a.rs"),
                previous_path: None,
                status: ChangeKind::Added,
                additions: 3,
                deletions: 0,
            },
            PrFile {
                path: PathBuf::from("b.rs"),
                previous_path: None,
                status: ChangeKind::Deleted,
                additions: 0,
                deletions: 7,
            },
        ];
        let map = statuses_of(&files);
        assert_eq!(map.get(&PathBuf::from("a.rs")), Some(&ChangeKind::Added));
        assert_eq!(map.get(&PathBuf::from("b.rs")), Some(&ChangeKind::Deleted));
    }
}
