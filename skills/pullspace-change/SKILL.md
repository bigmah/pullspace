---
name: pullspace-change
description: Before opening or updating a pull request, write its reviewer's guide to .pullspace/changes/<branch>.html (why, a line-anchored reading order, decisions, risks, what was checked) and bring the .pullspace/map pages it affects up to date, so pullspace can show the reviewer how the explanation of the code changed, not just the lines. Use when asked to "prep this PR for review", "write the pullspace change page", "update .pullspace for this branch", or when finishing a branch in a repository that has a .pullspace/ directory.
---

# pullspace-change

A pull request that goes through this skill carries two extra things:

1. **A guide**, `.pullspace/changes/<branch>.html`: what the change does, why,
   and the order in which to read it. pullspace opens a PR on this page.
2. **The map pages it affects**, rewritten or restamped. pullspace shows each
   rewritten page before and after, side by side, so the reviewer sees how the
   explanation of the system moved. If the PR changes code under a page and
   leaves the page alone, pullspace flags it.

This skill uses the format and scripts of **pullspace-map**, which is
installed beside it. Below, `<map>` means that skill's directory: the sibling
`pullspace-map/` of this skill's own directory. Read `<map>/FORMAT.md` first.
The scripts are in `<map>/scripts/` and can be run from anywhere in the
repository. If `.pullspace/` doesn't exist in this repository
yet, write only the guide, and suggest running pullspace-map to create the map.

## 1. What is the change

```sh
base=$(git merge-base HEAD "$(git symbolic-ref -q --short refs/remotes/origin/HEAD || echo origin/main)")
git log --oneline "$base"..HEAD
git diff --stat "$base"
git diff "$base"
```

`git diff "$base"` (no `..HEAD`) includes uncommitted work. If there is
uncommitted work, check with the user whether it belongs to this PR. Also read
`.pullspace/seed.md`, and the map pages of the directories the diff touches:
they describe the "before" your change moves away from.

## 2. Update the map

List the pages the change falls under:

```sh
<map>/scripts/affected.sh "$base"
```

Each line is a page, the number of changed files it is the *nearest* page to,
the number under it at any depth, and whether it is current. Work on the pages
with a nearest count above zero. Pages with a nearest count of 0 are only
ancestors (see the last paragraph of this section). `nopage` lines are changed
directories that no page covers except the root.

For each page with changed files nearest to it:

- **If the page's prose is no longer true**, rewrite the parts that changed,
  keeping the section order and every sentence that is still right. The
  reviewer will read a before/after of this page, and a page rewritten from
  scratch is an unreadable comparison.
- **If it is still true**, update only the stamp and `pullspace:written`, and
  fix any `#L` line anchors your change moved: they still pass `check.sh` but
  now point at the wrong lines.
- New directories that deserve a page (see pullspace-map §2) get one.
  Directories the PR deletes lose theirs.

Stamp every page you touch with `<map>/scripts/stamp.sh <dir>` once
the code is final. Use `--head` if the code is committed and the working tree
has unrelated edits.

**Leave ancestors alone unless their text is wrong.** A change deep in
`src/backend/net/` usually leaves the root page just as true. Don't restamp it
"to keep it current": that edit would conflict with every other open PR. It
will show as out of date until the next pullspace-map run on the main branch,
and that is correct.

## 3. Write the guide

File name: the branch name with `/` replaced by `-`, in
`.pullspace/changes/`. Get the branch with `git branch --show-current`. Start
from `<map>/templates/change.html` and follow FORMAT.md's change
page. What makes it worth reading:

- **The lede**: one sentence a reviewer could repeat to someone else.
- **Why**: the problem, not the solution. Link the issue if there is one.
- **The reading order** is the heart of it. Give 3–8 steps in the order the
  diff makes sense (the new type before its uses, the core change before the
  plumbing). Each step is a `/path#Lstart-Lend` link into the PR's head
  version and one sentence on what to look for there. Get line numbers from the
  files as they stand now: `grep -n` them, don't guess.
- **Decisions**: every non-obvious choice, what the alternative was, and why
  it lost. This is what code review usually has to dig out of the author.
- **Risks**: what could break, where, and how someone would notice. Be
  concrete. "Might have bugs" is not a risk.
- **Not changed**: what a reviewer would expect this PR to touch and why it
  doesn't. Omit the section if there is nothing.
- **How it was checked**: the tests added or run and the manual checks done,
  truthfully. If something wasn't verified, say so.

If a map page was rewritten, say so in the guide and link the directory
(`/src/backend/`) so the reviewer can open its before/after.

## 4. Check and hand over

```sh
<map>/scripts/check.sh
<map>/scripts/plan.sh
```

Every page you touched should be `current` in `plan.sh`, and `check.sh` should
report nothing. Tell the user which pages you rewrote, restamped and added,
and where the guide is. Don't commit unless asked. If you do, commit the
`.pullspace/` changes as their own commit on the branch
(`docs(pullspace): guide and map for <branch>`).
