use std::path::{Path, PathBuf};

use regex::Regex;

pub const MAX_HITS: usize = 400;
const MAX_FILE_BYTES: u64 = 1_500_000;

#[derive(Clone, PartialEq)]
pub struct Hit {
    pub path: PathBuf, // relative to repo root
    pub line: usize,   // 1-based
    pub text: String,  // trimmed preview
}

fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|&b| b == 0)
}

/// Regex search across all non-ignored text files under root. Capped at MAX_HITS.
pub fn search_repo(root: &Path, re: &Regex) -> Vec<Hit> {
    let mut hits = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .follow_links(false)
        .build();
    'outer: for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if entry.metadata().map(|m| m.len() > MAX_FILE_BYTES).unwrap_or(true) {
            continue;
        }
        let Ok(bytes) = std::fs::read(entry.path()) else {
            continue;
        };
        if looks_binary(&bytes) {
            continue;
        }
        let content = String::from_utf8_lossy(&bytes);
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        for (i, line) in content.lines().enumerate() {
            if re.is_match(line) {
                let mut preview = line.trim().to_string();
                if preview.len() > 200 {
                    preview.truncate(200);
                }
                hits.push(Hit {
                    path: rel.to_path_buf(),
                    line: i + 1,
                    text: preview,
                });
                if hits.len() >= MAX_HITS {
                    break 'outer;
                }
            }
        }
    }
    hits
}

/// Case-insensitive plain-text search.
pub fn text_query(query: &str) -> Option<Regex> {
    if query.trim().is_empty() {
        return None;
    }
    Regex::new(&format!("(?i){}", regex::escape(query))).ok()
}

/// Whole-word, case-sensitive search (used for references).
pub fn word_query(word: &str) -> Option<Regex> {
    if word.trim().is_empty() {
        return None;
    }
    Regex::new(&format!(r"\b{}\b", regex::escape(word))).ok()
}
