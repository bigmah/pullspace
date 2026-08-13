use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
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
    /// Changed files anywhere below this directory; 0 for a file.
    ///
    /// A count rather than a flag, because a folder that is closed still has to
    /// answer "how much is in there" — which is the whole question a reviewer
    /// is asking when they look at a collapsed tree.
    pub changed: usize,
}

impl FileNode {
    /// Whether this directory holds anything changed. Directories that do are
    /// the ones a review wants open on arrival.
    pub fn has_changes(&self) -> bool {
        self.changed > 0
    }

    /// True for the node the tree is rooted at — the repository, the pull
    /// request, the comparison. It is the one directory that always starts
    /// open, and it is the only one whose path is empty.
    fn is_root(&self) -> bool {
        self.path.as_os_str().is_empty()
    }
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

    fn build(
        self,
        name: String,
        path: PathBuf,
        statuses: &HashMap<PathBuf, ChangeKind>,
    ) -> FileNode {
        let mut children = Vec::new();
        let mut changed = 0;
        for (dname, dtmp) in self.dirs {
            let dpath = if path.as_os_str().is_empty() {
                PathBuf::from(&dname)
            } else {
                path.join(&dname)
            };
            let child = dtmp.build(dname, dpath, statuses);
            changed += child.changed;
            children.push(child);
        }
        for (fname, fpath) in self.files {
            let status = statuses.get(&fpath).copied();
            changed += usize::from(status.is_some());
            children.push(FileNode {
                name: fname,
                path: fpath,
                is_dir: false,
                children: Vec::new(),
                status,
                changed: 0,
            });
        }
        FileNode {
            name,
            path,
            is_dir: true,
            children,
            status: None,
            changed,
        }
    }
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
// ---------- what the explorer actually draws ----------

/// One line of the explorer, already flattened out of the tree.
///
/// The view renders a list of these rather than a component per node holding
/// its own subtree. A node carries its children, so handing one to a component
/// means cloning and then equality-checking everything underneath it on every
/// render — which is how a collapsed `target/` that nobody can even see comes
/// to cost more than the whole visible tree put together.
#[derive(Clone, PartialEq, Debug)]
pub struct Row {
    pub name: String,
    pub path: PathBuf,
    pub depth: usize,
    pub kind: RowKind,
}

#[derive(Clone, PartialEq, Debug)]
pub enum RowKind {
    Dir {
        open: bool,
        /// Changed files below it, for the count a closed folder shows.
        changed: usize,
    },
    File {
        status: Option<ChangeKind>,
        /// The directory holding it. Only set in a filtered list, where rows
        /// have been lifted out of the tree and `src/ui/mod.rs` and
        /// `src/backend/mod.rs` would otherwise both read as `mod.rs`.
        hint: Option<String>,
    },
}

/// Whether a directory is open, for a tree that has not been seeded yet.
///
/// [`seed_expansion`] normally settles this before anything is drawn; this is
/// the answer for the one frame in between, and it has to agree with the
/// seeding rule or the tree visibly rearranges itself on arrival.
fn open_by_default(node: &FileNode) -> bool {
    node.is_root() || node.has_changes()
}

/// Give every directory an entry in the expansion map, so that from here on
/// what is open is a record of what was clicked and nothing else.
///
/// This is the fix for a tree that unrolls on its own. Deriving "open" from
/// "holds a change" means every save re-answers the question for directories
/// nobody touched: write a file three levels down and the path to it unrolls
/// under the pointer, stage it and the same path snaps shut. Deciding once and
/// recording the answer keeps the tree where the reader put it.
///
/// `initial` — the first tree of a workspace — opens the root and everything
/// holding a change, which is where a review wants to start. Afterwards a
/// directory seen for the first time starts closed: a file saved into a folder
/// nobody has opened should raise that folder's count, not unroll it.
pub fn seed_expansion(node: &FileNode, initial: bool, out: &mut HashMap<PathBuf, bool>) {
    if !node.is_dir {
        return;
    }
    if !out.contains_key(&node.path) {
        out.insert(node.path.clone(), initial && open_by_default(node));
    }
    for child in &node.children {
        seed_expansion(child, initial, out);
    }
}

/// Flatten the tree into the rows on screen — every node down to the first
/// closed directory on each branch, and nothing beneath one.
pub fn visible_rows(root: &FileNode, expanded: &HashMap<PathBuf, bool>) -> Vec<Row> {
    let mut out = Vec::new();
    push_rows(root, 0, expanded, &mut out);
    out
}

fn push_rows(node: &FileNode, depth: usize, expanded: &HashMap<PathBuf, bool>, out: &mut Vec<Row>) {
    if !node.is_dir {
        out.push(Row {
            name: node.name.clone(),
            path: node.path.clone(),
            depth,
            kind: RowKind::File {
                status: node.status,
                hint: None,
            },
        });
        return;
    }
    let open = expanded
        .get(&node.path)
        .copied()
        .unwrap_or_else(|| open_by_default(node));
    out.push(Row {
        name: node.name.clone(),
        path: node.path.clone(),
        depth,
        kind: RowKind::Dir {
            open,
            changed: node.changed,
        },
    });
    if open {
        for child in &node.children {
            push_rows(child, depth + 1, expanded, out);
        }
    }
}

/// Every changed file, in the order the explorer draws them.
///
/// What "next changed file" steps through. Taken off the tree rather than off
/// the pull request's own list of changed files, so that stepping and the
/// explorer agree about what comes next: GitHub returns changed files in an
/// order of its own, and following it would send the highlight jumping about a
/// tree that is grouped by directory.
///
/// Whether a directory happens to be folded up makes no difference — a file is
/// no less changed for being out of sight, and a step that skipped it would be
/// a step that quietly left files out of the review.
pub fn changed_paths(root: &FileNode) -> Vec<PathBuf> {
    let mut out = Vec::new();
    push_changed(root, &mut out);
    out
}

fn push_changed(node: &FileNode, out: &mut Vec<PathBuf>) {
    if !node.is_dir {
        if node.status.is_some() {
            out.push(node.path.clone());
        }
        return;
    }
    // Nothing changed anywhere below: the count is there to be able to say so
    // without walking it.
    if !node.has_changes() {
        return;
    }
    for child in &node.children {
        push_changed(child, out);
    }
}

/// Files whose path contains `needle`, as a flat list.
///
/// Filtering answers "where is that file", and the folders on the way to it are
/// not part of the answer — so the hierarchy is dropped and each match carries
/// the directory it came from instead. Name matches come first: typing `mod`
/// is a search for a file called that, not for the eleven files that happen to
/// live under `models/`.
///
/// `limit` caps what is returned, and the count of everything found comes back
/// with it so the caller can say what it is not showing.
pub fn matching_rows(root: &FileNode, needle: &str, limit: usize) -> (Vec<Row>, usize) {
    let mut by_name = Vec::new();
    let mut by_path = Vec::new();
    collect_matches(root, needle, &mut by_name, &mut by_path);
    let total = by_name.len() + by_path.len();
    by_name.extend(by_path);
    by_name.truncate(limit);
    (by_name, total)
}

fn collect_matches(node: &FileNode, needle: &str, by_name: &mut Vec<Row>, by_path: &mut Vec<Row>) {
    if node.is_dir {
        for child in &node.children {
            collect_matches(child, needle, by_name, by_path);
        }
        return;
    }
    let name_hit = contains_ci(&node.name, needle);
    if !name_hit && !contains_ci(&node.path.to_string_lossy(), needle) {
        return;
    }
    let hint = node
        .path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_string_lossy().into_owned());
    let row = Row {
        name: node.name.clone(),
        path: node.path.clone(),
        depth: 0,
        kind: RowKind::File {
            status: node.status,
            hint,
        },
    };
    if name_hit { by_name } else { by_path }.push(row);
}

/// Substring search that ignores case, without lowercasing either side first.
///
/// Every keystroke in the filter box runs this over every path in the
/// repository; allocating a lowercase copy of each one is the difference
/// between a filter that keeps up with typing and one that does not. Case is
/// folded for ASCII only, which is what filenames are nearly always made of.
fn contains_ci(haystack: &str, needle: &str) -> bool {
    let (hay, ndl) = (haystack.as_bytes(), needle.as_bytes());
    if ndl.is_empty() {
        return true;
    }
    hay.len() >= ndl.len() && hay.windows(ndl.len()).any(|w| w.eq_ignore_ascii_case(ndl))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn statuses(pairs: &[(&str, ChangeKind)]) -> HashMap<PathBuf, ChangeKind> {
        pairs.iter().map(|(p, k)| (PathBuf::from(p), *k)).collect()
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
        assert_eq!(src.changed, 1, "parent dir counts what is under it");
    }

    fn rows_of(tree: &FileNode, expanded: &HashMap<PathBuf, bool>) -> Vec<String> {
        visible_rows(tree, expanded)
            .into_iter()
            .map(|r| r.path.display().to_string())
            .collect()
    }

    #[test]
    fn closed_directories_hide_what_is_under_them() {
        let paths = [Path::new("src/a.rs"), Path::new("src/deep/b.rs")];
        let tree = build_tree_from_paths("pr", paths, &statuses(&[]));

        let mut expanded = HashMap::new();
        seed_expansion(&tree, true, &mut expanded);
        // Nothing changed, so only the root opens.
        assert_eq!(rows_of(&tree, &expanded), vec!["", "src"]);

        expanded.insert(PathBuf::from("src"), true);
        assert_eq!(
            rows_of(&tree, &expanded),
            vec!["", "src", "src/deep", "src/a.rs"]
        );
    }

    #[test]
    fn a_file_appearing_later_does_not_reopen_the_tree() {
        let paths = [Path::new("src/deep/b.rs")];
        let tree = build_tree_from_paths("pr", paths, &statuses(&[]));
        let mut expanded = HashMap::new();
        seed_expansion(&tree, true, &mut expanded);
        assert!(!expanded[&PathBuf::from("src")], "nothing changed yet");

        // The same tree, now with a change in it — the state of a directory
        // already seen must not be rewritten under the reader.
        let changed = build_tree_from_paths(
            "pr",
            [Path::new("src/deep/b.rs")],
            &statuses(&[("src/deep/b.rs", ChangeKind::Modified)]),
        );
        seed_expansion(&changed, false, &mut expanded);
        assert!(!expanded[&PathBuf::from("src")], "stayed put");
        assert_eq!(rows_of(&changed, &expanded), vec!["", "src"]);
    }

    #[test]
    fn a_directory_first_seen_after_load_starts_closed() {
        let first = build_tree_from_paths("pr", [Path::new("a.rs")], &statuses(&[]));
        let mut expanded = HashMap::new();
        seed_expansion(&first, true, &mut expanded);

        let later = build_tree_from_paths(
            "pr",
            [Path::new("a.rs"), Path::new("new/x.rs")],
            &statuses(&[("new/x.rs", ChangeKind::Added)]),
        );
        seed_expansion(&later, false, &mut expanded);
        assert!(!expanded[&PathBuf::from("new")]);
        // It shows up, carrying its count — it just does not unroll.
        let rows = visible_rows(&later, &expanded);
        let dir = rows.iter().find(|r| r.name == "new").unwrap();
        assert_eq!(
            dir.kind,
            RowKind::Dir {
                open: false,
                changed: 1
            }
        );
    }

    #[test]
    fn the_initial_tree_opens_the_path_to_every_change() {
        let tree = build_tree_from_paths(
            "pr",
            [Path::new("src/deep/b.rs"), Path::new("docs/c.md")],
            &statuses(&[("src/deep/b.rs", ChangeKind::Modified)]),
        );
        let mut expanded = HashMap::new();
        seed_expansion(&tree, true, &mut expanded);
        assert_eq!(
            rows_of(&tree, &expanded),
            vec!["", "docs", "src", "src/deep", "src/deep/b.rs"]
        );
    }

    #[test]
    fn filtering_flattens_and_puts_name_matches_first() {
        let tree = build_tree_from_paths(
            "pr",
            [
                Path::new("mod/other.rs"),
                Path::new("src/ui/mod.rs"),
                Path::new("src/backend/mod.rs"),
            ],
            &statuses(&[]),
        );
        let (rows, total) = matching_rows(&tree, "MOD", 10);
        assert_eq!(total, 3);
        let paths: Vec<String> = rows.iter().map(|r| r.path.display().to_string()).collect();
        assert_eq!(
            paths,
            vec!["src/backend/mod.rs", "src/ui/mod.rs", "mod/other.rs"],
            "the two called mod.rs come before the one merely living in mod/"
        );
        // Lifted out of the tree, each row says where it came from.
        assert!(matches!(
            &rows[0].kind,
            RowKind::File { hint: Some(h), .. } if h == "src/backend"
        ));
    }

    #[test]
    fn filtering_reports_what_it_left_out() {
        let tree = build_tree_from_paths(
            "pr",
            [Path::new("a1.rs"), Path::new("a2.rs"), Path::new("a3.rs")],
            &statuses(&[]),
        );
        let (rows, total) = matching_rows(&tree, "a", 2);
        assert_eq!((rows.len(), total), (2, 3));
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
    fn changed_files_come_back_in_the_order_they_are_drawn() {
        let paths = [
            Path::new("README.md"),
            Path::new("src/a.rs"),
            Path::new("src/deep/b.rs"),
            Path::new("src/z.rs"),
        ];
        let st = statuses(&[
            ("README.md", ChangeKind::Modified),
            ("src/deep/b.rs", ChangeKind::Added),
            ("src/z.rs", ChangeKind::Modified),
        ]);
        let tree = build_tree_from_paths("pr", paths, &st);
        let stepped: Vec<String> = changed_paths(&tree)
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        // Directories before files at each level, which is how the rows are
        // laid out — and unchanged `src/a.rs` is not among them.
        assert_eq!(stepped, vec!["src/deep/b.rs", "src/z.rs", "README.md"]);
    }

    #[test]
    fn a_folded_directory_still_offers_its_changed_files() {
        let tree = build_tree_from_paths(
            "pr",
            [Path::new("deep/inside/x.rs")],
            &statuses(&[("deep/inside/x.rs", ChangeKind::Modified)]),
        );
        // Nothing is expanded here at all, and the file is still steppable.
        assert_eq!(
            changed_paths(&tree),
            vec![PathBuf::from("deep/inside/x.rs")]
        );
    }

    #[test]
    fn nothing_changed_is_nothing_to_step_through() {
        let tree = build_tree_from_paths("repo", [Path::new("a.rs")], &statuses(&[]));
        assert!(changed_paths(&tree).is_empty());
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
