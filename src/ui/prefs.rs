//! The appearance panel: theme, accent, font, size.
//!
//! Everything it sets is a CSS custom property — see
//! [`crate::backend::prefs`] — so this file is six rows of buttons and nothing
//! else. What it draws underneath them is the app's own code renderer on a
//! sample, because "12px, tight, tabs at four" is not a thing anybody can
//! picture from the words.

use dioxus::prelude::*;

use crate::backend::highlight::highlight_lang;
use crate::backend::prefs::{Accent, MAX_PX, MIN_PX, Mono, Prefs, Spacing, Theme};

use super::app::St;

/// The sample. Short, indented with a real tab so the tab-width buttons have
/// something to move, and Rust because that is what this is written in.
const SAMPLE: &str = "fn main() {\n\tlet who = \"pullspace\";\n\tprintln!(\"hello, {who}\");\n}";

/// A segmented button, on or off.
fn seg(on: bool) -> &'static str {
    if on { "modebtn on" } else { "modebtn" }
}

#[component]
pub fn PrefsPanel() -> Element {
    let st = use_context::<St>();
    let mut open = st.prefs_open;
    let now = *st.prefs.read();
    let light = now.theme.is_light();

    rsx! {
        div {
            class: "ghoverlay",
            // Click-away closes, but only on the backdrop itself.
            onclick: move |_| open.set(false),
            tabindex: "-1",
            onkeydown: move |e| {
                if e.key() == Key::Escape {
                    open.set(false);
                }
            },
            div {
                class: "ghpanel narrow",
                onclick: move |e| e.stop_propagation(),
                div { class: "ghhdr",
                    span { class: "ghtitle", "Appearance" }
                    span { class: "spacer" }
                    button {
                        class: "iconbtn",
                        title: "Close  (Esc)",
                        onclick: move |_| open.set(false),
                        "✕"
                    }
                }
                div { class: "ghbody",
                    div { class: "prefrow",
                        span { class: "preflabel", "Theme" }
                        div { class: "modegroup",
                            for t in [Theme::Dark, Theme::Midnight, Theme::Light] {
                                button {
                                    key: "{t:?}",
                                    class: seg(t == now.theme),
                                    onclick: move |_| st.set_prefs(Prefs { theme: t, ..now }),
                                    "{t.label()}"
                                }
                            }
                        }
                    }

                    div { class: "prefrow",
                        span { class: "preflabel", "Accent" }
                        div { class: "swatches",
                            for a in [Accent::Blue, Accent::Violet, Accent::Green, Accent::Amber, Accent::Rose] {
                                button {
                                    key: "{a:?}",
                                    class: if a == now.accent { "swatch on" } else { "swatch" },
                                    // The colour it will actually be, which
                                    // depends on the theme it is going onto.
                                    style: "--swatch:{a.css_color(light)}",
                                    title: "{a.label()}",
                                    onclick: move |_| st.set_prefs(Prefs { accent: a, ..now }),
                                }
                            }
                        }
                    }

                    div { class: "prefrow",
                        span { class: "preflabel", "Code font" }
                        div { class: "modegroup",
                            // The faces this machine already has — a page
                            // cannot install a font, so these are named for
                            // the platform the app was opened on, and there
                            // are as many as it has worth offering.
                            for m in Mono::all() {
                                button {
                                    key: "{m:?}",
                                    class: seg(m == now.mono),
                                    title: "{m.title()}",
                                    onclick: move |_| st.set_prefs(Prefs { mono: m, ..now }),
                                    "{m.label()}"
                                }
                            }
                        }
                    }

                    div { class: "prefrow",
                        span { class: "preflabel", "Size" }
                        div { class: "modegroup",
                            button {
                                class: "modebtn",
                                title: "Smaller",
                                disabled: now.code_px <= MIN_PX,
                                onclick: move |_| st.set_prefs(now.resized(-1)),
                                "−"
                            }
                            span { class: "prefnum", "{now.code_px}px" }
                            button {
                                class: "modebtn",
                                title: "Bigger",
                                disabled: now.code_px >= MAX_PX,
                                onclick: move |_| st.set_prefs(now.resized(1)),
                                "+"
                            }
                        }
                    }

                    div { class: "prefrow",
                        span { class: "preflabel", "Line spacing" }
                        div { class: "modegroup",
                            for s in [Spacing::Tight, Spacing::Normal, Spacing::Loose] {
                                button {
                                    key: "{s:?}",
                                    class: seg(s == now.spacing),
                                    onclick: move |_| st.set_prefs(Prefs { spacing: s, ..now }),
                                    "{s.label()}"
                                }
                            }
                        }
                    }

                    div { class: "prefrow",
                        span { class: "preflabel", "Tab width" }
                        div { class: "modegroup",
                            for w in [2u8, 4, 8] {
                                button {
                                    key: "{w}",
                                    class: seg(w == now.tab),
                                    title: "Draw a tab as {w} characters",
                                    onclick: move |_| st.set_prefs(Prefs { tab: w, ..now }),
                                    "{w}"
                                }
                            }
                        }
                    }

                    Sample {}

                    div { class: "prefrow",
                        span { class: "prefhelp",
                            "Kept in this browser, per site. Nothing here leaves the page."
                        }
                        span { class: "spacer" }
                        button {
                            class: "modebtn",
                            title: "Back to the look this app ships with",
                            disabled: now == Prefs::default(),
                            onclick: move |_| st.set_prefs(Prefs::default()),
                            "Reset"
                        }
                    }
                }
            }
        }
    }
}

/// Four lines of code, drawn by the same renderer as the viewer.
///
/// It re-colours with the theme because [`highlight_lang`] reads the theme the
/// app is on — which makes this the one place the syntax colours can be seen
/// changing at the moment the button is pressed.
#[component]
fn Sample() -> Element {
    let st = use_context::<St>();
    // Subscribed to, not read: the sample is redrawn because the font, the
    // size and the syntax theme have all just moved.
    let _ = st.prefs.read();
    rsx! {
        div { class: "prefsample",
            div { class: "code",
                for (i , spans) in highlight_lang("rust", SAMPLE).into_iter().enumerate() {
                    div { key: "{i}", class: "cl",
                        span { class: "ln", "{i + 1}" }
                        span { class: "lc",
                            for (j , sp) in spans.iter().enumerate() {
                                span { key: "{j}", style: "color:{sp.color}", "{sp.text}" }
                            }
                        }
                    }
                }
            }
        }
    }
}
