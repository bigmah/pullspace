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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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

/// Compare two versions of a file.
///
/// `ignore_ws` is the review-noise switch: a block that was reindented is a
/// screenful of red and green saying nothing, and the one real change in the
/// file is somewhere underneath it. See [`fold_whitespace`] for exactly how
/// much it forgives, which is deliberately less than it could.
pub fn diff_file(old: &str, new: &str, ignore_ws: bool) -> FileDiff {
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
    if ignore_ws {
        fold_whitespace(&mut lines);
    }
    if !lines.iter().any(|l| l.kind != LineKind::Ctx) {
        return FileDiff::default();
    }
    let gaps = contract(&lines);
    FileDiff { lines, gaps }
}

/// One line's text, as the file has it.
fn text_of(l: &Line) -> String {
    l.segs.iter().map(|s| s.text.as_str()).collect()
}

/// Turn every block of removals-then-additions that says the same thing twice
/// back into unchanged lines.
///
/// All or nothing, per block, and only where the two sides line up one for
/// one. The partial case — five lines reindented and one of them also edited —
/// is left alone on purpose: collapsing the five would leave the sixth
/// stranded in the middle of a run with nothing around it to read it against,
/// and a diff that has quietly dropped context is worse than a noisy one. What
/// this is for is the whole-block reindent, which is the case that actually
/// buries a review.
fn fold_whitespace(lines: &mut Vec<Line>) {
    let mut out: Vec<Line> = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        if lines[i].kind != LineKind::Del {
            out.push(lines[i].clone());
            i += 1;
            continue;
        }
        let dels = i + lines[i..]
            .iter()
            .position(|l| l.kind != LineKind::Del)
            .unwrap_or(lines.len() - i);
        let adds = dels
            + lines[dels..]
                .iter()
                .position(|l| l.kind != LineKind::Add)
                .unwrap_or(lines.len() - dels);
        let (n, m) = (dels - i, adds - dels);
        let paired = n > 0
            && n == m
            && (0..n).all(|t| text_of(&lines[i + t]).trim() == text_of(&lines[dels + t]).trim());
        if !paired {
            // Copy the whole block through untouched and carry on past it, so
            // a block this does not fold is never looked at twice.
            out.extend_from_slice(&lines[i..adds.max(dels)]);
            i = adds.max(dels);
            continue;
        }
        for t in 0..n {
            // The added side is what the file now says, so it is the text and
            // the emphasis that survive; the removed side is only still here
            // for its numbering.
            let mut line = lines[dels + t].clone();
            line.kind = LineKind::Ctx;
            line.old_no = lines[i + t].old_no;
            for seg in &mut line.segs {
                seg.emph = false;
            }
            out.push(line);
        }
        i = adds;
    }
    *lines = out;
}

/// Where each run of changes begins, as a line of the new file — what "next
/// change" steps through.
///
/// A run that only removes lines has no new-side number of its own, so it
/// answers with the last unchanged line before it: the reader lands looking at
/// the gap where the code used to be, which is where the change is.
pub fn change_lines(diff: &FileDiff) -> Vec<usize> {
    let mut out = Vec::new();
    let mut last_ctx: Option<usize> = None;
    let mut i = 0;
    while i < diff.lines.len() {
        if diff.lines[i].kind == LineKind::Ctx {
            last_ctx = diff.lines[i].new_no.or(last_ctx);
            i += 1;
            continue;
        }
        let start = i;
        while i < diff.lines.len() && diff.lines[i].kind != LineKind::Ctx {
            i += 1;
        }
        let anchor = diff.lines[start..i]
            .iter()
            .find_map(|l| l.new_no)
            .or(last_ctx)
            .unwrap_or(1);
        // A removal and the addition replacing it are one change, not two —
        // and `similar` hands them over as two runs only when something
        // unchanged sits between them, which this cannot see twice.
        if out.last() != Some(&anchor) {
            out.push(anchor);
        }
    }
    out
}

/// One band on the strip beside the scrollbar: where a change is in the whole
/// laid-out file, as a fraction of it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Mark {
    /// 0.0 at the top of the view, 1.0 at the bottom.
    pub at: f32,
    pub len: f32,
    pub kind: LineKind,
}

/// What each row of the laid-out view is: a change, or not.
///
/// Rows, not lines — the two views count differently. Side by side, a removal
/// and the addition replacing it are one row; inline they are two. The ruler
/// stands beside a scrollbar and has to agree with what the scrollbar is
/// scrolling, so it counts what is actually drawn.
fn row_kinds(lines: &[Line], split: bool) -> Vec<Option<LineKind>> {
    if !split {
        return lines
            .iter()
            .map(|l| (l.kind != LineKind::Ctx).then_some(l.kind))
            .collect();
    }
    to_rows(lines)
        .into_iter()
        .map(|r| {
            let right = r.right.map(|l| l.kind).filter(|k| *k != LineKind::Ctx);
            let left = r.left.map(|l| l.kind).filter(|k| *k != LineKind::Ctx);
            // A row with both sides changed is a modification, which reads as
            // an addition: the new text is the one being reviewed.
            right.or(left)
        })
        .collect()
}

/// Where the changes are in the view as it currently stands — folded stretches
/// folded, opened ones opened.
///
/// Adjacent rows of the same kind come back as one band, because a hundred
/// separate one-pixel marks is a smear rather than a map.
pub fn overview(diff: &FileDiff, open: &HashMap<usize, Expansion>, split: bool) -> Vec<Mark> {
    let mut kinds: Vec<Option<LineKind>> = Vec::new();
    for block in blocks(diff, open) {
        match block {
            Block::Gap { .. } => kinds.push(None),
            Block::Lines { header, from, to } => {
                if header.is_some() {
                    kinds.push(None);
                }
                kinds.extend(row_kinds(&diff.lines[from..to], split));
            }
        }
    }
    let total = kinds.len();
    if total == 0 {
        return Vec::new();
    }
    let mut out: Vec<Mark> = Vec::new();
    let mut i = 0;
    while i < total {
        let Some(kind) = kinds[i] else {
            i += 1;
            continue;
        };
        let start = i;
        while i < total && kinds[i] == Some(kind) {
            i += 1;
        }
        out.push(Mark {
            at: start as f32 / total as f32,
            len: (i - start) as f32 / total as f32,
            kind,
        });
    }
    out
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
        let diff = diff_file("a\nb\nc\n", "a\nX\nc\n", false);
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
        let diff = diff_file("x\n", "y\n", false);
        for l in &diff.lines {
            let text: String = l.segs.iter().map(|s| s.text.as_str()).collect();
            assert!(!text.ends_with('\n'));
        }
    }

    #[test]
    fn stats_count_adds_and_removes() {
        let s = stats(&diff_file("a\n", "a\nb\nc\n", false));
        assert_eq!(s.added, 2);
        assert_eq!(s.removed, 0);
    }

    #[test]
    fn identical_files_have_nothing_to_show() {
        let diff = diff_file("a\nb\n", "a\nb\n", false);
        assert!(diff.is_empty());
        assert!(diff.gaps.is_empty());
        assert!(blocks(&diff, &HashMap::new()).is_empty());
    }

    /// A change in the middle of a long file leaves one gap above it and one
    /// below, each keeping `CONTEXT` lines on the side facing the change.
    #[test]
    fn long_unchanged_runs_are_contracted() {
        let (old, new) = pair(40, 20);
        let diff = diff_file(&old, &new, false);
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
        let diff = diff_file("a\nb\nc\n", "a\nX\nc\n", false);
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
        let diff = diff_file(&old, &new, false);
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
        let diff = diff_file(&old, &new, false);
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
        let diff = diff_file(&old, &new, false);
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
        let diff = diff_file(&old, &new, false);
        let bs = blocks(&diff, &HashMap::new());
        let Block::Lines { header, .. } = &bs[1] else {
            panic!("a stretch of lines follows the first gap");
        };
        assert_eq!(header.as_deref(), Some("@@ -17,7 +17,7 @@"));
    }

    #[test]
    fn a_reindented_block_is_noise_the_whitespace_switch_removes() {
        let old = "fn a() {\nlet x = 1;\nlet y = 2;\n}\n";
        let new = "fn a() {\n    let x = 1;\n    let y = 2;\n}\n";
        assert!(!diff_file(old, new, false).is_empty(), "it is a change");
        assert!(
            diff_file(old, new, true).is_empty(),
            "and it is the only one"
        );
    }

    #[test]
    fn the_indentation_the_file_now_has_is_the_one_shown() {
        let old = "a\nb\nc\n";
        let new = "a\n    b\nc\n";
        // Only one line moved, so nothing else in the file changed either —
        // fold it and the whole diff goes away. Give it a real change too, so
        // there is something left to look at.
        let old = format!("{old}z\n");
        let new = format!("{new}zz\n");
        let d = diff_file(&old, &new, true);
        let moved = d
            .lines
            .iter()
            .find(|l| text_of(l).trim() == "b")
            .expect("the reindented line is still in the diff");
        assert_eq!(moved.kind, LineKind::Ctx);
        assert_eq!(text_of(moved), "    b");
        assert_eq!(moved.old_no, Some(2), "and still knows where it came from");
        assert_eq!(moved.new_no, Some(2));
    }

    #[test]
    fn a_block_with_one_real_edit_in_it_is_left_alone() {
        let old = "a\nb\nc\n";
        let new = "    a\n    B\n    c\n";
        let d = diff_file(old, new, true);
        assert_eq!(
            d.lines.iter().filter(|l| l.kind == LineKind::Add).count(),
            3,
            "all three lines stay in the diff, not just the edited one"
        );
    }

    #[test]
    fn folding_whitespace_needs_the_two_sides_to_line_up() {
        // Two lines out, three in: nothing to pair, so nothing folds.
        let old = "a\nb\nz\n";
        let new = "  a\n  b\n  extra\nz\n";
        let d = diff_file(old, new, true);
        assert!(!d.is_empty());
    }

    #[test]
    fn every_run_of_changes_gives_next_change_somewhere_to_go() {
        let (old, new) = pair(60, 30);
        let d = diff_file(&old, &new, false);
        assert_eq!(change_lines(&d), vec![30]);
    }

    #[test]
    fn a_pure_removal_is_anchored_to_the_line_above_it() {
        let old = "a\nb\nc\nd\n";
        let new = "a\nd\n";
        let d = diff_file(old, new, false);
        // `b` and `c` are gone; the place to stand is on `a`, the last line
        // still there before the hole.
        assert_eq!(change_lines(&d), vec![1]);
    }

    #[test]
    fn two_changes_far_apart_are_two_stops() {
        let mut old = String::new();
        let mut new = String::new();
        for i in 1..=80 {
            old.push_str(&format!("line {i}\n"));
            new.push_str(&match i {
                10 => "changed ten\n".to_string(),
                70 => "changed seventy\n".to_string(),
                _ => format!("line {i}\n"),
            });
        }
        assert_eq!(change_lines(&diff_file(&old, &new, false)), vec![10, 70]);
    }

    #[test]
    fn a_file_with_nothing_changed_has_nowhere_to_step() {
        assert!(change_lines(&FileDiff::default()).is_empty());
    }

    #[test]
    fn the_ruler_puts_a_band_where_the_change_is() {
        let (old, new) = pair(100, 50);
        let d = diff_file(&old, &new, false);
        let marks = overview(&d, &HashMap::new(), false);
        assert!(!marks.is_empty());
        for m in &marks {
            assert!((0.0..=1.0).contains(&m.at), "{m:?}");
            assert!(m.len > 0.0 && m.at + m.len <= 1.0001, "{m:?}");
        }
    }

    #[test]
    fn the_ruler_counts_rows_and_not_lines_so_split_and_inline_differ() {
        let old = "a\nb\nc\n";
        let new = "a\nB\nc\n";
        let d = diff_file(old, new, false);
        let inline = overview(&d, &HashMap::new(), false);
        let split = overview(&d, &HashMap::new(), true);
        // Inline draws the removal and the addition one under the other;
        // side by side they share a row, so the band is a bigger share of a
        // shorter view.
        assert!(split[0].len > inline[0].len, "{split:?} vs {inline:?}");
    }

    #[test]
    fn opening_a_folded_stretch_moves_the_bands_down_the_ruler() {
        let (old, new) = pair(200, 150);
        let d = diff_file(&old, &new, false);
        let shut = overview(&d, &HashMap::new(), false);
        let mut open = HashMap::new();
        open.insert(
            0,
            Expansion {
                top: 0,
                bottom: usize::MAX / 2,
            },
        );
        let opened = overview(&d, &open, false);
        assert!(
            opened[0].at > shut[0].at,
            "the change is further down a longer view: {} vs {}",
            opened[0].at,
            shut[0].at
        );
    }

    #[test]
    fn nothing_changed_leaves_the_ruler_blank() {
        assert!(overview(&FileDiff::default(), &HashMap::new(), false).is_empty());
    }
}
