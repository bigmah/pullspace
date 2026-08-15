//! Pane sizes, kept between visits.
//!
//! A layout someone dragged into place is worth remembering. Widening the
//! explorer again at every launch is exactly the sort of small tax that makes a
//! tool feel unfinished — and it is three numbers, so losing it costs nothing
//! either.

use serde::{Deserialize, Serialize};

use super::store;

/// Pane sizes in CSS pixels.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layout {
    /// The explorer's width.
    pub side_w: f64,
    /// The conversation pane's width, when it is not folded away. Wide enough
    /// for its three headings to be words rather than stubs — see `.convtab`
    /// in the stylesheet, which is where the room actually goes.
    pub conv_w: f64,
    /// The results panel's height, when something has put it up.
    pub bottom_h: f64,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            side_w: 280.0,
            conv_w: 440.0,
            bottom_h: 240.0,
        }
    }
}

impl Layout {
    /// Bring anything absurd back into range. A saved layout is data from
    /// outside the program — a hand-edited storage entry should cost the
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
            bottom_h: ok(self.bottom_h, 80.0, 1200.0, d.bottom_h),
        }
    }
}

/// What was saved last, or the defaults. Never fails: a layout that cannot be
/// read is one the user drags back into place, not a reason to stop.
pub fn load() -> Layout {
    store::get(store::LAYOUT)
        .and_then(|raw| serde_json::from_str::<Layout>(&raw).ok())
        .unwrap_or_default()
        .sane()
}

pub fn save(layout: Layout) {
    if let Ok(body) = serde_json::to_string(&layout) {
        store::set(store::LAYOUT, &body);
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
            bottom_h: f64::INFINITY,
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
            bottom_h: 300.0,
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
        assert_eq!(back.bottom_h, Layout::default().bottom_h);
    }
}
