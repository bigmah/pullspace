# The `.pullspace/` page format

This is the contract between the agent that writes summary pages and pullspace,
which reads them next to the code. Follow it exactly: pullspace relies on the
paths, the stamp and the link rules. The prose and structure inside a page are
yours to write.

## Layout

```
.pullspace/
  seed.md                         the repo's steering prompt: who reads these, what matters, what to skip
  map/
    index.html                    the repository root
    src/index.html                the directory src/
    src/backend/index.html        the directory src/backend/
  changes/
    <branch>.html                 one per pull request, named after its branch
```

- The map mirrors the repository's directories. A directory's page is always
  `.pullspace/map/<dir>/index.html`. Files never get pages of their own; the
  directory's page covers them.
- Not every directory needs a page. A directory with no page is covered by the
  nearest ancestor that has one, and pullspace lists its children from there.
  Fold small or obvious directories (icons, fixtures, one-file configs) into
  their parent.
- A change page's name is the branch name with `/` replaced by `-`
  (`feat/branch-reversal` becomes `feat-branch-reversal.html`). Every PR gets
  its own file, so two PRs never conflict over a change page.

## The stamp

Every map page carries the git tree hash of the directory it describes, as the
directory stood when the page was written:

```html
<meta name="pullspace:tree" content="3f9a0c…40 hex…">
<meta name="pullspace:written" content="2026-09-26">
```

Get it from `scripts/stamp.sh <dir>`. Never compute it by hand, and never copy
it from another page. For any directory except the root it is
`git rev-parse <tree>:<dir>`. For the root it is the root tree with
`.pullspace` left out, because otherwise writing a summary would make the root
page out of date. pullspace compares the stamp with the directory's current
hash. If they match, the page shows as **current**. If not, it shows as **out of
date**, and the reader can list what changed since.

Write the stamp exactly as above: double quotes, `name` before `content`. The
scripts read it with a plain pattern match. Change pages have no stamp.

## Rules for every page

1. **One self-contained static HTML file.** No `<script>`, no `on…=`
   attributes, no external stylesheets, fonts, images or iframes. pullspace
   renders the page in a sandbox that denies all of these, so anything that
   depends on them breaks.
2. **No CSS of your own.** pullspace supplies a stylesheet that matches the
   app's light and dark themes, built around the classes listed below. Use
   plain semantic HTML and those classes. A `style` attribute is fine for a
   one-off width or alignment. Colours belong to the classes.
3. **Links are repository paths starting with `/`:**
   - `/src/backend/store.rs` opens the file.
   - `/src/backend/store.rs#L40-L80` opens it at line 40 (`#L40` works too).
     `check.sh` makes sure the line exists.
   - `/src/backend/` opens that directory's summary, or reveals the directory
     if it has no page.
   - `https://…` opens in a new browser tab.
   - `#section-id` scrolls within the page.

   Don't link to another page's file (`.pullspace/map/…`). Link to its
   directory and pullspace finds the page. Other files under `.pullspace/`,
   such as `seed.md`, can be linked like any file. Line numbers are from the version you are describing (the PR's
   head for a change page).
4. **Keep it short.** A reviewer should be able to read a directory page in
   three to five minutes. Explain the ideas and the connections, not every
   function. Name the one or two things a newcomer would get wrong. In
   `dl.parts`, give a line to each file that matters, and group the rest ("the
   remaining `*_test.go` files: fixtures"). If a section would only say
   "nothing notable", leave it out.
5. **Diagrams are inline SVG.** Draw with the classes below so they work in
   both themes. Use them only when a picture beats a paragraph: data flow,
   layering, a state machine. `id`s (an arrowhead `<marker>`, say) must be
   unique within the page, so give each diagram's its own prefix.
6. **Showing HTML as an example?** Escape it (`&lt;meta …&gt;`) inside
   `<pre><code>`, as any HTML page would have to.

## A directory page

```html
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="pullspace:tree" content="…">
<meta name="pullspace:written" content="2026-09-26">
<title>src/backend</title>
</head>
<body>
<header>
  <h1>src/backend</h1>
  <p class="lede">One sentence: what this directory is for, in terms of the product.</p>
</header>

<section class="how">
  <h2>How it works</h2>
  <p>The mechanism, in a few paragraphs. How a request or piece of data moves
  through this directory, and which parts talk to which.</p>
</section>

<section class="parts">
  <h2>What's in here</h2>
  <dl class="parts">
    <dt><a href="/src/backend/store.rs">store.rs</a></dt><dd>Settings in localStorage; best-effort by design.</dd>
    <dt><a href="/src/backend/net/">net/</a></dt><dd>Everything that talks to GitHub.</dd>
  </dl>
</section>

<section class="invariants">
  <h2>Invariants</h2>
  <ul><li>Things that must stay true, and what breaks if they don't.</li></ul>
</section>

<section class="gotchas">
  <h2>Gotchas</h2>
  <aside class="warn"><p>The trap a newcomer walks into, and how to avoid it.</p></aside>
</section>

<section class="plans">
  <h2>Plans</h2>
  <p>Work that is known to be coming or is half done, if the code, docs or
  <code>seed.md</code> say so. Never invent plans.</p>
</section>
</body>
</html>
```

Sections, in this order, all optional except the header: `how`, `parts`,
`invariants`, `gotchas`, `plans`. The root page also has a `start` section
right after the header, giving the order in which to read the repository:

```html
<section class="start">
  <h2>Where to start</h2>
  <ol class="tour">
    <li><a href="/src/main.rs">src/main.rs</a>: why to read it first.</li>
    <li><a href="/src/backend/">src/backend/</a>: what you'll find there.</li>
  </ol>
</section>
```

Keeping the same sections in the same order is what makes before/after comparisons of a page readable.

## A change page

```html
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Branch reversal and the side-by-side slider</title>
</head>
<body>
<header>
  <h1>Branch reversal and the side-by-side slider</h1>
  <p class="lede">One sentence: what this PR does and why, for a reviewer who has not seen it.</p>
</header>

<section class="why">
  <h2>Why</h2>
  <p>The problem and the motivation. Link the issue if there is one.</p>
</section>

<section class="tour">
  <h2>Reading order</h2>
  <ol class="tour">
    <li><a href="/src/ui/panes.rs#L140-L200">panes.rs — the new drag edge</a>: start here, because everything else hangs off it.</li>
    <li>…</li>
  </ol>
</section>

<section class="decisions">
  <h2>Decisions</h2>
  <aside class="decision"><p><strong>Chose X over Y</strong> because … Y would have …</p></aside>
</section>

<section class="risks">
  <h2>Risks</h2>
  <aside class="risk"><p>What could break, where, and how you'd notice.</p></aside>
</section>

<section class="unchanged">
  <h2>Not changed</h2>
  <p>Things a reviewer would expect this PR to touch that it deliberately does not, and why.</p>
</section>

<section class="verify">
  <h2>How it was checked</h2>
  <ul><li>Tests added or run, and manual checks.</li></ul>
</section>
</body>
</html>
```

The tour is the most valuable part. Put 3–8 steps in the order a reviewer
should read the diff, each a line-anchored link and a reason. Order by
dependency, not by file name.

## Class vocabulary

| Class | Use |
|---|---|
| `p.lede` | The one-sentence summary under the `h1`. |
| `dl.parts` | A directory's contents: `dt` is a link, `dd` is one line. |
| `ol.tour` | A numbered reading order. |
| `aside` / `aside.warn` / `aside.risk` / `aside.decision` | Callouts: a note, a trap, a hazard, a choice made. |
| `span.tag` / `.tag.added` / `.tag.changed` / `.tag.removed` | A small pill. |
| `figure.diagram` + `figcaption` | Wraps an inline `<svg>`. |
| SVG: `.box`, `.box.accent`, `.box.new`, `.box.gone` | Rectangles. |
| SVG: `.edge`, `.edge.accent`, `.arrow` | Lines, and the arrowhead's fill. |
| SVG: `text`, `text.dim`, `text.mono` | Labels. |

A minimal diagram:

```html
<figure class="diagram">
  <svg viewBox="0 0 360 60" width="360" height="60" role="img" aria-label="clone feeds the store">
    <defs><marker id="flow-arrow" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="8" markerHeight="8" orient="auto"><path class="arrow" d="M0,0 L8,4 L0,8 z"/></marker></defs>
    <rect class="box" x="1" y="15" width="110" height="30" rx="5"/><text x="56" y="34" text-anchor="middle">clone.rs</text>
    <line class="edge" x1="111" y1="30" x2="246" y2="30" marker-end="url(#flow-arrow)"/>
    <rect class="box accent" x="248" y="15" width="110" height="30" rx="5"/><text x="303" y="34" text-anchor="middle">blobs.rs</text>
  </svg>
  <figcaption>A clone writes each blob once, keyed by its git hash.</figcaption>
</figure>
```
