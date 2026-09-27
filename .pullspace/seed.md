# Summary seed

Steering for the agents that write `.pullspace/`. Read before every run of the
pullspace-map and pullspace-change skills.

## Who reads these
Contributors and reviewers of pullspace. They know Rust. They may not know
Dioxus, wasm, or the browser APIs this leans on (OPFS, sandboxed frames,
`postMessage`, session vs local storage).

## What matters most here
- pullspace is a **static page**: Rust compiled to wasm, calling api.github.com
  straight from the browser. There is no server. Anything that sounds like it
  needs one is done in the tab instead.
- **The security boundary**: a GitHub token sits in localStorage, and every
  file shown is untrusted. Markdown is parsed into the app's own elements and
  never handed over as markup. HTML only ever renders in a sandboxed frame.
  Say where each of those lines is drawn.
- **Content-addressed caching**: files are kept in OPFS under their git blob
  SHA, which is why opening a second PR on a repository is nearly free.
- **Signals and spaces**: all UI state is Dioxus signals on one `St`, and a
  "space" swaps the whole per-space set in and out (`spaces::Held`). A signal
  added without being registered there leaks between reviews.
- **Panics are fatal** in wasm (the tab stops). The recurring traps:
  `std::time`, byte-slicing `&str`, `peek()` held across a call that writes the
  same signal, unbounded recursion on untrusted input.

## Vocabulary
- **workspace**: what is open. A PR, a repo at a branch, a commit, or a compare.
- **space**: one of several workspaces open in the same browser tab.
- **snapshot**: a commit's file list with blob SHAs (`github::Snapshot`).
- **summary / map / guide**: the `.pullspace/` pages this seed steers.

## Skip
`extension/icons`, `assets` (fold into the root page), `demo`, `deploy`,
`.github`, `.claude` (symlinks to `skills/`), `dist`, `target`.

## Plans
From the README: a smoother browser extension handoff. Nothing else unless the
code says so.
