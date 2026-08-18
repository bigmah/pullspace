// The one setting there is, and a worked example of what it does.
//
// The preview is the point of this page rather than decoration on it: an
// address that is wrong by a directory produces a 404 on the next click and
// nothing else, so the panel shows the whole URL a click would open — escaping
// and all — while it is being typed.

import { DEFAULT_BASE, handoffUrl, normalizeBase } from "./handoff.js";

/// A page worth showing the escaping of: a file, on a branch, at a line, which
/// is the form with something in it for `?url=` to do.
const SHOWN = "https://github.com/bigmah/pullspace/blob/main/src/main.rs#L58";

const field = document.querySelector("#base");
const newTab = document.querySelector("#newTab");
const preview = document.querySelector("#preview");
const said = document.querySelector("#said");

/// Redraw the worked example under the box.
function show() {
  try {
    preview.textContent = handoffUrl(field.value, SHOWN);
    preview.classList.remove("bad");
    return true;
  } catch {
    preview.textContent = "That is not an address pullspace could be served from.";
    preview.classList.add("bad");
    return false;
  }
}

/// Say something, briefly. Long enough to be read and not long enough to
/// become part of the furniture.
function say(text, bad) {
  said.textContent = text;
  said.classList.toggle("bad", Boolean(bad));
  setTimeout(() => {
    said.textContent = "";
  }, 2000);
}

async function save() {
  if (!show()) {
    return say("Not saved.", true);
  }
  // Stored tidied rather than as typed, so the missing trailing slash is fixed
  // once here instead of on every click for the life of the setting.
  await chrome.storage.sync.set({
    base: normalizeBase(field.value),
    newTab: newTab.checked,
  });
  field.value = normalizeBase(field.value);
  show();
  say("Saved.");
}

const stored = await chrome.storage.sync.get({ base: DEFAULT_BASE, newTab: true });
field.value = stored.base;
newTab.checked = stored.newTab;
show();

field.addEventListener("input", show);
field.addEventListener("keydown", (e) => {
  if (e.key === "Enter") {
    save();
  }
});
document.querySelector("#save").addEventListener("click", save);
