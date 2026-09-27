use super::*;

#[derive(Deserialize)]
pub(super) struct RawPr {
    pub(super) number: u64,
    pub(super) title: String,
    /// The description. Null on a pull request opened without one.
    #[serde(default)]
    pub(super) body: Option<String>,
    #[serde(default)]
    pub(super) user: Option<User>,
    #[serde(default)]
    pub(super) draft: bool,
    pub(super) state: String,
    /// Set only on a pull request that was closed by being merged, which is the
    /// one thing `state` does not say.
    #[serde(default)]
    pub(super) merged_at: Option<String>,
    #[serde(default)]
    pub(super) created_at: Option<String>,
    pub(super) updated_at: String,
    pub(super) html_url: String,
    pub(super) head: RawRef,
    pub(super) base: RawRef,
    #[serde(default)]
    pub(super) labels: Vec<RawLabel>,
    /// Who has been asked to look and has not yet — GitHub takes a name off
    /// this list the moment its owner submits a review.
    #[serde(default)]
    pub(super) requested_reviewers: Vec<User>,
    #[serde(default)]
    pub(super) requested_teams: Vec<RawTeam>,
}

#[derive(Deserialize)]
pub(super) struct RawLabel {
    #[serde(default)]
    name: String,
    /// Six hex digits, no `#`.
    #[serde(default)]
    color: String,
}

#[derive(Deserialize)]
pub(super) struct RawTeam {
    #[serde(default)]
    slug: String,
}

/// One of the labels on a pull request.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Label {
    pub name: String,
    /// The colour its repository gave it, as `#rrggbb` — or empty, when what
    /// GitHub sent is not one. It ends up inside a `style` attribute, so it is
    /// checked here rather than trusted there.
    pub color: String,
}

pub(super) fn label_of(raw: RawLabel) -> Option<Label> {
    let name = raw.name.trim().to_string();
    if name.is_empty() {
        return None;
    }
    let hex = raw.color.trim();
    let color = if hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        format!("#{hex}")
    } else {
        String::new()
    };
    Some(Label { name, color })
}

/// One pull request, as a list of them knows it.
///
/// Everything here arrives with the list itself — one request for the lot — so
/// a row can say who, when, where from and what about without a request of its
/// own. What a list cannot be asked for that way is in [`PrMore`].
///
/// `Eq`, which is what lets a list hold these behind an `Rc` and have two of
/// them compared by where they are rather than by reading both descriptions.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PrSummary {
    pub number: u64,
    pub title: String,
    pub author: String,
    /// The author's picture, or empty for an account that no longer has one.
    #[serde(default)]
    pub avatar: String,
    pub draft: bool,
    /// `open` or `closed`, as GitHub says it.
    pub state: String,
    /// Closed by landing rather than by being turned down. GitHub calls both
    /// `closed`, and they are not the same news.
    pub merged: bool,
    #[serde(default)]
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub html_url: String,
    pub head_ref: String,
    /// The fork the head is a branch of, when it is not the repository the pull
    /// request is against.
    #[serde(default)]
    pub head_repo: Option<RepoRef>,
    pub base_ref: String,
    /// The description, as the markdown it was written in. Empty when nobody
    /// wrote one.
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub labels: Vec<Label>,
    /// Who is still being waited on for a review: people by login, teams by
    /// slug.
    #[serde(default)]
    pub reviewers: Vec<String>,
}

impl PrSummary {
    pub fn is_open(&self) -> bool {
        self.state == "open"
    }

    /// Which of the four things a pull request can be.
    pub fn status(&self) -> PrStatus {
        match () {
            _ if self.merged => PrStatus::Merged,
            _ if !self.is_open() => PrStatus::Closed,
            _ if self.draft => PrStatus::Draft,
            _ => PrStatus::Open,
        }
    }

    /// The branch it comes from, with the fork's owner in front when it comes
    /// from somebody else's — GitHub's own `owner:branch`.
    pub fn head_label(&self) -> String {
        match &self.head_repo {
            Some(fork) => format!("{}:{}", fork.owner, self.head_ref),
            None => self.head_ref.clone(),
        }
    }
}

/// Where a pull request has got to.
///
/// GitHub's `state` has two values and its badge has four, because a draft is
/// open and a merge is closed and neither is the same news as its neighbour.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrStatus {
    Open,
    Draft,
    Merged,
    Closed,
}

impl PrStatus {
    pub fn label(self) -> &'static str {
        match self {
            PrStatus::Open => "open",
            PrStatus::Draft => "draft",
            PrStatus::Merged => "merged",
            PrStatus::Closed => "closed",
        }
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

pub(super) fn author_of(user: &Option<User>) -> String {
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
    Ok(raw.into_iter().map(|p| summary_of(repo, p)).collect())
}

/// `base` is the repository the list was asked of, which is what says whether a
/// head is in a fork.
pub(super) fn summary_of(base: &RepoRef, p: RawPr) -> PrSummary {
    let reviewers = p
        .requested_reviewers
        .into_iter()
        .map(|u| u.login)
        .chain(p.requested_teams.into_iter().map(|t| t.slug))
        .filter(|name| !name.is_empty())
        .collect();
    PrSummary {
        number: p.number,
        title: p.title,
        author: author_of(&p.user),
        avatar: p.user.and_then(|u| u.avatar_url).unwrap_or_default(),
        draft: p.draft,
        merged: p.merged_at.is_some(),
        state: p.state,
        created_at: p.created_at.unwrap_or_default(),
        updated_at: p.updated_at,
        html_url: p.html_url,
        head_repo: fork_of(base, p.head.repo.as_ref()),
        head_ref: p.head.name,
        base_ref: p.base.name,
        body: p.body.unwrap_or_default(),
        labels: p.labels.into_iter().filter_map(label_of).collect(),
        reviewers,
    }
}

#[derive(Deserialize)]
pub(super) struct RawFile {
    filename: String,
    status: String,
    #[serde(default)]
    previous_filename: Option<String>,
}

pub(super) fn change_kind(status: &str) -> ChangeKind {
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
pub(super) fn file_of(f: &RawFile) -> PrFile {
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
pub(super) struct RawCompare {
    pub(super) merge_base_commit: RawCommit,
    #[serde(default)]
    pub(super) base_commit: RawCommit,
    /// `identical`, `ahead`, `behind` or `diverged`, from the base's point of
    /// view.
    #[serde(default)]
    pub(super) status: String,
    #[serde(default)]
    pub(super) ahead_by: u32,
    #[serde(default)]
    pub(super) behind_by: u32,
    /// Every commit between them, not only the ones on this page.
    #[serde(default)]
    pub(super) total_commits: u32,
    #[serde(default)]
    pub(super) commits: Vec<RawPrCommit>,
    #[serde(default)]
    pub(super) files: Vec<RawFile>,
    #[serde(default)]
    pub(super) html_url: String,
}

#[derive(Deserialize, Default)]
pub(super) struct RawCommit {
    #[serde(default)]
    pub(super) sha: String,
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
    /// The fork `head_ref` is a branch of, when it is not `repo` — see
    /// [`head_label`](Self::head_label).
    #[serde(default)]
    pub head_repo: Option<RepoRef>,
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
pub(super) fn blob_key_in<'a>(
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
pub(super) fn blob_key_of_in<'a>(
    tree: &'a Snapshot,
    base_tree: &'a Snapshot,
    f: &PrFile,
) -> Cow<'a, str> {
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

    /// The head, named the way the repository it is against has to name it:
    /// the branch, or `owner:branch` for a branch of a fork — GitHub's own
    /// spelling, and what its compare endpoint takes.
    pub fn head_label(&self) -> String {
        match &self.head_repo {
            Some(fork) => format!("{}:{}", fork.owner, self.head_ref),
            None => self.head_ref.clone(),
        }
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
    let head_repo = fork_of(repo, pr.head.repo.as_ref());
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
        head_repo,
        base_ref: pr.base.name,
        tree: Snapshot::unknown(repo, &head_sha),
        base_tree: Snapshot::unknown(repo, &base_sha),
        base_sha,
        head_sha,
        files,
        truncated,
    })
}
