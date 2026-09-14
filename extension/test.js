// What the extension works out before it touches a `chrome.*` API.
//
//   node --test extension/
//
// No dependencies and no browser: handoff.js is a plain module on purpose, so
// the half of this that can be wrong quietly — the escaping — is the half that
// is checked here. The other end of the same contract is tested in Rust, in
// `param` and `github_link`; the two meet in `a_handed_over_url_is_read_out_of
// _the_query` over there and in `the escaping is what route.rs undoes` here.

import { test } from "node:test";
import assert from "node:assert/strict";

import {
  DEFAULT_BASE,
  handoffUrl,
  isGithub,
  isPullspace,
  normalizeBase,
  originPattern,
  pullspaceTab,
} from "./handoff.js";

test("the escaping is what route.rs undoes", () => {
  const url = handoffUrl(
    "https://pullspace.example.com/",
    "https://github.com/o/r/blob/main/src/main.rs#L58",
  );
  // Every character that would otherwise be structure — the `#` above all,
  // which is what makes the query the only place this can travel.
  assert.equal(
    url,
    "https://pullspace.example.com/?url=https%3A%2F%2Fgithub.com%2Fo%2Fr%2Fblob%2Fmain%2Fsrc%2Fmain.rs%23L58",
  );
  // And it survives the trip back, which is the thing that actually matters.
  assert.equal(
    new URL(url).searchParams.get("url"),
    "https://github.com/o/r/blob/main/src/main.rs#L58",
  );
});

test("a base is taken as meant", () => {
  // A bare host means https, and a subdirectory means a directory.
  assert.equal(normalizeBase("pullspace.example.com/app"), "https://pullspace.example.com/app/");
  assert.equal(normalizeBase("  https://x.dev/  "), "https://x.dev/");
  assert.equal(normalizeBase("http://localhost:8080"), "http://localhost:8080/");
  // This machine is the one place a bare host means http: nothing is listening
  // on https there, and it is also the one place the app does not need it.
  assert.equal(normalizeBase("localhost:8080"), "http://localhost:8080/");
  assert.equal(normalizeBase("127.0.0.1:8080/pullspace"), "http://127.0.0.1:8080/pullspace/");
  // A host that merely starts with the word is somebody else's.
  assert.equal(normalizeBase("localhost.evil.test/p"), "https://localhost.evil.test/p/");
  // A page named outright is left as the page it is.
  assert.equal(normalizeBase("https://x.dev/app/index.html"), "https://x.dev/app/index.html");
  // Whatever was on the end of it would only fight what the handoff puts there.
  assert.equal(normalizeBase("https://x.dev/p/?a=1#/o/r"), "https://x.dev/p/");
  for (const nonsense of ["", "   ", "http://", null, undefined]) {
    assert.throws(() => normalizeBase(nonsense), `${nonsense}`);
  }
});

test("only github.com is worth handing over", () => {
  for (const yes of [
    "https://github.com/o/r",
    "http://github.com/o/r/pull/1",
    "https://www.github.com/o/r/blob/main/a.rs#L1",
    "https://github.com",
  ]) {
    assert.ok(isGithub(yes), yes);
  }
  for (const no of [
    "https://gitlab.com/o/r",
    // The suffix is not the host: this is somebody else's domain.
    "https://notgithub.com/o/r",
    "https://github.com.evil.test/o/r",
    "https://gist.github.com/o/1",
    "about:blank",
    "",
    null,
  ]) {
    assert.ok(!isGithub(no), `${no}`);
  }
});

test("nothing to open lands on the front page", () => {
  // No `?url=` rather than an empty one: pullspace would read the field, find
  // nothing in it and show its own front page either way, but an address with
  // a dangling field in it is one nobody can read.
  assert.equal(handoffUrl(DEFAULT_BASE, ""), DEFAULT_BASE);
  assert.equal(handoffUrl(DEFAULT_BASE, "https://gitlab.com/o/r"), DEFAULT_BASE);
  assert.equal(handoffUrl(DEFAULT_BASE, undefined), DEFAULT_BASE);
});

test("the settings the options page offers all produce an address", () => {
  for (const base of ["https://pullspace.example.com/", "http://localhost:8080/", "https://x.dev/"]) {
    const url = new URL(handoffUrl(base, "https://github.com/o/r/tree/feat/thing/src"));
    assert.equal(url.searchParams.get("url"), "https://github.com/o/r/tree/feat/thing/src");
    assert.ok(url.toString().startsWith(base));
  }
});

test("a pullspace tab is the page at the base, at any route", () => {
  const base = "https://x.dev/app/";
  for (const yes of [
    "https://x.dev/app/",
    "https://x.dev/app/#/o/r/pull/1",
    "https://x.dev/app/index.html#/o/r",
    // Mid-handoff, before the app has taken the field back out.
    "https://x.dev/app/?url=https%3A%2F%2Fgithub.com%2Fo%2Fr",
  ]) {
    assert.ok(isPullspace(base, yes), yes);
  }
  for (const no of [
    // Something else on the same host.
    "https://x.dev/",
    "https://x.dev/app/other.html",
    "https://x.dev/application/",
    "http://x.dev/app/",
    "https://x.dev:8443/app/",
    "https://github.com/o/r",
    // What a tab looks like when Chrome has not shown this extension its address.
    undefined,
    "",
  ]) {
    assert.ok(!isPullspace(base, no), `${no}`);
  }
  // A dev server is its port, not every port on the machine.
  assert.ok(isPullspace("localhost:8123", "http://localhost:8123/#/o/r"));
  assert.ok(!isPullspace("localhost:8123", "http://localhost:8080/#/o/r"));
});

test("the tab to reuse is in this window, and the one looked at last", () => {
  const base = "https://x.dev/";
  const tab = (id, windowId, lastAccessed, url = "https://x.dev/#/o/r") => ({
    id,
    windowId,
    lastAccessed,
    url,
  });
  const tabs = [
    tab(1, 1, 100),
    tab(2, 2, 900),
    tab(3, 1, 500),
    tab(4, 1, 999, "https://github.com/o/r"),
    // No address: not one Chrome let this extension see, so not a candidate.
    { id: 5, windowId: 1, lastAccessed: 1000 },
  ];
  assert.equal(pullspaceTab(base, tabs, 1)?.id, 3);
  assert.equal(pullspaceTab(base, tabs, 2)?.id, 2);
  // None in this window: the most recent anywhere, rather than a new tab.
  assert.equal(pullspaceTab(base, tabs, 7)?.id, 2);
  assert.equal(pullspaceTab(base, [tab(4, 1, 1, "https://github.com/o/r")], 1), undefined);
  assert.equal(pullspaceTab(base, [], 1), undefined);
});

test("the permission asked for is the base's host, port and all", () => {
  assert.equal(originPattern(DEFAULT_BASE), "https://pullspace.dev/*");
  assert.equal(originPattern("https://x.dev/app/"), "https://x.dev/*");
  assert.equal(originPattern("localhost:8123"), "http://localhost:8123/*");
});
