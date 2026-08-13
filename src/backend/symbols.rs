//! Where things are defined, worked out by reading rather than by compiling.
//!
//! There is no language server here and there is not going to be one — this is
//! a page, and the repository it is looking at was downloaded ninety seconds
//! ago. What there is instead is a line-at-a-time regex per language, of the
//! sort `ctags` has used for thirty years. It knows that `pub async fn foo(`
//! defines `foo` and nothing whatever about what `foo` means.
//!
//! Which is worth being clear about, because the failure mode is not an error:
//! two `new`s in a repository are two definitions and the index cannot say
//! which one you meant. So Go to Definition offers the list when there is more
//! than one, rather than picking. Within one file of one language it is right
//! nearly always; across a repository it is a very good guess, delivered
//! instantly, on a machine that has no compiler on it.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

/// One definition.
#[derive(Clone, PartialEq, Debug)]
pub struct Symbol {
    pub name: String,
    /// `fn`, `struct`, `class` — whatever the language calls it.
    pub kind: &'static str,
    /// Repo-relative.
    pub path: PathBuf,
    /// 1-based.
    pub line: usize,
    /// The defining line, trimmed — enough to tell two `new`s apart in a list.
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
                extensions: &["js", "jsx", "ts", "tsx", "mjs", "cjs", "mts", "cts"],
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
                extensions: &["py", "pyi"],
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
                extensions: &["java", "kt", "kts", "swift", "cs", "scala"],
                patterns: vec![
                    ("class", re(r"^\s*(?:public\s+|private\s+|internal\s+|open\s+|final\s+|abstract\s+|sealed\s+|static\s+)*(?:class|interface|enum|struct|protocol|object|record)\s+([A-Za-z_][A-Za-z0-9_]*)")),
                    ("fn", re(r"^\s*(?:public\s+|private\s+|protected\s+|internal\s+|open\s+|override\s+|static\s+|final\s+|async\s+)*func?\s+([A-Za-z_][A-Za-z0-9_]*)\s*[(<]")),
                ],
            },
            LangSpec {
                extensions: &["c", "h", "cc", "cpp", "cxx", "hpp", "hh"],
                patterns: vec![
                    ("class", re(r"^\s*(?:class|struct|union)\s+([A-Za-z_][A-Za-z0-9_]*)\s*(?:final\s*)?[:{]")),
                    ("type", re(r"^\s*(?:typedef\s+.*\s|using\s+)([A-Za-z_][A-Za-z0-9_]*)\s*[=;]")),
                    ("macro", re(r"^\s*#\s*define\s+([A-Za-z_][A-Za-z0-9_]*)")),
                    // A definition, not a declaration: it has to open a body on
                    // the same line, or every prototype in every header is one.
                    ("fn", re(r"^[A-Za-z_][A-Za-z0-9_ \*&:<>,]*\s[\*&]?([A-Za-z_][A-Za-z0-9_]*)\s*\([^;]*\)\s*(?:const\s*)?\{")),
                ],
            },
            LangSpec {
                extensions: &["php"],
                patterns: vec![
                    ("fn", re(r"^\s*(?:(?:public|private|protected|static|final|abstract)\s+)*function\s+&?([A-Za-z_][A-Za-z0-9_]*)")),
                    ("class", re(r"^\s*(?:abstract\s+|final\s+)*(?:class|interface|trait|enum)\s+([A-Za-z_][A-Za-z0-9_]*)")),
                ],
            },
            LangSpec {
                extensions: &["sh", "bash", "zsh"],
                patterns: vec![
                    ("fn", re(r"^\s*(?:function\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*\(\s*\)\s*\{")),
                ],
            },
            LangSpec {
                extensions: &["sql"],
                patterns: vec![
                    ("table", re(r"(?i)^\s*create\s+(?:or\s+replace\s+)?(?:temp(?:orary)?\s+)?table\s+(?:if\s+not\s+exists\s+)?([A-Za-z_][A-Za-z0-9_.]*)")),
                    ("view", re(r"(?i)^\s*create\s+(?:or\s+replace\s+)?view\s+([A-Za-z_][A-Za-z0-9_.]*)")),
                    ("fn", re(r"(?i)^\s*create\s+(?:or\s+replace\s+)?(?:function|procedure)\s+([A-Za-z_][A-Za-z0-9_.]*)")),
                ],
            },
        ]
    })
}

fn spec_for(ext: &str) -> Option<&'static LangSpec> {
    let ext = ext.to_ascii_lowercase();
    langs()
        .iter()
        .find(|l| l.extensions.contains(&ext.as_str()))
}

/// Read one file's definitions into `out`.
///
/// One symbol per line at most: the first pattern that matches wins, so a line
/// is a `struct` or a `type` but never both.
pub fn scan_file(path: &Path, text: &str, out: &mut Vec<Symbol>) {
    let Some(spec) = path.extension().and_then(|e| e.to_str()).and_then(spec_for) else {
        return;
    };
    for (i, line) in text.lines().enumerate() {
        for (kind, re) in &spec.patterns {
            if let Some(m) = re.captures(line).and_then(|c| c.get(1)) {
                out.push(Symbol {
                    name: m.as_str().to_string(),
                    kind,
                    path: path.to_path_buf(),
                    line: i + 1,
                    preview: line.trim().to_string(),
                });
                break;
            }
        }
    }
}

/// Every definition of `name`, nearest first.
///
/// "Nearest" is the file being read, if the caller says which: a `render` in
/// the file you are looking at is nearly always the `render` you meant, and
/// putting it first is what makes Go to Definition land in one jump instead of
/// opening a list of eleven.
pub fn find_definitions(index: &[Symbol], name: &str, near: Option<&Path>) -> Vec<Symbol> {
    let mut found: Vec<Symbol> = index.iter().filter(|s| s.name == name).cloned().collect();
    found.sort_by_key(|s| {
        let here = near.is_some_and(|p| p == s.path);
        (!here, s.path.clone(), s.line)
    });
    found
}

/// Sort an index into the order the panels want to show it in.
pub fn sorted(mut index: Vec<Symbol>) -> Vec<Symbol> {
    index.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first(ext: &str, line: &str) -> Option<(&'static str, String)> {
        let mut out = Vec::new();
        scan_file(Path::new(&format!("x.{ext}")), line, &mut out);
        out.pop().map(|s| (s.kind, s.name))
    }

    #[test]
    fn rust_patterns() {
        assert_eq!(
            first("rs", "pub async fn fetch_data() {"),
            Some(("fn", "fetch_data".into()))
        );
        assert_eq!(
            first("rs", "pub(crate) struct Foo {"),
            Some(("struct", "Foo".into()))
        );
        assert_eq!(
            first("rs", "    fn helper() {"),
            Some(("fn", "helper".into()))
        );
        assert_eq!(
            first("rs", "pub const MAX: usize = 3;"),
            Some(("const", "MAX".into()))
        );
        assert_eq!(first("rs", "let x = call();"), None);
    }

    #[test]
    fn ts_and_py_patterns() {
        assert_eq!(
            first("ts", "export default async function main() {"),
            Some(("fn", "main".into()))
        );
        assert_eq!(
            first("ts", "export const config = {"),
            Some(("var", "config".into()))
        );
        assert_eq!(
            first("py", "    async def run(self):"),
            Some(("fn", "run".into()))
        );
        assert_eq!(
            first("py", "class Widget:"),
            Some(("class", "Widget".into()))
        );
    }

    #[test]
    fn a_c_prototype_is_not_a_definition() {
        assert_eq!(
            first("c", "int add(int a, int b) {"),
            Some(("fn", "add".into()))
        );
        assert_eq!(first("h", "int add(int a, int b);"), None);
        assert_eq!(
            first("c", "#define MAX_LEN 32"),
            Some(("macro", "MAX_LEN".into()))
        );
    }

    #[test]
    fn an_unknown_extension_yields_nothing_rather_than_guessing() {
        assert_eq!(first("txt", "fn thing() {"), None);
        // A file with no extension at all is not something to guess about.
        let mut out = Vec::new();
        scan_file(Path::new("Makefile"), "build:\n", &mut out);
        assert!(out.is_empty());
        // And an extension is an extension whatever case it arrives in.
        assert_eq!(
            first("TSX", "export class Widget {"),
            Some(("class", "Widget".into()))
        );
    }

    #[test]
    fn one_line_defines_one_thing() {
        let mut out = Vec::new();
        scan_file(
            Path::new("a.rs"),
            "pub struct Foo;\npub enum Bar {}\n",
            &mut out,
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].line, 1);
        assert_eq!(out[1].line, 2);
        assert_eq!(out[1].preview, "pub enum Bar {}");
    }

    #[test]
    fn the_definition_in_the_file_being_read_comes_first() {
        let index = vec![
            Symbol {
                name: "new".into(),
                kind: "fn",
                path: "a/z.rs".into(),
                line: 9,
                preview: String::new(),
            },
            Symbol {
                name: "new".into(),
                kind: "fn",
                path: "b/y.rs".into(),
                line: 3,
                preview: String::new(),
            },
            Symbol {
                name: "other".into(),
                kind: "fn",
                path: "b/y.rs".into(),
                line: 1,
                preview: String::new(),
            },
        ];
        let found = find_definitions(&index, "new", Some(Path::new("b/y.rs")));
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].path, PathBuf::from("b/y.rs"));

        // With nowhere in particular to be near, it is path order.
        let found = find_definitions(&index, "new", None);
        assert_eq!(found[0].path, PathBuf::from("a/z.rs"));
        assert!(find_definitions(&index, "missing", None).is_empty());
    }
}
