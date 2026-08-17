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
use crate::observe::template::Template;
use crate::win::capture::Frame;

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

    /// Is this measurement consistent with the drag that was asked for?
    ///
    /// **A measurement can be wrong, and wrong is not the same as absent.** [`measure`] returns
    /// `None` when it cannot correlate the patch at all, and callers handle that. What it cannot
    /// report is a correlation that *succeeded* on the wrong piece of map — and the result is not a
    /// near miss, it is a displacement with no relation to the drag.
    ///
    /// The run of 2026-08-17 0436Z ended on one. Crossing `l10`:
    ///
    /// ```text
    /// panned by (380, -132) of (0, -293) wanted;  → now at (1868, 1121)
    /// panned by (-68, -162) of (-68, -161) wanted; → now at (1800, 959)
    /// input 140: click (1800,959) — crossing: select
    /// area slot: something else (Combat 0.1257, gate 0.95)
    /// ```
    ///
    /// 380 px sideways from a drag that asked for none. The second pull then measured almost exactly
    /// what it requested — it was a good drag — but it moved from an already-false belief about
    /// where the node was, so the click landed on empty ground. `0.1257` is an **empty** area slot;
    /// a slot with a button in it scored ~0.86 elsewhere in the same run.
    ///
    /// ## The rule, per axis
    ///
    /// - **An axis the drag did not ask to move must not have moved much.** This is the case above
    ///   and it is unambiguous.
    /// - **An axis it did ask for may fall short, including to nothing** — scrolling is clamped to
    ///   the map bounds (`clampWithinBoundsX`, `overworldview.lua:293-297`) and says nothing when it
    ///   clamps, so a short pull is ordinary. Only the *direction* is asserted.
    ///
    /// [`CROSS_AXIS_SLACK`] is a judgement rather than a measurement, and it is not load-bearing:
    /// the failure this exists for was 380 px against a limit of 16, more than an order of magnitude
    /// clear of wherever the line is drawn.
    pub fn agrees_with(&self, want: Shift) -> bool {
        let axis = |got: f64, asked: f64| {
            if asked.abs() < 1.0 {
                return got.abs() <= CROSS_AXIS_SLACK;
            }
            got.abs() <= CROSS_AXIS_SLACK || got.signum() == asked.signum()
        };
        axis(self.dx, want.dx) && axis(self.dy, want.dy)
    }
}

/// How far an axis may move when the drag asked nothing of it before the measurement is disbelieved.
///
/// Sixteen pixels, which is the node hit box the game itself uses: `mouseIsOverLocation` accepts a
/// click within `(selectionRadiusX or 16) * scale * zoomMult` (`overworldview.lua:1280-1293`), so at
/// zoom 1 a cross-axis error smaller than this cannot move a click off the node it was aimed at.
const CROSS_AXIS_SLACK: f64 = 16.0;

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

#[cfg(test)]
mod agreement_tests {
    use super::*;

    /// The measurement that ended the run of 2026-08-17 0436Z, and the good one beside it.
    ///
    /// Both numbers are transcribed from the log rather than invented, which is what makes the
    /// second assertion worth as much as the first: the pull that followed the bad one was almost
    /// perfect, so a rule that rejected it too would have thrown away a working pan and left the
    /// crossing no better off.
    #[test]
    fn a_pan_that_moved_sideways_from_a_vertical_request_is_disbelieved() {
        let asked = Shift { dx: 0.0, dy: -293.0 };
        let measured = Shift { dx: 380.0, dy: -132.0 };
        assert!(!measured.agrees_with(asked), "380 px on an axis we asked nothing of");

        let good = Shift { dx: -68.0, dy: -162.0 };
        assert!(good.agrees_with(Shift { dx: -68.0, dy: -161.0 }), "the next pull was fine");
    }

    /// Clamping is ordinary and must not read as a fault.
    ///
    /// `clampWithinBoundsX` says nothing when it clamps, so a pull that asked for a lot and got a
    /// little — or nothing — is the map's own edge answering, which [`pan_again`] already treats as
    /// a reason to stop rather than a reason to distrust the number.
    ///
    /// [`pan_again`]: crate::navigate
    #[test]
    fn a_short_or_clamped_pull_still_agrees() {
        let asked = Shift { dx: 0.0, dy: -300.0 };
        assert!(Shift { dx: 0.0, dy: -40.0 }.agrees_with(asked), "short is clamping, not error");
        assert!(Shift { dx: 0.0, dy: 0.0 }.agrees_with(asked), "and nothing at all is the bound");
        assert!(Shift { dx: 9.0, dy: -40.0 }.agrees_with(asked), "a little cross-axis slop is fine");
        // The wrong way, by more than the slack, is not clamping.
        assert!(!Shift { dx: 0.0, dy: 120.0 }.agrees_with(asked), "backwards is not a short pull");
    }
}

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

/// Minimum inlier score for a tracked patch to be believed.
///
/// A genuine frame-to-frame patch match scores ~1.000, because it is the same rendering compared
/// against itself. The readings that produced a bogus constant `-192` in the first travel attempt
/// scored 0.718 and 0.601 — the metric was there all along, and not checking it is what turned a
/// self-diagnosing instrument into a silent liar.
pub const MIN_INLIERS: f64 = 0.95;

/// Side of the square lifted from the map to track the pan with.
pub const PATCH: i32 = 96;

/// Lifts a square of the frame as a template.
///
/// Channels are swapped on the way: [`crate::win::capture::Frame`] is BGRA and
/// [`super::template::Template`] is compared as RGBA (`find_at_scale_in` reads `bgra[i+2]` as red).
/// A patch built without the swap still matches *itself* perfectly, so the mistake survives every
/// obvious test and only shows as a silent failure to find anything.
pub fn patch_from(frame: &Frame, x: i32, y: i32, size: i32) -> Option<Template> {
    if x < 0 || y < 0 || size <= 0 || x + size > frame.width || y + size > frame.height {
        return None;
    }
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for row in 0..size {
        for col in 0..size {
            let i = (((y + row) * frame.width + (x + col)) * 4) as usize;
            rgba.push(frame.bgra[i + 2]);
            rgba.push(frame.bgra[i + 1]);
            rgba.push(frame.bgra[i]);
            rgba.push(255);
        }
    }
    Some(Template { name: "map-patch".into(), width: size as u32, height: size as u32, rgba })
}

/// Mean per-channel variance — a cheap proxy for how distinctive a patch is.
///
/// A patch of flat void matches equally well everywhere it is searched, so the displacement it
/// reports is whichever candidate the sweep happened to visit first. Rejecting flat patches before
/// the drag is cheaper than disbelieving the answer afterwards.
pub fn variance(t: &Template) -> f64 {
    let n = (t.rgba.len() / 4) as f64;
    if n == 0.0 {
        return 0.0;
    }
    let mut sum = [0f64; 3];
    for px in t.rgba.chunks_exact(4) {
        for c in 0..3 {
            sum[c] += px[c] as f64;
        }
    }
    let mean: Vec<f64> = sum.iter().map(|s| s / n).collect();
    let mut var = 0f64;
    for px in t.rgba.chunks_exact(4) {
        for c in 0..3 {
            var += (px[c] as f64 - mean[c]).powi(2);
        }
    }
    var / (n * 3.0)
}

/// Finds `patch` again after a pan and reports how far the map actually moved.
///
/// `taken_at` is where the patch was lifted from, `expected` the shift we asked for. The search is
/// bounded around the expected landing place: that is a speed fix, and it also keeps distant
/// periodic false matches — cobblestone ground tiles, in this game — out of contention.
///
/// Returns `None` rather than a guess when the match is not convincing. The caller must treat that
/// as "the map is now somewhere unknown" and re-establish position, because a pan that happened but
/// was not measured is strictly worse than one that never happened.
pub fn measure(
    after: &Frame, patch: &Template, taken_at: (i32, i32), expected: Shift, radius: i32,
) -> Option<Shift> {
    let (ex, ey) = (
        taken_at.0 + expected.dx.round() as i32,
        taken_at.1 + expected.dy.round() as i32,
    );
    let bounds = Some((ex - radius, ey - radius, ex + radius, ey + radius));
    let m = super::template::find_at_scale_in(after, patch, 1.0, 1, bounds)?;
    (m.inliers >= MIN_INLIERS)
        .then(|| Shift { dx: (m.x - taken_at.0) as f64, dy: (m.y - taken_at.1) as f64 })
}
