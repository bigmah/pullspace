# Open in pullspace

A Chrome extension with one job: take the github.com page you are looking at —
a pull request, a file, a branch, a directory — and open the same thing in
[pullspace](../README.md).

Click the toolbar button, press `Alt+Shift+P`, or right-click the page (or any
link to GitHub) and choose *Open this page in pullspace*.

## How it works

There is no cleverness here and deliberately so. The extension reads the URL of
the tab you invoked it on and opens one of its own:

```text
https://pullspace.dev/?url=https%3A%2F%2Fgithub.com%2Fo%2Fr%2Fblob%2Fmain%2Fsrc%2Fmain.rs%23L58
```

pullspace reads that `?url=` field on the way in, works out what it names, and
takes the field back out of the address bar — leaving one of its own
`#/owner/repo/…` links in its place, which is the link worth keeping and the
one a reload comes back to.

The field and not the fragment, because github.com writes the line it is
pointing at in a fragment of its own (`#L58`) and a URL has room for exactly
one. Handed over unescaped it would be read as *pullspace's* fragment and the
file it belongs to would be lost with it.

Everything that decides what the URL comes out as lives in `handoff.js`, which
touches no `chrome.*` API for exactly that reason: `node --test test.js` runs
it. The other end of the same contract is
`what_a_browser_escapes_is_what_this_reads_back` in `src/backend/route.rs` —
the strings in that test came out of this one.

### What it asks for

`activeTab`, `contextMenus`, `storage`, and no host permissions at all.
`activeTab` is granted at the moment you invoke the extension and not before,
so it can read the address of the tab you pressed the button on and nothing
else — no content script, no page access, nothing running in the background
while you browse.

## Files

```text
manifest.json    what Chrome reads first — Manifest V3
background.js    the service worker: the button, the menu, the shortcut
handoff.js       the URL, worked out — pure, and the only part worth testing
options.html/js  where your copy of pullspace is served from
icons/           the app's own favicon, at the four sizes Chrome asks for
test.js          node --test test.js
make.sh          the zip the Chrome Web Store wants
```

## Running it from source

1. `chrome://extensions`
2. Turn on **Developer mode**, top right.
3. **Load unpacked**, and pick this `extension/` directory.
4. The options panel opens by itself the first time. Put in the address your
   copy of pullspace is served from and press **Save** — the box shows you the
   exact URL a click will open, so you can see it is right before you find out
   the hard way.

Editing a file afterwards takes a **↻** on the extension's card in
`chrome://extensions` for the service worker to pick it up; the options page
picks up its own changes on reopen. If something is not working, *Inspect views
→ service worker* on that card is where its errors are.

## Hosting pullspace itself

`DEFAULT_BASE` in `handoff.js` is `https://pullspace.dev/`, so a fresh install
points at the deployment already and the options panel is only there for a
second copy or a local one.

**The deployment has to be new enough.** `?url=` is read by `take_handoff` in
`src/backend/route.rs`, which does not exist in a build from before it — such a
build leaves the field sitting in the address bar and shows its own front page,
which looks exactly like the extension doing nothing at all. If that is what
you see, it is a redeploy and not a bug:

```sh
dx build --platform web --release
rsync -a --delete target/dx/pullspace/release/web/public/ \
      you@host:/var/www/pullspace/
```

- **A server of your own.** [`deploy/Caddyfile`](../deploy/Caddyfile) is a
  worked configuration — the four headers that matter and why each is there —
  and the rsync that puts the build behind it:

  ```sh
  rsync -a --delete target/dx/pullspace/release/web/public/ \
        you@host:/var/www/pullspace/
  ```

  Serve it over HTTPS. The Origin Private File System — which is where the
  local copy of a repository lives — is only handed to a secure context, so
  over plain http it degrades to a few files held in memory, with nothing on
  screen saying so.
- **Under a subdirectory.** If it is not at the root of its domain, set
  `base_path` in `Dioxus.toml` to that directory before building: the generated
  index.html names its `.js` and `.wasm` by absolute path, so a bundle built
  without it 404s everywhere else. Put the same path in the extension's options
  box.
- **Just trying it.** `dx serve --platform web --port 8123`, and put
  `localhost:8123` in the options box. A bare `localhost` is taken as http,
  which is the one place that is what you meant — and the one place a secure
  context is granted over it anyway.

## Publishing to the Chrome Web Store

Everything below is done once. Budget an afternoon for the first submission and
a few days of waiting; updates after that are minutes.

**1. Build the package.**

```sh
./extension/make.sh          # -> extension/pullspace-0.1.0.zip
```

The store wants a zip of the extension's *contents*, not of a folder containing
them — a `manifest.json` one directory down is the most common rejection there
is. `make.sh` zips from inside and leaves out the development files.

**2. Register as a developer.** <https://chrome.google.com/webstore/devconsole>
— a Google account, and a **one-time $5 fee**. Since 2024 the account also has
to verify a contact email before anything can be published, which is a link in
a mail; do it while you are there rather than discovering it at submission.

**3. Check the default address.** `DEFAULT_BASE` in `handoff.js` is what every
installer gets before they touch the options panel. It is
`https://pullspace.dev/`; if that ever moves, change it there and rebuild the
zip.

**4. New item, and upload the zip.** The listing wants:

- **Description.** A short one and a long one. Say that it opens GitHub pages
  in pullspace and that pullspace is a static page talking to GitHub from your
  browser — reviewers read this against the permissions.
- **Screenshots.** At least one, 1280×800 or 640×400. A github.com file page
  and the same file in pullspace side by side is the whole pitch.
- **Icon.** 128×128 — `icons/icon128.png` is already that.
- **Category.** Developer Tools.
- **Single purpose.** "Open the GitHub page the user is on in pullspace." The
  store rejects extensions that do several unrelated things; this does one.
- **Permission justifications**, one line each — this is where most first
  submissions come back:
  - *activeTab*: "Reads the URL of the tab the user invokes the extension on,
    so it can open the same page in pullspace."
  - *contextMenus*: "Adds a right-click entry on GitHub pages and links."
  - *storage*: "Remembers which pullspace instance the user has chosen."
- **Privacy.** There is no remote code, no analytics and no data collection —
  tick *does not collect user data* and say so. You still have to give a
  privacy policy URL; the repository README will do if you have nothing else.

**5. Submit.** Review is usually a few days for something this small, and
longer the first time. Rejections arrive by email with a reason attached and
are almost always a permission justification or the single-purpose statement.

**6. Updates.** Bump `"version"` in `manifest.json` — the store refuses a zip
whose version it has already seen — then `make.sh`, upload, submit. Published
updates roll out to installed copies within a few hours.

### Instead of the store

You do not have to publish it. **Load unpacked** works forever and is how you
will use it while you are working on pullspace anyway. For a handful of other
people, a zip they load unpacked themselves is a perfectly good answer — Chrome
only refuses `.crx` files installed from outside the store, not unpacked
directories.

### Other browsers

Edge takes this manifest as it is, through its own Partner Center. Firefox
needs a `browser_specific_settings.gecko.id` added to the manifest and its own
review; the rest of the code is standard WebExtension API and works there
unchanged.
