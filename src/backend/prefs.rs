//! What the app looks like, kept between visits.
//!
//! Every one of these is a CSS custom property the stylesheet already reads, so
//! nothing here knows what a pane looks like — it decides values, and
//! [`Prefs::css`] hands them to the page as a `:root` block that overrides the
//! defaults compiled into `style.css`. Adding a colour to the app therefore
//! means naming it in the stylesheet once and here once, rather than writing a
//! second stylesheet per theme.
//!
//! Saved like the pane sizes, and for the same reason: picking a font again at
//! every launch is the sort of small tax that makes a tool feel unfinished.

use serde::{Deserialize, Serialize};

use super::store;

/// The overall palette. `Midnight` is `Dark` with the lights turned further
/// down — the same app on an OLED panel in a dark room.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    Dark,
    Midnight,
    Light,
}

/// One colour, carried through everything that is a link, a selection or a
/// highlight. Two sets of values, because a blue that reads on `#17181d` is a
/// blue that disappears on white.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Accent {
    #[default]
    Blue,
    Violet,
    Green,
    Amber,
    Rose,
}

/// Which machine the page is on, because the faces it can be set in are the
/// ones already on it.
///
/// Read off the user agent, which is a blunt instrument and the right one here:
/// the only question is which of three font tables to offer, and every browser
/// still names its operating system in that string even after the rest of it
/// was frozen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Platform {
    Mac,
    Windows,
    /// Linux, and anything else: a table of the families a desktop is likely to
    /// have, with the browser's own monospace under them.
    Other,
}

impl Platform {
    fn detect() -> Self {
        // `cargo test` builds for the host, where there is no window to ask:
        // web-sys reaches for a wasm import and panics on the way in. Off the
        // page the machine the binary was built for is the honest answer.
        #[cfg(target_arch = "wasm32")]
        {
            let ua = web_sys::window()
                .and_then(|w| w.navigator().user_agent().ok())
                .unwrap_or_default();
            Platform::from_user_agent(&ua)
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            // Spelled the way a user agent spells it, so that both paths end in
            // the same matcher rather than in two answers to one question.
            let host = if cfg!(target_os = "macos") {
                "Macintosh"
            } else if cfg!(target_os = "windows") {
                "Windows"
            } else {
                "X11"
            };
            Platform::from_user_agent(host)
        }
    }

    fn from_user_agent(ua: &str) -> Self {
        // "Macintosh" and "iPhone"/"iPad" both mean the Apple font set; iOS
        // Safari reports the latter two.
        if ua.contains("Mac") || ua.contains("iPhone") || ua.contains("iPad") {
            Platform::Mac
        } else if ua.contains("Windows") {
            Platform::Windows
        } else {
            Platform::Other
        }
    }
}

/// The one detection, kept. `css()` runs on every render of the app and this
/// answer cannot change inside a tab.
fn platform() -> Platform {
    static PLATFORM: std::sync::OnceLock<Platform> = std::sync::OnceLock::new();
    *PLATFORM.get_or_init(Platform::detect)
}

/// The face the code is set in: a slot in the machine's own table.
///
/// A slot rather than a family, because which family it is depends on where the
/// page is open — and because a page cannot install a font. Naming a family
/// that is not on the machine changes nothing at all: the browser walks
/// straight past it to the next one, which is how this setting came to have
/// four buttons that all drew the same face. The tables below hold the faces
/// each platform ships with, and under slot zero whatever the browser sets code
/// in by itself. They are not the same length, because the platforms are not
/// equally well supplied.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Mono(u8);

/// How far apart the lines of a file are set.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Spacing {
    Tight,
    #[default]
    Normal,
    Loose,
}

/// Where the code font size can go. Below the floor the diff gutters stop
/// lining up with the text; above the ceiling a split diff stops fitting.
pub const MIN_PX: u8 = 10;
pub const MAX_PX: u8 = 18;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub theme: Theme,
    pub accent: Accent,
    pub mono: Mono,
    /// The code panes' font size, in CSS pixels.
    pub code_px: u8,
    pub spacing: Spacing,
    /// How wide a tab is drawn, in characters. Eight is the browser's own
    /// default, which is what every file in this app has been drawn with until
    /// somebody says otherwise.
    pub tab: u8,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            theme: Theme::default(),
            accent: Accent::default(),
            mono: Mono::default(),
            code_px: 12,
            spacing: Spacing::default(),
            tab: 8,
        }
    }
}

impl Theme {
    pub fn label(self) -> &'static str {
        match self {
            Theme::Dark => "Dark",
            Theme::Midnight => "Midnight",
            Theme::Light => "Light",
        }
    }

    pub fn is_light(self) -> bool {
        matches!(self, Theme::Light)
    }

    /// What `color-scheme` is told, which is what turns the text caret, the
    /// native focus rings and the form controls' own internals over with the
    /// rest of it.
    fn scheme(self) -> &'static str {
        if self.is_light() { "light" } else { "dark" }
    }

    fn palette(self) -> Palette {
        match self {
            Theme::Dark => DARK,
            Theme::Midnight => MIDNIGHT,
            Theme::Light => LIGHT,
        }
    }
}

impl Accent {
    pub fn label(self) -> &'static str {
        match self {
            Accent::Blue => "Blue",
            Accent::Violet => "Violet",
            Accent::Green => "Green",
            Accent::Amber => "Amber",
            Accent::Rose => "Rose",
        }
    }

    /// The colour itself. The light set is several shades down: these are read
    /// as text and as hairlines, and both want contrast against the page they
    /// are on rather than a fixed brightness.
    pub fn rgb(self, light: bool) -> (u8, u8, u8) {
        match (self, light) {
            (Accent::Blue, false) => (92, 156, 245),
            (Accent::Blue, true) => (37, 99, 235),
            (Accent::Violet, false) => (167, 139, 250),
            (Accent::Violet, true) => (109, 66, 214),
            (Accent::Green, false) => (79, 191, 122),
            (Accent::Green, true) => (22, 131, 80),
            (Accent::Amber, false) => (226, 179, 76),
            (Accent::Amber, true) => (166, 110, 15),
            (Accent::Rose, false) => (244, 114, 138),
            (Accent::Rose, true) => (198, 45, 84),
        }
    }

    /// The swatch in the appearance panel, as a colour a style attribute can
    /// take.
    pub fn css_color(self, light: bool) -> String {
        let (r, g, b) = self.rgb(light);
        format!("#{r:02x}{g:02x}{b:02x}")
    }
}

/// A face as the panel needs it: what to put on the button, what to say when
/// the pointer rests on it, and what to ask the browser for.
struct Face {
    /// Short enough for a row of four — the family in full goes in `title`.
    label: &'static str,
    title: &'static str,
    /// Ends in `monospace` every time, so a machine missing the family still
    /// draws code in something fixed-width.
    stack: &'static str,
}

/// The slot every table starts with: the face the browser sets code in by
/// itself, which is also the one the app has been drawing in all along. The
/// ones named after it are chosen to be faces the browser would *not* have
/// picked, so that every button means a different face.
///
/// `ui-monospace` is the whole entry — the families behind it are for an engine
/// that does not know the keyword, and they are there because bare `monospace`
/// in WebKit is Courier, which is not a face to review code in. This is
/// character for character the `--mono` that `style.css` starts on, so the
/// first paint and this slot ask for exactly the same thing; the test below
/// keeps them that way.
const SYSTEM: Face = Face {
    label: "System",
    title: "Whatever this browser sets code in",
    stack: "ui-monospace, Menlo, Consolas, \"DejaVu Sans Mono\", monospace",
};

/// A Mac, which has the most to offer and so gets the longest table. Measured
/// in both engines rather than assumed, because they disagree: `ui-monospace`
/// is SF Mono in WebKit and Menlo in Chrome, and `"SF Mono"` by name resolves in
/// neither — it is installed under a hidden one. Either way `System` is already
/// the best of the coding faces here, so the rest are the remaining ones macOS
/// ships that are unmistakably not it, and five are five in both.
const MAC: &[Face] = &[
    SYSTEM,
    Face {
        label: "Monaco",
        title: "Monaco",
        stack: "Monaco, monospace",
    },
    Face {
        label: "Andale",
        title: "Andale Mono",
        stack: "\"Andale Mono\", monospace",
    },
    Face {
        label: "PT Mono",
        title: "PT Mono",
        stack: "\"PT Mono\", monospace",
    },
    // Last because it is the odd one: a typewriter face rather than a screen
    // one, and the only entry here somebody has to want on purpose.
    Face {
        label: "Courier",
        title: "Courier New",
        stack: "\"Courier New\", monospace",
    },
];

/// Windows, where the browser's own monospace is Consolas — so Consolas is what
/// `System` is, and these are the three beside it. Cascadia arrives with
/// Windows 11 and with the Terminal before it, which makes it the one here that
/// can be missing; Lucida Console and Courier New have shipped with every
/// version this app runs on. There is no fifth: this is every monospace face
/// Windows installs.
const WINDOWS: &[Face] = &[
    SYSTEM,
    Face {
        label: "Cascadia",
        title: "Cascadia Mono",
        stack: "\"Cascadia Mono\", \"Cascadia Code\", monospace",
    },
    Face {
        label: "Lucida",
        title: "Lucida Console",
        stack: "\"Lucida Console\", monospace",
    },
    Face {
        label: "Courier",
        title: "Courier New",
        stack: "\"Courier New\", monospace",
    },
];

/// Linux, and whatever else. Nothing is guaranteed on a desktop somebody
/// assembled, so these are the three families a distribution is most likely to
/// have pulled in. One of them is probably also what the browser picks for
/// `System`, and there is no way to know which from in here — unlike the two
/// tables above, this one is the best guess rather than a measurement.
const OTHER: &[Face] = &[
    SYSTEM,
    Face {
        label: "DejaVu",
        title: "DejaVu Sans Mono",
        stack: "\"DejaVu Sans Mono\", monospace",
    },
    Face {
        label: "Liberation",
        title: "Liberation Mono",
        stack: "\"Liberation Mono\", monospace",
    },
    Face {
        label: "Noto",
        title: "Noto Sans Mono",
        stack: "\"Noto Sans Mono\", monospace",
    },
];

/// The tables are not the same length, because the platforms do not have the
/// same number of faces to give: macOS has five worth offering and Windows
/// installs four monospace families in total.
fn table(on: Platform) -> &'static [Face] {
    match on {
        Platform::Mac => MAC,
        Platform::Windows => WINDOWS,
        Platform::Other => OTHER,
    }
}

impl Mono {
    /// The slots this machine offers, in the order the panel draws them. The
    /// only way to make one, so a slot outside the table is not something that
    /// can be chosen — only something that can arrive from storage, which
    /// [`Mono::deserialize`] turns back into the default.
    pub fn all() -> impl Iterator<Item = Mono> {
        (0..table(platform()).len() as u8).map(Mono)
    }

    fn face(self, on: Platform) -> &'static Face {
        let t = table(on);
        // A slot saved on a machine with more faces than this one has. The
        // first is always there and is always a monospace.
        t.get(self.0 as usize).unwrap_or(&t[0])
    }

    /// What this machine calls the face — "Cascadia" on Windows, "Monaco" on a
    /// Mac — so that a button never offers a font that is not there.
    pub fn label(self) -> &'static str {
        self.face(platform()).label
    }

    /// The family in full, for the button's tooltip. The labels are short
    /// because the row they sit in is 318px wide.
    pub fn title(self) -> &'static str {
        self.face(platform()).title
    }

    fn stack(self) -> &'static str {
        self.face(platform()).stack
    }
}

impl Serialize for Mono {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u8(self.0)
    }
}

impl<'de> Deserialize<'de> for Mono {
    /// Anything that is not a slot this machine offers is the default rather
    /// than an error. The faces have changed once already, and what was saved
    /// then — a font's name, from a set that no longer exists — has to cost the
    /// font and nothing else: refusing the record outright would take the
    /// theme, the size and the spacing saved beside it too. A slot from a
    /// machine with a longer table goes the same way, and for the same reason
    /// it has to: a choice with no button to light is worse than the default.
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let slot = serde_json::Value::deserialize(d)?
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(usize::MAX);
        Ok(Mono::all().nth(slot).unwrap_or_default())
    }
}

impl Spacing {
    pub fn label(self) -> &'static str {
        match self {
            Spacing::Tight => "Tight",
            Spacing::Normal => "Normal",
            Spacing::Loose => "Loose",
        }
    }

    fn line_height(self) -> &'static str {
        match self {
            Spacing::Tight => "1.35",
            Spacing::Normal => "1.55",
            Spacing::Loose => "1.85",
        }
    }
}

/// Every colour in the app that changes with the theme.
///
/// The names are the custom properties they are written to, which is the whole
/// trick: a theme is this struct filled in, and the stylesheet never learns
/// there is more than one.
struct Palette {
    /// The canvas, then four surfaces raised off it — or, on a light theme,
    /// four sunk into it.
    bg: &'static str,
    bg_1: &'static str,
    bg_2: &'static str,
    bg_3: &'static str,
    bg_4: &'static str,
    border: &'static str,
    border_soft: &'static str,
    line: &'static str,
    fg: &'static str,
    fg_dim: &'static str,
    fg_faint: &'static str,
    fg_bright: &'static str,
    /// The explorer's indent guides.
    guide: &'static str,
    /// The two overlays that lift a row off what is under it. White on a dark
    /// app, black on a light one — the one thing that has to turn over.
    tint: &'static str,
    tint_soft: &'static str,
    /// A label on top of the accent colour.
    on_accent: &'static str,
    added: &'static str,
    modified: &'static str,
    deleted: &'static str,
    /// The fifth tone, and the only one that is not already a fact about a
    /// diff: what a `> [!IMPORTANT]` is drawn in. GitHub's own colour for it,
    /// and there is nothing else in the app that means purple.
    violet: &'static str,
    add_bg: &'static str,
    add_emph: &'static str,
    del_bg: &'static str,
    del_emph: &'static str,
}

const DARK: Palette = Palette {
    bg: "#17181d",
    bg_1: "#1e2026",
    bg_2: "#24262e",
    bg_3: "#2c2f38",
    bg_4: "#363a45",
    border: "#33363f",
    border_soft: "#2a2d35",
    line: "#424755",
    fg: "#d4d8e0",
    fg_dim: "#9aa0b0",
    fg_faint: "#848a9c",
    fg_bright: "#e8ecf4",
    guide: "#2b2e38",
    tint: "rgba(255,255,255,0.028)",
    tint_soft: "rgba(255,255,255,0.018)",
    on_accent: "#0f1116",
    added: "#4fbf7a",
    modified: "#e2b34c",
    deleted: "#e06c75",
    violet: "#a78bfa",
    add_bg: "rgba(79,191,122,0.13)",
    add_emph: "rgba(79,191,122,0.34)",
    del_bg: "rgba(224,108,117,0.13)",
    del_emph: "rgba(224,108,117,0.36)",
};

const MIDNIGHT: Palette = Palette {
    bg: "#0a0b0f",
    bg_1: "#101218",
    bg_2: "#161821",
    bg_3: "#1d2029",
    bg_4: "#262a35",
    border: "#262a34",
    border_soft: "#1c1f27",
    line: "#363c4a",
    fg: "#cfd4de",
    fg_dim: "#949aab",
    fg_faint: "#7d8496",
    fg_bright: "#e6eaf2",
    guide: "#202430",
    ..DARK
};

const LIGHT: Palette = Palette {
    bg: "#ffffff",
    bg_1: "#f7f8fa",
    bg_2: "#eef0f4",
    bg_3: "#e4e7ed",
    bg_4: "#d7dbe3",
    border: "#d5d9e0",
    border_soft: "#e7e9ef",
    line: "#c2c8d2",
    fg: "#1f2329",
    fg_dim: "#4e5560",
    fg_faint: "#6a7280",
    fg_bright: "#0d1117",
    guide: "#e6e9ef",
    tint: "rgba(0,0,0,0.035)",
    tint_soft: "rgba(0,0,0,0.022)",
    on_accent: "#ffffff",
    added: "#1f9254",
    modified: "#a3720f",
    deleted: "#cf3f4c",
    violet: "#6d42d6",
    add_bg: "rgba(31,146,84,0.12)",
    add_emph: "rgba(31,146,84,0.28)",
    del_bg: "rgba(207,63,76,0.12)",
    del_emph: "rgba(207,63,76,0.28)",
};

impl Prefs {
    /// Bring anything absurd back into range. Saved preferences are data from
    /// outside the program — a hand-edited storage entry should cost a setting,
    /// not the page.
    fn sane(self) -> Self {
        Self {
            code_px: self.code_px.clamp(MIN_PX, MAX_PX),
            // Anything else was not offered and is not drawable as a width.
            tab: match self.tab {
                2 | 4 | 8 => self.tab,
                _ => Prefs::default().tab,
            },
            ..self
        }
    }

    /// One step of the size control, kept inside the range either way.
    pub fn resized(self, by: i8) -> Self {
        Self {
            code_px: (self.code_px as i16 + by as i16).clamp(MIN_PX as i16, MAX_PX as i16) as u8,
            ..self
        }
    }

    /// These preferences, as the stylesheet that applies them.
    ///
    /// A second `:root` block after the app's own, so it wins on specificity
    /// ties by order and every rule in the app picks it up without knowing it
    /// exists. Nothing here is user-typed — every value comes out of the
    /// closed sets above — so there is nothing in it to escape.
    pub fn css(&self) -> String {
        let p = self.theme.palette();
        let (r, g, b) = self.accent.rgb(self.theme.is_light());
        format!(
            ":root{{\
             color-scheme:{scheme};\
             --bg:{bg};--bg-1:{bg_1};--bg-2:{bg_2};--bg-3:{bg_3};--bg-4:{bg_4};\
             --border:{border};--border-soft:{border_soft};--line:{line};\
             --fg:{fg};--fg-dim:{fg_dim};--fg-faint:{fg_faint};--fg-bright:{fg_bright};\
             --guide:{guide};--tint:{tint};--tint-soft:{tint_soft};--on-accent:{on_accent};\
             --added:{added};--modified:{modified};--deleted:{deleted};--violet:{violet};\
             --add-bg:{add_bg};--add-emph:{add_emph};--del-bg:{del_bg};--del-emph:{del_emph};\
             --accent:#{r:02x}{g:02x}{b:02x};--accent-rgb:{r},{g},{b};\
             --accent-soft:rgba({r},{g},{b},0.16);--accent-line:rgba({r},{g},{b},0.45);\
             --mono:{mono};--code-px:{px}px;--code-lh:{lh};--tab:{tab};\
             }}",
            scheme = self.theme.scheme(),
            bg = p.bg,
            bg_1 = p.bg_1,
            bg_2 = p.bg_2,
            bg_3 = p.bg_3,
            bg_4 = p.bg_4,
            border = p.border,
            border_soft = p.border_soft,
            line = p.line,
            fg = p.fg,
            fg_dim = p.fg_dim,
            fg_faint = p.fg_faint,
            fg_bright = p.fg_bright,
            guide = p.guide,
            tint = p.tint,
            tint_soft = p.tint_soft,
            on_accent = p.on_accent,
            added = p.added,
            modified = p.modified,
            deleted = p.deleted,
            violet = p.violet,
            add_bg = p.add_bg,
            add_emph = p.add_emph,
            del_bg = p.del_bg,
            del_emph = p.del_emph,
            mono = self.mono.stack(),
            px = self.code_px,
            lh = self.spacing.line_height(),
            tab = self.tab,
        )
    }
}

/// What was saved last, or the defaults. Never fails: preferences that cannot
/// be read are ones the reader picks again, not a reason to stop.
pub fn load() -> Prefs {
    store::get(store::PREFS)
        .and_then(|raw| serde_json::from_str::<Prefs>(&raw).ok())
        .unwrap_or_default()
        .sane()
}

pub fn save(prefs: Prefs) {
    if let Ok(body) = serde_json::to_string(&prefs) {
        store::set(store::PREFS, &body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonsense_falls_back_to_the_default() {
        let d = Prefs::default();
        let broken = Prefs {
            code_px: 200,
            tab: 37,
            ..d
        };
        let fixed = broken.sane();
        assert_eq!(fixed.code_px, MAX_PX);
        assert_eq!(fixed.tab, d.tab);
    }

    #[test]
    fn a_chosen_look_survives_a_round_trip() {
        let mine = Prefs {
            theme: Theme::Light,
            accent: Accent::Rose,
            mono: Mono(2),
            code_px: 14,
            spacing: Spacing::Loose,
            tab: 4,
        };
        let raw = serde_json::to_string(&mine).unwrap();
        let back: Prefs = serde_json::from_str(&raw).unwrap();
        assert_eq!(back.sane(), mine);
    }

    #[test]
    fn missing_fields_take_the_default() {
        let back: Prefs = serde_json::from_str(r#"{"theme":"light"}"#).unwrap();
        assert_eq!(back.theme, Theme::Light);
        assert_eq!(back.code_px, Prefs::default().code_px);
        assert_eq!(back.accent, Prefs::default().accent);
    }

    #[test]
    fn the_size_control_stops_at_both_ends() {
        let d = Prefs::default();
        assert_eq!(d.resized(1).code_px, d.code_px + 1);
        let big = Prefs {
            code_px: MAX_PX,
            ..d
        };
        assert_eq!(big.resized(1).code_px, MAX_PX);
        let small = Prefs {
            code_px: MIN_PX,
            ..d
        };
        assert_eq!(small.resized(-1).code_px, MIN_PX);
    }

    /// Whatever is chosen, the block is one rule and every property in it is
    /// closed — the page never sees a value somebody typed.
    #[test]
    fn the_stylesheet_is_one_rule_of_known_values() {
        for theme in [Theme::Dark, Theme::Midnight, Theme::Light] {
            for accent in [Accent::Blue, Accent::Rose] {
                let css = Prefs {
                    theme,
                    accent,
                    ..Prefs::default()
                }
                .css();
                assert!(css.starts_with(":root{"), "{css}");
                assert_eq!(css.matches('{').count(), 1, "{css}");
                assert_eq!(css.matches('}').count(), 1, "{css}");
                assert!(!css.contains("</"), "{css}");
                // Every colour the stylesheet reads has to arrive, or the
                // default under it shows through in the wrong theme.
                for name in ["--bg:", "--fg:", "--accent:", "--tint:", "--guide:"] {
                    assert!(css.contains(name), "{name} missing from {css}");
                }
            }
        }
    }

    /// The bug this table exists to fix: four buttons that all drew the same
    /// face, because three of them named families a page cannot install and the
    /// browser walked past every one of them. Every slot has to ask for a
    /// different family, and has to be named after the family it asks for —
    /// otherwise a button offers a font that is not on the machine, which is
    /// the same nothing wearing a different label.
    #[test]
    fn every_platform_offers_different_faces_under_their_own_names() {
        for on in [Platform::Mac, Platform::Windows, Platform::Other] {
            let faces = table(on);
            assert!(faces.len() >= 4, "{on:?} offers only {}", faces.len());
            for (i, face) in faces.iter().enumerate() {
                assert!(
                    face.stack.ends_with("monospace"),
                    "{:?} {} does not end in the generic",
                    on,
                    face.label
                );
                // The family asked for first is the one named on the button and
                // in its tooltip. The system slot is the exception: it has no
                // family, it has whatever the browser reaches for.
                if i > 0 {
                    let first = face
                        .stack
                        .split(',')
                        .next()
                        .unwrap()
                        .trim()
                        .replace('"', "");
                    assert_eq!(first, face.title, "{on:?}");
                    assert!(face.title.starts_with(face.label), "{on:?} {}", face.label);
                }
                for other in &faces[i + 1..] {
                    assert_ne!(face.label, other.label, "{on:?}");
                    assert_ne!(face.stack, other.stack, "{on:?}");
                }
            }
        }
    }

    /// The stylesheet's own `--mono` is the `System` slot, spelled the same
    /// way. A page is drawn once before the saved preferences are read, and if
    /// these two drifted apart that first paint would be in a different face
    /// from the one the panel says is on.
    #[test]
    fn the_stylesheet_starts_on_the_face_the_system_slot_asks_for() {
        let css = include_str!("../../assets/style.css");
        assert!(
            css.contains(&format!("--mono: {};", SYSTEM.stack)),
            "style.css does not start on {}",
            SYSTEM.stack
        );
    }

    /// Why the labels are short. The row they sit in is 318px across — a 460px
    /// panel, less its padding, less the 96px label column and the gap after it
    /// — and `.modegroup` neither wraps nor scrolls, so a longer set of names
    /// runs out of the panel. The estimate here is deliberately pessimistic:
    /// 32px of button around each label and 4.9px a character at 11px in the
    /// app's own sans. Drawn for real in both engines, the five on a Mac come to
    /// 305.6px against the 318 and the set this replaced came to 332.1px, which
    /// did overflow — so this budget rejects a row a little before the panel
    /// does.
    #[test]
    fn the_row_of_names_fits_the_panel() {
        for on in [Platform::Mac, Platform::Windows, Platform::Other] {
            let faces = table(on);
            let chars: usize = faces.iter().map(|f| f.label.chars().count()).sum();
            let width = 32.0 * faces.len() as f32 + 4.9 * chars as f32;
            assert!(width <= 318.0, "{on:?} needs {width:.0}px of 318");
        }
    }

    #[test]
    fn the_machine_is_read_off_the_user_agent() {
        let cases = [
            (
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15",
                Platform::Mac,
            ),
            (
                "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X)",
                Platform::Mac,
            ),
            (
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36",
                Platform::Windows,
            ),
            (
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36",
                Platform::Other,
            ),
            // A browser that will not say, which is a table rather than a
            // crash.
            ("", Platform::Other),
        ];
        for (ua, want) in cases {
            assert_eq!(Platform::from_user_agent(ua), want, "{ua}");
        }
    }

    /// A name saved from the set of fonts this used to offer is a font that is
    /// gone, and nothing else. The theme and the size were saved in the same
    /// record and are still good.
    #[test]
    fn a_font_that_is_no_longer_offered_costs_only_the_font() {
        for saved in ["\"fira\"", "9", "null"] {
            let raw = format!(r#"{{"theme":"light","mono":{saved},"code_px":15}}"#);
            let back: Prefs = serde_json::from_str(&raw).unwrap();
            assert_eq!(back.mono, Mono::default(), "{raw}");
            assert_eq!(back.theme, Theme::Light, "{raw}");
            assert_eq!(back.code_px, 15, "{raw}");
        }
    }

    #[test]
    fn the_chosen_face_is_the_one_the_stylesheet_asks_for() {
        for m in Mono::all() {
            let css = Prefs {
                mono: m,
                ..Prefs::default()
            }
            .css();
            assert!(css.contains(&format!("--mono:{};", m.stack())), "{css}");
        }
        // And picking one is a change, not a relabelling of the same face.
        for m in Mono::all().skip(1) {
            assert_ne!(m.stack(), Mono::default().stack(), "{}", m.label());
        }
    }

    /// A slot saved where there were more faces to choose from — a Mac, which
    /// has five — opened somewhere with four. It has to land somewhere real,
    /// and the only honest somewhere is the default.
    #[test]
    fn a_slot_this_machine_does_not_have_falls_back() {
        let over = table(platform()).len();
        let raw = format!(r#"{{"mono":{over}}}"#);
        let back: Prefs = serde_json::from_str(&raw).unwrap();
        assert_eq!(back.mono, Mono::default());
        // And one that does exist survives, so the fallback is not just always
        // firing.
        let last = over - 1;
        let raw = format!(r#"{{"mono":{last}}}"#);
        let back: Prefs = serde_json::from_str(&raw).unwrap();
        assert_eq!(back.mono, Mono(last as u8));
    }

    #[test]
    fn a_light_theme_says_so_to_the_browser() {
        let light = Prefs {
            theme: Theme::Light,
            ..Prefs::default()
        };
        assert!(light.css().contains("color-scheme:light"));
        assert!(Prefs::default().css().contains("color-scheme:dark"));
    }
}
