//! Opening up and closing the contracted stretches of a diff.

use super::*;

impl St {
    /// Show more of one contracted stretch of the open file's diff.
    ///
    /// `from_top` takes the lines nearest the code above the gap, so the
    /// stretch grows downwards out of it; otherwise they come off the bottom
    /// and it grows up towards the change below. Overshooting the end of the
    /// gap is harmless — `difftool::blocks` will not show more than is there.
    pub fn expand_gap(&self, gap: usize, by: usize, from_top: bool) {
        let Some(rel) = self.open.peek().clone() else {
            return;
        };
        let mut expansions = self.expansions;
        let mut w = expansions.write();
        let e = w.entry(rel).or_default().entry(gap).or_default();
        if from_top {
            e.top += by;
        } else {
            e.bottom += by;
        }
    }

    /// Show all of one contracted stretch. Taken off the bottom, which is what
    /// puts the bar that folds it away again at the head of what it opened.
    pub fn expand_gap_fully(&self, gap: usize, len: usize) {
        let Some(rel) = self.open.peek().clone() else {
            return;
        };
        let mut expansions = self.expansions;
        expansions.write().entry(rel).or_default().insert(
            gap,
            Expansion {
                top: 0,
                bottom: len,
            },
        );
    }

    /// Fold one stretch back up.
    pub fn contract_gap(&self, gap: usize) {
        let Some(rel) = self.open.peek().clone() else {
            return;
        };
        let mut expansions = self.expansions;
        let mut w = expansions.write();
        let Some(file) = w.get_mut(&rel) else { return };
        file.remove(&gap);
        // Nothing open in it is the same as never having been touched, and is
        // what `has_expansions` — and so the Reset button — is asking.
        if file.is_empty() {
            w.remove(&rel);
        }
    }

    /// Put the open file's diff back the way it arrived: every stretch of it
    /// contracted again, in one click.
    pub fn contract_all_gaps(&self) {
        let Some(rel) = self.open.peek().clone() else {
            return;
        };
        let mut expansions = self.expansions;
        expansions.write().remove(&rel);
    }

    /// Which stretches of the open file are open, if any.
    pub fn open_gaps(&self) -> HashMap<usize, Expansion> {
        let Some(rel) = self.open.read().clone() else {
            return HashMap::new();
        };
        self.expansions
            .read()
            .get(&rel)
            .cloned()
            .unwrap_or_default()
    }
}
