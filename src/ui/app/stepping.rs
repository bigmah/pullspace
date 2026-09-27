//! Moving through the open file and the changed ones: next file, next change, next find hit.

use super::*;

impl St {
    /// Where the open file stands among the changed ones, when it is one of
    /// them. `None` while reading something the pull request does not touch —
    /// which is where a definition three files away lands you.
    pub fn file_at(&self) -> Option<usize> {
        let open = self.open.read();
        let here = open.as_deref()?;
        self.changed_files.read().iter().position(|p| p == here)
    }

    /// The next or previous changed file, in the order the explorer draws them.
    ///
    /// From somewhere that is not one of them — an unchanged file opened to
    /// read around the change — it enters the list at the end you are heading
    /// for rather than doing nothing.
    pub fn step_file(&self, forward: bool) {
        let files = self.changed_files.peek();
        let target = match (self.file_at(), forward) {
            (Some(at), true) => files.get(at + 1),
            (Some(0), false) => None,
            (Some(at), false) => files.get(at - 1),
            (None, true) => files.first(),
            (None, false) => files.last(),
        };
        let Some(next) = target.cloned() else { return };
        drop(files);
        self.open_file(next);
    }

    /// Put the reader on a line of the file already open, without disturbing
    /// anything else about the view.
    ///
    /// Not [`open_at`](Self::open_at), which is for arriving from somewhere
    /// else and lands in the source view: the reader is already here, and
    /// dropping them out of a split diff to show them a line of it would be
    /// answering a question they did not ask.
    pub fn jump_line(&self, line: usize) {
        if self.open.peek().is_none() {
            return;
        }
        let mut at = self.at_line;
        at.set(Some(line));
        // The viewer listens for the write rather than for what this holds —
        // see `St::scroll_to` — so asking twice for the same line scrolls
        // back to it, which is what pressing the key again means.
        let mut ps = self.scroll_to;
        ps.set(Some(line));
    }

    /// The next or previous run of changes in the open file.
    ///
    /// Round the end rather than stopping at it. A diff is read more than once
    /// and the last change is next to the first one in every way that matters.
    pub fn step_change(&self, forward: bool) {
        let lines = self.change_lines.peek();
        let here = *self.at_line.peek();
        let found = match (here, forward) {
            (Some(at), true) => lines.iter().find(|&&l| l > at).copied(),
            (Some(at), false) => lines.iter().rev().find(|&&l| l < at).copied(),
            (None, true) => lines.first().copied(),
            (None, false) => lines.last().copied(),
        };
        let target = found.or_else(|| {
            if forward {
                lines.first().copied()
            } else {
                lines.last().copied()
            }
        });
        drop(lines);
        if let Some(line) = target {
            self.jump_line(line);
        }
    }

    /// The next or previous line matching the find bar, and round the end the
    /// same way.
    pub fn step_find(&self, forward: bool) {
        let lines = self.find_lines.peek();
        let n = lines.len();
        if n == 0 {
            return;
        }
        let next = match *self.find_at.peek() {
            Some(i) if forward => (i + 1) % n,
            Some(i) => (i + n - 1) % n,
            // Nothing stood on yet. Start from where the reader is in the file
            // rather than from the top of it — a find is nearly always a
            // question about the part already on screen.
            None => {
                let here = self.at_line.peek().unwrap_or(0);
                if forward {
                    lines.iter().position(|&l| l > here).unwrap_or(0)
                } else {
                    lines.iter().rposition(|&l| l < here).unwrap_or(n - 1)
                }
            }
        };
        let line = lines[next];
        drop(lines);
        let mut at = self.find_at;
        at.set(Some(next));
        self.jump_line(line);
    }

    /// Put the find bar up, or take it down and forget what was in it.
    pub fn toggle_find(&self, showing: bool) {
        let mut open = self.find_open;
        open.set(showing);
        if !showing {
            let mut at = self.find_at;
            at.set(None);
        }
    }
}
