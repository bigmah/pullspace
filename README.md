# pullspace

A lightweight, IDE-style code diff viewer written entirely in Rust, with a
[Dioxus](https://dioxuslabs.com) UI.

Two panes, like a traditional IDE but without the weight:

- **Left** — file tree of the repository (respects `.gitignore`), with badges
  marking changed files: `M` modified, `A` added, `U` untracked, `D` deleted,
  `R` renamed. Directories containing changes get a dot and auto-expand.
  The `Δ` button filters the tree down to changed files only.
- **Right** — source viewer. Any file opens with full syntax highlighting;
  changed files open as diffs against `HEAD`, viewable **side-by-side (Split)**
  or **unified (Inline)**, both with word-level change emphasis.

IDE-ish niceties:

- **Search in files** — top-bar search box (Enter), results in a bottom panel,
  click a hit to jump to that line.
- **Go to Definition** — click any identifier in the source view, then
  "Go to Definition". Backed by a lightweight regex symbol index
  (Rust, JS/TS, Python, Go, Ruby, Java/Kotlin/Swift/C#) built in the
  background at startup.
- **Find References** — whole-word search for the clicked identifier across
  the repo.
- Jumped-to lines scroll into view and flash.
- **GitHub pull requests** — sign in once, then review any PR in the same two
  panes. See [Reviewing GitHub pull requests](#reviewing-github-pull-requests).

## Running

```sh
cargo run --release                    # open the repo containing the cwd
cargo run --release -- /path/to/repo   # open a specific repo
```

Optional extras:

```sh
pullspace /path/to/repo src/lib.rs              # open straight into a file
pullspace /path/to/repo src/lib.rs --mode=inline  # source | inline | split
```

The `⟳` button re-reads git status and the file tree after you make changes
outside the app.

## Reviewing GitHub pull requests

The top-bar chip on the right opens the GitHub panel. Type `owner/repo` to list
its open pull requests, or paste a pull request URL to jump straight to one.

The explorer then shows the **whole repository** at the PR's head commit, just
like a local checkout: changed files carry the usual `M`/`A`/`D`/`R` badges,
their directories get a dot and auto-expand, and `Δ` narrows the tree to the
changed files. Unchanged files open as plain highlighted source; changed ones
open in the same Split / Inline diff views, against the **merge base** — so you
see the PR's own changes, not everything that landed on the base branch since it
was opened.

The repository does not have to be cloned locally; file contents come from the
API, fetched per file as you open them. `✕ close PR` returns to the local
working tree.

### Signing in

pullspace uses the **OAuth device flow**, which needs no client secret and no
redirect server — nothing confidential is stored in the binary. It does need a
client ID, which is public information, and you register that once:

1. Open <https://github.com/settings/applications/new>.
2. Give it any name; the homepage and callback URL are not used — put anything
   valid in them (e.g. `http://localhost`).
3. Create the app, then on its settings page tick **Enable Device Flow**.
   Sign-in fails with a clear message if you skip this.
4. Copy the **Client ID** into the panel and press *Sign in with GitHub*. Enter
   the code shown, in the browser tab that opens.

The token is written to `~/.config/pullspace/auth.json` with `0600`
permissions, and the client ID to `config.json` beside it. *Sign out* deletes
the token.

If you would rather not register an app, pullspace also picks up an existing
credential at startup, in this order:

| Source | Notes |
| --- | --- |
| `~/.config/pullspace/auth.json` | Written by signing in through the app. |
| `$GITHUB_TOKEN` / `$GH_TOKEN` | Needs the `repo` scope for private repos. |
| `gh auth token` | Any authenticated [`gh`](https://cli.github.com) CLI. |

`PULLSPACE_GITHUB_CLIENT_ID` overrides the saved client ID. Signing in through
the app takes precedence over the ambient credentials, so it always has a
visible effect. Public repositories can be browsed with no credential at all,
subject to GitHub's 60 requests/hour unauthenticated limit.

## Architecture

```
src/
  backend/          UI-agnostic engine
    mod.rs          FileContent: one side of a diff, whatever its source
    auth.rs         GitHub device flow, token storage & discovery
    github.rs       GitHub REST: PR lists, PR files, blob content
    gitio.rs        git2: repo discovery, statuses, HEAD blob content
    tree.rs         file-tree model built from the worktree + git status
    difftool.rs     hunk/line/segment diff model (similar), split-row pairing
    highlight.rs    syntect syntax highlighting (pure-Rust fancy-regex build)
    symbols.rs      regex-based symbol index (definitions)
    search.rs       repo-wide text/word search (ignore walker)
  ui/               Dioxus components
    app.rs          state (signals + context), root layout
    github.rs       sign-in and pull request picker overlay
    topbar.rs       brand, search, index status, account chip, refresh
    filetree.rs     recursive tree with status badges
    viewer.rs       source view, inline & split diff views, symbol actions
    bottom.rs       search / references / definitions results panel
```

The backend is deliberately platform-agnostic (pure functions over paths and
strings); Dioxus compiles to native and wasm, so a web build mainly needs the
`git2`/filesystem access in `gitio.rs` swapped for a server or wasm-friendly
data source.

## Notes & limits

- Local diffs are worktree vs `HEAD` (staged + unstaged combined); pull request
  diffs are merge-base vs PR head.
- In PR mode, search / Go to Definition / Find References are disabled — they
  walk the local working tree, which the PR's files are not part of.
- Pull requests are loaded as a snapshot; `⟳` does not re-poll GitHub.
- The top bar flags a partial explorer: `truncated` (over GitHub's 3000-file
  cap per PR), `partial tree` (repo past GitHub's recursive-tree limit — about
  60k files), or `changed files only` (the tree could not be read at all).
- Files over ~400 KB / 6k lines render without highlighting for speed;
  binary files are detected and not rendered.
- Search results are capped at 400 hits.
- Go to Definition is regex-based (ctags-style), not a full language server —
  fast and dependency-free, but approximate.

## Development

```sh
cargo test    # backend unit tests
cargo run     # debug build

# Network smoke test against a real public PR, excluded from the normal run:
cargo test -- --ignored live_pr_round_trip
```
