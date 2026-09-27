//! Where the reader has been and what they have open: the tab strip, Back and Forward, and the documents that take over the middle pane.

use super::*;

impl St {
    /// Open a file from the tree: changed files land in split-diff view, and
    /// prose and pages land rendered — markdown is written to be read, a page
    /// to be looked at, and the source of either is one button away.
    ///
    /// All of which is about a file being opened for the first time. One that
    /// is already in the strip above the code opens as its tab has it, because
    /// that is where whoever asked for it left it.
    pub fn open_file(&self, rel: PathBuf) {
        // Before the tab is read back, not after: a file that is already open
        // is one whose tab this is what brings up to date.
        self.stow();
        if let Some(spot) = self.tabbed(&rel) {
            return self.go(spot);
        }
        let changed = self.statuses.peek().get(&rel).is_some();
        let mode = match () {
            _ if changed => ViewMode::Split,
            _ if markdown::is_markdown(&rel) || crate::ui::viewer::is_html(&rel) => {
                ViewMode::Preview
            }
            _ => ViewMode::Source,
        };
        self.go(Spot {
            path: rel,
            mode,
            line: None,
        });
    }

    /// Jump to a specific line — what a comment on the diff, a search hit and a
    /// definition all link to.
    pub fn open_at(&self, rel: PathBuf, line: usize) {
        self.stow();
        self.go(Spot {
            path: rel,
            mode: ViewMode::Source,
            line: Some(line),
        });
    }

    // ------------------------------------------------------------ the trail

    /// Where the reader is standing, as somewhere to come back to.
    pub(super) fn here(&self) -> Option<Spot> {
        Some(Spot {
            path: self.open.peek().clone()?,
            mode: *self.view_mode.peek(),
            line: *self.at_line.peek(),
        })
    }

    /// Note where we were, on the way to somewhere else.
    ///
    /// Only when the file changes. A jump within the file you are already
    /// reading leaves no mark, because a Back that lands on the same file at
    /// the top of it looks like a button that did nothing.
    pub(super) fn mark(&self, going_to: &Path) {
        let Some(here) = self.here().filter(|h| h.path != going_to) else {
            return;
        };
        let mut trail = self.trail;
        {
            let mut trail = trail.write();
            trail.push(here);
            if trail.len() > TRAIL {
                trail.remove(0);
            }
        }
        // Going somewhere new is what ends the branch you had gone back along.
        let mut ahead = self.ahead;
        if !ahead.peek().is_empty() {
            ahead.set(Vec::new());
        }
    }

    pub fn can_go_back(&self) -> bool {
        !self.trail.read().is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.ahead.read().is_empty()
    }

    pub fn go_back(&self) {
        let mut trail = self.trail;
        let Some(spot) = trail.write().pop() else {
            return;
        };
        if let Some(here) = self.here() {
            let mut ahead = self.ahead;
            ahead.write().push(here);
        }
        self.land(spot);
    }

    pub fn go_forward(&self) {
        let mut ahead = self.ahead;
        let Some(spot) = ahead.write().pop() else {
            return;
        };
        if let Some(here) = self.here() {
            let mut trail = self.trail;
            trail.write().push(here);
        }
        self.land(spot);
    }

    /// Show a spot as it was — and without marking the trail, since moving
    /// along it is not the same as leaving it.
    pub(super) fn land(&self, spot: Spot) {
        self.stow();
        self.install(spot);
    }

    // ------------------------------------------------------------- the tabs

    /// Where `rel` was left, when it is one of the files being held open.
    pub(super) fn tabbed(&self, rel: &Path) -> Option<Spot> {
        self.tabs
            .peek()
            .iter()
            .find(|t| t.at.path == rel)
            .map(|t| t.at.clone())
    }

    /// Note where the reader is standing in what is on screen, so that its tab
    /// comes back to that rather than to the top of the file.
    ///
    /// Done on the way out of a file rather than on every move inside one: the
    /// view and the line only change when something changes them, and both are
    /// read straight off the signals that hold them. Where the file is
    /// scrolled to is the exception, and is not kept here at all — see
    /// [`tabs`].
    ///
    /// Leaving a *space* is one more way out of a file, which is why this is
    /// reachable from [`spaces`].
    pub(in crate::ui) fn stow(&self) {
        let Some(here) = self.here() else { return };
        let mut tabs = self.tabs;
        let mut held = tabs.write();
        if let Some(tab) = held.iter_mut().find(|t| t.at.path == here.path) {
            tab.at = here;
        }
    }

    /// Put a file in the viewer, with a tab for it in the strip above.
    ///
    /// One tab per file, ever: opening something that is already open brings
    /// its tab forward rather than making a second one. New tabs go on the
    /// end, and once the strip is full the one nobody has been back to for
    /// longest is let go — never the file being opened, which is the one just
    /// used.
    ///
    /// Callers [`stow`](Self::stow) first. This does not, because the two that
    /// do not want it to — closing a tab, and coming back along the trail —
    /// have already dealt with the place being left.
    pub(super) fn install(&self, spot: Spot) {
        {
            let mut tabs = self.tabs;
            let mut held = tabs.write();
            let used = held.iter().map(|t| t.used).max().unwrap_or(0) + 1;
            match held.iter_mut().find(|t| t.at.path == spot.path) {
                Some(tab) => {
                    tab.at = spot.clone();
                    tab.used = used;
                }
                None => held.push(OpenTab {
                    at: spot.clone(),
                    used,
                }),
            }
            if held.len() > TABS
                && let Some(oldest) = (0..held.len()).min_by_key(|&i| held[i].used)
            {
                let gone = held.remove(oldest);
                tabs::forget(&gone.at.path);
            }
        }
        let mut vm = self.view_mode;
        vm.set(spot.mode);
        let mut at = self.at_line;
        at.set(spot.line);
        // A line is a jump, and the viewer is listening for the write rather
        // than for what the signal holds — see `St::scroll_to`. Without one
        // there is nowhere to jump to, and the file arrives back where it was
        // left instead.
        if spot.line.is_some() {
            let mut ps = self.scroll_to;
            ps.set(spot.line);
        }
        let mut open = self.open;
        open.set(Some(spot.path));
        // The middle pane shows one thing. Opening a file is asking for it to
        // be that file — and whatever was being read there is still in the
        // conversation, one click from being picked up again. A summary keeps
        // its tab for the same reason.
        self.stop_reading();
        self.hide_summary();
    }

    /// Somewhere new: the trail remembers where we were, and the strip gets a
    /// tab for where we are going.
    pub(super) fn go(&self, spot: Spot) {
        self.mark(&spot.path);
        self.install(spot);
    }

    /// The files in the strip, most recently looked at first.
    ///
    /// What the picker opens on: with nothing typed, the answer to "which
    /// file" is nearly always one of the few already in hand.
    pub fn recent_paths(&self) -> Vec<PathBuf> {
        let held = self.tabs.read();
        let mut order: Vec<&OpenTab> = held.iter().collect();
        order.sort_by_key(|t| std::cmp::Reverse(t.used));
        order.iter().map(|t| t.at.path.clone()).collect()
    }

    /// Pick the last file put down back up, where it was left.
    pub fn reopen_tab(&self) {
        let mut closed = self.closed;
        let Some(spot) = closed.write().pop() else {
            return;
        };
        self.stow();
        self.go(spot);
    }

    /// Close one tab, and hand the viewer to the file beside it — the one to
    /// its right, or, closing the last of them, the one to its left. Which is
    /// what makes closing something you have finished with land on the next
    /// thing rather than on nothing.
    ///
    /// Closing the only tab leaves the welcome page, because there is nothing
    /// else it could leave. Closing one that is not the one being read moves
    /// nothing at all.
    pub fn close_tab(&self, rel: &Path) {
        // Where the reader is standing, before the tab holding it is taken
        // away: a tab's place is only written on the way out of it, and
        // closing is a way out. Without this a file put down at line 400 is
        // one that reopens at line 1.
        self.stow();
        let mut tabs = self.tabs;
        let Some(at) = tabs.peek().iter().position(|t| t.at.path == rel) else {
            return;
        };
        let leaving = self.open.peek().as_deref() == Some(rel);
        let next = {
            let mut held = tabs.write();
            let gone = held.remove(at);
            // Where it was left, so picking it back up is picking up the file
            // and not the top of it.
            let mut closed = self.closed;
            {
                let mut stack = closed.write();
                stack.retain(|s| s.path != gone.at.path);
                stack.push(gone.at);
                // Long enough to undo a run of closes, short enough not to be
                // a second copy of the strip.
                if stack.len() > REOPEN {
                    stack.remove(0);
                }
            }
            // Whatever has slid into its place, or the end of the strip.
            held.get(at).or_else(|| held.last()).map(|t| t.at.clone())
        };
        tabs::forget(rel);
        if !leaving {
            return;
        }
        match next {
            Some(spot) => {
                // The trail can already name what we are landing on: it is
                // where the reader was before they opened the tab they have
                // just closed. Left there, the next Back would be a key that
                // did nothing.
                let mut trail = self.trail;
                if trail.peek().last().is_some_and(|s| s.path == spot.path) {
                    trail.write().pop();
                }
                self.install(spot);
            }
            None => {
                let mut open = self.open;
                open.set(None);
                let mut line = self.at_line;
                line.set(None);
            }
        }
    }

    /// Close the tab being read — what ⌥W is.
    ///
    /// A description in the pane has a tab of its own at the head of the
    /// strip, and it is the one marked as being read. So it is the one this
    /// closes, and the file underneath is what ⌥W reaches next.
    pub fn close_open_tab(&self) {
        if self.reading.peek().is_some() {
            return self.stop_reading();
        }
        if *self.summary_on.peek() {
            return self.close_summary();
        }
        let Some(rel) = self.open.peek().clone() else {
            return;
        };
        self.close_tab(&rel);
    }

    // ------------------------------------------------------- reading a body

    /// Hand the middle pane to a description or a comment.
    ///
    /// Nothing is closed to make room: the file that was there keeps its tab
    /// and its place in it, and clicking that tab is what comes back.
    pub fn read_doc(&self, doc: Reading) {
        self.stow();
        self.hide_summary();
        let mut reading = self.reading;
        reading.set(Some(doc));
    }

    /// Put it down, and let the file underneath have the pane back.
    pub fn stop_reading(&self) {
        let mut reading = self.reading;
        if reading.peek().is_some() {
            reading.set(None);
        }
    }

    // ---------------------------------------------------- reading a summary

    /// Hand the middle pane to a summary page — a directory's, or a pull
    /// request's guide to itself. See [`summary`](crate::ui::summary).
    pub fn open_summary(&self, page: PathBuf) {
        self.stow();
        self.stop_reading();
        let mut summary = self.summary;
        if summary.peek().as_ref() != Some(&page) {
            summary.set(Some(page));
        }
        let mut on = self.summary_on;
        if !*on.peek() {
            on.set(true);
        }
    }

    /// Bring the last summary back, from its tab.
    pub fn show_summary(&self) {
        if let Some(page) = self.summary.peek().clone() {
            self.open_summary(page);
        }
    }

    /// Let the file underneath have the pane, keeping the summary's tab.
    pub fn hide_summary(&self) {
        let mut on = self.summary_on;
        if *on.peek() {
            on.set(false);
        }
    }

    /// Put the summary down altogether, tab and all.
    pub fn close_summary(&self) {
        self.hide_summary();
        let mut summary = self.summary;
        if summary.peek().is_some() {
            summary.set(None);
        }
    }
}
