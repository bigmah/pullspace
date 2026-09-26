//! Summaries: pages a coding agent writes about a repository, kept in the
//! repository, and read here beside the code they describe.
//!
//! ```text
//! .pullspace/
//!   seed.md                        what the repository wants its summaries to say
//!   map/index.html                 the repository root
//!   map/src/index.html             src/
//!   map/src/backend/index.html     src/backend/ — one page per directory worth one
//!   changes/<branch>.html          one per pull request: the reviewer's guide to it
//! ```
//!
//! The format is written down for the agents in `skills/pullspace-map`, which
//! is what writes these. Everything here is the reading half of that contract.
//!
//! **Freshness is git's own hash.** A map page carries
//! `<meta name="pullspace:tree" content="…">`: the tree hash of the directory
//! it was written against, which is `git rev-parse HEAD:src/backend` and
//! changes exactly when something under that directory does. So a page is
//! current when that hash is still the directory's, and out of date otherwise,
//! with nothing kept anywhere but the repository. The root is the one special
//! case — its tree contains `.pullspace/` itself, so every summary written would
//! make the root's out of date. It is stamped with the root's tree built with
//! `.pullspace` left out, which [`root_stamp`] builds the same way `git mktree`
//! does.
//!
//! **A page is somebody else's HTML.** It is drawn in a sandboxed frame with an
//! opaque origin, so it can reach nothing of this page — above all not the
//! token in its storage. What the frame gets on top of the page is written in
//! [`frame_doc`]: a content policy that denies every script but one, and that
//! one, which does nothing but hand a clicked link up to the app, so a summary
//! can link to code and to other summaries and have those links work here.

use std::path::{Component, Path, PathBuf};

use super::github::{Snapshot, TreeEntry};
use super::route::decoded;
use super::tree::ChangeKind;

/// Where everything lives, in the repository being read.
pub const ROOT: &str = ".pullspace";
/// The summary tree: one directory of the repository per directory here.
pub const MAP: &str = ".pullspace/map";
/// One page per pull request.
pub const CHANGES: &str = ".pullspace/changes";
/// Each directory's page.
const INDEX: &str = "index.html";

/// The page summarising `dir`, which is `""` for the repository root.
pub fn page_of(dir: &Path) -> PathBuf {
    let mut page = PathBuf::from(MAP);
    if !dir.as_os_str().is_empty() {
        page.push(dir);
    }
    page.push(INDEX);
    page
}

/// Which directory a map page summarises — `None` for anything that is not
/// one, including a change page.
pub fn dir_of(page: &Path) -> Option<PathBuf> {
    let rest = page.strip_prefix(MAP).ok()?;
    if rest.file_name()? != INDEX {
        return None;
    }
    Some(rest.parent().map(Path::to_path_buf).unwrap_or_default())
}

/// Whether a path is a pull request's page.
pub fn is_change(page: &Path) -> bool {
    page.parent() == Some(Path::new(CHANGES))
        && page
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("html"))
}

/// Whether a path is a summary of either kind.
pub fn is_page(page: &Path) -> bool {
    dir_of(page).is_some() || is_change(page)
}

/// Whether this snapshot of a repository has summaries in it at all.
pub fn has_map(snap: &Snapshot) -> bool {
    snap.entry(&page_of(Path::new(""))).is_some() || !summarized(snap).is_empty()
}

/// Every directory with a page of its own, in path order.
pub fn summarized(snap: &Snapshot) -> Vec<PathBuf> {
    // Sorted already: the snapshot is, and the page path orders the same way
    // as the directory it is under. Mostly — `src/index.html` sorts after
    // `src/a-b/…` — so it is sorted again rather than trusted.
    let mut dirs: Vec<PathBuf> = snap.files.iter().filter_map(|f| dir_of(&f.path)).collect();
    dirs.sort();
    dirs
}

/// The pages directly under `dir` in the summary tree.
///
/// Directly under in the *summary* tree, not the repository's: a directory
/// with no page of its own does not hide its children, which belong to the
/// nearest directory above them that does have one. So a repository that
/// summarises `src/backend` and not `src` lists it from the root.
pub fn children(snap: &Snapshot, dir: &Path) -> Vec<PathBuf> {
    let all = summarized(snap);
    all.iter()
        .filter(|d| d.as_path() != dir && d.starts_with(dir))
        .filter(|d| {
            // The nearest summarised ancestor of `d`, other than itself.
            let mut up = d.parent();
            while let Some(p) = up {
                if p == dir {
                    return true;
                }
                if all.iter().any(|a| a == p) {
                    return false;
                }
                up = p.parent();
            }
            false
        })
        .cloned()
        .collect()
}

/// The way back up from `dir` to the root, root first: each directory's name,
/// its path, and whether it has a page to go to.
pub fn crumbs(snap: &Snapshot, dir: &Path) -> Vec<(String, PathBuf, bool)> {
    let mut out = vec![(
        snap.repo.name.clone(),
        PathBuf::new(),
        snap.entry(&page_of(Path::new(""))).is_some(),
    )];
    let mut at = PathBuf::new();
    for part in dir.components() {
        at.push(part);
        let has = snap.entry(&page_of(&at)).is_some();
        out.push((
            part.as_os_str().to_string_lossy().into_owned(),
            at.clone(),
            has,
        ));
    }
    out
}

/// Every pull request's page in the snapshot.
pub fn change_pages(snap: &Snapshot) -> Vec<PathBuf> {
    snap.files
        .iter()
        .filter(|f| is_change(&f.path))
        .map(|f| f.path.clone())
        .collect()
}

// ------------------------------------------------------------------ the stamp

/// What a page says about itself.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Meta {
    /// The tree hash it was written against.
    pub tree: Option<String>,
    /// When, as the agent wrote it down. Shown, never compared.
    pub written: Option<String>,
    /// `<title>`, for the tab-like header over the frame.
    pub title: Option<String>,
}

/// Read a page's `<meta name="pullspace:…">` tags and its title.
///
/// Scanned rather than parsed: the head of an HTML file is the one part of it
/// with a predictable shape, and a tag written some other way is a page with
/// no stamp — which says "unknown", not something wrong.
pub fn meta(html: &str) -> Meta {
    let mut out = Meta::default();
    // Only the head matters, and a page can be long.
    let head = match find_ci(html, "<body") {
        Some(end) => &html[..end],
        None => html,
    };
    let mut at = 0;
    while let Some(off) = find_ci(&head[at..], "<meta") {
        let start = at + off;
        let Some(len) = head[start..].find('>') else {
            break;
        };
        let tag = &head[start..start + len];
        at = start + len;
        let (Some(name), Some(content)) = (attr(tag, "name"), attr(tag, "content")) else {
            continue;
        };
        let content = content.trim().to_string();
        match name.trim().to_ascii_lowercase().as_str() {
            "pullspace:tree" if is_hash(&content) => out.tree = Some(content.to_ascii_lowercase()),
            "pullspace:written" if !content.is_empty() => out.written = Some(content),
            _ => {}
        }
    }
    if let Some(open) = find_ci(head, "<title")
        && let Some(gt) = head[open..].find('>')
        && let Some(close) = find_ci(&head[open + gt..], "</title")
    {
        let text = head[open + gt + 1..open + gt + close].trim();
        if !text.is_empty() {
            out.title = Some(text.to_string());
        }
    }
    out
}

fn is_hash(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Case-insensitive `find`, in bytes. Every needle here is ASCII, so any
/// position it returns is a char boundary of the haystack.
fn find_ci(hay: &str, needle: &str) -> Option<usize> {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    if n.len() > h.len() {
        return None;
    }
    (0..=h.len() - n.len()).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}

/// One attribute's value out of a tag's text: quoted either way, or bare.
fn attr(tag: &str, name: &str) -> Option<String> {
    let bytes = tag.as_bytes();
    let mut from = 0;
    while let Some(off) = find_ci(&tag[from..], name) {
        let at = from + off;
        from = at + name.len();
        // A whole attribute name: `name` inside `data-name` is not it.
        let before = at.checked_sub(1).map(|i| bytes[i]);
        if before.is_some_and(|b| !b.is_ascii_whitespace()) {
            continue;
        }
        let rest = tag[from..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        return Some(match rest.chars().next() {
            Some(q @ ('"' | '\'')) => rest[1..].split(q).next().unwrap_or("").to_string(),
            _ => rest
                .split(|c: char| c.is_ascii_whitespace() || c == '/')
                .next()
                .unwrap_or("")
                .to_string(),
        });
    }
    None
}

/// How a page stands against the code it describes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Fresh {
    /// Written against the directory exactly as it is.
    Current,
    /// Something under the directory has changed since.
    Stale,
    /// No stamp on the page, or no hash for the directory to hold it up to —
    /// a tree GitHub truncated, or one read before this was kept.
    Unknown,
}

/// Hold a page's stamp up to the directory it summarises.
pub fn freshness(meta: &Meta, snap: &Snapshot, dir: &Path) -> Fresh {
    match (meta.tree.as_deref(), snap.dir_sha(dir)) {
        (Some(stamp), Some(now)) if stamp.eq_ignore_ascii_case(now) => Fresh::Current,
        (Some(_), Some(_)) => Fresh::Stale,
        _ => Fresh::Unknown,
    }
}

/// What has changed under `dir` since the tree a page was stamped with.
///
/// `then` is that tree, read recursively, so its paths are relative to `dir`.
/// Summaries are left out: rewriting the page is not a change to the code.
pub fn drift(then: &[TreeEntry], snap: &Snapshot, dir: &Path) -> Vec<(PathBuf, ChangeKind)> {
    let then: std::collections::HashMap<PathBuf, &str> = then
        .iter()
        .map(|e| (dir.join(&e.path), e.sha.as_str()))
        .collect();
    let mut out = Vec::new();
    let under = |p: &Path| p.starts_with(dir) && !p.starts_with(ROOT);
    for f in snap.files.iter().filter(|f| under(&f.path)) {
        match then.get(&f.path) {
            None => out.push((f.path.clone(), ChangeKind::Added)),
            Some(sha) if *sha != f.sha => out.push((f.path.clone(), ChangeKind::Modified)),
            Some(_) => {}
        }
    }
    for path in then.keys().filter(|p| under(p)) {
        if snap.entry(path).is_none() {
            out.push((path.clone(), ChangeKind::Deleted));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// The root's tree with `.pullspace` left out — what a root summary is
/// stamped with, and what `git ls-tree HEAD | grep -v $'\t.pullspace$' |
/// git mktree` prints.
///
/// `entries` are the root's own entries as GitHub lists them: mode, name and
/// hash. Built here byte for byte as git builds a tree object, since this one
/// was only ever made on the machine that wrote the summary, and GitHub has
/// never heard of it.
pub fn root_stamp<'a>(entries: impl IntoIterator<Item = (&'a str, &'a str, &'a str)>) -> String {
    let mut kept: Vec<(String, &str, Vec<u8>)> = Vec::new();
    for (mode, name, sha) in entries {
        if name == ROOT {
            continue;
        }
        // GitHub pads a tree's mode to six digits; git writes `40000`.
        let mode = mode.trim_start_matches('0').to_string();
        let Some(raw) = unhex(sha) else { continue };
        kept.push((mode, name, raw));
    }
    // Git's order: by name, with a directory compared as if its name ended in
    // a slash — so `src.txt` comes before the directory `src`.
    let key = |(mode, name, _): &(String, &str, Vec<u8>)| {
        let mut k = name.as_bytes().to_vec();
        if mode == "40000" {
            k.push(b'/');
        }
        k
    };
    kept.sort_by_key(key);
    let mut body = Vec::new();
    for (mode, name, raw) in &kept {
        body.extend_from_slice(mode.as_bytes());
        body.push(b' ');
        body.extend_from_slice(name.as_bytes());
        body.push(0);
        body.extend_from_slice(raw);
    }
    let mut hash = sha1_smol::Sha1::new();
    hash.update(format!("tree {}\0", body.len()).as_bytes());
    hash.update(&body);
    hash.digest().to_string()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !is_hash(s) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

// ------------------------------------------------------------------ the links

/// Where a link in a summary goes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Link {
    /// Out of the app.
    Web(String),
    /// A path in the repository — a file, or a directory, which is its
    /// summary when it has one. `line` is the first of a `#L10-L20`.
    Path { path: PathBuf, line: Option<usize> },
    /// Nowhere this app can go: a `mailto:`, or a path out of the repository.
    Nowhere,
}

/// Read a link out of a summary.
///
/// A path starting `/` is from the repository root, which is how the format
/// asks for every link to be written. A relative one is taken as relative to
/// `base` — the directory a map page describes, or the root for a change page
/// — rather than to where the page itself sits under `.pullspace/`, because
/// that is the only reading under which an agent writing `store.rs` in the
/// summary of `src/backend` meant what it wrote.
pub fn resolve(base: &Path, href: &str) -> Link {
    let href = href.trim();
    let lower = href.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Link::Web(href.to_string());
    }
    if href.is_empty() || href.starts_with('#') || href.starts_with("//") || lower.contains(':') {
        return Link::Nowhere;
    }
    let (path, frag) = match href.split_once('#') {
        Some((p, f)) => (p, Some(f)),
        None => (href, None),
    };
    // A query means nothing to a file in a repository.
    let path = path.split('?').next().unwrap_or("");
    let (from_root, path) = match path.strip_prefix('/') {
        Some(rest) => (true, rest),
        None => (false, path),
    };
    let mut out = if from_root {
        PathBuf::new()
    } else {
        base.to_path_buf()
    };
    for part in Path::new(&decoded(path)).components() {
        match part {
            Component::Normal(p) => out.push(p),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return Link::Nowhere;
                }
            }
            _ => return Link::Nowhere,
        }
    }
    Link::Path {
        path: out,
        line: frag.and_then(line_of),
    }
}

/// `L42`, `L42-L60` or `L42-60` → 42.
fn line_of(frag: &str) -> Option<usize> {
    let rest = frag.strip_prefix('L').or_else(|| frag.strip_prefix('l'))?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok().filter(|&n| n > 0)
}

// ------------------------------------------------------------------ the frame

/// The script the frame is allowed to run: it hands a clicked link to the app
/// and does nothing else.
///
/// Including a link to somewhere on the page itself, which it scrolls to by
/// hand. A `srcdoc` document's base URL is its *parent's*, so left to the
/// browser `#how` is this app's own address with a fragment on it — and
/// following it loads the app into the frame.
const BRIDGE_JS: &str = r#"
document.addEventListener('click', function (e) {
  if (e.defaultPrevented || e.button !== 0) return;
  var a = e.target && e.target.closest ? e.target.closest('a[href]') : null;
  if (!a) return;
  var href = a.getAttribute('href') || '';
  e.preventDefault();
  if (href.charAt(0) === '#') {
    var id = href.slice(1);
    try { id = decodeURIComponent(id); } catch (_) {}
    var to = id ? document.getElementById(id) : null;
    if (to) to.scrollIntoView({ block: 'start' });
    else if (!id) window.scrollTo(0, 0);
    return;
  }
  parent.postMessage({ pullspace: TOKEN, href: href }, '*');
}, true);
"#;

/// Marks what one side of a rewritten page says that the other does not.
///
/// The other side arrives as a string and is parsed with `DOMParser`, which
/// builds an inert document — nothing in it runs, and nothing it names is
/// fetched. Each side then compares its own passages against that document's
/// by their text: a paragraph, list item or heading whose words are not
/// anywhere on the other side is lit. So a page rewritten in a pull request
/// shows the reviewer which sentences of the explanation are new, and which
/// are gone, without anybody having to read the two side by side.
///
/// Passages rather than words, on purpose: what moved in an explanation is a
/// claim, and a claim is a sentence or a bullet. Only the innermost passages
/// are compared, so a list item is not lit because a paragraph inside it was.
const COMPARE_JS: &str = r#"
(function () {
  var BLOCKS = 'h1,h2,h3,h4,h5,p,li,dt,dd,pre,td,th,figcaption,blockquote,aside';
  function norm(t) { return (t || '').replace(/\s+/g, ' ').trim(); }
  function leaves(root) {
    var all = root.querySelectorAll(BLOCKS), out = [];
    for (var i = 0; i < all.length; i++) {
      if (!all[i].querySelector(BLOCKS)) out.push(all[i]);
    }
    return out;
  }
  function mark() {
    var other = new DOMParser().parseFromString(OTHER, 'text/html');
    var seen = {};
    leaves(other).forEach(function (el) { seen[norm(el.textContent)] = true; });
    var lit = 0;
    leaves(document).forEach(function (el) {
      var t = norm(el.textContent);
      if (t && !seen[t]) { el.classList.add(CLASS); lit++; }
    });
    document.documentElement.setAttribute('data-ps-marked', String(lit));
  }
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', mark);
  else mark();
})();
"#;

/// Which side of a rewritten page a frame is showing, for [`frame_doc`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Against {
    /// The page before the change: what it says that the new one does not is
    /// what the change took out.
    Before,
    /// After: what it says that the old one did not is what the change added.
    After,
}

/// How a summary looks unless it says otherwise: the app's own palette, which
/// arrives as the variables `Prefs::css` writes, so a page turns over with the
/// theme like everything else. The format asks pages not to style themselves,
/// and this is why that is not a loss.
const STYLE: &str = include_str!("../../assets/summary.css");

/// The document the frame is handed: the page, behind a content policy, with
/// the app's stylesheet under it and the link bridge beside it.
///
/// `theme` is the app's `:root { … }` block. `nonce` is what lets the bridge
/// run and nothing else — the page's own scripts and `on…` handlers are all
/// refused — and `token` is what the bridge signs its messages with, so the
/// app can tell them from anything else posted to it.
///
/// The frame has no origin of its own either way (see the `sandbox` it is
/// drawn with), so the policy is not what keeps the token safe. It is what
/// keeps a summary a page: no network, so nothing it draws can come from
/// anywhere but itself, and no script, so what is drawn is what was written.
///
/// `compare` is the other side of a page the change rewrote, and which side
/// this is — see [`COMPARE_JS`].
pub fn frame_doc(
    html: &str,
    theme: &str,
    nonce: &str,
    token: &str,
    compare: Option<(&str, Against)>,
) -> String {
    let body = strip_doctype(html);
    let body = strip_refresh(body);
    let csp = format!(
        "default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:; \
         script-src 'nonce-{nonce}'; base-uri 'none'; form-action 'none'"
    );
    let mut script = BRIDGE_JS.replace("TOKEN", &format!("'{token}'"));
    if let Some((other, side)) = compare {
        let class = match side {
            Against::Before => "'ps-gone'",
            Against::After => "'ps-new'",
        };
        script.push_str(
            &COMPARE_JS
                .replace("OTHER", &script_string(other))
                .replace("CLASS", class),
        );
    }
    format!(
        "<!doctype html>\n<meta http-equiv=\"Content-Security-Policy\" content=\"{csp}\">\n\
         <meta charset=\"utf-8\">\n<style>{theme}\n{STYLE}</style>\n\
         <script nonce=\"{nonce}\">{script}</script>\n{body}"
    )
}

/// A string as a JavaScript literal that is safe inside a `<script>`: JSON,
/// with every `<` escaped so that no `</script>` or `<!--` in the page can
/// end the script it is carried in.
fn script_string(s: &str) -> String {
    serde_json::to_string(s)
        .unwrap_or_else(|_| "\"\"".to_string())
        .replace('<', "\\u003c")
}

/// The page's own `<!doctype>`, which would otherwise land after ours as a
/// stray tag.
fn strip_doctype(html: &str) -> &str {
    let trimmed = html.trim_start_matches('\u{feff}').trim_start();
    if trimmed.len() >= 9 && trimmed[..9].eq_ignore_ascii_case("<!doctype") {
        return match trimmed.find('>') {
            Some(end) => &trimmed[end + 1..],
            None => "",
        };
    }
    trimmed
}

/// A `<meta http-equiv="refresh">` navigates the frame away from the page,
/// which no content policy can stop. Nothing a summary is for needs one.
fn strip_refresh(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut at = 0;
    while let Some(off) = find_ci(&html[at..], "<meta") {
        let start = at + off;
        let end = html[start..]
            .find('>')
            .map_or(html.len(), |e| start + e + 1);
        let tag = &html[start..end];
        out.push_str(&html[at..start]);
        let refresh =
            attr(tag, "http-equiv").is_some_and(|v| v.trim().eq_ignore_ascii_case("refresh"));
        if !refresh {
            out.push_str(tag);
        }
        at = end;
    }
    out.push_str(&html[at..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::github::RepoRef;

    fn entry(path: &str, sha: &str) -> TreeEntry {
        TreeEntry {
            path: PathBuf::from(path),
            sha: sha.to_string(),
            size: 1,
        }
    }

    fn snap(files: &[&str]) -> Snapshot {
        let mut files: Vec<TreeEntry> = files.iter().map(|p| entry(p, &"a".repeat(40))).collect();
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Snapshot {
            repo: RepoRef {
                owner: "o".into(),
                name: "n".into(),
            },
            commit: "c".into(),
            files,
            ..Default::default()
        }
    }

    #[test]
    fn pages_and_directories_map_both_ways() {
        assert_eq!(
            page_of(Path::new("")),
            Path::new(".pullspace/map/index.html")
        );
        assert_eq!(
            page_of(Path::new("src/backend")),
            Path::new(".pullspace/map/src/backend/index.html")
        );
        assert_eq!(
            dir_of(Path::new(".pullspace/map/index.html")),
            Some(PathBuf::new())
        );
        assert_eq!(
            dir_of(Path::new(".pullspace/map/src/ui/index.html")),
            Some(PathBuf::from("src/ui"))
        );
        assert_eq!(dir_of(Path::new(".pullspace/map/src/ui/other.html")), None);
        assert_eq!(dir_of(Path::new("src/index.html")), None);
        assert!(is_change(Path::new(".pullspace/changes/feat-x.html")));
        assert!(!is_change(Path::new(".pullspace/changes/deep/feat-x.html")));
        assert!(!is_change(Path::new(".pullspace/map/index.html")));
    }

    #[test]
    fn children_skip_over_directories_with_no_page() {
        let s = snap(&[
            ".pullspace/map/index.html",
            ".pullspace/map/src/backend/index.html",
            ".pullspace/map/src/backend/deep/index.html",
            ".pullspace/map/docs/index.html",
            "src/backend/a.rs",
        ]);
        assert_eq!(
            children(&s, Path::new("")),
            vec![PathBuf::from("docs"), PathBuf::from("src/backend")]
        );
        assert_eq!(
            children(&s, Path::new("src/backend")),
            vec![PathBuf::from("src/backend/deep")]
        );
        assert!(has_map(&s));
        let crumbs = crumbs(&s, Path::new("src/backend"));
        let names: Vec<_> = crumbs.iter().map(|c| (c.0.as_str(), c.2)).collect();
        assert_eq!(names, vec![("n", true), ("src", false), ("backend", true)]);
    }

    #[test]
    fn meta_reads_the_stamp_whatever_order_the_attributes_are_in() {
        let sha = "3F9A".to_string() + &"0".repeat(36);
        let html = format!(
            "<!doctype html><html><head><meta charset=utf-8>\
             <META content='{sha}' name=\"pullspace:tree\">\
             <meta name=pullspace:written content=2026-09-26>\
             <title> src/backend </title></head><body>\
             <meta name=\"pullspace:tree\" content=\"{}\"></body>",
            "b".repeat(40)
        );
        let m = meta(&html);
        assert_eq!(m.tree, Some(sha.to_ascii_lowercase()));
        assert_eq!(m.written.as_deref(), Some("2026-09-26"));
        assert_eq!(m.title.as_deref(), Some("src/backend"));
        // No stamp, or not a hash: unknown rather than wrong.
        assert_eq!(meta("<p>hi").tree, None);
        assert_eq!(meta("<meta name=pullspace:tree content=HEAD>").tree, None);
        // `data-name` is not `name`.
        assert_eq!(
            meta(&format!(
                "<meta data-name=x name=pullspace:tree content={}>",
                "c".repeat(40)
            ))
            .tree,
            Some("c".repeat(40))
        );
    }

    #[test]
    fn freshness_compares_the_stamp_with_the_directory() {
        let mut s = snap(&["src/a.rs"]);
        s.dirs = vec![entry("", &"1".repeat(40)), entry("src", &"2".repeat(40))];
        let stamped = |t: &str| Meta {
            tree: Some(t.repeat(40)),
            ..Default::default()
        };
        assert_eq!(
            freshness(&stamped("2"), &s, Path::new("src")),
            Fresh::Current
        );
        assert_eq!(freshness(&stamped("3"), &s, Path::new("src")), Fresh::Stale);
        assert_eq!(freshness(&stamped("1"), &s, Path::new("")), Fresh::Current);
        assert_eq!(
            freshness(&Meta::default(), &s, Path::new("src")),
            Fresh::Unknown
        );
        assert_eq!(
            freshness(&stamped("2"), &s, Path::new("gone")),
            Fresh::Unknown
        );
    }

    #[test]
    fn drift_lists_what_moved_under_the_directory() {
        let mut s = snap(&[]);
        s.files = vec![
            entry("src/a.rs", &"1".repeat(40)),
            entry("src/b.rs", &"9".repeat(40)),
            entry("src/new.rs", &"3".repeat(40)),
            entry("zz.rs", &"4".repeat(40)),
        ];
        let then = vec![
            entry("a.rs", &"1".repeat(40)),
            entry("b.rs", &"2".repeat(40)),
            entry("old.rs", &"5".repeat(40)),
        ];
        assert_eq!(
            drift(&then, &s, Path::new("src")),
            vec![
                (PathBuf::from("src/b.rs"), ChangeKind::Modified),
                (PathBuf::from("src/new.rs"), ChangeKind::Added),
                (PathBuf::from("src/old.rs"), ChangeKind::Deleted),
            ]
        );
    }

    /// Against `git mktree`, run on a fixture with the orderings that catch a
    /// naive sort: `a-b/` before `a.c`, and `src.txt` before the directory
    /// `src`. The expected hash is what git printed for it.
    #[test]
    fn root_stamp_is_what_git_mktree_builds() {
        let entries = [
            (
                "040000",
                ".pullspace",
                "86c336d99951499a894e80724f2d1a7d78b7edc6",
            ),
            (
                "100644",
                "README.md",
                "d00491fd7e5bb6fa28c517a0bb32b8b506539d4d",
            ),
            ("040000", "a-b", "7cafae9f1b71ea42ad1c9db8ef5969f9b4a9f11a"),
            ("100644", "a.c", "4286f428e3b19fe84de503916ce0e7dc8deefea1"),
            ("120000", "link", "42061c01a1c70097d1e4579f29a5adf40abdec95"),
            (
                "100755",
                "run.sh",
                "f5bdd214e01603ecd6c83be9f66d88579c588ec6",
            ),
            ("040000", "src", "f26db84cb30cbba9a8ec0c6fb839c78b1a7f64f8"),
            (
                "100644",
                "src.txt",
                "718f4d2ff533cf8ead8d3556cf43912bd245fbc4",
            ),
        ];
        assert_eq!(
            root_stamp(entries),
            "0d31c502bcaa70cb7e6b7ef80009bd7e09a0e645"
        );
        // Given in any order, which is how GitHub is free to give them.
        let mut reversed = entries;
        reversed.reverse();
        assert_eq!(
            root_stamp(reversed),
            "0d31c502bcaa70cb7e6b7ef80009bd7e09a0e645"
        );
    }

    #[test]
    fn links_resolve_from_the_root_or_the_directory_described() {
        let base = Path::new("src/backend");
        assert_eq!(
            resolve(base, "/src/ui/app.rs#L40-L80"),
            Link::Path {
                path: PathBuf::from("src/ui/app.rs"),
                line: Some(40)
            }
        );
        assert_eq!(
            resolve(base, "store.rs#L7"),
            Link::Path {
                path: PathBuf::from("src/backend/store.rs"),
                line: Some(7)
            }
        );
        assert_eq!(
            resolve(base, "../ui/"),
            Link::Path {
                path: PathBuf::from("src/ui"),
                line: None
            }
        );
        assert_eq!(
            resolve(base, "/"),
            Link::Path {
                path: PathBuf::new(),
                line: None
            }
        );
        assert_eq!(
            resolve(base, "/docs/getting%20started.md"),
            Link::Path {
                path: PathBuf::from("docs/getting started.md"),
                line: None
            }
        );
        assert_eq!(
            resolve(base, "https://example.com/x"),
            Link::Web("https://example.com/x".into())
        );
        assert_eq!(resolve(base, "../../../etc"), Link::Nowhere);
        assert_eq!(resolve(base, "javascript:alert(1)"), Link::Nowhere);
        assert_eq!(resolve(base, "mailto:a@b"), Link::Nowhere);
        assert_eq!(resolve(base, "#top"), Link::Nowhere);
    }

    #[test]
    fn the_frame_gets_a_policy_first_and_the_page_after_it() {
        let doc = frame_doc(
            "<!DOCTYPE html><html><head><meta http-equiv=refresh content='0;url=https://x'>\
             <title>t</title></head><body><script>alert(1)</script></body></html>",
            ":root{--bg:#000}",
            "N0NCE",
            "T0KEN",
            None,
        );
        let csp = doc.find("Content-Security-Policy").unwrap();
        let page = doc.find("<title>").unwrap();
        assert!(csp < page, "the policy must be in force before the page is");
        assert!(doc.contains("script-src 'nonce-N0NCE'"));
        assert!(doc.contains("<script nonce=\"N0NCE\">"));
        assert!(doc.contains("pullspace: 'T0KEN'"));
        assert!(!doc.contains("refresh"), "{doc}");
        assert_eq!(doc.matches("<!doctype html>").count(), 1);
        assert!(!doc.contains("<!DOCTYPE"));
        // The page's own script is still there, and still refused: that is
        // the policy's job, not a rewrite's.
        assert!(doc.contains("<script>alert(1)</script>"));
        assert!(!doc.contains("DOMParser"), "nothing to compare against");
    }

    /// The other side of a rewritten page rides along inside the bridge's own
    /// script, so nothing in it may be able to end that script early.
    #[test]
    fn the_other_side_cannot_break_out_of_the_script_it_rides_in() {
        let other = "<p>old</p></script><script>alert(1)</script><!-- x";
        let doc = frame_doc("<p>new</p>", "", "N", "T", Some((other, Against::After)));
        let script = &doc[doc.find("<script nonce").unwrap()..];
        let end = script.find("</script>").unwrap();
        let script = &script[..end];
        assert!(script.contains("DOMParser"));
        assert!(script.contains("'ps-new'"));
        assert!(!script.contains("<p>"), "every < is escaped");
        assert!(script.contains("\\u003c/script>"));
        assert_eq!(doc.matches("</script>").count(), 1, "only ours closes");
    }
}
