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
  (Split)** or **unified (Inline)**, both with word-level change emphasis. The
  unchanged stretches between changes arrive folded: the bar standing in for one
  opens it twenty lines at a time from either end, or all at once, and folds it
  back up again — `Reset` puts the whole file back the way it arrived. Markdown
  opens as the prose it is, with **Source** one button away.

A third pane holds the **conversation**: the description, the discussion, the
review summaries and the comments left on lines of the diff, merged into one
list in the order they were written.

A panel across the bottom of the code holds the answer to "where else is this?"
— search hits, references, definitions — and closes again once it has been read.

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

For **a server of your own**, [`deploy/Caddyfile`](deploy/Caddyfile) is a
working configuration with the four things that matter commented in place. The
one worth knowing before you host this anywhere: **serve it over HTTPS**. The
Origin Private File System is only handed to a secure context, so over plain
`http` the local copy quietly becomes a handful of files in memory — no
persistence, no cheap second visit, and nothing on screen to say so.
`localhost` is exempt, which is why development never catches it.

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

An account is found the same way. A name that could be a login is looked up
alongside the search, and the account it belongs to is offered above the
repositories — which is what makes typing an organisation's name work rather
than nearly work, since GitHub's index ranks repositories by *their* names, and
an organisation whose repositories are not called after it cannot be found by
searching for it. Picking that row scopes the box to `owner/`, listing
everything the account owns and narrowing it as you carry on typing.

The explorer shows the **whole repository** at the PR's head commit, not just
what changed. Unchanged files open as plain highlighted source; changed ones
open as diffs against the **merge base** — so you see the PR's own changes, not
everything that landed on the base branch since it was opened.

Every changed file has a **Viewed** box above it, and the arrows beside it —
`⌥↑` and `⌥↓` — step to the previous and next changed file in the order the
explorer lists them, folded directories included. The number between them says
where you are: `4/17`. A file you have ticked dims in the explorer and takes a
tick of its own, and the count beside EXPLORER becomes `4/17`, so what is left
to read is a glance rather than a scroll.

What a tick remembers is the **git blob hash** of that file — the same hash the
local copy files its contents under, not the file's name. So a force-push
behaves the way a reviewer would want it to: a rebase that leaves a file
byte-identical leaves it ticked, and anything genuinely rewritten comes back
unticked, for another look. The marks are kept in `localStorage`, per pull
request, for the last two dozen you have opened.

The address bar follows what is open — `#/owner/repo/pull/123` for a pull
request, `#/owner/repo` for a repository being read on its own. A review is
therefore a link: one to send to somebody, one to bookmark, and one this tab
comes back to on reload. The browser's own Back and Forward walk the pull
requests you have opened, and a link pasted into a tab that is already open
loads it. It routes after the `#` because there is no server here to teach
about routes — the fragment is the one part of a URL no host ever sees, so a
deep link works on GitHub Pages, under a `base_path`, and out of a bare
directory, with nothing to configure anywhere.

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

## Reading the code

The local copy is what makes the rest of this possible. Search, Go to Definition
and Find References all walked a directory on a machine that had one; here they
walk the commit's file list and read the bytes off the browser's filesystem, and
none of the three touches the network.

**Search** is the box in the top bar — `⌘F` puts the cursor in it, `Enter` runs
it, and the three buttons beside it are match case, whole word and regular
expression. Hits land in the panel across the bottom, a row to a line, with the
parts that matched picked out; clicking one opens the file there. It stops at
500 lines, because nobody reads the six hundredth.

**Double-clicking an identifier** in the code selects it, and every other
occurrence of it in the file lights up. Buttons appear on the bar above: go to
where it is defined (`F12`), show that definition below without leaving the
file, find every use of it in the repository (`⇧F12`), and put it down again.

Definitions come out of an index built in the background once the download has
finished — the top bar counts it up, then says how many it found. It is read
rather than compiled: a regular expression per language, of the sort `ctags` has
used for thirty years, covering Rust, JavaScript/TypeScript, Python, Go, Ruby,
Java/Kotlin/Swift/C#/Scala, C/C++, PHP, shell and SQL. It knows that
`pub async fn foo(` defines `foo`, and nothing whatever about what `foo` means —
so where a repository has eleven things called `new` it offers the list rather
than picking one, nearest to the file you are reading first.

Following a definition three files away is only worth doing if getting back is
one key. `⌘[` and `⌘]` — or `Alt`+`←`/`→` — walk back and forward through where
you have been, and a diff you were reading comes back as the diff rather than as
source. `Alt`+`↑`/`↓` walk the other axis: the changed files, in review order.
`⌘P` puts the cursor in the explorer's filter box. `Esc` closes the panel, and
then clears the selection.

What is *not* read is what was never downloaded: files over 2 MB, the ones the
clone leaves behind, and anything a clone that was interrupted never reached.
The panel says how many, every time — a search that quietly misses a file is
worse than one that admits to it.

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
    route.rs        what is open, as `#/owner/repo/pull/123`, and back again
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
    conversation.rs the pull request's description and comments
    panes.rs        draggable dividers
```

One crate, one target, and no `#[cfg(target_arch)]` anywhere in it. Everything
is pure except `http.rs`, `store.rs`, `opfs.rs`, `route.rs` and the
`open_browser` in `auth.rs` — the five places that touch the browser at all.
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
- A link opens the pull request, not the file: `#/owner/repo/pull/123` and no
  deeper. Which file you had open, and how, is where the tab left off rather
  than something to send to anyone.
- Viewed marks are per browser, like everything else here. They are yours, not
  the pull request's — nothing is written back to GitHub, so the boxes you tick
  are invisible to it and to everybody else on the review.
- There is no local mode: pullspace reviews GitHub, it does not diff your
  working tree.

## Development

```sh
cargo test      # the pure logic — diff model, markdown, tree, search, parsing
cargo check     # host target, which is what `cargo test` builds
cargo check --target wasm32-unknown-unknown   # what actually ships
dx build --platform web --release
```

There are no network tests. They would need a browser to run in, since the only
HTTP client here is `fetch`; the CORS behaviour they would have covered is
checked by hand with `curl -H "Origin: https://example.com"`.

CI runs all four of the above on every push and pull request, on the toolchain
pinned in `rust-toolchain.toml` — the same one a fresh checkout gets, so a
green run means the compiler agreed with yours and not merely with itself.

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
