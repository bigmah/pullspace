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
//!
//! Links arrive from github.com as well as from here, and github.com writes
//! two of these six differently enough to need their own reading: its
//! `blob/main/src/main.rs` puts the branch where ours puts the path. That is
//! [`github_link`], which is checked first wherever text could be either — the
//! host on the front is what tells them apart, and it is why [`strip_host`]
//! answers with an `Option`. [`take_handoff`] is the same thing arriving in a
//! query field instead, which is how something outside the page — a browser
//! extension, a bookmarklet — says "open this".

use std::path::{Path, PathBuf};

use super::github::{
    RepoRef, encode_segment, is_sha, parse_commit_target, parse_target, strip_host,
};

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
    /// The repository this is inside, which everything but home is inside one
    /// of.
    pub fn repo(&self) -> Option<&RepoRef> {
        match self {
            Target::Home => None,
            Target::Repo(repo)
            | Target::Branch(repo, _)
            | Target::Compare(repo, ..)
            | Target::Pr(repo, _)
            | Target::Commit(repo, _) => Some(repo),
        }
    }

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

// ------------------------------------------------------------- github.com

/// The query field a link is handed over in: `?url=…`.
///
/// See [`take_handoff`].
const HANDOFF: &str = "url";

/// What a github.com URL names.
///
/// The address bar of whatever browser somebody is reading GitHub in, handed
/// over whole — a link pasted into the picker, dropped in after our own `#`,
/// or passed by an extension with the repository page still on screen.
///
/// github.com writes four of the six things this app opens exactly as it does,
/// once the host is off the front. The other two it writes differently enough
/// that reading them takes a question only the repository can answer.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Link {
    /// A pull request, a commit, a comparison, a repository: already a route.
    At(Route),
    /// `blob/…` or `tree/…`, which are not.
    ///
    /// Those write the ref and the path inside it with one separator and no
    /// mark between them, so `main/src/main.rs` is the branch `main` holding
    /// `src/main.rs` — or the branch `main/src` holding `main.rs`, and nothing
    /// in the URL says which. github.com knows its own branches and this page
    /// does not, so the pair travels un-split as far as whatever can ask: see
    /// [`longest_ref`].
    Ref {
        repo: RepoRef,
        /// Everything after the word, unescaped — `main/src/main.rs`.
        rest: String,
        /// `#L58`, which github.com writes in a fragment of its own.
        line: Option<usize>,
    },
}

/// Read a github.com URL, in github.com's own grammar.
///
/// `None` for anything that is not one, which is what keeps this from touching
/// the links this app writes: `#/owner/repo/blob/src/main.rs` has the path
/// where github.com puts the branch, and only the host on the front tells the
/// two apart. That is the same signal [`strip_host`] exists to give, and the
/// reason it answers with an `Option`.
///
/// Everything github.com has no answer for here — issues, actions, settings —
/// comes back as the repository, which is the part of such a page this app can
/// open. A URL naming no repository at all is `None`, so an account page is
/// still left to the picker's own search.
pub fn github_link(input: &str) -> Option<Link> {
    let text = input.trim();
    // The fragment first and the query second, in the order a URL writes them:
    // `#L58` is the line being linked to and `?plain=1` is the view it was
    // being read in, and neither is part of the path. A `#` inside a filename
    // reaches us as `%23`, so cutting at the first one cuts in the right place.
    let (head, frag) = match text.split_once('#') {
        Some((head, frag)) => (head, frag),
        None => (text, ""),
    };
    let head = head.split_once('?').map_or(head, |(path, _)| path);
    let rest = strip_host(head)?;
    let line = anchor_line(frag);

    let parts: Vec<String> = rest
        .split('/')
        .filter(|p| !p.is_empty())
        .map(decoded)
        .collect();
    let [owner, name, tail @ ..] = parts.as_slice() else {
        return None;
    };
    let name = name.trim_end_matches(".git");
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    let repo = RepoRef {
        owner: owner.clone(),
        name: name.to_string(),
    };
    let tail: Vec<&str> = tail.iter().map(String::as_str).collect();
    let joined = |rest: &[&str]| {
        Some(Link::Ref {
            repo: repo.clone(),
            rest: rest.join("/"),
            line,
        })
    };

    match tail.as_slice() {
        // The two github.com writes differently. `blob`, `blame` and `raw` are
        // three ways of looking at one file and `tree` is the directory it sits
        // in; all four put a ref where our own links put the path.
        ["blob" | "blame" | "raw" | "tree", rest @ ..] if !rest.is_empty() => joined(rest),
        // `commits/<ref>` is a branch's history, which is the branch — and the
        // same ref-or-path question. `commits/<sha>` is a commit, and falls
        // through with the rest.
        ["commits", rest @ ..] if !rest.is_empty() && !is_sha(rest[0]) => joined(rest),
        // One commit of a pull request is that commit. The pull request it was
        // being read inside of is not something this app's links can hold, and
        // the commit is what was linked to.
        ["pull" | "pulls", _, "commits", sha, ..] if is_sha(sha) => {
            Some(Link::At(Route::to(Target::Commit(repo, sha.to_string()))))
        }
        // And everything else github.com writes as this app does, once the host
        // is off the front: a pull request on whichever of its tabs, a commit,
        // a comparison — `owner:ref` and all — and, for anything it has a page
        // for that this app does not, the repository that page is of.
        //
        // Read by `parse`, which is where that grammar already lives rather
        // than a second copy of it here.
        _ => {
            let route = parse(rest);
            (route.at != Target::Home).then_some(Link::At(route))
        }
    }
}

/// The line a github.com anchor names.
///
/// `#L58`, and the `#L58-L72` and `#L58C5-L72C13` a selection is written as —
/// the first line of one, since that is where a reader wants to be put down.
/// Anything else github.com anchors with, `#diff-…` and `#issuecomment-…`
/// among them, names no line.
fn anchor_line(frag: &str) -> Option<usize> {
    let digits = frag.trim().trim_start_matches('#').strip_prefix('L')?;
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    match digits[..end].parse::<usize>() {
        Ok(line) if line > 0 => Some(line),
        _ => None,
    }
}

/// The ref `rest` begins with, out of the names that could be it: the longest
/// one that is the whole front of it, up to a separator.
///
/// `main/src/main.rs` in a repository with both `main` and `main/src` is the
/// second — the longer name accounts for more of what was written, and where
/// two branches could both be meant the one that reaches further into the URL
/// is the one whose page it came off.
///
/// `None` when nothing matches, which is what a link to a tag looks like: tags
/// are not branches, and no list of branches will ever hold one.
pub fn longest_ref<S: AsRef<str>>(rest: &str, names: &[S]) -> Option<String> {
    names
        .iter()
        .map(AsRef::as_ref)
        .filter(|name| {
            !name.is_empty()
                && (rest == *name || rest.strip_prefix(*name).is_some_and(|r| r.starts_with('/')))
        })
        .max_by_key(|name| name.len())
        .map(str::to_string)
}

/// The route a github.com `blob`/`tree` tail names, once its ref is known.
///
/// Whatever `name` does not account for is the path inside it, and an empty
/// remainder is the branch on its own — `tree/main` names no file.
pub fn ref_route(repo: RepoRef, rest: &str, name: &str, line: Option<usize>) -> Route {
    let tail = rest
        .strip_prefix(name)
        .unwrap_or_default()
        .trim_start_matches('/');
    Route {
        at: Target::Branch(repo, name.to_string()),
        place: path_of(tail).map(|path| Place { path, line }),
    }
}

/// A path inside the repository, out of the segments github.com wrote it as.
///
/// No `:L42` to pick off the end the way [`place_of`] does: github.com writes
/// the line in a fragment, so everything here is name.
fn path_of(tail: &str) -> Option<PathBuf> {
    let mut path = PathBuf::new();
    for seg in tail.split('/') {
        match seg {
            // A link that climbs out of the repository names nothing in it.
            "" | "." => {}
            ".." => return None,
            other => path.push(other),
        }
    }
    (!path.as_os_str().is_empty()).then_some(path)
}

/// One field of a query string, decoded.
///
/// Form encoding, which is what a query string is: `+` is a space and the rest
/// is `%XX`. Written out here rather than handed to `URLSearchParams` so that
/// it can be tested off a browser, like everything else in this module.
pub fn param(query: &str, name: &str) -> Option<String> {
    query
        .trim_start_matches('?')
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == name)
        .map(|(_, value)| decoded(&value.replace('+', " ")))
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

/// The fragment as written, before anything has been made of it.
///
/// What arrives there is not always one of our own links — a github.com URL
/// dropped in after the `#` is read as what it obviously means, and telling
/// which of the two it is takes [`github_link`] rather than [`parse`].
pub fn fragment() -> String {
    location().and_then(|l| l.hash().ok()).unwrap_or_default()
}

/// The link something outside the page handed over, out of the query string.
///
/// ```text
/// …/?url=https%3A%2F%2Fgithub.com%2Fo%2Fr%2Fblob%2Fmain%2Fsrc%2Fmain.rs%23L58
/// ```
///
/// This is how anything holding a github.com URL says "open this" — a browser
/// extension with the repository page on screen, a bookmarklet, a link in a
/// chat. The query and not the fragment because github.com writes the line in
/// a fragment of its own and a URL has room for exactly one: `#L58` handed
/// over unescaped would be read as *this* page's fragment, and the file it
/// belongs to would be lost with it.
///
/// It is taken back out of the address bar as it is read, whether or not it
/// parsed. What a reload re-opens should be the route the app went on to
/// write — the fragment, which by then names the same place and is the link
/// worth keeping — rather than the handoff that started it.
pub fn take_handoff() -> Option<Link> {
    let at = location()?;
    let handed = param(&at.search().ok()?, HANDOFF)?;
    // Before the parse, so that a URL this page cannot read is still gone on
    // the next visit rather than failing again forever.
    let hash = at.hash().unwrap_or_default();
    clear_query(&at);
    let mut link = github_link(&handed)?;
    // The line, for a handoff that let `#L58` fall through to us: unescaped,
    // the fragment of the URL being handed over becomes the fragment of the
    // page it is handed to, and this is the other half of it arriving.
    if let Link::Ref { line, .. } = &mut link
        && line.is_none()
    {
        *line = anchor_line(&hash);
    }
    Some(link)
}

/// Put the address back to the page itself, with neither the handoff nor
/// whatever fragment came in beside it — the route replaces both.
fn clear_query(at: &web_sys::Location) {
    let Some(win) = web_sys::window() else { return };
    let Ok(history) = win.history() else { return };
    let path = at.pathname().unwrap_or_else(|_| "/".to_string());
    let _ = history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&path));
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

    fn repo_of(owner: &str, name: &str) -> RepoRef {
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
        let route = Route::to(Target::Pr(repo_of("rust-lang", "rust"), 12345));
        assert_eq!(route.hash(), "#/rust-lang/rust/pull/12345");
        assert_eq!(parse(&route.hash()), route);
    }

    #[test]
    fn a_repository_round_trips() {
        let route = Route::to(Target::Repo(repo_of("DioxusLabs", "dioxus")));
        assert_eq!(route.hash(), "#/DioxusLabs/dioxus");
        assert_eq!(parse(&route.hash()), route);
    }

    #[test]
    fn a_branch_round_trips() {
        let route = Route::to(Target::Branch(
            repo_of("DioxusLabs", "dioxus"),
            "main".to_string(),
        ));
        assert_eq!(route.hash(), "#/DioxusLabs/dioxus/tree/main");
        assert_eq!(parse(&route.hash()), route);

        // And a file of one, which is what a link into a branch is usually
        // made to point at.
        let route = Route {
            at: Target::Branch(repo_of("o", "r"), "dev".to_string()),
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
            at: Target::Branch(repo_of("o", "r"), "feat/branch-list".to_string()),
            place: place("src/ui/app.rs", None),
        };
        assert_eq!(
            route.hash(),
            "#/o/r/tree/feat/branch-list/blob/src/ui/app.rs"
        );
        assert_eq!(parse(&route.hash()), route);

        // Written out in full, with no file after it.
        let route = Route::to(Target::Branch(repo_of("o", "r"), "a/b/c".to_string()));
        assert_eq!(route.hash(), "#/o/r/tree/a/b/c");
        assert_eq!(parse(&route.hash()), route);
    }

    #[test]
    fn a_comparison_round_trips() {
        let route = Route::to(Target::Compare(
            repo_of("o", "r"),
            "main".to_string(),
            "feat/thing".to_string(),
        ));
        assert_eq!(route.hash(), "#/o/r/compare/main...feat/thing");
        assert_eq!(parse(&route.hash()), route);

        // And a line of a file inside one.
        let route = Route {
            at: Target::Compare(repo_of("o", "r"), "v1.0".to_string(), "main".to_string()),
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
                repo_of("o", "r"),
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
                repo_of("o", "r"),
                "main".to_string(),
                "dev".to_string()
            ))
        );
        assert_eq!(
            parse("#/o/r/compare/bigmah:ecbee88...bigmah:2740204"),
            Route::to(Target::Compare(
                repo_of("o", "r"),
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
                Route::to(Target::Repo(repo_of("o", "r"))),
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
                repo_of("rust-lang", "rust"),
                "master".to_string()
            ))
        );
        // And `tree` with nothing after it is just the repository.
        assert_eq!(
            parse("#/rust-lang/rust/tree/"),
            Route::to(Target::Repo(repo_of("rust-lang", "rust")))
        );
    }

    /// The marker is only a marker after the branch, so a branch that shares
    /// its name is still a branch.
    #[test]
    fn a_branch_called_blob_is_still_a_branch() {
        assert_eq!(
            parse("#/o/r/tree/blob"),
            Route::to(Target::Branch(repo_of("o", "r"), "blob".to_string()))
        );
        assert_eq!(
            parse("#/o/r/tree/blob/blob/a.rs"),
            Route {
                at: Target::Branch(repo_of("o", "r"), "blob".to_string()),
                place: place("a.rs", None),
            }
        );
    }

    #[test]
    fn a_line_of_a_pull_request_round_trips() {
        let route = Route {
            at: Target::Pr(repo_of("rust-lang", "rust"), 7),
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
        let route = Route::to(Target::Commit(
            repo_of("rust-lang", "rust"),
            sha.to_string(),
        ));
        assert_eq!(route.hash(), format!("#/rust-lang/rust/commit/{sha}"));
        assert_eq!(parse(&route.hash()), route);

        // And a line of a file inside one, which is what a link to a commit is
        // usually made to point at.
        let route = Route {
            at: Target::Commit(repo_of("o", "r"), "abc1234".to_string()),
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
            Route::to(Target::Commit(repo_of("o", "r"), "abc1234".to_string()))
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
                Route::to(Target::Repo(repo_of("o", "r"))),
                "{text}"
            );
        }
    }

    #[test]
    fn a_file_of_a_repository_round_trips() {
        let route = Route {
            at: Target::Repo(repo_of("rust-lang", "rust")),
            place: place("README.md", None),
        };
        assert_eq!(route.hash(), "#/rust-lang/rust/blob/README.md");
        assert_eq!(parse(&route.hash()), route);
    }

    #[test]
    fn a_path_with_something_in_it_that_a_url_cannot_hold() {
        let route = Route {
            at: Target::Repo(repo_of("o", "r")),
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
            Route::to(Target::Pr(repo_of("torvalds", "files"), 9))
        );
        assert_eq!(
            parse("#/torvalds/blob"),
            Route::to(Target::Repo(repo_of("torvalds", "blob")))
        );
        assert_eq!(
            parse("#/torvalds/tree/pull/9"),
            Route::to(Target::Pr(repo_of("torvalds", "tree"), 9))
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
            Route::to(Target::Pr(repo_of("rust-lang", "rust"), 7))
        );
        // And a trailing slash is not a third path segment.
        assert_eq!(
            parse("#/rust-lang/rust/"),
            Route::to(Target::Repo(repo_of("rust-lang", "rust")))
        );
        // github.com's own file link, which is the one people have to hand.
        assert_eq!(
            parse("#https://github.com/rust-lang/rust/pull/7/files/src/main.rs:L2"),
            Route {
                at: Target::Pr(repo_of("rust-lang", "rust"), 7),
                place: place("src/main.rs", Some(2)),
            }
        );
    }

    #[test]
    fn a_pull_number_that_is_not_a_number_is_still_the_repository() {
        assert_eq!(
            parse("#/rust-lang/rust/pull/abc"),
            Route::to(Target::Repo(repo_of("rust-lang", "rust")))
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

    // ---------------------------------------------------------- github.com

    fn refs(link: &Link) -> (&str, Option<usize>) {
        match link {
            Link::Ref { rest, line, .. } => (rest, *line),
            other => panic!("not a ref: {other:?}"),
        }
    }

    /// The whole reason this grammar is read separately: github.com writes the
    /// branch where our own links write the path, and the pair arrives joined.
    #[test]
    fn a_github_blob_url_keeps_the_ref_and_the_path_together() {
        let link = github_link("https://github.com/bigmah/arxiv-reader/blob/main/src/main.rs#L58")
            .unwrap();
        assert_eq!(refs(&link), ("main/src/main.rs", Some(58)));
        let Link::Ref { repo, .. } = &link else {
            unreachable!()
        };
        assert_eq!(*repo, repo_of("bigmah", "arxiv-reader"));
    }

    /// And a directory, which is what the page a reader is on usually is.
    #[test]
    fn a_github_tree_url_is_a_ref_and_a_directory() {
        let link = github_link("https://github.com/bigmah/arxiv-reader/blob/main/src/").unwrap();
        assert_eq!(refs(&link), ("main/src", None));
        let link = github_link("github.com/o/r/tree/main/src/ui").unwrap();
        assert_eq!(refs(&link), ("main/src/ui", None));
    }

    /// Nothing but a github.com URL: our own links put the path straight after
    /// `blob`, and reading one in github.com's grammar would eat a directory
    /// as the branch.
    #[test]
    fn our_own_links_are_not_github_links() {
        for ours in [
            "#/o/r/blob/src/main.rs",
            "/o/r/blob/src/main.rs",
            "o/r/tree/main",
            "o/r",
            "",
        ] {
            assert!(github_link(ours).is_none(), "{ours}");
        }
        // A URL naming no repository is the picker's business, not ours.
        assert!(github_link("https://github.com/torvalds").is_none());
    }

    /// The three other words for a file, and the selection anchors github.com
    /// writes when more than one line is picked out.
    #[test]
    fn the_other_ways_github_writes_a_file() {
        for url in [
            "https://github.com/o/r/blob/main/a.rs#L58-L72",
            "https://github.com/o/r/blame/main/a.rs#L58C5-L72C13",
            "http://www.github.com/o/r/raw/main/a.rs#L58",
            "https://github.com/o/r/blob/main/a.rs?plain=1#L58",
        ] {
            assert_eq!(
                refs(&github_link(url).unwrap()),
                ("main/a.rs", Some(58)),
                "{url}"
            );
        }
        // An anchor that is not a line names none: a diff, and a comment.
        for url in [
            "https://github.com/o/r/blob/main/a.rs#diff-2740204",
            "https://github.com/o/r/blob/main/a.rs#L0",
            "https://github.com/o/r/blob/main/a.rs",
        ] {
            assert_eq!(
                refs(&github_link(url).unwrap()),
                ("main/a.rs", None),
                "{url}"
            );
        }
    }

    /// Everything github.com writes the way this app does, which needs no
    /// asking after.
    #[test]
    fn the_forms_github_and_this_app_agree_on() {
        fn at(url: &str) -> Route {
            match github_link(url) {
                Some(Link::At(route)) => route,
                other => panic!("not a route: {other:?}"),
            }
        }
        assert_eq!(
            at("https://github.com/rust-lang/rust/pull/12345/files"),
            Route::to(Target::Pr(repo_of("rust-lang", "rust"), 12345))
        );
        // A pull request's other tabs are the same pull request.
        assert_eq!(
            at("https://github.com/o/r/pull/7/checks?check_run_id=1"),
            Route::to(Target::Pr(repo_of("o", "r"), 7))
        );
        let sha = "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0";
        assert_eq!(
            at(&format!("https://github.com/o/r/commit/{sha}#diff-abc")),
            Route::to(Target::Commit(repo_of("o", "r"), sha.to_string()))
        );
        // One commit of a pull request is that commit.
        assert_eq!(
            at(&format!("https://github.com/o/r/pull/7/commits/{sha}")),
            Route::to(Target::Commit(repo_of("o", "r"), sha.to_string()))
        );
        assert_eq!(
            at("https://github.com/o/r/compare/main...feat/thing"),
            Route::to(Target::Compare(
                repo_of("o", "r"),
                "main".to_string(),
                "feat/thing".to_string()
            ))
        );
        // A branch's history is that branch, and needs the same asking after
        // as any other ref.
        assert_eq!(
            refs(&github_link("https://github.com/o/r/commits/main").unwrap()),
            ("main", None)
        );
        // And the pages this app has nothing of its own to show for are the
        // repository they are pages of.
        for url in [
            "https://github.com/o/r",
            "https://github.com/o/r.git",
            "https://github.com/o/r/issues/12",
            "https://github.com/o/r/actions",
            "https://github.com/o/r/pull/not-a-number",
            "https://github.com/o/r/compare/main",
        ] {
            assert_eq!(at(url), Route::to(Target::Repo(repo_of("o", "r"))), "{url}");
        }
    }

    /// The shared forms go through `parse`, so a file named inside one is
    /// still read — the two grammars only part company at `blob` and `tree`.
    #[test]
    fn a_file_named_inside_a_shared_form_survives_the_host() {
        let Some(Link::At(route)) =
            github_link("https://github.com/rust-lang/rust/pull/7/files/src/main.rs:L2")
        else {
            panic!("not a route")
        };
        assert_eq!(
            route,
            Route {
                at: Target::Pr(repo_of("rust-lang", "rust"), 7),
                place: place("src/main.rs", Some(2)),
            }
        );
        // And the same link with no host on it reads identically.
        assert_eq!(parse("#/rust-lang/rust/pull/7/files/src/main.rs:L2"), route);
    }

    /// The other end of the extension's contract, in the escaping a browser
    /// actually produces.
    ///
    /// Every string below came out of `URLSearchParams` in `extension/
    /// handoff.js` — see its own `the escaping is what route.rs undoes`. The
    /// four that matter are the ones a hand-written encoder gets wrong: a
    /// space is `+`, a literal plus is `%2B`, and `&` and `=` inside the value
    /// are escaped rather than left to be read as structure.
    #[test]
    fn what_a_browser_escapes_is_what_this_reads_back() {
        for (query, meant) in [
            (
                "?url=https%3A%2F%2Fgithub.com%2Fo%2Fr%2Fblob%2Fmain%2Fsrc%2Fmain.rs%23L58",
                "https://github.com/o/r/blob/main/src/main.rs#L58",
            ),
            (
                "?url=https%3A%2F%2Fgithub.com%2Fo%2Fr%2Fblob%2Fmain%2Fdocs%2Fgetting+started.md",
                "https://github.com/o/r/blob/main/docs/getting started.md",
            ),
            (
                "?url=https%3A%2F%2Fgithub.com%2Fo%2Fr%2Fblob%2Fmain%2Fa.rs%3Fplain%3D1%23L58-L72",
                "https://github.com/o/r/blob/main/a.rs?plain=1#L58-L72",
            ),
            (
                "?url=https%3A%2F%2Fgithub.com%2Fo%2Fr%2Fcompare%2Fmain...user%3Afeat%2Bx",
                "https://github.com/o/r/compare/main...user:feat+x",
            ),
            (
                "?url=https%3A%2F%2Fgithub.com%2Fo%2Fr%2Fblob%2Fmain%2Fdocs%2F%E6%97%A5%E6%9C%AC%E8%AA%9E.md",
                "https://github.com/o/r/blob/main/docs/日本語.md",
            ),
            (
                "?url=https%3A%2F%2Fgithub.com%2Fo%2Fr%2Fblob%2Fmain%2Fa%26b%3Dc.md",
                "https://github.com/o/r/blob/main/a&b=c.md",
            ),
        ] {
            assert_eq!(param(query, "url").as_deref(), Some(meant), "{query}");
        }

        // And the whole way through, which is what the extension's button
        // actually does: a query field in, a route out.
        let handed = param(
            "?url=https%3A%2F%2Fgithub.com%2Fo%2Fr%2Fblob%2Fmain%2Fdocs%2Fgetting+started.md%23L4",
            "url",
        )
        .unwrap();
        let link = github_link(&handed).unwrap();
        assert_eq!(refs(&link), ("main/docs/getting started.md", Some(4)));
        assert_eq!(
            ref_route(
                repo_of("o", "r"),
                "main/docs/getting started.md",
                "main",
                Some(4)
            ),
            Route {
                at: Target::Branch(repo_of("o", "r"), "main".to_string()),
                place: place("docs/getting started.md", Some(4)),
            }
        );
    }

    /// The question the branch list answers: the longest name that is the
    /// whole front of what was written, up to a separator.
    #[test]
    fn the_longest_branch_that_fits_is_the_branch() {
        let rest = "main/src/main.rs";
        assert_eq!(longest_ref(rest, &["main"]).as_deref(), Some("main"));
        assert_eq!(
            longest_ref(rest, &["main", "main/src", "mainline"]).as_deref(),
            Some("main/src")
        );
        // A name that is not the front of it at a separator is not it.
        assert_eq!(longest_ref(rest, &["mainline", "ma", "src"]), None);
        // The whole of it, with nothing after: a branch with no file named.
        assert_eq!(
            longest_ref("feat/thing", &["feat", "feat/thing"]).as_deref(),
            Some("feat/thing")
        );
        assert_eq!(longest_ref("main", &[] as &[&str]), None);
    }

    /// And what falls out of the answer, both ways round.
    #[test]
    fn a_ref_and_a_path_become_a_route() {
        assert_eq!(
            ref_route(repo_of("o", "r"), "main/src/main.rs", "main", Some(58)),
            Route {
                at: Target::Branch(repo_of("o", "r"), "main".to_string()),
                place: place("src/main.rs", Some(58)),
            }
        );
        assert_eq!(
            ref_route(repo_of("o", "r"), "feat/thing/a.rs", "feat/thing", None),
            Route {
                at: Target::Branch(repo_of("o", "r"), "feat/thing".to_string()),
                place: place("a.rs", None),
            }
        );
        // A branch with nothing after it names no file.
        assert_eq!(
            ref_route(repo_of("o", "r"), "feat/thing", "feat/thing", None),
            Route::to(Target::Branch(repo_of("o", "r"), "feat/thing".to_string()))
        );
        // And the route it becomes is one of ours, which round-trips.
        let route = ref_route(repo_of("o", "r"), "main/src/main.rs", "main", Some(58));
        assert_eq!(route.hash(), "#/o/r/tree/main/blob/src/main.rs:L58");
        assert_eq!(parse(&route.hash()), route);
    }

    /// A path that climbs out of the repository names nothing in it, here as
    /// much as in a link of our own.
    #[test]
    fn a_github_path_cannot_climb_out_of_the_repository() {
        let route = ref_route(repo_of("o", "r"), "main/../../etc/passwd", "main", None);
        assert_eq!(route.place, None);
    }

    /// What github.com escapes, and what it does not: a space in a filename is
    /// `%20`, and the slashes of a branch are slashes.
    #[test]
    fn a_github_url_is_unescaped_segment_by_segment() {
        let link = github_link("https://github.com/o/r/blob/feat/a%20b/docs/getting%20started.md")
            .unwrap();
        assert_eq!(refs(&link), ("feat/a b/docs/getting started.md", None));
        assert_eq!(
            ref_route(
                repo_of("o", "r"),
                "feat/a b/docs/getting started.md",
                "feat/a b",
                None
            ),
            Route {
                at: Target::Branch(repo_of("o", "r"), "feat/a b".to_string()),
                place: place("docs/getting started.md", None),
            }
        );
    }

    /// The handoff, as an extension will write it: the whole URL escaped into
    /// one field, `#L58` and all.
    #[test]
    fn a_handed_over_url_is_read_out_of_the_query() {
        let query = "?url=https%3A%2F%2Fgithub.com%2Fo%2Fr%2Fblob%2Fmain%2Fsrc%2Fmain.rs%23L58";
        let handed = param(query, "url").unwrap();
        assert_eq!(handed, "https://github.com/o/r/blob/main/src/main.rs#L58");
        assert_eq!(
            refs(&github_link(&handed).unwrap()),
            ("main/src/main.rs", Some(58))
        );

        // Among other fields, and with the form encoding a query string has.
        assert_eq!(param("?a=1&url=o%2Br&b=2", "url").as_deref(), Some("o+r"));
        assert_eq!(param("url=a+b", "url").as_deref(), Some("a b"));
        assert_eq!(param("?other=1", "url"), None);
        assert_eq!(param("", "url"), None);
    }

    #[test]
    fn percent_escapes_are_undone_and_the_rest_is_left_alone() {
        assert_eq!(decoded("plain.md"), "plain.md");
        assert_eq!(decoded("a%20b"), "a b");
        assert_eq!(decoded("100%"), "100%");
        assert_eq!(decoded("%zz"), "%zz");
    }
}
