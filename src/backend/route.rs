//! What is open, said in the address bar — and read back out of it.
//!
//! `#/owner/repo/pull/123`, and `#/owner/repo` for a repository being read on
//! its own. Behind the `#` on purpose: this is a static page, and the fragment
//! is the one part of a URL no host ever sees, so a deep link works on GitHub
//! Pages, on a bare directory, and under any `base_path` — with nothing to
//! configure and no server to teach about routes.
//!
//! It is the same two things a link is for either way: a review that can be
//! sent to somebody, and a tab that comes back to where it was on reload.

use super::github::{RepoRef, parse_target};

/// Where the app is, as a link.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub enum Route {
    /// Nothing open — the picker.
    #[default]
    Home,
    /// A repository being read on its own, at its default branch.
    Repo(RepoRef),
    Pr(RepoRef, u64),
}

impl Route {
    /// The fragment this route is written as, `#` and all.
    pub fn hash(&self) -> String {
        match self {
            Route::Home => "#/".to_string(),
            Route::Repo(repo) => format!("#/{}/{}", repo.owner, repo.name),
            Route::Pr(repo, number) => format!("#/{}/{}/pull/{number}", repo.owner, repo.name),
        }
    }
}

/// Read a fragment, with or without its `#`.
///
/// It goes through the same parser the picker uses for anything pasted into it,
/// so a whole `https://github.com/owner/repo/pull/1` dropped after the `#` is
/// read as what it obviously means. Anything that names no repository is
/// [`Route::Home`] — a link nobody can open is not worth an error.
pub fn parse(hash: &str) -> Route {
    let text = hash.trim().trim_start_matches('#');
    match parse_target(text) {
        Some((repo, Some(number))) => Route::Pr(repo, number),
        Some((repo, None)) => Route::Repo(repo),
        None => Route::Home,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(owner: &str, name: &str) -> RepoRef {
        RepoRef {
            owner: owner.to_string(),
            name: name.to_string(),
        }
    }

    #[test]
    fn a_pull_request_round_trips() {
        let route = Route::Pr(repo("rust-lang", "rust"), 12345);
        assert_eq!(route.hash(), "#/rust-lang/rust/pull/12345");
        assert_eq!(parse(&route.hash()), route);
    }

    #[test]
    fn a_repository_round_trips() {
        let route = Route::Repo(repo("DioxusLabs", "dioxus"));
        assert_eq!(route.hash(), "#/DioxusLabs/dioxus");
        assert_eq!(parse(&route.hash()), route);
    }

    #[test]
    fn home_is_what_names_nothing() {
        assert_eq!(Route::Home.hash(), "#/");
        for empty in ["", "#", "#/", "  ", "#/torvalds"] {
            assert_eq!(parse(empty), Route::Home, "{empty:?}");
        }
    }

    #[test]
    fn a_link_pasted_after_the_hash_is_read_as_one() {
        assert_eq!(
            parse("#https://github.com/rust-lang/rust/pull/7"),
            Route::Pr(repo("rust-lang", "rust"), 7)
        );
        // And a trailing slash is not a third path segment.
        assert_eq!(
            parse("#/rust-lang/rust/"),
            Route::Repo(repo("rust-lang", "rust"))
        );
    }

    #[test]
    fn a_pull_number_that_is_not_a_number_is_still_the_repository() {
        assert_eq!(
            parse("#/rust-lang/rust/pull/abc"),
            Route::Repo(repo("rust-lang", "rust"))
        );
    }
}
