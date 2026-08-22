//! What we know about one location, and every question a planner asks of it.
//!
//! Split out of `overworld.rs` on 2026-08-21 (#76). This is the half of the planner that reasons
//! about a **single** node — is it a settlement, does it owe a consecration, is it a fight we
//! should bank for — with no reference to the graph around it. That is what makes it separable at
//! all: of the twelve mentions of [`WorldMap`] below, every one is a doc link and none is a call.
//!
//! The predicates are deliberately here rather than at their call sites. Most of them were written
//! from the game's Lua and carry the citation that justifies them, and a rule whose justification
//! lives somewhere else is a rule that gets re-litigated. See the parent module for the graph, the
//! frame and the goal ladder.

use std::collections::BTreeSet;

// Free functions and types that stay in the parent. `WorldMap`, `Frame` and
// `SHRINES_BEFORE_THE_ANOMALY` are imported for the **doc links** below and are not called from
// here — carrying the arguments intact was the point of the split, and requalifying a dozen links
// would have edited the design record to suit the file layout.
#[allow(unused_imports)]
use super::{heading_has_combat, key_is_major_shrine, Frame, WorldMap, SHRINES_BEFORE_THE_ANOMALY};

/// What we know about one location.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Place {
    pub key: String,
    /// `AreaHeading` output. Carries the level and type for combat nodes.
    pub heading: String,
    /// The combat level this node carried while it was **not** corrupted, kept for good.
    ///
    /// ## Why the heading cannot be the only record of it
    ///
    /// `AreaHeading` prints `— level N` only while `locationHasCombat` is true
    /// (`overworldview.lua:388-389`), so **clearing a node deletes its level from every future
    /// reading of it**. That is correct for the live game and wrong for a file we keep: the run of
    /// 2026-08-22 cleared five combat nodes and wrote `l13 Bilton crypt` into the world cache where
    /// it had met `Bilton — level 2 crypt`. A later run on the same seed absorbs that, finds the
    /// crypt *not* complete in its own save, and reads it as a free walk — poisoning
    /// [`Place::may_be_a_fight`], and through it every route cost the planner computes.
    ///
    /// Combat-zone-ness is a fact about the world, which is a pure function of the seed
    /// (`overworld/generators/world.lua:65`). Completion is a fact about one profile. Keeping the
    /// level in a field of its own separates them, so a cleared node still says *this is a fight if
    /// you have not had it yet*.
    ///
    /// ## Why "uncorrupted", and what is derivable from what
    ///
    /// Corruption rewrites the level upward — `level = math.max(3, baseLevel, 7-baseLevel)`
    /// (`world.lua:499-502`) — so 1, 2 and 3 come back as 6, 5 and 4 and nothing else moves. That
    /// map is not invertible: a level 4 reading is either a base 4 or a corrupted base 3. The dev's
    /// ruling, 2026-08-22: *I'd agree with tracking specifically the uncorrupted node level. Future
    /// runs can re-derive it from the corrupted state.* So this records only what was read while
    /// [`Place::corrupted`] was false, and the corrupted level stays derivable from it; the reverse
    /// would not be.
    ///
    /// A place first met already corrupted keeps `None` here, and loses nothing by it — `corrupted`
    /// is itself the second clause of [`Place::may_be_a_fight`], and it is read fresh from the save
    /// every step.
    ///
    /// **One window where this can overstate.** Corruption arrives from the save and headings arrive
    /// from dumps, so a node corrupted between one save read and the next dump is recorded at its
    /// inflated level. It errs toward a harder fight than the world holds, which is the direction
    /// [`Place::deliberate_fight_level`] wants to be wrong in, and one save read closes it.
    pub base_level: Option<u32>,
    /// Neighbours we have seen named. Never includes hidden ones.
    pub neighbours: BTreeSet<String>,
    /// How many locations this one connects to, as the dump reports it. A dead end is 1, and a
    /// well-connected node is more likely to open the map up.
    pub connections: u32,
    /// How many neighbours the game refused to describe, from the dump taken **at** this node.
    /// `None` until we have stood here — a neighbour's dump cannot tell us this.
    pub hidden: Option<usize>,
    /// The subworld this sits inside, if any. Anomaly eligibility turns on this being `None`.
    pub parent: Option<String>,
    /// Where this sits on the overworld, in the run's own frame. `None` until a dump places it.
    ///
    /// Not the numbers the dump printed: those are screen space and move with every pan. See
    /// [`WorldMap::registration`] for how they are put into one frame, and what that frame is worth
    /// (everything up to a global translation and scale, which is all a *comparison* needs).
    ///
    /// **A position is not a route.** It says which way a place lies, never whether we can get
    /// there — the two are different questions and this project has an expensive habit of answering
    /// the second with the first. Use it where a route is unavailable and a direction is better than
    /// a coin toss, which is what [`WorldMap::exit_toward`]'s fallback does.
    pub pos: Option<(f64, f64)>,
    /// Have we stood on it and taken a dump?
    pub visited: bool,
    /// Has a dump **this run** named this place — as opposed to the heading coming off disk?
    ///
    /// Per-run and deliberately not cached, for the same reason [`Place::visited`] is not: it is a
    /// statement about what we have been told since the process started, and a file cannot make it
    /// true. It is what lets [`Place::remembered_level`] treat a silent live heading as the game
    /// saying *free* while treating a silent recalled one as *we no longer know*.
    pub heading_from_dump: bool,
    /// From `mainSaveData.overworld.completedAreas`.
    pub completed: bool,
    /// `<key>_consecrated` in `areaFlags`, which is exactly what `isConsecrated` reads
    /// (`overworldview.lua:319`). Only meaningful on a shrine.
    pub consecrated: bool,
    /// Has corruption ever reset this area to incomplete?
    ///
    /// From `<key>_first_corrupt_time`, which `setAreaIncomplete` stamps with the activity counter
    /// and never overwrites (`overworldview.lua:191-192`). This is what separates the two ways a
    /// place can be incomplete: **never fought** (no flag — an ordinary fight) from **corrupted**
    /// (flag present — reset by the spreading hell radius, and a nastier fight for it).
    ///
    /// Not a perfect oracle. `setAreaIncomplete` is also called twice during world generation
    /// "just in case" (`utils/world.lua:1222`, `:1237`), so a false positive is possible; every
    /// other caller is corruption — `setHellValue` (`hellportal.lua:173`), the `hellOpens` sequence
    /// (`utils/events.lua:43,66,96`), and the subworld propagation downstream of them.
    pub corrupted: bool,
    /// Completed **while corrupt**: `completedAreas[key..'_corrupt']` (`overworldview.lua:172`).
    ///
    /// On the anomaly this is the run's whole objective.
    pub completed_corrupt: bool,
    /// `<key>_used` in `areaFlags` — the flag `areaHasBeenUsed` reads
    /// (`overworldview.lua:215-218`). For a campfire this is the difference between a free rest and
    /// a wasted walk.
    pub used: bool,
    /// `<key>subs` in `areaFlags` — **the shrine's word has been played at least once.**
    ///
    /// No underscore: the game builds it as `data.dataKey..'subs'` (`shrineview.lua:267`), and it
    /// writes the whole submissions list there on every accepted guess. `shrine.lua:495` hands it
    /// back to the view on the next entry, which is why a shrine solved in an earlier run opens
    /// already won.
    ///
    /// What it is *for* here is the negative case. `hasWon()` is
    /// `word == submitions[#submitions-1]` (`shrineview.lua:57-59`), so no submissions means no win,
    /// which means `ShowAGoodButton()` is false and `Consecrate` is drawn greyed. A shrine with no
    /// `subs` cannot be consecrated by anything that does not first solve the word — and
    /// `shrineplay::consecrate` has no typist at all.
    ///
    /// Present rather than parsed: a partial attempt writes `subs` too, so this says "played", not
    /// "won". That is the right strength for the one question it answers, which is whether pressing
    /// a button could possibly do anything.
    pub played: bool,
    /// This node is a subworld **container** — a forest, a village — not a place we fight on.
    ///
    /// Learned by evidence, never by type name. `getLocationButtons` checks `typeData.subworld`
    /// *before* `basicCombatZone` (`overworldview.lua:462-467`), so a forest's lone area button
    /// enters the subworld even though its heading carries a level. A live run read
    /// `Eight Timberland — level 4 forest` as a fight, clicked, and walked into the forest.
    ///
    /// Set when a dump reports us inside a subworld whose parent is this key. Mirroring the game's
    /// type table would be the other way to know, and it is exactly the drift this project avoids.
    pub subworld_container: bool,
    /// `<key>_attack` is in `areaFlags`: a fight for this settlement is happening now.
    ///
    /// Its buildings answer `Enter` with `ui.building_empty` while it lasts
    /// (`overworld/generators/village.lua:97`, `:371-388`) — the same button, a different room, and
    /// no bed in it. Cleared when the attack is beaten (`:54`).
    pub under_attack: bool,
    /// `<key>_attacked` is in `areaFlags`: this settlement, or this building, was lost.
    ///
    /// Permanent. All that is left is `Loot` (`village.lua:393-395`), which is task #26 and not a
    /// rest. See [`Place::trades`] and [`Place::loot_left`].
    pub sacked: bool,
    /// `<parent>_attack.playerAttack` — **we** are the ones who sacked this settlement.
    ///
    /// Recorded on the parent, and the third of the three ways a building's buttons become the
    /// destroyed set (`village.lua:91-96`). Distinct from [`Place::under_attack`], which is the same
    /// `_attack` flag merely existing: an attack in progress leaves the buildings shut but intact,
    /// and this says the village is already past saving.
    pub player_sacked: bool,
    /// `<key>_looted` — how many times this building has been picked over.
    ///
    /// The counter `createLootButton` tests: the button stays pressable while
    /// `(areaFlag(<key>_looted) or 0) < (loot or 2)` (`utils/world.lua:1317-1332`), so it is a budget
    /// rather than a flag. Written by `incrementLootCounters` on every outcome, including the one
    /// that finds nothing.
    pub looted: u32,
    /// A lost woods we have already been swallowed by, and will not enter again.
    ///
    /// Routing treats these as walls and only passes through one if there is no other way at all.
    ///
    /// Two channels write it, and they say the same thing at different times:
    ///
    /// - the `lost_woods_known_*` save flags ([`crate::subworld::LOST_WOODS_KNOWN`]), read by
    ///   [`WorldMap::apply_save`];
    /// - the mist event, read off the console by [`WorldMap::mark_lost_woods`] the moment it is
    ///   answered. `mainSaveData` is written on screen **exit**, so the flag the game sets in the
    ///   same `onSelect` does not reach us until we have already crossed the woods.
    ///
    /// **The heading is deliberately not a third channel**, though it is tempting: `getTypeName`
    /// starts printing `lost woods` from the instant the flag is set (see [`Place::in_lost_woods`]),
    /// which is exactly when we would like to know. But `corrupt_lost_woods` prints the same words
    /// unconditionally (`lost_woods.lua:41`) with `thickFog` and `lostOrientation` both **false**
    /// (`:44-47`) — an ordinary forest as far as we are concerned — and the flag that separates them
    /// is `corrupted`, which comes from the save we are trying not to wait for. Walling off a node
    /// is permanent and never re-examined, so it takes a signal that cannot mean two things. The
    /// event is one; the heading is not.
    pub avoid: bool,
    /// This node is **inside a lost woods**, where the generator disguises its own roads.
    ///
    /// Set in [`WorldMap::fold`] from the container's live heading, for the container and every
    /// subnode of it. It is not a save flag and must not be confused with [`Place::avoid`], which
    /// says "do not go in there" and is read from `lost_woods_known_*` at load. This says "we are in
    /// there now", which is the situation `avoid` exists to prevent and cannot help with.
    ///
    /// ## What it changes, and why the heading is enough to know it
    ///
    /// `lost_woods.lua:29` makes the type name conditional:
    ///
    /// ```lua
    /// getTypeName = function(self)
    ///     return overworldview.areaFlag('lost_woods_known_'..self.key) and 'lost woods'
    ///         or self.typeData.typeName  -- 'forest'
    /// end
    /// ```
    ///
    /// so the woods read as an ordinary `forest` until the mist event names them, and thereafter the
    /// game itself prints `lost woods` in every heading. Evaluated live, which is why this is learned
    /// by observation rather than mirrored from a type table.
    ///
    /// The one confusable is `corrupt_lost_woods`, whose `typeName` is `'lost woods'`
    /// **unconditionally** (`:41`) — but it also sets `lostOrientation = false` and `thickFog = false`
    /// (`:44-47`), so none of the disguises below apply to it. [`Place::corrupted`] separates them.
    pub in_lost_woods: bool,
}

/// What standing on a place would cost us. See [`Place::arrival`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrival {
    /// Nothing. `competeOnVisit` holds, or the area is already cleared, so walking on finishes it.
    Free,
    /// A fight at this level, which we may not decline.
    Fight { level: u32 },
    /// We have a key and no heading, so we cannot say. **Not** free — see [`Place::arrival`].
    Unknown,
}

impl Arrival {
    /// May a hurt run walk onto this without picking a fight?
    ///
    /// Deliberately false for [`Arrival::Unknown`]. "We have not seen it" is not "it is safe", and
    /// reading the two as the same thing is what let a run at 1/20 be routed onto a level 6 crypt.
    pub fn is_free(self) -> bool {
        matches!(self, Arrival::Free)
    }
}

/// How bad a place is likely to be, ordered safest first.
///
/// This is the dev's ranking, and the source bears out each rung:
///
/// - [`Risk::Free`] — no fight at all. `AreaHeading` prints no level.
/// - [`Risk::Forest`] — a fight, but the gentlest kind. A forest is a subworld whose interior nodes
///   are individually peaceful or not (`competeOnVisit = subnodeIsPeaceful`, `forest.lua:109,123,
///   137,…`), so a route across one can take **zero** fights, and the ones it does take are short.
///   There is an upside too: an apple orchard is a forest until entered — `getTypeName` returns
///   `'orchard'` only once `variant == 2`, which needs the area flagged or complete
///   (`orchards.lua:23`, `in_forest.lua:8`).
/// - [`Risk::Fight`] — a fight we cannot route around. A crypt is `basicCombatZone` with no
///   `competeOnVisit`, so there is exactly one node and it is hostile.
/// - [`Risk::Unseen`] — no heading, so no claim either way. Below the known fights because being
///   unable to show something is safe is worse than knowing what it costs.
/// - [`Risk::Corrupt`] — uncleared and reset by the hell radius, which also raises the level
///   (`world.lua:499-502`). The worst on the map that is not the anomaly itself.
///
/// ## The bandit-camp gamble, priced
///
/// The reason a forest was previously lumped in with the worst of them: `world.lua:466-475` rewrites
/// some forests into `bandit_camp_pine` / `bandit_camp_oak`, and a camp is a forest as far as
/// anything outside it can tell. Inside one, `completeOnVisit` returns
/// `areaIsComplete(parentNode.key)` (`bandit_camp_forest.lua:52-57`) — so **every** subnode fights
/// until the camp is cleared, and the peaceful-route argument above evaporates.
///
/// What makes it a gamble worth taking rather than a wall is the count. It is
/// `modifyDistributedLocations(forests, 3, …)` — **exactly three per world**, not a rate. With a
/// dozen forests visible the prior is a quarter, and it falls as more are revealed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Risk {
    Free,
    Forest,
    Fight,
    Unseen,
    Corrupt,
}

impl Place {
    /// Combat level from the heading, mirroring [`crate::observe::adjacency::Node::level`].
    pub fn level(&self) -> Option<u32> {
        let rest = self.heading.split(" — level ").nth(1)?;
        rest.split_whitespace().next()?.parse().ok()
    }

    pub fn has_combat(&self) -> bool {
        self.level().is_some()
    }

    /// Record a heading **the game printed this run**, and bank the level out of it.
    ///
    /// The single door for every heading that arrives from a dump, so that [`Place::base_level`]
    /// cannot be forgotten at one of the four fold sites that write one. A heading with no level in
    /// it — which is what a cleared node prints — leaves the banked level alone: **a stripped
    /// heading must never un-teach a fight.**
    pub fn observe_heading(&mut self, heading: &str) {
        self.heading = heading.to_string();
        self.heading_from_dump = true;
        self.bank_level();
    }

    /// The same, for a heading coming back off disk rather than off the console.
    ///
    /// It fills a gap and claims nothing about the present, which is what
    /// [`Place::heading_from_dump`] stays false to say. The level is still worth taking: a cache
    /// written before the level had a column of its own kept it in whichever headings had not yet
    /// been stripped.
    pub fn recall_heading(&mut self, heading: &str) {
        self.heading = heading.to_string();
        self.bank_level();
    }

    /// Uncorrupted readings only; see [`Place::base_level`] for why the corrupted level is the
    /// derivable direction rather than the fact worth storing.
    fn bank_level(&mut self) {
        if !self.corrupted {
            if let Some(level) = self.level() {
                self.base_level = Some(level);
            }
        }
    }

    /// The level to reason about a fight with: what the game says if it has said anything this run,
    /// and what we banked if all we have is a recollection.
    ///
    /// ## A live heading always wins, including when it is silent
    ///
    /// `AreaHeading` is `locationHasCombat` evaluated on the spot (`overworldview.lua:383-392`), so
    /// a dump that names a place **without** a level is the game stating that arriving there is
    /// free — a stronger answer than `completedAreas`, which reaches us through a file the game
    /// only writes on screen *exit*. Falling back to the bank in that case reintroduced the bug the
    /// bank was built to fix, one layer down: replaying two runs' dumps, a subnode of `l40` cleared
    /// mid-crossing came back hostile, and `cross_toward` and its neighbour began nominating each
    /// other. The replay never applies a save, which is the extreme of the same window a live run
    /// opens for a step or two after every fight.
    ///
    /// So the bank speaks only where nothing else can: a place whose heading we have from the cache
    /// and have not seen named this run. That is exactly the population #79 is about.
    pub fn remembered_level(&self) -> Option<u32> {
        self.level().or_else(|| match self.heading_from_dump {
            true => None,
            false => self.base_level,
        })
    }

    /// **What arriving costs**, which is not the same question as [`Place::hostile_to_enter`].
    ///
    /// The game answers this one for us and we were not listening. `AreaHeading`
    /// (`overworldview.lua:383-392`) branches on `locationHasCombat`:
    ///
    /// ```lua
    /// if core.locationHasCombat(location) then
    ///     return location.name..' — level '..location.level..' '..typeName
    /// else
    ///     return location.name..' '..typeName
    /// end
    ///
    /// -- :305-310
    /// function core.locationHasCombat(location)
    ///     if core.areaIsComplete(location.key) then return false end
    ///     return not core.locationIsCompleteOnVisit(location)
    /// end
    /// ```
    ///
    /// So **`— level N` in a heading means arriving costs a fight, and its absence means it does
    /// not** — evaluated live, so it already accounts for corruption and for shrine karma. No type
    /// table to mirror and no drift to maintain. An uncorrupted shrine prints `Gembling shrine`
    /// (`shrine.lua:37-45`); a wizards' tower prints `<name> wizards' tower`, unconditionally
    /// (`wizard_tower.lua:18`); a crypt always carries a level (`crypt.lua`, no `competeOnVisit`,
    /// `basicCombatZone = true`).
    ///
    /// ## Why [`Arrival::Unknown`] has to exist
    ///
    /// A place we know only by key — learned from `completedAreas` or an `areaFlags` suffix, never
    /// seen in a dump — has an empty heading, and an empty heading has no level in it. Read as a
    /// plain boolean that is indistinguishable from "free", so every unheaded node advertised itself
    /// as safe. It is also a **frontier** by [`Place::is_frontier`], since we have not stood on it,
    /// so it is eligible to be chosen. Eleven of the twenty-two places in the last live run's map
    /// were in that state.
    pub fn arrival(&self) -> Arrival {
        // `locationHasCombat` short-circuits on completion before it looks at anything else, so a
        // cleared node is free however it got that way -- and our stored heading may predate the
        // clearing, which is the case this ordering protects against.
        if self.completed {
            return Arrival::Free;
        }
        if self.heading.trim().is_empty() {
            return Arrival::Unknown;
        }
        match self.remembered_level() {
            Some(level) => Arrival::Fight { level },
            None => Arrival::Free,
        }
    }

    /// How much we would rather not go here, for ranking rather than for filtering.
    ///
    /// [`Arrival`] says whether a fight is owed; this says how bad it is likely to be, and the
    /// ordering is the dev's, from playing the game. See [`Risk`] for what each rung is worth.
    pub fn risk(&self) -> Risk {
        if self.completed {
            return Risk::Free;
        }
        // Ahead of the heading, because corruption rewrites the level upward without changing the
        // type: `level = math.max(3, baseLevel, 7-baseLevel)` (`world.lua:499-502`). A corrupted
        // level 1 forest is a level 6 fight wearing a forest's name.
        if self.corrupted {
            return Risk::Corrupt;
        }
        match self.arrival() {
            Arrival::Free => Risk::Free,
            Arrival::Unknown => Risk::Unseen,
            Arrival::Fight { .. } if self.type_is("forest") => Risk::Forest,
            Arrival::Fight { .. } => Risk::Fight,
        }
    }

    /// Would going in here commit us to a fight **to get back out**?
    ///
    /// One of two danger axes and not the more obvious one. [`Place::arrival`] answers "does landing
    /// here cost a fight"; this answers "having landed, can we leave". They are independent, and
    /// conflating them is what put a run at 1/20 on a level 6 crypt: this predicate was the
    /// planner's only danger filter, so it blocked unvisited *forests* — where the fights are short
    /// and often avoidable — while waving through *crypts*, where a fight is guaranteed. Exactly
    /// backwards, because it was never asking the arrival question at all.
    ///
    /// Being hurt is no reason to avoid a subworld as such — a quiet village is somewhere to *rest*.
    /// The thing to avoid is a **hostile** one, because a subworld's exit is gated the same way its
    /// interior is: `canTravelToDirect` refuses to step off an incomplete node
    /// (`overworldview.lua:1316-1321`), so entering one that is under attack means clearing a node
    /// before you can leave again. That is how a live run at 4/12 walked into Ulrome and had to
    /// fight its way back out.
    ///
    /// Two ways to be hostile, and only one of them is visible from outside:
    ///
    /// - **An uncleared corrupted area.** Corruption puts the place under attack and swaps its
    ///   buttons for `underAttackAreaButtons` (`overworld/generators/village.lua:371-395`). Knowable
    ///   in advance, from `<key>_first_corrupt_time` against `completedAreas`.
    /// - **A bandit camp.** Not knowable. `overworld/generators/world.lua:466-475` takes an ordinary
    ///   forest and rewrites `forest.type` to `bandit_camp_pine` or `bandit_camp_oak`, so the camp
    ///   *is* a forest as far as anything outside it can tell. There is no heading to read and no
    ///   flag to check — you find out by arriving.
    ///
    /// The second is why an unvisited **forest** counts as hostile rather than unknown: we cannot
    /// show it is safe, only that it has not bitten us yet.
    ///
    /// Forests specifically, and not subworlds in general. `banditos` maps exactly `pine_forest` and
    /// `oak_forest`, so a village is never rewritten into a camp — and treating every unvisited
    /// container as hostile would block resting at the first quiet village we find, which is the
    /// opposite of what wanting a rest is for.
    ///
    /// ## The over-correction this used to carry, and where it went
    ///
    /// Not every unvisited forest is a camp — an **apple orchard** is a perfectly peaceful one — and
    /// while this was the only danger filter, refusing forests outright meant refusing the orchards
    /// too. That is now [`Risk::Forest`]'s job: a forest is still *avoided* while we are hurt and
    /// have a free alternative, but when everything left costs a fight it is **preferred** over a
    /// crypt rather than lumped in with it. Telling an orchard from a camp before entering is still
    /// impossible (`orchards.lua:23`), and still not worth researching; ranking makes it cost a
    /// worse-first ordering instead of a wall.
    ///
    /// Cleared is safe, whichever way it got that way: corruption that has been fought off leaves
    /// `completed` set, and a camp we have already emptied is not going to refill.
    /// ## Inside a lost woods, `forest` in a heading does not mean what it means anywhere else
    ///
    /// The bandit-camp reasoning above is about a *container* we are deciding whether to enter. It
    /// leaks onto the interior because `node` — the generator's filler type — has
    /// `typeName = 'forest'` (`forest.lua:131-135`), so an unvisited subnode reads hostile too. In an
    /// ordinary forest that is survivable: the road and crossroads nodes keep their own type names,
    /// so the paved route across is exempt and there is always something to walk on.
    ///
    /// A lost woods removes that escape. `forest.lua:653-659` renames every interior road:
    ///
    /// ```lua
    /// if areaData.lostOrientation then
    ///     for k, loc in pairs(locationData) do
    ///         if loc.type=='road' and not loc.targetNode then
    ///             loc.type = 'node'
    ///         end
    ///     end
    /// end
    /// ```
    ///
    /// and interior roads never carry a `targetNode` — `world.lua:1174` sets only `type` — so the
    /// whole interior reads `forest`, every neighbour looks hostile, and a hurt run has nowhere it is
    /// willing to step. The disguise is the *point* of the place; treating it as evidence of a bandit
    /// camp is reading the generator's costume as a threat.
    ///
    /// Narrowed to the lost woods rather than lifted for all subnodes, which is the other defensible
    /// fix. `banditos` rewrites surface forests only (`world.lua:466-475`), so on the merits the rule
    /// has no business inside *any* subworld — but widening it that far changes where hurt runs are
    /// willing to walk everywhere, and that wants its own run to justify it.
    pub fn hostile_to_enter(&self) -> bool {
        if self.completed {
            return false;
        }
        self.corrupted || (self.type_is("forest") && !self.visited && !self.in_lost_woods)
    }

    /// A made road: the paved way through a subworld's interior.
    ///
    /// `forest.lua:117-130` gives `road` and `:107-116` `crossroads`, both with
    /// `competeOnVisit = subnodeIsPeaceful` — so a paved node is peaceful unless something is
    /// standing on it, and the generator strings them into a route from entrance to exit.
    ///
    /// One subtlety worth stating rather than discovering: [`Place::type_is`] is a suffix test and
    /// `"crossroads"` ends with `"road"`, so the first check already covers the second. Both are
    /// written out because a reader should not have to notice that.
    ///
    /// ## In a lost woods this stays correct and stops meaning the same thing
    ///
    /// The rename at `forest.lua:653-659` (quoted in [`Place::hostile_to_enter`]) turns every
    /// interior `road` into a plain `node`, because only the out-roads carry a `targetNode`. Two
    /// things therefore survive as paved in there, and both are better to walk to than the road
    /// network we lost:
    ///
    /// - **`crossroads`**, untouched because its type is `'crossroads'` and the rename tests for
    ///   `'road'`. `world.lua:1173-1174` gives that type to a node with more than two road segments
    ///   through it, so what survives is exactly the junctions of the entrance-to-plaza spine.
    /// - **the out-roads**, which are the exits themselves.
    ///
    /// So "prefer paved" changes from *follow the road across* to *make for a junction or a way out*,
    /// which is the better instruction of the two when the map is fogged. The tile layer agrees and
    /// is not ours to read: `paveRoads = false` with `paveInsetRoads = true`
    /// (`lost_woods.lua:8-9`) means the only tiles actually painted as road are the exit approaches.
    pub fn is_paved(&self) -> bool {
        self.type_is("road") || self.type_is("crossroads")
    }

    /// A treasure chest node. `typeName = 'chest'` (`overworld/generators/forest.lua:178`).
    ///
    /// **Opening one is a fight, not a reward.** `chestOpenButton` calls
    /// `overworld.startNewRun(scenarios.chest(location))` (`:30-38`), which builds a chest enemy with
    /// `maxHealth = level + 3` (`utils/enemies.lua:205-209`) — four at level 1.
    ///
    /// And it can be a **mimic**, which is worse than it looks. `Open` produces a lone chest, so it
    /// takes the second of the two mimic paths (`utils/combat.lua:442`):
    ///
    /// ```lua
    /// if #enemies==1 and enemies[1].type=='chest' and seedDigits%30 < scenario.level then
    /// ```
    ///
    /// So the chance is `level/30`: **0% only at level 0**, 1 in 30 at level 1, and upward from
    /// there. A level 1 forest is the lowest non-zero risk rather than a safe one, and
    /// `combat.calculateLevel` (`:456-468`) floors curse reductions at `min(1, startLevel)`, so a
    /// level 1 node cannot be cursed down to the safe case. The other path
    /// (`:422-425`) needs `level >= 3` and a boss to disguise as, so it never applies here.
    ///
    /// It is deterministic per seed rather than a die roll — `seedNormal` decides it — so a chest
    /// either is or is not a mimic before we touch it. We just cannot see which.
    /// **Opening one is a fight**, which is why nothing special is needed to play it. Its area
    /// button is `Open` at the same `ss(0, 0.85)`, `xOffset 0.75` slot every other area button uses
    /// (`overworld/generators/forest.lua:30-39`), and pressing it calls
    /// `overworld.startNewRun(utils.combat.scenarios.chest(…))`. The node completes when the chest
    /// or the mimic inside it dies (`:190-194`), and `getAreaButtons` (`:186-188`) offers `Open`
    /// only while it is incomplete — so an opened chest has no button at all.
    pub fn is_chest(&self) -> bool {
        self.type_is("chest")
    }

    /// The inn subnode of a village. `typeName = 'inn'` (`village.lua:341`).
    ///
    /// Nothing else in a village ends in those three letters — the other types are `crossroads`,
    /// `guard post`, `guard tower`, `well`, `market stall`, `general store`, `apothecary`, `house`
    /// and `chapel` (`village.lua:161-495`) — so the suffix test is safe here in a way it would not
    /// be for a shorter word.
    ///
    /// **Found, not derived.** `store_inn` is assigned to whichever `<parent>sub<N>` slot the
    /// generator reaches first (`village.lua:685`), so unlike an exit road there is no key to build:
    /// the inn has to be *seen* before it can be walked to, which is why [`WorldMap::cross_toward`]
    /// has a state for looking.
    pub fn is_inn(&self) -> bool {
        self.type_is("inn")
    }

    /// A village's general store. `typeName = 'general store'`
    /// (`overworld/generators/village.lua:291-292`), read from the heading the same way
    /// [`Place::is_inn`] reads its own.
    /// Might walking onto this cost a fight? **Unverified is not safe.**
    ///
    /// The dev's rule, 2026-08-15: **the cache says what exists; the dump says what it is.** Two
    /// things mean a fight may be owed, and neither of them is an unverified heading:
    ///
    /// - a level in the heading, which is the reading itself. `AreaHeading` prints `— level N` only
    ///   when `locationHasCombat` (`overworldview.lua:388-389`), so the level *is* the combat flag;
    /// - corruption, from `<key>_first_corrupt_time` in the save — read fresh every step, and the
    ///   one thing that turns a cleared node back into a fight (`world.lua:499-502`).
    ///
    /// **Which is why a recalled heading needs no pessimism of its own.** The case that started this
    /// was `l4`, cached as `Riccall crypt` from a run in which it was clear and met as
    /// `Riccall — level 6 crypt`. What changed it was corruption, and corruption is in the save — so
    /// the second clause catches it without the first having to distrust every remembered name on
    /// the map. An earlier version marked recalled headings as suspect and went quiet on both
    /// detours across a whole cached world; the dev's correction is that the cache was never the
    /// thing making the safety claim.
    ///
    /// **And corruption does not speak for a node that completes on visit** — task #93's
    /// parenthetical, and the reason is the game's own definition of combat:
    ///
    /// ```lua
    /// function core.locationHasCombat(location)
    ///     if core.areaIsComplete(location.key) then
    ///         return false
    ///     end
    ///     return not core.locationIsCompleteOnVisit(location)
    /// end
    /// ```
    ///
    /// `overworldview.lua:305-310`. Arriving costs a fight when the area is **incomplete** *and*
    /// does **not** complete on visit — which is exactly `!completed && may_be_a_fight()` as the
    /// callers spell it, with [`Place::completes_on_visit`] standing in for the second half.
    ///
    /// Corruption is `setAreaIncomplete` (`overworldview.lua:183-206`) and nothing more: it clears
    /// `completedAreas[key]`, so it moves the *first* clause and leaves the second alone. On a type
    /// that completes on visit the first clause was already the only thing holding the answer down,
    /// and taking completion away gives it straight back on arrival. The `|| self.corrupted` here
    /// was pessimism standing in for a save flag the heading cannot carry, and on those types it is
    /// pessimism about nothing.
    ///
    /// `completed` is checked by the caller, since it is what says whether a fight is still owed.
    pub fn may_be_a_fight(&self) -> bool {
        heading_has_combat(&self.heading)
            || self.remembered_level().is_some()
            || (self.corrupted && !self.completes_on_visit())
    }

    /// Does the game complete this node the moment we walk onto it, with no fight?
    ///
    /// `core.locationIsCompleteOnVisit` reads `typeData.competeOnVisit` (`overworldview.lua:288-291`
    /// — the game's own spelling of "compete"), which is either a literal or a function of the
    /// location. This is the **literal `true`** set, and only that set: six files under
    /// `overworld/locations/`, each one a surface type.
    ///
    /// | file | `typeName` |
    /// |---|---|
    /// | `campfire.lua:19` | `campfire` |
    /// | `capital.lua:61` | `city` |
    /// | `crossroads.lua:6` | `crossroads` |
    /// | `grave.lua:10` | `grave` |
    /// | `out_road.lua:29` | `road` |
    /// | `wizard_tower.lua:18` | `wizards' tower` |
    ///
    /// **The function forms are deliberately absent, and they are the ones corruption bites.** A
    /// shrine's is `not (location.corrupt or shrineKarma>5)` (`locations/shrine.lua:37-44`) and a
    /// village's is `not location.corrupt` (`locations/village.lua:63-70`) — both read the very flag
    /// this exemption is about, so both stay a fight when corrupted, which is what they are.
    ///
    /// ## Surface only, and the parent gate is not caution — it is a different file
    ///
    /// The subworld generators reuse two of these nouns for their own subnode types and give them
    /// `competeOnVisit = subnodeIsPeaceful`, a *function* that asks whether enemies are standing
    /// there (`generators/forest.lua:13-15,103-131`). That is the dev's tree: an interior crossroads
    /// may hold one. So `Bainton Clump crossroads` inside a forest is not this, and the caches prove
    /// it can say so out loud — `Bainton Clump — level 6 road` and `Bainton Clump — level 6
    /// crossroads` are both real headings, with the level a surface one can never print.
    ///
    /// [`Place::type_is`] matches the heading's tail, so without the gate `Gripthorpe Brush road`
    /// would read as an out_road. Every `competeOnVisit = true` file lives in `overworld/locations/`
    /// and every function form lives in `overworld/generators/`, so "has no parent" separates them
    /// on the same line the source does.
    pub fn completes_on_visit(&self) -> bool {
        const PEACEFUL: &[&str] =
            &["campfire", "city", "crossroads", "grave", "road", "wizards' tower"];
        self.parent.is_none() && PEACEFUL.iter().any(|t| self.type_is(t))
    }

    /// Does this settlement's general store stock a `Heart`? **The heading says so outright.**
    ///
    /// `getTypeName` (`overworld/locations/village.lua:5-14`) picks the noun from the special stock
    /// itself:
    ///
    /// ```lua
    /// local gearB, healthB = self.specialStock.gearSlotsBuff, self.specialStock.healthBuff
    /// if gearB then return 'town'
    /// elseif not gearB and not healthB then return 'hamlet' end
    /// return 'village'
    /// ```
    ///
    /// So `village` *is* "has a healthBuff and no gearSlotsBuff", `town` is "has a gearSlotsBuff,
    /// and may have a heart as well", and `hamlet` is "neither". The dev's standing assumption —
    /// every village starts with one — turns out to be the game's own definition rather than a
    /// guess, and it comes with the two cases the assumption missed: a town may have one too, and a
    /// hamlet never does.
    ///
    /// It does not go stale when the heart is bought. `specialStock` seeds the shop
    /// (`shop.lua:372-379`) and a purchase decrements `shopData`, not this — which is why *whether
    /// it is still for sale* is a different question, asked of the save.
    /// **A corrupted settlement stocks nothing until its enemies are cleared.**
    ///
    /// The dev, 2026-08-16: *corruption prevents us from buying anything in that village until all
    /// of the enemies are cleared.* The driver's rest gate has carried the same clause
    /// (`!p.corrupted || p.completed`) since villages under attack were first met; the planner's
    /// heart filter never did, so a corrupted village stayed a shopping destination — walk there,
    /// find no shop to open, walk away, and the planner nominates it again. That is the shape of
    /// every bounce this project has had.
    ///
    /// `completed` is the release, not the corruption flag, for the same reason it is everywhere
    /// else here: corruption is a bill, and clearing the node pays it.
    pub fn stocks_a_heart(&self) -> bool {
        self.is_settlement() && (!self.corrupted || self.completed) && self.trades()
    }

    /// Are this settlement's buildings open for business at all?
    ///
    /// The dev, 2026-08-16: *the inn at a village that is under attack or lost cannot be rested at.*
    /// The same is true of its shop, because `getAreaButtons` swaps the button set for **every**
    /// building in the village at once (`overworld/generators/village.lua:84-98`), and the inn and
    /// the general store carry the same three sets (`:371`, `:393`, `:323`, `:332`).
    ///
    /// Two states, one answer, and the first is the dangerous one:
    ///
    /// - **under attack** — `Enter` still says `Enter`, and opens `ui.building_empty` instead of
    ///   `ui.inn`. A button that looks right and does nothing we want is worse than a missing one,
    ///   because the driver's press succeeds and the errand quietly does not.
    /// - **lost** — only `Loot` remains, which is #26 and not a rest.
    ///
    /// Read from `areaFlags`, which is where the game itself reads it, and drawn on the map as an
    /// attack indicator — so this is the same information a player has.
    pub fn trades(&self) -> bool {
        !self.under_attack && !self.sacked
    }

    /// How many times this building may still be looted, if it is a building that can be. — #26
    ///
    /// **Says nothing about whether the village is destroyed**, which is the other half of the
    /// question and belongs to the parent — see [`WorldMap::loot_here`]. This is the per-node budget
    /// only: which types carry a `Loot` button at all, and how many presses each allows.
    ///
    /// The table is `createLootButton(shopType, icon, iconScale, loot)` with `loot` defaulting to 2
    /// (`utils/world.lua:1317-1332`), read off every call site in the game:
    ///
    /// | type | where | presses |
    /// |---|---|---|
    /// | market stall | `village.lua:284` | 2 |
    /// | general store | `:333` | 2 |
    /// | inn | `:394` | 2 |
    /// | apothecary | `:454` | 2 |
    /// | house | `:491` | **1** |
    /// | chapel | `:549` | 2 |
    ///
    /// **Three more sites exist and are deliberately not here**, because each is reached by a
    /// *different* rule and one of them is not about destruction at all:
    ///
    /// - a **church** on church grounds (`church_ground.lua:176`) keeps its Loot in
    ///   `lootAreaButtons`, chosen by that file's own `getAreaButtons`;
    /// - a **lodge** in an apple orchard (`apple_orchard.lua:248`, 1 press) is destroyed-gated like a
    ///   village but by the orchard's generator;
    /// - a **chapel ruin** in a forest (`forest.lua:266`, 1 press) carries Loot in its ordinary
    ///   `areaButtons`, gated only by `subnodeHasEnemies` (`:86-88`) — so it is lootable on any
    ///   peaceful visit, **destroyed or not**. That is a better grab than this whole entry and it is
    ///   a separate rule; it is written up in #26 rather than guessed at here.
    ///
    /// The well is the trap in this table: it has a `destroyedAreaButtons` (`village.lua:238`) and
    /// that button is `wellWater`, not `Loot`. Enumerating types with destroyed buttons would have
    /// pressed it.
    pub fn loot_left(&self) -> u32 {
        let allowed: u32 = match () {
            _ if self.type_is("house") => 1,
            _ if self.type_is("market stall")
                || self.type_is("general store")
                || self.type_is("inn")
                || self.type_is("apothecary")
                || self.type_is("chapel") => 2,
            _ => return 0,
        };
        allowed.saturating_sub(self.looted)
    }

    /// A village or a town — somewhere with an inn and shops inside.
    ///
    /// **The name every gate should have been using.** All three settlement nouns are the same
    /// *kind* of place to walk into; only the shelf differs.
    ///
    /// Live 2026-08-15, and the dev's word for it was brittleness. The planner's heart filter asked
    /// [`Place::stocks_a_heart`] — village **or town** — while the driver's arrival gate and both
    /// `seeking_*` predicates asked `type_is("village")`. So `l28 Enholmes town` could be chosen and
    /// could never be entered: the run arrived, the gate declined, and the planner immediately
    /// re-picked the next settlement out. `l28 -> l27 (for l44, Heart)`, `l27 -> l28 (for l28,
    /// Heart)`, fourteen times, until the run was stopped by hand.
    ///
    /// That is the third bounce in this project with one shape: **the planner and the driver asking
    /// different questions about the same place.** Any predicate that chooses a destination has to
    /// be the one that greets us on arriving at it.
    pub fn is_settlement(&self) -> bool {
        self.type_is("village") || self.type_is("town")
    }

    /// Is there a subworld behind this place — **whether or not we have ever been inside it**?
    ///
    /// [`Place::subworld_container`] is the older answer and is only ever set from a dump taken
    /// *inside* one, so a village we have not entered this run reads as an ordinary fight. That cost
    /// the run of 2026-08-22 1855Z a step logged `fighting l10` and a wait that sat four seconds
    /// looking for a pregame while we stood inside Ulrome. The heading knew all along.
    ///
    /// ## The nouns, enumerated from the source rather than guessed
    ///
    /// Four files declare a `subworld`, and the surface heading shows `getTypeName`, which is not
    /// always the `typeName`:
    ///
    /// | source | `subworld` | what the heading can read |
    /// |---|---|---|
    /// | `village.lua:72` | `village` | `village`, and `town`/`hamlet` from `getTypeName` (`:6-14`) |
    /// | `in_forest.lua:14` | `forest` | `forest` |
    /// | `shrine_forest_raw.lua:10` | `forest` | `forest`, or `shrine` once revealed |
    /// | `world.lua:19` | `church_ground` | `church` |
    ///
    /// and eight more types are `in_forest{…}` derivatives that inherit the subworld and rename
    /// themselves through `trueTypeName` once `variant ~= 1` — `bandit camp` (`bandits.lua:22,28`),
    /// `graveyard` (`graveyards.lua:17,29`), `mausoleum` (`mausoleums.lua:37,49`) and `spider forest`
    /// (`spider_forests.lua:36,52`). The last needs no entry of its own, ending in `forest` already.
    ///
    /// **`shrine` is deliberately not in the list.** A revealed shrine-forest reads exactly like the
    /// plain `shrine.lua` type, which is not a container — `shrine2` in that same run was `Gransmoor
    /// shrine`, played through `Visit` and a word game. Nothing in the heading separates them, so
    /// this declines rather than guessing, and a revealed shrine-forest stays known only from the
    /// inside, as everything was before.
    ///
    /// ## Why the parent test is not optional
    ///
    /// A forest's *interior* nodes are typed `forest` too, so `shrine1sub4 Gripthorpe Brush forest`
    /// would otherwise read as a container sitting inside another one. Only a surface node can be a
    /// container, and a surface node is one with no parent — a fact the cache round-trips
    /// (`WorldMap::cache_text`), so it survives a restart along with the heading.
    pub fn is_container(&self) -> bool {
        self.subworld_container || self.heading_says_container()
    }

    /// The heading half of [`Place::is_container`], which is where the reasoning lives.
    fn heading_says_container(&self) -> bool {
        const NOUNS: &[&str] =
            &["village", "town", "hamlet", "forest", "bandit camp", "graveyard", "mausoleum", "church"];
        self.parent.is_none() && NOUNS.iter().any(|t| self.type_is(t))
    }

    /// **Somewhere with a bed** — which is all three settlement nouns, task #75.
    ///
    /// Distinct from [`Place::is_settlement`], and the distinction is the whole of #75.
    /// `is_settlement` is a **heart** question and answers `village || town`, because those are the
    /// two nouns that can carry a `healthBuff`. A bed is a different question, and the game answers
    /// it the same way for every settlement it generates:
    ///
    /// ```lua
    /// for i, ttype in ipairs{
    ///     (parentNode.subnodeCount<10 and 'store_market_stall' or 'store_general'),
    ///     'store_inn', 'house', 'house', 'house', 'house', 'house', 'shop_apothecary'
    /// } do
    /// ```
    /// (`overworld/generators/village.lua:684-685`) — unconditional, and *above* the
    /// `specialStock.gearSlotsBuff` branch that adds a town's extra houses and chapel.
    ///
    /// The nouns come from the stock alone (`overworld/locations/village.lua:6-14`): `gearSlotsBuff`
    /// makes a town, neither buff makes a hamlet, otherwise a village. `world.lua:520-529` hands out
    /// the two buffs in **separate passes over the same list**, so they are independent — which is
    /// why a town tells us nothing about its heart, and why no noun tells us anything about its inn.
    ///
    /// So: a hamlet has a bed and never a heart, a village has both, and a town has a bed and
    /// *might* have a heart.
    pub fn has_an_inn(&self) -> bool {
        self.is_settlement() || self.type_is("hamlet")
    }

    pub fn is_general_store(&self) -> bool {
        self.type_is("general store")
    }

    pub fn type_is(&self, type_name: &str) -> bool {
        self.heading.trim_end().ends_with(type_name)
    }

    /// A major shrine — **by key**, the way the game decides it.
    ///
    /// ```lua
    /// if hereData.key:sub(1,6)=='shrine' then
    ///     ...
    ///     hereData.majorShrine = true
    /// ```
    /// `overworld/generators/world.lua:87-90`. The key is the whole test there; the heading plays no
    /// part in it.
    ///
    /// ## Why the heading is not enough, and what trusting it cost
    ///
    /// A [`Place`] learns its fields from two sources with different lifetimes. `completed`,
    /// `corrupted` and `consecrated` come from save flags and so survive across runs. `heading`
    /// comes from an adjacency dump, which only exists for somewhere this run has *seen*.
    ///
    /// So a shrine cleared in an earlier run is rebuilt from its flags alone and comes back
    /// **unheaded** — and [`Place::type_is`] matches on the heading, so it is not a shrine to any
    /// policy that asks. Live 2026-08-14: `shrine1` sat `[done] [corrupted]` and unconsecrated for
    /// two consecutive runs, holding a free consecration nobody could see, because
    /// `type_is("shrine")` was false on an empty string. Fixing the corrupted filter in
    /// [`WorldMap::next_target`] did not help — that filter is downstream of this gate.
    ///
    /// Digits only after the prefix, which is narrower than the game's `sub(1,6)`. A shrine's own
    /// subworld holds nodes keyed `shrine1sub7`, and those are places inside it rather than the
    /// destination — matching them would make the planner target a node whose parent is where it
    /// actually wants to go.
    pub fn is_shrine(&self) -> bool {
        self.type_is("shrine") || key_is_major_shrine(&self.key)
    }

    /// Can this shrine **ever** be consecrated, or is a consecration here impossible?
    ///
    /// `showConsecrateButton` opens with `shrineLocation.majorShrine` (`shrine.lua:93-96`), and
    /// `majorShrine` is set only on overworld nodes whose key starts `shrine`
    /// (`overworld/generators/world.lua:87-90`). A subworld node can still qualify, but only by
    /// **promotion**: `setShrineLocation` (`shrine.lua:420-427`) reassigns `shrineLocation` to the
    /// parent when
    ///
    /// ```lua
    /// if location.parentNode.majorShrine and location.type=='shrine' then
    /// ```
    ///
    /// so the promotion needs the node's own type to be plain `'shrine'`. The plaza qualifies — it
    /// is keyed `<parent>_plaza` and takes the forest's plaza type (`generators/forest.lua:638-640`),
    /// which for a shrine forest is `'shrine'`. A **woodland shrine does not**: it is typed
    /// `'shrine_woodland'` (`:222-224`, `:630-631`), keeps itself as `shrineLocation`, and has no
    /// `majorShrine` of its own. `Consecrate` can therefore never appear at one.
    ///
    /// ## Asked of the key, not the heading, and that is deliberate
    ///
    /// The heading would answer this too — `woodland shrine` is the `typeName` — but headings come
    /// from adjacency dumps and only exist for somewhere this run has seen, which is the same
    /// lifetime trap written up on [`Place::is_shrine`]. A shrine rebuilt from save flags alone
    /// comes back unheaded, and an unheaded woodland shrine would read as consecratable. Keys are
    /// always known, and they fail in neither direction.
    ///
    /// **A woodland shrine is still worth visiting.** `showPrayButton` (`shrine.lua:98-101`) has
    /// `not shrineLocation.majorShrine` as the first arm of its middle clause, so at a minor shrine
    /// that clause is satisfied unconditionally — the blessing is available the moment the word is
    /// won, portal open or shut. It owes a prayer and never a consecration.
    pub fn can_be_consecrated(&self) -> bool {
        key_is_major_shrine(&self.key)
            || self.key.strip_suffix("_plaza").is_some_and(key_is_major_shrine)
    }

    /// Would arriving here open the anomaly?
    ///
    /// Delegated to [`crate::subworld`], which owns what a parent node implies. The remaining
    /// conditions in the event's check — `node_has_no_followups`, the `hell` flag, and a
    /// heretic/blood-curse exclusion — are not properties of a heading, so they are answered by
    /// [`WorldMap::anomaly_is_open`] and by the run itself.
    pub fn triggers_anomaly(&self) -> bool {
        crate::subworld::triggers_anomaly(self.parent.as_deref(), self.remembered_level())
    }
    /// **The level of a fight we would take here on purpose**, or `None` if arriving is not us
    /// choosing to fight.
    ///
    /// This is the trigger for the Well-Rested bank ([`crate::rest::worth_banking_for`]), and the
    /// distinction it draws is the dev's, 2026-08-20:
    ///
    /// > Forests have significantly shorter combat nodes on the path, and we don't currently have
    /// > any code for deliberately clearing spider nests.
    ///
    /// So a forest's level is not a description of a fight we are about to have — it is the depth
    /// of somewhere we walk *through*, one short node at a time, and holding the run at an inn for
    /// sixteen rests before crossing one would be preparation for a fight nobody intends to take.
    /// The three that do describe an intended fight:
    ///
    /// - **The anomaly.** The objective, and the fight the whole rule exists for.
    /// - **A crypt.** The dev's original wording, and the thing that has killed the most runs.
    /// - **A corrupted shrine.** Added on the dev's own second thought: *if we wish to fight at a
    ///   corrupted shrine for whatever reason, perhaps enhanced by rule 1, we do want the "2x
    ///   Well-Rested" rule.* Rule 1 — [`SHRINES_BEFORE_THE_ANOMALY`] consecrations before the anomaly — is what makes that
    ///   reachable, so the two arrived together.
    ///
    /// ## The corrupted shrine's level is assumed, and has to be
    ///
    /// [`Place::level`] parses the heading, and a shrine rebuilt from save flags has none: the
    /// flags survive a restart and the heading does not. That is the same absence recorded at
    /// [`Place::is_shrine`], which cost two runs on 2026-08-14. So an unheaded corrupted shrine
    /// falls back to [`crate::rest::ASSUMED_SHRINE_LEVEL`] rather than dropping out of the rule —
    /// the dev's instruction, and the safe direction, since the alternative is walking into the one
    /// fight we know least about with an empty bank.
    ///
    /// A shrine we *have* seen this run uses its real level, so `Gripthorpe Brush — level 6 shrine`
    /// still asks for twelve rather than fourteen.
    ///
    /// **Corruption is the whole shrine clause.** An uncorrupted shrine costs no fight at all —
    /// that is why it is cheap preparation and outranks the portal — so it has nothing to bank for.
    pub fn deliberate_fight_level(&self) -> Option<u32> {
        if self.completed {
            return None;
        }
        if self.type_is("anomaly") || self.type_is("crypt") {
            return self.remembered_level();
        }
        if self.is_shrine() && self.corrupted {
            return Some(self.remembered_level().unwrap_or(crate::rest::ASSUMED_SHRINE_LEVEL));
        }
        None
    }


    /// ## There was an `opens_the_anomaly` here, and the state it existed for cannot happen
    ///
    /// It was `triggers_anomaly() && !completed`, on the strength of the event's full check —
    /// `locationHasCombat(location) and (location.level or 0) > 3`
    /// (`overworld/events/arrived/world_evil.lua:15-18`), where `locationHasCombat` opens with
    /// `if core.areaIsComplete(location.key) then return false end` (`overworldview.lua:305-310`).
    /// True as a reading of the Lua, and worthless as a rule, because **clearing a level 5 crypt
    /// requires arriving at it, and arriving is what opens the portal**. The event's other gate is
    /// `areaFlag'hell' == 0`, so by the time that crypt is complete the anomaly is open and the only
    /// caller — the pre-anomaly door filter in [`WorldMap::choose_exit`] — has switched itself off.
    ///
    /// The dev, 2026-08-16: *we can't clear a level 5 crypt without first stepping on it, thereby
    /// triggering the anomaly. I don't think it's a scenario that can exist.*
    ///
    /// The case it was justified with does not need it either. Leaving Colden Brake the choice was
    /// `l20` against `l41`, and `l20` is **level 2** — it never triggers, cleared or not.
    ///
    /// Kept as a note because the mistake is worth more than the code was: the test written for it
    /// built a world with `hell = 0` and a completed level 5 crypt, and passed. A green test in an
    /// impossible world is the same error as the surface-chest fixtures two commits earlier, and
    /// both times the code was checked against the Lua and never against the game's reachable
    /// states.

    /// The rules in force where this place sits.
    pub fn rules(&self) -> crate::subworld::Rules {
        crate::subworld::Rules::for_parent(self.parent.as_deref())
    }

    /// Somewhere the map might still open up: unvisited, or visited with neighbours we never saw.
    pub fn is_frontier(&self) -> bool {
        !self.visited || self.hidden.unwrap_or(0) > 0
    }

    /// Standing here cannot show us a node we do not already have.
    ///
    /// True when the edges we know about account for the node's **whole** degree. It is not a
    /// statement about having been there — it is a statement that there is nothing there to find.
    ///
    /// ## Why the two numbers can be compared at all
    ///
    /// `connections` is the true degree, not the visible one. `verboseAdjacencyData` filters *which*
    /// neighbours it prints by `isCloudCovered` and `locationIsVisible`, but the figure it prints
    /// for each is `table.len(location.connections)` (`overworldview.lua:1031-1035`) — the full
    /// count. Under thick fog that distinction is the whole rule: a visibility-filtered count would
    /// under-report and retire nodes that still had neighbours behind the cloud.
    ///
    /// And `neighbours` accumulates from every side. [`WorldMap::fold`] records each edge in both
    /// directions for every node a dump names, so an **unvisited** node collects its edges as the
    /// nodes around it are walked.
    ///
    /// ## What it saves
    ///
    /// Two shapes, and they are the same rule:
    ///
    /// - **A leaf.** Degree 1, and the one neighbour is the node we would arrive from. The run of
    ///   2026-08-09 spent six of twenty-three steps on `e1sub65`, `e1sub67` and `e1sub84`, each
    ///   reported `connections: 1` by every dump that named it.
    /// - **A fork that merges.** Two paths from `A` to `D` whose interior nodes have degree 2. Walk
    ///   one of them and the other's nodes are seen from both ends, completing their degree — so the
    ///   second prong retires instead of being re-walked. In graph terms the two prongs are parallel
    ///   edges between `A` and `D` once degree-2 vertices are smoothed away; this reaches the same
    ///   answer locally, without needing the cycle to be identified, which matters because under fog
    ///   it usually cannot be.
    ///
    /// The saving arrives one step later than it sounds: the second prong only closes when the far
    /// end is reached. Nothing can do better with what we are given — and what we are given was
    /// checked, because the obvious question is why we do not simply read the neighbour's edges.
    ///
    /// ## Why the late close is not a shortcoming: the edges are not published
    ///
    /// A player looking at the screen can see the roads leaving a node they are not standing on, and
    /// judge the fork by eye. That view is not in any data we get:
    ///
    /// - **The console gives exactly one level.** `verboseAdjacencyData`
    ///   (`overworldview.lua:1022-1053`) is the game's only adjacency print, and per neighbour it
    ///   emits key, heading, position and `table.len(location.connections)` — the *count*. Never the
    ///   neighbour's own neighbours. The count is what makes this rule possible at all.
    /// - **The save has no graph.** The only subnode entries in `mainSaveData` are `completedAreas`
    ///   flags. A subworld's `locationData` is generated from a seed at load and never persisted,
    ///   which is also what lets `regenerateMap` re-roll it.
    ///
    /// One other source exists and is deliberately unused. `connectDiscontiguousLocations` prints
    /// `Connecting disconnected nodes <a> <b>` (`utils/world.lua:467`) when generation produces a
    /// split graph and has to bridge it — real named edges, and a live run logged four of them
    /// naming eight nodes it had never stood on. Tempting, and rejected: it is a generation-time
    /// debug line rather than a described interface, it fires only for graphs that happened to come
    /// out disconnected, and in a lost woods it would have to be re-read after every
    /// `regenerateMap`. Building the map's correctness on an incidental print is a brittleness we
    /// would be choosing rather than inheriting. Recorded so the next person can weigh it without
    /// repeating the search.
    ///
    /// ## Three ways it deliberately does not fire
    ///
    /// **Zero connections is silence, not a verdict.** The field defaults to 0 for a place known by
    /// key alone — from `completedAreas`, which on a resume is most of the map.
    ///
    /// **A subworld container is never retired.** Its edges are surface roads; what it has to offer
    /// is an interior, and no count of neighbours describes that.
    ///
    /// **Reveals nothing is not offers nothing, and a chest is the case that proves it.** A leaf's
    /// only neighbour is the one you arrive from, so entering costs two steps for nothing — right
    /// for exploring, wrong for loot. Task #16, landed 2026-08-16: an unopened chest is never
    /// retired, however little map it has left to give.
    ///
    /// Only while it is **unopened**. `getAreaButtons` offers `Open` on an incomplete chest and
    /// nothing at all on a complete one (`overworld/generators/forest.lua:186-188`), so a chest we
    /// have already emptied really is a leaf with nothing behind it, and walking back to it is the
    /// bounce this whole field exists to prevent.
    pub fn nothing_left_to_reveal(&self) -> bool {
        if self.is_chest() && !self.completed {
            return false;
        }
        // [`Place::is_container`] and not `subworld_container`: a village has an interior whether or
        // not we have stood in it, so counting its surface neighbours can never mean there is
        // nothing left to see. The narrower flag made every unvisited village look exhausted.
        !self.is_container()
            && self.connections > 0
            && self.neighbours.len() as u32 >= self.connections
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overworld::fixtures::*;
    use crate::overworld::WorldMap;

    /// **A village is a container before we have ever been inside one**, which is the whole point.
    ///
    /// Every heading here is verbatim from a run report. `l10`'s is the one that was logged as
    /// `fighting` on 2026-08-22 while the press it produced walked us into Ulrome.
    #[test]
    fn a_heading_says_whether_there_is_a_subworld_behind_it() {
        let surface = |heading: &str| Place { heading: heading.into(), ..Default::default() };
        let inside = |heading: &str, parent: &str| {
            Place { heading: heading.into(), parent: Some(parent.into()), ..Default::default() }
        };

        // The four `subworld` declarations, in every name their heading can take.
        assert!(surface("Ulrome — level 6 village").is_container());
        assert!(surface("Dane town").is_container());
        assert!(surface("Kexby hamlet").is_container());
        assert!(surface("Bainton Clump — level 1 forest").is_container());
        assert!(surface("Ottringham church").is_container());
        // The `in_forest{…}` derivatives, which rename themselves once seen.
        assert!(surface("Cottam Boscage — level 2 mausoleum").is_container());
        assert!(surface("Riccall — level 3 graveyard").is_container());
        assert!(surface("Skerne — level 4 bandit camp").is_container());
        assert!(surface("Bainton Clump — level 1 spider forest").is_container());

        // Not containers: an ordinary fight, and the plain shrine type.
        assert!(!surface("Weedley Copse — level 0 crypt").is_container());
        assert!(!surface("Rayslack — level 4 crypt").is_container());
        assert!(!surface("Firby crossroads").is_container());
        // **The ambiguity this declines to guess at.** A revealed shrine-forest reads the same, and
        // is left to the proved-from-inside flag rather than swept in with the plain type.
        assert!(!surface("Gransmoor shrine").is_container());

        // **Interior nodes are typed like their container and must not be mistaken for one.**
        assert!(!inside("Gripthorpe Brush forest", "shrine1").is_container());
        assert!(!inside("Ulrome — level 6 house", "l10").is_container());

        // The old flag still wins on its own: proved from inside beats any heading.
        let proved = Place { subworld_container: true, ..surface("Weedley Copse — level 0 crypt") };
        assert!(proved.is_container());
    }

    /// A container always has an interior we have not enumerated, so its surface neighbour count
    /// can never mean it is exhausted. Before this, every unvisited village looked fully revealed.
    #[test]
    fn a_village_we_have_never_entered_is_not_a_place_with_nothing_left() {
        let mut village = Place {
            heading: "Ulrome — level 6 village".into(),
            connections: 2,
            ..Default::default()
        };
        village.neighbours.insert("l7".into());
        village.neighbours.insert("l18".into());
        assert!(!village.subworld_container, "the point is that nothing has told us from inside");
        assert!(!village.nothing_left_to_reveal());

        // A crypt with the same shape is exhausted, which is what keeps this from being vacuous.
        let crypt = Place { heading: "Weedley Copse — level 0 crypt".into(), ..village.clone() };
        assert!(crypt.nothing_left_to_reveal());
    }

    /// **What counts as a fight we take on purpose**, which is the dev's forest correction.
    #[test]
    fn a_forest_is_crossed_and_a_crypt_is_fought() {
        let at = |heading: &str| Place { heading: heading.into(), ..Default::default() };

        assert_eq!(at("Riccall — level 6 crypt").deliberate_fight_level(), Some(6));
        assert_eq!(at("The Rift — level 8 anomaly").deliberate_fight_level(), Some(8));

        // The dev, 2026-08-20: *forests have significantly shorter combat nodes on the path, and we
        // don't currently have any code for deliberately clearing spider nests.* `shrine7`, the
        // level 9 forest a run walked into on 2026-08-17, is the live example — deep, and never a
        // fight we chose.
        assert_eq!(at("Cottam Boscage — level 9 forest").deliberate_fight_level(), None);
        assert_eq!(at("Bursall Hedge — level 3 spider forest").deliberate_fight_level(), None);
        assert_eq!(at("Rowlston Covert village").deliberate_fight_level(), None);

        // A cleared crypt is not a fight at all any more.
        let done =
            Place { heading: "Riccall — level 6 crypt".into(), completed: true, ..Default::default() };
        assert_eq!(done.deliberate_fight_level(), None);
    }

    /// A corrupted shrine, including the one whose heading a restart threw away.
    #[test]
    fn a_corrupted_shrine_with_no_heading_is_assumed_to_be_level_seven() {
        let seen = Place {
            key: "shrine1".into(),
            heading: "Gripthorpe Brush — level 6 shrine".into(),
            corrupted: true,
            ..Default::default()
        };
        assert_eq!(seen.deliberate_fight_level(), Some(6), "a heading we have beats any assumption");

        // The state a resumed run actually holds: flags survive, the heading does not. This is the
        // absence recorded at `Place::is_shrine`, which cost two runs on 2026-08-14.
        let remembered = Place { key: "shrine1".into(), corrupted: true, ..Default::default() };
        assert_eq!(remembered.heading, "", "the fixture must really be unheaded");
        assert_eq!(remembered.level(), None, "so nothing can read a level off it");
        assert_eq!(
            remembered.deliberate_fight_level(),
            Some(crate::rest::ASSUMED_SHRINE_LEVEL),
            "the dev's instruction: assume level 7"
        );

        // **Corruption is the whole clause.** A clean shrine costs no fight, which is why it is the
        // cheap preparation that outranks the portal.
        let clean = Place { key: "shrine1".into(), ..Default::default() };
        assert_eq!(clean.deliberate_fight_level(), None);
    }

    /// A woodland shrine is a shrine that can never be consecrated, and the run must not go back for
    /// one.
    ///
    /// Pinned because the rule lives in the *game*, three files from anything we control, and
    /// nothing else here would notice it changing: `showConsecrateButton` needs
    /// `shrineLocation.majorShrine` (`shrine.lua:93-96`), which a `shrine_woodland` never has and
    /// never gets by promotion (`:420-427`).
    ///
    /// The plaza is the case that makes this more than a prefix test. It is a subworld node, so it
    /// has no `majorShrine` of its own, and it is consecratable anyway — its type is plain `'shrine'`
    /// (`generators/forest.lua:638-640`) so `setShrineLocation` hands the flag to its parent. Get
    /// this arm wrong in the safe-looking direction and the run stops walking back for consecrations
    /// it *can* claim.
    #[test]
    fn a_woodland_shrine_can_never_be_consecrated_but_a_plaza_can() {
        let consecratable = |key: &str, heading: &str| {
            let mut p = Place::default();
            p.key = key.into();
            p.heading = heading.into();
            assert!(p.is_shrine(), "`{key}` has to be a shrine at all for this to mean anything");
            p.can_be_consecrated()
        };
        assert!(consecratable("shrine1", "Gripthorpe Brush shrine"), "the overworld node itself");
        assert!(consecratable("shrine1_plaza", "Gripthorpe Brush shrine"), "promoted to its parent");
        assert!(
            !consecratable("shrine1sub1", "Gripthorpe Brush woodland shrine"),
            "a woodland shrine inside a shrine's own forest — the one the run of 2026-08-17 stopped at"
        );
        assert!(
            !consecratable("l4sub24", "Bainton Clump woodland shrine"),
            "and the same node type in an ordinary forest, which is where most of them are"
        );

        // The heading is what `is_shrine` leans on, and it is the field that goes missing when a
        // place is rebuilt from save flags alone. The key still has to carry the answer.
        let mut unheaded = Place::default();
        unheaded.key = "shrine1sub1".into();
        assert!(
            !unheaded.can_be_consecrated(),
            "an unheaded woodland shrine must not read as consecratable"
        );
        let mut rebuilt = Place::default();
        rebuilt.key = "shrine2".into();
        assert!(rebuilt.can_be_consecrated(), "and an unheaded major shrine must still read as one");
    }

    /// The heading alone answers "does arriving cost a fight", because the game builds it that way.
    #[test]
    fn a_missing_level_in_a_heading_is_the_games_own_all_clear() {
        let free = |heading: &str| Place { heading: heading.into(), ..Default::default() };
        // `AreaHeading` omits the level exactly when `locationHasCombat` is false
        // (`overworldview.lua:383-392`). Real headings, and the two that matter most:
        assert_eq!(free("Gembling shrine").arrival(), Arrival::Free, "uncorrupted shrine");
        assert_eq!(free("Wetwang wizards' tower").arrival(), Arrival::Free, "competeOnVisit = true");
        assert_eq!(free("Cottam campfire").arrival(), Arrival::Free);
        assert_eq!(free("Ulrome village").arrival(), Arrival::Free);
        // And carries it exactly when a fight is owed.
        assert_eq!(
            free("Burtonfields — level 6 crypt").arrival(),
            Arrival::Fight { level: 6 },
            "the node the last live run walked onto at 1/20"
        );
        assert_eq!(free("Bainton Coppice — level 5 forest").arrival(), Arrival::Fight { level: 5 });
    }

    #[test]
    fn a_place_we_have_never_seen_a_heading_for_is_not_safe() {
        // The trap this was written to close. Eleven of the twenty-two places in the last live run's
        // map were `(unheaded)` -- known by key from `completedAreas` or an `areaFlags` suffix, never
        // seen in a dump. An empty heading has no `— level N` in it, so a plain boolean test read
        // every one of them as free to walk onto.
        let unseen = Place { key: "l1".into(), ..Default::default() };
        assert_eq!(unseen.arrival(), Arrival::Unknown);
        assert!(!unseen.arrival().is_free(), "unseen is not safe");
        // And it is eligible to be chosen, which is what makes the distinction matter rather than
        // being pedantry: never visited, so it counts as a frontier.
        assert!(unseen.is_frontier());

        // Completion is checked first, so a cleared node is free even if its heading is stale or
        // was never read -- mirroring `locationHasCombat`'s own short-circuit (`:306-308`).
        let done = Place { completed: true, ..unseen.clone() };
        assert_eq!(done.arrival(), Arrival::Free);
    }

    #[test]
    fn the_two_danger_questions_are_independent() {
        // The conflation that caused the bug, stated as a table. A crypt cannot trap anyone -- it is
        // not a subworld -- but it always fights. An unvisited forest may trap us and may not fight.
        let crypt = Place {
            heading: "Burtonfields — level 6 crypt".into(),
            visited: true,
            ..Default::default()
        };
        assert!(!crypt.hostile_to_enter(), "nothing to be held inside");
        assert!(!crypt.arrival().is_free(), "but a fight is compulsory");

        let forest =
            Place { heading: "Bainton Coppice — level 5 forest".into(), ..Default::default() };
        assert!(forest.hostile_to_enter(), "might be one of the three bandit camps");
        assert!(!forest.arrival().is_free());

        let shrine = Place { heading: "Gembling shrine".into(), ..Default::default() };
        assert!(!shrine.hostile_to_enter());
        assert!(shrine.arrival().is_free(), "free on both axes: somewhere a hurt run can go");
    }

    #[test]
    fn a_forest_outranks_a_crypt_when_everything_costs_a_fight() {
        // The dev's ranking, and the one place it changes an outcome. Both are level 6, so the old
        // level-only sort split them by key -- `c1` before `f1`, the wrong way round.
        let crypt = Place { key: "c1".into(), heading: "Yokefleet — level 6 crypt".into(), ..Default::default() };
        let forest = Place { key: "f1".into(), heading: "Asselby Bush — level 6 forest".into(), ..Default::default() };
        assert!(forest.risk() < crypt.risk());

        // The full order, safest first. `Unseen` sits below the known fights because being unable to
        // show something is safe is worse than knowing its price.
        assert!(Risk::Free < Risk::Forest);
        assert!(Risk::Fight < Risk::Unseen);
        assert!(Risk::Unseen < Risk::Corrupt);

        // Corruption is read ahead of the heading, because it rewrites the level upward without
        // touching the type name (`world.lua:499-502`).
        let corrupt_forest = Place { corrupted: true, ..forest.clone() };
        assert_eq!(corrupt_forest.risk(), Risk::Corrupt, "still says 'forest'; is not one any more");
        // Cleared is free on every axis, corruption included.
        assert_eq!(Place { completed: true, ..corrupt_forest }.risk(), Risk::Free);
    }

    /// An unvisited **forest** is hostile, because a bandit camp is one until you stand in it
    /// (`overworld/generators/world.lua:466-475`). An unvisited village is not — `banditos` maps
    /// only `pine_forest` and `oak_forest`, and treating every container as hostile would block
    /// resting at the first quiet village we find.
    #[test]
    fn an_unvisited_forest_is_hostile_but_an_unvisited_village_is_not() {
        let forest = Place {
            key: "f1".into(),
            heading: "Bainton Clump — level 1 forest".into(),
            ..Default::default()
        };
        assert!(forest.hostile_to_enter(), "could be a bandit camp; nothing outside it says so");

        let village = Place {
            key: "v1".into(),
            heading: "Ulrome — level 6 village".into(),
            ..Default::default()
        };
        assert!(!village.hostile_to_enter(), "somewhere to REST, which is the whole point");

        let visited = Place { visited: true, ..forest.clone() };
        assert!(!visited.hostile_to_enter(), "we have stood in it and come back out");

        let cleared = Place { completed: true, corrupted: true, ..forest.clone() };
        assert!(!cleared.hostile_to_enter(), "corruption fought off is corruption gone");
    }

    /// The container's type name is re-read on every dump, because the game re-evaluates it.
    ///
    /// `getTypeName` (`lost_woods.lua:29`) returns `'forest'` until `lost_woods_known_<key>` is set
    /// and `'lost woods'` after, so the two headings are the same place at different times. Keeping
    /// the first one meant the run never learned where it was — the report it printed on the way out
    /// still called `e1` a forest.
    #[test]
    fn walking_into_a_lost_woods_updates_what_we_call_it() {
        let m = a_lost_woods();
        let e1 = m.get("e1").expect("container");
        assert!(e1.type_is("lost woods"), "the fresher heading wins, got {:?}", e1.heading);
        assert!(e1.in_lost_woods);
        for k in ["e1_plaza", "e1sub1", "e1sub2", "e1sub3"] {
            assert!(m.get(k).unwrap().in_lost_woods, "{k} is inside it");
        }
    }

    /// A disguised road must not be read as a possible bandit camp.
    ///
    /// `e1sub2` prints `forest` because the generator renamed it, not because anything is hiding in
    /// it. Left alone, every unvisited node in the woods owes a fight on the `hostile_to_enter` axis
    /// and a hurt run has nowhere it is willing to step.
    #[test]
    fn the_lost_woods_interior_is_not_a_bandit_camp() {
        let m = a_lost_woods();
        let disguised = m.get("e1sub2").unwrap();
        assert!(disguised.type_is("forest"), "this is what the dump says");
        assert!(!disguised.hostile_to_enter(), "renamed road, not a camp");

        // And the rule it is exempted from still holds everywhere else: `l12`'s neighbour `e1` is a
        // surface forest we have not entered, which is exactly the gamble the rule prices.
        let mut surface = WorldMap::new();
        surface.fold(&dump("l12", "Standing — level 2 crypt",
            vec![node("e1", "Howden Timberland — level 2 forest")]));
        assert!(surface.get("e1").unwrap().hostile_to_enter(), "an unentered forest still might be");
    }

    /// **A chest at a dead end is still worth the two steps.** Task #16.
    ///
    /// The leaf rule is right for exploring and wrong for loot: a leaf's only neighbour is the one
    /// you arrive from, so it reveals nothing — but "nothing to reveal" is not "nothing to gain",
    /// and a chest sits at a leaf often enough. The task named this exception when the rule was
    /// written; this is it landing.
    ///
    /// Only while unopened, which is the other half. `getAreaButtons` offers `Open` on an incomplete
    /// chest and nothing at all once it is done (`overworld/generators/forest.lua:186-188`), so an
    /// emptied chest really is a leaf with nothing behind it — and walking back to it is exactly the
    /// bounce the leaf rule exists to prevent.
    #[test]
    fn an_unopened_chest_is_worth_a_detour_that_reveals_nothing() {
        let leaf = |heading: &str, done: bool| {
            let mut p = Place { heading: heading.into(), ..Place::default() };
            p.connections = 1;
            p.neighbours.insert("here".into());
            p.completed = done;
            p
        };

        // The control: an ordinary leaf is still skipped, or this proves only that the rule is gone.
        assert!(
            leaf("Fangfoss grave", false).nothing_left_to_reveal(),
            "an ordinary dead end reveals nothing and is not a destination"
        );
        assert!(
            !leaf("Riccall chest", false).nothing_left_to_reveal(),
            "an unopened chest is worth the two steps whatever it reveals"
        );
        assert!(
            leaf("Riccall chest", true).nothing_left_to_reveal(),
            "and an emptied one is a leaf again — walking back is the bounce this prevents"
        );
    }

    /// A town is a settlement, and both halves of the program have to agree about that.
    ///
    /// The `l28 <-> l27` bounce of 2026-08-15, in the smallest form that reproduces it. The planner
    /// chose `l28 Enholmes town` because [`Place::stocks_a_heart`] counts towns; the driver's arrival
    /// gate asked `type_is("village")` and declined; the planner re-picked the settlement beyond it;
    /// and the run walked back and forth fourteen times until it was stopped by hand.
    ///
    /// This pins the predicate the two now share. The driver's own gate cannot be reached from a
    /// test — it needs a live game — so what is pinned here is that there is one question rather
    /// than two, which is the part that was wrong.
    #[test]
    fn a_town_is_a_settlement_everywhere_that_asks() {
        let town = Place { heading: "Enholmes town".into(), ..Default::default() };
        let village = Place { heading: "Rowlston Covert village".into(), ..Default::default() };
        let hamlet = Place { heading: "Wetwang hamlet".into(), ..Default::default() };
        let forest = Place { heading: "Bursall Hedge — level 2 forest".into(), ..Default::default() };
        for p in [&town, &village] {
            assert!(p.is_settlement(), "{} is somewhere to walk into", p.heading);
            assert_eq!(p.stocks_a_heart(), p.is_settlement(), "one question, not two");
        }
        // A hamlet has neither buff (`village.lua:5-14`), so it is not a heart destination. It is
        // still not a *fight*, which is a separate axis and not this predicate's business.
        assert!(!hamlet.is_settlement());
        assert!(!forest.is_settlement());
    }

    #[test]
    fn a_shrine_known_only_from_the_save_is_still_a_shrine() {
        // The state both runs of 2026-08-14 held: `shrine1` cleared and corrupted in an earlier run,
        // rebuilt this run from save flags alone, and so unheaded. `type_is` reads the heading, and
        // an empty heading ends with nothing — so the free consecration was invisible.
        let mut m = WorldMap::new();
        let mut d = dump("here", "camp", vec![node("l2", "Quiet Glade meadow")]);
        d.hidden = 1;
        m.fold(&d);
        m.hell = Some(0.1);
        {
            let p = m.entry("shrine1");
            p.corrupted = true;
            p.completed = true;
        }
        assert_eq!(m.entry("shrine1").heading, "", "the premise: nothing has seen it this run");
        assert!(m.entry("shrine1").is_shrine(), "the game reads the key, not the heading");
        assert!(m.worth_consecrating_here("shrine1"));

        // A node *inside* a shrine's subworld is not the shrine. Targeting one would send the
        // planner at a place whose parent is where it wanted to be.
        assert!(!m.entry("shrine1sub7").is_shrine());
        // And the heading still speaks when it is there — this widens the test, it does not move it.
        assert!(m.entry("l40").is_shrine() == false);
        let seen = m.entry("q1");
        seen.heading = "Foggathorpe shrine".into();
        assert!(seen.is_shrine(), "an unkeyed shrine is still named by its heading");
    }

    /// **Corruption does not make a fight out of somewhere that completes on visit.** Task #93.
    ///
    /// The dev's parenthetical — *excluding a crossroads that may involve a tree* — read against
    /// `core.locationHasCombat`, which is `not complete and not completeOnVisit`
    /// (`overworldview.lua:305-310`). Corruption is `setAreaIncomplete` and moves only the first
    /// half, so on a `competeOnVisit = true` type it takes completion away and arrival gives it
    /// straight back. `|| self.corrupted` used to answer for those types anyway.
    ///
    /// The four cases below are the whole rule: the literal set, the function set, and the parent
    /// gate that separates them — because the subworld generators reuse two of the same nouns for
    /// subnodes whose `competeOnVisit` asks whether enemies are standing there.
    #[test]
    fn a_corrupted_crossroads_is_not_a_fight_but_a_corrupted_village_is() {
        let mut m = WorldMap::new();
        m.fold(&dump(
            "here",
            "camp",
            vec![
                node("xrd", "Trenwick crossroads"),
                node("l5", "Dalton Copse village"),
                node("fire", "Emswell campfire"),
            ],
        ));
        for k in ["xrd", "l5", "fire"] {
            m.entry(k).corrupted = true;
        }

        // The literal `competeOnVisit = true` set: corruption says nothing about them.
        assert!(m.entry("xrd").completes_on_visit(), "crossroads.lua:6");
        assert!(!m.entry("xrd").may_be_a_fight(), "the dev's parenthetical, and the whole of #93");
        assert!(!m.entry("fire").may_be_a_fight(), "campfire.lua:19, the same rule");

        // The function forms read `location.corrupt` themselves, so for them corruption *is* the
        // fight — `locations/village.lua:63-70`. This is the half that must not move.
        assert!(!m.entry("l5").completes_on_visit());
        assert!(m.entry("l5").may_be_a_fight(), "a corrupted village is exactly what it looks like");

        // **The parent gate**, which is not caution: `generators/forest.lua:103-131` gives its own
        // crossroads `competeOnVisit = subnodeIsPeaceful`, and an interior one can print a level a
        // surface one never can. Both of these are real cache headings.
        let inside = m.entry("l4_plaza");
        inside.heading = "Bainton Clump — level 6 crossroads".into();
        inside.parent = Some("l4".into());
        assert!(!m.entry("l4_plaza").completes_on_visit(), "it has a parent, so it is a subnode");
        assert!(m.entry("l4_plaza").may_be_a_fight(), "and the level says so outright");

        // And without the gate the tail match alone would clear it, which is the fault being
        // guarded against rather than a hypothetical: `type_is` reads the last word.
        let quiet = m.entry("l4sub2");
        quiet.heading = "Bainton Clump crossroads".into();
        quiet.parent = Some("l4".into());
        quiet.corrupted = true;
        assert!(!m.entry("l4sub2").completes_on_visit());
        assert!(m.entry("l4sub2").may_be_a_fight(), "a tree may be standing on it");
    }
}
