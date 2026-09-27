use super::*;

#[derive(Deserialize)]
pub(super) struct RawTree {
    #[serde(default)]
    tree: Vec<RawTreeEntry>,
    /// GitHub sets this when the repo exceeds its tree limits.
    #[serde(default)]
    truncated: bool,
}

#[derive(Deserialize)]
pub(super) struct RawTreeEntry {
    path: String,
    #[serde(default)]
    mode: String,
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
    /// Every directory and the tree hash it has at this commit, sorted by path
    /// like `files`. The root is the entry with an empty path, and its hash is
    /// the root's tree with `.pullspace` left out — see
    /// [`summary::root_stamp`](crate::backend::summary::root_stamp) for why. What a
    /// summary is held up to, to say whether it is still true.
    ///
    /// Empty in a snapshot kept from before this was, which
    /// [`blobs::load`](crate::backend::blobs::load) takes as a reason to read it again.
    #[serde(default)]
    pub dirs: Vec<TreeEntry>,
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
            dirs: Vec::new(),
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

    /// The tree hash of a directory at this commit — `""` for the root, as a
    /// summary stamps it.
    pub fn dir_sha(&self, dir: &std::path::Path) -> Option<&str> {
        self.dirs
            .binary_search_by(|d| d.path.as_path().cmp(dir))
            .ok()
            .map(|i| self.dirs[i].sha.as_str())
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
    Ok(snapshot_of(repo, sha, raw))
}

pub(super) fn snapshot_of(repo: &RepoRef, sha: &str, raw: RawTree) -> Snapshot {
    // The root's own entries, for the one tree hash GitHub cannot give: the
    // root without `.pullspace`. Only from a whole tree — a truncated one may
    // be missing some of them, and a wrong stamp is worse than none.
    let root = (!raw.truncated).then(|| {
        crate::backend::summary::root_stamp(
            raw.tree
                .iter()
                .filter(|e| !e.path.contains('/'))
                .map(|e| (e.mode.as_str(), e.path.as_str(), e.sha.as_str())),
        )
    });
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    for e in raw.tree {
        // "commit" entries are submodules, which have nothing to read. The
        // file tree is rebuilt from the blob paths alone; the "tree" entries
        // are kept only for their hashes.
        let list = match e.kind.as_str() {
            "blob" => &mut files,
            "tree" => &mut dirs,
            _ => continue,
        };
        list.push(TreeEntry {
            path: PathBuf::from(e.path),
            sha: e.sha,
            size: e.size,
        });
    }
    if let Some(root) = root {
        dirs.push(TreeEntry {
            path: PathBuf::new(),
            sha: root,
            size: 0,
        });
    }
    // Git writes trees in its own order, which is nearly but not quite this
    // one. Sorting here is what makes `entry` a binary search.
    files.sort_by(|a, b| a.path.cmp(&b.path));
    dirs.sort_by(|a, b| a.path.cmp(&b.path));
    Snapshot {
        repo: repo.clone(),
        commit: sha.to_string(),
        files,
        dirs,
        truncated: raw.truncated,
    }
}
