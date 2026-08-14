# pullspace

An IDE-style code reviewer written entirely in Rust and compiled to WASM

It is a **static page**. An `index.html`, a `.wasm` and a `.js` — no server, no
backend, no account with anyone. The page talks to `api.github.com` from the tab
it is open in, because GitHub answers cross-origin requests.

![pullspace showing a pull request diff side-by-side](diff.png)


Two panes, like a traditional IDE but without the weight:

- **Left** — the file tree of the repository at the pull request's head commit,
  with badges marking changed files: `M` modified, `A` added, `D` deleted, `R`
  renamed. Directories containing changes get a dot and auto-expand. The `Δ`
  button filters the tree down to changed files only.
- **Right** — source viewer. Any file opens with full syntax highlighting;
  changed files open as diffs against the merge base, viewable **side-by-side
  (Split)** or **unified (Inline)**, both with word-level change emphasis. The
  unchanged stretches between changes arrive folded: the bar standing in for one
  opens it twenty lines at a time from either end, or all at once, and folds it
  back up again — `Reset` puts the whole file back the way it arrived. Markdown
  opens as the prose it is, with **Source** one button away.

A third pane holds the **conversation**: the description, the discussion, the
review summaries and the comments left on lines of the diff, merged into one
list in the order they were written. Bodies are drawn as the markdown they are
— headings, lists, tables, and fenced code with the same highlighter the viewer
uses — through the renderer described under [Markdown](#markdown), so nothing
anybody wrote on the pull request is ever handed to this page as markup. Beside
it, **COMMITS** is the other half of the same pane: every commit on the branch,
oldest first, each one its subject line, its author and its short SHA. Clicking
one puts *that commit's* diff in the two panes — what it changed, against the
commit before it — while the list stays where it is, so a branch can be read the
way it was written, one commit at a time.

A panel across the bottom of the code holds the answer to "where else is this?"
— search hits, references, definitions — and closes again once it has been read.

## Running it
```sh
cargo install dioxus-cli --version 0.6.3 --locked   # once, for `dx`

dx build --platform web --release
```

## Development

```sh
cargo test      # the pure logic — diff model, markdown, tree, search, parsing
cargo check     # host target, which is what `cargo test` builds
cargo check --target wasm32-unknown-unknown   # what actually ships
dx build --platform web --release
```

## Signing in (Private repos)

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

## Markdown + HTML

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

The conversation pane goes through the same renderer, at the size of the column
it is in — which is the answer to the obvious worry about drawing text that
anybody with a GitHub account can write on a pull request. There is no HTML
path to attack: the parse produces blocks and styled runs, the viewer builds
elements out of them, and a comment that was mostly a `<details>` block says
`html not drawn` under it rather than quietly losing half of itself.

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
    route.rs        what is open, as `#/owner/repo/pull/123/files/x.rs:L42`
    prefs.rs        theme, accent, font and size, as a `:root` stylesheet
    clip.rs         the clipboard, for the link to a line
    viewed.rs       which files have been ticked off, by blob hash
    opfs.rs         the browser's filesystem, as bytes in and bytes out
    blobs.rs        the local copy: blobs by content hash, manifests, sweeping
    clone.rs        pulling a whole commit down, and reading files back out
    scan.rs         reading a commit back out of the store, and the text cache
    search.rs       patterns, and the lines that match them
    symbols.rs      where things are defined, by regex per language
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
    ide.rs          search, definitions, references, and the keyboard
    nav.rs          the address bar read back: links, Back and Forward
    bottom.rs       the panel under the code: hits, definitions, a peek at one
    markdown.rs     markdown drawn as elements; link targets
    conversation.rs the pull request's description and comments, drawn
    prefs.rs        the appearance panel
    page.rs         the browser tab: its name, and the icon on it
    panes.rs        draggable dividers
```

One crate, one target, and no `#[cfg(target_arch)]` anywhere in it. Everything
is pure except `http.rs`, `store.rs`, `opfs.rs`, `route.rs`, `clip.rs` and the
`open_browser` in `auth.rs` — the six places that touch the browser at all.
Nothing in the test suite reaches any of them, which is what keeps `cargo test`
running on the host.

`cargo test` still runs natively: the gloo/web-sys crates compile for the host
even though their calls only work in a page, and everything with tests — the
diff model, highlighting, markdown, the tree, search patterns, the symbol
patterns, URL parsing — is pure.

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
  100k entries or 7 MB), or `changed files only` (the tree could not be read at
  all).
- Highlighting and diffing run on the browser's single thread. Files over
  ~400 KB / 6k lines render without highlighting for speed; binary files are
  detected and not rendered.
- Search, Go to Definition and Find References read the local copy rather than
  GitHub, so they cover what was downloaded and say what they missed. The index
  behind the last two is regular expressions per language and not a compiler:
  right nearly always within one file, a very good guess across a repository,
  and it offers the list when a name is defined more than once.
- The index waits for the download to finish before it starts, since both go
  through the same filesystem. On a large repository the first Go to Definition
  of a session may arrive a moment after the code does.
- A link carries the file and the line, but not the mode: it opens as source at
  that line, because that is the one form every file has. A line of a diff is
  therefore two clicks away — `Inline` or `Split` keeps the line lit.
- Appearance is per browser, like everything else here: a theme is `localStorage`
  on this origin, not an account setting, and a fresh browser starts on Dark.
  The code fonts on offer are the ones your operating system already ships, and
  are named for it — a page cannot install a font, and none is bundled, so
  offering one you do not have would be a button that changes nothing.
- Viewed marks are per browser, like everything else here. They are yours, not
  the pull request's — nothing is written back to GitHub, so the boxes you tick
  are invisible to it and to everybody else on the review.
- There is no local mode: pullspace reviews GitHub, it does not diff your
  working tree.

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <https://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this work by you, as defined in the Apache-2.0 license, shall
be dual-licensed as above, without any additional terms or conditions.
