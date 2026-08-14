//! The browser tab: what it is called, and the icon on it.
//!
//! Both are set from here rather than from the generated `index.html`, for the
//! same reason the stylesheet is `include_str!` rather than a file beside it —
//! `dx build` leaves an index.html, a .wasm and a .js, and nothing about this
//! app should add a fourth thing to serve. The icon is therefore a data URI:
//! 300 bytes of SVG, built at startup, with no request behind it.
//!
//! The name is worth setting for a different reason. A review of four pull
//! requests is four tabs, and four tabs all called "pullspace" is a row of
//! identical favicons to hunt through.

use dioxus::prelude::*;

use super::app::{St, Workspace};

/// The icon, as the file it would have been.
///
/// A diff, drawn small enough to survive being 16 pixels wide: the gutter down
/// the left, and three lines of a file — one added, one removed, one that
/// stayed. Single quotes throughout, because [`data_uri`] has enough to encode
/// without them.
const ICON: &str = "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 32 32'>\
<rect width='32' height='32' rx='7' fill='#17181d'/>\
<rect x='5' y='6' width='3' height='20' rx='1.5' fill='#5c9cf5'/>\
<rect x='12' y='7' width='15' height='4' rx='2' fill='#4fbf7a'/>\
<rect x='12' y='14' width='11' height='4' rx='2' fill='#e06c75'/>\
<rect x='12' y='21' width='15' height='4' rx='2' fill='#848a9c'/>\
</svg>";

/// SVG as a URL.
///
/// Percent-encoding rather than base64: it keeps the icon above readable as the
/// picture it is, and an SVG that is mostly angle brackets and hex colours
/// encodes to about the same length either way.
fn data_uri(svg: &str) -> String {
    const SAFE: &str = "-_.~!'()*/:=,;";
    let mut out = String::from("data:image/svg+xml,");
    for byte in svg.bytes() {
        match byte {
            b if b.is_ascii_alphanumeric() => out.push(b as char),
            b if SAFE.as_bytes().contains(&b) => out.push(b as char),
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// What the tab is called: whatever is open, and the app after it.
fn title(ws: &Workspace) -> String {
    match ws {
        Workspace::Empty => "pullspace".to_string(),
        Workspace::Pr(pr) => format!("{} #{} · pullspace", pr.repo, pr.number),
        Workspace::Repo(view) => format!("{} · pullspace", view.repo),
        Workspace::Commit(view) => {
            format!("{} {} · pullspace", view.repo, view.commit.short())
        }
    }
}

/// Draws nothing. Both of these are head elements — dioxus puts them in the
/// document rather than in the tree they are written in.
#[component]
pub fn Tab() -> Element {
    let st = use_context::<St>();
    let name = title(&st.workspace.read());
    rsx! {
        document::Link {
            rel: "icon",
            r#type: "image/svg+xml",
            href: data_uri(ICON),
        }
        document::Title { "{name}" }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_icon_survives_being_a_url() {
        let uri = data_uri(ICON);
        assert!(uri.starts_with("data:image/svg+xml,%3Csvg"), "{uri}");
        // The ones that would split the URL, end the attribute, or — the one
        // that actually happens, since every colour in there has one — start a
        // fragment halfway through the picture.
        for bad in ['<', '>', '#', ' ', '"'] {
            assert!(!uri.contains(bad), "{bad:?} left raw in {uri}");
        }
        // And every escape is a whole escape.
        let bytes = uri.as_bytes();
        for (i, _) in uri.match_indices('%') {
            let pair = bytes.get(i + 1..i + 3).unwrap_or_default();
            assert!(
                pair.len() == 2 && pair.iter().all(u8::is_ascii_hexdigit),
                "half an escape at {i} in {uri}"
            );
        }
    }

    #[test]
    fn the_tab_says_what_is_open() {
        assert_eq!(title(&Workspace::Empty), "pullspace");
    }
}
