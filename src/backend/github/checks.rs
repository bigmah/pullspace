use super::*;

// ------------------------------------------------------------------- checks

/// How something that ran against a commit went, or that it has not gone
/// anywhere yet.
///
/// GitHub keeps two lists and spells the outcome differently in each: a check
/// run has a `status` and, once it is finished, a `conclusion`; a commit status
/// has one `state`. Both come down to these four, which are the four the panel
/// draws — the exact word GitHub used is kept alongside in [`Check::label`], so
/// nothing is flattened away, only coloured.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum CheckState {
    /// Queued, or running now.
    Running,
    Passed,
    Failed,
    /// Finished without a verdict: skipped, cancelled, stale, or deliberately
    /// neutral. Not a failure — and not something to count as a pass either.
    Quiet,
}

impl CheckState {
    /// What it is at a glance. A tick and a cross, because that is what
    /// everybody already reads them as.
    pub fn glyph(self) -> &'static str {
        match self {
            CheckState::Running => "●",
            CheckState::Passed => "✓",
            CheckState::Failed => "✕",
            CheckState::Quiet => "○",
        }
    }

    /// The stylesheet's name for the colour that goes with it.
    pub fn tone(self) -> &'static str {
        match self {
            CheckState::Running => "run",
            CheckState::Passed => "ok",
            CheckState::Failed => "bad",
            CheckState::Quiet => "off",
        }
    }

    /// Where it belongs in a list: what is broken first, then what is still
    /// going, then everything that needs no attention.
    fn rank(self) -> u8 {
        match self {
            CheckState::Failed => 0,
            CheckState::Running => 1,
            CheckState::Passed => 2,
            CheckState::Quiet => 3,
        }
    }
}

/// One thing that ran against a commit: a check run, or one of the older commit
/// statuses a CI service posts.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Check {
    /// What to ask GitHub for this one's annotations by. Zero for a commit
    /// status, which is not a check run and has none.
    pub id: u64,
    pub name: String,
    /// Who ran it — the app behind a check run, or the context a status was
    /// posted under. Empty when GitHub named neither.
    pub source: String,
    pub state: CheckState,
    /// GitHub's own word for how it went: `success`, `timed out`, `in
    /// progress`, `action required`, … Shown as written, since the four states
    /// above are a colour and not the whole story.
    pub label: String,
    /// The one line it left behind, when it left one.
    pub summary: String,
    /// Everything it wrote about itself, as the markdown it wrote it in — the
    /// output's summary and, under it, its longer text. This is where a
    /// coverage report or a bundle-size table lives; empty for the many checks
    /// that write nothing but a conclusion.
    ///
    /// Empty as well when it would only repeat [`summary`](Self::summary),
    /// which is the common case for a check whose whole output is one line.
    pub report: String,
    pub html_url: String,
    /// How long it took, already in words — `1m 20s`. Empty while it is still
    /// running, and for anything that did not say when it started.
    pub took: String,
    /// How many lines of the code this check marked up. Fetched separately —
    /// see [`check_annotations`] — so this is what says whether there is
    /// anything to go and fetch.
    pub annotations: u32,
}

impl Check {
    /// Whether there is anything behind this row worth opening it for.
    pub fn has_detail(&self) -> bool {
        !self.report.is_empty() || self.annotations > 0
    }
}

/// How loudly a check marked one line of the code.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Level {
    Failure,
    Warning,
    Notice,
}

impl Level {
    pub fn tone(self) -> &'static str {
        match self {
            Level::Failure => "bad",
            Level::Warning => "run",
            Level::Notice => "off",
        }
    }

    /// Not the check's own tick and cross: these mark a line rather than
    /// report a verdict, and a row of crosses down the side of a list of
    /// errors says nothing the colour has not.
    pub fn glyph(self) -> &'static str {
        match self {
            Level::Failure => "✕",
            Level::Warning => "▲",
            Level::Notice => "•",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Level::Failure => "failure",
            Level::Warning => "warning",
            Level::Notice => "notice",
        }
    }
}

/// One line of the code a check had something to say about: the compiler error,
/// the failed assertion, the lint.
///
/// This is the part of a check worth having inside an editor rather than on a
/// web page — it names a file and a line, and the file is in the explorer.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Annotation {
    pub path: PathBuf,
    /// Where it starts, 1-based. GitHub also sends an end line, which is only
    /// worth keeping to say `12–18` beside the message.
    pub line: usize,
    pub end_line: usize,
    pub level: Level,
    /// A heading the check gave it, when it gave it one.
    pub title: String,
    pub message: String,
    /// The long form — a traceback, a diff, the failing assertion in full.
    /// Whitespace in it is the shape somebody's tool printed it in, so it is
    /// kept exactly as it arrived and drawn in a monospaced block.
    pub raw_details: String,
}

/// Everything that ran against one commit.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct Checks {
    /// The commit they are about — the head of the pull request, or the commit
    /// being read out of one. Checks belong to a commit and not to a branch,
    /// and a panel that did not say which would be answering a question nobody
    /// asked.
    pub sha: String,
    pub items: Vec<Check>,
    /// True when there were more check runs than [`MAX_CHECK_PAGES`] holds.
    pub truncated: bool,
}

/// How many of each — what the line at the head of the list says.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Tally {
    pub passed: usize,
    pub failed: usize,
    pub running: usize,
    pub quiet: usize,
}

impl Tally {
    /// The sentence at the top of the panel. Only the parts there is something
    /// to say about: "0 failing" beside twelve passes is noise, and noise in
    /// the one line that answers "is it broken?" is worse than nowhere else.
    pub fn phrase(&self) -> String {
        let mut parts = Vec::new();
        for (n, word) in [
            (self.failed, "failing"),
            (self.running, "running"),
            (self.passed, "passed"),
            (self.quiet, "other"),
        ] {
            if n > 0 {
                parts.push(format!("{n} {word}"));
            }
        }
        if parts.is_empty() {
            return "nothing has run".to_string();
        }
        parts.join(" · ")
    }

    pub fn total(&self) -> usize {
        self.passed + self.failed + self.running + self.quiet
    }

    /// The verdict, as one state.
    ///
    /// A failure outranks anything still running, which outranks a pass:
    /// something already red is red however much of the rest is green, and a
    /// build still going is not one that has passed.
    pub fn state(&self) -> CheckState {
        match () {
            _ if self.failed > 0 => CheckState::Failed,
            _ if self.running > 0 => CheckState::Running,
            _ if self.passed > 0 => CheckState::Passed,
            _ => CheckState::Quiet,
        }
    }
}

impl Checks {
    pub fn tally(&self) -> Tally {
        let mut t = Tally::default();
        for c in &self.items {
            let slot = match c.state {
                CheckState::Passed => &mut t.passed,
                CheckState::Failed => &mut t.failed,
                CheckState::Running => &mut t.running,
                CheckState::Quiet => &mut t.quiet,
            };
            *slot += 1;
        }
        t
    }

    /// The commit's verdict, as one state — see [`Tally::state`].
    pub fn state(&self) -> CheckState {
        self.tally().state()
    }
}

/// The check runs of one commit, a page at a time — an object with the list
/// inside it rather than a bare array, which is why this cannot go through
/// [`get_paged`].
#[derive(Deserialize)]
pub(super) struct RawCheckPage {
    #[serde(default)]
    check_runs: Vec<RawCheckRun>,
}

#[derive(Deserialize)]
pub(super) struct RawCheckRun {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    name: String,
    /// `queued`, `in_progress`, `completed` — and `waiting`, `requested` and
    /// `pending`, which GitHub added later and which all still mean "not yet".
    #[serde(default)]
    status: String,
    /// Set once `status` is `completed`, and null until then.
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    completed_at: Option<String>,
    /// Where the run itself is — the CI service's own page, usually. The
    /// check's page on github.com is the fallback.
    #[serde(default)]
    details_url: Option<String>,
    #[serde(default)]
    html_url: Option<String>,
    #[serde(default)]
    output: Option<RawCheckOutput>,
    #[serde(default)]
    app: Option<RawApp>,
}

#[derive(Deserialize, Default)]
pub(super) struct RawCheckOutput {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    /// The long half of what a check wrote. Null far more often than not.
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    annotations_count: u32,
}

#[derive(Deserialize)]
pub(super) struct RawApp {
    #[serde(default)]
    name: String,
}

/// The combined status of a commit: GitHub answers with the latest status per
/// context, which is exactly the list worth showing.
#[derive(Deserialize)]
pub(super) struct RawStatuses {
    #[serde(default)]
    statuses: Vec<RawStatus>,
}

#[derive(Deserialize)]
pub(super) struct RawStatus {
    /// `success`, `failure`, `error` or `pending`.
    #[serde(default)]
    state: String,
    /// The name the service posts under — `ci/circleci`, `continuous-integration/travis-ci`.
    #[serde(default)]
    context: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    target_url: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
}

/// One of GitHub's machine words, as a person would say it.
pub(super) fn humanised(word: &str) -> String {
    word.to_ascii_lowercase().replace('_', " ")
}

/// How far a summary is worth reading in a pane this narrow. Past this it is a
/// build log, and the link beside it is the way to read one.
pub(super) const MAX_SUMMARY: usize = 160;

/// The first line worth showing of whatever a check said about itself.
///
/// A check's output is markdown, and can be a whole report — tables, badges,
/// stack traces. None of that belongs in a 380px column, and the row links to
/// where it is drawn properly.
pub(super) fn one_line(text: &str) -> String {
    let Some(line) = text.lines().map(str::trim).find(|l| !l.is_empty()) else {
        return String::new();
    };
    match line.char_indices().nth(MAX_SUMMARY) {
        Some((cut, _)) => format!("{}…", line[..cut].trim_end()),
        None => line.to_string(),
    }
}

/// What a check run amounts to, and the word GitHub used for it.
pub(super) fn run_state(status: &str, conclusion: Option<&str>) -> (CheckState, String) {
    match conclusion.unwrap_or_default() {
        // Finished and said nothing about how. Rare enough to have no word of
        // its own, and not something to colour green.
        "" if status == "completed" => (CheckState::Quiet, "finished".to_string()),
        "" => (CheckState::Running, humanised(status)),
        "success" => (CheckState::Passed, "success".to_string()),
        // `action_required` is a build that stopped and is waiting to be let
        // through, which is a red mark on the pull request like any other.
        other @ ("failure" | "timed_out" | "action_required" | "startup_failure") => {
            (CheckState::Failed, humanised(other))
        }
        other => (CheckState::Quiet, humanised(other)),
    }
}

pub(super) fn check_of(raw: RawCheckRun) -> Check {
    let (state, label) = run_state(&raw.status, raw.conclusion.as_deref());
    let output = raw.output.unwrap_or_default();
    // The title is a summary somebody wrote for exactly this purpose; the body
    // of the output is what there is when nobody did.
    let summary = match one_line(output.title.as_deref().unwrap_or_default()) {
        line if line.is_empty() => one_line(output.summary.as_deref().unwrap_or_default()),
        line => line,
    };
    // The two halves of the output as one document, which is what they are
    // written as — and nothing at all where it would only say the row's own
    // line back again, which is most one-line outputs.
    let mut report = String::new();
    for part in [
        output.summary.unwrap_or_default(),
        output.text.unwrap_or_default(),
    ] {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if !report.is_empty() {
            report.push_str("\n\n");
        }
        report.push_str(part);
    }
    if report == summary {
        report.clear();
    }
    Check {
        id: raw.id,
        name: if raw.name.is_empty() {
            "check".to_string()
        } else {
            raw.name
        },
        source: raw.app.map(|a| a.name).unwrap_or_default(),
        state,
        label,
        summary,
        report,
        html_url: raw
            .details_url
            .filter(|u| !u.is_empty())
            .or(raw.html_url)
            .unwrap_or_default(),
        took: took(raw.started_at.as_deref(), raw.completed_at.as_deref()),
        annotations: output.annotations_count,
    }
}

pub(super) fn status_check_of(raw: RawStatus) -> Check {
    let state = match raw.state.as_str() {
        "success" => CheckState::Passed,
        "failure" | "error" => CheckState::Failed,
        "pending" => CheckState::Running,
        _ => CheckState::Quiet,
    };
    Check {
        // A commit status is not a check run: there is nothing to ask GitHub
        // about it by, and nothing to ask for — the description below is the
        // whole of what it has to say.
        id: 0,
        name: if raw.context.is_empty() {
            "status".to_string()
        } else {
            raw.context
        },
        // A commit status has no app behind it; the context is the whole of
        // what it is called, and it is already in the name.
        source: String::new(),
        state,
        label: humanised(&raw.state),
        summary: one_line(raw.description.as_deref().unwrap_or_default()),
        report: String::new(),
        annotations: 0,
        html_url: raw.target_url.unwrap_or_default(),
        // A status is posted, not run: the two timestamps are when it was first
        // posted and when it last changed, which for a finished one is how long
        // it took to get there.
        took: match state {
            CheckState::Running => String::new(),
            _ => took(raw.created_at.as_deref(), raw.updated_at.as_deref()),
        },
    }
}

/// Everything that ran against one commit: its check runs, and the commit
/// statuses the older CI services still post.
///
/// Two requests, because GitHub keeps two lists and a repository may be using
/// either or both. A failure on either fails the pair — a list of checks
/// missing the half that was red is worse than one that says it could not be
/// read.
pub async fn commit_checks(token: &str, repo: &RepoRef, sha: &str) -> Result<Checks> {
    let base = format!(
        "{API}/repos/{}/{}/commits/{}",
        encode_segment(&repo.owner),
        encode_segment(&repo.name),
        encode_segment(sha),
    );

    let mut items = Vec::new();
    let mut truncated = false;
    for page in 1..=MAX_CHECK_PAGES {
        let raw: RawCheckPage = get_json(
            token,
            &format!("{base}/check-runs?per_page=100&page={page}"),
        )
        .await
        .with_context(|| format!("reading the checks on {sha}"))?;
        let full_page = raw.check_runs.len() == 100;
        items.extend(raw.check_runs.into_iter().map(check_of));
        if !full_page {
            break;
        }
        if page == MAX_CHECK_PAGES {
            truncated = true;
        }
    }

    let combined: RawStatuses = get_json(token, &format!("{base}/status?per_page=100"))
        .await
        .with_context(|| format!("reading the commit statuses on {sha}"))?;
    items.extend(combined.statuses.into_iter().map(status_check_of));

    // What is broken first, then what is still going. A list read from the top
    // answers "is anything wrong?" before it answers anything else — and by
    // name within each rank, so reloading it does not shuffle the rows.
    items.sort_by(|a, b| {
        a.state
            .rank()
            .cmp(&b.state.rank())
            .then_with(|| a.name.cmp(&b.name))
    });

    Ok(Checks {
        sha: sha.to_string(),
        items,
        truncated,
    })
}

#[derive(Deserialize)]
pub(super) struct RawAnnotation {
    #[serde(default)]
    path: String,
    #[serde(default)]
    start_line: usize,
    #[serde(default)]
    end_line: usize,
    /// `failure`, `warning` or `notice`.
    #[serde(default)]
    annotation_level: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    raw_details: Option<String>,
}

pub(super) fn annotation_of(raw: RawAnnotation) -> Annotation {
    let line = raw.start_line.max(1);
    Annotation {
        path: PathBuf::from(raw.path),
        line,
        end_line: raw.end_line.max(line),
        level: match raw.annotation_level.as_deref().unwrap_or_default() {
            "failure" => Level::Failure,
            "warning" => Level::Warning,
            // `notice` and anything GitHub adds later: something worth reading,
            // and not something worth colouring like a broken build.
            _ => Level::Notice,
        },
        title: raw.title.unwrap_or_default().trim().to_string(),
        message: raw.message.unwrap_or_default().trim_end().to_string(),
        raw_details: raw.raw_details.unwrap_or_default().trim_end().to_string(),
    }
}

/// Every line one check marked up: the compiler errors, the failed assertions,
/// the lints.
///
/// A separate request per check, and only for a check that says it has some —
/// which is why [`Check::annotations`] is fetched with the list and these are
/// not. It is the one part of a check's report that names a file and a line, so
/// it is the part this app can do something with: see `ui::conversation`, where
/// each one is a way into the code it is about.
pub async fn check_annotations(token: &str, repo: &RepoRef, check: u64) -> Result<Vec<Annotation>> {
    let base = format!(
        "{API}/repos/{}/{}/check-runs/{check}/annotations",
        encode_segment(&repo.owner),
        encode_segment(&repo.name),
    );
    let (raw, _): (Vec<RawAnnotation>, bool) = get_paged(token, &base, MAX_ANNOTATION_PAGES)
        .await
        .with_context(|| "reading what this check marked up".to_string())?;
    Ok(raw.into_iter().map(annotation_of).collect())
}
