//! Draggable dividers between the explorer, the code, the results panel and the
//! conversation.
//!
//! The sizes are published as CSS custom properties on the app's root element
//! and read back by the stylesheet, so moving a divider re-renders exactly one
//! short template — not the file tree, and not the ten thousand lines of code
//! next to it. On the desktop backend every DOM event costs a round trip into
//! Rust, so what a `mousemove` touches is the whole cost of a drag.
//!
//! A drag is measured from where the press landed rather than from the pointer
//! alone, so the divider stays exactly under the cursor however far it travels
//! and whatever it passes over on the way.

use std::time::Duration;

// std's Instant panics on wasm32-unknown-unknown; web-time is std on native.
use web_time::Instant;

use dioxus::html::input_data::MouseButton;
use dioxus::prelude::*;

use crate::backend::layout::{self, Layout};

use super::app::St;

/// Which divider is in hand.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    /// The explorer's right-hand edge.
    Sidebar,
    /// The conversation pane's left-hand edge.
    Conv,
    /// The results panel's top edge — the one that moves vertically.
    Bottom,
}

/// A drag in progress: which divider, and the two numbers every later pointer
/// position is measured against.
#[derive(Clone, Copy, PartialEq)]
pub struct Drag {
    edge: Edge,
    /// Where along the divider's own axis the press landed — `x` for the two
    /// vertical dividers, `y` for the horizontal one.
    origin: f64,
    /// What the pane measured at that moment, already inside its limits.
    start: f64,
}

const MIN_SIDE: f64 = 170.0;
const MAX_SIDE: f64 = 720.0;
const MIN_CONV: f64 = 250.0;
const MAX_CONV: f64 = 780.0;
/// The results panel, folded as small as it is worth being. Matches
/// `.bottom`'s `min-height`.
const MIN_BOTTOM: f64 = 92.0;
/// What a drag always leaves for the code itself — the point of the window is
/// the file in the middle of it.
///
/// The stylesheet states these limits a second time, as `min-width` and
/// `max-width` on the panes — that copy is what catches a window resized
/// smaller than the layout it was left in. Change one and change the other.
const MIN_CENTER: f64 = 300.0;
/// And what a drag always leaves for the file above the results panel.
const MIN_ABOVE: f64 = 160.0;
/// The conversation pane folded down to its rail. Matches `.convrail` in the
/// stylesheet.
const RAIL_W: f64 = 34.0;
/// A press this soon after a *click* on the same divider — a press that let go
/// without moving it — is the second half of a double-click, and puts it back
/// where it started.
const DOUBLE: Duration = Duration::from_millis(400);

/// Far enough to have been a drag rather than a click. A press never lands on
/// exactly one pixel.
const SLOP: f64 = 1.0;

impl Edge {
    /// Whether this divider moves up and down rather than side to side.
    fn vertical(self) -> bool {
        matches!(self, Edge::Bottom)
    }

    /// Which way the pane grows as the pointer moves. The explorer opens to
    /// the right of its divider; the conversation is behind its own, and grows
    /// backwards — as does the results panel, which grows upwards.
    fn sign(self) -> f64 {
        match self {
            Edge::Sidebar => 1.0,
            Edge::Conv | Edge::Bottom => -1.0,
        }
    }

    fn size(self, st: &St) -> Signal<f64> {
        match self {
            Edge::Sidebar => st.side_w,
            Edge::Conv => st.conv_w,
            Edge::Bottom => st.bottom_h,
        }
    }

    fn default_size(self) -> f64 {
        let d = Layout::default();
        match self {
            Edge::Sidebar => d.side_w,
            Edge::Conv => d.conv_w,
            Edge::Bottom => d.bottom_h,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Edge::Sidebar => "explorer",
            Edge::Conv => "conversation",
            Edge::Bottom => "results panel",
        }
    }
}

/// Whatever the right-hand side of the window is already spending, so widening
/// the explorer cannot squeeze the code out from between them.
fn right_reserved(st: &St) -> f64 {
    if st.workspace.peek().pr().is_none() {
        0.0
    } else if *st.conv_open.peek() {
        *st.conv_w.peek()
    } else {
        RAIL_W
    }
}

/// How far this divider may travel, given what the rest of the window is using.
///
/// `main_size` is `(0, 0)` until the first resize report, which is the one
/// frame before anything can be dragged anyway; the fixed limits stand in for
/// it rather than pretending the window is zero pixels wide.
fn bounds(edge: Edge, st: &St) -> (f64, f64) {
    let (w, h) = *st.main_size.peek();
    let (lo, hi) = match edge {
        Edge::Sidebar => {
            let room = w - right_reserved(st) - MIN_CENTER;
            (MIN_SIDE, if w > 0.0 { room.min(MAX_SIDE) } else { MAX_SIDE })
        }
        Edge::Conv => {
            let room = w - *st.side_w.peek() - MIN_CENTER;
            (MIN_CONV, if w > 0.0 { room.min(MAX_CONV) } else { MAX_CONV })
        }
        Edge::Bottom => {
            let room = h - MIN_ABOVE;
            (MIN_BOTTOM, if h > 0.0 { room } else { 640.0 })
        }
    };
    // A window too small for both limits still has to answer with a range, and
    // the minimum is the one worth keeping — a pane below it is unreadable.
    (lo, hi.max(lo))
}

/// The grab strip between two panes: a hairline to look at, a wider band to
/// aim at.
#[component]
pub fn Splitter(edge: Edge) -> Element {
    let st = use_context::<St>();
    let mut drag = st.drag;
    let mut last = st.last_grab;

    let holding = matches!(*st.drag.read(), Some(d) if d.edge == edge);
    let axis = if edge.vertical() { "horiz" } else { "vert" };
    let cls = if holding {
        format!("splitter {axis} on")
    } else {
        format!("splitter {axis}")
    };
    let what = edge.label();

    rsx! {
        div {
            class: "{cls}",
            title: "Drag to resize the {what} · double-click to reset",
            onmousedown: move |e| {
                // Otherwise the press starts a text selection that runs away
                // across the window for as long as the drag lasts.
                e.prevent_default();
                let mut size = edge.size(&st);
                // Paired against the last press that let go without moving
                // anything — see `release`. Measured from the previous press
                // instead, an ordinary drag followed by a quick re-grab to
                // fine-tune would read as a double-click and throw the drag
                // away.
                let now = Instant::now();
                let again = (*last.peek())
                    .is_some_and(|(prev, at)| prev == edge && now.duration_since(at) < DOUBLE);
                if again {
                    // Cleared, so a third press is a fresh start rather than a
                    // second reset.
                    last.set(None);
                    size.set(edge.default_size());
                    remember(st);
                    return;
                }
                let (lo, hi) = bounds(edge, &st);
                let at = *size.peek();
                let p = e.client_coordinates();
                drag.set(Some(Drag {
                    edge,
                    origin: if edge.vertical() { p.y } else { p.x },
                    start: at.clamp(lo, hi),
                }));
            },
        }
    }
}

/// Everything under the pointer for as long as a drag lasts.
///
/// It carries the resize cursor over the whole window, keeps the panes from
/// lighting up as the pointer sweeps across them, and — the reason it exists —
/// catches the `mousemove`s that a divider one pixel wide never would.
#[component]
pub fn DragMask() -> Element {
    let st = use_context::<St>();
    let Some(d) = *st.drag.read() else {
        return rsx! {};
    };
    let cls = if d.edge.vertical() {
        "dragmask rows"
    } else {
        "dragmask cols"
    };
    rsx! {
        div {
            class: "{cls}",
            onmousemove: move |e| {
                // The button came up somewhere we could not see it — over the
                // window's title bar, or off the screen entirely. There is no
                // event for that, so this is where it is noticed: the first
                // move back inside with nothing held down ends the drag.
                //
                // Which is why there is no `onmouseleave` here. Ending on it
                // would cut a drag short at exactly the edge you are heading
                // for when you are giving a pane the whole window.
                if !e.held_buttons().contains(MouseButton::Primary) {
                    return release(st);
                }
                let p = e.client_coordinates();
                let at = if d.edge.vertical() { p.y } else { p.x };
                let (lo, hi) = bounds(d.edge, &st);
                let mut size = d.edge.size(&st);
                size.set((d.start + d.edge.sign() * (at - d.origin)).clamp(lo, hi));
            },
            onmouseup: move |_| release(st),
        }
    }
}

fn release(st: St) {
    let mut drag = st.drag;
    let Some(d) = *drag.peek() else { return };
    drag.set(None);

    // Only a press that let go without moving anything can be the first half
    // of a double-click — which is the rule everywhere else, and the reason a
    // drag is not one.
    let moved = (*d.edge.size(&st).peek() - d.start).abs() >= SLOP;
    let mut last = st.last_grab;
    last.set((!moved).then_some((d.edge, Instant::now())));
    if moved {
        remember(st);
    }
}

/// Bring the panes back inside limits that have moved: the window was resized,
/// or the conversation folded away and gave back the width it was holding.
///
/// This is what keeps the size the app is holding equal to the size on screen.
/// A divider can only follow the pointer if the number it starts from is the
/// number the pane is drawn at — so nothing else is allowed to do the
/// clamping. The panes are `flex: none` in the stylesheet, and this is it.
///
/// What was saved is deliberately left alone: a window made narrower for a
/// minute should not cost anyone the layout they chose.
pub fn refit(st: &St) {
    // The explorer first, because the conversation's room is measured from
    // what it leaves.
    for edge in [Edge::Sidebar, Edge::Conv, Edge::Bottom] {
        let mut size = edge.size(st);
        let (lo, hi) = bounds(edge, st);
        let now = *size.peek();
        let want = now.clamp(lo, hi);
        if want != now {
            size.set(want);
        }
    }
}

/// Write the sizes out, wherever the service layer keeps them. Once per drag,
/// so the cost of remembering a layout is a few dozen bytes when the mouse
/// comes up.
fn remember(st: St) {
    layout::save(Layout {
        side_w: *st.side_w.peek(),
        conv_w: *st.conv_w.peek(),
        bottom_h: *st.bottom_h.peek(),
    });
}
