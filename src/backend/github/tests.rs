use std::path::Path;

use super::*;

/// A moment `secs` from now, as `x-ratelimit-reset` writes it.
fn reset_in(secs: u64) -> String {
    let now = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    (now + secs).to_string()
}

#[test]
fn a_spent_search_budget_says_so_and_says_what_still_works() {
    let e = format!(
        "{:#}",
        rate_limited("", Some("search"), Some(&reset_in(40)))
    );
    assert!(e.contains("10 repository searches a minute"), "{e}");
    assert!(e.contains("40 seconds"), "{e}");
    // The whole point: the rest of the app is still open for business.
    assert!(e.contains("owner/name"), "{e}");
}

#[test]
fn a_signed_in_search_budget_is_the_larger_one() {
    let e = format!("{:#}", rate_limited("ghp_x", Some("search"), None));
    assert!(e.contains("30 repository searches a minute"), "{e}");
    assert!(!e.contains("signed out"), "{e}");
}

#[test]
fn the_hourly_budget_names_the_token_that_would_raise_it() {
    let e = format!(
        "{:#}",
        rate_limited("", Some("core"), Some(&reset_in(1800)))
    );
    assert!(e.contains("60 API requests an hour"), "{e}");
    assert!(e.contains("5000"), "{e}");
    assert!(e.contains("30 minutes"), "{e}");
}

#[test]
fn a_reset_already_past_is_not_a_wait_of_zero() {
    assert_eq!(seconds_until(Some("1")), None);
    assert_eq!(seconds_until(None), None);
    assert_eq!(seconds_until(Some("not a number")), None);
    let e = format!("{:#}", rate_limited("", Some("core"), Some("1")));
    assert!(e.contains("Try again shortly."), "{e}");
}

#[test]
fn waits_read_in_whichever_unit_is_shorter() {
    assert_eq!(wait_phrase(45), "Try again in 45 seconds.");
    assert_eq!(wait_phrase(90), "Try again in 90 seconds.");
    // Rounded up: "1 minute" that is really 91 seconds sends people back
    // early, and early is another spent request.
    assert_eq!(wait_phrase(91), "Try again in 2 minutes.");
    assert_eq!(wait_phrase(3600), "Try again in 60 minutes.");
}

/// A snapshot of `(path, blob)` pairs, sorted the way a real one is.
fn snapshot(files: &[(&str, &str)]) -> Snapshot {
    let mut files: Vec<TreeEntry> = files
        .iter()
        .map(|(path, sha)| TreeEntry {
            path: PathBuf::from(path),
            sha: sha.to_string(),
            size: 0,
        })
        .collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Snapshot {
        files,
        ..Snapshot::default()
    }
}

fn changed(path: &str, status: ChangeKind) -> PrFile {
    PrFile {
        path: PathBuf::from(path),
        previous_path: None,
        status,
    }
}

/// A pull request with only the three fields `blob_key` reads filled in.
fn pr_with(head: Snapshot, base: Snapshot, files: Vec<PrFile>) -> PrDetail {
    PrDetail {
        repo: RepoRef::default(),
        number: 1,
        title: String::new(),
        body: String::new(),
        author: String::new(),
        state: String::new(),
        draft: false,
        html_url: String::new(),
        head_ref: String::new(),
        head_repo: None,
        base_ref: String::new(),
        base_sha: String::new(),
        head_sha: String::new(),
        files,
        truncated: false,
        tree: head,
        base_tree: base,
    }
}

#[test]
fn a_file_is_remembered_as_the_blob_it_is_made_of() {
    let pr = pr_with(
        snapshot(&[("src/a.rs", "aaa"), ("src/b.rs", "bbb")]),
        snapshot(&[("src/a.rs", "old")]),
        vec![changed("src/a.rs", ChangeKind::Modified)],
    );
    // The head side: what the file is now, not what it was called.
    assert_eq!(pr.blob_key(Path::new("src/a.rs")), "aaa");
}

#[test]
fn a_deleted_file_is_remembered_as_the_side_it_still_has() {
    let mut renamed = changed("new/name.rs", ChangeKind::Renamed);
    renamed.previous_path = Some(PathBuf::from("old/name.rs"));
    let pr = pr_with(
        snapshot(&[("new/name.rs", "moved")]),
        snapshot(&[("gone.rs", "was-here"), ("old/name.rs", "before")]),
        vec![changed("gone.rs", ChangeKind::Deleted), renamed],
    );
    // Deleted: no head side at all, so the base blob is its identity.
    assert_eq!(pr.blob_key(Path::new("gone.rs")), "was-here");
    // Renamed: it does have a head side, which is the one that counts —
    // moving a file without touching it must not untick it.
    assert_eq!(pr.blob_key(Path::new("new/name.rs")), "moved");
}

#[test]
fn a_pull_request_with_no_tree_falls_back_to_the_path() {
    let pr = pr_with(
        Snapshot::default(),
        Snapshot::default(),
        vec![changed("src/a.rs", ChangeKind::Modified)],
    );
    assert_eq!(pr.blob_key(Path::new("src/a.rs")), "path:src/a.rs");
}

#[test]
fn rewriting_a_file_changes_what_it_is_remembered_as() {
    let before = pr_with(
        snapshot(&[("src/a.rs", "aaa"), ("src/b.rs", "bbb")]),
        Snapshot::default(),
        vec![
            changed("src/a.rs", ChangeKind::Modified),
            changed("src/b.rs", ChangeKind::Modified),
        ],
    );
    // A force-push: `a.rs` was rewritten, `b.rs` was carried over untouched.
    let after = pr_with(
        snapshot(&[("src/a.rs", "zzz"), ("src/b.rs", "bbb")]),
        Snapshot::default(),
        before.files.clone(),
    );
    assert_ne!(
        before.blob_key(Path::new("src/a.rs")),
        after.blob_key(Path::new("src/a.rs")),
        "rewritten, so a tick against it must not carry over"
    );
    assert_eq!(
        before.blob_key(Path::new("src/b.rs")),
        after.blob_key(Path::new("src/b.rs")),
        "byte-identical, so it stays read"
    );
}

/// A commit as the endpoint sends it, with only the fields read here.
fn raw_commit(sha: &str, message: &str, login: Option<&str>, name: &str) -> RawPrCommit {
    RawPrCommit {
        sha: sha.to_string(),
        commit: RawCommitBody {
            message: message.to_string(),
            author: Some(RawSignature {
                name: name.to_string(),
                date: "2026-08-13T09:00:00Z".to_string(),
            }),
        },
        author: login.map(|login| User {
            login: login.to_string(),
            avatar_url: None,
        }),
        html_url: String::new(),
    }
}

#[test]
fn a_commit_link_is_read_however_it_arrives() {
    let sha = "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0";
    for typed in [
        format!("o/r/commit/{sha}"),
        format!("https://github.com/o/r/commit/{sha}"),
        format!("github.com/o/r/commits/{sha}"),
        // GitHub hangs a file and a line off its own commit links.
        format!("https://github.com/o/r/commit/{sha}/files/src/main.rs"),
    ] {
        let (repo, got) = parse_commit_target(&typed).expect("{typed}");
        assert_eq!(repo.to_string(), "o/r", "{typed}");
        assert_eq!(got, sha, "{typed}");
    }
    // The short form people actually paste out of a terminal.
    assert_eq!(
        parse_commit_target("o/r/commit/abc1234").map(|(_, sha)| sha),
        Some("abc1234".to_string())
    );
}

#[test]
fn only_something_shaped_like_a_sha_is_a_commit() {
    // A branch, a tag, half a sha, and a word that is not hex: all of them
    // are a repository with something after it, not a commit.
    for typed in [
        "o/r/commit/main",
        "o/r/commit/v1.2.3",
        "o/r/commit/abc123",
        "o/r/commit/zzzzzzzz",
        "o/r/commit/",
        "o/r/pull/12",
        "o/r",
    ] {
        assert!(parse_commit_target(typed).is_none(), "{typed}");
    }
    assert!(is_sha("abc1234"));
    assert!(is_sha(&"a".repeat(40)));
    assert!(!is_sha(&"a".repeat(41)));
    assert!(!is_sha("abc123"));
}

/// The refs endpoint answers with whole ref names, and only the ones under
/// `refs/heads/` are branches.
#[test]
fn a_ref_is_read_back_as_the_branch_it_names() {
    let branch = |name: &str| {
        branch_of_ref(
            serde_json::from_str(&format!(
                r#"{{"ref":"{name}","object":{{"sha":"abc1234","type":"commit"}}}}"#
            ))
            .unwrap(),
        )
    };
    let got = branch("refs/heads/feat/branch-list").expect("a branch");
    assert_eq!(got.name, "feat/branch-list");
    assert_eq!(got.sha, "abc1234");
    // Not knowing is not the same as knowing it is unprotected — see
    // `Branch::protected`.
    assert!(!got.protected);

    // A tag is not a branch, and neither is a ref name with nothing after
    // the prefix.
    assert!(branch("refs/tags/v1.0").is_none());
    assert!(branch("refs/heads/").is_none());
}

#[test]
fn a_commit_remembers_its_files_by_the_blob_they_are_made_of() {
    let view = CommitView {
        repo: RepoRef::default(),
        commit: commit_of(raw_commit("abc1234", "m", None, "Ada")),
        parent_sha: "parent".to_string(),
        merge: false,
        files: vec![
            changed("src/a.rs", ChangeKind::Modified),
            changed("gone.rs", ChangeKind::Deleted),
        ],
        truncated: false,
        tree: snapshot(&[("src/a.rs", "now")]),
        base_tree: snapshot(&[("src/a.rs", "before"), ("gone.rs", "was-here")]),
        from: CommitFrom::Alone,
    };
    // The head side, exactly as a pull request's is…
    assert_eq!(view.blob_key(Path::new("src/a.rs")), "now");
    // …and the base side for the one with no head side left.
    assert_eq!(view.blob_key(Path::new("gone.rs")), "was-here");
    assert_eq!(
        view.blob_key_of(&changed("src/a.rs", ChangeKind::Modified)),
        "now"
    );
}

#[test]
fn a_commit_message_is_a_subject_and_what_follows_it() {
    let one = commit_of(raw_commit(
        "a".repeat(40).as_str(),
        "fix the thing",
        None,
        "Ada",
    ));
    assert_eq!(one.short(), "aaaaaaa", "seven characters, not eight");
    assert_eq!(one.subject(), "fix the thing");
    assert_eq!(one.body(), "", "a one-line message has nothing under it");

    let full = commit_of(raw_commit(
        "0123456789abcdef",
        "fix the thing\n\nBecause it was broken.\n\nCloses #12\n",
        None,
        "Ada",
    ));
    assert_eq!(full.short(), "0123456");
    assert_eq!(full.subject(), "fix the thing");
    assert_eq!(full.body(), "Because it was broken.\n\nCloses #12");
}

/// A sha shorter than the seven characters everybody quotes — which is not
/// something GitHub sends, and is not something to panic over either.
#[test]
fn a_short_sha_is_as_short_as_it_is() {
    let stub = commit_of(raw_commit("abc", "x", None, ""));
    assert_eq!(stub.short(), "abc");
}

#[test]
fn a_commit_is_attributed_to_the_account_first_and_the_signature_after() {
    let with_account = commit_of(raw_commit("s", "m", Some("ada"), "Ada Lovelace"));
    assert_eq!(
        with_account.author, "ada",
        "the login is what the PR is under"
    );

    let no_account = commit_of(raw_commit("s", "m", None, "Ada Lovelace"));
    assert_eq!(
        no_account.author, "Ada Lovelace",
        "an email GitHub does not know still has a name on it"
    );

    let anonymous = commit_of(raw_commit("s", "m", None, ""));
    assert_eq!(anonymous.author, "unknown", "rather than an empty column");
}

#[test]
fn merged_and_closed_are_not_the_same_news() {
    let raw = |state: &str, merged: bool| RawPr {
        number: 1,
        title: "t".to_string(),
        body: None,
        user: None,
        draft: false,
        state: state.to_string(),
        merged_at: merged.then(|| "2026-08-13T09:00:00Z".to_string()),
        created_at: None,
        updated_at: String::new(),
        html_url: String::new(),
        head: RawRef {
            name: "feature".to_string(),
            sha: String::new(),
            repo: None,
        },
        base: RawRef {
            name: "main".to_string(),
            sha: String::new(),
            repo: None,
        },
        labels: Vec::new(),
        requested_reviewers: Vec::new(),
        requested_teams: Vec::new(),
    };
    let base = RepoRef::default();
    let open = summary_of(&base, raw("open", false));
    assert!(open.is_open() && !open.merged);
    assert_eq!(open.status(), PrStatus::Open);
    // Both of these are `closed` to GitHub, and the badge on them differs.
    let landed = summary_of(&base, raw("closed", true));
    assert!(!landed.is_open() && landed.merged);
    assert_eq!(landed.status(), PrStatus::Merged);
    let dropped = summary_of(&base, raw("closed", false));
    assert!(!dropped.is_open() && !dropped.merged);
    assert_eq!(dropped.status(), PrStatus::Closed);
    // And nobody's account is still somebody.
    assert_eq!(open.author, "ghost");
}

/// Everything a row of the list says comes out of the one request that
/// listed it — which is the whole reason a row can afford to say it.
#[test]
fn a_listed_pull_request_says_who_when_and_what_about() {
    let raw: RawPr = serde_json::from_str(
        r#"{"number":482,"title":"Fix the crash","body":"Wide characters.\n\nFixes #480.",
            "state":"open","draft":true,
            "user":{"login":"ada","avatar_url":"https://avatars.githubusercontent.com/u/1?v=4"},
            "created_at":"2026-09-18T09:00:00Z","updated_at":"2026-09-21T07:30:00Z",
            "html_url":"https://github.com/o/r/pull/482",
            "labels":[{"name":"bug","color":"d73a4a"},{"name":"odd","color":"red;x"},{"name":" "}],
            "requested_reviewers":[{"login":"bob"}],
            "requested_teams":[{"slug":"core"}],
            "head":{"ref":"fix/wide","sha":"1","repo":{"full_name":"ada/r"}},
            "base":{"ref":"main","sha":"2","repo":{"full_name":"o/r"}}}"#,
    )
    .unwrap();
    let base = RepoRef {
        owner: "o".to_string(),
        name: "r".to_string(),
    };
    let pr = summary_of(&base, raw);
    assert_eq!(
        pr.status(),
        PrStatus::Draft,
        "a draft is open, and says draft"
    );
    assert_eq!(pr.author, "ada");
    assert!(pr.avatar.ends_with("/u/1?v=4"));
    assert_eq!(pr.created_at, "2026-09-18T09:00:00Z");
    assert!(pr.body.starts_with("Wide characters."));
    assert_eq!(
        pr.head_label(),
        "ada:fix/wide",
        "a fork is named by its owner"
    );
    assert_eq!(pr.reviewers, ["bob", "core"]);
    // A colour goes into a style attribute, so only a colour gets there —
    // and a label with no name is not a label.
    let labels: Vec<_> = pr
        .labels
        .iter()
        .map(|l| (l.name.as_str(), l.color.as_str()))
        .collect();
    assert_eq!(labels, [("bug", "#d73a4a"), ("odd", "")]);
}

#[test]
fn how_long_ago_is_said_in_the_largest_unit_worth_reading() {
    let now = epoch_secs("2026-09-21T12:00:00Z").unwrap();
    let said = |ts: &str| (ago(ts, now), ago_short(ts, now));
    let both = |long: &str, short: &str| (long.to_string(), short.to_string());

    assert_eq!(said("2026-09-21T11:59:30Z"), both("just now", "now"));
    assert_eq!(said("2026-09-21T11:59:00Z"), both("1 minute ago", "1m"));
    assert_eq!(said("2026-09-21T09:10:00Z"), both("2 hours ago", "2h"));
    assert_eq!(said("2026-09-18T12:00:00Z"), both("3 days ago", "3d"));
    assert_eq!(said("2026-08-29T12:00:00Z"), both("3 weeks ago", "3w"));
    assert_eq!(said("2026-05-01T12:00:00Z"), both("4 months ago", "4mo"));
    assert_eq!(said("2024-09-01T12:00:00Z"), both("2 years ago", "2y"));
    // Somebody else's clock, a few seconds fast.
    assert_eq!(said("2026-09-21T12:00:05Z"), both("just now", "now"));
    // And something that is not a time says nothing rather than something
    // wrong.
    assert_eq!(said("yesterday"), both("", ""));
}

/// The order asked of GraphQL is the order asked of REST, or the pages of
/// one would not be the top of the other.
#[test]
fn the_extras_are_asked_for_in_the_order_of_the_list() {
    assert!(MORE_QUERY.contains("field:UPDATED_AT,direction:DESC"));
    assert!(MORE_QUERY.contains("first:$first,after:$after"));
    assert_eq!(PR_PAGE % MORE_PAGE, 0, "whole pages add up to the list");
}

#[test]
fn what_graphql_says_about_a_list_is_read_by_number() {
    let body = br#"{"data":{"repository":{"pullRequests":{"nodes":[
        {"number":7,"additions":120,"deletions":30,"changedFiles":8,
         "reviewDecision":"CHANGES_REQUESTED","mergeable":"CONFLICTING","totalCommentsCount":4,
         "commits":{"nodes":[{"commit":{"statusCheckRollup":{"state":"FAILURE","contexts":{
           "checkRunCountsByState":[{"state":"SUCCESS","count":19},{"state":"FAILURE","count":1},
             {"state":"SKIPPED","count":2},{"state":"IN_PROGRESS","count":0}],
           "statusContextCountsByState":[{"state":"PENDING","count":1},{"state":"ERROR","count":1}]}}}}]}},
        null,
        {"number":8,"additions":1,"deletions":0,"changedFiles":1,
         "reviewDecision":null,"mergeable":"UNKNOWN","totalCommentsCount":0,
         "commits":{"nodes":[{"commit":{"statusCheckRollup":null}}]}}
    ],"pageInfo":{"hasNextPage":true,"endCursor":"Y3Vyc29y"}}}}}"#;
    let page = parse_more(body).unwrap();
    assert_eq!(
        page.next.as_deref(),
        Some("Y3Vyc29y"),
        "and there is a page after it"
    );
    let more = page.more;
    assert_eq!(
        more.len(),
        2,
        "a null entry is one GitHub would not show, not a failure"
    );

    let red = &more[&7];
    assert_eq!((red.additions, red.deletions, red.files), (120, 30, 8));
    assert_eq!(red.comments, 4);
    assert_eq!(red.review, Some(Review::ChangesRequested));
    assert!(red.conflicts);
    // Both of GitHub's lists, in one tally — sorted the way the checks pane
    // sorts them.
    let t = red.checks.unwrap();
    assert_eq!((t.passed, t.failed, t.running, t.quiet), (19, 2, 1, 2));
    assert_eq!(t.state(), CheckState::Failed);

    let plain = &more[&8];
    assert_eq!(plain.review, None, "nobody has to approve anything here");
    assert_eq!(plain.checks, None, "and nothing ran");
    assert!(!plain.conflicts, "not known to conflict is not a conflict");
}

/// GraphQL fails by halves, and the half that arrived is worth having.
#[test]
fn a_field_the_token_may_not_read_costs_that_field_and_nothing_else() {
    let body = br#"{"data":{"repository":{"pullRequests":{"nodes":[
        {"number":7,"additions":5,"deletions":5,"changedFiles":1,"reviewDecision":"APPROVED",
         "commits":{"nodes":[{"commit":{"statusCheckRollup":{"state":"SUCCESS","contexts":null}}}]}}
    ],"pageInfo":{"hasNextPage":false,"endCursor":"Y3Vyc29y"}}}},
    "errors":[{"message":"Resource not accessible by personal access token"}]}"#;
    let page = parse_more(body).unwrap();
    assert_eq!(
        page.next, None,
        "the last page has nothing after it, cursor or no cursor"
    );
    let more = page.more;
    assert_eq!(more[&7].review, Some(Review::Approved));
    // A verdict with no counts behind it still colours the row.
    assert_eq!(more[&7].checks.unwrap().state(), CheckState::Passed);

    // Nothing at all, though, is an error — and it is GitHub's own words
    // that say which.
    let refused = br#"{"data":{"repository":null},"errors":[{"message":"Could not resolve to a Repository"}]}"#;
    let e = format!("{:#}", parse_more(refused).unwrap_err());
    assert!(e.contains("Could not resolve"), "{e}");
}

#[test]
fn parses_owner_repo() {
    let (r, n) = parse_target("rust-lang/rust").unwrap();
    assert_eq!(r.owner, "rust-lang");
    assert_eq!(r.name, "rust");
    assert_eq!(n, None);
}

#[test]
fn parses_browser_url() {
    let (r, n) = parse_target("https://github.com/DioxusLabs/dioxus").unwrap();
    assert_eq!(r.to_string(), "DioxusLabs/dioxus");
    assert_eq!(n, None);
}

#[test]
fn parses_pull_request_url() {
    let (r, n) = parse_target("https://github.com/rust-lang/rust/pull/12345").unwrap();
    assert_eq!(r.to_string(), "rust-lang/rust");
    assert_eq!(n, Some(12345));
}

#[test]
fn rejects_junk() {
    assert!(parse_target("").is_none());
    assert!(parse_target("   ").is_none());
    assert!(parse_target("just-an-owner").is_none());
}

#[test]
fn an_owner_is_read_however_it_arrives() {
    for typed in [
        "torvalds",
        "  torvalds  ",
        "torvalds/",
        "@torvalds",
        "github.com/torvalds",
        "https://github.com/torvalds",
        "https://www.github.com/torvalds/",
        "https://github.com/orgs/torvalds/repositories",
    ] {
        assert_eq!(parse_owner(typed).as_deref(), Some("torvalds"), "{typed}");
    }
}

#[test]
fn a_repository_is_not_an_owner() {
    // Both halves named: that is a target, not an account.
    assert!(parse_owner("rust-lang/rust").is_none());
    assert!(parse_owner("https://github.com/rust-lang/rust/pull/1").is_none());
    // `orgs/` only counts as GitHub's own URL, never as something typed.
    assert_eq!(parse_owner("orgs/rust-lang").as_deref(), None);
}

#[test]
fn only_a_login_shaped_name_is_worth_a_request() {
    assert!(parse_owner("").is_none());
    assert!(parse_owner("two words").is_none());
    assert!(parse_owner("dots.and.such").is_none());
    assert!(parse_owner("-leading").is_none());
    assert!(parse_owner("trailing-").is_none());
    assert!(parse_owner(&"a".repeat(40)).is_none());
    assert_eq!(parse_owner(&"a".repeat(39)).unwrap().len(), 39);
}

#[test]
fn an_owner_hit_keeps_what_tells_two_accounts_apart() {
    let raw: RawOwner = serde_json::from_str(
        r#"{"login":"rust-lang","type":"Organization",
            "name":"The Rust Programming Language","public_repos":142}"#,
    )
    .unwrap();
    let hit = owner_of(raw).unwrap();
    assert!(hit.org);
    assert_eq!(hit.name, "The Rust Programming Language");
    assert_eq!(hit.public_repos, 142);
}

#[test]
fn an_owner_hit_survives_a_bare_response() {
    let raw: RawOwner = serde_json::from_str(r#"{"login":"bigmah"}"#).unwrap();
    let hit = owner_of(raw).unwrap();
    assert!(!hit.org, "no type means a person, which is the common case");
    assert!(hit.name.is_empty());
    assert_eq!(hit.public_repos, 0);

    // A display name that is the login again says nothing twice.
    let raw: RawOwner = serde_json::from_str(r#"{"login":"bigmah","name":"BigMah"}"#).unwrap();
    assert!(owner_of(raw).unwrap().name.is_empty());
}

#[test]
fn search_hits_split_the_full_name() {
    let raw: RawRepo = serde_json::from_str(
        r#"{"full_name":"DioxusLabs/dioxus","description":"Fullstack UI",
            "private":false,"fork":false,"archived":false,
            "stargazers_count":21000,"pushed_at":"2026-08-01T12:33:04Z"}"#,
    )
    .unwrap();
    let hit = hit_of(raw).unwrap();
    assert_eq!(hit.repo.to_string(), "DioxusLabs/dioxus");
    assert_eq!(hit.stars, 21000);
    // The time of day is dropped; the date is what the row shows.
    assert_eq!(hit.pushed, "2026-08-01");
}

#[test]
fn search_hits_survive_a_bare_response() {
    // Everything but `full_name` is optional, and a hit without one is not
    // a repository we could open.
    let raw: RawRepo = serde_json::from_str(r#"{"full_name":"owner/name"}"#).unwrap();
    let hit = hit_of(raw).unwrap();
    assert_eq!(hit.repo.to_string(), "owner/name");
    assert!(hit.description.is_empty());
    assert!(hit.pushed.is_empty());

    let orphan: RawRepo = serde_json::from_str(r#"{"full_name":"no-slash"}"#).unwrap();
    assert!(hit_of(orphan).is_none());
}

#[test]
fn encodes_awkward_paths() {
    assert_eq!(encode_path("src/main.rs"), "src/main.rs");
    assert_eq!(encode_path("a b/c#d.rs"), "a%20b/c%23d.rs");
    // Separators survive; segment contents do not.
    assert_eq!(encode_path("dir/sub dir/f.rs"), "dir/sub%20dir/f.rs");
}

/// Directories are not entries of the tree — it is rebuilt from the blob
/// paths — so this is what a link naming one is checked against.
#[test]
fn a_directory_is_a_path_some_file_is_under() {
    let mut tree = Snapshot::default();
    for path in ["src/main.rs", "src/ui/app.rs", "srcery/lib.rs", "README.md"] {
        tree.files.push(TreeEntry {
            path: PathBuf::from(path),
            sha: String::new(),
            size: 0,
        });
    }
    assert!(tree.has_dir(Path::new("src")));
    assert!(tree.has_dir(Path::new("src/ui")));
    // Component by component: `src` is not the front of `srcery`.
    assert!(!tree.has_dir(Path::new("sr")));
    assert!(!tree.has_dir(Path::new("src/nope")));
    // A file is not a directory to open, and the root is not one to name.
    assert!(!tree.has_dir(Path::new("")));
    // The root itself has no entry, and neither does an empty repository.
    assert!(!Snapshot::default().has_dir(Path::new("src")));
}

#[test]
fn maps_github_statuses() {
    assert_eq!(change_kind("added"), ChangeKind::Added);
    assert_eq!(change_kind("removed"), ChangeKind::Deleted);
    assert_eq!(change_kind("renamed"), ChangeKind::Renamed);
    assert_eq!(change_kind("modified"), ChangeKind::Modified);
    assert_eq!(change_kind("changed"), ChangeKind::Modified);
}

#[test]
fn renamed_files_read_the_old_path_on_the_base_side() {
    let f = PrFile {
        path: PathBuf::from("new.rs"),
        previous_path: Some(PathBuf::from("old.rs")),
        status: ChangeKind::Renamed,
    };
    assert_eq!(f.base_path(), &PathBuf::from("old.rs"));

    let plain = PrFile {
        path: PathBuf::from("same.rs"),
        previous_path: None,
        status: ChangeKind::Modified,
    };
    assert_eq!(plain.base_path(), &PathBuf::from("same.rs"));
}

#[test]
fn discussion_comments_carry_who_and_when() {
    let raw: RawComment = serde_json::from_str(
        r#"{"user":{"login":"octocat"},"body":"Looks good to me\n\n",
            "created_at":"2026-08-01T09:12:00Z",
            "html_url":"https://github.com/o/n/pull/1#issuecomment-9"}"#,
    )
    .unwrap();
    let c = comment_of(raw, CommentKind::Discussion);
    assert_eq!(c.author, "octocat");
    assert_eq!(c.created_at, "2026-08-01T09:12:00Z");
    // Trailing blank lines would render as empty space in the pane.
    assert_eq!(c.body, "Looks good to me");
    assert!(c.verdict.is_empty());
    assert_eq!(c.path, None);
}

#[test]
fn reviews_date_themselves_with_submitted_at() {
    let raw: RawComment = serde_json::from_str(
        r#"{"user":{"login":"reviewer"},"body":"","state":"APPROVED",
            "submitted_at":"2026-08-02T10:00:00Z","html_url":"u"}"#,
    )
    .unwrap();
    assert!(
        !review_is_noise(&raw),
        "a bare approval still says something"
    );
    let c = comment_of(raw, CommentKind::Review);
    assert_eq!(c.created_at, "2026-08-02T10:00:00Z");
    assert_eq!(c.verdict, "approved");
}

#[test]
fn empty_reviews_that_only_wrap_line_comments_are_dropped() {
    let wrapper: RawComment =
        serde_json::from_str(r#"{"body":"","state":"COMMENTED","submitted_at":"t"}"#).unwrap();
    assert!(review_is_noise(&wrapper));

    let spoken: RawComment =
        serde_json::from_str(r#"{"body":"one nit","state":"COMMENTED","submitted_at":"t"}"#)
            .unwrap();
    assert!(!review_is_noise(&spoken));

    // A draft nobody else can see.
    let pending: RawComment =
        serde_json::from_str(r#"{"body":"wip","state":"PENDING","submitted_at":"t"}"#).unwrap();
    assert!(review_is_noise(&pending));

    let rejected: RawComment =
        serde_json::from_str(r#"{"body":"","state":"CHANGES_REQUESTED"}"#).unwrap();
    assert!(!review_is_noise(&rejected));
    assert_eq!(verdict_label("CHANGES_REQUESTED"), "changes requested");
}

#[test]
fn line_comments_keep_a_line_to_jump_to() {
    let fresh: RawComment = serde_json::from_str(
        r#"{"user":{"login":"a"},"body":"nit","path":"src/main.rs","line":42,
            "original_line":7,"created_at":"t","html_url":"u"}"#,
    )
    .unwrap();
    let c = comment_of(fresh, CommentKind::Inline);
    assert_eq!(c.path, Some(PathBuf::from("src/main.rs")));
    assert_eq!(c.line, Some(42));

    // Outdated: the lines it was written against are gone, so GitHub sends
    // a null `line` and only remembers where it started out.
    let stale: RawComment =
        serde_json::from_str(r#"{"body":"nit","path":"src/old.rs","line":null,"original_line":7}"#)
            .unwrap();
    let c = comment_of(stale, CommentKind::Inline);
    assert_eq!(c.line, Some(7));
    assert!(c.outdated);
}

#[test]
fn a_reply_knows_its_thread_and_its_side() {
    let root: RawComment =
        serde_json::from_str(r#"{"id":10,"body":"why?","path":"a.rs","line":3,"side":"LEFT"}"#)
            .unwrap();
    let reply: RawComment = serde_json::from_str(
        r#"{"id":11,"in_reply_to_id":10,"body":"because","path":"a.rs","line":3,"side":"LEFT"}"#,
    )
    .unwrap();
    let root = comment_of(root, CommentKind::Inline);
    let reply = comment_of(reply, CommentKind::Inline);
    assert_eq!(root.side, Some(Side::Left));
    assert!(!root.outdated);
    assert_eq!(root.thread_root(), 10);
    assert_eq!(reply.thread_root(), 10);
}

#[test]
fn a_review_sends_only_what_was_written() {
    let bare = review_body(None, Verdict::Approve, "  ", &[]);
    assert_eq!(bare, serde_json::json!({ "event": "APPROVE" }));

    let note = LineNote {
        path: "src/lib.rs".to_string(),
        line: 12,
        side: Side::Right,
        body: "off by one?".to_string(),
    };
    let full = review_body(Some("abc"), Verdict::RequestChanges, "see notes", &[note]);
    assert_eq!(
        full,
        serde_json::json!({
            "event": "REQUEST_CHANGES",
            "body": "see notes",
            "commit_id": "abc",
            "comments": [
                { "path": "src/lib.rs", "line": 12, "side": "RIGHT", "body": "off by one?" }
            ],
        })
    );
}

#[test]
fn a_refused_write_says_what_github_said() {
    let e = write_error(
        422,
        br#"{"message":"Unprocessable Entity","errors":["Can not approve your own pull request"]}"#,
    );
    let said = format!("{e}");
    assert!(
        said.contains("Can not approve your own pull request"),
        "{said}"
    );

    // The other shape `errors` comes in.
    let e = write_error(
        422,
        br#"{"message":"Validation Failed","errors":[{"resource":"PullRequestReviewComment","message":"line could not be resolved"}]}"#,
    );
    assert!(format!("{e}").contains("line could not be resolved"));

    // And a 404 on a write is a permission, not a missing pull request.
    let e = format!("{}", write_error(404, b"{}"));
    assert!(e.contains("Read and write"), "{e}");

    assert_eq!(
        format!("{}", write_error(500, b"not json")),
        "GitHub refused this (HTTP 500)."
    );
}

#[test]
fn a_pull_request_without_a_description_reads_as_empty() {
    let raw: RawPr = serde_json::from_str(
        r#"{"number":1,"title":"t","body":null,"state":"open","updated_at":"t",
            "html_url":"u","head":{"ref":"h","sha":"1"},"base":{"ref":"b","sha":"2"}}"#,
    )
    .unwrap();
    assert_eq!(raw.body.unwrap_or_default(), "");
}

#[test]
fn a_head_in_a_fork_is_named_by_the_fork() {
    let raw: RawPr = serde_json::from_str(
        r#"{"number":1,"title":"t","state":"open","updated_at":"t","html_url":"u",
            "head":{"ref":"fix","sha":"1","repo":{"full_name":"someone/Dioxus"}},
            "base":{"ref":"main","sha":"2","repo":{"full_name":"DioxusLabs/dioxus"}}}"#,
    )
    .unwrap();
    let base = RepoRef {
        owner: "DioxusLabs".to_string(),
        name: "dioxus".to_string(),
    };
    let fork = fork_of(&base, raw.head.repo.as_ref()).expect("another owner is a fork");
    assert_eq!(fork.to_string(), "someone/Dioxus");
    // The base's own repository is not a fork of itself, whatever the case.
    assert_eq!(fork_of(&base, raw.base.repo.as_ref()), None);
    let shouted = RawRefRepo {
        full_name: "dioxuslabs/DIOXUS".to_string(),
    };
    assert_eq!(fork_of(&base, Some(&shouted)), None);
    // And a deleted fork leaves nothing to name.
    assert_eq!(fork_of(&base, None), None);

    let mut pr = pr_with(Snapshot::default(), Snapshot::default(), Vec::new());
    pr.head_ref = "fix".to_string();
    assert_eq!(pr.head_label(), "fix");
    pr.head_repo = Some(fork);
    assert_eq!(pr.head_label(), "someone:fix");
}

#[test]
fn a_path_finds_its_own_blob_and_never_a_neighbour() {
    // `-` sorts before `/` as bytes, but a path is compared component by
    // component, where `gguf` sorts before `gguf-rs`. The lookup is a
    // binary search, so it and the sort behind it have to agree about
    // which — and if they ever stop agreeing, the failure is a file served
    // with another file's contents, which nothing downstream can see is
    // wrong. A source file that arrives as a `.gguf` model reads as binary.
    let paths = [
        "README.md",
        "crates/gguf-rs/src/lib.rs",
        "crates/gguf/src/lib.rs",
        "crates/gguf/src/read.rs",
        "crates/gguf/tests/model.gguf",
    ];
    let mut files: Vec<TreeEntry> = paths
        .iter()
        .enumerate()
        .map(|(i, p)| TreeEntry {
            path: PathBuf::from(p),
            sha: format!("{i:040}"),
            size: 10,
        })
        .collect();
    // Exactly what `repo_tree` does with what GitHub sends.
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let snapshot = Snapshot {
        repo: RepoRef::default(),
        commit: "c".into(),
        files,
        dirs: Vec::new(),
        truncated: false,
    };

    for (i, path) in paths.iter().enumerate() {
        let found = snapshot.entry(std::path::Path::new(path)).unwrap();
        assert_eq!(found.path, PathBuf::from(path));
        assert_eq!(found.sha, format!("{i:040}"), "{path} found the wrong blob");
    }
    assert!(
        snapshot
            .entry(std::path::Path::new("crates/gguf/src/nope.rs"))
            .is_none()
    );
}

/// A tree as GitHub sends it: the directories come back with their hashes
/// and the root is stamped without `.pullspace`, while the file list is
/// what it always was.
#[test]
fn a_tree_keeps_its_directory_hashes_and_stamps_the_root() {
    let raw: RawTree = serde_json::from_str(
        r#"{"tree":[
          {"path":".pullspace","mode":"040000","type":"tree","sha":"86c336d99951499a894e80724f2d1a7d78b7edc6"},
          {"path":".pullspace/map","mode":"040000","type":"tree","sha":"1111111111111111111111111111111111111111"},
          {"path":".pullspace/map/index.html","mode":"100644","type":"blob","sha":"2222222222222222222222222222222222222222","size":2},
          {"path":"README.md","mode":"100644","type":"blob","sha":"d00491fd7e5bb6fa28c517a0bb32b8b506539d4d","size":2},
          {"path":"a-b","mode":"040000","type":"tree","sha":"7cafae9f1b71ea42ad1c9db8ef5969f9b4a9f11a"},
          {"path":"a.c","mode":"100644","type":"blob","sha":"4286f428e3b19fe84de503916ce0e7dc8deefea1","size":2},
          {"path":"link","mode":"120000","type":"blob","sha":"42061c01a1c70097d1e4579f29a5adf40abdec95","size":9},
          {"path":"run.sh","mode":"100755","type":"blob","sha":"f5bdd214e01603ecd6c83be9f66d88579c588ec6","size":4},
          {"path":"src","mode":"040000","type":"tree","sha":"f26db84cb30cbba9a8ec0c6fb839c78b1a7f64f8"},
          {"path":"src/a","mode":"040000","type":"tree","sha":"b6e218aa4186392584086d14f778bd0b81311882"},
          {"path":"src.txt","mode":"100644","type":"blob","sha":"718f4d2ff533cf8ead8d3556cf43912bd245fbc4","size":2}
        ],"truncated":false}"#,
    )
    .unwrap();
    let snap = snapshot_of(&RepoRef::default(), "c", raw);
    assert_eq!(snap.files.len(), 6);
    assert_eq!(
        snap.dir_sha(std::path::Path::new("")),
        Some("0d31c502bcaa70cb7e6b7ef80009bd7e09a0e645")
    );
    assert_eq!(
        snap.dir_sha(std::path::Path::new("src/a")),
        Some("b6e218aa4186392584086d14f778bd0b81311882")
    );
    assert_eq!(snap.dir_sha(std::path::Path::new("nope")), None);
}

#[test]
fn file_list_becomes_a_status_map() {
    let files = vec![
        PrFile {
            path: PathBuf::from("a.rs"),
            previous_path: None,
            status: ChangeKind::Added,
        },
        PrFile {
            path: PathBuf::from("b.rs"),
            previous_path: None,
            status: ChangeKind::Deleted,
        },
    ];
    let map = statuses_of(&files);
    assert_eq!(map.get(&PathBuf::from("a.rs")), Some(&ChangeKind::Added));
    assert_eq!(map.get(&PathBuf::from("b.rs")), Some(&ChangeKind::Deleted));
}

// ------------------------------------------------------------ checks

#[test]
fn a_check_that_has_not_finished_is_still_running() {
    for status in ["queued", "in_progress", "waiting", "requested"] {
        let (state, label) = run_state(status, None);
        assert_eq!(state, CheckState::Running, "{status}");
        // The word GitHub used, spelled the way a person would.
        assert!(!label.contains('_'), "{label}");
    }
    assert_eq!(run_state("in_progress", None).1, "in progress");
}

#[test]
fn a_conclusion_is_what_a_finished_check_is_coloured_by() {
    let cases = [
        ("success", CheckState::Passed),
        ("failure", CheckState::Failed),
        ("timed_out", CheckState::Failed),
        // Stopped and waiting to be let through: a red mark on the pull
        // request like any other.
        ("action_required", CheckState::Failed),
        ("skipped", CheckState::Quiet),
        ("cancelled", CheckState::Quiet),
        ("neutral", CheckState::Quiet),
        ("stale", CheckState::Quiet),
    ];
    for (conclusion, want) in cases {
        let (state, label) = run_state("completed", Some(conclusion));
        assert_eq!(state, want, "{conclusion}");
        assert_eq!(label, humanised(conclusion));
    }
    // Finished, and said nothing about how. Not something to call a pass.
    assert_eq!(run_state("completed", None).0, CheckState::Quiet);
}

/// The four states, as the rows they are counted out of.
fn checks_of(states: &[CheckState]) -> Checks {
    Checks {
        sha: "abc1234".to_string(),
        items: states
            .iter()
            .enumerate()
            .map(|(i, state)| Check {
                id: i as u64 + 1,
                name: format!("job {i}"),
                source: String::new(),
                state: *state,
                label: String::new(),
                summary: String::new(),
                report: String::new(),
                html_url: String::new(),
                took: String::new(),
                annotations: 0,
            })
            .collect(),
        truncated: false,
    }
}

#[test]
fn a_failure_outranks_everything_still_running() {
    let checks = checks_of(&[
        CheckState::Passed,
        CheckState::Running,
        CheckState::Failed,
        CheckState::Passed,
    ]);
    assert_eq!(checks.state(), CheckState::Failed);
    let t = checks.tally();
    assert_eq!((t.passed, t.failed, t.running), (2, 1, 1));
    assert_eq!(t.phrase(), "1 failing · 1 running · 2 passed");
}

#[test]
fn a_build_still_going_is_not_one_that_has_passed() {
    let checks = checks_of(&[CheckState::Passed, CheckState::Running]);
    assert_eq!(checks.state(), CheckState::Running);
    assert_eq!(checks.tally().phrase(), "1 running · 1 passed");
}

#[test]
fn nothing_but_skips_is_neither_green_nor_red() {
    let checks = checks_of(&[CheckState::Quiet, CheckState::Quiet]);
    assert_eq!(checks.state(), CheckState::Quiet);
    assert_eq!(checks.tally().phrase(), "2 other");
    // And nothing at all says so rather than counting to zero.
    assert_eq!(checks_of(&[]).tally().phrase(), "nothing has run");
    assert_eq!(checks_of(&[]).state(), CheckState::Quiet);
}

#[test]
fn a_check_run_carries_its_app_its_link_and_its_first_line() {
    let raw: RawCheckRun = serde_json::from_str(
        r#"{
            "name": "test (ubuntu-latest)",
            "status": "completed",
            "conclusion": "failure",
            "started_at": "2024-05-06T07:08:09Z",
            "completed_at": "2024-05-06T07:10:29Z",
            "details_url": "https://ci.example/run/1",
            "html_url": "https://github.com/o/r/runs/1",
            "output": { "title": "", "summary": "3 tests failed\nsee the log" },
            "app": { "name": "GitHub Actions" }
        }"#,
    )
    .unwrap();
    let check = check_of(raw);
    assert_eq!(check.state, CheckState::Failed);
    assert_eq!(check.label, "failure");
    assert_eq!(check.source, "GitHub Actions");
    // The run itself, not the page about it.
    assert_eq!(check.html_url, "https://ci.example/run/1");
    assert_eq!(check.summary, "3 tests failed");
    assert_eq!(check.took, "2m 20s");
}

#[test]
fn a_check_keeps_the_whole_of_what_it_wrote_as_well_as_the_first_line() {
    let raw: RawCheckRun = serde_json::from_str(
        r#"{
            "id": 951133849,
            "name": "coverage",
            "status": "completed",
            "conclusion": "success",
            "output": {
                "title": "Coverage 81%",
                "summary": "| file | % |\n|---|---|\n| a.rs | 81 |",
                "text": "The long form.",
                "annotations_count": 3
            }
        }"#,
    )
    .unwrap();
    let check = check_of(raw);
    assert_eq!(check.id, 951133849);
    // The row shows the line somebody wrote for the purpose…
    assert_eq!(check.summary, "Coverage 81%");
    // …and opening it shows the two halves of the output, as one document.
    assert_eq!(
        check.report,
        "| file | % |\n|---|---|\n| a.rs | 81 |\n\nThe long form."
    );
    assert_eq!(check.annotations, 3);
    assert!(check.has_detail());
}

#[test]
fn a_one_line_output_is_not_shown_twice() {
    let one_liner = |json: &str| check_of(serde_json::from_str::<RawCheckRun>(json).unwrap());
    // Nothing but a summary: it is the row's line, so there is nothing
    // left to unfold.
    let check = one_liner(
        r#"{"name":"links","status":"completed","conclusion":"success",
            "output":{"summary":"No broken links found"}}"#,
    );
    assert_eq!(check.summary, "No broken links found");
    assert_eq!(check.report, "");
    assert!(!check.has_detail());

    // And a check that wrote nothing at all — most of them.
    let bare = one_liner(r#"{"name":"build","status":"completed","conclusion":"success"}"#);
    assert!(!bare.has_detail());
    assert_eq!(bare.summary, "");

    // Marked-up lines are worth opening for on their own, with no report.
    let marked = one_liner(
        r#"{"name":"pylint","status":"completed","conclusion":"failure",
            "output":{"annotations_count":2}}"#,
    );
    assert!(marked.has_detail());
}

#[test]
fn an_annotation_names_a_line_to_go_to() {
    let raw: RawAnnotation = serde_json::from_str(
        r#"{
            "path": "tests/components/portainer/test_event.py",
            "start_line": 7,
            "end_line": 9,
            "annotation_level": "failure",
            "title": "",
            "message": "E0611: No name 'DockerEvent' in module\n",
            "raw_details": "  assert 1 == 2\n"
        }"#,
    )
    .unwrap();
    let a = annotation_of(raw);
    assert_eq!(
        a.path,
        PathBuf::from("tests/components/portainer/test_event.py")
    );
    assert_eq!((a.line, a.end_line), (7, 9));
    assert_eq!(a.level, Level::Failure);
    // Trailing newlines go; the shape inside does not — it is a tool's own
    // layout, and the block is drawn monospaced for exactly that reason.
    assert_eq!(a.message, "E0611: No name 'DockerEvent' in module");
    assert_eq!(a.raw_details, "  assert 1 == 2");
}

#[test]
fn an_annotation_without_a_line_still_lands_somewhere() {
    let bare: RawAnnotation = serde_json::from_str(r#"{"path": ".github"}"#).unwrap();
    let a = annotation_of(bare);
    // Line 0 is not a line. The top of the file is where it points.
    assert_eq!((a.line, a.end_line), (1, 1));
    // And an unknown level is something to read, not something to redden.
    assert_eq!(a.level, Level::Notice);

    for (word, want) in [
        ("failure", Level::Failure),
        ("warning", Level::Warning),
        ("notice", Level::Notice),
    ] {
        let raw: RawAnnotation =
            serde_json::from_str(&format!(r#"{{"path":"a.rs","annotation_level":"{word}"}}"#))
                .unwrap();
        assert_eq!(annotation_of(raw).level, want, "{word}");
    }
}

#[test]
fn an_older_commit_status_reads_as_a_check_like_any_other() {
    let raw: RawStatus = serde_json::from_str(
        r#"{
            "state": "error",
            "context": "ci/circleci: build",
            "description": "Your tests failed on CircleCI",
            "target_url": "https://circleci.example/1",
            "created_at": "2024-05-06T07:08:09Z",
            "updated_at": "2024-05-06T07:08:19Z"
        }"#,
    )
    .unwrap();
    let check = status_check_of(raw);
    assert_eq!(check.state, CheckState::Failed);
    assert_eq!(check.name, "ci/circleci: build");
    assert_eq!(check.summary, "Your tests failed on CircleCI");
    assert_eq!(check.took, "10s");
}

#[test]
fn a_summary_is_one_line_and_not_a_build_log() {
    assert_eq!(one_line("\n\n  hello  \nworld\n"), "hello");
    assert_eq!(one_line(""), "");
    let long = "x".repeat(400);
    let cut = one_line(&long);
    assert_eq!(cut.chars().count(), MAX_SUMMARY + 1);
    assert!(cut.ends_with('…'));
    // Cut by characters, not by bytes: a summary is somebody's prose.
    let wide = "é".repeat(400);
    assert!(one_line(&wide).starts_with('é'));
}

#[test]
fn a_duration_reads_the_way_a_build_log_says_it() {
    let at = |s: &str| epoch_secs(s).unwrap();
    assert_eq!(at("1970-01-01T00:00:00Z"), 0);
    assert_eq!(at("2024-05-06T07:08:09Z"), 1_714_979_289);
    // A fraction of a second, and an offset, are past what this measures.
    assert_eq!(at("2024-05-06T07:08:09.512Z"), at("2024-05-06T07:08:09Z"));
    assert_eq!(at("2024-05-06T07:08:09+00:00"), at("2024-05-06T07:08:09Z"));

    let took_between = |a: &str, b: &str| took(Some(a), Some(b));
    assert_eq!(
        took_between("2024-05-06T07:08:09Z", "2024-05-06T07:08:54Z"),
        "45s"
    );
    assert_eq!(
        took_between("2024-05-06T07:08:09Z", "2024-05-06T07:09:29Z"),
        "1m 20s"
    );
    assert_eq!(
        took_between("2024-05-06T07:08:09Z", "2024-05-06T08:10:09Z"),
        "1h 2m"
    );
    // One end missing, both ends nonsense, or two clocks disagreeing: a
    // line that says a little less, not a row that fails to draw.
    assert_eq!(took(Some("2024-05-06T07:08:09Z"), None), "");
    assert_eq!(took(Some("yesterday"), Some("today")), "");
    assert_eq!(
        took_between("2024-05-06T07:08:09Z", "2024-05-06T07:08:08Z"),
        ""
    );
}
