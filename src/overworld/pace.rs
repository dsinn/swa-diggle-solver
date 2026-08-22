//! How long a walk should be given before we call it a hang.
//!
//! The dev, 2026-08-22: *instead of a hardcoded timeout for every travel, set it according to the
//! world unit distance travelled and whether or not we have the turbo snail passive.*
//!
//! # The two walks the game actually has
//!
//! Which one runs is decided by a single gear flag (`overworldview.lua:1183`,
//! `snapMove = playerHasGearFlag'mapLerpMove'`), and they scale with distance in completely
//! different ways:
//!
//! - **On foot**, the avatar accelerates toward a hard cap of 120 world units per second
//!   (`:1201`, `maxX = dirVX*delta*120`), brakes at any corner sharper than 0.3 rad (`:1202-1207`),
//!   and arrives within 45 units (`arriveDistSq` default 2025). Time is *linear* in the distance
//!   walked.
//! - **On the Magic turbo-snail**, it is an exponential ease instead: `easeDecay(pos, target, 7.5,
//!   dt)` = `target + (pos-target)*exp(-7.5*dt)` (`:1188`, `utils/maths.lua:74`). The gap decays by
//!   `e^-7.5` per second whatever it started at, so arriving takes `ln(d/45)/7.5` — *logarithmic* in
//!   the distance, and under half a second even for the longest edge on the map. The snail is a joke
//!   about its appearance; on a long hop it is faster by an order of magnitude.
//!
//! The passive is `turboSnail` (`items/overworldgear.lua:52-68`), whose flag list is exactly
//! `{'mapLerpMove'}`, and flags are hashed from `playerData.passives` and `playerData.gear` together
//! (`overworld.lua:105-109`) — so the save's `passives` list is where we read it.
//!
//! # Two frames, two models, both measured
//!
//! 120 units per second is in *world* coordinates and we do not have those. A dump prints
//! `xoffset + location.posX*zoomMult` (`overworldview.lua:1033`), which [`Frame`] registers into the
//! map's own frame — consistent within a run, but tied to the game's units by whatever zoom seeded
//! it. So both models below are fitted from the run reports, in the frames we actually hold.
//!
//! **On the surface**, pairing 147 hops from the 2026-08-20..22 reports against the routed distance
//! between their endpoints gives `time = 1.08 s + path/157` (r = 0.62, residual sd 1.47 s). Note
//! *routed*: straight-line distance does not fit at all — `l17 -> l4` is 175 units apart and took
//! 11.4 s, because a Travel press walks every node on the path (`overworldview.lua:1210-1216`).
//! Pricing the route instead brought that hop from 7.8x its prediction to inside the residual.
//!
//! **Inside a subworld the same fit collapses** — 576 paired interior steps give r = 0.18, and the
//! extremes say it plainly: a 2324-unit step took 0.9 s while a 186-unit one took 2.7 s. The
//! interior frame carries its own scale (see [`InsideFrame`]), fitted per container against a view
//! zoomed to that container, so its units are not the surface's and its distances are inflated. What
//! those 576 samples *do* bound is the leg: the slowest is 2.7 s. So an interior leg is priced flat
//! and only the count of them matters.
//!
//! [`Frame`]: super::Frame
//! [`InsideFrame`]: super::InsideFrame
//!
//! # The residual is events, so the deadline moves
//!
//! Fitting leaves a one-sided tail — surface residuals reach +6.3 s — and its cause is not walking. A
//! lore screen or a merchant on the way pauses the walk for as long as it takes us to answer, which
//! is unbounded: no envelope covers it without being uselessly large. So the caller pushes its
//! deadline out whenever the wait actually handles something, and the budget here only has to cover
//! *walking*. That is what makes a timeout mean "nothing is happening" rather than "this is taking a
//! while" — which was the dev's question about the fixed one this replaces.

use std::time::Duration;

/// Map-frame units per second on the surface, fitted from 147 paired hops. See the module docs.
///
/// The game's own figure is 120 *world* units (`overworldview.lua:1201`); the difference is the zoom
/// that seeded the frame. **If the surface frame is ever seeded from a zoomed-out dump this constant
/// moves with it** — the thing to check first if budgets start coming out wrong, and something #29
/// (zoom out instead of dragging) would have to re-derive.
pub const WALK_SPEED: f64 = 157.0;

/// What one leg inside a subworld costs, before [`SAFETY`]. The slowest of 576 measured interior
/// steps is 2.7 s and distance does not predict them, so this is a flat per-leg price.
pub const INSIDE_LEG: f64 = 3.0;

/// How close counts as arrived: `arriveDistSq` is 2025 by default (`overworldview.lua:1185`).
pub const ARRIVE_RADIUS: f64 = 45.0;

/// The turbo-snail's decay factor, from `easeDecay(.., .., 7.5, dt)` (`overworldview.lua:1188`).
pub const SNAIL_DECAY: f64 = 7.5;

/// The part of a hop that is not walking: our click, the camera settling, the arrival dump, the
/// 300 ms poll granularity. The surface fit puts it at 1.08 s; this is that with room, and it is
/// what a zero-distance hop gets.
pub const OVERHEAD: Duration = Duration::from_secs(5);

/// Multiplier on the walking term. 2.0 is the smallest that covers all 147 fitted surface hops when
/// paired with [`OVERHEAD`] — 1.5 needs a 6 s floor and 2.5 a 3 s one, and the flatter combination
/// leaves more slack on the short hops that make up most of a run.
pub const SAFETY: f64 = 2.0;

/// A backstop for a route we have priced badly, and what an unpriceable one falls back to. Equal to
/// the fixed wait this replaced, so nothing can now wait *longer* than it used to.
pub const CEILING: Duration = Duration::from_secs(60);

/// Where the walk happens, which decides how its legs are priced. See the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ground {
    /// The overworld map, whose frame is shared and whose legs are priced by distance.
    Surface,
    /// Inside a subworld, whose frame has its own scale and whose legs are priced flat.
    Inside,
}

/// How long to allow for a walk along `legs`, each a distance in the map frame for that `ground`.
///
/// One entry per node the path passes through: the brake at each corner and the ramp out of it are
/// per-leg costs, the snail's cost is per-leg by nature, and inside a subworld the leg *is* the
/// unit. An empty slice is a walk we could not price, and gets the [`CEILING`] the fixed wait used
/// to give everything.
pub fn walk_budget(legs: &[f64], snail: bool, ground: Ground) -> Duration {
    if legs.is_empty() {
        return CEILING;
    }
    let seconds: f64 = legs
        .iter()
        .map(|&d| match (ground, snail) {
            (Ground::Inside, _) => INSIDE_LEG,
            // `exp(-7.5t)` shrinks the gap to `ARRIVE_RADIUS`; below that we are already there.
            (Ground::Surface, true) => (d.max(ARRIVE_RADIUS) / ARRIVE_RADIUS).ln() / SNAIL_DECAY,
            (Ground::Surface, false) => d.max(0.0) / WALK_SPEED,
        })
        .sum();
    (OVERHEAD + Duration::from_secs_f64(seconds * SAFETY)).min(CEILING)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overworld::WorldMap;

    /// A placed node and its edges, set directly.
    ///
    /// **Not built from dumps, deliberately.** A dump prints its *neighbours'* positions and never
    /// the player's own (`overworldview.lua:1031`), so the node a walk starts from is placed only by
    /// some earlier dump that happened to list it. Reproducing that here would make every test below
    /// a test of frame registration, which `frame.rs` already owns. In a real run it resolves on its
    /// own: 147 of the 150 surface hops in the 2026-08-20..22 reports had both endpoints placed, and
    /// the three that did not fall back to the fixed budget.
    fn place(m: &mut WorldMap, key: &str, at: (f64, f64), neighbours: &[&str]) {
        m.entry(key).pos = Some(at);
        for n in neighbours {
            m.entry(key).neighbours.insert((*n).into());
            m.entry(n).neighbours.insert(key.into());
        }
    }

    /// **The route, not the crow's line** — the thing the surface fit turned on.
    ///
    /// `a` and `c` are 100 apart in a straight line, but the only way between them goes out to `b`
    /// and back, which is 600 of walking. Pricing the straight line would budget a sixth of what the
    /// walk needs, and that is exactly how `l17 -> l4` came out at 7.8x its prediction.
    #[test]
    fn a_route_is_priced_along_the_ground_it_covers_and_not_end_to_end() {
        let mut m = WorldMap::default();
        place(&mut m, "a", (0.0, 0.0), &["b"]);
        place(&mut m, "b", (0.0, 300.0), &["c"]);
        place(&mut m, "c", (100.0, 0.0), &[]);
        let legs = m.walk_legs("a", "c").expect("both ends are placed");
        assert_eq!(legs.len(), 2, "the route goes through `b`");
        let walked: f64 = legs.iter().sum();
        assert!(walked > 600.0, "walked {walked}, but a-c is only 100 apart");
        assert!(
            walk_budget(&legs, false, Ground::Surface)
                > walk_budget(&[100.0], false, Ground::Surface)
        );
    }

    /// A node we have never been adjacent to has no position, and that is an ordinary state rather
    /// than a fault — the caller falls back to the fixed budget, so it can only wait *longer*.
    #[test]
    fn a_route_through_an_unplaced_node_cannot_be_priced() {
        let mut m = WorldMap::default();
        place(&mut m, "a", (0.0, 0.0), &["b"]);
        place(&mut m, "b", (0.0, 300.0), &["c"]);
        // `c` is a known neighbour of `b` that has never been printed with a position of its own,
        // which is the ordinary state of anything a hop beyond where we have stood.
        assert_eq!(m.walk_legs("a", "c"), None);
        assert_eq!(walk_budget(&m.walk_legs("a", "c").unwrap_or_default(), false, Ground::Surface), CEILING);
    }

    /// Standing on it already: no legs, no walk, and the wait is pure overhead.
    #[test]
    fn going_nowhere_costs_only_the_overhead() {
        let mut m = WorldMap::default();
        place(&mut m, "a", (0.0, 0.0), &["b"]);
        assert_eq!(m.walk_legs("a", "a"), Some(Vec::new()));
    }


    /// The fitted model, at both ends of the range it was fitted over.
    ///
    /// A short hop is overhead-dominated and a long one is not, which is the point of pricing by
    /// distance at all: 5 s covers the first and would have to be 23 s to cover the second.
    #[test]
    fn a_surface_walk_is_priced_by_how_far_it_actually_goes() {
        let near = walk_budget(&[60.0], false, Ground::Surface);
        let far = walk_budget(&[1400.0], false, Ground::Surface);
        assert_eq!(near, OVERHEAD + Duration::from_secs_f64(60.0 / WALK_SPEED * SAFETY));
        assert!(far > Duration::from_secs(22), "a long hop needs more than the floor: {far:?}");
        assert!(far < CEILING);
    }

    /// **The slowest arrival on record fits inside it**, which is the test that would catch a budget
    /// tuned by taste. `l17 -> l4` on 2026-08-22 was a four-leg route that took 11.4 s.
    #[test]
    fn the_longest_arrival_we_have_ever_seen_fits_inside_the_budget() {
        let legs = [175.0, 420.0, 380.0, 395.0];
        assert!(
            walk_budget(&legs, false, Ground::Surface) > Duration::from_secs_f64(11.4),
            "the budget has to cover the slowest arrival we have measured"
        );
    }

    /// The snail's whole character: distance stops mattering.
    ///
    /// Twenty times the distance costs `ln(20)/7.5` = 0.4 s more, not twenty times as long — so a hop
    /// that is 18 s on foot is about a second mounted, and the budget falls back to its overhead.
    #[test]
    fn the_turbo_snail_makes_a_long_hop_cost_barely_more_than_a_short_one() {
        let near = walk_budget(&[60.0], true, Ground::Surface);
        let far = walk_budget(&[1400.0], true, Ground::Surface);
        assert!(far - near < Duration::from_secs(2), "snail cost is logarithmic: {near:?} {far:?}");
        assert!(
            far < walk_budget(&[1400.0], false, Ground::Surface),
            "and it has to beat walking the same ground"
        );
    }

    /// **Inside, distance is not the unit and must not be treated as one.** The two legs here are
    /// the real extremes of the 576 measured interior steps: 2324 units took 0.9 s and 186 units
    /// took 2.7 s. Pricing them by distance would have got both backwards.
    #[test]
    fn an_interior_leg_costs_the_same_whether_it_is_long_or_short() {
        let long = walk_budget(&[2324.0], false, Ground::Inside);
        let short = walk_budget(&[186.0], false, Ground::Inside);
        assert_eq!(long, short);
        assert_eq!(long, OVERHEAD + Duration::from_secs_f64(INSIDE_LEG * SAFETY));
        assert!(
            long > Duration::from_secs_f64(2.7),
            "and it still has to cover the slowest interior step measured"
        );
        // A three-leg far hop inside is the case that stalled the 0649Z run.
        assert!(walk_budget(&[300.0; 3], false, Ground::Inside) < CEILING);
    }

    /// A node closer than the arrival radius is already arrived at, so it cannot cost negative time.
    #[test]
    fn a_leg_shorter_than_the_arrival_radius_costs_nothing_extra() {
        assert_eq!(walk_budget(&[10.0], true, Ground::Surface), OVERHEAD);
    }

    /// **A walk we cannot price waits exactly as long as it used to.** Positions are per-run and a
    /// node we have never been adjacent to has none, so this is a normal path and not an error — and
    /// falling back to the old fixed budget means the change can only ever shorten a wait.
    #[test]
    fn a_route_we_cannot_price_falls_back_to_the_fixed_wait() {
        assert_eq!(walk_budget(&[], false, Ground::Surface), CEILING);
        assert_eq!(walk_budget(&[], true, Ground::Inside), CEILING);
        assert_eq!(walk_budget(&[100_000.0], false, Ground::Surface), CEILING);
    }
}
