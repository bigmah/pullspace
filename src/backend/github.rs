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
/// GitHub answers at most 250 commits on a pull request, so three pages is
/// every one there is to have from this endpoint.
const MAX_COMMIT_PAGES: u32 = 3;
/// And at most 300 files on a single commit, which is the same three pages.
const MAX_COMMIT_FILE_PAGES: u32 = 3;
/// And at most 300 on a comparison — all on the first page of it, whatever
/// `per_page` says, which is why this is a count and not a number of pages.
const MAX_COMPARE_FILES: usize = 300;
/// 100 check runs a page. Three pages is three hundred jobs on one commit —
/// past anything a person reads the results of one by one, and the panel says
/// when it stopped there.
const MAX_CHECK_PAGES: u32 = 3;
/// And 200 marked-up lines from any one check. A build that failed in two
/// hundred places has said what it has to say.
const MAX_ANNOTATION_PAGES: u32 = 2;
/// How many pull requests one listing holds. GitHub's own maximum per page,
/// asked for in one request — a second page of a list nobody scrolls to the
/// bottom of is not worth the wait or the budget.
pub const PR_PAGE: usize = 100;
/// 100 branches a page. Three pages is three hundred of them — more than any
/// list is read down, and the pane says when it stopped there.
const MAX_BRANCH_PAGES: u32 = 3;
/// How many commits of a branch's history arrive at once. A branch is not a
/// pull request: there is no end to it, so it comes down a page at a time and
/// the list asks for the next one when somebody scrolls to the bottom of this
/// one.
pub const HISTORY_PAGE: usize = 100;

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

/// Also the encoder behind `backend::route`'s share links — the unreserved set
/// is the same on both sides of the address bar.
pub(crate) fn encode_segment(seg: &str) -> String {
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

/// The commit one piece of text names, when it names one: `owner/repo` and a
/// hex sha after the word github.com writes it under.
///
/// `owner/repo/commit/<sha>` as typed, and the browser URL it came from. It is
/// checked before [`parse_target`] wherever both could answer, since that reads
/// the same text as a bare repository with something after it.
pub fn parse_commit_target(input: &str) -> Option<(RepoRef, String)> {
    let s = input.trim();
    let rest = strip_host(s).unwrap_or(s);
    let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
    match parts.as_slice() {
        [owner, name, "commit" | "commits", sha, ..] if is_sha(sha) => Some((
            RepoRef {
                owner: owner.to_string(),
                name: name.trim_end_matches(".git").to_string(),
            },
            sha.to_string(),
        )),
        _ => None,
    }
}

/// A commit, as git lets one be written: hex, and enough of it to be worth
/// resolving. Seven is what everybody quotes and what GitHub's own links use;
/// forty is the whole hash.
pub fn is_sha(s: &str) -> bool {
    (7..=40).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
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

    let sha = branch_head(token, repo, &branch)
        .await
        .with_context(|| format!("{repo} may have no commits yet"))?;

    Ok(RepoHead { branch, sha })
}

/// The commit at the tip of one branch.
///
/// The same endpoint a sha goes to: git's names for a commit and the commit
/// itself are interchangeable there, which is what lets a link naming a branch
/// open the way a link naming a commit does — and what makes `⟳` on a branch
/// pick up whatever has been pushed to it since.
pub async fn branch_head(token: &str, repo: &RepoRef, branch: &str) -> Result<String> {
    let raw: RawCommit = get_json(
        token,
        &format!(
            "{API}/repos/{}/{}/commits/{}",
            encode_segment(&repo.owner),
            encode_segment(&repo.name),
            encode_ref(branch),
        ),
    )
    .await
    .with_context(|| format!("reading the tip of {branch}"))?;
    Ok(raw.sha)
}

/// One branch: what it is called, and the commit at its tip.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Branch {
    pub name: String,
    pub sha: String,
    /// Whether GitHub refuses pushes straight at it — which is nearly always
    /// the branch everything else is merged into.
    ///
    /// False for one found by [`matching_branches`], which answers out of
    /// GitHub's index of refs and says nothing about protection. Unknown
    /// rather than untrue, and it costs a pill on a row rather than anything
    /// that could mislead.
    pub protected: bool,
}

/// A repository's branches, by name, as GitHub keeps them.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Branches {
    pub items: Vec<Branch>,
    /// More branches than [`MAX_BRANCH_PAGES`] holds, and this says so.
    pub truncated: bool,
}

#[derive(Deserialize)]
struct RawBranch {
    #[serde(default)]
    name: String,
    #[serde(default)]
    commit: RawCommit,
    #[serde(default)]
    protected: bool,
}

/// One ref as the refs endpoints write it: the whole `refs/heads/…` name, and
/// the object it points at.
#[derive(Deserialize)]
struct RawMatchingRef {
    #[serde(rename = "ref", default)]
    name: String,
    #[serde(default)]
    object: RawCommit,
}

/// What a ref is called once it is not a ref: `refs/heads/dev` is the branch
/// `dev`, and anything else under `refs/` is not a branch at all.
fn branch_of_ref(raw: RawMatchingRef) -> Option<Branch> {
    let name = raw.name.strip_prefix("refs/heads/")?;
    (!name.is_empty()).then(|| Branch {
        name: name.to_string(),
        sha: raw.object.sha,
        protected: false,
    })
}

/// The branches whose names begin with `prefix`, asked of GitHub rather than of
/// the list already in hand.
///
/// A repository with thousands of branches is one [`list_branches`] stops short
/// of, and a filter over what was fetched cannot find what was not fetched:
/// microsoft/vscode has some forty-eight hundred branches, of which the list
/// holds the first three hundred. This is the way to the rest — one request,
/// answered straight out of GitHub's index of refs, and cheap enough to spend
/// on somebody typing.
///
/// Prefix, and only prefix, and case-sensitively: the REST API has no substring
/// search for refs, so `ocr` will not find `client-ocr-telemetry` and `dileepy`
/// will not find `DileepY/1.109` — both confirmed against microsoft/vscode. The
/// pane says so rather than letting either read as "no such branch".
pub async fn matching_branches(token: &str, repo: &RepoRef, prefix: &str) -> Result<Vec<Branch>> {
    let prefix = prefix.trim();
    // No prefix matches every ref there is, which is the request this exists to
    // avoid making.
    if prefix.is_empty() {
        return Ok(Vec::new());
    }
    let url = format!(
        "{API}/repos/{}/{}/git/matching-refs/heads/{}?per_page=100",
        encode_segment(&repo.owner),
        encode_segment(&repo.name),
        encode_ref(prefix),
    );
    let raw: Vec<RawMatchingRef> = get_json(token, &url)
        .await
        .with_context(|| format!("looking for branches starting with {prefix}"))?;
    Ok(raw.into_iter().filter_map(branch_of_ref).collect())
}

/// Every branch of a repository — what the pane lists, and what each row is a
/// way into.
///
/// In GitHub's own order, which is by name, and the only one that costs
/// nothing: sorting by when each was last pushed to would be a request per
/// branch. The first [`MAX_BRANCH_PAGES`] pages of it, which on a repository
/// with thousands of branches is a small part of the front — so a list that
/// says it was cut short is one to search rather than to scroll, and
/// [`matching_branches`] is what searches it.
pub async fn list_branches(token: &str, repo: &RepoRef) -> Result<Branches> {
    let base = format!(
        "{API}/repos/{}/{}/branches",
        encode_segment(&repo.owner),
        encode_segment(&repo.name),
    );
    let (raw, truncated): (Vec<RawBranch>, bool) = get_paged(token, &base, MAX_BRANCH_PAGES)
        .await
        .with_context(|| format!("reading the branches of {repo}"))?;
    Ok(Branches {
        items: raw
            .into_iter()
            .filter(|b| !b.name.is_empty())
            .map(|b| Branch {
                name: b.name,
                sha: b.commit.sha,
                protected: b.protected,
            })
            .collect(),
        truncated,
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
    /// Whether that is the repository's default branch — which is what a link
    /// with no branch written in it opens, and so what decides which of the two
    /// forms the address bar takes. See [`Target`](super::route::Target).
    pub default: bool,
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

/// A git ref as part of a URL path.
///
/// Segment by segment, because a branch called `feat/thing` is two segments of
/// the path and not one escaped string — GitHub answers `commits/feat/thing`
/// and not `commits/feat%2Fthing`.
fn encode_ref(name: &str) -> String {
    name.split('/')
        .filter(|s| !s.is_empty())
        .map(encode_segment)
        .collect::<Vec<_>>()
        .join("/")
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
    /// Set only on a pull request that was closed by being merged, which is the
    /// one thing `state` does not say.
    #[serde(default)]
    merged_at: Option<String>,
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
    /// `open` or `closed`, as GitHub says it.
    pub state: String,
    /// Closed by landing rather than by being turned down. GitHub calls both
    /// `closed`, and they are not the same news.
    pub merged: bool,
    pub updated_at: String,
    pub head_ref: String,
    pub base_ref: String,
}

impl PrSummary {
    pub fn is_open(&self) -> bool {
        self.state == "open"
    }
}

/// Which of a repository's pull requests to ask for.
///
/// GitHub's own three, in GitHub's own words — a list that offers anything else
/// is a list somebody has to work out the meaning of.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum PrState {
    #[default]
    Open,
    /// Turned down or landed: GitHub calls both of those closed.
    Closed,
    All,
}

impl PrState {
    /// The three, in the order they are offered.
    pub const EVERY: [PrState; 3] = [PrState::Open, PrState::Closed, PrState::All];

    /// What GitHub calls it, which is also what the button says.
    pub fn label(self) -> &'static str {
        match self {
            PrState::Open => "open",
            PrState::Closed => "closed",
            PrState::All => "all",
        }
    }

    pub fn why(self) -> &'static str {
        match self {
            PrState::Open => "Pull requests still open",
            PrState::Closed => "Pull requests that have been merged or turned down",
            PrState::All => "Every pull request, open or closed",
        }
    }
}

fn author_of(user: &Option<User>) -> String {
    user.as_ref()
        .map(|u| u.login.clone())
        .unwrap_or_else(|| "ghost".to_string())
}

/// A repository's pull requests, most recently updated first.
///
/// One page, which is [`PR_PAGE`] of them — enough that "the pull requests on
/// this repository" is answered in full for very nearly every repository, and
/// the caller is told which are the ones it is not.
pub async fn list_prs(token: &str, repo: &RepoRef, state: PrState) -> Result<Vec<PrSummary>> {
    let url = format!(
        "{API}/repos/{}/{}/pulls?state={}&sort=updated&direction=desc&per_page={PR_PAGE}",
        encode_segment(&repo.owner),
        encode_segment(&repo.name),
        state.label(),
    );
    let raw: Vec<RawPr> = get_json(token, &url).await?;
    Ok(raw.into_iter().map(summary_of).collect())
}

fn summary_of(p: RawPr) -> PrSummary {
    PrSummary {
        number: p.number,
        title: p.title,
        author: author_of(&p.user),
        draft: p.draft,
        merged: p.merged_at.is_some(),
        state: p.state,
        updated_at: p.updated_at,
        head_ref: p.head.name,
        base_ref: p.base.name,
    }
}

#[derive(Deserialize)]
struct RawFile {
    filename: String,
    status: String,
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

/// One entry of a changed-file list, whether it came from a pull request or
/// from a single commit — GitHub writes both the same way.
fn file_of(f: &RawFile) -> PrFile {
    PrFile {
        path: PathBuf::from(&f.filename),
        previous_path: f.previous_filename.as_deref().map(PathBuf::from),
        status: change_kind(&f.status),
    }
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct PrFile {
    pub path: PathBuf,
    /// Set for renames — the path to read on the base side.
    pub previous_path: Option<PathBuf>,
    pub status: ChangeKind,
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

    /// Whether anything in the repository is inside this directory.
    ///
    /// Directories are not entries of their own — the tree is rebuilt from the
    /// blob paths alone, see [`repo_tree`] — so a directory is a path some file
    /// is under. Component by component, so `src` is the front of `src/main.rs`
    /// and not of `srcery/lib.rs`.
    pub fn has_dir(&self, dir: &std::path::Path) -> bool {
        !dir.as_os_str().is_empty() && self.files.iter().any(|f| f.path.starts_with(dir))
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
    #[serde(default)]
    base_commit: RawCommit,
    /// `identical`, `ahead`, `behind` or `diverged`, from the base's point of
    /// view.
    #[serde(default)]
    status: String,
    #[serde(default)]
    ahead_by: u32,
    #[serde(default)]
    behind_by: u32,
    /// Every commit between them, not only the ones on this page.
    #[serde(default)]
    total_commits: u32,
    #[serde(default)]
    commits: Vec<RawPrCommit>,
    #[serde(default)]
    files: Vec<RawFile>,
    #[serde(default)]
    html_url: String,
}

#[derive(Deserialize, Default)]
struct RawCommit {
    #[serde(default)]
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

/// What one changed file is remembered as, once somebody has marked it read:
/// the git blob it is made of.
///
/// The blob rather than the path, because a mark is a statement about
/// contents — see [`viewed`](crate::backend::viewed). Nearly always the head
/// side; the base side for a file the change deletes, which has no head side to
/// hash. Falling back to the path covers the one case with no blob at all: a
/// tree GitHub would not serve, where a mark keyed by name is still better than
/// no marks.
///
/// A free function because a commit is diffed the same way a pull request is,
/// and both hold the same three things to answer it with.
fn blob_key_in<'a>(
    tree: &'a Snapshot,
    base_tree: &'a Snapshot,
    files: &[PrFile],
    path: &std::path::Path,
) -> Cow<'a, str> {
    if let Some(entry) = tree.entry(path) {
        return Cow::Borrowed(entry.sha.as_str());
    }
    find_file(files, path)
        .and_then(|f| base_tree.entry(f.base_path()))
        .map_or_else(
            || Cow::Owned(format!("path:{}", path.display())),
            |entry| Cow::Borrowed(entry.sha.as_str()),
        )
}

/// [`blob_key_in`] for a caller already holding the changed file — asked once
/// per file when the read count is taken, where the lookup above would fall
/// back to a scan of the whole list each time.
fn blob_key_of_in<'a>(tree: &'a Snapshot, base_tree: &'a Snapshot, f: &PrFile) -> Cow<'a, str> {
    if let Some(entry) = tree.entry(&f.path) {
        return Cow::Borrowed(entry.sha.as_str());
    }
    base_tree.entry(f.base_path()).map_or_else(
        || Cow::Owned(format!("path:{}", f.path.display())),
        |entry| Cow::Borrowed(entry.sha.as_str()),
    )
}

impl PrDetail {
    pub fn blob_key(&self, path: &std::path::Path) -> Cow<'_, str> {
        blob_key_in(&self.tree, &self.base_tree, &self.files, path)
    }

    pub fn blob_key_of(&self, f: &PrFile) -> Cow<'_, str> {
        blob_key_of_in(&self.tree, &self.base_tree, f)
    }

    /// The half of a pull request that is worth keeping hold of while one of
    /// its commits is on screen — see [`PrHeader`].
    pub fn header(&self) -> PrHeader {
        PrHeader {
            number: self.number,
            title: self.title.clone(),
            body: self.body.clone(),
            author: self.author.clone(),
            draft: self.draft,
            html_url: self.html_url.clone(),
        }
    }
}

/// A pull request, as much of it as anything other than the diff needs.
///
/// It travels with a commit opened out of one, which is what lets the
/// conversation pane stay whole while a single commit is being read — the
/// description, the discussion and the list of commits all belong to the pull
/// request, not to whichever of its commits is on screen. Strings only: the
/// trees and the changed files are the part that is expensive, and they are the
/// part a commit view is replacing.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct PrHeader {
    pub number: u64,
    pub title: String,
    pub body: String,
    pub author: String,
    pub draft: bool,
    pub html_url: String,
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
        files.extend(raw.iter().map(file_of));
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

/// Read a list endpoint page by page. The bool is true when there was more than
/// `pages` worth — which every caller has to say something about, since a list
/// silently cut off is a list read as complete.
async fn get_paged<T: serde::de::DeserializeOwned>(
    token: &str,
    base: &str,
    pages: u32,
) -> Result<(Vec<T>, bool)> {
    let mut out = Vec::new();
    for page in 1..=pages {
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
        MAX_COMMENT_PAGES,
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
        MAX_COMMENT_PAGES,
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
        MAX_COMMENT_PAGES,
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

// ----------------------------------------------------------------- commits

/// One commit of a pull request, as the list of them reads.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct CommitSummary {
    pub sha: String,
    /// The whole message. Its first line is the subject and the rest is the
    /// body — split by [`subject`](Self::subject) and [`body`](Self::body)
    /// rather than at the seam, so nothing anybody wrote is thrown away.
    pub message: String,
    /// The GitHub account that wrote it, or — for a commit whose author has no
    /// account here — the name git has for them.
    pub author: String,
    /// ISO 8601, as git recorded it.
    pub date: String,
    pub html_url: String,
}

/// The seven characters everybody actually says a commit by.
pub fn short_sha(sha: &str) -> &str {
    let end = sha.char_indices().nth(7).map_or(sha.len(), |(i, _)| i);
    &sha[..end]
}

impl CommitSummary {
    /// The seven characters everybody actually says a commit by.
    pub fn short(&self) -> &str {
        short_sha(&self.sha)
    }

    /// The first line, which is what a list of commits is a list of.
    pub fn subject(&self) -> &str {
        self.message.lines().next().unwrap_or_default().trim_end()
    }

    /// Everything after it, empty for the one-line message most commits are.
    pub fn body(&self) -> &str {
        match self.message.split_once('\n') {
            Some((_, rest)) => rest.trim(),
            None => "",
        }
    }
}

/// A list of commits: everything on a pull request, oldest first — the order
/// they were written in, which is the order they are read in — or a page of a
/// branch's history, newest first, which is the order `git log` writes it in.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct Commits {
    pub items: Vec<CommitSummary>,
    /// There is more than this. On a pull request that means GitHub's own limit
    /// of 250 was reached and the rest cannot be had; on a branch it means the
    /// last page came back full, and the next one is a request away.
    pub truncated: bool,
    /// How many pages of a branch's history are in `items`. Zero for a pull
    /// request, whose commits arrive in one go — see [`branch_commits`].
    #[serde(default)]
    pub pages: u32,
    /// How many there are in all, where that is known — a comparison is asked
    /// and answers, and `5798` next to a hundred rows is worth saying. Zero
    /// where nobody said, which is everywhere else.
    #[serde(default)]
    pub total: u32,
}

#[derive(Deserialize)]
struct RawPrCommit {
    #[serde(default)]
    sha: String,
    #[serde(default)]
    commit: RawCommitBody,
    /// The GitHub account, when the email on the commit belongs to one.
    #[serde(default)]
    author: Option<User>,
    #[serde(default)]
    html_url: String,
}

#[derive(Deserialize, Default)]
struct RawCommitBody {
    #[serde(default)]
    message: String,
    #[serde(default)]
    author: Option<RawSignature>,
}

/// Git's own idea of who wrote something and when — a name and a date typed
/// into a commit, with no account behind either.
#[derive(Deserialize)]
struct RawSignature {
    #[serde(default)]
    name: String,
    #[serde(default)]
    date: String,
}

fn commit_of(raw: RawPrCommit) -> CommitSummary {
    let signature = raw.commit.author;
    let named = signature
        .as_ref()
        .map(|a| a.name.clone())
        .unwrap_or_default();
    // The account first: it is the name the rest of the pull request is written
    // under. The git author is the fallback for a commit written from an email
    // address GitHub does not know.
    let author = raw
        .author
        .map(|u| u.login)
        .filter(|login| !login.is_empty())
        .or(Some(named))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    CommitSummary {
        sha: raw.sha,
        message: raw.commit.message,
        author,
        date: signature.map(|a| a.date).unwrap_or_default(),
        html_url: raw.html_url,
    }
}

/// What a commit was opened out of — and so what the pane beside it goes on
/// showing while it is read.
///
/// A commit is nearly always reached from a list of them, and that list is the
/// context for reading it: the pull request whose branch it is on, or the
/// branch whose history it is part of. Either travels along inside the commit
/// so that stepping into one does not empty the pane the row was clicked in.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub enum CommitFrom {
    /// A link, or a sha pasted into the picker: nothing around it.
    #[default]
    Alone,
    /// One commit of a pull request — whose conversation stays beside it.
    Pr(PrHeader),
    /// One commit of a branch's history — which stays beside it.
    Branch(String),
    /// One commit out of a comparison — which stays beside it, base first.
    Compare(String, String),
}

/// One commit, opened the way a pull request is: the files it changes, diffed
/// against the commit before it.
///
/// The same shape as a [`PrDetail`] where it matters — two trees, a
/// changed-file list, and the two commits they belong to — because everything
/// downstream of that is the same work. What it is *not* is a pull request:
/// there is no conversation on a commit and no merge base under it, and the
/// pull request it was opened out of rides along in `pr` rather than being
/// reconstructed from it.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct CommitView {
    pub repo: RepoRef,
    pub commit: CommitSummary,
    /// The first parent — what this is diffed against. Empty for the first
    /// commit in a repository, which has nothing before it.
    pub parent_sha: String,
    /// More than one parent. Worth saying: GitHub lists no files for most merge
    /// commits, and an empty diff with no explanation reads as a bug.
    pub merge: bool,
    pub files: Vec<PrFile>,
    /// GitHub stops at 300 files on a commit, and this says when it did.
    pub truncated: bool,
    /// Every file in the repository at this commit, so the explorer shows the
    /// whole tree rather than only what the commit touched. Filled in by the
    /// caller, as [`PrDetail`]'s is.
    pub tree: Snapshot,
    /// And at the parent, which every left-hand side is read from.
    pub base_tree: Snapshot,
    /// What this was opened out of, when it was opened out of anything.
    pub from: CommitFrom,
}

impl CommitView {
    /// The pull request this commit belongs to, when it was read out of one.
    pub fn pr(&self) -> Option<&PrHeader> {
        match &self.from {
            CommitFrom::Pr(pr) => Some(pr),
            _ => None,
        }
    }

    /// The branch whose history it was read out of, when it was read out of
    /// one.
    pub fn branch(&self) -> Option<&str> {
        match &self.from {
            CommitFrom::Branch(name) => Some(name),
            _ => None,
        }
    }

    /// And the comparison it was read out of, when it was read out of one.
    pub fn compare(&self) -> Option<(&str, &str)> {
        match &self.from {
            CommitFrom::Compare(base, head) => Some((base, head)),
            _ => None,
        }
    }

    pub fn blob_key(&self, path: &std::path::Path) -> Cow<'_, str> {
        blob_key_in(&self.tree, &self.base_tree, &self.files, path)
    }

    pub fn blob_key_of(&self, f: &PrFile) -> Cow<'_, str> {
        blob_key_of_in(&self.tree, &self.base_tree, f)
    }

    /// Where this is on github.com.
    pub fn html_url(&self) -> String {
        if self.commit.html_url.is_empty() {
            return format!(
                "https://github.com/{}/{}/commit/{}",
                self.repo.owner, self.repo.name, self.commit.sha
            );
        }
        self.commit.html_url.clone()
    }
}

#[derive(Deserialize)]
struct RawCommitDetail {
    #[serde(default)]
    sha: String,
    #[serde(default)]
    commit: RawCommitBody,
    #[serde(default)]
    author: Option<User>,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    parents: Vec<RawCommit>,
    #[serde(default)]
    files: Vec<RawFile>,
}

/// One commit and what it changed.
///
/// The file list is paged because GitHub's is: it answers up to 300 files on a
/// commit, a hundred to a page, and repeats the commit itself on each of them.
pub async fn load_commit(token: &str, repo: &RepoRef, sha: &str) -> Result<CommitView> {
    let base = format!(
        "{API}/repos/{}/{}/commits/{}",
        encode_segment(&repo.owner),
        encode_segment(&repo.name),
        encode_segment(sha),
    );

    let mut head: Option<RawCommitDetail> = None;
    let mut files = Vec::new();
    let mut truncated = false;
    for page in 1..=MAX_COMMIT_FILE_PAGES {
        let raw: RawCommitDetail = get_json(token, &format!("{base}?per_page=100&page={page}"))
            .await
            .with_context(|| format!("reading commit {sha}"))?;
        let full_page = raw.files.len() == 100;
        files.extend(raw.files.iter().map(file_of));
        if head.is_none() {
            head = Some(raw);
        }
        if !full_page {
            break;
        }
        if page == MAX_COMMIT_FILE_PAGES {
            truncated = true;
        }
    }
    // Only reachable with `MAX_COMMIT_FILE_PAGES` set to zero, which it is not.
    let raw = head.context("GitHub said nothing about that commit")?;

    let mut parents = raw.parents.iter();
    let parent_sha = parents.next().map(|p| p.sha.clone()).unwrap_or_default();
    let merge = parents.next().is_some();
    // The endpoint answers with the full sha whatever was asked for, which is
    // what everything downstream should be keyed by.
    let sha = if raw.sha.is_empty() {
        sha.to_string()
    } else {
        raw.sha.clone()
    };

    Ok(CommitView {
        tree: Snapshot::unknown(repo, &sha),
        base_tree: Snapshot::unknown(repo, &parent_sha),
        repo: repo.clone(),
        commit: commit_of(RawPrCommit {
            sha,
            commit: raw.commit,
            author: raw.author,
            html_url: raw.html_url,
        }),
        parent_sha,
        merge,
        files,
        truncated,
        from: CommitFrom::Alone,
    })
}

/// Two refs of one repository, and everything that lies between them.
///
/// The same shape as a [`PrDetail`] where it matters — two trees, a changed
/// file list and the commits behind it — because it is the same question a
/// pull request asks, without anybody having opened one. What it compares
/// against is the merge base and not the tip of `base`, which is the comparison
/// github.com's own compare page shows and the one that answers "what does this
/// branch add" rather than "how do these two differ right now".
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct CompareView {
    pub repo: RepoRef,
    /// What is being compared into — the left-hand side, and the side whose
    /// version of a file the diff reads as "before".
    pub base: String,
    /// And what is being compared in.
    pub head: String,
    /// The merge base: where the two last agreed, and what every diff here is
    /// read against.
    pub base_sha: String,
    /// Where `head` points now.
    pub head_sha: String,
    /// And where `base` points now, which is not what anything is diffed
    /// against — it is what `behind` is counted from.
    pub base_tip: String,
    /// GitHub's word for how the two stand: `identical`, `ahead`, `behind` or
    /// `diverged`.
    pub status: String,
    /// How many commits `head` has that `base` does not, and the other way
    /// about.
    pub ahead: u32,
    pub behind: u32,
    pub files: Vec<PrFile>,
    /// GitHub stops at 300 files on a comparison, and this says when it did.
    pub truncated: bool,
    /// Every file in the repository at `head_sha`, so the explorer shows the
    /// whole thing rather than only what differs. Filled in by the caller, as
    /// [`PrDetail`]'s is.
    pub tree: Snapshot,
    /// And at the merge base, which every left-hand side is read from.
    pub base_tree: Snapshot,
    /// The commits between them, oldest first — the first page of them, which
    /// arrives with the comparison itself and costs nothing extra.
    pub commits: Commits,
    pub html_url: String,
}

impl CompareView {
    pub fn blob_key(&self, path: &std::path::Path) -> Cow<'_, str> {
        blob_key_in(&self.tree, &self.base_tree, &self.files, path)
    }

    pub fn blob_key_of(&self, f: &PrFile) -> Cow<'_, str> {
        blob_key_of_in(&self.tree, &self.base_tree, f)
    }

    /// How the two stand, in words — what the bar says beside the two names.
    ///
    /// Both numbers where both are interesting, because "3 ahead" on its own
    /// reads as the whole story on a branch that is also 40 behind.
    pub fn summary(&self) -> String {
        let commits = |n: u32| if n == 1 { "commit" } else { "commits" };
        match (self.ahead, self.behind) {
            (0, 0) => "identical".to_string(),
            (ahead, 0) => format!("{ahead} {} ahead", commits(ahead)),
            (0, behind) => format!("{behind} {} behind", commits(behind)),
            (ahead, behind) => format!("{ahead} ahead · {behind} behind"),
        }
    }

    /// Whether there is anything in `head` that `base` does not already have.
    /// A comparison with nothing in it is not a broken one, and the bar says
    /// which of the two reasons it is.
    pub fn is_empty(&self) -> bool {
        self.ahead == 0
    }

    /// Where this is on github.com.
    pub fn html_url(&self) -> String {
        if self.html_url.is_empty() {
            return format!(
                "https://github.com/{}/{}/compare/{}...{}",
                self.repo.owner, self.repo.name, self.base, self.head
            );
        }
        self.html_url.clone()
    }
}

/// Compare two refs: what lies between them, and the files that differ.
///
/// One request. GitHub answers the whole comparison on the first page — up to
/// 300 files, and the first hundred commits with it — so the pane that lists
/// those commits costs nothing to open. Pages past the first carry commits
/// alone, which is what [`compare_commits`] asks for.
pub async fn load_compare(
    token: &str,
    repo: &RepoRef,
    base: &str,
    head: &str,
) -> Result<CompareView> {
    let raw: RawCompare = get_json(token, &compare_url(repo, base, head, 1))
        .await
        .with_context(|| format!("comparing {base} with {head}"))?;

    // Where `head` actually is. The commits come back oldest first, so the last
    // of them is the head — but only when they all came back: past a hundred,
    // the last one on this page is somewhere in the middle, and a tree read at
    // it would show the repository half way along the comparison.
    let complete = raw.total_commits as usize <= raw.commits.len();
    let last = raw.commits.last().map(|c| c.sha.clone());
    let head_sha = match last {
        // Nothing between them: whatever `head` names is the merge base itself.
        None => raw.merge_base_commit.sha.clone(),
        Some(sha) if complete && !sha.is_empty() => sha,
        Some(sha) => match branch_head(token, repo, head).await {
            Ok(resolved) => resolved,
            // A ref this repository cannot resolve on its own, which is what a
            // comparison across forks arrives as — `owner:branch`. The last
            // commit on the page is not the head, and the explorer will show
            // the repository as of it; the list of what differs, which is what
            // the comparison is for, is right either way.
            Err(_) => sha,
        },
    };
    let base_sha = raw.merge_base_commit.sha;

    Ok(CompareView {
        tree: Snapshot::unknown(repo, &head_sha),
        base_tree: Snapshot::unknown(repo, &base_sha),
        repo: repo.clone(),
        base: base.to_string(),
        head: head.to_string(),
        base_tip: raw.base_commit.sha,
        status: raw.status,
        ahead: raw.ahead_by,
        behind: raw.behind_by,
        truncated: raw.files.len() >= MAX_COMPARE_FILES,
        files: raw.files.iter().map(file_of).collect(),
        commits: Commits {
            truncated: raw.commits.len() == HISTORY_PAGE,
            total: raw.total_commits,
            items: raw.commits.into_iter().map(commit_of).collect(),
            pages: 1,
        },
        html_url: raw.html_url,
        base_sha,
        head_sha,
    })
}

/// One more page of the commits between two refs, for the pane that lists them.
///
/// Oldest first, as the comparison itself is — so the page after the first is
/// the commits *after* the ones already read, and the button that asks for it
/// says so.
pub async fn compare_commits(
    token: &str,
    repo: &RepoRef,
    base: &str,
    head: &str,
    page: u32,
) -> Result<Commits> {
    let raw: RawCompare = get_json(token, &compare_url(repo, base, head, page))
        .await
        .with_context(|| format!("reading the commits between {base} and {head}"))?;
    Ok(Commits {
        truncated: raw.commits.len() == HISTORY_PAGE,
        total: raw.total_commits,
        items: raw.commits.into_iter().map(commit_of).collect(),
        pages: page,
    })
}

/// `base...head`, three dots, as github.com writes it and as the API reads it.
///
/// Three rather than two on purpose: it is the comparison against where the two
/// last agreed, so what it shows is what `head` adds rather than everything
/// that has happened on `base` since. Git forbids `..` inside a ref name, which
/// is what makes the separator unambiguous however the two are called.
fn compare_url(repo: &RepoRef, base: &str, head: &str, page: u32) -> String {
    format!(
        "{API}/repos/{}/{}/compare/{}...{}?per_page={HISTORY_PAGE}&page={page}",
        encode_segment(&repo.owner),
        encode_segment(&repo.name),
        encode_ref(base),
        encode_ref(head),
    )
}

/// Every commit on a pull request — what the branch is made of, rather than
/// what it adds up to.
pub async fn pr_commits(token: &str, repo: &RepoRef, number: u64) -> Result<Commits> {
    let base = format!(
        "{API}/repos/{}/{}/pulls/{number}/commits",
        encode_segment(&repo.owner),
        encode_segment(&repo.name),
    );
    let (raw, truncated): (Vec<RawPrCommit>, bool) = get_paged(token, &base, MAX_COMMIT_PAGES)
        .await
        .with_context(|| format!("reading the commits on #{number}"))?;
    Ok(Commits {
        items: raw.into_iter().map(commit_of).collect(),
        truncated,
        pages: 0,
        // GitHub does not say how many a pull request has beyond the ones it
        // hands over; `truncated` is the whole of what it will admit.
        total: 0,
    })
}

/// One page of a branch's history, newest first — `git log`, as a list.
///
/// A branch does not end the way a pull request does, so this is a page at a
/// time rather than all of it: [`HISTORY_PAGE`] commits, and `truncated` says
/// the page came back full, which is as close as GitHub comes to saying there
/// is more behind it.
///
/// The ref goes in the query rather than the path, which is what lets the same
/// call answer for a branch, a tag or a sha.
pub async fn branch_commits(
    token: &str,
    repo: &RepoRef,
    branch: &str,
    page: u32,
) -> Result<Commits> {
    let url = format!(
        "{API}/repos/{}/{}/commits?sha={}&per_page={HISTORY_PAGE}&page={page}",
        encode_segment(&repo.owner),
        encode_segment(&repo.name),
        encode_segment(branch),
    );
    let raw: Vec<RawPrCommit> = get_json(token, &url)
        .await
        .with_context(|| format!("reading the commits on {branch}"))?;
    Ok(Commits {
        truncated: raw.len() == HISTORY_PAGE,
        // A branch has no end to count to, so there is no total to say.
        total: 0,
        items: raw.into_iter().map(commit_of).collect(),
        pages: page,
    })
}

// ------------------------------------------------------------------- checks

/// How something that ran against a commit went, or that it has not gone
/// anywhere yet.
///
/// GitHub keeps two lists and spells the outcome differently in each: a check
/// run has a `status` and, once it is finished, a `conclusion`; a commit status
/// has one `state`. Both come down to these four, which are the four the panel
/// draws — the exact word GitHub used is kept alongside in [`Check::label`], so
/// nothing is flattened away, only coloured.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum CheckState {
    /// Queued, or running now.
    Running,
    Passed,
    Failed,
    /// Finished without a verdict: skipped, cancelled, stale, or deliberately
    /// neutral. Not a failure — and not something to count as a pass either.
    Quiet,
}

impl CheckState {
    /// What it is at a glance. A tick and a cross, because that is what
    /// everybody already reads them as.
    pub fn glyph(self) -> &'static str {
        match self {
            CheckState::Running => "●",
            CheckState::Passed => "✓",
            CheckState::Failed => "✕",
            CheckState::Quiet => "○",
        }
    }

    /// The stylesheet's name for the colour that goes with it.
    pub fn tone(self) -> &'static str {
        match self {
            CheckState::Running => "run",
            CheckState::Passed => "ok",
            CheckState::Failed => "bad",
            CheckState::Quiet => "off",
        }
    }

    /// Where it belongs in a list: what is broken first, then what is still
    /// going, then everything that needs no attention.
    fn rank(self) -> u8 {
        match self {
            CheckState::Failed => 0,
            CheckState::Running => 1,
            CheckState::Passed => 2,
            CheckState::Quiet => 3,
        }
    }
}

/// One thing that ran against a commit: a check run, or one of the older commit
/// statuses a CI service posts.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Check {
    /// What to ask GitHub for this one's annotations by. Zero for a commit
    /// status, which is not a check run and has none.
    pub id: u64,
    pub name: String,
    /// Who ran it — the app behind a check run, or the context a status was
    /// posted under. Empty when GitHub named neither.
    pub source: String,
    pub state: CheckState,
    /// GitHub's own word for how it went: `success`, `timed out`, `in
    /// progress`, `action required`, … Shown as written, since the four states
    /// above are a colour and not the whole story.
    pub label: String,
    /// The one line it left behind, when it left one.
    pub summary: String,
    /// Everything it wrote about itself, as the markdown it wrote it in — the
    /// output's summary and, under it, its longer text. This is where a
    /// coverage report or a bundle-size table lives; empty for the many checks
    /// that write nothing but a conclusion.
    ///
    /// Empty as well when it would only repeat [`summary`](Self::summary),
    /// which is the common case for a check whose whole output is one line.
    pub report: String,
    pub html_url: String,
    /// How long it took, already in words — `1m 20s`. Empty while it is still
    /// running, and for anything that did not say when it started.
    pub took: String,
    /// How many lines of the code this check marked up. Fetched separately —
    /// see [`check_annotations`] — so this is what says whether there is
    /// anything to go and fetch.
    pub annotations: u32,
}

impl Check {
    /// Whether there is anything behind this row worth opening it for.
    pub fn has_detail(&self) -> bool {
        !self.report.is_empty() || self.annotations > 0
    }
}

/// How loudly a check marked one line of the code.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Level {
    Failure,
    Warning,
    Notice,
}

impl Level {
    pub fn tone(self) -> &'static str {
        match self {
            Level::Failure => "bad",
            Level::Warning => "run",
            Level::Notice => "off",
        }
    }

    /// Not the check's own tick and cross: these mark a line rather than
    /// report a verdict, and a row of crosses down the side of a list of
    /// errors says nothing the colour has not.
    pub fn glyph(self) -> &'static str {
        match self {
            Level::Failure => "✕",
            Level::Warning => "▲",
            Level::Notice => "•",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Level::Failure => "failure",
            Level::Warning => "warning",
            Level::Notice => "notice",
        }
    }
}

/// One line of the code a check had something to say about: the compiler error,
/// the failed assertion, the lint.
///
/// This is the part of a check worth having inside an editor rather than on a
/// web page — it names a file and a line, and the file is in the explorer.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Annotation {
    pub path: PathBuf,
    /// Where it starts, 1-based. GitHub also sends an end line, which is only
    /// worth keeping to say `12–18` beside the message.
    pub line: usize,
    pub end_line: usize,
    pub level: Level,
    /// A heading the check gave it, when it gave it one.
    pub title: String,
    pub message: String,
    /// The long form — a traceback, a diff, the failing assertion in full.
    /// Whitespace in it is the shape somebody's tool printed it in, so it is
    /// kept exactly as it arrived and drawn in a monospaced block.
    pub raw_details: String,
}

/// Everything that ran against one commit.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct Checks {
    /// The commit they are about — the head of the pull request, or the commit
    /// being read out of one. Checks belong to a commit and not to a branch,
    /// and a panel that did not say which would be answering a question nobody
    /// asked.
    pub sha: String,
    pub items: Vec<Check>,
    /// True when there were more check runs than [`MAX_CHECK_PAGES`] holds.
    pub truncated: bool,
}

/// How many of each — what the line at the head of the list says.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Tally {
    pub passed: usize,
    pub failed: usize,
    pub running: usize,
    pub quiet: usize,
}

impl Tally {
    /// The sentence at the top of the panel. Only the parts there is something
    /// to say about: "0 failing" beside twelve passes is noise, and noise in
    /// the one line that answers "is it broken?" is worse than nowhere else.
    pub fn phrase(&self) -> String {
        let mut parts = Vec::new();
        for (n, word) in [
            (self.failed, "failing"),
            (self.running, "running"),
            (self.passed, "passed"),
            (self.quiet, "other"),
        ] {
            if n > 0 {
                parts.push(format!("{n} {word}"));
            }
        }
        if parts.is_empty() {
            return "nothing has run".to_string();
        }
        parts.join(" · ")
    }
}

impl Checks {
    pub fn tally(&self) -> Tally {
        let mut t = Tally::default();
        for c in &self.items {
            let slot = match c.state {
                CheckState::Passed => &mut t.passed,
                CheckState::Failed => &mut t.failed,
                CheckState::Running => &mut t.running,
                CheckState::Quiet => &mut t.quiet,
            };
            *slot += 1;
        }
        t
    }

    /// The commit's verdict, as one state.
    ///
    /// A failure outranks anything still running, which outranks a pass:
    /// something already red is red however much of the rest is green, and a
    /// build still going is not one that has passed.
    pub fn state(&self) -> CheckState {
        let t = self.tally();
        match () {
            _ if t.failed > 0 => CheckState::Failed,
            _ if t.running > 0 => CheckState::Running,
            _ if t.passed > 0 => CheckState::Passed,
            _ => CheckState::Quiet,
        }
    }
}

/// The check runs of one commit, a page at a time — an object with the list
/// inside it rather than a bare array, which is why this cannot go through
/// [`get_paged`].
#[derive(Deserialize)]
struct RawCheckPage {
    #[serde(default)]
    check_runs: Vec<RawCheckRun>,
}

#[derive(Deserialize)]
struct RawCheckRun {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    name: String,
    /// `queued`, `in_progress`, `completed` — and `waiting`, `requested` and
    /// `pending`, which GitHub added later and which all still mean "not yet".
    #[serde(default)]
    status: String,
    /// Set once `status` is `completed`, and null until then.
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    completed_at: Option<String>,
    /// Where the run itself is — the CI service's own page, usually. The
    /// check's page on github.com is the fallback.
    #[serde(default)]
    details_url: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    output: Option<RawCheckOutput>,
    #[serde(default)]
    app: Option<RawApp>,
}

#[derive(Deserialize, Default)]
struct RawCheckOutput {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    /// The long half of what a check wrote. Null far more often than not.
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    annotations_count: u32,
}

#[derive(Deserialize)]
struct RawApp {
    #[serde(default)]
    name: String,
}

/// The combined status of a commit: GitHub answers with the latest status per
/// context, which is exactly the list worth showing.
#[derive(Deserialize)]
struct RawStatuses {
    #[serde(default)]
    statuses: Vec<RawStatus>,
}

#[derive(Deserialize)]
struct RawStatus {
    /// `success`, `failure`, `error` or `pending`.
    #[serde(default)]
    state: String,
    /// The name the service posts under — `ci/circleci`, `continuous-integration/travis-ci`.
    #[serde(default)]
    context: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    target_url: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
}

/// One of GitHub's machine words, as a person would say it.
fn humanised(word: &str) -> String {
    word.to_ascii_lowercase().replace('_', " ")
}

/// How far a summary is worth reading in a pane this narrow. Past this it is a
/// build log, and the link beside it is the way to read one.
const MAX_SUMMARY: usize = 160;

/// The first line worth showing of whatever a check said about itself.
///
/// A check's output is markdown, and can be a whole report — tables, badges,
/// stack traces. None of that belongs in a 380px column, and the row links to
/// where it is drawn properly.
fn one_line(text: &str) -> String {
    let Some(line) = text.lines().map(str::trim).find(|l| !l.is_empty()) else {
        return String::new();
    };
    match line.char_indices().nth(MAX_SUMMARY) {
        Some((cut, _)) => format!("{}…", line[..cut].trim_end()),
        None => line.to_string(),
    }
}

/// What a check run amounts to, and the word GitHub used for it.
fn run_state(status: &str, conclusion: Option<&str>) -> (CheckState, String) {
    match conclusion.unwrap_or_default() {
        // Finished and said nothing about how. Rare enough to have no word of
        // its own, and not something to colour green.
        "" if status == "completed" => (CheckState::Quiet, "finished".to_string()),
        "" => (CheckState::Running, humanised(status)),
        "success" => (CheckState::Passed, "success".to_string()),
        // `action_required` is a build that stopped and is waiting to be let
        // through, which is a red mark on the pull request like any other.
        other @ ("failure" | "timed_out" | "action_required" | "startup_failure") => {
            (CheckState::Failed, humanised(other))
        }
        other => (CheckState::Quiet, humanised(other)),
    }
}

fn check_of(raw: RawCheckRun) -> Check {
    let (state, label) = run_state(&raw.status, raw.conclusion.as_deref());
    let output = raw.output.unwrap_or_default();
    // The title is a summary somebody wrote for exactly this purpose; the body
    // of the output is what there is when nobody did.
    let summary = match one_line(output.title.as_deref().unwrap_or_default()) {
        line if line.is_empty() => one_line(output.summary.as_deref().unwrap_or_default()),
        line => line,
    };
    // The two halves of the output as one document, which is what they are
    // written as — and nothing at all where it would only say the row's own
    // line back again, which is most one-line outputs.
    let mut report = String::new();
    for part in [
        output.summary.unwrap_or_default(),
        output.text.unwrap_or_default(),
    ] {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if !report.is_empty() {
            report.push_str("\n\n");
        }
        report.push_str(part);
    }
    if report == summary {
        report.clear();
    }
    Check {
        id: raw.id,
        name: if raw.name.is_empty() {
            "check".to_string()
        } else {
            raw.name
        },
        source: raw.app.map(|a| a.name).unwrap_or_default(),
        state,
        label,
        summary,
        report,
        html_url: raw
            .details_url
            .filter(|u| !u.is_empty())
            .or(raw.html_url)
            .unwrap_or_default(),
        took: took(raw.started_at.as_deref(), raw.completed_at.as_deref()),
        annotations: output.annotations_count,
    }
}

fn status_check_of(raw: RawStatus) -> Check {
    let state = match raw.state.as_str() {
        "success" => CheckState::Passed,
        "failure" | "error" => CheckState::Failed,
        "pending" => CheckState::Running,
        _ => CheckState::Quiet,
    };
    Check {
        // A commit status is not a check run: there is nothing to ask GitHub
        // about it by, and nothing to ask for — the description below is the
        // whole of what it has to say.
        id: 0,
        name: if raw.context.is_empty() {
            "status".to_string()
        } else {
            raw.context
        },
        // A commit status has no app behind it; the context is the whole of
        // what it is called, and it is already in the name.
        source: String::new(),
        state,
        label: humanised(&raw.state),
        summary: one_line(raw.description.as_deref().unwrap_or_default()),
        report: String::new(),
        annotations: 0,
        html_url: raw.target_url.unwrap_or_default(),
        // A status is posted, not run: the two timestamps are when it was first
        // posted and when it last changed, which for a finished one is how long
        // it took to get there.
        took: match state {
            CheckState::Running => String::new(),
            _ => took(raw.created_at.as_deref(), raw.updated_at.as_deref()),
        },
    }
}

/// Days between 1970-01-01 and a date, by Howard Hinnant's `days_from_civil` —
/// the standard closed form, and shorter than the leap-year rules it encodes.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    // March-first years, so the leap day lands at the end where it costs
    // nothing to reason about.
    let y = y - i64::from(m <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Seconds since the epoch for `2024-05-06T07:08:09Z`, which is the only shape
/// GitHub writes a time in.
///
/// `None` for anything else — the only thing read out of the answer is a
/// duration, and a missing duration is a line that says a little less rather
/// than a row that fails to draw.
fn epoch_secs(ts: &str) -> Option<i64> {
    let (date, rest) = ts.split_once('T')?;
    let mut ymd = date.split('-');
    let year: i64 = ymd.next()?.parse().ok()?;
    let month: i64 = ymd.next()?.parse().ok()?;
    let day: i64 = ymd.next()?.parse().ok()?;

    let mut hms = rest.split(':');
    let hour: i64 = hms.next()?.parse().ok()?;
    let minute: i64 = hms.next()?.parse().ok()?;
    // Whatever follows the seconds — the `Z`, a fraction, an offset — is past
    // what a duration in whole seconds is measured in.
    let secs: i64 = hms
        .next()?
        .split(['Z', '.', '+', '-'])
        .next()?
        .parse()
        .ok()?;

    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + secs)
}

/// How long something took, in the words a build log uses.
///
/// Both ends are needed: a check that is still running has no end to measure
/// to, and one that never said when it started cannot be measured at all.
fn took(started: Option<&str>, finished: Option<&str>) -> String {
    let Some((from, to)) = started
        .and_then(epoch_secs)
        .zip(finished.and_then(epoch_secs))
    else {
        return String::new();
    };
    let secs = to - from;
    // Two machines, two clocks. A negative duration is not one to print.
    if secs < 0 {
        return String::new();
    }
    match (secs / 3_600, (secs % 3_600) / 60, secs % 60) {
        (0, 0, s) => format!("{s}s"),
        (0, m, s) => format!("{m}m {s}s"),
        (h, m, _) => format!("{h}h {m}m"),
    }
}

/// Everything that ran against one commit: its check runs, and the commit
/// statuses the older CI services still post.
///
/// Two requests, because GitHub keeps two lists and a repository may be using
/// either or both. A failure on either fails the pair — a list of checks
/// missing the half that was red is worse than one that says it could not be
/// read.
pub async fn commit_checks(token: &str, repo: &RepoRef, sha: &str) -> Result<Checks> {
    let base = format!(
        "{API}/repos/{}/{}/commits/{}",
        encode_segment(&repo.owner),
        encode_segment(&repo.name),
        encode_segment(sha),
    );

    let mut items = Vec::new();
    let mut truncated = false;
    for page in 1..=MAX_CHECK_PAGES {
        let raw: RawCheckPage = get_json(
            token,
            &format!("{base}/check-runs?per_page=100&page={page}"),
        )
        .await
        .with_context(|| format!("reading the checks on {sha}"))?;
        let full_page = raw.check_runs.len() == 100;
        items.extend(raw.check_runs.into_iter().map(check_of));
        if !full_page {
            break;
        }
        if page == MAX_CHECK_PAGES {
            truncated = true;
        }
    }

    let combined: RawStatuses = get_json(token, &format!("{base}/status?per_page=100"))
        .await
        .with_context(|| format!("reading the commit statuses on {sha}"))?;
    items.extend(combined.statuses.into_iter().map(status_check_of));

    // What is broken first, then what is still going. A list read from the top
    // answers "is anything wrong?" before it answers anything else — and by
    // name within each rank, so reloading it does not shuffle the rows.
    items.sort_by(|a, b| {
        a.state
            .rank()
            .cmp(&b.state.rank())
            .then_with(|| a.name.cmp(&b.name))
    });

    Ok(Checks {
        sha: sha.to_string(),
        items,
        truncated,
    })
}

#[derive(Deserialize)]
struct RawAnnotation {
    #[serde(default)]
    path: String,
    #[serde(default)]
    start_line: usize,
    #[serde(default)]
    end_line: usize,
    /// `failure`, `warning` or `notice`.
    #[serde(default)]
    annotation_level: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    raw_details: Option<String>,
}

fn annotation_of(raw: RawAnnotation) -> Annotation {
    let line = raw.start_line.max(1);
    Annotation {
        path: PathBuf::from(raw.path),
        line,
        end_line: raw.end_line.max(line),
        level: match raw.annotation_level.as_deref().unwrap_or_default() {
            "failure" => Level::Failure,
            "warning" => Level::Warning,
            // `notice` and anything GitHub adds later: something worth reading,
            // and not something worth colouring like a broken build.
            _ => Level::Notice,
        },
        title: raw.title.unwrap_or_default().trim().to_string(),
        message: raw.message.unwrap_or_default().trim_end().to_string(),
        raw_details: raw.raw_details.unwrap_or_default().trim_end().to_string(),
    }
}

/// Every line one check marked up: the compiler errors, the failed assertions,
/// the lints.
///
/// A separate request per check, and only for a check that says it has some —
/// which is why [`Check::annotations`] is fetched with the list and these are
/// not. It is the one part of a check's report that names a file and a line, so
/// it is the part this app can do something with: see `ui::conversation`, where
/// each one is a way into the code it is about.
pub async fn check_annotations(token: &str, repo: &RepoRef, check: u64) -> Result<Vec<Annotation>> {
    let base = format!(
        "{API}/repos/{}/{}/check-runs/{check}/annotations",
        encode_segment(&repo.owner),
        encode_segment(&repo.name),
    );
    let (raw, _): (Vec<RawAnnotation>, bool) = get_paged(token, &base, MAX_ANNOTATION_PAGES)
        .await
        .with_context(|| "reading what this check marked up".to_string())?;
    Ok(raw.into_iter().map(annotation_of).collect())
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

/// [`file_at`]'s reply before anything is made of it: the status, and the bytes
/// as they arrived. What a picture is read with, since deciding a file is
/// "binary" is exactly the wrong thing to do to one.
pub async fn bytes_at(
    token: &str,
    repo: &RepoRef,
    sha: &str,
    path: &std::path::Path,
) -> Result<(u16, Vec<u8>)> {
    if token.is_empty() {
        return raw_file(repo, sha, path).await;
    }
    let rel = slashed(path);
    let url = format!(
        "{API}/repos/{}/{}/contents/{}?ref={}",
        encode_segment(&repo.owner),
        encode_segment(&repo.name),
        encode_path(&rel),
        encode_segment(sha),
    );
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
    let (status, body) = bytes_at(token, repo, sha, path).await?;

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

    /// A commit as the endpoint sends it, with only the fields read here.
    fn raw_commit(sha: &str, message: &str, login: Option<&str>, name: &str) -> RawPrCommit {
        RawPrCommit {
            sha: sha.to_string(),
            commit: RawCommitBody {
                message: message.to_string(),
                author: Some(RawSignature {
                    name: name.to_string(),
                    date: "2026-08-13T09:00:00Z".to_string(),
                }),
            },
            author: login.map(|login| User {
                login: login.to_string(),
            }),
            html_url: String::new(),
        }
    }

    #[test]
    fn a_commit_link_is_read_however_it_arrives() {
        let sha = "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0";
        for typed in [
            format!("o/r/commit/{sha}"),
            format!("https://github.com/o/r/commit/{sha}"),
            format!("github.com/o/r/commits/{sha}"),
            // GitHub hangs a file and a line off its own commit links.
            format!("https://github.com/o/r/commit/{sha}/files/src/main.rs"),
        ] {
            let (repo, got) = parse_commit_target(&typed).expect("{typed}");
            assert_eq!(repo.to_string(), "o/r", "{typed}");
            assert_eq!(got, sha, "{typed}");
        }
        // The short form people actually paste out of a terminal.
        assert_eq!(
            parse_commit_target("o/r/commit/abc1234").map(|(_, sha)| sha),
            Some("abc1234".to_string())
        );
    }

    #[test]
    fn only_something_shaped_like_a_sha_is_a_commit() {
        // A branch, a tag, half a sha, and a word that is not hex: all of them
        // are a repository with something after it, not a commit.
        for typed in [
            "o/r/commit/main",
            "o/r/commit/v1.2.3",
            "o/r/commit/abc123",
            "o/r/commit/zzzzzzzz",
            "o/r/commit/",
            "o/r/pull/12",
            "o/r",
        ] {
            assert!(parse_commit_target(typed).is_none(), "{typed}");
        }
        assert!(is_sha("abc1234"));
        assert!(is_sha(&"a".repeat(40)));
        assert!(!is_sha(&"a".repeat(41)));
        assert!(!is_sha("abc123"));
    }

    /// The refs endpoint answers with whole ref names, and only the ones under
    /// `refs/heads/` are branches.
    #[test]
    fn a_ref_is_read_back_as_the_branch_it_names() {
        let branch = |name: &str| {
            branch_of_ref(
                serde_json::from_str(&format!(
                    r#"{{"ref":"{name}","object":{{"sha":"abc1234","type":"commit"}}}}"#
                ))
                .unwrap(),
            )
        };
        let got = branch("refs/heads/feat/branch-list").expect("a branch");
        assert_eq!(got.name, "feat/branch-list");
        assert_eq!(got.sha, "abc1234");
        // Not knowing is not the same as knowing it is unprotected — see
        // `Branch::protected`.
        assert!(!got.protected);

        // A tag is not a branch, and neither is a ref name with nothing after
        // the prefix.
        assert!(branch("refs/tags/v1.0").is_none());
        assert!(branch("refs/heads/").is_none());
    }

    #[test]
    fn a_commit_remembers_its_files_by_the_blob_they_are_made_of() {
        let view = CommitView {
            repo: RepoRef::default(),
            commit: commit_of(raw_commit("abc1234", "m", None, "Ada")),
            parent_sha: "parent".to_string(),
            merge: false,
            files: vec![
                changed("src/a.rs", ChangeKind::Modified),
                changed("gone.rs", ChangeKind::Deleted),
            ],
            truncated: false,
            tree: snapshot(&[("src/a.rs", "now")]),
            base_tree: snapshot(&[("src/a.rs", "before"), ("gone.rs", "was-here")]),
            from: CommitFrom::Alone,
        };
        // The head side, exactly as a pull request's is…
        assert_eq!(view.blob_key(Path::new("src/a.rs")), "now");
        // …and the base side for the one with no head side left.
        assert_eq!(view.blob_key(Path::new("gone.rs")), "was-here");
        assert_eq!(
            view.blob_key_of(&changed("src/a.rs", ChangeKind::Modified)),
            "now"
        );
    }

    #[test]
    fn a_commit_message_is_a_subject_and_what_follows_it() {
        let one = commit_of(raw_commit(
            "a".repeat(40).as_str(),
            "fix the thing",
            None,
            "Ada",
        ));
        assert_eq!(one.short(), "aaaaaaa", "seven characters, not eight");
        assert_eq!(one.subject(), "fix the thing");
        assert_eq!(one.body(), "", "a one-line message has nothing under it");

        let full = commit_of(raw_commit(
            "0123456789abcdef",
            "fix the thing\n\nBecause it was broken.\n\nCloses #12\n",
            None,
            "Ada",
        ));
        assert_eq!(full.short(), "0123456");
        assert_eq!(full.subject(), "fix the thing");
        assert_eq!(full.body(), "Because it was broken.\n\nCloses #12");
    }

    /// A sha shorter than the seven characters everybody quotes — which is not
    /// something GitHub sends, and is not something to panic over either.
    #[test]
    fn a_short_sha_is_as_short_as_it_is() {
        let stub = commit_of(raw_commit("abc", "x", None, ""));
        assert_eq!(stub.short(), "abc");
    }

    #[test]
    fn a_commit_is_attributed_to_the_account_first_and_the_signature_after() {
        let with_account = commit_of(raw_commit("s", "m", Some("ada"), "Ada Lovelace"));
        assert_eq!(
            with_account.author, "ada",
            "the login is what the PR is under"
        );

        let no_account = commit_of(raw_commit("s", "m", None, "Ada Lovelace"));
        assert_eq!(
            no_account.author, "Ada Lovelace",
            "an email GitHub does not know still has a name on it"
        );

        let anonymous = commit_of(raw_commit("s", "m", None, ""));
        assert_eq!(anonymous.author, "unknown", "rather than an empty column");
    }

    #[test]
    fn merged_and_closed_are_not_the_same_news() {
        let raw = |state: &str, merged: bool| RawPr {
            number: 1,
            title: "t".to_string(),
            body: None,
            user: None,
            draft: false,
            state: state.to_string(),
            merged_at: merged.then(|| "2026-08-13T09:00:00Z".to_string()),
            updated_at: String::new(),
            html_url: String::new(),
            head: RawRef {
                name: "feature".to_string(),
                sha: String::new(),
            },
            base: RawRef {
                name: "main".to_string(),
                sha: String::new(),
            },
        };
        let open = summary_of(raw("open", false));
        assert!(open.is_open() && !open.merged);
        // Both of these are `closed` to GitHub, and the badge on them differs.
        let landed = summary_of(raw("closed", true));
        assert!(!landed.is_open() && landed.merged);
        let dropped = summary_of(raw("closed", false));
        assert!(!dropped.is_open() && !dropped.merged);
        // And nobody's account is still somebody.
        assert_eq!(open.author, "ghost");
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

    /// Directories are not entries of the tree — it is rebuilt from the blob
    /// paths — so this is what a link naming one is checked against.
    #[test]
    fn a_directory_is_a_path_some_file_is_under() {
        let mut tree = Snapshot::default();
        for path in ["src/main.rs", "src/ui/app.rs", "srcery/lib.rs", "README.md"] {
            tree.files.push(TreeEntry {
                path: PathBuf::from(path),
                sha: String::new(),
                size: 0,
            });
        }
        assert!(tree.has_dir(Path::new("src")));
        assert!(tree.has_dir(Path::new("src/ui")));
        // Component by component: `src` is not the front of `srcery`.
        assert!(!tree.has_dir(Path::new("sr")));
        assert!(!tree.has_dir(Path::new("src/nope")));
        // A file is not a directory to open, and the root is not one to name.
        assert!(!tree.has_dir(Path::new("")));
        // The root itself has no entry, and neither does an empty repository.
        assert!(!Snapshot::default().has_dir(Path::new("src")));
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
        };
        assert_eq!(f.base_path(), &PathBuf::from("old.rs"));

        let plain = PrFile {
            path: PathBuf::from("same.rs"),
            previous_path: None,
            status: ChangeKind::Modified,
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
            },
            PrFile {
                path: PathBuf::from("b.rs"),
                previous_path: None,
                status: ChangeKind::Deleted,
            },
        ];
        let map = statuses_of(&files);
        assert_eq!(map.get(&PathBuf::from("a.rs")), Some(&ChangeKind::Added));
        assert_eq!(map.get(&PathBuf::from("b.rs")), Some(&ChangeKind::Deleted));
    }

    // ------------------------------------------------------------ checks

    #[test]
    fn a_check_that_has_not_finished_is_still_running() {
        for status in ["queued", "in_progress", "waiting", "requested"] {
            let (state, label) = run_state(status, None);
            assert_eq!(state, CheckState::Running, "{status}");
            // The word GitHub used, spelled the way a person would.
            assert!(!label.contains('_'), "{label}");
        }
        assert_eq!(run_state("in_progress", None).1, "in progress");
    }

    #[test]
    fn a_conclusion_is_what_a_finished_check_is_coloured_by() {
        let cases = [
            ("success", CheckState::Passed),
            ("failure", CheckState::Failed),
            ("timed_out", CheckState::Failed),
            // Stopped and waiting to be let through: a red mark on the pull
            // request like any other.
            ("action_required", CheckState::Failed),
            ("skipped", CheckState::Quiet),
            ("cancelled", CheckState::Quiet),
            ("neutral", CheckState::Quiet),
            ("stale", CheckState::Quiet),
        ];
        for (conclusion, want) in cases {
            let (state, label) = run_state("completed", Some(conclusion));
            assert_eq!(state, want, "{conclusion}");
            assert_eq!(label, humanised(conclusion));
        }
        // Finished, and said nothing about how. Not something to call a pass.
        assert_eq!(run_state("completed", None).0, CheckState::Quiet);
    }

    /// The four states, as the rows they are counted out of.
    fn checks_of(states: &[CheckState]) -> Checks {
        Checks {
            sha: "abc1234".to_string(),
            items: states
                .iter()
                .enumerate()
                .map(|(i, state)| Check {
                    id: i as u64 + 1,
                    name: format!("job {i}"),
                    source: String::new(),
                    state: *state,
                    label: String::new(),
                    summary: String::new(),
                    report: String::new(),
                    html_url: String::new(),
                    took: String::new(),
                    annotations: 0,
                })
                .collect(),
            truncated: false,
        }
    }

    #[test]
    fn a_failure_outranks_everything_still_running() {
        let checks = checks_of(&[
            CheckState::Passed,
            CheckState::Running,
            CheckState::Failed,
            CheckState::Passed,
        ]);
        assert_eq!(checks.state(), CheckState::Failed);
        let t = checks.tally();
        assert_eq!((t.passed, t.failed, t.running), (2, 1, 1));
        assert_eq!(t.phrase(), "1 failing · 1 running · 2 passed");
    }

    #[test]
    fn a_build_still_going_is_not_one_that_has_passed() {
        let checks = checks_of(&[CheckState::Passed, CheckState::Running]);
        assert_eq!(checks.state(), CheckState::Running);
        assert_eq!(checks.tally().phrase(), "1 running · 1 passed");
    }

    #[test]
    fn nothing_but_skips_is_neither_green_nor_red() {
        let checks = checks_of(&[CheckState::Quiet, CheckState::Quiet]);
        assert_eq!(checks.state(), CheckState::Quiet);
        assert_eq!(checks.tally().phrase(), "2 other");
        // And nothing at all says so rather than counting to zero.
        assert_eq!(checks_of(&[]).tally().phrase(), "nothing has run");
        assert_eq!(checks_of(&[]).state(), CheckState::Quiet);
    }

    #[test]
    fn a_check_run_carries_its_app_its_link_and_its_first_line() {
        let raw: RawCheckRun = serde_json::from_str(
            r#"{
                "name": "test (ubuntu-latest)",
                "status": "completed",
                "conclusion": "failure",
                "started_at": "2024-05-06T07:08:09Z",
                "completed_at": "2024-05-06T07:10:29Z",
                "details_url": "https://ci.example/run/1",
                "html_url": "https://github.com/o/r/runs/1",
                "output": { "title": "", "summary": "3 tests failed\nsee the log" },
                "app": { "name": "GitHub Actions" }
            }"#,
        )
        .unwrap();
        let check = check_of(raw);
        assert_eq!(check.state, CheckState::Failed);
        assert_eq!(check.label, "failure");
        assert_eq!(check.source, "GitHub Actions");
        // The run itself, not the page about it.
        assert_eq!(check.html_url, "https://ci.example/run/1");
        assert_eq!(check.summary, "3 tests failed");
        assert_eq!(check.took, "2m 20s");
    }

    #[test]
    fn a_check_keeps_the_whole_of_what_it_wrote_as_well_as_the_first_line() {
        let raw: RawCheckRun = serde_json::from_str(
            r#"{
                "id": 951133849,
                "name": "coverage",
                "status": "completed",
                "conclusion": "success",
                "output": {
                    "title": "Coverage 81%",
                    "summary": "| file | % |\n|---|---|\n| a.rs | 81 |",
                    "text": "The long form.",
                    "annotations_count": 3
                }
            }"#,
        )
        .unwrap();
        let check = check_of(raw);
        assert_eq!(check.id, 951133849);
        // The row shows the line somebody wrote for the purpose…
        assert_eq!(check.summary, "Coverage 81%");
        // …and opening it shows the two halves of the output, as one document.
        assert_eq!(
            check.report,
            "| file | % |\n|---|---|\n| a.rs | 81 |\n\nThe long form."
        );
        assert_eq!(check.annotations, 3);
        assert!(check.has_detail());
    }

    #[test]
    fn a_one_line_output_is_not_shown_twice() {
        let one_liner = |json: &str| check_of(serde_json::from_str::<RawCheckRun>(json).unwrap());
        // Nothing but a summary: it is the row's line, so there is nothing
        // left to unfold.
        let check = one_liner(
            r#"{"name":"links","status":"completed","conclusion":"success",
                "output":{"summary":"No broken links found"}}"#,
        );
        assert_eq!(check.summary, "No broken links found");
        assert_eq!(check.report, "");
        assert!(!check.has_detail());

        // And a check that wrote nothing at all — most of them.
        let bare = one_liner(r#"{"name":"build","status":"completed","conclusion":"success"}"#);
        assert!(!bare.has_detail());
        assert_eq!(bare.summary, "");

        // Marked-up lines are worth opening for on their own, with no report.
        let marked = one_liner(
            r#"{"name":"pylint","status":"completed","conclusion":"failure",
                "output":{"annotations_count":2}}"#,
        );
        assert!(marked.has_detail());
    }

    #[test]
    fn an_annotation_names_a_line_to_go_to() {
        let raw: RawAnnotation = serde_json::from_str(
            r#"{
                "path": "tests/components/portainer/test_event.py",
                "start_line": 7,
                "end_line": 9,
                "annotation_level": "failure",
                "title": "",
                "message": "E0611: No name 'DockerEvent' in module\n",
                "raw_details": "  assert 1 == 2\n"
            }"#,
        )
        .unwrap();
        let a = annotation_of(raw);
        assert_eq!(
            a.path,
            PathBuf::from("tests/components/portainer/test_event.py")
        );
        assert_eq!((a.line, a.end_line), (7, 9));
        assert_eq!(a.level, Level::Failure);
        // Trailing newlines go; the shape inside does not — it is a tool's own
        // layout, and the block is drawn monospaced for exactly that reason.
        assert_eq!(a.message, "E0611: No name 'DockerEvent' in module");
        assert_eq!(a.raw_details, "  assert 1 == 2");
    }

    #[test]
    fn an_annotation_without_a_line_still_lands_somewhere() {
        let bare: RawAnnotation = serde_json::from_str(r#"{"path": ".github"}"#).unwrap();
        let a = annotation_of(bare);
        // Line 0 is not a line. The top of the file is where it points.
        assert_eq!((a.line, a.end_line), (1, 1));
        // And an unknown level is something to read, not something to redden.
        assert_eq!(a.level, Level::Notice);

        for (word, want) in [
            ("failure", Level::Failure),
            ("warning", Level::Warning),
            ("notice", Level::Notice),
        ] {
            let raw: RawAnnotation =
                serde_json::from_str(&format!(r#"{{"path":"a.rs","annotation_level":"{word}"}}"#))
                    .unwrap();
            assert_eq!(annotation_of(raw).level, want, "{word}");
        }
    }

    #[test]
    fn an_older_commit_status_reads_as_a_check_like_any_other() {
        let raw: RawStatus = serde_json::from_str(
            r#"{
                "state": "error",
                "context": "ci/circleci: build",
                "description": "Your tests failed on CircleCI",
                "target_url": "https://circleci.example/1",
                "created_at": "2024-05-06T07:08:09Z",
                "updated_at": "2024-05-06T07:08:19Z"
            }"#,
        )
        .unwrap();
        let check = status_check_of(raw);
        assert_eq!(check.state, CheckState::Failed);
        assert_eq!(check.name, "ci/circleci: build");
        assert_eq!(check.summary, "Your tests failed on CircleCI");
        assert_eq!(check.took, "10s");
    }

    #[test]
    fn a_summary_is_one_line_and_not_a_build_log() {
        assert_eq!(one_line("\n\n  hello  \nworld\n"), "hello");
        assert_eq!(one_line(""), "");
        let long = "x".repeat(400);
        let cut = one_line(&long);
        assert_eq!(cut.chars().count(), MAX_SUMMARY + 1);
        assert!(cut.ends_with('…'));
        // Cut by characters, not by bytes: a summary is somebody's prose.
        let wide = "é".repeat(400);
        assert!(one_line(&wide).starts_with('é'));
    }

    #[test]
    fn a_duration_reads_the_way_a_build_log_says_it() {
        let at = |s: &str| epoch_secs(s).unwrap();
        assert_eq!(at("1970-01-01T00:00:00Z"), 0);
        assert_eq!(at("2024-05-06T07:08:09Z"), 1_714_979_289);
        // A fraction of a second, and an offset, are past what this measures.
        assert_eq!(at("2024-05-06T07:08:09.512Z"), at("2024-05-06T07:08:09Z"));
        assert_eq!(at("2024-05-06T07:08:09+00:00"), at("2024-05-06T07:08:09Z"));

        let took_between = |a: &str, b: &str| took(Some(a), Some(b));
        assert_eq!(
            took_between("2024-05-06T07:08:09Z", "2024-05-06T07:08:54Z"),
            "45s"
        );
        assert_eq!(
            took_between("2024-05-06T07:08:09Z", "2024-05-06T07:09:29Z"),
            "1m 20s"
        );
        assert_eq!(
            took_between("2024-05-06T07:08:09Z", "2024-05-06T08:10:09Z"),
            "1h 2m"
        );
        // One end missing, both ends nonsense, or two clocks disagreeing: a
        // line that says a little less, not a row that fails to draw.
        assert_eq!(took(Some("2024-05-06T07:08:09Z"), None), "");
        assert_eq!(took(Some("yesterday"), Some("today")), "");
        assert_eq!(
            took_between("2024-05-06T07:08:09Z", "2024-05-06T07:08:08Z"),
            ""
        );
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
        Self::between(
            &pr.repo,
            &pr.base_sha,
            &pr.head_sha,
            &pr.tree,
            &pr.base_tree,
            rel,
            f,
        )
    }

    /// For any path of a commit being read on its own.
    pub fn in_commit(view: &CommitView, rel: &std::path::Path) -> Self {
        Self::commit_build(view, rel, find_file(&view.files, rel))
    }

    /// [`in_commit`](Self::in_commit) for a caller already holding the file.
    pub fn for_commit_change<'a>(view: &'a CommitView, f: &'a PrFile) -> Self {
        Self::commit_build(view, &f.path, Some(f))
    }

    fn commit_build(view: &CommitView, rel: &std::path::Path, f: Option<&PrFile>) -> Self {
        Self::between(
            &view.repo,
            &view.parent_sha,
            &view.commit.sha,
            &view.tree,
            &view.base_tree,
            rel,
            f,
        )
    }

    /// For any path of a comparison — read against the merge base, which is
    /// what `base_sha` is.
    pub fn in_compare(view: &CompareView, rel: &std::path::Path) -> Self {
        Self::compare_build(view, rel, find_file(&view.files, rel))
    }

    /// [`in_compare`](Self::in_compare) for a caller already holding the file.
    pub fn for_compare_change<'a>(view: &'a CompareView, f: &'a PrFile) -> Self {
        Self::compare_build(view, &f.path, Some(f))
    }

    fn compare_build(view: &CompareView, rel: &std::path::Path, f: Option<&PrFile>) -> Self {
        Self::between(
            &view.repo,
            &view.base_sha,
            &view.head_sha,
            &view.tree,
            &view.base_tree,
            rel,
            f,
        )
    }

    /// One path, between two commits of one repository — which is all a diff
    /// ever is here, whether the two commits are a pull request's or a single
    /// commit and the one before it.
    #[allow(clippy::too_many_arguments)]
    fn between(
        repo: &RepoRef,
        base_sha: &str,
        head_sha: &str,
        tree: &Snapshot,
        base_tree: &Snapshot,
        rel: &std::path::Path,
        f: Option<&PrFile>,
    ) -> Self {
        let base_path = f.map_or_else(|| rel.to_path_buf(), |f| f.base_path().clone());
        FetchJob {
            repo: repo.clone(),
            base_sha: base_sha.to_string(),
            head_sha: head_sha.to_string(),
            head_blob: tree.entry(rel).cloned(),
            base_blob: base_tree.entry(&base_path).cloned(),
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
