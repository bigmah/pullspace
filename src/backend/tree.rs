use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Untracked,
    Conflicted,
}

impl ChangeKind {
    pub fn badge(&self) -> &'static str {
        match self {
            ChangeKind::Added => "A",
            ChangeKind::Modified => "M",
            ChangeKind::Deleted => "D",
            ChangeKind::Renamed => "R",
            ChangeKind::Untracked => "U",
            ChangeKind::Conflicted => "!",
        }
    }

    pub fn css(&self) -> &'static str {
        match self {
            ChangeKind::Added | ChangeKind::Untracked => "st-added",
            ChangeKind::Modified | ChangeKind::Renamed => "st-modified",
            ChangeKind::Deleted => "st-deleted",
            ChangeKind::Conflicted => "st-conflict",
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct FileNode {
    pub name: String,
    pub path: PathBuf, // relative to repo root
    pub is_dir: bool,
    pub children: Vec<FileNode>,
    pub status: Option<ChangeKind>,
    pub contains_changes: bool,
}

#[derive(Default)]
struct DirTmp {
    dirs: BTreeMap<String, DirTmp>,
    files: BTreeMap<String, PathBuf>,
}

impl DirTmp {
    fn insert(&mut self, rel: &Path) {
        let mut parts: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        if parts.is_empty() {
            return;
        }
        let file = parts.pop().unwrap();
        let mut cur = self;
        for p in parts {
            cur = cur.dirs.entry(p).or_default();
        }
        cur.files.insert(file, rel.to_path_buf());
    }

    fn build(self, name: String, path: PathBuf, statuses: &HashMap<PathBuf, ChangeKind>) -> FileNode {
        let mut children = Vec::new();
        let mut contains_changes = false;
        for (dname, dtmp) in self.dirs {
            let dpath = if path.as_os_str().is_empty() {
                PathBuf::from(&dname)
            } else {
                path.join(&dname)
            };
            let child = dtmp.build(dname, dpath, statuses);
            contains_changes |= child.contains_changes || child.status.is_some();
            children.push(child);
        }
        for (fname, fpath) in self.files {
            let status = statuses.get(&fpath).copied();
            contains_changes |= status.is_some();
            children.push(FileNode {
                name: fname,
                path: fpath,
                is_dir: false,
                children: Vec::new(),
                status,
                contains_changes: false,
            });
        }
        FileNode {
            name,
            path,
            is_dir: true,
            children,
            status: None,
            contains_changes,
        }
    }
}

/// Walk the working tree (respecting .gitignore) and merge in any paths known
/// only from git status (e.g. deleted files that no longer exist on disk).
pub fn build_tree(root: &Path, statuses: &HashMap<PathBuf, ChangeKind>) -> FileNode {
    let mut tmp = DirTmp::default();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .follow_links(false)
        .build();
    for entry in walker.flatten() {
        if entry.file_type().is_some_and(|t| t.is_file()) {
            if let Ok(rel) = entry.path().strip_prefix(root) {
                tmp.insert(rel);
            }
        }
    }
    for rel in statuses.keys() {
        tmp.insert(rel);
    }
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_string());
    tmp.build(name, PathBuf::new(), statuses)
}

/// Build a tree from an explicit path list. Used for a pull request, where
/// there is no working directory to walk — `paths` is the repository tree at
/// the PR's head.
///
/// Status paths are merged in the same way [`build_tree`] merges them over the
/// worktree: a file deleted by the PR is absent from the head tree but still
/// belongs in the explorer. Passing an empty `paths` degrades to a
/// changed-files-only tree.
pub fn build_tree_from_paths<'a>(
    label: &str,
    paths: impl IntoIterator<Item = &'a Path>,
    statuses: &HashMap<PathBuf, ChangeKind>,
) -> FileNode {
    let mut tmp = DirTmp::default();
    for rel in paths {
        tmp.insert(rel);
    }
    for rel in statuses.keys() {
        tmp.insert(rel);
    }
    tmp.build(label.to_string(), PathBuf::new(), statuses)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn statuses(pairs: &[(&str, ChangeKind)]) -> HashMap<PathBuf, ChangeKind> {
        pairs
            .iter()
            .map(|(p, k)| (PathBuf::from(p), *k))
            .collect()
    }

    fn names(node: &FileNode) -> Vec<String> {
        let mut out = Vec::new();
        for c in &node.children {
            out.push(c.path.display().to_string());
            out.extend(names(c));
        }
        out
    }

    #[test]
    fn unchanged_files_are_kept_and_changed_ones_badged() {
        let paths = [Path::new("src/a.rs"), Path::new("src/b.rs")];
        let st = statuses(&[("src/a.rs", ChangeKind::Modified)]);
        let tree = build_tree_from_paths("pr", paths, &st);

        let listed = names(&tree);
        assert!(listed.contains(&"src/b.rs".to_string()), "{listed:?}");

        let src = tree.children.iter().find(|c| c.is_dir).unwrap();
        let a = src.children.iter().find(|c| c.name == "a.rs").unwrap();
        let b = src.children.iter().find(|c| c.name == "b.rs").unwrap();
        assert_eq!(a.status, Some(ChangeKind::Modified));
        assert_eq!(b.status, None);
        assert!(src.contains_changes, "parent dir marked so it auto-expands");
    }

    #[test]
    fn files_deleted_by_the_pr_still_appear() {
        // A deleted file is absent from the head tree, but must stay visible.
        let paths = [Path::new("keep.rs")];
        let st = statuses(&[("gone.rs", ChangeKind::Deleted)]);
        let tree = build_tree_from_paths("pr", paths, &st);
        let listed = names(&tree);
        assert!(listed.contains(&"gone.rs".to_string()), "{listed:?}");
    }

    #[test]
    fn empty_path_list_degrades_to_changed_files_only() {
        let st = statuses(&[("only.rs", ChangeKind::Added)]);
        let tree = build_tree_from_paths("pr", std::iter::empty(), &st);
        assert_eq!(names(&tree), vec!["only.rs".to_string()]);
    }

    #[test]
    fn changes_filter_strips_unchanged_files() {
        let paths = [Path::new("src/a.rs"), Path::new("docs/b.md")];
        let st = statuses(&[("src/a.rs", ChangeKind::Modified)]);
        let tree = build_tree_from_paths("pr", paths, &st);
        let filtered = filter_changed(&tree).expect("something changed");
        let listed = names(&filtered);
        assert!(listed.contains(&"src/a.rs".to_string()), "{listed:?}");
        assert!(!listed.contains(&"docs/b.md".to_string()), "{listed:?}");
    }
}

/// Reduce the tree to only nodes that are changed or contain changes.
pub fn filter_changed(node: &FileNode) -> Option<FileNode> {
    if node.is_dir {
        let children: Vec<FileNode> = node.children.iter().filter_map(filter_changed).collect();
        if children.is_empty() {
            return None;
        }
        let mut out = node.clone();
        out.children = children;
        Some(out)
    } else if node.status.is_some() {
        Some(node.clone())
    } else {
        None
    }
}
