use super::*;

// ----------------------------------------------------------------- writing
//
// Everything pullspace says back to GitHub. Four requests, all of them a POST
// of a small JSON body, and all of them on behalf of whoever pasted the token:
// there is no server here to say anything as anybody else.

/// Which side of a diff a line is counted on — the base's numbering or the
/// head's. A removed line only exists on the left; an added one only on the
/// right; an unchanged one is on both, and GitHub files it under whichever the
/// reviewer pointed at.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    pub(super) fn parse(s: &str) -> Option<Side> {
        match s.to_ascii_uppercase().as_str() {
            "LEFT" => Some(Side::Left),
            "RIGHT" => Some(Side::Right),
            _ => None,
        }
    }

    /// As GitHub spells it.
    pub fn wire(self) -> &'static str {
        match self {
            Side::Left => "LEFT",
            Side::Right => "RIGHT",
        }
    }
}

/// What a review says about the pull request as a whole.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    Comment,
    Approve,
    RequestChanges,
}

impl Verdict {
    fn wire(self) -> &'static str {
        match self {
            Verdict::Comment => "COMMENT",
            Verdict::Approve => "APPROVE",
            Verdict::RequestChanges => "REQUEST_CHANGES",
        }
    }
}

/// A comment on one line of the diff, as it is sent.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LineNote {
    pub path: String,
    pub line: usize,
    pub side: Side,
    pub body: String,
}

impl LineNote {
    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "path": self.path,
            "line": self.line,
            "side": self.side.wire(),
            "body": self.body,
        })
    }
}

/// What GitHub says when it will not do something: a sentence, and sometimes a
/// list of the particular things wrong with what was asked.
#[derive(Deserialize, Default)]
pub(super) struct RawError {
    #[serde(default)]
    message: String,
    #[serde(default)]
    errors: Vec<serde_json::Value>,
}

/// Why a write came back refused, in GitHub's words where it has any.
///
/// Worth the care because the refusals are specific and the reader can act on
/// them — "Can not approve your own pull request", "Line could not be
/// resolved", "pull request review thread line must be part of the diff" — and
/// the status alone says none of that.
pub(super) fn write_error(status: u16, body: &[u8]) -> anyhow::Error {
    let raw: RawError = serde_json::from_slice(body).unwrap_or_default();
    // `errors` is a list of strings on some endpoints and of objects with a
    // `message` on others.
    let details: Vec<String> = raw
        .errors
        .iter()
        .filter_map(|e| match e {
            serde_json::Value::String(s) => Some(s.clone()),
            other => other.get("message")?.as_str().map(str::to_string),
        })
        .filter(|d| !d.is_empty() && *d != raw.message)
        .collect();
    let said = match (raw.message.is_empty(), details.is_empty()) {
        (true, true) => String::new(),
        (false, true) => raw.message,
        (true, false) => details.join("; "),
        (false, false) => format!("{}: {}", raw.message, details.join("; ")),
    };
    match status {
        // A token that can read a repository but not write to it gets a 404
        // from the write endpoints, not a 403 — GitHub will not confirm what
        // it will not let you touch.
        404 => anyhow::anyhow!(
            "GitHub would not take this (404). The token can read this pull \
             request but may not be allowed to write to it — a classic token \
             needs the `repo` scope (or `public_repo`), a fine-grained one \
             needs \"Pull requests: Read and write\" on this repository."
        ),
        _ if said.is_empty() => anyhow::anyhow!("GitHub refused this (HTTP {status})."),
        _ => anyhow::anyhow!("GitHub refused this: {said}"),
    }
}

/// POST `body` as JSON to `url`, as the signed-in account.
pub(super) async fn post_json(token: &str, url: &str, body: serde_json::Value) -> Result<()> {
    if token.is_empty() {
        bail!("Sign in to GitHub first — writing needs a token.");
    }
    let auth = format!("Bearer {token}");
    let headers = [
        ("Accept", "application/vnd.github+json"),
        ("Content-Type", "application/json"),
        ("X-GitHub-Api-Version", API_VERSION),
        ("Authorization", auth.as_str()),
    ];
    let reply = http::post(url, &headers, body.to_string()).await?;
    if reply.status == 403 {
        // Not `refused`'s "may lack the repo scope" for every 403: a write is
        // refused for reasons of its own (a locked conversation, an archived
        // repository), and GitHub names them.
        let spent = reply.rate_remaining.as_deref() == Some("0");
        if !spent {
            return Err(write_error(403, &reply.body));
        }
    }
    refused(token, &reply)?;
    if !(200..300).contains(&reply.status) {
        return Err(write_error(reply.status, &reply.body));
    }
    Ok(())
}

pub(super) fn pulls_url(repo: &RepoRef, number: u64) -> String {
    format!(
        "{API}/repos/{}/{}/pulls/{number}",
        encode_segment(&repo.owner),
        encode_segment(&repo.name)
    )
}

/// Add to the pull request's discussion — the box at the foot of the
/// conversation on github.com.
pub async fn post_comment(token: &str, repo: &RepoRef, number: u64, body: &str) -> Result<()> {
    let url = format!(
        "{API}/repos/{}/{}/issues/{number}/comments",
        encode_segment(&repo.owner),
        encode_segment(&repo.name)
    );
    post_json(token, &url, serde_json::json!({ "body": body }))
        .await
        .with_context(|| format!("commenting on #{number}"))
}

/// Answer a thread of line comments. `to` is any comment in it; GitHub files
/// the reply under the thread's root either way.
pub async fn reply_to(token: &str, repo: &RepoRef, number: u64, to: u64, body: &str) -> Result<()> {
    let url = format!("{}/comments/{to}/replies", pulls_url(repo, number));
    post_json(token, &url, serde_json::json!({ "body": body }))
        .await
        .with_context(|| format!("replying on #{number}"))
}

/// One line comment, on its own and at once — GitHub's "Add single comment".
///
/// `commit` is the head the line was read at. A line number means nothing
/// without the diff it was counted in, and the head may have moved since.
pub async fn post_line_comment(
    token: &str,
    repo: &RepoRef,
    number: u64,
    commit: &str,
    note: &LineNote,
) -> Result<()> {
    let mut body = note.json();
    body["commit_id"] = serde_json::Value::String(commit.to_string());
    post_json(
        token,
        &format!("{}/comments", pulls_url(repo, number)),
        body,
    )
    .await
    .with_context(|| format!("commenting on {}:{}", note.path, note.line))
}

/// The JSON a review goes as. Apart from [`submit_review`] so that its shape
/// can be checked without a network.
pub(super) fn review_body(
    commit: Option<&str>,
    verdict: Verdict,
    body: &str,
    notes: &[LineNote],
) -> serde_json::Value {
    let mut out = serde_json::json!({ "event": verdict.wire() });
    // An empty body is left out rather than sent: GitHub reads `""` on an
    // approval as a summary somebody wrote, and shows an empty one.
    if !body.trim().is_empty() {
        out["body"] = serde_json::Value::String(body.to_string());
    }
    if let Some(commit) = commit {
        out["commit_id"] = serde_json::Value::String(commit.to_string());
    }
    if !notes.is_empty() {
        out["comments"] = notes.iter().map(LineNote::json).collect();
    }
    out
}

/// Submit a review: a verdict, a summary, and the line comments that were
/// being held for it, all in one request — so they arrive together, as one
/// review, the way they do on github.com.
pub async fn submit_review(
    token: &str,
    repo: &RepoRef,
    number: u64,
    commit: Option<&str>,
    verdict: Verdict,
    body: &str,
    notes: &[LineNote],
) -> Result<()> {
    let ask = review_body(commit, verdict, body, notes);
    post_json(token, &format!("{}/reviews", pulls_url(repo, number)), ask)
        .await
        .with_context(|| format!("reviewing #{number}"))
}
