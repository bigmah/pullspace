# pullspace

A lightweight, IDE-style pull request reviewer written entirely in Rust, with a
[Dioxus](https://dioxuslabs.com) UI.

It is a **static page**. An `index.html`, a `.wasm` and a `.js` — no server, no
backend, no account with anyone. The page talks to `api.github.com` from the tab
it is open in, because GitHub answers cross-origin requests.

Two panes, like a traditional IDE but without the weight:

- **Left** — the file tree of the repository at the pull request's head commit,
  with badges marking changed files: `M` modified, `A` added, `D` deleted, `R`
  renamed. Directories containing changes get a dot and auto-expand. The `Δ`
  button filters the tree down to changed files only.
- **Right** — source viewer. Any file opens with full syntax highlighting;
  changed files open as diffs against the merge base, viewable **side-by-side
  (Split)** or **unified (Inline)**, both with word-level change emphasis.
  Markdown opens as the prose it is, with **Source** one button away.

A third pane holds the **conversation**: the description, the discussion, the
review summaries and the comments left on lines of the diff, merged into one
list in the order they were written.

## Running it

Any static host will do. To try it locally:

```sh
cargo install dioxus-cli --version 0.6.3 --locked   # once, for `dx`

dx build --platform web --release
python3 -m http.server -d target/dx/pullspace/release/web/public 8380
```

The bundle is about **1 MB brotli-compressed**.

For a **GitHub Pages project site** — `you.github.io/pullspace/` — uncomment
`base_path` in `Dioxus.toml` first and set it to the repository name. The
generated `index.html` references its assets by absolute path, so a bundle
built without it 404s anywhere but a domain root. A user/organisation site or a
custom domain needs no `base_path`.

## Signing in

Paste a token. That is a constraint rather than a shortcut: GitHub's OAuth
endpoints — `github.com/login/device/code` and `/login/oauth/access_token` —
send no CORS headers, so no page may call them, and the web flow additionally
needs a client secret, which means a server holding it. Every OAuth route ends
at infrastructure somebody has to run and everybody has to trust with their
tokens.

A pasted token has neither problem. It is kept in `localStorage` on the origin
serving the page, and it is sent to `api.github.com` and nowhere else. You can
confirm that in the network tab.

- A **fine-grained** token needs read access to *Contents*, *Pull requests* and
  *Metadata*.
- A **classic** token needs `repo`.

Make one at <https://github.com/settings/personal-access-tokens/new>.
*Sign out* removes it from storage.

**Without a token, public repositories still work** — the panel opens straight
onto the picker, signed in or not. File contents come from
`raw.githubusercontent.com`, which is not metered, so only metadata — the PR
list, the file list, the repo tree — spends GitHub's anonymous allowance of 60
requests an hour, and a tree read once is read off the disk after that. A token
raises the allowance to 5000 and reaches private repositories.

Repository *search* is metered separately and much harder: **10 a minute signed
out**, 30 with a token. Answers already seen are remembered for as long as the
panel is open, so backspacing through a name costs nothing, but running it out
is easy — it refills within the minute, and typing a full `owner/name` or
pasting a link goes to the hourly budget instead and keeps working meanwhile.

## Reviewing a pull request

The chip on the right of the top bar opens the GitHub panel. Type part of a
repository name and pick it from the results — `↑`/`↓` and `Enter`, or click —
to list its open pull requests. With the box empty it offers your own
repositories, most recently pushed first.

Nothing has to be typed in full: `owner/repo` works, an exact name is pinned to
the top of the results, and pasting a pull request URL jumps straight to it.

The explorer shows the **whole repository** at the PR's head commit, not just
what changed. Unchanged files open as plain highlighted source; changed ones
open as diffs against the **merge base** — so you see the PR's own changes, not
everything that landed on the base branch since it was opened.

Opening a PR **downloads the whole repository** into the browser's filesystem,
twelve files at a time, starting with the ones the PR changes. The top bar
shows `cloning n/total` while it runs. It costs a few seconds on the way in and
after that nothing in the explorer touches the network: every click is a read
off the disk.

`⟳` re-polls GitHub. New commits move the head, so the changed-file list, the
tree and the cached contents are all rebuilt from it; the file you are reading
and the tree you have expanded are kept.

A repository with no open pull requests can be opened on its own, at the tip of
its default branch — the same two panes, with nothing marked as changed.

## The local copy

What is downloaded stays downloaded. Files are kept in the browser's **Origin
Private File System** — a real filesystem, private to the site, with none of
`localStorage`'s 5 MB ceiling — and they are filed under the **git blob hash**
of their contents rather than under a commit.

That is what makes the second visit cheap. A pull request opened next week has
a head commit that did not exist today, but nearly every file in it hashes to
what it hashes to now, so opening it downloads the handful of files that
actually differ and reads the other few thousand off the disk. The same goes
for the base side of every diff, for a second pull request on the same
repository, and for `⟳` after a push.

The GitHub panel says how much is stored and has a **Clear** button. Nothing is
sent anywhere: the store is per-origin, so it is readable by this page and by
nothing else, exactly like the token.

A clone runs in the background and gets out of the way of the person it is for:
it stands aside whenever a file is being opened, since both go through the same
filesystem and it has thousands of files in hand where the reader has one, and
it gives the browser a turn every couple of dozen files so the tree still
scrolls while it works. Hovering a row still warms it — but only when it is not
already here, which after the first minute it usually is.

Left to itself it stays under **1 GB**. Past that, the repositories you have not
opened in the longest are dropped, and every file no remaining repository refers
to goes with them. Files over 1 MB and the ones the viewer would only call
binary — images, archives, fonts, compiled objects — are not downloaded up
front; opening one fetches it then.

## Markdown

Markdown files open **drawn**: headings, lists, tables, block quotes and task
boxes, with fenced code run through the same syntax highlighter as the source
view. `Source` shows the file exactly as written, and the two swap without
reloading anything.

Links work. An outside one opens in a new tab; one pointing at a file in the
repository opens it in this viewer, resolved relative to the document it is
written in — so a README that links to `docs/design.md` is a way around the
repository rather than a dead end.

What is *not* drawn is anything the file smuggles in as HTML. The bar says
`raw HTML not drawn` when a file contained some. Images are not fetched either:
their alt text is shown in their place.

HTML files get a **Preview** that is the real page, rendered in an `iframe` with
an empty `sandbox` — no scripts, no forms, no navigation, and no same-origin
access, so a previewed file cannot reach the token in local storage.

## Architecture

```
src/
  main.rs           one launch
  backend/          UI-agnostic engine
    mod.rs          FileContent: one side of a diff
    http.rs         one GET, on the browser's fetch
    github.rs       GitHub REST: repo search, PR lists, PR files, trees, blobs
    auth.rs         the token, and localStorage
    store.rs        localStorage: what a desktop app would put in ~/.config
    opfs.rs         the browser's filesystem, as bytes in and bytes out
    blobs.rs        the local copy: blobs by content hash, manifests, sweeping
    clone.rs        pulling a whole commit down, and reading files back out
    layout.rs       pane sizes, kept between visits
    tree.rs         file-tree model built from a path list + change statuses
    difftool.rs     hunk/line/segment diff model (similar), split-row pairing
    highlight.rs    syntect syntax highlighting (pure-Rust fancy-regex build)
    markdown.rs     markdown parsed to blocks and styled runs (no HTML)
  ui/               Dioxus components
    app.rs          state (signals + context), root layout
    compat.rs       sleep, on the browser's event loop
    github.rs       sign-in, repository search, pull request picker overlay
    prcache.rs      what is decoded in memory, and what warms it
    topbar.rs       what is on screen, warm-up progress, account, refresh
    filetree.rs     recursive tree with status badges
    viewer.rs       source view, inline & split diff views
    markdown.rs     markdown drawn as elements; link targets
    conversation.rs the pull request's description and comments
    panes.rs        draggable dividers
```

One crate, one target, and no `#[cfg(target_arch)]` anywhere in it. Everything
is pure except `http.rs`, `store.rs`, `opfs.rs` and the `open_browser` in
`auth.rs`, which are the only four places that touch the browser at all — and
each of them answers "there is no browser here" without complaining, which is
what keeps `cargo test` running on the host.

`cargo test` still runs natively: the gloo/web-sys crates compile for the host
even though their calls only work in a page, and everything with tests — the
diff model, highlighting, markdown, the tree, URL parsing — is pure.

## Notes & limits

- The clone is one request per file, because a page cannot do it in one:
  `github.com`'s smart-HTTP endpoints send no CORS headers, and `codeload`
  answers archive requests only to GitHub's own origin, so neither the git
  protocol nor the tarball can be had from a browser tab. What can is
  `raw.githubusercontent.com`, which answers anybody and is not metered — so a
  public repository costs bandwidth and none of the API's hourly allowance,
  whether or not you are signed in. Private repositories have no CDN and are
  read through the API's blob endpoint, one request of the hour's 5000 each,
  which is why cloning one is worth doing exactly once.
- Diffs are merge-base vs head, which is the comparison GitHub's "Files
  changed" tab shows.
- The top bar flags a partial explorer: `truncated` (over GitHub's 3000-file
  cap per PR), `partial tree` (repo past GitHub's recursive-tree limit, about
  60k files), or `changed files only` (the tree could not be read at all).
- Highlighting and diffing run on the browser's single thread. Files over
  ~400 KB / 6k lines render without highlighting for speed; binary files are
  detected and not rendered.
- There is no search, no Go to Definition and no Find References. All three
  walked a directory, and a page has none.
- There is no local mode: pullspace reviews GitHub, it does not diff your
  working tree.

## Development

```sh
cargo test      # the pure logic — diff model, markdown, tree, parsing
cargo check     # host target, which is what `cargo test` builds
cargo check --target wasm32-unknown-unknown   # what actually ships
dx build --platform web --release
```

There are no network tests. They would need a browser to run in, since the only
HTTP client here is `fetch`; the CORS behaviour they would have covered is
checked by hand with `curl -H "Origin: https://example.com"`.
