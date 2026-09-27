use super::*;

// -------------------------------------------------------------- repo target

#[derive(Clone, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RepoRef {
    pub owner: String,
    pub name: String,
}

impl std::fmt::Display for RepoRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

/// Strip scheme / host / SSH prefix down to the `owner/repo/...` tail.
///
/// `None` when there was none of that to strip, which is how the caller tells
/// something pasted from a browser from something typed by hand — the two do
/// not mean quite the same thing once the host is off the front.
pub(crate) fn strip_host(s: &str) -> Option<&str> {
    s.strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .map(|r| r.trim_start_matches("www."))
        .and_then(|r| r.strip_prefix("github.com/"))
        .or_else(|| s.strip_prefix("git@github.com:"))
        .or_else(|| s.strip_prefix("github.com/"))
}

/// Accepts what a person is likely to paste: `owner/repo`, a browser URL, an
/// SSH remote, or a link to a specific pull request.
pub fn parse_target(input: &str) -> Option<(RepoRef, Option<u64>)> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }

    let rest = strip_host(s).unwrap_or(s);

    let mut parts = rest.split('/').filter(|p| !p.is_empty());
    let owner = parts.next()?.to_string();
    let name = parts.next()?.trim_end_matches(".git").to_string();
    if owner.is_empty() || name.is_empty() {
        return None;
    }

    // `.../pull/123` (or `/pulls/123`) opens that PR directly.
    let number = match (parts.next(), parts.next()) {
        (Some("pull" | "pulls"), Some(n)) => n.parse::<u64>().ok(),
        _ => None,
    };
    Some((RepoRef { owner, name }, number))
}

/// The commit one piece of text names, when it names one: `owner/repo` and a
/// hex sha after the word github.com writes it under.
///
/// `owner/repo/commit/<sha>` as typed, and the browser URL it came from. It is
/// checked before [`parse_target`] wherever both could answer, since that reads
/// the same text as a bare repository with something after it.
pub fn parse_commit_target(input: &str) -> Option<(RepoRef, String)> {
    let s = input.trim();
    let rest = strip_host(s).unwrap_or(s);
    let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
    match parts.as_slice() {
        [owner, name, "commit" | "commits", sha, ..] if is_sha(sha) => Some((
            RepoRef {
                owner: owner.to_string(),
                name: name.trim_end_matches(".git").to_string(),
            },
            sha.to_string(),
        )),
        _ => None,
    }
}

/// A commit, as git lets one be written: hex, and enough of it to be worth
/// resolving. Seven is what everybody quotes and what GitHub's own links use;
/// forty is the whole hash.
pub fn is_sha(s: &str) -> bool {
    (7..=40).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The account one piece of text names, when it names an account and not a
/// repository inside one.
///
/// `torvalds`, `torvalds/`, `@torvalds`, and the two URLs GitHub hands out for
/// an account: its profile, and the `orgs/…/repositories` page the
/// Repositories tab lands on. Anything with a repository in it belongs to
/// [`parse_target`] instead.
///
/// The login is checked against GitHub's own rule rather than sent as typed:
/// it is what keeps a half-written search phrase from costing a request that
/// can only come back 404.
pub fn parse_owner(input: &str) -> Option<String> {
    let s = input.trim();
    let hosted = strip_host(s);
    let rest = hosted.unwrap_or(s);
    // Only from a URL: `orgs/x` typed by hand is a repository called `x`.
    let rest = match hosted {
        Some(_) => rest.strip_prefix("orgs/").unwrap_or(rest),
        None => rest,
    };

    let mut parts = rest.split('/').filter(|p| !p.is_empty());
    let login = parts.next()?.trim_start_matches('@');
    // The Repositories tab is still the account; a repository name is not.
    if !matches!(parts.next(), None | Some("repositories")) {
        return None;
    }
    is_login(login).then(|| login.to_string())
}

/// GitHub's rule for a login: letters, digits and hyphens, up to 39 of them,
/// and not starting or ending with one.
pub(super) fn is_login(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 39
        && !s.starts_with('-')
        && !s.ends_with('-')
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
