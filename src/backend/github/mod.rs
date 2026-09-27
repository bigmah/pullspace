//! Minimal GitHub REST client: enough to list pull requests and pull the two
//! sides of a file so the existing diff engine can render them.
//!
//! Every call here is async, and none of them touches the machine: the only
//! transport is [`http`](super::http), which is the browser's own fetch. That
//! is what lets a static page talk to GitHub with no server of its own in
//! between — and, since nothing outside that one module is browser-specific,
//! it is also what keeps the parsing in here testable on the host.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::FileContent;
use super::http;
use super::tree::ChangeKind;

const API: &str = "https://api.github.com";
const API_VERSION: &str = "2022-11-28";
/// Public file bytes, straight from the CDN.
///
/// Worth the special case twice over: it is not metered against the API's rate
/// limit, and it answers `Access-Control-Allow-Origin: *`, so the static web
/// build can read any public repository from a browser with no token at all.
/// It takes no `Authorization` header — sending one would turn a plain
/// cross-origin GET into a preflighted one, which this host does not answer —
/// so private repositories go the long way, through [`API`].
const RAW: &str = "https://raw.githubusercontent.com";
/// GitHub itself stops at 3000 files per PR; 100 per page.
const MAX_FILE_PAGES: u32 = 30;
/// 100 comments per page, per kind. A thread past this is one nobody is
/// reading to the end of anyway, and the pane says when it was cut short.
const MAX_COMMENT_PAGES: u32 = 5;
/// GitHub answers at most 250 commits on a pull request, so three pages is
/// every one there is to have from this endpoint.
const MAX_COMMIT_PAGES: u32 = 3;
/// And at most 300 files on a single commit, which is the same three pages.
const MAX_COMMIT_FILE_PAGES: u32 = 3;
/// And at most 300 on a comparison — all on the first page of it, whatever
/// `per_page` says, which is why this is a count and not a number of pages.
const MAX_COMPARE_FILES: usize = 300;
/// 100 check runs a page. Three pages is three hundred jobs on one commit —
/// past anything a person reads the results of one by one, and the panel says
/// when it stopped there.
const MAX_CHECK_PAGES: u32 = 3;
/// And 200 marked-up lines from any one check. A build that failed in two
/// hundred places has said what it has to say.
const MAX_ANNOTATION_PAGES: u32 = 2;
/// How many pull requests one listing holds. GitHub's own maximum per page,
/// asked for in one request — a second page of a list nobody scrolls to the
/// bottom of is not worth the wait or the budget.
pub const PR_PAGE: usize = 100;
/// 100 branches a page — GitHub's own maximum, which is what every list here
/// asks for.
pub const BRANCH_PAGE: usize = 100;
/// Three pages of them to begin with. More than any list is read down in one
/// go, and the pane offers the next page rather than stopping there — see
/// [`list_branches`].
const MAX_BRANCH_PAGES: u32 = 3;
/// How many commits of a branch's history arrive at once. A branch is not a
/// pull request: there is no end to it, so it comes down a page at a time and
/// the list asks for the next one when somebody scrolls to the bottom of this
/// one.
pub const HISTORY_PAGE: usize = 100;

mod checks;
mod commits;
mod conversation;
mod fetch;
mod find;
mod more;
mod pulls;
mod repo;
mod request;
mod snapshot;
mod target;
#[cfg(test)]
mod tests;
mod time;
mod writes;

pub use checks::*;
pub use commits::*;
pub use conversation::*;
pub use fetch::*;
pub use find::*;
pub use more::*;
pub use pulls::*;
pub use repo::*;
pub use request::*;
pub use snapshot::*;
pub use target::*;
pub use time::*;
pub use writes::*;
