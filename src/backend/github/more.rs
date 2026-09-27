use super::*;

// ------------------------------------------------- the list, filled in

/// GitHub's other API, which answers one question this one cannot.
pub(super) const GRAPHQL: &str = "https://api.github.com/graphql";

/// What the reviewers have made of a pull request, where its repository asks
/// for their say-so.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Review {
    Approved,
    ChangesRequested,
    /// The branch needs an approval and does not have one yet.
    Required,
}

impl Review {
    pub fn label(self) -> &'static str {
        match self {
            Review::Approved => "approved",
            Review::ChangesRequested => "changes requested",
            Review::Required => "review required",
        }
    }

    /// The stylesheet's name for the colour that goes with it — the same four
    /// a check is drawn in.
    pub fn tone(self) -> &'static str {
        match self {
            Review::Approved => "ok",
            Review::ChangesRequested => "bad",
            Review::Required => "off",
        }
    }
}

/// What a list of pull requests does not say about one of them, and a row of
/// that list is worth reading for: whether the build is green, what the
/// reviewers made of it, how big it is, how much has been said.
///
/// The REST list has none of it. Asked for there, the checks alone are two
/// requests a row — two hundred for a full page, which is more than an
/// anonymous caller gets in three hours. GraphQL answers for the whole page in
/// one, and the price is that it will not talk to anybody who has not signed
/// in: so this is an extra, laid over rows that are complete without it.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct PrMore {
    pub additions: u32,
    pub deletions: u32,
    pub files: u32,
    /// Everything said on it: the conversation, the reviews, and the comments
    /// on lines.
    pub comments: u32,
    /// `None` where nobody has to approve anything.
    pub review: Option<Review>,
    /// What ran against its head commit. `None` where nothing did.
    pub checks: Option<Tally>,
    /// It cannot be merged as it stands: the base has moved under it.
    pub conflicts: bool,
}

/// How many pull requests to ask GraphQL about at once.
///
/// Not the whole list, though the whole list is one query: rolling up the
/// checks of a hundred head commits takes GitHub about as long as it allows a
/// query to take, and the answer to running over is a 502 with nothing in it. A
/// quarter of that comes back in a couple of seconds, from the top of the list
/// down — which is the order the rows are being read in anyway.
pub const MORE_PAGE: usize = 25;

pub(super) const MORE_QUERY: &str = "query($owner:String!,$name:String!,$states:[PullRequestState!],\
$first:Int!,$after:String){\
repository(owner:$owner,name:$name){\
pullRequests(first:$first,after:$after,states:$states,orderBy:{field:UPDATED_AT,direction:DESC}){\
pageInfo{hasNextPage endCursor} \
nodes{number additions deletions changedFiles reviewDecision mergeable totalCommentsCount \
commits(last:1){nodes{commit{statusCheckRollup{state contexts{\
checkRunCountsByState{state count} statusContextCountsByState{state count}}}}}}}}}}";

/// One page of [`PrMore`], and where the page after it starts.
#[derive(Debug, Default)]
pub struct MorePage {
    pub more: HashMap<u64, PrMore>,
    /// The cursor to ask for the next page with. `None` at the end of the list.
    pub next: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct GqlReply {
    #[serde(default)]
    data: Option<GqlData>,
    #[serde(default)]
    errors: Vec<GqlError>,
}

#[derive(Deserialize)]
pub(super) struct GqlError {
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
pub(super) struct GqlData {
    #[serde(default)]
    repository: Option<GqlRepo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct GqlRepo {
    pull_requests: GqlNodes<RawMore>,
}

/// A GraphQL list. Any entry of one can be null — it is how GraphQL says "this
/// one I may not show you" without failing the rest.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct GqlNodes<T> {
    #[serde(default = "Vec::new")]
    nodes: Vec<Option<T>>,
    #[serde(default)]
    page_info: Option<GqlPageInfo>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct GqlPageInfo {
    #[serde(default)]
    has_next_page: bool,
    #[serde(default)]
    end_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RawMore {
    number: u64,
    #[serde(default)]
    additions: u32,
    #[serde(default)]
    deletions: u32,
    #[serde(default)]
    changed_files: u32,
    /// `APPROVED`, `CHANGES_REQUESTED`, `REVIEW_REQUIRED` — or null, on a branch
    /// nobody has to approve anything for.
    #[serde(default)]
    review_decision: Option<String>,
    /// `MERGEABLE`, `CONFLICTING`, or `UNKNOWN` while GitHub works it out.
    #[serde(default)]
    mergeable: Option<String>,
    #[serde(default)]
    total_comments_count: Option<u32>,
    #[serde(default)]
    commits: Option<GqlNodes<RawMoreCommit>>,
}

#[derive(Deserialize)]
pub(super) struct RawMoreCommit {
    commit: RawMoreCommitBody,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RawMoreCommitBody {
    #[serde(default)]
    status_check_rollup: Option<RawRollup>,
}

#[derive(Deserialize)]
pub(super) struct RawRollup {
    /// The rollup's own verdict. Only read when the counts under it are
    /// missing, which is what a token that may not see them leaves behind.
    #[serde(default)]
    state: String,
    #[serde(default)]
    contexts: Option<RawRollupCounts>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RawRollupCounts {
    #[serde(default)]
    check_run_counts_by_state: Vec<RawCount>,
    #[serde(default)]
    status_context_counts_by_state: Vec<RawCount>,
}

#[derive(Deserialize)]
pub(super) struct RawCount {
    #[serde(default)]
    state: String,
    #[serde(default)]
    count: usize,
}

/// Which of the four one of GraphQL's words comes to — the check runs' fourteen
/// and the commit statuses' five, which overlap and agree where they do.
///
/// The same sorting as [`run_state`] and [`status_check_of`], so a row here and
/// the checks pane beside the code cannot colour one build two ways.
pub(super) fn rollup_state(word: &str) -> CheckState {
    match word {
        "SUCCESS" => CheckState::Passed,
        "FAILURE" | "ERROR" | "TIMED_OUT" | "ACTION_REQUIRED" | "STARTUP_FAILURE" => {
            CheckState::Failed
        }
        "IN_PROGRESS" | "PENDING" | "QUEUED" | "WAITING" | "REQUESTED" | "EXPECTED" => {
            CheckState::Running
        }
        // Skipped, cancelled, stale, neutral — and `COMPLETED`, which is a run
        // that finished and never said how.
        _ => CheckState::Quiet,
    }
}

pub(super) fn tally_of(rollup: RawRollup) -> Tally {
    let mut t = Tally::default();
    let counts = rollup.contexts.unwrap_or(RawRollupCounts {
        check_run_counts_by_state: Vec::new(),
        status_context_counts_by_state: Vec::new(),
    });
    for c in counts
        .check_run_counts_by_state
        .iter()
        .chain(&counts.status_context_counts_by_state)
    {
        let slot = match rollup_state(&c.state) {
            CheckState::Passed => &mut t.passed,
            CheckState::Failed => &mut t.failed,
            CheckState::Running => &mut t.running,
            CheckState::Quiet => &mut t.quiet,
        };
        *slot += c.count;
    }
    // No counts to go on, but a verdict: one of whatever it was, so the row
    // still gets its colour.
    if t.total() == 0 {
        let slot = match rollup_state(&rollup.state) {
            CheckState::Passed => &mut t.passed,
            CheckState::Failed => &mut t.failed,
            CheckState::Running => &mut t.running,
            CheckState::Quiet => &mut t.quiet,
        };
        *slot = 1;
    }
    t
}

pub(super) fn more_of(raw: RawMore) -> (u64, PrMore) {
    let checks = raw
        .commits
        .and_then(|c| c.nodes.into_iter().flatten().next())
        .and_then(|c| c.commit.status_check_rollup)
        .map(tally_of);
    let more = PrMore {
        additions: raw.additions,
        deletions: raw.deletions,
        files: raw.changed_files,
        comments: raw.total_comments_count.unwrap_or_default(),
        review: match raw.review_decision.as_deref() {
            Some("APPROVED") => Some(Review::Approved),
            Some("CHANGES_REQUESTED") => Some(Review::ChangesRequested),
            Some("REVIEW_REQUIRED") => Some(Review::Required),
            _ => None,
        },
        checks,
        conflicts: raw.mergeable.as_deref() == Some("CONFLICTING"),
    };
    (raw.number, more)
}

/// Read the answer to [`MORE_QUERY`], by pull request number.
///
/// GraphQL fails by halves: a field this token may not read comes back null
/// with a complaint beside it, and everything else comes back as asked. So the
/// complaints only become the error when there is nothing else to show.
pub(super) fn parse_more(body: &[u8]) -> Result<MorePage> {
    let reply: GqlReply =
        serde_json::from_slice(body).context("parsing what GitHub said about the list")?;
    let Some(repo) = reply.data.and_then(|d| d.repository) else {
        match reply.errors.first() {
            Some(e) if !e.message.is_empty() => bail!("GitHub said: {}", e.message),
            _ => bail!("GitHub had nothing to say about this repository's pull requests"),
        }
    };
    let list = repo.pull_requests;
    Ok(MorePage {
        next: list
            .page_info
            .filter(|p| p.has_next_page)
            .and_then(|p| p.end_cursor),
        more: list.nodes.into_iter().flatten().map(more_of).collect(),
    })
}

/// Checks, reviews, sizes and comment counts for the pull requests
/// [`list_prs`] lists — the same ones, asked for the same way round, a
/// [`MORE_PAGE`] at a time. `after` is where the last page said the next one
/// starts; `None` asks for the top of the list.
///
/// Signed in only: GraphQL has no anonymous tier, and the caller is expected to
/// know that before asking rather than find out from a 401 that reads like a
/// rejected token.
pub async fn pr_more(
    token: &str,
    repo: &RepoRef,
    state: PrState,
    after: Option<&str>,
) -> Result<MorePage> {
    let states: &[&str] = match state {
        PrState::Open => &["OPEN"],
        PrState::Closed => &["CLOSED", "MERGED"],
        PrState::All => &["OPEN", "CLOSED", "MERGED"],
    };
    let ask = serde_json::json!({
        "query": MORE_QUERY,
        "variables": {
            "owner": repo.owner,
            "name": repo.name,
            "states": states,
            "first": MORE_PAGE,
            "after": after,
        },
    });
    let auth = format!("Bearer {token}");
    let headers = [
        ("Accept", "application/vnd.github+json"),
        ("Content-Type", "application/json"),
        ("Authorization", auth.as_str()),
    ];
    let reply = http::post(GRAPHQL, &headers, ask.to_string()).await?;
    refused(token, &reply)?;
    if !(200..300).contains(&reply.status) {
        bail!("GitHub returned HTTP {}", reply.status);
    }
    parse_more(&reply.body)
}
