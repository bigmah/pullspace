use super::*;

// ------------------------------------------------------------- conversation

/// Where a piece of writing on a pull request came from. GitHub keeps these on
/// three separate endpoints, and they read differently enough to be worth
/// telling apart once they are back in one list.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum CommentKind {
    /// The pull request's own discussion thread.
    Discussion,
    /// What a reviewer wrote when submitting a review.
    Review,
    /// Left on a line of the diff.
    Inline,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Comment {
    pub kind: CommentKind,
    pub author: String,
    /// ISO 8601, as GitHub sends it — kept whole because it is what the
    /// three lists are merged on.
    pub created_at: String,
    /// Markdown source. Empty for a bare approval, which is still worth showing.
    pub body: String,
    pub html_url: String,
    /// `approved`, `changes requested`, … for a review; empty otherwise.
    pub verdict: String,
    /// The file a line comment hangs off, and the line in the head commit.
    pub path: Option<PathBuf>,
    pub line: Option<usize>,
    /// GitHub's id for it — what a reply is addressed to. Zero where GitHub
    /// sent none, which nothing can be addressed by.
    #[serde(default)]
    pub id: u64,
    /// Line comments only: the comment this one answers, which is always the
    /// first of its thread — GitHub files every reply under the root.
    #[serde(default)]
    pub reply_to: Option<u64>,
    /// Line comments only: which side of the diff `line` counts on.
    #[serde(default)]
    pub side: Option<Side>,
    /// Line comments only: the lines it was left on have since been rewritten,
    /// so `line` is where it *was*, in a diff that no longer exists.
    #[serde(default)]
    pub outdated: bool,
}

impl Comment {
    /// The comment a thread of line comments is filed under: this one, unless
    /// it is a reply.
    pub fn thread_root(&self) -> u64 {
        self.reply_to.unwrap_or(self.id)
    }
}

/// Everything written on a pull request, oldest first.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct Thread {
    pub comments: Vec<Comment>,
    /// True when one of the lists ran past [`MAX_COMMENT_PAGES`].
    pub truncated: bool,
}

/// One JSON shape for all three endpoints: they agree on the fields that
/// matter and each leaves the rest out, which `Option` already handles.
#[derive(Deserialize)]
pub(super) struct RawComment {
    #[serde(default)]
    user: Option<User>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    /// Reviews date themselves with this instead.
    #[serde(default)]
    submitted_at: Option<String>,
    #[serde(default)]
    html_url: String,
    /// Reviews only: `APPROVED`, `CHANGES_REQUESTED`, `COMMENTED`, `PENDING`.
    #[serde(default)]
    state: Option<String>,
    /// Line comments only.
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    line: Option<usize>,
    /// Where the comment was left when it was written — the fallback for one
    /// whose lines have since been rewritten, which GitHub answers with a null
    /// `line`.
    #[serde(default)]
    original_line: Option<usize>,
    #[serde(default)]
    id: u64,
    /// Line comments only: the root of the thread a reply belongs to.
    #[serde(default)]
    in_reply_to_id: Option<u64>,
    /// Line comments only: `LEFT` or `RIGHT`.
    #[serde(default)]
    side: Option<String>,
}

/// GitHub's review states, in the words the pane uses.
pub(super) fn verdict_label(state: &str) -> String {
    match state.to_ascii_uppercase().as_str() {
        "APPROVED" => "approved".to_string(),
        "CHANGES_REQUESTED" => "changes requested".to_string(),
        "DISMISSED" => "dismissed".to_string(),
        "COMMENTED" => String::new(),
        other => other.to_ascii_lowercase().replace('_', " "),
    }
}

pub(super) fn comment_of(raw: RawComment, kind: CommentKind) -> Comment {
    Comment {
        kind,
        author: author_of(&raw.user),
        created_at: raw.created_at.or(raw.submitted_at).unwrap_or_default(),
        // Trailing blank lines are common in a template-filled description and
        // would otherwise be rendered as empty space.
        body: raw.body.unwrap_or_default().trim_end().to_string(),
        html_url: raw.html_url,
        verdict: raw.state.as_deref().map(verdict_label).unwrap_or_default(),
        outdated: raw.path.is_some() && raw.line.is_none(),
        path: raw.path.map(PathBuf::from),
        line: raw.line.or(raw.original_line),
        id: raw.id,
        reply_to: raw.in_reply_to_id,
        side: raw.side.as_deref().and_then(Side::parse),
    }
}

/// Read a list endpoint page by page from the beginning. The bool is true when
/// there was more than `pages` worth — which every caller has to say something
/// about, since a list silently cut off is a list read as complete.
pub(super) async fn get_paged<T: serde::de::DeserializeOwned>(
    token: &str,
    base: &str,
    pages: u32,
) -> Result<(Vec<T>, bool)> {
    let (items, _, more) = get_pages(token, base, 1, pages).await?;
    Ok((items, more))
}

/// The same, from page `from` rather than from the first — for a list that is
/// read on and on rather than once, and so has to say where it got to.
///
/// The count of pages actually read comes back with them: a short page ends the
/// walk early, and a caller that assumed it had read `count` of them would ask
/// for the wrong page next time.
pub(super) async fn get_pages<T: serde::de::DeserializeOwned>(
    token: &str,
    base: &str,
    from: u32,
    count: u32,
) -> Result<(Vec<T>, u32, bool)> {
    let mut out = Vec::new();
    let mut read = 0;
    for page in from..from.saturating_add(count) {
        let url = format!("{base}?per_page=100&page={page}");
        let raw: Vec<T> = get_json(token, &url).await?;
        let full_page = raw.len() == 100;
        out.extend(raw);
        read += 1;
        if !full_page {
            return Ok((out, read, false));
        }
    }
    Ok((out, read, true))
}

/// A submitted review with nothing to say is just the envelope its line
/// comments arrived in — those are fetched separately, so showing the envelope
/// as well would double every one of them.
pub(super) fn review_is_noise(raw: &RawComment) -> bool {
    let empty = raw.body.as_deref().unwrap_or_default().trim().is_empty();
    let state = raw
        .state
        .as_deref()
        .unwrap_or_default()
        .to_ascii_uppercase();
    // A pending review is a draft, visible only to the person writing it.
    state == "PENDING" || (empty && state != "APPROVED" && state != "CHANGES_REQUESTED")
}

/// The whole conversation: the discussion, the review summaries, and the
/// comments left on lines of the diff, merged into one list in the order they
/// were written.
///
/// Three requests, because GitHub keeps the three on separate endpoints. A
/// failure on any of them fails the lot — a conversation with a third of itself
/// silently missing is worse than one that says it could not be loaded.
pub async fn pr_comments(token: &str, repo: &RepoRef, number: u64) -> Result<Thread> {
    let owner = encode_segment(&repo.owner);
    let name = encode_segment(&repo.name);

    let mut comments = Vec::new();
    let mut truncated = false;

    let (discussion, more): (Vec<RawComment>, bool) = get_paged(
        token,
        &format!("{API}/repos/{owner}/{name}/issues/{number}/comments"),
        MAX_COMMENT_PAGES,
    )
    .await
    .with_context(|| format!("reading the discussion on #{number}"))?;
    truncated |= more;
    comments.extend(
        discussion
            .into_iter()
            .map(|c| comment_of(c, CommentKind::Discussion)),
    );

    let (inline, more): (Vec<RawComment>, bool) = get_paged(
        token,
        &format!("{API}/repos/{owner}/{name}/pulls/{number}/comments"),
        MAX_COMMENT_PAGES,
    )
    .await
    .with_context(|| format!("reading the line comments on #{number}"))?;
    truncated |= more;
    comments.extend(
        inline
            .into_iter()
            .map(|c| comment_of(c, CommentKind::Inline)),
    );

    let (reviews, more): (Vec<RawComment>, bool) = get_paged(
        token,
        &format!("{API}/repos/{owner}/{name}/pulls/{number}/reviews"),
        MAX_COMMENT_PAGES,
    )
    .await
    .with_context(|| format!("reading the reviews of #{number}"))?;
    truncated |= more;
    comments.extend(
        reviews
            .into_iter()
            .filter(|r| !review_is_noise(r))
            .map(|c| comment_of(c, CommentKind::Review)),
    );

    // ISO 8601 in UTC, which sorts as text. A stable sort keeps a review and
    // the line comments it was submitted with in the order they came back.
    comments.sort_by(|a, b| a.created_at.cmp(&b.created_at));

    Ok(Thread {
        comments,
        truncated,
    })
}
