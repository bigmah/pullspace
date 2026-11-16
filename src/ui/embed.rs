//! pullspace inside another page, reviewing the files that page hands it — see
//! [`crate::backend::host`].
//!
//! Embedded, there is no GitHub, no address bar to follow and no spaces: one
//! comparison is open from start to finish, read again whenever the host says
//! its files moved.

use dioxus::prelude::*;

use crate::backend::host;

use super::app::{Fetch, PrFileState, St};

/// The host's events, for as long as the app is up. Only from the parent, and
/// only from a page of this one's origin: anything else can post here too.
const EVENTS_JS: &str = r#"
(function () {
  if (window.__pullspace_host) window.__pullspace_host();
  var on = function (e) {
    if (e.source !== window.parent || e.origin !== location.origin) return;
    var d = e.data;
    if (d && d.ps === 1 && typeof d.event === 'string') dioxus.send(d.event);
  };
  window.addEventListener('message', on);
  window.__pullspace_host = function () { window.removeEventListener('message', on); };
})();
"#;

/// Open what the host has, and open it again each time it says it changed.
pub async fn run(st: St) {
    // Listening before the first read, so a change made while it is out is
    // not missed.
    let mut events = document::eval(EVENTS_JS);
    load(st).await;
    while let Ok(event) = events.recv::<String>().await {
        if event == pullspace_embed::protocol::CHANGED {
            load(st).await;
        }
    }
}

/// Read both sides from the host and show them.
pub async fn load(st: St) {
    let mut fetch = st.fetch;
    fetch.set(Fetch::Working("reading the workspace".to_string()));
    let snap = match host::snapshot().await {
        Ok(snap) => snap,
        Err(e) => {
            fetch.set(Fetch::Failed(format!("{e:#}")));
            return;
        }
    };
    fetch.set(Fetch::Idle);
    let view = host::compare_view(snap);
    let first = {
        let held = st.workspace.peek();
        match held.compare() {
            // Nothing moved: a reload would only throw away what is decoded.
            Some(open) if open.tree == view.tree && open.base_tree == view.base_tree => return,
            Some(_) => None,
            None => view.files.first().map(|f| f.path.clone()),
        }
    };
    st.enter_compare(view);
    // The first time, the first change: it is what there is to review.
    if let Some(path) = first {
        st.open_file(path);
    }
}

/// The line picked out of the open file, quoted for the host to ask about.
pub fn quote(st: &St) {
    let Some(path) = st.open.peek().clone() else {
        return;
    };
    let Some(line) = *st.at_line.peek() else {
        return;
    };
    let text = match st.pr_files.peek().get(&path) {
        Some(PrFileState::Ready { base, head }) => head
            .text()
            .filter(|t| !t.is_empty())
            .or_else(|| base.text())
            .and_then(|t| t.lines().nth(line.saturating_sub(1)))
            .map(str::to_string),
        _ => None,
    };
    let quote = pullspace_embed::protocol::Quote {
        path: path.display().to_string(),
        line,
        text,
    };
    if let Ok(body) = serde_json::to_value(quote) {
        host::post(pullspace_embed::protocol::QUOTE, body);
    }
}

/// What stands in for the app until the host has answered.
#[component]
pub fn Waiting() -> Element {
    let st = use_context::<St>();
    let failed = match &*st.fetch.read() {
        Fetch::Failed(e) => Some(e.clone()),
        _ => None,
    };
    rsx! {
        div { class: "opening",
            div { class: "opening-col",
                div { class: "opening-brand", "pullspace" }
                if let Some(e) = failed {
                    div { class: "opening-note", "Couldn't read the workspace: {e}" }
                    button {
                        class: "opening-stop",
                        onclick: move |_| {
                            spawn(load(st));
                        },
                        "Try again"
                    }
                } else {
                    div { class: "opening-bar" }
                    div { class: "opening-note", "Reading the workspace…" }
                }
            }
        }
    }
}
