//! The ticks against the files of a review that have been read.

use super::*;

impl St {
    /// What a changed file of what is open is remembered as, or `None` when it
    /// is not one — an unchanged file, or a repository being browsed. Ticking a
    /// box against those would be a note about nothing: a review is over the
    /// files that changed.
    pub fn viewed_key(&self, rel: &Path) -> Option<String> {
        // The status map first, because this is asked once per row of the
        // explorer and answering it is a hash lookup where the workspace would
        // be a scan of every changed file.
        if !self.statuses.peek().contains_key(rel) {
            return None;
        }
        match &*self.workspace.peek() {
            Workspace::Pr(pr) => Some(pr.blob_key(rel).into_owned()),
            Workspace::Commit(view) => Some(view.blob_key(rel).into_owned()),
            Workspace::Compare(view) => Some(view.blob_key(rel).into_owned()),
            _ => None,
        }
    }

    /// Whether this file has been marked read. A reactive read: ticking one box
    /// redraws its row in the explorer and the header above the code.
    pub fn is_viewed(&self, rel: &Path) -> bool {
        // Nothing ticked is the common case and it costs nothing to answer, but
        // subscribe first — a component that skipped the read would not redraw
        // when the first box was ticked.
        let marks = self.viewed.read();
        !marks.is_empty() && self.viewed_key(rel).is_some_and(|key| marks.contains(&key))
    }

    /// Tick or untick one file, and keep it.
    pub fn toggle_viewed(&self, rel: &Path) {
        let Some(key) = self.viewed_key(rel) else {
            return;
        };
        let mut marks = self.viewed;
        {
            let mut held = marks.write();
            // Removing tells us whether it was there, so this is one lookup
            // rather than a `contains` and then a branch.
            if !held.remove(&key) {
                held.insert(key);
            }
        }
        let held = self.workspace.peek();
        if let Some(key) = marks_key(&held) {
            viewed::save(&key, &marks.peek());
        }
    }

    /// How many of the changed files have been marked read — the explorer's
    /// progress readout.
    pub fn viewed_count(&self) -> usize {
        let marks = self.viewed.read();
        if marks.is_empty() {
            return 0;
        }
        match &*self.workspace.read() {
            Workspace::Pr(pr) => pr
                .files
                .iter()
                .filter(|f| marks.contains(pr.blob_key_of(f).as_ref()))
                .count(),
            Workspace::Commit(view) => view
                .files
                .iter()
                .filter(|f| marks.contains(view.blob_key_of(f).as_ref()))
                .count(),
            Workspace::Compare(view) => view
                .files
                .iter()
                .filter(|f| marks.contains(view.blob_key_of(f).as_ref()))
                .count(),
            _ => 0,
        }
    }
}
