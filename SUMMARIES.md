# Summaries: a map of your repository, and a guide to every PR

Coding agents write code faster than anyone can review it. AI code review
helps with the mechanics, but it can't tell a reviewer what a change is *for*,
how it fits the system, or what it quietly makes untrue.

pullspace summaries move that understanding into the repository, next to the
code:

- **The map.** Every important directory gets a short HTML page explaining how
  it works: its purpose, its mechanism, its invariants and the traps in it. The
  pages link to each other and into the code at specific lines.
- **The guide.** Every PR gets a page written by the agent that made the
  change: why, the order to read the diff in, the decisions made, the risks,
  and what was tested.
- **The diff of the explanation.** A PR that changes how a directory works also
  rewrites that directory's page. pullspace shows the old and new page side by
  side and highlights the passages that were added or removed, so the reviewer
  sees how the account of the system changed, not only which lines did.

Everything lives in a `.pullspace/` directory that is committed with the code.
Any coding agent writes the pages by following two skills shipped in this
repository. No service, no API key and no build step are needed.

## Quick start

**1. Install the skills into your repository.** They are plain directories
with a `SKILL.md`, the format Claude Code and other agent harnesses load
skills from.

```sh
mkdir -p .claude/skills
curl -sL https://github.com/bigmah/pullspace/archive/refs/heads/main.tar.gz \
  | tar -xz --strip-components=2 -C .claude/skills pullspace-main/skills
```

This gives you `.claude/skills/pullspace-map` and
`.claude/skills/pullspace-change`. Commit them so everyone on the team, and
their agents, have the same ones. With a harness that doesn't load skills, tell
the agent: *"Read `.claude/skills/pullspace-map/SKILL.md` and follow it."*

**2. Build the map** (once):

```
> /pullspace-map
```

or just *"build the pullspace summary map"*. On first run the agent writes
`.pullspace/seed.md`, a short steering prompt: who reads these pages, what
matters in this codebase, and what to skip. It asks you a few questions if it
can. It then writes one page per important directory, bottom-up, stamps each
with the git hash of the directory it describes, and checks every link. Review
the pages the way you'd review docs, then commit `.pullspace/`.

**3. Before opening each PR:**

```
> /pullspace-change
```

The agent reads the branch's diff, rewrites the map pages whose explanation the
change makes untrue, and writes `.pullspace/changes/<branch>.html`. Commit it
with the branch.

**4. Review in pullspace.** Open the PR in pullspace. It opens on the guide.

## Reading summaries in pullspace

- **◈ in the explorer header** opens the PR's guide, or the root of the map.
  Directories with a page have a ◈ on their row. The command palette has
  *Open Summaries*. When nothing is open yet, the welcome pane offers the
  guide.
- **Links work.** Clicking a directory link goes down the map. Clicking a file
  link opens the file at the linked line. The summary keeps its tab in the
  strip while you read code, so coming back is one click.
- **Breadcrumbs** lead back up the map. The strip under the header lists the
  pages one level down, with a dot on those whose code the PR touches.
- **Before / Both / After** appears when the PR rewrote the page. Passages only
  in the new version are lit green, and passages only in the old one red.
  *Source* opens the page's HTML as an ordinary diff.
- **Freshness pills.** **current** means the page was written against this
  directory exactly as it is. **out of date** means code under it changed since.
  Click *What changed since it was written?* for the list of files. When a PR
  changes code under a page without touching the page, pullspace says so. That
  is usually the first thing worth asking the author about.

## How it fits together

```
.pullspace/
  seed.md                       steering for the agents; edit freely
  map/index.html                the repository root
  map/src/index.html            src/
  map/src/backend/index.html    src/backend/, and so on down
  changes/feat-new-thing.html   one per PR, named after its branch
```

**Pages are static HTML** in a small shared vocabulary: a lede, sections for
how it works, what's in here, invariants, gotchas and plans, callouts, a
reading-order list, and inline-SVG diagrams. The contract is
[`skills/pullspace-map/FORMAT.md`](skills/pullspace-map/FORMAT.md). Pages
bring no CSS; pullspace styles them to match its light and dark themes.

**Freshness costs nothing to track.** Each map page carries
`<meta name="pullspace:tree" content="…">`, the git tree hash of its directory
when the page was written (`git rev-parse HEAD:src/backend`). That hash changes
exactly when something under the directory changes, so comparing it with the
directory's current hash tells pullspace whether the page is current. The
agent uses the same comparison to skip pages that are already current, and
`git diff <old-stamp> <new-stamp>` shows it exactly what moved. No extra store
or cache is needed. The one special case is the root, whose tree includes
`.pullspace/` itself; it is stamped with `.pullspace` left out.

**Merge conflicts stay rare.** Guides have one file per branch. A PR rewrites
only the pages whose text it makes untrue, and leaves ancestors alone even
though their stamps go stale. They show as out of date until the next
`/pullspace-map` run on the main branch, which is honest. Run that
occasionally (weekly, or after a big merge) to bring every page back to
current.

**Pages can't do anything.** A summary is someone else's HTML, rendered next to
your GitHub token. It is shown in a sandboxed frame with its own opaque origin,
so it cannot read pullspace's storage, and under a content policy that blocks
every script, event handler and network request. The only script in the frame
is pullspace's own: it forwards link clicks to the app and does the
before/after highlighting.

## The scripts

The skills call these, and you can run them yourself from anywhere in the
repository:

| Script | What it tells you |
|---|---|
| `skills/pullspace-map/scripts/plan.sh` | Every page: `current`, `stale`, `unstamped` or `orphan`, then the directories with no page, biggest first. |
| `skills/pullspace-map/scripts/stamp.sh <dir>` | The stamp for a directory (`""` for the root), from the working tree (default), `--index` or `--head`. |
| `skills/pullspace-map/scripts/check.sh` | Anything a page does that the format or the sandbox forbids, broken links, and line anchors past the end of a file. |
| `skills/pullspace-map/scripts/affected.sh [base]` | Which pages a branch's changes fall under, and which of those are nearest to the changed files. |

Once installed, the scripts are under `.claude/skills/pullspace-map/scripts/`.
They need only `git`, `sh` and `awk`.

## This repository

pullspace keeps its own map in [`.pullspace/`](.pullspace/). Open this
repository in pullspace to see it, or read
[`.pullspace/map/index.html`](.pullspace/map/index.html) in a browser.
