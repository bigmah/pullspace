//! Pictures out of the repository, turned into something a page can draw.
//!
//! Everything here is about one problem. A picture in a README is named by a
//! path — `![a diff](diff.png)` — and there is no server underneath this app to
//! resolve that path against. The file is in the repository, which means it is
//! on GitHub or in the local store, and the only way to put it on screen is to
//! carry its bytes into the document itself. So that is what this does: the
//! bytes become a `data:` URL, and the `<img>` drawn around it asks nobody for
//! anything.
//!
//! Which is also why the URL is built here rather than pointed at
//! `raw.githubusercontent.com` directly. A private repository serves nothing to
//! an unauthenticated `<img>`, the store on disk already holds most of what is
//! being read, and a page that fetches its pictures from a URL is a page that
//! has to be trusted about which URL.
//!
//! Images that live somewhere else — a badge on shields.io, a screenshot on
//! somebody's CDN — are deliberately *not* drawn. The same renderer draws pull
//! request comments, and a comment is text a stranger wrote: an `<img>` in one
//! is a request to whatever host it names, from the tab holding a GitHub token,
//! reporting who read the pull request and when. GitHub proxies those through
//! camo for exactly this reason and a static page has nothing to proxy with, so
//! the honest answer is the alt text and a link.

use std::ops::Range;
use std::path::Path;

/// The most a picture may weigh before it is left as alt text.
///
/// It is carried in memory twice over — the bytes, then the base64 of them,
/// which is a third longer again — and it is carried inside the document's own
/// markup. A screenshot is a few hundred kilobytes; something past this is a
/// video that lost its way, and drawing it costs more than seeing it is worth.
pub const MAX_IMAGE_BYTES: u64 = 8 << 20;

/// What a browser is told a file is, by the name it is filed under.
///
/// Extensions rather than magic bytes on purpose: this is asked before anything
/// is fetched, and being able to say "not a picture" without a request is the
/// point. Nothing is claimed about the contents — an `<img>` that turns out to
/// hold something else simply does not draw.
///
/// SVG is here and is safe here for one specific reason: script inside an SVG
/// does not run when the SVG is loaded through an `<img>`. Every drawing path
/// in this app is an `<img>`; none of them is an `<object>`, an `<embed>`, or
/// an inline `<svg>`, and that is not an accident.
pub fn media_type(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "svg" => "image/svg+xml",
        _ => return None,
    })
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// A whole file as a URL: its bytes in base64, with their type on the front.
pub fn data_uri(mime: &str, bytes: &[u8]) -> String {
    // Sized up front. This runs over megabytes, and growing a string that long
    // by doubling is the difference between one allocation and twenty.
    let mut out = String::with_capacity(mime.len() + 14 + bytes.len().div_ceil(3) * 4);
    out.push_str("data:");
    out.push_str(mime);
    out.push_str(";base64,");
    for chunk in bytes.chunks(3) {
        let trio = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let packed = u32::from(trio[0]) << 16 | u32::from(trio[1]) << 8 | u32::from(trio[2]);
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            // The last group is padded when the input did not divide into
            // three: two bytes make three characters and an `=`, one makes two.
            if i > chunk.len() {
                out.push('=');
            } else {
                out.push(ALPHABET[(packed >> shift) as usize & 63] as char);
            }
        }
    }
    out
}

/// A picture that is not there, drawn as nothing at all.
///
/// One transparent pixel. What a `src` this app could not resolve is replaced
/// with inside a previewed page: left as it was written, the browser would
/// resolve it against *this* app's URL and fetch some 404 of ours, and the
/// broken-image icon it drew afterwards would be blamed on the repository.
pub const BLANK: &str =
    "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7";

/// One `src` in a page being previewed: what it points at, and where in the
/// document it says so.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Ref {
    /// The URL alone — not the attribute, not the quotes around it — so
    /// swapping one out is a splice and the rest of the file is untouched.
    pub at: Range<usize>,
    pub url: String,
}

/// Every picture an HTML file asks for.
///
/// A scanner rather than a parser, and it stays one: nothing here is
/// interpreting the document, executing it, or handing it to anybody who does.
/// It reads `src` off `<img>` and `<source>`, and the candidates out of a
/// `srcset` — which between them is how a README's screenshots and its
/// light/dark `<picture>` pairs are written — and everything else in the file
/// goes to the preview exactly as it arrived.
pub fn image_refs(html: &str) -> Vec<Ref> {
    let bytes = html.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(lt) = memchr(bytes, i, b'<') {
        // A comment is not markup, and a `<img>` inside one is an example
        // somebody wrote about, not a picture the page draws.
        if bytes[lt..].starts_with(b"<!--") {
            i = find(bytes, lt + 4, b"-->").map_or(bytes.len(), |end| end + 3);
            continue;
        }
        let (name, after) = tag_name(html, lt + 1);
        if name != "img" && name != "source" {
            i = lt + 1;
            continue;
        }
        i = attrs(html, after, |attr, at| match attr {
            "src" => out.push(Ref {
                at: at.clone(),
                url: html[at].to_string(),
            }),
            "srcset" => srcset(html, at, &mut out),
            _ => {}
        });
    }
    out
}

/// Put something else in place of each `src`, leaving every byte around them.
///
/// `swap` says what one becomes, and `None` leaves it as it was written — which
/// is what an absolute URL gets, since a page is welcome to name a picture on a
/// host of its own and this app is not the one fetching it.
pub fn rewrite(html: &str, refs: &[Ref], mut swap: impl FnMut(&Ref) -> Option<String>) -> String {
    let mut out = String::with_capacity(html.len());
    let mut at = 0;
    for r in refs {
        // Ranges come out of the scan in order and never overlap; anything else
        // is a bug here, and skipping is better than panicking in a browser.
        let Some(replacement) = swap(r).filter(|_| r.at.start >= at) else {
            continue;
        };
        out.push_str(&html[at..r.at.start]);
        out.push_str(&replacement);
        at = r.at.end;
    }
    out.push_str(&html[at..]);
    out
}

/// The name of the tag opening at `i`, lowercased, and where it ends.
fn tag_name(html: &str, i: usize) -> (String, usize) {
    let bytes = html.as_bytes();
    let mut end = i;
    while end < bytes.len() && bytes[end].is_ascii_alphanumeric() {
        end += 1;
    }
    (html[i..end].to_ascii_lowercase(), end)
}

/// Walk one tag's attributes, reporting each one that has a value. Returns
/// where the tag ended, which is where scanning picks up again.
fn attrs(html: &str, mut i: usize, mut each: impl FnMut(&str, Range<usize>)) -> usize {
    let bytes = html.as_bytes();
    while i < bytes.len() && bytes[i] != b'>' {
        if bytes[i].is_ascii_whitespace() || bytes[i] == b'/' {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len()
            && !matches!(bytes[i], b'=' | b'>' | b'/')
            && !bytes[i].is_ascii_whitespace()
        {
            i += 1;
        }
        // An attribute name out of a document that is not ASCII where it should
        // be. Not one of the two this is looking for, whatever it is.
        let name = html.get(start..i).unwrap_or_default().to_ascii_lowercase();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        // `<img hidden src=…>` — a bare attribute, and `i` is already standing
        // on whatever follows it.
        if bytes.get(i) != Some(&b'=') {
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let value = match bytes.get(i) {
            Some(&quote @ (b'"' | b'\'')) => {
                let from = i + 1;
                let to = memchr(bytes, from, quote).unwrap_or(bytes.len());
                i = (to + 1).min(bytes.len());
                from..to
            }
            Some(_) => {
                let from = i;
                while i < bytes.len() && bytes[i] != b'>' && !bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                from..i
            }
            None => break,
        };
        each(&name, value);
    }
    (i + 1).min(bytes.len())
}

/// The candidates in a `srcset`: `a.png 1x, b.png 2x` is two pictures, each
/// followed by a descriptor that is not part of its name.
fn srcset(html: &str, at: Range<usize>, out: &mut Vec<Ref>) {
    let bytes = html.as_bytes();
    let mut i = at.start;
    while i < at.end {
        while i < at.end && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        let start = i;
        while i < at.end && !bytes[i].is_ascii_whitespace() && bytes[i] != b',' {
            i += 1;
        }
        if start < i {
            out.push(Ref {
                at: start..i,
                url: html[start..i].to_string(),
            });
        }
        // Past the descriptor — `2x`, `600w` — to the next candidate.
        while i < at.end && bytes[i] != b',' {
            i += 1;
        }
    }
}

fn memchr(bytes: &[u8], from: usize, needle: u8) -> Option<usize> {
    bytes
        .get(from..)?
        .iter()
        .position(|&b| b == needle)
        .map(|at| at + from)
}

fn find(bytes: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    bytes
        .get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|at| at + from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_are_known_by_their_extension() {
        assert_eq!(media_type(Path::new("diff.png")), Some("image/png"));
        assert_eq!(media_type(Path::new("docs/A.JPEG")), Some("image/jpeg"));
        assert_eq!(media_type(Path::new("logo.svg")), Some("image/svg+xml"));
        // Not a picture, so not something to fetch and inline.
        assert_eq!(media_type(Path::new("notes.txt")), None);
        assert_eq!(media_type(Path::new("Makefile")), None);
    }

    #[test]
    fn base64_matches_the_padding_rules() {
        // The vectors from RFC 4648, which is where the padding is specified.
        let uri = |s: &str| data_uri("image/png", s.as_bytes());
        assert_eq!(uri(""), "data:image/png;base64,");
        assert_eq!(uri("f"), "data:image/png;base64,Zg==");
        assert_eq!(uri("fo"), "data:image/png;base64,Zm8=");
        assert_eq!(uri("foo"), "data:image/png;base64,Zm9v");
        assert_eq!(uri("foob"), "data:image/png;base64,Zm9vYg==");
        assert_eq!(uri("fooba"), "data:image/png;base64,Zm9vYmE=");
        assert_eq!(uri("foobar"), "data:image/png;base64,Zm9vYmFy");
    }

    #[test]
    fn base64_covers_the_whole_alphabet() {
        // Every bit pattern, so a wrong entry in the table cannot hide.
        let bytes: Vec<u8> = (0..=255u8).collect();
        let uri = data_uri("application/octet-stream", &bytes);
        let encoded = uri.split_once(";base64,").unwrap().1;
        assert_eq!(
            encoded.len(),
            344,
            "256 bytes is 344 characters with padding"
        );
        assert!(encoded.starts_with("AAECAwQFBgcICQoLDA0ODxAR"), "{encoded}");
        assert!(encoded.ends_with("9vf4+fr7/P3+/w=="), "{encoded}");
    }

    fn urls(html: &str) -> Vec<String> {
        image_refs(html).into_iter().map(|r| r.url).collect()
    }

    #[test]
    fn src_is_read_however_it_is_quoted() {
        assert_eq!(urls(r#"<img src="a.png">"#), ["a.png"]);
        assert_eq!(urls("<img src='b.png'>"), ["b.png"]);
        assert_eq!(urls("<img src=c.png>"), ["c.png"]);
        assert_eq!(urls("<IMG  SRC = \"d.png\" >"), ["d.png"]);
        // Other attributes, before and after, and one with no value at all.
        assert_eq!(
            urls(r#"<img loading="lazy" hidden src="e.png" alt="x">"#),
            ["e.png"]
        );
        // Self-closing, as an XHTML-flavoured README writes it.
        assert_eq!(urls(r#"<img src="f.png" />"#), ["f.png"]);
    }

    #[test]
    fn only_the_tags_that_draw_pictures_are_read() {
        let html = r#"<a href="x.png"><script src="app.js"></script><img src="y.png"></a>"#;
        assert_eq!(urls(html), ["y.png"]);
    }

    #[test]
    fn a_picture_element_is_all_of_its_candidates() {
        let html = r#"
            <picture>
              <source srcset="dark.png 1x, dark@2x.png 2x" media="(prefers-color-scheme: dark)">
              <source src="light.png">
              <img src="fallback.png" alt="a chart">
            </picture>"#;
        assert_eq!(
            urls(html),
            ["dark.png", "dark@2x.png", "light.png", "fallback.png"]
        );
    }

    #[test]
    fn commented_out_markup_is_not_markup() {
        let html = r#"<!-- <img src="old.png"> --><img src="new.png">"#;
        assert_eq!(urls(html), ["new.png"]);
        // An unterminated comment swallows the rest of the file, as it does in
        // a browser — and does not run off the end of it here.
        assert_eq!(urls(r#"<!-- <img src="old.png">"#), Vec::<String>::new());
    }

    #[test]
    fn swapping_a_src_leaves_everything_around_it() {
        let html = r#"<p>hi</p><img alt="a" src="a.png" width="20"><img src="b.png">"#;
        let refs = image_refs(html);
        let out = rewrite(html, &refs, |r| {
            (r.url == "a.png").then(|| "data:image/png;base64,AA".to_string())
        });
        assert_eq!(
            out,
            r#"<p>hi</p><img alt="a" src="data:image/png;base64,AA" width="20"><img src="b.png">"#
        );
    }

    #[test]
    fn rewriting_nothing_returns_the_document() {
        let html = "<p>no pictures here</p>";
        assert_eq!(rewrite(html, &image_refs(html), |_| None), html);
    }

    #[test]
    fn text_around_the_tags_survives_being_multibyte() {
        let html = "<p>— naïve —</p><img src=\"π.png\"><p>ünicode</p>";
        let refs = image_refs(html);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].url, "π.png");
        let out = rewrite(html, &refs, |_| Some("x".to_string()));
        assert_eq!(out, "<p>— naïve —</p><img src=\"x\"><p>ünicode</p>");
    }

    #[test]
    fn a_tag_that_never_closes_does_not_run_off_the_end() {
        assert_eq!(urls(r#"<img src="a.png"#), ["a.png"]);
        assert_eq!(urls("<img src="), Vec::<String>::new());
        assert_eq!(urls("<img"), Vec::<String>::new());
        assert_eq!(urls("<"), Vec::<String>::new());
    }
}
