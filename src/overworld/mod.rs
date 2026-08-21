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

/// Where to head, and why. The reason is carried so a route can be explained rather than just taken.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub target: String,
    pub reason: Goal,
    /// What direction exploring was steered by, when it was steered at all.
    ///
    /// `(bearing, placed, total)` — the node being aimed at, how many frontier candidates could be
    /// measured against it, and how many there were. Recorded because steering has three ways to do
    /// nothing and the log could not tell them apart: no bearing at all, a bearing nothing can place,
    /// and a bearing sitting on the node we are already standing on. All three come out as
    /// `Goal::Explore` and a nearest-unvisited hop, which is what a run that had never heard of
    /// steering would do — so "we fixed this" and "it went the wrong way" were both true and neither
    /// was checkable. Live 2026-08-12 a run wandered out of the corruption into a bandit camp and
    /// this is the field that would have said why.
    pub steered_by: Option<(String, usize, usize)>,
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

/// One move: the adjacent node to travel to, and the plan it serves.
#[derive(Debug, Clone, PartialEq)]
pub struct Hop {
    pub step: String,
    pub plan: Plan,
    /// True when this step is on a route to [`Plan::target`], false when it is a guess in its
    /// direction — [`WorldMap::next_hop`]'s fallback for a target nothing can reach.
    ///
    /// Carried purely so the log can tell them apart, and #24 is why. Five consecutive travel lines
    /// reading `(for start, Anomaly)` were read as five steps of a journey to the anomaly; they were
    /// five guesses at a node with no edges, and the run ended in a crypt nobody had chosen.
    /// Route-gating makes this the rare case rather than the usual one, which is exactly why it
    /// needs to be visible when it does happen.
    pub routed: bool,
}

/// What we can say about getting somewhere without taking a fight.
///
/// Three answers rather than two, because the missing one was doing damage. See
/// [`WorldMap::access_without_a_fight`] for the dev's correction that produced it: a `false` that
/// meant "a fight is in the way" and a `false` that meant "I have never looked over there" are not
/// the same claim, and the second is not ours to make.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// A route exists over recorded edges and takes no fight.
    Free,
    /// Every route we hold takes a fight, and the fight-free region around us is closed — every
    /// place in it has all the roads the game says it has.
    Blocked,
    /// No route over what we hold, but the fight-free region touches roads we have not looked down,
    /// so it may not end where our edges do. Go and see.
    Unknown,
}

/// Why a hop is being taken.
///
/// **A label, not a control switch.** Nothing branches on it except the log; it exists so that a
/// report can be read afterwards and say what the run believed it was doing.
///
/// `Copy` was dropped when [`Goal::RouteTo`] arrived. It is not free — every match site had to be
/// looked at — but the alternative was a parallel enum of "goals that can be sought", kept in step
/// with this one by hand, which is the shape that produced `Screen::ALL` and its keeper test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Goal {
    /// The anomaly is open and this is it. Everything else can wait.
    ///
    /// Named against [`Goal::OpenAnomaly`]: one *triggers* the portal, this one *finishes* it. The
    /// pair used to be `OpenTheAnomaly` and a bare `Anomaly`, which read as a step and its subject
    /// rather than as two ends of the same job.
    CloseAnomaly,
    /// A level > 3 surface node: arriving opens the anomaly.
    OpenAnomaly,
    /// Health is down and somewhere nearby can restore it.
    Rest,
    /// **Not hurt — under-banked.** An inn, to buy Well-Rested stacks before a deliberate deep fight.
    ///
    /// Distinct from [`Goal::Rest`] in what it is answering. `Rest` is a response to damage already
    /// taken and stops when the bar is full; this is preparation for damage not yet taken, it is
    /// bought at full health, and it stops when the bank is deep enough for the fight ahead
    /// ([`crate::rest::stacks_wanted`]).
    ///
    /// The game permits it: `getCanRest` for an inn is a flat `getPlayerGold() >= 10`
    /// (`ui/rest.lua:49`) with no health condition, and `doRest` grants the stack on its own line,
    /// separate from the heal (`:355-357`). So a run at full health can and should keep paying.
    StockUp,
    /// A village whose general store sells a `Heart` — four maximum health for a hundred gold.
    ///
    /// Ranked with the shrine detours rather than with [`Goal::Rest`]: it is not a response to being
    /// hurt, it is preparation bought while the road is free. See [`WorldMap::wants_a_heart`].
    Heart,
    /// An unopened treasure chest. Task #16.
    ///
    /// Ranked with the other detours rather than with exploring, and for the same reason as
    /// [`Goal::Heart`]: it is not a response to anything, it is loot taken while the road is free.
    /// **Opening it is a fight** — `Open` calls `scenarios.chest`
    /// (`overworld/generators/forest.lua:30-39`) — so this only fires while a rest is not wanted,
    /// which is the dev's own condition: *when the goal is not rest and a chest is visible, detour
    /// to it, open it, then rejoin the path.*
    Chest,
    /// A shrine we have not consecrated.
    ///
    /// Ranked below [`Goal::OpenAnomaly`] because consecrating is *impossible* until the anomaly
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
    /// We know where we want to go and not how to get there, so we explore toward it.
    ///
    /// Carries the goal that was declined, because that is what the hop is really *for* and the log
    /// had no way to say it. `Explore` used to cover this case too, which is how a reader concluded
    /// that exploring meant the anomaly had not opened yet — while the run had in fact been unable to
    /// route to an open portal for its entire length.
    ///
    /// **Not the same question as whether it can steer.** Knowing the destination is one thing;
    /// having a position to aim at is another, and [`Plan::steered_by`] answers that separately. It
    /// can be `None` while this is `Some`, and that combination is exactly the failure that walked a
    /// run out of the corruption on 2026-08-12.
    RouteTo(Box<Goal>),
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
    pub fn cache_text(&self) -> String {
        let mut out = format!("{CACHE_VERSION}\n");
        for p in self.places.values() {
            let (x, y) = match p.pos {
                Some((x, y)) => (x.to_string(), y.to_string()),
                None => ("-".to_string(), "-".to_string()),
            };
            out.push_str(&format!(
                "p\t{}\t{}\t{x}\t{y}\t{}\t{}\t{}\n",
                p.key,
                p.heading,
                p.connections,
                p.hidden.map(|h| h.to_string()).unwrap_or_else(|| "-".into()),
                p.parent.clone().unwrap_or_default(),
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
        let positions_are_trustworthy = text.starts_with(CACHE_VERSION);
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
                    let place = self.entry(key);
                    if place.heading.is_empty() {
                        if let Some(h) = heading.filter(|h| !h.is_empty()) {
                            place.heading = h.to_string();
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
                p.heading = heading.clone();
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
            here.heading = a.here_heading.clone();
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
            place.heading = n.heading.clone();
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
                    place.heading = e.to_heading.clone();
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
    /// Where to go next, with the Well-Rested bank taken into account.
    ///
    /// Two layers, deliberately separate. [`WorldMap::next_errand`] answers *what this run wants to
    /// do*, which is the whole ladder and every rule already written into it. This one asks one
    /// further question of the answer: **is what we are walking into a fight we should not take
    /// with an empty bank?** — and if so, buys stacks first.
    ///
    /// A wrapper rather than another rung, because the condition is not about a destination's
    /// merit. Every branch of the ladder can name a deep fight, and a rung would have to be
    /// repeated in each of them; asking afterwards catches all of them once.
    pub fn next_target(&self) -> Option<Plan> {
        self.next_errand().map(|p| self.bank_first(p))
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

    /// **Uncorrupted major shrines still available to consecrate**, folded the same way
    /// [`WorldMap::consecrations`] folds them so a shrine and its plaza never count twice.
    ///
    /// The denominator of [`WorldMap::corrupted_shrines_are_needed`]. `abandoned` and `avoid` are
    /// excluded because a shrine the driver gave up on is not a shrine we can still bank — leaving
    /// it in would hold the corrupted ones off the list for ever on the strength of one we cannot
    /// finish.
    fn clean_shrines_left(&self) -> usize {
        self.places
            .values()
            .filter(|p| p.can_be_consecrated() && !p.consecrated && !p.corrupted && !p.avoid)
            .filter(|p| !self.abandoned.contains(&p.key))
            .map(|p| p.key.strip_suffix("_plaza").unwrap_or(&p.key))
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    }

    /// **Has the free supply run out with the bar still short?** Task #74.
    ///
    /// The dev, 2026-08-21: *a corrupted shrine should become a target when it is the last to
    /// satisfy the requirement. That also means that if two shrines are in the corruption, then they
    /// become targets after two consecrations of uncorrupted shrines. Generalize this to any number
    /// of corrupted shrines.*
    ///
    /// Both sentences describe this one condition. The second is what rules out the other reading:
    /// *we can already see we will need them* would admit a corrupted shrine at nought
    /// consecrations whenever the clean ones are too few, and the dev put the moment at **after**
    /// the clean ones are spent. Take the free ones first, always — a corrupted shrine costs a fight
    /// that can end the run, and a free consecration banked before it is one kept.
    ///
    /// **Both halves are load-bearing.** Without the bar test this becomes the shrine-chase of
    /// 2026-08-15 again, since the clean supply is empty once the bar is met and every corrupted
    /// shrine would turn back into a detour on the way to the portal.
    ///
    /// The count is over shrines we **know about**, which is sound only because this is reached with
    /// the portal open, and opening it reveals every shrine in the world — the beams, and skipping
    /// the cutscene does not skip the revelation (`shrine.lua`'s `initStates`, *so we can save right
    /// away and main menu skip*). Before the portal there is no corruption to weigh anyway.
    fn corrupted_shrines_are_needed(&self) -> bool {
        self.consecrations() < SHRINES_BEFORE_THE_ANOMALY && self.clean_shrines_left() == 0
    }

    /// **Buy Well-Rested stacks before walking into a deliberate deep fight.**
    ///
    /// Returns `plan` untouched unless every clause of the dev's rule holds, in which case the trip
    /// becomes [`Goal::StockUp`] at the nearest inn:
    ///
    /// - the destination is a fight we would be **taking on purpose**
    ///   ([`Place::deliberate_fight_level`], which excludes forests for the dev's reason);
    /// - it is deep enough to be worth the errand ([`crate::rest::worth_banking_for`]);
    /// - the bank is short of twice its level ([`crate::rest::stacks_short`]);
    /// - **there is still ten gold**, the dev's floor and the inn's own gate — below it the
    ///   requirement is unmeetable rather than merely unmet, and holding the run at it would stall;
    /// - and a bed can actually be reached.
    ///
    /// The last two are what stop this looping. Every other clause is a fact about the world that
    /// the errand itself changes: each rest spends ten gold and adds one stack, so the run walks
    /// toward the condition it is testing and either satisfies it or runs out of money.
    ///
    /// Note it does **not** consult `wants_rest`. That flag is about damage taken; this is about
    /// damage to come, and the two are independent — a run at full health with an empty bank is
    /// exactly the case this exists for.
    /// How many more Well-Rested stacks a trip to `target` wants. Zero unless every clause holds.
    ///
    /// Split out of [`WorldMap::bank_first`] because the inn needs the same number: the planner uses
    /// it to decide *whether to go*, and [`crate::navigate::Run::rest_at_inn`] uses it to decide
    /// **how many times to press `Rest`** once it is there. Two answers from one function, so a
    /// run cannot set off for stacks it will not then buy.
    fn stacks_short_for(&self, target: &str) -> i64 {
        let Some(level) = self.places.get(target).and_then(Place::deliberate_fight_level) else {
            return 0;
        };
        match crate::rest::worth_banking_for(level) {
            true => crate::rest::stacks_short(self.well_rested, level),
            false => 0,
        }
    }

    /// **How short the bank is for the deepest deliberate fight we know about.**
    ///
    /// The *inn's* question, and deliberately a different one from [`WorldMap::bank_first`]'s.
    /// That one asks "is the place I am walking to a fight I should bank for", because it is
    /// deciding whether to make a special trip. This one asks "are stacks wanted at all", because
    /// by the time it is asked we are already standing in the village and the only decision left is
    /// whether to spend ten gold on the way past.
    ///
    /// Asking the ladder again here would answer with the inn we are standing in — whose
    /// `deliberate_fight_level` is `None` — and the count would collapse to zero exactly where it
    /// is needed. The deepest known fight is the honest stand-in: the anomaly is on the map from
    /// the moment the portal opens, and a crypt we have seen does not stop being a crypt because
    /// this step is heading elsewhere.
    ///
    /// ## Why this terminates, which is the part that matters
    ///
    /// It is the one question this file has been burned by before — a bed that stays wanted is a
    /// loop, and `docs/superpowers/notes/navigation-loops.md` catalogues six of them. **Every rest
    /// moves both terms in the same direction**: one stack banked, ten gold gone. So the condition
    /// is strictly monotone under its own errand and ends either when the bank is deep enough or
    /// when the purse drops below [`crate::rest::INN_COST`] — the dev's floor, and the inn's own
    /// gate. Neither exit depends on anything we cannot see.
    pub fn stacks_short_ahead(&self) -> i64 {
        self.places
            .values()
            .filter_map(Place::deliberate_fight_level)
            .filter(|l| crate::rest::worth_banking_for(*l))
            .max()
            .map(|l| crate::rest::stacks_short(self.well_rested, l))
            .unwrap_or(0)
    }

    /// **Which fight [`WorldMap::stacks_short_ahead`] is pricing.** For the log.
    ///
    /// The shortfall alone is not readable: `16 stack(s) short` is `2 x 8 - 0` and equally
    /// `2 x 9 - 2`, and the run of 2026-08-21 1519Z printed 16 at startup and 0 at every rest with
    /// three thousand gold in the purse and the same level 8 anomaly on the map throughout. Two
    /// numbers cannot be told apart by their difference. Naming the node and the bank beside it
    /// makes the line answer for itself.
    ///
    /// Reproduced from `map-cache/world-0.txt` alone: 699 places, deepest `start`
    /// (`Cottam - level 8 anomaly`), and `stacks_short_ahead` is exactly 16 with an empty bank. So
    /// the *shortfall* was never in doubt; what the line could not say was what it thought the
    /// bank held.
    pub fn deepest_fight(&self) -> Option<(&str, u32)> {
        self.places
            .values()
            .filter_map(|p| p.deliberate_fight_level().map(|l| (l, p.key.as_str())))
            .filter(|(l, _)| crate::rest::worth_banking_for(*l))
            .max()
            .map(|(l, k)| (k, l))
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

    /// Is there still a heart on a shelf somewhere we have not written off?
    ///
    /// Mirrors the filter in [`WorldMap::plan`]'s heart branch, and must keep mirroring it: this is
    /// what reserves the gold that branch will want, so a disagreement would hoard for an errand
    /// that can never fire. The standing assumption is the dev's — every village's general store
    /// starts with one — so the interesting state is which no longer do.
    fn a_heart_is_still_for_sale(&self) -> bool {
        self.places.values().any(|p| {
            p.stocks_a_heart()
                && !p.avoid
                && !self.heart_bought.contains(&p.key)
                && !self.abandoned.contains(&p.key)
        })
    }

    /// **Gold that banking may not touch.**
    ///
    /// The dev's ruling, 2026-08-20: *hearts have priority over the Well-Rested stacks.* Both are
    /// preparation bought with the same purse, and left to compete a sixteen-stack bank at ten gold
    /// a stack empties a hundred-gold heart's budget on the way past — which trades four **maximum**
    /// health, permanent for the rest of the run, for a bank that a single fight spends.
    ///
    /// [`HEART_FLOOR`] rather than [`HEART_COST`], because that is the bar the heart errand itself
    /// nominates on: the price *plus a night's bed*, which the dev raised it to on 2026-08-15 after
    /// watching the errand spend a run down to nothing. Reserving less would leave a purse that can
    /// buy the heart and not survive it.
    ///
    /// Zero once no heart is left to buy — hoarding for an errand that cannot fire is just a run
    /// that never banks.
    ///
    /// **Health is not subject to this.** `wants_rest` keeps its own plain [`crate::rest::INN_COST`]
    /// gate in [`WorldMap::wants_a_bed`]: the ruling ranks two kinds of *preparation*, and being
    /// hurt outranks both.
    fn heart_reserve(&self) -> i64 {
        match self.a_heart_is_still_for_sale() {
            true => HEART_FLOOR,
            false => 0,
        }
    }

    /// **How many stacks to actually buy**, after the heart's reserve is taken out of the purse.
    ///
    /// [`WorldMap::stacks_short_ahead`] is how deep the want is; this is how much of it the purse
    /// may serve. Everything that spends on stacks asks *this* one — the planner deciding whether
    /// the trip is worth making, and the inn deciding how many times to press — so a run cannot set
    /// off for stacks it will then refuse to pay for.
    ///
    /// The dev's original floor survives inside it: with no heart left to reserve for, this is
    /// positive exactly when the purse holds [`crate::rest::INN_COST`], which is the inn's own gate
    /// and the point below which the requirement is unmeetable rather than merely unmet.
    pub fn stacks_to_buy(&self) -> i64 {
        let spendable = (self.gold - self.heart_reserve()).max(0);
        self.stacks_short_ahead().min(spendable / crate::rest::INN_COST)
    }

    /// **Is a bed wanted at all**, for either of the two reasons there are?
    ///
    /// [`WorldMap::wants_rest`] is damage already taken; [`WorldMap::stacks_short_ahead`] is damage
    /// to come. The village-crossing rules ask this rather than `wants_rest` alone, or a run sent to
    /// an inn by [`WorldMap::bank_first`] would walk into the village at full health, find nothing
    /// that wanted a bed, and walk out again — which is the exact shape of the campfire stall
    /// recorded at [`crate::rest::CAMPFIRE_REST_IS_BUILT`], and it ends the same way, with the loop
    /// guard.
    ///
    /// The gold gate is left where it already was in each caller: both of them check it, and both
    /// check it against the same [`crate::rest::INN_COST`].
    pub fn wants_a_bed(&self) -> bool {
        self.wants_rest || self.top_up || self.stacks_to_buy() > 0
    }

    fn bank_first(&self, plan: Plan) -> Plan {
        // **Both questions, and they are not the same one.** `stacks_short_for` asks whether the
        // place we are walking to is a fight worth banking for; `stacks_to_buy` asks whether the
        // purse may serve it once the heart's reserve is out. A trip taken on the first alone would
        // arrive at an inn that then declined to press.
        if self.stacks_short_for(&plan.target) == 0 || self.stacks_to_buy() == 0 {
            return plan;
        }
        let here = self.here.as_deref().unwrap_or("");
        let dist = self.distances(here);
        // The route test only. Hostile ground is the rest goal's concern, and this errand is taken
        // at whatever health we happen to have — refusing a bed for being awkward to reach would
        // leave the fight itself as the alternative.
        let ok = |p: &Place| self.can_route_to(&p.key);
        match self.best_rest_site(here, &dist, &ok) {
            Some(bed) if bed.key != plan.target => {
                Plan { target: bed.key.clone(), reason: Goal::StockUp, steered_by: None }
            }
            _ => plan,
        }
    }

    fn next_errand(&self) -> Option<Plan> {
        // While a rest is wanted, EVERY branch skips areas that cost a fight to leave — not just the
        // one heading for the objective. A plan is a plan whatever its reason, and arriving at a
        // shrine or a frontier through an uncleared corrupted village hurts exactly as much as
        // arriving at an anomaly trigger that way.
        //
        // Two passes, so the rule cannot strand the run: if avoiding hostile ground leaves nothing
        // at all to do, the second pass drops the restriction and we get on with it.
        if self.wants_rest {
            if let Some(plan) = self.plan(true, true) {
                return Some(plan);
            }
            if let Some(plan) = self.plan(true, false) {
                return Some(plan);
            }
            // Nowhere safe left on the map. Rather than fall straight through to the objective —
            // which is ordered by *usefulness* and can put a level 8 anomaly first — take the
            // cheapest fight there is.
            if let Some(plan) = self.easiest_hostile() {
                return Some(plan);
            }
        }
        // **Not hurt, so hostility is not a filter — and note which argument is which.**
        //
        // `plan(skip_hostile, need_route)`. Both calls here pass `skip_hostile = false`, so the
        // dev's standing rule already holds where targets are chosen: risk ordering belongs to the
        // rest goal and nothing else. It is counterproductive on the way to the anomaly, because
        // corruption *is* high node levels, so skipping hostile ground points a run away from the
        // one place it is trying to reach.
        //
        // The two passes differ on `need_route`, which is a different question entirely: prefer a
        // target we can already reach, and fall back to one we cannot rather than stranding.
        // Written down because the pair reads like a hostility cascade at a glance, and on
        // 2026-08-15 it fooled me into "fixing" a rule that was not broken.
        self.plan(false, true).or_else(|| self.plan(false, false))
    }

    /// Is there a known route to `key` that takes **no fight at all** on the way?
    ///
    /// The dev's rule, stated many times and finally written down here: *go toward the anomaly
    /// unless there is an accessible shrine that does not require combat*. A shrine is worth the
    /// detour because it is free — it pays in wildcard tiles and costs only walking — and the moment
    /// a fight is on the way it is not free, it is the anomaly's budget spent early.
    ///
    /// **Combat, not crypts.** The first version of this tested `type_is("crypt")`, because the run
    /// that prompted it had died in one. That is the example, not the rule, and the difference is
    /// most of a map: `l22` is a bandit camp, `l24` a level 3 spider forest, `l40` a level 5
    /// graveyard — every one of them a fight, none of them a crypt, and all four sat on the route
    /// this function called free on 2026-08-15. The test is [`heading_has_combat`], which is the
    /// same one the rest of this file uses to know a fight when it sees one.
    ///
    /// **Only an unfinished fight counts**, because `completed` is what says whether one is still
    /// owed — the same reading as everywhere else here.
    ///
    /// **The destination is not exempt**, unlike [`WorldMap::step_avoiding`]'s blocked set. There,
    /// refusing to path to where we were told to go would be failure; here the question *is*
    /// whether going costs a fight, and a shrine whose own node still has to be fought for costs one
    /// — `Swanland — level 6 shrine` is a real heading from this very save.
    ///
    /// Breadth-first over the same edges as [`WorldMap::distances`] and deliberately *not* weighted:
    /// the question is whether such a route exists at all, not which one is cheapest.
    fn reachable_without_a_fight(&self, from: &str, to: &str) -> bool {
        matches!(self.access_without_a_fight(from, to), Access::Free)
    }

    /// Does this place have roads we have never looked down?
    ///
    /// `connections` is the **game's** degree for the node, printed beside it in every dump
    /// (`connections: 4`), while `neighbours` is what we have actually seen named. A gap between them
    /// is not a guess: it is the game telling us there is more here than we have recorded.
    ///
    /// Deliberately not [`Place::is_frontier`], which is the other candidate and would be wrong.
    /// That reads `!visited || hidden > 0`, and `visited` means "stood here **this run**" and is
    /// pointedly not restored from the cache — so on a resumed run every place is a frontier and the
    /// answer degenerates to "always". This one survives a restart because both halves of it do.
    fn has_unexplored_roads(&self, key: &str) -> bool {
        self.places
            .get(key)
            // **And we have not already stood there.** Task #73, the dev 2026-08-21: *one probe per
            // node, remembered.*
            //
            // Standing on a node is the act that makes the game name its neighbours, so a gap that
            // survives a visit is one visiting cannot close — most often a **secret** neighbour,
            // which `verboseAdjacencyData` prints as `Hidden location` and never names until a tower
            // reveals it (`locationIsVisible`, `overworldview.lua:554-556`; task #14 is the reveal).
            // Counting it as unexplored makes the node a permanent magnet: the errand nominates it,
            // `next_target` excludes `here` so the pull vanishes on arrival, exploring hops away, and
            // from over there it is a candidate again.
            //
            // **A ranking was tried first and was not enough.** `probe_toward_the_unknown` sorts on
            // `(visited, dist)`, which decides between candidates — and with a single candidate it
            // decides nothing. One is the ordinary case: the `l11` / `l13` bounce of the 1519Z run
            // went three full laps *with that ranking in the binary* before `LOOP_WRITE_OFF` broke
            // it.
            //
            // This is the stricter reading of the dev's earlier correction, not a retreat from it:
            // *it should be the navigator's responsibility to probe and build the cache, not to
            // assume from the cache.* Probing is exactly what a visit is. Concluding **after** one is
            // the responsibility discharged; concluding before it is the thing they objected to.
            //
            // `visited` is set only by standing somewhere **this run** and is pointedly not restored
            // from the map cache, so a resumed run probes afresh — the right side to err on, since a
            // tower may have revealed the neighbour while we were away.
            .map(|p| !p.visited && p.connections as usize > p.neighbours.len())
            .unwrap_or(false)
    }

    /// [`WorldMap::reachable_without_a_fight`], with the two kinds of "no" kept apart.
    ///
    /// ## The dev's correction, 2026-08-15
    ///
    /// > It should be the navigator's responsibility to probe and build the in-memory cache of which
    /// > nodes are accessible without combat, not to assume from the cache that they are
    /// > inaccessible without combat.
    ///
    /// Right, and the bare `bool` is what made that impossible to honour: it collapsed *I know a
    /// fight is in the way* into the same answer as *I have no edges over there at all*, and the
    /// heart errand read both as "there is no heart to be had for free". Live 2026-08-15 the map held
    /// `l28 Enholmes town` with **four** declared connections and exactly **one** recorded — three
    /// roads never looked down — and the run reported it unreachable as a fact.
    ///
    /// [`Access::Unknown`] is that case: nothing in the fight-free region reaches the target, but the
    /// region touches a node with unexplored roads, so it may not end where our edges do.
    ///
    /// ## What is *not* a source of pessimism, measured
    ///
    /// Recalled headings are not, and this is worth stating because it is the natural suspicion.
    /// `AreaHeading` prints the level only when `locationHasCombat`, and that is
    ///
    /// ```lua
    /// function core.locationHasCombat(location)
    ///     if core.areaIsComplete(location.key) then return false end
    ///     return not core.locationIsCompleteOnVisit(location)
    /// end
    /// ```
    ///
    /// (`overworldview.lua:305-310`). `locationIsCompleteOnVisit` is a property of the location's
    /// *type* and does not change; `areaIsComplete` is a save flag we re-read every step and already
    /// AND into [`Place::may_be_a_fight`]. So a recalled `— level N` can only stop being true through
    /// the one input we already take live. Corruption moves it the other way and arrives in the save
    /// too. Of the four surface nodes the 2026-08-15 run named live, four agreed with the cache.
    ///
    /// The edges are the whole of the gap, which is why this counts roads and not headings.
    fn access_without_a_fight(&self, from: &str, to: &str) -> Access {
        let costly = |k: &str| {
            self.places.get(k).map(|p| !p.completed && p.may_be_a_fight()).unwrap_or(false)
        };
        if costly(to) {
            return Access::Blocked;
        }
        let mut seen: BTreeSet<&str> = [from].into_iter().collect();
        let mut queue: std::collections::VecDeque<&str> = [from].into();
        while let Some(k) = queue.pop_front() {
            if k == to {
                return Access::Free;
            }
            let Some(p) = self.places.get(k) else { continue };
            for n in &p.neighbours {
                if seen.contains(n.as_str()) || costly(n) {
                    continue;
                }
                seen.insert(n);
                queue.push_back(n);
            }
        }
        // The target is out of reach over the edges we hold. Whether that is a fact about the map or
        // a fact about our ignorance depends on whether the region we just walked has any way out we
        // have not tried.
        match seen.iter().any(|k| self.has_unexplored_roads(k)) {
            true => Access::Unknown,
            false => Access::Blocked,
        }
    }

    /// The nearest place inside the fight-free region around `from` that still has roads we have not
    /// looked down — somewhere to go and *find out*, rather than a target in itself.
    ///
    /// Walks the same region [`WorldMap::access_without_a_fight`] does, so a probe it returns is
    /// reachable on the same terms as the errand that wanted one: no fight to get there.
    fn probe_toward_the_unknown(
        &self,
        from: &str,
        dist: &BTreeMap<String, usize>,
    ) -> Option<&Place> {
        let costly = |k: &str| {
            self.places.get(k).map(|p| !p.completed && p.may_be_a_fight()).unwrap_or(false)
        };
        let mut seen: BTreeSet<&str> = [from].into_iter().collect();
        let mut queue: std::collections::VecDeque<&str> = [from].into();
        while let Some(k) = queue.pop_front() {
            let Some(p) = self.places.get(k) else { continue };
            for n in &p.neighbours {
                if seen.contains(n.as_str()) || costly(n) {
                    continue;
                }
                seen.insert(n);
                queue.push_back(n);
            }
        }
        seen.into_iter()
            .filter(|k| *k != from && self.has_unexplored_roads(k))
            .filter(|k| !self.abandoned.contains(*k))
            .filter_map(|k| self.places.get(k))
            // **Somewhere we have not stood first, and only then somewhere we have.**
            //
            // The dev, 2026-08-21: *shouldn't the heart errand keep searching the frontier?* Yes,
            // and re-nominating a node it has already stood on is how it stopped.
            //
            // `has_unexplored_roads` compares the game's declared `connections` against the
            // neighbours we have seen named, and being *at* a node is what makes the game name them.
            // So a node we have stood on whose gap is still open has a gap that visiting cannot
            // close — most often a **secret** neighbour, which `verboseAdjacencyData` prints as
            // `Hidden location` and never names until a tower reveals it (`locationIsVisible`,
            // `overworldview.lua:554-556`). Probing it again buys nothing and the errand at the far
            // end nominates the way back. Live 2026-08-21, `l11 Argham crossroads` against `l5
            // Dalton Copse`, two full laps before the write-off broke it:
            //
            // ```text
            //   6. l11 -> **l5**  (for l39, Explore)
            //   7. l5  -> **l11** (for l11, Heart)
            //   8. l11 -> **l5**  (for l39, Explore)
            //   9. l5  -> **l11** (for l11, Heart)
            //  10. `l11` is written off — stood on 2 times with nothing learned
            // ```
            //
            // **A preference and not a filter**, which is the whole of the dev's earlier correction:
            // *it should be the navigator's responsibility to probe and build the cache, not to
            // assume from the cache.* Excluding visited nodes outright was tried and it broke
            // `a_heart_behind_the_fog_is_probed_rather_than_written_off`, where the one candidate has
            // been stood on and going anyway is right — there is nothing else to try, and a probe
            // that refuses the only road left has stopped searching. Ranked, that fixture is
            // unchanged and the bounce still ends: with two candidates the unvisited one wins.
            //
            // `visited` is set only by standing somewhere **this run**, so a resumed run probes
            // afresh — the right side to err on, since the gap may have closed while we were away.
            .min_by_key(|p| (p.visited, dist_or_far(dist, &p.key)))
    }

    /// Could [`WorldMap::next_hop`] actually set off toward `key`, over the edges we have recorded?
    ///
    /// The question `plan` failed to ask, and the run of 2026-08-09 is what that cost. Every travel
    /// goal it printed read `(for start, Anomaly)` — five of five — while the map held `start` with
    /// no heading and no edges at all, learned from `start_first_corrupt_time` in the save and never
    /// seen. The anomaly branch outranks the shrine branch and does not yield, so `shrine6 Borsea
    /// shrine` — heading known, unused, unconsecrated, uncorrupted, and reachable — was never
    /// considered. `next_hop` found no route, fell through to its any-frontier fallback, and the run
    /// wandered onto a level 6 crypt and died there while the log claimed it was going somewhere.
    ///
    /// So three costs, and only the first is cosmetic: the log lies, a reachable objective is
    /// skipped, and the fallback picks fights nobody chose.
    ///
    /// ## Unreachable and unmeasured look the same, and this does not try to tell them apart
    ///
    /// A node we have never stood beside has no edges, so "no route to it" is also what "we have not
    /// mapped the way yet" looks like — [`Arrival::Unknown`] exists because conflating two such
    /// states put a run at 1/20 in front of a crypt. This deliberately does not distinguish them.
    /// It reports what `next_hop` can do *now*, and the caller uses it as a preference with a second
    /// pass behind it, so a target we merely have not mapped is demoted rather than discarded.
    ///
    /// Asked with `shun` false, which is `next_hop`'s second attempt: a route that has to cross a
    /// lost woods we escaped is a poor route and still a route. The rest of the exclusions —
    /// abandoned nodes, chests while hurt — are shared with [`WorldMap::step_avoiding`] by calling
    /// it, rather than restated here where the two could drift apart.
    ///
    /// Standing on the target counts. `first_step_toward` answers `None` for `from == to` because
    /// there is no step to take, which is not the same as being unable to get there, and treating it
    /// as unreachable would make the planner walk away from somewhere it had just arrived.
    fn can_route_to(&self, key: &str) -> bool {
        let Some(here) = self.here.as_deref() else { return true };
        key == here || self.first_step_toward(here, key, false).is_some()
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
    ///
    /// ## Not route-gated, unlike [`WorldMap::plan`], and that is a decision
    ///
    /// #24 made every branch of the planner refuse a target nothing can reach. This one is exempt,
    /// because here the two orderings pull against each other: preferring what we can route to means
    /// taking a reachable level 6 crypt over an unreachable level 1 forest, and this function exists
    /// precisely to *not* do that — it is reached only while hurt, with nowhere safe left, which is
    /// the state where picking the wrong fight ends the run.
    ///
    /// So an unreachable cheapest fight is still named, and `next_hop` steps its way. Nothing has
    /// measured which of the two costs more, and the safe direction while hurt is the gentle fight.
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
            .map(|p| Plan { target: p.key.clone(), reason: Goal::EasiestHostile { level: p.level() }, steered_by: None })
    }

    /// The planner proper. `skip_hostile` excludes anywhere a fight is owed, on either axis;
    /// `need_route` excludes anywhere [`WorldMap::next_hop`] could not actually set off toward.
    ///
    /// **Both axes, and that is the fix.** It used to consult [`Place::hostile_to_enter`] alone,
    /// which asks whether a subworld will hold us in — so a crypt, which is not a subworld and
    /// cannot trap anybody, sailed through the hurt pass and a run at 1/20 was routed onto a level 6
    /// one under [`Goal::Explore`]. Meanwhile `shrine5` sat on the same map printing
    /// `Gembling shrine` with no level at all, free to walk onto.
    ///
    /// ## Why `need_route` is a pass over the whole ladder and not a patch on one branch
    ///
    /// A branch that cannot make progress used to outrank every branch that could. The anomaly is
    /// the case that showed it — see [`WorldMap::can_route_to`] for the live run — but nothing about
    /// it is specific to the anomaly: a shrine in a component we have not linked up yet, or a
    /// frontier node known only by key from `completedAreas`, suppress the branches below them in
    /// exactly the same way. So the question is asked of every candidate, in `ok`, alongside the
    /// hostility one.
    ///
    /// It is a *preference*, never a filter on the map: [`WorldMap::next_target`] runs this twice
    /// and the second pass drops the requirement. Nothing routable is a real state — a fresh launch
    /// standing nowhere is one — and a run that refuses to name a target it cannot yet reach has
    /// stopped, which is worse than aiming at one and stepping the way it lies.
    ///
    /// ## And it is asked on the SURFACE ONLY
    ///
    /// Inside a subworld our graph is **known** to be disconnected: nothing links an interior node
    /// to its container, so `distances` from inside reaches no surface node at all — the limitation
    /// [`WorldMap::exit_toward`] is written around. Every surface target therefore fails a route
    /// test from in there, for a reason that is about our model and not about the world, and gating
    /// on it would hand the run to whichever interior frontier happened to be walkable.
    ///
    /// Which is not hypothetical: it turned "I am at 1 of 20 health and there is a campfire out the
    /// west door" into `Goal::Explore`, and the door commitment follows the errand. The route test
    /// is a statement about what [`WorldMap::next_hop`] can do, and while we are inside, `next_hop`
    /// is not what moves us — [`WorldMap::cross_toward`] is, and it reaches the surface through
    /// exits rather than edges.
    /// **The best place to sleep**, by site then distance then key — or `None` if none can serve.
    ///
    /// Extracted so the two errands that want a bed cannot drift apart. [`Goal::Rest`] asks because
    /// health is down; [`Goal::StockUp`] asks because the Well-Rested bank is short of what the next
    /// deliberate deep fight wants. They differ in *why* and not at all in *where*, and this file
    /// has been bitten twice by two predicates that were meant to agree and quietly stopped —
    /// see [`Place::is_shrine`] and the `l28 <-> l27` bounce.
    ///
    /// `ok` is the caller's own admissibility test, so the hostile-ground and route rules stay where
    /// they are decided rather than being re-derived here.
    fn best_rest_site<'a>(
        &'a self, here: &str, dist: &BTreeMap<String, usize>, ok: &dyn Fn(&Place) -> bool,
    ) -> Option<&'a Place> {
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
            // **Verified, and it is a race.** The inn has three button sets
            // (`overworld/generators/village.lua:360-393`) and only one of them can serve:
            //
            // ```text
            //   areaButtons            Enter -> ui.inn             the innkeeper, rest works
            //   underAttackAreaButtons Enter -> ui.building_empty  an empty shell, no rest
            //   destroyedAreaButtons   a loot button only          the inn is gone for good
            // ```
            //
            // So corruption does not merely gate the inn, it starts a clock: clear the village
            // in time and the real inn comes back, leave it and the inn is destroyed
            // permanently. Confirmed by the dev, 2026-08-12, and it replaces the guess that used
            // to sit here — which had the release right and knew nothing about the deadline.
            //
            // `completed` is that release and the filter is correct for it. **The gap is the
            // third state**: nothing here distinguishes a village still under attack from one
            // whose inn is already rubble, because both are `corrupted` and neither is
            // `completed`. Today that costs nothing — both are excluded — but a run that clears
            // a long-corrupted village expecting a bed will find a loot pile, and `completed`
            // will say it may sleep there. See task #26, which wants the loot anyway.
            .filter(|p| p.key != here && !p.avoid && (!p.corrupted || p.completed) && ok(p))
            // A settlement under attack or lost has no bed — see [`Place::trades`]. Campfires
            // are unaffected by it and excluded elsewhere entirely
            // (`rest::CAMPFIRE_REST_IS_BUILT`).
            .filter(|p| !p.has_an_inn() || p.trades())
            .filter_map(|p| crate::rest::site(&p.heading).map(|s| (p, s)))
            .filter(|(p, s)| crate::rest::can_rest_at(*s, self.gold, self.fuel, !p.used))
            .collect();
        // **Site, then distance, then key.** The middle term is new, and its absence is the
        // whole of the dev's question of 2026-08-15: *"after we completed the level 7 crypt, why
        // did the navigator choose such a distant village to rest at instead of the ones we
        // bought healthBuffs from?"*
        //
        // Because the tie-break was `pa.key.cmp(&pb.key)` and nothing else. Two inns of equal
        // rank were separated **alphabetically**, so the run walked four hops to `l11` while
        // `l19` — rested at earlier in that same run, and two hops away through `l9` — lost the
        // comparison to a string. `spike-run-20260815-1913Z.md` steps 49-53 are that walk. It is
        // not a near miss either: `l100` sorts before `l11`, which sorts before `l2`.
        //
        // Distance is in hops rather than anything cleverer, because hops are what we have —
        // see #21. Unreachable sites sort last rather than being dropped: `next_target` runs the
        // whole ladder a second time with the route requirement lifted, and a bed we cannot yet
        // plot a course to is still better than no bed at all.
        //
        // The key stays as the final tie-break so the choice is deterministic, which several
        // tests depend on. **It is no longer ordering anything that matters.**
        let far = |p: &Place| dist.get(&p.key).copied().unwrap_or(usize::MAX);
        sites.sort_by(|(pa, sa), (pb, sb)| {
            sb.rank()
                .cmp(&sa.rank())
                .then(far(pa).cmp(&far(pb)))
                .then(pa.key.cmp(&pb.key))
        });
        sites.first().map(|(p, _)| *p)
    }

    fn plan(&self, skip_hostile: bool, need_route: bool) -> Option<Plan> {
        let here = self.here.as_deref().unwrap_or("");
        let need_route = need_route && self.inside().is_none();
        // Both halves are deliberately last in every filter chain below: `can_route_to` runs a
        // search, and the cheap tests in front of it decide most candidates for free.
        let ok = |p: &Place| {
            !(skip_hostile && Self::owes_a_fight(p)) && (!need_route || self.can_route_to(&p.key))
        };

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
        //
        // Hoisted above the rest branch so both can use it. It used to be computed below, which is
        // why the bed was the one thing on this ladder chosen with no idea how far away it was.
        let dist = self.distances(here);
        if self.wants_rest {
            if let Some(p) = self.best_rest_site(here, &dist, &ok) {
                return Some(Plan { target: p.key.clone(), reason: Goal::Rest, steered_by: None });
            }
        }


        let anomaly_open = self.anomaly_is_open().unwrap_or(false);
        // A shrine still worth walking to. `Pray` retires on `areaUnused`; `Consecrate` needs
        // `hell ~= 0`, so an open portal makes an unconsecrated shrine live again whatever its
        // `_used` flag says. See the fuller account at the closed-portal branch below.
        //
        // **`corrupted` is a bill, not a verdict, and `completed` says whether it is still owed.**
        // The filter reads `corrupted && !completed` for the same reason
        // [`WorldMap::worth_consecrating_here`] does, and the two must agree: a shrine whose fight we
        // have already won costs nothing more to consecrate than an uncorrupted one. Live
        // 2026-08-14, they did not agree. `shrine1` sat `[done] [corrupted]` and unconsecrated for a
        // whole run — the arrival test would have consecrated it on sight, while this filter dropped
        // it before `ok` could even ask for a route, so the run never went. Dropping only the
        // `completed` clause here is what let the pair drift apart.
        //
        // **And the pair drifted again on 2026-08-17, the other way round.**
        // [`Place::can_be_consecrated`] was added to the arrival test and not to this one. The
        // consecration clause below is `!consecrated`, which for a **minor** shrine is true and stays
        // true for ever: `Consecrate` requires `majorShrine` (`shrine.lua:93-96`), so a woodland
        // shrine can never take the flag that would retire it. A prayed-at woodland shrine would
        // therefore have become a permanent `Goal::Shrine` the moment the portal opened — the same
        // shape as the `shrine1 -> l10 -> shrine1` bounce recorded at the closed-portal branch, but
        // with nothing that could ever end it, because arrival would decline the trip this filter
        // kept proposing.
        //
        // So the consecration half is gated on the consecration being *possible*. The prayer half is
        // not, and must not be: `showPrayButton` leads with `not shrineLocation.majorShrine`
        // (`shrine.lua:98-101`), so an unprayed minor shrine is a real errand whatever the portal is
        // doing.
        let worth_a_trip =
            |p: &Place| !p.used || (anomaly_open && p.can_be_consecrated() && !p.consecrated);
        // Hoisted: it walks every place, and asking it once per candidate would make the shrine
        // branch quadratic in the size of the map for an answer that cannot change inside one pass.
        let corrupted_are_needed = anomaly_open && self.corrupted_shrines_are_needed();
        let pick_shrine = || {
            self.places
                .values()
                .filter(|p| p.key != here && !p.avoid && p.is_shrine())
                // **A surface destination, so nothing with a parent.** [`Place::is_shrine`] answers
                // by key *or* heading, and only the key arm excludes subnodes — its own doc says
                // matching them "would make the planner target a node whose parent is where it
                // actually wants to go". The heading arm re-admits exactly what the key arm keeps
                // out, as soon as a dump names one: `Gripthorpe Brush woodland shrine` ends in
                // `shrine`, so `shrine1sub1` and `shrine1_plaza` are both candidates here.
                //
                // Mostly hidden today, and hidden is not fixed. Our graph has no edge across a
                // subworld boundary (see [`WorldMap::plan`]'s note), so `dist` scores an interior
                // node `usize::MAX` and `ok`'s route test drops it — but `plan` runs a **second pass
                // with `need_route` off**, and there the route test is not there to catch it. What is
                // left is a target `next_hop` cannot step toward.
                //
                // The right entry to an interior shrine is its container, and once inside,
                // [`WorldMap::shrine_inside`] and [`WorldMap::worth_consecrating_here`] own it. This
                // says that in the filter instead of relying on a distance that happens to sort last.
                .filter(|p| p.parent.is_none())
                .filter(|p| !self.abandoned.contains(&p.key))
                .filter(|p| worth_a_trip(p))
                // **With the portal shut, a shrine that would open it is not a free errand.**
                //
                // The dev, 2026-08-17, watching a run at 52/52 walk into `shrine7`, *Cottam Boscage
                // — level 9 forest*: *why did we even enter the lv9 forest instead of immediately
                // backtracking? That was nearly suicidal.*
                //
                // Both filters below are written `!anomaly_open || …`, so **before the portal opens
                // they are no-ops** and nothing at all priced a shrine's cost. `worth_a_trip`
                // reduces to `!used`, which a never-prayed level 9 forest satisfies as happily as a
                // level 1 one, and `Goal::Shrine` walked us into the fight.
                //
                // Arriving is also what *opened* the anomaly, which is the second half of the harm:
                // `world_evil.lua:15-18` fires on `locationHasCombat and level > 3`, so the errand
                // did not merely pick an expensive fight, it ended the phase it belonged to.
                //
                // ## `triggers_anomaly()` alone, and the missing `&& !completed` is the point
                //
                // This first read `triggers_anomaly() && !completed`, which is the predicate removed
                // from [`Place`] as guarding an unreachable state — and I cited that note as having
                // been about the whole rule. It is not. The dev, 2026-08-17:
                //
                // > There was apparently code to check whether a lv4+ node was considered "gentle"
                // > as internally, that is the case when it's already been cleared. However, that
                // > specific situation cannot happen because no nodes with levels are considered
                // > cleared to begin with.
                //
                // The unreachable state is **level 4+, cleared, portal still shut**. Clearing needs
                // arriving and arriving opens the portal, so while `hell == 0` a level 4+ node is
                // uncompleted without exception. `!completed` is therefore *always true* here, and
                // writing it says the opposite — that a cleared one is a case worth admitting.
                //
                // So the rule is the trigger and nothing else. That also leaves nothing for the note
                // on [`Place`] to disagree with: it retired a redundant clause, and this adds a
                // filter at a caller that never had one.
                .filter(|p| anomaly_open || !p.triggers_anomaly())
                // **With the portal open, a shrine has to be cheap or it is not a destination.**
                //
                // The dev's rule, 2026-08-15: target only unconsecrated, uncorrupted shrines that
                // can be reached without fighting a crypt on the way. A corrupted shrine is not a
                // target at all — consecrating one is something we do *if we happen to be there*,
                // having gone through it for another reason, and never a reason to make the trip.
                //
                // What this replaces was a filter that admitted any corrupted shrine whose fight was
                // already won, and ranked purely on distance. It sent the run of 2026-08-15 from
                // `shrine5` through `l50`, `l41`, `l51` and `l35` into `l49` — Yokefleet, a level 6
                // crypt — chasing another shrine, with the anomaly untouched. Every hop of it read
                // `RouteTo(Shrine)`.
                //
                // The cost is bounded and the benefit is not: a shrine bought with a crypt fight is
                // no longer cheap preparation, it is the level 8 fight's budget spent early.
                //
                // **Unless it is the only way left to reach the bar** — task #74, and the exception
                // the gate makes necessary. `SHRINES_BEFORE_THE_ANOMALY` requires consecrations the
                // world may not be able to supply cleanly, and a rule that refuses corrupted shrines
                // outright would leave such a world exploring an exhausted frontier until the
                // release fires. See [`WorldMap::corrupted_shrines_are_needed`], which admits them
                // only once the clean supply is spent *and* the bar is still short — so the free
                // ones are always taken first, and none of this reopens once the bar is met.
                .filter(|p| !anomaly_open || !p.consecrated)
                //
                // **And it must be able to advance the bar**, or the exception pays a fight for
                // nothing. `is_shrine` answers by heading as well as by key, so a minor shrine is a
                // candidate here — and a minor shrine can never be consecrated
                // ([`Place::can_be_consecrated`]), so no number of them moves `consecrations`. A
                // corrupted one is a fight whose reward the gate cannot spend.
                .filter(|p| {
                    !anomaly_open
                        || !p.corrupted
                        || (corrupted_are_needed && p.can_be_consecrated())
                })
                // **The route's cost is NOT a filter, and that is task #70.**
                //
                // There was a `reachable_without_a_fight` here, and it is gone. The dev, 2026-08-21:
                // *shrines should never be blocked by level unless the anomaly has not yet opened.*
                //
                // It cannot stay, because it does not price anything — `may_be_a_fight` is
                // `heading_has_combat || corrupted`, a **boolean**, so a level 1 forest disqualified
                // a route exactly as hard as a level 9 crypt. The heading is where the level comes
                // from at all (`AreaHeading` prints `— level N` only when `locationHasCombat`), so
                // the number was parsed and then thrown away.
                //
                // Live, 1519Z: the eastern shrines were revealed from the south and struck off while
                // a forest stood between, then consecrated from the north twenty steps later. The
                // detour bought nothing — one step after the second shrine the run took a level 6
                // forest fight anyway, to explore. The log carries **zero** `RouteTo(Shrine)` lines
                // in 175 steps, which is what proves this filter and not `ok`'s route test: a
                // candidate that survived to `ok` and failed it would have said so.
                //
                // **What replaced it is the gate, not nothing.** The 2026-08-15 rule this reverses
                // was written when a shrine was optional reward, and paying a crypt for one spent the
                // level 8 fight's budget early. [`SHRINES_BEFORE_THE_ANOMALY`] makes four of them a
                // precondition — see the `CloseAnomaly` branch, which already argues that *the
                // shrines are not a reward to collect on the way, they are the preparation the fight
                // needs*. Preparation we are required to buy cannot also be refused for costing
                // something.
                //
                // The pre-anomaly filter above is untouched and is a different rule: `triggers_anomaly`
                // refuses a level 4+ shrine while `hell == 0` because arriving there is what *opens*
                // the portal, which is not a question about cost at all.
                .filter(|p| ok(p))
                .min_by_key(|p| dist_or_far(&dist, &p.key))
        };

        // **With the portal live, a reachable shrine outranks the portal itself** — the dev's call,
        // 2026-08-10, and the argument is already written under [`Goal::Shrine`]. Consecrating is
        // *only* possible while `hell ~= 0` (`shrine.lua:93-96`), so the window is exactly now; it
        // costs no fight at an uncorrupted shrine, nor at a corrupted one whose fight is already
        // won, which is exactly the pair the filter above admits; and it pays in gold- and
        // silver-bordered wildcard tiles
        // (`utils/blessings.lua:95-110`), the mechanic this solver handles best.
        //
        // So the ordering is not "reward before objective", it is **cheap preparation before a level
        // 8 fight, during the only window in which it can be bought**. `ok` still demands a known
        // route, so this can only ever choose a shrine we can actually walk to.
        if anomaly_open {
            if let Some(p) = pick_shrine() {
                return Some(Plan { target: p.key.clone(), reason: Goal::Shrine, steered_by: None });
            }
        }

        // **A chest, if one is standing about unopened.** Task #16, and the dev's rule verbatim:
        // *when the goal is not rest and a chest is visible, detour to it, open it, then rejoin the
        // path.*
        //
        // ## This branch has never fired and cannot, in v52.4. The rule lives in `chest_inside`
        //
        // `next_target` plans over **surface** nodes, and there are no chests on the surface:
        // `typeName = 'chest'` occurs in exactly two files, `overworld/generators/forest.lua:178`
        // and `bandit_camp_forest.lua:201`, and both build subnodes off `parentNode.subnodeCount`.
        // Every chest in the game is inside a forest.
        //
        // It is kept rather than deleted because it is correct, cheap, and would be wanted the day a
        // generator places one outdoors — and deleted-and-rewritten is how this rule got written
        // twice already. But **do not read its existence as the chest detour working**: that is
        // [`WorldMap::chest_inside`], reached through [`WorldMap::errand_inside`] while crossing.
        // The tests under this branch use surface fixtures and prove only that the branch is wired.
        //
        // `wants_rest` is the whole of "the goal is not rest", and it is the right gate rather than a
        // cautious one: `Open` starts a combat scenario (`overworld/generators/forest.lua:30-39`),
        // so a chest is a fight, and a fight is the last thing a hurt run should detour for. The
        // rest branch above has already claimed those runs anyway.
        //
        // `ok` still demands a route, and `abandon` still writes one off, so this cannot nominate a
        // chest we could not reach or have already given up on. Nearest first — a chest is worth the
        // walk, not any length of walk.
        if !self.wants_rest {
            let chest = self
                .places
                .values()
                .filter(|p| p.key != here && p.is_chest() && !p.completed && !p.avoid)
                .filter(|p| !self.abandoned.contains(&p.key) && ok(p))
                .min_by_key(|p| (dist_or_far(&dist, &p.key), p.key.clone()));
            if let Some(p) = chest {
                return Some(Plan { target: p.key.clone(), reason: Goal::Chest, steered_by: None });
            }
        }

        // **A heart, if one can be had for nothing but walking.**
        //
        // The dev's rule, 2026-08-15, given the evening a run finally died going the right way:
        // two level 6 corrupted crypts back to back, and the second ran the board dry against a
        // five-deep queue. Four maximum health for a hundred gold is the cheapest preparation
        // available, and the anomaly is a level 8 fight.
        //
        // **Outside the `anomaly_open` block, unlike the shrine detour above it** — the dev,
        // 2026-08-16: *we should want the healthBuff regardless of anomaly state.* The shrine really
        // is windowed, because consecrating needs `hell ~= 0` (`shrine.lua:93-96`) and cannot be
        // bought early at any price. A heart has no such window, and the fights that *open* the
        // anomaly are the level 6 ones that killed the run this rule came from. See
        // [`WorldMap::wants_a_heart`] for how the misreading survived: the shut state had never been
        // played until an adventure started from a cleared profile.
        //
        // Above the anomaly and above `OpenAnomaly`, so the preparation happens before the fight it
        // is preparation for, in both states.
        //
        // Every remaining clause is the dev's: the gold must be *over* the price, the village must
        // be reachable **without combat** (the same test the shrine detour uses, for the same reason
        // — a detour paid for with a fight is not a detour, it is the fight), and a village whose
        // store we have already emptied is not a destination. `has_heart` is the standing assumption
        // the dev set: every village's general store starts with one.
        if self.wants_a_heart() {
            let shops: Vec<&Place> = self
                .places
                .values()
                .filter(|p| p.key != here && !p.avoid && p.stocks_a_heart())
                .filter(|p| !self.heart_bought.contains(&p.key) && !self.abandoned.contains(&p.key))
                .collect();
            let heart = shops
                .iter()
                .filter(|p| self.reachable_without_a_fight(here, &p.key))
                .min_by_key(|p| dist_or_far(&dist, &p.key));
            if let Some(p) = heart {
                return Some(Plan {
                    target: p.key.clone(),
                    reason: Goal::Heart,
                    steered_by: None,
                });
            }
            // **"I have not looked" is not "there is nothing there", and the difference is a
            // probe.** The dev's correction, 2026-08-15: the navigator's job is to go and find
            // out which nodes are reachable without combat, not to conclude from an incomplete
            // cache that none are.
            //
            // So a shop we cannot route to for free, whose [`Access`] is `Unknown` rather than
            // `Blocked`, sends us to the nearest place with roads we have never looked down —
            // still inside the fight-free region, so the probe costs walking and nothing else.
            // Whatever it reveals lands in the map, and the next pass either routes to the shop
            // or downgrades it to `Blocked` on evidence.
            //
            // Still `Goal::Heart`, because that is what the walk is *for*, and a log that said
            // `Explore` here would hide the errand that chose it.
            if shops
                .iter()
                .any(|p| self.access_without_a_fight(here, &p.key) == Access::Unknown)
            {
                if let Some(p) = self.probe_toward_the_unknown(here, &dist) {
                    return Some(Plan {
                        target: p.key.clone(),
                        reason: Goal::Heart,
                        steered_by: None,
                    });
                }
            }
        }

        // The anomaly is hostile by construction, so `ok` skips it too on the first pass. That is
        // the point rather than an oversight: it is a level 8 fight, and walking into it below half
        // health is the single most expensive thing this run can do. On the first pass we would
        // rather go exploring — which is also how an unknown rest site gets found — and the second
        // pass takes it when there is genuinely nothing else.
        // **And [`SHRINES_BEFORE_THE_ANOMALY`] consecrations before it**, the dev's rule of 2026-08-20 and the
        // reason this branch can now decline.
        //
        // A consecration is bought with walking and pays in gold- and silver-bordered wildcards
        // (`utils/blessings.lua:95-110`); the anomaly is a level 8 fight that the run of that
        // morning reached and lost at turn 24. So the shrines are not a reward to collect on the
        // way, they are the preparation the fight needs, and taking the portal early spends a
        // character that was never made ready.
        //
        // Declining here drops through to `OpenAnomaly` and then to exploring — *which is exactly
        // what the dev asked for*: "we need to keep exploring if the anomaly opens before we've
        // revealed 4 shrines." The shrine branch above this one has already had its chance and is
        // reached first, so a shrine we can reach is taken in preference to more exploring.
        //
        // **The release is at the foot of this function, not here.** A gate with no release is a
        // stall: a world that places too few shrines, or puts them behind ground we cannot
        // cross, would leave the run walking a frontier it has already exhausted for ever. See
        // there for why it is the last thing tried rather than a clause in this condition.
        if self.consecrations() >= SHRINES_BEFORE_THE_ANOMALY {
            if let Some(p) = self.anomaly().filter(|p| ok(p)) {
                return Some(Plan { target: p.key.clone(), reason: Goal::CloseAnomaly, steered_by: None });
            }
        }



        // Opening the anomaly comes BEFORE shrines, and not only because it is the objective:
        // **consecration is impossible until it is open.** `showConsecrateButton`
        // (`shrine.lua:93-96`) needs `hell ~= 0`, so a run that saved its shrines for first would
        // arrive at every one of them unable to finish it.
        //
        // Only worth aiming at while the trigger is unspent — `anomaly_is_open` is `None` before any
        // save has been read, and an unknown flag is read as not open, since a wasted trip is
        // cheaper than stranding the run.
        if !self.anomaly_is_open().unwrap_or(false) {
            // **Everything under the trigger's level first, and the trigger only when nothing else
            // is left.**
            //
            // The dev, 2026-08-16, after a run walked out of a village straight into a level 7 crypt
            // and died there: *we don't have the warrior's gear to carry us through difficult
            // crypts, so instead of beelining for the nearest level 4 node, I now want the
            // pre-anomaly navigator to visit every node that is lower than level 4, so that we only
            // visit a level 4 node when the entire frontier is at least level 4.*
            //
            // Opening the portal needs a win at a node **above level 3**
            // (`crate::subworld::triggers_anomaly`, and `world_evil.lua:15-21`), so the trigger is
            // by definition the hardest thing on the board at that moment. Taking the nearest one
            // the moment it appears spends a fresh character on the worst fight available while
            // easier ground — gold, gear, hearts, and the map itself — sits unwalked beside it.
            //
            // A frontier node with no level at all counts as under the bar: no level means no combat
            // (`Place::has_combat`), so it is free to visit and cannot be the thing we are avoiding.
            //
            // This does not *refuse* the trigger, it queues it. When every frontier is level 4 or
            // above, the branch below runs exactly as it did.
            // Suppression rather than pre-emption. An earlier cut returned the gentle frontier here
            // and labelled it `Explore`, which stole the target from whichever branch below would
            // have claimed it — a free shrine came back as exploring, and eight tests said so. The
            // ladder is left to answer; this only declines to answer *with the trigger*.
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
            if let Some(p) = candidates.first().filter(|_| !self.gentler_ground_remains(here, &ok)) {
                return Some(Plan { target: p.key.clone(), reason: Goal::OpenAnomaly, steered_by: None });
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
        // [`Goal::CloseAnomaly`] outranks this branch, and once it is finished there is no path left. So
        // a corrupted shrine **whose fight is still owed** is never a *destination* — passing through
        // one is judged on arrival, by [`WorldMap::worth_consecrating_here`]. One already cleared is a
        // destination like any other, since nothing is left to avoid.
        // `anomaly_open`, `dist` and the shrine pick itself are all hoisted above the anomaly branch
        // now, since the open-portal case has to choose a shrine before that branch runs.
        //
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
        // Reached only with the portal **shut** — the open case is handled above the anomaly branch,
        // where it now outranks it. Same selection, so it is the same closure rather than a second
        // copy of the filter chain that could drift from this one.
        if let Some(p) = pick_shrine() {
            return Some(Plan { target: p.key.clone(), reason: Goal::Shrine, steered_by: None });
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
        let gentle_only = self.gentler_ground_remains(here, &ok);
        let mut frontier: Vec<&Place> = self
            .places
            .values()
            .filter(|p| p.key != here && !p.visited && !p.avoid)
            .filter(|p| !self.abandoned.contains(&p.key))
            // Nothing to find there, so it is not a destination. It stays a waypoint: routing runs
            // over the edges, and this only decides what is worth walking *to*.
            .filter(|p| !p.nothing_left_to_reveal())
            .filter(|p| ok(p))
            // **The same bar exploring is asked to respect.** Suppressing the trigger branch alone
            // would only move the problem: the frontier below it holds the very same level 4+ nodes,
            // and `Explore` would walk onto one anyway. So while gentler ground remains, it is the
            // only ground exploring considers. See [`WorldMap::gentler_ground_remains`].
            .filter(|p| !gentle_only || !p.triggers_anomaly())
            .collect();
        // Order matters, and a live run showed why. From l10 with l1 and l18 both adjacent and both
        // unvisited, sorting by key chose `l1` — already **completed**, so it revealed nothing — and
        // the run then had to walk back through l10 to reach l18. Two of three hops wasted, on an
        // objective that is explicitly about speed.
        //
        // So: unvisited first, then **unfinished**, then nearest, then whatever has most left to
        // give.
        //
        // ## Why `completed` outranks distance, which it did not used to
        //
        // `visited` is set only by standing somewhere *this run* — nothing seeds it from the save.
        // So on a resume it is uniformly false, the first key ties for every place on the map, and
        // distance decides everything. A run that picks up an in-progress save therefore prefers
        // near ground it has already cleared to fresh ground slightly further off, and re-walks its
        // own footprints.
        //
        // `completed` is the half we do load (`overworld.completedAreas`), and in a subworld it is a
        // good proxy for having been there: peaceful subnodes carry
        // `competeOnVisit = subnodeIsPeaceful` (`forest.lua:107-130`), so arriving finishes them.
        // Promoting it puts the resume case back on the ordering the fresh case always had.
        //
        // Ranked, not filtered. On a resume we know completed places **by key with no edges**, so
        // excluding them outright could empty the frontier in a subworld we have partly cleared.
        // Preferring the unfinished keeps the cleared ground available as a fallback, which costs
        // nothing when there is anything better and saves the run when there is not.
        let far = usize::MAX;
        let dist = self.distances(here);
        // **Where we would be going if the map were connected**, and only asked here, on the last
        // branch, where it is the only thing left that can use it. It is the errand this pass has
        // just given up on for want of a route.
        //
        // Without it the fix trades one failure for a quieter one. The old behaviour aimed at the
        // unreachable target and let `next_hop`'s fallback step the way it lay, which at least
        // pointed somewhere; refusing to name the target at all would leave exploration with no
        // direction and no memory of what it was for.
        //
        // The recursion terminates at one level: the inner call reaches this line too, takes the
        // `false` arm, and asks nothing further.
        // The whole plan is kept, not just its target: the goal it names is what this hop is *for*,
        // and dropping it is what left every such hop labelled `Explore` — indistinguishable from a
        // run with nowhere to be.
        let declined: Option<Plan> = match need_route {
            true => self.plan(skip_hostile, false).filter(|p| !self.can_route_to(&p.target)),
            false => None,
        };
        let bearing = declined.as_ref().map(|p| p.target.clone());
        // A direction heuristic, and it used to be argued *against* here on the grounds that this
        // branch is reached only when there is nothing above to head toward. Route-gating made that
        // false: the branches above can now yield while knowing perfectly well where they wanted to
        // go, having only failed to find a way. `bearing` is that place, and this is the same
        // potential function `next_hop`'s fallback uses, applied one level up where it can pick
        // among *all* the frontiers rather than only the adjacent ones.
        //
        // Below distance on purpose. Hops are what a run actually spends; direction breaks the ties,
        // which on a surface where several neighbours sit one step away is most of the choices there
        // are. `None` for every candidate — no bearing, or a target no dump has placed — leaves the
        // key equal throughout and the ordering exactly as it was.
        let toward = |p: &Place| -> f64 {
            bearing.as_ref().and_then(|t| self.gap(&p.key, t)).unwrap_or(f64::MAX)
        };
        // **Direction outranks distance once the portal is live** — because that is where the
        // objective is. Not because it is safer, which is what this comment used to say.
        //
        // ## The safety argument was backwards, and the generator says so
        //
        // The claim here was "node level scales with distance from the origin, so exploring away
        // from the corruption walks into progressively worse fights". That holds only while the
        // portal is **shut** — which is precisely when this branch does not steer. Opening it runs
        // `overworld/generators/world.lua:496-501` over every corrupted location:
        //
        // ```lua
        //   location.level = math.max(3, location.baseLevel, 7-location.baseLevel)
        // ```
        //
        // `7-baseLevel` is an inversion, and it bites hardest exactly where `baseLevel` is lowest —
        // the nodes nearest the origin. A level 1 neighbour of the portal becomes 6; a level 2
        // becomes 5; `start` itself is set to 8 (`:507`). The level map stops being a bowl and
        // becomes a **U**: high at the rim, high at the core, a floor of 3 in the band between.
        //
        // So steering inward is steering *into* the worse fights, not away from them. It is still
        // right, for the only reason that survives: the anomaly is at the origin and finishing it is
        // the run. What does not survive is the idea that the heuristic protects us — a run that
        // aims at the corruption should expect the fights to get harder as it closes, and should
        // arrive healthy rather than assume the approach is the safe part.
        //
        // The death this comment cited is still real and still argues for steering: live 2026-08-10,
        // with no bearing at all, "nearest unvisited" drifted outward from `l19` to `l28` to `l49`,
        // a level 6 crypt, and the run died there. Both rims are dangerous. Only one of them has the
        // objective on it.
        //
        // Below distance is where this used to sit, on the argument that hops are what a run
        // actually spends. That holds while the anomaly is shut, when exploring is genuinely about
        // growing the map and one direction is as good as another. It stops holding the moment
        // there is somewhere to be.
        // **A bearing is a direction, not a name**, and this used to switch on the name.
        //
        // `bearing.is_some()` says only that somewhere was wanted. Whether it can *order* the
        // frontier is a different question: `toward` needs `gap` to place both ends, and it returns
        // `None` — leaving every candidate at `f64::MAX` and the comparison equal throughout —
        // whenever the bearing has no position. The anomaly's own position is estimated from the
        // centroid of corrupted nodes that are *already placed* (see [`WorldMap::pos_for`]), so a map
        // whose corrupted nodes came from save flags rather than from dumps yields nothing to aim at.
        //
        // In that state the old flag read `true` while the term did nothing at all, and the run
        // sorted by hops — nearest-unvisited — which is what it would have done with no steering
        // written. Live 2026-08-12 that walked out of the corruption and into a bandit camp.
        //
        // A bearing that lands on the node we are standing on is the same failure wearing a
        // different hat: every candidate is measured against a point already reached. `placed`
        // counting only *finite* distances is what excludes both, because `gap` to an unplaced
        // bearing and `gap` from an unplaced candidate are both `None`.
        //
        // Compared against the sentinel rather than with `is_finite`, which is the trap here:
        // `toward` substitutes `f64::MAX` for an unmeasurable candidate, and `f64::MAX` *is* finite.
        // The first version of this counted every candidate as placed and reported steering on a map
        // with nothing to aim at — the exact bug it was written to catch.
        let placed = frontier.iter().filter(|p| toward(p) < f64::MAX).count();
        let steer = self.anomaly_is_open().unwrap_or(false) && placed > 0;
        let steered_by =
            steer.then(|| (bearing.clone().unwrap_or_default(), placed, frontier.len()));
        frontier.sort_by(|a, b| {
            let by_hops =
                dist.get(&a.key).unwrap_or(&far).cmp(dist.get(&b.key).unwrap_or(&far));
            let by_bearing = toward(a).total_cmp(&toward(b));
            a.visited
                .cmp(&b.visited)
                .then(a.completed.cmp(&b.completed))
                .then(match steer {
                    true => by_bearing.then(by_hops),
                    false => by_hops.then(by_bearing),
                })
                .then(b.connections.cmp(&a.connections))
                .then(b.hidden.unwrap_or(0).cmp(&a.hidden.unwrap_or(0)))
                .then(a.key.cmp(&b.key))
        });
        // **What this hop is for**, which is a different question from whether it can be aimed.
        // `RouteTo` whenever an errand was declined for want of a route, steered or not; plain
        // `Explore` only when nothing was wanted in the first place.
        let reason = match declined {
            Some(p) => Goal::RouteTo(Box::new(p.reason)),
            None => Goal::Explore,
        };
        frontier
            .first()
            .map(|p| Plan { target: p.key.clone(), reason: reason.clone(), steered_by: steered_by.clone() })
            // **The release for [`SHRINES_BEFORE_THE_ANOMALY`].**
            //
            // Reached only when every branch above declined *and* there is no frontier left to
            // walk — so the run has consecrated what it could, explored what it could, and the bar
            // is still unmet. Nothing further will change that: exploring is the only thing that
            // reveals a new shrine, and it has just run out.
            //
            // Placed here rather than as a clause on the gate so that it cannot fire early. Written
            // into the condition it would have to reproduce "is there anything else at all worth
            // doing", which is precisely what the rest of this function computes; asking it *after*
            // the answer is known is the only version that stays true as branches are added.
            //
            // The run then walks into the anomaly under-prepared and probably dies, which is the
            // honest ending. A plan it can never satisfy is not the safer alternative — it is the
            // loop guard ending the run four laps later with nothing learned.
            .or_else(|| {
                self.anomaly()
                    .filter(|p| ok(p))
                    .map(|p| Plan { target: p.key.clone(), reason: Goal::CloseAnomaly, steered_by: None })
            })
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
            return Some(Hop { step, plan, routed: true });
        }
        if let Some(step) = self.first_step_toward(here, &plan.target, false) {
            return Some(Hop { step, plan, routed: true });
        }
        // No known route. Step to an adjacent frontier instead: mapping outward is what will
        // eventually connect us to the target.
        //
        // The plan is carried through unchanged. Replacing it with the frontier node would make the
        // goal follow the footsteps — every hop would re-derive a nearby target and the run would
        // wander instead of working toward the trigger.
        //
        // **And this is where a direction is worth having.** We know where we are going and have no
        // way to get there, so the graph has nothing to say and the choice used to come down to the
        // lowest key — a run heading for the anomaly stepping whichever way the alphabet pointed.
        //
        // [`WorldMap::gap`] answers it whenever the target has been placed by some dump, which is
        // the one thing a route needs and a direction does not: the target may sit in a component we
        // cannot reach and its position is still true. This is the potential function
        // `docs/superpowers/notes/navigation-loops.md` says every navigation bug here came down to
        // lacking, and this is the branch that most needed one.
        //
        // Its limit is worth stating next to it. A place no dump has ever shown us has **no position
        // either**, so a target learned only from `completedAreas` — which is how the anomaly key
        // arrives on a fresh launch — gets `None` here and the ordering falls back to what it always
        // did. Coordinates speak about places we have seen, never about places we have merely heard
        // of. Getting direction on the anomaly before meeting it would need a different source.
        let me = self.places.get(here)?;
        let mut options: Vec<&Place> = me
            .neighbours
            .iter()
            .filter_map(|k| self.places.get(k))
            .filter(|p| p.is_frontier())
            .collect();
        let toward = |p: &Place| -> i64 {
            self.gap(&p.key, &plan.target).map(|d| d as i64).unwrap_or(i64::MAX)
        };
        // Avoided places sort last rather than out, so they remain a last resort.
        options.sort_by(|a, b| {
            a.avoid
                .cmp(&b.avoid)
                .then(a.visited.cmp(&b.visited))
                .then(toward(a).cmp(&toward(b)))
                .then(a.key.cmp(&b.key))
        });
        options.first().map(|p| Hop { step: p.key.clone(), plan, routed: false })
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

    pub fn wants_a_heart(&self) -> bool {
        self.gold >= HEART_FLOOR
    }

    /// How many hearts this purse can take off a shelf, holding a bed back.
    ///
    /// **Settlements stock more than one.** The standing assumption was one each; the dev found it
    /// wrong on 2026-08-15, so the shop empties the shelf instead of taking a single item off it.
    /// Every one still leaves [`crate::rest::INN_COST`] behind — the reserve is a floor under the
    /// whole visit, not a check on the first purchase.
    pub fn hearts_affordable(&self) -> i64 {
        ((self.gold - crate::rest::INN_COST) / HEART_COST).max(0)
    }

    /// Should we stop for a bed at `key`, purely because we are standing at one?
    ///
    /// The dev's rule, 2026-08-15: **at a settlement, below full health, with the inn's price in
    /// pocket — rest.** Deliberately weaker than [`WorldMap::wants_rest`], which waits for half
    /// health or a four-point drop, because those are the bars for making a *detour*. Standing in
    /// the doorway is not a detour: six health for ten gold before the next fight is the cheapest
    /// trade on the board, and this run keeps dying two points short.
    ///
    /// Takes `&mut self` because deciding is also *recording*. Everything that walks us to the inn
    /// from inside — `inn_inside`, `seeking_a_rest`, `cross_toward` — is written around
    /// `wants_rest`, so a top-up that did not set it would be a decision the rest of the map never
    /// heard about. That is the planner/driver split this same commit is fixing elsewhere, and it is
    /// not worth re-introducing one flag lower down.
    ///
    /// `false` when health has never been read: an unknown reading is not evidence of a wound, and
    /// the errand costs a subworld crossing.
    pub fn top_up_at(&mut self, key: &str) -> bool {
        let settlement = self.places.get(key).map(|p| p.has_an_inn()).unwrap_or(false);
        let hurt = self.health.map(|h| !h.is_full()).unwrap_or(false);
        if !(settlement && hurt && self.gold >= crate::rest::INN_COST) {
            return false;
        }
        // **[`WorldMap::top_up`], not `wants_rest`** — task #72, and the whole point of the split.
        //
        // This wrote `wants_rest` until 2026-08-21, which made the doorway rule a detour rule. Live
        // in the 1519Z run: health 77/80, a three-point drop that neither `note_health`
        // (needs four) nor `note_health_level` (needs half) can act on, and the plan came out
        // `Rest -> l63` — a surface hop taken to heal three points for ten gold. The dev: *why did
        // we rest at Treasured Balsa despite barely having any missing health?*
        //
        // It also fired for settlements the caller then refused: `navigate.rs` computes this
        // *before* testing `trades()` and corruption, so a village under attack set the flag on the
        // way past, the top-up was declined, and the detour outlived it.
        self.top_up = true;
        true
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
        if self.inside().is_some() {
            return None;
        }
        self.far_chain(from, to, &|p: &Place| self.worth_consecrating_here(&p.key))
    }

    /// Walks our own route to `to`, as far as the game would carry us in one press.
    ///
    /// Shared by [`WorldMap::far_hop`] and [`WorldMap::far_hop_inside`], which differ only in
    /// `stop_at` — the question "is this intermediate node one we must not stride past?". Keeping one
    /// walker means the two can never disagree about the *route*, only about where to get off it.
    fn far_chain(
        &self, from: &str, to: &str, stop_at: &dyn Fn(&Place) -> bool,
    ) -> Option<String> {
        if from == to {
            return None;
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
        reach
            .iter()
            .rev()
            .find(|k| self.shortest_paths_are_gentle(from, k, k.as_str() == to))
            // One hop is not a multi-hop. Anything the caller could have worked out itself is `None`.
            .filter(|k| !self.can_step_is_adjacent(from, k))
            .cloned()
    }

    /// Distance in hops from `origin` to every node our own edges can reach.
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

    /// Could the game route us through a level 4+ node on the way to `to`?
    ///
    /// **Every** shortest path is checked, not the one we would have walked, and that is forced by
    /// how the game chooses. `canTravelToIndirect` (`overworldview.lua:1330-1373`) is a
    /// breadth-first search — `toCheck` is popped from the back (`table.remove(toCheck)`) and pushed
    /// at the front (`table.insert(toCheck, 1, location2)`), so a ring is exhausted before the next
    /// begins — and `pathHash[key]` is written only `if not pathHash[key]`, so the predecessor that
    /// sticks is the shallowest one. The path handed to `travelTo` is therefore a **shortest** path.
    ///
    /// Which one, we cannot know: ties are broken by `pairs(location.connections)`, whose order Lua
    /// does not define. So the only safe question is whether *all* of them are gentle, and that is
    /// the dev's rule of 2026-08-17.
    ///
    /// The standard characterisation, over the edges we have recorded: `n` lies on some shortest
    /// path exactly when `d(from, n) + d(n, to) == d(from, to)`. Two breadth-first sweeps answer it
    /// for every node at once.
    ///
    /// `from` is exempt because we are already standing on it. `to` is exempt only when it is the
    /// errand's own destination — the planner chose that and has its own rules for choosing it — and
    /// **not** when it is an intermediate node this chain picked, because landing there is then our
    /// doing and nobody else's.
    ///
    /// ## What this deliberately does not do, and it is worth doing later
    ///
    /// A shortest path with a level 6 crypt on it blocks the hop outright, even when a slightly
    /// longer route round the crypt exists and the game would happily walk it if asked. The fix is
    /// to aim at an intermediate node that forces the detour, and then hop again — the dev's own
    /// suggestion, parked as post-MVP. Until then this is conservative in the direction of taking an
    /// ordinary step, which is what the run did before fast hops existed.
    fn shortest_paths_are_gentle(&self, from: &str, to: &str, exempt_end: bool) -> bool {
        let out = self.hops_from(from);
        let back = self.hops_from(to);
        let Some(&total) = out.get(to) else { return false };
        for (key, &d) in &out {
            if back.get(key).map(|b| d + b) != Some(total) {
                continue; // not on any shortest path
            }
            if key == from || (exempt_end && key == to) {
                continue;
            }
            // An unknown node counts as dangerous: we cannot read a level off a heading we do not
            // have, and a hop is an optimisation — declining one costs a press, taking a bad one can
            // cost the run.
            let gentle = self.places.get(key).map(|p| p.level().unwrap_or(0) <= 3).unwrap_or(false);
            if !gentle {
                return false;
            }
        }
        true
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

    /// **A corrupted shrine becomes a target exactly when the clean ones cannot reach the bar.**
    /// Task #74.
    ///
    /// The dev, 2026-08-21: *a corrupted shrine should become a target when it is the last to
    /// satisfy the requirement. That also means that if two shrines are in the corruption, then they
    /// become targets after two consecrations of uncorrupted shrines. Generalize this to any number
    /// of corrupted shrines.*
    ///
    /// Both sentences describe the same condition — **the clean candidates have run out and the bar
    /// is still short** — and the second is what rules out the other reading. "We can already see
    /// we will need them" would admit a corrupted shrine at nought consecrations whenever the clean
    /// ones are too few, and the dev put the moment at *after two consecrations*: take the free ones
    /// first, always. That ordering is not a nicety. A corrupted shrine costs a fight that can end
    /// the run, and a free consecration banked before it is a free consecration kept.
    #[test]
    fn a_corrupted_shrine_is_a_target_only_once_the_clean_ones_run_out() {
        // `clean` uncorrupted and `foul` corrupted, all reachable, portal open.
        let world = |clean: usize, foul: usize| {
            let mut m = WorldMap::new();
            let mut names: Vec<String> = Vec::new();
            for i in 0..clean + foul {
                names.push(format!("shrine{}", i + 1));
            }
            m.fold(&dump(
                "here",
                "camp",
                names.iter().map(|k| node(k, "Gransmoor shrine")).collect(),
            ));
            for (i, k) in names.iter().enumerate() {
                m.entry(k).corrupted = i >= clean;
            }
            m.here = Some("here".into());
            m.hell = Some(0.1);
            m
        };
        let consecrate = |m: &mut WorldMap, n: usize| {
            for i in 0..n {
                let p = m.entry(&format!("shrine{}", i + 1));
                p.consecrated = true;
                p.used = true;
            }
        };
        let target = |m: &WorldMap| m.next_target().map(|p| (p.reason, p.target));

        // **The dev's second sentence**, at the bar's own value: two clean, the rest corrupted.
        // Before the clean ones are spent, a corrupted shrine is not a destination.
        let mut m = world(2, 2);
        assert_eq!(
            target(&m),
            Some((Goal::Shrine, "shrine1".into())),
            "the clean one first, even though a corrupted one is exactly as near"
        );
        consecrate(&mut m, 1);
        assert_eq!(target(&m), Some((Goal::Shrine, "shrine2".into())), "and the second clean one");

        // Both clean ones are spent and the bar is still short: now they are targets.
        consecrate(&mut m, 2);
        assert!(m.consecrations() < SHRINES_BEFORE_THE_ANOMALY, "the premise: still short");
        assert_eq!(
            target(&m),
            Some((Goal::Shrine, "shrine3".into())),
            "nothing clean is left and the bar is unmet, so the fight is worth it"
        );

        // **Generalised.** Plenty of clean shrines and the corrupted ones are never wanted: the bar
        // is met before they are reached, and `CloseAnomaly` takes over.
        let mut m = world(SHRINES_BEFORE_THE_ANOMALY, 3);
        consecrate(&mut m, SHRINES_BEFORE_THE_ANOMALY);
        m.entry("start").heading = "The Rift — level 8 anomaly".into();
        m.entry("start").corrupted = true;
        m.fold(&dump("here", "camp", vec![node("start", "The Rift — level 8 anomaly")]));
        assert_eq!(
            target(&m).map(|(r, _)| r),
            Some(Goal::CloseAnomaly),
            "the bar is met, so a corrupted shrine is not a detour on the way to the portal"
        );

        // **A corrupted shrine that cannot advance the bar is never bought.** `is_shrine` answers by
        // heading too, so `s1` is a candidate; `can_be_consecrated` reads the key, so it can never
        // be consecrated and no number of them satisfies the gate. Found by an older fixture rather
        // than by this one, which was too tidy — every shrine in it is keyed `shrineN`.
        let mut m = world(0, 0);
        m.fold(&dump("here", "camp", vec![node("s1", "Faraway shrine")]));
        m.entry("s1").corrupted = true;
        assert!(m.corrupted_shrines_are_needed(), "the exception is open");
        assert_ne!(target(&m).map(|(r, _)| r), Some(Goal::Shrine), "and it still does not apply");

        // And with the bar met, a corrupted shrine stays off the list even with clean ones gone —
        // which is the half that stops this from becoming the 2026-08-15 shrine-chase again.
        let mut m = world(SHRINES_BEFORE_THE_ANOMALY, 2);
        consecrate(&mut m, SHRINES_BEFORE_THE_ANOMALY);
        assert_ne!(
            target(&m).map(|(_, t)| t),
            Some(format!("shrine{}", SHRINES_BEFORE_THE_ANOMALY + 1)),
            "no corrupted shrine is wanted once the requirement is satisfied"
        );
    }

    /// **The three changes of 2026-08-21 that share `pick_shrine`, exercised together.**
    ///
    /// Written as an interaction check rather than for a fault: #70 removed the fight-free route
    /// filter, #74 admitted corrupted shrines once the clean supply is spent, and the bar dropped to
    /// three, all on the same afternoon and none of it run. Each has its own test; this asks what
    /// they do *at once*, which is the case a live run will actually present.
    ///
    /// The combination is deliberately the most permissive one reachable: portal open, bar short,
    /// nothing clean left, and the only candidate corrupted and behind a level 9 forest. Every guard
    /// that used to refuse it has been removed by one of the three, so if the run can be sent
    /// somewhere it cannot act, this is where.
    #[test]
    fn the_shrine_rules_of_2026_08_21_do_not_combine_into_an_unreachable_target() {
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("l53", "Beeford Hedge — level 9 forest")]));
        m.fold(&dump(
            "l53",
            "Beeford Hedge — level 9 forest",
            vec![node("shrine2", "Gransmoor shrine")],
        ));
        m.here = Some("here".into());
        m.hell = Some(0.1);
        m.entry("shrine2").corrupted = true;

        // The premises, each named so a fixture that drifts says which rule stopped applying.
        assert!(m.corrupted_shrines_are_needed(), "#74: nothing clean and the bar is short");
        assert!(
            !m.reachable_without_a_fight("here", "shrine2"),
            "#70: the only way there is a level 9 forest, and that no longer disqualifies it"
        );

        let plan = m.next_target().expect("a plan");
        assert_eq!(plan.reason, Goal::Shrine);
        assert_eq!(plan.target, "shrine2");

        // **The half that matters.** A target the driver cannot set off toward is worse than no
        // target: it is the shape of every loop this project has had. `next_hop` is what the step
        // actually asks, so ask it.
        let hop = m.next_hop().expect("a hop toward the shrine we just nominated");
        assert_eq!(hop.plan.target, "shrine2");
        assert_eq!(hop.step, "l53", "through the forest, which is the fight we agreed to pay");

        // And with the bar met, the same map must stop offering it — the guard that keeps #74 from
        // becoming the shrine-chase of 2026-08-15.
        ready_for_the_anomaly(&mut m);
        assert!(!m.corrupted_shrines_are_needed());
        assert_ne!(
            m.next_target().map(|p| p.reason),
            Some(Goal::Shrine),
            "satisfied, so a corrupted shrine behind a level 9 forest is a detour again"
        );
    }

    /// **The portal waits for [`SHRINES_BEFORE_THE_ANOMALY`]**, and what the run does while short.
    ///
    /// The dev, 2026-08-20, after the run that reached the anomaly and died at turn 24: *before the
    /// anomaly fight, ensure that at least 4 major shrines have been consecrated. This means that we
    /// need to keep exploring if the anomaly opens before we've revealed 4 shrines.* Lowered to
    /// three on 2026-08-21.
    ///
    /// **Written against the constant, not against a number.** It named four in its title and
    /// counted to three by hand, so lowering the bar broke it in a way that read as a regression —
    /// `assertion left != right failed: CloseAnomaly, CloseAnomaly`, which says nothing about the
    /// bar at all. Off-by-one is the point of a bar, so the off-by-one is now derived from it.
    #[test]
    fn the_portal_waits_for_its_consecrations_and_we_explore_until_then() {
        let build = || {
            let mut m = WorldMap::new();
            m.fold(&dump(
                "here",
                "camp",
                vec![node("rift", "The Rift anomaly"), node("l2", "Bainton Clump road")],
            ));
            m.here = Some("here".into());
            m.hell = Some(0.1);
            m
        };

        // Nothing consecrated: the portal is on the map, reachable, and still not the errand.
        let m = build();
        assert_eq!(m.consecrations(), 0);
        assert!(m.anomaly().is_some(), "the portal really is there to be chosen");
        let plan = m.next_target().expect("a plan");
        assert_ne!(plan.reason, Goal::CloseAnomaly, "under the bar, the portal waits");
        assert_eq!(plan.reason, Goal::Explore, "and exploring is what the dev asked for instead");

        // One short is still short. Off by one is the whole point of a bar.
        let mut m = build();
        let short = SHRINES_BEFORE_THE_ANOMALY - 1;
        for i in 1..=short {
            let p = m.entry(&format!("shrine{i}"));
            p.consecrated = true;
            p.used = true;
        }
        assert_eq!(m.consecrations(), short);
        assert_ne!(m.next_target().unwrap().reason, Goal::CloseAnomaly);

        // The last one opens the gate.
        let mut m = build();
        ready_for_the_anomaly(&mut m);
        assert_eq!(m.consecrations(), SHRINES_BEFORE_THE_ANOMALY);
        let plan = m.next_target().expect("a plan");
        assert_eq!(plan.reason, Goal::CloseAnomaly);
        assert_eq!(plan.target, "rift");
    }

    /// **The release**, without which the gate is a stall rather than a rule.
    ///
    /// A world that never yields enough shrines — too few placed, or all of them behind ground we
    /// cannot cross — must still end its run at the anomaly rather than walking an exhausted
    /// frontier for ever. That is the case the loop guard would otherwise have to end.
    #[test]
    fn with_nothing_left_to_explore_the_portal_is_taken_anyway() {
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("rift", "The Rift anomaly")]));
        m.here = Some("here".into());
        m.hell = Some(0.1);
        assert_eq!(m.consecrations(), 0, "still under the bar, and nothing will change that");

        // Exhaust the frontier: everywhere is visited and nowhere is hiding a neighbour, so
        // exploring has nothing left to offer.
        for key in ["here", "rift"] {
            let p = m.entry(key);
            p.visited = true;
            p.hidden = Some(0);
            p.connections = 1;
        }

        let plan = m.next_target().expect("a plan even so");
        assert_eq!(
            plan.reason,
            Goal::CloseAnomaly,
            "nothing left to prepare with, so the run goes and dies honestly"
        );
        assert_eq!(plan.target, "rift");
    }

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

    /// **Bank Well-Rested stacks before a deliberate deep fight**, and every clause of the dev's
    /// rule of 2026-08-20.
    #[test]
    fn a_deep_fight_sends_us_to_an_inn_for_stacks_first() {
        let build = || {
            let mut m = WorldMap::new();
            m.fold(&dump(
                "here",
                "camp",
                vec![
                    node("rift", "The Rift — level 8 anomaly"),
                    node("l11", "Rowlston Covert village"),
                ],
            ));
            m.here = Some("here".into());
            m.hell = Some(0.1);
            m.gold = HEART_FLOOR - 1;
            // **The shelf is bare, and that is what isolates this test.** A village with a heart
            // still on it does two things at once: `Goal::Heart` outranks the portal, and the
            // heart's reserve stops the bank spending. Both are the dev's ruling of 2026-08-20 and
            // both are pinned in `the_heart_outranks_the_bank_for_the_same_purse`. Here the
            // question is what the *portal* does to the plan, so the heart is taken off the board.
            m.bought_the_heart("l11");
            ready_for_the_anomaly(&mut m);
            m
        };

        // Spent out, which is how the run of 2026-08-20 arrived at the portal.
        let m = build();
        assert_eq!(m.stacks_short_ahead(), 16, "twice the level 8 anomaly");
        let plan = m.next_target().expect("a plan");
        assert_eq!(plan.reason, Goal::StockUp, "the bed comes before the fight");
        assert_eq!(plan.target, "l11", "and it is the village that has one");

        // A bank already deep enough leaves the errand exactly as it was.
        let mut m = build();
        m.well_rested = crate::rest::stacks_wanted(8);
        assert_eq!(m.stacks_short_ahead(), 0);
        assert_eq!(m.next_target().unwrap().reason, Goal::CloseAnomaly, "nothing left to buy");

        // **The dev's floor.** Below the inn's price the requirement is unmeetable, not merely
        // unmet, and holding the run at it would stall instead of preparing.
        let mut m = build();
        m.gold = crate::rest::INN_COST - 1;
        assert_eq!(m.next_target().unwrap().reason, Goal::CloseAnomaly, "broke, so get on with it");

        // Eleven stacks — what a real run reached, in `at-woodland-shrine-unprayed` — is still five
        // short of the anomaly, so the errand still fires.
        let mut m = build();
        m.well_rested = 11;
        assert_eq!(m.stacks_short_ahead(), 5);
        assert_eq!(m.next_target().unwrap().reason, Goal::StockUp);
    }

    /// **Hearts outrank the bank, because they share one purse.** The dev's ruling, 2026-08-20.
    ///
    /// Four maximum health is permanent for the rest of the run; a bank of stacks is spent by one
    /// fight. Left to compete, sixteen stacks at ten gold apiece walk a hundred-gold heart's budget
    /// out of the purse on the way past, and nothing in the ordering noticed — which is the open
    /// question this closes.
    #[test]
    fn the_heart_outranks_the_bank_for_the_same_purse() {
        let build = |gold: i64| {
            let mut m = WorldMap::new();
            m.fold(&dump(
                "here",
                "camp",
                vec![
                    node("rift", "The Rift — level 8 anomaly"),
                    node("l11", "Rowlston Covert village"),
                ],
            ));
            m.here = Some("here".into());
            m.hell = Some(0.1);
            m.gold = gold;
            ready_for_the_anomaly(&mut m);
            m
        };

        // The purse from the live save on 2026-08-20: plenty for both, so both happen — and the
        // heart goes first because it outranks on the ladder.
        let m = build(465);
        assert_eq!(m.stacks_short_ahead(), 16, "the want is unchanged by the reserve");
        assert_eq!(m.stacks_to_buy(), 16, "and 465 less the reserve still covers all of it");
        assert_eq!(m.next_target().unwrap().reason, Goal::Heart, "the heart is the errand");

        // **The case the ruling is about.** Enough for a heart and a bed, and not a coin more: every
        // stack bought here is a heart not bought.
        let m = build(HEART_FLOOR);
        assert_eq!(m.stacks_short_ahead(), 16, "still wanted");
        assert_eq!(m.stacks_to_buy(), 0, "and not one of them may be paid for");
        assert!(!m.wants_a_bed(), "so the inn is not an errand while the shelf is stocked");

        // Ten over the reserve buys exactly one stack, and no more.
        let m = build(HEART_FLOOR + crate::rest::INN_COST);
        assert_eq!(m.stacks_to_buy(), 1);

        // **The reserve is the heart's, so it lifts when the heart is gone.** The same purse that
        // could buy nothing above now buys eleven.
        let mut m = build(HEART_FLOOR);
        m.bought_the_heart("l11");
        assert_eq!(m.stacks_to_buy(), 11, "110 gold is eleven rests once nothing is being saved for");
        assert!(m.wants_a_bed());

        // **Being hurt is not subject to any of this.** The ruling ranks two kinds of preparation;
        // a wound outranks both, and `wants_rest` keeps its own plain ten-gold gate.
        let mut m = build(HEART_FLOOR);
        assert!(!m.wants_a_bed(), "the control: nothing wants a bed at this purse yet");
        m.note_health_level(crate::rest::Health { current: 1, max: 52 });
        assert!(m.wants_rest(), "the fixture must really be hurt");
        assert!(m.wants_a_bed(), "and a hurt run still goes to bed with the heart unbought");
    }

    /// A forest does not hold the run at an inn, however deep it is.
    #[test]
    fn a_level_nine_forest_is_not_worth_banking_for() {
        let mut m = WorldMap::new();
        m.fold(&dump(
            "here",
            "camp",
            vec![
                node("shrine7", "Cottam Boscage — level 9 forest"),
                node("l11", "Rowlston Covert village"),
            ],
        ));
        m.here = Some("here".into());
        m.gold = 500;
        assert_eq!(m.well_rested, 0, "an empty bank, so only the forest rule can be answering");
        assert_eq!(
            m.stacks_short_ahead(),
            0,
            "the forest is walked through, not cleared — the dev's correction"
        );
        assert!(!m.wants_a_bed(), "so nothing sends us to a bed");
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

    /// Knowing the roster does not let the anomaly through the gate.
    ///
    /// Worth its own assertion because the two numbers are one word apart and the dev's phrasing
    /// used "revealed" for what the code counts as consecrated. Seven shrines known and none
    /// consecrated is exactly the state the 2026-08-20 run was in when it walked into the portal.
    #[test]
    fn a_full_roster_with_nothing_consecrated_still_holds_the_portal() {
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("rift", "The Rift anomaly")]));
        m.here = Some("here".into());
        m.apply_save(
            &crate::game::save::parse(
                "return { overworld = { areaFlags = {
                     hell = 0.1,
                     shrine1_explored = 1, shrine2_explored = 1, shrine3_explored = 1,
                     shrine4_explored = 1, shrine5_explored = 1, shrine6_explored = 1,
                     shrine7_explored = 1,
                 } } }",
            )
            .unwrap(),
        );
        assert_eq!(m.shrines_known(), 7);
        assert_eq!(m.consecrations(), 0);
        assert_ne!(
            m.next_target().unwrap().reason,
            Goal::CloseAnomaly,
            "knowing where they are is not having done them"
        );
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

    /// A shrine that would open the anomaly is not an errand while the anomaly is shut.
    ///
    /// **The fixture is the run of 2026-08-17 0436Z**, which is why it is a level 9 forest and not
    /// a round number. The log reads:
    ///
    /// ```text
    /// 17. l55 -> **shrine7** (for shrine7, Shrine)
    /// 18. skipped the anomaly cinematic
    /// 19. fighting `shrine7` (Cottam Boscage — level 9 forest)
    /// ```
    ///
    /// at 52/52 health, with the portal shut when the target was chosen. Both cost filters in
    /// `pick_shrine` are written `!anomaly_open || …`, so before the portal opens nothing priced
    /// the trip at all.
    ///
    /// The level 1 shrine is the control, and it is the half that stops this being "switch the
    /// shrine errand off pre-anomaly". The dev, on the woodland shrine the same day: *triggering a
    /// fight at a shrine is fine.* A fight is fine; ending the pre-anomaly phase to get one is not.
    ///
    /// The rule is [`Place::triggers_anomaly`] alone — see the filter for why `&& !completed` was a
    /// clause about a world that cannot exist, and why this test does not build one.
    #[test]
    fn a_shrine_that_would_open_the_anomaly_is_not_a_pre_anomaly_errand() {
        let mut m = WorldMap::new();
        m.fold(&dump(
            "l55",
            "Emswell campfire",
            vec![
                node("shrine7", "Cottam Boscage — level 9 forest"),
                node("shrine1", "Gripthorpe Brush shrine"),
            ],
        ));
        m.here = Some("l55".into());
        m.hell = Some(0.0);

        assert!(m.get("shrine7").unwrap().triggers_anomaly(), "the premise: level 9 opens it");
        assert!(!m.get("shrine1").unwrap().triggers_anomaly(), "and a plain shrine does not");

        // The gentle shrine is the errand, so the branch is alive and both are reachable.
        let plan = m.next_target().expect("a plan");
        assert_eq!(plan.reason, Goal::Shrine);
        assert_eq!(plan.target, "shrine1", "the shrine that costs nothing to reach");

        // Take it away and the level 9 one must not inherit the errand.
        m.entry("shrine1").used = true;
        if let Some(plan) = m.next_target() {
            assert_ne!(
                plan.target, "shrine7",
                "walking here opens the portal, which is the phase we are still in: {plan:?}"
            );
        }

        // **No `completed` case here, deliberately.** The obvious third assertion — clear `shrine7`
        // and watch it stop being excluded — is a green test in a world that cannot exist: while
        // `hell == 0` a level 4+ node is uncompleted without exception, because clearing it needs
        // arriving and arriving opens the portal. That is precisely the mistake the note on
        // [`Place::triggers_anomaly`] was kept to record, and it was written here anyway on the
        // first attempt before the dev caught it.
    }

    /// With the portal open, a prayed-at **minor** shrine must stop being a destination.
    ///
    /// The pairing this pins is stated at `worth_a_trip` itself: the goal filter and
    /// [`WorldMap::worth_consecrating_here`] must agree, and this project has now watched them drift
    /// apart twice in opposite directions. The second time was self-inflicted on 2026-08-17 —
    /// `can_be_consecrated` went into the arrival test alone, leaving the goal filter proposing a
    /// trip that arrival would always decline.
    ///
    /// A minor shrine can never take `_consecrated`, so `!p.consecrated` is true for ever; without
    /// the gate this is not a bounce that ends, it is one that cannot.
    ///
    /// The major shrine in the same fixture is the control. It differs only in its key, and it *is*
    /// still a destination — so this cannot pass by the shrine branch having switched itself off.
    #[test]
    fn an_open_portal_does_not_make_a_prayed_minor_shrine_a_destination_for_ever() {
        let mut m = WorldMap::new();
        m.fold(&dump(
            "l10",
            "Trenwick — level 1 crypt",
            vec![node("shrine1", "Swanland shrine"), node("l4sub9", "Bainton Clump woodland shrine")],
        ));
        m.here = Some("l10".into());
        m.hell = Some(0.1);
        for k in ["shrine1", "l4sub9"] {
            m.entry(k).completed = true;
            m.entry(k).visited = true;
            // Prayed at, so the only thing either could still owe is a consecration.
            m.entry(k).used = true;
        }

        // The major shrine still owes one, so the branch is alive and the fixture is routable.
        let plan = m.next_target().expect("a plan");
        assert_eq!(plan.reason, Goal::Shrine, "the major shrine is genuinely unconsecrated");
        assert_eq!(plan.target, "shrine1");

        // Retire it, and nothing may fall through to the woodland shrine.
        m.entry("shrine1").consecrated = true;
        if let Some(plan) = m.next_target() {
            assert_ne!(
                plan.reason,
                Goal::Shrine,
                "a minor shrine can never be consecrated, so this errand could never end: {plan:?}"
            );
            assert_ne!(plan.target, "l4sub9", "and it must not be the target under any goal");
        }
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
    fn routing_prefers_a_trigger_node_over_plain_exploration() {
        let mut m = WorldMap::new();
        m.fold(&dump(
            "start",
            "camp",
            vec![node("l4", "Grim Barrow — level 4 crypt"), node("l2", "Quiet Glade meadow")],
        ));

        // **The dev's rule of 2026-08-16 reversed what this used to assert.** Opening the portal
        // needs a win above level 3, so the trigger is the hardest fight available; a meadow with no
        // level is free. The meadow goes first, and `l4` is not even offered as somewhere to
        // explore while it stands.
        let plan = m.next_target().unwrap();
        assert_eq!(plan.target, "l2", "the gentle ground before the level 4 crypt");
        assert_ne!(plan.reason, Goal::OpenAnomaly);

        // With the meadow walked and nothing left under the bar, the trigger is the plan again.
        m.entry("l2").visited = true;
        m.entry("l2").hidden = Some(0);
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::OpenAnomaly);
        assert_eq!(plan.target, "l4");
    }

    #[test]
    fn once_the_anomaly_has_opened_the_trigger_is_no_longer_a_goal() {
        let mut m = WorldMap::new();
        // Standing on `start`, which the anomaly promoted to the portal under us.
        m.fold(&dump("start", "camp", vec![node("l4", "Grim Barrow — level 4 crypt")]));
        m.hell = Some(0.1); // the anomaly already opened
        let plan = m.next_target().unwrap();
        assert_ne!(plan.reason, Goal::OpenAnomaly, "chasing a spent trigger wastes the run");
    }

    #[test]
    fn the_anomaly_is_known_before_any_dump_names_it() {
        // `start` is promoted to the portal in place, so once `hell` is nonzero we know where to go
        // without exploring for it -- and after corruption, `start` is usually "(unheaded)" in our
        // map because we only heard of it through save flags.
        let mut m = WorldMap::new();
        ready_for_the_anomaly(&mut m);
        m.fold(&dump("l39", "Eight Timberland — level 4 forest", vec![node("l29", "Rookdale — level 3 crypt")]));
        m.entry("start").corrupted = true;
        m.hell = Some(0.1);
        assert_eq!(m.anomaly().map(|p| p.key.as_str()), Some("start"));
        assert!(m.anomaly_is_assumed(), "no heading has confirmed it yet");

        // **Knowing where it is and being able to go there are different things**, and the errand
        // follows the second. Heard of through a save flag, `start` has no edges at all, so naming
        // it as the target would only have handed the move to `next_hop`'s fallback while the log
        // claimed an objective. Explore, which is what mends the gap.
        assert_eq!(
            m.next_target().unwrap().reason,
            Goal::RouteTo(Box::new(Goal::CloseAnomaly)),
            "no route, so no nomination — but the errand it stands for is still on the record"
        );

        // Learn one edge to it and it becomes the errand, with nothing else about the map changed.
        m.fold(&dump("l29", "Rookdale — level 3 crypt", vec![node("start", "")]));
        m.here = Some("l39".into());
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::CloseAnomaly);
        assert_eq!(plan.target, "start");
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
        ready_for_the_anomaly(&mut m);
        m.fold(&dump(
            "start",
            "camp",
            vec![node("l4", "Grim Barrow — level 4 crypt"), node("hp", "The Rift anomaly")],
        ));
        // There is no portal to go to until the anomaly is actually open.
        assert!(m.anomaly().is_none());
        m.hell = Some(0.1);
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::CloseAnomaly);
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
        assert_eq!(hop.plan.reason, Goal::OpenAnomaly);
    }

    #[test]
    fn an_adjacent_target_is_stepped_to_directly() {
        let mut m = WorldMap::new();
        m.fold(&dump("start", "camp", vec![node("l4", "Grim Barrow — level 4 crypt")]));
        let hop = m.next_hop().unwrap();
        assert_eq!(hop.step, "l4");
        assert_eq!(hop.plan.target, "l4");
        assert!(hop.routed, "a step we can justify by an edge");
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
        assert_eq!(hop.step, "l2", "the move is the only way onward we know");

        // **The move is unchanged and the reason for it is not**, which is the whole of #24. `l4`
        // is a level 4 trigger we cannot reach, so it stops being what we claim to be doing;
        // `next_hop`'s any-frontier fallback used to supply this same step under a goal that was
        // not being pursued. Now the goal is the step.
        // Since 2026-08-16 the errand is exploring rather than routing to the trigger: `l4` is a
        // level 4 node and `l2` is not, so the trigger is queued behind the gentler ground rather
        // than declined for want of a route. The move and the honesty about it are unchanged.
        assert_eq!(hop.plan.reason, Goal::Explore);
        assert_eq!(hop.plan.target, "l2", "we say where we are actually going");
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
        assert_eq!(m.anomaly_is_open(), Some(false));
    }

    #[test]
    fn losing_four_health_sends_us_to_rest_before_exploring() {
        let m = hurt_at_l1();
        assert!(m.wants_rest());
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Rest);
        // The village. The campfire is nearer and free and would be the better answer, but arriving
        // at one does nothing yet — see `rest::CAMPFIRE_REST_IS_BUILT`.
        assert_eq!(plan.target, "l10", "the inn, while the campfire press is unwritten");
    }

    /// **The dev's question, reproduced.** *"After we completed the level 7 crypt, why did the
    /// navigator choose such a distant village to rest at instead of the ones we bought healthBuffs
    /// from?"*
    ///
    /// Because two inns of equal rank were separated by `pa.key.cmp(&pb.key)` — alphabetically —
    /// and `l11` sorts before `l19`. The map here is the one from
    /// `spike-run-20260815-1913Z.md` steps 49-53, cut down to the part that decides: standing at
    /// `l1`, with `l19` two hops away through `l9` and `l11` four hops away through `l9`, `l10` and
    /// `l4`.
    ///
    /// The second assertion is the positive control, and without it this test proves nothing: it
    /// pins the fact that the two candidates really are in the order that used to lose, so a fix
    /// that quietly stopped offering `l11` at all would not pass by accident.
    #[test]
    fn the_nearer_of_two_inns_wins_however_the_keys_happen_to_sort() {
        let mut m = WorldMap::new();
        m.fold(&dump("l1", "Weedley Copse crypt", vec![node("l9", "Kelk Wold — level 2 forest")]));
        m.fold(&dump(
            "l9",
            "Kelk Wold — level 2 forest",
            vec![
                node("l1", "Weedley Copse crypt"),
                node("l19", "Dane village"),
                node("l10", "Ulrome — level 3 forest"),
            ],
        ));
        m.fold(&dump("l10", "Ulrome — level 3 forest", vec![node("l4", "Bainton Clump — level 1 forest")]));
        m.fold(&dump("l4", "Bainton Clump — level 1 forest", vec![node("l11", "Rowlston Covert village")]));
        // Back where the run was when it chose, and hurt enough to want a bed.
        m.fold(&dump("l1", "Weedley Copse crypt", vec![node("l9", "Kelk Wold — level 2 forest")]));
        m.gold = 50;
        m.note_health(
            crate::rest::Health { current: 20, max: 20 },
            crate::rest::Health { current: 9, max: 20 },
        );
        assert!(m.wants_rest());

        assert!(
            "l11" < "l19",
            "the control: the far inn is the one an alphabetical tie-break would pick"
        );
        let plan = m.next_target().expect("a bed should be planned");
        assert_eq!(plan.reason, Goal::Rest);
        assert_eq!(plan.target, "l19", "two hops beats four, whatever the keys say");
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

    /// A village under attack, or lost, is neither a bed nor a shop.
    ///
    /// The dev, 2026-08-16: *the inn at a village that is under attack or lost cannot be rested at.*
    /// `getAreaButtons` (`overworld/generators/village.lua:84-98`) swaps the button set for every
    /// building in the village at once, from two `areaFlags` and nothing else.
    ///
    /// The under-attack case is the one worth a test rather than a comment: `Enter` still reads
    /// `Enter` and opens `ui.building_empty` instead of `ui.inn` (`:371-388`). A driver that ignored
    /// this would cross the village, press a button, succeed, and rest nothing — which is the
    /// campfire stall again with a working button in place of a missing one.
    #[test]
    fn a_village_under_attack_or_lost_is_not_somewhere_to_rest_or_shop() {
        let village = |flag: &str| {
            let mut m = WorldMap::new();
            m.fold(&dump(
                "l1",
                "Weedley Copse crypt",
                vec![node("l10", "Ulrome village"), node("l11", "Rowlston village")],
            ));
            m.note_health(
                crate::rest::Health { current: 12, max: 12 },
                crate::rest::Health { current: 7, max: 12 },
            );
            m.gold = 200;
            if !flag.is_empty() {
                m.apply_save(
                    &crate::game::save::parse(&format!(
                        "return {{ player = {{ gold = 200 }}, overworld = {{
                             areaFlags = {{ hell = 0, {flag} }} }} }}"
                    ))
                    .unwrap(),
                );
            }
            m
        };

        // Untouched, `l10` is the nearer bed and the planner says so.
        assert_eq!(village("").next_target().unwrap().target, "l10", "the premise");

        // `l10_attack` — villagers are fighting for it right now. `Enter` opens an empty room.
        let besieged = village("l10_attack = { attackingEnemies = 3 }");
        assert!(besieged.get("l10").unwrap().under_attack);
        assert!(!besieged.get("l10").unwrap().trades(), "an empty room is not an inn");
        let plan = besieged.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Rest);
        assert_eq!(plan.target, "l11", "the other village, which still has a bed");

        // `l10_attacked` — it was lost. Only `Loot` is left, and that is #26, not a rest.
        let sacked = village("l10_attacked = 4");
        assert!(sacked.get("l10").unwrap().sacked);
        assert_eq!(sacked.next_target().unwrap().target, "l11", "a sacked village is not a bed");

        // And the shelf goes with the bed, because the general store carries the same three button
        // sets (`village.lua:323`, `:332`).
        assert!(!sacked.get("l10").unwrap().stocks_a_heart(), "nor a shop");
        assert!(village("").get("l10").unwrap().stocks_a_heart(), "the control");
    }

    /// A campfire is never the rest destination, in any of the states that used to choose it.
    ///
    /// This test used to be about *cost* — a used campfire with no firewood restores nothing, so pay
    /// for the inn instead; carry firewood and the free one wins again. Both readings were right and
    /// both are now beside the point, because arriving at a campfire does nothing at all:
    /// `rest::CAMPFIRE_REST_IS_BUILT` is false and the driver has no handler to press.
    ///
    /// The dev, 2026-08-16, after a run walked to `l7` for a rest and straight back out again: *the
    /// campfire stalled the run; we can sidestep this by not trying to rest there.* Kept as a test
    /// rather than deleted so that turning the constant back on has something to fail against.
    #[test]
    fn a_campfire_is_never_the_rest_destination_while_the_press_is_unwritten() {
        let mut m = hurt_at_l1();
        m.entry("start").used = true;
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Rest);
        assert_eq!(plan.target, "l10", "the inn is the only rest we can actually take");

        // Firewood used to put it back on the table and outrank paying. It no longer does.
        m.fuel = 2;
        assert_eq!(m.next_target().unwrap().target, "l10", "fuel does not build the press");

        // Nor does an untouched one, which is the free-and-nearest case and the most tempting.
        m.entry("start").used = false;
        assert_eq!(m.next_target().unwrap().target, "l10", "and neither does a fresh campfire");
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
        assert_eq!(m.next_target().unwrap().reason, Goal::CloseAnomaly);
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

    /// Resuming a save: prefer ground we have not finished over ground we have, even if it is further.
    ///
    /// `visited` is set only by standing somewhere *this run*, and nothing seeds it from the save.
    /// So on a resume the first sort key ties for every place on the map and distance used to decide
    /// everything — which sent a resumed run back over its own cleared nodes because they were
    /// nearer. `completed` is the half we do load, so it is what carries the knowledge across.
    #[test]
    fn a_resumed_run_prefers_unfinished_ground_to_nearer_finished_ground() {
        let mut m = WorldMap::new();
        // `near` is one hop away and `far` is two, so distance and completion genuinely disagree.
        // An earlier version of this test put both at one hop, where the two orderings give the
        // same answer — it passed against the code it was written to catch.
        m.fold(&dump("here", "Somewhere", vec![node("near", "Nearby crossroads")]));
        m.entry("far").heading = "Distant crossroads".into();
        m.entry("near").neighbours.insert("far".into());
        m.entry("far").neighbours.insert("near".into());
        // One hop and two, at the uncleared-ground price every edge in this fixture carries.
        assert_eq!(m.distances("here").get("near"), Some(&CROSSING));
        assert_eq!(m.distances("here").get("far"), Some(&(2 * CROSSING)));
        // Nothing has been stood on this run but `here`: that is what a resume looks like.
        assert!(!m.get("near").unwrap().visited && !m.get("far").unwrap().visited);
        // What the save told us, and the only thing that separates them.
        m.entry("near").completed = true;

        let target = m.next_target().expect("something to explore").target;
        assert_eq!(target, "far", "cleared ground is not worth walking to first");
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

    /// With nothing at all we can route to, aim at the unreachable target and step the way it lies.
    ///
    /// This is the branch coordinates were wanted for, and #24 narrowed rather than removed it:
    /// something routable now wins the nomination outright, so the fallback is reached only when
    /// *nothing* is. The choice among adjacent nodes used to come down to the lowest key, so a run
    /// heading for the anomaly stepped whichever way the alphabet pointed.
    ///
    /// **The earlier fixture for this could not fail.** It reached `start` through a second dump
    /// that shared a node with the first, and `fold` records edges both ways — so a route existed,
    /// `first_step_toward` answered `west` on its own, and the bearing was never consulted. Two
    /// leaves either side of a crossroads and an anomaly known only from a save flag is the real
    /// shape of it.
    #[test]
    fn with_no_route_we_step_the_way_the_target_lies() {
        let mut m = WorldMap::new();
        ready_for_the_anomaly(&mut m);
        // Two leaves. Nothing left to reveal at either, so the planner will not explore to them --
        // and they are the only two moves there are.
        m.fold(&dump("here", "The Wold crossroads", vec![
            Node { key: "west".into(), heading: "West Field".into(),
                   x: -100.0, y: 0.0, connections: 1 },
            Node { key: "east".into(), heading: "East Field".into(),
                   x: 100.0, y: 0.0, connections: 1 },
        ]));
        // The anomaly as a live run meets it: heard of through `start_first_corrupt_time`, with no
        // heading and no edges, so no route. Its bearing comes from the corrupted blob, which is
        // west of here.
        m.apply_save(&crate::game::save::parse(
            "return { overworld = { areaFlags = {
                 hell = 0.1, start_first_corrupt_time = 12, west_first_corrupt_time = 12,
             } } }",
        ).unwrap());

        assert!(!m.can_route_to("start"), "the fixture must actually have no route");
        let hop = m.next_hop().expect("a step");
        assert_eq!(hop.plan.reason, Goal::CloseAnomaly, "nothing routable, so aim at it anyway");
        assert_eq!(hop.plan.target, "start");
        assert!(m.gap("west", "start").unwrap() < m.gap("east", "start").unwrap());
        assert_eq!(hop.step, "west", "toward the anomaly, not the lower key");
        assert!(!hop.routed, "and the log has to say so");
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

    /// **The detour itself**, which is what #16 is named for.
    ///
    /// The dev's rule: *when the goal is not rest and a chest is visible, detour to it, open it,
    /// then rejoin the path.* Both conditions are asserted here, because the leaf exception below
    /// only makes a chest *reachable* — on its own it would leave a chest unvisited whenever
    /// something else was the errand, which is every step after the portal opens.
    #[test]
    fn an_unopened_chest_earns_a_detour_unless_we_are_hurt() {
        let build = || {
            let mut m = WorldMap::new();
            m.fold(&dump(
                "here",
                "camp",
                vec![node("l5", "Riccall chest"), node("l6", "Bainton Clump — level 1 forest")],
            ));
            m.here = Some("here".into());
            m
        };

        let m = build();
        let plan = m.next_target().expect("a plan");
        assert_eq!(plan.reason, Goal::Chest);
        assert_eq!(plan.target, "l5");

        // Hurt, and the chest is a fight: `Open` starts a combat scenario, so the bed comes first.
        let mut hurt = build();
        hurt.gold = crate::rest::INN_COST;
        hurt.fold(&dump("here", "camp", vec![node("l7", "Greenoak campfire")]));
        hurt.fuel = 1;
        hurt.note_health_level(crate::rest::Health { current: 3, max: 20 });
        assert!(hurt.wants_rest());
        assert_ne!(hurt.next_target().expect("a plan").reason, Goal::Chest, "a fight is not a rest");

        // And an opened one is not a destination at all.
        let mut done = build();
        done.entry("l5").completed = true;
        assert_ne!(done.next_target().expect("a plan").reason, Goal::Chest);
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

    /// A heart before the anomaly, and every clause of the dev's rule.
    ///
    /// Given the evening a run died going the right way: two level 6 corrupted crypts back to back,
    /// and the second ran the board dry against a five-deep queue. Four maximum health for a hundred
    /// gold is the cheapest preparation there is, and the window is while the road is still free.
    #[test]
    fn a_heart_is_worth_the_walk_when_the_road_to_it_is_free() {
        let village = |k: &str| node(k, "Rowlston Covert village");
        let build = || {
            let mut m = WorldMap::new();
            m.fold(&dump("here", "camp", vec![village("l11"), node("l4", "Riccall — level 6 crypt")]));
            m.here = Some("here".into());
            m.hell = Some(0.1);
            m.gold = HEART_FLOOR;
            m
        };

        let m = build();
        let plan = m.next_target().expect("a plan");
        assert_eq!(plan.reason, Goal::Heart, "the anomaly is open, the gold is there, the road is free");
        assert_eq!(plan.target, "l11");

        // **And with the portal shut**, which is the dev's correction of 2026-08-16. The clause
        // used to be "the goal must already be the anomaly", read as "the anomaly is open" — and
        // the fights that open it are the level 6 ones this rule was written after.
        let mut shut = build();
        shut.hell = Some(0.0);
        assert!(!shut.anomaly_is_open().unwrap_or(true));
        let plan = shut.next_target().expect("a plan");
        assert_eq!(plan.reason, Goal::Heart, "the heart does not wait for the portal");
        assert_eq!(plan.target, "l11");

        // A pound short and it is not a plan. **The bar is the price plus a bed**, not the price:
        // the dev raised it to 110 on 2026-08-15 after watching the errand spend a run down to
        // nothing, which trades four maximum health for the six-a-press that keeps it alive.
        let mut m = build();
        m.gold = HEART_FLOOR - 1;
        assert_ne!(m.next_target().unwrap().reason, Goal::Heart, "the price alone is not enough");
        assert_eq!(HEART_FLOOR, HEART_COST + crate::rest::INN_COST, "the reserve is exactly one night");

        // A fight on the way and it is not a detour, it is the fight.
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("l4", "Riccall — level 6 crypt")]));
        m.fold(&dump("l4", "Riccall — level 6 crypt", vec![village("l11")]));
        m.here = Some("here".into());
        m.hell = Some(0.1);
        m.gold = HEART_FLOOR;
        assert_ne!(m.next_target().unwrap().reason, Goal::Heart, "the crypt is in the way");

        // And a village we have already emptied is not a destination.
        let mut m = build();
        m.bought_the_heart("l11");
        assert_ne!(m.next_target().unwrap().reason, Goal::Heart, "the shelf is bare");
    }

    /// **A bed at every settlement noun; a heart at only two of them.** Task #75.
    ///
    /// The two questions were one predicate, and `is_settlement` — `village || town` — was answering
    /// both. It is the heart's bar and it is right for that: `healthBuff` is what makes a village a
    /// village, and a hamlet is defined by having neither buff
    /// (`overworld/locations/village.lua:6-14`). It is the wrong bar for a bed, because `store_inn`
    /// is unconditional in every settlement's roster (`overworld/generators/village.lua:684-685`).
    ///
    /// A town has a bed and *may* have a heart — `world.lua:520-529` hands out `gearSlotsBuff` and
    /// `healthBuff` in separate passes over the same list, so `getTypeName` returning `town` on the
    /// first tells us nothing about the second. Targeting one for a heart is therefore a gamble the
    /// planner takes knowingly; targeting one for a bed is not a gamble at all.
    #[test]
    fn a_hamlet_has_a_bed_and_never_a_heart() {
        let hurt_beside = |heading: &str| {
            let mut m = WorldMap::new();
            m.fold(&dump("here", "camp", vec![node("l27", heading)]));
            m.here = Some("here".into());
            m.gold = 500;
            m.hell = Some(0.1);
            ready_for_the_anomaly(&mut m);
            m.note_health_level(crate::rest::Health { current: 4, max: 20 });
            m
        };

        for heading in ["Rowlston Covert village", "Enholmes town", "Fordon hamlet"] {
            let m = hurt_beside(heading);
            let plan = m.next_target().unwrap_or_else(|| panic!("no plan beside `{heading}`"));
            assert_eq!(plan.reason, Goal::Rest, "`{heading}` has an inn like every settlement");
            assert_eq!(plan.target, "l27");
        }

        // The heart is the other question, and the hamlet is where the two part company.
        let heart_beside = |heading: &str| {
            let mut m = WorldMap::new();
            m.fold(&dump("here", "camp", vec![node("l27", heading)]));
            m.here = Some("here".into());
            m.gold = 500;
            m.hell = Some(0.1);
            ready_for_the_anomaly(&mut m);
            m
        };
        for heading in ["Rowlston Covert village", "Enholmes town"] {
            assert!(heart_beside(heading).get("l27").unwrap().stocks_a_heart(), "{heading}");
        }
        let m = heart_beside("Fordon hamlet");
        assert!(!m.get("l27").unwrap().stocks_a_heart(), "a hamlet has neither buff, so no heart");
        assert_ne!(
            m.next_target().map(|p| p.reason),
            Some(Goal::Heart),
            "and it is never worth the walk for one"
        );
    }

    /// Standing at a settlement, short of full, with the price in pocket: rest.
    ///
    /// The dev's rule, 2026-08-15. Deliberately weaker than [`WorldMap::wants_rest`] — half health
    /// or a four-point drop are the bars for a *detour*, and standing in the doorway is not one.
    #[test]
    fn passing_a_settlement_at_less_than_full_health_is_worth_a_bed() {
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("l28", "Enholmes town")]));
        // **Set, and load-bearing.** Without it `next_target` has no origin, every branch below
        // falls through to `Explore`, and the two assertions further down would both pass without
        // testing anything. The control caught exactly that while this test was being written.
        m.here = Some("here".into());
        m.gold = 50;
        m.health = Some(crate::rest::Health { current: 18, max: 20 });
        assert!(!m.wants_rest(), "two points down is nowhere near the detour bar");
        assert!(m.top_up_at("l28"), "but we are standing at the door");

        // **And it stays off the detour bar.** Task #72. This asserted `wants_rest()` until
        // 2026-08-21, on the reasoning that the errand machinery had to hear about the decision —
        // true, but it heard through the wrong channel. `wants_rest` is what the surface planner's
        // `Goal::Rest` branch reads, so the doorway rule was minting detours: live at 77/80 the plan
        // came out `Rest -> l63`, a surface hop to heal three points for ten gold.
        assert!(!m.wants_rest(), "a scratch is not a reason to walk anywhere");
        assert!(m.wants_a_bed(), "but it is a reason to use the bed we are standing on");
        assert_ne!(
            m.next_target().map(|p| p.reason),
            Some(Goal::Rest),
            "and no trip is planned for it"
        );

        // The control, and the half that must not regress: real damage still buys the detour.
        //
        // Against a **village**, not the town above. `rest::site` matches `village`, `inn` and
        // `campfire` and does not know the word `town` — so no town is ever a rest site, which is
        // task #75 and not this one. The control was silently green against the town until that
        // turned up, which is the second thing it has caught here.
        m.fold(&dump("here", "camp", vec![node("l27", "Rowlston Covert village")]));
        m.note_health_level(crate::rest::Health { current: 4, max: 20 });
        assert!(m.wants_rest());
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Rest, "a wound still walks");
        assert_eq!(plan.target, "l27");

        // Both are cleared by the same events, so a top-up cannot outlive the bed it was about.
        m.rest_errand_over();
        assert!(!m.wants_rest());
        assert!(!m.wants_a_bed(), "the top-up flag is cleared alongside it, not left standing");

        // Full health buys nothing.
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("l28", "Enholmes town")]));
        m.gold = 50;
        m.health = Some(crate::rest::Health { current: 20, max: 20 });
        assert!(!m.top_up_at("l28"));

        // Nor does a purse that cannot pay the innkeeper.
        m.health = Some(crate::rest::Health { current: 18, max: 20 });
        m.gold = crate::rest::INN_COST - 1;
        assert!(!m.top_up_at("l28"));
        m.gold = crate::rest::INN_COST;
        assert!(m.top_up_at("l28"), "exactly the price is enough for a bed — `getCanRest` is `>=`");

        // And a forest is not a settlement, whatever our health is.
        m.fold(&dump("here", "camp", vec![node("l16", "Bursall Hedge — level 2 forest")]));
        m.gold = 500;
        assert!(!m.top_up_at("l16"));
    }

    /// The shelf is emptied, and the bed is never spent.
    #[test]
    fn the_purse_always_keeps_one_night_back() {
        let mut m = WorldMap::new();
        m.gold = 110;
        assert_eq!(m.hearts_affordable(), 1, "110 buys one and leaves the inn's ten");
        m.gold = 209;
        assert_eq!(m.hearts_affordable(), 1, "nine short of the second");
        m.gold = 210;
        assert_eq!(m.hearts_affordable(), 2);
        m.gold = 1040;
        assert_eq!(m.hearts_affordable(), 10);
        m.gold = 0;
        assert_eq!(m.hearts_affordable(), 0, "never negative, whatever the purse");
    }

    /// `l4`: remembered as `Riccall crypt`, met as `Riccall — level 6 crypt`.
    ///
    /// The level is in a heading only while a fight is owed (`AreaHeading`,
    /// `overworldview.lua:388-389`), so a heading cached while the node was clear says nothing about
    /// the node after corruption reset it.
    ///
    /// **And corruption is what closes that gap, not suspicion of the cache.** The dev's rule: the
    /// cache says what exists, the dump says what it is, and the save says what is corrupted. A
    /// remembered name is not evidence of danger and is not treated as any.
    #[test]
    fn corruption_is_what_makes_a_remembered_heading_a_fight() {
        let cached = format!(
            "{CACHE_VERSION}
p	l4	Riccall crypt	951	275	4	0
p	l11	Rowlston Covert village	900	300	2	0
e	l4	l11
"
        );
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("l4", "")]));
        assert!(m.absorb_cache(&cached) > 0);
        m.here = Some("here".into());
        m.gold = HEART_COST + 1;

        // Remembered and uncorrupted: no reason to think it is a fight, and no pessimism about it.
        m.hell = Some(0.1);
        assert!(!m.get("l4").expect("recalled").may_be_a_fight(), "a remembered name is not a threat");
        assert!(m.reachable_without_a_fight("here", "l11"), "so the village behind it is a free trip");

        // The save says otherwise, and the save is read fresh every step.
        m.apply_save(
            &crate::game::save::parse(
                "return { overworld = { areaFlags = { hell = 0.1, l4_first_corrupt_time = 12 } } }",
            )
            .unwrap(),
        );
        assert!(m.get("l4").unwrap().corrupted);
        assert!(m.get("l4").unwrap().may_be_a_fight(), "corruption is what turns it back into one");
        assert!(!m.reachable_without_a_fight("here", "l11"), "and the trip stops being free");
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

    /// The last decision of the run of 2026-08-09, rebuilt from `spike-run-raw.log:1336-1344`.
    ///
    /// Standing on `l28`, the dump named `shrine6` for the first time — two connections, uncorrupted,
    /// unconsecrated, one step away — alongside `l49`, a level 6 crypt. The planner named `start`,
    /// which sat in the map unheaded and edgeless, stepped onto the crypt because no route to `start`
    /// existed, and the run died there. A branch that could make no progress outranked one that could.
    ///
    /// **`shrine6` was known for exactly this one decision**, not for the five hops before it — the
    /// earlier `(for start, Anomaly)` lines had no shrine in the map to skip. Worth being precise
    /// about: what #24 recovers here is the last move, and what it changes about the other four is
    /// only that they stop claiming an errand and explore under their own name.
    #[test]
    fn a_shrine_we_can_reach_beats_an_anomaly_we_cannot() {
        let mut m = WorldMap::new();
        m.fold(&dump("l28", "Enholmes town", vec![
            node("l27", "Barkerdale village"),
            node("l19", "Dane village"),
            node("l49", "Yokefleet — level 6 crypt"),
            node("shrine6", "Borsea shrine"),
        ]));
        // `start` as the run actually held it: heard of through `start_first_corrupt_time`, no
        // heading, no edges. `hell = 0.1` means the portal is open -- see `anomaly_is_open`.
        m.apply_save(&crate::game::save::parse(
            "return { overworld = { areaFlags = { hell = 0.1, start_first_corrupt_time = 12 } } }",
        ).unwrap());
        m.here = Some("l28".into());

        assert_eq!(m.anomaly().map(|p| p.key.as_str()), Some("start"), "still known for what it is");
        assert!(!m.can_route_to("start"), "and still nothing we can walk to");
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Shrine, "the branch that can make progress gets its turn");
        assert_eq!(plan.target, "shrine6");
        assert_eq!(m.next_hop().unwrap().step, "shrine6", "not the level 6 crypt next door");
    }

    /// Giving up on an errand we cannot route to does not mean giving up its direction.
    ///
    /// The half of #24 that is easy to lose. Refusing to *nominate* an unreachable target is right;
    /// letting exploration then wander is how the fix would have paid for a truthful log with a
    /// slower run. Both frontiers here are one hop away and equally informative, so nothing but the
    /// bearing separates them — and the keys are chosen so the alphabet points the wrong way, which
    /// is what the ordering fell back on before.
    #[test]
    fn we_explore_toward_the_errand_we_could_not_route_to() {
        let mut m = WorldMap::new();
        ready_for_the_anomaly(&mut m);
        m.fold(&dump("here", "The Wold crossroads", vec![
            Node { key: "zwest".into(), heading: "West Field".into(),
                   x: -100.0, y: 0.0, connections: 3 },
            Node { key: "aeast".into(), heading: "East Field".into(),
                   x: 100.0, y: 0.0, connections: 3 },
        ]));
        m.apply_save(&crate::game::save::parse(
            "return { overworld = { areaFlags = {
                 hell = 0.1, start_first_corrupt_time = 12, zwest_first_corrupt_time = 12,
             } } }",
        ).unwrap());

        assert!(!m.can_route_to("start"), "so the anomaly branch yields");
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::RouteTo(Box::new(Goal::CloseAnomaly)));
        assert_eq!(plan.target, "zwest", "the alphabet says `aeast`; the corruption says west");
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
        assert!(m.anomaly_is_open().unwrap(), "fixture must have the door open");

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
    fn an_open_portal_makes_a_clean_shrine_the_errand_and_a_corrupted_one_no_errand_at_all() {
        // Two opposite rules meet once the door opens, and this pins both.
        //
        // An **uncorrupted** shrine outranks the portal: consecrating is only possible while
        // `hell ~= 0`, it costs no fight, and it pays in wildcard tiles before a level 8 fight. A
        // **corrupted** one has to be fought through again, so it is never a destination -- it is
        // worth something only when we are already standing on it.
        //
        // here — shrine2(shrine) is a dead end; here — shrine1(shrine) — rift is the route.
        //
        // **The shrines are keyed `shrineN` because the game keys them that way**, and since
        // 2026-08-17 that is load-bearing rather than cosmetic: `majorShrine` is set from
        // `key:sub(1,6)=='shrine'` (`overworld/generators/world.lua:87-90`), and `Consecrate` needs
        // it (`shrine.lua:93-96`), so [`Place::can_be_consecrated`] reads the key. These used to be
        // `mid` and `detour`, which no overworld shrine can be called — a fixture in a state play
        // cannot produce, quietly asserting on it.
        let mut m = WorldMap::new();
        ready_for_the_anomaly(&mut m);
        m.fold(&dump(
            "here",
            "camp",
            vec![node("shrine2", "Faraway shrine"), node("shrine1", "Midway shrine")],
        ));
        m.fold(&dump(
            "shrine1",
            "Midway shrine",
            vec![node("here", "camp"), node("rift", "The Rift anomaly")],
        ));
        m.here = Some("here".into());
        m.hell = Some(0.1);

        // Clean shrines reachable, so the shrine is the errand -- ahead of the portal itself. This
        // assertion was the opposite before 2026-08-10; the run that changed it walked past three
        // shrines and died in a level 6 crypt with the portal never once nominated.
        assert_eq!(m.next_target().unwrap().reason, Goal::Shrine);

        // The route is a separate question from the goal, and it is unchanged.
        let route = m.anomaly_route().unwrap();
        assert!(route.contains(&"shrine1".to_string()), "route: {route:?}");
        assert!(!route.contains(&"shrine2".to_string()), "the dead-end shrine is not on the way");

        // Corrupt both. Now neither is a destination, and the portal is what is left -- which is
        // also the control: without the corruption filter this would still say `Shrine`, so the
        // first assertion above would pass for the wrong reason.
        m.entry("shrine1").corrupted = true;
        m.entry("shrine2").corrupted = true;
        assert_eq!(
            m.next_target().unwrap().reason,
            Goal::CloseAnomaly,
            "a corrupted shrine is never walked to, so the portal is the errand again"
        );

        // Arrival is the other axis and it did not move: the one on the way earns its fight
        // because we are crossing it regardless, the dead end does not.
        assert!(m.worth_consecrating_here("shrine1"), "we are walking through it regardless");
        assert!(
            !m.worth_consecrating_here("shrine2"),
            "a corrupted dead end is not worth the fight"
        );

        // **And once the fight is won the objection is spent.** Corruption is a bill, not a
        // property; `completed` means it has been paid and cannot be charged twice. The dead end is
        // still a dead end and still off every route — that is what makes this the right control.
        m.entry("shrine2").completed = true;
        assert!(
            m.worth_consecrating_here("shrine2"),
            "the fight this was avoiding has already been fought"
        );
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
    fn a_map_kept_between_runs_gives_the_planner_a_route_it_had_walked() {
        // The live failure of 2026-08-14, in miniature. An earlier run walked `here -> mid -> s1`
        // and stood on the shrine; a later run starts knowing only what the save carries — that
        // `s1` exists, is complete, is corrupted and is not consecrated — and therefore cannot
        // route to it. `RouteTo(Shrine)` for a whole run, and a free consecration never taken.
        let mut walked = WorldMap::new();
        walked.fold(&dump("here", "camp", vec![node("mid", "Quiet Glade meadow")]));
        walked.fold(&dump("mid", "Quiet Glade meadow", vec![node("s1", "Faraway shrine")]));
        let text = walked.cache_text();

        // The restart: flags only, exactly as `apply_save` leaves it.
        let mut fresh = WorldMap::new();
        fresh.fold(&dump("here", "camp", vec![]));
        fresh.hell = Some(0.1);
        {
            // Cleared, and **uncorrupted**: since 2026-08-15 a corrupted shrine is not a target at
            // all, and this test is about the cache restoring a route rather than about corruption.
            let p = fresh.entry("s1");
            p.completed = true;
        }
        assert!(
            !matches!(fresh.next_target().map(|p| p.reason), Some(Goal::Shrine)),
            "the premise: with no edges the shrine cannot be planned"
        );

        // Now hand it back what it had already learned.
        assert!(fresh.absorb_cache(&text) > 0, "the cache carried no edges");
        // **And say which of that ground is cleared**, which is the save's job rather than the
        // cache's. Since 2026-08-15 a recalled heading cannot prove a node is safe to cross — the
        // level is in the heading only while a fight is owed (`AreaHeading`,
        // `overworldview.lua:388-389`), so a heading remembered from before corruption reads tamer
        // than the node now is. Without this the fixture asserts something it never meant to: that
        // unwalked ground with a remembered name is known to be fightless.
        for k in ["here", "mid"] {
            fresh.entry(k).completed = true;
        }
        let plan = fresh.next_target().expect("a plan");
        assert_eq!(plan.reason, Goal::Shrine, "the route was known all along");
        assert_eq!(plan.target, "s1");
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

    #[test]
    fn a_corrupted_shrine_is_not_a_destination_but_is_still_consecrated_on_arrival() {
        // **The rule changed on 2026-08-15 and this test changed with it.** It used to assert that a
        // corrupted shrine whose fight we had already won was a destination again, which was the
        // dev's earlier correction — corruption is a level, not a locked door. The refinement:
        // corruption still is not a locked door, but it is not a reason to make the trip either.
        // Only unconsecrated, uncorrupted shrines are targets while the anomaly is open; a corrupted
        // one is consecrated **if we happen to be standing on it**, having gone there for something
        // else.
        //
        // What that protects: the run of 2026-08-15 walked `shrine5 -> l50 -> l41 -> l51 -> l35 ->
        // l49` chasing shrines, into a level 6 crypt, with the anomaly untouched.
        //
        // Both halves are asserted on one map, because the two used to drift apart and the drift is
        // what left `shrine1` unconsecrated for four runs.
        // Keyed `shrine1` rather than `s1`, because `Consecrate` is gated on `majorShrine` and the
        // game derives that from the key (`world.lua:87-90`) — see [`Place::can_be_consecrated`].
        let mut m = WorldMap::new();
        let mut d =
            dump("here", "camp", vec![node("shrine1", "Faraway shrine"), node("l2", "Quiet Glade meadow")]);
        d.hidden = 1;
        m.fold(&d);
        m.hell = Some(0.1);
        m.entry("shrine1").corrupted = true;
        // **The bar has to be met, or #74 makes this shrine a target and the test is about a
        // different rule.** Corruption stops being a veto once the clean supply is spent and
        // `SHRINES_BEFORE_THE_ANOMALY` is still short — which, on a fixture holding exactly one
        // shrine and it corrupted, is true from the first line. Satisfying the gate isolates the
        // 2026-08-15 rule this test is actually about.
        ready_for_the_anomaly(&mut m);

        assert_ne!(m.next_target().unwrap().reason, Goal::Shrine, "the fight is still owed");
        assert!(
            !m.worth_consecrating_here("shrine1"),
            "and the arrival test agrees while it is owed"
        );

        m.entry("shrine1").completed = true;
        assert_ne!(
            m.next_target().unwrap().reason,
            Goal::Shrine,
            "cleared or not, a corrupted shrine is not somewhere we set off for"
        );
        assert!(
            m.worth_consecrating_here("shrine1"),
            "but standing on it, the consecration is free and we take it"
        );

        // Uncorrupt the same shrine and it becomes a destination, which is what pins the filter to
        // corruption rather than to something else about the fixture.
        m.entry("shrine1").corrupted = false;
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Shrine);
        assert_eq!(plan.target, "shrine1");
    }

    /// A shrine behind a fight is still a destination **once the portal is open**. Task #70.
    ///
    /// This asserted the opposite until 2026-08-21, on the rule *go toward the anomaly unless there
    /// is an accessible shrine that does not require combat*. The dev reversed it after watching the
    /// 1519Z run: *shrines should never be blocked by level unless the anomaly has not yet opened.*
    ///
    /// The reversal is not a change of mind about cost, it is the gate changing what a shrine **is**.
    /// When that rule was written a shrine was optional reward, and paying a crypt for one spent the
    /// level 8 fight's budget early. [`SHRINES_BEFORE_THE_ANOMALY`] made four of them a
    /// precondition, and the argument at the `CloseAnomaly` branch already says so: *the shrines are
    /// not a reward to collect on the way, they are the preparation the fight needs.* Preparation we
    /// are required to buy cannot also be refused for costing something.
    ///
    /// Live: the eastern shrines of the 1519Z run were revealed from the south and struck off the
    /// candidate list while a forest stood between, then consecrated from the north twenty steps
    /// later. The whole detour bought nothing — one step after the second shrine the run took a
    /// level 6 forest fight anyway, to explore.
    #[test]
    fn a_shrine_behind_a_crypt_is_still_worth_the_trip_once_the_portal_is_open() {
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("l49", "Yokefleet — level 6 crypt")]));
        m.fold(&dump("l49", "Yokefleet — level 6 crypt", vec![node("shrine2", "Faraway shrine")]));
        m.here = Some("here".into());
        m.hell = Some(0.1);

        // The route still costs a fight, and we go anyway. Both halves matter: a fixture that had
        // quietly become fight-free would pass this for the wrong reason.
        assert!(!m.reachable_without_a_fight("here", "shrine2"), "the only way through is the crypt");
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Shrine);
        assert_eq!(plan.target, "shrine2");
    }

    /// **Before the portal opens, level still blocks** — and that is a different rule, kept.
    ///
    /// #70 removed the *route* test, not the pre-anomaly one. `triggers_anomaly` exists because
    /// arriving at a level 4+ node is what opens the portal (`world_evil.lua:15-18`), so a shrine
    /// above level 3 is not a free errand while `hell == 0` — it is the trigger, and taking it ends
    /// the phase it belongs to. The run of 2026-08-17 walked into `shrine7`, *Cottam Boscage — level
    /// 9 forest*, at 52/52 exactly this way.
    #[test]
    fn before_the_portal_a_deep_shrine_is_still_refused() {
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("shrine7", "Cottam Boscage — level 9 forest")]));
        m.here = Some("here".into());
        m.hell = Some(0.0);

        assert_ne!(
            m.next_target().map(|p| p.reason),
            Some(Goal::Shrine),
            "a level 9 shrine is the anomaly trigger, not a cheap errand"
        );
    }

    /// "No route I can see" and "no route" are different claims, and only one of them is ours.
    ///
    /// The dev's correction, 2026-08-15, after a run bought one heart and went straight at a level 7
    /// crypt: *it should be the navigator's responsibility to probe and build the in-memory cache of
    /// which nodes are accessible without combat, not to assume from the cache that they are
    /// inaccessible.*
    ///
    /// The live map is the fixture. `l28 Enholmes town` declared **four** connections with exactly
    /// **one** recorded, so three of its roads had never been looked down — and the planner reported
    /// it unreachable-without-a-fight as though that were a fact about the map.
    #[test]
    fn an_unlooked_road_is_unknown_access_and_a_closed_region_is_blocked() {
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("free", "Quiet Glade meadow")]));
        m.fold(&dump("free", "Quiet Glade meadow", vec![node("l49", "Yokefleet — level 6 crypt")]));
        m.fold(&dump("l49", "Yokefleet — level 6 crypt", vec![node("l28", "Enholmes town")]));
        m.here = Some("here".into());

        // Closed first: every free node has all the roads the game says it has, so the crypt really
        // is the only way and saying so is honest.
        for k in ["here", "free"] {
            let n = m.entry(k).neighbours.len() as u32;
            m.entry(k).connections = n;
        }
        assert_eq!(m.access_without_a_fight("here", "l28"), Access::Blocked);

        // Now put a road we have never looked down inside the region — the `l28` case, one node
        // earlier. The answer must stop being a claim about the map.
        //
        // **On ground we have not stood on**, which is what changed with #73. Bumping `free`'s own
        // degree used to do it, but `free` is a dump's `here_key` and so is `visited`: a gap that
        // survived standing there is a secret neighbour, not an unlooked road, and it no longer
        // counts. `lane` is named by the dump and never walked to, which is the ordinary shape of a
        // frontier and needs no hand-set degree at all.
        m.fold(&dump(
            "free",
            "Quiet Glade meadow",
            vec![node("l49", "Yokefleet — level 6 crypt"), node("lane", "Fordon Lane meadow")],
        ));
        assert!(!m.get("lane").unwrap().visited, "the premise: named, never stood on");
        assert_eq!(m.access_without_a_fight("here", "l28"), Access::Unknown);

        // And an unlooked road never upgrades a *known* fight into a maybe: the crypt on the only
        // recorded route is still a crypt.
        assert_eq!(m.access_without_a_fight("here", "l49"), Access::Blocked, "the fight is not in doubt");
    }

    /// On `Unknown`, the heart errand walks to the unlooked road instead of abandoning the errand.
    #[test]
    fn a_heart_behind_the_fog_is_probed_rather_than_written_off() {
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("free", "Quiet Glade meadow")]));
        m.fold(&dump("free", "Quiet Glade meadow", vec![node("l49", "Yokefleet — level 6 crypt")]));
        m.fold(&dump("l49", "Yokefleet — level 6 crypt", vec![node("l28", "Enholmes town")]));
        m.here = Some("here".into());
        m.hell = Some(0.1);
        m.gold = 500;
        assert!(m.wants_a_heart(), "anomaly open and the price is affordable");

        // Closed: nothing to look at, so the errand cannot be served and the run gets on with the
        // anomaly. This half is what says the probe is evidence-driven rather than a wander.
        for k in ["here", "free"] {
            let n = m.entry(k).neighbours.len() as u32;
            m.entry(k).connections = n;
        }
        assert_ne!(
            m.next_target().map(|p| p.reason),
            Some(Goal::Heart),
            "a closed fight-free region really has no heart in it"
        );

        // One unlooked road, and the same map is worth walking into. **Named and never walked to**
        // since #73 — a gap on `free`, which we have stood on, is a secret neighbour rather than an
        // unlooked road, and buys no second visit.
        m.fold(&dump(
            "free",
            "Quiet Glade meadow",
            vec![node("l49", "Yokefleet — level 6 crypt"), node("lane", "Fordon Lane meadow")],
        ));
        let plan = m.next_target().expect("something to do");
        assert_eq!(plan.reason, Goal::Heart, "the walk is still the heart errand, and says so");
        assert_eq!(plan.target, "lane", "the probe is the node with the road we have not tried");
    }

    /// **A node we have stood on is not a probe candidate at all.** Task #73.
    ///
    /// The 1519Z bounce, `l11 Argham crossroads` against `l13 Bilton crypt`, and the shape that
    /// showed the preference added earlier the same day is not enough:
    ///
    /// ```text
    ///   83. l44 -> **l11**  (for l11, Heart)
    ///   84. l11 -> **l13**  (for l36, Explore)
    ///   85. l13 -> **l11**  (for l11, Heart)
    ///   86. l11 -> **l13**  (for l36, Explore)
    ///   87. l13 -> **l11**  (for l11, Heart)
    ///   88. `l11` is written off — stood on 2 times with nothing learned
    /// ```
    ///
    /// Three laps. `min_by_key((visited, dist))` ranks candidates against each other, so with
    /// **one** candidate it changes nothing — and one is the ordinary case. The dev, 2026-08-21,
    /// choosing between ranking and filtering with this run in front of them: *one probe per node,
    /// remembered.*
    #[test]
    fn a_probe_is_not_offered_the_same_node_twice() {
        let mut m = WorldMap::new();
        m.fold(&dump("l13", "Bilton — level 4 crypt", vec![node("l11", "Argham crossroads")]));
        m.here = Some("l13".into());
        let dist = m.distances("l13");

        // One candidate, and it declares a road we have never looked down.
        m.entry("l11").connections = 4;
        assert!(m.has_unexplored_roads("l11"), "the premise: the game says there is more here");
        assert_eq!(
            m.probe_toward_the_unknown("l13", &dist).map(|p| p.key.as_str()),
            Some("l11"),
            "so the first probe goes"
        );

        // Stand on it. The dump names one neighbour and the declared degree is still four, so the
        // gap survived the one act that could have closed it — a secret neighbour, which
        // `verboseAdjacencyData` prints as `Hidden location` and never names until a tower reveals
        // it (`overworldview.lua:554-556`, task #14).
        m.fold(&dump("l11", "Argham crossroads", vec![node("l13", "Bilton — level 4 crypt")]));
        m.entry("l11").connections = 4;
        m.here = Some("l13".into());

        assert!(!m.has_unexplored_roads("l11"), "a gap that survives a visit is not unexplored");
        assert_eq!(
            m.probe_toward_the_unknown("l13", &dist),
            None,
            "and there is no second probe to bounce off"
        );
    }

    /// **The probe goes outward, and never back to where it has stood.**
    ///
    /// The `l11 Argham crossroads` / `l5 Dalton Copse` bounce of 2026-08-21, in the smallest shape
    /// that produces it. Standing on a node is what makes the game name its neighbours, so a gap
    /// that survives a visit is one visiting cannot close — a secret neighbour, most often. The
    /// probe kept nominating it, the errand at the far end nominated the way back, and it took two
    /// full laps and a write-off to break.
    ///
    /// Named `..._prefers_...` when it was a ranking. #73 made it a filter, because a ranking has
    /// nothing to decide when there is one candidate — see
    /// [`WorldMap::has_unexplored_roads`] and the second half of this test.
    #[test]
    fn the_heart_probe_never_returns_to_ground_it_has_stood_on() {
        let build = || {
            let mut m = WorldMap::new();
            // Two ways out of the camp, both fight-free, both with a road we have not looked down.
            // **`stood` is strictly nearer**, or the ranking proves nothing: a tie on distance is
            // broken by iteration order, and `fresh` happens to sort first. The bounce is precisely
            // the case where the node we have stood on is the *closest* one still declaring a road.
            m.fold(&dump("here", "camp", vec![node("stood", "Argham crossroads")]));
            m.fold(&dump(
                "stood",
                "Argham crossroads",
                vec![node("here", "camp"), node("fresh", "Quiet Glade meadow")],
            ));
            m.here = Some("here".into());
            m.hell = Some(0.1);
            m.gold = 500;
            // A village exists but no route to it is known, which is what sends the errand probing.
            m.entry("far").heading = "Rowlston Covert village".into();
            for k in ["stood", "fresh"] {
                let n = m.entry(k).neighbours.len() as u32;
                m.entry(k).connections = n + 1;
            }
            m
        };

        // `stood` is nearer by key order and has been walked; `fresh` has not.
        let mut m = build();
        m.entry("stood").visited = true;
        let plan = m.next_target().expect("a plan");
        assert_eq!(plan.reason, Goal::Heart, "the walk is still the heart errand");
        assert_eq!(
            plan.target, "fresh",
            "a node we have already stood on has no more roads to give us"
        );

        // **And since #73 it is a refusal, not a preference.** This asserted the opposite until
        // 2026-08-21: with nothing else left, the visited node was still nominated, on the dev's
        // rule of 2026-08-15 that the navigator probes rather than concluding from an incomplete
        // cache.
        //
        // The 1519Z run is why it changed. A ranking only decides between candidates, and the
        // ordinary case has **one** — `l11` against `l13` went three full laps with this very
        // preference in the binary. Concluding *after* a visit is the 2026-08-15 responsibility
        // discharged, not dodged: standing there is the probe, and it came back empty.
        let mut m = build();
        m.entry("stood").visited = true;
        let n = m.entry("fresh").neighbours.len() as u32;
        m.entry("fresh").connections = n;
        assert_ne!(
            m.next_target().map(|p| p.reason),
            Some(Goal::Heart),
            "every road has been walked, so the errand stops rather than re-walking one"
        );
    }

    /// Not a crypt, and still a fight — the four nodes the first rule waved through.
    ///
    /// Rebuilt from the route the run of 2026-08-15 took out of `shrine1`: a bandit camp, a spider
    /// forest and a graveyard, chasing a shrine while the only corrupted node it could reach sat one
    /// hop the other way.
    #[test]
    fn a_bandit_camp_is_a_fight_even_though_it_is_not_a_crypt() {
        for heading in [
            "Norton Firth — level 3 bandit camp",
            "Sancton Regrow — level 3 spider forest",
            "Fosholme Growth — level 5 graveyard",
            "Swanland — level 6 shrine",
        ] {
            let mut m = WorldMap::new();
            m.fold(&dump("here", "camp", vec![node("mid", heading)]));
            m.fold(&dump("mid", heading, vec![node("s1", "Faraway shrine")]));
            m.here = Some("here".into());
            m.hell = Some(0.1);
            assert!(
                !m.reachable_without_a_fight("here", "s1"),
                "`{heading}` is a fight on the way, whatever it is called"
            );
        }
    }

    /// And the shrine's own node counts, which is why the destination is not exempt.
    #[test]
    fn a_shrine_we_would_have_to_fight_for_is_not_free_either() {
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("s1", "Swanland — level 6 shrine")]));
        m.here = Some("here".into());
        m.hell = Some(0.1);
        assert!(!m.reachable_without_a_fight("here", "s1"), "the shrine itself is the fight");
        m.entry("s1").completed = true;
        assert!(m.reachable_without_a_fight("here", "s1"), "and cleared, it is free again");
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
        // **What this measures changed on 2026-08-16 and the game fact behind it did not.**
        // Consecrating still needs `hell ~= 0`, so a run cannot finish a shrine before the portal
        // opens — but it was never this branch that enforced that, and the shrine branch below is
        // deliberately reachable with the portal shut, because `Pray` is available there and worth
        // having. What used to keep the run off shrines early was `OpenAnomaly` outranking them,
        // and the dev's rule has queued the trigger behind every node under level 4 — which a
        // shrine, carrying no level at all, is.
        //
        // So the assertion is the half that is still this test's own: the trigger is taken as soon
        // as nothing gentler is left, and not before.
        assert_ne!(
            m.next_target().unwrap().reason,
            Goal::OpenAnomaly,
            "a level 4 forest is not the first stop while an unwalked shrine stands"
        );
        m.entry("shrine2").visited = true;
        m.entry("shrine2").hidden = Some(0);
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::OpenAnomaly);
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

    /// The shortfall and the fight it prices have to agree, or the line is no better than the one
    /// it replaced. `16` is `2 x 8 - 0` and equally `2 x 9 - 2`.
    #[test]
    fn the_deepest_fight_is_the_one_the_shortfall_is_priced_against() {
        let mut m = WorldMap::new();
        m.entry("start").heading = "Cottam — level 8 anomaly".into();
        m.entry("l61").heading = "Meaux — level 7 crypt".into();
        m.entry("l23").heading = "Smithy — level 3 crypt".into();
        assert_eq!(m.deepest_fight(), Some(("start", 8)));
        assert_eq!(m.stacks_short_ahead(), crate::rest::stacks_wanted(8));

        // Clearing it moves both together, which is the property that makes them comparable.
        m.entry("start").completed = true;
        assert_eq!(m.deepest_fight(), Some(("l61", 7)));
        assert_eq!(m.stacks_short_ahead(), crate::rest::stacks_wanted(7));

        // And a map with nothing deep enough says so rather than naming a fight it is not pricing.
        m.entry("l61").completed = true;
        assert_eq!(m.deepest_fight(), None, "level 3 is below the banking floor");
        assert_eq!(m.stacks_short_ahead(), 0);
    }

    /// The 1519Z line, reconstructed: `16 stack(s) short` came from an empty bank against the
    /// level 8 anomaly, and the number alone cannot say that.
    #[test]
    fn an_empty_bank_and_a_level_eight_anomaly_are_the_sixteen_the_run_printed() {
        let mut m = WorldMap::new();
        m.entry("start").heading = "Cottam — level 8 anomaly".into();
        assert_eq!(m.well_rested, 0);
        assert_eq!(m.stacks_short_ahead(), 16);
        // The save's own reading is what settles it, and 17 banked is not 16 short.
        m.well_rested = 17;
        assert_eq!(m.stacks_short_ahead(), 0, "a full bank wants nothing");
    }

    /// **#73's probe magnet is gone, measured against the two runs it caught.**
    ///
    /// `l11 Argham crossroads` declares more roads than any dump names, so
    /// `connections > neighbours` held there **permanently** and standing on it could not close the
    /// gap. `next_target` excludes `here`, so the pull vanished on arrival and reappeared one node
    /// away: the dev watched the run walk `l11 -> l13 -> l11 -> l13` and reported it twice. The fix
    /// (`1fc548e`) makes a visited node stop counting as unexplored.
    ///
    /// Replayed here: fold each run's surface dumps and ask the **new** planner what it would do at
    /// each position the game really produced. `l11` appears in no immediate reversal in either
    /// run, against four in each run as they actually happened.
    ///
    /// ## What this replay cannot see, which is more than the crossing one
    ///
    /// It never calls `apply_save`, so completions, consecrations, gold and health are absent and
    /// the planner keeps re-targeting things the run had already finished. The replay's *own*
    /// reversals are all of that kind — `l43 -> shrine3` then `shrine3 -> l43`, a shrine it thinks
    /// is still unconsecrated — or ordinary goal-completion turns like `l37 -> e4` then
    /// `e4 -> l12`. So this asserts the narrow thing it can support and not "the planner never
    /// reverses", which would be false and would deserve to be.
    #[test]
    fn the_crossroads_no_longer_pulls_the_run_back_and_forth() {
        let is_reversal = |a: &(String, String), b: &(String, String)| a.0 == b.1 && a.1 == b.0;
        let mut runs = 0;
        for stem in ["spike-run-20260821-1519Z", "spike-run-20260821-0313Z"] {
            let Ok(log) = std::fs::read_to_string(format!("{stem}.log")) else {
                eprintln!("SKIP: {stem}.log is not present");
                continue;
            };
            let lines: Vec<String> = log.lines().map(|l| l.to_string()).collect();
            let dumps = crate::observe::adjacency::Reader::new().push(&lines);
            assert!(dumps.len() > 100, "{stem}: expected a whole run");

            let mut m = WorldMap::new();
            let mut seq: Vec<(String, String)> = Vec::new();
            for a in &dumps {
                m.fold(a);
                if a.subworld.is_some() {
                    continue;
                }
                let Some(h) = m.next_hop() else { continue };
                if h.step == a.here_key {
                    continue;
                }
                let d = (a.here_key.clone(), h.step.clone());
                if seq.last() != Some(&d) {
                    seq.push(d);
                }
            }
            assert!(seq.len() > 20, "{stem}: only {} surface decisions to judge", seq.len());
            assert!(
                seq.iter().any(|(f, _)| f == "l11"),
                "{stem}: the run never stood on `l11`, so this proves nothing about the magnet"
            );

            let magnetic: Vec<_> = seq
                .windows(2)
                .filter(|w| is_reversal(&w[0], &w[1]))
                .filter(|w| w[0].0 == "l11" || w[0].1 == "l11")
                .collect();
            assert!(
                magnetic.is_empty(),
                "{stem}: `l11` is still pulling the run back and forth: {magnetic:?}"
            );
            runs += 1;
        }
        if runs == 0 {
            return;
        }

        // The control: the runs did bounce there, four times each, and the report says so.
        let Ok(report) = std::fs::read_to_string("spike-run-20260821-1519Z.md") else {
            eprintln!("SKIP the control: the report is not present");
            return;
        };
        let mut hops: Vec<(String, String)> = Vec::new();
        for l in report.lines() {
            // `NN. from -> **to** (for target, Reason)`
            let Some((_, rest)) = l.split_once(". ") else { continue };
            let Some((from, rest)) = rest.split_once(" -> **") else { continue };
            let Some((to, _)) = rest.split_once("**") else { continue };
            if from.contains(' ') || to.contains(' ') {
                continue;
            }
            hops.push((from.to_string(), to.to_string()));
        }
        let bounced = hops.windows(2).filter(|w| is_reversal(&w[0], &w[1])).count();
        assert!(
            bounced >= 4,
            "the 1519Z report should show the `l11` bounce; found {bounced} reversals in {} hops, \
             so this detector is measuring nothing",
            hops.len()
        );
    }
}
