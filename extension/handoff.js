// Turning "the page I am on" into "the same thing, in pullspace".
//
// The whole contract with the app is one query field. pullspace reads `?url=`
// on the way in, opens what it names, and then takes the field back out of the
// address bar and writes one of its own links in its place — see
// `take_handoff` in `src/backend/route.rs`.
//
// The field and not the fragment, because github.com writes the line it is
// pointing at in a fragment of its own (`#L58`) and a URL has room for exactly
// one. Handed over unescaped it would be read as *pullspace's* fragment, and
// the file it belongs to would be lost with it. `searchParams` escapes it,
// which is the whole of what this file is for.
//
// Nothing here touches a `chrome.*` API, so it can be — and is — run straight
// through node: see `test.js`.

/// Where the extension looks unless it is told otherwise: the deployment this
/// copy is built against, and the one line to change if it ever moves.
///
/// The options panel overrides it per profile and remembers the override, so
/// this is a default rather than a setting — it is what a fresh install gets
/// before anybody opens that panel, and what makes the button work without
/// one being opened at all.
export const DEFAULT_BASE = "https://pullspace.dev/";

/// The pages worth handing over. `www.` because github.com answers on both,
/// and `http` because a link written years ago still says it.
const GITHUB = /^https?:\/\/(www\.)?github\.com(\/|$)/i;

/// Whether pullspace has anything to show for this address.
///
/// Only that it is github.com — which of its pages is a question with a much
/// longer answer, and the app already has it. A gist, a user page or the
/// settings screen all pass here and land on pullspace's own front page, which
/// is the right place for "nothing to open".
export function isGithub(url) {
  return typeof url === "string" && GITHUB.test(url.trim());
}

/// The machine this browser is on, under the three names it answers to. A
/// dev server there is the one place `http` is meant rather than mistyped —
/// and the one place the app still works over it, a secure context being
/// granted to localhost either way.
const LOCAL = /^(localhost|127\.0\.0\.1|\[::1\])(:|\/|$)/i;

/// Tidy up what somebody typed into the options box.
///
/// A bare host is meant as https — unless it is this machine, where nothing is
/// listening on it. A missing trailing slash is not meant as anything, and a
/// fragment or a query on the base would fight the ones the handoff and the
/// app put there. Throws what `URL` throws when it is not an address at all,
/// which is what the options page reports.
export function normalizeBase(text) {
  const typed = String(text ?? "").trim();
  if (!typed) throw new TypeError("Nowhere to open.");
  const scheme = /^[a-z][a-z0-9+.-]*:\/\//i.test(typed) ? "" : LOCAL.test(typed) ? "http://" : "https://";
  const at = new URL(`${scheme}${typed}`);
  at.hash = "";
  at.search = "";
  // `…/pullspace` and `…/pullspace/` are the same directory to a static host,
  // and only one of them is the same directory to `new URL(relative, base)`.
  if (!at.pathname.endsWith("/") && !at.pathname.endsWith(".html")) {
    at.pathname += "/";
  }
  return at.toString();
}

/// The address to open: the base, with the page being handed over in `url`.
///
/// `target` empty, or naming somewhere pullspace has nothing to say about,
/// gives the base on its own — the front page, which is where "open pullspace"
/// with nothing to open should land rather than on an error.
export function handoffUrl(base, target) {
  const at = new URL(normalizeBase(base));
  if (isGithub(target)) {
    // Form encoding, which is what a query string is: `/`, `:` and the `#` of
    // an `#L58` all come out as `%XX`, and a space as `+`. `param` in
    // `route.rs` undoes exactly this.
    at.searchParams.set("url", target.trim());
  }
  return at.toString();
}
