//! The address bar, in the other direction: what it says, opened.
//!
//! Writing it is [`St::enter`](super::app::St) and
//! [`close_workspace`](super::app::St::close_workspace), which are the two
//! moments what is on screen changes. This is everything that has to happen
//! when the *bar* is what changed: the page loaded on a link somebody sent,
//! Back and Forward, or a URL edited in place.
//!
//! Between them the pair make the ordinary web contract hold — a review has an
//! address, the address survives a reload, and the browser's own back button
//! walks the pull requests you have opened.

use std::time::Duration;

use dioxus::prelude::*;

use crate::backend::github::RepoRef;
use crate::backend::route::{self, Route};

use super::app::{Account, St};
use super::compat;
use super::github::{browse_repo, open_pr};

/// How often to look in on the saved token while it is being checked. Short
/// enough not to be felt on the way in to a link.
const POLL: Duration = Duration::from_millis(50);

/// One listener, on the window, for as long as the app is up.
///
/// `hashchange` is what Back and Forward raise on a page that routes in the
/// fragment, and it is also what our own writes raise — which is why the loop
/// below checks what is already open before fetching anything.
const HASH_JS: &str = r#"
(function () {
  // A reload of this page's script should not leave the last listener behind.
  if (window.__pullspace_hash) window.__pullspace_hash();
  var on = function () { dioxus.send(location.hash || ''); };
  window.addEventListener('hashchange', on);
  window.__pullspace_hash = function () {
    window.removeEventListener('hashchange', on);
  };
})();
"#;

/// Follow the address bar: Back, Forward, and a link pasted into a tab that is
/// already open.
pub async fn watch(st: St) {
    let mut eval = document::eval(HASH_JS);
    while let Ok(hash) = eval.recv::<String>().await {
        let asked = route::parse(&hash);
        // Our own writes come back through here. What is already on screen is
        // nothing to go and fetch — and reloading it under somebody would throw
        // away the file they were reading.
        if asked == st.workspace.peek().route() {
            continue;
        }
        go(st, asked);
    }
}

/// Open whatever the page was opened on.
///
/// It waits for the saved token to be checked first, however the check comes
/// out. A private pull request asked for anonymously comes back 404, and a link
/// to one is exactly the sort of link somebody pastes into a fresh tab.
pub async fn landing(st: St) {
    while matches!(*st.account.peek(), Account::Checking) {
        compat::sleep(POLL).await;
    }
    // Read now rather than at mount: nothing writes the bar before this runs,
    // and reading it here is one less thing to keep in step.
    match route::current() {
        // Nothing linked to. The picker is already up, which is the whole of
        // what "home" means here.
        Route::Home => {}
        asked => go(st, asked),
    }
}

/// Go where the address bar says.
fn go(st: St, asked: Route) {
    match asked {
        Route::Home => st.close_workspace(),
        Route::Repo(repo) => {
            name_it(st, &repo);
            // Root scope: a load outlives whatever component happens to be on
            // screen when the URL changes.
            spawn_forever(browse_repo(st, repo));
        }
        Route::Pr(repo, number) => {
            name_it(st, &repo);
            spawn_forever(open_pr(st, repo, number));
        }
    }
}

/// Put the repository in the picker's box on the way past, so that opening the
/// panel after arriving on a link offers that repository's pull requests rather
/// than a blank box and a stranger's guess at what you wanted.
fn name_it(st: St, repo: &RepoRef) {
    let mut input = st.repo_input;
    input.set(repo.to_string());
}
