use std::collections::HashMap;

use similar::{ChangeTag, TextDiff};

/// Unchanged lines kept either side of every change — what makes a diff a diff
/// rather than the file printed twice.
pub const CONTEXT: usize = 3;

/// How much of a contracted stretch one click opens up. Enough to see what the
/// change is sitting in; short of dropping a thousand lines on the reader at
/// once, which is what the whole-stretch control is for.
pub const STEP: usize = 20;

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

/// A stretch of unchanged lines the contracted view holds back.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Gap {
    /// Where it starts, as an index into [`FileDiff::lines`].
    pub at: usize,
    pub len: usize,
}

/// A whole comparison: every line of it, and where it is folded up.
///
/// Keeping the unchanged lines is the point. A contracted stretch can be
/// opened up because what is in it is already here — expanding one is a
/// question about the view, not a second pass over the file.
#[derive(Clone, Default, PartialEq)]
pub struct FileDiff {
    pub lines: Vec<Line>,
    /// In order, and never overlapping.
    pub gaps: Vec<Gap>,
}

impl FileDiff {
    /// Nothing changed. Distinct from a file with no lines in it, which has
    /// nothing to say either — both come out of here empty.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

/// One row of a side-by-side view: removed line on the left, added on the right.
///
/// Borrowed rather than owned: the split view rebuilds its rows on every
/// render, and a row that carried copies of its lines would be copying every
/// segment of the visible diff each time.
#[derive(Clone, Copy, PartialEq)]
pub struct Row<'a> {
    pub left: Option<&'a Line>,
    pub right: Option<&'a Line>,
}

pub fn diff_file(old: &str, new: &str) -> FileDiff {
    let diff = TextDiff::from_lines(old, new);
    let mut lines = Vec::new();
    // Every op, not `grouped_ops(CONTEXT)`: the unchanged runs between the
    // changes are what the reader may ask to see. Equal ops skip the
    // word-level pass inside `similar`, so carrying them costs the lines
    // themselves and nothing more.
    for op in diff.ops() {
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
    if !lines.iter().any(|l| l.kind != LineKind::Ctx) {
        return FileDiff::default();
    }
    let gaps = contract(&lines);
    FileDiff { lines, gaps }
}

/// Where the contracted view folds: the middle of every unchanged run long
/// enough to have one, with `CONTEXT` lines left showing on whichever side has
/// a change to stand off from.
fn contract(lines: &[Line]) -> Vec<Gap> {
    let mut gaps = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].kind != LineKind::Ctx {
            i += 1;
            continue;
        }
        let start = i;
        while i < lines.len() && lines[i].kind == LineKind::Ctx {
            i += 1;
        }
        // The top and bottom of the file are not changes, and nothing needs to
        // be held clear of them.
        let head = if start == 0 { 0 } else { CONTEXT };
        let tail = if i == lines.len() { 0 } else { CONTEXT };
        if i - start > head + tail {
            gaps.push(Gap {
                at: start + head,
                len: i - start - head - tail,
            });
        }
    }
    gaps
}

/// How much of one gap the reader has opened up: lines taken from its top
/// edge, and lines taken from its bottom. Both zero — the default — is the
/// stretch as it arrives, contracted.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Expansion {
    pub top: usize,
    pub bottom: usize,
}

/// One piece of the view: lines, or the bar standing in for lines.
#[derive(Clone, PartialEq)]
pub enum Block {
    Lines {
        /// The `@@ … @@` line, as a range over [`FileDiff::lines`].
        ///
        /// Only a stretch following something still hidden gets one. Once a
        /// gap is fully open the code runs straight on through it, and a
        /// header in the middle would be announcing a break that is no longer
        /// there.
        header: Option<String>,
        from: usize,
        to: usize,
    },
    /// `index` is the gap's position in [`FileDiff::gaps`] — what an expansion
    /// is recorded against. `hidden` is what is still folded away, `shown`
    /// what has been opened up out of it.
    Gap {
        index: usize,
        hidden: usize,
        shown: usize,
    },
}

/// Lay the diff out under the expansions asked for so far.
pub fn blocks(diff: &FileDiff, open: &HashMap<usize, Expansion>) -> Vec<Block> {
    let mut out = Vec::new();
    let mut cursor = 0;
    let mut header_due = true;
    for (index, gap) in diff.gaps.iter().enumerate() {
        let e = open.get(&index).copied().unwrap_or_default();
        // Clamped here rather than at the click, so a control that is pressed
        // twice quickly cannot run past the end of what it is opening.
        let mut top = e.top.min(gap.len);
        let mut bottom = e.bottom.min(gap.len - top);
        // Open all the way, the bar is no longer standing between two
        // stretches — it is the handle on one, and belongs at the head of it.
        if top + bottom == gap.len {
            top = 0;
            bottom = gap.len;
        }
        let hidden = gap.len - top - bottom;
        let upto = gap.at + top;
        if upto > cursor {
            out.push(Block::Lines {
                header: header_due.then(|| header(&diff.lines[cursor..upto])),
                from: cursor,
                to: upto,
            });
            header_due = hidden > 0;
        } else {
            // A gap at the very top of the file, still shut: no lines came
            // before it, so the header it owes is owed by whatever comes next.
            header_due = header_due || hidden > 0;
        }
        out.push(Block::Gap {
            index,
            hidden,
            shown: top + bottom,
        });
        cursor = gap.at + gap.len - bottom;
    }
    if cursor < diff.lines.len() {
        out.push(Block::Lines {
            header: header_due.then(|| header(&diff.lines[cursor..])),
            from: cursor,
            to: diff.lines.len(),
        });
    }
    out
}

/// Where one side of a stretch starts, and how many of its lines are in it.
fn side(lines: &[Line], pick: fn(&Line) -> Option<usize>) -> (usize, usize) {
    let mut nos = lines.iter().filter_map(pick);
    match nos.next() {
        Some(first) => (first, 1 + nos.count()),
        None => (0, 0),
    }
}

fn header(lines: &[Line]) -> String {
    let (old, old_n) = side(lines, |l| l.old_no);
    let (new, new_n) = side(lines, |l| l.new_no);
    format!("@@ -{old},{old_n} +{new},{new_n} @@")
}

/// Pair up runs of deletions and additions into side-by-side rows.
pub fn to_rows(lines: &[Line]) -> Vec<Row<'_>> {
    let mut rows = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        match lines[i].kind {
            LineKind::Ctx => {
                rows.push(Row {
                    left: Some(&lines[i]),
                    right: Some(&lines[i]),
                });
                i += 1;
            }
            LineKind::Del => {
                let dels_from = i;
                while i < lines.len() && lines[i].kind == LineKind::Del {
                    i += 1;
                }
                let adds_from = i;
                while i < lines.len() && lines[i].kind == LineKind::Add {
                    i += 1;
                }
                let dels = &lines[dels_from..adds_from];
                let adds = &lines[adds_from..i];
                for k in 0..dels.len().max(adds.len()) {
                    rows.push(Row {
                        left: dels.get(k),
                        right: adds.get(k),
                    });
                }
            }
            LineKind::Add => {
                rows.push(Row {
                    left: None,
                    right: Some(&lines[i]),
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

pub fn stats(diff: &FileDiff) -> DiffStats {
    let mut added = 0;
    let mut removed = 0;
    for l in &diff.lines {
        match l.kind {
            LineKind::Add => added += 1,
            LineKind::Del => removed += 1,
            LineKind::Ctx => {}
        }
    }
    DiffStats { added, removed }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` numbered lines, with `changed` rewritten — a file long enough to
    /// have something contracted out of the middle of it.
    fn pair(n: usize, changed: usize) -> (String, String) {
        let old: String = (1..=n).map(|i| format!("line {i}\n")).collect();
        let new: String = (1..=n)
            .map(|i| {
                if i == changed {
                    format!("LINE {i} changed\n")
                } else {
                    format!("line {i}\n")
                }
            })
            .collect();
        (old, new)
    }

    fn shown(diff: &FileDiff, open: &HashMap<usize, Expansion>) -> usize {
        blocks(diff, open)
            .iter()
            .map(|b| match b {
                Block::Lines { from, to, .. } => to - from,
                Block::Gap { .. } => 0,
            })
            .sum()
    }

    #[test]
    fn rows_pair_changes() {
        let diff = diff_file("a\nb\nc\n", "a\nX\nc\n");
        let del: Vec<_> = diff
            .lines
            .iter()
            .filter(|l| l.kind == LineKind::Del)
            .collect();
        let add: Vec<_> = diff
            .lines
            .iter()
            .filter(|l| l.kind == LineKind::Add)
            .collect();
        assert_eq!(del.len(), 1);
        assert_eq!(add.len(), 1);
        assert_eq!(del[0].old_no, Some(2));
        assert_eq!(add[0].new_no, Some(2));

        let rows = to_rows(&diff.lines);
        let paired = rows
            .iter()
            .find(|r| r.left.as_ref().is_some_and(|l| l.kind == LineKind::Del))
            .expect("paired row");
        assert!(
            paired
                .right
                .as_ref()
                .is_some_and(|l| l.kind == LineKind::Add)
        );
    }

    #[test]
    fn no_trailing_newlines_in_segs() {
        let diff = diff_file("x\n", "y\n");
        for l in &diff.lines {
            let text: String = l.segs.iter().map(|s| s.text.as_str()).collect();
            assert!(!text.ends_with('\n'));
        }
    }

    #[test]
    fn stats_count_adds_and_removes() {
        let s = stats(&diff_file("a\n", "a\nb\nc\n"));
        assert_eq!(s.added, 2);
        assert_eq!(s.removed, 0);
    }

    #[test]
    fn identical_files_have_nothing_to_show() {
        let diff = diff_file("a\nb\n", "a\nb\n");
        assert!(diff.is_empty());
        assert!(diff.gaps.is_empty());
        assert!(blocks(&diff, &HashMap::new()).is_empty());
    }

    /// A change in the middle of a long file leaves one gap above it and one
    /// below, each keeping `CONTEXT` lines on the side facing the change.
    #[test]
    fn long_unchanged_runs_are_contracted() {
        let (old, new) = pair(40, 20);
        let diff = diff_file(&old, &new);
        assert_eq!(diff.gaps.len(), 2);
        // Lines 1..=16 of the old side sit above the kept context; the diff
        // carries one extra line for the change itself, hence the offsets.
        assert_eq!(diff.gaps[0].at, 0);
        assert_eq!(diff.gaps[0].len, 16);
        assert_eq!(diff.gaps[1].len, 17);
        // Kept: three lines above, the removal and the addition, three below.
        assert_eq!(shown(&diff, &HashMap::new()), 2 * CONTEXT + 2);
    }

    /// A short file has nothing to contract, and nothing between the changes
    /// to break the view into more than one stretch.
    #[test]
    fn short_files_are_left_whole() {
        let diff = diff_file("a\nb\nc\n", "a\nX\nc\n");
        assert!(diff.gaps.is_empty());
        let bs = blocks(&diff, &HashMap::new());
        assert_eq!(bs.len(), 1);
        assert!(matches!(
            bs[0],
            Block::Lines {
                header: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn expanding_one_end_shrinks_the_gap() {
        let (old, new) = pair(40, 20);
        let diff = diff_file(&old, &new);
        let base = shown(&diff, &HashMap::new());

        let mut open = HashMap::new();
        open.insert(0, Expansion { top: 5, bottom: 0 });
        assert_eq!(shown(&diff, &open), base + 5);
        let hidden: Vec<_> = blocks(&diff, &open)
            .into_iter()
            .filter_map(|b| match b {
                Block::Gap { hidden, shown, .. } => Some((hidden, shown)),
                _ => None,
            })
            .collect();
        assert_eq!(hidden[0], (11, 5));
        // The gap nobody touched is untouched.
        assert_eq!(hidden[1], (17, 0));
    }

    /// Opened all the way, a gap stops being a break: the lines either side of
    /// it join into one stretch, and the header that announced the break goes.
    #[test]
    fn a_fully_expanded_gap_leaves_no_header() {
        let (old, new) = pair(40, 20);
        let diff = diff_file(&old, &new);
        let mut open = HashMap::new();
        for (i, g) in diff.gaps.iter().enumerate() {
            open.insert(
                i,
                Expansion {
                    top: 0,
                    bottom: g.len,
                },
            );
        }
        assert_eq!(shown(&diff, &open), diff.lines.len());
        let headers = blocks(&diff, &open)
            .iter()
            .filter(|b| {
                matches!(
                    b,
                    Block::Lines {
                        header: Some(_),
                        ..
                    }
                )
            })
            .count();
        assert_eq!(headers, 1);
        // The bars stay: they are what folds the stretch back up.
        let bars = blocks(&diff, &open)
            .iter()
            .filter(|b| matches!(b, Block::Gap { hidden: 0, .. }))
            .count();
        assert_eq!(bars, 2);
    }

    /// Expanding past the end of a gap opens it and stops there.
    #[test]
    fn expansion_cannot_overrun_its_gap() {
        let (old, new) = pair(40, 20);
        let diff = diff_file(&old, &new);
        let mut open = HashMap::new();
        open.insert(
            0,
            Expansion {
                top: 900,
                bottom: 900,
            },
        );
        open.insert(
            1,
            Expansion {
                top: 900,
                bottom: 900,
            },
        );
        assert_eq!(shown(&diff, &open), diff.lines.len());
    }

    /// Line numbers, not indices: a header names the lines the reader can see
    /// in the gutter beside it.
    #[test]
    fn headers_count_the_lines_of_their_own_stretch() {
        let (old, new) = pair(40, 20);
        let diff = diff_file(&old, &new);
        let bs = blocks(&diff, &HashMap::new());
        let Block::Lines { header, .. } = &bs[1] else {
            panic!("a stretch of lines follows the first gap");
        };
        assert_eq!(header.as_deref(), Some("@@ -17,7 +17,7 @@"));
    }
}
