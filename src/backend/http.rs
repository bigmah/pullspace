//! One GET, on the browser's fetch.
//!
//! fetch brings its own connection pool, cache and timeouts, so there is
//! nothing to configure here — which is most of why this module is short.
//!
//! GET is all there is, because reading is all pullspace does: it shows you
//! pull requests, it does not write them.

use anyhow::{Result, anyhow};
use gloo_net::http::Request;

/// What a request came back with.
///
/// The status rides alongside the body rather than becoming an error. A 404 is
/// a real answer here — it is what the base side of an added file looks like —
/// so which statuses count as failures is the caller's decision.
pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
    /// `x-ratelimit-remaining`, when it was sent. The one header worth lifting
    /// out: it separates "you may not see this" from "not so fast".
    pub rate_remaining: Option<String>,
    /// `x-ratelimit-resource`: which budget the number above belongs to.
    ///
    /// GitHub keeps more than one, and they are nothing like each other —
    /// `search` is ten a minute to an anonymous caller where `core` is sixty an
    /// hour. Running one out says nothing about the others, so an error that
    /// does not name the one that ran out is telling the reader to give up on
    /// an app that still works.
    pub rate_resource: Option<String>,
    /// `x-ratelimit-reset`: when the budget refills, in seconds since the epoch.
    pub rate_reset: Option<String>,
}

/// GET `url` with `headers`.
///
/// `User-Agent` is deliberately absent: browsers forbid setting it and send
/// their own.
pub async fn get(url: &str, headers: &[(&str, &str)]) -> Result<Reply> {
    let mut req = Request::get(url);
    for (name, value) in headers {
        req = req.header(name, value);
    }
    // A failure here happened before GitHub saw the request — the network, or a
    // cross-origin rule. The browser will not say which, on purpose, so neither
    // can we.
    let res = req
        .send()
        .await
        .map_err(|e| anyhow!("GET {url} never reached GitHub: {e}"))?;

    let status = res.status();
    let rate_remaining = res.headers().get("x-ratelimit-remaining");
    let rate_resource = res.headers().get("x-ratelimit-resource");
    let rate_reset = res.headers().get("x-ratelimit-reset");
    let body = res
        .binary()
        .await
        .map_err(|e| anyhow!("reading the answer to GET {url}: {e}"))?;

    Ok(Reply {
        status,
        body,
        rate_remaining,
        rate_resource,
        rate_reset,
    })
}
