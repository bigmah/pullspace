//! Minimal GitHub REST client: enough to list pull requests and pull the two
//! sides of a file so the existing diff engine can render them.
//!
//! Every call here is async, and none of them touches the machine: the only
//! transport is [`http`](super::http), which is the browser's own fetch. That
//! is what lets a static page talk to GitHub with no server of its own in
//! between — and, since nothing outside that one module is browser-specific,
//! it is also what keeps the parsing in here testable on the host.

use std::borrow::Cow;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::FileContent;
use super::http;
use super::tree::ChangeKind;

const API: &str = "https://api.github.com";
const API_VERSION: &str = "2022-11-28";
/// Public file bytes, straight from the CDN.
///
/// Worth the special case twice over: it is not metered against the API's rate
/// limit, and it answers `Access-Control-Allow-Origin: *`, so the static web
/// build can read any public repository from a browser with no token at all.
/// It takes no `Authorization` header — sending one would turn a plain
/// cross-origin GET into a preflighted one, which this host does not answer —
/// so private repositories go the long way, through [`API`].
const RAW: &str = "https://raw.githubusercontent.com";
/// GitHub itself stops at 3000 files per PR; 100 per page.
const MAX_FILE_PAGES: u32 = 30;
/// 100 comments per page, per kind. A thread past this is one nobody is
/// reading to the end of anyway, and the pane says when it was cut short.
const MAX_COMMENT_PAGES: u32 = 5;

/// Percent-encode one path segment into `out`. Avoids a dependency for the
/// handful of characters that actually show up in repo paths.
fn push_encoded(out: &mut String, seg: &str) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for b in seg.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => {
                out.push('%');
                out.push(HEX[usize::from(b >> 4)] as char);
                out.push(HEX[usize::from(b & 0xf)] as char);
            }
        }
    }
}

fn encode_segment(seg: &str) -> String {
    let mut out = String::with_capacity(seg.len());
    push_encoded(&mut out, seg);
    out
}

fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 8);
    for (i, seg) in path.split('/').enumerate() {
        if i > 0 {
            out.push('/');
        }
        push_encoded(&mut out, seg);
    }
    out
}

// -------------------------------------------------------------- repo target

#[derive(Clone, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RepoRef {
    pub owner: String,
    pub name: String,
}

impl std::fmt::Display for RepoRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

/// Strip scheme / host / SSH prefix down to the `owner/repo/...` tail.
///
/// `None` when there was none of that to strip, which is how the caller tells
/// something pasted from a browser from something typed by hand — the two do
/// not mean quite the same thing once the host is off the front.
pub(crate) fn strip_host(s: &str) -> Option<&str> {
    s.strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .map(|r| r.trim_start_matches("www."))
        .and_then(|r| r.strip_prefix("github.com/"))
        .or_else(|| s.strip_prefix("git@github.com:"))
        .or_else(|| s.strip_prefix("github.com/"))
}

/// Accepts what a person is likely to paste: `owner/repo`, a browser URL, an
/// SSH remote, or a link to a specific pull request.
pub fn parse_target(input: &str) -> Option<(RepoRef, Option<u64>)> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }

    let rest = strip_host(s).unwrap_or(s);

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

/// The account one piece of text names, when it names an account and not a
/// repository inside one.
///
/// `torvalds`, `torvalds/`, `@torvalds`, and the two URLs GitHub hands out for
/// an account: its profile, and the `orgs/…/repositories` page the
/// Repositories tab lands on. Anything with a repository in it belongs to
/// [`parse_target`] instead.
///
/// The login is checked against GitHub's own rule rather than sent as typed:
/// it is what keeps a half-written search phrase from costing a request that
/// can only come back 404.
pub fn parse_owner(input: &str) -> Option<String> {
    let s = input.trim();
    let hosted = strip_host(s);
    let rest = hosted.unwrap_or(s);
    // Only from a URL: `orgs/x` typed by hand is a repository called `x`.
    let rest = match hosted {
        Some(_) => rest.strip_prefix("orgs/").unwrap_or(rest),
        None => rest,
    };

    let mut parts = rest.split('/').filter(|p| !p.is_empty());
    let login = parts.next()?.trim_start_matches('@');
    // The Repositories tab is still the account; a repository name is not.
    if !matches!(parts.next(), None | Some("repositories")) {
        return None;
    }
    is_login(login).then(|| login.to_string())
}

/// GitHub's rule for a login: letters, digits and hyphens, up to 39 of them,
/// and not starting or ending with one.
fn is_login(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 39
        && !s.starts_with('-')
        && !s.ends_with('-')
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

// ------------------------------------------------------------------ request

/// How long until a budget refills, from `x-ratelimit-reset`.
///
/// `None` when the header is missing or already in the past — a wait of "0
/// seconds" is worse than not saying.
fn seconds_until(reset: Option<&str>) -> Option<u64> {
    let at: u64 = reset?.trim().parse().ok()?;
    // web_time reads `Date.now()` in a page; std's SystemTime panics there.
    let now = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    at.checked_sub(now).filter(|left| *left > 0)
}

fn wait_phrase(secs: u64) -> String {
    match secs {
        s if s <= 90 => format!("Try again in {s} seconds."),
        s => format!("Try again in {} minutes.", s.div_ceil(60)),
    }
}

/// What ran out, how long it is out for, and what still works meanwhile.
///
/// Worth this much care because the honest answer is usually reassuring. The
/// budget people actually exhaust is `search`, which refills every minute and
/// is spent by typing — while the hourly budget that opens repositories and
/// pull requests sits there untouched. "Rate limit exceeded" over that reads as
/// "come back in an hour", and sends people away from an app that would have
/// opened anything they could name.
fn rate_limited(token: &str, resource: Option<&str>, reset: Option<&str>) -> anyhow::Error {
    let wait = seconds_until(reset)
        .map(wait_phrase)
        .unwrap_or_else(|| "Try again shortly.".to_string());
    let anon = token.is_empty();

    if resource == Some("search") {
        let allowance = if anon {
            "GitHub allows 10 repository searches a minute when signed out"
        } else {
            "GitHub allows 30 repository searches a minute"
        };
        return anyhow::anyhow!(
            "{allowance}, and this browser has used them. {wait} Searching is the \
             only thing affected — typing a full owner/name, or pasting a link to \
             a pull request, still opens it."
        );
    }

    if anon {
        return anyhow::anyhow!(
            "GitHub allows 60 API requests an hour when signed out, and this browser \
             has used them. {wait} A token raises it to 5000."
        );
    }
    anyhow::anyhow!("GitHub API rate limit exceeded. {wait}")
}

async fn get_raw(token: &str, url: &str, accept: &str) -> Result<(u16, Vec<u8>)> {
    // An empty token means anonymous: public repos still work, at GitHub's
    // much lower unauthenticated rate limit.
    let auth = format!("Bearer {token}");
    let mut headers = vec![("Accept", accept), ("X-GitHub-Api-Version", API_VERSION)];
    if !token.is_empty() {
        headers.push(("Authorization", auth.as_str()));
    }
    let reply = http::get(url, &headers).await?;

    if reply.status == 401 {
        bail!("GitHub rejected the token (401). Sign in again.");
    }
    // An emptied budget is a 403 for search and for the API at large, a 429 when
    // GitHub feels strongly about it, and occasionally a 404. What tells them
    // apart from a genuine refusal is the budget reading zero.
    let spent = reply.rate_remaining.as_deref() == Some("0");
    if spent && matches!(reply.status, 403 | 404 | 429) {
        return Err(rate_limited(
            token,
            reply.rate_resource.as_deref(),
            reply.rate_reset.as_deref(),
        ));
    }
    if reply.status == 429 {
        return Err(rate_limited(token, None, reply.rate_reset.as_deref()));
    }
    if reply.status == 403 {
        bail!("GitHub denied access (403). The token may lack the `repo` scope.");
    }
    Ok((reply.status, reply.body))
}

async fn get_json<T: serde::de::DeserializeOwned>(token: &str, url: &str) -> Result<T> {
    let (status, body) = get_raw(token, url, "application/vnd.github+json").await?;
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
pub async fn viewer_login(token: &str) -> Result<String> {
    let user: User = get_json(token, &format!("{API}/user")).await?;
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
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
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
pub async fn search_repos(token: &str, query: &str, limit: u32) -> Result<Vec<RepoHit>> {
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
        if let Ok(raw) = get_json::<RawRepo>(token, &url).await {
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
    match get_json::<RawSearch>(token, &url).await {
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
pub async fn my_repos(token: &str, limit: u32) -> Result<Vec<RepoHit>> {
    if token.is_empty() {
        return Ok(Vec::new());
    }
    let url = format!(
        "{API}/user/repos?sort=pushed&direction=desc&per_page={limit}\
         &affiliation=owner,collaborator,organization_member"
    );
    let raw: Vec<RawRepo> = get_json(token, &url).await?;
    Ok(hits_of(raw))
}

fn hits_of(raw: Vec<RawRepo>) -> Vec<RepoHit> {
    raw.into_iter().filter_map(hit_of).collect()
}

// -------------------------------------------------------- finding an account

#[derive(Deserialize)]
struct RawOwner {
    login: String,
    /// `User` or `Organization`.
    #[serde(rename = "type", default)]
    kind: String,
    /// The display name — "The Rust Programming Language" over `rust-lang`.
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    public_repos: u64,
}

/// An account offered as a suggestion: the row that opens up everything it
/// owns, rather than one repository.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct OwnerHit {
    pub login: String,
    /// An organisation rather than a person. Only decoration — both list their
    /// repositories the same way.
    pub org: bool,
    /// The display name, empty when the account has none or it is the login
    /// again.
    pub name: String,
    pub public_repos: u64,
}

fn owner_of(raw: RawOwner) -> Option<OwnerHit> {
    if raw.login.is_empty() {
        return None;
    }
    let name = raw.name.unwrap_or_default();
    let name = if name.eq_ignore_ascii_case(&raw.login) {
        String::new()
    } else {
        name
    };
    Some(OwnerHit {
        org: raw.kind == "Organization",
        login: raw.login,
        name,
        public_repos: raw.public_repos,
    })
}

/// The account a name belongs to, if it belongs to one.
///
/// This is what makes typing an organisation's name work rather than nearly
/// work: the search index ranks repositories by *their* names, so an
/// organisation whose repositories are not called after it — which is most of
/// them — cannot be found by searching for it. A login, on the other hand, is
/// a lookup and always exact.
///
/// It costs one request on the `core` budget, where typing otherwise spends
/// only `search`. That is the trade, and it is the right way round: `core` is
/// sixty an hour signed out against ten a minute for `search`, and this only
/// goes out for text shaped like a login in the first place.
///
/// `None` covers every way it can fail, because they all mean the same thing
/// here — no account row to offer, and a search still on its way.
pub async fn lookup_owner(token: &str, login: &str) -> Option<OwnerHit> {
    if !is_login(login) {
        return None;
    }
    let url = format!("{API}/users/{}", encode_segment(login));
    owner_of(get_json::<RawOwner>(token, &url).await.ok()?)
}

/// Everything one account owns, most recently pushed first.
///
/// Three endpoints, because GitHub keeps three lists and only two of them can
/// see anything private: `/user/repos` for the signed-in account itself,
/// `/orgs/…/repos` for an organisation the token is a member of, and
/// `/users/…/repos`, which is public, answers for people and organisations
/// alike, and is where anonymous browsing ends up.
pub async fn owner_repos(
    token: &str,
    viewer: &str,
    owner: &str,
    limit: u32,
) -> Result<Vec<RepoHit>> {
    let login = encode_segment(owner);
    let page = format!("sort=pushed&direction=desc&per_page={limit}");

    // Your own account, which is the one whose private repositories you are
    // most likely to be looking for.
    if !viewer.is_empty() && viewer.eq_ignore_ascii_case(owner) {
        let url = format!("{API}/user/repos?affiliation=owner&{page}");
        return Ok(hits_of(get_json(token, &url).await?));
    }
    // A member's token sees an organisation's private repositories here and
    // nowhere else. Anonymously this answers with the same public list as the
    // call below, so it is not worth the request — and 404s for a person.
    if !token.is_empty() {
        let url = format!("{API}/orgs/{login}/repos?type=all&{page}");
        if let Ok(raw) = get_json::<Vec<RawRepo>>(token, &url).await {
            return Ok(hits_of(raw));
        }
    }
    let url = format!("{API}/users/{login}/repos?{page}");
    Ok(hits_of(get_json(token, &url).await?))
}

/// A repository's default branch and the commit at its tip.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RepoHead {
    pub branch: String,
    pub sha: String,
}

/// Where "just open the repository" points: the tip of the default branch.
///
/// Two requests, because the repository record names the branch but not its
/// head. A repository with no commits has nothing to browse, and says so rather
/// than surfacing a bare 404.
pub async fn repo_head(token: &str, repo: &RepoRef) -> Result<RepoHead> {
    let owner = encode_segment(&repo.owner);
    let name = encode_segment(&repo.name);

    let raw: RawRepo = get_json(token, &format!("{API}/repos/{owner}/{name}")).await?;
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
    .await
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
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct RepoView {
    pub repo: RepoRef,
    /// The branch `head_sha` came from, for the breadcrumb.
    pub branch: String,
    pub head_sha: String,
    /// Every file in the repository at `head_sha`.
    pub tree: Snapshot,
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
    /// The description. Null on a pull request opened without one.
    #[serde(default)]
    body: Option<String>,
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

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
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
pub async fn list_prs(token: &str, repo: &RepoRef) -> Result<Vec<PrSummary>> {
    let url = format!(
        "{API}/repos/{}/{}/pulls?state=open&sort=updated&direction=desc&per_page=50",
        encode_segment(&repo.owner),
        encode_segment(&repo.name),
    );
    let raw: Vec<RawPr> = get_json(token, &url).await?;
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

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
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
    #[serde(default)]
    sha: String,
    #[serde(default)]
    size: u64,
}

/// One file of a repository at one commit: where it is, and which git blob it
/// is made of.
///
/// The SHA is the point. It is git's hash of the file's contents, so it is the
/// same forty characters in every commit, branch and repository that file ever
/// appears in — which is what lets a local copy be kept once and found again by
/// a pull request opened a week later against a commit that did not exist yet.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TreeEntry {
    pub path: PathBuf,
    pub sha: String,
    /// Bytes, as GitHub counts them. What the clone budgets against, so it is
    /// spent before a single request is made.
    pub size: u64,
}

/// Every file in a repository as of one commit — a checkout's worth of
/// filenames, without the contents.
///
/// Kept sorted by path, so a lookup is a binary search rather than a hash map
/// alongside: this is written to disk as-is and read back on the next visit.
#[derive(Clone, Default, PartialEq, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub repo: RepoRef,
    pub commit: String,
    pub files: Vec<TreeEntry>,
    /// GitHub returned only part of the tree — past about 100k entries / 7 MB.
    pub truncated: bool,
}

impl Snapshot {
    /// A snapshot of a commit whose tree could not be read. Everything degrades
    /// to fetching by path, which is what the app did before it had a store.
    pub fn unknown(repo: &RepoRef, commit: &str) -> Self {
        Snapshot {
            repo: repo.clone(),
            commit: commit.to_string(),
            files: Vec::new(),
            truncated: false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn paths(&self) -> impl Iterator<Item = &std::path::Path> {
        self.files.iter().map(|f| f.path.as_path())
    }

    /// The blob one path is made of at this commit.
    pub fn entry(&self, path: &std::path::Path) -> Option<&TreeEntry> {
        self.files
            .binary_search_by(|f| f.path.as_path().cmp(path))
            .ok()
            .map(|i| &self.files[i])
    }

    /// Where the files are kept on disk, and how it is found again.
    pub fn key(&self) -> String {
        format!(
            "{}~{}~{}.json",
            self.repo.owner, self.repo.name, self.commit
        )
    }
}

/// Every file in the repository as of `sha`, in one request, so a pull request
/// can be browsed like a checkout rather than just a list of changes.
pub async fn repo_tree(token: &str, repo: &RepoRef, sha: &str) -> Result<Snapshot> {
    let url = format!(
        "{API}/repos/{}/{}/git/trees/{}?recursive=1",
        encode_segment(&repo.owner),
        encode_segment(&repo.name),
        encode_segment(sha),
    );
    let raw: RawTree = get_json(token, &url).await?;
    let mut files: Vec<TreeEntry> = raw
        .tree
        .into_iter()
        // "tree" entries are directories and "commit" entries are submodules;
        // the file tree is rebuilt from the blob paths alone.
        .filter(|e| e.kind == "blob")
        .map(|e| TreeEntry {
            path: PathBuf::from(e.path),
            sha: e.sha,
            size: e.size,
        })
        .collect();
    // Git writes trees in its own order, which is nearly but not quite this
    // one. Sorting here is what makes `entry` a binary search.
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(Snapshot {
        repo: repo.clone(),
        commit: sha.to_string(),
        files,
        truncated: raw.truncated,
    })
}

#[derive(Deserialize)]
struct RawCompare {
    merge_base_commit: RawCommit,
}

#[derive(Deserialize)]
struct RawCommit {
    sha: String,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct PrDetail {
    pub repo: RepoRef,
    pub number: u64,
    pub title: String,
    /// The description, as markdown source — empty when there is none. Carried
    /// on the pull request itself, so the conversation pane has something to
    /// show before its own request comes back.
    pub body: String,
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
    /// tree and not just what changed. Filled in by the caller from
    /// [`repo_tree`], and empty when that failed — which degrades to a
    /// changed-files-only explorer.
    pub tree: Snapshot,
    /// The same at the merge base, which is what the left-hand side of every
    /// diff is read from. Only the changed files are ever wanted out of it, but
    /// having it by blob SHA is what lets those come from the local store
    /// too — the base commit is usually a branch tip that has been read before.
    pub base_tree: Snapshot,
}

impl PrDetail {
    /// What one changed file is remembered as, once somebody has marked it
    /// read: the git blob it is made of.
    ///
    /// The blob rather than the path, because a mark is a statement about
    /// contents — see [`viewed`](crate::backend::viewed). Nearly always the
    /// head side; the base side for a file this pull request deletes, which has
    /// no head side to hash. Falling back to the path covers the one case with
    /// no blob at all: a pull request whose tree GitHub would not serve, where
    /// a mark keyed by name is still better than no marks.
    pub fn blob_key(&self, path: &std::path::Path) -> Cow<'_, str> {
        if let Some(entry) = self.tree.entry(path) {
            return Cow::Borrowed(entry.sha.as_str());
        }
        find_file(&self.files, path)
            .and_then(|f| self.base_tree.entry(f.base_path()))
            .map_or_else(
                || Cow::Owned(format!("path:{}", path.display())),
                |entry| Cow::Borrowed(entry.sha.as_str()),
            )
    }

    /// [`blob_key`](Self::blob_key) for a caller already holding the changed
    /// file — asked once per file when the read count is taken, where the
    /// lookup above would fall back to a scan of the whole list each time.
    pub fn blob_key_of(&self, f: &PrFile) -> Cow<'_, str> {
        if let Some(entry) = self.tree.entry(&f.path) {
            return Cow::Borrowed(entry.sha.as_str());
        }
        self.base_tree.entry(f.base_path()).map_or_else(
            || Cow::Owned(format!("path:{}", f.path.display())),
            |entry| Cow::Borrowed(entry.sha.as_str()),
        )
    }
}

/// Load a PR: metadata, the merge base, the changed-file list, and the full
/// repository tree at the PR's head.
///
/// The merge base matters — diffing against `base.sha` would show every commit
/// that landed on the base branch since the PR was opened as part of the PR.
pub async fn load_pr(token: &str, repo: &RepoRef, number: u64) -> Result<PrDetail> {
    let owner = encode_segment(&repo.owner);
    let name = encode_segment(&repo.name);

    let pr: RawPr = get_json(token, &format!("{API}/repos/{owner}/{name}/pulls/{number}")).await?;

    let compare: RawCompare = get_json(
        token,
        &format!(
            "{API}/repos/{owner}/{name}/compare/{}...{}",
            pr.base.sha, pr.head.sha
        ),
    )
    .await
    .with_context(|| format!("resolving the merge base for #{number}"))?;

    let mut files = Vec::new();
    let mut truncated = false;
    for page in 1..=MAX_FILE_PAGES {
        let url =
            format!("{API}/repos/{owner}/{name}/pulls/{number}/files?per_page=100&page={page}");
        let raw: Vec<RawFile> = get_json(token, &url).await?;
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

    let base_sha = compare.merge_base_commit.sha;
    let head_sha = pr.head.sha;
    Ok(PrDetail {
        repo: repo.clone(),
        number: pr.number,
        title: pr.title,
        body: pr.body.unwrap_or_default(),
        author: author_of(&pr.user),
        state: pr.state,
        draft: pr.draft,
        html_url: pr.html_url,
        head_ref: pr.head.name,
        base_ref: pr.base.name,
        tree: Snapshot::unknown(repo, &head_sha),
        base_tree: Snapshot::unknown(repo, &base_sha),
        base_sha,
        head_sha,
        files,
        truncated,
    })
}

// ------------------------------------------------------------- conversation

/// Where a piece of writing on a pull request came from. GitHub keeps these on
/// three separate endpoints, and they read differently enough to be worth
/// telling apart once they are back in one list.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum CommentKind {
    /// The pull request's own discussion thread.
    Discussion,
    /// What a reviewer wrote when submitting a review.
    Review,
    /// Left on a line of the diff.
    Inline,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Comment {
    pub kind: CommentKind,
    pub author: String,
    /// ISO 8601, as GitHub sends it — kept whole because it is what the
    /// three lists are merged on.
    pub created_at: String,
    /// Markdown source. Empty for a bare approval, which is still worth showing.
    pub body: String,
    pub html_url: String,
    /// `approved`, `changes requested`, … for a review; empty otherwise.
    pub verdict: String,
    /// The file a line comment hangs off, and the line in the head commit.
    pub path: Option<PathBuf>,
    pub line: Option<usize>,
}

/// Everything written on a pull request, oldest first.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct Thread {
    pub comments: Vec<Comment>,
    /// True when one of the lists ran past [`MAX_COMMENT_PAGES`].
    pub truncated: bool,
}

/// One JSON shape for all three endpoints: they agree on the fields that
/// matter and each leaves the rest out, which `Option` already handles.
#[derive(Deserialize)]
struct RawComment {
    #[serde(default)]
    user: Option<User>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    /// Reviews date themselves with this instead.
    #[serde(default)]
    submitted_at: Option<String>,
    #[serde(default)]
    html_url: String,
    /// Reviews only: `APPROVED`, `CHANGES_REQUESTED`, `COMMENTED`, `PENDING`.
    #[serde(default)]
    state: Option<String>,
    /// Line comments only.
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    line: Option<usize>,
    /// Where the comment was left when it was written — the fallback for one
    /// whose lines have since been rewritten, which GitHub answers with a null
    /// `line`.
    #[serde(default)]
    original_line: Option<usize>,
}

/// GitHub's review states, in the words the pane uses.
fn verdict_label(state: &str) -> String {
    match state.to_ascii_uppercase().as_str() {
        "APPROVED" => "approved".to_string(),
        "CHANGES_REQUESTED" => "changes requested".to_string(),
        "DISMISSED" => "dismissed".to_string(),
        "COMMENTED" => String::new(),
        other => other.to_ascii_lowercase().replace('_', " "),
    }
}

fn comment_of(raw: RawComment, kind: CommentKind) -> Comment {
    Comment {
        kind,
        author: author_of(&raw.user),
        created_at: raw.created_at.or(raw.submitted_at).unwrap_or_default(),
        // Trailing blank lines are common in a template-filled description and
        // would otherwise be rendered as empty space.
        body: raw.body.unwrap_or_default().trim_end().to_string(),
        html_url: raw.html_url,
        verdict: raw.state.as_deref().map(verdict_label).unwrap_or_default(),
        path: raw.path.map(PathBuf::from),
        line: raw.line.or(raw.original_line),
    }
}

/// Read a list endpoint page by page. The bool is true when there was more
/// than [`MAX_COMMENT_PAGES`] worth.
async fn get_paged<T: serde::de::DeserializeOwned>(
    token: &str,
    base: &str,
) -> Result<(Vec<T>, bool)> {
    let mut out = Vec::new();
    for page in 1..=MAX_COMMENT_PAGES {
        let url = format!("{base}?per_page=100&page={page}");
        let raw: Vec<T> = get_json(token, &url).await?;
        let full_page = raw.len() == 100;
        out.extend(raw);
        if !full_page {
            return Ok((out, false));
        }
    }
    Ok((out, true))
}

/// A submitted review with nothing to say is just the envelope its line
/// comments arrived in — those are fetched separately, so showing the envelope
/// as well would double every one of them.
fn review_is_noise(raw: &RawComment) -> bool {
    let empty = raw.body.as_deref().unwrap_or_default().trim().is_empty();
    let state = raw
        .state
        .as_deref()
        .unwrap_or_default()
        .to_ascii_uppercase();
    // A pending review is a draft, visible only to the person writing it.
    state == "PENDING" || (empty && state != "APPROVED" && state != "CHANGES_REQUESTED")
}

/// The whole conversation: the discussion, the review summaries, and the
/// comments left on lines of the diff, merged into one list in the order they
/// were written.
///
/// Three requests, because GitHub keeps the three on separate endpoints. A
/// failure on any of them fails the lot — a conversation with a third of itself
/// silently missing is worse than one that says it could not be loaded.
pub async fn pr_comments(token: &str, repo: &RepoRef, number: u64) -> Result<Thread> {
    let owner = encode_segment(&repo.owner);
    let name = encode_segment(&repo.name);

    let mut comments = Vec::new();
    let mut truncated = false;

    let (discussion, more): (Vec<RawComment>, bool) = get_paged(
        token,
        &format!("{API}/repos/{owner}/{name}/issues/{number}/comments"),
    )
    .await
    .with_context(|| format!("reading the discussion on #{number}"))?;
    truncated |= more;
    comments.extend(
        discussion
            .into_iter()
            .map(|c| comment_of(c, CommentKind::Discussion)),
    );

    let (inline, more): (Vec<RawComment>, bool) = get_paged(
        token,
        &format!("{API}/repos/{owner}/{name}/pulls/{number}/comments"),
    )
    .await
    .with_context(|| format!("reading the line comments on #{number}"))?;
    truncated |= more;
    comments.extend(
        inline
            .into_iter()
            .map(|c| comment_of(c, CommentKind::Inline)),
    );

    let (reviews, more): (Vec<RawComment>, bool) = get_paged(
        token,
        &format!("{API}/repos/{owner}/{name}/pulls/{number}/reviews"),
    )
    .await
    .with_context(|| format!("reading the reviews of #{number}"))?;
    truncated |= more;
    comments.extend(
        reviews
            .into_iter()
            .filter(|r| !review_is_noise(r))
            .map(|c| comment_of(c, CommentKind::Review)),
    );

    // ISO 8601 in UTC, which sorts as text. A stable sort keeps a review and
    // the line comments it was submitted with in the order they came back.
    comments.sort_by(|a, b| a.created_at.cmp(&b.created_at));

    Ok(Thread {
        comments,
        truncated,
    })
}

/// One file's bytes from the CDN, named by commit and path.
///
/// Not metered, and no credential to send — which is what makes reading a whole
/// repository this way reasonable, and why the clone tries it first whether or
/// not anyone is signed in. Private repositories 404 here and go to
/// [`api_blob`] instead.
pub async fn raw_file(
    repo: &RepoRef,
    commit: &str,
    path: &std::path::Path,
) -> Result<(u16, Vec<u8>)> {
    let rel = slashed(path);
    let url = format!(
        "{RAW}/{}/{}/{}/{}",
        encode_segment(&repo.owner),
        encode_segment(&repo.name),
        encode_segment(commit),
        encode_path(&rel),
    );
    let reply = http::get(&url, &[]).await?;
    Ok((reply.status, reply.body))
}

/// One blob's bytes from the API, named by its git SHA.
///
/// Reaches private repositories, and costs one of the hour's requests each
/// time. The blob SHA is enough on its own — no commit, no path — because it is
/// what the content hashes to.
pub async fn api_blob(token: &str, repo: &RepoRef, sha: &str) -> Result<(u16, Vec<u8>)> {
    let url = format!(
        "{API}/repos/{}/{}/git/blobs/{}",
        encode_segment(&repo.owner),
        encode_segment(&repo.name),
        encode_segment(sha),
    );
    // The `raw` media type returns the blob's bytes instead of base64-in-JSON.
    get_raw(token, &url, "application/vnd.github.raw").await
}

/// One side of a file, at a specific commit, for a caller that knows the path
/// but not the blob it is made of — a repository whose tree would not load, or
/// a base side with no base tree behind it.
///
/// A 404 means the file does not exist there, which is expected for the base
/// side of an added file. Anonymous callers are sent to [`RAW`]: against the
/// API an unauthenticated browser would spend its whole hourly allowance on a
/// medium pull request, and against the CDN it spends none of it.
pub async fn file_at(
    token: &str,
    repo: &RepoRef,
    sha: &str,
    path: &std::path::Path,
) -> Result<FileContent> {
    let (status, body) = if token.is_empty() {
        raw_file(repo, sha, path).await?
    } else {
        let rel = slashed(path);
        let url = format!(
            "{API}/repos/{}/{}/contents/{}?ref={}",
            encode_segment(&repo.owner),
            encode_segment(&repo.name),
            encode_path(&rel),
            encode_segment(sha),
        );
        get_raw(token, &url, "application/vnd.github.raw").await?
    };

    if status == 404 {
        return Ok(FileContent::Absent);
    }
    if !(200..300).contains(&status) {
        bail!("GitHub returned HTTP {status} for {}", path.display());
    }
    Ok(FileContent::from_bytes(&body))
}

pub fn statuses_of(files: &[PrFile]) -> std::collections::HashMap<PathBuf, ChangeKind> {
    files.iter().map(|f| (f.path.clone(), f.status)).collect()
}

fn find_file<'a>(files: &'a [PrFile], path: &std::path::Path) -> Option<&'a PrFile> {
    files.iter().find(|f| f.path == path)
}

/// A repo-relative path as GitHub spells it. The replace only ever matters to
/// a native run on Windows — and only there is it worth an allocation.
fn slashed(path: &std::path::Path) -> Cow<'_, str> {
    let rel = path.to_string_lossy();
    if rel.contains('\\') {
        Cow::Owned(rel.replace('\\', "/"))
    } else {
        rel
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    /// A moment `secs` from now, as `x-ratelimit-reset` writes it.
    fn reset_in(secs: u64) -> String {
        let now = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        (now + secs).to_string()
    }

    #[test]
    fn a_spent_search_budget_says_so_and_says_what_still_works() {
        let e = format!(
            "{:#}",
            rate_limited("", Some("search"), Some(&reset_in(40)))
        );
        assert!(e.contains("10 repository searches a minute"), "{e}");
        assert!(e.contains("40 seconds"), "{e}");
        // The whole point: the rest of the app is still open for business.
        assert!(e.contains("owner/name"), "{e}");
    }

    #[test]
    fn a_signed_in_search_budget_is_the_larger_one() {
        let e = format!("{:#}", rate_limited("ghp_x", Some("search"), None));
        assert!(e.contains("30 repository searches a minute"), "{e}");
        assert!(!e.contains("signed out"), "{e}");
    }

    #[test]
    fn the_hourly_budget_names_the_token_that_would_raise_it() {
        let e = format!(
            "{:#}",
            rate_limited("", Some("core"), Some(&reset_in(1800)))
        );
        assert!(e.contains("60 API requests an hour"), "{e}");
        assert!(e.contains("5000"), "{e}");
        assert!(e.contains("30 minutes"), "{e}");
    }

    #[test]
    fn a_reset_already_past_is_not_a_wait_of_zero() {
        assert_eq!(seconds_until(Some("1")), None);
        assert_eq!(seconds_until(None), None);
        assert_eq!(seconds_until(Some("not a number")), None);
        let e = format!("{:#}", rate_limited("", Some("core"), Some("1")));
        assert!(e.contains("Try again shortly."), "{e}");
    }

    #[test]
    fn waits_read_in_whichever_unit_is_shorter() {
        assert_eq!(wait_phrase(45), "Try again in 45 seconds.");
        assert_eq!(wait_phrase(90), "Try again in 90 seconds.");
        // Rounded up: "1 minute" that is really 91 seconds sends people back
        // early, and early is another spent request.
        assert_eq!(wait_phrase(91), "Try again in 2 minutes.");
        assert_eq!(wait_phrase(3600), "Try again in 60 minutes.");
    }

    /// A snapshot of `(path, blob)` pairs, sorted the way a real one is.
    fn snapshot(files: &[(&str, &str)]) -> Snapshot {
        let mut files: Vec<TreeEntry> = files
            .iter()
            .map(|(path, sha)| TreeEntry {
                path: PathBuf::from(path),
                sha: sha.to_string(),
                size: 0,
            })
            .collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Snapshot {
            files,
            ..Snapshot::default()
        }
    }

    fn changed(path: &str, status: ChangeKind) -> PrFile {
        PrFile {
            path: PathBuf::from(path),
            previous_path: None,
            status,
            additions: 0,
            deletions: 0,
        }
    }

    /// A pull request with only the three fields `blob_key` reads filled in.
    fn pr_with(head: Snapshot, base: Snapshot, files: Vec<PrFile>) -> PrDetail {
        PrDetail {
            repo: RepoRef::default(),
            number: 1,
            title: String::new(),
            body: String::new(),
            author: String::new(),
            state: String::new(),
            draft: false,
            html_url: String::new(),
            head_ref: String::new(),
            base_ref: String::new(),
            base_sha: String::new(),
            head_sha: String::new(),
            files,
            truncated: false,
            tree: head,
            base_tree: base,
        }
    }

    #[test]
    fn a_file_is_remembered_as_the_blob_it_is_made_of() {
        let pr = pr_with(
            snapshot(&[("src/a.rs", "aaa"), ("src/b.rs", "bbb")]),
            snapshot(&[("src/a.rs", "old")]),
            vec![changed("src/a.rs", ChangeKind::Modified)],
        );
        // The head side: what the file is now, not what it was called.
        assert_eq!(pr.blob_key(Path::new("src/a.rs")), "aaa");
    }

    #[test]
    fn a_deleted_file_is_remembered_as_the_side_it_still_has() {
        let mut renamed = changed("new/name.rs", ChangeKind::Renamed);
        renamed.previous_path = Some(PathBuf::from("old/name.rs"));
        let pr = pr_with(
            snapshot(&[("new/name.rs", "moved")]),
            snapshot(&[("gone.rs", "was-here"), ("old/name.rs", "before")]),
            vec![changed("gone.rs", ChangeKind::Deleted), renamed],
        );
        // Deleted: no head side at all, so the base blob is its identity.
        assert_eq!(pr.blob_key(Path::new("gone.rs")), "was-here");
        // Renamed: it does have a head side, which is the one that counts —
        // moving a file without touching it must not untick it.
        assert_eq!(pr.blob_key(Path::new("new/name.rs")), "moved");
    }

    #[test]
    fn a_pull_request_with_no_tree_falls_back_to_the_path() {
        let pr = pr_with(
            Snapshot::default(),
            Snapshot::default(),
            vec![changed("src/a.rs", ChangeKind::Modified)],
        );
        assert_eq!(pr.blob_key(Path::new("src/a.rs")), "path:src/a.rs");
    }

    #[test]
    fn rewriting_a_file_changes_what_it_is_remembered_as() {
        let before = pr_with(
            snapshot(&[("src/a.rs", "aaa"), ("src/b.rs", "bbb")]),
            Snapshot::default(),
            vec![
                changed("src/a.rs", ChangeKind::Modified),
                changed("src/b.rs", ChangeKind::Modified),
            ],
        );
        // A force-push: `a.rs` was rewritten, `b.rs` was carried over untouched.
        let after = pr_with(
            snapshot(&[("src/a.rs", "zzz"), ("src/b.rs", "bbb")]),
            Snapshot::default(),
            before.files.clone(),
        );
        assert_ne!(
            before.blob_key(Path::new("src/a.rs")),
            after.blob_key(Path::new("src/a.rs")),
            "rewritten, so a tick against it must not carry over"
        );
        assert_eq!(
            before.blob_key(Path::new("src/b.rs")),
            after.blob_key(Path::new("src/b.rs")),
            "byte-identical, so it stays read"
        );
    }

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
    fn rejects_junk() {
        assert!(parse_target("").is_none());
        assert!(parse_target("   ").is_none());
        assert!(parse_target("just-an-owner").is_none());
    }

    #[test]
    fn an_owner_is_read_however_it_arrives() {
        for typed in [
            "torvalds",
            "  torvalds  ",
            "torvalds/",
            "@torvalds",
            "github.com/torvalds",
            "https://github.com/torvalds",
            "https://www.github.com/torvalds/",
            "https://github.com/orgs/torvalds/repositories",
        ] {
            assert_eq!(parse_owner(typed).as_deref(), Some("torvalds"), "{typed}");
        }
    }

    #[test]
    fn a_repository_is_not_an_owner() {
        // Both halves named: that is a target, not an account.
        assert!(parse_owner("rust-lang/rust").is_none());
        assert!(parse_owner("https://github.com/rust-lang/rust/pull/1").is_none());
        // `orgs/` only counts as GitHub's own URL, never as something typed.
        assert_eq!(parse_owner("orgs/rust-lang").as_deref(), None);
    }

    #[test]
    fn only_a_login_shaped_name_is_worth_a_request() {
        assert!(parse_owner("").is_none());
        assert!(parse_owner("two words").is_none());
        assert!(parse_owner("dots.and.such").is_none());
        assert!(parse_owner("-leading").is_none());
        assert!(parse_owner("trailing-").is_none());
        assert!(parse_owner(&"a".repeat(40)).is_none());
        assert_eq!(parse_owner(&"a".repeat(39)).unwrap().len(), 39);
    }

    #[test]
    fn an_owner_hit_keeps_what_tells_two_accounts_apart() {
        let raw: RawOwner = serde_json::from_str(
            r#"{"login":"rust-lang","type":"Organization",
                "name":"The Rust Programming Language","public_repos":142}"#,
        )
        .unwrap();
        let hit = owner_of(raw).unwrap();
        assert!(hit.org);
        assert_eq!(hit.name, "The Rust Programming Language");
        assert_eq!(hit.public_repos, 142);
    }

    #[test]
    fn an_owner_hit_survives_a_bare_response() {
        let raw: RawOwner = serde_json::from_str(r#"{"login":"bigmah"}"#).unwrap();
        let hit = owner_of(raw).unwrap();
        assert!(!hit.org, "no type means a person, which is the common case");
        assert!(hit.name.is_empty());
        assert_eq!(hit.public_repos, 0);

        // A display name that is the login again says nothing twice.
        let raw: RawOwner = serde_json::from_str(r#"{"login":"bigmah","name":"BigMah"}"#).unwrap();
        assert!(owner_of(raw).unwrap().name.is_empty());
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

    #[test]
    fn discussion_comments_carry_who_and_when() {
        let raw: RawComment = serde_json::from_str(
            r#"{"user":{"login":"octocat"},"body":"Looks good to me\n\n",
                "created_at":"2026-08-01T09:12:00Z",
                "html_url":"https://github.com/o/n/pull/1#issuecomment-9"}"#,
        )
        .unwrap();
        let c = comment_of(raw, CommentKind::Discussion);
        assert_eq!(c.author, "octocat");
        assert_eq!(c.created_at, "2026-08-01T09:12:00Z");
        // Trailing blank lines would render as empty space in the pane.
        assert_eq!(c.body, "Looks good to me");
        assert!(c.verdict.is_empty());
        assert_eq!(c.path, None);
    }

    #[test]
    fn reviews_date_themselves_with_submitted_at() {
        let raw: RawComment = serde_json::from_str(
            r#"{"user":{"login":"reviewer"},"body":"","state":"APPROVED",
                "submitted_at":"2026-08-02T10:00:00Z","html_url":"u"}"#,
        )
        .unwrap();
        assert!(
            !review_is_noise(&raw),
            "a bare approval still says something"
        );
        let c = comment_of(raw, CommentKind::Review);
        assert_eq!(c.created_at, "2026-08-02T10:00:00Z");
        assert_eq!(c.verdict, "approved");
    }

    #[test]
    fn empty_reviews_that_only_wrap_line_comments_are_dropped() {
        let wrapper: RawComment =
            serde_json::from_str(r#"{"body":"","state":"COMMENTED","submitted_at":"t"}"#).unwrap();
        assert!(review_is_noise(&wrapper));

        let spoken: RawComment =
            serde_json::from_str(r#"{"body":"one nit","state":"COMMENTED","submitted_at":"t"}"#)
                .unwrap();
        assert!(!review_is_noise(&spoken));

        // A draft nobody else can see.
        let pending: RawComment =
            serde_json::from_str(r#"{"body":"wip","state":"PENDING","submitted_at":"t"}"#).unwrap();
        assert!(review_is_noise(&pending));

        let rejected: RawComment =
            serde_json::from_str(r#"{"body":"","state":"CHANGES_REQUESTED"}"#).unwrap();
        assert!(!review_is_noise(&rejected));
        assert_eq!(verdict_label("CHANGES_REQUESTED"), "changes requested");
    }

    #[test]
    fn line_comments_keep_a_line_to_jump_to() {
        let fresh: RawComment = serde_json::from_str(
            r#"{"user":{"login":"a"},"body":"nit","path":"src/main.rs","line":42,
                "original_line":7,"created_at":"t","html_url":"u"}"#,
        )
        .unwrap();
        let c = comment_of(fresh, CommentKind::Inline);
        assert_eq!(c.path, Some(PathBuf::from("src/main.rs")));
        assert_eq!(c.line, Some(42));

        // Outdated: the lines it was written against are gone, so GitHub sends
        // a null `line` and only remembers where it started out.
        let stale: RawComment = serde_json::from_str(
            r#"{"body":"nit","path":"src/old.rs","line":null,"original_line":7}"#,
        )
        .unwrap();
        let c = comment_of(stale, CommentKind::Inline);
        assert_eq!(c.line, Some(7));
    }

    #[test]
    fn a_pull_request_without_a_description_reads_as_empty() {
        let raw: RawPr = serde_json::from_str(
            r#"{"number":1,"title":"t","body":null,"state":"open","updated_at":"t",
                "html_url":"u","head":{"ref":"h","sha":"1"},"base":{"ref":"b","sha":"2"}}"#,
        )
        .unwrap();
        assert_eq!(raw.body.unwrap_or_default(), "");
    }

    #[test]
    fn a_path_finds_its_own_blob_and_never_a_neighbour() {
        // `-` sorts before `/` as bytes, but a path is compared component by
        // component, where `gguf` sorts before `gguf-rs`. The lookup is a
        // binary search, so it and the sort behind it have to agree about
        // which — and if they ever stop agreeing, the failure is a file served
        // with another file's contents, which nothing downstream can see is
        // wrong. A source file that arrives as a `.gguf` model reads as binary.
        let paths = [
            "README.md",
            "crates/gguf-rs/src/lib.rs",
            "crates/gguf/src/lib.rs",
            "crates/gguf/src/read.rs",
            "crates/gguf/tests/model.gguf",
        ];
        let mut files: Vec<TreeEntry> = paths
            .iter()
            .enumerate()
            .map(|(i, p)| TreeEntry {
                path: PathBuf::from(p),
                sha: format!("{i:040}"),
                size: 10,
            })
            .collect();
        // Exactly what `repo_tree` does with what GitHub sends.
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let snapshot = Snapshot {
            repo: RepoRef::default(),
            commit: "c".into(),
            files,
            truncated: false,
        };

        for (i, path) in paths.iter().enumerate() {
            let found = snapshot.entry(std::path::Path::new(path)).unwrap();
            assert_eq!(found.path, PathBuf::from(path));
            assert_eq!(found.sha, format!("{i:040}"), "{path} found the wrong blob");
        }
        assert!(
            snapshot
                .entry(std::path::Path::new("crates/gguf/src/nope.rs"))
                .is_none()
        );
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

// -------------------------------------------------------------- fetch jobs

/// Everything needed to read one file of a pull request or browsed repository,
/// lifted out of app state so the read can travel.
///
/// It carries both what the file is called and what it hashes to. The names are
/// what the CDN answers to; the hashes are what the local store is keyed by, so
/// a job whose blobs are already on disk needs no network at all — see
/// [`clone::read_pair`](super::clone::read_pair).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct FetchJob {
    pub repo: RepoRef,
    pub base_sha: String,
    pub head_sha: String,
    pub path: PathBuf,
    /// Differs from `path` for renames.
    pub base_path: PathBuf,
    /// The git blob at each side, when a tree was read to say what it is —
    /// which is both what the local store is keyed by and how big the answer
    /// should turn out to be. `None` falls back to reading by path.
    pub head_blob: Option<TreeEntry>,
    pub base_blob: Option<TreeEntry>,
    /// `None` for a file the PR does not touch — browsable, but not a diff.
    pub status: Option<ChangeKind>,
}

impl FetchJob {
    /// For any path in the PR's repository, changed or not.
    pub fn new(pr: &PrDetail, rel: &std::path::Path) -> Self {
        // Unchanged files are in the repo tree but not the PR's file list.
        Self::build(pr, rel, find_file(&pr.files, rel))
    }

    /// [`new`](Self::new) for a caller already holding the changed file.
    /// The clone makes one of these per changed file, and looking each one up
    /// in the list it came from would walk that list once per entry.
    pub fn for_changed<'a>(pr: &'a PrDetail, f: &'a PrFile) -> Self {
        Self::build(pr, &f.path, Some(f))
    }

    fn build(pr: &PrDetail, rel: &std::path::Path, f: Option<&PrFile>) -> Self {
        let base_path = f.map_or_else(|| rel.to_path_buf(), |f| f.base_path().clone());
        FetchJob {
            repo: pr.repo.clone(),
            base_sha: pr.base_sha.clone(),
            head_sha: pr.head_sha.clone(),
            head_blob: pr.tree.entry(rel).cloned(),
            base_blob: pr.base_tree.entry(&base_path).cloned(),
            path: rel.to_path_buf(),
            base_path,
            status: f.map(|f| f.status),
        }
    }

    /// For a path in a repository being browsed on its own. Nothing is changed
    /// here, so there is only ever one side to read.
    pub fn browsing(view: &RepoView, rel: &std::path::Path) -> Self {
        FetchJob {
            repo: view.repo.clone(),
            base_sha: String::new(),
            head_sha: view.head_sha.clone(),
            head_blob: view.tree.entry(rel).cloned(),
            base_blob: None,
            path: rel.to_path_buf(),
            base_path: rel.to_path_buf(),
            status: None,
        }
    }

    /// Whether this side is worth reading at all.
    ///
    /// An added file has no base side, a deleted one has no head side, and an
    /// untouched file is never diffed — so in each case a read would be wasted
    /// or 404 anyway.
    pub fn wants_base(&self) -> bool {
        !matches!(self.status, None | Some(ChangeKind::Added))
    }

    pub fn wants_head(&self) -> bool {
        self.status != Some(ChangeKind::Deleted)
    }
}
