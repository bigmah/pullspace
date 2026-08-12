use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use git2::{Repository, Status, StatusOptions};

use super::tree::ChangeKind;
use super::FileContent;

/// Resolve the repository work-dir root from any path inside it.
pub fn discover_root(path: &Path) -> Result<PathBuf> {
    let repo = Repository::discover(path)?;
    let root = repo
        .workdir()
        .ok_or_else(|| anyhow::anyhow!("bare repository has no working directory"))?
        .to_path_buf();
    Ok(root)
}

/// Which side of `git add` a working-tree change is on.
///
/// Only the working tree has one. A pull request and a branch comparison are
/// both diffs between two commits, and there is no index anywhere in that —
/// which is why this is kept beside the change kinds rather than inside them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    /// In the index and matching what is on disk: `git commit` would record
    /// exactly this.
    Staged,
    /// On disk only — never added, or added and then reverted in the index.
    Unstaged,
    /// Both, and they differ: part of this file is staged and part of it is
    /// not. The one state where "the diff" is genuinely two diffs.
    Both,
}

impl Stage {
    /// What to call it, in the words `git add` uses.
    pub fn label(&self) -> &'static str {
        match self {
            Stage::Staged => "staged",
            Stage::Unstaged => "unstaged",
            Stage::Both => "partly staged",
        }
    }

    /// The longer form, for a tooltip that has room to say why it matters.
    ///
    /// Kind-agnostic on purpose: the same three sentences have to be true of a
    /// file that was written, one that was renamed and one that was deleted.
    pub fn note(&self) -> &'static str {
        match self {
            Stage::Staged => "staged — the index has this change, and the file on disk agrees",
            Stage::Unstaged => "not staged — this is on disk only, and no commit would take it",
            Stage::Both => "staged, and changed again since — the two halves differ",
        }
    }

    /// Modifier class for the explorer badge.
    pub fn css(&self) -> &'static str {
        match self {
            Stage::Staged => "stg",
            Stage::Unstaged => "unstg",
            Stage::Both => "half",
        }
    }

    pub fn is_staged(&self) -> bool {
        matches!(self, Stage::Staged | Stage::Both)
    }

    pub fn is_unstaged(&self) -> bool {
        matches!(self, Stage::Unstaged | Stage::Both)
    }
}

/// The working tree's changes: what changed, and which side of `git add` each
/// change is on.
#[derive(Clone, Default, PartialEq)]
pub struct Changes {
    /// Repo-relative path -> change kind, index and worktree state combined.
    /// What the explorer badges and the viewer diff against HEAD.
    pub kinds: HashMap<PathBuf, ChangeKind>,
    /// The same paths, split by staging. Empty for anything that is not the
    /// local working tree.
    pub stages: HashMap<PathBuf, Stage>,
}

/// Which side of the index a status flag set falls on.
///
/// git reports the two halves separately — `INDEX_*` is what `git add` put
/// there, `WT_*` is what the file on disk says now — so a file that was staged
/// and then edited again carries both.
fn stage_of(s: Status) -> Stage {
    let indexed = s.intersects(
        Status::INDEX_NEW
            | Status::INDEX_MODIFIED
            | Status::INDEX_DELETED
            | Status::INDEX_RENAMED
            | Status::INDEX_TYPECHANGE,
    );
    let on_disk = s.intersects(
        Status::WT_NEW
            | Status::WT_MODIFIED
            | Status::WT_DELETED
            | Status::WT_RENAMED
            | Status::WT_TYPECHANGE,
    );
    match (indexed, on_disk) {
        (true, true) => Stage::Both,
        (true, false) => Stage::Staged,
        // Untracked files, and what a conflict leaves behind: nothing about
        // either is in the index yet.
        _ => Stage::Unstaged,
    }
}

/// Read the working tree's status: what changed, and what has been added.
pub fn load_changes(root: &Path) -> Changes {
    let mut out = Changes::default();
    let Ok(repo) = Repository::discover(root) else {
        return out;
    };
    let mut opts = StatusOptions::new();
    opts.include_untracked(true)
        .recurse_untracked_dirs(true)
        .include_ignored(false)
        .renames_head_to_index(true);
    let Ok(statuses) = repo.statuses(Some(&mut opts)) else {
        return out;
    };
    for entry in statuses.iter() {
        let s = entry.status();
        let kind = if s.contains(Status::CONFLICTED) {
            ChangeKind::Conflicted
        } else if s.contains(Status::WT_DELETED) || s.contains(Status::INDEX_DELETED) {
            ChangeKind::Deleted
        } else if s.contains(Status::WT_NEW) {
            ChangeKind::Untracked
        } else if s.contains(Status::INDEX_NEW) {
            ChangeKind::Added
        } else if s.contains(Status::INDEX_RENAMED) || s.contains(Status::WT_RENAMED) {
            ChangeKind::Renamed
        } else if s.contains(Status::WT_MODIFIED)
            || s.contains(Status::INDEX_MODIFIED)
            || s.contains(Status::WT_TYPECHANGE)
            || s.contains(Status::INDEX_TYPECHANGE)
        {
            ChangeKind::Modified
        } else {
            continue;
        };
        if let Ok(p) = entry.path() {
            let path = PathBuf::from(p);
            out.stages.insert(path.clone(), stage_of(s));
            out.kinds.insert(path, kind);
        }
    }
    out
}

/// Content of a file as of HEAD, or Absent if there is no HEAD (unborn branch)
/// or the path is not in the HEAD tree.
pub fn head_file(root: &Path, rel: &Path) -> FileContent {
    let Ok(repo) = Repository::discover(root) else {
        return FileContent::Absent;
    };
    let Ok(head) = repo.head() else {
        return FileContent::Absent;
    };
    let Ok(tree) = head.peel_to_tree() else {
        return FileContent::Absent;
    };
    let Ok(entry) = tree.get_path(rel) else {
        return FileContent::Absent;
    };
    let Ok(obj) = entry.to_object(&repo) else {
        return FileContent::Absent;
    };
    let Some(blob) = obj.as_blob() else {
        return FileContent::Absent;
    };
    if blob.is_binary() {
        FileContent::Binary
    } else {
        FileContent::Text(String::from_utf8_lossy(blob.content()).into_owned())
    }
}

/// Content of a file as it is staged — what `git commit` would record right
/// now — or Absent if the index has no entry for it.
pub fn index_file(root: &Path, rel: &Path) -> FileContent {
    let Ok(repo) = Repository::discover(root) else {
        return FileContent::Absent;
    };
    let Ok(index) = repo.index() else {
        return FileContent::Absent;
    };
    // Stage 0 is the ordinary entry. A conflicted path has stages 1-3 instead —
    // base, ours and theirs — and no single staged version to show.
    let Some(entry) = index.get_path(rel, 0) else {
        return FileContent::Absent;
    };
    let Ok(blob) = repo.find_blob(entry.id) else {
        return FileContent::Absent;
    };
    if blob.is_binary() {
        FileContent::Binary
    } else {
        FileContent::Text(String::from_utf8_lossy(blob.content()).into_owned())
    }
}

/// Content of a file in the working tree, or Absent if it is not on disk.
pub fn worktree_file(root: &Path, rel: &Path) -> FileContent {
    match std::fs::read(root.join(rel)) {
        Ok(bytes) => FileContent::from_bytes(&bytes),
        Err(_) => FileContent::Absent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repository with an index worth reading, so the tests can say exactly
    /// what was added and when.
    struct Fixture {
        dir: PathBuf,
        repo: Repository,
    }

    impl Fixture {
        fn new(name: &str) -> Fixture {
            let dir = std::env::temp_dir().join(format!("pullspace-gitio-{name}"));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("make the test repo directory");
            let repo = Repository::init(&dir).expect("git init");
            let mut cfg = repo.config().expect("config");
            cfg.set_str("user.name", "pullspace tests").unwrap();
            cfg.set_str("user.email", "tests@pullspace.invalid").unwrap();
            Fixture { dir, repo }
        }

        fn write(&self, rel: &str, text: &str) {
            std::fs::write(self.dir.join(rel), text).unwrap();
        }

        /// `git add`, and nothing else — the whole point of these tests.
        fn add(&self, rel: &str) {
            let mut index = self.repo.index().unwrap();
            index.add_path(Path::new(rel)).unwrap();
            index.write().unwrap();
        }

        fn commit(&self, message: &str) {
            let mut index = self.repo.index().unwrap();
            index
                .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
                .unwrap();
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

        fn stage_of_path(&self, rel: &str) -> Option<Stage> {
            load_changes(&self.dir).stages.get(Path::new(rel)).copied()
        }

        fn kind_of(&self, rel: &str) -> Option<ChangeKind> {
            load_changes(&self.dir).kinds.get(Path::new(rel)).copied()
        }

        fn sides(&self, rel: &str) -> (FileContent, FileContent, FileContent) {
            let rel = Path::new(rel);
            (
                head_file(&self.dir, rel),
                index_file(&self.dir, rel),
                worktree_file(&self.dir, rel),
            )
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn text(c: &FileContent) -> Option<&str> {
        match c {
            FileContent::Text(t) => Some(t),
            _ => None,
        }
    }

    #[test]
    fn adding_a_change_moves_it_across_the_index() {
        let fx = Fixture::new("across");
        fx.write("a.txt", "one\n");
        fx.commit("first");
        assert_eq!(fx.stage_of_path("a.txt"), None, "nothing changed yet");

        fx.write("a.txt", "two\n");
        assert_eq!(fx.stage_of_path("a.txt"), Some(Stage::Unstaged));
        // Not added, so the index still says exactly what HEAD says — which is
        // why the unstaged diff is index-against-disk and not HEAD-against-disk.
        let (head, index, disk) = fx.sides("a.txt");
        assert_eq!(text(&head), Some("one\n"));
        assert_eq!(text(&index), Some("one\n"));
        assert_eq!(text(&disk), Some("two\n"));

        fx.add("a.txt");
        assert_eq!(fx.stage_of_path("a.txt"), Some(Stage::Staged));
        assert_eq!(text(&fx.sides("a.txt").1), Some("two\n"));

        // Edited again after adding: the one state where the two diffs differ.
        fx.write("a.txt", "three\n");
        assert_eq!(fx.stage_of_path("a.txt"), Some(Stage::Both));
        let (head, index, disk) = fx.sides("a.txt");
        assert_eq!(
            (text(&head), text(&index), text(&disk)),
            (Some("one\n"), Some("two\n"), Some("three\n")),
            "three distinct sides, which is what the three views show"
        );
    }

    #[test]
    fn a_new_file_is_unstaged_until_it_is_added() {
        let fx = Fixture::new("newfile");
        fx.write("a.txt", "one\n");
        fx.commit("first");

        fx.write("b.txt", "fresh\n");
        assert_eq!(fx.kind_of("b.txt"), Some(ChangeKind::Untracked));
        assert_eq!(fx.stage_of_path("b.txt"), Some(Stage::Unstaged));
        // Nothing in the index to show, so the staged view has nothing for it.
        assert!(matches!(fx.sides("b.txt").1, FileContent::Absent));

        fx.add("b.txt");
        assert_eq!(fx.kind_of("b.txt"), Some(ChangeKind::Added));
        assert_eq!(fx.stage_of_path("b.txt"), Some(Stage::Staged));
        assert_eq!(text(&fx.sides("b.txt").1), Some("fresh\n"));
    }

    #[test]
    fn deleting_a_file_is_staged_the_same_way() {
        let fx = Fixture::new("deleted");
        fx.write("a.txt", "one\n");
        fx.write("b.txt", "two\n");
        fx.commit("first");

        std::fs::remove_file(fx.dir.join("a.txt")).unwrap();
        assert_eq!(fx.kind_of("a.txt"), Some(ChangeKind::Deleted));
        assert_eq!(fx.stage_of_path("a.txt"), Some(Stage::Unstaged));
        // Still in the index, which is what the unstaged diff removes.
        assert_eq!(text(&fx.sides("a.txt").1), Some("one\n"));

        let mut index = fx.repo.index().unwrap();
        index.remove_path(Path::new("a.txt")).unwrap();
        index.write().unwrap();
        assert_eq!(fx.kind_of("a.txt"), Some(ChangeKind::Deleted));
        assert_eq!(fx.stage_of_path("a.txt"), Some(Stage::Staged));
        assert!(matches!(fx.sides("a.txt").1, FileContent::Absent));
    }

    #[test]
    fn staged_and_unstaged_halves_are_told_apart() {
        // Added and not touched since.
        assert_eq!(stage_of(Status::INDEX_NEW), Stage::Staged);
        assert_eq!(stage_of(Status::INDEX_MODIFIED), Stage::Staged);
        // Edited on disk, never added.
        assert_eq!(stage_of(Status::WT_MODIFIED), Stage::Unstaged);
        assert_eq!(stage_of(Status::WT_NEW), Stage::Unstaged);
        // Added, then edited again — the case the two diffs exist for.
        assert_eq!(
            stage_of(Status::INDEX_MODIFIED | Status::WT_MODIFIED),
            Stage::Both
        );
        assert_eq!(
            stage_of(Status::INDEX_NEW | Status::WT_MODIFIED),
            Stage::Both
        );
        // A conflict is nobody's staged version.
        assert_eq!(stage_of(Status::CONFLICTED), Stage::Unstaged);
    }

    #[test]
    fn a_stage_knows_which_diffs_it_has() {
        assert!(Stage::Staged.is_staged() && !Stage::Staged.is_unstaged());
        assert!(Stage::Unstaged.is_unstaged() && !Stage::Unstaged.is_staged());
        assert!(Stage::Both.is_staged() && Stage::Both.is_unstaged());
    }
}
