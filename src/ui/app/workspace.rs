//! Opening, closing and switching what is open: a pull request, a repository, a commit or a compare.

use super::*;

impl St {
    /// Clear everything tied to whatever was open: the file, the scroll, the
    /// trail through it, and the tree's expansion state.
    pub(super) fn clear_view(&self) {
        let mut open = self.open;
        open.set(None);
        self.stop_reading();
        self.close_summary();
        let mut ps = self.scroll_to;
        ps.set(None);
        self.reset_tree_folds();
        // Narrowing the explorer was about the files that were in it. Left
        // behind, it hides most of whatever is being opened instead.
        let mut tree_filter = self.tree_filter;
        tree_filter.set(String::new());
        self.clear_ide();
        let mut trail = self.trail;
        trail.set(Vec::new());
        let mut ahead = self.ahead;
        ahead.set(Vec::new());
        // And the strip, every file of which belongs to what is being closed.
        let mut tabs = self.tabs;
        tabs.set(Vec::new());
        tabs::forget_all();
        // Including the ones put down: reopening a file from the review before
        // last is not something the key should be able to do.
        let mut closed = self.closed;
        closed.set(Vec::new());
        self.toggle_find(false);
        let mut find_text = self.find_text;
        find_text.set(String::new());
    }

    /// Put down the results of reading the last thing.
    ///
    /// Every one of these is about files that are no longer on screen: hits in
    /// them, definitions of them, an identifier picked out of one. The search
    /// text itself stays — searching two pull requests for the same thing is a
    /// reasonable morning.
    pub(super) fn clear_ide(&self) {
        let mut panel = self.panel;
        panel.set(Panel::Hidden);
        let mut sel = self.selected;
        sel.set(None);
        let mut index = self.index;
        index.set(Index::Off);
        let mut err = self.search_error;
        err.set(None);
    }

    /// Forget which directories were open, so the next tree drawn gets the
    /// arrival view rather than the last one's folds.
    ///
    /// For a different tree, not a changed one: a file appearing or a status
    /// moving must leave the explorer exactly where the reader put it.
    pub fn reset_tree_folds(&self) {
        let mut expanded = self.expanded;
        expanded.set(HashMap::new());
        let mut seeded = self.tree_seeded;
        seeded.set(false);
    }

    /// Shut every directory in the explorer.
    ///
    /// Every one but the root, which is the tree itself: closing that would
    /// leave a panel with one row in it and no way back that is not this same
    /// button.
    pub fn collapse_tree(&self) {
        let mut expanded = self.expanded;
        let mut held = expanded.write();
        for (path, open) in held.iter_mut() {
            if !path.as_os_str().is_empty() {
                *open = false;
            }
        }
    }

    /// Show a pull request. `statuses` becomes the PR's change list, so the
    /// tree, badges and viewer need no special casing.
    pub fn enter_pr(&self, pr: PrDetail) {
        let reload = self
            .workspace
            .peek()
            .pr()
            .is_some_and(|open| open.repo == pr.repo && open.number == pr.number);
        self.enter(Workspace::Pr(Box::new(pr)), reload);
    }

    /// Show a repository on its own — no pull request, so no changed files and
    /// no diffs, just the code at `view.head_sha`.
    ///
    /// It opens on the README, drawn rather than as source. A repository with
    /// no pull request in it is being opened to be read, and the front page is
    /// what it is written to be read from — an empty pane with a button in it
    /// is not the answer to "show me this repository".
    ///
    /// Switching branch is the exception, and is why `was_reading` exists: the
    /// question there is what this file looks like over there, so the file
    /// being read follows the switch wherever the other branch still has it.
    pub fn enter_repo(&self, view: RepoView) {
        let (reload, same_repo) = {
            let held = self.workspace.peek();
            let open = held.repo();
            (
                open.is_some_and(|open| open.repo == view.repo && open.branch == view.branch),
                open.is_some_and(|open| open.repo == view.repo),
            )
        };
        let was_reading = self.open.peek().clone();
        let readme = markdown::readme_of(view.tree.paths());
        let linked = self.enter(Workspace::Repo(Box::new(view)), reload);
        // Not on a reload: that keeps whatever was being read, and coming back
        // to the README is a click on the tree away. Nor when the link that
        // brought us here named a file — that file is the front page it asked
        // for.
        if reload || linked {
            return;
        }
        // The same file on the other branch, when the other branch has it. A
        // branch that deleted it, or a different repository altogether, falls
        // through to the front page.
        let carried = same_repo
            .then_some(was_reading)
            .flatten()
            .filter(|path| self.has_file(path));
        if let Some(path) = carried.or(readme) {
            self.open_file(path);
        }
    }

    /// Show two refs held up against each other.
    ///
    /// The commits between them come with the comparison, so the pane beside it
    /// is filled in here rather than left to fetch what is already in hand —
    /// after [`enter`](Self::enter), which is what clears the last list out.
    ///
    /// Swapping the two sides is the same pair asked the other way round, so
    /// the file being read follows the swap, as it follows a branch switch —
    /// the question is what this file looks like from the other side.
    pub fn enter_compare(&self, view: CompareView) {
        let (reload, swapped) = {
            let held = self.workspace.peek();
            let open = held.compare().filter(|open| open.repo == view.repo);
            (
                open.is_some_and(|open| open.base == view.base && open.head == view.head),
                open.is_some_and(|open| open.base == view.head && open.head == view.base),
            )
        };
        let was_reading = self.open.peek().clone();
        let commits = view.commits.clone();
        let linked = self.enter(Workspace::Compare(Box::new(view)), reload);
        let mut held = self.commits;
        held.set(CommitList::Ready(Box::new(commits)));
        if swapped && !linked {
            let carried = was_reading.filter(|path| self.has_file(path));
            if let Some(path) = carried {
                self.open_file(path);
            }
        }
    }

    /// Show one commit, diffed against the commit before it.
    ///
    /// The pull request it came out of stays beside it — the conversation, the
    /// description and the list of commits are all facts about the pull
    /// request, and reading one of its commits does not change any of them.
    pub fn enter_commit(&self, view: CommitView) {
        let reload = self
            .workspace
            .peek()
            .commit()
            .is_some_and(|open| open.repo == view.repo && open.commit.sha == view.commit.sha);
        self.enter(Workspace::Commit(Box::new(view)), reload);
    }

    /// Swap in something fetched from GitHub.
    ///
    /// `reload` means the same thing is already open, so the file being read
    /// and the tree as it was expanded are kept — resetting the view out from
    /// under someone who asked for fresh data is not what they asked for. Only
    /// what describes the old commit is dropped.
    ///
    /// Answers whether a file named by the link that brought us here was
    /// opened, which is the one thing the caller may still have to decide about
    /// — see [`St::enter_repo`].
    pub(super) fn enter(&self, ws: Workspace, reload: bool) -> bool {
        if !reload {
            self.clear_view();
        } else {
            // The same pull request at a different commit. What was read about
            // the last one — the index above all — is not about this one.
            self.clear_ide();
        }
        // Contents are keyed by path, not commit, so they have to go either
        // way: this is where a reload picks up what was pushed.
        self.forget_contents();
        let mut cloning = self.cloning;
        cloning.set(None);
        // The conversation belongs to a pull request, not to whichever of its
        // commits is on screen — so stepping from a pull request into one of
        // its commits, or from one commit to the next, keeps it rather than
        // fetching it again.
        //
        // Dropped here rather than when the new one lands, so the pane never
        // shows the last pull request's comments under this one's title.
        //
        // Not on a reload, though: `⟳` means fetch all of it again, and the
        // conversation is part of what may have moved.
        let same_review = !reload
            && self
                .workspace
                .peek()
                .review_key()
                .is_some_and(|was| Some(was) == ws.review_key());
        if !same_review {
            let mut conv = self.conv;
            conv.set(Conversation::Loading);
        }
        // The commits are the same question for longer: stepping from a pull
        // request into one of its commits keeps that list, and so does stepping
        // from a branch into one of the commits on it — which is what makes
        // reading a branch a matter of clicking down the pane beside it.
        let same_commits = !reload
            && self
                .workspace
                .peek()
                .commits_key()
                .is_some_and(|was| Some(was) == ws.commits_key());
        if !same_commits {
            // Idle rather than empty: it is what has the pane go and fetch the
            // commits the moment somebody looks at the tab, and — on a reload —
            // what picks up whatever was just pushed. Before the workspace is
            // written, since that write is what the fetch hangs off.
            let mut commits = self.commits;
            commits.set(CommitList::Idle);
        }
        // And the branches outlive both, because they are a fact about the
        // repository — and every pull request, branch and commit of one is
        // inside it.
        let same_repo = !reload && {
            let held = self.workspace.peek();
            held.repo_ref()
                .is_some_and(|was| Some(was) == ws.repo_ref())
        };
        if !same_repo {
            let mut branches = self.branches;
            branches.set(BranchList::Idle);
        }
        // The checks are asked again more often than either, because they are
        // about the commit and not about the pull request: stepping from a
        // review into one of its commits keeps the conversation and drops
        // these, which is the difference between the two questions.
        let same_checks = !reload
            && self
                .workspace
                .peek()
                .checks_key()
                .is_some_and(|was| Some(was) == ws.checks_key());
        if !same_checks {
            let mut checks = self.checks;
            checks.set(CheckList::Idle);
            self.forget_annotations();
        }
        let mut statuses = self.statuses;
        statuses.set(ws.statuses());
        // What was ticked here last time. On a reload as well: a mark is about
        // a blob, so the ones that survived the push are still the answer, and
        // the ones that did not have quietly stopped matching anything.
        let mut marks = self.viewed;
        marks.set(
            marks_key(&ws)
                .map(|key| viewed::load(&key))
                .unwrap_or_default(),
        );
        let link = Route::to(ws.target());
        let mut w = self.workspace;
        w.set(ws);
        // Landed. Whatever was standing in for this while it was fetched has a
        // workspace to give way to now.
        self.arrived();
        // The address bar follows whatever is open, so every review has a link:
        // one to send to somebody, and one this tab comes back to on reload.
        //
        // After the workspace and not before it. Writing the bar raises a
        // `hashchange`, which is how Back reaches us — and the listener tells
        // one of those from the other by asking what is already open.
        route::show(&link);
        let mut gh = self.gh_open;
        gh.set(false);
        // And the board, which is a list of places to go: having got to one,
        // it has done what it was opened for. Here rather than on the click,
        // so it stays up — with the row still under the pointer — for as long
        // as the loading takes.
        let mut board = self.pr_board;
        board.set(false);
        self.bump_tick();
        // And now that there is a tree to look a path up in, whatever the link
        // named inside it.
        self.take_pending()
    }

    /// Open the file a link named, if it named one and it is still there.
    ///
    /// A path that has moved since the link was written is not an error worth
    /// showing: the pull request it points into is open, which is most of what
    /// was being asked for.
    pub(super) fn take_pending(&self) -> bool {
        let mut pending = self.pending;
        let Some(place) = pending.write().take() else {
            return false;
        };
        self.open_place(place)
    }

    /// Go where a link points, inside what is already open.
    pub fn open_place(&self, place: Place) -> bool {
        if !self.has_file(&place.path) {
            // Not a file: a link naming a directory, which github.com hands
            // out as readily as one naming a file — and which a browser
            // extension passing on the page it was called from will hand over
            // whatever the reader happened to be looking at. The explorer
            // opened down to it is all there is to show for one, since there
            // is nothing to put in the viewer.
            return self.reveal_dir(&place.path);
        }
        match place.line {
            Some(line) => self.open_at(place.path, line),
            None => self.open_file(place.path),
        }
        true
    }

    /// Whether a repo-relative path is a directory of what is on show —
    /// which [`has_file`](Self::has_file) answers no to, the tree being built
    /// from file paths alone.
    pub fn has_dir(&self, rel: &Path) -> bool {
        match &*self.workspace.peek() {
            Workspace::Empty => false,
            Workspace::Pr(pr) => pr.tree.has_dir(rel),
            Workspace::Repo(view) => view.tree.has_dir(rel),
            Workspace::Commit(view) => view.tree.has_dir(rel),
            Workspace::Compare(view) => view.tree.has_dir(rel),
        }
    }

    /// Open the explorer down to a directory, and say whether there was one.
    ///
    /// Writes the folds directly rather than going through the file being
    /// read, which is what the explorer's own reveal hangs off — there is no
    /// file here, and a directory is the whole of what was asked for.
    pub(super) fn reveal_dir(&self, rel: &Path) -> bool {
        if !self.has_dir(rel) {
            return false;
        }
        let mut expanded = self.expanded;
        let mut folds = expanded.write();
        let mut dir = Some(rel);
        while let Some(d) = dir.filter(|d| !d.as_os_str().is_empty()) {
            folds.insert(d.to_path_buf(), true);
            dir = d.parent();
        }
        true
    }

    /// Where the reader is, as a link — what is open, the file being read, and
    /// the line they picked out of it.
    ///
    /// Reactive: this is what the address bar is kept in step with, so it has
    /// to be re-answered when any of the three moves.
    pub fn route(&self) -> Route {
        Route {
            at: self.workspace.read().target(),
            place: self.open.read().clone().map(|path| Place {
                path,
                line: *self.at_line.read(),
            }),
        }
    }

    /// Pick a line out of the open file, or put it down again.
    ///
    /// It is not a jump — the reader is looking at the line already. All it
    /// changes is what the address bar says, which is what makes the link in it
    /// a link to *here*.
    pub fn mark_line(&self, line: usize) {
        if self.open.peek().is_none() {
            return;
        }
        let mut at = self.at_line;
        let now = *at.peek();
        at.set((now != Some(line)).then_some(line));
    }

    /// Say that this space is on its way somewhere, so that what stands in for
    /// it until it lands is the place it is going rather than the front page.
    ///
    /// Home is nowhere to be on the way to: it *is* the landing page, and
    /// saying so would have the app stand in for itself forever.
    pub fn arriving_at(&self, route: &Route) {
        let mut incoming = self.incoming;
        incoming.set((route.at != Target::Home).then(|| route.clone()));
    }

    /// And that it is not on its way any more — it landed, it failed, or what
    /// the address bar said turned out to name nothing.
    pub fn arrived(&self) {
        let mut incoming = self.incoming;
        if incoming.peek().is_some() {
            incoming.set(None);
        }
    }

    /// Close whatever is open, back to nothing — which puts the landing page
    /// up, since choosing something is the only thing left to do.
    pub fn close_workspace(&self) {
        let mut w = self.workspace;
        w.set(Workspace::Empty);
        // Nothing is coming, either: closing is the one way to ask for the
        // landing page on purpose.
        self.arrived();
        let mut commits = self.commits;
        commits.set(CommitList::Idle);
        let mut branches = self.branches;
        branches.set(BranchList::Idle);
        let mut checks = self.checks;
        checks.set(CheckList::Idle);
        self.forget_annotations();
        self.forget_contents();
        let mut cloning = self.cloning;
        cloning.set(None);
        let mut statuses = self.statuses;
        statuses.set(HashMap::new());
        let mut marks = self.viewed;
        marks.set(HashSet::new());
        self.clear_view();
        // Closing is a place to come back to as much as opening is: without
        // this the address bar would go on naming a pull request that is no
        // longer on screen, and reloading would open it again.
        route::show(&Route::home());
        // The overlay was for switching away from something open. With nothing
        // open the landing page is the picker, and it is up by virtue of the
        // workspace being empty.
        let mut gh = self.gh_open;
        gh.set(false);
        // The board hangs off the top bar, and the top bar has just gone.
        let mut board = self.pr_board;
        board.set(false);
        self.bump_tick();
    }

    /// Whether a repo-relative path is one of the files on show. What a link in
    /// a README is checked against before it is followed, so a stale one is
    /// left alone rather than opening an error where the document was.
    pub fn has_file(&self, rel: &Path) -> bool {
        match &*self.workspace.peek() {
            Workspace::Empty => false,
            // A pull request whose tree could not be read still knows the files
            // it changes.
            Workspace::Pr(pr) => {
                pr.tree.entry(rel).is_some() || pr.files.iter().any(|f| f.path == rel)
            }
            Workspace::Repo(view) => view.tree.entry(rel).is_some(),
            Workspace::Commit(view) => {
                view.tree.entry(rel).is_some() || view.files.iter().any(|f| f.path == rel)
            }
            Workspace::Compare(view) => {
                view.tree.entry(rel).is_some() || view.files.iter().any(|f| f.path == rel)
            }
        }
    }

    /// Forget what the checks marked up. Keyed by check run id, and a new
    /// commit's checks are new runs — so what is held here is about nothing
    /// once the checks it belongs to have gone.
    pub fn forget_annotations(&self) {
        let mut annots = self.annots;
        if !annots.peek().is_empty() {
            annots.set(HashMap::new());
        }
    }

    /// Drop every decoded file held in memory.
    ///
    /// Only the decoded copies: what was cloned stays on disk, so the files
    /// dropped here come back off the filesystem rather than off the network.
    pub(super) fn forget_contents(&self) {
        let mut cache = self.pr_files;
        cache.set(HashMap::new());
        let mut order = self.warm_order;
        order.set(VecDeque::new());
        // Pictures too, and for the same reason: the next commit's `logo.png`
        // is a different file that happens to have the same name.
        let mut images = self.images;
        images.set(HashMap::new());
        let mut image_order = self.image_order;
        image_order.set(VecDeque::new());
        // Which stretches of a diff were opened up is a fact about that diff,
        // and every diff here is about to be built again out of whatever
        // arrives. Held on to, the positions would refer to gaps in a
        // comparison that no longer exists.
        let mut expansions = self.expansions;
        expansions.set(HashMap::new());
    }
}
