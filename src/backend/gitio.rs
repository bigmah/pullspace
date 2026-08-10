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

/// Map of repo-relative path -> change kind, combining index and worktree state.
pub fn load_statuses(root: &Path) -> HashMap<PathBuf, ChangeKind> {
    let mut out = HashMap::new();
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
            out.insert(PathBuf::from(p), kind);
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

/// Content of a file in the working tree, or Absent if it is not on disk.
pub fn worktree_file(root: &Path, rel: &Path) -> FileContent {
    match std::fs::read(root.join(rel)) {
        Ok(bytes) => FileContent::from_bytes(&bytes),
        Err(_) => FileContent::Absent,
    }
}
