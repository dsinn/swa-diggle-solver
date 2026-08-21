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
//! **Where `thickFog` is actually set, which narrows this a great deal.** One file:
//! `lost_woods.lua:15`, with `:47` turning it back off for `corrupt_lost_woods`. Not villages, not
//! ordinary forests. The blanket assumption below is still right for anything whose *kind* we cannot
//! pin down — which is forests, for the reason two sections on — but it was also refusing multi-hop
//! inside settlements, where the heading is unambiguous and there is no fog to fear. That exception
//! is [`crate::overworld::WorldMap::far_hop_inside`], added 2026-08-17.
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

/// Crossing one, which is not yet implemented.
///
/// Required rather than optional: villages gate resting and forests sit on the path. The shape,
/// from the user, and the parts already in hand:
///
/// 1. **Answer the entry dialogue.** The first option is the right one here — the violent options a
///    corrupted village can offer are out of reach with current gear, and
///    [`crate::observe::event::Event::safe_choice`] refuses them regardless.
/// 2. **Cross from the entry to the exit that leads where we are going.** Each exit serves exactly
///    one overworld neighbour and the dump names it, so this is a choice, not a search —
///    [`crate::overworld::WorldMap::exit_toward`] already picks it.
/// 3. **Fight what is in the way.** Many nodes in a corrupted village carry combat, and it shows as
///    a button once we have landed on the node.
/// 4. **Free the inn** — *assumed, not verified.* The working belief is that clearing the corruption
///    off a village's `store_inn` makes it usable again, so a corrupted village is a rest stop to
///    unlock rather than write off, and routing already treats `completed` as the release. No run
///    has demonstrated it: the MVP is to reach the anomaly *quickly* — shrines included — so nothing
///    has yet had reason to clear a corrupted village and then sleep there. It is cheap to believe
///    and cheap to correct: being wrong costs one wasted detour, and the shape it would fail in is
///    `destroyedAreaButtons` (`overworld/generators/village.lua:393-395`) replacing the button set
///    outright, leaving nothing to rest at however thoroughly the place is cleared.
///
/// What is missing is a *nested goal*: while inside, the objective is "reach this exit", not the
/// overworld target. Routing currently aims outside, finds no path, and falls through to leaving.
pub mod crossing {}

/// What holds where we are standing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rules {
    /// Can a node that is not adjacent be selected and travelled to?
    ///
    /// False under `thickFog`, and assumed false in any subworld — see the module note on why
    /// detection is not attempted.
    ///
    /// **Not the whole story since 2026-08-17, and the exception is deliberate.**
    /// [`crate::overworld::WorldMap::far_hop_inside`] allows it inside a **settlement**, where the
    /// blanket assumption was costing a press per node on every walk out of a town. The reasoning
    /// this field records is about telling a forest from a lost woods, which no heading resolves; a
    /// village is a different generator and its heading names it. This field is left conservative
    /// because it is read where the *kind* of subworld is not known.
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

/// The title the mist event prints, and the one unambiguous announcement a lost woods makes.
///
/// `overworld/events/arrived/lost_woods.lua:17`, under a `requireCheck` that is worth reading in
/// full because it is what makes this proof rather than evidence (`:11-15`):
///
/// ```lua
/// return location.type=='lost_woods'
/// and location.typeData.subworld=='forest'
/// and not overworldview.areaFlag('lost_woods_known_'..location.key)
/// and utils.node_has_no_secrets(location)
/// ```
///
/// So the event fires **only** on the genuine article and **only** the first time. It cannot be
/// `corrupt_lost_woods`, whose `type` is different and whose flags are all false — which is the
/// confusable that stops the *heading* being usable for the same purpose. See
/// [`crate::overworld::Place::avoid`].
///
/// The single `Continue` writes `lost_woods_known_<key>` and calls `enterSubworld` in the same
/// `onSelect` (`:23-27`), so the game knows at that instant and we do not until a save is written —
/// `mainSaveData` lands on screen *exit*. Reading it here closes that gap.
pub const MIST_EVENT: &str = "Lost in the mists!";

/// Is this event title the mist event? See [`MIST_EVENT`].
pub fn is_the_mist_event(title: &str) -> bool {
    title.trim() == MIST_EVENT
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

    /// The mist event's title, matched against the console text a run actually printed.
    ///
    /// Not against the string constant — that would be comparing a literal with itself. This parses
    /// the real dialogue through [`crate::observe::event::parse_events`], which is the path the
    /// navigator uses, so a change to either the parser or the game's wording fails here.
    #[test]
    fn the_mist_event_is_recognised_from_the_console_text() {
        let printed = r#"Event:  Lost in the mists!
While travelling the misty road into Bainton Clump you suddenly become aware that you've lost track of the path.
Choices = {
    {
        text = "Continue",
        posX = 960,
        posY = 745,
    },
}
"#;
        let lines: Vec<String> = printed.lines().map(str::to_string).collect();
        let ev = crate::observe::event::parse_events(&lines).pop().expect("one event");
        assert!(is_the_mist_event(&ev.title), "title was {:?}", ev.title);
        // One choice, and it is the one that writes the flag and enters the subworld
        // (`overworld/events/arrived/lost_woods.lua:22-27`). There is no declining it, which is why
        // recording the node is the whole of what we can do about it.
        assert_eq!(ev.choices.len(), 1);

        // **And nothing else is it.** A forest event that is not the mists must not wall off a node.
        assert!(!is_the_mist_event("Stump in the road"));
        assert!(!is_the_mist_event("Lost in the mists"), "the exclamation mark is part of it");
        assert!(!is_the_mist_event(""));
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
