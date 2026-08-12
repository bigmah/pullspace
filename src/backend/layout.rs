//! Pane sizes, kept between runs.
//!
//! A layout someone dragged into place is worth remembering. Widening the
//! explorer again at every launch is exactly the sort of small tax that makes a
//! tool feel unfinished — and the file is three numbers, so losing it costs
//! nothing either.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::auth::config_dir;

/// Pane sizes in CSS pixels.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    /// The explorer's width.
    pub side_w: f64,
    /// The conversation pane's width, when it is not folded away.
    pub conv_w: f64,
    /// The search panel's height, when one is open.
    pub bottom_h: f64,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            side_w: 280.0,
            conv_w: 380.0,
            bottom_h: 240.0,
        }
    }
}

impl Layout {
    /// Bring anything absurd back into range. A saved layout is data from
    /// outside the program — an edited or truncated file should cost the
    /// layout, not the window.
    fn sane(self) -> Self {
        let ok = |v: f64, lo: f64, hi: f64, fallback: f64| {
            if v.is_finite() && v >= lo && v <= hi {
                v
            } else {
                fallback
            }
        };
        let d = Layout::default();
        Self {
            side_w: ok(self.side_w, 120.0, 1200.0, d.side_w),
            conv_w: ok(self.conv_w, 160.0, 1200.0, d.conv_w),
            bottom_h: ok(self.bottom_h, 60.0, 1600.0, d.bottom_h),
        }
    }
}

fn layout_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("layout.json"))
}

/// What was saved last, or the defaults. Never fails: a layout that cannot be
/// read is one the user drags back into place, not a reason to stop.
pub fn load() -> Layout {
    let Some(path) = layout_path() else {
        return Layout::default();
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Layout>(&raw).ok())
        .unwrap_or_default()
        .sane()
}

/// Best effort — a layout that could not be written is not worth interrupting
/// anyone over.
pub fn save(layout: Layout) {
    let Some(path) = layout_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(body) = serde_json::to_string(&layout) {
        let _ = std::fs::write(path, body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonsense_falls_back_to_the_default() {
        let d = Layout::default();
        let broken = Layout {
            side_w: f64::NAN,
            conv_w: -40.0,
            bottom_h: 9_000.0,
        };
        let fixed = broken.sane();
        assert_eq!(fixed.side_w, d.side_w);
        assert_eq!(fixed.conv_w, d.conv_w);
        assert_eq!(fixed.bottom_h, d.bottom_h);
    }

    #[test]
    fn a_dragged_layout_survives_a_round_trip() {
        let mine = Layout {
            side_w: 331.0,
            conv_w: 500.0,
            bottom_h: 180.0,
        };
        let raw = serde_json::to_string(&mine).unwrap();
        let back: Layout = serde_json::from_str(&raw).unwrap();
        assert_eq!(back.sane(), mine);
    }

    #[test]
    fn missing_fields_take_the_default() {
        let back: Layout = serde_json::from_str(r#"{"side_w":200.0}"#).unwrap();
        assert_eq!(back.side_w, 200.0);
        assert_eq!(back.conv_w, Layout::default().conv_w);
    }
}
