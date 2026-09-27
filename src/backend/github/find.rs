use super::*;

// ----------------------------------------------------------- finding a repo

#[derive(Deserialize)]
pub(super) struct RawRepo {
    pub(super) full_name: String,
    #[serde(default)]
    pub(super) description: Option<String>,
    #[serde(default)]
    pub(super) private: bool,
    #[serde(default)]
    pub(super) fork: bool,
    #[serde(default)]
    pub(super) archived: bool,
    #[serde(default)]
    pub(super) stargazers_count: u64,
    #[serde(default)]
    pub(super) pushed_at: Option<String>,
    #[serde(default)]
    pub(super) default_branch: String,
}

#[derive(Deserialize)]
pub(super) struct RawSearch {
    #[serde(default)]
    items: Vec<RawRepo>,
}

/// A repository offered as a suggestion in the picker.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RepoHit {
    pub repo: RepoRef,
    pub description: String,
    pub private: bool,
    pub fork: bool,
    pub archived: bool,
    pub stars: u64,
    /// `YYYY-MM-DD` of the last push, empty when GitHub did not say.
    pub pushed: String,
}

/// `full_name` is the only field we cannot do without — everything else is
/// decoration, so a response missing it is the one that gets dropped.
pub(super) fn hit_of(raw: RawRepo) -> Option<RepoHit> {
    let (owner, name) = raw.full_name.split_once('/')?;
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    Some(RepoHit {
        repo: RepoRef {
            owner: owner.to_string(),
            name: name.to_string(),
        },
        description: raw.description.unwrap_or_default(),
        private: raw.private,
        fork: raw.fork,
        archived: raw.archived,
        stars: raw.stargazers_count,
        // The rest of the timestamp is the time of day, which says nothing
        // useful about how current a repository is.
        pushed: raw
            .pushed_at
            .map(|d| d.chars().take(10).collect())
            .unwrap_or_default(),
    })
}

/// Repositories matching free text, best match first — so a repository can be
/// found by name rather than pasted as a link.
///
/// An exact `owner/name` is looked up directly as well and pinned to the top:
/// search runs off an index that a brand-new, renamed or private repository may
/// not be in yet, and the name typed in full is not a guess to be ranked.
pub async fn search_repos(token: &str, query: &str, limit: u32) -> Result<Vec<RepoHit>> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }

    let mut out = Vec::new();
    if let Some((repo, _)) = parse_target(q) {
        let url = format!(
            "{API}/repos/{}/{}",
            encode_segment(&repo.owner),
            encode_segment(&repo.name),
        );
        // A 404 here is the ordinary case — half of a repository name typed so
        // far is not a repository — so the error is the answer, not a failure.
        if let Ok(raw) = get_json::<RawRepo>(token, &url).await {
            out.extend(hit_of(raw));
        }
    }
    // A complete name that resolves is the answer. Searching for it as well
    // would bury it under near-misses, and a pasted URL would go to the index
    // as `https github com owner name pull 3`.
    if !out.is_empty() && q.contains('/') {
        return Ok(out);
    }

    // GitHub's index does not read `owner/name` as a path, so the slash is only
    // noise in the query that reaches it.
    let text = q.replace('/', " ");
    let url = format!(
        "{API}/search/repositories?q={}&per_page={limit}",
        encode_segment(text.trim()),
    );
    match get_json::<RawSearch>(token, &url).await {
        Ok(raw) => {
            for hit in raw.items.into_iter().filter_map(hit_of) {
                if !out.iter().any(|h: &RepoHit| h.repo == hit.repo) {
                    out.push(hit);
                }
            }
        }
        // Search has its own, much smaller rate limit than the rest of the API.
        // Spending it should not cost us a repository already in hand.
        Err(e) if out.is_empty() => return Err(e),
        Err(_) => {}
    }
    out.truncate(limit as usize);
    Ok(out)
}

/// The signed-in account's repositories, most recently pushed first — what the
/// picker offers before anything is typed, since the pull requests you are
/// asked to review are nearly always on one of them.
pub async fn my_repos(token: &str, limit: u32) -> Result<Vec<RepoHit>> {
    if token.is_empty() {
        return Ok(Vec::new());
    }
    let url = format!(
        "{API}/user/repos?sort=pushed&direction=desc&per_page={limit}\
         &affiliation=owner,collaborator,organization_member"
    );
    let raw: Vec<RawRepo> = get_json(token, &url).await?;
    Ok(hits_of(raw))
}

pub(super) fn hits_of(raw: Vec<RawRepo>) -> Vec<RepoHit> {
    raw.into_iter().filter_map(hit_of).collect()
}

// -------------------------------------------------------- finding an account

#[derive(Deserialize)]
pub(super) struct RawOwner {
    login: String,
    /// `User` or `Organization`.
    #[serde(rename = "type", default)]
    kind: String,
    /// The display name — "The Rust Programming Language" over `rust-lang`.
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    public_repos: u64,
}

/// An account offered as a suggestion: the row that opens up everything it
/// owns, rather than one repository.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct OwnerHit {
    pub login: String,
    /// An organisation rather than a person. Only decoration — both list their
    /// repositories the same way.
    pub org: bool,
    /// The display name, empty when the account has none or it is the login
    /// again.
    pub name: String,
    pub public_repos: u64,
}

pub(super) fn owner_of(raw: RawOwner) -> Option<OwnerHit> {
    if raw.login.is_empty() {
        return None;
    }
    let name = raw.name.unwrap_or_default();
    let name = if name.eq_ignore_ascii_case(&raw.login) {
        String::new()
    } else {
        name
    };
    Some(OwnerHit {
        org: raw.kind == "Organization",
        login: raw.login,
        name,
        public_repos: raw.public_repos,
    })
}

/// The account a name belongs to, if it belongs to one.
///
/// This is what makes typing an organisation's name work rather than nearly
/// work: the search index ranks repositories by *their* names, so an
/// organisation whose repositories are not called after it — which is most of
/// them — cannot be found by searching for it. A login, on the other hand, is
/// a lookup and always exact.
///
/// It costs one request on the `core` budget, where typing otherwise spends
/// only `search`. That is the trade, and it is the right way round: `core` is
/// sixty an hour signed out against ten a minute for `search`, and this only
/// goes out for text shaped like a login in the first place.
///
/// `None` covers every way it can fail, because they all mean the same thing
/// here — no account row to offer, and a search still on its way.
pub async fn lookup_owner(token: &str, login: &str) -> Option<OwnerHit> {
    if !is_login(login) {
        return None;
    }
    let url = format!("{API}/users/{}", encode_segment(login));
    owner_of(get_json::<RawOwner>(token, &url).await.ok()?)
}

/// Everything one account owns, most recently pushed first.
///
/// Three endpoints, because GitHub keeps three lists and only two of them can
/// see anything private: `/user/repos` for the signed-in account itself,
/// `/orgs/…/repos` for an organisation the token is a member of, and
/// `/users/…/repos`, which is public, answers for people and organisations
/// alike, and is where anonymous browsing ends up.
pub async fn owner_repos(
    token: &str,
    viewer: &str,
    owner: &str,
    limit: u32,
) -> Result<Vec<RepoHit>> {
    let login = encode_segment(owner);
    let page = format!("sort=pushed&direction=desc&per_page={limit}");

    // Your own account, which is the one whose private repositories you are
    // most likely to be looking for.
    if !viewer.is_empty() && viewer.eq_ignore_ascii_case(owner) {
        let url = format!("{API}/user/repos?affiliation=owner&{page}");
        return Ok(hits_of(get_json(token, &url).await?));
    }
    // A member's token sees an organisation's private repositories here and
    // nowhere else. Anonymously this answers with the same public list as the
    // call below, so it is not worth the request — and 404s for a person.
    if !token.is_empty() {
        let url = format!("{API}/orgs/{login}/repos?type=all&{page}");
        if let Ok(raw) = get_json::<Vec<RawRepo>>(token, &url).await {
            return Ok(hits_of(raw));
        }
    }
    let url = format!("{API}/users/{login}/repos?{page}");
    Ok(hits_of(get_json(token, &url).await?))
}
