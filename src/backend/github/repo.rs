use super::*;

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
    /// More branches behind these, one request away — see [`list_branches`].
    pub truncated: bool,
    /// How many pages of them are in `items`, so the next ask knows where to
    /// carry on from.
    #[serde(default)]
    pub pages: u32,
}

#[derive(Deserialize)]
pub(super) struct RawBranch {
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
pub(super) struct RawMatchingRef {
    #[serde(rename = "ref", default)]
    name: String,
    #[serde(default)]
    object: RawCommit,
}

/// What a ref is called once it is not a ref: `refs/heads/dev` is the branch
/// `dev`, and anything else under `refs/` is not a branch at all.
pub(super) fn branch_of_ref(raw: RawMatchingRef) -> Option<Branch> {
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

/// The branches of a repository — what the pane lists, and what each row is a
/// way into.
///
/// In GitHub's own order, which is by name, and the only one that costs
/// nothing: sorting by when each was last pushed to would be a request per
/// branch.
///
/// `read` is how many pages the caller already holds. Zero — the first ask —
/// takes [`MAX_BRANCH_PAGES`] of them together, since a pane that opens on a
/// hundred names and a button is a pane that has stopped short of a question it
/// could have answered. After that they come one at a time, on request, for as
/// long as there are more: microsoft/vscode's forty-eight hundred branches are
/// forty-eight pages, and nobody clicks through all of them — but the ones past
/// the third are reachable, which is the whole difference between a long list
/// and a truncated one. [`matching_branches`] is the other way to the same
/// place, and the faster one when the name is known.
pub async fn list_branches(token: &str, repo: &RepoRef, read: u32) -> Result<Branches> {
    let base = format!(
        "{API}/repos/{}/{}/branches",
        encode_segment(&repo.owner),
        encode_segment(&repo.name),
    );
    let count = if read == 0 { MAX_BRANCH_PAGES } else { 1 };
    let (raw, pages, truncated): (Vec<RawBranch>, u32, bool) =
        get_pages(token, &base, read + 1, count)
            .await
            .with_context(|| format!("reading the branches of {repo}"))?;
    // The pages are cut from a list sorted by name, so a branch created while
    // they are read shifts the next one along and a name arrives twice — which
    // is a repeated row key as well as a branch that is not there.
    let mut seen = HashSet::new();
    Ok(Branches {
        items: raw
            .into_iter()
            .filter(|b| !b.name.is_empty() && seen.insert(b.name.clone()))
            .map(|b| Branch {
                name: b.name,
                sha: b.commit.sha,
                protected: b.protected,
            })
            .collect(),
        truncated,
        pages: read + pages,
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
    /// forms the address bar takes. See [`Target`](crate::backend::route::Target).
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
pub(super) fn encode_ref(name: &str) -> String {
    name.split('/')
        .filter(|s| !s.is_empty())
        .map(encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

#[derive(Deserialize)]
pub(super) struct RawRef {
    #[serde(rename = "ref")]
    pub(super) name: String,
    pub(super) sha: String,
    /// Which repository the ref is in. Null on the head of a pull request whose
    /// fork has since been deleted.
    #[serde(default)]
    pub(super) repo: Option<RawRefRepo>,
}

#[derive(Deserialize)]
pub(super) struct RawRefRepo {
    pub(super) full_name: String,
}

/// The fork a pull request's head is in, when it is not the repository the pull
/// request is against.
///
/// `None` for a branch of that repository — and for a fork that has been
/// deleted, which leaves nowhere else to read the branch from. GitHub's names
/// are not case-sensitive, so neither is the comparison.
pub(super) fn fork_of(base: &RepoRef, head: Option<&RawRefRepo>) -> Option<RepoRef> {
    let (owner, name) = head?.full_name.split_once('/')?;
    let same = owner.eq_ignore_ascii_case(&base.owner) && name.eq_ignore_ascii_case(&base.name);
    (!same && !owner.is_empty() && !name.is_empty()).then(|| RepoRef {
        owner: owner.to_string(),
        name: name.to_string(),
    })
}
