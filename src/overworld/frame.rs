//! Where a node **is on screen**, and how much that answer is worth.
//!
//! Split out of `overworld.rs` on 2026-08-21 (#76). This module owns the one hard problem in the
//! planner: a dump prints screen coordinates under the current pan and zoom, so a position is
//! aimable *now* and meaningless after the view moves. Joining successive dumps into one frame is
//! what lets us aim at a node the current dump does not name.
//!
//! Four queued tasks land here and nowhere else — #21 (world coordinates as a heuristic), #29 (zoom
//! rather than drag), #57 (a per-container frame) and #64/#65 (resolution support) — which is why
//! it is the piece worth separating first.
//!
//! ## The two frames, and why the scale is remembered rather than assumed
//!
//! [`WorldMap::registration`] carries the full account; the short version is that a fit is a
//! **similarity**, `world = drawn * scale + offset`, because `zoomMult` scales every printed
//! position. One anchor cannot see the scale, and assuming `1.0` once split the frame in two and
//! cost a run. [`Frame::disagreement`] is the control that catches it, and no fit that fails
//! [`Frame::is_sound`] may place anything.
//!
//! The surface and the interior keep **separate** frames ([`InsideFrame`]) rather than one with a
//! flag, because interior coordinates are re-rolled per visit and a shared origin would join two
//! worlds that have nothing to do with each other.

use super::{exit_node_key, WorldMap, ANOMALY_KEY};
// Doc links only, same as in `place.rs`: the arguments moved here intact rather than being
// requalified to suit the file layout, because the doc comments in this crate are the design record.
#[allow(unused_imports)]
use super::Place;
use crate::observe::adjacency::Adjacency;
use std::collections::BTreeMap;

/// How a dump's printed coordinates relate to the map's own frame: `world = drawn * scale + offset`.
///
/// A similarity rather than a translation, because `zoomMult` scales every printed position
/// (`overworldview.lua:1033`) and the run of 2026-08-16 0802Z aimed a click through a frame recorded
/// at twice the current zoom. See [`WorldMap::registration`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub scale: f64,
    pub dx: f64,
    pub dy: f64,
    /// How many already-placed nodes this fit rests on. **One means the scale was not measured from
    /// this dump** — enough to keep placing nodes, not enough to aim a click at one.
    pub anchors: usize,
    /// Whether this dump measured its own scale, or inherited [`WorldMap::screen_scale`].
    pub scale_measured: bool,
    /// The furthest any anchor lands from where the frame already says it is, in screen pixels.
    ///
    /// **The positive control on the whole frame.** Every anchor is a node whose position we claim to
    /// know, so a fit that puts one of them somewhere else is telling us the frame is not one frame.
    ///
    /// **It needs three anchors to see anything**, and that is not a shortcoming to fix. The scale
    /// comes from a distance and the offset from one anchor, so any *two* anchors are satisfied
    /// exactly by construction whenever they differ by a scale — which is precisely what two mixed
    /// populations of positions do. The third is the one that has to choose between them. In the
    /// 1752Z run 60 dumps had three or more and every one of them screamed; the four that had two
    /// went quietly by. So this is the alarm, not the lock: the lock is measuring the scale rather
    /// than assuming it, above.
    pub disagreement: f64,
}

/// How far two anchors in one dump may disagree and still count as the same frame, in pixels.
///
/// Measured rather than chosen: replaying all 95 surface dumps of the 1752Z run under the current
/// rule, every dump agrees to **0.00 px** exactly — the transform really is exact, not approximate —
/// while the same replay under the old rule reaches 229 px. So anything above a few pixels is a
/// different frame rather than noise, and four leaves room for an icon that shifts when a node is
/// completed without coming near the corruption it is there to catch.
///
/// It also has to stay well under the game's own click tolerance, which is what a frame error
/// actually spends: `mouseIsOverLocation` accepts a click within `selectionRadius * scale * zoomMult`
/// of a node (`overworldview.lua:1283-1289`), 16 px by default and **8 px zoomed out**.
pub const FRAME_TOLERANCE: f64 = 4.0;

impl Frame {
    /// Whether nodes may be placed from this fit at all. See [`Frame::disagreement`].
    pub fn is_sound(&self) -> bool {
        self.disagreement <= FRAME_TOLERANCE
    }

    /// The fit a dump gets when it **defines** a frame rather than joining one: its own numbers,
    /// unshifted.
    ///
    /// `scale_measured` is true because "one screen pixel at this zoom" is exactly what the frame's
    /// units now mean — there is nothing to measure it against and nothing to get wrong. `anchors`
    /// is zero, which is what stops anything being *aimed* from it: see [`WorldMap::screen_position`]
    /// and [`WorldMap::inside_screen_position`], both of which demand two.
    pub const fn defining() -> Self {
        Frame { scale: 1.0, dx: 0.0, dy: 0.0, anchors: 0, scale_measured: true, disagreement: 0.0 }
    }
}

/// Fits `world = drawn * scale + offset` to nodes whose position we already know.
///
/// The arithmetic behind [`WorldMap::registration`], lifted out whole so that the interior of a
/// subworld can use it too ([`WorldMap::inside_registration`]). The two differ in *which* positions
/// they anchor against and in nothing else, and keeping one fitter means a bug found in one is
/// fixed in both — which matters here, because every line of it was written by a run that ended.
///
/// `inherited` is the scale to use when this dump cannot measure its own; `None` there means the
/// zoom has moved and nothing has re-measured, which is the one state in which fitting anyway would
/// write a wrong scale into a frame permanently. Answering `None` is the whole point of it.
///
/// Callers pass a non-empty `anchors`; an empty one is a *defining* dump and is the caller's
/// business, since only the caller knows whether its frame has been defined yet.
fn fit_frame(anchors: &[((f64, f64), (f64, f64))], inherited: Option<f64>) -> Option<Frame> {
    // The widest baseline on screen, which is the best-conditioned pair for a scale.
    let mut best: Option<(f64, usize, usize)> = None;
    for i in 0..anchors.len() {
        for j in (i + 1)..anchors.len() {
            let (_, (ax, ay)) = anchors[i];
            let (_, (bx, by)) = anchors[j];
            let d = ((bx - ax).powi(2) + (by - ay).powi(2)).sqrt();
            if best.map(|(bd, _, _)| d > bd).unwrap_or(true) {
                best = Some((d, i, j));
            }
        }
    }
    // A baseline of a few pixels cannot measure a ratio; treat it as one anchor rather than
    // trusting it. Nodes are drawn tens of pixels apart at any usable zoom.
    let (scale, scale_measured) = match best.filter(|(d, _, _)| *d > 8.0) {
        Some((drawn, i, j)) => {
            let ((wax, way), _) = anchors[i];
            let ((wbx, wby), _) = anchors[j];
            (((wbx - wax).powi(2) + (wby - way).powi(2)).sqrt() / drawn, true)
        }
        // Not measurable here, so fall back on the last dump that could measure it.
        None => (inherited?, false),
    };
    let ((wx, wy), (sx, sy)) = *anchors.first()?;
    let (dx, dy) = (wx - sx * scale, wy - sy * scale);
    // Where the fit puts each anchor, against where the frame already has it. Zero for one
    // anchor, which is the one the offset came from. See [`Frame::disagreement`].
    let disagreement = anchors
        .iter()
        .map(|((wx, wy), (sx, sy))| {
            ((sx * scale + dx - wx).powi(2) + (sy * scale + dy - wy).powi(2)).sqrt()
        })
        .fold(0.0f64, f64::max);
    Some(Frame { scale, dx, dy, anchors: anchors.len(), scale_measured, disagreement })
}

/// Where the inside of the subworld we are standing in is drawn, for **this visit only**.
///
/// ## Why the surface frame cannot do this job
///
/// [`WorldMap::registration`] refuses subworld dumps outright, and correctly: interior coordinates
/// are not in the surface frame and never can be. So [`Place::pos`] is empty for every subnode, and
/// a far hop that stops on an ordinary interior node has nothing to click — which is #57, and which
/// cost the run of 2026-08-21 four separate presses to reach an inn three nodes inside Enthorpe.
///
/// A dump prints positions for **adjacent connections** and for **subworld exits at any distance**
/// (`overworldview.lua:1030-1047`), so the doors were already reachable in one press and the rooms
/// were not.
///
/// ## Per visit, because that is exactly how long an interior layout lasts
///
/// `lostOrientation` re-rolls every interior coordinate — two reflections and a transpose,
/// `forest.lua:483-490` — and `overworldview.lua:1613` re-runs it from `loadLight`, which is every
/// reload rather than merely every re-entry. [`crate::subworld::Rules::positions_survive_reentry`]
/// is false for that reason and this obeys it: [`WorldMap::fold`] throws the whole thing away the
/// moment a dump names a different container, or none.
///
/// That is the rule, and the *guard* is separate and stronger: a re-roll is a reflection, which is
/// not a similarity, so a fit against stale positions disagrees with itself and
/// [`Frame::is_sound`] refuses it. Nothing is placed and nothing is aimed. Two independent
/// protections on a rule the map has been wrong about before.
///
/// ## What anchors it
///
/// A dump never prints the player's own position, so consecutive interior dumps often share no
/// *node* at all: standing at A we learn A's neighbours, and standing at B we learn B's, and B
/// itself was placed while A's dump was on screen. What carries the frame across is the **exits**,
/// which print at any distance and therefore appear in every dump of an uncorrupted interior. They
/// are ordinary road nodes in the same coordinates as everything else here.
#[derive(Debug, Default, Clone)]
pub struct InsideFrame {
    /// The container this frame belongs to. `None` on the surface, and a mismatch is what discards
    /// it — see [`WorldMap::fold`].
    pub(super) container: Option<String>,
    /// Interior nodes and doors, by key, in the frame the visit's first dump defined.
    pub(super) pos: BTreeMap<String, (f64, f64)>,
    /// This frame's units in screen pixels, as last measured. The interior twin of
    /// [`WorldMap::screen_scale`], and separate from it because the two frames were defined at
    /// different moments and so need not be at the same zoom.
    pub(super) scale: Option<f64>,
}

/// What one unit of the world frame is worth in screen pixels right now.
///
/// `None` means we have moved the zoom and no dump has measured the new scale yet — see
/// [`WorldMap::zoom_changed`]. Nothing may be placed in that state, because placing is what writes a
/// wrong scale into the frame permanently.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenScale(pub(super) Option<f64>);

impl Default for ScreenScale {
    /// A fresh map's frame is **defined** by its first dump, so it starts at screen scale exactly.
    /// That is why an early one-anchor dump can place nodes at all, and it is true right up until the
    /// first zoom.
    fn default() -> Self {
        ScreenScale(Some(1.0))
    }
}

impl WorldMap {
    /// Where a place is, with one estimate allowed: the anomaly, which we can locate without
    /// having seen it.
    ///
    /// **The anomaly is at the world origin, by construction.** `overworld/generators/world.lua:73`
    /// builds `locationData.start` at `posX=0, posY=0`, and `:505-507` turns that same node into the
    /// portal when hell opens. It never moves.
    ///
    /// That is not directly usable, because our frame is screen units with an unknown offset and
    /// scale ([`WorldMap::registration`]), so world `(0,0)` has no address in it. What *is* usable is
    /// that corruption spreads from the same origin. `hellCheck`
    /// (`overworld/locations/hellportal.lua:16-23`) is
    ///
    /// ```lua
    /// local dist = math.vdist(0,0,x,y)/42 --0 at the center
    /// return hellval > perlin*dist
    /// ```
    ///
    /// with `perlin` in `[0.5, 1]`, so a node is corrupt iff it lies within a radius of the origin
    /// that varies by at most 2x with direction. The corrupted nodes are a blob centred on the
    /// anomaly, and their centroid points at it.
    ///
    /// **This is an estimate and it is biased by where we have been.** The centroid is over the
    /// corrupted nodes *we have positioned*, so a run that has only seen the eastern edge of the
    /// blob will think the anomaly is east of where it is. It gets better as the map grows, and it
    /// is never worse than the alternative, which is no direction at all. It is used only to choose
    /// between adjacent steps when no route exists — never to decide that we have arrived.
    ///
    /// A real sighting always wins: once any dump has shown `start`, that position is exact and this
    /// estimate is not consulted.
    pub(super) fn pos_for(&self, key: &str) -> Option<(f64, f64)> {
        let place = self.places.get(key)?;
        if let Some(p) = place.pos {
            return Some(p);
        }
        if key != ANOMALY_KEY && !place.type_is("anomaly") {
            return None;
        }
        let corrupt: Vec<(f64, f64)> = self
            .places
            .values()
            .filter(|p| p.corrupted && p.parent.is_none())
            .filter_map(|p| p.pos)
            .collect();
        match corrupt.len() {
            0 => None,
            n => {
                let (sx, sy) = corrupt.iter().fold((0.0, 0.0), |(x, y), (a, b)| (x + a, y + b));
                Some((sx / n as f64, sy / n as f64))
            }
        }
    }

    /// Where this dump's coordinates sit relative to the frame we are assembling, if we can tell.
    ///
    /// ## The numbers in a dump are screen space, and they move
    ///
    /// `overworldview.lua:1033` prints
    ///
    /// ```lua
    /// posX = xoffset + location.posX*zoomMult
    /// posY = yoffset + location.posY*zoomMult + (typeData.offsetY or 0)*scale*zoomMult
    /// ```
    ///
    /// so one node reads differently in two dumps of the same place — the pan offset changed. What
    /// is underneath (`location.posX`) is a stable world coordinate, and the transform is affine, so
    /// **within a single dump every node shares one offset**. Two dumps can therefore be put in the
    /// same frame by any node they have in common: the difference between its known position and its
    /// reading here is the shift for everything else in that dump.
    ///
    /// Consecutive dumps always share nodes — we arrive somewhere adjacent to where we were — so the
    /// map assembles into one frame, up to a global translation and scale nobody needs to know.
    ///
    /// ## Surface only, deliberately
    ///
    /// `zoomMult` is a *shared* factor, so mixing frames at different zooms would silently rescale
    /// distances, and nothing here checks the zoom. Subworld interiors are excluded rather than
    /// assumed safe: they already route by BFS over recorded edges, which works, so they have
    /// nothing to gain and a wrong scale to lose. The exits printed inside a subworld are road nodes
    /// in the *subworld's* frame anyway — the surface node each one names is learned from a surface
    /// dump, which is where the useful position comes from.
    ///
    /// Returns `None` when this dump shares no placed node with the frame, which is not an error: it
    /// is what an unregistered island looks like, and those get placed the first time a dump links
    /// them to something we have already seen.
    /// ## Zoom is not a detail, and one anchor cannot see it
    ///
    /// This returned a translation and nothing else, on the reasoning that a dump differs from the
    /// frame by an offset. That is true only **at a fixed zoom**, which the doc above said and the
    /// code did not check — and the run of 2026-08-16 0802Z paid for it. Standing at `l7`, aiming
    /// one press at `l32`, the first anchor in the dump gave a shift of `(-249.9, -280.9)` and the
    /// second gave `(-176.6, -189.1)`. Two anchors, two answers, from the same dump.
    ///
    /// The separations say why: `l10` to `l1` is 235.01 apart in the frame and 117.50 apart on
    /// screen — a ratio of **2.0000**, exactly. The stored positions were recorded at one zoom and
    /// the dump was printed at another, `zoomMult` being a shared factor (`overworldview.lua:1033`).
    /// The click went to (463, 841) instead of roughly (672, 713), landed on empty ground, and the
    /// run stopped with `no arrival at l32`.
    ///
    /// So the fit is a **similarity**, not a translation: `world = drawn * scale + offset`. Two
    /// anchors determine it, and the pair furthest apart on screen is used because a short baseline
    /// turns small pixel differences into large scale errors.
    ///
    /// ## One anchor cannot see the scale, and assuming `1.0` broke the frame permanently
    ///
    /// The paragraph that used to sit here said the scale is "assumed unchanged" with one anchor —
    /// "right nearly always, since the zoom only moves when we move it, and wrong exactly when it has
    /// just moved" — and concluded that a caller may decline to *aim* on such a fit, "which placing a
    /// node is not required to do". **That conclusion was backwards, and it cost the run of
    /// 2026-08-16 1752Z.** A bad aim costs one click. A bad *placement* is written into the frame and
    /// into the cache, and every later fit inherits it.
    ///
    /// What "assumed unchanged" actually assumed was `scale = 1.0`, i.e. that the frame is still at
    /// screen scale — true only before the first zoom. The run zoomed out at step 43. Four dumps
    /// later a one-anchor dump placed three nodes at half size; from then on the frame was **two
    /// frames**, each internally exact, differing by a factor of two, and every fit that drew an
    /// anchor from each returned a meaningless average. Replayed from the log: dumps 0–22 agree to
    /// 0.00 px, and 60 of the 95 surface dumps after it have anchors disagreeing by up to 229 px.
    /// 130 steps later a far hop aimed at `l11` from that frame, clicked (727, 123), and the node was
    /// drawn at (763, 237) — 119 px away, against a selection box that is ±8 px when zoomed out
    /// (`overworldview.lua:1283-1289`). Nothing was selected, `Travel` did nothing, and the run
    /// stopped with `no arrival at l11`. The frame at the end of that run is `map-cache/world-0.txt`,
    /// and it is why [`CACHE_VERSION`] is now v3.
    ///
    /// So the scale is **remembered rather than assumed**: [`WorldMap::screen_scale`] holds the last
    /// one a dump measured, a one-anchor dump inherits it, and after a zoom it is `None` until some
    /// dump measures the new one ([`WorldMap::zoom_changed`]). Replaying the same 95 dumps under that
    /// rule places all 40 nodes the old rule placed — nothing is lost — with a worst disagreement of
    /// **0.00 px** across the whole run.
    ///
    /// [`Frame::disagreement`] is the positive control that would have caught it in one line either
    /// way, and no fit that fails it may place anything. See [`Frame::is_sound`].
    pub(super) fn registration(&self, a: &Adjacency) -> Option<Frame> {
        if a.subworld.is_some() {
            return None;
        }
        // Everything in this dump we have already placed, in the frame and on screen.
        let anchors: Vec<((f64, f64), (f64, f64))> = a
            .nodes
            .iter()
            .filter_map(|n| self.places.get(&n.key).and_then(|p| p.pos).map(|w| (w, (n.x, n.y))))
            .collect();
        if anchors.is_empty() {
            // Nothing placed yet anywhere: this dump *defines* the frame, so its own numbers are the
            // frame. Only ever taken once per run.
            return match self.places.values().any(|p| p.pos.is_some()) {
                false => Some(Frame::defining()),
                true => None,
            };
        }
        // `None` when the scale is not measurable here and nothing has measured it since a zoom —
        // the one state in which placing would write a wrong scale into the frame for ever, so this
        // dump does nothing at all. See [`fit_frame`], which the interior shares.
        fit_frame(&anchors, self.screen_scale.0)
    }

    /// Everything in an interior dump that the visit's frame has already placed.
    ///
    /// Nodes **and** doors, and the doors are the load-bearing half: the exits section prints at any
    /// distance (`overworldview.lua:1040-1047`), so it is the one part of a dump guaranteed to
    /// overlap with the last one. Adjacent connections need not overlap at all — see
    /// [`InsideFrame`].
    pub(super) fn inside_anchors(&self, a: &Adjacency) -> Vec<((f64, f64), (f64, f64))> {
        let Some((container, _)) = a.subworld.as_ref() else { return Vec::new() };
        let mut out = Vec::new();
        for n in &a.nodes {
            if let Some(&w) = self.inside_frame.pos.get(&n.key) {
                out.push((w, (n.x, n.y)));
            }
        }
        for e in &a.exits {
            if let Some(&w) = self.inside_frame.pos.get(&exit_node_key(container, &e.to_key)) {
                out.push((w, (e.x, e.y)));
            }
        }
        out
    }

    /// Where this interior dump's coordinates sit relative to the visit's frame, if we can tell.
    ///
    /// The interior twin of [`WorldMap::registration`]; see [`InsideFrame`] for what makes the two
    /// separate rather than one function with a flag. Answers `None` for a surface dump, for a
    /// container that is not the one the frame was built for — [`WorldMap::fold`] resets it before
    /// asking, so that case is a caller mistake rather than a state — and for a dump that shares
    /// nothing with a frame that has already been defined.
    pub(super) fn inside_registration(&self, a: &Adjacency) -> Option<Frame> {
        let (container, _) = a.subworld.as_ref()?;
        if self.inside_frame.container.as_deref() != Some(container.as_str()) {
            return None;
        }
        let anchors = self.inside_anchors(a);
        if anchors.is_empty() {
            // The first dump of a visit *defines* the frame. A later one that shares nothing with it
            // cannot be joined to it, and inventing a second origin is how a frame becomes two.
            return match self.inside_frame.pos.is_empty() {
                true => Some(Frame::defining()),
                false => None,
            };
        }
        fit_frame(&anchors, self.inside_frame.scale)
    }

    /// Where an interior node we have already placed would be **drawn right now**, given this dump.
    ///
    /// The interior counterpart of [`WorldMap::screen_position`], and #57's whole payoff: a far hop
    /// that stops on an ordinary room rather than on a door now has somewhere to click. See
    /// [`InsideFrame`] for the lifetime of the answer, which is one visit.
    ///
    /// Two anchors are demanded before anything may be aimed at, exactly as on the surface — one
    /// anchor fixes an offset without ever testing it, and a click is spent whether or not the test
    /// would have passed.
    pub fn inside_screen_position(&self, a: &Adjacency, key: &str) -> Option<(f64, f64)> {
        let f = self.inside_registration(a).filter(|f| f.anchors >= 2 && f.is_sound())?;
        let (wx, wy) = self.inside_frame.pos.get(key).copied()?;
        Some(((wx - f.dx) / f.scale, (wy - f.dy) / f.scale))
    }

    pub fn zoom_changed(&mut self) {
        self.screen_scale = ScreenScale(None);
        // The interior frame is a second frame with a second scale, and the zoom is shared between
        // them — `zoomMult` is one module local (`overworldview.lua:1033`). Forgetting this half
        // would reproduce the 1752Z fault one level down, where it would be harder to see: an
        // interior frame lives for one visit, so the corrupted one dies before anyone reads a log.
        self.inside_frame.scale = None;
    }

    /// How far this dump disagrees with the frame, when it disagrees enough to stop us placing.
    ///
    /// For the log line that names the fault on the spot. The 1752Z run had this signal in every
    /// dump for 130 steps and printed none of it, so the corruption was only found afterwards, from a
    /// screenshot that happened to catch the tooltip of the node we had missed.
    pub fn frame_disagreement(&self, a: &Adjacency) -> Option<f64> {
        self.registration(a).filter(|f| !f.is_sound()).map(|f| f.disagreement)
    }

    /// Where a node we have already placed would be **drawn right now**, given this dump.
    ///
    /// The inverse of [`WorldMap::registration`], and the piece that was missing rather than the
    /// frame itself. `registration` returns the shift that takes this dump's numbers into the world
    /// frame — `world = drawn + shift` — so the way back is simply `drawn = world - shift`.
    ///
    /// **This is what makes a distant node clickable.** A dump prints positions for adjacent
    /// connections only (`overworldview.lua:1030-1035`), so a node two hops away has no coordinate
    /// of its own in it; that was read as "we cannot aim at it" and it is not, because the dump
    /// carries several nodes we *have* placed and any one of them fixes the camera exactly. Measured
    /// over 80 settled dumps from the run of 2026-08-16, every node shared between consecutive dumps
    /// agreed on the same shift **to 0.0000 px** — the transform is a pure translation at fixed
    /// zoom, so this is exact rather than an estimate.
    ///
    /// Two things it is not:
    ///
    /// - **not valid across a zoom.** `zoomMult` scales the frame (`:1033`) and nothing here checks
    ///   it, which is the same limitation `registration` already carries. A zoom invalidates the
    ///   answer until a fresh dump re-registers.
    /// - **not valid after a pan.** The shift is this dump's. Pan, and the drawn position moves with
    ///   everything else — which is exactly what the caller's pan machinery already measures.
    ///
    /// `None` when the node has no world position, or when the dump shares nothing with the frame.
    pub fn screen_position(&self, a: &Adjacency, key: &str) -> Option<(f64, f64)> {
        let f = self.registration(a).filter(|f| f.anchors >= 2 && f.is_sound())?;
        let (wx, wy) = self.places.get(key)?.pos?;
        Some(((wx - f.dx) / f.scale, (wy - f.dy) / f.scale))
    }

    /// Why [`WorldMap::screen_position`] could not answer. For the log, and only for the log.
    ///
    /// **Written because the one line this replaced named the wrong culprit for three runs.** It
    /// said *the frame cannot place it*, which reads as too few anchors and sends the reader at
    /// #21; replaying `spike-run-20260821-0313Z.log` and `-0357Z.log` says the frame was usable at
    /// **every one of their 73 and 109 surface dumps**, so in those two runs it cannot have been
    /// the frame at all. The three ways to have no coordinate are three different pieces of work
    /// and only one of them is the frame's.
    ///
    /// The interesting one is the middle: [`WorldMap::apply_save`] calls [`WorldMap::entry`] for
    /// every completed, corrupt, sacked, besieged and lost-woods key the save names, so a resumed
    /// run knows places it has never seen drawn — position `None`, no neighbours. The save's
    /// `_path_to_` completions land in `roads_done` and authorise a direct hop between the two
    /// nodes such a key names, so [`WorldMap::far_hop`] will happily nominate one. It is a real
    /// destination we simply cannot aim at yet, and stepping is the right answer; the frame is
    /// blameless and adding anchors would change nothing.
    ///
    /// Every node declined in the three runs above (`l20`, `shrine2`, `l10`, `l26`, `l28`) has its
    /// **first mention in the whole report on the decline line itself** — five for five.
    pub fn unplaceable(&self, a: &Adjacency, key: &str) -> String {
        let Some(p) = self.places.get(key) else {
            return format!("`{key}` is not on our map at all");
        };
        if p.pos.is_none() {
            return format!(
                "no dump has ever drawn `{key}` — we know it from the save, not from the screen"
            );
        }
        match self.registration(a) {
            None => "this dump shares nothing already placed, so the frame cannot speak here".into(),
            Some(f) if f.anchors < 2 => format!(
                "this dump shares {} placed node(s) with the frame, and measuring a scale needs two",
                f.anchors
            ),
            Some(f) => format!(
                "the frame disagrees with itself by {:.0} px, so nothing may be placed from it",
                f.disagreement
            ),
        }
    }

    /// Where the most recent dump put `key`, in that dump's own frame — doors included.
    ///
    /// Answers `None` for anything the latest dump did not name, which is most of the map. That is
    /// the point rather than a shortcoming: an answer here is always comparable with any other
    /// answer here, because both came from one print. Two calls are a straight-line distance apart;
    /// a call and a [`Place::pos`] are not, and must never be mixed.
    pub(super) fn placed_now(&self, key: &str) -> Option<(f64, f64)> {
        self.frame.get(key).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observe::adjacency::{Exit, Node};
    use crate::overworld::fixtures::*;
    use crate::overworld::*;

    /// Exploring with the portal live must walk **toward the corruption**, even when that is the
    /// longer way round in hops.
    ///
    /// The dev's rule, and their reason is simply that **the anomaly is there** — go where the goal
    /// is. The survival argument this doc used to carry was never theirs; it was added here and then
    /// cited back to them, which is worth remembering as a way of being wrong that leaves no trace.
    ///
    /// It was also false — see the note at the sort itself. Level falls with distance from the
    /// origin only until the portal opens; after it,
    /// `math.max(3, baseLevel, 7-baseLevel)` inverts the core (`world.lua:496-501`), so both the rim
    /// and the middle are dangerous and only one of them has the anomaly on it. Live 2026-08-10 a run
    /// at full health explored `l19 -> l28 -> l49`, a level 6 crypt, and died in it — with no bearing
    /// available, "nearest unvisited" was the whole strategy.
    ///
    /// The map: `far` is corrupted and positioned, so it stands in for the portal's direction.
    /// `toward` sits beside it but is **two** hops away; `away` is one hop and in the opposite
    /// direction. Hops alone pick `away`; the bearing picks `toward`.
    #[test]
    fn with_the_portal_open_exploration_heads_into_the_corruption_not_to_the_nearest_node() {
        let mut m = WorldMap::new();
        ready_for_the_anomaly(&mut m);
        m.fold(&dump(
            "here",
            "camp",
            vec![node_at("away", "Westerly meadow", -100.0, 0.0), node_at("step", "Easterly road", 50.0, 0.0)],
        ));
        // Lists `away`, which the first dump already placed, so this one can be **registered**
        // against the frame — `registration` anchors on a node it has a position for, and a dump it
        // cannot anchor places nothing at all.
        m.fold(&dump(
            "step",
            "Easterly road",
            vec![
                node_at("away", "Westerly meadow", -100.0, 0.0),
                node_at("toward", "Furtherly meadow", 100.0, 0.0),
            ],
        ));
        // Anchoring cost `away` its last unknown neighbour, and a node with nothing left to reveal
        // is not a destination at all — which would decide the test for the wrong reason. Both
        // candidates must stay worth walking to, so the only thing separating them is direction.
        m.entry("away").connections = 3;
        m.entry("toward").connections = 3;

        // The corruption blob, positioned and off the graph. `pos_for` averages it into a bearing
        // for the portal, which is the mechanism under test.
        m.entry("far").pos = Some((200.0, 0.0));
        m.entry("far").corrupted = true;

        // The portal, known and **unroutable** — which is the only case that produces a bearing at
        // all: `Goal::CloseAnomaly` declines for want of a way there, and exploring inherits the
        // direction it wanted. Reached by giving it an edge to a component of its own, since a node
        // with no edges is not the same thing as a node we cannot get to. Its position is never
        // observed; this dump cannot be registered against the frame, so it places nothing, and the
        // corruption centroid is what stands in.
        m.fold(&dump("island", "island camp", vec![node_at("start", "The Rift anomaly", 0.0, 0.0)]));
        m.here = Some("here".into());

        // Control: portal shut, so direction is not a consideration and the nearest wins.
        m.hell = Some(0.0);
        let shut = m.next_target().expect("something to explore");
        assert_eq!(shut.reason, Goal::Explore);
        assert_eq!(shut.target, "away", "with no portal, nearest unvisited is the whole rule");

        // Open: the same map, and now the far side of the corruption wins despite the extra hop.
        m.hell = Some(0.1);
        let open = m.next_target().expect("something to explore");
        assert_eq!(open.target, "toward", "must head at the corruption, not at the nearest node");
        // Steering that worked says so, and says how much of the frontier it could measure.
        let (toward, placed, total) = open.steered_by.expect("this hop was steered");
        assert_eq!(toward, "start");
        assert!(placed > 0 && placed <= total, "{placed} of {total}");
    }

    #[test]
    fn a_bearing_nothing_can_place_does_not_count_as_steering() {
        // The regression that let a run wander out of the corruption while the code believed it was
        // steering. Everything is present except a *position* to aim at: the portal is open, known
        // and unroutable, so a bearing is produced — but no corrupted node has ever been placed, so
        // `pos_for` has no centroid to average and `gap` returns `None` for every candidate. The
        // ordering key is then equal throughout and the frontier sorts by hops, which is precisely
        // what a run with no steering written would do.
        //
        // The old test was `bearing.is_some()`, which is true here, so this state reported steering
        // and did none. What separates the two is whether anything could be *measured*.
        let mut m = WorldMap::new();
        ready_for_the_anomaly(&mut m);
        m.fold(&dump(
            "here",
            "camp",
            vec![node_at("away", "Westerly meadow", -100.0, 0.0), node_at("step", "Easterly road", 50.0, 0.0)],
        ));
        m.fold(&dump(
            "step",
            "Easterly road",
            vec![
                node_at("away", "Westerly meadow", -100.0, 0.0),
                node_at("toward", "Furtherly meadow", 100.0, 0.0),
            ],
        ));
        m.entry("away").connections = 3;
        m.entry("toward").connections = 3;

        // Corrupted, exactly as a save's area flags leave it: flagged, never seen, never placed.
        m.entry("far").corrupted = true;
        m.fold(&dump("island", "island camp", vec![node_at("start", "The Rift anomaly", 0.0, 0.0)]));
        // **And unplaced.** Stated rather than assumed: the first draft of this test left it to the
        // island dump failing to register, and it turned out to be placed anyway -- so the test
        // measured a map with a perfectly good bearing and proved nothing. The state under test is
        // "known, unroutable, nowhere", so it is written down.
        m.entry("start").pos = None;
        m.here = Some("here".into());
        m.hell = Some(0.1);

        let plan = m.next_target().expect("something to explore");
        // The pair that only this split can express: we know the errand, and we cannot aim at it.
        assert_eq!(plan.reason, Goal::RouteTo(Box::new(Goal::CloseAnomaly)), "the errand is known");
        assert_eq!(plan.steered_by, None, "a bearing that cannot order the frontier is not steering");
        assert_eq!(plan.target, "away", "so it falls back to nearest unvisited, and says so");

        // The control: place that same corrupted node and the identical map now steers. Only
        // `pos` changes, which is what pins the cause.
        m.entry("far").pos = Some((200.0, 0.0));
        let steered = m.next_target().expect("something to explore");
        assert!(steered.steered_by.is_some(), "a placed corruption gives it something to aim at");
        assert_eq!(steered.target, "toward");
    }

    /// A node the current dump never mentions can still be clicked, because the frame places it.
    ///
    /// The dev's correction, 2026-08-16, and the whole of what #21 was missing. A dump prints
    /// adjacent connections only, which I read as "a distant node cannot be aimed at". It carries
    /// nodes we have already placed, and one of those fixes the camera exactly — measured over 80
    /// settled dumps from that evening's run, every shared node agreed on the shift to 0.0000 px.
    #[test]
    fn a_node_missing_from_the_dump_is_placed_from_the_frame() {
        let mut m = WorldMap::new();
        m.fold(&dump("here", "Somewhere", vec![
            Node { key: "a".into(), heading: "A".into(), x: 100.0, y: 100.0, connections: 2 },
            Node { key: "b".into(), heading: "B".into(), x: 200.0, y: 100.0, connections: 2 },
        ]));
        m.fold(&dump("a", "A", vec![
            Node { key: "b".into(), heading: "B".into(), x: 250.0, y: 70.0, connections: 2 },
            Node { key: "c".into(), heading: "C".into(), x: 350.0, y: 70.0, connections: 2 },
        ]));

        // Standing at `b`, panned again. `a` is not in this dump; `b` and `c` are, and two anchors
        // is the minimum a fit needs — one cannot see the zoom, which is what ended a live run.
        let now = dump("here2", "elsewhere", vec![
            Node { key: "b".into(), heading: "B".into(), x: -100.0, y: 0.0, connections: 2 },
            Node { key: "c".into(), heading: "C".into(), x: 0.0, y: 0.0, connections: 2 },
        ]);
        assert!(!now.nodes.iter().any(|n| n.key == "a"), "the fixture must not name `a`");
        // `c` is world (300,100) drawn at (0,0) and `b` is world (200,100) drawn at (-100,0): same
        // scale, so the shift is (300,100) and `a` — world (100,100) — is drawn 200 left of `c`.
        assert_eq!(m.screen_position(&now, "c"), Some((0.0, 0.0)), "the anchor draws where it says");
        assert_eq!(m.screen_position(&now, "a"), Some((-200.0, 0.0)), "and the rest move with it");

        // **One anchor is not enough to aim with.** The scale would have to be assumed, and assuming
        // it is what aimed a click at (463, 841) instead of (672, 713) on 2026-08-16.
        let thin = dump("here3", "elsewhere", vec![
            Node { key: "c".into(), heading: "C".into(), x: 0.0, y: 0.0, connections: 2 },
        ]);
        assert_eq!(m.screen_position(&thin, "a"), None, "a fit that cannot be checked is not offered");

        // And a dump at half the zoom is followed rather than fought: `b` and `c` are 100 apart in
        // the frame and 50 apart here, so the scale is 2 and `a` sits 100 left of `c` on screen.
        let zoomed = dump("here4", "elsewhere", vec![
            Node { key: "b".into(), heading: "B".into(), x: -50.0, y: 0.0, connections: 2 },
            Node { key: "c".into(), heading: "C".into(), x: 0.0, y: 0.0, connections: 2 },
        ]);
        assert_eq!(m.screen_position(&zoomed, "a"), Some((-100.0, 0.0)), "scale, not just offset");

        // A node the frame has never placed cannot be invented.
        assert_eq!(m.screen_position(&now, "never-seen"), None);
    }

    /// **The rooms of a village, not only its doors** — #57, and the interior half of the test above.
    ///
    /// Enthorpe on 2026-08-21: four separate presses to reach an inn three nodes inside a village
    /// whose interior was complete and uncorrupted. [`WorldMap::far_hop_inside`] named the inn
    /// correctly every time and the driver threw the answer away, because a dump prints positions
    /// for **adjacent connections** and for **subworld exits** (`overworldview.lua:1030-1047`) and
    /// an inn two rooms in is neither. The doors were already reachable in one press; the rooms
    /// were not.
    ///
    /// So both halves are asserted here, because the bug lived precisely in the join: the hop was
    /// computed and there was nowhere to click.
    #[test]
    fn a_far_hop_inside_a_village_can_aim_at_a_room_the_dump_does_not_name() {
        let mut m = enthorpe();

        // Back at `l32sub1` with the camera 50 left of where the frame was defined. The inn is two
        // hops on and this dump does not mention it.
        let now = inside_dump(
            "l32",
            "l32sub1",
            "Enthorpe house",
            vec![
                Node { key: "l32_path_to_l7".into(), heading: "Somewhere l7 crossroads".into(), x: 450.0, y: 500.0, connections: 2 },
                Node { key: "l32sub2".into(), heading: "Enthorpe house".into(), x: 650.0, y: 500.0, connections: 2 },
            ],
            vec![
                Exit { x: 450.0, y: 500.0, to_key: "l7".into(), to_heading: "Somewhere l7 crossroads".into() },
                Exit { x: 850.0, y: 500.0, to_key: "l1".into(), to_heading: "Somewhere l1 crossroads".into() },
            ],
        );
        m.fold(&now);
        assert!(!now.nodes.iter().any(|n| n.key == "l32sub3"), "the fixture must not name the inn");

        // **Half one: the hop is legal and worth taking.** Two rooms on, so not an ordinary step.
        assert_eq!(
            m.far_hop_inside("l32sub1", "l32sub3").as_deref(),
            Some("l32sub3"),
            "the whole interior is clear, so the game would walk us there in one press"
        );

        // **Half two: it is now somewhere we can click.** The frame has the inn at world 800 and
        // this dump is panned 50 left of the frame, so it is drawn at 750.
        assert_eq!(m.inside_screen_position(&now, "l32sub3"), Some((750.0, 500.0)));
        // The anchors themselves draw where the dump says they do, which is the fit's own control.
        assert_eq!(m.inside_screen_position(&now, "l32sub2"), Some((650.0, 500.0)));
        assert_eq!(m.inside_screen_position(&now, "l32_path_to_l1"), Some((850.0, 500.0)));

        // A room nobody has ever been next to cannot be invented, however good the fit is.
        assert_eq!(m.inside_screen_position(&now, "l32sub9"), None);

        // **One anchor is not enough to aim with**, exactly as on the surface: the scale would have
        // to be assumed, and assuming it is what aimed a click at (463, 841) on 2026-08-16.
        let thin = inside_dump(
            "l32",
            "l32sub1",
            "Enthorpe house",
            Vec::new(),
            vec![Exit { x: 450.0, y: 500.0, to_key: "l7".into(), to_heading: "h".into() }],
        );
        assert_eq!(m.inside_screen_position(&thin, "l32sub3"), None);
    }

    /// **The interior frame lasts one visit, and leaving is what ends it.**
    ///
    /// `lostOrientation` re-rolls every interior coordinate (`forest.lua:483-490`) and
    /// `overworldview.lua:1613` re-runs it from `loadLight`, which is why
    /// [`crate::subworld::Rules::positions_survive_reentry`] is false everywhere. A village does not
    /// carry the flag, and the frame is still thrown away — being right about which generators
    /// re-roll is not a bet worth taking for the sake of a press.
    ///
    /// Leaving prints a surface dump, and a surface dump has no container, which is the test.
    #[test]
    fn the_interior_frame_does_not_survive_leaving_the_village() {
        let mut m = enthorpe();
        let inside = inside_dump(
            "l32",
            "l32sub1",
            "Enthorpe house",
            Vec::new(),
            vec![
                Exit { x: 450.0, y: 500.0, to_key: "l7".into(), to_heading: "h".into() },
                Exit { x: 850.0, y: 500.0, to_key: "l1".into(), to_heading: "h".into() },
            ],
        );
        // The positive control: while we are still inside, that dump does place the inn.
        assert_eq!(m.inside_screen_position(&inside, "l32sub3"), Some((750.0, 500.0)));

        // Out onto the surface, then straight back in by the same door.
        m.fold(&dump("l7", "Somewhere l7 crossroads", vec![node("l32", "Enthorpe village")]));
        m.fold(&inside_dump(
            "l32",
            "l32_path_to_l7",
            "road",
            vec![node("l32sub1", "Enthorpe house")],
            vec![
                Exit { x: 500.0, y: 500.0, to_key: "l7".into(), to_heading: "h".into() },
                Exit { x: 900.0, y: 500.0, to_key: "l1".into(), to_heading: "h".into() },
            ],
        ));
        assert_eq!(
            m.inside_screen_position(&inside, "l32sub3"),
            None,
            "the visit ended, so where the inn was drawn last time is not evidence"
        );
        // And the edges are untouched, which is the property the whole map rests on —
        // `lostOrientation` moves positions and nothing else.
        assert_eq!(m.far_hop_inside("l32sub1", "l32sub3").as_deref(), Some("l32sub3"));
    }

    /// **A re-rolled interior disagrees with itself, and disagreement refuses the fit.**
    ///
    /// The second guard, independent of the first. `lostOrientation` applies one of the square's
    /// eight orientations — `loc.posX, loc.posY = loc.posX*x, loc.posY*y` and an optional transpose
    /// (`forest.lua:483-490`) — and a reflection is not a similarity, so no `world = drawn*scale +
    /// offset` can satisfy two anchors that have been through one. [`Frame::disagreement`] measures
    /// exactly that, and [`Frame::is_sound`] is what stops it being written into the frame.
    ///
    /// This is the difference between the interior frame and the surface one that ended the run of
    /// 2026-08-16 1752Z: there, a bad fit placed nodes and every later fit inherited the damage.
    #[test]
    fn a_re_rolled_interior_is_refused_rather_than_aimed_at() {
        let mut m = enthorpe();
        // The same two doors, transposed: 400 apart in y where the frame has them 400 apart in x.
        // The scale still measures as 1 and the offset still fits the first anchor exactly — which
        // is why two anchors are needed to see it at all.
        let rerolled = inside_dump(
            "l32",
            "l32sub1",
            "Enthorpe house",
            vec![Node { key: "l32sub4".into(), heading: "Enthorpe house".into(), x: 500.0, y: 700.0, connections: 2 }],
            vec![
                Exit { x: 500.0, y: 500.0, to_key: "l7".into(), to_heading: "h".into() },
                Exit { x: 500.0, y: 900.0, to_key: "l1".into(), to_heading: "h".into() },
            ],
        );
        assert_eq!(
            m.inside_screen_position(&rerolled, "l32sub3"),
            None,
            "a fit that cannot place its own anchors may not place anything else"
        );
        m.fold(&rerolled);
        assert!(
            m.inside_screen_position(&rerolled, "l32sub4").is_none(),
            "and nothing from a refused dump is written into the frame"
        );

        // **The positive control.** Untransposed — the doors 400 apart in x, as the frame has them —
        // the very same dump does place its new room, so the refusal above is the reflection and not
        // the fixture.
        let mut ok = enthorpe();
        let straight = inside_dump(
            "l32",
            "l32sub1",
            "Enthorpe house",
            vec![Node { key: "l32sub4".into(), heading: "Enthorpe house".into(), x: 700.0, y: 500.0, connections: 2 }],
            vec![
                Exit { x: 500.0, y: 500.0, to_key: "l7".into(), to_heading: "h".into() },
                Exit { x: 900.0, y: 500.0, to_key: "l1".into(), to_heading: "h".into() },
            ],
        );
        ok.fold(&straight);
        assert_eq!(ok.inside_screen_position(&straight, "l32sub4"), Some((700.0, 500.0)));
        assert_eq!(ok.inside_screen_position(&straight, "l32sub3"), Some((800.0, 500.0)));
    }

    /// The scale is remembered, not assumed — which is what ended the run of 2026-08-16 1752Z.
    ///
    /// The sequence is the run's, at a tenth of the size. A frame is built at one zoom; the map is
    /// zoomed out; a dump with two anchors measures the new scale correctly; and then a dump with
    /// **one** anchor arrives, which is the ordinary way a new node is learned — you walk somewhere
    /// new and the only node in the dump you have already placed is the one you came from.
    ///
    /// The old rule assumed `scale = 1.0` there, i.e. that the frame was still at screen scale, and
    /// wrote the new node into the frame at half size. From then on the frame was two frames.
    #[test]
    fn a_one_anchor_dump_inherits_the_measured_scale_instead_of_assuming_one() {
        let mut m = WorldMap::new();
        // The frame, defined by its first dump: `a` at (100,100) and `b` at (200,100).
        m.fold(&dump("here", "Somewhere", vec![
            node_at("a", "A", 100.0, 100.0),
            node_at("b", "B", 200.0, 100.0),
        ]));

        // Zoomed out a step, so everything draws at half size. Two anchors, so the scale is there to
        // be measured: `a` and `b` are 100 apart in the frame and 50 apart here.
        m.zoom_changed();
        m.fold(&dump("n1", "Halfway", vec![
            node_at("a", "A", 50.0, 50.0),
            node_at("b", "B", 100.0, 50.0),
            node_at("d", "D", 150.0, 50.0),
        ]));
        assert_eq!(m.get("d").unwrap().pos, Some((300.0, 100.0)), "measured, so `d` is in frame units");

        // **The step that used to poison everything.** One anchor, still at the zoomed-out scale.
        // Assuming 1.0 here would put `e` at (350, 100) — half a node's spacing out, and permanent.
        m.fold(&dump("n2", "Further", vec![
            node_at("d", "D", 150.0, 50.0),
            node_at("e", "E", 200.0, 50.0),
        ]));
        assert_eq!(m.get("e").unwrap().pos, Some((400.0, 100.0)), "the remembered scale still applies");

        // And the frame is still one frame: a later dump mixing an old node with a new one agrees
        // with both. Under the old rule this is where the two populations met and averaged.
        let mixed = dump("n3", "Elsewhere", vec![
            node_at("b", "B", 100.0, 50.0),
            node_at("e", "E", 200.0, 50.0),
            node_at("d", "D", 150.0, 50.0),
        ]);
        assert_eq!(m.frame_disagreement(&mixed), None, "no two anchors contradict each other");
        assert_eq!(m.screen_position(&mixed, "a"), Some((50.0, 50.0)), "so aiming still lands");
    }

    /// After a zoom, nothing is placed until some dump has measured the new scale.
    ///
    /// The remembered scale is right until the moment we change the zoom, and then it is exactly as
    /// wrong as the assumption it replaced. We are the only thing that changes the zoom — `setZoom`
    /// is reached from `core:wheelmoved` and the options screen, and `zoomMult` is otherwise a module
    /// local that entering a subworld does not touch — so saying so costs one call.
    #[test]
    fn a_zoom_suspends_placing_until_a_dump_can_measure_it() {
        let mut m = WorldMap::new();
        m.fold(&dump("here", "Somewhere", vec![
            node_at("a", "A", 100.0, 100.0),
            node_at("b", "B", 200.0, 100.0),
        ]));
        m.zoom_changed();

        // One anchor and an unknown scale: this dump can say nothing about where `d` is.
        m.fold(&dump("n1", "Halfway", vec![node_at("a", "A", 50.0, 50.0), node_at("d", "D", 150.0, 50.0)]));
        assert_eq!(m.get("d").unwrap().pos, None, "a guess here is a guess for the rest of the run");
        assert!(m.get("d").is_some(), "the node is still known — only its position is withheld");

        // Two anchors measure it, and `d` is placed the moment a dump can say where it is.
        m.fold(&dump("n2", "Halfway", vec![
            node_at("a", "A", 50.0, 50.0),
            node_at("b", "B", 100.0, 50.0),
            node_at("d", "D", 150.0, 50.0),
        ]));
        assert_eq!(m.get("d").unwrap().pos, Some((300.0, 100.0)));
    }

    /// A fit whose anchors contradict each other places nothing and aims nothing.
    ///
    /// The frame's own positive control. Three anchors are the fewest that can show it — see
    /// [`Frame::disagreement`] — and the wrong one here is off by exactly the amount the old
    /// placement rule would have introduced.
    #[test]
    fn anchors_that_disagree_stop_the_frame_being_used_at_all() {
        let mut m = WorldMap::new();
        m.fold(&dump("here", "Somewhere", vec![
            node_at("a", "A", 100.0, 100.0),
            node_at("b", "B", 200.0, 100.0),
        ]));
        // `c` planted at a position no dump could have produced, standing in for a node placed at
        // the wrong scale before this rule existed — or read out of a cache written by such a run.
        m.entry("c").pos = Some((350.0, 100.0));

        let bad = dump("n1", "Halfway", vec![
            node_at("a", "A", 50.0, 50.0),
            node_at("b", "B", 100.0, 50.0),
            node_at("c", "C", 150.0, 50.0),
            node_at("new", "New", 200.0, 50.0),
        ]);
        let px = m.frame_disagreement(&bad).expect("the anchors cannot all be right");
        assert!(px > FRAME_TOLERANCE, "{px} px of disagreement is a broken frame, not noise");
        m.fold(&bad);
        assert_eq!(m.get("new").unwrap().pos, None, "placing from a broken frame spreads it");
        assert_eq!(m.screen_position(&bad, "a"), None, "and aiming from one clicks empty ground");
    }

    /// The middle case, and the one every decline in the archive turned out to be.
    ///
    /// `apply_save` gives a resumed run places it has never seen drawn. They are real destinations
    /// — a save `_path_to_` completion authorises a hop straight to one — and there is nothing on
    /// screen to aim at, so the step is right and the frame is blameless.
    #[test]
    fn a_place_the_save_named_but_no_dump_ever_drew_says_so() {
        let mut m = WorldMap::new();
        let at = |k: &str, x: f64, y: f64| Node {
            key: k.into(), heading: "road".into(), x, y, connections: 2
        };
        // Two dumps, so `here` and its neighbours are placed and the frame can measure a scale.
        m.fold(&dump("l1", "road", vec![at("l2", 400.0, 400.0), at("l3", 800.0, 600.0)]));
        m.fold(&dump("l2", "road", vec![at("l1", 100.0, 100.0), at("l3", 800.0, 600.0)]));
        let fresh = dump("l2", "road", vec![at("l1", 100.0, 100.0), at("l3", 800.0, 600.0)]);

        // What the save does: a Place, with nothing else.
        m.entry("l26").completed = true;
        assert_eq!(m.screen_position(&fresh, "l26"), None, "there is nothing to aim at");

        let why = m.unplaceable(&fresh, "l26");
        assert!(why.contains("no dump has ever drawn"), "{why}");
        assert!(why.contains("l26"), "{why}");
        // The negative control that makes the diagnosis mean anything: the SAME dump places a node
        // it has drawn, so the frame was never the reason.
        assert!(
            m.screen_position(&fresh, "l3").is_some(),
            "the frame is usable here, which is exactly why blaming it was wrong"
        );
    }

    #[test]
    fn a_key_we_have_never_heard_of_is_not_reported_as_a_frame_fault() {
        let m = WorldMap::new();
        let why = m.unplaceable(&dump("l1", "road", vec![]), "l99");
        assert!(why.contains("not on our map at all"), "{why}");
    }

    /// The genuine frame case: a dump with nothing in common with what we have placed.
    #[test]
    fn a_dump_sharing_nothing_placed_blames_the_frame_and_says_why() {
        let mut m = WorldMap::new();
        let at = |k: &str, x: f64, y: f64| Node {
            key: k.into(), heading: "road".into(), x, y, connections: 2
        };
        m.fold(&dump("l1", "road", vec![at("l2", 400.0, 400.0), at("l3", 800.0, 600.0)]));
        // A dump from somewhere else entirely: nothing in it has a position in our frame.
        let stranger = dump("l77", "road", vec![at("l78", 10.0, 10.0)]);
        let why = m.unplaceable(&stranger, "l3");
        assert!(
            why.contains("frame cannot speak") || why.contains("needs two"),
            "a real frame failure should still say so: {why}"
        );
    }

    /// **The measurement behind [`WorldMap::unplaceable`]**, replayed from the run archive rather
    /// than asserted from memory.
    ///
    /// #63 recorded that `the frame cannot place it` means "too few anchors to register the node"
    /// and pointed the fix at #21, more anchors. Replaying the dumps of the runs it cites says
    /// otherwise: in the two 2026-08-21 runs the frame was usable at **every** surface dump, and
    /// the only places that end a run without a position are interior ones, which live in the
    /// subworld frame and are not the world frame's to hold.
    ///
    /// So a surface node, once drawn, is always placeable — and a surface node that is *never*
    /// drawn is the one the far hop declines. That is [`WorldMap::apply_save`]'s doing, not the
    /// frame's.
    ///
    /// Reads the archive because it is a claim about real dumps; skips when it is not there, the
    /// same way [`crate::parity`] does.
    #[test]
    fn the_world_frame_is_not_what_declines_a_far_hop() {
        // 0436Z is included and deliberately not asserted on: its 9 mute dumps are the opening of
        // a run, before anything has been placed, and folding that into the claim would make the
        // claim vaguer rather than stronger.
        for (path, mute_allowed) in [
            ("spike-run-20260821-0313Z.log", 0),
            ("spike-run-20260821-0357Z.log", 0),
        ] {
            let Ok(log) = std::fs::read_to_string(path) else {
                eprintln!("SKIP: {path} is not present");
                continue;
            };
            let lines: Vec<String> = log.lines().map(|l| l.to_string()).collect();
            let dumps = crate::observe::adjacency::Reader::new().push(&lines);
            assert!(dumps.len() > 100, "{path} should hold a whole run, got {}", dumps.len());

            let mut m = WorldMap::new();
            let (mut surface, mut mute) = (0, 0);
            for a in &dumps {
                m.fold(a);
                if a.subworld.is_some() {
                    continue;
                }
                surface += 1;
                if m.registration(a).filter(|f| f.anchors >= 2 && f.is_sound()).is_none() {
                    mute += 1;
                }
            }
            assert!(surface > 50, "{path}: only {surface} surface dumps");
            assert_eq!(
                mute, mute_allowed,
                "{path}: {mute} of {surface} surface dumps could not place anything"
            );

            // Interior keys: subnodes, plazas, the roads out, and the crossroads the game names by
            // coordinate. None of them is the world frame's to hold — see [`InsideFrame`].
            let interior = |k: &str| {
                k.contains("sub") || k.contains("_plaza") || k.contains("_path_to_") || k.contains("xrd")
            };
            let stranded: Vec<&str> = m
                .places
                .values()
                .filter(|p| p.pos.is_none() && !interior(&p.key))
                .map(|p| p.key.as_str())
                .collect();
            assert!(
                stranded.is_empty(),
                "{path}: surface nodes left unplaced by the world frame: {stranded:?}"
            );
        }
    }
}
