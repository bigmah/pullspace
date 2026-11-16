//! The messages between pullspace's frame and the page it is embedded in — see the crate
//! documentation for how they travel.

use serde::{Deserialize, Serialize};

/// `t` of the frame's request for both sides' files, answered with a [`Snapshot`].
pub const SNAPSHOT: &str = "snapshot";
/// `t` of the frame's request for blob contents ([`BlobsRequest`]), answered with a
/// [`BlobsReply`].
pub const BLOBS: &str = "blobs";
/// `t` of the frame's [`Quote`], which has no answer.
pub const QUOTE: &str = "quote";
/// The `event` that tells the frame the files may have moved.
pub const CHANGED: &str = "changed";

/// What is being reviewed: two sides, every file in each.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// What to call the repository — the checkout's directory, usually.
    pub name: String,
    pub base: Side,
    pub head: Side,
}

/// One side: a name to show, something to file it under, and every file in it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Side {
    /// What the bar calls it: `HEAD`, `working tree`.
    pub label: String,
    /// A commit SHA where there is one. A working tree has none, so the host makes up
    /// something stable to name it by. Empty for a side with nothing in it.
    pub commit: String,
    pub files: Vec<Entry>,
}

/// One file of one side.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Relative to the repository, with `/` between components.
    pub path: String,
    /// git's blob hash of the contents, which is what the frame keeps them by.
    pub sha: String,
    /// Bytes. The frame checks what it is sent against this.
    pub size: u64,
}

/// Blobs the frame wants.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BlobsRequest {
    pub wants: Vec<Want>,
}

/// One blob, and the path it was listed at — where the host may find one that is only on
/// disk, like a working-tree file git hasn't stored.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Want {
    pub sha: String,
    pub path: String,
}

/// The blobs the host has, base64, by hash. One it doesn't have is left out (or `null`);
/// it may also send fewer than were asked for, and the frame asks again for the rest.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BlobsReply {
    pub blobs: std::collections::BTreeMap<String, Option<String>>,
}

/// A line the reader asked the host about.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Quote {
    pub path: String,
    /// 1-based.
    pub line: usize,
    /// The line itself, when the frame has the file as text.
    #[serde(default)]
    pub text: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_have_the_documented_shape() {
        let snap: Snapshot = serde_json::from_str(
            r#"{"name":"r","base":{"label":"HEAD","commit":"c","files":[{"path":"a","sha":"ab","size":3}]},
                "head":{"label":"working tree","commit":"w","files":[]}}"#,
        )
        .unwrap();
        assert_eq!(snap.base.files[0].size, 3);
        let req: BlobsRequest =
            serde_json::from_str(r#"{"wants":[{"sha":"ab","path":"a"}]}"#).unwrap();
        assert_eq!(req.wants[0].path, "a");
        let reply: BlobsReply =
            serde_json::from_str(r#"{"blobs":{"ab":"YQ==","cd":null}}"#).unwrap();
        assert_eq!(reply.blobs["ab"].as_deref(), Some("YQ=="));
        let quote: Quote = serde_json::from_str(r#"{"path":"a","line":3}"#).unwrap();
        assert_eq!(quote.text, None);
    }
}
