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
