use super::*;

/// Percent-encode one path segment into `out`. Avoids a dependency for the
/// handful of characters that actually show up in repo paths.
pub(super) fn push_encoded(out: &mut String, seg: &str) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for b in seg.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => {
                out.push('%');
                out.push(HEX[usize::from(b >> 4)] as char);
                out.push(HEX[usize::from(b & 0xf)] as char);
            }
        }
    }
}

/// Also the encoder behind `backend::route`'s share links — the unreserved set
/// is the same on both sides of the address bar.
pub(crate) fn encode_segment(seg: &str) -> String {
    let mut out = String::with_capacity(seg.len());
    push_encoded(&mut out, seg);
    out
}

pub(super) fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 8);
    for (i, seg) in path.split('/').enumerate() {
        if i > 0 {
            out.push('/');
        }
        push_encoded(&mut out, seg);
    }
    out
}

// ------------------------------------------------------------------ request

/// How long until a budget refills, from `x-ratelimit-reset`.
///
/// `None` when the header is missing or already in the past — a wait of "0
/// seconds" is worse than not saying.
pub(super) fn seconds_until(reset: Option<&str>) -> Option<u64> {
    let at: u64 = reset?.trim().parse().ok()?;
    // web_time reads `Date.now()` in a page; std's SystemTime panics there.
    let now = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    at.checked_sub(now).filter(|left| *left > 0)
}

pub(super) fn wait_phrase(secs: u64) -> String {
    match secs {
        s if s <= 90 => format!("Try again in {s} seconds."),
        s => format!("Try again in {} minutes.", s.div_ceil(60)),
    }
}

/// What ran out, how long it is out for, and what still works meanwhile.
///
/// Worth this much care because the honest answer is usually reassuring. The
/// budget people actually exhaust is `search`, which refills every minute and
/// is spent by typing — while the hourly budget that opens repositories and
/// pull requests sits there untouched. "Rate limit exceeded" over that reads as
/// "come back in an hour", and sends people away from an app that would have
/// opened anything they could name.
pub(super) fn rate_limited(
    token: &str,
    resource: Option<&str>,
    reset: Option<&str>,
) -> anyhow::Error {
    let wait = seconds_until(reset)
        .map(wait_phrase)
        .unwrap_or_else(|| "Try again shortly.".to_string());
    let anon = token.is_empty();

    if resource == Some("search") {
        let allowance = if anon {
            "GitHub allows 10 repository searches a minute when signed out"
        } else {
            "GitHub allows 30 repository searches a minute"
        };
        return anyhow::anyhow!(
            "{allowance}, and this browser has used them. {wait} Searching is the \
             only thing affected — typing a full owner/name, or pasting a link to \
             a pull request, still opens it."
        );
    }

    if anon {
        return anyhow::anyhow!(
            "GitHub allows 60 API requests an hour when signed out, and this browser \
             has used them. {wait} A token raises it to 5000."
        );
    }
    anyhow::anyhow!("GitHub API rate limit exceeded. {wait}")
}

pub(super) async fn get_raw(token: &str, url: &str, accept: &str) -> Result<(u16, Vec<u8>)> {
    // An empty token means anonymous: public repos still work, at GitHub's
    // much lower unauthenticated rate limit.
    let auth = format!("Bearer {token}");
    let mut headers = vec![("Accept", accept), ("X-GitHub-Api-Version", API_VERSION)];
    if !token.is_empty() {
        headers.push(("Authorization", auth.as_str()));
    }
    let reply = http::get(url, &headers).await?;
    refused(token, &reply)?;
    Ok((reply.status, reply.body))
}

/// The answers that are GitHub declining to answer, whatever was asked and
/// however it was asked: a token it will not take, a budget that has run out,
/// a door that is shut.
pub(super) fn refused(token: &str, reply: &http::Reply) -> Result<()> {
    if reply.status == 401 {
        // Read on the sign-in form as often as anywhere else, so it says what
        // is wrong rather than what to do about it — "sign in again" is no help
        // to somebody in the middle of signing in.
        bail!(
            "GitHub rejected the token (401). It may have expired, been revoked, \
             or been copied short."
        );
    }
    // An emptied budget is a 403 for search and for the API at large, a 429 when
    // GitHub feels strongly about it, and occasionally a 404. What tells them
    // apart from a genuine refusal is the budget reading zero.
    let spent = reply.rate_remaining.as_deref() == Some("0");
    if spent && matches!(reply.status, 403 | 404 | 429) {
        return Err(rate_limited(
            token,
            reply.rate_resource.as_deref(),
            reply.rate_reset.as_deref(),
        ));
    }
    if reply.status == 429 {
        return Err(rate_limited(token, None, reply.rate_reset.as_deref()));
    }
    if reply.status == 403 {
        bail!("GitHub denied access (403). The token may lack the `repo` scope.");
    }
    Ok(())
}

pub(super) async fn get_json<T: serde::de::DeserializeOwned>(token: &str, url: &str) -> Result<T> {
    let (status, body) = get_raw(token, url, "application/vnd.github+json").await?;
    if status == 404 {
        bail!(
            "Not found (404). Check the name — and if it is a private repository, \
             sign in to an account that can see it."
        );
    }
    if !(200..300).contains(&status) {
        bail!("GitHub returned HTTP {status}");
    }
    serde_json::from_slice(&body).with_context(|| format!("parsing response from {url}"))
}

// ------------------------------------------------------------------- models

#[derive(Deserialize)]
pub(super) struct User {
    pub(super) login: String,
    /// Where their picture is. Only the lists that draw one read it.
    #[serde(default)]
    pub(super) avatar_url: Option<String>,
}

/// Verify a token and get the account it belongs to.
pub async fn viewer_login(token: &str) -> Result<String> {
    let user: User = get_json(token, &format!("{API}/user")).await?;
    Ok(user.login)
}
