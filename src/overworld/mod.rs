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

mod crossing;
#[cfg(test)]
mod fixtures;
mod frame;
pub use crossing::Crossing;
mod place;
mod plan;
pub use plan::{Access, Goal, Hop, Plan};
pub use frame::{Frame, InsideFrame, ScreenScale, FRAME_TOLERANCE};
pub use place::{Arrival, Place, Risk};

/// The map cache's format marker, and the reason it is versioned at all.
///
/// **v1 held raw per-dump screen coordinates**, registered by translation alone. A file written
/// across a zoom change therefore contains positions at two scales with nothing recording which is
/// which, and no way to tell them apart afterwards. That is what aimed a click at (463, 841) instead
/// of roughly (672, 713) on 2026-08-16 and ended a run with `no arrival at l32`.
///
/// v2 positions are fitted with [`Frame`], scale included, so every one is in the same frame
/// whatever the zoom was when it was seen — **as far as aiming goes**. Placing still guessed.
///
/// **v3 exists because v2 files carry the same disease one level down.** `registration` measured the
/// scale when the dump gave it two anchors and *assumed 1.0* when it gave one, and a dump with one
/// anchor is the ordinary way a new node is learned: you arrive somewhere, and the only node in the
/// dump you have already placed is the one you came from. Before any zoom the assumption is exactly
/// right. Straight after one it is wrong by a factor of two, and the nodes placed from it go into the
/// file in the *other* scale. `world-0.txt` as written by the run of 2026-08-16 1752Z is provably
/// such a file: its 45 positions are two mutually consistent populations, one at each zoom, and 60 of
/// the run's 95 surface dumps have anchors that disagree with each other by up to 229 px.
///
/// **A v1 or v2 file is still read, minus its positions.** Only the coordinates were ever scaled;
/// edges, headings and connection counts describe the world's shape and are true at any zoom.
/// Refusing the whole file threw away 1188 edges of a well-walked world to avoid trusting its
/// coordinates, which is a bad trade — routing is what this project keeps failing for want of, and
/// aiming rebuilds itself from this run's dumps anyway. See [`WorldMap::absorb_cache`].
pub const CACHE_VERSION: &str = "# diggle map cache v3";

/// A snapshot of how much the run has achieved. See [`WorldMap::progress`].
///
/// Equality is the whole interface: two of these being equal means nothing was gained between them.
/// The individual counts are public so a stop can say *which* of them a run was failing to move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    /// Places on the map. Rises whenever a dump names somewhere new — so exploring always counts.
    pub known: usize,
    /// Nodes cleared. The main one: a fight won moves this and nothing else does.
    pub completed: usize,
    pub consecrated: usize,
    /// Shrines prayed at, campfires burnt, towers spent — `<key>_used` in the save.
    pub used: usize,
    /// Falls when we buy and rises when we loot, so either direction is something happening.
    pub gold: i64,
    pub portal_open: bool,
    /// General stores whose heart we have taken.
    pub shops_emptied: usize,
    /// Places the driver has given up on. Rises when we *learn* something is not worth returning to,
    /// which is progress of a kind and, more to the point, is the thing that breaks most loops.
    pub written_off: usize,
}

/// Everything we have folded together, plus the run state that decides routing.
///
/// `Clone` exists for one caller and one reason: [`WorldMap::choose_exit`] has to ask the planner a
/// question *from outside the subworld we are standing in*, and the only honest way to move the
/// vantage point is to copy the whole map and change where it is standing. It used to copy a
/// hand-picked list of six fields, which is how a village came to forget it had already bought its
/// hearts. See there.
#[derive(Debug, Default, Clone)]
pub struct WorldMap {
    places: BTreeMap<String, Place>,
    /// Places the driver has had its one go at and will not enter again this run.
    ///
    /// Distinct from `Place::used`, which is the *game's* record of a completed interaction. A shrine
    /// whose puzzle could not be read is untouched as far as the game is concerned, so `used` stays
    /// false and it remains a legitimate destination forever. This is the driver's own memory of
    /// having tried, and it exists because the planner and the caller must not disagree about what is
    /// still worth walking to — when they did, a run spent thirty steps crossing the same crypt.
    ///
    /// The oldest piece of this project's loop-prevention machinery, and the archetype for the rest:
    /// **state a dump can never give us**. `docs/superpowers/notes/navigation-loops.md` catalogues
    /// all six cycles found so far, the two guards that could never fire, and the rule this file's
    /// routing has to obey — memory or a monotone measure, never a ranking. Read it before adding
    /// another.
    abandoned: std::collections::HashSet<String>,
    /// Villages whose `Heart` we have already bought.
    ///
    /// **The standing assumption is the dev's: every village's general store starts with one.** So
    /// the interesting state is not which villages have a heart — they all do — but which no longer
    /// do, and this is that. A flag on `Place` was tried first and could only lie: it defaults to
    /// false, so the errand could never fire on a village we had just heard of, which is every
    /// village at the moment it becomes a candidate.
    ///
    /// The save *could* answer the stock question exactly. `shop.load` keeps it in
    /// `areaFlags[<key>_shops][<storeType>].inventoryHash` (`shop.lua:364`), per item and per count
    /// — the live save carries `l19_shops` with three inn items and their stocks. But that flag is
    /// only written once a shop screen has been *opened*, so for a store nobody has visited there is
    /// nothing to read and the stock is generated on the spot. The assumption is what makes this
    /// plannable before the first visit; the save is what would confirm it after.
    heart_bought: std::collections::HashSet<String>,
    /// Completed `<container>_path_to_<neighbour>` roads, by their own key.
    ///
    /// The other half of `areaOrExitToComplete` (`overworldview.lua:1312-1313`), and the reason it
    /// is a set rather than a `Place` flag: these keys were already being read out of
    /// `completedAreas` and **thrown away** by `apply_save`, because folding them in as places
    /// invented destinations that routing tried to walk to. That worry was sound and this keeps it —
    /// nothing here creates a `Place`, so nothing here can become a target. It only records that a
    /// road has been walked, which is the fact [`WorldMap::can_step`] needs and did not have.
    roads_done: std::collections::HashSet<String>,
    /// What the frame's units are worth in screen pixels, as last measured. See [`ScreenScale`] and
    /// [`WorldMap::registration`], which is the only thing that writes it.
    screen_scale: ScreenScale,
    /// The inside of the subworld we are standing in, for this visit. See [`InsideFrame`].
    ///
    /// Deliberately **not** in the cache: an interior layout does not survive leaving
    /// ([`crate::subworld::Rules::positions_survive_reentry`]), so a restored one could only be
    /// wrong.
    inside_frame: InsideFrame,
    /// Where the last dump said we were.
    here: Option<String>,
    /// `areaFlags.hell` — zero means the anomaly has not opened and the trigger is still live.
    /// A **float**, and that matters: `hellOpens` sets it to `0.1`
    /// (`utils/events.lua:39`, `setHellValue(0.1)`) and it grows from there. Read as an integer it
    /// parses as nothing at all, `anomaly_is_open` falls back to "not open", and the run sets off to
    /// trigger an anomaly that is already open.
    hell: Option<f64>,
    /// Set when a node cost us [`crate::rest::REST_THRESHOLD`] or more health, cleared once we are
    /// topped up. Held on the map because the decision is "where to go next", which is its job.
    ///
    /// **This one means "worth a trip" and nothing else.** For "there is a bed right here and we are
    /// scratched", see [`WorldMap::top_up`] — the two were one flag until 2026-08-21 and #72.
    wants_rest: bool,
    /// Set by [`WorldMap::top_up_at`]: a bed is under our feet and we are short of full.
    ///
    /// Kept apart from [`WorldMap::wants_rest`] because the two answer different questions and the
    /// bars are deliberately different — any wound at all here, half health or a four-point drop
    /// there. Sharing one field made the doorway rule a *detour* rule, which is the opposite of what
    /// its own doc says it is for. Read only through [`WorldMap::wants_a_bed`], so the surface
    /// planner's `Goal::Rest` branch cannot see it. Cleared wherever `wants_rest` is.
    top_up: bool,
    /// `player.gold` — an inn will not serve us below [`crate::rest::INN_COST`].
    gold: i64,
    /// Campfire fuel carried, which makes a campfire usable even at a used area.
    fuel: i64,
    /// **The frontier a crossing is currently walking to**, with the container it belongs to.
    ///
    /// Memory, and the reason is the rule this file keeps re-learning: *memory or a monotone
    /// measure, never a ranking* (`docs/superpowers/notes/navigation-loops.md`).
    ///
    /// The frontier walk below is argued to be cycle-free because "the BFS distance to the chosen
    /// frontier strictly decreases with each step". True — **of a fixed frontier**. The pick was
    /// re-ranked from scratch every step, and the ranking is a function of where we are standing, so
    /// two nodes could each nominate a target whose route runs through the other. Live in `l48`
    /// (Rowlston) on 2026-08-20, from the crossroads and back four times:
    ///
    /// ```text
    ///   22. l48xrd33x152 — Heart -> l48sub7
    ///   23. l48sub4      — Heart -> l48sub11
    ///   24. l48xrd33x152 — Heart -> l48sub7
    ///   25. l48sub4      — Heart -> l48sub11
    /// ```
    ///
    /// Each step was individually correct and the pair never terminated, which is the signature of
    /// every bounce recorded here. Holding the target still is what restores the argument the
    /// comment already makes.
    ///
    /// Keyed by container so leaving invalidates it without anything having to remember to clear it
    /// — and re-entering a subworld must invalidate it, since `lostOrientation` re-rolls the
    /// interior (`forest.lua:483-490`).
    probing_toward: Option<(String, String)>,
    /// **Well-Rested stacks banked**, summed across both flavours.
    ///
    /// A consumable, not an aura: one is spent per kill that heals (`rpgview.lua:1204-1209`), and
    /// the only source we can reach is an inn at [`crate::rest::INN_COST`] a stack
    /// (`ui/rest.lua:355-357`). Held here for the same reason `gold` is — the decision it feeds is
    /// where to go next. See [`crate::rest::stacks_wanted`] for what a fight is worth banking for.
    ///
    /// Zero is an **absent key**, not a stored zero: `affectPlayerStatus` deletes a status the
    /// moment it reaches zero (`overworld.lua:45-47`), so a spent-out character has no
    /// `statusEffects` entry at all.
    well_rested: i64,
    /// `player.health` / `player.maxHealth`, as of the last save read.
    ///
    /// Kept here for the same reason `gold` is: the decision it feeds is "where to go next". See
    /// [`WorldMap::top_up_at`], which is the one rule that needs the *exact* reading rather than
    /// the half-health line [`WorldMap::wants_rest`] is set from.
    health: Option<crate::rest::Health>,
    /// The surface node we were standing on when we entered the subworld we are in.
    ///
    /// `None` on the surface. Its one job is to let [`WorldMap::exit_toward`] recognise the entrance,
    /// because leaving a village by the door you came in is a retreat, and nothing visible inside
    /// distinguishes that road from any other.
    entered_from: Option<String>,
    /// The door this crossing is making for, and the errand it was chosen to serve.
    ///
    /// The subworld twin of [`crate::navigate::Run::committed_to`], and it exists for the same
    /// reason. [`WorldMap::exit_toward`] asks [`WorldMap::next_target`] afresh on every step, so the
    /// door being crossed toward was a function of the *whole map* — and of where we happen to be
    /// standing. Walk two nodes into a forest and the argmax can move; walk back and it moves again.
    /// Two halves of a forest each sending the run to the other is the shape of all six cycles in
    /// `docs/superpowers/notes/navigation-loops.md`.
    ///
    /// **The `Goal` is the load-bearing half of this pair.** Read those six together and they are
    /// one bug: *the goal never changed, the target flipped anyway, because `here` moved.* So this
    /// is keyed by goal rather than held unconditionally, which splits the two cases the old code
    /// could not tell apart:
    ///
    /// - **Same errand, different door** — always the bug. The commitment holds and the run finishes
    ///   what it started.
    /// - **Different errand** — the anomaly opened, a fight left us hurt, a shrine got finished.
    ///   Those are real events in the world, not artefacts of our own movement, and re-planning on
    ///   them is the correct response. The commitment is dropped.
    ///
    /// That distinction is what keeps this from being blind commitment. Its safety argument is that
    /// goal changes are *monotone across a single crossing*: nothing inside a subworld un-opens the
    /// anomaly or un-finishes a shrine, and nothing heals us back above the rest threshold. A goal
    /// cannot flip back and forth in here, so it cannot be a cycle of its own.
    ///
    /// The **route** is not committed and must not be: it is re-derived every step so fog, fresh
    /// fights and corruption are always accounted for. Destination bold, route cautious — which is
    /// Kinny & Georgeff's result applied to a world with two rates of change, since within a
    /// crossing the layout is static and only fog lifts.
    crossing_to: Option<(String, Goal)>,
    /// Why the door in `crossing_to` was chosen, for the log. Set wherever the choice is made.
    door_reason: Option<Door>,
    /// **What the choice actually saw**: the errand, and every door with its distance to that
    /// errand's target.
    ///
    /// The reason alone has twice been too little. On 2026-08-16 the run left Ulrome by the road to
    /// `l7` when the road to `l1` — one hop from the anomaly against `l7`'s two — was among the five
    /// doors the game printed, and the report could not say whether the errand was the anomaly,
    /// whether `l1` was ranked at all, or what it scored. Answering that meant reading the *game's*
    /// console log for the exit list and reasoning backwards.
    ///
    /// A ranking nothing records is a ranking nobody can check, and this one decides where a run
    /// walks. Set beside [`WorldMap::door_reason`] and printed with the crossing.
    door_note: Option<String>,
    /// Set on any step that honours a commitment instead of choosing afresh, so
    /// [`WorldMap::door_note`] can say the note is not this step's. Cleared when a choice is made.
    door_held: Option<String>,
    /// **The most recent dump's own frame**, rewritten wholesale by every [`WorldMap::fold`]: every
    /// node and every exit it named, at the coordinates it printed for them.
    ///
    /// Not the assembled map. [`Place::pos`] is a frame built across dumps by [`WorldMap::registration`],
    /// which deliberately refuses subworld dumps — `zoomMult` is a shared factor and nothing checks
    /// the zoom, so two interior dumps could silently disagree on scale. This sidesteps that
    /// entirely by never comparing two dumps: everything in here was printed by the *same* dump, in
    /// the same instant, under the same offset and the same zoom, so differences between two entries
    /// are real whatever the view is doing.
    ///
    /// That is the whole reason it exists. A dump's exits section gives a door's **position and its
    /// destination heading but never its key** (`overworldview.lua:1041-1047`), so the road out of a
    /// subworld is not a node we can route to until some dump happens to name it as a neighbour —
    /// which in `l2` on 2026-08-09 was the second-to-last step of a 22-step crossing. Its position
    /// was on screen the entire time. See [`WorldMap::placed_now`].
    ///
    /// Keyed for exits by the synthesised `{parent}_path_to_{to_key}`, so a door and an ordinary node
    /// are asked for by the same name.
    frame: BTreeMap<String, (f64, f64)>,
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

/// Why a subworld crossing is heading for the door it picked.
///
/// Carried into the run's log because three live runs turned on this and none of them recorded it.
/// The branches look alike in the report — every one prints `crossing X toward Y` — and telling them
/// apart afterwards meant rebuilding the map by hand and reasoning about what the code must have
/// done. That reasoning was wrong twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Door {
    /// The exit whose far side is fewest hops from the target, over edges we have recorded. The
    /// answer we want, and the rarest: it needs the target to be in a component we can reach.
    NearestToTarget,
    /// The nearest was the door we came in by, and something unvisited was on offer instead.
    NotBackOutAgain,
    /// No exit had a measurable distance, but the target has a position — so the safest door, and
    /// among equally safe ones the one that points at it. On an open anomaly the position is
    /// usually the corruption centroid rather than a sighting; see [`WorldMap::pos_for`].
    TowardTheCorruption,
    /// No distance and no bearing either. Safest door, then whatever sorts first — a real answer,
    /// but the weakest one, and the state a fresh launch starts in.
    SafestOfWhatIsLeft,
}

impl Door {
    pub fn why(self) -> &'static str {
        match self {
            Door::NearestToTarget => "nearest to the target",
            Door::NotBackOutAgain => "not back out the way we came",
            Door::TowardTheCorruption => "safest, and toward the anomaly",
            Door::SafestOfWhatIsLeft => "safest available; no bearing",
        }
    }
}

/// `completedAreas` entry that means the anomaly has been beaten — the run's whole objective.
///
/// `setAreaComplete` writes `completedAreas[key..'_corrupt'] = loc.corrupt or nil`
/// (`overworldview.lua:172`), so finishing the corrupted `start` sets exactly this.
/// Kept as documentation of the game's own check rather than used directly: we read the
/// `_corrupt` suffix generically into [`Place::completed_corrupt`], so the objective is asked of
/// whichever node turns out to be the portal.
pub const ANOMALY_BEATEN_KEY: &str = "start_corrupt";

/// What a hop costs when the game will not simply let us walk it — see [`WorldMap::step_cost`].
///
/// Six, measured rather than chosen. Crossing `l62` on 2026-08-15 took **five interior hops**
/// (`l62sub10`, `l62sub2`, `l62sub5`, `l62sub18`, then the exit road) on top of the press that
/// entered it, and that was a crossing which *worked*; the same forest ended two runs at the exit it
/// could not reach. Fights on the road are extra and not counted here, so this is a floor.
///
/// Being roughly right matters more than being exactly right. The number decides how many free hops
/// are worth spending to avoid one crossing, and the honest answer from watching it is "about six".
pub const CROSSING: usize = 6;

/// What a `Heart` costs at a general store — `cost = 100` (`items/ephemeral.lua:4-9`).
///
/// The item is `healthBuff`, *"Permanently increases your maximum health by 4; a whole heart"*, and
/// it reaches a general store as `specialStock` rather than as a random roll, so a village that has
/// one has it by construction rather than by luck.
pub const HEART_COST: i64 = 100;

/// The purse a heart errand needs before it will set off: the price, plus a bed.
///
/// The dev's number, 2026-08-15 — *at least 110 gold before buying so that we can rest if needed
/// afterwards.* Buying down to nothing trades four maximum health for the ability to heal six at a
/// time, which is the wrong way round for a run that keeps dying just short.
pub const HEART_FLOOR: i64 = HEART_COST + crate::rest::INN_COST;

/// **Major shrines to have consecrated before the anomaly is worth walking into.**
///
/// The dev's number, 2026-08-20: *before the anomaly fight, ensure that at least 4 major shrines
/// have been consecrated. This means that we need to keep exploring if the anomaly opens before
/// we've revealed 4 shrines.* **Lowered to three on 2026-08-21**, in the same breath as the
/// corrupted-shrine exception ([`WorldMap::corrupted_shrines_are_needed`]) — the two arrived
/// together and are worth reading together: the exception widens what may be spent to meet the bar,
/// and this lowers the bar, so the pair make a world that could not supply four clean shrines much
/// less likely to need a fight for the fourth.
///
/// Three of a possible seven — `generateNLocationsOfNameAroundLocationsWithinBounds(…, 7, 'shrine',
/// …)` (`overworld/generators/world.lua:81`) — so the bar is reachable rather than aspirational,
/// though nothing guarantees all seven get placed or that all of them can be reached.
///
/// **The evidence for four being enough is one run, and it is not evidence for three.** The 1519Z
/// run of 2026-08-21 consecrated four and beat the level 8 anomaly in 13 turns at 84/84 without
/// taking a point of damage — which says four was ample and says nothing about where the floor is.
/// Three is the dev's judgement, and the number to revisit first if a run loses that fight.
///
/// A consecration pays in gold- and silver-bordered wildcard tiles (`utils/blessings.lua:95-110`),
/// which is the mechanic this solver handles best, and it is bought with walking rather than
/// health. That is the whole argument for spending time on it before a level 8 fight.
///
/// **This gates the anomaly, it does not forbid it.** See the release at the foot of
/// [`WorldMap::plan`]: when exploring has nothing left to offer, the run goes anyway rather than
/// standing still with a plan it will never satisfy.
pub const SHRINES_BEFORE_THE_ANOMALY: usize = 3;

/// Cost to `key`, or [`usize::MAX`] when no known route reaches it — so unreachable places sort
/// last rather than first.
///
/// **Cost, not hops.** It was hops until 2026-08-15, and reads like hops for any map we have not
/// cleared anything on, because every edge is worth [`CROSSING`] there and the ordering is the
/// uniform one. Once ground is cleared the two diverge — see [`WorldMap::step_cost`] — and the
/// number stops being countable in moves. Every consumer sorts and compares with it, which is why
/// the unit could change at all; none of them may start testing it against a literal.
fn dist_or_far(dist: &BTreeMap<String, usize>, key: &str) -> usize {
    dist.get(key).copied().unwrap_or(usize::MAX)
}

/// Is this the key of a **major** shrine — `shrine` followed by digits and nothing else?
///
/// The game's own test is `hereData.key:sub(1,6)=='shrine'` (`overworld/generators/world.lua:87-90`),
/// applied while laying out the overworld, so it only ever sees overworld keys. Ours has to run
/// against every key we know, and `sub(1,6)` would swallow `shrine1sub7` and `shrine1_path_to_l5`
/// with it. Digits-only is the same test restricted to the population the game applied it to.
fn key_is_major_shrine(key: &str) -> bool {
    key.strip_prefix("shrine")
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
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

impl WorldMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn here(&self) -> Option<&str> {
        self.here.as_deref()
    }

    /// The learned map, as text, for keeping between runs.
    ///
    /// ## Why this exists
    ///
    /// A `Place` is assembled from two sources with different lifetimes, and only one of them
    /// survives a restart. `completed`, `corrupted` and `consecrated` come from save flags, so they
    /// come back. **Edges and positions come from dumps, and dumps are per-run**, so every restart
    /// begins knowing the *names* of places it has cleared and nothing about where they are or how
    /// they connect.
    ///
    /// That is what makes an old objective unreachable. Live 2026-08-14: `shrine1` had been fought
    /// and stood on in an earlier run, and came back `[done] [corrupted]` with no edges — so
    /// `can_route_to("shrine1")` was false, the shrine could only ever be a `RouteTo` bearing, and
    /// the ten corrupted nodes had no positions between them, which is why steering has never once
    /// switched on. Not one of the ten was named in a single dump of that run.
    ///
    /// ## Positions are worth keeping, and the frame makes that non-obvious
    ///
    /// A frame's origin is arbitrary — the first dump that registers *defines* it
    /// ([`WorldMap::registration`]), so two runs need not agree. Restoring positions anyway is
    /// correct because of how registration works: it anchors a new dump against any node that
    /// **already** carries a position, so a run that loads this file adopts the earlier run's frame
    /// rather than inventing its own. Restoring the edges alone would leave the compass empty.
    ///
    /// Tab-separated and hand-rolled, for the reason [`crate::stamp`] gives: this is a private file
    /// we both write and read, and a serialisation crate is a dependency and a build cost for
    /// something a `split('\t')` does. Unknown leading tokens are skipped rather than rejected, so a
    /// newer writer cannot break an older reader.
    ///
    /// ## The level has a column of its own, and the version did not move
    ///
    /// A `p` row ends `…\t<parent>\t<level>`, where the level is [`Place::base_level`] — see there
    /// for why the heading alone could not carry it, and #79 for the world this file was written
    /// blind. Appending rather than bumping [`CACHE_VERSION`] is the point of the paragraph above:
    /// an older reader takes seven fields and ignores the eighth, and a newer reader finds `None`
    /// in an older file and falls back to whatever level its headings still hold. Bumping would
    /// have been actively worse — the version string is what gates `positions_are_trustworthy`, so
    /// a new one would have thrown away every coordinate in every cache we have, to announce a
    /// column that costs nothing to miss.
    pub fn cache_text(&self) -> String {
        let mut out = format!("{CACHE_VERSION}\n");
        for p in self.places.values() {
            let (x, y) = match p.pos {
                Some((x, y)) => (x.to_string(), y.to_string()),
                None => ("-".to_string(), "-".to_string()),
            };
            out.push_str(&format!(
                "p\t{}\t{}\t{x}\t{y}\t{}\t{}\t{}\t{}\n",
                p.key,
                p.heading,
                p.connections,
                p.hidden.map(|h| h.to_string()).unwrap_or_else(|| "-".into()),
                p.parent.clone().unwrap_or_default(),
                p.base_level.map(|l| l.to_string()).unwrap_or_else(|| "-".into()),
            ));
            for n in &p.neighbours {
                out.push_str(&format!("e\t{}\t{n}\n", p.key));
            }
        }
        out
    }

    /// Folds a [`WorldMap::cache_text`] back in, and reports how many edges it restored.
    ///
    /// **Additive, and never authoritative over this run.** Anything the live game has already told
    /// us wins: a heading is only filled in where we have none, and a position only where we have
    /// none, so a stale cache cannot overwrite a fresh dump. `visited` is deliberately *not*
    /// restored — it means "we have stood here **this run**", and the frontier ordering leans on
    /// that meaning.
    ///
    /// `completed` is not restored either. The save carries it, and the save is the game's own
    /// answer rather than our recollection.
    pub fn absorb_cache(&mut self, text: &str) -> usize {
        self.absorb_cache_inner(text, true)
    }

    /// The same, for a cache that arrives **after** this run has already placed something.
    ///
    /// Edges, headings, connection counts and parentage are statements about the world's shape and
    /// are always safe to take. Positions are not: they are in whatever frame the run that wrote
    /// them was using, and dropping them into a frame this run has already anchored gives
    /// [`WorldMap::registration`] a set of nodes that disagree with each other — the fault
    /// [`WorldMap::unplaceable`] reports as *the frame disagrees with itself*.
    ///
    /// This is the same trade a v1 file already gets, and for the same reason. Aiming rebuilds
    /// positions from this run's dumps, which is where a trustworthy one has to come from anyway.
    ///
    /// **Why a late load happens at all.** A fresh profile has no `mainSaveData` when the run
    /// starts — the game writes it on screen *exit*, and a run that has just walked into the
    /// overworld has exited nothing. So the seed cannot be read, the cache cannot be found, and the
    /// run begins blind. See [`crate::navigate::Run::recall_map`], which keeps asking.
    pub fn absorb_cache_structure(&mut self, text: &str) -> usize {
        self.absorb_cache_inner(text, false)
    }

    /// Has anything been placed on screen yet — that is, do we have a frame of our own to protect?
    pub fn any_placed(&self) -> bool {
        self.places.values().any(|p| p.pos.is_some())
    }

    fn absorb_cache_inner(&mut self, text: &str, trust_positions: bool) -> usize {
        // **An older cache keeps everything except where things are.**
        //
        // The first cut of this refused a v1 file whole, on the grounds that its coordinates mix two
        // zoom scales and cannot be repaired. The coordinates cannot — but they are the only part
        // that was ever scaled. **Edges, headings and connection counts are statements about the
        // world's shape**, and the shape does not change with the camera: `l25_path_to_l32` is true
        // at any zoom.
        //
        // Refusing all of it threw away the most valuable thing in the file. `world-5.txt` holds 407
        // places and 1188 edges from an adventure that walked much further than tonight's, and
        // routing is exactly what this project keeps failing for want of.
        //
        // So a v1 file is read for structure and its positions are dropped on the floor. Aiming then
        // rebuilds from this run's dumps, which is where a trustworthy position has to come from in
        // any case — see [`WorldMap::registration`].
        let positions_are_trustworthy = trust_positions && text.starts_with(CACHE_VERSION);
        let mut edges = 0;
        // Set when any position comes back off disk. See the assignment at the end of this function.
        let mut inherited_a_frame = false;
        for line in text.lines() {
            let mut f = line.split('\t');
            match f.next() {
                Some("p") => {
                    let Some(key) = f.next().filter(|k| !k.is_empty()) else { continue };
                    let (heading, x, y) = (f.next(), f.next(), f.next());
                    let (conns, hidden, parent) = (f.next(), f.next(), f.next());
                    let level = f.next();
                    let place = self.entry(key);
                    if place.heading.is_empty() {
                        if let Some(h) = heading.filter(|h| !h.is_empty()) {
                            // `recall_heading`, not `observe_heading`: this is disk, not a
                            // dump, so it must not claim the game said anything this run. It still
                            // gives up whatever levels a file written before the column existed
                            // happens to have kept; the explicit column below wins where it exists.
                            place.recall_heading(h);
                        }
                    }
                    if place.base_level.is_none() {
                        if let Some(Ok(l)) = level.map(str::parse::<u32>) {
                            place.base_level = Some(l);
                        }
                    }
                    if place.pos.is_none() && positions_are_trustworthy {
                        if let (Some(Ok(x)), Some(Ok(y))) =
                            (x.map(str::parse::<f64>), y.map(str::parse::<f64>))
                        {
                            place.pos = Some((x, y));
                            inherited_a_frame = true;
                        }
                    }
                    if place.connections == 0 {
                        if let Some(Ok(c)) = conns.map(str::parse::<u32>) {
                            place.connections = c;
                        }
                    }
                    if place.hidden.is_none() {
                        if let Some(Ok(h)) = hidden.map(str::parse::<usize>) {
                            place.hidden = Some(h);
                        }
                    }
                    if place.parent.is_none() {
                        if let Some(p) = parent.filter(|p| !p.is_empty()) {
                            place.parent = Some(p.to_string());
                        }
                    }
                }
                Some("e") => {
                    if let (Some(a), Some(b)) = (f.next(), f.next()) {
                        if !a.is_empty() && !b.is_empty() {
                            self.entry(a).neighbours.insert(b.to_string());
                            edges += 1;
                        }
                    }
                }
                _ => {}
            }
        }
        // **A restored frame is inherited, not defined**, and its units are whatever the zoom was in
        // the run that wrote the file. [`ScreenScale`] defaults to `Some(1.0)` on the reasoning that
        // a fresh map's first dump *is* the frame — true, and false the moment a cache supplies one.
        // Left alone, a one-anchor dump would place at this run's zoom into last run's frame, which
        // is exactly the fault `WorldMap::registration` was rewritten to stop, arriving by post.
        //
        // The cost is nothing in practice: the first surface dump names several nodes the cache has
        // already placed, so the scale is measured before there is anything new to place.
        if inherited_a_frame {
            self.screen_scale = ScreenScale(None);
        }
        edges
    }

    /// How many times `key` may still be looted, counting the state of the settlement around it.
    ///
    /// The dev, 2026-08-09: *passing through a corrupted village lost to demons, a `Loot` button
    /// appears in the bottom-left slot at every lootable node. High priority, easy grab.* This is the
    /// question behind that, and the whole of what a caller has to ask before pressing.
    ///
    /// Two conditions, from `getAreaButtons` (`overworld/generators/village.lua:84-98`):
    ///
    /// ```lua
    /// if overworldview.areaFlag(location.key..'_attacked')
    /// or overworldview.areaFlag(location.parentNode.key..'_attacked')
    /// or overworldview.areaFlag(location.parentNode.key..'_attack').playerAttack then
    ///     return location.typeData.destroyedAreaButtons or {}
    /// end
    /// ```
    ///
    /// — the building or its village was lost, **or we sacked it ourselves** — and then the per-type
    /// budget in [`Place::loot_left`].
    ///
    /// **The parent clause is not a detail.** A village is lost as a whole: the flag lands on the
    /// container and the buildings inherit it, so asking only `<key>_attacked` would find nothing at
    /// almost every node that actually has a `Loot` button.
    ///
    /// Zero when there is nothing to take, so a caller can treat it as a count and a condition at
    /// once. **Nothing presses this yet** — see #26 for why the press is held back and what a live
    /// run has to show first.
    pub fn loot_here(&self, key: &str) -> u32 {
        let Some(p) = self.places.get(key) else { return 0 };
        let destroyed = p.sacked
            || p.parent
                .as_ref()
                .and_then(|c| self.places.get(c))
                .map(|c| c.sacked || c.player_sacked)
                .unwrap_or(false);
        match destroyed {
            true => p.loot_left(),
            false => 0,
        }
    }

    /// Records that we have had our one attempt at `key` and it should stop being a destination.
    ///
    /// Deliberately not folded into `apply_save`: the save is the game's view and would overwrite
    /// this on the next read, which is precisely the disagreement that caused the bounce.
    pub fn abandon(&mut self, key: &str) {
        self.abandoned.insert(key.to_string());
    }

    /// Have we given up on `key`?
    ///
    /// [`WorldMap::abandon`] is a set insert and so is idempotent, but writing off is also
    /// *progress* — it is a field of [`WorldMap::progress`] — and a caller that cannot tell a fresh
    /// write-off from a repeat would report one every time it passed. See `navigate::LOOP_WRITE_OFF`,
    /// which relies on the second attempt being silent so the give-up guard behind it still fires.
    pub fn is_written_off(&self, key: &str) -> bool {
        self.abandoned.contains(key)
    }

    pub fn get(&self, key: &str) -> Option<&Place> {
        self.places.get(key)
    }

    /// How many places we could actually aim a click at: those with a world position.
    ///
    /// The complement of [`WorldMap::len`] is the answer to "why did a route exist and a one-press
    /// travel not happen" — routing needs edges, which the save carries, and aiming needs a
    /// position, which only a dump can give.
    pub fn places_with_positions(&self) -> usize {
        self.places.values().filter(|p| p.pos.is_some()).count()
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
        if !self.anomaly_is_open().unwrap_or(false) || self.anomaly_beaten() {
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

    /// Is the portal open and spreading? `None` when no save has been read yet.
    ///
    /// ```text
    ///   hell != 0   ->  Some(true)   the portal is live; consecration is possible, go and fight it
    ///   hell == 0   ->  Some(false)  nothing has opened it yet; go and trigger one
    /// ```
    ///
    /// `hellOpens` sets `hell` to `0.1` and it grows from there (`utils/events.lua:39`), and
    /// `shrine.lua:50` reads the same flag as `hell = areaFlag'hell' ~= 0` — so this is phrased the
    /// way the game phrases its own conditions, and a caller that negates it is stating a rule
    /// rather than undoing a name.
    ///
    /// It was once `anomaly_available`, meaning the *trigger* was still available to spend — the
    /// exact opposite reading, and the inversion was not hypothetical: a run header printing
    /// `anomaly available Some(false)` was written up as "the anomaly is not open yet" when the
    /// portal was open and eating the island.
    ///
    /// Unknown counts as **not open**, at every caller. A wasted trip to trigger one is cheaper
    /// than a run that will not go and open the thing it came to close.
    pub fn anomaly_is_open(&self) -> Option<bool> {
        self.hell.map(|h| h != 0.0)
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
            self.top_up = false;
        }
    }

    /// `player.gold`, as of the last save read. What an inn will and will not do is a gold check.
    pub fn gold(&self) -> i64 {
        self.gold
    }

    /// Well-Rested stacks banked, both flavours summed, as of the last save read.
    ///
    /// **Exposed to be logged**, and the 1519Z run is why. It printed `16 stack(s) short` at
    /// startup and `0 stack(s) short` at every rest, with the same level 8 anomaly on the map
    /// throughout. The bank itself was never printed, so the log could not say which reading was
    /// wrong — a shortfall is a *derived* number, and printing it without its operand is what made
    /// a contradiction unreadable for a whole run.
    ///
    /// It was the startup one. That run resumed **mid-fight**, and mid-fight `mainSaveData` has no
    /// status effects at all; see [`crate::rest::well_rested_from`].
    pub fn well_rested(&self) -> i64 {
        self.well_rested
    }

    /// The bank, from a source [`WorldMap::apply_save`] cannot see.
    ///
    /// Its one caller is [`crate::navigate::Run::apply_save`], which reads `combatSaveData` when
    /// the main save is silent about the effects. Kept as a setter rather than folded into
    /// `apply_save` because that function takes one table and the second file is the driver's to
    /// find — the map has never known where the save directory is.
    pub fn note_well_rested(&mut self, stacks: i64) {
        self.well_rested = stacks;
    }

    /// The frontier node the crossing is currently walking to, if it holds one.
    ///
    /// **Exposed to be logged**, and #57 is why it became necessary. While there were two crossing
    /// arms, the alternation itself was the diagnosis — the 1519Z run's faults were all found by
    /// watching `probing`/`steering` swap in the log. With one arm the steps read as a coherent walk
    /// whether or not they are going anywhere sensible, so the thing worth printing is no longer
    /// *which rule chose this step* but **where the walk thinks it is going**.
    ///
    /// The step is already logged and is not the same thing: a step is one hop along a route to this,
    /// and a route may cross ground the target itself was never a candidate on.
    pub fn frontier_target(&self) -> Option<&str> {
        self.probing_toward.as_ref().map(|(_, k)| k.as_str())
    }

    /// Cleared once health is back up.
    pub fn rested(&mut self, now: crate::rest::Health) {
        if now.is_full() {
            self.wants_rest = false;
            self.top_up = false;
        }
    }

    /// Below full health, as of the last save read. Unknown counts as **not** hurt.
    ///
    /// The narrow reading [`crate::itemchoice::Boon`] needs — worth nothing at full health — kept
    /// apart from [`WorldMap::wants_rest`], which is about whether to make a trip.
    pub fn is_hurt(&self) -> bool {
        self.health.map(|h| !h.is_full()).unwrap_or(false)
    }

    pub fn wants_rest(&self) -> bool {
        self.wants_rest
    }

    /// The rest errand is finished, on the inn's own word rather than on a health reading.
    ///
    /// [`WorldMap::note_health_level`] and [`WorldMap::rested`] both clear the intent only when they
    /// *see* full health, and the save that would show it is written when the inn screen is exited —
    /// so between healing and reading there is a window in which the run still believes it wants a
    /// rest and will walk back in. This is that window closed from the other side: the rest screen's
    /// own `healthNeed = 0` is as authoritative as the flag and arrives a screen earlier.
    pub fn rest_errand_over(&mut self) {
        self.wants_rest = false;
        self.top_up = false;
    }

    /// Has this shrine been consecrated, as the **save** reports it?
    ///
    /// Named after the game's own `isConsecrated`, and reading the same `<key>_consecrated` flag. The
    /// authoritative answer to "did that press work", as against any screen-shaped proxy for it —
    /// see [`crate::navigate::Run::confirm_consecrated`] for what believing the proxy cost.
    pub fn is_consecrated(&self, key: &str) -> bool {
        self.places.get(key).map(|p| p.consecrated).unwrap_or(false)
    }

    /// Folds one adjacency dump into the map.
    ///
    /// Everything here is additive except the fields a dump is authoritative for. A neighbour's
    /// entry is created if unseen, but its `hidden` count and `visited` flag are left alone: only a
    /// dump taken *at* a node can speak to those.
    /// Why the door we are crossing toward was chosen. `None` before any choice this crossing.
    pub fn door_reason(&self) -> Option<Door> {
        self.door_reason
    }

    /// The errand and the doors it was ranked against — see [`WorldMap::door_note`]'s field.
    ///
    /// **A held commitment says so, in the returned string.** The note itself is only written when
    /// [`WorldMap::choose_exit`] runs, so on every step that honours a commitment the last fresh
    /// note stays put — and printed bare, it reads as this step's reasoning. That cost a false
    /// diagnosis on 2026-08-16: `Heart -> l32` on every lap of a ping-pong, while the save said that
    /// shop was empty and the plan was `CloseAnomaly`.
    pub fn door_note(&self) -> Option<String> {
        match (&self.door_held, &self.door_note) {
            (Some(held), Some(note)) => Some(format!("HELD {held}; last decided — {note}")),
            (Some(held), None) => Some(format!("HELD {held}; nothing was ever decided")),
            (None, note) => note.clone(),
        }
    }

    /// Straight-line distance between two places, when both have been placed in the frame.
    ///
    /// The units are the run's own and mean nothing on their own; only comparisons between two
    /// answers from the same run are meaningful, which is all any caller wants.
    ///
    /// This is the **potential function** `docs/superpowers/notes/navigation-loops.md` says every
    /// one of our navigation bugs came down to lacking. A ranking cannot stop a cycle, because a
    /// stable preference between two alternating states *is* a cycle; a quantity that strictly
    /// decreases as we approach can, and this is one. It is only a heuristic about *direction* —
    /// walls, water and fights are invisible to it — so it belongs where a real route is unavailable
    /// and never in place of one.
    pub fn gap(&self, from: &str, to: &str) -> Option<f64> {
        let (ax, ay) = self.pos_for(from)?;
        let (bx, by) = self.pos_for(to)?;
        Some(((ax - bx).powi(2) + (ay - by).powi(2)).sqrt())
    }

    /// Records that we have moved the zoom, so nothing is placed until a dump measures the new scale.
    ///
    /// The zoom is ours alone to move: `setZoom` is reached from `core:wheelmoved`
    /// (`overworldview.lua:1529-1531`) and from the options screen, and `zoomMult` is otherwise a
    /// module local that entering or leaving a subworld does not touch. So this cannot be missed by
    /// forgetting to call it somewhere the game zooms on its own — there is no such place.
    ///
    /// It is belt and braces beside the remembered scale rather than the fix itself: replaying the
    /// 1752Z run, the re-centring dump straight after the zoom carried four anchors and measured the
    /// new scale immediately. This covers the case where it carries one.
    /// Records a lost woods from the console, at the moment the mist event is answered.
    ///
    /// The event's `Continue` sets `lost_woods_known_<key>` and enters the subworld in one
    /// `onSelect` (`overworld/events/arrived/lost_woods.lua:23-27`), so the game knows immediately.
    /// We would not, until a save was written and read — and `mainSaveData` is written on screen
    /// exit, which for a crossing means after the whole woods has been walked. The run of
    /// 2026-08-21 spent 69 steps inside `e3` with the flag sitting unread in memory.
    ///
    /// Sets [`Place::in_lost_woods`] as well as [`Place::avoid`], which is not the same fact twice:
    /// `avoid` keeps us from coming back, and `in_lost_woods` is what
    /// [`WorldMap::far_hop_inside`] reads to refuse a fast hop through fog. [`WorldMap::fold`] would
    /// set the second one dump later, off the container's heading; setting it here means the first
    /// step inside is covered too.
    ///
    /// Idempotent, and safe to call for a woods already known — which is the state after any restart
    /// that read the save.
    pub fn mark_lost_woods(&mut self, key: &str) {
        let p = self.entry(key);
        p.in_lost_woods = true;
        p.avoid = true;
    }

    /// Everything that counts as having got somewhere, in one comparable value.
    ///
    /// The measure under the loop guard — see [`crate::navigate::Run::sterile_here`]. It exists
    /// because three ping-pongs in two days were each diagnosed to a different root cause and **not
    /// one of them was caught by anything general**. `docs/superpowers/notes/navigation-loops.md`
    /// has said all along that every navigation bug here comes down to lacking a monotone measure or
    /// a memory; this is the measure.
    ///
    /// What belongs in it is anything whose change means the run got something done — a fight won, a
    /// node learned, gold spent, a shop emptied, a shrine consecrated, the portal opened. What must
    /// **not** be in it is anything that changes merely by moving: `here` most of all, since walking
    /// in a circle would then look like progress, which is the exact mistake this is built to catch.
    ///
    /// Coarse on purpose. It answers one question — *is this lap the same as the last one?* — and a
    /// counter that moves for the wrong reason only ever costs a stop we could have deferred.
    pub fn progress(&self) -> Progress {
        Progress {
            known: self.places.len(),
            completed: self.places.values().filter(|p| p.completed).count(),
            consecrated: self.places.values().filter(|p| p.consecrated).count(),
            used: self.places.values().filter(|p| p.used).count(),
            gold: self.gold,
            portal_open: self.anomaly_is_open().unwrap_or(false),
            shops_emptied: self.heart_bought.len(),
            written_off: self.abandoned.len(),
        }
    }

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
            // A fresh crossing decides its own door. Re-entering a subworld is the one moment the
            // world is allowed to have changed under us — `subworld::Rules::edges_survive_reentry`
            // exists because the interior can re-roll — so a commitment made on the last visit is
            // worth nothing and holding it would be blind rather than bold.
            self.crossing_to = None;
        } else if a.subworld.is_none() {
            self.entered_from = None;
            self.crossing_to = None;
        }
        self.here = Some(a.here_key.clone());

        // Being inside a subworld names its container, which is the only reliable way we learn that
        // a node is one. Its heading will not say so — `Eight Timberland — level 4 forest` reads
        // exactly like a fight.
        //
        // The heading is taken **whenever it is offered**, not only to fill a gap. It used to be
        // `if p.heading.is_empty()`, which threw away the one line that says a forest has turned out
        // to be a lost woods: we learn `Howden Timberland — level 2 forest` from the surface, walk
        // in, and the game prints `Howden Timberland — level 2 lost woods` from that point on. The
        // surface heading is not wrong, it is *stale* — `getTypeName` is evaluated per call
        // (`lost_woods.lua:29`) — and preferring the older of two live readings is never right.
        // A run ended inside `e1` with the answer sitting in the dump it had already parsed.
        if let Some((key, heading)) = a.subworld.as_ref() {
            let p = self.entry(key);
            p.subworld_container = true;
            if !heading.trim().is_empty() {
                p.observe_heading(heading);
            }
        }

        // Are we inside a lost woods? Asked once here rather than at every use, because the answer
        // is a property of the container and the places that need it are its subnodes.
        //
        // `corrupt_lost_woods` prints the same type name with none of the behaviour, so corruption
        // excludes it — see [`Place::in_lost_woods`].
        let lost_woods = a
            .subworld
            .as_ref()
            .and_then(|(k, _)| self.places.get(k))
            .map(|p| p.type_is("lost woods") && !p.corrupted)
            .unwrap_or(false);
        if lost_woods {
            if let Some((key, _)) = a.subworld.as_ref() {
                self.entry(key).in_lost_woods = true;
            }
        }

        {
            let here = self.entry(&a.here_key);
            here.observe_heading(&a.here_heading);
            here.visited = true;
            here.hidden = Some(a.hidden);
            here.parent = parent.clone();
            here.in_lost_woods |= lost_woods;
        }

        // **A different container, or none, is a different world.** The interior frame lasts one
        // visit and this is the line that ends it — see [`InsideFrame`] for why a remembered
        // interior layout is worth nothing. Done before the registration below, so that the fit is
        // always asked of a frame that belongs to where we are standing.
        //
        // Leaving and coming straight back is covered by the same test, because leaving prints a
        // surface dump and a surface dump has no container at all.
        match a.subworld.as_ref().map(|(k, _)| k.as_str()) {
            Some(c) if self.inside_frame.container.as_deref() == Some(c) => {}
            Some(c) => {
                self.inside_frame =
                    InsideFrame { container: Some(c.to_string()), pos: BTreeMap::new(), scale: Some(1.0) }
            }
            None => self.inside_frame = InsideFrame::default(),
        }

        // Where this dump's coordinates sit relative to the frame we are building. `None` means we
        // cannot place anything from it — see [`WorldMap::registration`].
        //
        // A fit whose anchors disagree with each other is refused outright: the frame is already two
        // frames, and placing more nodes from it spreads the damage rather than diluting it.
        let shift = self.registration(a).filter(Frame::is_sound);
        // **Only a dump that measured the scale may update it.** Everything else inherits.
        if let Some(f) = shift.filter(|f| f.scale_measured) {
            self.screen_scale = ScreenScale(Some(f.scale));
        }
        // The same two steps for the interior, against the interior's own frame and its own scale.
        // `is_sound` is doing more work here than it does above: an interior that has been re-rolled
        // under us reads as a reflection, and a reflection is not a similarity, so the fit disagrees
        // with itself and nothing is placed.
        let inside_shift = self.inside_registration(a).filter(Frame::is_sound);
        if let Some(f) = inside_shift.filter(|f| f.scale_measured) {
            self.inside_frame.scale = Some(f.scale);
        }

        for n in &a.nodes {
            // Connections of a node inside a subworld are inside the same subworld: the dump lists
            // `locationData[playerLocation].connections` here and reaches the parent's connections
            // only through the separate exits section (`overworldview.lua:1030-1047`).
            let p = parent.clone();
            let place = self.entry(&n.key);
            place.observe_heading(&n.heading);
            place.connections = n.connections;
            if place.parent.is_none() {
                place.parent = p;
            }
            // Never cleared once set. A subnode we saw from inside the woods is in the woods, and a
            // later dump that happens not to name the container must not un-learn that.
            place.in_lost_woods |= lost_woods;
            // First fix wins. Averaging repeated sightings would be defensible, but a position that
            // never moves is easier to reason about and the registration below is exact, not
            // approximate — every reading of one node in one frame differs only by the shift.
            if place.pos.is_none() {
                if let Some(f) = shift {
                    place.pos = Some((n.x * f.scale + f.dx, n.y * f.scale + f.dy));
                }
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
                    place.observe_heading(&e.to_heading);
                }
                place.neighbours.insert(container.clone());
                self.entry(container).neighbours.insert(e.to_key.clone());
            }
        }

        // **The interior's own positions**, which no other frame can hold. See [`InsideFrame`].
        //
        // Doors go in beside rooms and under the same synthesised key, because a far hop can end on
        // either and the caller should not have to know which it got. First fix wins, as on the
        // surface: within one visit an interior node does not move, so a second reading can only
        // agree or be evidence that the frame is broken — and disagreement is measured above rather
        // than averaged away here.
        if let (Some((container, _)), Some(f)) = (a.subworld.as_ref(), inside_shift) {
            let place = |x: f64, y: f64| (x * f.scale + f.dx, y * f.scale + f.dy);
            for n in &a.nodes {
                self.inside_frame.pos.entry(n.key.clone()).or_insert_with(|| place(n.x, n.y));
            }
            for e in &a.exits {
                self.inside_frame
                    .pos
                    .entry(exit_node_key(container, &e.to_key))
                    .or_insert_with(|| place(e.x, e.y));
            }
        }

        // **This dump's own frame**, replacing whatever the last one left. Everything here shares one
        // offset and one zoom because one print produced it, which is what makes the entries
        // comparable without any of `registration`'s caveats — and why it is rebuilt rather than
        // merged. See [`WorldMap::frame`].
        //
        // The exits go in under their synthesised node keys. That name is the one thing a dump does
        // not give us about a door, and the one thing everything else asks for it by.
        // **Roll the steering measure forward before the old frame is thrown away.**
        //
        // **A `steered_gap` was maintained here**, and #57 removed it with the arm that read it.
        //
        // It was a high-water mark on the squared distance to the door — the nearest we had ever
        // been on this crossing — and it was the steer's guarantee that each step strictly improved
        // on the last. Taken here because the node we have just arrived at was a *neighbour* in the
        // frame about to be discarded, and that reading is the only one we ever get of where we now
        // stand: a dump prints its neighbours' positions and never the player's own.
        //
        // The measure was sound. What defeated it was that the frontier walk also moves us, and the
        // two arms did not share a device — see the note where the steer used to be. With one
        // ranking there is nothing to ratchet: `doorward` is re-read from the current frame every
        // step, and the walk cannot cycle because `probing_toward` holds the target and
        // `first_step_toward` routes to it.

        self.frame.clear();
        for n in &a.nodes {
            self.frame.insert(n.key.clone(), (n.x, n.y));
        }
        if let Some((container, _)) = a.subworld.as_ref() {
            for e in &a.exits {
                self.frame.insert(exit_node_key(container, &e.to_key), (e.x, e.y));
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
            // **Replaced, not accumulated, because completion is a thing the game takes away.**
            //
            // The dev, 2026-08-16: *the navigator is confused because the corruption of the node cut
            // off its neighbours.* Exactly that, and `core.setAreaIncomplete`
            // (`overworldview.lua:179-206`) is the whole mechanism:
            //
            // ```lua
            // overworldData.completedAreas[key] = nil
            // core.setAreaFlag(key..'_first_corrupt_time', … )
            // …
            // if overworldData.completedAreas[key..'_path_to_'..k] then
            //     overworldData.completedAreas[key..'_path_to_'..k] = false
            // end
            // ```
            //
            // A corrupted location is **removed** from this table and each of its roads is set to
            // **false**. This loop only ever wrote `true`, so both survived as stale completions:
            // `can_step` is `done(from) || done(to) || road(…)`, it went on saying yes, and the
            // game's `canTravelToDirect` — which needs a genuinely complete endpoint — went on
            // saying no. `spike-run-20260816-0310Z.md` ends there: `l10 -> l7` clicked into an
            // inactive Travel button **203 times**, and the map printed `l10 … [done] [corrupted]`,
            // which is the contradiction written out in full.
            //
            // So the table is read as the truth about this instant. Anything not in it is not
            // complete, whatever we believed a moment ago.
            for p in self.places.values_mut() {
                p.completed = false;
                p.completed_corrupt = false;
            }
            self.roads_done.clear();
            // Not every key here is a location. `setAreaComplete` also writes `<key>_corrupt` for a
            // corrupt area (`overworldview.lua:172`), and `setAreaIncomplete` manages
            // `<key>_path_to_<k>` entries for subworld exits (`:201`). Folding those in as places
            // invented destinations that routing would then try to walk to — `start_corrupt` showed
            // up as an unvisited frontier and became the plan.
            for (key, value) in &t.map {
                // **The value is read, and it is not always `true`.** A road cut by corruption stays
                // in the table carrying `false`; only the locations are removed outright. Iterating
                // the keys alone could not tell those apart.
                if value.as_bool() == Some(false) || matches!(value, crate::game::save::Value::Nil) {
                    continue;
                }
                if let Some(base) = key.strip_suffix("_corrupt") {
                    self.entry(base).completed_corrupt = true;
                } else if key.contains("_path_to_") {
                    // Recorded, not skipped. Still no `Place` — see `roads_done` for why that part
                    // of the original decision stands — but the completion itself is exactly what
                    // authorises a direct hop between the two nodes the key names, and dropping it
                    // made `can_step` stricter than the game.
                    self.roads_done.insert(key.clone());
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
            // **Whether a general store still has its heart, read rather than assumed.**
            //
            // `shop.load` keeps a shop's state in `areaFlags[<key>_shops][<storeType>]`
            // (`shop.lua:364`), and `core.save` writes it back when the shop *closes* (`:303-309`).
            // So three cases, and absence is the informative one:
            //
            // - no `generalStoreStock` sub-table: that store has never been opened, and buying
            //   requires opening it — so the heart is still there. Nothing is recorded here.
            // - a `healthBuff` entry with stock: still for sale. Nothing is recorded here either.
            // - the entry gone or at zero: sold, and the village stops being a destination.
            //
            // The sub-table matters rather than the flag: the live save carries `l19_shops` holding
            // only `innStock`, because an inn was visited there and the general store was not.
            //
            // And "opened" is not enough — it must have been *closed*. Our own run of 2026-08-15
            // opened `l11`'s store and stopped on the screen, and no `l11_shops` was written at all.
            // That is why the buying code presses the back arrow before it reads the save.
            let spent: Vec<String> = flags
                .map
                .keys()
                .filter_map(|k| k.strip_suffix("_shops"))
                .filter(|village| {
                    let base = format!("overworld.areaFlags.{village}_shops.generalStoreStock");
                    save.table_at(&base).is_some()
                        && save
                            .int_at(&format!("{base}.inventoryHash.{}.stock", crate::shopplay::HEART))
                            .unwrap_or(0)
                            <= 0
                })
                .map(|s| s.to_string())
                .collect();
            for k in spent {
                self.heart_bought.insert(k);
            }
            // **A settlement whose buildings are shut**, which the game decides entirely from these
            // two flags (`overworld/generators/village.lua:84-98`):
            //
            // ```lua
            // if areaFlag(location.key..'_attacked')
            // or areaFlag(location.parentNode.key..'_attacked')
            // or areaFlag(location.parentNode.key..'_attack').playerAttack then
            //     return location.typeData.destroyedAreaButtons or {}
            // end
            // return areaFlag(location.parentNode.key..'_attack')
            //     and location.typeData.underAttackAreaButtons or location.typeData.areaButtons
            // ```
            //
            // The dev, 2026-08-16: *the inn at a village that is under attack or lost cannot be
            // rested at.* The Lua is blunter than that even — under attack, `Enter` opens
            // `ui.building_empty` (`:371-388`) rather than `ui.inn`, so the button is **the same
            // word doing something else**; destroyed, the only button left is `Loot` (`:393-395`).
            // The general store carries the identical three sets (`:323`, `:332`), so this shuts the
            // heart errand as well as the bed.
            //
            // `_attack` is the live fight and clears when it is won (`village.lua:54`); `_attacked`
            // is the aftermath and does not.
            let sacked: Vec<String> = flags
                .map
                .keys()
                .filter_map(|k| k.strip_suffix("_attacked"))
                .map(|s| s.to_string())
                .collect();
            for k in sacked {
                self.entry(&k).sacked = true;
            }
            let besieged: Vec<String> = flags
                .map
                .keys()
                .filter_map(|k| k.strip_suffix("_attack"))
                .map(|s| s.to_string())
                .collect();
            for k in besieged {
                self.entry(&k).under_attack = true;
            }
            // **`_attack.playerAttack`, which is not the same statement as `_attack` existing.** The
            // flag is a table, and the game's destroyed test reads that one field of it
            // (`village.lua:93`): an attack in progress shuts the buildings, but an attack *we*
            // pressed has already finished them. Taken as a pair with the key so the borrow ends
            // before `entry` is called.
            let player_sacked: Vec<String> = flags
                .map
                .iter()
                .filter(|(_, v)| {
                    v.as_table().and_then(|t| t.get("playerAttack")).and_then(|v| v.as_bool())
                        == Some(true)
                })
                .filter_map(|(k, _)| k.strip_suffix("_attack"))
                .map(|s| s.to_string())
                .collect();
            for k in player_sacked {
                self.entry(&k).player_sacked = true;
            }
            // `<key>_looted`, a **count** rather than a flag — see [`Place::looted`].
            let looted: Vec<(String, u32)> = flags
                .map
                .iter()
                .filter_map(|(k, v)| Some((k.strip_suffix("_looted")?, v)))
                .map(|(k, v)| (k.to_string(), v.as_int().unwrap_or(0).max(0) as u32))
                .collect();
            for (k, n) in looted {
                self.entry(&k).looted = n;
            }
            // **Every road the save names is an edge, and we were throwing all of them away.**
            //
            // The dev, 2026-08-16: *why is the planner only choosing one node over when we've
            // explored so much of the map already? When we wanted to rest, the navigator could have
            // selected any of the uncorrupted villages we already visited, because a path existed.*
            //
            // It could not, and the reason was not the ranking. A place learned from `completedAreas`
            // arrives **as a key with no edges** — `entry(&k)` and nothing else — so most of the map
            // was known by name and unroutable, `ok()` refused every rest site for want of a route,
            // and the run fell through to stepping at an adjacent frontier node. That is what every
            // `RouteTo(Rest)` line in `spike-run-20260816-0826Z.md` is.
            //
            // But the edges were in the save the whole time, spelled into the road nodes' own names.
            // `overworldview.lua:1043` builds them as `parent.key..'_path_to_'..k`, so
            // `l25_path_to_l32` *is* the statement "l25 and l32 are adjacent" — and the save carries
            // 35 of them for this island, in `completedAreas` and in `areaFlags` (`_explored`).
            //
            // Node keys carry no underscores of their own (`l25`, `shrine1`, `e2`, `l10sub7`), so
            // splitting on the marker is unambiguous; the right-hand side only needs its flag suffix
            // taken off. Anything unrecognised is left alone rather than guessed at.
            let mut roads: Vec<(String, String)> = Vec::new();
            let flag_keys = flags.map.keys().map(|k| k.as_str());
            let done_keys = save
                .table_at("overworld.completedAreas")
                .map(|t| t.map.keys().map(|k| k.as_str()).collect::<Vec<_>>())
                .unwrap_or_default();
            for key in flag_keys.chain(done_keys) {
                let Some((from, rest)) = key.split_once("_path_to_") else { continue };
                let to = [
                    "_explored",
                    "_used",
                    "_corrupt",
                    "_attacked",
                    "_attack",
                    "_revealed",
                    "_looted",
                    "_first_corrupt_time",
                ]
                .iter()
                .find_map(|suffix| rest.strip_suffix(suffix))
                .unwrap_or(rest);
                if !from.is_empty() && !to.is_empty() {
                    roads.push((from.to_string(), to.to_string()));
                }
            }
            for (from, to) in roads {
                // The surface edge, which is the thing routing needs. The road node itself is an
                // interior node of `from` and is recorded wherever a dump names it.
                self.entry(&from).neighbours.insert(to.clone());
                self.entry(&to).neighbours.insert(from);
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
            // `<dataKey>subs`, the shrine submissions list — see [`Place::played`].
            //
            // **The dataKey is not the location key**, which the first attempt at this assumed. The
            // save holds `shrine1_shrine_subs`, so stripping only `subs` leaves `shrine1_shrine_` —
            // a name no shrine has. Live 2026-08-15 that set the flag on a phantom and, worse,
            // `entry()` minted the phantom as a place: `shrine1_shrine_`, `shrine2_shrine_` and two
            // more turned up in the run's own place list, exactly the invention this function's
            // opening comment warns about.
            //
            // So both suffixes come off, and the remainder must be non-empty to count.
            let played: Vec<String> = flags
                .map
                .keys()
                .filter_map(|k| k.strip_suffix("subs"))
                .filter_map(|k| k.strip_suffix("_shrine_"))
                .filter(|k| !k.is_empty())
                .map(|s| s.to_string())
                .collect();
            for k in played {
                self.entry(&k).played = true;
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
            // **The shrine roster, which the game hands over the instant the portal opens.**
            //
            // `events.hellOpens` sets `shrineN_explored = 1` for every shrine that exists and
            // **saves before the beams play** (`utils/events.lua:60-74`), rolling the flags back in
            // memory only so the animation has something to reveal. Its own comment says why: *so we
            // can save right away and main menu skip* — the path [`crate::navigate`]'s
            // `skip_cinematic` takes. So the roster is on disk either way, and reading it costs a
            // walk of nobody.
            //
            // Nothing else tells us. The adjacency dump lists **adjacent** nodes only, and its two
            // filters are `isCloudCovered` — never true for a neighbour, since adjacency is one of
            // its own escape clauses (`overworldview.lua:701-706`) — and `locationIsVisible`, which
            // is about *secret* nodes rather than fog (`:554-556`). The fog lifting changes what a
            // human sees and nothing about what is printed to us.
            //
            // **Keys only, and that is the honest limit of it.** A `Place` made here has no heading,
            // no edges and no position, so it cannot be routed to and will not be steered toward. It
            // says *this shrine exists and here is its name* — which is the difference between a run
            // that knows there are seven to find and one that cannot tell whether four are findable
            // at all. It does not move [`WorldMap::consecrations`], which counts what has been
            // finished rather than what has been revealed.
            //
            // Major shrines only. [`key_is_major_shrine`] is `shrine` followed by digits and nothing
            // else, so `shrine1_plaza`, `shrine1sub2` and `shrine1_path_to_l4` are all excluded —
            // the first two belong to a shrine we have already listed, and inventing places for the
            // third is the mistake `roads_done` exists to avoid.
            let roster: Vec<String> = flags
                .map
                .keys()
                .filter_map(|k| k.strip_suffix("_explored"))
                .filter(|k| key_is_major_shrine(k))
                .map(|s| s.to_string())
                .collect();
            for k in roster {
                self.entry(&k);
            }
            self.lend_a_shrine_its_plaza_the_rewards_it_earned();
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
        // **Only when this save can answer.** Mid-fight it cannot: the effects are in
        // `combatSaveData` and there is no `statusEffects` table here at all, which the old
        // reading summed to zero. See [`crate::rest::well_rested_from`], and
        // [`WorldMap::note_well_rested`] for where the other copy comes in.
        if let Some(w) = crate::rest::well_rested_from(save, crate::rest::STATUS_IN_MAIN) {
            self.well_rested = w;
        }
        // **Health belongs here with gold and fuel, not in the hands of whoever took the reading.**
        //
        // It used to be the caller's job: four sites in the driver did `let now = r.apply_save()`
        // and then `note_health_level(now)` by hand, and the fifth — the one at the end of a rest —
        // read the save, printed `health is now 20/20`, and dropped the reading on the floor. Live
        // 2026-08-15 at The Quacking Duck the errand therefore survived its own completion and the
        // run walked back into the inn it had just filled up in.
        //
        // A fact that has to be re-plumbed at every call site is a fact that will be missed at one
        // of them. See [`WorldMap::note_health_level`] for what it does with the number; it sets
        // below half, clears at full, and deliberately leaves a partial heal alone.
        if let Some(h) = crate::rest::Health::from_save(save) {
            self.health = Some(h);
            self.note_health_level(h);
        }
    }

    /// **How many major shrines are consecrated**, counted by shrine rather than by node.
    ///
    /// A consecration can be recorded against either half of a promoted pair — `setShrineLocation`
    /// reassigns to the parent when the plaza's `parentNode.majorShrine` (`shrine.lua:420-427`), and
    /// the save may carry the flag under `shrineN` or `shrineN_plaza` depending on which node the
    /// game was standing on. Both satisfy [`Place::can_be_consecrated`], so counting places would
    /// count one shrine twice and let three shrines pass for four.
    ///
    /// Keys are folded to the shrine itself, which is the thing the dev's rule is about.
    /// **How many major shrines we know the name of**, revealed or walked to.
    ///
    /// The denominator to [`WorldMap::consecrations`], and the number that says whether
    /// [`SHRINES_BEFORE_THE_ANOMALY`] is reachable in this world at all. A world generates up to
    /// seven (`overworld/generators/world.lua:81`) and nothing guarantees all of them are placed.
    ///
    /// Diagnostic today: the gate counts consecrations and the release turns on an exhausted
    /// frontier, so neither reads this. It is logged at the start of a run because a reader
    /// otherwise cannot tell a run that had shrines to spare and ignored them from one that only ever
    /// found two.
    pub fn shrines_known(&self) -> usize {
        self.places.values().filter(|p| key_is_major_shrine(&p.key)).count()
    }

    pub fn consecrations(&self) -> usize {
        self.places
            .values()
            .filter(|p| p.consecrated && p.can_be_consecrated())
            .map(|p| p.key.strip_suffix("_plaza").unwrap_or(&p.key))
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    }

    /// **Are we standing on the node we set out for, to do the one thing it is entered with?**
    ///
    /// The step whose next action is certain before anything is read: a single press on the area
    /// slot. `committed_to` is the target the last hop was taking us to and `here` is where the
    /// arrival dump put us, so both halves are known without asking the screen.
    ///
    /// Its one caller uses this to take the cheaper of two locate-me forms
    /// ([`crate::navigate::Run::select_here`] rather than `recentre`) — **not** to skip the press
    /// altogether, which was tried on 2026-08-20 and ended a run. See the note at
    /// `Run::recentre` for why the arrow press cannot go: it is what selects our own location.
    ///
    /// A fight and a settlement, because both are one press on that slot — `Combat` and `Enter`.
    /// The dev raised the second: *it seems that we re-centered before entering Boreas town*, and
    /// the first version's combat-only rule was drawn on a distinction that does not exist. What
    /// happens *after* the press differs wildly; what this asks about is the press.
    ///
    /// Deliberately silent about a step that will click a **node**, where the coordinates a full
    /// re-centre refreshes are the entire point.
    pub fn standing_on_what_we_came_for(&self, committed_to: Option<&str>) -> bool {
        let Some(here) = self.here.as_deref() else { return false };
        if committed_to != Some(here) {
            return false;
        }
        self.places.get(here).is_some_and(|p| {
            p.parent.is_none() && ((p.has_combat() && !p.completed) || p.has_an_inn())
        })
    }

    /// Is a single step from `from` to `to` legal?
    ///
    /// ## A single step is not the only move the game offers, and we only ever make single steps
    ///
    /// The dev, 2026-08-15: *so far we've only been hopping to adjacent nodes with our inputs. The
    /// game allows us to go directly to visited nodes in general; only the corruption can block off
    /// a previously created path.*
    ///
    /// Correct, and confirmed in source. `core.mousereleased` (`overworldview.lua:1479`) hands a
    /// clicked location to `canTravelToIndirect`, which is a **breadth-first search from the player**
    /// (`:1330-1373`) building a `pathHash`; `core.travelTo` (`:1394`) then walks the whole path in
    /// one action. So clicking a distant node travels there, however many hops away it is.
    ///
    /// The chaining rule is looser than this function, which is worth knowing before anyone models
    /// it: the **first** hop is `canTravelToDirect`, but every hop after it is `couldTravelBetween`
    /// (`:1323-1328`), which adds `core.locationIsCompleteOnVisit(location1)` to the disjunction — a
    /// node that completes merely by being visited chains even with no completion flag on either
    /// side. And the dev's "only corruption blocks it" is exactly right about the mechanism:
    /// `setHellValue` resets areas to incomplete, and `areaOrExitToComplete` is the whole of the
    /// condition, so corruption is the one thing that can cut a path we have already walked.
    ///
    /// **What stops us using it is not the rule, it is the pixel.** Selecting a distant node means
    /// clicking where it is drawn, and a dump prints positions only for nodes adjacent to us (plus
    /// subworld exits) — so the coordinate for anywhere further is not on the feed. It is
    /// *derivable*: [`WorldMap::registration`] anchors each dump against nodes that already carry a
    /// position, which is what steering already uses to aim at a door the current dump does not
    /// name. That is task #21, and this is the payoff it was always for: routing would stop being a
    /// sequence of hops, each with its own pan, steer and settle, and become one click.
    ///
    /// Until then every move here is one hop, and this is the rule for one hop.
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
    /// **Advisory, not a filter.** It is deliberately not used to prune routes: a node we have merely
    /// never heard of reads as incomplete, so pruning on it removed every route in a map that had not
    /// been walked yet. It answers one question only: *must* we fight where we stand, or can we
    /// simply leave?
    ///
    /// ## The exit clause, which this used to be missing
    ///
    /// `areaOrExitToComplete` is `areaIsComplete(a) or areaIsComplete(a..'_path_to_'..b)`, and
    /// `canTravelToDirect` asks it **both ways round** — so a walked road authorises the hop between
    /// the two nodes it joins, whether or not either node has been cleared. This modelled only the
    /// first half of each side, which made it strictly stricter than the game: a legal move read as
    /// illegal, `must_fight_here` came back true, and the run fought or crossed something it could
    /// have walked away from. That is the same shape as the bug in the paragraph above, one level
    /// down.
    ///
    /// Live 2026-08-15 at `l62`, a level 7 spider forest: `completedAreas` held
    /// `l62_path_to_l57 = true`, so stepping out to `l57` was legal, and this said it was not.
    /// (It was right about the move the run actually wanted — neither `l62`, `l62_path_to_l36` nor
    /// `l36` was complete, so crossing the interior was genuinely the only way to `l36`. The dev
    /// spotted the general fault from a case where the answer happened to come out correct.)
    pub fn can_step(&self, from: &str, to: &str) -> bool {
        let done = |k: &str| self.places.get(k).map(|p| p.completed).unwrap_or(false);
        let road = |a: &str, b: &str| self.roads_done.contains(&format!("{a}_path_to_{b}"));
        done(from) || done(to) || road(from, to) || road(to, from)
    }

    /// Are we in a position to want a heart at all?
    ///
    /// The dev's rule, 2026-08-15: **while the goal is the anomaly and we hold more than
    /// [`HEART_COST`] gold, a heart reachable without a fight is worth the detour.** The anomaly
    /// costs a level 8 fight and the two before it cost this run its life at level 6 — four maximum
    /// health for a hundred gold is the cheapest preparation on the board.
    ///
    /// **"While the goal is the anomaly" was read as "the anomaly is open", and that was wrong.**
    /// The dev, 2026-08-16: *we should want the healthBuff regardless of anomaly state.*
    ///
    /// The misreading survived because it could not be seen. Every run the original rule was written
    /// against had already opened the portal — every checkpoint in the store reads `anomaly OPEN` —
    /// so the shut state was never played until the first adventure started from a cleared profile,
    /// and then it showed at once: a village with a bed and a shelf, over 110 gold in hand, and the
    /// shelf never asked about.
    ///
    /// Nothing in the reasoning depended on the portal. Opening it means winning a fight at a combat
    /// node above level 3 (`overworld/events/arrived/world_evil.lua:15-21`) — the level 6 fights that
    /// killed the earlier runs are on *this* side of the trigger — so the preparation is worth as
    /// much before as after. Unlike the shrine detour beside it, which really is windowed:
    /// consecrating needs `hell ~= 0` (`shrine.lua:93-96`) and cannot be done early at any price.
    ///
    /// **The floor is the price plus a bed**, which the dev raised to that on 2026-08-15 after
    /// watching the errand drain a purse: *I want us to be at at least 110 gold before buying so
    /// that we can rest if needed afterwards.* Spending down to nothing buys four maximum health and
    /// removes the six-a-press that keeps a run alive between fights, which is a bad trade at any
    /// price. So [`HEART_COST`] plus [`crate::rest::INN_COST`], and the same reserve is held back
    /// again when emptying a shelf.
    /// Has this village's heart already been bought? See [`WorldMap::heart_bought`].
    pub fn heart_is_spent(&self, village: &str) -> bool {
        self.heart_bought.contains(village)
    }

    /// The driver's record that a village's heart is spent — see [`WorldMap::heart_bought`].
    pub fn bought_the_heart(&mut self, village: &str) {
        self.heart_bought.insert(village.to_string());
    }

    /// Standing here, is this shrine worth consecrating before moving on?
    ///
    /// This is where "unless it is on the shortest direct path to the anomaly" actually lives.
    /// Routing cannot honour that clause — see the note in [`WorldMap::next_target`] — so it is
    /// asked on arrival instead, when the cost is only the shrine itself and not a detour.
    ///
    /// An uncorrupted shrine is always worth it. A corrupted one costs a fight, and earns it only
    /// when we are walking through anyway — **or when that fight has already been won**.
    ///
    /// That last clause is the whole point of the `completed` test below. Corruption is not a
    /// property of the shrine, it is a bill: a fight standing between us and the shrine screen. Once
    /// the node is complete the bill is paid and cannot be paid again, so refusing on the grounds of
    /// cost is refusing over money already spent. Live 2026-08-12 a run resumed into the fight at
    /// `shrine1`, won it in two turns, and walked away from the shrine because the node was still
    /// flagged corrupted and no route to the anomaly was known — declining a free consecration on
    /// the strength of a fight it had just finished.
    /// Copies a major shrine's earned flags onto its plaza, because the game records them there.
    ///
    /// ## The mismatch this exists for
    ///
    /// A shrine forest has two places that are the same shrine: the overworld node `shrineN`, and
    /// `shrineN_plaza`, the node you actually stand on inside it. `setShrineLocation`
    /// (`shrine.lua:420-427`) collapses them — standing on the plaza, `shrineLocation` becomes the
    /// **parent** — so every reward the game writes goes to `shrineN`. `doPray` calls
    /// `setAreaUsed(shrineLocation)` (`:133`), and `<dataKey>subs` and `_consecrated` follow the same
    /// promoted key.
    ///
    /// [`WorldMap::apply_save`] projects a flag onto the key it names and no other, so
    /// `shrineN_plaza` came back `used = false` from a shrine that had just been prayed at — and
    /// there is no flag the game will ever write that would clear it.
    ///
    /// **Measured, not predicted.** The save after the run of 2026-08-17 holds `shrine1_used = true`
    /// with no `shrine1_plaza_used` beside it, and the checkpoint from before that run has neither.
    /// So the prayer at the plaza landed, and it landed on the parent.
    ///
    /// ## Why this was fixed before the next run rather than filed
    ///
    /// [`WorldMap::shrine_inside`] offers any shrine in the container with `!used` as an errand, and
    /// the arrival branch enters on `!used || worth_consecrating_here`. Both would send the run back
    /// to a plaza it had already prayed at, where `showPrayButton` needs
    /// `areaUnused(shrineLocation.key)` (`shrine.lua:101`) — false, since the parent is used — so no
    /// button appears, `play` returns neither a prayer nor a consecration, and the pre-MVP rule stops
    /// the run. [`WorldMap::abandon`] masks it within a single run only: `abandoned` is in-memory and
    /// every run starts with it empty.
    ///
    /// ## One way, and only these three flags
    ///
    /// Parent to plaza, because that is the direction the game's promotion runs. `corrupted` is
    /// deliberately not carried: it comes from `_first_corrupt_time`, it is a statement about the
    /// area and its fight rather than about the shrine's rewards, and nothing has shown the two nodes
    /// share it.
    ///
    /// Written with `get_mut` rather than [`WorldMap::entry`] so it can never mint a place. Inventing
    /// `shrineN_plaza` for a shrine whose forest we have never entered is exactly the phantom-place
    /// failure recorded on the `subs` parsing in [`WorldMap::apply_save`]. A plaza we have not met yet
    /// is picked up by the next `apply_save` after a dump introduces it.
    fn lend_a_shrine_its_plaza_the_rewards_it_earned(&mut self) {
        let plazas: Vec<String> = self
            .places
            .keys()
            .filter(|k| k.strip_suffix("_plaza").is_some_and(key_is_major_shrine))
            .cloned()
            .collect();
        for plaza in plazas {
            let Some(parent) = plaza.strip_suffix("_plaza") else { continue };
            let Some(p) = self.places.get(parent) else { continue };
            let (used, played, consecrated) = (p.used, p.played, p.consecrated);
            let Some(here) = self.places.get_mut(&plaza) else { continue };
            here.used |= used;
            here.played |= played;
            here.consecrated |= consecrated;
        }
    }

    pub fn worth_consecrating_here(&self, key: &str) -> bool {
        let Some(p) = self.places.get(key) else { return false };
        if !p.is_shrine() || p.consecrated {
            return false;
        }
        // **A minor shrine owes a prayer and never a consecration** — see
        // [`Place::can_be_consecrated`]. Without this the answer here is yes for a woodland shrine
        // the moment the anomaly opens, and the arrival branch's `!used || worth_consecrating_here`
        // sends the run back into one it has already prayed at. There it finds no `Consecrate`
        // (impossible) and no `Pray` (`areaUnused` is already false), returns neither, and the
        // pre-MVP rule stops the run over an interaction that was never available.
        if !p.can_be_consecrated() {
            return false;
        }
        // Consecrating needs the anomaly open at all (`shrine.lua:93-96`).
        if !self.anomaly_is_open().unwrap_or(false) {
            return false;
        }
        if !p.corrupted || p.completed {
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
        use std::cmp::Reverse;
        use std::collections::BinaryHeap;

        let mut out: BTreeMap<String, usize> = BTreeMap::new();
        let mut heap: BinaryHeap<(Reverse<usize>, String)> = BinaryHeap::new();
        heap.push((Reverse(0), from.to_string()));
        while let Some((Reverse(d), key)) = heap.pop() {
            if out.contains_key(&key) {
                continue;
            }
            out.insert(key.clone(), d);
            if let Some(p) = self.places.get(&key) {
                for n in &p.neighbours {
                    if !out.contains_key(n) {
                        heap.push((Reverse(d + self.step_cost(&key, n)), n.clone()));
                    }
                }
            }
        }
        out
    }

    /// What one hop from `a` to `b` is really worth, in hops.
    ///
    /// **A hop we can walk and a hop we have to carve are not the same move, and this map counted
    /// them the same.** An edge into a subworld container is one line in the graph and up to a dozen
    /// actions in the world: enter, cross the interior node by node, fight what stands on the road,
    /// find the exit and leave. Counting that as `1` is what made a route through a level 7 spider
    /// forest look shorter than a route around it.
    ///
    /// Live 2026-08-15, twice, and reported both times by the dev. From `shrine7` the planner picked
    /// `l62 -> l36`, two hops to the target, and paid for it with a five-node crossing that then
    /// stalled on an exit it could not pan to. `l62 -> l57` was **one hop longer on paper and free in
    /// practice**: `l62_path_to_l57` was already complete, so `canTravelToDirect` allowed it
    /// outright, and `l57` reaches every unprayed shrine without touching `l62` again.
    ///
    /// [`WorldMap::can_step`] is the whole test, which is why it had to learn about walked roads
    /// first — before that it could not tell the free hop from the expensive one either.
    ///
    /// ## Why this does not quietly become "prefer the ground we know"
    ///
    /// On unexplored map nothing is complete and no road is walked, so **every** edge costs
    /// [`CROSSING`] and the ordering is exactly the uniform one this replaced. The weighting only
    /// starts to matter once we have cleared something, which is precisely when there is a cheap
    /// route to prefer. A run that has explored nothing still explores by hop count.
    fn step_cost(&self, a: &str, b: &str) -> usize {
        match self.can_step(a, b) {
            true => 1,
            false => CROSSING,
        }
    }

    /// First move on a shortest known path from `from` to `to`, or `None` if we know of no route.
    ///
    /// With `shun`, places marked [`Place::avoid`] are treated as walls.
    /// The first hop of a route, **preferring the paved path**.
    ///
    /// The dev's rule, from playing the game: inside a subworld, **stick to the road**. Two passes,
    /// first match wins:
    ///
    /// ```text
    ///   1. paved     the road the generator strung from entrance to exit
    ///   2. anything  no road known yet, or the road does not go where we are going
    /// ```
    ///
    /// ## Routing round combat used to be pass 2 of 4, and is gone
    ///
    /// The old cascade put "any type, no fight on it" *above* "paved, fight and all", so a combat
    /// node on the road sent the route into the brush. The dev has withdrawn that: it is what the
    /// live run of 2026-08-08 spent twenty-four steps doing — road, brush, back to the road, never
    /// reaching the exit — because every detour round a nest is also a detour away from the only
    /// structure in the map that leads anywhere. `forest.lua:117-130` strings `road` and
    /// `crossroads` from entrance to exit; the brush is filler.
    ///
    /// So a fight on the road is now simply crossed. What decides whether that fight is *survivable*
    /// is destination choice, upstream — `Risk`, `hostile_to_enter`, and `next_target`'s two passes,
    /// which still keep a hurt run from picking hostile ground to walk into. Once we are on the
    /// path, the path is the path.
    ///
    /// Paved nodes are still subject to `blocked`, so an abandoned or shunned road is skipped by
    /// both passes — those are hard exclusions, this is a preference. `to` is exempt from both, for
    /// the same reason: refusing to path to where we were told to go is not routing.
    /// Will the game let us walk straight from `a` to `b`, given what we know is cleared?
    ///
    /// Our model of `core.canTravelToDirect` (`overworldview.lua:1316-1321`), minus the two secret
    /// clauses we cannot see:
    ///
    /// ```lua
    /// return isAdjacent(location2)
    /// and (core.areaOrExitToComplete(location1.key, location2.key)
    ///      or core.areaOrExitToComplete(location2.key, location1.key))
    /// ```
    ///
    /// and `areaOrExitToComplete(x, y)` is `areaIsComplete(x) or areaIsComplete(x..'_path_to_'..y)`
    /// (`:1312-1314`) — which is why the *road* nodes count, and why they are keyed exactly as
    /// [`exit_node_key`] builds them.
    ///
    /// **Conservative by construction.** Beyond the first ring the game actually uses
    /// `couldTravelBetween` (`:1323-1328`), which additionally admits a `locationIsCompleteOnVisit`
    /// node — a peaceful one. We cannot evaluate that from a heading, so this under-approximates:
    /// worst case we stop a multi-hop short and take an ordinary step, which is what we did before
    /// any of this existed.
    pub fn can_travel_direct(&self, a: &str, b: &str) -> bool {
        let done = |k: &str| self.places.get(k).map(|p| p.completed).unwrap_or(false);
        let adjacent = self
            .places
            .get(a)
            .map(|p| p.neighbours.contains(b))
            .unwrap_or(false);
        adjacent
            && (done(a) || done(b) || done(&exit_node_key(a, b)) || done(&exit_node_key(b, a)))
    }

    /// The furthest node along our own route to `to` that we may select and travel to in one press.
    ///
    /// The dev, 2026-08-16: *make it possible for the navigator to directly hop to distant, visited
    /// nodes as long as the corruption hasn't cut off the path.* The game will do the walking —
    /// `travelTo` takes any node `canTravelToIndirect` can reach and paths to it
    /// (`overworldview.lua:1394-1400`) — and we were taking one node per press, each costing a
    /// click, sometimes a pan, and up to a minute of arrival wait.
    ///
    /// **It is not a teleport, and that is what shapes the rules below.** `:1210-1216` calls
    /// `core.arriveAt` at *every* node on the path, so every intermediate arrival fires its events
    /// and prints its dump. Nothing is skipped and the map still accumulates — but nothing is
    /// avoided either.
    ///
    /// So the chain stops at the first node that fails any of:
    ///
    /// - **the game would refuse the step** ([`WorldMap::can_travel_direct`]) — which is exactly the
    ///   dev's "as long as the corruption hasn't cut off the path", since `setAreaIncomplete`
    ///   (`overworldview.lua:179-206`) is what takes completion away from a node and every one of
    ///   its roads;
    /// - **it would open the anomaly** while the portal is shut — arrivals fire on the way, so a
    ///   walk-through is as fatal to the level 4 rule as a destination would be;
    /// - **it is worth stopping at** — an unconsecrated shrine we would otherwise stride past,
    ///   since the driver only acts on the node it ends its step on.
    ///
    /// Surface only: [`crate::subworld::Rules::multi_hop_travel`] is false inside a subworld, where
    /// selecting a distant node moved a live run 0.015 of a screen and nothing else.
    ///
    /// Returns `None` when the answer is the ordinary single step, so the caller can keep doing what
    /// it did before. This is an accelerator and never a different route: it walks the same
    /// [`WorldMap::first_step_toward`] the hops would have taken, one node at a time, so the two can
    /// never disagree about which way we are going.
    pub fn far_hop(&self, from: &str, to: &str) -> Option<String> {
        self.far_hop_chain(from, to).into_iter().next()
    }

    /// Every node [`WorldMap::far_hop`] would accept, **furthest first**.
    ///
    /// `far_hop` is this list's head, and that is all it ever was. The list exists because the
    /// caller has one question this cannot answer: whether the node is somewhere it can *click*.
    /// A hop that is off the clickable map used to be discarded whole and replaced by a single step
    /// — the dev, 2026-08-22: *if it makes it more reliable and simpler, fast-hop to the farthest
    /// node on the path that's still visible without panning.* Walking down from the far end is
    /// exactly that, and its worst case is the adjacent node, which is today's fallback, so it
    /// cannot be worse than what it replaces.
    ///
    /// Live 2026-08-22, `l4` toward `l4_path_to_shrine1`: refused six times running for being off
    /// the map, the door travellable in one press throughout, one of the refusals 86 px past the
    /// right edge with its y already on screen.
    pub fn far_hop_chain(&self, from: &str, to: &str) -> Vec<String> {
        if self.inside().is_some() {
            return Vec::new();
        }
        self.far_chain_all(from, to, &|p: &Place| self.worth_consecrating_here(&p.key))
    }

    /// Walks our own route to `to`, as far as the game would carry us in one press.
    ///
    /// Shared by [`WorldMap::far_hop`] and [`WorldMap::far_hop_inside`], which differ only in
    /// `stop_at` — the question "is this intermediate node one we must not stride past?". Keeping one
    /// walker means the two can never disagree about the *route*, only about where to get off it.
    fn far_chain_all(
        &self, from: &str, to: &str, stop_at: &dyn Fn(&Place) -> bool,
    ) -> Vec<String> {
        if from == to {
            return Vec::new();
        }
        let mut cur = from.to_string();
        // Every node the chain could legally end on, nearest first.
        let mut reach: Vec<String> = Vec::new();
        // Bounded so a cycle in our own edges cannot spin here. Ten hops is far past the point where
        // the saving matters.
        for _ in 0..10 {
            let Some(next) = self.first_step_toward(&cur, to, true) else { break };
            if !self.can_travel_direct(&cur, &next) {
                break;
            }
            reach.push(next.clone());
            if next == to {
                break;
            }
            // Worth going *to* — a shrine we would otherwise stride past, which is a place we want
            // to be rather than one we want to avoid. An unknown node stops the chain too.
            if self.places.get(&next).map(stop_at).unwrap_or(true) {
                break;
            }
            cur = next;
        }
        // **The longest hop whose every shortest route is gentle**, rather than the longest hop.
        //
        // The dev, 2026-08-17: *don't remove fast hops because of potential intermediate level 4+
        // nodes. Use BFS to find all the shortest paths from source to destination based on what is
        // known; if none of the shortest paths contain a lv4+ node, you're clear to do the fast hop.*
        //
        // The first version of this broke the chain at the first dangerous node it walked past, which
        // threw away the hop over a node the game might never route through. What decides it is not
        // our route but the game's, and the game's is a **shortest** path — see
        // [`WorldMap::shortest_paths_are_gentle`] for why that is a fact about
        // `canTravelToIndirect` rather than an assumption.
        //
        // Furthest first, so a long hop is preferred and a shorter one is the fallback rather than
        // the whole answer.
        //
        // **All of them rather than the first**, so a caller that cannot use the furthest can take
        // the next one down instead of falling all the way back to a single step. See
        // [`WorldMap::far_hop_chain`] for the ruling that asked for it.
        reach
            .iter()
            .rev()
            .filter(|k| self.shortest_paths_are_gentle(from, k, k.as_str() == to))
            // One hop is not a multi-hop. Anything the caller could have worked out itself is left
            // out of the list entirely.
            .filter(|k| !self.can_step_is_adjacent(from, k))
            .cloned()
            .collect()
    }

    /// Distance in hops from `origin` to every node our own edges can reach.
    ///
    /// **Not [`WorldMap::distances`]**, which is what almost everything else wants: that prices a
    /// step through [`WorldMap::can_step`], so an edge the game would not currently let us walk in
    /// one press costs `CROSSING` rather than 1. Right for routing, wrong for a rule phrased in
    /// hops — the dev's minor-shrine condition of 2026-08-22, *one hop or less*, is about the map's
    /// shape and not about what is traversable this minute. An unvisited woodland shrine next door
    /// is one hop away and `CROSSING` to reach, and reading that as "far" is what made the first cut
    /// of that rule refuse every minor shrine in the game.
    fn hops_from(&self, origin: &str) -> BTreeMap<String, usize> {
        let mut dist: BTreeMap<String, usize> = BTreeMap::new();
        dist.insert(origin.to_string(), 0);
        let mut queue: std::collections::VecDeque<String> = [origin.to_string()].into();
        while let Some(k) = queue.pop_front() {
            let d = dist[&k];
            let Some(p) = self.places.get(&k) else { continue };
            for n in &p.neighbours {
                if !dist.contains_key(n) {
                    dist.insert(n.clone(), d + 1);
                    queue.push_back(n.clone());
                }
            }
        }
        dist
    }

    /// Is `b` a neighbour of `a` in our own graph? Used to tell a multi-hop from an ordinary step.
    fn can_step_is_adjacent(&self, a: &str, b: &str) -> bool {
        self.places.get(a).map(|p| p.neighbours.contains(b)).unwrap_or(false)
    }

    fn first_step_toward(&self, from: &str, to: &str, shun: bool) -> Option<String> {
        // **Pass 2 must not be removed.** A subworld can have no road at all between here and the
        // destination, and a run that refuses every route has stalled — which is worse than any
        // single bad step.
        let passes: [&dyn Fn(&Place) -> bool; 2] =
            [&|p: &Place| !p.is_paved(), &|_: &Place| false];
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
    use super::fixtures::*;
    use crate::observe::adjacency::{Exit, Node};

    /// A promoted shrine is **one** consecration, whichever half of the pair carries the flag.
    ///
    /// `setShrineLocation` reassigns to the parent when the plaza's `parentNode.majorShrine`
    /// (`shrine.lua:420-427`), so the save may record it under either key. Counting places rather
    /// than shrines would let three shrines pass for four.
    #[test]
    fn a_shrine_and_its_plaza_are_one_consecration_not_two() {
        let mut m = WorldMap::new();
        for key in ["shrine1", "shrine1_plaza"] {
            let p = m.entry(key);
            p.consecrated = true;
            p.used = true;
        }
        assert_eq!(m.consecrations(), 1, "one shrine, two nodes");

        // And a minor shrine is not one at all: `Consecrate` needs `majorShrine`
        // (`shrine.lua:93-96`), so a woodland shrine can never take the flag.
        m.entry("shrine1sub1").consecrated = true;
        assert_eq!(m.consecrations(), 1, "a subworld shrine cannot be consecrated at all");
    }

    /// The bank reads out of `mainSaveData`, and zero is an **absent key**.
    ///
    /// `affectPlayerStatus` deletes a status the moment it reaches zero (`overworld.lua:45-47`), so
    /// a spent-out character has no entry at all rather than a stored `0`. Every shape below was
    /// transcribed from a checkpoint under `checkpoints/`, which is where the positive control
    /// lives: without one, "we read no stacks" and "there were no stacks to read" are the same
    /// result.
    #[test]
    fn well_rested_stacks_read_out_of_the_save() {
        let read = |status: &str| {
            let mut m = WorldMap::new();
            m.apply_save(
                &crate::game::save::parse(&format!("return {{ player = {{ {status} }} }}")).unwrap(),
            );
            m.well_rested
        };

        // `at-woodland-shrine-unprayed`, the furthest state in the store.
        assert_eq!(read("statusEffects = { wellRestedInn = 11 }"), 11);
        // `at-shrine1` — the two a fresh character carries (`rpg/classes/warrior.lua:69`).
        assert_eq!(read("statusEffects = { wellRestedCampfire = 2 }"), 2);
        // `before-clear`.
        assert_eq!(read("statusEffects = { wellRestedInn = 5 }"), 5);
        // `pre-anomaly` and `anomaly-open`, both spent out.
        assert_eq!(read("statusEffects = {}"), 0);
        // And a save with no `statusEffects` at all, which is what the live file held after the
        // 2026-08-20 death.
        assert_eq!(read("health = 50"), 0);

        // Both flavours at once. The heal does not care which it spends and takes the campfire one
        // first (`rpgview.lua:1204-1209`), so the bank is their sum.
        assert_eq!(read("statusEffects = { wellRestedCampfire = 2, wellRestedInn = 3 }"), 5);
    }

    /// **The shrine roster arrives with the portal**, out of the save the cutscene writes early.
    ///
    /// `events.hellOpens` flags every shrine and saves *before* the beams (`utils/events.lua:60-74`)
    /// so the main-menu skip keeps them — which is the skip `skip_cinematic` performs. The flags are
    /// the only channel: the adjacency dump lists adjacent nodes and says nothing about a shrine
    /// three hops away, revealed or not.
    #[test]
    fn the_shrines_are_named_by_the_save_when_the_portal_opens() {
        let mut m = WorldMap::new();
        assert_eq!(m.shrines_known(), 0, "before the portal, nothing has named them");
        // A vantage point, or `can_route_to` short-circuits to `true` for want of anywhere to stand
        // and the assertions below would prove nothing.
        m.fold(&dump("here", "camp", vec![node("l4", "Bainton Clump road")]));
        m.here = Some("here".into());

        // Seven shrines flagged, which is what `hellOpens` writes for a full world. The noise around
        // them is real save content, and every piece of it must be ignored.
        m.apply_save(
            &crate::game::save::parse(
                "return { overworld = { areaFlags = {
                     hell = 0.1,
                     shrine1_explored = 1, shrine2_explored = 1, shrine3_explored = 1,
                     shrine4_explored = 1, shrine5_explored = 1, shrine6_explored = 1,
                     shrine7_explored = 1,
                     shrine1_plaza_explored = 1,
                     shrine1sub2_explored = 1,
                     shrine1_path_to_l4_explored = 1,
                     l7_explored = 0,
                 } } }",
            )
            .unwrap(),
        );

        assert_eq!(m.shrines_known(), 7, "the whole roster, and nothing but the roster");
        for i in 1..=7 {
            assert!(m.get(&format!("shrine{i}")).is_some(), "shrine{i} is on the map");
        }

        // **Keys only.** Nothing here can be routed to, and saying so is the point: the roster tells
        // the run what to look for, not where it is.
        let s1 = m.get("shrine1").unwrap();
        assert_eq!(s1.heading, "", "no dump has named it");
        assert!(!m.can_route_to("shrine2"), "and nothing in the roster brings an edge with it");

        // **`shrine1` is the exception, and it is not this fold's doing.** The fixture carries
        // `shrine1_path_to_l4_explored`, and a road's *name* is the statement that its two ends are
        // adjacent — which `apply_save` has parsed into an edge since long before the roster
        // existed. So a shrine whose road has been flagged arrives routable, and one whose road has
        // not arrives as a name. Both are worth having and only the second needed writing.
        assert!(m.can_route_to("shrine1"), "the road flag names the edge, so this one can be reached");
        assert!(m.can_route_to("l4"), "and the control: the vantage point really does route somewhere");

        // The plaza, the subnode and the road are not shrines in the roster's sense. The plaza and
        // subnode belong to a shrine already counted; the road is the mistake `roads_done` exists to
        // avoid.
        assert!(!key_is_major_shrine("shrine1_plaza"));
        assert!(!key_is_major_shrine("shrine1sub2"));
        assert!(!key_is_major_shrine("shrine1_path_to_l4"));

        // And an ordinary node's fog flag makes nothing at all. `l7_explored = 0` is a live shape:
        // the value is a highlight that decays (`overworldview.lua:1167-1169`), so zero still means
        // explored — and it is still not a shrine.
        assert!(m.get("l7").is_none(), "an ordinary explored node is not roster business");

        // **The roster is not the gate.** `consecrations` counts what has been finished, and knowing
        // seven names finishes none of them.
        assert_eq!(m.consecrations(), 0);
    }

    /// **What we walked to, and what is entered with one press on the area slot.**
    ///
    /// The condition behind taking `select_here` instead of a full `recentre` — the selection
    /// without the twelve-second wait for a pan nobody is going to use.
    #[test]
    fn what_we_came_for_is_known_before_the_camera_is_asked() {
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("l4", "Riccall — level 6 crypt")]));
        m.here = Some("l4".into());

        assert!(
            m.standing_on_what_we_came_for(Some("l4")),
            "we set out for it, we are on it, and the fight is still owed"
        );

        // **A settlement counts too**, which the first version got wrong. The dev, watching a run
        // re-centre before Boreas: entering a town is `Enter` on the same slot, one press, and the
        // combat-only rule was drawn on a distinction that does not exist.
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("l59", "Boreas town")]));
        m.here = Some("l59".into());
        assert!(m.standing_on_what_we_came_for(Some("l59")), "a town is one press as well");

        // **Only the node we chose.** Passing through a fight on the way elsewhere is a different
        // decision, taken later and on other grounds.
        assert!(!m.standing_on_what_we_came_for(Some("l7")), "committed elsewhere");
        assert!(!m.standing_on_what_we_came_for(None), "committed to nothing");

        // A fight already won is not one to enter, and its node is not a settlement either.
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("l4", "Riccall — level 6 crypt")]));
        m.here = Some("l4".into());
        m.entry("l4").completed = true;
        assert!(!m.standing_on_what_we_came_for(Some("l4")), "nothing left to fight");

        // An ordinary road is neither, so the camera work stands.
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("l2", "Bainton Clump road")]));
        m.here = Some("l2".into());
        assert!(!m.standing_on_what_we_came_for(Some("l2")), "nothing is entered at a road");

        // And nowhere at all, which is the state before the first dump lands.
        assert!(!WorldMap::new().standing_on_what_we_came_for(Some("l4")), "not anywhere yet");
    }

    /// The inn can end the rest errand before a health reading could.
    ///
    /// Live 2026-08-10: the run healed to full, walked out, and walked straight back in — because
    /// `wants_rest` clears only on *seeing* full health, and `overworld:save()` runs in the inn's
    /// `goBack`, so the evidence lands after the decision to return has been taken. It opened the
    /// rest screen again to be told `healthNeed = 0`, eight inn screens in one run.
    ///
    /// The control is the middle case, and it is the one that must not regress: a **partial** heal
    /// leaves the intent standing, or a run interrupted mid-top-up would walk away at 14/20.
    #[test]
    fn the_inn_can_end_a_rest_errand_that_a_health_reading_has_not_caught_up_with() {
        let mut m = WorldMap::new();
        m.note_health_level(crate::rest::Health { current: 1, max: 20 });
        assert!(m.wants_rest(), "1/20 asks for a rest");

        // Healed, but the save still shows the old figure — exactly the window that bit us.
        m.note_health_level(crate::rest::Health { current: 1, max: 20 });
        assert!(m.wants_rest(), "a stale reading must not clear it by itself");

        m.rest_errand_over();
        assert!(!m.wants_rest(), "the inn said `healthNeed = 0`, which is the game's own answer");

        // And the middle case is untouched: half-healed is still a rest in progress.
        let mut m2 = WorldMap::new();
        m2.note_health_level(crate::rest::Health { current: 2, max: 20 });
        m2.note_health_level(crate::rest::Health { current: 14, max: 20 });
        assert!(m2.wants_rest(), "a partial top-up must not cancel the errand");
    }

    /// The save is the only thing that knows a consecration happened.
    ///
    /// Live 2026-08-10 at `shrine3`, `consecrate` reported `shrine screen closed=true` and the run
    /// believed it. The screen had closed because the **stats history page** opened over it, and the
    /// shrine finished the run unconsecrated with the log saying otherwise. Two shrines that turn did
    /// work, so nothing in aggregate gave the false positive away.
    ///
    /// The two polarities come out of **one** save here on purpose: a fixture that only ever asserts
    /// the true case cannot tell "reads the flag" from "returns true".
    #[test]
    fn only_the_save_flag_says_a_shrine_was_consecrated() {
        let mut m = WorldMap::new();
        m.apply_save(
            &crate::game::save::parse(
                "return { overworld = { areaFlags = {
                     hell = 0.1, shrine2_consecrated = 1, shrine3_used = 1,
                 } } }",
            )
            .unwrap(),
        );
        assert!(m.is_consecrated("shrine2"), "the flag is set, so the game says it is done");
        assert!(
            !m.is_consecrated("shrine3"),
            "used but not consecrated — this is the shrine the run wrongly believed it had finished"
        );
        assert!(!m.is_consecrated("shrine9"), "a shrine we have never heard of is not consecrated");
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
        //
        // Both hops cost [`CROSSING`], because nothing in this fixture has been cleared and no road
        // in it has been walked — which is the ordinary state of unexplored map, and the reason
        // weighting the edges left every existing route ordering alone. What the test is about is
        // that the far side is *reachable at all* and ranks behind the near side.
        let d = m.distances("l7");
        assert_eq!(d.get("l10"), Some(&CROSSING));
        assert_eq!(d.get("l19"), Some(&(2 * CROSSING)));
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
        assert_eq!(m.anomaly_is_open(), Some(false));
    }

    /// An old cache keeps its shape and loses its coordinates.
    ///
    /// The dev asked whether we had lost the world-0 cache. We had not — but `world-5.txt` was being
    /// refused whole for being v1, and it holds 407 places and 1188 edges from an adventure that
    /// walked much further than any since. Only the coordinates were ever scaled by the zoom; the
    /// edges are statements about the world's shape and survive it.
    ///
    /// **v2 joined v1 on 2026-08-17**, for the same fault one level down: a v2 writer measured the
    /// scale when it could and assumed `1.0` when it could not, so a file written across a zoom holds
    /// two scales exactly as a v1 file does. See [`CACHE_VERSION`].
    #[test]
    fn an_old_cache_keeps_its_edges_and_drops_its_positions() {
        let v1 = "# diggle map cache v1\np\tl9\tSaltagh Park forest\t100\t200\t3\t0\t\ne\tl9\tl19\n";
        for old in ["v1", "v2"] {
            let text = v1.replacen("v1", old, 1);
            let mut m = WorldMap::new();
            assert!(m.absorb_cache(&text) > 0, "the edges are the point of reading {old} at all");
            assert!(m.get("l9").unwrap().neighbours.contains("l19"), "shape survives a zoom");
            assert_eq!(m.get("l9").unwrap().heading, "Saltagh Park forest", "so does a name");
            assert_eq!(m.get("l9").unwrap().pos, None, "{old} coordinates must not be kept");
        }

        // The current format keeps everything, which is the whole difference between them.
        let mut m = WorldMap::new();
        m.absorb_cache(&v1.replacen("# diggle map cache v1", CACHE_VERSION, 1));
        assert_eq!(m.get("l9").unwrap().pos, Some((100.0, 200.0)));
    }

    /// A cache that arrives late is read for its shape and not for where things were.
    ///
    /// The run of 2026-08-22 0203Z is why this exists. A fresh profile has no `mainSaveData` when
    /// the run starts, so the seed could not be read, so `no map remembered for this world` — and
    /// the run then went round in circles inside a village it had 699 places for. The cache is now
    /// asked for again on every save, which means it can turn up after this run has already placed
    /// things, and old coordinates dropped into a frame we have anchored ourselves are the
    /// disagreement `registration` exists to refuse.
    #[test]
    fn a_cache_that_arrives_after_we_have_placed_something_keeps_only_the_shape() {
        let text = format!(
            "{CACHE_VERSION}\np\tl9\tSaltagh Park forest\t100\t200\t3\t0\t\ne\tl9\tl19\n"
        );

        // Nothing placed yet: this is the startup case, and it takes the positions.
        let mut early = WorldMap::new();
        assert!(!early.any_placed(), "a new map has no frame to protect");
        assert!(early.absorb_cache(&text) > 0);
        assert_eq!(early.get("l9").unwrap().pos, Some((100.0, 200.0)));

        // A frame of our own, from this run's own dump. Now the same file is structure only.
        let mut late = WorldMap::new();
        late.fold(&fixtures::dump("l1", "camp", vec![fixtures::node_at("l2", "camp", 500.0, 500.0)]));
        assert!(late.any_placed(), "the fixture must actually place something, or this proves nothing");
        assert!(late.absorb_cache_structure(&text) > 0, "the edges are the point of reading it");
        assert!(late.get("l9").unwrap().neighbours.contains("l19"), "the shape is always safe");
        assert_eq!(late.get("l9").unwrap().heading, "Saltagh Park forest", "and so is a name");
        assert_eq!(late.get("l9").unwrap().pos, None, "old coordinates must not enter our frame");
        // And what we placed ourselves is untouched.
        assert_eq!(late.get("l2").unwrap().pos, Some((500.0, 500.0)));
    }

    /// The save has been carrying the road network all along, and we were reading none of it.
    ///
    /// The dev, 2026-08-16: *why is the planner only choosing one node over when we've explored so
    /// much of the map already? When we wanted to rest, the navigator could have selected any of the
    /// uncorrupted villages we already visited, because a path existed.*
    ///
    /// A path did exist and we could not see it. `completedAreas` hands us places **as bare keys**,
    /// so the map filled with nodes that had no edges, `ok()` refused every rest site for want of a
    /// route, and the run stepped to an adjacent frontier instead — every `RouteTo(Rest)` line in
    /// `spike-run-20260816-0826Z.md`.
    ///
    /// Measured against a real save rather than a fixture, because the claim is about what the game
    /// writes down, not about what we can construct.
    #[test]
    fn the_roads_named_in_a_save_are_edges() {
        let path = std::path::Path::new("checkpoints/ping-pong-l10-l18/mainSaveData");
        let Ok(text) = std::fs::read_to_string(path) else {
            eprintln!("SKIP: {} is not present", path.display());
            return;
        };
        let save = crate::game::save::parse(&text).expect("the checkpoint parses");
        let mut m = WorldMap::new();
        m.apply_save(&save);

        // `l25_path_to_l32` appears in the save's flags, and says these two are adjacent.
        assert!(
            m.get("l25").map(|p| p.neighbours.contains("l32")).unwrap_or(false),
            "the road `l25_path_to_l32` names an edge and it must be one"
        );
        assert!(
            m.get("l32").map(|p| p.neighbours.contains("l25")).unwrap_or(false),
            "and edges are recorded from both ends, as `fold` does"
        );

        // The point of it: a route the planner can measure, from the save alone, before this run has
        // walked anywhere at all.
        m.here = Some("l25".into());
        assert!(m.can_route_to("l32"), "Enthorpe is routable from Aike without a single dump");
        assert!(m.can_route_to("shrine2"), "and so is what lies beyond it");

        // A flag suffix must not be mistaken for part of the destination key.
        assert!(
            !m.places.keys().any(|k| k.ends_with("_explored")),
            "`l25_path_to_l32_explored` must yield `l32`, not `l32_explored`"
        );
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

    /// The save is where every other fact arrives from, and health must not be the exception.
    ///
    /// Live 2026-08-15 at The Quacking Duck: three presses took 2/20 to 20/20, the driver re-read the
    /// save, printed `health is now 20/20` — and threw the reading away, because noting it was a
    /// thing four *other* call sites in the driver remembered to do. `wants_rest` stayed set, the
    /// arrival dispatch saw an inn under its feet, and the run walked straight back in to be told
    /// `healthNeed = 0`.
    ///
    /// `gold` and `fuel` were already read here. This is the field that was being passed around by
    /// hand next to them.
    #[test]
    fn a_full_bar_in_the_save_ends_the_rest_errand() {
        let mut m = WorldMap::default();
        m.note_health_level(crate::rest::Health { current: 2, max: 20 });
        assert!(m.wants_rest(), "2/20 asks for a bed");
        let save = crate::game::save::parse(
            r#"return { player = { health = 20, maxHealth = 20, gold = 1010 } }"#,
        )
        .unwrap();
        m.apply_save(&save);
        assert!(!m.wants_rest(), "the save says the bar is full, and that is the whole errand");
    }

    /// And the other direction, which is the one that must not need a watched drop to fire.
    #[test]
    fn a_low_bar_in_the_save_asks_for_a_rest_with_no_before_reading() {
        let mut m = WorldMap::default();
        let save = crate::game::save::parse(
            r#"return { player = { health = 4, maxHealth = 12, gold = 107 } }"#,
        )
        .unwrap();
        m.apply_save(&save);
        assert!(m.wants_rest(), "a resumed run at a third health has no delta to observe");
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

    /// A prong of a fork that merges retires once the far end names it — not before.
    ///
    /// The dev's graph: `A-B`, `A-C`, `B-C`, `B-D`, `C-D`. Walk `A -> B -> D` and `C` is pure
    /// detour, because everything it leads to is already reachable. The saving is real and it is
    /// **late**: standing at `B` we know `C` has degree 3 and can see only two of its edges, and the
    /// third could as easily go somewhere new as to `D`. Guessing there would trade a wasted step for
    /// a missed branch, which is the worse error.
    #[test]
    fn the_second_prong_of_a_fork_retires_when_the_far_end_closes_it() {
        let deg = |key: &str, connections: u32| Node {
            key: key.into(),
            heading: format!("{key} crossroads"),
            x: 0.0,
            y: 0.0,
            connections,
        };
        let mut m = WorldMap::new();
        m.fold(&dump("a", "A crossroads", vec![deg("b", 3), deg("c", 3)]));
        assert!(!m.get("c").unwrap().nothing_left_to_reveal(), "one edge of three seen");

        m.fold(&dump("b", "B crossroads", vec![deg("a", 2), deg("c", 3), deg("d", 2)]));
        // Two of `c`'s three edges now, and the third is still worth walking for: it could lead
        // anywhere. This is the step where guessing would be tempting and wrong.
        assert_eq!(m.get("c").unwrap().neighbours.len(), 2);
        assert!(!m.get("c").unwrap().nothing_left_to_reveal(), "the third edge is still unknown");

        m.fold(&dump("d", "D crossroads", vec![deg("b", 3), deg("c", 3)]));
        assert!(
            m.get("c").unwrap().nothing_left_to_reveal(),
            "all three edges known, so standing on `c` cannot show us anything"
        );
        // And it is not a claim about having been there — nothing walked to `c`.
        assert!(!m.get("c").unwrap().visited);
    }

    /// Silence is not a dead end. A place known only by key has no connection count at all.
    ///
    /// `completedAreas` gives us keys and nothing else, which on a resume is most of the map. If
    /// `0` read as a leaf the frontier would be empty almost everywhere.
    #[test]
    fn a_place_we_know_only_by_key_is_not_a_dead_end() {
        let mut m = WorldMap::new();
        assert_eq!(m.entry("l44").connections, 0, "never seen in a dump");
        assert!(!m.get("l44").unwrap().nothing_left_to_reveal());
    }

    /// Paved still means something in here, and it means something better.
    ///
    /// The rename at `forest.lua:653-659` strips `road` from every interior node that has no
    /// `targetNode`, so what is left calling itself paved is the junctions of the spine and the ways
    /// out. `e1sub3` is the only one of the three, and it is the one to walk to.
    #[test]
    fn only_the_crossroads_survives_the_lost_woods_rename() {
        let m = a_lost_woods();
        assert!(m.get("e1sub3").unwrap().is_paved(), "crossroads is not renamed");
        assert!(!m.get("e1sub2").unwrap().is_paved(), "an interior road now reads as forest");
    }

    /// `corrupt_lost_woods` prints the same two words and behaves like neither.
    ///
    /// Its `typeName` is `'lost woods'` unconditionally (`lost_woods.lua:41`) while
    /// `lostOrientation` and `thickFog` are both false (`:44-47`) — so the roads keep their names and
    /// none of the exemptions apply. Corruption is what tells the two apart.
    #[test]
    fn a_corrupted_lost_woods_is_not_the_disguised_kind() {
        let mut m = WorldMap::new();
        m.entry("e2").corrupted = true;
        m.fold(&Adjacency {
            subworld: Some(("e2".into(), "Burnt Timberland — level 6 lost woods".into())),
            ..dump("e2sub1", "Burnt Timberland forest", vec![])
        });
        assert!(!m.get("e2").unwrap().in_lost_woods, "corrupt, so nothing is disguised");
        assert!(!m.get("e2sub1").unwrap().in_lost_woods);
    }

    /// A node seen in two dumps of different places lands in one frame.
    ///
    /// The whole trick: the printed numbers are `xoffset + world*zoomMult` (`overworldview.lua:1033`)
    /// so they move with the pan, but one node in common fixes the shift for everything else in the
    /// dump. These readings are the shape of the real thing — `l38`'s neighbours were printed three
    /// times in one run at (586.95, 813.93), (693.12, 664.22) and (713.70, 635.20).
    #[test]
    fn two_dumps_of_different_places_agree_on_one_map() {
        let mut m = WorldMap::new();
        // First dump defines the frame: `a` at (100, 100), `b` at (200, 100).
        m.fold(&dump("here", "Somewhere", vec![
            Node { key: "a".into(), heading: "A".into(), x: 100.0, y: 100.0, connections: 2 },
            Node { key: "b".into(), heading: "B".into(), x: 200.0, y: 100.0, connections: 2 },
        ]));
        // Walk to `a` and pan: every number shifts by (+50, -30). `b` is the node in common.
        m.fold(&dump("a", "A", vec![
            Node { key: "b".into(), heading: "B".into(), x: 250.0, y: 70.0, connections: 2 },
            Node { key: "c".into(), heading: "C".into(), x: 350.0, y: 70.0, connections: 2 },
        ]));
        // `c` is 100 beyond `b` in the second frame, so 300 in the first — the pan cancels.
        assert_eq!(m.get("c").unwrap().pos, Some((300.0, 100.0)), "registered through `b`");
        assert_eq!(m.gap("a", "c").map(|d| d as i64), Some(200), "and distances survive the pan");
    }

    /// A level 4+ node **off** every shortest path does not cancel the hop.
    ///
    /// The dev, 2026-08-17: *don't remove fast hops because of potential intermediate level 4+
    /// nodes. Use BFS to find all the shortest paths from source to destination based on what is
    /// known; if none of the shortest paths contain a lv4+ node, you're clear.*
    ///
    /// The case that separates the two rules is **our route not being the game's route**.
    /// `first_step_toward` prefers paved ground, which is the dev's own crossing rule, so the chain
    /// walks `a -> c -> e -> d` down the road; the game runs a breadth-first search
    /// (`overworldview.lua:1330-1373`) and takes `a -> b -> d`, which is shorter and never goes near
    /// the crypt. Breaking the chain at `c` threw away a hop the game would have walked safely.
    #[test]
    fn a_crypt_the_game_would_never_route_through_does_not_cancel_the_hop() {
        let mut m = WorldMap::default();
        // Two ways from `a` to `d`: the short brush path, and a longer paved one past a level 6
        // crypt. Every node cleared, so travel legality is not what is being tested here.
        for (x, y) in [("a", "b"), ("b", "d"), ("a", "c"), ("c", "e"), ("e", "d")] {
            m.entry(x).neighbours.insert(y.into());
            m.entry(y).neighbours.insert(x.into());
        }
        // Paved *and* dangerous, which is the combination that makes the chain walk into it:
        // `type_is` reads the heading's suffix and `level()` its `— level N` part, and the 1043Z run
        // shows the form is real (`Bainton Clump — level 1 road`).
        m.entry("c").heading = "Bessingby — level 6 road".into();
        m.entry("e").heading = "Bessingby road".into();
        m.entry("b").heading = "Bessingby thicket".into();
        m.here = Some("a".into());
        m.apply_save(
            &crate::game::save::parse(
                "return { overworld = { areaFlags = { hell = 0.1 }, completedAreas = {
                     a = true, b = true, c = true, d = true, e = true } } }",
            )
            .unwrap(),
        );

        // **The positive control.** Without this the test would pass on a map where nothing ever
        // went near the crypt, and prove nothing: the chain has to actually walk into `c` for the
        // old break-at-the-first-dangerous-node rule to have refused this hop.
        assert_eq!(
            m.first_step_toward("a", "d", true).as_deref(),
            Some("c"),
            "our own route takes the road past the crypt, which is the whole point"
        );
        // And the game's does not: `a -> b -> d` is the only shortest path, so the hop is clear.
        assert_eq!(m.far_hop("a", "d").as_deref(), Some("d"));

        // And the rule still bites when the crypt *is* unavoidable: drop the brush path and every
        // shortest route runs through `c`.
        let mut only_the_road = m.clone();
        only_the_road.entry("a").neighbours.remove("b");
        only_the_road.entry("b").neighbours.remove("a");
        assert_eq!(only_the_road.far_hop("a", "d"), None);
    }

    /// #26: what is left to loot in a village the demons have taken.
    ///
    /// The dev's rule, and the two halves it needs — the settlement is destroyed, and the building is
    /// a kind that carries a `Loot` button with presses left on it. Nothing here presses anything;
    /// see the entry for why the press is held back.
    #[test]
    fn a_destroyed_village_says_what_is_left_to_loot() {
        let village = |flags: &str| {
            let mut m = WorldMap::new();
            m.fold(&inside_dump(
                "l10",
                "l10sub1",
                "Ulrome general store",
                vec![
                    node("l10sub2", "Ulrome inn"),
                    node("l10sub3", "Ulrome house"),
                    node("l10sub4", "Ulrome well"),
                    node("l10sub5", "Ulrome crossroads"),
                ],
                vec![exit("l19")],
            ));
            m.entry("l10").heading = "Ulrome village".into();
            m.apply_save(
                &crate::game::save::parse(&format!(
                    "return {{ overworld = {{ areaFlags = {{ hell = 0, {flags} }} }} }}"
                ))
                .unwrap(),
            );
            m
        };

        // An intact village has nothing to loot, however lootable the buildings are.
        let intact = village("");
        for k in ["l10sub1", "l10sub2", "l10sub3"] {
            assert_eq!(intact.loot_here(k), 0, "{k} is a shop, not a ruin");
        }

        // Lost — and the flag lands on the **village**, which is the clause that matters. Asking only
        // `<key>_attacked` would find nothing at any of these.
        let lost = village("l10_attacked = 4");
        assert_eq!(lost.loot_here("l10sub1"), 2, "a general store is worth two presses");
        assert_eq!(lost.loot_here("l10sub2"), 2, "and so is an inn");
        assert_eq!(lost.loot_here("l10sub3"), 1, "a house allows one (`village.lua:491`)");
        assert_eq!(lost.loot_here("l10sub4"), 0, "a well's destroyed button is water, not loot");
        assert_eq!(lost.loot_here("l10sub5"), 0, "and a crossroads has no button at all");

        // Sacking it ourselves counts, and it is a different flag: `_attack.playerAttack`, not the
        // mere presence of `_attack`, which is a fight still in progress.
        let ours = village("l10_attack = { playerAttack = true }");
        assert!(ours.get("l10").unwrap().player_sacked);
        assert_eq!(ours.loot_here("l10sub1"), 2, "we did this, and it is still lootable");
        let besieged = village("l10_attack = { attackingEnemies = 3 }");
        assert_eq!(besieged.loot_here("l10sub1"), 0, "a fight in progress is not a ruin yet");

        // The counter is a budget: it counts down and stops the press when it is spent.
        let picked = village("l10_attacked = 4, l10sub1_looted = 1, l10sub3_looted = 1");
        assert_eq!(picked.loot_here("l10sub1"), 1, "one press taken of two");
        assert_eq!(picked.loot_here("l10sub3"), 0, "the house is done");
    }

    /// A frame that came off disk has an unknown scale until a dump measures it.
    ///
    /// The same fault as the zoom, arriving by post. `ScreenScale` starts at `Some(1.0)` because a
    /// fresh map's first dump *defines* the frame — which stops being true the moment a cache
    /// supplies one, since the file was written at whatever zoom that run ended on.
    #[test]
    fn a_cached_frame_is_inherited_rather_than_defined() {
        let mut m = WorldMap::new();
        m.absorb_cache(&format!(
            "{CACHE_VERSION}\np\ta\tA\t100\t100\t2\t0\t\np\tb\tB\t200\t100\t2\t0\t\n"
        ));
        assert_eq!(m.get("a").unwrap().pos, Some((100.0, 100.0)), "the premise: positions restored");

        // One anchor and a cache-supplied frame: this dump cannot say what the units are worth, and
        // half of it being right is not good enough for a position that never changes again.
        m.fold(&dump("n1", "Somewhere", vec![node_at("a", "A", 50.0, 50.0), node_at("d", "D", 150.0, 50.0)]));
        assert_eq!(m.get("d").unwrap().pos, None, "assuming 1.0 here is assuming last run's zoom");

        // And the moment a dump carries two, the scale is measured and everything proceeds.
        m.fold(&dump("n2", "Somewhere", vec![
            node_at("a", "A", 50.0, 50.0),
            node_at("b", "B", 100.0, 50.0),
            node_at("d", "D", 150.0, 50.0),
        ]));
        assert_eq!(m.get("d").unwrap().pos, Some((300.0, 100.0)));
    }

    /// Positions come from the surface only, so a subworld's own frame cannot contaminate it.
    #[test]
    fn a_subworld_dump_places_nothing() {
        let mut m = WorldMap::new();
        m.fold(&inside_dump("l9", "l9sub1", "Saltagh Park road",
            vec![Node { key: "l9sub2".into(), heading: "road".into(), x: 5.0, y: 5.0, connections: 2 }],
            vec![exit("l19")]));
        assert_eq!(m.get("l9sub2").unwrap().pos, None, "zoomMult is unchecked inside a subworld");
    }

    /// The anomaly can be aimed at before it has ever been seen, because corruption surrounds it.
    ///
    /// `world.lua:73` puts `locationData.start` at world `(0,0)` and `:505-507` makes that node the
    /// portal, so the anomaly is at the origin by construction. `hellCheck`
    /// (`hellportal.lua:16-23`) measures from the same `(0,0)`, so the corrupted nodes are a blob
    /// around it — which means the corrupted places we have positioned point at an anomaly we have
    /// never stood next to. That is the whole case the coordinate work was for: the run that walked
    /// into a level 5 crypt had an open anomaly whose key it knew and whose position it did not.
    #[test]
    fn corruption_points_at_an_anomaly_we_have_never_seen() {
        let mut m = WorldMap::new();
        m.fold(&dump("here", "The Wold crossroads", vec![
            Node { key: "west".into(), heading: "West Field".into(),
                   x: -100.0, y: 0.0, connections: 3 },
            Node { key: "east".into(), heading: "East Field".into(),
                   x: 100.0, y: 0.0, connections: 3 },
            // Two corrupted nodes, both well to the west. `start` itself is never dumped.
            Node { key: "c1".into(), heading: "Burnt Hollow — level 6 crypt".into(),
                   x: -480.0, y: -40.0, connections: 2 },
            Node { key: "c2".into(), heading: "Ashen Reach — level 6 forest".into(),
                   x: -520.0, y: 40.0, connections: 2 },
        ]));
        for k in ["c1", "c2"] {
            m.entry(k).corrupted = true;
        }
        // `start` arrives as a bare key, the way `completedAreas` supplies it on a live run: known
        // to exist, with no heading, no edges and no position.
        m.entry("start");
        assert_eq!(m.get("start").unwrap().pos, None, "never dumped, so no real position");

        // The centroid of the corruption is (-500, 0), so `west` is nearer the anomaly than `east`.
        let west = m.gap("west", "start").expect("estimated through the corrupted blob");
        let east = m.gap("east", "start").expect("estimated through the corrupted blob");
        assert!(west < east, "west {west} should beat east {east}");

        // And a real sighting overrides the estimate rather than being averaged with it.
        m.entry("start").pos = Some((1000.0, 0.0));
        assert!(m.gap("east", "start").unwrap() < m.gap("west", "start").unwrap(), "sighting wins");
    }

    /// `shrine5`, and the trip that could not have worked.
    ///
    /// The save records a shrine's submissions under `<key>subs` (`shrineview.lua:267`), with no
    /// underscore. `shrine5` had none — nobody had ever played it — so `hasWon()` was false,
    /// `ShowAGoodButton()` was false, and `Consecrate` was drawn greyed. The run walked there anyway
    /// and pressed nothing, because the branch it took has no typist.
    ///
    /// The shrine is still a destination. That is the distinction the fix turns on: worth walking
    /// to, and not yet worth pressing at.
    #[test]
    fn a_shrine_nobody_has_played_is_worth_the_walk_but_not_the_button() {
        let mut m = WorldMap::new();
        m.apply_save(
            &crate::game::save::parse(
                "return { overworld = {
                     completedAreas = { shrine5 = true, shrine2 = true },
                     areaFlags = { hell = 0.1, shrine2_shrine_subs = 3, shrine2_used = 1 },
                 } }",
            )
            .unwrap(),
        );

        assert!(!m.get("shrine5").expect("known from the save").played, "no `subs`, never played");
        assert!(
            m.get("shrine2").expect("known from the save").played,
            "`shrine2_shrine_subs` says otherwise — and the dataKey is not the location key"
        );
        // Both are worth a trip while the anomaly is open and neither is consecrated — the planner's
        // question, which this must not change.
        assert!(m.worth_consecrating_here("shrine5"));
        assert!(m.worth_consecrating_here("shrine2"));
        // And no `subs` key invented a place of its own.
        for junk in ["shrine2subs", "shrine2_shrine_", "shrine2_shrine_subs"] {
            assert!(!m.places.contains_key(junk), "the suffix strip must not mint `{junk}`");
        }
    }

    /// **Corruption takes completion away, and the save is how we hear about it.**
    ///
    /// The dev, watching `spike-run-20260816-0310Z.md` stall: *the navigator is confused because the
    /// corruption of the node cut off its neighbours.* It ended `Failed("no arrival at l7")` after
    /// clicking an inactive `Travel` **203 times**, with `l10 … [done] [corrupted]` in its own map —
    /// the contradiction written out in full.
    ///
    /// `core.setAreaIncomplete` (`overworldview.lua:179-206`) does it two ways, and this covers both:
    /// the location is **removed** from `completedAreas`, and each of its roads is set to **false**
    /// rather than removed. A fold that only ever wrote `true`, over keys alone, could not see
    /// either.
    #[test]
    fn a_corrupted_node_stops_being_complete_and_its_roads_stop_being_walkable() {
        let complete = |body: &str| {
            crate::game::save::parse(&format!("return {{ overworld = {{ completedAreas = {{ {body} }} }} }}"))
                .unwrap()
        };
        let mut m = WorldMap::new();
        m.fold(&dump("l10", "Ulrome — level 6 village", vec![node("l7", "Greenoak Backwoods campfire")]));

        m.apply_save(&complete("l10 = true, l10_path_to_l7 = true"));
        assert!(m.places.get("l10").unwrap().completed);
        assert!(m.can_step("l10", "l7"), "the control: with l10 complete the hop is legal");

        // The hell radius reaches Ulrome. The game drops the location and falsifies the road.
        m.apply_save(&complete("l10_path_to_l7 = false"));
        assert!(!m.places.get("l10").unwrap().completed, "removed from the table is not complete");
        assert!(
            !m.can_step("l10", "l7"),
            "and the road it wrote `false` over is not a road — this is the 203 clicks"
        );

        // Clearing the village again puts both back, so this is revocable rather than one-way.
        m.apply_save(&complete("l10 = true, l10_path_to_l7 = true"));
        assert!(m.can_step("l10", "l7"), "fighting it out restores the hop");
    }

    /// Whether a heart is still for sale, read out of the save rather than assumed.
    ///
    /// Three cases, and absence is the informative one: a store nobody has opened cannot have had
    /// its heart bought, because buying requires opening it.
    #[test]
    fn the_save_says_which_hearts_are_still_on_the_shelf() {
        let with = |flags: &str| {
            let mut m = WorldMap::new();
            m.fold(&dump(
                "here",
                "camp",
                vec![
                    node("v1", "Rowlston Covert village"),
                    node("v2", "Enholmes town"),
                    node("v3", "Little Nowhere hamlet"),
                ],
            ));
            m.apply_save(
                &crate::game::save::parse(&format!(
                    "return {{ overworld = {{ areaFlags = {{ hell = 0.1, {flags} }} }} }}"
                ))
                .unwrap(),
            );
            m
        };

        // The heading is the stock list: a village has a heart, a town may, a hamlet never does.
        let m = with("");
        assert!(m.get("v1").unwrap().stocks_a_heart(), "village");
        assert!(m.get("v2").unwrap().stocks_a_heart(), "town");
        assert!(!m.get("v3").unwrap().stocks_a_heart(), "hamlet");

        // Never opened: nothing is recorded, so the heart is still there.
        assert!(!m.heart_is_spent("v1"));

        // An inn was visited and the general store was not — which is the shape of the live save's
        // `l19_shops`. Still nothing said about the heart.
        let m = with("v1_shops = { innStock = { inventoryHash = { } } }");
        assert!(!m.heart_is_spent("v1"), "an inn visit says nothing about the shelf");

        // Opened, and the heart still on it.
        let m = with(
            "v1_shops = { generalStoreStock = { inventoryHash = { healthBuff = { stock = 1 } } } }",
        );
        assert!(!m.heart_is_spent("v1"));

        // Opened, and bought.
        let m = with(
            "v1_shops = { generalStoreStock = { inventoryHash = { healthBuff = { stock = 0 } } } }",
        );
        assert!(m.heart_is_spent("v1"), "sold");
    }

    /// The route the dev asked for twice, and the reason it was not taken.
    ///
    /// Standing on `l62`, a level 7 spider forest, with a shrine somewhere past `l42`. Two ways on:
    ///
    /// ```text
    ///   l62 -> l36 -> l42     two hops, and the first is a five-node crossing of the forest
    ///   l62 -> l57 -> l42     two hops, and the first is free: `l62_path_to_l57` is walked
    /// ```
    ///
    /// Counted in hops those are the same move, which is why the planner kept picking the crossing —
    /// and the crossing is what ended two runs at an exit they could not pan to. Counted in what
    /// they cost, one is worth six of the other.
    #[test]
    fn a_route_round_a_forest_beats_a_route_through_it() {
        let mut m = WorldMap::default();
        for (a, b) in
            [("l62", "l36"), ("l62", "l57"), ("l36", "l42"), ("l57", "l42"), ("l42", "shrine9")]
        {
            m.entry(a).neighbours.insert(b.into());
            m.entry(b).neighbours.insert(a.into());
        }
        // The road out to `l57` has been walked; nothing else here has been cleared.
        m.apply_save(
            &crate::game::save::parse(
                "return { overworld = { completedAreas = { l62_path_to_l57 = true } } }",
            )
            .unwrap(),
        );

        let d = m.distances("l62");
        assert_eq!(d.get("l57"), Some(&1), "a walked road is one cheap step");
        assert_eq!(d.get("l36"), Some(&CROSSING), "and the forest crossing is not");
        // The shrine is three edges away either way. Through the forest that is three unwalked
        // edges; round it, the first one is free, and the whole route is a crossing cheaper.
        assert_eq!(
            d.get("shrine9"),
            Some(&(1 + 2 * CROSSING)),
            "the route is costed round the forest, not through it ({} would be through)",
            3 * CROSSING
        );
    }

    /// Rebuilt from the save as it stood at the stop of the run of 2026-08-15.
    ///
    /// Standing on `l62`, a level 7 spider forest, with `l62_path_to_l57` and `l62_path_to_shrine7`
    /// complete and the forest itself uncleared. The game would have let us walk straight out to
    /// either of those two neighbours (`canTravelToDirect`, `overworldview.lua:1316-1321`) and
    /// refused the hop to `l36`, whose road is unwalked and whose crypt is unfought.
    ///
    /// Before the exit clause existed, all three read the same: illegal. The two that matter cost a
    /// level 7 crossing each time we believed it.
    #[test]
    fn a_walked_road_lets_us_leave_a_forest_we_never_cleared() {
        let mut m = WorldMap::default();
        m.apply_save(
            &crate::game::save::parse(
                "return { overworld = { completedAreas = {
                     l62_path_to_l57 = true, l62_path_to_shrine7 = true, shrine7 = true,
                 } } }",
            )
            .unwrap(),
        );

        assert!(m.can_step("l62", "l57"), "the road out to `l57` is walked, so leaving is legal");
        assert!(m.can_step("l57", "l62"), "and the game asks the pair both ways round");
        assert!(
            m.can_step("l62", "shrine7"),
            "`shrine7` is complete as well, which was already enough on its own"
        );
        assert!(
            !m.can_step("l62", "l36"),
            "but an unwalked road to an unfought crypt is still a fight we have to take"
        );
        assert!(
            !m.places.contains_key("l62_path_to_l57"),
            "and no road became a place — routing must not acquire a new destination from this"
        );
    }

    /// The dev's crossing rule: **the road is the road.** A fight on it is crossed, not detoured
    /// around.
    ///
    /// Case 2 asserted the opposite until 2026-08-08 — "a combat-less pathway beats a blocked road"
    /// — and it is the case the dev withdrew. Every detour round a nest is also a detour away from
    /// the only structure in the map that leads anywhere (`forest.lua:117-130` strings the road from
    /// entrance to exit), and a live run spent twenty-four steps proving it: road, brush, back to
    /// the road, never crossing.
    #[test]
    fn a_forest_is_crossed_by_the_road_fight_and_all() {
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

        // 2. A fight on the road and a clear way round: **still the road.** This is the reversal.
        //    The nest gets fought; the brush leads nowhere in particular.
        assert_eq!(
            build(true, false).first_step_toward("here", "exit", false).as_deref(),
            Some("road1"),
            "a fight on the path is crossed, not routed around"
        );

        // 3. A fight on the road and a fight in the woods too: the road, obviously.
        assert_eq!(
            build(true, true).first_step_toward("here", "exit", false).as_deref(),
            Some("road1"),
            "if both cost a fight, the road is still the road"
        );

        // 4. And a clear road against a wooded fight, which never depended on the withdrawn pass.
        assert_eq!(
            build(false, true).first_step_toward("here", "exit", false).as_deref(),
            Some("road1")
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

    #[test]
    fn a_cache_never_overwrites_what_this_run_has_seen() {
        // A stale cache must lose to a live dump, or a moved node or a changed heading would be
        // believed for the whole run. Only gaps are filled.
        let mut fresh = WorldMap::new();
        fresh.fold(&dump("here", "camp", vec![node("mid", "Quiet Glade meadow")]));
        let seen = fresh.entry("mid").pos;
        assert!(seen.is_some(), "the premise: this run placed it");

        let stale = format!("{CACHE_VERSION}\np\tmid\tSomewhere Else crypt\t999\t999\t7\t3\t\n");
        fresh.absorb_cache(&stale);
        assert_eq!(fresh.entry("mid").pos, seen, "a cache moved a node this run had placed");
        assert_eq!(fresh.entry("mid").heading, "Quiet Glade meadow", "a cache renamed a live node");

        // But a place the run has never heard of is taken whole.
        fresh.absorb_cache(&format!("{CACHE_VERSION}\np\tfar\tBorsea shrine\t12\t34\t2\t1\t\n"));
        assert_eq!(fresh.entry("far").heading, "Borsea shrine");
        assert_eq!(fresh.entry("far").pos, Some((12.0, 34.0)));
    }

    /// **#79, from the run that wrote the fault onto disk.** Replays a real adventure's dumps and
    /// asks what the cache it would write says about a crypt it cleared on the way through.
    ///
    /// `AreaHeading` stops printing `— level N` the moment `locationHasCombat` goes false
    /// (`overworldview.lua:305-310, 383-392`), so the run met `Bilton — level 2 crypt`, beat it,
    /// and every later sighting said `Bilton crypt`. `cache_text` wrote the heading it had. A
    /// second profile on the same seed — the world is a pure function of it
    /// (`overworld/generators/world.lua:65`) — absorbs that, finds `l13` absent from its own
    /// `completedAreas`, and reads a level 2 fight as a free walk.
    ///
    /// The three assertions are the three links: the game really does strip it, the run banks it
    /// anyway, and a reader on the other side of the file gets it back. The **negative control** is
    /// the same file with the new column cut off, which is byte-for-byte what the old writer
    /// produced — it has to come back free, or the test is passing on something else.
    #[test]
    fn a_crypt_cleared_last_run_is_still_a_fight_to_the_next_one() {
        let stem = "spike-run-20260822-0238Z";
        let Ok(log) = std::fs::read_to_string(format!("{stem}.log")) else {
            eprintln!("SKIP: {stem}.log is not present");
            return;
        };
        let lines: Vec<String> = log.lines().map(|l| l.to_string()).collect();
        let dumps = crate::observe::adjacency::Reader::new().push(&lines);
        assert!(dumps.len() > 100, "expected a whole run, got {}", dumps.len());
        let mut lived = WorldMap::new();
        for a in &dumps {
            lived.fold(a);
        }

        // The premise, measured rather than assumed: the heading we are left holding has no level.
        let l13 = lived.get("l13").expect("the run travelled to l13");
        assert_eq!(l13.heading, "Bilton crypt", "the run's last word on l13");
        assert_eq!(l13.level(), None, "so nothing can read a level off it");
        assert_eq!(l13.base_level, Some(2), "but the fight it was is banked");
        // And while the game is still saying `free`, that is the answer we give — the live heading
        // outranks the bank. See [`Place::remembered_level`].
        assert_eq!(l13.remembered_level(), None, "a live heading wins, including when it is silent");

        // Across the file, into a run that has never heard of this crypt.
        let text = lived.cache_text();
        let mut next = WorldMap::new();
        next.absorb_cache(&text);
        let recalled = next.get("l13").expect("the cache carries it");
        assert_eq!(recalled.heading, "Bilton crypt", "the stripped heading is what was written");
        assert_eq!(recalled.remembered_level(), Some(2), "and the level comes back beside it");
        assert!(recalled.may_be_a_fight(), "#79: this is what a route was being priced against");

        // The control. Cut the column the fix added and the old fault comes straight back.
        let old: String = text
            .lines()
            .map(|l| match l.starts_with("p\t") {
                true => l.rsplit_once('\t').map(|(head, _)| head).unwrap_or(l),
                false => l,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut without = WorldMap::new();
        without.absorb_cache(&old);
        let blind = without.get("l13").expect("the cache still carries it");
        assert_eq!(blind.remembered_level(), None, "the control is vacuous: it never lost the level");
        assert!(!blind.may_be_a_fight(), "the fault #79 reported, reproduced");
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

}
