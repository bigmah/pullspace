---
name: pullspace-map
description: Build or refresh the repository's summary map in .pullspace/map/ — one static HTML page per important directory, explaining how the code works, stamped with git tree hashes so a reader (and pullspace, the code-review app) can tell which pages are current. Use when asked to "summarize the repo", "build/update the pullspace map", "refresh .pullspace", or to set pullspace summaries up in a repository for the first time.
---

# pullspace-map

You are writing the **map**: one page per important directory of this
repository, in `.pullspace/map/<dir>/index.html`, each explaining that
directory to an engineer who has to review changes to it. pullspace shows these
pages next to the code, walks the tree through their links, and flags a page
as out of date as soon as the code under it changes.

Read `FORMAT.md` (next to this file) before writing anything. It is the
contract. The scripts in `scripts/` (in this skill's directory, written below as
`<this skill>/scripts/…`) are how you get stamps and check your work. Run them
from anywhere in the repository. The templates are in `templates/`.

## 1. Steering: `.pullspace/seed.md`

Read `.pullspace/seed.md` if it exists. It says who the pages are for, what
matters in this codebase, the vocabulary, and what to skip. Its instructions
override the defaults below.

If it does not exist, create it from `templates/seed.md`. If you can talk to
the user, ask them two or three quick questions first: who will read these,
what they most want explained, and what to leave out. Otherwise fill it in from
what the README and the code tell you. Keep it short.

## 2. See where the map stands

```sh
<this skill>/scripts/plan.sh
```

It lists every existing page as `current`, `stale`, `unstamped` or `orphan`,
then every directory without a page with its file counts (total and direct).

Decide which directories get pages:

- A directory with a distinct responsibility, or real code in it (as a rough
  guide, at least 3 source files directly, or a subtree someone would review as
  a unit), gets a page. The root always gets one.
- Fold anything small, generated, vendored, assets-only or config-only into its
  parent's page: no page of its own, a line in the parent's. What `seed.md`
  lists under "Skip" gets no page either. Mention it in the parent only if a
  reviewer would need to know it exists.
- Keep the map shallow. Most repositories need 5–30 pages, not one per
  directory.

Delete orphan pages (their directory is gone).

## 3. Write pages bottom-up

Work from the deepest directories up to the root, so that each parent can be
written from its children's pages instead of from all the code under it.

For each directory that needs a page (missing, stale or unstamped):

1. **Read** the directory's own files. Read files of up to about 400 lines in
   full. For longer files, read the module docs at the top, the public types
   and functions with their doc comments, and whatever the rest of the
   directory calls into; skip test modules (`#[cfg(test)]`, `*_test.*`,
   `tests/`). Read config files (`Cargo.toml`, `package.json`, CI workflows)
   in full: their comments are often the best record of why things are the
   way they are. Also read the pages of its child directories. Don't
   re-summarize the children from their source; their pages are what the
   parent summarizes. A targeted look into a child's source to check one claim
   the parent makes ("nothing in `backend/` imports `ui/`", say) is fine and
   expected: do it with a grep, not a read-through.

   Before writing a parent, check with `plan.sh` that its children's pages are
   `current`. Refresh a stale child first, or the parent inherits whatever the
   child got wrong.
2. **If the page is stale**, first check what actually changed. The stamp is a
   tree hash, so `git diff <old-stamp> <new-stamp> --stat` (then without
   `--stat`) shows exactly what moved under the directory. If that tree is not
   in your object database, use
   `git log -1 --format=%H -- .pullspace/map/<dir>/index.html` and diff from
   that commit: `git diff <commit> -- <dir>`. If the page is still true, **only
   update the stamp and `pullspace:written`**, after re-checking its `#L` line
   anchors: code that moved still exists, so `check.sh` won't notice that an
   anchor now points at the wrong lines. Don't rewrite prose that is
   still correct, because every rewrite shows up as a change to the page in
   review.
3. **Write** the page to `.pullspace/map/<dir>/index.html` (root:
   `.pullspace/map/index.html`) following FORMAT.md. What makes a page useful:
   - The lede says what the directory is *for*, not what it contains.
   - "How it works" explains the mechanism: the path data or control takes
     through the code, the key types, and the reasons behind non-obvious
     choices. Quote reasons the code's own comments give.
   - Link generously, with `/`-rooted paths and `#L` line anchors, so every
     claim is one click from the code that backs it.
   - Invariants and gotchas are the parts a reviewer can't get from reading one
     diff. Put your effort there.
   - Be specific and honest. If you aren't sure how something works, say so
     rather than guessing.
4. **Stamp it** with `<this skill>/scripts/stamp.sh <dir>` (use `""` for the
   root) and put the hash in `<meta name="pullspace:tree" content="…">`, with
   today's date in `pullspace:written`. `stamp.sh <dir> --write` does both in
   a page that already has the two tags. The stamp should match the code as it
   will be committed, so pick the mode that matches:
   - `--worktree` (default): everything on disk, including untracked files
     that aren't ignored, without touching your staging area. Right when the
     commit will contain the working tree as it is.
   - `--index`: exactly what is staged. Right when some of the working tree
     won't be committed: stage what will be, then stamp.
   - `--head`: the last commit. Right when the code is already committed.

   If the commit ends up different from what was stamped, the page just shows
   as out of date. Nothing breaks, and running `plan.sh` then restamping (step
   3.2) fixes it.

Independent subtrees can be written in parallel (for example by sub-agents,
each given this skill, FORMAT.md and one subtree). A parent must wait for its
children. A sub-agent checks only its own pages: `check.sh <page> …`.

## 4. Check

```sh
<this skill>/scripts/check.sh
<this skill>/scripts/plan.sh      # everything you wrote should now say "current"
```

Fix everything `check.sh` reports: a broken link is a claim a reviewer can't
follow. Then tell the user which pages you added, rewrote, restamped and
deleted. Don't commit unless asked. If you do commit, commit `.pullspace/` on
its own (`docs(pullspace): refresh summary map`).

## What not to do

- Don't restamp a page you didn't re-verify. A current stamp is a promise that
  the page is true.
- Don't put anything in a page that the sandbox refuses (scripts, external
  resources) or any CSS beyond a stray `style` attribute. See FORMAT.md.
- Don't invent plans, history or intent. Take them from the code, comments,
  docs, commit messages or `seed.md`, or leave them out.
