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
