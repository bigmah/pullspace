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

## Architecture

```
src/
  backend/          UI-agnostic engine
    gitio.rs        git2: repo discovery, statuses, HEAD blob content
    tree.rs         file-tree model built from the worktree + git status
    difftool.rs     hunk/line/segment diff model (similar), split-row pairing
    highlight.rs    syntect syntax highlighting (pure-Rust fancy-regex build)
    symbols.rs      regex-based symbol index (definitions)
    search.rs       repo-wide text/word search (ignore walker)
  ui/               Dioxus components
    app.rs          state (signals + context), root layout
    topbar.rs       brand, search, index status, refresh
    filetree.rs     recursive tree with status badges
    viewer.rs       source view, inline & split diff views, symbol actions
    bottom.rs       search / references / definitions results panel
```

The backend is deliberately platform-agnostic (pure functions over paths and
strings); Dioxus compiles to native and wasm, so a web build mainly needs the
`git2`/filesystem access in `gitio.rs` swapped for a server or wasm-friendly
data source.

## Notes & limits

- Diffs are worktree vs `HEAD` (staged + unstaged combined).
- Files over ~400 KB / 6k lines render without highlighting for speed;
  binary files are detected and not rendered.
- Search results are capped at 400 hits.
- Go to Definition is regex-based (ctags-style), not a full language server —
  fast and dependency-free, but approximate.

## Development

```sh
cargo test    # backend unit tests
cargo run     # debug build
```
