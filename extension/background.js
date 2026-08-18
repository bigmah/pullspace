// The extension itself: a button, a context menu and a keyboard shortcut, all
// three of which do the one thing this exists to do.
//
// A service worker rather than a page, which is what Manifest V3 gives you.
// It is stopped whenever nothing is happening and started again by the next
// event, so every listener is registered at the top level here — one added
// inside a callback would only exist in whichever run happened to get that far.

import { DEFAULT_BASE, handoffUrl } from "./handoff.js";

/// The two menu entries, by the ids their clicks come back under.
const PAGE = "pullspace-page";
const LINK = "pullspace-link";

/// Only github.com, for both of them. A menu item that offers to open the page
/// you are on in a GitHub reader, on a page that is not GitHub, is an item that
/// has to be explained.
const ON_GITHUB = ["*://github.com/*", "*://www.github.com/*"];

/// Where the reader has said their copy of pullspace is served from.
///
/// `sync` rather than `local`, so it follows a Chrome profile onto the next
/// machine — a self-hosted copy is exactly the sort of setting nobody wants to
/// type twice.
async function base() {
  const { base } = await chrome.storage.sync.get({ base: DEFAULT_BASE });
  return base || DEFAULT_BASE;
}

/// The address of the tab a click came from.
///
/// `activeTab` is what puts `url` on the tab object, and it is granted at the
/// moment the extension is invoked — the button, the menu, the shortcut — and
/// not before. The query is the same question asked a second way, for the
/// paths that hand over a tab without one.
async function addressOf(tab) {
  if (tab?.url) {
    return tab.url;
  }
  const [active] = await chrome.tabs.query({ active: true, currentWindow: true });
  return active?.url ?? "";
}

/// Open it, in a tab beside the one it came from.
///
/// Beside rather than at the end of the strip: this is a second view of the
/// page being read, and the two belong together. `openerTabId` is what has
/// Chrome put the reader back on the GitHub tab when the pullspace one is
/// closed.
async function open(target, tab) {
  let url;
  try {
    url = handoffUrl(await base(), target);
  } catch {
    // The only way here is a base nobody can open, which is a thing to fix
    // rather than a thing to report on every click.
    return chrome.runtime.openOptionsPage();
  }
  const { newTab } = await chrome.storage.sync.get({ newTab: true });
  if (!newTab && tab?.id !== undefined) {
    return chrome.tabs.update(tab.id, { url });
  }
  await chrome.tabs.create({
    url,
    openerTabId: tab?.id,
    index: tab?.index === undefined ? undefined : tab.index + 1,
  });
}

chrome.action.onClicked.addListener(async (tab) => {
  await open(await addressOf(tab), tab);
});

chrome.contextMenus.onClicked.addListener(async (info, tab) => {
  const target = info.menuItemId === LINK ? info.linkUrl : (info.pageUrl ?? (await addressOf(tab)));
  await open(target, tab);
});

// Menus are stored by Chrome, not by this worker, so they are created once when
// the extension is installed or updated rather than every time it wakes up —
// creating one that already exists is an error. Removing them first is what
// makes an update that renames one safe.
chrome.runtime.onInstalled.addListener((details) => {
  chrome.contextMenus.removeAll(() => {
    chrome.contextMenus.create({
      id: PAGE,
      title: "Open this page in pullspace",
      contexts: ["page"],
      documentUrlPatterns: ON_GITHUB,
    });
    chrome.contextMenus.create({
      id: LINK,
      title: "Open this link in pullspace",
      contexts: ["link"],
      targetUrlPatterns: ON_GITHUB,
    });
  });
  // Nothing works until it is pointed at a copy of pullspace, and a button
  // that does nothing is a button nobody presses twice.
  if (details.reason === "install") {
    chrome.runtime.openOptionsPage();
  }
});
