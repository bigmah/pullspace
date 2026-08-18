//! What is open, said in the address bar — and read back out of it.
//!
//! `#/owner/repo/pull/123`, `#/owner/repo/commit/<sha>` for one commit of it,
//! `#/owner/repo` for a repository being read on its own,
//! `#/owner/repo/tree/<branch>` for one of its branches and
//! `#/owner/repo/compare/<base>...<head>` for two of them held up against each
//! other — the five words github.com writes the same five things under. Behind
//! the `#` on purpose:
//! this is a static page, and the fragment
//! is the one part of a URL no host ever sees, so a deep link works on GitHub
//! Pages, on a bare directory, and under any `base_path` — with nothing to
//! configure and no server to teach about routes.
//!
//! A file, and a line of it, are written after that:
//!
//! ```text
//! #/rust-lang/rust/pull/123/files/src/main.rs:L42
//! #/rust-lang/rust/blob/src/main.rs:L42
//! ```
//!
//! `files` and `blob` are the words github.com uses for the same two things,
//! and the line is `:L42` rather than `#L42` because there is only one `#` in a
//! URL and the route is already living in it.
//!
//! It is the same three things a link is for whichever form it takes: a review
//! that can be sent to somebody, a line that can be pointed at, and a tab that
//! comes back to where it was on reload.

use std::path::{Path, PathBuf};

use super::github::{RepoRef, encode_segment, parse_commit_target, parse_target, strip_host};

/// Which pull request, commit or repository is open.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub enum Target {
    /// Nothing open — the picker.
    #[default]
    Home,
    /// A repository being read on its own, at its default branch.
    Repo(RepoRef),
    /// The same, at a branch somebody named — which is the one thing a link
    /// with no branch in it cannot come back to.
    Branch(RepoRef, String),
    /// Two refs of it, held up against each other: base first, as github.com
    /// writes a comparison and as everybody reads one.
    Compare(RepoRef, String, String),
    Pr(RepoRef, u64),
    /// One commit, diffed against the one before it.
    Commit(RepoRef, String),
}

impl Target {
    /// The word that separates the repository from the path inside it, as
    /// github.com writes it. `None` for home, which has no inside.
    fn marker(&self) -> Option<&'static str> {
        match self {
            Target::Home => None,
            Target::Repo(_) | Target::Branch(..) => Some("blob"),
            Target::Pr(..) | Target::Commit(..) | Target::Compare(..) => Some("files"),
        }
    }
}

/// Somewhere inside it: the file being read, and the line a link was made at.
///
/// The line is what turns "here is the pull request" into "here is the line I
/// am talking about", which is most of what a review is spent saying.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Place {
    /// Relative to the root of the repository.
    pub path: PathBuf,
    pub line: Option<usize>,
}

/// Where the app is, as a link.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Route {
    pub at: Target,
    pub place: Option<Place>,
}

impl Route {
    /// Nothing open.
    pub fn home() -> Self {
        Route::default()
    }

    /// Something open, with no file named inside it.
    pub fn to(at: Target) -> Self {
        Route { at, place: None }
    }

    /// The fragment this route is written as, `#` and all.
    pub fn hash(&self) -> String {
        let mut out = match &self.at {
            Target::Home => "#/".to_string(),
            Target::Repo(repo) => format!("#/{}/{}", repo.owner, repo.name),
            Target::Branch(repo, branch) => format!(
                "#/{}/{}/tree/{}",
                repo.owner,
                repo.name,
                encoded_ref(branch)
            ),
            Target::Compare(repo, base, head) => format!(
                "#/{}/{}/compare/{}...{}",
                repo.owner,
                repo.name,
                encoded_ref(base),
                encoded_ref(head),
            ),
            Target::Pr(repo, number) => {
                format!("#/{}/{}/pull/{number}", repo.owner, repo.name)
            }
            Target::Commit(repo, sha) => {
                format!("#/{}/{}/commit/{sha}", repo.owner, repo.name)
            }
        };
        if let Some(marker) = self.at.marker()
            && let Some(place) = &self.place
        {
            out.push('/');
            out.push_str(marker);
            out.push('/');
            out.push_str(&encoded(&place.path));
            if let Some(line) = place.line {
                out.push_str(&format!(":L{line}"));
            }
        }
        out
    }
}

/// Read a fragment, with or without its `#`.
///
/// It goes through the same parser the picker uses for anything pasted into it,
/// so a whole `https://github.com/owner/repo/pull/1` dropped after the `#` is
/// read as what it obviously means. Anything that names no repository is
/// [`Target::Home`] — a link nobody can open is not worth an error.
pub fn parse(hash: &str) -> Route {
    let text = hash.trim().trim_start_matches('#');
    let (head, place) = split(text);
    // A commit first: `owner/repo/commit/<sha>` is a repository with something
    // after it as far as `parse_target` is concerned, so asking it first would
    // answer with the repository every time.
    let at = match parse_commit_target(&head) {
        Some((repo, sha)) => Target::Commit(repo, sha),
        // And a comparison and a branch before a bare repository, for the same
        // reason: `parse_target` reads all three as a repository with something
        // after it.
        None => match compare_target(&head) {
            Some((repo, base, head)) => Target::Compare(repo, base, head),
            None => match branch_target(&head) {
                Some((repo, branch)) => Target::Branch(repo, branch),
                None => match parse_target(&head) {
                    Some((repo, Some(number))) => Target::Pr(repo, number),
                    Some((repo, None)) => Target::Repo(repo),
                    None => Target::Home,
                },
            },
        },
    };
    // A file named under nothing is a file nobody can open.
    let place = place.filter(|_| at != Target::Home);
    Route { at, place }
}

/// Split the repository off the path inside it.
///
/// Segment by segment, and only where the marker actually falls — a repository
/// called `files` is a repository, not a separator, and `owner/files/pull/1`
/// has to keep parsing as the pull request it is.
fn split(text: &str) -> (String, Option<Place>) {
    let rest = strip_host(text).unwrap_or(text);
    let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
    let cut = match parts.as_slice() {
        [_, _, "pull" | "pulls", _, "files", ..] => 5,
        [_, _, "commit" | "commits", _, "files", ..] => 5,
        // A branch may have slashes in it, so the marker after one is wherever
        // it falls rather than at a fixed depth — but never before the branch
        // itself, so a branch actually called `blob` still names a branch.
        [_, _, "tree", _, ..] => match parts.iter().skip(4).position(|p| *p == "blob") {
            Some(at) => at + 5,
            None => return (rest.to_string(), None),
        },
        // And the same for a comparison, whose two names may each have slashes
        // in them.
        [_, _, "compare", _, ..] => match parts.iter().skip(4).position(|p| *p == "files") {
            Some(at) => at + 5,
            None => return (rest.to_string(), None),
        },
        [_, _, "blob", ..] => 3,
        _ => return (rest.to_string(), None),
    };
    // Everything up to the marker is the repository; everything after it is
    // the way to the file.
    (
        parts[..cut - 1].join("/"),
        place_of(&parts[cut..].join("/")),
    )
}

/// `src/main.rs:L42` — and `src/main.rs`, which is the same place with no line
/// picked out in it.
fn place_of(tail: &str) -> Option<Place> {
    if tail.is_empty() {
        return None;
    }
    let (text, line) = match tail.rsplit_once(":L") {
        Some((path, number)) if !path.is_empty() => match number.parse::<usize>() {
            Ok(n) if n > 0 => (path, Some(n)),
            // `:L` and then something that is not a line: part of the name.
            _ => (tail, None),
        },
        _ => (tail, None),
    };
    let mut path = PathBuf::new();
    for seg in text.split('/') {
        match seg {
            // A link that climbs out of the repository names nothing in it.
            "" | "." => {}
            ".." => return None,
            other => path.push(decoded(other)),
        }
    }
    (!path.as_os_str().is_empty()).then_some(Place { path, line })
}

/// The branch one piece of text names, when it names one: `owner/repo` and
/// whatever follows the word github.com writes a branch under.
///
/// Everything after `tree/` is the branch, joined back up with the slashes it
/// was written with — `feat/thing` is one branch, not a branch and a path.
/// Each segment is unescaped on the way, so a name written the way [`Route`]
/// writes it and one pasted out of a browser both come back as themselves.
///
/// Checked before [`parse_target`] wherever both could answer, since that reads
/// the same text as a bare repository with something after it.
pub fn branch_target(input: &str) -> Option<(RepoRef, String)> {
    let text = input.trim();
    let rest = strip_host(text).unwrap_or(text);
    let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
    let [owner, name, "tree", branch @ ..] = parts.as_slice() else {
        return None;
    };
    if branch.is_empty() {
        return None;
    }
    Some((
        RepoRef {
            owner: owner.to_string(),
            name: name.trim_end_matches(".git").to_string(),
        },
        branch
            .iter()
            .map(|s| decoded(s))
            .collect::<Vec<_>>()
            .join("/"),
    ))
}

/// The comparison one piece of text names, when it names one: `owner/repo`, and
/// two refs with three dots between them.
///
/// Everything after `compare/` is the pair, and the dots are what separates
/// them — git forbids `..` inside a ref name, so the separator cannot be part
/// of either however the two are called. Each side is then a path's worth of
/// segments, unescaped the way [`branch_target`] unescapes one.
///
/// GitHub's own `owner:ref` form survives untouched, which is what a link to a
/// comparison across forks arrives as — it is passed straight back to the API,
/// which reads it.
pub fn compare_target(input: &str) -> Option<(RepoRef, String, String)> {
    let text = input.trim();
    let rest = strip_host(text).unwrap_or(text);
    let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
    let [owner, name, "compare", spec @ ..] = parts.as_slice() else {
        return None;
    };
    let spec = spec
        .iter()
        .map(|s| decoded(s))
        .collect::<Vec<_>>()
        .join("/");
    let (base, head) = spec.split_once("...")?;
    // A comparison needs both halves; one of them alone names nothing.
    if base.is_empty() || head.is_empty() {
        return None;
    }
    Some((
        RepoRef {
            owner: owner.to_string(),
            name: name.trim_end_matches(".git").to_string(),
        },
        base.to_string(),
        head.to_string(),
    ))
}

/// A branch as a link: segment by segment, like the path it is written as on
/// github.com, so `feat/thing` stays two segments and everything else in the
/// name is escaped.
fn encoded_ref(name: &str) -> String {
    name.split('/')
        .filter(|s| !s.is_empty())
        .map(encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

/// A whole path, segment by segment — the separators are the one thing not
/// escaped, since they are what makes it a path. [`encode_segment`] sends
/// everything outside the unreserved set to `%XX`, including the two that
/// would otherwise be read as structure: `%` itself, and the `:` this module
/// separates a line number with.
fn encoded(path: &Path) -> String {
    path.to_string_lossy()
        .split('/')
        .filter(|s| !s.is_empty())
        .map(encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

/// `docs/getting%20started.md` is a path with a space in it. Only the escapes
/// are undone — everything else in the segment is already the name it is.
pub fn decoded(seg: &str) -> String {
    if !seg.contains('%') {
        return seg.to_string();
    }
    let bytes = seg.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = (i + 2 < bytes.len())
            .then(|| std::str::from_utf8(&bytes[i + 1..i + 3]).ok())
            .flatten()
            .filter(|_| bytes[i] == b'%')
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn location() -> Option<web_sys::Location> {
    Some(web_sys::window()?.location())
}

/// What the address bar says now.
pub fn current() -> Route {
    location()
        .and_then(|l| l.hash().ok())
        .map(|hash| parse(&hash))
        .unwrap_or_default()
}

/// The whole address, as something to paste somewhere else.
pub fn href() -> Option<String> {
    location().and_then(|l| l.href().ok())
}

/// Put what is open in the address bar, as a new history entry — so Back goes
/// to whatever was open before it, which is what a browser's Back means
/// everywhere else.
///
/// Nothing is written when the bar already names this route, which is what
/// keeps the two from chasing each other: every write here comes back as a
/// `hashchange`, and a route that is already on screen is nothing to go and
/// open. It compares routes rather than text, so a link typed by hand — a
/// trailing slash, a pasted github.com URL — is left as the reader wrote it.
pub fn show(route: &Route) {
    if current() == *route {
        return;
    }
    if let Some(l) = location() {
        let _ = l.set_hash(&route.hash());
    }
}

/// The same, without a history entry — for moving *within* what is already
/// open.
///
/// Reading four files of a pull request is one place visited, not four: Back
/// belongs to the pull requests somebody opened, and a browser that needs
/// eleven presses to leave one of them is a browser nobody presses Back in.
/// This also raises no `hashchange`, so the watcher never sees our own writing.
pub fn replace(route: &Route) {
    if current() == *route {
        return;
    }
    if let Some(win) = web_sys::window()
        && let Ok(history) = win.history()
    {
        let _ =
            history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&route.hash()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(owner: &str, name: &str) -> RepoRef {
        RepoRef {
            owner: owner.to_string(),
            name: name.to_string(),
        }
    }

    fn place(path: &str, line: Option<usize>) -> Option<Place> {
        Some(Place {
            path: PathBuf::from(path),
            line,
        })
    }

    #[test]
    fn a_pull_request_round_trips() {
        let route = Route::to(Target::Pr(repo("rust-lang", "rust"), 12345));
        assert_eq!(route.hash(), "#/rust-lang/rust/pull/12345");
        assert_eq!(parse(&route.hash()), route);
    }

    #[test]
    fn a_repository_round_trips() {
        let route = Route::to(Target::Repo(repo("DioxusLabs", "dioxus")));
        assert_eq!(route.hash(), "#/DioxusLabs/dioxus");
        assert_eq!(parse(&route.hash()), route);
    }

    #[test]
    fn a_branch_round_trips() {
        let route = Route::to(Target::Branch(
            repo("DioxusLabs", "dioxus"),
            "main".to_string(),
        ));
        assert_eq!(route.hash(), "#/DioxusLabs/dioxus/tree/main");
        assert_eq!(parse(&route.hash()), route);

        // And a file of one, which is what a link into a branch is usually
        // made to point at.
        let route = Route {
            at: Target::Branch(repo("o", "r"), "dev".to_string()),
            place: place("src/main.rs", Some(42)),
        };
        assert_eq!(route.hash(), "#/o/r/tree/dev/blob/src/main.rs:L42");
        assert_eq!(parse(&route.hash()), route);
    }

    /// Branches are named `release/1.2` as often as not, and the slash in one
    /// is part of the name rather than a step down into anything.
    #[test]
    fn a_branch_with_a_slash_in_it_round_trips() {
        let route = Route {
            at: Target::Branch(repo("o", "r"), "feat/branch-list".to_string()),
            place: place("src/ui/app.rs", None),
        };
        assert_eq!(
            route.hash(),
            "#/o/r/tree/feat/branch-list/blob/src/ui/app.rs"
        );
        assert_eq!(parse(&route.hash()), route);

        // Written out in full, with no file after it.
        let route = Route::to(Target::Branch(repo("o", "r"), "a/b/c".to_string()));
        assert_eq!(route.hash(), "#/o/r/tree/a/b/c");
        assert_eq!(parse(&route.hash()), route);
    }

    #[test]
    fn a_comparison_round_trips() {
        let route = Route::to(Target::Compare(
            repo("o", "r"),
            "main".to_string(),
            "feat/thing".to_string(),
        ));
        assert_eq!(route.hash(), "#/o/r/compare/main...feat/thing");
        assert_eq!(parse(&route.hash()), route);

        // And a line of a file inside one.
        let route = Route {
            at: Target::Compare(repo("o", "r"), "v1.0".to_string(), "main".to_string()),
            place: place("src/main.rs", Some(9)),
        };
        assert_eq!(
            route.hash(),
            "#/o/r/compare/v1.0...main/files/src/main.rs:L9"
        );
        assert_eq!(parse(&route.hash()), route);
    }

    /// Both names can have slashes in them, and the three dots are the only
    /// thing that separates the two — git forbids `..` inside a ref name, which
    /// is what makes that safe.
    #[test]
    fn a_comparison_of_two_slashed_branches_round_trips() {
        let route = Route {
            at: Target::Compare(
                repo("o", "r"),
                "release/1.2".to_string(),
                "feat/deep/work".to_string(),
            ),
            place: place("a.rs", None),
        };
        assert_eq!(
            route.hash(),
            "#/o/r/compare/release/1.2...feat/deep/work/files/a.rs"
        );
        assert_eq!(parse(&route.hash()), route);
    }

    /// github.com's own compare URL, including the `owner:ref` form its
    /// permalinks use.
    #[test]
    fn a_compare_link_pasted_after_the_hash_is_read_as_one() {
        assert_eq!(
            parse("#https://github.com/o/r/compare/main...dev"),
            Route::to(Target::Compare(
                repo("o", "r"),
                "main".to_string(),
                "dev".to_string()
            ))
        );
        assert_eq!(
            parse("#/o/r/compare/bigmah:ecbee88...bigmah:2740204"),
            Route::to(Target::Compare(
                repo("o", "r"),
                "bigmah:ecbee88".to_string(),
                "bigmah:2740204".to_string()
            ))
        );
    }

    /// Half a comparison names nothing to compare.
    #[test]
    fn a_comparison_missing_a_side_is_just_the_repository() {
        for text in [
            "#/o/r/compare/main",
            "#/o/r/compare/...dev",
            "#/o/r/compare/",
        ] {
            assert_eq!(
                parse(text),
                Route::to(Target::Repo(repo("o", "r"))),
                "{text}"
            );
        }
    }

    /// github.com's own branch URL, which is the one anybody has to hand.
    #[test]
    fn a_branch_link_pasted_after_the_hash_is_read_as_one() {
        assert_eq!(
            parse("#https://github.com/rust-lang/rust/tree/master"),
            Route::to(Target::Branch(
                repo("rust-lang", "rust"),
                "master".to_string()
            ))
        );
        // And `tree` with nothing after it is just the repository.
        assert_eq!(
            parse("#/rust-lang/rust/tree/"),
            Route::to(Target::Repo(repo("rust-lang", "rust")))
        );
    }

    /// The marker is only a marker after the branch, so a branch that shares
    /// its name is still a branch.
    #[test]
    fn a_branch_called_blob_is_still_a_branch() {
        assert_eq!(
            parse("#/o/r/tree/blob"),
            Route::to(Target::Branch(repo("o", "r"), "blob".to_string()))
        );
        assert_eq!(
            parse("#/o/r/tree/blob/blob/a.rs"),
            Route {
                at: Target::Branch(repo("o", "r"), "blob".to_string()),
                place: place("a.rs", None),
            }
        );
    }

    #[test]
    fn a_line_of_a_pull_request_round_trips() {
        let route = Route {
            at: Target::Pr(repo("rust-lang", "rust"), 7),
            place: place("src/ui/app.rs", Some(42)),
        };
        assert_eq!(
            route.hash(),
            "#/rust-lang/rust/pull/7/files/src/ui/app.rs:L42"
        );
        assert_eq!(parse(&route.hash()), route);
    }

    #[test]
    fn a_commit_round_trips() {
        let sha = "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0";
        let route = Route::to(Target::Commit(repo("rust-lang", "rust"), sha.to_string()));
        assert_eq!(route.hash(), format!("#/rust-lang/rust/commit/{sha}"));
        assert_eq!(parse(&route.hash()), route);

        // And a line of a file inside one, which is what a link to a commit is
        // usually made to point at.
        let route = Route {
            at: Target::Commit(repo("o", "r"), "abc1234".to_string()),
            place: place("src/main.rs", Some(42)),
        };
        assert_eq!(route.hash(), "#/o/r/commit/abc1234/files/src/main.rs:L42");
        assert_eq!(parse(&route.hash()), route);
    }

    /// github.com's own commit URL, which is the one anybody has to hand.
    #[test]
    fn a_commit_link_pasted_after_the_hash_is_read_as_one() {
        assert_eq!(
            parse("#https://github.com/o/r/commit/abc1234"),
            Route::to(Target::Commit(repo("o", "r"), "abc1234".to_string()))
        );
    }

    /// Only hex, and only enough of it — anything else after `commit/` is a
    /// repository being read with a word after it, not a commit.
    #[test]
    fn something_that_is_not_a_sha_is_not_a_commit() {
        for text in [
            "#/o/r/commit/main",
            "#/o/r/commit/abc",
            "#/o/r/commit/",
            "#/o/r/commit/zzzzzzz",
        ] {
            assert_eq!(
                parse(text),
                Route::to(Target::Repo(repo("o", "r"))),
                "{text}"
            );
        }
    }

    #[test]
    fn a_file_of_a_repository_round_trips() {
        let route = Route {
            at: Target::Repo(repo("rust-lang", "rust")),
            place: place("README.md", None),
        };
        assert_eq!(route.hash(), "#/rust-lang/rust/blob/README.md");
        assert_eq!(parse(&route.hash()), route);
    }

    #[test]
    fn a_path_with_something_in_it_that_a_url_cannot_hold() {
        let route = Route {
            at: Target::Repo(repo("o", "r")),
            place: place("docs/getting started #1.md", Some(3)),
        };
        assert_eq!(
            route.hash(),
            "#/o/r/blob/docs/getting%20started%20%231.md:L3"
        );
        assert_eq!(parse(&route.hash()), route);
    }

    /// The separator is only a separator where it is written to be one.
    #[test]
    fn a_repository_called_files_is_still_a_repository() {
        assert_eq!(
            parse("#/torvalds/files/pull/9"),
            Route::to(Target::Pr(repo("torvalds", "files"), 9))
        );
        assert_eq!(
            parse("#/torvalds/blob"),
            Route::to(Target::Repo(repo("torvalds", "blob")))
        );
        assert_eq!(
            parse("#/torvalds/tree/pull/9"),
            Route::to(Target::Pr(repo("torvalds", "tree"), 9))
        );
    }

    #[test]
    fn a_colon_in_a_name_is_not_a_line_number() {
        // Written by us it is escaped, so this is a hand-typed link.
        let route = parse("#/o/r/blob/weird:name.rs");
        assert_eq!(route.place, place("weird:name.rs", None));
        let route = parse("#/o/r/blob/notes:Lx.md");
        assert_eq!(route.place, place("notes:Lx.md", None));
    }

    #[test]
    fn home_is_what_names_nothing() {
        assert_eq!(Route::home().hash(), "#/");
        for empty in ["", "#", "#/", "  ", "#/torvalds"] {
            assert_eq!(parse(empty), Route::home(), "{empty:?}");
        }
    }

    #[test]
    fn a_link_pasted_after_the_hash_is_read_as_one() {
        assert_eq!(
            parse("#https://github.com/rust-lang/rust/pull/7"),
            Route::to(Target::Pr(repo("rust-lang", "rust"), 7))
        );
        // And a trailing slash is not a third path segment.
        assert_eq!(
            parse("#/rust-lang/rust/"),
            Route::to(Target::Repo(repo("rust-lang", "rust")))
        );
        // github.com's own file link, which is the one people have to hand.
        assert_eq!(
            parse("#https://github.com/rust-lang/rust/pull/7/files/src/main.rs:L2"),
            Route {
                at: Target::Pr(repo("rust-lang", "rust"), 7),
                place: place("src/main.rs", Some(2)),
            }
        );
    }

    #[test]
    fn a_pull_number_that_is_not_a_number_is_still_the_repository() {
        assert_eq!(
            parse("#/rust-lang/rust/pull/abc"),
            Route::to(Target::Repo(repo("rust-lang", "rust")))
        );
    }

    #[test]
    fn a_path_that_climbs_out_of_the_repository_names_nothing() {
        assert_eq!(parse("#/o/r/blob/../../etc/passwd").place, None);
        assert_eq!(parse("#/o/r/blob/").place, None);
        assert_eq!(parse("#/o/r/files/src/main.rs").place, None);
    }

    #[test]
    fn line_zero_is_not_a_line() {
        assert_eq!(parse("#/o/r/blob/a.rs:L0").place, place("a.rs:L0", None));
    }

    #[test]
    fn percent_escapes_are_undone_and_the_rest_is_left_alone() {
        assert_eq!(decoded("plain.md"), "plain.md");
        assert_eq!(decoded("a%20b"), "a b");
        assert_eq!(decoded("100%"), "100%");
        assert_eq!(decoded("%zz"), "%zz");
    }
}
