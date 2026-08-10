//! GitHub sign-in: OAuth device flow, plus token discovery from the
//! environment and the `gh` CLI so the app is usable before registering an
//! OAuth app of your own.
//!
//! The device flow is the right grant for a desktop app: it needs no client
//! secret and no loopback redirect, so nothing confidential ships in the
//! binary. It does need a client id, which is public by design — see
//! [`client_id`] for where that comes from.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

/// `repo` covers private repositories and PR contents; `read:org` lets the
/// picker see repos behind an org.
pub const SCOPE: &str = "repo read:org";

const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(30)))
        .user_agent("pullspace")
        .build()
        .into()
}

/// `$XDG_CONFIG_HOME/pullspace`, else `~/.config/pullspace` (`%APPDATA%` on
/// Windows). Matches where `gh` keeps its own config.
pub fn config_dir() -> Option<PathBuf> {
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME")
        && !x.is_empty()
    {
        return Some(PathBuf::from(x).join("pullspace"));
    }
    #[cfg(windows)]
    {
        dirs::config_dir().map(|d| d.join("pullspace"))
    }
    #[cfg(not(windows))]
    {
        dirs::home_dir().map(|h| h.join(".config").join("pullspace"))
    }
}

fn auth_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("auth.json"))
}

fn config_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("config.json"))
}

/// Write with owner-only permissions — this file holds a bearer token.
fn write_private(path: &PathBuf, body: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(path, body).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("securing {}", path.display()))?;
    }
    Ok(())
}

// ---------------------------------------------------------------- client id

#[derive(Default, Serialize, Deserialize)]
struct AppConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_id: Option<String>,
}

/// The OAuth app's client id. Public information — it identifies the app, it
/// does not authorise anything on its own.
///
/// `PULLSPACE_GITHUB_CLIENT_ID` wins so a checkout can be pointed at a
/// different app without touching the saved config.
pub fn client_id() -> Option<String> {
    if let Ok(v) = std::env::var("PULLSPACE_GITHUB_CLIENT_ID") {
        let v = v.trim().to_string();
        if !v.is_empty() {
            return Some(v);
        }
    }
    let path = config_path()?;
    let raw = std::fs::read_to_string(path).ok()?;
    let cfg: AppConfig = serde_json::from_str(&raw).ok()?;
    cfg.client_id.filter(|v| !v.trim().is_empty())
}

pub fn save_client_id(id: &str) -> Result<()> {
    let path = config_path().ok_or_else(|| anyhow!("no config directory available"))?;
    let cfg = AppConfig {
        client_id: Some(id.trim().to_string()),
    };
    write_private(&path, &serde_json::to_string_pretty(&cfg)?)
}

// ------------------------------------------------------------------- tokens

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TokenSource {
    /// Signed in through the device flow, stored in `auth.json`.
    Stored,
    /// `GITHUB_TOKEN` or `GH_TOKEN`.
    Env(&'static str),
    /// Borrowed from an authenticated `gh` CLI.
    GhCli,
}

impl TokenSource {
    pub fn label(&self) -> String {
        match self {
            TokenSource::Stored => "signed in".to_string(),
            TokenSource::Env(name) => format!("${name}"),
            TokenSource::GhCli => "gh cli".to_string(),
        }
    }

    /// Only a token we stored ourselves can be signed out from in-app.
    pub fn revocable(&self) -> bool {
        matches!(self, TokenSource::Stored)
    }
}

#[derive(Clone, PartialEq)]
pub struct Token {
    pub value: String,
    pub source: TokenSource,
}

impl std::fmt::Debug for Token {
    /// Never let a token reach a log line by accident.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Token")
            .field("value", &"<redacted>")
            .field("source", &self.source)
            .finish()
    }
}

#[derive(Serialize, Deserialize)]
struct StoredToken {
    access_token: String,
}

fn stored_token() -> Option<String> {
    let raw = std::fs::read_to_string(auth_path()?).ok()?;
    let stored: StoredToken = serde_json::from_str(&raw).ok()?;
    Some(stored.access_token).filter(|t| !t.is_empty())
}

fn env_token() -> Option<(String, &'static str)> {
    for name in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(v) = std::env::var(name) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return Some((v, name));
            }
        }
    }
    None
}

fn gh_cli_token() -> Option<String> {
    let out = Command::new("gh").args(["auth", "token"]).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let token = String::from_utf8(out.stdout).ok()?.trim().to_string();
    Some(token).filter(|t| !t.is_empty())
}

/// Find a usable token. An in-app sign-in takes precedence over ambient
/// credentials, so signing in has a visible effect even when `GITHUB_TOKEN`
/// happens to be set.
pub fn find_token() -> Option<Token> {
    if let Some(value) = stored_token() {
        return Some(Token {
            value,
            source: TokenSource::Stored,
        });
    }
    if let Some((value, name)) = env_token() {
        return Some(Token {
            value,
            source: TokenSource::Env(name),
        });
    }
    gh_cli_token().map(|value| Token {
        value,
        source: TokenSource::GhCli,
    })
}

pub fn save_token(access_token: &str) -> Result<()> {
    let path = auth_path().ok_or_else(|| anyhow!("no config directory available"))?;
    let body = serde_json::to_string_pretty(&StoredToken {
        access_token: access_token.to_string(),
    })?;
    write_private(&path, &body)
}

/// Forget the stored token. Leaves env/`gh` credentials alone — they are not
/// ours to remove.
pub fn sign_out() -> Result<()> {
    let Some(path) = auth_path() else {
        return Ok(());
    };
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {}", path.display())),
    }
}

// -------------------------------------------------------------- device flow

#[derive(Clone, PartialEq, Deserialize)]
pub struct DeviceCode {
    /// Secret half — posted back when polling, never shown to the user.
    pub device_code: String,
    /// The short code the user types into GitHub.
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    /// Minimum seconds between polls, per GitHub.
    pub interval: u64,
}

#[derive(Deserialize)]
struct OAuthError {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

impl OAuthError {
    fn message(&self) -> String {
        match self.error.as_str() {
            "device_flow_disabled" => {
                "This OAuth app does not have device flow enabled. Turn on \
                 \"Enable Device Flow\" in the app's settings on GitHub."
                    .to_string()
            }
            "incorrect_client_credentials" => {
                "GitHub did not recognise that client id.".to_string()
            }
            _ => self
                .error_description
                .clone()
                .unwrap_or_else(|| self.error.clone()),
        }
    }
}

/// Step 1: ask GitHub for a code pair. The user enters `user_code` at
/// `verification_uri`; we poll with `device_code`.
pub fn start_device_flow(client_id: &str) -> Result<DeviceCode> {
    let mut res = agent()
        .post(DEVICE_CODE_URL)
        .header("Accept", "application/json")
        .send_form([("client_id", client_id), ("scope", SCOPE)])
        .context("requesting a device code from GitHub")?;

    let status = res.status().as_u16();
    let body = res.body_mut().read_to_string()?;

    if let Ok(err) = serde_json::from_str::<OAuthError>(&body) {
        bail!("{}", err.message());
    }
    if status != 200 {
        bail!("GitHub returned HTTP {status} when starting sign-in");
    }
    serde_json::from_str(&body).context("parsing GitHub's device code response")
}

pub enum PollOutcome {
    /// User has not finished authorising yet.
    Pending,
    /// We polled too fast; `interval` is the new floor.
    SlowDown { interval: u64 },
    Token(String),
    Denied,
    Expired,
}

#[derive(Deserialize)]
struct AccessTokenOk {
    access_token: String,
}

/// Step 2: exchange the device code for a token, once the user has approved.
/// Call no more often than the `interval` from [`start_device_flow`].
pub fn poll_device_flow(client_id: &str, device_code: &str) -> Result<PollOutcome> {
    let mut res = agent()
        .post(ACCESS_TOKEN_URL)
        .header("Accept", "application/json")
        .send_form([
            ("client_id", client_id),
            ("device_code", device_code),
            ("grant_type", GRANT_TYPE),
        ])
        .context("polling GitHub for the access token")?;

    let body = res.body_mut().read_to_string()?;

    if let Ok(ok) = serde_json::from_str::<AccessTokenOk>(&body) {
        return Ok(PollOutcome::Token(ok.access_token));
    }
    let err: OAuthError =
        serde_json::from_str(&body).context("parsing GitHub's access token response")?;
    Ok(match err.error.as_str() {
        "authorization_pending" => PollOutcome::Pending,
        // GitHub asks for +5s on each slow_down.
        "slow_down" => PollOutcome::SlowDown { interval: 5 },
        "expired_token" => PollOutcome::Expired,
        "access_denied" => PollOutcome::Denied,
        _ => bail!("{}", err.message()),
    })
}

/// Open the verification page in the user's browser. Best-effort: the UI shows
/// the URL either way.
pub fn open_browser(url: &str) {
    let _ = open::that_detached(url);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_code_parses() {
        let raw = r#"{"device_code":"dc","user_code":"ABCD-1234",
            "verification_uri":"https://github.com/login/device",
            "expires_in":900,"interval":5}"#;
        let dc: DeviceCode = serde_json::from_str(raw).unwrap();
        assert_eq!(dc.user_code, "ABCD-1234");
        assert_eq!(dc.interval, 5);
    }

    #[test]
    fn pending_is_not_an_error() {
        let err: OAuthError =
            serde_json::from_str(r#"{"error":"authorization_pending"}"#).unwrap();
        assert_eq!(err.error, "authorization_pending");
    }

    #[test]
    fn device_flow_disabled_explains_the_fix() {
        let err: OAuthError = serde_json::from_str(r#"{"error":"device_flow_disabled"}"#).unwrap();
        assert!(err.message().contains("Enable Device Flow"));
    }

    #[test]
    fn token_debug_hides_the_secret() {
        let t = Token {
            value: "ghu_supersecret".to_string(),
            source: TokenSource::Stored,
        };
        let shown = format!("{t:?}");
        assert!(!shown.contains("supersecret"), "{shown}");
    }

    #[test]
    fn only_stored_tokens_are_revocable() {
        assert!(TokenSource::Stored.revocable());
        assert!(!TokenSource::GhCli.revocable());
        assert!(!TokenSource::Env("GITHUB_TOKEN").revocable());
    }
}
