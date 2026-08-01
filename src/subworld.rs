//! Subworlds — forests, mausoleums, camps — and the two rules that make them not the overworld.
//!
//! A subworld is entered with `enterSubworld` and drawn by the same `overworldview`, which is why it
//! is tempting to treat it as more overworld. It is not, and two generator flags are the reason.
//! Both belong here rather than in [`crate::overworld`], because a map that quietly assumed surface
//! behaviour would be wrong in exactly the places that matter.
//!
//! ## `thickFog` — knowledge does not accumulate
//!
//! `isCloudCovered` (`overworldview.lua:696-699`):
//!
//! ```lua
//! if thickFog then
//!     return not core.playerIsAt(locationKey)
//!     and not locationData[locationKey].connections[overworldData.playerLocation]
//! end
//! ```
//!
//! Visibility collapses to "am I standing there, or is it adjacent right now". The persistent
//! `_explored` flags are ignored, and both `clearFogAroundPoint` (`:531`) and
//! `exploreNNodesAroundLocation` (`:1402`) early-return. So the re-fogging players describe is not
//! state being cleared on traversal — it is state that was never kept.
//!
//! The consequence for us: **nothing beyond one hop can be selected**, so `canTravelToIndirect` and
//! the `Travel` button cannot carry us to a remembered node. Multi-hop travel is unavailable.
//!
//! ## `lostOrientation` — the layout shuffles, the graph does not
//!
//! `overworld/generators/forest.lua:483-490`:
//!
//! ```lua
//! local x, y, t = math.ranSign(), math.ranSign(), love.math.random()<0.5
//! for k, loc in pairs(locationData) do
//!     loc.posX, loc.posY = loc.posX*x, loc.posY*y
//!     if t then loc.posX, loc.posY = loc.posY, loc.posX end
//! ```
//!
//! Two reflections and a transpose — one of the square's eight orientations — applied to **`posX`
//! and `posY` only**. Keys and `connections` are untouched, and `overworldview.lua:1613` re-runs it
//! from `loadLight`, which is why it appears to happen after every encounter.
//!
//! This is the good news, and it is worth stating plainly because the opposite would have been
//! fatal: **an accumulated map stays correct.** What expires is every screen coordinate, on every
//! re-entry rather than merely on a pan.
//!
//! ## Why we do not try to detect any of this
//!
//! We cannot, reliably. The flags live on `typeData.generatorData`, which is never printed, and the
//! heading is ambiguous in both directions: `lost_woods.getTypeName` shows `forest` until the area
//! flag is set and `lost woods` after, while `corrupt_lost_woods` is *always* called `lost woods`
//! and has `thickFog = false, lostOrientation = false`
//! (`overworld/locations/worlds/world/lost_woods.lua:29,41-47`). So the same heading means fogged or
//! not depending on state we cannot see.
//!
//! The way out is that guessing is unnecessary. [`Rules::inside`] is what the fogged case demands,
//! and applying it to *every* subworld costs nothing: we already travel one hop at a time
//! everywhere, and screen coordinates are already re-read from each dump. Being conservative here is
//! free, and being wrong would strand a run.

/// What holds where we are standing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rules {
    /// Can a node that is not adjacent be selected and travelled to?
    ///
    /// False under `thickFog`, and assumed false in any subworld — see the module note on why
    /// detection is not attempted.
    pub multi_hop_travel: bool,
    /// Do screen coordinates survive leaving and coming back?
    ///
    /// False wherever `lostOrientation` applies, because the whole layout is re-rolled on
    /// `loadLight`. Coordinates must be re-read from a fresh dump after *any* absence.
    pub positions_survive_reentry: bool,
    /// Do recorded edges stay true?
    ///
    /// Always. `lostOrientation` moves positions and nothing else, so an accumulated graph remains
    /// valid even in the lost woods. This is the property the whole map depends on.
    pub edges_survive_reentry: bool,
    /// Can arriving here open the anomaly?
    ///
    /// Never inside a subworld: `overworld/events/arrived/world_evil.lua:16` requires
    /// `not location.parentNode`, whatever the node's level.
    pub can_trigger_anomaly: bool,
}

impl Rules {
    /// On the surface: the game will path for us, and arriving at a level > 3 node opens the
    /// anomaly.
    pub const fn surface() -> Self {
        Rules {
            multi_hop_travel: true,
            // Not because of orientation, but because a pan or zoom moves everything anyway.
            positions_survive_reentry: false,
            edges_survive_reentry: true,
            can_trigger_anomaly: true,
        }
    }

    /// Inside any subworld: the conservative set, which is what a fogged, re-oriented one needs.
    pub const fn inside() -> Self {
        Rules {
            multi_hop_travel: false,
            positions_survive_reentry: false,
            edges_survive_reentry: true,
            can_trigger_anomaly: false,
        }
    }

    /// The rules that apply given the subworld parent a dump reported, if any.
    pub fn for_parent(parent: Option<&str>) -> Self {
        match parent {
            Some(_) => Self::inside(),
            None => Self::surface(),
        }
    }
}

/// Prefix of the save flag written once a lost woods has swallowed the player.
///
/// `overworld/events/arrived/lost_woods.lua:25` does
/// `setAreaFlag('lost_woods_known_'..location.key, 1)` on the way in, so `mainSaveData`'s
/// `areaFlags` ends up naming the **exact location key** of every lost woods we have met.
///
/// This is the identification channel to use, and the only reliable one. Headings cannot do it: the
/// same `lost woods` text covers both the fogged original and `corrupt_lost_woods`, which has both
/// hostile flags false. A flag, by contrast, is set by the very event that proves the place is what
/// we think it is.
pub const LOST_WOODS_KNOWN: &str = "lost_woods_known_";

/// The location key a `lost_woods_known_*` flag names, if the flag is one.
pub fn lost_woods_key(flag: &str) -> Option<&str> {
    flag.strip_prefix(LOST_WOODS_KNOWN).filter(|k| !k.is_empty())
}

/// Does arriving at a node with this parent and level open the anomaly?
///
/// The level test is `> 3`, not `== 4` — `overworld/events/arrived/world_evil.lua:18` reads
/// `(location.level or 0) > 3`. The event's remaining conditions (`node_has_no_followups`, the
/// `hell` flag, a heretic/blood-curse exclusion) are not properties of a node's heading and are
/// answered elsewhere.
pub fn triggers_anomaly(parent: Option<&str>, level: Option<u32>) -> bool {
    Rules::for_parent(parent).can_trigger_anomaly && level.unwrap_or(0) > 3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_subworld_forbids_multi_hop_travel() {
        assert!(!Rules::for_parent(Some("forest")).multi_hop_travel);
        assert!(Rules::for_parent(None).multi_hop_travel);
    }

    #[test]
    fn edges_survive_everywhere_including_the_lost_woods() {
        // The property the accumulated map rests on. `lostOrientation` rewrites posX/posY and
        // nothing else, so if this ever becomes false the map has to be thrown away on re-entry.
        assert!(Rules::inside().edges_survive_reentry);
        assert!(Rules::surface().edges_survive_reentry);
    }

    #[test]
    fn positions_never_survive_reentry() {
        // Inside, because the layout is re-rolled; outside, because panning moves everything.
        assert!(!Rules::inside().positions_survive_reentry);
        assert!(!Rules::surface().positions_survive_reentry);
    }

    #[test]
    fn the_anomaly_needs_a_surface_node_above_level_three() {
        assert!(triggers_anomaly(None, Some(4)));
        assert!(triggers_anomaly(None, Some(7)));
        // Level 3 is below the `> 3` the event demands.
        assert!(!triggers_anomaly(None, Some(3)));
        // No combat at all.
        assert!(!triggers_anomaly(None, None));
        // The case that would send a run into a forest expecting an anomaly that cannot fire.
        assert!(!triggers_anomaly(Some("forest"), Some(4)));
    }
}
