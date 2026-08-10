use similar::{ChangeTag, TextDiff};

#[derive(Clone, PartialEq)]
pub struct Seg {
    pub text: String,
    pub emph: bool, // word-level change emphasis
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Ctx,
    Add,
    Del,
}

#[derive(Clone, PartialEq)]
pub struct Line {
    pub kind: LineKind,
    pub old_no: Option<usize>,
    pub new_no: Option<usize>,
    pub segs: Vec<Seg>,
}

#[derive(Clone, PartialEq)]
pub struct Hunk {
    pub header: String,
    pub lines: Vec<Line>,
}

/// One row of a side-by-side view: removed line on the left, added on the right.
#[derive(Clone, PartialEq)]
pub struct Row {
    pub left: Option<Line>,
    pub right: Option<Line>,
}

pub fn diff_hunks(old: &str, new: &str) -> Vec<Hunk> {
    let diff = TextDiff::from_lines(old, new);
    let mut hunks = Vec::new();
    for group in diff.grouped_ops(3) {
        let (Some(first), Some(last)) = (group.first(), group.last()) else {
            continue;
        };
        let old_start = first.old_range().start;
        let new_start = first.new_range().start;
        let header = format!(
            "@@ -{},{} +{},{} @@",
            old_start + 1,
            last.old_range().end - old_start,
            new_start + 1,
            last.new_range().end - new_start,
        );
        let mut lines = Vec::new();
        for op in &group {
            for change in diff.iter_inline_changes(op) {
                let kind = match change.tag() {
                    ChangeTag::Equal => LineKind::Ctx,
                    ChangeTag::Insert => LineKind::Add,
                    ChangeTag::Delete => LineKind::Del,
                };
                let mut segs: Vec<Seg> = change
                    .iter_strings_lossy()
                    .map(|(emph, text)| Seg {
                        text: text.into_owned(),
                        emph,
                    })
                    .collect();
                if let Some(last_seg) = segs.last_mut() {
                    while last_seg.text.ends_with('\n') || last_seg.text.ends_with('\r') {
                        last_seg.text.pop();
                    }
                }
                lines.push(Line {
                    kind,
                    old_no: change.old_index().map(|i| i + 1),
                    new_no: change.new_index().map(|i| i + 1),
                    segs,
                });
            }
        }
        hunks.push(Hunk { header, lines });
    }
    hunks
}

/// Pair up runs of deletions and additions into side-by-side rows.
pub fn to_rows(hunk: &Hunk) -> Vec<Row> {
    let lines = &hunk.lines;
    let mut rows = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        match lines[i].kind {
            LineKind::Ctx => {
                rows.push(Row {
                    left: Some(lines[i].clone()),
                    right: Some(lines[i].clone()),
                });
                i += 1;
            }
            LineKind::Del => {
                let mut dels = Vec::new();
                while i < lines.len() && lines[i].kind == LineKind::Del {
                    dels.push(lines[i].clone());
                    i += 1;
                }
                let mut adds = Vec::new();
                while i < lines.len() && lines[i].kind == LineKind::Add {
                    adds.push(lines[i].clone());
                    i += 1;
                }
                for k in 0..dels.len().max(adds.len()) {
                    rows.push(Row {
                        left: dels.get(k).cloned(),
                        right: adds.get(k).cloned(),
                    });
                }
            }
            LineKind::Add => {
                rows.push(Row {
                    left: None,
                    right: Some(lines[i].clone()),
                });
                i += 1;
            }
        }
    }
    rows
}

pub struct DiffStats {
    pub added: usize,
    pub removed: usize,
}

pub fn stats(hunks: &[Hunk]) -> DiffStats {
    let mut added = 0;
    let mut removed = 0;
    for h in hunks {
        for l in &h.lines {
            match l.kind {
                LineKind::Add => added += 1,
                LineKind::Del => removed += 1,
                LineKind::Ctx => {}
            }
        }
    }
    DiffStats { added, removed }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hunks_and_rows_pair_changes() {
        let hunks = diff_hunks("a\nb\nc\n", "a\nX\nc\n");
        assert_eq!(hunks.len(), 1);
        let lines = &hunks[0].lines;
        let del: Vec<_> = lines.iter().filter(|l| l.kind == LineKind::Del).collect();
        let add: Vec<_> = lines.iter().filter(|l| l.kind == LineKind::Add).collect();
        assert_eq!(del.len(), 1);
        assert_eq!(add.len(), 1);
        assert_eq!(del[0].old_no, Some(2));
        assert_eq!(add[0].new_no, Some(2));

        let rows = to_rows(&hunks[0]);
        let paired = rows
            .iter()
            .find(|r| r.left.as_ref().is_some_and(|l| l.kind == LineKind::Del))
            .expect("paired row");
        assert!(paired.right.as_ref().is_some_and(|l| l.kind == LineKind::Add));
    }

    #[test]
    fn no_trailing_newlines_in_segs() {
        let hunks = diff_hunks("x\n", "y\n");
        for h in &hunks {
            for l in &h.lines {
                let text: String = l.segs.iter().map(|s| s.text.as_str()).collect();
                assert!(!text.ends_with('\n'));
            }
        }
    }

    #[test]
    fn stats_count_adds_and_removes() {
        let hunks = diff_hunks("a\n", "a\nb\nc\n");
        let s = stats(&hunks);
        assert_eq!(s.added, 2);
        assert_eq!(s.removed, 0);
    }
}
