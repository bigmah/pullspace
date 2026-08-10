use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

const MAX_FILE_BYTES: u64 = 1_500_000;

#[derive(Clone, PartialEq)]
pub struct Symbol {
    pub name: String,
    pub kind: &'static str,
    pub path: PathBuf, // relative to repo root
    pub line: usize,   // 1-based
    pub preview: String,
}

struct LangSpec {
    extensions: &'static [&'static str],
    patterns: Vec<(&'static str, Regex)>,
}

fn langs() -> &'static Vec<LangSpec> {
    static LANGS: OnceLock<Vec<LangSpec>> = OnceLock::new();
    LANGS.get_or_init(|| {
        let re = |s: &str| Regex::new(s).expect("bad symbol pattern");
        vec![
            LangSpec {
                extensions: &["rs"],
                patterns: vec![
                    ("fn", re(r#"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:default\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?(?:extern\s+"[^"]*"\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)"#)),
                    ("struct", re(r"^\s*(?:pub(?:\([^)]*\))?\s+)?struct\s+([A-Za-z_][A-Za-z0-9_]*)")),
                    ("enum", re(r"^\s*(?:pub(?:\([^)]*\))?\s+)?enum\s+([A-Za-z_][A-Za-z0-9_]*)")),
                    ("trait", re(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:unsafe\s+)?trait\s+([A-Za-z_][A-Za-z0-9_]*)")),
                    ("type", re(r"^\s*(?:pub(?:\([^)]*\))?\s+)?type\s+([A-Za-z_][A-Za-z0-9_]*)")),
                    ("mod", re(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)")),
                    ("const", re(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:const|static)\s+([A-Za-z_][A-Za-z0-9_]*)\s*:")),
                    ("macro", re(r"^\s*macro_rules!\s+([A-Za-z_][A-Za-z0-9_]*)")),
                ],
            },
            LangSpec {
                extensions: &["js", "jsx", "ts", "tsx", "mjs", "cjs"],
                patterns: vec![
                    ("fn", re(r"^\s*(?:export\s+)?(?:default\s+)?(?:async\s+)?function\s*\*?\s*([A-Za-z_$][A-Za-z0-9_$]*)")),
                    ("class", re(r"^\s*(?:export\s+)?(?:default\s+)?(?:abstract\s+)?class\s+([A-Za-z_$][A-Za-z0-9_$]*)")),
                    ("var", re(r"^\s*(?:export\s+)?(?:const|let|var)\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*=")),
                    ("interface", re(r"^\s*(?:export\s+)?interface\s+([A-Za-z_$][A-Za-z0-9_$]*)")),
                    ("type", re(r"^\s*(?:export\s+)?type\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*=")),
                    ("enum", re(r"^\s*(?:export\s+)?(?:const\s+)?enum\s+([A-Za-z_$][A-Za-z0-9_$]*)")),
                ],
            },
            LangSpec {
                extensions: &["py"],
                patterns: vec![
                    ("fn", re(r"^\s*(?:async\s+)?def\s+([A-Za-z_][A-Za-z0-9_]*)")),
                    ("class", re(r"^\s*class\s+([A-Za-z_][A-Za-z0-9_]*)")),
                ],
            },
            LangSpec {
                extensions: &["go"],
                patterns: vec![
                    ("fn", re(r"^func\s+(?:\([^)]*\)\s*)?([A-Za-z_][A-Za-z0-9_]*)")),
                    ("type", re(r"^type\s+([A-Za-z_][A-Za-z0-9_]*)")),
                    ("var", re(r"^(?:var|const)\s+([A-Za-z_][A-Za-z0-9_]*)")),
                ],
            },
            LangSpec {
                extensions: &["rb"],
                patterns: vec![
                    ("fn", re(r"^\s*def\s+(?:self\.)?([A-Za-z_][A-Za-z0-9_?!]*)")),
                    ("class", re(r"^\s*(?:class|module)\s+([A-Za-z_][A-Za-z0-9_]*)")),
                ],
            },
            LangSpec {
                extensions: &["java", "kt", "swift", "cs"],
                patterns: vec![
                    ("class", re(r"^\s*(?:public\s+|private\s+|internal\s+|open\s+|final\s+|abstract\s+|sealed\s+|static\s+)*(?:class|interface|enum|struct|protocol|object|record)\s+([A-Za-z_][A-Za-z0-9_]*)")),
                    ("fn", re(r"^\s*(?:public\s+|private\s+|protected\s+|internal\s+|open\s+|override\s+|static\s+|final\s+|async\s+)*func?\s+([A-Za-z_][A-Za-z0-9_]*)\s*[(<]")),
                ],
            },
        ]
    })
}

fn spec_for(ext: &str) -> Option<&'static LangSpec> {
    langs().iter().find(|l| l.extensions.contains(&ext))
}

/// Build a repo-wide symbol index with lightweight per-language regex patterns.
pub fn build_index(root: &Path) -> Vec<Symbol> {
    let mut out = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .follow_links(false)
        .build();
    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Some(ext) = entry.path().extension().and_then(|e| e.to_str()) else {
            continue;
        };
        let Some(spec) = spec_for(ext) else {
            continue;
        };
        if entry.metadata().map(|m| m.len() > MAX_FILE_BYTES).unwrap_or(true) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        for (i, line) in content.lines().enumerate() {
            for (kind, re) in &spec.patterns {
                if let Some(cap) = re.captures(line) {
                    if let Some(m) = cap.get(1) {
                        out.push(Symbol {
                            name: m.as_str().to_string(),
                            kind,
                            path: rel.to_path_buf(),
                            line: i + 1,
                            preview: line.trim().to_string(),
                        });
                        break;
                    }
                }
            }
        }
    }
    out
}

pub fn find_definitions(index: &[Symbol], name: &str) -> Vec<Symbol> {
    index.iter().filter(|s| s.name == name).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds_for(ext: &str, line: &str) -> Option<(&'static str, String)> {
        let spec = spec_for(ext)?;
        for (kind, re) in &spec.patterns {
            if let Some(cap) = re.captures(line) {
                if let Some(m) = cap.get(1) {
                    return Some((kind, m.as_str().to_string()));
                }
            }
        }
        None
    }

    #[test]
    fn rust_patterns() {
        assert_eq!(kinds_for("rs", "pub async fn fetch_data() {"), Some(("fn", "fetch_data".into())));
        assert_eq!(kinds_for("rs", "pub(crate) struct Foo {"), Some(("struct", "Foo".into())));
        assert_eq!(kinds_for("rs", "    fn helper() {"), Some(("fn", "helper".into())));
        assert_eq!(kinds_for("rs", "pub const MAX: usize = 3;"), Some(("const", "MAX".into())));
        assert_eq!(kinds_for("rs", "let x = call();"), None);
    }

    #[test]
    fn ts_and_py_patterns() {
        assert_eq!(kinds_for("ts", "export default async function main() {"), Some(("fn", "main".into())));
        assert_eq!(kinds_for("ts", "export const config = {"), Some(("var", "config".into())));
        assert_eq!(kinds_for("py", "    async def run(self):"), Some(("fn", "run".into())));
        assert_eq!(kinds_for("py", "class Widget:"), Some(("class", "Widget".into())));
    }
}
