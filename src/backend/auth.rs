//! The GitHub token, and where a page can keep one.
//!
//! Sign-in here is a token you paste, and that is a constraint rather than a
//! shortcut. GitHub's OAuth endpoints — `github.com/login/device/code` and
//! `/login/oauth/access_token` — send no `Access-Control-Allow-Origin`, so no
//! page may call them; the web flow additionally needs a client secret, which
//! means a server holding it. Every OAuth route ends at infrastructure somebody
//! runs and everybody trusts with their tokens.
//!
//! A pasted token has neither problem. It lives in local storage on the origin
//! serving the page, and it goes to api.github.com and nowhere else.

use serde::{Deserialize, Serialize};

use super::store;

/// A GitHub token, as pasted.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Token {
    pub value: String,
}

impl std::fmt::Debug for Token {
    /// Never let a token reach a log line by accident.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Token")
            .field("value", &"<redacted>")
            .finish()
    }
}

/// Whatever was pasted last time.
///
/// There is no `$GITHUB_TOKEN` and no `gh` CLI to fall back on — a page can see
/// neither — so this is the only place a token comes from.
pub fn find_token() -> Option<Token> {
    store::get(store::TOKEN).map(|value| Token { value })
}

pub fn save_token(token: &str) {
    store::set(store::TOKEN, token);
}

pub fn sign_out() {
    store::remove(store::TOKEN);
}

/// Open a page in a new tab. Best effort: the UI shows the URL either way.
pub fn open_browser(url: &str) {
    if let Some(win) = web_sys::window() {
        let _ = win.open_with_url_and_target(url, "_blank");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_debug_hides_the_secret() {
        let t = Token {
            value: "ghp_supersecret".to_string(),
        };
        let shown = format!("{t:?}");
        assert!(!shown.contains("supersecret"), "{shown}");
    }
}
