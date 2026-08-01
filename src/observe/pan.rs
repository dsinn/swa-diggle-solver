//! Reaching a node that is on the map but not on the screen.
//!
//! A dump gives every adjacent node's position in screen space, but the map is larger than the
//! window and the HUD is drawn on top of it, so a node can be perfectly adjacent and still be
//! somewhere no click may be sent — see [`super::hud`]. One live dump put the exit road at
//! `(213, 18)`, under the inventory button, and two other exits at `(2153, -3)` and `(1958, 1654)`,
//! entirely off-screen.
//!
//! The map can be scrolled to bring such a node into reach. What makes this delicate is that
//! scrolling is **silent**.
//!
//! ## Why the offset must be tracked by us
//!
//! There are three ways `xoffset` changes, and only one of them announces itself:
//!
//! | mechanism | source | dump? |
//! |---|---|---|
//! | animated centring | `centreScreenOn(.., instant=false)` sets `offsetTransition = 0` (`:1544-1554`) | **yes**, `Screen pan finished` when it reaches 1 (`:1253-1255`) |
//! | instant centring | same function, `instant = true` — assigns `xoffset` outright | no |
//! | hotspot scrolling | `xoffset = clampWithinBoundsX(xoffset - hotspotX*delta*300*accel)` (`:1263-1265`) | no |
//! | mouse drag | `xoffset = clampWithinBoundsX(xoffset + x - mouseDragStartX)` (`:1511`) | no |
//!
//! Arrow-key panning is the third row. It integrates a velocity per frame while the key is held, so
//! the distance travelled depends on frame timing, and nothing prints afterwards. A run that pans and
//! then trusts its old coordinates is in exactly the state that put a click 42 px short of the
//! Ulrome well.
//!
//! So the offset is ours to track, and the only honest way to know it is to **measure the frame**.
//! `diggle pantest` already does this by crop-and-track: lift a patch from the map, find it again
//! after the pan, and the displacement is the shift. That needs no sprite identity and no
//! calibration constant, and it cancels tint and scale because it compares a rendering against
//! itself.
//!
//! ## What this module is
//!
//! The bookkeeping half — where a node will be after a shift, and what shift would bring it into
//! reach. The measuring half is the frame comparison, which belongs with the capture code.

use crate::observe::adjacency::Adjacency;

/// A translation of the map in screen pixels, as measured between two frames.
///
/// Positive `dx` means map content moved right, so a node's screen x grew by `dx`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shift {
    pub dx: f64,
    pub dy: f64,
}

impl Shift {
    pub const NONE: Shift = Shift { dx: 0.0, dy: 0.0 };

    /// Did the map move enough to invalidate coordinates taken before it?
    ///
    /// The threshold is deliberately small. The drift that defeated three runs was about
    /// `(+42, +21)`, which is well under a node sprite's width — a shift does not have to be
    /// dramatic to move a click off a target and onto nothing.
    pub fn matters(&self) -> bool {
        self.dx.abs() >= 4.0 || self.dy.abs() >= 4.0
    }
}

/// Where a point recorded before a shift is now.
pub fn moved(point: (f64, f64), by: Shift) -> (f64, f64) {
    (point.0 + by.dx, point.1 + by.dy)
}

/// A dump's node and exit positions, corrected for a measured shift.
///
/// Returns a new `Adjacency` rather than mutating, so the original stays available as the record of
/// what the game actually printed — which is what any later disagreement has to be checked against.
pub fn corrected(dump: &Adjacency, by: Shift) -> Adjacency {
    let mut out = dump.clone();
    for n in &mut out.nodes {
        let (x, y) = moved((n.x, n.y), by);
        n.x = x;
        n.y = y;
    }
    for e in &mut out.exits {
        let (x, y) = moved((e.x, e.y), by);
        e.x = x;
        e.y = y;
    }
    out
}

/// Margin kept between a target and the edge of the window, in pixels.
///
/// A node exactly on the boundary is not usefully clickable: its sprite is half drawn, and the
/// click point is the node's centre rather than whatever pixel happens to be visible.
const EDGE: f64 = 120.0;

/// The shift that would bring `point` into a comfortably clickable part of the window.
///
/// Returns [`Shift::NONE`] when the point is already fine. Note that this answers "how far", not
/// "how" — the caller still has to produce the movement and then *measure* what it actually got,
/// because the movement is open-loop (see the module note).
///
/// Deliberately does not consult [`super::hud`]. Chrome sits at the window's corners, so a point
/// nudged clear of the edges is usually clear of chrome too — but not always, and a caller must
/// re-check with `hud::is_map_point` after measuring rather than assuming this was sufficient.
pub fn shift_to_reach(point: (f64, f64), client_w: i32, client_h: i32) -> Shift {
    let (w, h) = (client_w as f64, client_h as f64);
    let dx = if point.0 < EDGE {
        EDGE - point.0
    } else if point.0 > w - EDGE {
        w - EDGE - point.0
    } else {
        0.0
    };
    let dy = if point.1 < EDGE {
        EDGE - point.1
    } else if point.1 > h - EDGE {
        h - EDGE - point.1
    } else {
        0.0
    };
    Shift { dx, dy }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observe::adjacency::{Exit, Node};

    fn dump_with(nodes: Vec<Node>, exits: Vec<Exit>) -> Adjacency {
        Adjacency {
            reason: "Arrived at location".into(),
            here_key: "l10sub6".into(),
            here_heading: "Ulrome guard post".into(),
            subworld: Some(("l10".into(), "Ulrome — level 6 village".into())),
            nodes,
            hidden: 0,
            exits,
            hidden_exits: 0,
        }
    }

    #[test]
    fn the_drift_that_defeated_three_runs_counts_as_movement() {
        // Measured between the arrival dump and the screen: about (+42, +21). Small enough to look
        // like noise, big enough to select nothing.
        assert!(Shift { dx: 42.0, dy: 21.0 }.matters());
        assert!(!Shift { dx: 1.0, dy: -2.0 }.matters());
    }

    #[test]
    fn correcting_a_dump_moves_nodes_and_exits_alike() {
        // Exits drift with everything else -- they are drawn from the same offset -- so a correction
        // that fixed only the nodes would leave the way out wrong.
        let d = dump_with(
            vec![Node { key: "l10_plaza".into(), heading: "Ulrome well".into(), x: 1183.0, y: 679.0, connections: 7 }],
            vec![Exit { x: 213.0, y: 18.0, to_key: "l19".into(), to_heading: "Gipsyville crypt".into() }],
        );
        let c = corrected(&d, Shift { dx: 42.0, dy: 21.0 });
        assert_eq!((c.nodes[0].x, c.nodes[0].y), (1225.0, 700.0));
        assert_eq!((c.exits[0].x, c.exits[0].y), (255.0, 39.0));
        // The original is left intact as the record of what the game printed.
        assert_eq!((d.nodes[0].x, d.nodes[0].y), (1183.0, 679.0));
    }

    #[test]
    fn a_node_already_in_the_open_needs_no_shift() {
        assert_eq!(shift_to_reach((1183.0, 679.0), 1920, 1080), Shift::NONE);
    }

    #[test]
    fn the_exit_under_the_inventory_button_is_pulled_clear() {
        // (213, 18) is inside the inventory button. Pulling it to the EDGE margin puts it at
        // (120, 120), which hud::is_map_point accepts.
        let s = shift_to_reach((213.0, 18.0), 1920, 1080);
        let (x, y) = moved((213.0, 18.0), s);
        assert_eq!((x, y), (213.0, 120.0));
        assert!(crate::observe::hud::is_map_point(x as i32, y as i32, 1920, 1080));
    }

    #[test]
    fn an_exit_far_off_screen_is_pulled_all_the_way_in() {
        // (2153, -3), from the same live dump: past the right edge and above the top.
        let s = shift_to_reach((2153.0, -3.0), 1920, 1080);
        let (x, y) = moved((2153.0, -3.0), s);
        assert_eq!((x, y), (1800.0, 120.0));
        assert!(crate::observe::hud::is_map_point(x as i32, y as i32, 1920, 1080));
    }
}
