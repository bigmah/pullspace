pub mod auth;
pub mod blobs;
pub mod clone;
pub mod difftool;
pub mod github;
pub mod highlight;
pub mod http;
pub mod layout;
pub mod markdown;
pub mod opfs;
pub mod route;
pub mod scan;
pub mod search;
pub mod store;
pub mod symbols;
pub mod tree;
pub mod viewed;

/// One side of a diff. `Absent` covers "not in this commit" — the base side of
/// an added file, the head side of a deleted one — which the viewer treats as
/// an empty side either way.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum FileContent {
    Text(String),
    Binary,
    Absent,
}

impl FileContent {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        if looks_binary(bytes) {
            FileContent::Binary
        } else {
            FileContent::Text(String::from_utf8_lossy(bytes).into_owned())
        }
    }

    /// The text to diff. Absent sides diff as empty; binary has no text at all.
    pub fn text(&self) -> Option<&str> {
        match self {
            FileContent::Text(t) => Some(t),
            FileContent::Absent => Some(""),
            FileContent::Binary => None,
        }
    }
}

/// A NUL byte near the start is the same heuristic git uses.
pub fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|&b| b == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nul_bytes_mean_binary() {
        assert!(looks_binary(b"abc\0def"));
        assert!(!looks_binary(b"plain text"));
        // Only the first 8k is sampled.
        let mut late = vec![b'a'; 9000];
        late.push(0);
        assert!(!looks_binary(&late));
    }

    #[test]
    fn absent_diffs_as_empty_binary_does_not_diff() {
        assert_eq!(FileContent::Absent.text(), Some(""));
        assert_eq!(FileContent::Text("x".into()).text(), Some("x"));
        assert_eq!(FileContent::Binary.text(), None);
    }

    #[test]
    fn bytes_classify() {
        assert!(matches!(
            FileContent::from_bytes(b"hello"),
            FileContent::Text(_)
        ));
        assert!(matches!(
            FileContent::from_bytes(b"\0\0"),
            FileContent::Binary
        ));
    }
}
