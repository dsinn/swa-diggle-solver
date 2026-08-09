//! The map Diggle builds for itself, and where to go next.
//!
//! ## Why we accumulate one at all
//!
//! Nothing hands us the world. `mainSaveData.overworld` carries only `playerLocation`, `seed`,
//! `completedAreas`, `areaFlags` and friends — the map itself is generated at runtime from the seed
//! into in-memory `locationData`, so there is no graph on disk to read. Regenerating it from the
//! seed was considered and rejected: the world generator is among the highest-churn parts of the
//! game, and mirroring it would break on every change.
//!
//! What we get instead is `verboseAdjacencyData` (`overworldview.lua:1022-1053`) — the current node,
//! its *visible* neighbours, the subworld parent, and the subworld exits. One hop, and only when it
//! fires: on pan-finish (`:1255`), arrival (`:1442`), or world-load (`:1607`). `overworldview` has
//! no `onActive` hook, so simply returning to the map announces nothing. Folding those dumps
//! together as we travel is the only way to know what exists.
//!
//! ## What we deliberately do not know
//!
//! Cloud-covered and not-yet-visible neighbours print as `Hidden location` — a count, never an
//! identity ([`Adjacency::hidden`]). We keep the count and nothing else, because a hidden node
//! cannot be travelled to anyway, so knowing its name would be knowledge the player does not have
//! and cannot use. A node with hidden neighbours stays a **frontier**: "there is no shrine adjacent"
//! is never a conclusion while that count is nonzero.
//!
//! ## Pathfinding is the game's job, not ours
//!
//! `canTravelToIndirect` (`:1330`) breadth-firsts the whole connection graph from the player, and
//! the `Travel` button's `activeIf` *is* that function (`:395-405`). So a route is: select the
//! destination, read whether Travel is active, click it. We never path-find, and a dead end is not a
//! trap — any reachable known node is one Travel away. This map exists to choose *where*, not *how*.
//!
//! ## Positions are not identity
//!
//! The dump prints `xoffset + location.posX*zoomMult` — screen coordinates under the current pan and
//! zoom. They are aimable *now* and meaningless after the view moves, so they are stored with the
//! dump that produced them and never used to identify a node. The `key` is the identity.

use crate::observe::adjacency::Adjacency;
use std::collections::{BTreeMap, BTreeSet};

/// What we know about one location.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Place {
    pub key: String,
    /// `AreaHeading` output. Carries the level and type for combat nodes.
    pub heading: String,
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
    /// Have we stood on it and taken a dump?
    pub visited: bool,
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
    /// A lost woods we have already been swallowed by, and will not enter again.
    ///
    /// Set from the `lost_woods_known_*` save flags — see [`crate::subworld::LOST_WOODS_KNOWN`].
    /// Routing treats these as walls and only passes through one if there is no other way at all.
    pub avoid: bool,
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
        match self.level() {
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
    pub fn hostile_to_enter(&self) -> bool {
        if self.completed {
            return false;
        }
        self.corrupted || (self.type_is("forest") && !self.visited)
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

    pub fn type_is(&self, type_name: &str) -> bool {
        self.heading.trim_end().ends_with(type_name)
    }

    /// Would arriving here open the anomaly?
    ///
    /// Delegated to [`crate::subworld`], which owns what a parent node implies. The remaining
    /// conditions in the event's check — `node_has_no_followups`, the `hell` flag, and a
    /// heretic/blood-curse exclusion — are not properties of a heading, so they are answered by
    /// [`WorldMap::anomaly_available`] and by the run itself.
    pub fn triggers_anomaly(&self) -> bool {
        crate::subworld::triggers_anomaly(self.parent.as_deref(), self.level())
    }

    /// The rules in force where this place sits.
    pub fn rules(&self) -> crate::subworld::Rules {
        crate::subworld::Rules::for_parent(self.parent.as_deref())
    }

    /// Somewhere the map might still open up: unvisited, or visited with neighbours we never saw.
    pub fn is_frontier(&self) -> bool {
        !self.visited || self.hidden.unwrap_or(0) > 0
    }
}

/// Everything we have folded together, plus the run state that decides routing.
#[derive(Debug, Default)]
pub struct WorldMap {
    places: BTreeMap<String, Place>,
    /// Places the driver has had its one go at and will not enter again this run.
    ///
    /// Distinct from `Place::used`, which is the *game's* record of a completed interaction. A shrine
    /// whose puzzle could not be read is untouched as far as the game is concerned, so `used` stays
    /// false and it remains a legitimate destination forever. This is the driver's own memory of
    /// having tried, and it exists because the planner and the caller must not disagree about what is
    /// still worth walking to — when they did, a run spent thirty steps crossing the same crypt.
    abandoned: std::collections::HashSet<String>,
    /// Where the last dump said we were.
    here: Option<String>,
    /// `areaFlags.hell` — zero means the anomaly has not opened and the trigger is still live.
    /// A **float**, and that matters: `hellOpens` sets it to `0.1`
    /// (`utils/events.lua:39`, `setHellValue(0.1)`) and it grows from there. Read as an integer it
    /// parses as nothing at all, `anomaly_available` falls back to "still available", and the run
    /// sets off to trigger an anomaly that is already open.
    hell: Option<f64>,
    /// Set when a node cost us [`crate::rest::REST_THRESHOLD`] or more health, cleared once we are
    /// topped up. Held on the map because the decision is "where to go next", which is its job.
    wants_rest: bool,
    /// `player.gold` — an inn will not serve us below [`crate::rest::INN_COST`].
    gold: i64,
    /// Campfire fuel carried, which makes a campfire usable even at a used area.
    fuel: i64,
    /// The surface node we were standing on when we entered the subworld we are in.
    ///
    /// `None` on the surface. Its one job is to let [`WorldMap::exit_toward`] recognise the entrance,
    /// because leaving a village by the door you came in is a retreat, and nothing visible inside
    /// distinguishes that road from any other.
    entered_from: Option<String>,
}

/// Where to head, and why. The reason is carried so a route can be explained rather than just taken.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub target: String,
    pub reason: Goal,
}

/// Where the anomaly is, once it opens: the node we started on.
///
/// Not a guess and not something to go looking for. `overworld/generators/world.lua:492-507` runs
/// when `areaFlag'hell' ~= 0` and does, in as many words:
///
/// ```lua
/// locationData.start.corrupt = true
/// locationData.start.type = 'portal'
/// locationData.start.level = 8
/// ```
///
/// So the portal *is* `start`, promoted in place — which is also why the objective was always
/// `areaIsComplete'start_corrupt'`: beating the anomaly means completing the corrupted `start`.
/// It is reached by a path we have necessarily already walked, since the run began there.
///
/// **A prior, not a law.** That is one generator; other worlds and start situations need not put
/// the portal there. [`WorldMap::anomaly`] prefers a seen `anomaly` heading over this whenever it
/// has one.
pub const ANOMALY_KEY: &str = "start";

/// `completedAreas` entry that means the anomaly has been beaten — the run's whole objective.
///
/// `setAreaComplete` writes `completedAreas[key..'_corrupt'] = loc.corrupt or nil`
/// (`overworldview.lua:172`), so finishing the corrupted `start` sets exactly this.
/// Kept as documentation of the game's own check rather than used directly: we read the
/// `_corrupt` suffix generically into [`Place::completed_corrupt`], so the objective is asked of
/// whichever node turns out to be the portal.
pub const ANOMALY_BEATEN_KEY: &str = "start_corrupt";

/// Hops to `key`, or [`usize::MAX`] when no known route reaches it — so unreachable places sort
/// last rather than first.
fn dist_or_far(dist: &BTreeMap<String, usize>, key: &str) -> usize {
    dist.get(key).copied().unwrap_or(usize::MAX)
}

/// The key of the road node inside `parent` that leads out to `to`.
///
/// Built the same way the game builds it (`overworldview.lua:1043`), so it needs no lookup and works
/// for exits we have not yet seen.
pub fn exit_node_key(parent: &str, to: &str) -> String {
    format!("{parent}_path_to_{to}")
}

/// Does this heading use the combat form, `<name> — level <n> <type>`?
///
/// The em-dash is the marker (`overworldview.lua:388-389`), which is why the console codepage
/// matters — see [`crate::observe::log`].
fn heading_has_combat(heading: &str) -> bool {
    heading.contains("— level ")
}

/// What to do next while crossing a subworld.
///
/// Usually toward its exit — but not always. A village is entered *for* something inside it, and
/// [`Crossing::Arrive`] and [`Crossing::Seek`] are the two states that has which crossing a forest
/// does not: standing on the thing we came for, and not yet having found it.
#[derive(Debug, Clone, PartialEq)]
pub enum Crossing {
    /// The node underfoot has an unfinished fight, so nothing else is legal yet.
    Fight { at: String },
    /// Move to this adjacent node, which is on the known route to `toward`.
    Step { to: String, toward: String },
    /// No route is known yet. Move here to learn more of the interior.
    Explore { to: String, toward: String },
    /// Standing on the interior destination — the inn — with the errand still to do.
    ///
    /// The counterpart of [`Crossing::Leave`] for a destination that is *inside* rather than out.
    Arrive { at: String },
    /// Looking for a destination we have not seen yet. Move here to open the fog.
    ///
    /// Distinct from [`Crossing::Explore`], which knows where it is going and is only short of a
    /// route. This one has no target at all, so it must not be told to head for an exit: leaving is
    /// precisely the thing that would abandon the errand. Its own variant rather than a flag,
    /// because `Step` and `Explore` already log identically and a third silent case would make an
    /// old log unreadable.
    Seek { to: String },
    /// Standing on the exit road: leave for this overworld node.
    Leave { to: String },
    /// Hurt, and the way onward is a fight — so go back the way we came instead.
    ///
    /// Legal for the same reason [`WorldMap::can_step`] documents and [`WorldMap::blocks_departure`]
    /// forgot: `canTravelToDirect` needs **one** endpoint complete, and the node behind us is
    /// complete by definition, since we walked through it. An unfinished fight blocks going onward,
    /// never going back.
    Retreat { to: String },
}

/// One move: the adjacent node to travel to, and the plan it serves.
#[derive(Debug, Clone, PartialEq)]
pub struct Hop {
    pub step: String,
    pub plan: Plan,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Goal {
    /// The anomaly is open and this is it. Everything else can wait.
    Anomaly,
    /// A level > 3 surface node: arriving opens the anomaly.
    OpenTheAnomaly,
    /// Health is down and somewhere nearby can restore it.
    Rest,
    /// A shrine we have not consecrated.
    ///
    /// Ranked below [`Goal::OpenTheAnomaly`] because consecrating is *impossible* until the anomaly
    /// is open — `showConsecrateButton` requires `hell ~= 0` (`shrine.lua:93-96`). Once it is open,
    /// corruption resets areas to incomplete, so a shrine needing a fight only earns a detour if it
    /// already lies on the route; one that is merely awaiting a `Visit` stays cheap.
    ///
    /// ## An UNCORRUPTED shrine is strictly beneficial, and that is worth stating
    ///
    /// It costs no fight, and the payoff is combat-relevant. The full chain, verified in source:
    ///
    /// ```text
    ///   ShowAGoodButton      hasWon() and not heretic            shrine.lua:36-40
    ///   Consecrate           + majorShrine + hell ~= 0           shrine.lua:92-95
    ///   Pray                 + areaUnused + (hell == 0 or consecrated or desecrated)
    ///                                                            shrine.lua:98-103
    ///   -> wildcardRewards   queues 3 gold-bordered, 3 silver,   utils/blessings.lua:95-110
    ///                        or 1 gold WILDCARD tile
    /// ```
    ///
    /// **Every shrine is a major one.** `overworld/generators/world.lua:86-89` sets
    /// `majorShrine = true` for any location whose key starts with `shrine`, so there is no minor
    /// shrine to pray at without consecrating first. With the anomaly already open — `hell ~= 0` —
    /// that ordering is satisfied, and solve → Consecrate → Pray runs straight through.
    ///
    /// Wildcards are the mechanic Diggle handles best and has exercised live. So an uncorrupted
    /// shrine is the cheapest way to make the *next* fight winnable, which is precisely what a run
    /// at low health needs — and why this outranks [`Goal::EasiestHostile`] rather than being a
    /// luxury deferred until healthy.
    Shrine,
    /// Somewhere unseen, to grow the map toward one of the above.
    Explore,
    /// Hurt, and **everywhere** is hostile — so take the cheapest fight on the map.
    ///
    /// The last resort of [`WorldMap::next_target`]'s first pass. Distinct from the goals above
    /// because it is not pursuing any of them: it is choosing where to bleed. Carries the level so
    /// a caller can log what it is walking into.
    EasiestHostile { level: Option<u32> },
}

impl WorldMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn here(&self) -> Option<&str> {
        self.here.as_deref()
    }

    /// Records that we have had our one attempt at `key` and it should stop being a destination.
    ///
    /// Deliberately not folded into `apply_save`: the save is the game's view and would overwrite
    /// this on the next read, which is precisely the disagreement that caused the bounce.
    pub fn abandon(&mut self, key: &str) {
        self.abandoned.insert(key.to_string());
    }

    pub fn get(&self, key: &str) -> Option<&Place> {
        self.places.get(key)
    }

    pub fn len(&self) -> usize {
        self.places.len()
    }

    pub fn is_empty(&self) -> bool {
        self.places.is_empty()
    }

    pub fn places(&self) -> impl Iterator<Item = &Place> {
        self.places.values()
    }

    /// The anomaly, if it is open and still standing.
    ///
    /// Evidence first, prior second, and the order matters:
    ///
    /// 1. **A heading that says `anomaly`** is proof. `hellportal` has `typeName = 'anomaly'`, so a
    ///    dump naming it settles the question wherever the portal ended up.
    /// 2. **Otherwise [`ANOMALY_KEY`]**, but only as an assumption. Promoting `start` to the portal
    ///    is what `overworld/generators/world.lua:492-507` does — that is *one* generator, and other
    ///    worlds and start situations need not place it there. Preferring the key outright would
    ///    send the run to a node that is merely where it began.
    ///
    /// The prior is still worth having: it is right for the default world, and it is available the
    /// instant the anomaly opens, long before a dump shows the portal's new heading. Being wrong
    /// costs a wasted trip, and the heading corrects it as soon as one is seen.
    ///
    /// [`WorldMap::anomaly_is_assumed`] says which of the two answered.
    pub fn anomaly(&self) -> Option<&Place> {
        if self.anomaly_available().unwrap_or(true) || self.anomaly_beaten() {
            return None;
        }
        self.places
            .values()
            .find(|p| p.type_is("anomaly") && !p.completed)
            .or_else(|| self.places.get(ANOMALY_KEY))
    }

    /// True when [`WorldMap::anomaly`] is going on the `start` prior rather than a seen heading.
    ///
    /// Worth surfacing: a caller that arrives and finds no portal should stop trusting it and
    /// explore, rather than keep walking back to the same node.
    pub fn anomaly_is_assumed(&self) -> bool {
        self.anomaly().is_some_and(|p| !p.type_is("anomaly"))
    }

    /// Has the run's objective been met?
    ///
    /// Completing the portal *while corrupt* is what sets it, which is why the game's own check is
    /// `areaIsComplete'start_corrupt'`. Asked of whichever node is the portal, so it does not depend
    /// on the `start` prior being right.
    pub fn anomaly_beaten(&self) -> bool {
        self.places
            .values()
            .any(|p| p.completed_corrupt && (p.key == ANOMALY_KEY || p.type_is("anomaly")))
    }

    /// Has the anomaly already opened? `None` when no save has been read yet.
    pub fn anomaly_available(&self) -> Option<bool> {
        self.hell.map(|h| h == 0.0)
    }

    /// Records what a node cost us, and whether that is worth a detour to heal.
    ///
    /// Called with the health readings from either side of a node. Once set, the intent survives
    /// until [`WorldMap::rested`] clears it — a rest site may be several hops away, and forgetting
    /// on the way would strand us at low health next to the fight we were avoiding.
    pub fn note_health(&mut self, before: crate::rest::Health, after: crate::rest::Health) {
        if crate::rest::should_rest(before, after) {
            self.wants_rest = true;
        }
    }

    /// Records the current health on its own, with no before-reading to compare against.
    ///
    /// [`WorldMap::note_health`] can only fire when we *watched* health fall, which leaves a run
    /// that resumes at low health with no intent set at all — it walks into the next fight at a
    /// third health because nothing ever observed the drop. Call this wherever a reading is taken,
    /// including the very first one at startup.
    ///
    /// Sets and clears, so it is safe to call repeatedly: below half asks for a rest, full cancels
    /// it, and anything in between leaves an existing intent standing. That middle case matters —
    /// a partial heal must not cancel a rest we are still walking to.
    pub fn note_health_level(&mut self, now: crate::rest::Health) {
        if crate::rest::health_is_low(now) {
            self.wants_rest = true;
        } else if now.is_full() {
            self.wants_rest = false;
        }
    }

    /// `player.gold`, as of the last save read. What an inn will and will not do is a gold check.
    pub fn gold(&self) -> i64 {
        self.gold
    }

    /// Cleared once health is back up.
    pub fn rested(&mut self, now: crate::rest::Health) {
        if now.is_full() {
            self.wants_rest = false;
        }
    }

    pub fn wants_rest(&self) -> bool {
        self.wants_rest
    }

    /// Folds one adjacency dump into the map.
    ///
    /// Everything here is additive except the fields a dump is authoritative for. A neighbour's
    /// entry is created if unseen, but its `hidden` count and `visited` flag are left alone: only a
    /// dump taken *at* a node can speak to those.
    pub fn fold(&mut self, a: &Adjacency) {
        let parent = a.subworld.as_ref().map(|(k, _)| k.clone());
        // Crossing into a subworld: remember the surface node we came from, because leaving by the
        // same exit is a retreat. Recorded here rather than derived later — once inside, nothing on
        // screen distinguishes the entrance road from any other exit.
        if let Some((container, _)) = a.subworld.as_ref().filter(|_| self.inside().is_none()) {
            // The road we land on names where it goes, and we have just come from there.
            //
            // Taking `self.here` instead was wrong and silently disabled the whole rule: entering a
            // village goes l19 -> l10 -> l10_path_to_l19, so at this point `here` is the CONTAINER,
            // l10. Storing that meant the entrance never matched any exit's `to_key`, and a live run
            // turned straight back out of Ulrome with the guard against exactly that in place.
            //
            // The key is built by the game as `parent.key..'_path_to_'..k` (`overworldview.lua:1043`),
            // so the suffix is the overworld node on the other side.
            let prefix = format!("{container}_path_to_");
            self.entered_from = a
                .here_key
                .strip_prefix(&prefix)
                .filter(|k| !k.is_empty())
                .map(str::to_string);
        } else if a.subworld.is_none() {
            self.entered_from = None;
        }
        self.here = Some(a.here_key.clone());

        // Being inside a subworld names its container, which is the only reliable way we learn that
        // a node is one. Its heading will not say so — `Eight Timberland — level 4 forest` reads
        // exactly like a fight.
        if let Some((key, heading)) = a.subworld.as_ref() {
            let p = self.entry(key);
            p.subworld_container = true;
            if p.heading.is_empty() {
                p.heading = heading.clone();
            }
        }

        {
            let here = self.entry(&a.here_key);
            here.heading = a.here_heading.clone();
            here.visited = true;
            here.hidden = Some(a.hidden);
            here.parent = parent.clone();
        }

        for n in &a.nodes {
            // Connections of a node inside a subworld are inside the same subworld: the dump lists
            // `locationData[playerLocation].connections` here and reaches the parent's connections
            // only through the separate exits section (`overworldview.lua:1030-1047`).
            let p = parent.clone();
            let place = self.entry(&n.key);
            place.heading = n.heading.clone();
            place.connections = n.connections;
            if place.parent.is_none() {
                place.parent = p;
            }
            place.neighbours.insert(a.here_key.clone());
            self.entry(&a.here_key).neighbours.insert(n.key.clone());
        }

        // Exits lead OUT of the subworld, so what they name belongs to the parent world, not here —
        // and each one is a **surface edge**, `container <-> to_key`, which is the only place we ever
        // learn the container's neighbourhood.
        //
        // Dropping those edges was expensive. Standing inside Ulrome the game names all five of its
        // overworld neighbours (`overworldview.lua:1040-1047`), and we kept only their headings. So
        // `distances(target)` could reach exactly one of them — the one an earlier surface dump had
        // happened to mention — and ranking exits by distance degenerated into "leave the way you
        // came", every time, regardless of where the anomaly was.
        //
        // Entering a subworld is otherwise a blind spot: we travel onto the container and go straight
        // inside, so no surface dump is ever taken standing on it. This is the substitute, and it
        // arrives for free.
        if let Some((container, _)) = a.subworld.as_ref() {
            for e in &a.exits {
                let place = self.entry(&e.to_key);
                if place.heading.is_empty() {
                    place.heading = e.to_heading.clone();
                }
                place.neighbours.insert(container.clone());
                self.entry(container).neighbours.insert(e.to_key.clone());
            }
        }
    }

    /// Applies `mainSaveData`: which areas are complete, and whether the anomaly has opened.
    ///
    /// `completedAreas` also names places we may not have seen a dump for. Those are recorded as
    /// known-but-unheaded rather than skipped — a completed area is a real place, and forgetting it
    /// would let routing propose it as unexplored.
    pub fn apply_save(&mut self, save: &crate::game::save::Table) {
        if let Some(t) = save.table_at("overworld.completedAreas") {
            // Not every key here is a location. `setAreaComplete` also writes `<key>_corrupt` for a
            // corrupt area (`overworldview.lua:172`), and `setAreaIncomplete` manages
            // `<key>_path_to_<k>` entries for subworld exits (`:201`). Folding those in as places
            // invented destinations that routing would then try to walk to — `start_corrupt` showed
            // up as an unvisited frontier and became the plan.
            for key in t.map.keys() {
                if let Some(base) = key.strip_suffix("_corrupt") {
                    self.entry(base).completed_corrupt = true;
                } else if key.contains("_path_to_") {
                    continue;
                } else {
                    self.entry(key).completed = true;
                }
            }
        }
        if let Some(flags) = save.table_at("overworld.areaFlags") {
            // Every lost woods we have already been lost in names itself here. Once is enough.
            let keys: Vec<String> = flags
                .map
                .keys()
                .filter_map(|k| crate::subworld::lost_woods_key(k))
                .map(|s| s.to_string())
                .collect();
            for k in keys {
                self.entry(&k).avoid = true;
            }
            // `<key>_used`, which for a campfire decides whether resting there is free or futile.
            let used: Vec<String> = flags
                .map
                .keys()
                .filter_map(|k| k.strip_suffix("_used"))
                .map(|s| s.to_string())
                .collect();
            for k in used {
                self.entry(&k).used = true;
            }
            // `<key>_first_corrupt_time`. The `_shop` variant has a different suffix and so does
            // not match, which is what we want — it says nothing about the area itself.
            let corrupted: Vec<String> = flags
                .map
                .keys()
                .filter_map(|k| k.strip_suffix("_first_corrupt_time"))
                .map(|s| s.to_string())
                .collect();
            for k in corrupted {
                self.entry(&k).corrupted = true;
            }
            let consecrated: Vec<String> = flags
                .map
                .keys()
                .filter_map(|k| k.strip_suffix("_consecrated"))
                .map(|s| s.to_string())
                .collect();
            for k in consecrated {
                self.entry(&k).consecrated = true;
            }
        }
        self.hell = save
            .table_at("overworld.areaFlags")
            .and_then(|f| f.get("hell"))
            .and_then(|v| v.as_f64());
        if let Some(loc) = save.str_at("overworld.playerLocation") {
            self.here.get_or_insert_with(|| loc.to_string());
        }
        self.gold = save.int_at("player.gold").unwrap_or(0);
        self.fuel = crate::rest::fuel_from_save(save);
    }

    /// Where to go next.
    ///
    /// Priority follows the objective directly. If the anomaly is open, go to it — nothing else
    /// shortens the run. Otherwise head for a surface node of level > 3, because *arriving* there
    /// opens the anomaly and no fight is required to do it. Failing that, explore, preferring
    /// frontiers that could still reveal such a node.
    ///
    /// Returns `None` when there is nothing left worth travelling to, which is a real answer and not
    /// a failure: it means the map is fully explored with no trigger found, and the caller has to
    /// widen the search rather than keep walking.
    pub fn next_target(&self) -> Option<Plan> {
        // While a rest is wanted, EVERY branch skips areas that cost a fight to leave — not just the
        // one heading for the objective. A plan is a plan whatever its reason, and arriving at a
        // shrine or a frontier through an uncleared corrupted village hurts exactly as much as
        // arriving at an anomaly trigger that way.
        //
        // Two passes, so the rule cannot strand the run: if avoiding hostile ground leaves nothing
        // at all to do, the second pass drops the restriction and we get on with it.
        if self.wants_rest {
            if let Some(plan) = self.plan(true) {
                return Some(plan);
            }
            // Nowhere safe left on the map. Rather than fall straight through to the objective —
            // which is ordered by *usefulness* and can put a level 8 anomaly first — take the
            // cheapest fight there is.
            if let Some(plan) = self.easiest_hostile() {
                return Some(plan);
            }
        }
        self.plan(false)
    }

    /// Does going here cost a fight, on **either** axis?
    ///
    /// The exact complement of `plan`'s `ok`, so that the two passes of [`WorldMap::next_target`]
    /// tile the map with no gap and no overlap: everywhere `plan(true)` refuses,
    /// [`WorldMap::easiest_hostile`] will consider, and nowhere is refused by both.
    /// `the_two_passes_between_them_consider_every_place` pins that.
    fn owes_a_fight(p: &Place) -> bool {
        p.hostile_to_enter() || !p.arrival().is_free()
    }

    /// The cheapest fight on the map, for when being hurt has run out of safe options.
    ///
    /// Two reasons this beats going at the objective while hurt, and the second is the better one:
    ///
    /// 1. It is the fight least likely to end the run.
    /// 2. **Winning it may be what unlocks a rest.** A corrupted village's inn is locked behind the
    ///    village being cleared (`overworld/generators/village.lua:371-395`), so clearing the
    ///    cheapest one is a route to the rest we could not otherwise reach — not merely the least
    ///    bad way to keep moving.
    ///
    /// **The anomaly is excluded, and an unknown level sorts LAST.** Both because of the same trap:
    /// the anomaly's heading is `The Rift anomaly`, with no `— level N` in it, so [`Place::level`]
    /// returns `None` for the hardest fight on the map. Treating an absent level as zero ranked it
    /// as the gentlest option available — the exact opposite of this function's purpose, and it
    /// would have sent a run at a third health straight into the level 8 fight it was trying to
    /// avoid.
    ///
    /// So: unknown means unknown, and goes to the back. And the anomaly is not a stepping stone
    /// towards a rest under any reading — it is the objective, and reaching it is what the whole
    /// first pass exists to defer.
    ///
    /// ## [`Risk`] first, then level — **deferred as post-MVP, not settled**
    ///
    /// This used to sort on level alone, which cannot express the dev's ranking at all: a level 5
    /// forest and a level 5 crypt are not the same errand, and the forest is much the better one.
    /// See [`Risk`] for what separates them.
    ///
    /// Putting the class ahead of the level is a choice with a cost, and it is worth being plain
    /// about it: **a level 7 forest outranks a level 1 crypt.** The argument for it is that the
    /// forest's fights are individually declinable — a route across peaceful subnodes may cost
    /// nothing at all — while the crypt's one fight is compulsory, so the forest's *expected* cost
    /// can be zero where the crypt's never is. The argument against is that when it does bite, it
    /// bites at level 7, and at 1/20 health any level is fatal.
    ///
    /// Neither argument wins on the evidence available, and settling it wants live data on what a
    /// forest crossing actually costs — which no run has yet produced. The ordering follows the
    /// dev's read of the game until then, deliberately and with this written down. Swapping the two
    /// `.then` clauses is the whole change.
    fn easiest_hostile(&self) -> Option<Plan> {
        let here = self.here.as_deref().unwrap_or("");
        let mut candidates: Vec<&Place> = self
            .places
            .values()
            .filter(|p| p.key != here && !p.avoid && Self::owes_a_fight(p))
            .filter(|p| p.key != ANOMALY_KEY && !p.type_is("anomaly"))
            .collect();
        candidates.sort_by(|a, b| {
            a.risk()
                .cmp(&b.risk())
                .then(a.level().unwrap_or(u32::MAX).cmp(&b.level().unwrap_or(u32::MAX)))
                .then(a.key.cmp(&b.key))
        });
        candidates
            .first()
            .map(|p| Plan { target: p.key.clone(), reason: Goal::EasiestHostile { level: p.level() } })
    }

    /// The planner proper. `skip_hostile` excludes anywhere a fight is owed, on either axis.
    ///
    /// **Both axes, and that is the fix.** It used to consult [`Place::hostile_to_enter`] alone,
    /// which asks whether a subworld will hold us in — so a crypt, which is not a subworld and
    /// cannot trap anybody, sailed through the hurt pass and a run at 1/20 was routed onto a level 6
    /// one under [`Goal::Explore`]. Meanwhile `shrine5` sat on the same map printing
    /// `Gembling shrine` with no level at all, free to walk onto.
    fn plan(&self, skip_hostile: bool) -> Option<Plan> {
        let here = self.here.as_deref().unwrap_or("");
        let ok = |p: &Place| !(skip_hostile && Self::owes_a_fight(p));

        // **Health first, ahead of everything including a live anomaly.**
        //
        // This used to sit below the anomaly branch, on the reading that nothing shortens the run
        // like going straight at the objective. That is true and it is also how a run dies: the
        // anomaly is level 8, and arriving at it on a third health converts "the objective" into
        // "the end of the save". The anomaly does not expire, and health does not recover by
        // itself — so the detour is only ever a delay, while skipping it can be terminal.
        //
        // Ranked by site — a campfire costs nothing and needs no subworld — then by whether we can
        // actually rest there, which for an inn means carrying the ten gold it charges.
        //
        // Falls through when nothing qualifies, which is the common case on a corrupted map: every
        // inn there is locked behind its village being cleared. So this is a preference, not a
        // guarantee, and a run with no reachable rest still gets on with the objective rather than
        // standing still.
        //
        // Health before exploration: walking into the next fight hurt is how a run ends. Ranked by
        // site — a campfire costs nothing and needs no subworld — then by whether we can actually
        // rest there, which for an inn means carrying the ten gold it charges.
        if self.wants_rest {
            let mut sites: Vec<(&Place, crate::rest::Site)> = self
                .places
                .values()
                // A corrupted rest stop is locked, not lost. Corruption swaps a location's type for
                // its `corruptType` and puts the place under attack, so the inn sits behind
                // `underAttackAreaButtons` or `destroyedAreaButtons`
                // (`overworld/generators/village.lua:371-395`) — while the heading still ends in
                // "village", which is why nothing else here notices. Met live as
                // `Ulrome — level 6 village [corrupted]`.
                //
                // **ASSUMED, NOT VERIFIED.** We take it that clearing the corruption frees the inn,
                // so `completed` is the release and a corrupted site we have fought through serves
                // like any other. Nothing has tested it: the MVP is to reach the anomaly *quickly*,
                // shrines included, so no run has yet had reason to clear a corrupted village and
                // then sleep in it. If an inn stays destroyed once corruption has had it —
                // `destroyedAreaButtons` replaces the button set outright
                // (`overworld/generators/village.lua:393-395`) — this filter is wrong and corrupted
                // villages go back to being written off.
                .filter(|p| p.key != here && !p.avoid && (!p.corrupted || p.completed) && ok(p))
                .filter_map(|p| crate::rest::site(&p.heading).map(|s| (p, s)))
                .filter(|(p, s)| crate::rest::can_rest_at(*s, self.gold, self.fuel, !p.used))
                .collect();
            sites.sort_by(|(pa, sa), (pb, sb)| sb.rank().cmp(&sa.rank()).then(pa.key.cmp(&pb.key)));
            if let Some((p, _)) = sites.first() {
                return Some(Plan { target: p.key.clone(), reason: Goal::Rest });
            }
        }


        // The anomaly is hostile by construction, so `ok` skips it too on the first pass. That is
        // the point rather than an oversight: it is a level 8 fight, and walking into it below half
        // health is the single most expensive thing this run can do. On the first pass we would
        // rather go exploring — which is also how an unknown rest site gets found — and the second
        // pass takes it when there is genuinely nothing else.
        if let Some(p) = self.anomaly().filter(|p| ok(p)) {
            return Some(Plan { target: p.key.clone(), reason: Goal::Anomaly });
        }



        // Opening the anomaly comes BEFORE shrines, and not only because it is the objective:
        // **consecration is impossible until it is open.** `showConsecrateButton`
        // (`shrine.lua:93-96`) needs `hell ~= 0`, so a run that saved its shrines for first would
        // arrive at every one of them unable to finish it.
        //
        // Only worth aiming at while the trigger is unspent — `anomaly_available` is `None` before
        // any save has been read, and an unknown flag is treated as still available, since a wasted
        // trip is cheaper than stranding the run.
        if self.anomaly_available().unwrap_or(true) {
            let mut candidates: Vec<&Place> = self
                .places
                .values()
                .filter(|p| p.key != here && p.triggers_anomaly() && !p.avoid && ok(p))
                .collect();
            // Any qualifying node does; prefer one we have not cleared, then the lowest level for a
            // predictable arrival, then by key so the choice is stable across runs.
            candidates.sort_by(|a, b| {
                a.completed
                    .cmp(&b.completed)
                    .then(a.level().cmp(&b.level()))
                    .then(a.key.cmp(&b.key))
            });
            if let Some(p) = candidates.first() {
                return Some(Plan { target: p.key.clone(), reason: Goal::OpenTheAnomaly });
            }
        }

        // Shrines are worth going out of the way for, once the anomaly has opened and made
        // consecrating them possible at all. Getting one done means clearing its combat and then
        // `Visit` (`overworld/locations/shrine.lua:61-72`), which the loop reaches naturally: the
        // trip stops at the fight on the way in.
        //
        // Corruption changes the price of one, and "incomplete" is too blunt a proxy for it. A
        // shrine we revealed but never fought — perhaps far from the hell radius entirely — is
        // incomplete and perfectly ordinary; a corrupted one was complete until the radius reached
        // it. Both need a fight; only the second is the fight we want to avoid.
        //
        // [`Place::corrupted`] tells them apart from the save, so no prediction is needed.
        // `hellCheck` (`overworld/locations/hellportal.lua:16-23`) is in any case a radius about the
        // world ORIGIN with a perlin boundary — not about the anomaly — so predicting it would mean
        // mirroring the noise field as well as the geometry.
        //
        // Note what this does NOT do. "Take a corrupted shrine if it is on the direct path to the
        // anomaly" cannot be expressed here: while the anomaly is open and unfinished
        // [`Goal::Anomaly`] outranks this branch, and once it is finished there is no path left. So
        // a corrupted shrine is never a *destination* — passing through one is judged on arrival,
        // by [`WorldMap::worth_consecrating_here`].
        let anomaly_open = !self.anomaly_available().unwrap_or(true);
        let dist = self.distances(here);
        // **What is actually left to do at a shrine**, which `!consecrated` alone does not answer.
        // Two actions live there and they have different gates:
        //
        // * `Pray` needs the word solved and the area **unused** (`showPrayButton`,
        //   `shrine.lua:98-102`). Note what that condition does *not* require: for a major shrine the
        //   clause `hell == 0 or isConsecrated or isDesecrated` is satisfied by `hell == 0` outright,
        //   so praying is available **before** the anomaly opens, not after it. `areaUnused`
        //   (`overworldview.lua:215-218`) is the only thing that ever retires it.
        // * `Consecrate` needs `hell ~= 0` (`shrine.lua:93-96`, `activeIf` at `:244`), so it is
        //   impossible until the anomaly is open.
        //
        // A shrine offering neither is finished and must stop being a destination. It used to not,
        // and a live run showed what that costs: with `hell = 0` every shrine read as
        // "unconsecrated" forever, so the nearest one was always a valid target — and because this
        // branch excludes `here`, the node just left re-entered the candidate set at distance 1. The
        // run bounced `shrine1 -> l10 -> shrine1` twenty times and stopped having done nothing,
        // while the crypt it kept walking across never got fought because it was only ever a
        // waypoint between two shrines that each dissolved on arrival.
        //
        // The same bounce came back wearing different clothes, and `worth_a_trip` could not stop it.
        // A shrine whose puzzle we walked away from is genuinely unused — the game never set its
        // `_used` flag, because nothing was ever prayed — so it stays a perfectly valid destination
        // for a driver that has already decided never to enter it again. The caller marks the attempt
        // as spent; without [`WorldMap::abandon`] telling the planner too, the two disagree and the
        // run walks back and forth between the shrine it will not re-enter and the crypt on the way
        // to the next one. Thirty steps of `l10 -> shrine2 -> l10` is what that looks like.
        let worth_a_trip = |p: &Place| !p.used || (anomaly_open && !p.consecrated);
        if let Some(p) = self
            .places
            .values()
            .filter(|p| p.key != here && !p.avoid && ok(p) && p.type_is("shrine"))
            .filter(|p| !self.abandoned.contains(&p.key))
            .filter(|p| worth_a_trip(p))
            .filter(|p| !(anomaly_open && p.corrupted))
            .min_by_key(|p| dist_or_far(&dist, &p.key))
        {
            return Some(Plan { target: p.key.clone(), reason: Goal::Shrine });
        }
        // Nothing qualifying is known yet, so grow the map. Unvisited places first — standing on one
        // is what produces a dump — and among those prefer the ones still hiding neighbours.
        // **Somewhere we have never stood**, which is narrower than [`Place::is_frontier`] and
        // deliberately so.
        //
        // A frontier is either unvisited *or* visited with neighbours the game refused to name. The
        // second half is worth remembering — "there is no shrine adjacent" is not a conclusion while
        // that count is nonzero — but it is **not worth walking back to**, and treating it as a
        // destination is a loop. Standing on a node again produces the same dump: the hidden count
        // is a cloud count, and clouds are cleared by `clearFogAroundPoint` (`wizard_tower.lua:61`),
        // never by arriving.
        //
        // Live, on 2026-08-08: `l41` was visited with hidden neighbours, so it stayed a frontier for
        // ever. Standing on `l35` the planner explored to `l41`; standing on `l41` the branch
        // excludes `here`, found no other free frontier, fell through to `easiest_hostile`, and
        // routed to `l9` — whose first hop is `l35`. `l35 -> l41 -> l35` until the run failed.
        //
        // Abandoned places drop out here too, matching the shrine branch. Same reason: the planner
        // and the driver must not disagree about what is still worth walking to.
        //
        // What this gives up, and it is real: after a wizard's tower clears fog, a node we have
        // already stood on may genuinely have new neighbours. Nothing clears fog yet
        // (`tower::press_reveal` is a stub), so there is no such case to lose today — and when there
        // is, the honest fix is to clear `visited` on the nodes the reveal touched rather than to
        // walk back to every one of them on spec.
        let mut frontier: Vec<&Place> = self
            .places
            .values()
            .filter(|p| p.key != here && !p.visited && !p.avoid && ok(p))
            .filter(|p| !self.abandoned.contains(&p.key))
            .collect();
        // Order matters, and a live run showed why. From l10 with l1 and l18 both adjacent and both
        // unvisited, sorting by key chose `l1` — already **completed**, so it revealed nothing — and
        // the run then had to walk back through l10 to reach l18. Two of three hops wasted, on an
        // objective that is explicitly about speed.
        //
        // So: unvisited first, then nearest, then whatever has most left to give — an uncompleted
        // node over a finished one, and a better-connected node over a dead end.
        let far = usize::MAX;
        let dist = self.distances(here);
        frontier.sort_by(|a, b| {
            a.visited
                .cmp(&b.visited)
                .then(dist.get(&a.key).unwrap_or(&far).cmp(dist.get(&b.key).unwrap_or(&far)))
                .then(a.completed.cmp(&b.completed))
                .then(b.connections.cmp(&a.connections))
                .then(b.hidden.unwrap_or(0).cmp(&a.hidden.unwrap_or(0)))
                .then(a.key.cmp(&b.key))
        });
        frontier.first().map(|p| Plan { target: p.key.clone(), reason: Goal::Explore })
    }

    /// The single adjacent node to step to next, and the plan it serves.
    ///
    /// Travel is taken one hop at a time rather than by naming a distant destination, because the
    /// destination may not be selectable — see [`crate::subworld::Rules::multi_hop_travel`], which
    /// is false wherever the map does not accumulate. Moving one hop is correct everywhere, so it
    /// is what we always do.
    ///
    /// The hop is chosen by breadth-first search over the edges **we** have recorded, which stay
    /// valid even where the layout is re-rolled
    /// ([`crate::subworld::Rules::edges_survive_reentry`]). That is not re-implementing the game's
    /// pathfinder: it only decides which neighbour points the right way, and the game still refuses
    /// the move if it disagrees.
    ///
    /// Falls back to any adjacent frontier when the target is not reachable through known edges —
    /// which is the normal case early on, when the way there simply has not been mapped yet.
    pub fn next_hop(&self) -> Option<Hop> {
        let here = self.here.as_deref()?;
        let plan = self.next_target()?;
        // Two passes. The first refuses to route through a lost woods we have already escaped, even
        // if that means a much longer way round. The second allows it, and exists only so that a
        // world where every route passes through one leaves us moving rather than stuck — being
        // swallowed again is bad, standing still forever is worse.
        if let Some(step) = self.first_step_toward(here, &plan.target, true) {
            return Some(Hop { step, plan });
        }
        if let Some(step) = self.first_step_toward(here, &plan.target, false) {
            return Some(Hop { step, plan });
        }
        // No known route. Step to an adjacent frontier instead: mapping outward is what will
        // eventually connect us to the target.
        //
        // The plan is carried through unchanged. Replacing it with the frontier node would make the
        // goal follow the footsteps — every hop would re-derive a nearby target and the run would
        // wander instead of working toward the trigger.
        let me = self.places.get(here)?;
        let mut options: Vec<&Place> = me
            .neighbours
            .iter()
            .filter_map(|k| self.places.get(k))
            .filter(|p| p.is_frontier())
            .collect();
        // Avoided places sort last rather than out, so they remain a last resort.
        options.sort_by(|a, b| {
            a.avoid.cmp(&b.avoid).then(a.visited.cmp(&b.visited)).then(a.key.cmp(&b.key))
        });
        options.first().map(|p| Hop { step: p.key.clone(), plan })
    }

    /// Is a single step from `from` to `to` legal?
    ///
    /// `canTravelToDirect` (`overworldview.lua:1316-1321`) requires **one of the two** endpoints to
    /// be complete:
    ///
    /// ```lua
    /// and (core.areaOrExitToComplete(location1.key, location2.key)
    ///      or core.areaOrExitToComplete(location2.key, location1.key))
    /// ```
    ///
    /// So an unfinished fight blocks going *onward*, never going *back* — the node behind us is
    /// complete by definition, since we came through it. A live run missed this and fought a node
    /// it only had to walk away from: it asked "am I standing on a fight?" before asking "where am
    /// I going?", and answered the first question in isolation.
    ///
    /// Secret nodes have a further `_revealed` condition we cannot see; being wrong there costs a
    /// refused move, and the game is still the authority.
    ///
    /// **Advisory, not a filter.** It is deliberately not used to prune routes: `areaOrExitToComplete`
    /// carries an exit notion this does not model, and a node we have merely never heard of reads as
    /// incomplete — so pruning on it removed every route in a map that had not been walked yet. It
    /// answers one question only: *must* we fight where we stand, or can we simply leave?
    pub fn can_step(&self, from: &str, to: &str) -> bool {
        let done = |k: &str| self.places.get(k).map(|p| p.completed).unwrap_or(false);
        done(from) || done(to)
    }

    /// Are we currently inside a subworld, and if so which one?
    pub fn inside(&self) -> Option<&str> {
        let here = self.here.as_deref()?;
        self.places.get(here)?.parent.as_deref()
    }

    /// Which overworld node to leave a subworld toward.
    ///
    /// Each exit leads to exactly one neighbour of the container, and the dump names it
    /// (`Exit::to_key`, from `overworldview.lua:1041-1047`) — so leaving is a choice of destination,
    /// not a search. Picks the exit that lands nearest the current target; failing that, any exit,
    /// because being inside a subworld we cannot navigate is worse than being anywhere outside it.
    ///
    /// Returns the key to head for. The caller matches it against the exits in a **current** dump,
    /// since exit positions expire with every pan like everything else.
    pub fn exit_toward(&self, exits: &[crate::observe::adjacency::Exit]) -> Option<String> {
        if exits.is_empty() {
            return None;
        }
        let entrance = self.entered_from.clone();
        let target = self.next_target().map(|p| p.target);
        if let Some(target) = target {
            let dist = self.distances(&target);
            if let Some(best) = exits
                .iter()
                .filter(|e| dist.contains_key(&e.to_key))
                .min_by_key(|e| dist_or_far(&dist, &e.to_key))
            {
                // Retreating through the entrance is only right when it is genuinely the way on —
                // backtracking to an inn, say.
                //
                // The root cause of a live run turning straight back out of Ulrome was upstream of
                // here, in `fold`: the container's exit edges were being thrown away, so `distances`
                // could reach exactly one exit and "nearest" meant "the only one we had". That is
                // fixed, and this is no longer what saves that case.
                //
                // It stays because the degenerate case is real whenever the target is not reachable
                // through *any* exit yet -- which is the normal state, since heading for the anomaly
                // means heading for a node we have never stood on. Preference must not collapse onto
                // already-visited ground: an unvisited exit is not a worse bet than the room we just
                // left, it is an unmeasured one, and unmeasured is the entire point of exploring.
                if Some(&best.to_key) == entrance.as_ref() {
                    if let Some(onward) = exits.iter().find(|e| {
                        Some(&e.to_key) != entrance.as_ref()
                            && !self.places.get(&e.to_key).map(|p| p.visited).unwrap_or(false)
                    }) {
                        return Some(onward.to_key.clone());
                    }
                }
                return Some(best.to_key.clone());
            }
        }
        // No target, or none of the exits lead anywhere we can measure: still prefer not to retreat.
        exits
            .iter()
            .find(|e| Some(&e.to_key) != entrance.as_ref())
            .or_else(|| exits.first())
            .map(|e| e.to_key.clone())
    }

    /// One move toward getting out of the subworld we are standing in.
    ///
    /// [`WorldMap::exit_toward`] answers *which* exit; this answers what to do next about reaching
    /// it, which is a different question and the one a live run got wrong. It clicked the exit
    /// directly and the screen moved 0.015 — because an exit several nodes away cannot simply be
    /// travelled to.
    ///
    /// Two rules from the game make crossing a walk rather than a jump:
    ///
    /// - `canTravelToDirect` (`overworldview.lua:1316-1321`) needs **one endpoint complete**. Inside
    ///   a corrupted subworld `setHellValue` has reset everything to incomplete, so until the node
    ///   under our feet is cleared there is no legal move off it at all. That is the mechanism behind
    ///   "you can't walk past a combat node".
    /// - Exits are only printed while visible (`:1044`), so the set of exits is what we can see now,
    ///   not the full list.
    ///
    /// The exit's own key is derivable rather than guessable: the dump builds it as
    /// `parent.key..'_path_to_'..k` (`:1043`), which is why the player location reads
    /// `l10_path_to_l19` while standing on one.
    pub fn cross_toward(&self, exits: &[crate::observe::adjacency::Exit]) -> Option<Crossing> {
        let parent = self.inside()?.to_string();
        let here = self.here.as_deref()?.to_string();

        // Where inside this subworld are we trying to get to?
        //
        // Three answers, and only the third is what crossing a forest ever needed:
        //
        // - **the inn**, when a rest is what brought us into a village and we have seen one;
        // - **nowhere yet**, when we are in a village looking for an inn the fog still hides —
        //   which must NOT fall back to the exit, because leaving is the one move that abandons the
        //   errand, and the doorway we came in by is usually the nearest thing to walk to;
        // - **the road out**, which is every other case.
        let inn = self.inn_inside(&parent).map(|p| p.key.clone());
        let leaving_to = match inn.is_some() || self.seeking_a_rest(&parent) {
            true => None,
            false => Some(self.exit_toward(exits)?),
        };
        let dest = match (&inn, &leaving_to) {
            (Some(k), _) => Some(k.clone()),
            (None, Some(to)) => Some(exit_node_key(&parent, to)),
            (None, None) => None,
        };

        // Standing on the inn: the crossing is over and the errand starts. Guarded by
        // `blocks_departure` because a village under attack puts a fight on its subnodes
        // (`village.lua:371-395`), and a fight underfoot is dealt with before anything else.
        if inn.as_deref() == Some(here.as_str()) && !self.blocks_departure(&here) {
            return Some(Crossing::Arrive { at: here });
        }
        // Already standing on the road out.
        if let Some(to) = leaving_to.filter(|to| here == exit_node_key(&parent, to)) {
            return Some(Crossing::Leave { to });
        }
        // An unfinished fight underfoot blocks going ONWARD. It does not block going back.
        //
        // This returned `Fight` unconditionally, and a live run paid for it: at 0 health, crossing
        // Ulrome toward the `l7` campfire, it stepped onto `l10sub11` — a level 6 guard post — and
        // concluded the only legal move was to fight it. Three turns later the board was down to one
        // tile with nothing playable.
        //
        // `canTravelToDirect` needs one of the two endpoints complete
        // (`overworldview.lua:1316-1321`), and the node we arrived from is complete by definition.
        // [`WorldMap::can_step`] says so in as many words; this function simply never asked. So when
        // a rest is what we are after, retreating is available and is the better move — the fight
        // ahead is the thing we are trying to avoid, and taking it because we walked one node too
        // far is the worst version of that.
        if self.blocks_departure(&here) {
            // Retreating is a *preference*, and the caller is what stops it becoming a habit — see
            // `Run::retreats_running`. Deciding it here was tried and is wrong: the obvious test,
            // "is there another route to the exit", cannot tell "the fight ahead is unavoidable"
            // from "the interior is still fogged and we have not learned the route yet", and the
            // second is the normal state on arrival. `hurt_and_blocked_we_back_out_instead_of_
            // fighting` is exactly that case and says so.
            if self.wants_rest {
                if let Some(back) = self.retreat_step(&here) {
                    return Some(Crossing::Retreat { to: back });
                }
            }
            return Some(Crossing::Fight { at: here });
        }
        if let Some(step) = dest.as_ref().and_then(|d| self.first_step_toward(&here, d, false)) {
            return Some(Crossing::Step { to: step, toward: dest? });
        }
        // No known route. Fog means the interior is learned a hop at a time, so an unknown route is
        // the normal early state rather than an error — walk into the dark, preferring somewhere we
        // have not been so the walk cannot cycle.
        //
        // **This used to take any neighbour at all**, and none of the routing above applied to it:
        // no chest shun, no paved preference, and — the one that hurt — no exclusion of the OTHER
        // exits. Live in `l9`, crossing toward `l9_path_to_l19`, it stepped onto `l9_path_to_l1` and
        // left the forest by the wrong door, straight into a level 7 corrupted crypt and a
        // highwayman who took all 763 gold on the way.
        //
        // The logging hid it too: `Step` and `Explore` print the same line, so the report read like
        // a considered route rather than a guess. That is worth knowing when reading old logs.
        let place = self.places.get(&here)?;
        //
        // With no destination at all — looking for an inn we have not seen — *every* exit is
        // "elsewhere", so this filter is also what keeps a search inside the village it is
        // searching.
        let exits_elsewhere: BTreeSet<String> = exits
            .iter()
            .map(|e| exit_node_key(&parent, &e.to_key))
            .filter(|k| Some(k) != dest.as_ref())
            .collect();
        let usable = |k: &String| {
            // Another exit road is not a step into the dark, it is a way out of the subworld — and
            // out is where we are trying to get to, just not by that door.
            !exits_elsewhere.contains(k)
                && !self.abandoned.contains(k)
                && self.places.get(k).map(|p| {
                    !p.avoid && !(self.wants_rest && p.is_chest() && !p.completed)
                }).unwrap_or(true)
        };
        // Same order the real router uses, so exploring does not undo what routing was for: paved
        // first, then anywhere; unvisited first within each, so the walk cannot cycle.
        let pick = |paved_only: bool| {
            let mut best: Option<&String> = None;
            for n in place.neighbours.iter().filter(|n| usable(n)) {
                let p = self.places.get(n);
                if paved_only && !p.map(|p| p.is_paved()).unwrap_or(false) {
                    continue;
                }
                let seen = p.map(|p| p.visited).unwrap_or(false);
                let better = match best {
                    None => true,
                    Some(b) => {
                        let b_seen = self.places.get(b).map(|p| p.visited).unwrap_or(false);
                        (!seen, n) < (!b_seen, b)
                    }
                };
                if better {
                    best = Some(n);
                }
            }
            best.cloned()
        };
        // Falls all the way through to an unfiltered neighbour rather than returning `None`: being
        // stuck in a subworld with nowhere to step is worse than any single bad step, and the health
        // gate still gets its say on whatever we land on.
        let step = pick(true)
            .or_else(|| pick(false))
            .or_else(|| place.neighbours.iter().min().cloned())?;
        Some(match dest {
            Some(toward) => Crossing::Explore { to: step, toward },
            None => Crossing::Seek { to: step },
        })
    }

    /// The inn inside `container`, when a rest is what we are in there for.
    ///
    /// `None` covers three different situations, and the caller does not need to tell them apart:
    /// we are not resting, we cannot pay, or the fog has not shown us the inn yet. The first two
    /// mean cross normally; the third is what [`WorldMap::seeking_a_rest`] separates out.
    ///
    /// The gold check is not an optimisation. `getCanRest` is a flat `getPlayerGold() >= 10`
    /// (`ui/rest.lua:49`), so walking a hurt run across a village with nine gold buys a wasted trip
    /// and the fights on the way back.
    fn inn_inside(&self, container: &str) -> Option<&Place> {
        if !self.wants_rest || self.gold < crate::rest::INN_COST {
            return None;
        }
        self.places
            .values()
            .find(|p| p.parent.as_deref() == Some(container) && p.is_inn() && !self.abandoned.contains(&p.key))
    }

    /// Are we inside a village on a rest errand, with its inn still to find?
    ///
    /// The container's own heading is what says "village" — `typeName = 'village'` on the surface
    /// node — so this asks a question about the place we are *in*, not the place we are on.
    ///
    /// **An abandoned inn ends the search**, which is the difference between this and a plain
    /// `inn_inside().is_none()`. [`WorldMap::abandon`] is the driver's record of having had its go,
    /// and without that clause a village whose inn refused to serve us would be searched forever:
    /// the inn is filtered out of `inn_inside`, so the fog case and the tried-it case would look
    /// identical from here. That is the same shape as every bounce this project has had.
    fn seeking_a_rest(&self, container: &str) -> bool {
        if !self.wants_rest || self.gold < crate::rest::INN_COST {
            return false;
        }
        if !self.places.get(container).map(|p| p.type_is("village")).unwrap_or(false) {
            return false;
        }
        !self
            .places
            .values()
            .any(|p| p.parent.as_deref() == Some(container) && p.is_inn() && self.abandoned.contains(&p.key))
    }

    /// A neighbour we may legally step back to from a node that blocks going onward.
    ///
    /// Any **completed** neighbour qualifies, because `canTravelToDirect` accepts the move when
    /// either endpoint is complete. Prefers one we have already stood on — that is the way we came,
    /// and it leads back toward the entrance rather than deeper in.
    fn retreat_step(&self, here: &str) -> Option<String> {
        let me = self.places.get(here)?;
        let mut back: Vec<&Place> = me
            .neighbours
            .iter()
            .filter_map(|k| self.places.get(k))
            .filter(|p| p.completed && !p.avoid)
            .collect();
        back.sort_by(|a, b| b.visited.cmp(&a.visited).then(a.key.cmp(&b.key)));
        back.first().map(|p| p.key.clone())
    }

    /// Does an unfinished fight on this node forbid stepping off it?
    fn blocks_departure(&self, key: &str) -> bool {
        self.places
            .get(key)
            .map(|p| !p.completed && heading_has_combat(&p.heading))
            .unwrap_or(false)
    }

    /// Standing here, is this shrine worth consecrating before moving on?
    ///
    /// This is where "unless it is on the shortest direct path to the anomaly" actually lives.
    /// Routing cannot honour that clause — see the note in [`WorldMap::next_target`] — so it is
    /// asked on arrival instead, when the cost is only the shrine itself and not a detour.
    ///
    /// An uncorrupted shrine is always worth it. A corrupted one costs a fight, and earns it only
    /// when we are walking through anyway.
    pub fn worth_consecrating_here(&self, key: &str) -> bool {
        let Some(p) = self.places.get(key) else { return false };
        if !p.type_is("shrine") || p.consecrated {
            return false;
        }
        // Consecrating needs the anomaly open at all (`shrine.lua:93-96`).
        if self.anomaly_available().unwrap_or(true) {
            return false;
        }
        if !p.corrupted {
            return true;
        }
        self.anomaly_route().map(|r| r.contains(&key.to_string())).unwrap_or(false)
    }

    /// The shortest known route to the open anomaly, or `None` while it is still shut.
    ///
    /// `None` means "detour freely" — before the anomaly opens there is no corruption to avoid and
    /// nothing to hurry toward. `Some(route)` restricts side trips to what is already underfoot.
    fn anomaly_route(&self) -> Option<Vec<String>> {
        let here = self.here.as_deref()?;
        let target = self.anomaly()?;
        self.route(here, &target.key)
    }

    /// Every step of a shortest known path from `from` to `to`, inclusive of `to`.
    fn route(&self, from: &str, to: &str) -> Option<Vec<String>> {
        let mut came: BTreeMap<String, String> = BTreeMap::new();
        let mut seen: BTreeSet<&str> = [from].into_iter().collect();
        let mut queue: std::collections::VecDeque<&str> = [from].into();
        while let Some(key) = queue.pop_front() {
            if key == to {
                let mut path = vec![to.to_string()];
                let mut cur = to.to_string();
                while let Some(prev) = came.get(&cur) {
                    if prev == from {
                        break;
                    }
                    path.push(prev.clone());
                    cur = prev.clone();
                }
                path.reverse();
                return Some(path);
            }
            if let Some(p) = self.places.get(key) {
                for n in &p.neighbours {
                    if seen.insert(n.as_str()) {
                        came.insert(n.clone(), key.to_string());
                        queue.push_back(n.as_str());
                    }
                }
            }
        }
        None
    }

    /// Hops from `from` to every place reachable through known edges.
    ///
    /// Used to prefer the nearest frontier. Avoided places are still traversed here — this measures
    /// how far away things are, and [`WorldMap::next_hop`] is where the shunning happens.
    fn distances(&self, from: &str) -> BTreeMap<String, usize> {
        let mut out = BTreeMap::new();
        out.insert(from.to_string(), 0usize);
        let mut queue: std::collections::VecDeque<String> = [from.to_string()].into();
        while let Some(key) = queue.pop_front() {
            let d = out[&key];
            if let Some(p) = self.places.get(&key) {
                for n in &p.neighbours {
                    if !out.contains_key(n) {
                        out.insert(n.clone(), d + 1);
                        queue.push_back(n.clone());
                    }
                }
            }
        }
        out
    }

    /// First move on a shortest known path from `from` to `to`, or `None` if we know of no route.
    ///
    /// With `shun`, places marked [`Place::avoid`] are treated as walls.
    /// The first hop of a route, **preferring the paved path**.
    ///
    /// The dev's rule, from playing the game: inside a forest, stick to the road unless a combat
    /// node sits on it and there is a combat-free way round. Four passes, first match wins:
    ///
    /// ```text
    ///   1. paved, and no fight on it        the ordinary crossing
    ///   2. any type, and no fight on it     the "combat-less pathway" the rule allows
    ///   3. paved, fight and all             the road is blocked and nothing else is clear
    ///   4. anything at all                  never stall; see below
    /// ```
    ///
    /// The order encodes the rule exactly: 2 only ever beats 3 when the road has a fight on it and
    /// the detour does not, because otherwise 1 would have matched already.
    ///
    /// **Pass 4 is the one that must not be removed.** Forest combat nodes can block progress
    /// outright — a spider forest is generated with nests across the interior — and a run that
    /// refuses every route because they all cost a fight has stalled, which is worse than fighting.
    /// Whether the fight is then *taken* is not decided here: `cross_toward` and the health gate in
    /// `navigate` have their own say, and this only reports that a way exists.
    ///
    /// Paved nodes are still subject to `blocked`, so an abandoned or shunned road is skipped by
    /// every pass — those are hard exclusions, this is a preference.
    fn first_step_toward(&self, from: &str, to: &str, shun: bool) -> Option<String> {
        let costs_a_fight = |p: &Place| !p.completed && heading_has_combat(&p.heading);
        // Ordered least to most tolerant. `to` is exempt from the preference for the same reason it
        // is exempt from `blocked`: refusing to path to where we were told to go is not routing.
        let passes: [&dyn Fn(&Place) -> bool; 4] = [
            &|p: &Place| !p.is_paved() || costs_a_fight(p),
            &|p: &Place| costs_a_fight(p),
            &|p: &Place| !p.is_paved(),
            &|_: &Place| false,
        ];
        for avoid in passes {
            if let Some(step) = self.step_avoiding(from, to, shun, avoid) {
                return Some(step);
            }
        }
        None
    }

    /// Breadth-first over the edges we have recorded, skipping anything `blocked` or `avoid` rejects.
    ///
    /// `blocked` is the hard exclusion — lost woods, chests while resting, abandoned nodes — and
    /// `avoid` is the caller's preference for this pass. Split so that [`first_step_toward`] can run
    /// several preferences over one set of exclusions.
    fn step_avoiding(
        &self, from: &str, to: &str, shun: bool, avoid: &dyn Fn(&Place) -> bool,
    ) -> Option<String> {
        if from == to {
            return None;
        }
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        seen.insert(from);
        // Each queue entry carries the first step that led to it, which is all we need back.
        // Two independent reasons to route around a node, and the destination itself is exempt from
        // both — refusing to path to where we were told to go is not avoidance, it is failure.
        //
        // `shun` is the lost-woods rule. The second is the dev's: **while a rest is the objective,
        // treasure chests are skipped.** Opening one is a fight (see [`Place::is_chest`]) and it
        // carries a `level/30` chance of being a mimic, which is a gamble worth nothing at all when
        // the errand is to stop being hurt. Note this shuns them as *waypoints* too, which is the
        // point: an unopened chest is incomplete, and `canTravelToDirect` will not let us step off it
        // onto another incomplete node, so crossing one is not something we can plan on.
        //
        // A chest whose fight is already **compulsory** is untouched by this. That case never reaches
        // here: `getAreaButtons` shows `combatButton` instead of `Open` when the subnode has enemies
        // (`forest.lua:186-188`), and `blocks_departure` catches it underfoot. Non-negotiable either
        // way, whatever the objective.
        let blocked = |k: &str| {
            k != to
                && self
                    .places
                    .get(k)
                    .map(|p| {
                        (shun && p.avoid)
                            || (self.wants_rest && p.is_chest() && !p.completed)
                            || self.abandoned.contains(&p.key)
                            || avoid(p)
                    })
                    .unwrap_or(false)
        };
        let mut queue: std::collections::VecDeque<(&str, String)> = self
            .places
            .get(from)?
            .neighbours
            .iter()
            .filter(|n| !blocked(n))
            .map(|n| (n.as_str(), n.clone()))
            .collect();
        for (k, _) in queue.iter() {
            seen.insert(k);
        }
        while let Some((key, first)) = queue.pop_front() {
            if key == to {
                return Some(first);
            }
            if let Some(p) = self.places.get(key) {
                for n in &p.neighbours {
                    if !blocked(n) && seen.insert(n.as_str()) {
                        queue.push_back((n.as_str(), first.clone()));
                    }
                }
            }
        }
        None
    }

    fn entry(&mut self, key: &str) -> &mut Place {
        self.places.entry(key.to_string()).or_insert_with(|| Place {
            key: key.to_string(),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observe::adjacency::{Exit, Node};

    fn node(key: &str, heading: &str) -> Node {
        Node { key: key.into(), heading: heading.into(), x: 0.0, y: 0.0, connections: 2 }
    }

    fn dump(here: &str, heading: &str, nodes: Vec<Node>) -> Adjacency {
        Adjacency {
            reason: "Arrived at location".into(),
            here_key: here.into(),
            here_heading: heading.into(),
            subworld: None,
            nodes,
            hidden: 0,
            exits: Vec::new(),
            hidden_exits: 0,
        }
    }

    /// A dump taken inside `parent`, with the exits the game would have printed.
    fn inside_dump(parent: &str, here: &str, heading: &str, nodes: Vec<Node>, exits: Vec<Exit>) -> Adjacency {
        Adjacency {
            subworld: Some((parent.into(), "Ulrome — level 6 village".into())),
            exits,
            ..dump(here, heading, nodes)
        }
    }

    fn exit(to: &str) -> Exit {
        Exit { x: 0.0, y: 0.0, to_key: to.into(), to_heading: format!("{to} heading") }
    }

    #[test]
    fn a_subworlds_exits_are_its_containers_overworld_edges() {
        // The only place the container's neighbourhood is ever learned. We travel onto a village and
        // step straight inside, so no surface dump is taken standing on it.
        let mut m = WorldMap::new();
        m.fold(&inside_dump("l10", "l10sub6", "Ulrome guard post", vec![],
            vec![exit("l7"), exit("l1"), exit("l18"), exit("l4"), exit("l19")]));
        let l10 = m.get("l10").expect("container");
        for k in ["l7", "l1", "l18", "l4", "l19"] {
            assert!(l10.neighbours.contains(k), "l10 should know it borders {k}");
            assert!(m.get(k).unwrap().neighbours.contains("l10"), "{k} should know it borders l10");
        }
    }

    #[test]
    fn exit_edges_make_a_route_through_the_village_visible() {
        // With the edges kept, a target beyond an unentered exit becomes reachable, so ranking by
        // distance means something. Without them every exit but the entrance scored infinite and the
        // village was a revolving door.
        let mut m = WorldMap::new();
        m.fold(&dump("l19", "Gipsyville crypt", vec![node("l10", "Ulrome — level 6 village")]));
        m.fold(&inside_dump("l10", "l10sub6", "Ulrome guard post", vec![],
            vec![exit("l19"), exit("l7")]));
        // l19 -> l10 -> l7 is now a path, so l7 is two hops from where we entered rather than absent.
        let d = m.distances("l7");
        assert_eq!(d.get("l10"), Some(&1));
        assert_eq!(d.get("l19"), Some(&2));
    }

    #[test]
    fn leaving_by_the_door_we_came_in_is_a_last_resort() {
        // The Ulrome case. We walk l19 -> l10 and enter; inside, l19 is the only exit whose distance
        // to anything is known, because every other neighbour is unexplored. Scoring unknown as
        // infinite made the run turn straight back out of the village it had just entered.
        let mut m = WorldMap::new();
        m.fold(&dump("l19", "Gipsyville crypt", vec![node("l10", "Ulrome — level 6 village")]));
        // Entering lands on the entrance ROAD first -- this is the real sequence, and taking the
        // previous `here` instead recorded the container and disabled the rule entirely.
        m.fold(&inside_dump("l10", "l10_path_to_l19", "Road to Gipsyville crypt",
            vec![node("l10sub6", "Ulrome guard post")], vec![exit("l19")]));
        assert_eq!(m.entered_from.as_deref(), Some("l19"), "the road names where it came from");
        m.fold(&inside_dump("l10", "l10sub6", "Ulrome guard post",
            vec![node("l10_path_to_l19", "Road to Gipsyville crypt")], vec![exit("l19")]));
        // Offered the entrance and one unexplored way on, take the unexplored one.
        let chosen = m.exit_toward(&[exit("l19"), exit("l7")]);
        assert_eq!(chosen, Some("l7".into()), "should head somewhere new, not back out");
    }

    #[test]
    fn the_entrance_is_still_taken_when_it_is_the_only_way_out() {
        // A dead-end subworld, or a deliberate backtrack. Refusing to retreat must not mean refusing
        // to move.
        let mut m = WorldMap::new();
        m.fold(&dump("l19", "Gipsyville crypt", vec![node("l10", "Ulrome — level 6 village")]));
        m.fold(&inside_dump("l10", "l10_path_to_l19", "Road to Gipsyville crypt",
            vec![node("l10sub6", "Ulrome guard post")], vec![exit("l19")]));
        assert_eq!(m.exit_toward(&[exit("l19")]), Some("l19".into()));
    }

    #[test]
    fn the_entrance_is_forgotten_on_the_way_out() {
        // Otherwise a stale entrance would go on biasing exit choice in the *next* subworld.
        let mut m = WorldMap::new();
        m.fold(&dump("l19", "Gipsyville crypt", vec![node("l10", "Ulrome — level 6 village")]));
        m.fold(&inside_dump("l10", "l10_path_to_l19", "Road to Gipsyville crypt", vec![],
            vec![exit("l19")]));
        assert_eq!(m.entered_from.as_deref(), Some("l19"));
        m.fold(&dump("l7", "Greenoak Backwoods campfire", vec![]));
        assert_eq!(m.entered_from, None);
    }

    #[test]
    fn the_exit_road_key_is_built_the_way_the_game_builds_it() {
        // `parent.key..'_path_to_'..k` (overworldview.lua:1043). Live confirmation: standing on one
        // reported the player location as exactly this.
        assert_eq!(exit_node_key("l10", "l19"), "l10_path_to_l19");
    }

    #[test]
    fn an_unfinished_fight_underfoot_is_the_only_legal_move() {
        // `canTravelToDirect` needs one endpoint complete, and corruption reset everything to
        // incomplete — so a fight here is not a detour we chose, it is the only thing available.
        let mut m = WorldMap::new();
        m.fold(&inside_dump(
            "l10",
            "l10sub6",
            "Ulrome guard post — level 4 crypt",
            vec![node("l10sub7", "Ulrome east guard post")],
            vec![exit("l19")],
        ));
        assert_eq!(m.cross_toward(&[exit("l19")]), Some(Crossing::Fight { at: "l10sub6".into() }));
    }

    #[test]
    fn a_cleared_node_steps_toward_the_exit_one_hop_at_a_time() {
        let mut m = WorldMap::new();
        // Learn the interior: sub6 -> sub7 -> the road out.
        m.fold(&inside_dump("l10", "l10sub6", "Ulrome guard post",
            vec![node("l10sub7", "Ulrome east guard post")], vec![exit("l19")]));
        m.fold(&inside_dump("l10", "l10sub7", "Ulrome east guard post",
            vec![node("l10sub6", "Ulrome guard post"), node("l10_path_to_l19", "Road to Gipsyville")],
            vec![exit("l19")]));
        m.fold(&inside_dump("l10", "l10sub6", "Ulrome guard post",
            vec![node("l10sub7", "Ulrome east guard post")], vec![exit("l19")]));
        // Not a jump to the exit — the neighbour on the way to it. Clicking the exit itself is what
        // moved the screen 0.015 and stalled a live run.
        assert_eq!(
            m.cross_toward(&[exit("l19")]),
            Some(Crossing::Step { to: "l10sub7".into(), toward: "l10_path_to_l19".into() })
        );
    }

    #[test]
    fn standing_on_the_road_out_means_leave() {
        let mut m = WorldMap::new();
        m.fold(&inside_dump("l10", "l10_path_to_l19", "Road to Gipsyville crypt",
            vec![node("l10sub7", "Ulrome east guard post")], vec![exit("l19")]));
        assert_eq!(m.cross_toward(&[exit("l19")]), Some(Crossing::Leave { to: "l19".into() }));
    }

    #[test]
    fn an_unknown_interior_is_explored_rather_than_refused() {
        // The normal early state: fog reveals one hop, so on arrival there is no route to anywhere.
        // Refusing here is what left a run standing in a village with nothing to do.
        let mut m = WorldMap::new();
        m.fold(&inside_dump("l10", "l10sub6", "Ulrome guard post",
            vec![node("l10sub7", "Ulrome east guard post")], vec![exit("l19")]));
        assert_eq!(
            m.cross_toward(&[exit("l19")]),
            Some(Crossing::Explore { to: "l10sub7".into(), toward: "l10_path_to_l19".into() })
        );
    }

    #[test]
    fn crossing_says_nothing_on_the_surface() {
        // `inside()` is None out here, and a Crossing plan would send the run walking into exits
        // that do not exist.
        let mut m = WorldMap::new();
        m.fold(&dump("l19", "Gipsyville crypt", vec![node("l10", "Ulrome — level 6 village")]));
        assert_eq!(m.cross_toward(&[exit("l19")]), None);
    }

    #[test]
    fn folding_records_both_ends_of_an_edge() {
        let mut m = WorldMap::new();
        m.fold(&dump("start", "Cottam campfire", vec![node("l1", "Weedley Copse — level 0 crypt")]));
        assert!(m.get("start").unwrap().neighbours.contains("l1"));
        assert!(m.get("l1").unwrap().neighbours.contains("start"));
        // We have stood on start, but only heard about l1.
        assert!(m.get("start").unwrap().visited);
        assert!(!m.get("l1").unwrap().visited);
    }

    #[test]
    fn a_neighbours_dump_does_not_claim_to_know_its_hidden_count() {
        let mut m = WorldMap::new();
        m.fold(&dump("start", "Cottam campfire", vec![node("l1", "somewhere")]));
        // Only a dump taken AT l1 can say how much l1 is hiding.
        assert_eq!(m.get("l1").unwrap().hidden, None);
        assert_eq!(m.get("start").unwrap().hidden, Some(0));
    }

    #[test]
    fn visiting_later_upgrades_what_we_know_without_losing_edges() {
        let mut m = WorldMap::new();
        m.fold(&dump("start", "Cottam campfire", vec![node("l1", "Weedley Copse — level 0 crypt")]));
        let mut second = dump("l1", "Weedley Copse — level 0 crypt", vec![node("l2", "far field")]);
        second.hidden = 2;
        m.fold(&second);
        let l1 = m.get("l1").unwrap();
        assert!(l1.visited);
        assert_eq!(l1.hidden, Some(2));
        assert!(l1.neighbours.contains("start"), "the earlier edge must survive");
        assert!(l1.neighbours.contains("l2"));
    }

    #[test]
    fn level_and_anomaly_eligibility_come_from_the_heading() {
        let mut m = WorldMap::new();
        m.fold(&dump(
            "start",
            "camp",
            vec![node("l4", "Grim Barrow — level 4 crypt"), node("l1", "Weedley Copse — level 0 crypt")],
        ));
        assert_eq!(m.get("l4").unwrap().level(), Some(4));
        assert!(m.get("l4").unwrap().triggers_anomaly());
        // Level 0 is combat, but nowhere near the > 3 the event requires.
        assert!(!m.get("l1").unwrap().triggers_anomaly());
        // A campfire has no level at all.
        assert!(!m.get("start").unwrap().has_combat());
    }

    #[test]
    fn a_level_four_node_inside_a_subworld_does_not_trigger_it() {
        // `world_evil.lua:16` requires `not location.parentNode`. This is the case that would
        // otherwise send the run into a forest or mausoleum expecting an anomaly that cannot fire.
        let mut m = WorldMap::new();
        let mut inside = dump("f1", "Deep Wood — level 4 crypt", vec![node("f2", "Deeper Wood — level 5 crypt")]);
        inside.subworld = Some(("forest".into(), "Lost Woods".into()));
        m.fold(&inside);
        assert_eq!(m.get("f1").unwrap().parent.as_deref(), Some("forest"));
        assert!(!m.get("f1").unwrap().triggers_anomaly());
        assert!(!m.get("f2").unwrap().triggers_anomaly(), "neighbours are in the same subworld");
    }

    #[test]
    fn a_container_is_learned_from_being_inside_it_not_from_its_heading() {
        // The live blunder: `Eight Timberland — level 4 forest` reads exactly like a fight, and
        // clicking its area button entered the forest instead of starting one.
        let mut m = WorldMap::new();
        m.fold(&dump("l39", "Eight Timberland — level 4 forest", vec![]));
        assert!(!m.get("l39").unwrap().subworld_container, "nothing has told us yet");
        assert!(m.get("l39").unwrap().has_combat(), "and the heading is no help");

        let mut inside = dump("l39sub8", "Eight Timberland crossroads", vec![]);
        inside.subworld = Some(("l39".into(), "Eight Timberland — level 4 forest".into()));
        m.fold(&inside);
        assert!(m.get("l39").unwrap().subworld_container);
        assert_eq!(m.inside(), Some("l39"));
    }

    #[test]
    fn leaving_a_subworld_picks_the_exit_nearest_the_target() {
        use crate::observe::adjacency::Exit;
        let mut m = WorldMap::new();
        // Outside: goal — near — container, and a far dead end.
        m.fold(&dump("near", "a road", vec![node("goal", "Grim Barrow — level 4 crypt"), node("cave", "a forest")]));
        m.fold(&dump("far", "elsewhere", vec![node("cave", "a forest")]));
        let mut inside = dump("cavesub1", "a clearing", vec![]);
        inside.subworld = Some(("cave".into(), "a forest".into()));
        inside.exits = vec![
            Exit { x: 0.0, y: 0.0, to_key: "far".into(), to_heading: "elsewhere".into() },
            Exit { x: 0.0, y: 0.0, to_key: "near".into(), to_heading: "a road".into() },
        ];
        m.fold(&inside);
        assert_eq!(m.exit_toward(&inside.exits).as_deref(), Some("near"), "the exit that gets us closer");
    }

    #[test]
    fn exits_name_places_in_the_parent_world_not_the_subworld() {
        let mut m = WorldMap::new();
        let mut inside = dump("f1", "Deep Wood", vec![]);
        inside.subworld = Some(("forest".into(), "Lost Woods".into()));
        inside.exits =
            vec![Exit { x: 0.0, y: 0.0, to_key: "l9".into(), to_heading: "Open Road".into() }];
        m.fold(&inside);
        // Recorded, but NOT marked as sitting inside the subworld we are currently in.
        assert_eq!(m.get("l9").unwrap().heading, "Open Road");
        assert_eq!(m.get("l9").unwrap().parent, None);
    }

    #[test]
    fn routing_prefers_a_trigger_node_over_plain_exploration() {
        let mut m = WorldMap::new();
        m.fold(&dump(
            "start",
            "camp",
            vec![node("l4", "Grim Barrow — level 4 crypt"), node("l2", "Quiet Glade meadow")],
        ));
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::OpenTheAnomaly);
        assert_eq!(plan.target, "l4");
    }

    #[test]
    fn once_the_anomaly_has_opened_the_trigger_is_no_longer_a_goal() {
        let mut m = WorldMap::new();
        // Standing on `start`, which the anomaly promoted to the portal under us.
        m.fold(&dump("start", "camp", vec![node("l4", "Grim Barrow — level 4 crypt")]));
        m.hell = Some(0.1); // the anomaly already opened
        let plan = m.next_target().unwrap();
        assert_ne!(plan.reason, Goal::OpenTheAnomaly, "chasing a spent trigger wastes the run");
    }

    #[test]
    fn the_anomaly_is_known_before_any_dump_names_it() {
        // `start` is promoted to the portal in place, so once `hell` is nonzero we know where to go
        // without exploring for it -- and after corruption, `start` is usually "(unheaded)" in our
        // map because we only heard of it through save flags.
        let mut m = WorldMap::new();
        m.fold(&dump("l39", "Eight Timberland — level 4 forest", vec![node("l29", "Rookdale — level 3 crypt")]));
        m.entry("start").corrupted = true;
        m.hell = Some(0.1);
        assert_eq!(m.anomaly().map(|p| p.key.as_str()), Some("start"));
        assert!(m.anomaly_is_assumed(), "no heading has confirmed it yet");
        assert_eq!(m.next_target().unwrap().reason, Goal::Anomaly);
    }

    #[test]
    fn a_seen_anomaly_heading_beats_the_start_prior() {
        // The portal is not always `start` -- that is one generator's doing. Wherever a dump names
        // an `anomaly`, that is the answer and the prior must not override it.
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("start", "Cottam campfire"), node("rift", "The Maw anomaly")]));
        m.hell = Some(0.1);
        assert_eq!(m.anomaly().map(|p| p.key.as_str()), Some("rift"));
        assert!(!m.anomaly_is_assumed(), "a heading confirmed it");
    }

    #[test]
    fn beating_the_anomaly_ends_the_search_for_it() {
        // The objective flag is `start_corrupt`, written by `setAreaComplete` for a corrupt area.
        let mut m = WorldMap::new();
        m.fold(&dump("l39", "a forest", vec![]));
        m.hell = Some(0.1);
        m.entry("start");
        assert!(m.anomaly().is_some());
        m.entry(ANOMALY_KEY).completed_corrupt = true;
        assert!(m.anomaly_beaten());
        assert!(m.anomaly().is_none(), "nothing left to go and fight");
    }

    #[test]
    fn the_anomaly_itself_outranks_everything() {
        let mut m = WorldMap::new();
        m.fold(&dump(
            "start",
            "camp",
            vec![node("l4", "Grim Barrow — level 4 crypt"), node("hp", "The Rift anomaly")],
        ));
        // There is no portal to go to until the anomaly is actually open.
        assert!(m.anomaly().is_none());
        m.hell = Some(0.1);
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Anomaly);
        assert_eq!(plan.target, "hp", "a named portal beats the `start` prior");
    }

    #[test]
    fn exploration_prefers_unvisited_then_the_most_hidden() {
        let mut m = WorldMap::new();
        let mut a = dump("start", "camp", vec![node("l1", "a meadow"), node("l2", "b meadow")]);
        a.hidden = 0;
        m.fold(&a);
        // Visit l1 and find it still hiding two neighbours.
        let mut b = dump("l1", "a meadow", vec![]);
        b.hidden = 2;
        m.fold(&b);
        // l2 is unvisited, so it wins over l1 despite l1's hidden neighbours.
        m.here = Some("l1".into());
        let plan = m.next_target().unwrap();
        assert_eq!(plan.target, "l2");
        assert_eq!(plan.reason, Goal::Explore);
    }

    #[test]
    fn we_never_propose_travelling_to_where_we_already_are() {
        let mut m = WorldMap::new();
        let mut a = dump("l4", "Grim Barrow — level 4 crypt", vec![]);
        a.hidden = 1;
        m.fold(&a);
        // Standing on the only trigger node, the plan must not be to travel to it.
        assert_ne!(m.next_target().map(|p| p.target), Some("l4".into()));
    }

    #[test]
    fn a_hop_steps_toward_a_distant_target_rather_than_naming_it() {
        // start — l1 — l4(level 4). Standing at start, the move is to l1, but the PLAN is still
        // the trigger node: where the map does not accumulate we could not select l4 at all, and
        // even elsewhere it may be off-screen.
        let mut m = WorldMap::new();
        m.fold(&dump("start", "camp", vec![node("l1", "a crypt — level 0 crypt")]));
        m.fold(&dump(
            "l1",
            "a crypt — level 0 crypt",
            vec![node("start", "camp"), node("l4", "Grim Barrow — level 4 crypt")],
        ));
        m.here = Some("start".into());
        let hop = m.next_hop().unwrap();
        assert_eq!(hop.step, "l1", "step to the neighbour on the way");
        assert_eq!(hop.plan.target, "l4");
        assert_eq!(hop.plan.reason, Goal::OpenTheAnomaly);
    }

    #[test]
    fn an_adjacent_target_is_stepped_to_directly() {
        let mut m = WorldMap::new();
        m.fold(&dump("start", "camp", vec![node("l4", "Grim Barrow — level 4 crypt")]));
        let hop = m.next_hop().unwrap();
        assert_eq!(hop.step, "l4");
        assert_eq!(hop.plan.target, "l4");
    }

    #[test]
    fn with_no_known_route_we_step_to_an_adjacent_frontier() {
        // The target was heard about from somewhere else and no path to it is mapped. Standing
        // still would be the wrong answer; stepping outward is what mends the gap.
        let mut m = WorldMap::new();
        m.fold(&dump("far", "elsewhere", vec![node("l4", "Grim Barrow — level 4 crypt")]));
        let mut here = dump("start", "camp", vec![node("l2", "Quiet Glade meadow")]);
        here.hidden = 1;
        m.fold(&here);
        m.here = Some("start".into());
        let hop = m.next_hop().unwrap();
        assert_eq!(hop.plan.target, "l4", "the goal is unchanged");
        assert_eq!(hop.step, "l2", "but the move is the only way onward we know");
        assert_eq!(hop.plan.reason, Goal::OpenTheAnomaly);
    }

    /// start branches two ways to the goal: a short way through `woods`, a long way round.
    fn two_routes() -> WorldMap {
        let mut m = WorldMap::new();
        m.fold(&dump("start", "camp", vec![node("woods", "Mistwood forest"), node("a", "a meadow")]));
        m.fold(&dump("woods", "Mistwood forest", vec![node("start", "camp"), node("goal", "Grim Barrow — level 4 crypt")]));
        m.fold(&dump("a", "a meadow", vec![node("start", "camp"), node("b", "b meadow")]));
        m.fold(&dump("b", "b meadow", vec![node("a", "a meadow"), node("goal", "Grim Barrow — level 4 crypt")]));
        m.here = Some("start".into());
        m
    }

    #[test]
    fn a_known_lost_woods_is_routed_around_even_though_it_is_shorter() {
        let mut m = two_routes();
        // Before we know better, the short way through the woods wins.
        assert_eq!(m.next_hop().unwrap().step, "woods");
        m.entry("woods").avoid = true;
        let hop = m.next_hop().unwrap();
        assert_eq!(hop.step, "a", "go the long way rather than back into the woods");
        assert_eq!(hop.plan.target, "goal", "the goal is unchanged");
    }

    #[test]
    fn a_lost_woods_is_still_used_when_it_is_the_only_way() {
        let mut m = WorldMap::new();
        m.fold(&dump("start", "camp", vec![node("woods", "Mistwood forest")]));
        m.fold(&dump("woods", "Mistwood forest", vec![node("start", "camp"), node("goal", "Grim Barrow — level 4 crypt")]));
        m.here = Some("start".into());
        m.entry("woods").avoid = true;
        // Being swallowed again is bad; standing still forever is worse.
        assert_eq!(m.next_hop().unwrap().step, "woods");
    }

    #[test]
    fn an_avoided_place_is_never_chosen_as_a_destination() {
        let mut m = WorldMap::new();
        let mut d = dump("start", "camp", vec![node("woods", "Mistwood forest"), node("a", "a meadow")]);
        d.hidden = 0;
        m.fold(&d);
        m.entry("woods").avoid = true;
        let plan = m.next_target().unwrap();
        assert_eq!(plan.target, "a", "exploring INTO a known lost woods is never the plan");
    }

    #[test]
    fn the_save_flags_name_which_places_to_avoid() {
        // `overworld/events/arrived/lost_woods.lua:25` writes lost_woods_known_<key> on the way in.
        let save = crate::game::save::parse(
            r#"return {
                overworld = {
                    playerLocation = "start",
                    completedAreas = { start = true },
                    areaFlags = { hell = 0, lost_woods_known_l4 = 1, l7_explored = 0 },
                },
            }"#,
        )
        .unwrap();
        let mut m = WorldMap::new();
        m.fold(&dump("start", "camp", vec![node("l4", "Mistwood forest"), node("l7", "a village")]));
        m.apply_save(&save);
        assert!(m.get("l4").unwrap().avoid, "l4 swallowed us once");
        assert!(!m.get("l7").unwrap().avoid, "an _explored flag is not a lost woods");
        assert_eq!(m.anomaly_available(), Some(true));
    }

    /// Real headings from the captured island: a campfire and a village both adjacent.
    fn hurt_at_l1() -> WorldMap {
        let mut m = WorldMap::new();
        m.fold(&dump(
            "l1",
            "Weedley Copse crypt",
            vec![
                node("start", "Cottam campfire"),
                node("l10", "Ulrome village"),
                node("l4", "Bainton Clump — level 1 forest"),
            ],
        ));
        m.note_health(crate::rest::Health { current: 12, max: 12 }, crate::rest::Health { current: 7, max: 12 });
        m
    }

    #[test]
    fn losing_four_health_sends_us_to_rest_before_exploring() {
        let m = hurt_at_l1();
        assert!(m.wants_rest());
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Rest);
        assert_eq!(plan.target, "start", "the campfire, not the village");
    }

    #[test]
    fn a_corrupted_village_is_not_a_rest_stop() {
        // Met live: `Ulrome — level 6 village [corrupted]`. The heading still says "village", so
        // without the corruption check the run walks to an inn that is under attack or destroyed.
        let mut m = hurt_at_l1();
        m.entry("start").used = true; // the campfire is spent, so only the village is left
        m.gold = 50;
        assert_eq!(m.next_target().unwrap().target, "l10");
        m.entry("l10").corrupted = true;
        assert_ne!(m.next_target().unwrap().reason, Goal::Rest, "its inn is overrun");

        // Locked, not lost: fighting the corruption out frees the inn again.
        m.entry("l10").completed = true;
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Rest);
        assert_eq!(plan.target, "l10", "a freed inn is a rest stop again");
    }

    #[test]
    fn a_used_campfire_is_skipped_for_the_inn_when_we_carry_no_fuel() {
        // The real island: `start_used = true`. Walking there with no firewood restores nothing, so
        // the ten gold at Ulrome is the honest choice.
        let mut m = hurt_at_l1();
        m.entry("start").used = true;
        m.gold = 50;
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Rest);
        assert_eq!(plan.target, "l10", "a used campfire without fuel is a wasted walk");

        // Firewood puts it back on the table, and it outranks paying.
        m.fuel = 2;
        assert_eq!(m.next_target().unwrap().target, "start");
    }

    #[test]
    fn with_nothing_usable_resting_is_not_a_plan_at_all() {
        let mut m = hurt_at_l1();
        m.entry("start").used = true;
        m.gold = 0;
        // Used campfire, no fuel, no gold: better to get on with exploring than walk somewhere
        // that will turn us away.
        assert_ne!(m.next_target().unwrap().reason, Goal::Rest);
    }

    #[test]
    fn a_village_is_not_a_plan_without_the_ten_gold() {
        let mut m = hurt_at_l1();
        // Strip the campfire so only the village remains, then arrive skint.
        m.places.remove("start");
        m.gold = 9;
        let plan = m.next_target().unwrap();
        assert_ne!(plan.reason, Goal::Rest, "nine gold buys a wasted walk and no health");
        m.gold = 10;
        assert_eq!(m.next_target().unwrap().reason, Goal::Rest);
    }

    #[test]
    fn healing_up_clears_the_intent() {
        let mut m = hurt_at_l1();
        m.rested(crate::rest::Health { current: 12, max: 12 });
        assert!(!m.wants_rest());
        assert_ne!(m.next_target().unwrap().reason, Goal::Rest);
    }

    /// **Reverses `the_anomaly_still_outranks_resting`**, on the user's instruction: resting is the
    /// highest priority outside combat whenever health is low.
    ///
    /// The old assertion read "health is worth a detour, but not at the cost of the objective",
    /// which is the wrong trade in this game. The anomaly does not expire and health does not come
    /// back on its own, so the detour costs only time — while arriving at a level 8 fight on a third
    /// health costs the save. Kept as an explicit test rather than deleted, so the reversal is
    /// visible to whoever reads this next.
    #[test]
    fn resting_outranks_even_a_live_anomaly() {
        let mut m = hurt_at_l1();
        m.fold(&dump("l1", "Weedley Copse crypt", vec![node("hp", "The Rift anomaly")]));
        m.hell = Some(0.1);
        assert_eq!(m.next_target().unwrap().reason, Goal::Rest);
    }

    /// The other half of that trade: with nowhere to rest we still move. But we move to the
    /// **cheapest fight**, not to the objective — and specifically NOT to the anomaly, whose heading
    /// carries no level and so once sorted as the gentlest thing on the map.
    #[test]
    fn with_nowhere_safe_we_take_the_cheapest_fight_not_the_anomaly() {
        let mut m = hurt_at_l1();
        m.fold(&dump("l1", "Weedley Copse crypt", vec![node("hp", "The Rift anomaly")]));
        m.hell = Some(0.1);
        m.gold = 0; // no inn will serve us
        m.fuel = 0; // and no campfire fuel
        for p in m.places.values_mut() {
            p.corrupted = true; // every village locked behind its own clearing
            p.completed = false;
            p.visited = true;
            p.hidden = Some(0); // and nothing left to explore
        }
        let plan = m.next_target().unwrap();
        assert_ne!(plan.target, "hp", "the anomaly is the one fight we must not pick as 'easiest'");
        assert!(matches!(plan.reason, Goal::EasiestHostile { .. }), "got {:?}", plan.reason);

        // Once health is back, the anomaly is the objective again.
        m.rested(crate::rest::Health { current: 12, max: 12 });
        assert_eq!(m.next_target().unwrap().reason, Goal::Anomaly);
    }

    /// Hurt, and the nearest anomaly trigger is an uncleared corrupted village: prefer the other
    /// one, because getting back out of the corrupted one costs a fight.
    #[test]
    fn while_hurt_a_hostile_subworld_is_not_the_way_to_open_the_anomaly() {
        let mut m = hurt_at_l1();
        m.hell = Some(0.0); // anomaly still to be opened
        // Take resting off the table, so the planner reaches the branch under test rather than
        // simply walking to the campfire the fixture provides.
        for p in m.places.values_mut() {
            p.used = true;
        }
        m.gold = 0;
        m.fuel = 0;
        m.fold(&dump(
            "l1",
            "Weedley Copse crypt",
            vec![node("v1", "Ulrome — level 6 village"), node("c1", "Rookdale — level 9 crypt")],
        ));
        {
            let v = m.entry("v1");
            v.corrupted = true;
            v.completed = false;
        }
        // Both trigger the anomaly (level > 3), and while hurt we take NEITHER. The crypt used to be
        // chosen here, on the reading that only the corrupted village was dangerous -- which was the
        // whole bug: a level 9 crypt is not a safe consolation prize, it is the worse fight of the
        // two. Exploring is preferred while any free frontier remains, because a frontier might hold
        // the rest site that fixes the actual problem.
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Explore);
        assert_ne!(plan.target, "v1");
        assert_ne!(plan.target, "c1");

        // With the map fully explored there is no such escape, and the choice moves to the
        // cheapest-fight ranking. It takes neither of the two anomaly triggers: the fixture's `l4`
        // is a level 1 forest, and a level 1 anything is a better place to bleed than a level 6
        // village or a level 9 crypt.
        for p in m.places.values_mut() {
            p.visited = true;
            p.hidden = Some(0);
        }
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::EasiestHostile { level: Some(1) });
        assert_eq!(plan.target, "l4");

        // And the original claim, asserted where it actually lives now. Between the two triggers the
        // corrupted village still loses -- `Risk::Corrupt` is the worst rung there is, so the crypt
        // wins despite being level 9 to the village's 6.
        assert!(m.get("c1").unwrap().risk() < m.get("v1").unwrap().risk());
    }

    /// ...but with genuinely nothing else to do, being hurt must not stop the run entirely.
    ///
    /// Note what "nothing else" has to mean now that the rule is global: exploring counts as
    /// something else, and is *preferred*, because a frontier might hold the rest site we are
    /// looking for. So this fixture leaves no frontier at all — every place visited, nothing
    /// hidden — which is the only state in which diving into hostile ground is the best move left.
    #[test]
    fn a_hostile_trigger_is_still_taken_when_it_is_the_only_one() {
        let mut m = hurt_at_l1();
        m.hell = Some(0.0);
        m.fold(&dump("l1", "Weedley Copse crypt", vec![node("v1", "Ulrome — level 6 village")]));
        // No rest anywhere, and every other neighbour too small to trigger the anomaly.
        for p in m.places.values_mut() {
            p.used = true;
            p.visited = true;
            p.hidden = Some(0); // no frontier left, so exploring is not an option
        }
        m.gold = 0;
        m.fuel = 0;
        {
            let v = m.entry("v1");
            v.corrupted = true;
            v.completed = false;
        }
        let plan = m.next_target().unwrap();
        assert_eq!(
            plan.reason,
            Goal::EasiestHostile { level: Some(1) },
            "with nowhere safe, the reason is 'cheapest fight', not 'objective'"
        );
        // `l4` is `Bainton Clump — level 1 forest` from the fixture, and it only became a candidate
        // when the filter started asking the arrival question: it is visited, so `hostile_to_enter`
        // says nothing about it, but its heading carries a level so a fight is still owed. Taking a
        // level 1 forest over a corrupted level 6 village is the point of the whole ranking.
        assert_eq!(plan.target, "l4", "a preference must not strand the run, nor pick the worst fight");
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

    #[test]
    fn the_two_passes_between_them_consider_every_place() {
        // `plan(true)` and `easiest_hostile` must tile the map: a place refused by both is a place
        // the planner can never reach, and a run that found only those would stop with `NoPlan`
        // while somewhere perfectly reachable sat on the map.
        //
        // Exercised rather than asserted about, by building a map on which EVERY place owes a fight
        // -- so the first pass has nothing at all and the second has to carry all of it.
        let mut m = WorldMap::new();
        m.fold(&dump(
            "here",
            "Somewhere crossroads",
            vec![
                node("c1", "Yokefleet — level 6 crypt"),
                node("f1", "Asselby Bush — level 4 forest"),
                node("v1", "Ulrome — level 6 village"),
            ],
        ));
        m.entry("v1").corrupted = true;
        m.entry("unheaded").key = "unheaded".into(); // known by key only, so `Arrival::Unknown`
        m.note_health_level(crate::rest::Health { current: 1, max: 20 });
        for p in m.places.values_mut() {
            p.used = true; // no rest available anywhere
        }
        assert!(
            m.places.values().filter(|p| p.key != "here").all(WorldMap::owes_a_fight),
            "fixture must leave the first pass nothing"
        );
        let plan = m.next_target().expect("the second pass must catch what the first refused");
        assert_eq!(plan.reason, Goal::EasiestHostile { level: Some(4) });
        assert_eq!(plan.target, "f1", "the forest, over two level 6s and an unknown");
    }

    /// The live failure of 2026-08-08, rebuilt from the map that run printed.
    #[test]
    fn a_free_shrine_beats_exploring_onto_a_level_6_crypt() {
        let mut m = WorldMap::new();
        m.fold(&dump(
            "l41",
            "Grimston crossroads",
            vec![
                node("l50", "Burtonfields — level 6 crypt"),
                node("shrine5", "Gembling shrine"),
            ],
        ));
        // 1/20 -- below half, so `health_is_low` sets the intent however we arrived.
        m.note_health_level(crate::rest::Health { current: 1, max: 20 });
        assert!(m.wants_rest());
        // No campfire and no village anywhere, which was the real map's situation: of 22 places, not
        // one was a rest site. So the `Rest` branch finds nothing and falls through -- and what it
        // falls through to used to be `Explore`, which had no danger term at all and picked the
        // crypt. The run walked onto it at 1/20 and stopped with `TooHurtToFight`.
        let plan = m.next_target().unwrap();
        assert_ne!(plan.target, "l50", "a level 6 crypt is not an exploration");
        assert_eq!(plan.target, "shrine5");
        // A shrine, not merely a safe square: `Gembling shrine` has no level, so arriving is free,
        // and an uncorrupted shrine pays in wildcard tiles -- which is what a hurt run wants most.
        assert_eq!(plan.reason, Goal::Shrine);
    }

    /// An unvisited subworld container counts as hostile, because a bandit camp is an ordinary
    /// forest until you are standing in it (`overworld/generators/world.lua:466-475`).
    /// Hurt and standing on a node that blocks going onward: back out rather than fight.
    ///
    /// The live failure this comes from: at 0 health, crossing Ulrome toward the `l7` campfire, the
    /// run stepped onto `l10sub11` — a level 6 guard post — and `cross_toward` reported the fight as
    /// the only legal move. It was not. `canTravelToDirect` needs one endpoint complete, and the
    /// node behind us is.
    #[test]
    fn hurt_and_blocked_we_back_out_instead_of_fighting() {
        let exits = vec![Exit {
            to_key: "l10_path_to_l7".into(),
            to_heading: "Road to Greenoak".into(),
            x: 100.0,
            y: 100.0,
        }];
        let mut m = WorldMap::new();
        m.fold(&inside_dump(
            "l10",
            "l10sub11",
            "Ulrome south — level 6 guard post",
            vec![node("l10sub10", "Ulrome house")],
            exits.clone(),
        ));
        m.entry("l10sub10").completed = true;
        m.entry("l10sub10").visited = true;
        m.entry("l10sub11").completed = false;

        // Healthy: the fight underfoot is the only way onward, and we take it.
        assert_eq!(
            m.cross_toward(&exits),
            Some(Crossing::Fight { at: "l10sub11".into() })
        );

        // Hurt: backing out is legal and is the better move.
        m.note_health_level(crate::rest::Health { current: 0, max: 12 });
        assert_eq!(
            m.cross_toward(&exits),
            Some(Crossing::Retreat { to: "l10sub10".into() }),
            "the node we came from is complete, so stepping back is allowed"
        );
    }

    /// Hurt, nowhere to rest, and an **uncorrupted** shrine within reach: take the shrine, not the
    /// cheapest fight.
    ///
    /// It costs no fight and pays in wildcard tiles (`utils/blessings.lua:95-110`), which is the one
    /// thing that makes the *next* fight winnable. A hurt run choosing a level 1 skirmish over a
    /// free blessing would have the trade exactly backwards.
    #[test]
    fn an_uncorrupted_shrine_beats_the_cheapest_fight_while_hurt() {
        let mut m = hurt_at_l1();
        m.hell = Some(0.1); // anomaly open, so consecrating is possible
        m.fold(&dump(
            "l1",
            "Weedley Copse crypt",
            vec![node("shrine2", "Gransmoor shrine"), node("c1", "Rookdale — level 2 crypt")],
        ));
        // No rest anywhere, and nothing left to explore.
        for p in m.places.values_mut() {
            p.used = true;
            p.visited = true;
            p.hidden = Some(0);
        }
        m.gold = 0;
        m.fuel = 0;
        // Everything hostile EXCEPT the shrine.
        for p in m.places.values_mut() {
            if p.key != "shrine2" {
                p.corrupted = true;
                p.completed = false;
            }
        }
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Shrine);
        assert_eq!(plan.target, "shrine2");
    }

    /// The anomaly must never be picked as the "easiest" fight. Its heading has no `— level N`, so
    /// a naive `unwrap_or(0)` ranked the hardest fight on the map as the gentlest.
    #[test]
    fn the_anomaly_is_never_the_cheapest_fight() {
        let mut m = hurt_at_l1();
        m.hell = Some(0.1);
        m.fold(&dump(
            "l1",
            "Weedley Copse crypt",
            vec![node("hp", "The Rift anomaly"), node("c1", "Rookdale — level 9 crypt")],
        ));
        for p in m.places.values_mut() {
            p.corrupted = true;
            p.completed = false;
            p.used = true;
            p.visited = true;
            p.hidden = Some(0);
        }
        m.gold = 0;
        m.fuel = 0;
        let plan = m.next_target().unwrap();
        assert_ne!(plan.target, "hp", "the anomaly must never rank as the cheapest fight");
        // The fixture's `l4` is a level 1 forest, which is genuinely the cheapest thing here.
        assert_eq!(plan.reason, Goal::EasiestHostile { level: Some(1) });
        assert_eq!(plan.target, "l4");
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

    /// Resuming at low health must ask for a rest even though no drop was ever observed — the delta
    /// rule cannot see a fight that happened before the process started.
    #[test]
    fn a_resumed_run_at_low_health_wants_a_rest() {
        let mut m = WorldMap::default();
        assert!(!m.wants_rest(), "nothing observed yet");
        m.note_health_level(crate::rest::Health { current: 4, max: 12 });
        assert!(m.wants_rest(), "4/12 is below half");
    }

    #[test]
    fn exactly_half_is_not_yet_low_and_a_partial_heal_does_not_cancel() {
        let mut m = WorldMap::default();
        m.note_health_level(crate::rest::Health { current: 6, max: 12 });
        assert!(!m.wants_rest(), "half is the game's `fear` line, and it is strict");

        m.note_health_level(crate::rest::Health { current: 5, max: 12 });
        assert!(m.wants_rest());
        // Healing part-way must NOT cancel a rest we are still walking to.
        m.note_health_level(crate::rest::Health { current: 8, max: 12 });
        assert!(m.wants_rest(), "still short of full, so the errand stands");
        m.note_health_level(crate::rest::Health { current: 12, max: 12 });
        assert!(!m.wants_rest(), "full clears it");
    }

    #[test]
    fn an_unconsecrated_shrine_is_worth_the_detour() {
        // Real heading from the captured island.
        let mut m = WorldMap::new();
        m.fold(&dump(
            "l19",
            "Gipsyville — level 2 crypt",
            vec![node("shrine2", "Gransmoor shrine"), node("l29", "Rookdale — level 3 crypt")],
        ));
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Shrine);
        assert_eq!(plan.target, "shrine2");

        // Once there is nothing left to do there, it stops pulling us off course.
        //
        // This used to set `consecrated` alone, and that was not enough of a condition — it only
        // looked sufficient while `Pray` was unimplemented. Consecrating and praying are separate
        // actions with separate gates, and `showPrayButton` (`shrine.lua:98-102`) retires on
        // `areaUnused`, so a consecrated shrine nobody has prayed at still has a blessing waiting.
        m.entry("shrine2").consecrated = true;
        m.entry("shrine2").used = true;
        assert_ne!(m.next_target().unwrap().reason, Goal::Shrine);
    }

    #[test]
    fn a_prayed_shrine_stops_being_a_destination_while_the_anomaly_is_shut() {
        // The regression for a live stall. With `hell = 0` nothing can be consecrated, so a test of
        // `!consecrated` can never be discharged and the shrine stays "outstanding" forever. Praying
        // is the only thing available, and once it is done there is genuinely nothing left there.
        let mut m = WorldMap::new();
        m.fold(&dump(
            "shrine1",
            "Swanland shrine",
            vec![node("l10", "Trenwick — level 1 crypt")],
        ));
        m.entry("shrine1").completed = true;
        m.fold(&dump("l10", "Trenwick — level 1 crypt", vec![node("shrine2", "Foggathorpe shrine")]));

        // Standing at l10, the unprayed shrine1 next door is a fair target.
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Shrine);
        assert_eq!(plan.target, "shrine1");

        // Having prayed there, it must not pull us back.
        m.entry("shrine1").used = true;
        let plan = m.next_target().unwrap();
        assert_ne!(plan.target, "shrine1", "a prayed shrine is finished while hell == 0");
    }

    #[test]
    fn a_prayed_shrine_is_worth_returning_to_once_the_anomaly_opens() {
        // The other half: `used` retires `Pray`, not `Consecrate`. Once `hell ~= 0` the same shrine
        // has work again, and writing it off permanently would forfeit it.
        let mut m = WorldMap::new();
        m.fold(&dump("l10", "Trenwick — level 1 crypt", vec![node("shrine1", "Swanland shrine")]));
        m.entry("shrine1").completed = true;
        m.entry("shrine1").used = true;
        // Asserted on the *reason*, not the target: an unvisited shrine is still a frontier, so
        // exploration may legitimately route through it. What must not happen is going there
        // *because it is a shrine*, which is the claim that there is shrine work to do.
        assert_ne!(m.next_target().unwrap().reason, Goal::Shrine, "nothing to do there yet");

        // `hell ~= 0` is the anomaly being open, which is what unlocks consecration.
        m.hell = Some(0.1);
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Shrine);
        assert_eq!(plan.target, "shrine1", "consecrating is now possible");
    }

    #[test]
    fn two_finished_shrines_either_side_of_a_crypt_do_not_ping_pong() {
        // The stall exactly as it was met: shrine1 and shrine2 with l10 between them, the run
        // walking shrine1 -> l10 -> shrine1 -> l10 for twenty steps. The crypt was never fought
        // because it was only ever a waypoint, and stepping *back* to a completed shrine is always
        // legal, so departure was never blocked.
        let mut m = WorldMap::new();
        m.fold(&dump("shrine1", "Swanland shrine", vec![node("l10", "Trenwick — level 1 crypt")]));
        m.fold(&dump(
            "l10",
            "Trenwick — level 1 crypt",
            vec![node("shrine1", "Swanland shrine"), node("shrine2", "Foggathorpe shrine")],
        ));
        for k in ["shrine1", "shrine2"] {
            m.entry(k).completed = true;
            m.entry(k).used = true;
            // Both have been stood on, so neither is a frontier and exploration has no reason to
            // route through them either. That isolates the claim to the shrine branch.
            m.entry(k).visited = true;
        }
        // Standing on the crypt, with both shrines finished, nothing may send us back to one.
        let plan = m.next_target();
        if let Some(plan) = plan {
            assert_ne!(plan.reason, Goal::Shrine, "still treating a finished shrine as work");
            assert!(
                !plan.target.starts_with("shrine"),
                "went back to a finished shrine: {plan:?}"
            );
        }
    }

    #[test]
    fn a_shrine_we_walked_away_from_stops_being_a_destination() {
        // The second bounce, which `worth_a_trip` could not catch. shrine2's puzzle could not be
        // read, so the run left without praying — and because nothing was prayed, the game never set
        // `_used`. The shrine is therefore genuinely unused, genuinely nearest, and genuinely one the
        // driver will never enter again. The live run walked `l10 -> shrine2 -> l10` for thirty
        // steps until its budget ran out.
        let mut m = WorldMap::new();
        m.fold(&dump(
            "l10",
            "Trenwick — level 1 crypt",
            vec![node("shrine2", "Foggathorpe shrine"), node("shrine3", "Skipsea shrine")],
        ));
        for k in ["shrine2", "shrine3"] {
            m.entry(k).completed = true;
            m.entry(k).visited = true;
        }
        // Standing on the crypt: with nothing abandoned, the nearest unused shrine is fair game.
        assert_eq!(m.next_target().map(|p| p.reason), Some(Goal::Shrine));

        m.abandon("shrine2");
        let plan = m.next_target().expect("shrine3 is still worth the trip");
        assert_eq!(plan.reason, Goal::Shrine);
        assert_eq!(plan.target, "shrine3", "kept choosing the shrine it refuses to enter");

        // And with both spent, the shrine branch must yield entirely rather than pick the least-bad.
        m.abandon("shrine3");
        if let Some(plan) = m.next_target() {
            assert_ne!(plan.reason, Goal::Shrine, "no shrine is left to visit: {plan:?}");
        }
    }

    /// Exploring the dark must not step out of the subworld by the wrong door.
    #[test]
    fn walking_into_the_dark_still_avoids_the_other_exits() {
        // `l9`, live: crossing toward `l9_path_to_l19` with no known route, the fallback took any
        // neighbour and chose `l9_path_to_l1`. The run left the forest, arrived at a level 7
        // corrupted crypt, and was robbed of all 763 gold by a highwayman on the way.
        let exits = vec![
            Exit { x: 0.0, y: 0.0, to_key: "l19".into(), to_heading: "Dane village".into() },
            Exit { x: 0.0, y: 0.0, to_key: "l1".into(), to_heading: "Cowlam — level 7 crypt".into() },
        ];
        let mut m = WorldMap::new();
        m.fold(&inside_dump(
            "l9",
            "here",
            "Saltagh Park crossroads",
            vec![
                node("l9_path_to_l1", "Road to Cowlam"),
                node("l9sub26", "Saltagh Park road"),
                node("l9sub21", "Saltagh Park forest"),
            ],
            exits.clone(),
        ));
        m.note_health_level(crate::rest::Health { current: 1, max: 20 });

        // No route to `l9_path_to_l19` is known -- it is not even on the map yet -- so this is the
        // explore fallback, which is the code under test.
        match m.cross_toward(&exits) {
            Some(Crossing::Explore { to, .. }) => {
                assert_ne!(to, "l9_path_to_l1", "that is a way OUT, by the wrong door");
                assert_eq!(to, "l9sub26", "the road, and unvisited");
            }
            other => panic!("expected an explore step, got {other:?}"),
        }
    }

    /// Inside Ulrome, hurt, with 763 gold — the sandbox's own state, one node further on.
    ///
    /// `nodes` is what the dump reports as adjacent, so the inn is present or absent by the same
    /// mechanism the fog uses.
    fn inside_a_village(here: (&str, &str), nodes: Vec<Node>, gold: i64) -> WorldMap {
        let mut m = WorldMap::new();
        m.fold(&dump("l19", "Gipsyville crypt", vec![node("l10", "Ulrome village")]));
        m.fold(&inside_dump("l10", here.0, here.1, nodes, vec![exit("l19"), exit("l7")]));
        m.apply_save(
            &crate::game::save::parse(&format!("return {{ player = {{ gold = {gold} }} }}")).unwrap(),
        );
        m.note_health_level(crate::rest::Health { current: 1, max: 20 });
        m
    }

    #[test]
    fn a_hurt_run_in_a_village_heads_for_the_inn_rather_than_the_way_out() {
        // The whole point of entering. `cross_toward` only ever knew how to head for an exit, so a
        // rest errand walked in one door and out the other.
        let m = inside_a_village(
            ("l10sub1", "Ulrome well"),
            vec![node("l10sub4", "The Wobbly Cat inn"), node("l10_path_to_l7", "Road to Greenoak")],
            763,
        );
        assert_eq!(
            m.cross_toward(&[exit("l19"), exit("l7")]),
            Some(Crossing::Step { to: "l10sub4".into(), toward: "l10sub4".into() })
        );
    }

    #[test]
    fn standing_on_the_inn_is_where_the_crossing_ends() {
        let m = inside_a_village(
            ("l10sub4", "The Wobbly Cat inn"),
            vec![node("l10sub1", "Ulrome house")],
            763,
        );
        // Not `Leave`, not a step: the errand is the reason we are in here.
        assert_eq!(
            m.cross_toward(&[exit("l19"), exit("l7")]),
            Some(Crossing::Arrive { at: "l10sub4".into() })
        );
    }

    #[test]
    fn an_inn_we_cannot_pay_for_is_not_a_destination() {
        // `getCanRest` is a flat `getPlayerGold() >= 10` (`ui/rest.lua:49`). Below it, walking to the
        // bar buys a wasted trip and the fights on the way back, so cross as normal.
        let m = inside_a_village(
            ("l10sub1", "Ulrome well"),
            vec![node("l10sub4", "The Wobbly Cat inn"), node("l10_path_to_l7", "Road to Greenoak")],
            9,
        );
        match m.cross_toward(&[exit("l19"), exit("l7")]) {
            Some(Crossing::Step { toward, .. }) | Some(Crossing::Explore { toward, .. }) => {
                assert!(toward.starts_with("l10_path_to_"), "heading out, not to the bar: {toward}");
            }
            other => panic!("expected an exit crossing, got {other:?}"),
        }
    }

    #[test]
    fn a_village_whose_inn_is_still_fogged_is_searched_not_left() {
        // `store_inn` lands on whichever `<parent>sub<N>` the generator reaches first
        // (`village.lua:685`), so unlike an exit road there is no key to build and no route to plan:
        // the inn has to be seen. Until it is, the one move that must NOT be made is the exit —
        // which is exactly what the old fallback reached for, the entrance road being the nearest
        // thing to walk to.
        let m = inside_a_village(
            ("l10sub1", "Ulrome well"),
            vec![
                node("l10sub2", "Ulrome house"),
                node("l10_path_to_l19", "Road to Gipsyville"),
                node("l10_path_to_l7", "Road to Greenoak"),
            ],
            763,
        );
        assert_eq!(
            m.cross_toward(&[exit("l19"), exit("l7")]),
            Some(Crossing::Seek { to: "l10sub2".into() })
        );
    }

    #[test]
    fn an_inn_we_have_already_tried_stops_the_search() {
        // The bounce this project keeps rediscovering, in its newest disguise. `abandon` is the
        // driver's record of having had its go; without consulting it here, "the fog hides the inn"
        // and "the inn would not serve us" look identical from inside the village, and the run
        // searches a village it has already finished with until its budget runs out.
        let mut m = inside_a_village(
            ("l10sub1", "Ulrome well"),
            vec![node("l10sub4", "The Wobbly Cat inn"), node("l10_path_to_l7", "Road to Greenoak")],
            763,
        );
        m.abandon("l10sub4");
        match m.cross_toward(&[exit("l19"), exit("l7")]) {
            Some(Crossing::Step { toward, .. }) | Some(Crossing::Explore { toward, .. }) => {
                assert!(toward.starts_with("l10_path_to_"), "back to crossing: {toward}");
            }
            other => panic!("expected an exit crossing, got {other:?}"),
        }
    }

    #[test]
    fn a_healthy_run_crosses_a_village_without_stopping_at_the_bar() {
        // The inn is only a destination while a rest is wanted. Otherwise a village is a subworld
        // like any other and the exit is the whole objective.
        let mut m = inside_a_village(
            ("l10sub1", "Ulrome well"),
            vec![node("l10sub4", "The Wobbly Cat inn"), node("l10_path_to_l7", "Road to Greenoak")],
            763,
        );
        m.note_health_level(crate::rest::Health { current: 20, max: 20 });
        assert!(!m.wants_rest());
        match m.cross_toward(&[exit("l19"), exit("l7")]) {
            Some(Crossing::Step { toward, .. }) | Some(Crossing::Explore { toward, .. }) => {
                assert!(toward.starts_with("l10_path_to_"), "straight through: {toward}");
            }
            other => panic!("expected an exit crossing, got {other:?}"),
        }
    }

    #[test]
    fn an_abandoned_node_is_still_stepped_on_when_it_is_the_only_way() {
        // The load-bearing assumption behind `Run::backed_out_of`. Backing out of a blocking node
        // abandons it, so the router looks for another way round — but if there is no other way, the
        // unfiltered fallback has to bring us back to it, or the run wanders instead of fighting and
        // the second strike never fires.
        //
        // `l9sub16` live: a level 1 spider nest, the only neighbour, on the only road to the exit.
        let mut m = WorldMap::new();
        m.fold(&inside_dump(
            "l9",
            "l9sub11",
            "Saltagh Park spider nest",
            vec![node("l9sub16", "Saltagh Park — level 1 spider nest")],
            vec![exit("l19")],
        ));
        m.note_health_level(crate::rest::Health { current: 1, max: 20 });
        m.abandon("l9sub16");
        // Not `None`, which would be a stall, and not a wander: the one neighbour there is.
        match m.cross_toward(&[exit("l19")]) {
            Some(Crossing::Explore { to, .. }) => assert_eq!(to, "l9sub16"),
            other => panic!("expected the fallback to step onto it anyway, got {other:?}"),
        }
    }

    #[test]
    fn abandoning_the_blocker_takes_the_other_road_when_there_is_one() {
        // The first strike's whole purpose. With a second way to the exit, retreating and retiring
        // the blocker has to actually change the route -- otherwise backing out is theatre and the
        // run bounces, which is exactly what five laps of `l9sub16` were.
        let mut m = WorldMap::new();
        m.fold(&inside_dump(
            "l9",
            "l9sub11",
            "Saltagh Park spider nest",
            vec![
                node("l9sub16", "Saltagh Park — level 1 spider nest"),
                node("l9sub18", "Saltagh Park road"),
            ],
            vec![exit("l19")],
        ));
        m.note_health_level(crate::rest::Health { current: 1, max: 20 });
        m.abandon("l9sub16");
        match m.cross_toward(&[exit("l19")]) {
            Some(Crossing::Explore { to, .. }) | Some(Crossing::Step { to, .. }) => {
                assert_eq!(to, "l9sub18", "round the blocker, by the road");
            }
            other => panic!("expected a step round it, got {other:?}"),
        }
    }

    /// The dev's crossing rule, one pass at a time. The ordering IS the rule, so each layer is
    /// exercised by making the one above it unavailable.
    #[test]
    fn a_forest_is_crossed_by_the_road_unless_the_road_is_blocked() {
        // Two ways from `here` to the exit: a paved one through `road1`, and a wooded one through
        // `wood1`. Everything peaceful to start with.
        let build = |road_fights: bool, wood_fights: bool| {
            let mut m = WorldMap::new();
            m.fold(&inside_dump(
                "l9",
                "here",
                "Saltagh Park crossroads",
                vec![
                    node("road1", if road_fights { "Saltagh Park — level 4 road" } else { "Saltagh Park road" }),
                    node("wood1", if wood_fights { "Saltagh Park — level 4 forest" } else { "Saltagh Park forest" }),
                ],
                vec![Exit { x: 0.0, y: 0.0, to_key: "l19".into(), to_heading: "Dane village".into() }],
            ));
            for k in ["road1", "wood1"] {
                m.entry(k).neighbours.insert("exit".into());
                m.entry("exit").neighbours.insert(k.into());
            }
            m.entry("exit").heading = "Road to Dane village".into();
            m
        };

        // 1. Both clear: the road wins on preference alone.
        assert_eq!(
            build(false, false).first_step_toward("here", "exit", false).as_deref(),
            Some("road1"),
            "paved and peaceful is the ordinary crossing"
        );

        // 2. A fight on the road and a clear way round: take the detour. This is the ONLY case the
        //    rule allows leaving the path, and it is the whole reason the second pass exists.
        assert_eq!(
            build(true, false).first_step_toward("here", "exit", false).as_deref(),
            Some("wood1"),
            "a combat-less pathway beats a blocked road"
        );

        // 3. A fight on the road and a fight in the woods too: stay on the road. Nothing is gained
        //    by wandering off it to pay the same price.
        assert_eq!(
            build(true, true).first_step_toward("here", "exit", false).as_deref(),
            Some("road1"),
            "if both cost a fight, the road is still the road"
        );

        // 4. And the detour is not preferred merely for being clear when the road is clear too --
        //    that would be pass 2 beating pass 1, which would make the preference meaningless.
        assert_eq!(
            build(false, true).first_step_toward("here", "exit", false).as_deref(),
            Some("road1")
        );
    }

    /// **Never stall.** A forest whose every route costs a fight still has a route.
    #[test]
    fn every_way_out_being_a_fight_is_still_a_way_out() {
        // A spider forest generates nests across the interior, so this is not a contrived shape: it
        // is what `l9` looked like. A router that returns `None` here has stalled the run, which is
        // worse than the fight it was avoiding -- and worse than saying so, because `cross_toward`
        // and the health gate both get their own say afterwards.
        let mut m = WorldMap::new();
        m.fold(&inside_dump(
            "l9",
            "here",
            "Saltagh Park crossroads",
            vec![node("nest1", "Saltagh Park — level 4 spider nest")],
            vec![Exit { x: 0.0, y: 0.0, to_key: "l19".into(), to_heading: "Dane village".into() }],
        ));
        m.entry("nest1").neighbours.insert("exit".into());
        m.entry("exit").heading = "Road to Dane village".into();
        m.note_health_level(crate::rest::Health { current: 1, max: 20 });

        assert_eq!(
            m.first_step_toward("here", "exit", false).as_deref(),
            Some("nest1"),
            "the last pass tolerates anything, because stalling is not an option"
        );
    }

    /// The fifth bounce, and the first inside a subworld: `l9sub13 -> l9sub11 -> l9sub13`.
    #[test]
    fn a_chest_is_not_on_the_way_to_anywhere_while_we_are_hurt() {
        // `l9sub13` is `Saltagh Park — level 1 chest`. Opening one is a fight, not a reward
        // (`forest.lua:30-38`), and at level 1 it carries a 1-in-30 chance of being a mimic
        // (`utils/combat.lua:442`). An unopened chest is also incomplete, so `canTravelToDirect`
        // will not let us step off it onto another incomplete node -- which is what turned a route
        // through one into a cycle.
        let mut m = WorldMap::new();
        m.fold(&inside_dump(
            "l9",
            "l9sub11",
            "Saltagh Park spider nest",
            vec![
                node("l9sub13", "Saltagh Park — level 1 chest"),
                node("l9sub16", "Saltagh Park road"),
            ],
            vec![Exit { x: 0.0, y: 0.0, to_key: "l19".into(), to_heading: "Dane village".into() }],
        ));
        m.entry("l9sub11").completed = true;
        m.entry("l9sub16").neighbours.insert("l9_path_to_l19".into());
        m.entry("l9_path_to_l19").heading = "Road to Dane village".into();

        // Healthy, a chest is just another node and may be routed through.
        assert!(m.get("l9sub13").unwrap().is_chest());
        let healthy = m.first_step_toward("l9sub11", "l9_path_to_l19", false);
        assert!(healthy.is_some(), "a route exists at full health");

        // Hurt, it is skipped -- as a WAYPOINT, which is the case that bit us. The route goes round.
        m.note_health_level(crate::rest::Health { current: 1, max: 20 });
        assert_eq!(
            m.first_step_toward("l9sub11", "l9_path_to_l19", false).as_deref(),
            Some("l9sub16"),
            "the road, not the chest"
        );

        // And a chest already opened is an ordinary cleared node again.
        m.entry("l9sub13").completed = true;
        assert!(!m.get("l9sub13").unwrap().is_chest() || m.get("l9sub13").unwrap().completed);
    }

    /// The fourth bounce, live on 2026-08-08: `l35 -> l41 -> l35`, until the run failed.
    #[test]
    fn a_node_we_have_already_stood_on_is_not_somewhere_to_explore() {
        // Rebuilt from that run's map. `l41` is visited and complete, with neighbours still under
        // cloud, so `is_frontier` was true for ever -- and standing there cannot change it, because
        // the hidden count is a cloud count and arriving does not lift clouds.
        //
        // The loop needed both halves. From `l35` the planner explored to `l41`; from `l41` the
        // branch excludes `here`, found nothing else free, fell through to `easiest_hostile` and
        // routed to `l9` -- whose first hop is `l35`. Neither plan was wrong on its own.
        let mut m = WorldMap::new();
        m.fold(&dump(
            "l35",
            "Thorpe crypt",
            vec![node("l41", "Grimston crossroads"), node("l9", "Saltagh Park — level 1 forest")],
        ));
        for k in ["l35", "l41"] {
            let p = m.entry(k);
            p.completed = true;
            p.visited = true;
            p.hidden = Some(2); // neighbours the game refused to name
        }
        m.note_health_level(crate::rest::Health { current: 1, max: 20 });

        // `l41` is still a frontier in the sense that matters for conclusions -- we cannot say what
        // is next to it -- and that stays true.
        assert!(m.get("l41").unwrap().is_frontier(), "hidden neighbours are still unknown");
        // But it is not somewhere to go. The only plan left is the cheapest fight, and crucially it
        // is the SAME plan from either end of the old cycle, so the run makes progress.
        let from_l35 = m.next_target().unwrap();
        assert_eq!(from_l35.reason, Goal::EasiestHostile { level: Some(1) });
        assert_eq!(from_l35.target, "l9");
        m.here = Some("l41".into());
        assert_eq!(m.next_target().unwrap().target, "l9", "the plan must not flip when we move");
    }

    /// The third bounce, live on 2026-08-08: `l10 -> shrine2 -> l10`, twelve times.
    #[test]
    fn a_used_but_unconsecrated_shrine_is_a_real_errand_and_ends_when_it_is_done() {
        // Both shrines prayed at in an earlier run and neither consecrated -- straight out of the
        // sandbox save, which carries `shrine2_used = true` and `shrine3_used = true` with no
        // `_consecrated` flag for either. `worth_a_trip`'s second clause is therefore true forever,
        // and the planner kept routing to them while `drive` declined every arrival, because the
        // branch that acts on a shrine needed `!used`.
        //
        // The clause was right and the driver was missing. Consecrating is now implemented, so what
        // this pins is that the errand EXISTS -- and that it ends.
        let mut m = WorldMap::new();
        m.fold(&dump(
            "l10",
            "Trenwick crypt",
            vec![node("shrine2", "Foggathorpe shrine"), node("shrine3", "Skipsea shrine")],
        ));
        for k in ["shrine2", "shrine3"] {
            let p = m.entry(k);
            p.completed = true;
            p.visited = true;
            p.used = true;
            p.consecrated = false;
        }
        m.hell = Some(0.1); // the anomaly is open, so consecrating is possible at all
        assert!(!m.anomaly_available().unwrap(), "fixture must have the door open");

        // There is something to do at both, and `worth_consecrating_here` is what says so -- the
        // function that existed and was called from nowhere while the run bounced.
        assert!(m.worth_consecrating_here("shrine2"));
        assert!(m.worth_consecrating_here("shrine3"));
        assert_eq!(m.next_target().map(|p| p.reason), Some(Goal::Shrine));

        // Each one is spent by arriving, whether or not the press worked. Standing on a shrine and
        // doing nothing now abandons it, so a decline can no longer feed the loop.
        m.abandon("shrine2");
        assert_eq!(m.next_target().unwrap().target, "shrine3");
        m.abandon("shrine3");
        if let Some(plan) = m.next_target() {
            assert_ne!(plan.reason, Goal::Shrine, "the errand must end: {plan:?}");
        }

        // Consecrating is what actually retires one, and it retires it for good.
        let mut m2 = m;
        m2.entry("shrine2").consecrated = true;
        assert!(!m2.worth_consecrating_here("shrine2"), "nothing left to do here");
    }

    #[test]
    fn once_the_anomaly_is_open_only_shrines_already_underfoot_are_worth_it() {
        // A corrupted shrine has to be fought through again, and corruption is invisible in the
        // heading -- so after the door opens we stop detouring and take only what is on the way.
        // here — detour(shrine) is a dead end; here — mid(shrine) — rift is the route.
        let mut m = WorldMap::new();
        m.fold(&dump(
            "here",
            "camp",
            vec![node("detour", "Faraway shrine"), node("mid", "Midway shrine")],
        ));
        m.fold(&dump("mid", "Midway shrine", vec![node("here", "camp"), node("rift", "The Rift anomaly")]));
        m.here = Some("here".into());

        // Open: the anomaly is the goal, and the route runs through `mid` but not `detour`.
        m.hell = Some(0.1);
        assert_eq!(m.next_target().unwrap().reason, Goal::Anomaly);
        let route = m.anomaly_route().unwrap();
        assert!(route.contains(&"mid".to_string()), "route: {route:?}");
        assert!(!route.contains(&"detour".to_string()), "the dead-end shrine is not on the way");

        // "Unless it is on the shortest direct path" is an ARRIVAL question, because `Anomaly`
        // outranks `Shrine` for as long as the anomaly is unfinished. Both shrines corrupted:
        // the one on the route earns its fight, the dead end does not.
        m.entry("mid").corrupted = true;
        m.entry("detour").corrupted = true;
        assert!(m.worth_consecrating_here("mid"), "we are walking through it regardless");
        assert!(!m.worth_consecrating_here("detour"), "a corrupted dead end is not worth the fight");

        // And a corrupted shrine is never a destination while the anomaly is open.
        assert_ne!(m.next_target().unwrap().reason, Goal::Shrine);
    }

    #[test]
    fn a_corrupted_shrine_is_skipped_where_a_merely_unfought_one_is_not() {
        // The distinction "incomplete" cannot make: a shrine revealed far from the hell radius and
        // never fought is an ordinary detour, while one the radius reset is the fight to avoid.
        let mut m = WorldMap::new();
        let mut d = dump("here", "camp", vec![node("s1", "Faraway shrine"), node("l2", "Quiet Glade meadow")]);
        d.hidden = 1;
        m.fold(&d);
        m.hell = Some(0.1);

        // Unfought but uncorrupted, and off-route: still worth going for.
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Shrine);
        assert_eq!(plan.target, "s1");

        // Same shrine, once corruption has reset it: now a fight we no longer need.
        m.entry("s1").corrupted = true;
        assert_ne!(m.next_target().unwrap().reason, Goal::Shrine);
    }

    #[test]
    fn opening_the_anomaly_comes_before_shrines() {
        // Not merely a priority call: `showConsecrateButton` needs `hell ~= 0`
        // (`shrine.lua:93-96`), so a run that did shrines first would reach each one unable to
        // finish it.
        let mut m = WorldMap::new();
        m.fold(&dump(
            "here",
            "camp",
            vec![node("shrine2", "Gransmoor shrine"), node("l39", "Eight Timberland — level 4 forest")],
        ));
        m.hell = Some(0.0);
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::OpenTheAnomaly);
        assert_eq!(plan.target, "l39");
    }

    #[test]
    fn the_consecrated_flag_comes_out_of_the_save() {
        let save = crate::game::save::parse(
            r#"return {
                overworld = {
                    playerLocation = "l19",
                    completedAreas = {},
                    areaFlags = {
                        hell = 0,
                        shrine2_consecrated = true,
                        shrine9_first_corrupt_time = 0.42,
                        shrine9_first_corrupt_time_shop = 3,
                    },
                },
            }"#,
        )
        .unwrap();
        let mut m = WorldMap::new();
        m.fold(&dump(
            "l19",
            "a crypt",
            vec![node("shrine2", "Gransmoor shrine"), node("shrine9", "Blighted shrine")],
        ));
        m.apply_save(&save);
        assert!(m.get("shrine2").unwrap().consecrated);
        assert!(!m.get("shrine2").unwrap().corrupted);
        // `_first_corrupt_time` marks the reset; the `_shop` variant is a different suffix and must
        // not be mistaken for a location key of its own.
        assert!(m.get("shrine9").unwrap().corrupted);
        assert!(m.get("shrine9_first_corrupt_time").is_none(), "no phantom place from the shop flag");
    }

    #[test]
    fn exploration_prefers_the_nearer_frontier() {
        // Both candidates unvisited, so the comparison is purely distance: `near` is one hop,
        // `far` is two, behind an already-exhausted `through`.
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("near", "a meadow"), node("through", "b meadow")]));
        let mut done = dump("through", "b meadow", vec![node("here", "camp"), node("far", "c meadow")]);
        done.hidden = 0;
        m.fold(&done);
        m.here = Some("here".into());
        assert_eq!(m.next_target().unwrap().target, "near", "one hop beats two");
    }

    #[test]
    fn a_completed_neighbour_loses_to_an_unfinished_one_at_the_same_distance() {
        // The live misstep: from l10, both l1 and l18 were adjacent and unvisited, but l1 was
        // already completed and had nothing to reveal. Sorting by key took it, and the run walked
        // back through l10 to reach l18 -- two hops wasted.
        let mut m = WorldMap::new();
        m.fold(&dump(
            "l10",
            "Ulrome village",
            vec![node("l1", "Weedley Copse crypt"), node("l18", "Stanningholme — level 2 crypt")],
        ));
        m.entry("l1").completed = true;
        assert_eq!(m.next_target().unwrap().target, "l18", "a finished node reveals nothing");
    }

    #[test]
    fn a_real_dump_folds_into_the_map_it_describes() {
        // Captured live at l1. The synthetic tests above pick keys like "l4" for level-4 nodes,
        // which reads as if keys encoded levels -- they do not, and this fixture is the proof:
        // the node keyed `l4` is `Bainton Clump — level 1 forest`. Routing must never infer a level
        // from a key.
        let raw = std::fs::read_to_string("tests/fixtures/overworld-dump-l1.txt").unwrap();
        let lines: Vec<String> = raw.lines().map(|s| s.to_string()).collect();
        let dumps = crate::observe::adjacency::parse(&lines);
        assert_eq!(dumps.len(), 1, "one complete block in the fixture");

        let mut m = WorldMap::new();
        m.fold(&dumps[0]);
        assert_eq!(m.here(), Some("l1"));
        assert_eq!(m.len(), 5, "the node we stand on plus its four neighbours");

        let l4 = m.get("l4").unwrap();
        assert_eq!(l4.level(), Some(1), "keyed l4, but level 1");
        assert!(!l4.triggers_anomaly(), "level 1 is nowhere near the > 3 the event needs");
        assert!(l4.type_is("forest"));

        // The em-dash is the combat marker, so a campfire and a village carry no level at all.
        assert_eq!(m.get("start").unwrap().level(), None);
        assert_eq!(m.get("l10").unwrap().level(), None);
        assert!(m.get("l1").unwrap().has_combat());

        // Nothing here can open the anomaly, so the only sensible plan is to go and look.
        assert_eq!(m.next_target().unwrap().reason, Goal::Explore);
    }

    #[test]
    fn a_fully_explored_map_with_nothing_to_do_says_so() {
        let mut m = WorldMap::new();
        let mut a = dump("start", "camp", vec![node("l1", "a meadow")]);
        a.hidden = 0;
        m.fold(&a);
        let mut b = dump("l1", "a meadow", vec![node("start", "camp")]);
        b.hidden = 0;
        m.fold(&b);
        m.here = Some("l1".into());
        m.hell = Some(0.1);
        m.entry(ANOMALY_KEY).completed_corrupt = true;
        // Everything is visited, nothing hides neighbours, the anomaly is beaten: no target.
        assert_eq!(m.next_target(), None, "walking on would be pointless, and saying so is the answer");
    }
}
