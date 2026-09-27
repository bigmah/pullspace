use super::*;

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
pub(super) struct RawPrCommit {
    #[serde(default)]
    pub(super) sha: String,
    #[serde(default)]
    pub(super) commit: RawCommitBody,
    /// The GitHub account, when the email on the commit belongs to one.
    #[serde(default)]
    pub(super) author: Option<User>,
    #[serde(default)]
    pub(super) html_url: String,
}

#[derive(Deserialize, Default)]
pub(super) struct RawCommitBody {
    #[serde(default)]
    pub(super) message: String,
    #[serde(default)]
    pub(super) author: Option<RawSignature>,
}

/// Git's own idea of who wrote something and when — a name and a date typed
/// into a commit, with no account behind either.
#[derive(Deserialize)]
pub(super) struct RawSignature {
    #[serde(default)]
    pub(super) name: String,
    #[serde(default)]
    pub(super) date: String,
}

pub(super) fn commit_of(raw: RawPrCommit) -> CommitSummary {
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
pub(super) struct RawCommitDetail {
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
pub(super) fn compare_url(repo: &RepoRef, base: &str, head: &str, page: u32) -> String {
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
