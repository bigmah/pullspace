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

/// The face the code is set in.
///
/// Named families with the old default behind each of them: this is a page, and
/// a page cannot install a font. Picking one that is not on the machine costs
/// nothing and changes nothing — the browser walks the list to something that
/// is there.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mono {
    #[default]
    System,
    JetBrains,
    Fira,
    Plex,
}

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

impl Mono {
    pub fn label(self) -> &'static str {
        match self {
            Mono::System => "System",
            Mono::JetBrains => "JetBrains Mono",
            Mono::Fira => "Fira Code",
            Mono::Plex => "IBM Plex Mono",
        }
    }

    fn stack(self) -> &'static str {
        // Every one of them ends where the default begins, so a family that is
        // not installed lands back on the system's own monospace.
        match self {
            Mono::System => {
                "\"SF Mono\", \"Menlo\", \"Cascadia Code\", \"JetBrains Mono\", monospace"
            }
            Mono::JetBrains => "\"JetBrains Mono\", \"SF Mono\", \"Menlo\", monospace",
            Mono::Fira => "\"Fira Code\", \"SF Mono\", \"Menlo\", monospace",
            Mono::Plex => "\"IBM Plex Mono\", \"SF Mono\", \"Menlo\", monospace",
        }
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
    conflict: &'static str,
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
    conflict: "#d858c8",
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
    conflict: "#a832a0",
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
             --added:{added};--modified:{modified};--deleted:{deleted};--conflict:{conflict};\
             --add-bg:{add_bg};--add-emph:{add_emph};--del-bg:{del_bg};--del-emph:{del_emph};\
             --accent:#{r:02x}{g:02x}{b:02x};\
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
            conflict = p.conflict,
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
            mono: Mono::Fira,
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
