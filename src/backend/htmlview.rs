//! Rendering an HTML file to a picture of itself.
//!
//! The webview pullspace draws its own UI in is not a safe place to put a file
//! out of a repository: it is served by a protocol that will hand any path on
//! disk to a script that asks, it carries an IPC bridge to this process, and it
//! has no content security policy. A page reviewed there could read the token
//! in `~/.config/pullspace/auth.json` and post it anywhere.
//!
//! So the page is never given to that engine. Blitz parses and lays out the
//! HTML and paints it to a buffer of pixels — it has no JavaScript engine at
//! all, so `<script>`, `onerror` and the rest are inert text rather than code
//! that has been sandboxed. [`OfflineNet`] refuses every subresource on top of
//! that, so nothing is fetched and nothing can be beaconed out. What reaches
//! the UI is a PNG: data, with no way to execute anything.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result};
use anyrender::ImageRenderer;
use anyrender_vello_cpu::VelloCpuImageRenderer;
use blitz_dom::DocumentConfig;
use blitz_html::HtmlDocument;
use blitz_paint::paint_scene;
use blitz_traits::net::{Bytes, NetHandler, NetProvider, Request};
use blitz_traits::shell::{ColorScheme, Viewport};

/// Page width in CSS pixels. A fixed width keeps a render cacheable and the
/// result predictable; the viewer scales the picture down to its pane.
const PAGE_WIDTH: u32 = 900;

/// Rendered at twice the size so the picture is not soft on a retina display.
const SCALE: f32 = 2.0;

/// Ceiling on the pixels held at once — four bytes each, and a page that
/// scrolls forever would otherwise ask for all of them. Roughly 32 MB.
const MAX_PIXELS: u32 = 8_000_000;

/// True for the files worth offering a preview of.
pub fn is_html(path: &std::path::Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref(),
        Some("html" | "htm" | "xhtml")
    )
}

/// Answers every request for a subresource with nothing at all.
///
/// It has to *answer*: a stylesheet linked in `<head>` goes on the document's
/// critical list, and painting is blocked while anything is still on it. A
/// provider that simply dropped the request — which is what Blitz's own
/// `DummyNetProvider` does — would leave the page blank forever.
struct OfflineNet {
    blocked: Arc<AtomicUsize>,
}

impl NetProvider for OfflineNet {
    fn fetch(&self, _doc_id: usize, request: Request, handler: Box<dyn NetHandler>) {
        self.blocked.fetch_add(1, Ordering::Relaxed);
        handler.bytes(request.url.to_string(), Bytes::new());
    }
}

/// A page, drawn.
pub struct Preview {
    pub png: Vec<u8>,
    /// Size of the image in real pixels — [`SCALE`] times the page's own.
    pub width: u32,
    pub height: u32,
    /// True when the page was taller than [`MAX_PIXELS`] allows and the picture
    /// stops part way down it.
    pub clipped: bool,
    /// How many stylesheets, images and fonts were refused.
    pub blocked: usize,
}

impl Preview {
    /// What the image should be displayed at, in CSS pixels: it is drawn at
    /// [`SCALE`] so it stays sharp, and shown back down at the size the page
    /// was actually laid out for.
    pub fn css_size(&self) -> (u32, u32) {
        (
            (self.width as f32 / SCALE) as u32,
            (self.height as f32 / SCALE) as u32,
        )
    }
}

/// Lay `html` out and paint it. Blocking and CPU-bound — callers run it off the
/// UI thread.
pub fn render(html: &str) -> Result<Preview> {
    let blocked = Arc::new(AtomicUsize::new(0));
    let width = (PAGE_WIDTH as f32 * SCALE) as u32;

    let mut doc = HtmlDocument::from_html(
        html,
        DocumentConfig {
            net_provider: Some(Arc::new(OfflineNet {
                blocked: blocked.clone(),
            })),
            viewport: Some(Viewport::new(width, 100, SCALE, ColorScheme::Light)),
            ..Default::default()
        },
    )
    .into_inner();

    // Lay out once against a short viewport to find how tall the content is,
    // then again at that height — otherwise the picture is a screenshot of the
    // first hundred pixels rather than of the page.
    doc.resolve(0.0);
    let content = doc.root_element().final_layout.size.height * SCALE;
    let wanted = (content.ceil() as u32).max(1);
    let cap = (MAX_PIXELS / width).max(1);
    let height = wanted.min(cap);
    doc.set_viewport(Viewport::new(width, height, SCALE, ColorScheme::Light));
    doc.resolve(0.0);

    let mut renderer = VelloCpuImageRenderer::new(width, height);
    let mut buf = Vec::new();
    renderer.render_to_vec(
        |scene| paint_scene(scene, &mut doc, SCALE as f64, width, height, 0, 0),
        &mut buf,
    );

    Ok(Preview {
        png: encode_png(&buf, width, height)?,
        width,
        height,
        clipped: wanted > height,
        blocked: blocked.load(Ordering::Relaxed),
    })
}

/// Flatten the painted buffer onto white and encode it.
///
/// Anywhere the page did not paint comes back fully transparent, which over the
/// app's dark chrome would leave black text on a black background. A page is a
/// page: it gets the white it was written against.
fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    for px in rgba.chunks_exact(4) {
        // Vello hands back premultiplied alpha, so the source is added to what
        // shows through rather than mixed with it.
        let over = |c: u8| c.saturating_add(255u8.saturating_sub(px[3]));
        rgb.extend_from_slice(&[over(px[0]), over(px[1]), over(px[2])]);
    }

    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, width, height);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.set_compression(png::Compression::Fast);
    enc.write_header()
        .context("writing PNG header")?
        .write_image_data(&rgb)
        .context("encoding the preview")?;
    Ok(out)
}

/// The picture as something an `<img>` can point at.
///
/// Base64 by hand: it is a table and three shifts, and the alternative is a
/// dependency for one call site.
pub fn data_uri(png: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(png.len().div_ceil(3) * 4 + 24);
    out.push_str("data:image/png;base64,");
    for chunk in png.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn html_files_are_previewable() {
        assert!(is_html(Path::new("index.html")));
        assert!(is_html(Path::new("docs/page.HTM")));
        assert!(!is_html(Path::new("src/main.rs")));
        assert!(!is_html(Path::new("README.md")));
        // No extension at all, and a directory-looking path.
        assert!(!is_html(Path::new("Makefile")));
    }

    #[test]
    fn base64_matches_the_known_answers() {
        // The RFC 4648 vectors, which are the ones that get the padding wrong.
        let of = |s: &str| data_uri(s.as_bytes()).replace("data:image/png;base64,", "");
        assert_eq!(of(""), "");
        assert_eq!(of("f"), "Zg==");
        assert_eq!(of("fo"), "Zm8=");
        assert_eq!(of("foo"), "Zm9v");
        assert_eq!(of("foob"), "Zm9vYg==");
        assert_eq!(of("fooba"), "Zm9vYmE=");
        assert_eq!(of("foobar"), "Zm9vYmFy");
    }

    #[test]
    fn a_page_paints_and_its_scripts_do_not_run() {
        // If any of this executed, the document would be rewritten and the
        // paint below would be of something else entirely.
        let preview = render(
            r#"<!doctype html><html><head>
               <link rel="stylesheet" href="https://evil.example/s.css">
               <style>body { background: #ffffff } h1 { color: #ff0000 }</style>
               <script>document.body.innerHTML = '<p>pwned</p>'</script>
               </head><body><h1>hello</h1>
               <img src="https://evil.example/beacon.png" onerror="alert(1)">
               </body></html>"#,
        )
        .expect("render");

        assert!(preview.height > 1, "the page has some height to it");
        assert_eq!(preview.png[1..4], *b"PNG");
        // The stylesheet and the image: refused, but answered, or the paint
        // would have been blocked and we would be looking at nothing.
        assert_eq!(preview.blocked, 2);
    }

    #[test]
    fn an_endless_page_is_clipped_rather_than_allocated_for() {
        let tall = format!(
            "<html><body>{}</body></html>",
            "<div style='height:400px'>x</div>".repeat(200),
        );
        let preview = render(&tall).expect("render");
        assert!(preview.clipped, "80000px of page does not fit");
        assert!(
            preview.width * preview.height <= MAX_PIXELS,
            "{}x{} is past the ceiling",
            preview.width,
            preview.height
        );
    }
}


