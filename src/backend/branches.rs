//! Which branch the repository is on, which others it has, and what stands
//! between any two of them.
//!
//! A comparison is deliberately the same shape as a pull request: the changed
//! files, the whole tree at the head, and two commits to read each side of a
//! file from. That is what lets the explorer, the badges and the diff views
//! show a pair of branches without knowing they are looking at one.
//!
//! And it is diffed the same way — against the **merge base** of the two, not
//! against the tip of the base branch. Diffing the tips would present every
//! commit that landed on `main` since the branch left it as part of the branch,
//! reversed. This is the three-dot comparison, and it is what `git diff a...b`
//! and GitHub's compare view both show.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use git2::{BranchType, Delta, DiffFindOptions, Oid, Repository};

use super::tree::ChangeKind;

/// Where `HEAD` is pointing.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Head {
    /// On a branch — including one with no commits on it yet, which still has
    /// a name and is still what `git status` calls being on a branch.
    Branch(String),
    /// Not on one: the short sha it is parked at.
    Detached(String),
}

impl Head {
    /// What the top bar shows.
    pub fn label(&self) -> String {
        match self {
            Head::Branch(name) => name.clone(),
            Head::Detached(sha) => format!("detached @ {sha}"),
        }
    }

    /// The branch name, for anything that needs a ref rather than a caption.
    pub fn name(&self) -> &str {
        match self {
            Head::Branch(name) => name,
            Head::Detached(sha) => sha,
        }
    }
}

/// The branch the working tree is on. `None` when the path is not in a
/// repository at all — pullspace opens plain directories too.
pub fn head_of(root: &Path) -> Option<Head> {
    let repo = Repository::discover(root).ok()?;
    if let Ok(head) = repo.head() {
        if head.is_branch()
            && let Ok(name) = head.shorthand()
        {
            return Some(Head::Branch(name.to_string()));
        }
        let sha = head.peel_to_commit().ok()?.id().to_string();
        return Some(Head::Detached(short(&sha)));
    }
    // Unborn: HEAD names a branch that has no commit on it yet, so it cannot be
    // resolved — but the name is right there in the symbolic ref.
    let head = repo.find_reference("HEAD").ok()?;
    let target = head.symbolic_target().ok()??;
    let name = target.strip_prefix("refs/heads/")?;
    Some(Head::Branch(name.to_string()))
}

/// Seven characters, which is what git shows and what fits in a chip.
fn short(sha: &str) -> String {
    sha.chars().take(7).collect()
}

/// One branch that can be compared against another.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Branch {
    /// `main`, or `origin/main` for a remote-tracking branch — which is also
    /// exactly what git resolves it by.
    pub name: String,
    pub remote: bool,
    /// The branch `HEAD` is on.
    pub current: bool,
}

/// Every branch worth offering, local ones first and the current one at the top
/// of them, then the remote-tracking ones.
///
/// `origin/HEAD` is left out: it is a pointer at whichever branch the remote
/// calls default, so it is always a second name for one of the rows below it.
pub fn list(root: &Path) -> Vec<Branch> {
    let Ok(repo) = Repository::discover(root) else {
        return Vec::new();
    };
    let current = head_of(root).and_then(|h| match h {
        Head::Branch(name) => Some(name),
        Head::Detached(_) => None,
    });

    let mut out = Vec::new();
    for kind in [BranchType::Local, BranchType::Remote] {
        let Ok(branches) = repo.branches(Some(kind)) else {
            continue;
        };
        let remote = kind == BranchType::Remote;
        let mut group: Vec<Branch> = branches
            .flatten()
            .filter_map(|(b, _)| b.name().ok().flatten().map(String::from))
            .filter(|name| !(remote && name.ends_with("/HEAD")))
            .map(|name| Branch {
                current: !remote && current.as_deref() == Some(name.as_str()),
                remote,
                name,
            })
            .collect();
        // The one you are on is the one you are most likely to be comparing.
        group.sort_by(|a, b| b.current.cmp(&a.current).then_with(|| a.name.cmp(&b.name)));
        out.extend(group);
    }
    out
}

/// The names a repository tends to give its trunk, in the order they are worth
/// guessing.
const TRUNKS: [&str; 4] = ["main", "master", "trunk", "develop"];

/// The pair to open the compare panel on, as `(base, head)`.
///
/// On a branch off the trunk, that is this branch held against what it came
/// from — its upstream when it has one, since that is the comparison its pull
/// request is going to make. On the trunk itself there is nothing above it to
/// be compared to, so the offer turns around: the trunk as the base, and
/// another branch as the head, which is the direction anything gets read in.
///
/// `None` outside a repository. The two come back equal on a repository with
/// one branch and nothing to hold it against; that is the honest answer, and it
/// makes an empty comparison rather than a wrong one.
pub fn default_pair(root: &Path) -> Option<(String, String)> {
    let repo = Repository::discover(root).ok()?;
    let head = head_of(root)?;
    let name = head.name().to_string();

    let upstream = match &head {
        Head::Branch(branch) => repo
            .find_branch(branch, BranchType::Local)
            .ok()
            .and_then(|b| b.upstream().ok())
            .and_then(|up| up.name().ok().flatten().map(String::from)),
        Head::Detached(_) => None,
    };
    if let Some(base) = upstream {
        return Some((base, name));
    }

    let others = list(root);
    let has = |want: &str| others.iter().any(|b| b.name == want);
    if let Some(trunk) = TRUNKS.iter().find(|t| ***t != *name && has(t)) {
        return Some((trunk.to_string(), name));
    }
    let other = others
        .iter()
        .find(|b| !b.remote && b.name != name)
        .map(|b| b.name.clone());
    Some(match other {
        Some(other) => (name, other),
        None => (name.clone(), name),
    })
}

/// A file that differs between the two sides.
#[derive(Clone, PartialEq, Debug)]
pub struct ChangedFile {
    pub path: PathBuf,
    /// Set for renames — where to read the base side from.
    pub previous_path: Option<PathBuf>,
    pub status: ChangeKind,
}

impl ChangedFile {
    pub fn base_path(&self) -> &PathBuf {
        self.previous_path.as_ref().unwrap_or(&self.path)
    }
}

/// Two branches, and what is between them.
#[derive(Clone, PartialEq, Debug)]
pub struct CompareView {
    /// The refs as chosen, which is what the breadcrumb says.
    pub base: String,
    pub head: String,
    /// The commit each side is actually read from. `base_sha` is the merge
    /// base, not the tip of the base branch.
    pub base_sha: String,
    pub head_sha: String,
    /// The object database both are read out of — the repository's own.
    pub git_dir: PathBuf,
    pub files: Vec<ChangedFile>,
    /// Every file at `head_sha`, so the explorer shows a repository rather than
    /// a list of changes.
    pub tree: Vec<PathBuf>,
    /// Commits on the head branch that the base does not have, and the other
    /// way round.
    pub ahead: usize,
    pub behind: usize,
    /// True when the two share no history at all, so there is no merge base and
    /// the comparison is against the base branch's tip instead.
    pub unrelated: bool,
}

impl CompareView {
    /// `main … feature`, for the breadcrumb and the explorer's root.
    pub fn label(&self) -> String {
        format!("{} … {}", self.base, self.head)
    }

    /// Where to read `rel`'s base side from — its old path if it was renamed.
    pub fn base_path(&self, rel: &Path) -> PathBuf {
        self.files
            .iter()
            .find(|f| f.path == rel)
            .map(|f| f.base_path().clone())
            .unwrap_or_else(|| rel.to_path_buf())
    }

    /// The change list as the map the tree and viewer already speak.
    pub fn statuses(&self) -> HashMap<PathBuf, ChangeKind> {
        self.files.iter().map(|f| (f.path.clone(), f.status)).collect()
    }
}

fn kind_of(delta: Delta) -> Option<ChangeKind> {
    match delta {
        Delta::Added | Delta::Copied => Some(ChangeKind::Added),
        Delta::Deleted => Some(ChangeKind::Deleted),
        Delta::Renamed => Some(ChangeKind::Renamed),
        Delta::Modified | Delta::Typechange => Some(ChangeKind::Modified),
        Delta::Conflicted => Some(ChangeKind::Conflicted),
        // Unmodified, ignored, untracked, unreadable: not part of a tree diff.
        _ => None,
    }
}

/// Compare two revisions — branches, tags, or shas.
///
/// Blocking (it walks two trees and diffs them), so callers run it off the UI
/// thread the way they run a clone off it.
pub fn compare(root: &Path, base: &str, head: &str) -> Result<CompareView> {
    let repo = Repository::discover(root)
        .with_context(|| format!("{} is not a git repository", root.display()))?;

    let resolve = |rev: &str| -> Result<Oid> {
        Ok(repo
            .revparse_single(rev)
            .with_context(|| format!("there is no branch or commit called “{rev}”"))?
            .peel_to_commit()
            .with_context(|| format!("“{rev}” does not name a commit"))?
            .id())
    };
    let base_tip = resolve(base)?;
    let head_id = resolve(head)?;

    // No merge base means two histories that never met — an orphan branch, or
    // a repository with an imported one. Diffing the tips is then the only
    // comparison there is, and the caller says so rather than pretending.
    let merge_base = repo.merge_base(base_tip, head_id).ok();
    let base_id = merge_base.unwrap_or(base_tip);

    let base_tree = repo.find_commit(base_id)?.tree()?;
    let head_tree = repo.find_commit(head_id)?.tree()?;
    let mut diff = repo
        .diff_tree_to_tree(Some(&base_tree), Some(&head_tree), None)
        .context("diffing the two commits")?;
    // Off by default, and it is what tells a file that moved from a file that
    // was deleted and another one written.
    let _ = diff.find_similar(Some(DiffFindOptions::new().renames(true)));

    let files: Vec<ChangedFile> = diff
        .deltas()
        .filter_map(|d| {
            let status = kind_of(d.status())?;
            let path = d.new_file().path().or_else(|| d.old_file().path())?;
            let previous = d
                .old_file()
                .path()
                .filter(|old| status == ChangeKind::Renamed && *old != path)
                .map(PathBuf::from);
            Some(ChangedFile {
                path: path.to_path_buf(),
                previous_path: previous,
                status,
            })
        })
        .collect();

    let git_dir = repo.path().to_path_buf();
    let head_sha = head_id.to_string();
    // The same walk a pull request's tree comes out of — one file list, one
    // set of rules about what is in it.
    let tree = super::mirror::tree_paths(&git_dir, &head_sha).unwrap_or_default();
    let (ahead, behind) = repo.graph_ahead_behind(head_id, base_tip).unwrap_or((0, 0));

    Ok(CompareView {
        base: base.to_string(),
        head: head.to_string(),
        base_sha: base_id.to_string(),
        head_sha,
        git_dir,
        files,
        tree,
        ahead,
        behind,
        unrelated: merge_base.is_none(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repository built commit by commit, so the tests can state exactly what
    /// they are comparing.
    struct Fixture {
        dir: PathBuf,
        repo: Repository,
    }

    impl Fixture {
        fn new(name: &str) -> Fixture {
            let dir = std::env::temp_dir().join(format!("pullspace-branches-{name}"));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("make the test repo directory");
            let repo = Repository::init(&dir).expect("git init");
            // Not everyone's global config has these, and committing needs them.
            let mut cfg = repo.config().expect("config");
            cfg.set_str("user.name", "pullspace tests").unwrap();
            cfg.set_str("user.email", "tests@pullspace.invalid").unwrap();
            Fixture { dir, repo }
        }

        fn write(&self, rel: &str, text: &str) {
            let path = self.dir.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, text).unwrap();
        }

        fn remove(&self, rel: &str) {
            std::fs::remove_file(self.dir.join(rel)).unwrap();
        }

        fn commit(&self, message: &str) {
            let mut index = self.repo.index().unwrap();
            index
                .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
                .unwrap();
            // `add_all` does not notice a file that is gone; this does.
            index.update_all(["*"], None).unwrap();
            index.write().unwrap();
            let tree = self.repo.find_tree(index.write_tree().unwrap()).unwrap();
            let sig = self.repo.signature().unwrap();
            let parents = match self.repo.head().ok().and_then(|h| h.peel_to_commit().ok()) {
                Some(c) => vec![c],
                None => Vec::new(),
            };
            let borrowed: Vec<&git2::Commit> = parents.iter().collect();
            self.repo
                .commit(Some("HEAD"), &sig, &sig, message, &tree, &borrowed)
                .unwrap();
        }

        fn branch(&self, name: &str) {
            let head = self.repo.head().unwrap().peel_to_commit().unwrap();
            self.repo.branch(name, &head, true).unwrap();
        }

        fn checkout(&self, name: &str) {
            let refname = format!("refs/heads/{name}");
            let obj = self.repo.revparse_single(&refname).unwrap();
            self.repo.checkout_tree(&obj, None).unwrap();
            self.repo.set_head(&refname).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn status_of(view: &CompareView, path: &str) -> Option<ChangeKind> {
        view.files
            .iter()
            .find(|f| f.path == Path::new(path))
            .map(|f| f.status)
    }

    #[test]
    fn head_names_the_branch_and_then_the_commit() {
        let fx = Fixture::new("head");
        // Before the first commit there is still a branch to be on.
        assert!(matches!(head_of(&fx.dir), Some(Head::Branch(_))));

        fx.write("a.txt", "one\n");
        fx.commit("first");
        let Some(Head::Branch(name)) = head_of(&fx.dir) else {
            panic!("expected to be on a branch")
        };

        // Detached: the label says so rather than naming a branch.
        let sha = fx.repo.head().unwrap().peel_to_commit().unwrap().id();
        fx.repo.set_head_detached(sha).unwrap();
        let head = head_of(&fx.dir).unwrap();
        assert!(matches!(head, Head::Detached(_)), "{head:?}");
        assert!(head.label().starts_with("detached @ "), "{head:?}");
        assert_ne!(head.label(), name);

        // A directory that is not a repository has no branch, and does not panic.
        assert_eq!(head_of(Path::new("/")), None);
    }

    #[test]
    fn branches_list_with_the_current_one_first() {
        let fx = Fixture::new("list");
        fx.write("a.txt", "one\n");
        fx.commit("first");
        fx.branch("feature");
        fx.branch("aaa-other");

        let names: Vec<String> = list(&fx.dir).iter().map(|b| b.name.clone()).collect();
        let current = list(&fx.dir).into_iter().find(|b| b.current).unwrap();
        assert_eq!(names[0], current.name, "the branch HEAD is on leads: {names:?}");
        assert!(names.contains(&"feature".to_string()), "{names:?}");
        // The rest are alphabetical, so a long list stays scannable.
        assert!(
            names[1..].windows(2).all(|w| w[0] <= w[1]),
            "not sorted: {names:?}"
        );
        assert!(list(&fx.dir).iter().all(|b| !b.remote), "no remotes here");
    }

    #[test]
    fn a_comparison_is_the_changed_files_and_the_whole_tree() {
        let fx = Fixture::new("compare");
        fx.write("keep.txt", "unchanged\n");
        fx.write("edit.txt", "before\n");
        fx.write("gone.txt", "doomed\n");
        fx.commit("base");
        fx.branch("feature");
        fx.checkout("feature");
        fx.write("edit.txt", "after\n");
        fx.write("new.txt", "fresh\n");
        fx.remove("gone.txt");
        fx.commit("work");

        let view = compare(&fx.dir, "master", "feature")
            .or_else(|_| compare(&fx.dir, "main", "feature"))
            .expect("compare");

        assert_eq!(status_of(&view, "edit.txt"), Some(ChangeKind::Modified));
        assert_eq!(status_of(&view, "new.txt"), Some(ChangeKind::Added));
        assert_eq!(status_of(&view, "gone.txt"), Some(ChangeKind::Deleted));
        assert_eq!(status_of(&view, "keep.txt"), None, "untouched files are not changes");

        // The explorer needs everything at the head, not just what moved.
        assert!(view.tree.contains(&PathBuf::from("keep.txt")), "{:?}", view.tree);
        assert!(!view.tree.contains(&PathBuf::from("gone.txt")));
        assert_eq!(view.ahead, 1, "one commit on the branch");
        assert_eq!(view.behind, 0);
        assert!(!view.unrelated);
        assert_eq!(view.statuses().len(), 3);
    }

    #[test]
    fn the_base_branch_moving_on_is_not_part_of_the_comparison() {
        // The whole reason for diffing against the merge base: work that
        // landed on the base after the branch left it belongs to neither side.
        let fx = Fixture::new("mergebase");
        fx.write("shared.txt", "start\n");
        fx.commit("base");
        let base = head_of(&fx.dir).unwrap().name().to_string();
        fx.branch("feature");

        fx.write("on-base.txt", "landed later\n");
        fx.commit("moved on");

        fx.checkout("feature");
        fx.write("on-branch.txt", "branch work\n");
        fx.commit("branch work");

        let view = compare(&fx.dir, &base, "feature").expect("compare");
        assert_eq!(status_of(&view, "on-branch.txt"), Some(ChangeKind::Added));
        assert_eq!(
            status_of(&view, "on-base.txt"),
            None,
            "a commit on the base branch is not the branch deleting it"
        );
        assert_eq!((view.ahead, view.behind), (1, 1));
        // The base side is the merge base, not the tip it was named by.
        let tip = fx.repo.revparse_single(&base).unwrap().id().to_string();
        assert_ne!(view.base_sha, tip);
    }

    #[test]
    fn a_renamed_file_remembers_where_to_read_its_old_side() {
        let fx = Fixture::new("rename");
        let body = "line one\nline two\nline three\nline four\n";
        fx.write("old.txt", body);
        fx.commit("base");
        let base = head_of(&fx.dir).unwrap().name().to_string();
        fx.branch("feature");
        fx.checkout("feature");
        fx.remove("old.txt");
        fx.write("new.txt", body);
        fx.commit("move it");

        let view = compare(&fx.dir, &base, "feature").expect("compare");
        assert_eq!(status_of(&view, "new.txt"), Some(ChangeKind::Renamed));
        assert_eq!(view.base_path(Path::new("new.txt")), PathBuf::from("old.txt"));
        // Anything else reads from its own path.
        assert_eq!(view.base_path(Path::new("other.txt")), PathBuf::from("other.txt"));
    }

    #[test]
    fn the_pair_to_start_from_is_this_branch_against_the_trunk() {
        let fx = Fixture::new("pair");
        fx.write("a.txt", "one\n");
        fx.commit("first");
        let trunk = head_of(&fx.dir).unwrap().name().to_string();

        // Nothing to compare against yet: both ends are this branch, which is
        // an empty comparison rather than a wrong one.
        assert_eq!(default_pair(&fx.dir), Some((trunk.clone(), trunk.clone())));

        fx.branch("feature");
        // Standing on the trunk, the comparison worth offering is outward:
        // what the other branch adds, not what the trunk is missing.
        assert_eq!(
            default_pair(&fx.dir),
            Some((trunk.clone(), "feature".to_string()))
        );

        fx.checkout("feature");
        let (base, head) = default_pair(&fx.dir).unwrap();
        assert_eq!(head, "feature");
        assert_eq!(base, trunk, "the branch it came from, not itself");

        assert_eq!(default_pair(Path::new("/")), None);
    }

    #[test]
    fn a_missing_branch_says_which_one() {
        let fx = Fixture::new("missing");
        fx.write("a.txt", "one\n");
        fx.commit("first");
        let err = compare(&fx.dir, "nope", "also-nope").expect_err("no such branch");
        assert!(format!("{err:#}").contains("nope"), "{err:#}");
    }

    #[test]
    fn unrelated_histories_compare_against_the_tip_instead() {
        let fx = Fixture::new("orphan");
        fx.write("a.txt", "one\n");
        fx.commit("first");
        let base = head_of(&fx.dir).unwrap().name().to_string();

        // An orphan branch: no parent, so no merge base with anything.
        fx.repo.set_head("refs/heads/orphan").unwrap();
        fx.repo.index().unwrap().clear().unwrap();
        std::fs::remove_file(fx.dir.join("a.txt")).unwrap();
        fx.write("b.txt", "two\n");
        fx.commit("unrelated");

        let view = compare(&fx.dir, &base, "orphan").expect("compare");
        assert!(view.unrelated);
        let tip = fx.repo.revparse_single(&base).unwrap().id().to_string();
        assert_eq!(view.base_sha, tip, "nothing else to diff against");
        assert_eq!(status_of(&view, "b.txt"), Some(ChangeKind::Added));
    }
}
