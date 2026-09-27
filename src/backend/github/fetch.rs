use super::*;

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

pub(super) fn find_file<'a>(files: &'a [PrFile], path: &std::path::Path) -> Option<&'a PrFile> {
    files.iter().find(|f| f.path == path)
}

/// A repo-relative path as GitHub spells it. The replace only ever matters to
/// a native run on Windows — and only there is it worth an allocation.
pub(super) fn slashed(path: &std::path::Path) -> Cow<'_, str> {
    let rel = path.to_string_lossy();
    if rel.contains('\\') {
        Cow::Owned(rel.replace('\\', "/"))
    } else {
        rel
    }
}

// -------------------------------------------------------------- fetch jobs

/// Everything needed to read one file of a pull request or browsed repository,
/// lifted out of app state so the read can travel.
///
/// It carries both what the file is called and what it hashes to. The names are
/// what the CDN answers to; the hashes are what the local store is keyed by, so
/// a job whose blobs are already on disk needs no network at all — see
/// [`clone::read_pair`](crate::backend::clone::read_pair).
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
