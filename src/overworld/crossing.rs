//! Getting **through** a subworld: which door to leave by, and which node to stand on next.
//!
//! Split out of `overworld.rs` on 2026-08-21 (#76). A crossing is its own problem and reads nothing
//! like surface planning. On the surface any reachable node is one `Travel` press away, because the
//! game path-finds for us; inside a village or a forest the interior is a fresh little graph that
//! the fog reveals a node at a time, the exits print their positions but never their keys, and the
//! errand we came in for may be behind ground we have not seen yet.
//!
//! #18 (generalise the errand list) and #57 (a per-container frame) both land here.
//!
//! ## One walk, not two arms
//!
//! There used to be a *steer* alongside the frontier walk, and the two could disagree about which
//! node was a candidate at all — six of seven crossings in the 1519Z run ended on the steer, and
//! three of that run's crossings bounced between a pair of nodes each nominating the other. They
//! are now one ranked walk with the door's position as a term; see [`WorldMap::cross_toward`] for
//! the ranking and `the_one_walk_crossing_nominates_no_pair_that_nominates_it_back` for the
//! measurement against both archived runs.
//!
//! ## The door is held, not re-derived
//!
//! [`WorldMap::committed_exit`] exists because re-ranking the exits from every new vantage point is
//! its own bounce: each step reveals a little more and flips the answer. The choice is made once
//! and kept for the visit.

use super::{
    dist_or_far, exit_node_key, heading_has_combat, key_is_major_shrine, Door, Goal, Place, Risk,
    WorldMap,
};
use std::collections::BTreeSet;

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
    ///
    /// **Named `Probe`, not `Explore`, and that is not cosmetic.** It shared a name with
    /// [`Goal::Explore`] while meaning something unrelated — one is an overworld objective ("nowhere
    /// in particular to be"), the other a step inside a subworld whose exit we cannot yet route to.
    /// A run pursuing the shrine with every hop labelled `RouteTo(Shrine)` still printed
    /// `exploring \`l16\` via \`l16sub28\`` on the lines in between, which reads as the objective
    /// having been abandoned. The dev raised the collision twice; it was argued down once and
    /// re-proposed as a fresh idea the second time, which is worse than either.
    Probe { to: String, toward: String },
    /// Standing on the interior destination — the inn — with the errand still to do.
    ///
    /// The counterpart of [`Crossing::Leave`] for a destination that is *inside* rather than out.
    Arrive { at: String },
    /// Looking for a destination we have not seen yet. Move here to open the fog.
    ///
    /// Distinct from [`Crossing::Probe`], which knows where it is going and is only short of a
    /// route. This one has no target at all, so it must not be told to head for an exit: leaving is
    /// precisely the thing that would abandon the errand. Its own variant rather than a flag,
    /// because `Step` and `Probe` already log identically and a third silent case would make an
    /// old log unreadable.
    Seek { to: String },
    /// Standing on the exit road: leave for this overworld node.
    ///
    /// There was a `Retreat` variant here — hurt, the way onward is a fight, so go back the way we
    /// came. It is gone. Backing out was legal (`canTravelToDirect` needs only one endpoint
    /// complete, and the node behind us is complete by definition) and it still cost more than it
    /// saved: one cycle, one guard that could never fire, and a run stopped in front of a level 1
    /// nest. `WorldMap::cross_toward` carries the full account. The MVP sticks to the path.
    Leave { to: String },
}

/// Is this place somewhere **inside** `container` that an errand could be waiting at?
///
/// The parent test alone is not enough, and that cost a run. An exit carries the container as its
/// parent — the dump builds its key as `parent.key..'_path_to_'..k` (`overworldview.lua:1043`) —
/// and its heading reads `Road to <wherever it goes>`. So `l32_path_to_shrine2`, the road out of
/// Enthorpe toward Gransmoor shrine, **ends in the word `shrine`**, and [`Place::is_shrine`]
/// answers by heading as well as by key.
///
/// Live 2026-08-22 0203Z: [`WorldMap::shrine_inside`] named that road as the errand inside `l32`,
/// which made `errand_at` a place we reach by *leaving*. `cross_toward` walked to it, the run left
/// the village it had entered to shop in, and from outside the village was again the nearest place
/// to buy a heart. Four laps of `l32` -> `shrine2` -> `l32` before the loop guard stopped it, with
/// `seeking_a_heart("l32")` reading **true** the whole time — the heart guard was right and never
/// got asked, because `errand_at.is_some()` is tested first.
///
/// The surface picker has the same hazard and guards it with `parent.is_none()` — see
/// `pick_shrine`. That is not available here: an errand inside a container is a child by
/// definition. So it is the way *out* that has to go.
fn inside_container(p: &Place, container: &str) -> bool {
    p.parent.as_deref() == Some(container) && !p.key.starts_with(&format!("{container}_path_to_"))
}

/// What a [`Crossing::Seek`] is looking for, since it is looking for something.
///
/// A `Seek` is a walk with **no destination**, and there are four ways to be in that state: the
/// three errands whose target the fog is still hiding, and the crossing whose way out we have not
/// seen. They are the four guards that send `cross_toward` exploring — see the `leaving_to` match —
/// and this reports which of them fired, in the same order, so the log can name it.
///
/// **The log used to name two of the four**, and everything else fell into the exit wording. Live
/// 2026-08-23 inside Boreas, on a `Heart` errand hunting the general store: *no way out of `l59` in
/// sight — probing via `l59sub3`*, for twelve steps, which is what made the route read as a failed
/// search for the exit. It was a shop search and it found the shop.
///
/// The wording of the pair that *was* covered has already cost a diagnosis once: reporting a fogged
/// forest crossing as `searching e1 for its inn` had the run looking for a bar in the woods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Searching {
    /// A village whose inn the fog still hides, on a rest errand.
    Inn,
    /// A village whose general store the fog still hides, on the heart errand.
    Store,
    /// A shrine forest whose plaza has never been drawn — which for an unexplored one is every
    /// visit, since `isRevealed` needs `<key>_plaza_explored` (`shrine_forest_raw.lua:5`).
    Shrine,
    /// No errand: a way out we have not seen yet, which in a lost woods is every exit.
    Exit,
}

impl Searching {
    /// The thing being looked for, as the log says it. `Exit` has no phrase here because its line
    /// is shaped differently — there is no *`container`'s* exit, only no way out in sight.
    pub fn what(self) -> &'static str {
        match self {
            Searching::Inn => "its inn",
            Searching::Store => "its general store",
            Searching::Shrine => "its shrine",
            Searching::Exit => "a way out",
        }
    }
}

impl WorldMap {
    /// Are we currently inside a subworld, and if so which one?
    pub fn inside(&self) -> Option<&str> {
        let here = self.here.as_deref()?;
        self.places.get(here)?.parent.as_deref()
    }

    /// **Ask the question from outside the forest.**
    ///
    /// `next_target` plans from `self.here`, and while we are inside a subworld `here` is an
    /// interior node. Nothing links an interior node to its container in this map — the two are
    /// separate components, as [`WorldMap::exit_toward`]'s doc has said all along — so every
    /// surface place is unroutable, `plan` returns nothing, and the ranked branch of
    /// [`WorldMap::choose_exit`] is skipped. What is left is `SafestOfWhatIsLeft`: a door
    /// picked for safety with **no reference to where we are going**.
    ///
    /// That is not a rare degenerate case, it is what happens every single time we cross a
    /// subworld, and it cost three runs on 2026-08-15. From inside `l62` it chose the exit to
    /// `shrine7` — a dead end already consecrated, prayed at and finished — walked there, turned
    /// round, re-entered the forest, and then chose `l40` to the south-east while `l57` to the
    /// south-west stood open on a road already walked and reaching every remaining shrine.
    ///
    /// The container is a surface node with surface edges, and it is where we will be standing
    /// the moment we step out. So plan from there. Every exit's `to_key` is a surface node too,
    /// which is why that ranking works at all once it is given a target it can measure.
    ///
    /// ## The whole map, with one thing changed. It used to be six fields, and that was the
    /// ## `l10` <-> `l18` ping-pong
    ///
    /// The list was `places`, `abandoned`, `roads_done`, `hell`, `wants_rest`, `gold`. Everything
    /// else took `Default`, and the one that mattered was **`heart_bought`** — the record of
    /// which general stores we have already emptied.
    ///
    /// So the planner, asked from inside Ulrome, was asked by a map that had forgotten every
    /// heart the run ever bought. With 294 gold in hand `wants_a_heart` was true, the nearest
    /// settlement with a shelf was `l32`, and the answer came back `Heart -> l32` — whose nearest
    /// door is `l18`. Outside, the real map remembered, answered `CloseAnomaly -> start`, and
    /// sent us back into the village. Three ping-pongs in two days, and the run of
    /// 2026-08-16 0602Z shows the two answers alternating on consecutive lines:
    ///
    /// ```text
    /// 7. l18 -> **l10** (for start, CloseAnomaly)
    /// 9. crossing `l10` toward `l10_path_to_l18` … door choice: Heart -> l32
    /// ```
    ///
    /// A hand-written field list is a promise to remember every future field, and this file has
    /// over twenty. Copying the map entire keeps that promise by construction; the vantage point
    /// is the only thing that should differ, so it is the only thing set.
    ///
    /// `here` being a surface node is what makes it "outside" — `inside()` reads the parent of
    /// `here`, and a container has none.
    pub fn plan_from_out_here(&self) -> Option<crate::overworld::Plan> {
        match self.inside() {
            Some(container) => {
                let mut m = self.clone();
                m.here = Some(container.to_string());
                m.next_target()
            }
            None => self.next_target(),
        }
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
    ///
    /// ## The ranking below mostly does not run, and that is measured
    ///
    /// `distances(target)` reaches **no surface node from inside a subworld**: nothing links a
    /// subworld node to its container, so the interior and the overworld are separate components of
    /// our graph. The `dist.contains_key` filter therefore empties, and the answer comes from the
    /// fallback at the bottom — *the first exit that is not the entrance*, which is to say whichever
    /// road the current pan happened to print first.
    ///
    /// That is why [`WorldMap::cross_toward`] asks this **once** per crossing and then holds the
    /// answer in [`WorldMap::crossing_to`]. Exits print only while visible
    /// (`overworldview.lua:1044`), so re-asking every step let fog reverse a crossing and reverse it
    /// back. See `docs/superpowers/notes/navigation-loops.md`.
    ///
    /// Connecting the two components is the real fix and is not attempted here — it changes what
    /// every distance in this file means, including `next_target`'s. Until then, read the ranking as
    /// what happens on the surface, and the fallback as what happens while crossing.
    pub fn exit_toward(&self, exits: &[crate::observe::adjacency::Exit]) -> Option<String> {
        self.choose_exit(exits).map(|(k, _, _)| k)
    }

    /// [`WorldMap::exit_toward`], and **which of its three answers produced the door**.
    ///
    /// Split out because the log could not say. Three live runs in a row turned on which branch had
    /// chosen an exit — the ranked one, the risk fallback, or the entrance rule — and every time the
    /// only way to find out was to reconstruct the map by hand from `spike-run-raw.log` and reason
    /// about what it must have done. Twice that reasoning was wrong.
    ///
    /// A decision worth making is worth being able to read afterwards. `Door` is carried to
    /// [`WorldMap::door_reason`] and printed with the crossing.
    fn choose_exit(
        &self, exits: &[crate::observe::adjacency::Exit],
    ) -> Option<(String, Door, String)> {
        if exits.is_empty() {
            return None;
        }
        let entrance = self.entered_from.clone();
        // **A door that has already handed us back is not a door.**
        //
        // [`WorldMap::refuse_door`] carries the l31/l47 ping-pong this exists for. Applied as a
        // filter on the exits rather than as a term in the ranking, for the reason the whole
        // note there gives: the ranking was not wrong, it was measuring the wrong thing — and a
        // memory that competes with a distance is a memory that loses to it.
        //
        // Dropped entirely if it would leave nothing, exactly as the level-4 gentle pass is:
        // being inside a container with no door named is worse than any one door.
        let container = self.inside().map(str::to_string);
        let kept: Vec<_> = match &container {
            Some(c) => {
                exits.iter().filter(|e| !self.door_is_refused(c, &e.to_key)).cloned().collect()
            }
            None => exits.to_vec(),
        };
        let exits: &[crate::observe::adjacency::Exit] = match kept.is_empty() {
            true => exits,
            false => &kept,
        };
        // **Ask the question from outside the forest** — see [`WorldMap::plan_from_out_here`],
        // which is the whole of why this is not `self.next_target()`.
        //
        // **The errand is kept, not just the destination.** It decides whether the anti-backtracking
        // rule below is allowed to overrule a measured answer — see there.
        let plan = self.plan_from_out_here();
        let errand = plan.as_ref().map(|p| p.reason.clone());
        let target = plan.map(|p| p.target);
        // **The whole ranking, written down before anything is chosen from it.** See the
        // `door_note` field: the reason alone could not answer "was the better door even ranked",
        // which is the question the run of 2026-08-16 left open.
        let mut note = match (&errand, &target) {
            (Some(g), Some(t)) => format!("{g:?} -> {t}"),
            _ => "no errand".to_string(),
        };
        // **Pre-anomaly, what is on the other side of a door has to be under level 4.**
        //
        // The dev's rule, 2026-08-16, and it is the level 4 rule (#44) finally being asked at the
        // place that decides. `gentler_ground_remains` governs which surface node
        // [`WorldMap::next_target`] may *nominate*; nothing governed which door we left by, and a
        // crossing picks its own way out. So the run of 2026-08-16 obeyed the rule at every
        // destination it chose — `l19`, `l18`, `l32`, `l25`, `l4`, `l20`, all level 2 or under —
        // and then walked out of a village through a level 5 crypt, because the door ranking had
        // never heard of it.
        //
        // Two escapes, both necessary:
        //
        // - **the portal is already open**, when there is nothing left to preserve and high level
        //   *is* the corruption we are heading for;
        // - **the target itself is the trigger**, which is [`Goal::OpenAnomaly`] doing its job. A
        //   rule that forbade the door to the node we have decided to open the anomaly at would
        //   deadlock the errand it is meant to sequence.
        //
        // The heading is the whole test. A `!completed` clause was tried here and removed — the note
        // where [`Place::opens_the_anomaly`] used to be says why, and the short version is that the
        // state it protected cannot occur while `hell == 0`.
        let gentle_doors_only = !self.anomaly_is_open().unwrap_or(false)
            && !target
                .as_deref()
                .and_then(|t| self.places.get(t))
                .map(|p| p.triggers_anomaly())
                .unwrap_or(false);
        let opens_the_portal = |to: &str| {
            gentle_doors_only && self.places.get(to).map(|p| p.triggers_anomaly()).unwrap_or(false)
        };
        if gentle_doors_only {
            note.push_str("; gentle doors only");
        }
        if let Some(target) = target.as_ref() {
            let dist = self.distances(target);
            {
                let mut seen: Vec<String> = exits
                    .iter()
                    .map(|e| match dist.get(&e.to_key) {
                        Some(d) => format!("{}={d}", e.to_key),
                        None => format!("{}=unmeasured", e.to_key),
                    })
                    .collect();
                seen.sort();
                note.push_str(&format!("; doors {}", seen.join(" ")));
            }
            // A door whose road we have written off is not a door. `abandoned` is the driver's
            // record of having had its go, and it is recorded against the *interior road node*
            // (`l9_path_to_l1`), not against the surface node beyond it — so ranking by surface
            // distance alone walks straight back onto a road we already gave up on. The safety
            // fallback happened to respect this and the ranked branch did not, which stayed hidden
            // for as long as the ranked branch never ran.
            let written_off = |to: &str| {
                self.inside()
                    .map(|c| self.abandoned.contains(&exit_node_key(c, to)))
                    .unwrap_or(false)
            };
            // **A tie between doors was being broken by the order the game printed them in.**
            //
            // `min_by_key` returns the *first* minimum, and the exits arrive in whatever order
            // `verboseAdjacencyData` walked them. Live 2026-08-16, leaving Colden Brake for Aike
            // town: `door choice: Heart -> l25; doors l20=1 l38=2 l41=1`. `l20` is a level 2 crypt
            // we had already cleared and `l41` is a level 5 crypt we had not; both one hop from the
            // target; the dump printed `l41` first, so `l41` won, and arriving there opened the
            // anomaly. That is the alphabetical-bed bug of 2026-08-15 one layer down — an arbitrary
            // order standing in for a preference nobody wrote.
            //
            // `Risk` is the preference, and it is already the fallback branch's first key when we
            // want a rest. Here it is strictly a tie-break: distance still decides, and safety only
            // speaks when distance has nothing left to say. `Free < Forest < Fight < Unseen <
            // Corrupt`, so a cleared node beats an unfought one and either beats a corrupted one.
            let ranked_by = |gentle: bool| {
                exits
                    .iter()
                    .filter(|e| dist.contains_key(&e.to_key) && !written_off(&e.to_key))
                    .filter(|e| !gentle || !opens_the_portal(&e.to_key))
                    .min_by_key(|e| {
                        let risk =
                            self.places.get(&e.to_key).map(|p| p.risk()).unwrap_or(Risk::Unseen);
                        // **The entrance goes last among equals** — a tie-break, never an override.
                        //
                        // Live 2026-08-22 2002Z, entering `l4` for `shrine7`:
                        // `doors l10=16 l1=16 l25=18 shrine1=19`. `l1` and `l10` tie, and both
                        // remaining terms then preferred `l1` — the door we had walked in through
                        // ninety seconds earlier. `risk` chose it because we had *just cleared it*,
                        // so it ranked `Free` against an uncleared village; `&e.to_key` would have
                        // chosen it too, `"l1" < "l10"`. Two independent preferences, both pointing
                        // at the way back. The crossing stepped straight out, the planner sent us
                        // back in, and the run ended `Looping("l4 visited 4 times with no
                        // progress")`.
                        //
                        // **Below distance, and that is the whole design.** The override further
                        // down is gated to `Goal::Explore` because overruling a *measured* distance
                        // walked a run away from the anomaly on 2026-08-16. This cannot do that: if
                        // the entrance is genuinely nearer it wins on the first term and never
                        // reaches this one. At a tie it cannot be better — going back out undoes the
                        // move we just made — so no errand gate is needed, and the fallback branch
                        // below has ranked it last this way all along.
                        let back_out = Some(&e.to_key) == entrance.as_ref();
                        (dist_or_far(&dist, &e.to_key), back_out, risk as i64, &e.to_key)
                    })
            };
            // **The gentle pass first, then the same ranking with the rule lifted.**
            //
            // Lifted rather than fatal, for the reason the `written_off` filter is dropped when it
            // empties: being inside a subworld with no door named is worse than any one door. A
            // village whose every exit is a level 4+ node has to be left by one of them.
            if let Some(best) = ranked_by(true).or_else(|| ranked_by(false)) {
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
                //
                // ## And that argument is only true while exploring, which is why it is gated now
                //
                // **It killed a run on 2026-08-16.** The anomaly is `start`, the adventure *begins*
                // at `start`, so the objective was a node we had stood on and every hop out of it
                // was recorded: from Stillingfleet, `distances` put `l39` — the way we came in — at
                // four hops from the portal and `l60` at six. The measured answer was to turn round.
                // This overrode it because `l60` was unvisited, and `l60` was a level 7 crypt with a
                // six-deep queue. `spike-run-20260816-0226Z.md` step 38, and the run ends on step 39.
                //
                // The dev, on that run: *we wandered farther away from the anomaly as opposed to
                // towards it.* Exactly so, and the log said `not back out the way we came` at every
                // crossing after the portal opened while `nearest to the target` had been firing
                // before it.
                //
                // So the override now applies only under [`Goal::Explore`], which is the errand its
                // own justification is written about. With a *named* destination — the anomaly, a
                // bed, a heart, a shrine — a measured distance is the whole point of having
                // measured, and refusing it because the door is familiar is how a run walks away
                // from its objective.
                let exploring = matches!(errand, Some(Goal::Explore) | None);
                if exploring && Some(&best.to_key) == entrance.as_ref() {
                    if let Some(onward) = exits.iter().find(|e| {
                        Some(&e.to_key) != entrance.as_ref()
                            && !self.places.get(&e.to_key).map(|p| p.visited).unwrap_or(false)
                    }) {
                        return Some((onward.to_key.clone(), Door::NotBackOutAgain, note));
                    }
                }
                return Some((best.to_key.clone(), Door::NearestToTarget, note));
            }
        }
        // No target, or none of the exits lead anywhere we can measure.
        //
        // ## This branch runs far more often than it looks like it should, and it used to pick the
        // ## first exit in the dump
        //
        // "None of the exits lead anywhere we can measure" is not an edge case. Places learned from
        // `completedAreas` are recorded **by key, with no edges** — `entry(&k)` and nothing else —
        // so `distances(target)` for any node we have not stood on this run is the singleton
        // `{target: 0}` and the filter above empties. The anomaly is exactly such a node: its key
        // comes from the save, and a fresh launch has walked nowhere, so the map has almost no
        // surface edges at all.
        //
        // Live, 2026-08-09: standing in Broach Copse with the anomaly open at `start`, the exits
        // were `l38` (level 5 crypt), `l54` (level 6 forest), `l26` (a mausoleum we had already
        // cleared) and `e1` (level 2 forest). Distance said nothing, so this took the first — `l38`
        // — and the run walked a healthy character out of a village into a level 5 crypt, where the
        // fight ran nine turns and then ran out of tiles. The cleared node was in the list.
        //
        // So when distance cannot speak, **rank by what a fight costs**, which we always know from
        // the heading. `Risk` is safest-first (`Free < Forest < Fight < Unseen < Corrupt`), which
        // puts a cleared exit ahead of an unfought one and a forest ahead of a crypt. Still avoiding
        // the entrance where there is any alternative, since that part was never the problem.
        //
        // ## Then which way the target actually lies
        //
        // `Risk` alone still leaves a coin toss between equally safe doors, and it was the dev who
        // pointed out we already have the answer and throw it away: every dump prints each node's
        // position, and [`WorldMap::gap`] turns those into straight-line distance. So among doors
        // that cost the same, take the one whose far side lies nearest the target.
        //
        // **Below `Risk` on purpose.** A heuristic that knows direction and not terrain will happily
        // point through a level 6 crypt because the objective is on the other side of it, and this
        // fallback runs precisely when we have no route to check that against. Safety first, then
        // direction, then a stable key so the choice never flaps. Promoting direction above risk is
        // a tuning decision, and it wants evidence from a run rather than an argument.
        //
        // Doors with no position sort last among their risk band rather than first: an unplaced node
        // is unmeasured, and guessing it is nearby is exactly the move that put a run in a crypt.
        //
        // Abandoned roads drop out first. Without this the memory in [`WorldMap::committed_exit`]
        // was decorative: it releases a commitment whose road we have written off, and then this
        // re-derived the very same road and committed to it again. A tabu list nothing consults is
        // not a tabu list. The `filter` is dropped entirely if it would leave nothing, because being
        // inside a subworld with no way out named is worse than any one door.
        let parent = self.inside();
        let live: Vec<&crate::observe::adjacency::Exit> = exits
            .iter()
            .filter(|e| {
                parent
                    .map(|p| !self.abandoned.contains(&exit_node_key(p, &e.to_key)))
                    .unwrap_or(true)
            })
            .collect();
        let mut ranked: Vec<&crate::observe::adjacency::Exit> =
            if live.is_empty() { exits.iter().collect() } else { live };
        // **And here too, because this branch runs more often than the ranked one.**
        //
        // Distance can say nothing about a node we have never stood on, which is most of them, so
        // "unranked, fell back on risk and bearing" is the ordinary case rather than the exceptional
        // one — the comment above this branch says as much. A rule that only guarded the measured
        // path would be a rule that mostly did not run. Same shape as everything else here: applied
        // when it leaves something, dropped when it would leave nothing.
        let gentle: Vec<&crate::observe::adjacency::Exit> =
            ranked.iter().copied().filter(|e| !opens_the_portal(&e.to_key)).collect();
        if !gentle.is_empty() {
            ranked = gentle;
        }
        // `f64` has no `Ord`, so the heuristic is bucketed into integers rather than compared as a
        // float. Rounding is harmless here — the question is which door points the right way, not
        // by how many pixels — and it keeps the whole key sortable and total.
        let toward = |k: &String| -> i64 {
            target.as_ref().and_then(|t| self.gap(k, t)).map(|d| d as i64).unwrap_or(i64::MAX)
        };
        // **Safety outranks direction only while we want a rest.**
        //
        // The dev's ruling, given for destination choice and repeated here for doors: risk ordering
        // is what a hurt run needs, and it is counterproductive on the way to the anomaly, because
        // corruption *is* high node levels — ranking those last points the run away from the one
        // place it is trying to reach. The comment this replaces called promoting direction "a
        // tuning decision that wants evidence from a run rather than an argument". Three runs on
        // 2026-08-15 supplied it: safest-first walked out of `l62` to a finished dead end, and then
        // out again by the south-east door while the south-west one stood open toward everything
        // that was left.
        //
        // So the two keys swap on `wants_rest`, and nothing else about the ordering changes: the
        // entrance is still last, and the key still breaks ties so the choice cannot flap.
        ranked.sort_by_key(|e| {
            let risk = self.places.get(&e.to_key).map(|p| p.risk()).unwrap_or(Risk::Unseen) as i64;
            let dir = toward(&e.to_key);
            let (first, second) = match self.wants_rest {
                true => (risk, dir),
                false => (dir, risk),
            };
            (Some(&e.to_key) == entrance.as_ref(), first, second, &e.to_key)
        });
        let best = ranked.first()?;
        let why = match toward(&best.to_key) {
            i64::MAX => Door::SafestOfWhatIsLeft,
            _ => Door::TowardTheCorruption,
        };
        note.push_str("; unranked, fell back on risk and bearing");
        Some((best.to_key.clone(), why, note))
    }

    /// The door this crossing already chose, if it is still worth believing in.
    ///
    /// Single-minded commitment: held until achieved or believed impossible. *Achieved* is handled
    /// elsewhere — leaving the subworld clears [`WorldMap::crossing_to`] in `fold`. Everything here
    /// is the "believed impossible" half, and each clause is deliberately narrow, because a
    /// commitment that lapses easily is not one.
    ///
    /// Three things drop it:
    ///
    /// 1. **The errand changed.** `errand` is the caller's current [`Goal`], and a commitment made
    ///    while exploring says nothing about where to go once we are hurt. This is the clause that
    ///    keeps boldness from becoming blindness — see [`WorldMap::crossing_to`].
    /// 2. **We wrote the road off.** `abandoned` is the driver's own memory of having tried
    ///    something, so an abandoned road out is one we have genuinely given up on.
    /// 3. **The plan names a different door of this same container** — #51, below.
    ///
    /// Note what does **not** drop it. Not "the exit is missing from the current dump": exits print
    /// only while visible (`overworldview.lua:1044`), so an absent exit is the ordinary state of a
    /// subworld we have just walked into, and treating that as impossibility would discard the
    /// commitment exactly when it is doing the most work. And not "we know no route to it": fog
    /// means no route is the *first* thing we know about anywhere, which is why `cross_toward` has a
    /// frontier fallback for walking toward a destination it cannot yet reach.
    ///
    /// ## #51: the door held while the target moved, and why the fix is this narrow
    ///
    /// `crossing_to` stores `(door, goal)` and no target, so two destinations under one `Goal` share
    /// a commitment. The 1043Z run of 2026-08-16 spent ten steps inside `l4` with every crossing line
    /// reading `l4_path_to_l1` while the errand read `Heart -> l4_path_to_l10` — a door the log itself
    /// called "not on any route we know", which is why the crossing fell through to the frontier walk
    /// and looped.
    ///
    /// The obvious repair — drop the commitment whenever the target changes — is wrong, and the
    /// diagnostic (`5de3917`) proved it on the first run that carried it. Inside `l21` the target
    /// churned `l21sub7`, `l21sub9`, `l21sub8`, `l21sub10` step by step while the door stayed `e1`,
    /// and **that churn is exactly what a commitment exists to ignore**: the plan is recomputed from
    /// `here`, `here` changes every step of a crossing, so a target that moves with us would re-derive
    /// the door every step and lose the single-mindedness the `HELD` marker was added for.
    ///
    /// What separates the two is not how far the target is or whether it is inside — `l4_path_to_l10`
    /// is *also* a child of the container, so "outside the subworld" would have missed it. It is that
    /// one target **is a door of this container and is not the one we committed to**, which is a
    /// direct contradiction rather than a wandering, and the only shape of target that can be one.
    fn committed_exit(
        &self, parent: &str, errand: Option<&Goal>, target: Option<&str>,
    ) -> Option<String> {
        let (to, goal) = self.crossing_to.as_ref()?;
        if errand != Some(goal) || self.abandoned.contains(&exit_node_key(parent, to)) {
            return None;
        }
        let committed = exit_node_key(parent, to);
        let contradicted =
            target.is_some_and(|t| t.starts_with(&format!("{parent}_path_to_")) && t != committed);
        match contradicted {
            true => None,
            false => Some(to.clone()),
        }
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
    ///
    /// Takes `&mut self` because choosing a door is also *recording* that we chose it — see
    /// [`WorldMap::crossing_to`]. Deriving the destination and remembering it cannot be separated
    /// without the caller re-deriving it, which is the bug.
    pub fn cross_toward(&mut self, exits: &[crate::observe::adjacency::Exit]) -> Option<Crossing> {
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
        // **What we came in for**, which is task #18 done rather than approached: see
        // [`WorldMap::errand_inside`]. The general store, the inn, an unopened chest and an unprayed
        // shrine all share this path, because *reaching* them is identical. What happens on arrival
        // is the driver's business and differs for each — see `errand_inside` for which of them
        // reach `Crossing::Arrive` and which are answered elsewhere.
        // **A shelf we can see outranks the bed; a shelf we cannot does not.**
        //
        // The dev, 2026-08-16: *once we are in a settlement with that goal, buy as many healthBuffs
        // as possible while still topping up your health at the inn before we leave the settlement.*
        //
        // The order matters for one reason and it is arithmetic: a heart raises **maximum** health
        // and gives none (`items/ephemeral.lua:4-9`). Resting first fills a bar that the purchase
        // then lengthens, so the bed has to come last or it is partly wasted — a run that rested at
        // Stillingfleet and then bought two walked out at 20/28. `hearts_affordable` holds
        // [`crate::rest::INN_COST`] back over the whole visit, so that last bed is always still
        // affordable, and buying is what sets `wants_rest` again (see the driver's purchase branch).
        //
        // **The previous rule was the reverse, and the case it was written for is still live.** A
        // run once stood in front of a general store at 2/20 health with an inn in the same village,
        // and `wants_rest` is not a preference — the two level 6 crypts that ended that evening were
        // both entered hurt. What that case actually turns on is *knowledge*, not priority: the inn
        // was not known yet, the store was, and the fallback went shopping mid-search.
        //
        // So the shelf only wins when it is **found**. `store_inside` returning `Some` means a
        // general store is on the map in this village; anything less falls straight through to the
        // bed, which leaves the hurt-run case exactly as it was.
        // When neither is found the answer is `None`, which is what makes `cross_toward` explore —
        // and exploring is how either of them gets found at all. The old shape said that through a
        // `seeking_a_rest` match whose arms both came to `None` once the store was asked first.
        let errand_at = self.errand_inside(&parent).map(|p| p.key.clone());
        let leaving_to = match errand_at.is_some()
            || self.seeking_a_rest(&parent)
            || self.seeking_a_heart(&parent)
            || self.seeking_a_shrine(&parent)
        {
            // The errand outranks the crossing, and leaves the commitment untouched rather than
            // clearing it: the rest is a detour, and the door we were making for is still the door.
            true => None,
            false => {
                // Committed first, derived only when there is nothing left to honour. The
                // commitment also survives an exits list we cannot see past — `exit_toward` gives up
                // on an empty one, and a pan that shows no exits is fog, not a dead end.
                //
                // ## No door yet is not the same as no move, and this used to end runs
                //
                // This was `self.choose_exit(exits)?`, and the `?` returned from `cross_toward`
                // itself — past the frontier walk 100 lines below, which is the entire answer to
                // "we cannot see a way out yet". The driver reads that `None` as
                // `inside {container} with no crossing plan` and stops (`navigate.rs:2282`).
                //
                // The comment directly above already said fog is not a dead end, and there is a test
                // pinning it (`fog_is_not_a_dead_end`) — but both only cover the case where a
                // commitment exists to fall back on. On the **first** step into a subworld there is
                // nothing committed, so the guard was unreachable exactly when it was needed.
                //
                // A lost woods makes this the normal case rather than a rare one: `thickFog = true`
                // (`lost_woods.lua:13`) means every exit prints as `Hidden location` on arrival, so
                // the run stopped in `e1` one step after walking in, with three perfectly good
                // neighbours named in the same dump.
                //
                // Falling through to `None` puts us in the same state as a village whose inn we have
                // not found: no destination, so explore — which for a fogged crossing is precisely
                // right, because seeing an exit is the thing exploring achieves.
                let plan = self.next_target();
                let errand = plan.as_ref().map(|p| p.reason.clone());
                let to = match self.committed_exit(
                    &parent,
                    errand.as_ref(),
                    plan.as_ref().map(|p| p.target.as_str()),
                ) {
                    // **A held commitment leaves the note alone, and the log used to print it as
                    // though it were this step's reasoning.**
                    //
                    // `door_note` was added on 2026-08-16 so a door decision could be read
                    // afterwards, and within a day it produced a false diagnosis of the `l10`/`l18`
                    // ping-pong: `door choice: Heart -> l32` printed on every lap, while the save
                    // said Enthorpe's shelf was empty and the plan from that save was
                    // `CloseAnomaly -> start`. The line was true, of a step long past. It was
                    // stale on that run's step 16 too, printing `l4`'s doors while we stood inside
                    // `l25`.
                    //
                    // So it now says so, and says what the errand is *now* — which is the one fact
                    // that decides whether the commitment still means anything, since
                    // `committed_exit` drops it the moment the errand changes.
                    Some(to) => {
                        self.door_held = Some(format!(
                            "`{to}`, errand now {}",
                            match &errand {
                                Some(g) => format!("{g:?}"),
                                None => "none".to_string(),
                            }
                        ));
                        Some(to)
                    }
                    None => self.choose_exit(exits).map(|(to, why, note)| {
                        self.door_reason = Some(why);
                        self.door_note = Some(note);
                        self.door_held = None;
                        to
                    }),
                };
                // Nothing to be single-minded *about* when there is no errand: `exit_toward` is down
                // to its own fallback, and recording that as a commitment would freeze a guess. And
                // nothing to record at all when no door was found — leaving any earlier commitment
                // untouched, which is what the old early return did by accident and this does on
                // purpose.
                if let Some(to) = to.as_ref() {
                    self.crossing_to = errand.map(|goal| (to.clone(), goal));
                }
                to
            }
        };
        let dest = match (&errand_at, &leaving_to) {
            (Some(k), _) => Some(k.clone()),
            (None, Some(to)) => Some(exit_node_key(&parent, to)),
            (None, None) => None,
        };

        // Standing on the errand: the crossing is over and the errand starts. Guarded by
        // `blocks_departure` because a village under attack puts a fight on its subnodes
        // (`village.lua:371-395`), and a fight underfoot is dealt with before anything else.
        //
        // **That guard is also the whole of "the chest needed a fight first".** An unopened chest is
        // incomplete and its heading carries the combat form, so `blocks_departure` says yes and
        // this falls through to [`Crossing::Fight`] — which the driver answers by pressing the area
        // slot and letting the top of its loop name whatever screen arrives. That is right for both
        // of a chest's two states: guarded, where `getAreaButtons` offers `Combat`
        // (`overworld/generators/forest.lua:187`), and clear, where it offers `Open` and the press
        // goes straight into the chest's own fight with no pregame at all.
        if errand_at.as_deref() == Some(here.as_str()) && !self.blocks_departure(&here) {
            return Some(Crossing::Arrive { at: here });
        }
        // Already standing on the road out.
        if let Some(to) = leaving_to.filter(|to| here == exit_node_key(&parent, to)) {
            return Some(Crossing::Leave { to });
        }
        // An unfinished fight underfoot blocks going onward, so it is the move.
        //
        // ## Backing out was tried for a year of runs and the dev has called it off
        //
        // The idea was that a hurt run should not have to take a fight it merely walked into:
        // `canTravelToDirect` needs one endpoint complete (`overworldview.lua:1316-1321`) and the
        // node behind us is complete by definition, so retreating is legal where going on is not.
        // It was sound in the small and cost more than it saved in the large:
        //
        // - it produced the `l9sub16` <-> `l9sub11` cycle, and the guard written to stop that
        //   (`retreats_running`) could never fire;
        // - `backed_out_of` replaced it, which meant the *second* block at a node took the fight
        //   anyway — so the rule bought one wasted round trip and then did the thing it had been
        //   avoiding;
        // - and when it could not retreat at all it fell through to `Fight`, which the driver's
        //   health gate then refused, ending a run in front of a **level 1** spider nest at 1/20.
        //   That is the run of 2026-08-08: twenty-four steps, no exit found, stopped by the guard
        //   rather than by the game.
        //
        // The dev's call, and it is a scope decision rather than a discovery: **for the MVP, stick
        // to the path and fight through whatever is standing on it.** A fight has an outcome; a
        // refusal has none, and a refusal in a spider forest is a run that ends where it stands.
        //
        // What this gives up is real and worth stating: a crossing that leads onto a level 6 guard
        // post will now take it. The protection that remains is in the *destination* choice, where
        // it belongs — `Risk`, `hostile_to_enter` and `next_target`'s two passes still keep a hurt
        // run from choosing to walk into hostile ground. This is only about what happens once the
        // path we are already on has something on it.
        if self.blocks_departure(&here) {
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
                && self
                    .places
                    .get(k)
                    .map(|p| !p.avoid && !(self.wants_rest && p.is_chest() && !p.completed))
                    .unwrap_or(true)
        };

        // **The steer used to be here, and #57 folded it into the ranking below.**
        //
        // It was a second arm: no route to the door, but the last dump printed where it *is*, so
        // step to whichever neighbour that dump puts closer. It earned its place — six of the seven
        // crossings in the 1519Z run ended with a steer as the last decision before the door was
        // named, and a crossing ends by the door being *named*, not walked to.
        //
        // It could not stay as an arm. It carried its own convergence device (`steered_gap`, a
        // high-water mark that only fell) and the frontier walk carries another (`probing_toward`, a
        // held target), and **neither device saw the other arm**. Alternating them cancelled both:
        // the frontier walk steps away from the door toward whatever teaches most, and from there
        // the steer read the node we had just left as an improvement and went back. `l63_plaza` ↔
        // `l63xrd60x-183`, three clean laps inside Upton Braken, with `l30` and `l39` doing the same
        // thing the same run.
        //
        // Underneath that they disagreed about what was even a candidate. The steer's only filter
        // was `usable` — no `is_frontier`, no `visited`, no `nothing_left_to_reveal` — so a node we
        // had stood on and fully named scored exactly as well as one we had never seen. The frontier
        // walk had already retired `l63_plaza` under `nothing_left_to_reveal`; the steer kept
        // choosing it because it was the nearest neighbour to the door.
        //
        // So the aim is now a term in the one ranking (`doorward`, below), where a retired node is
        // ineligible for free rather than by a memory. What is kept: the door's printed position as
        // the thing to head for, and paved before near. What is gone: a second procedure that could
        // disagree with the first.

        // **Head for the nearest place that can still teach us something, by a route.**
        //
        // "Unvisited first, then by key" is only cycle-free while an unvisited neighbour is in
        // reach. Once the ground around us is fully walked and the destination is still unknown, it
        // degenerates into a deterministic alphabetical pick — and two nodes that each pick the
        // other is a stable cycle, not a wander that eventually escapes.
        //
        // Live: `l9sub2` <-> `l9_plaza`, twenty laps. Both paved, both completed, neither blocking.
        // From `l9sub2` the smallest key is `l9_plaza` (`_` sorts below `s`); from `l9_plaza` it is
        // `l9sub2` (a prefix of `l9sub26`). Nothing was wrong with either choice on its own, which
        // is the signature of every bounce in this project.
        //
        // A route to a frontier cannot cycle: the BFS distance to the chosen frontier strictly
        // decreases with each step, so the walk terminates at something worth learning. Frontier is
        // [`Place::is_frontier`] — never visited, or visited while the game withheld neighbours —
        // so "worth learning" is the map's own record rather than a heuristic.
        //
        // **Paved before near, and that ordering is the whole of the dev's crossing rule.**
        //
        // It was `(fight, distance, paved, key)` — nearest first, with paved as a tiebreak that only
        // applied at equal distance. The live run of 2026-08-08 shows what that does over
        // twenty-four steps: the exit road was never on the map, so `first_step_toward` returned
        // `None` every single time and **this fallback was the entire crossing**. Ranking by
        // distance took every unvisited patch of brush one hop off the road before it would take a
        // road node two hops along — road, brush, back to the road, over and over, never crossing
        // the forest. The dev watched it happen and said so.
        //
        // The road is not a preference about safety, it is the map's own structure: `forest.lua`
        // strings `road` and `crossroads` from entrance to exit, so following it is how a forest is
        // crossed and everything else is a detour.
        //
        // Combat is no longer ranked at all here, for the same reason it is no longer routed around
        // in [`WorldMap::first_step_toward`] — see the note there. A nest on the road gets fought.
        //
        // This is now genuinely the same order `first_step_toward` uses, which the old comment
        // claimed while the code did something else: its passes are `[paved]` then `[any]`, and
        // BFS runs *within* each pass, so paved outranks distance there too. Choosing where to
        // explore and choosing how to get there cannot disagree.
        let hops = self.distances(&here);
        let exit_prefix = format!("{parent}_path_to_");
        // **What a blind search is blind *for***, which is what decides the ranking below.
        //
        // The dev, 2026-08-23, on their two rulings: *degree for the inn or store, and distance
        // (only if known) for the exit*. Computed here rather than in the key because the closure
        // below already holds a borrow of `self.places`.
        let searching = self.searching_for(&parent);
        // **Where the door is, for the ranking below** — task #57, and the whole of the steer folded
        // into one term.
        //
        // `placed_now` answers only for what the latest dump named, which is our neighbours and the
        // doors. So this orders the frontier nodes *adjacent to us* by how near the door they lie,
        // and everything further away shares the worst value and falls through to the terms behind
        // it. That is deliberate and it is exactly the reach the steer had — it only ever looked at
        // neighbours too — with the difference that the candidates are now frontier nodes rather
        // than any node at all.
        //
        // Absent when the destination has no printed position, which is the fogged-inn search: the
        // key below then reduces to what it has always been, so the dev's degree rule of 2026-08-15
        // keeps the village search it was written for.
        //
        // ## A subworld exit is **always** placed, and that is a fact about the game rather than
        // luck
        //
        // Worth stating here because the note this file used to carry assumed the opposite. Every
        // dump lists **every** exit of the subworld — `verboseAdjacencyData` iterates
        // `parent.connections`, not our neighbours (`overworldview.lua:1041-1051`) — and on entering
        // a subworld `subworldOnEnterBasics` sets `<key>_explored` on every `out_road`
        // (`utils/world.lua:884-892`), which is one of the four things that clear
        // `isCloudCovered` (`:696-706`). So the doors are placed from the first dump onward, before
        // we have walked to any of them.
        //
        // **Measured over every run log in the repo: 3,783 interior dumps, 14,063 exit lines,
        // 418 hidden — and 409 of those are one lost woods.** The two exceptions are exactly the two
        // the source names:
        //
        // - **`thickFog`**, where that loop is skipped entirely (`:885`). `e3 Nuthill Grove — level
        //   6 lost woods`, 409 hidden lines over 103 dumps: every exit, every dump.
        // - **a `secret` exit**, which fails `locationIsVisible` (`overworldview.lua:554-556`) until
        //   `<key>_revealed` — task #14's tower press. `e5 Shiptonthorpe Weald` printed two exits
        //   and one `Hidden location` in each of its 9 dumps.
        //
        // So inside an ordinary subworld this is `Some`, and a rule that needs to know where the way
        // out lies does not have to infer it. **It also means an unseen exit needs no prediction**:
        // the generator does place exits by surface bearing —
        // `worldUtils.generateSubworldOutroads` (`utils/world.lua:924-943`) puts the road toward `X`
        // on the interior bounding box in the direction `X` lies from the parent — but that is only
        // ever needed where the answer is already scrambled, since `lostOrientation` re-rolls the
        // whole interior through one of eight symmetries on every load (`forest.lua:483-495`).
        let door_now = dest.as_deref().and_then(|d| self.placed_now(d));
        let doorward = |p: &Place| -> u64 {
            match (door_now, self.placed_now(&p.key)) {
                (Some(d), Some(at)) => {
                    let g = (at.0 - d.0).powi(2) + (at.1 - d.1).powi(2);
                    // Squared throughout — it orders the same as the distance and takes no root.
                    // Clamped rather than cast blind: a NaN or an absurd coordinate must sort last,
                    // not wrap to zero and win.
                    if g.is_finite() && g >= 0.0 {
                        g.min(u64::MAX as f64 - 1.0) as u64
                    } else {
                        u64::MAX
                    }
                }
                _ => u64::MAX,
            }
        };
        let frontier = self
            .places
            .values()
            .filter(|p| p.parent.as_deref() == Some(parent.as_str()))
            .filter(|p| p.key != here && p.is_frontier() && usable(&p.key))
            // An exit road we have not walked is a frontier too, but heading for one is leaving —
            // and while looking for an inn, leaving is the move that abandons the errand.
            .filter(|p| !p.key.starts_with(&exit_prefix) || Some(&p.key) == dest.as_ref())
            .filter(|p| !p.nothing_left_to_reveal())
            // **How much this node would teach us, before how near it is.**
            //
            // The dev, 2026-08-15, watching a village search take far too long: *it was making
            // questionable choices about which frontier node to visit. Could it choose based on the
            // number of connections on the unvisited node?*
            //
            // Yes, and it is the right question: standing on a node is what makes the game name its
            // neighbours, so a node's degree is exactly how much of the interior one step buys. The
            // old key was `(paved, distance)`, and distance is a measure of what a step *costs* with
            // nothing at all about what it returns — so a two-connection dead end one hop away beat
            // a six-connection crossroads two hops away, every time, and a village got searched a
            // cul-de-sac at a time.
            //
            // **Unrevealed neighbours rather than raw degree**, which is the dev's count with the
            // part we already know taken off it: `connections` is the game's own figure for the node
            // (printed beside it in every dump) and `neighbours` is what we have seen named, so the
            // difference is what a visit would actually add. A six-way node with five neighbours
            // already named reveals one thing, and ranking it top would be ranking work we have
            // already done. Same arithmetic as [`WorldMap::has_unexplored_roads`].
            //
            // **Less what the game is withholding**, which is #106: the count is
            // [`Place::unrevealed`] rather than the raw difference, because a neighbour printed as
            // `Hidden location` in this section is a *secret* and no visit reveals it. Left in, a
            // node with a secret neighbour ranked for ever as though it still had something to
            // teach, which is `l59sub5` in the 0547Z run.
            //
            // Still under `is_paved`, which is a separate and older rule of the dev's and not one
            // this touches: roads are the map's own structure, and this reorders which road to take
            // rather than licensing the brush.
            // **Paved, then near, then doorward, then how much it teaches.** The dev settled both
            // of the orderings in this key on 2026-08-21, and they were settled a few minutes apart.
            //
            // *Doorward before degree*, choosing between two of their own rules where they meet:
            // *how much this node would teach us, before how near it is* was written for **searching**
            // a village for an inn the fog hides, where there is nothing to head for. Crossing to a
            // **known** exit is the other case, and there a high-degree node in the wrong direction
            // is a detour. So when we can see the door, go toward it, and let degree decide between
            // nodes lying equally near it.
            //
            // **And hops above doorward** — the dev: *the number of hops should be higher in the
            // ranking so that we don't backtrack until our current branch is done.* That is the
            // placement the goal requires and the reason is worth spelling out: with `doorward`
            // dominant, a frontier node four hops back that happens to lie nearer the door beats the
            // one at our feet, and the walk re-crosses ground it has already covered to get there.
            // Nearest-first makes the search expand outward from where we stand, and `doorward` then
            // chooses **which branch** to take at each fork rather than which side of the subworld to
            // be on.
            //
            // The doorward term is absent when the destination has no printed position, which is
            // both the fogged inn and any door that has merely gone off screen. Which of those two
            // it is decides nothing here any more — see the `dest` switch below and #81.
            .filter_map(|p| {
                let unrevealed = p.unrevealed();
                hops.get(&p.key).map(|d| {
                    // **Which of `hops` and degree leads depends on whether we have somewhere to
                    // be**, because the two rules were written for two different errands and only
                    // look like they contradict.
                    //
                    // *Crossing to a known exit, or to an errand inside.* Nearest first, so the
                    // search expands outward from where we stand and `doorward` chooses which branch
                    // at each fork — the dev's *don't backtrack until our current branch is done*.
                    // Degree decides between nodes equally near and equally doorward.
                    //
                    // *Searching for something the fog hides* — an inn, a store or a shrine we
                    // have not found, with nowhere at all to head for. Degree first, which is the
                    // rule of 2026-08-15 and the village that *got searched a cul-de-sac at a
                    // time*. `doorward` is `u64::MAX` for every candidate here, so it drops out and
                    // this is exactly the key that rule was given. **A fogged search for the way
                    // out is not in this group** — see the `searching` switch below.
                    //
                    // ## #81: this was `door_now.is_some()`, and that is not the same question
                    //
                    // `door_now` is the destination's position **in the latest dump**, so it is
                    // absent whenever the door is merely off the current screen — which, crossing a
                    // large forest, is most of the way across. A crossing with a perfectly definite
                    // target therefore kept falling into the fogged-search ordering and reorganising
                    // itself by degree, which is a global measure: the best frontier by degree can
                    // be anywhere, so the walk repeatedly abandoned the branch it was on.
                    //
                    // Wressle Wood, live 2026-08-22, steps 289-296 — four hops out to `l44sub28`
                    // and four straight back past the plaza to `l44sub3`, eight presses to move two
                    // nodes. Both are roads, so `is_paved` did not cause it; `l44sub28` has two
                    // connections and `l44sub3` three, so degree did.
                    //
                    // The dev's ruling, 2026-08-22: *use "distance from current position" as the
                    // highest rank.* Asking `dest` instead of `door_now` is what delivers that
                    // wherever there is a destination at all, and it leaves the fogged village
                    // search — which has no `dest` — with the exact ordering the dev gave it in the
                    // first place. `doorward` still degrades to `u64::MAX` on its own when the door
                    // has no printed position, so nothing else had to move.
                    //
                    // Inverted rather than wrapped in `Reverse` so both orderings are one type.
                    let near = *d as u64;
                    let teaches = u64::MAX - unrevealed as u64;
                    // ## #100: `dest.is_some()` was a proxy for the question, and it got one case
                    // wrong
                    //
                    // The dev, 2026-08-23, settling the two rulings that had been read as a
                    // contradiction: *degree for the inn or store, and distance (only if known) for
                    // the exit*. They are two errands, not two opinions about one errand.
                    //
                    // `dest.is_some()` separates *having somewhere to head for* from *searching
                    // blind*, which is almost the same cut and differs on exactly one case:
                    // **searching blind for the exit**. A fogged crossing has no `dest` — an empty
                    // exits list is fog, not a dead end — so it fell into the degree-first ordering
                    // written for the village, and degree is a global measure: the best frontier by
                    // degree can be anywhere, so the walk abandons the branch it is on. That is the
                    // Wressle Wood fault of #81 exactly, in the one branch #81's fix could not
                    // reach, because #81 replaced `door_now` with `dest` and a fogged crossing has
                    // neither.
                    //
                    // So the cut is now the errand itself, [`WorldMap::searching_for`], and it says
                    // what the dev said:
                    //
                    // - **`Inn`, `Store`, `Shrine`** — something hidden *inside*, with nowhere at
                    //   all to head for. Degree first: the rule of 2026-08-15 and the village that
                    //   *got searched a cul-de-sac at a time*. `doorward` is `u64::MAX` for every
                    //   candidate here, so it drops out and this is exactly the key that rule was
                    //   given.
                    // - **`Exit`** — a way out, seen or not. Nearest first, so the search expands
                    //   outward from where we stand and does not backtrack until the branch is
                    //   done, which is the ruling of 2026-08-22.
                    //
                    // **A lost woods is the case this most changes, and it is the case it was
                    // wanted for.** Its exits are the one thing in it that stays paved — `paveRoads
                    // = false` with `paveInsetRoads = true` (`lost_woods.lua:8-9`), and the rename
                    // at `forest.lua:653-659` leaves the `out_road_edge` approach corridors as the
                    // only paved nodes in the interior. `is_paved` leads this key, so the walk
                    // already prefers those corridors; leading with `near` behind it is *stick to
                    // the path you are on*, where degree was licensing a jump to the far side of
                    // the woods. The dev, 2026-08-23: *find the path and stick to it.*
                    let (lead, trail) = match dest.is_none() && searching != Searching::Exit {
                        true => (teaches, near),
                        false => (near, teaches),
                    };
                    (!p.is_paved(), lead, doorward(p), trail, &p.key)
                })
            })
            .min()
            .map(|(_, _, _, _, k)| k.clone());

        // **Keep walking to the frontier we chose, while it is still worth walking to.**
        //
        // See [`WorldMap::probing_toward`] for the `l48` bounce this exists to stop. The ranking
        // above is re-run from scratch every step and depends on where we are standing; holding the
        // answer still is what makes the "BFS distance strictly decreases" argument true rather than
        // merely plausible.
        //
        // Every condition here is a reason the old target has stopped being an answer, and each one
        // hands back to the ranking rather than stalling:
        //
        // * a different container — we have left, and an interior is re-rolled on re-entry;
        // * we are standing on it, so it has taught us what it had;
        // * it is no longer a frontier, or has nothing left to reveal, or has become unusable;
        // * no route to it from here, which a corrupted road can take away mid-crossing.
        let held = self
            .probing_toward
            .as_ref()
            .filter(|(c, _)| c.as_str() == parent.as_str())
            .map(|(_, k)| k.clone())
            .filter(|k| k.as_str() != here.as_str())
            .filter(|k| {
                self.places.get(k).is_some_and(|p| {
                    p.is_frontier() && !p.nothing_left_to_reveal() && usable(&p.key)
                })
            })
            .filter(|k| self.first_step_toward(&here, k, false).is_some());
        let chosen = held.or(frontier);
        if let Some(f) = chosen {
            if let Some(step) = self.first_step_toward(&here, &f, false) {
                self.probing_toward = Some((parent.clone(), f));
                return Some(match dest {
                    Some(toward) => Crossing::Probe { to: step, toward },
                    None => Crossing::Seek { to: step },
                });
            }
        }

        // Nothing left to learn, or no route to any of it. Same order the real router uses, so
        // exploring does not undo what routing was for: paved first, then anywhere; unvisited first
        // within each.
        let pick = |paved_only: bool| {
            let mut best: Option<&String> = None;
            for n in place.neighbours.iter().filter(|n| usable(n)) {
                let p = self.places.get(n);
                if paved_only && !p.map(|p| p.is_paved()).unwrap_or(false) {
                    continue;
                }
                // `seen` and not `!seen`. Written the other way round, this said "unvisited first"
                // in the comment and did the exact opposite: an unvisited node scores `!seen ==
                // true`, a visited one `false`, and `false < true` — so ground we had already walked
                // won every comparison. That is the whole of the `l9sub2` <-> `l9_plaza` bounce.
                // From the plaza the neighbours were `l9sub1` (unwalked), `l9sub2` (walked) and
                // `l9sub3` (unwalked), and it took `l9sub2` twenty times.
                let seen = p.map(|p| p.visited).unwrap_or(false);
                // And the same question the routed frontier now asks, asked of a single step: of the
                // neighbours we have not stood on, take the one that would name the most new places.
                // This branch is the one a fogged village actually runs — there is no route to a
                // frontier while the fog is hiding it — so ordering it by key alone was ordering it
                // alphabetically, which is where "questionable choices" came from.
                let reveal = |p: Option<&Place>| {
                    p.map(|p| p.connections.saturating_sub(p.neighbours.len() as u32)).unwrap_or(0)
                };
                let gain = std::cmp::Reverse(reveal(p));
                let better = match best {
                    None => true,
                    Some(b) => {
                        let bp = self.places.get(b);
                        let b_seen = bp.map(|p| p.visited).unwrap_or(false);
                        (seen, gain, n) < (b_seen, std::cmp::Reverse(reveal(bp)), b)
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
            Some(toward) => Crossing::Probe { to: step, toward },
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
    /// The general store inside `container`, while we are there to buy a heart.
    ///
    /// Shaped exactly like [`WorldMap::inn_inside`], including the `abandoned` filter: the driver's
    /// record of having had its go is the only thing that separates "the fog still hides it" from
    /// "we tried and it did not work", and conflating those is this project's oldest bounce.
    fn store_inside(&self, container: &str) -> Option<&Place> {
        // The spent check belongs here as well as in `seeking_a_heart`: this is what the crossing
        // asks when deciding whether we are still standing on an errand, and a store we have already
        // emptied is not one. Without it, arriving stays `Arrive` for ever and the driver is handed
        // a counter with nothing left on it.
        if !self.wants_a_heart() || self.heart_bought.contains(container) {
            return None;
        }
        // And a corrupted village sells nothing — see [`Place::stocks_a_heart`].
        if !self.places.get(container).map(|p| p.stocks_a_heart()).unwrap_or(false) {
            return None;
        }
        self.places.values().find(|p| {
            inside_container(p, container)
                && p.is_general_store()
                && !self.abandoned.contains(&p.key)
        })
    }

    /// Is there anywhere left to go that the anomaly's trigger level does not cover?
    ///
    /// The dev, 2026-08-16, after a fresh character died in a level 7 crypt on the way to the
    /// portal: *we don't have the warrior's gear to carry us through difficult crypts, so instead of
    /// beelining for the nearest level 4 node, I now want the pre-anomaly navigator to visit every
    /// node that is lower than level 4, so that we only visit a level 4 node when the entire
    /// frontier is at least level 4.*
    ///
    /// Opening the portal needs a win **above level 3** (`crate::subworld::triggers_anomaly`, from
    /// `world_evil.lua:15-21`), so the trigger is by definition the hardest thing on the board at
    /// that moment. Taking the nearest one the moment it appears spends a fresh character on the
    /// worst fight available while easier ground — gold, gear, hearts, and the map itself — lies
    /// unwalked beside it.
    ///
    /// **A frontier node with no level at all counts as gentler.** No level means no combat
    /// (`Place::has_combat`), so it is free to walk to and cannot be the thing being avoided.
    ///
    /// Only asked while the portal is shut. Once it is open the objective is the portal itself and
    /// this bar would keep a run wandering instead of finishing.
    ///
    /// **One caller since 2026-08-22, and it is [`Goal::OpenAnomaly`] alone.** `Explore` used to ask
    /// this too, and that pairing was the exhaustive sweep — a level 4+ node was not explorable
    /// while any gentle frontier existed anywhere. The dev retired that half; see the note where it
    /// used to be in [`WorldMap::next_target`]. What is left here is narrower and still true: do not
    /// go and open the portal on purpose while there is gentler ground to learn from first.
    pub(super) fn gentler_ground_remains(&self, here: &str, ok: &impl Fn(&Place) -> bool) -> bool {
        if self.anomaly_is_open().unwrap_or(false) {
            return false;
        }
        self.places.values().any(|p| {
            p.key != here
                && p.is_frontier()
                && !p.avoid
                && !self.abandoned.contains(&p.key)
                && !p.nothing_left_to_reveal()
                && !p.triggers_anomaly()
                && ok(p)
        })
    }

    /// Everything inside this subworld that is worth stopping at, in the order they outrank one
    /// another. Task #18.
    ///
    /// The crossing used to know two errands by name, bolted on one at a time — the inn because a
    /// rest is what first made a village worth entering, then the general store when hearts became
    /// worth buying. Each addition touched the same four places, and the third one was a chest we
    /// walked straight past.
    ///
    /// The order is not alphabetical and not accidental:
    ///
    /// 1. **the general store**, because a heart raises maximum health and the bed then fills the
    ///    larger bar — resting first wastes part of the purchase (`items/ephemeral.lua:4-9`, and
    ///    the full argument is at the call site);
    /// 2. **the inn**, which is where a hurt run is trying to get to;
    /// 3. **an unopened chest**, which is loot rather than preparation;
    /// 4. **an unprayed shrine**, added 2026-08-17 on the dev's rule that a pre-anomaly shrine is an
    ///    errand wherever it sits — see [`WorldMap::shrine_inside`] for the run that walked past
    ///    `shrine1` because the goal was satisfied by standing on the forest holding it.
    ///
    /// Each gates itself on whether we actually want it — [`WorldMap::store_inside`],
    /// [`WorldMap::inn_inside`], [`WorldMap::chest_inside`], [`WorldMap::shrine_inside`] — so this
    /// is an ordering, not a policy. `None` means nothing in here is worth a stop, which is what
    /// makes a crossing a crossing.
    ///
    /// **Only the first two reach [`Crossing::Arrive`]**, and that is not an oversight. A chest is
    /// incomplete with a combat heading, so `blocks_departure` sends it to [`Crossing::Fight`] and
    /// the area press opens it; a shrine is picked up by the driver's own shrine branch, which runs
    /// before the crossing block. Anything added here has to answer that question for itself.
    fn errand_inside(&self, container: &str) -> Option<&Place> {
        self.store_inside(container)
            .or_else(|| self.inn_inside(container))
            .or_else(|| self.chest_inside(container))
            .or_else(|| self.shrine_inside(container))
    }

    /// A shrine inside this subworld that has never been prayed at. — the dev's rule, 2026-08-17
    ///
    /// *Any pre-anomaly shrine should be solved as an errand anyway, just like an overworld shrine.
    /// The behaviour should be to Pray there, and then leave the Consecration for later.*
    ///
    /// **The case that prompted it.** `shrine1` is `Gripthorpe Brush — level 1 forest`: the shrine is
    /// a `woodland shrine` **subnode** (`forest.lua:223`), and the surface node is the forest holding
    /// it. So the run travelled there under `Goal::Shrine`, entered — and the goal was satisfied by
    /// standing on the container. `next_target` excludes `here`, the planner moved on to a heart, and
    /// the crossing walked out the far side. Live 2026-08-16 2103Z, steps 11 to 13: the errand did not
    /// fail, it *evaporated on arrival*.
    ///
    /// ## Why these conditions and no others
    ///
    /// They are the surface rule with the anomaly branch removed, and they have to stay that way —
    /// `next_target`'s `pick_shrine` is the pair this must agree with, and the last time a shrine
    /// filter drifted from its partner a shrine sat `[done] [corrupted]` and unconsecrated for a whole
    /// run. Pre-anomaly, `worth_a_trip` reduces to `!p.used`, and both of the `!anomaly_open ||`
    /// clauses pass trivially. That leaves: a shrine, not avoided, not abandoned, **not used**.
    ///
    /// - **Open or shut, because a minor shrine owes a prayer either way.** Task #71, and this
    ///   condition was the opposite until 2026-08-21. It read *pre-anomaly only*, on the argument
    ///   that `Consecrate` is drawn only while `hell ~= 0` (`shrine.lua:92-95`), so once the portal
    ///   opens an interior shrine is governed by [`WorldMap::worth_consecrating_here`] on arrival —
    ///   *consecrate it if we are walking through anyway*, never a reason to stop.
    ///
    ///   **That handed off to a rule about consecration, and a minor shrine's payload is the
    ///   prayer.** `showPrayButton` leads with `not shrineLocation.majorShrine`
    ///   (`shrine.lua:97-102`), which short-circuits the entire `hell` clause: the prayer is
    ///   available on identical terms before and after the portal opens. And
    ///   `worth_consecrating_here` returns false outright for anything that fails
    ///   [`Place::can_be_consecrated`], which every minor shrine does — so the handoff was to a
    ///   handler that declines. The dev, 2026-08-21, watching the 1519Z run cross `l53` in three
    ///   hops without touching the shrine in it: *at Beeford Hedge, the minor shrine should've been
    ///   an errand.*
    ///
    ///   Nothing about the pairing with `pick_shrine` is broken by this. The surface rule stopped
    ///   pricing the route on the same day (#70), so the two moved in the same direction; and the
    ///   cost argument behind the surface filters never applied here anyway, because the toll on a
    ///   node inside a container we are already crossing is a hop, not a fight.
    /// - **`!used`, not `!completed`.** `<key>_used` is what the game sets when a prayer lands, and
    ///   `completed` is about the fight. A shrine with no fight is `completed` on arrival
    ///   (`e799771`), so asking that would skip every peaceful shrine there is.
    /// - **`!abandoned` is load-bearing, not hygiene.** A shrine whose puzzle the driver gave up on is
    ///   *genuinely unused* — the game never set the flag, because nothing was prayed — so without
    ///   this the errand would send us back to it for ever. That is the thirty-step
    ///   `l10 -> shrine2 -> l10` bounce, and it is the reason the surface filter carries the same
    ///   clause.
    ///
    /// Last in [`WorldMap::errand_inside`], matching the surface ordering, where the chest branch
    /// comes before the pre-anomaly shrine pick. Nearest first, ties broken by key, because a
    /// `places` hash map would otherwise choose differently on different runs from the same state.
    fn shrine_inside(&self, container: &str) -> Option<&Place> {
        let dist = self.here.as_deref().map(|h| self.distances(h)).unwrap_or_default();
        // Hops, not travel cost, for the one-hop rule below — see [`WorldMap::hops_from`].
        let hops = self.here.as_deref().map(|h| self.hops_from(h)).unwrap_or_default();
        self.places
            .values()
            .filter(|p| inside_container(p, container))
            .filter(|p| p.is_shrine() && !p.used && !p.avoid)
            .filter(|p| !self.abandoned.contains(&p.key))
            // **A minor shrine is an errand only when we are already beside it** — the dev,
            // 2026-08-22: *I only care to do a minor shrine errand if we are adjacent to it*, and
            // then, on being offered two: *two hops is still not low enough for a minor shrine. One
            // hop or less should be the condition.*
            //
            // It does not retract the Beeford Hedge rule above; it puts a price on it. A woodland
            // shrine pays a prayer and nothing toward [`SHRINES_BEFORE_THE_ANOMALY`], so it is worth
            // a step off the road and not a search of the woods.
            .filter(|p| p.can_be_consecrated() || dist_or_far(&hops, &p.key) <= 1)
            // **Consecratable above near**, which the old key left to luck. A major shrine's plaza
            // is the only thing in here that can move the bar, and ranking on `(distance, key)`
            // alone let a woodland shrine one hop nearer take the errand instead — the key tiebreak
            // happens to favour `_plaza` when the distances are equal, which is not a rule.
            .min_by_key(|p| (!p.can_be_consecrated(), dist_or_far(&dist, &p.key), p.key.clone()))
    }

    /// Are we inside a shrine forest with its shrine still hidden by the fog?
    ///
    /// The third of the hold-us-inside guards, beside [`WorldMap::seeking_a_rest`] and
    /// [`WorldMap::seeking_a_heart`], and the one that was missing. Without it an errand can be
    /// satisfied by *arriving at the container* and then evaporate: `next_target` excludes the node
    /// we are standing on, so the plan moves on, and the crossing — finding nothing to stop for —
    /// walks straight out the far side.
    ///
    /// Live 2026-08-22: five hops to `shrine7`, *Cottam Boscage — level 9 forest*, in and out to
    /// `l55` on the very next step, with the plan already moved to `shrine3`. `shrine_inside` cannot
    /// help there, because on the first step inside there is nothing to find: `isRevealed` is
    /// `corrupt or areaIsComplete or areaFlag(key..'_plaza_explored')`
    /// (`overworld/locations/shrine_forest_raw.lua:5`), so an unexplored shrine forest hides its
    /// own shrine.
    ///
    /// ## Why the key is enough to know it is in there
    ///
    /// `shrine_forest_raw` is the **only** location type in the game with `trueTypeName = 'shrine'`,
    /// it is a `subworld = 'forest'`, and it declares `features = {plaza = {type = 'shrine'}}`
    /// (`:10-18`). The forest generator builds `parentNode.key..'_plaza'` as the first node of the
    /// interior, unconditionally (`overworld/generators/forest.lua:398-401`). So for any container
    /// whose key [`key_is_major_shrine`] accepts, the consecratable shrine exists and its key is
    /// known before we have laid eyes on it — which is exactly what lets this hold a run inside a
    /// place it has not searched.
    ///
    /// **Consecratable only**, or this reproduces the fault it fixes one layer down: a minor shrine
    /// under fog cannot even be measured for the one-hop rule above, so searching for one would be
    /// the unbounded errand that rule exists to refuse.
    ///
    /// Ends the moment the plaza is on the map at all — from a dump, the save, or the cache — after
    /// which [`WorldMap::shrine_inside`] answers for it and this has nothing to add. `abandon` ends
    /// it too, for the reason every one of these guards carries the clause: a shrine the driver has
    /// already had its go at is not one the fog is hiding.
    fn seeking_a_shrine(&self, container: &str) -> bool {
        if !key_is_major_shrine(container) {
            return false;
        }
        let plaza = format!("{container}_plaza");
        !self.abandoned.contains(&plaza) && !self.places.contains_key(&plaza)
    }

    /// An unopened chest in this subworld, nearest first.
    ///
    /// The interior half of task #16, and the half that had to exist for the task to do anything at
    /// all: `typeName = 'chest'` appears in exactly two files, `overworld/generators/forest.lua:178`
    /// and `bandit_camp_forest.lua:201`, and both build **subnodes** off `parentNode.subnodeCount`.
    /// There are no chests on the surface. So the detour that landed on 2026-08-16 —
    /// [`Goal::Chest`], which plans over surface nodes — could never once have fired in a real
    /// world, and the run that evening walked past `l4sub11` twice.
    ///
    /// `wants_rest` is the gate, and it is the dev's rule verbatim: *when the goal is not rest and a
    /// chest is visible, detour to it.* Opening one is a fight, and a fight is the last thing a hurt
    /// run should walk toward.
    ///
    /// **Nearest first, and by distance rather than by whatever the map iterates first.** A forest
    /// carries `math.ceil(nestCount/4)` chests (`forest.lua:586`), so more than one is ordinary, and
    /// `places` is a hash map — `find` would pick a different chest on different runs from the same
    /// state. The key breaks ties so the choice cannot flap.
    fn chest_inside(&self, container: &str) -> Option<&Place> {
        if self.wants_rest {
            return None;
        }
        let dist = self.here.as_deref().map(|h| self.distances(h)).unwrap_or_default();
        self.places
            .values()
            .filter(|p| inside_container(p, container))
            .filter(|p| p.is_chest() && !p.completed && !p.avoid)
            .filter(|p| !self.abandoned.contains(&p.key))
            .min_by_key(|p| (dist_or_far(&dist, &p.key), p.key.clone()))
    }

    fn inn_inside(&self, container: &str) -> Option<&Place> {
        if !self.wants_a_bed() || self.gold < crate::rest::INN_COST {
            return None;
        }
        // The village's buildings must actually be open — see [`Place::trades`]. Under attack the
        // inn's `Enter` opens an empty room, so a run that ignored this would walk the whole village
        // and press a button that works.
        if !self.places.get(container).map(|p| p.trades()).unwrap_or(true) {
            return None;
        }
        self.places.values().find(|p| {
            inside_container(p, container) && p.is_inn() && !self.abandoned.contains(&p.key)
        })
    }

    /// Which of the reasons a [`Crossing::Seek`] has no destination, for the log to say so.
    ///
    /// **The order is the `leaving_to` guard's order, and it has to be**: more than one of these can
    /// be true at once — a hurt run in a village it also means to shop in — and the search serves
    /// all of them at once, since the only way to find any of them is to explore. So this names the
    /// one that would have held us inside first rather than pretending there is a single answer.
    pub fn searching_for(&self, container: &str) -> Searching {
        if self.seeking_a_rest(container) {
            return Searching::Inn;
        }
        if self.seeking_a_heart(container) {
            return Searching::Store;
        }

        match self.seeking_a_shrine(container) {
            true => Searching::Shrine,
            false => Searching::Exit,
        }
    }

    /// Are we inside a village on the heart errand, with its general store still to find?
    ///
    /// The same shape as [`WorldMap::seeking_a_rest`], including the abandoned clause: a store we
    /// have already tried is not one the fog is hiding, and treating those alike is the shape of
    /// every bounce this project has had.
    fn seeking_a_heart(&self, container: &str) -> bool {
        if !self.wants_a_heart() || self.heart_bought.contains(container) {
            return false;
        }
        // `stocks_a_heart` rather than `is_settlement`, so the corruption clause is asked here too —
        // searching a corrupted village for a shop that will not open is the same wasted trip the
        // planner's filter now refuses to nominate.
        if !self.places.get(container).map(|p| p.stocks_a_heart()).unwrap_or(false) {
            return false;
        }
        !self.places.values().any(|p| {
            p.parent.as_deref() == Some(container)
                && p.is_general_store()
                && self.abandoned.contains(&p.key)
        })
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
        if !self.wants_a_bed() || self.gold < crate::rest::INN_COST {
            return false;
        }
        if !self.places.get(container).map(|p| p.has_an_inn() && p.trades()).unwrap_or(false) {
            return false;
        }
        !self.places.values().any(|p| {
            p.parent.as_deref() == Some(container) && p.is_inn() && self.abandoned.contains(&p.key)
        })
    }

    /// Does an unfinished fight on this node forbid stepping off it?
    fn blocks_departure(&self, key: &str) -> bool {
        self.places
            .get(key)
            .map(|p| !p.completed && heading_has_combat(&p.heading))
            .unwrap_or(false)
    }

    /// The same accelerator for crossing a **settlement**, which the surface rule was refusing.
    ///
    /// The dev, 2026-08-16, watching a run leave a town a node at a time: *the multi-hop in that
    /// town still didn't work when we were trying to leave town after resting at the inn.*
    ///
    /// ## Why "surface only" was too broad, and why "settlements only" still was
    ///
    /// [`crate::subworld`] assumes every subworld is fogged, on the grounds that `thickFog` collapses
    /// visibility to "standing there or adjacent right now" (`overworldview.lua:696-699`) and that we
    /// cannot detect it — `lost_woods.getTypeName` reads `forest` until the area flag is set, so a
    /// heading tells us nothing. The first half is true and the second is **only true from outside**:
    /// `thickFog = true` appears in exactly one place in the game, `lost_woods.lua:15`, with `:47`
    /// turning it back off for the corrupt variant.
    ///
    /// This was restricted to settlements for one commit on the strength of that ambiguity, and the
    /// dev's report was that forests never took a fast hop. The restriction was unnecessary, because
    /// **the ambiguity is already resolved by the time this function can be called**. The mist event
    /// sets `lost_woods_known_<key>` in the same `onSelect` that calls `enterSubworld`
    /// (`overworld/events/arrived/lost_woods.lua:23-27`) — the flag is written *before* we are
    /// inside — and from then on `getTypeName` prints `lost woods` in every heading. [`Place::in_lost_woods`]
    /// already carries exactly that, set in [`WorldMap::fold`] for the container and every subnode.
    ///
    /// So the question is not "which kind of subworld is this" but "is this the one kind that fogs",
    /// and we can answer it standing inside. A forest that has not swallowed us is ordinary ground.
    ///
    /// The measurement that appeared to back the rule does not either. The run that clicked an exit
    /// several nodes off and moved the screen 0.015 is explained where [`WorldMap::cross_toward`]
    /// explains it: inside a **corrupted** subworld `setHellValue` has reset every area to
    /// incomplete, so `canTravelToDirect` has no complete endpoint to work with and refuses. That is
    /// a completion failure, and [`WorldMap::can_travel_direct`] already models it exactly — which is
    /// what makes this safe to switch on rather than a gamble. An interior we have not cleared stops
    /// the chain on its own, one node at a time, exactly as before.
    ///
    /// ## Corruption was a permanent ban on a temporary condition — and that was wrong
    ///
    /// **This used to read `!c.corrupted`, and it cost the run of 2026-08-22 2002Z every fast hop it
    /// had.** Both containers it crossed were corrupted — `l4` had gone level 1 to level 6 and `l10`
    /// from unlevelled to level 6 — so the chain came back empty on every single crossing, and the
    /// run walked `l4` a node at a time: six steps to `l4sub21`, six more to `l4_path_to_l25`.
    ///
    /// The dev, on being shown that: *the corruption is only a one-time clearing of a node's visited
    /// state. Once we re-visit the nodes, they become fast-hoppable again.* That is the whole of it.
    /// `setAreaIncomplete` clears `completedAreas[key]` **once** (`overworldview.lua:183-206`); it
    /// does not make the container permanently unfoggable, and re-clearing a node restores exactly
    /// what was lost. A flag on the container was standing in for a condition that lives on each
    /// node and changes as we walk.
    ///
    /// So the container test is gone and the node test does the work. It always could:
    /// [`WorldMap::can_travel_direct`] models the completion rule exactly, refuses the chain one node
    /// at a time while the interior is genuinely uncleared, and *stops* refusing as we clear it —
    /// which a container-level boolean can never do. The paragraph above already said this was
    /// "belt and braces" over a guard that "would refuse most of the chain anyway"; the belt was
    /// welded shut.
    ///
    /// The earlier reasoning, kept because the fog it describes is real:
    ///
    /// The dev, 2026-08-17: *once a settlement is corrupted, the thick fog re-appears, but if that
    /// hasn't happened, then there is no thick fog to guard against.* True, and the fog lifts again
    /// the same way it fell — per node, as each is re-cleared.
    ///
    /// The mechanism is not `thickFog` itself — that really is set in one file — but the general
    /// cloud rule beside it, and the effect is the same. `isCloudCovered` (`overworldview.lua:701-706`)
    /// leaves a node visible only if it is **complete** or carries an `_explored` flag, and
    /// `subworldOnEnterBasics` sets that flag for `visibleOnEnter` nodes only
    /// (`utils/world.lua:884-891`). Corruption runs `setAreaIncomplete`, which clears
    /// `completedAreas[key]` and every `_path_to_` road with it (`overworldview.lua:183-206`) — so
    /// every interior node that was visible *because it was cleared* goes back under cloud.
    ///
    /// Belt and braces, and deliberately: `can_travel_direct` would refuse most of the chain in a
    /// corrupted interior anyway, for exactly the same reason. Two independent guards on a rule the
    /// dev has watched fail once is the right number.
    ///
    /// ## The extra stop rule, which the surface does not need
    ///
    /// Arrivals fire at every node on the path (`overworldview.lua:1210-1216`), so a chain through an
    /// uncleared guard post walks into that fight. On the surface that is the dev's standing ruling —
    /// stick to the path and fight through whatever is standing on it — but leaving a settlement
    /// after a rest is the one errand that is deliberately *not* looking for a fight, so the chain
    /// stops short of anything [`WorldMap::blocks_departure`] names. **This is the conservative half
    /// of a decision the dev has not made**; barrelling through is one line away if that is wanted.
    pub fn far_hop_inside(&self, from: &str, to: &str) -> Option<String> {
        let clear_air = self
            .inside()
            .and_then(|c| self.places.get(c))
            .map(|c| !c.in_lost_woods)
            .unwrap_or(false);
        if !clear_air {
            return None;
        }
        self.far_chain_all(from, to, &|p: &Place| self.blocks_departure(&p.key)).into_iter().next()
    }

    /// Every node [`WorldMap::far_hop_inside`] would accept, furthest first — see
    /// [`WorldMap::far_hop_chain`] for why the caller wants the list and not just its head.
    pub fn far_hop_chain_inside(&self, from: &str, to: &str) -> Vec<String> {
        let clear_air = self
            .inside()
            .and_then(|c| self.places.get(c))
            .map(|c| !c.in_lost_woods)
            .unwrap_or(false);
        if !clear_air {
            return Vec::new();
        }
        self.far_chain_all(from, to, &|p: &Place| self.blocks_departure(&p.key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observe::adjacency::{Exit, Node};
    use crate::overworld::fixtures::*;
    use crate::overworld::*;

    /// A crossing decision, as `(where we stood, where it said to go)`.
    type Decision = (String, String);

    /// Pairs where the walk said `A -> B` somewhere and `B -> A` somewhere else in the same
    /// crossing. That is the shape of every subworld bounce this project has had: not a node
    /// visited twice, which is legal on a route, but two nodes each nominating the other.
    fn reciprocated(d: &[Decision]) -> Vec<(String, String)> {
        let set: std::collections::BTreeSet<&Decision> = d.iter().collect();
        set.iter()
            .filter(|(a, b)| set.contains(&(b.clone(), a.clone())))
            .map(|(a, b)| (a.clone(), b.clone()))
            .collect()
    }

    /// What the run *actually did*, scraped from its own report.
    fn decisions_the_run_made(report: &str) -> Vec<(String, Vec<Decision>)> {
        let mut out: Vec<(String, Vec<Decision>)> = Vec::new();
        let mut here: Option<String> = None;
        let between = |l: &str, a: &str, b: &str| -> Option<String> {
            let rest = l.split_once(a)?.1;
            Some(rest.split_once(b)?.0.to_string())
        };
        for l in report.lines() {
            if let Some(k) = between(l, "arrived at `", "`") {
                here = Some(k);
                continue;
            }
            // Three openers, and `steering` is the one that matters: the deleted arm is half of
            // the alternation the bounce was made of, so a scraper without it reports the old
            // crossing as clean and the whole control silently proves nothing. It did, once.
            let step = ["crossing `", "probing `", "steering `"].iter().find_map(|opener| {
                let rest = l.split_once(opener)?.1;
                let (container, rest) = rest.split_once("` ")?;
                // `via \`` without a leading space: splitting the container off at "` " has
                // already eaten it on a `probing` line, and requiring the space silently dropped
                // every probe -- which left only the steers, and a bounce made of one arm is not
                // a bounce. That is how this control first came back empty.
                let via = between(rest, "via `", "`")?;
                Some((container.to_string(), via))
            });
            if let (Some((container, via)), Some(from)) = (step, here.clone()) {
                if out.last().map(|(c, _)| c.as_str()) != Some(container.as_str()) {
                    out.push((container.clone(), Vec::new()));
                }
                out.last_mut().expect("just pushed").1.push((from, via));
            }
        }
        out
    }

    /// A prayer at the plaza is recorded against its parent, and the plaza must not read as unprayed
    /// because of it.
    ///
    /// **The fixture is the real save**, not an invented one. After the run of 2026-08-17 the live
    /// save holds `shrine1_used = true` and `shrine1_shrine_subs`, with no `shrine1_plaza_*` beside
    /// them, and the checkpoint from before that run has none of the three. So this is the state the
    /// game actually produces when you pray at a plaza — which is the difference between pinning a
    /// rule and pinning a guess about one.
    ///
    /// Without the promotion, `shrine1_plaza` comes back `used = false` from a shrine that has just
    /// been prayed at, `shrine_inside` offers it as an errand again, and the visit finds no `Pray`
    /// to press. The last assertion is the one that would have stopped the next run.
    #[test]
    fn a_plaza_inherits_the_prayer_its_parent_was_credited_with() {
        let mut m = WorldMap::new();
        // Both nodes have to be known before the flags mean anything — the promotion never mints a
        // place, so a plaza we have not seen stays absent rather than becoming a phantom.
        m.fold(&dump(
            "shrine1",
            "Gripthorpe Brush shrine",
            vec![node("shrine1_plaza", "Gripthorpe Brush shrine")],
        ));
        m.apply_save(
            &crate::game::save::parse(
                "return { overworld = { areaFlags = {
                     hell = 0, shrine1_used = true, shrine1_shrine_subs = 1,
                 } } }",
            )
            .unwrap(),
        );

        assert!(m.get("shrine1").unwrap().used, "the parent is where the game wrote it");
        assert!(
            m.get("shrine1_plaza").unwrap().used,
            "and the plaza is the same shrine, so its blessing is claimed too"
        );
        assert!(m.get("shrine1_plaza").unwrap().played, "the word was played there as well");

        // A plaza nobody has seen must not be conjured by the promotion.
        assert!(m.get("shrine2_plaza").is_none(), "no phantom plaza for an unseen shrine");

        // The consecration axis is the same mistake in a different flag, so it is pinned here too:
        // with the portal open, an already-consecrated shrine must not be worth a second trip.
        m.hell = Some(0.1);
        m.entry("shrine1").completed = true;
        m.entry("shrine1_plaza").completed = true;
        assert!(
            m.worth_consecrating_here("shrine1_plaza"),
            "unconsecrated and the portal is open, so this one is genuinely owed"
        );
        m.apply_save(
            &crate::game::save::parse(
                "return { overworld = { areaFlags = {
                     hell = 0.1, shrine1_used = true, shrine1_consecrated = 1,
                 } } }",
            )
            .unwrap(),
        );
        assert!(
            !m.worth_consecrating_here("shrine1_plaza"),
            "the parent carries the consecration, so the plaza is finished with"
        );
    }

    /// A shrine **inside** a subworld is reached through its container, never targeted directly.
    ///
    /// `is_shrine` answers by key or by heading, and only the key arm excludes subnodes. The moment a
    /// dump names one, its heading ends in `shrine` and it becomes a candidate — so this pins the
    /// filter rather than the naming, because the naming is what the errand path needs to keep
    /// working.
    ///
    /// Both halves matter and they pull opposite ways: `next_target` must offer the **container**,
    /// and [`WorldMap::shrine_inside`] must still find the **subnode** once we are in there. A fix
    /// that made `is_shrine` stricter would have satisfied the first and broken the second.
    ///
    /// ## The fixture has to reach the second pass, and the first version did not
    ///
    /// [`WorldMap::next_target`] is `plan(false, true).or_else(|| plan(false, false))`, and only the
    /// **second** call drops the route test — which is the one thing otherwise keeping an interior
    /// node out, since our graph has no edge across a subworld boundary. Written the obvious way,
    /// with the forest left unexplored, this test passed with the filter deleted: an unvisited `l4`
    /// is a frontier, `Goal::Explore` answered from the routed pass, and the fallback never ran.
    ///
    /// So everything else here is deliberately finished — visited, no hidden neighbours, nothing
    /// owed — until the unroutable shrine is the only candidate left and the second pass is the only
    /// thing that can answer. That is the state the filter exists for, and a fixture that does not
    /// build it certifies nothing.
    #[test]
    fn an_interior_shrine_is_reached_through_its_container_not_targeted_directly() {
        let mut m = WorldMap::new();
        m.fold(&dump(
            "l10",
            "Trenwick crossroads",
            vec![node("l4", "Bainton Clump — level 1 forest")],
        ));
        // Standing in the forest teaches us the subnode and its parent, the way arrival does.
        m.fold(&dump(
            "l4sub9",
            "Bainton Clump woodland shrine",
            vec![node("l4sub3", "Bainton Clump road")],
        ));
        m.entry("l4sub9").parent = Some("l4".into());
        m.entry("l4sub3").parent = Some("l4".into());
        m.entry("l4").subworld_container = true;
        m.here = Some("l10".into());
        // Nothing on the surface is owed or unknown, so the routed pass has no answer at all.
        for k in ["l10", "l4"] {
            let p = m.entry(k);
            p.visited = true;
            p.completed = true;
            p.hidden = Some(0);
        }

        assert!(m.get("l4sub9").unwrap().is_shrine(), "the premise: the heading names it a shrine");
        assert!(
            !m.can_route_to("l4sub9"),
            "the premise: no edge crosses a subworld boundary, so only the fallback pass can reach it"
        );

        if let Some(plan) = m.next_target() {
            assert_ne!(
                plan.target, "l4sub9",
                "an interior node is not somewhere `next_hop` can step to: {plan:?}"
            );
        }

        // And the errand path, which is the half that needs the subnode to keep matching. A filter
        // that fixed the above by making `is_shrine` stricter would break this instead.
        m.here = Some("l4sub3".into());
        assert_eq!(
            m.errand_inside("l4").map(|p| p.key.as_str()),
            Some("l4sub9"),
            "once inside, the shrine is exactly what we are looking for"
        );
    }

    /// **A crossing holds the frontier it chose**, so a re-ranking under its feet cannot bounce it.
    ///
    /// The `l48` (Rowlston) loop of 2026-08-20, in the smallest shape that produces it. The run
    /// alternated between the crossroads and a house four times and the guard ended it:
    ///
    /// ```text
    ///   22. l48xrd33x152 — Heart -> l48sub7
    ///   23. l48sub4      — Heart -> l48sub11
    ///   24. l48xrd33x152 — Heart -> l48sub7
    ///   25. l48sub4      — Heart -> l48sub11
    /// ```
    ///
    /// The frontier walk is argued cycle-free because the BFS distance to the chosen frontier
    /// strictly decreases — which holds of a **fixed** frontier and not of one re-ranked from each
    /// new vantage point. Standing somewhere is what makes the game name its neighbours, so the
    /// ranking's own inputs change as the walk proceeds; here that is modelled by the rival frontier
    /// gaining connections between the two steps, which is exactly what a dump does.
    #[test]
    fn a_crossing_does_not_turn_round_when_the_ranking_moves_under_it() {
        let house = |k: &str| node(k, "Rowlston house");
        let build = || {
            let mut m = WorldMap::new();
            // xrd — a — f1  and  xrd — b — f2, so both frontiers are two hops from the crossroads
            // and the walk has a genuine choice.
            m.fold(&inside_dump(
                "l48",
                "xrd",
                "Rowlston crossroads",
                vec![house("a"), house("b")],
                vec![exit("l59")],
            ));
            m.fold(&inside_dump(
                "l48",
                "a",
                "Rowlston house",
                vec![node("xrd", "Rowlston crossroads"), house("f1")],
                vec![exit("l59")],
            ));
            m.fold(&inside_dump(
                "l48",
                "b",
                "Rowlston house",
                vec![node("xrd", "Rowlston crossroads"), house("f2")],
                vec![exit("l59")],
            ));
            m.here = Some("xrd".into());
            m
        };

        let mut m = build();
        let first = m.cross_toward(&[exit("l59")]).expect("a crossing");
        let step_one = match &first {
            Crossing::Probe { to, .. } | Crossing::Seek { to } => to.clone(),
            other => panic!("expected a probe, got {other:?}"),
        };
        assert!(
            step_one == "a" || step_one == "b",
            "the walk sets off toward a frontier: {step_one}"
        );

        // Take the step, and let the far side of the village suddenly look richer — a dump naming
        // the rival's neighbours is all it takes. This is the re-ranking that used to turn the walk
        // round.
        let rival = if step_one == "a" { "f2" } else { "f1" };
        m.here = Some(step_one.clone());
        m.entry(rival).connections = 9;

        let second = m.cross_toward(&[exit("l59")]).expect("a crossing");
        let step_two = match &second {
            Crossing::Probe { to, .. } | Crossing::Seek { to } => to.clone(),
            other => panic!("expected a probe, got {other:?}"),
        };
        assert_ne!(
            step_two, "xrd",
            "the walk turned round at the first re-ranking — this is the l48 bounce"
        );
    }

    /// A crossroads two hops away beats a dead end next door, because it names more of the village.
    ///
    /// The dev, 2026-08-15, after watching a settlement search crawl: *it was making questionable
    /// choices about which frontier node to visit. Could it choose based on the number of
    /// connections on the unvisited node?*
    ///
    /// It could not, and that is the bug in one line: the key was `(paved, distance)`, and distance
    /// measures what a step **costs** with nothing about what it returns. So the nearest scrap of
    /// unexplored ground won every time and a village got searched a cul-de-sac at a time.
    ///
    /// The fixture is the shape that loses under the old rule and wins under the new one: the dead
    /// end is strictly nearer, so any ordering that puts distance first must pick it.
    #[test]
    fn a_frontier_is_chosen_by_what_it_would_reveal_not_by_how_near_it_is() {
        let with_degree = |k: &str, n: u32| Node {
            key: k.into(),
            heading: format!("Rowlston Covert road"),
            x: 0.0,
            y: 0.0,
            connections: n,
        };
        let mut m = WorldMap::new();
        // Standing on a road inside the village. `dead` is adjacent and goes nowhere; `far` is a
        // hop further out and is a six-way crossroads we have never stood on.
        m.fold(&inside_dump(
            "l11",
            "l11sub1",
            "Rowlston Covert road",
            vec![with_degree("dead", 2), with_degree("mid", 3)],
            vec![],
        ));
        m.fold(&inside_dump(
            "l11",
            "mid",
            "Rowlston Covert road",
            vec![with_degree("l11sub1", 3), with_degree("far", 6)],
            vec![],
        ));
        // Back where we started, with `mid` now walked and both frontiers known.
        m.fold(&inside_dump(
            "l11",
            "l11sub1",
            "Rowlston Covert road",
            vec![with_degree("dead", 2), with_degree("mid", 3)],
            vec![],
        ));
        m.gold = 500;
        m.note_health_level(crate::rest::Health { current: 1, max: 20 });

        // The step taken is toward whichever frontier was chosen, so a one-hop `dead` would BE the
        // step. Reaching `far` has to go through `mid`.
        match m.cross_toward(&[]) {
            Some(Crossing::Step { to, .. })
            | Some(Crossing::Probe { to, .. })
            | Some(Crossing::Seek { to }) => assert_eq!(
                to, "mid",
                "the six-way crossroads is worth the extra hop; `dead` is nearer and teaches us two"
            ),
            other => panic!("expected a step into the village, got {other:?}"),
        }
    }

    /// **#81, and the other half of the rule above.** The same shape, with somewhere to be.
    ///
    /// The dev settled degree-before-distance on 2026-08-15 for a village search, and
    /// distance-before-everything on 2026-08-22 for a forest crossing, and the two are not in
    /// conflict: one is looking for something it cannot see, the other is going somewhere it can
    /// name. What was wrong was the question the code asked to tell them apart — `door_now`, the
    /// destination's position *in the latest dump*, which is absent whenever the door is merely off
    /// screen. Most of the way across a large forest, that is every step.
    ///
    /// So a crossing with a perfectly definite target kept reorganising itself by degree, and degree
    /// is global: the richest frontier can be anywhere, so the walk abandoned whichever branch it
    /// was on. Wressle Wood, live 2026-08-22, went four hops out to `l44sub28` and four straight
    /// back past the plaza to `l44sub3` — eight presses to move two nodes, both of them roads, so
    /// `is_paved` was not what did it.
    ///
    /// This fixture is
    /// [`a_frontier_is_chosen_by_what_it_would_reveal_not_by_how_near_it_is`] with an exit handed to
    /// `cross_toward` and nothing else changed. The exit is never folded, so it has no position and
    /// `door_now` is `None` — which is the live state, not a contrivance.
    #[test]
    fn a_crossing_with_a_door_to_head_for_takes_the_nearer_frontier() {
        let with_degree = |k: &str, n: u32| Node {
            key: k.into(),
            heading: "Saltagh Park road".into(),
            x: 0.0,
            y: 0.0,
            connections: n,
        };
        let mut m = WorldMap::new();
        m.fold(&inside_dump(
            "l9",
            "l9sub1",
            "Saltagh Park road",
            vec![with_degree("dead", 2), with_degree("mid", 3)],
            vec![],
        ));
        m.fold(&inside_dump(
            "l9",
            "mid",
            "Saltagh Park road",
            vec![with_degree("l9sub1", 3), with_degree("far", 6)],
            vec![],
        ));
        m.fold(&inside_dump(
            "l9",
            "l9sub1",
            "Saltagh Park road",
            vec![with_degree("dead", 2), with_degree("mid", 3)],
            vec![],
        ));

        // The premise, so a later change cannot make this pass for the wrong reason: we are leaving
        // for a door we can name and cannot see.
        assert_eq!(m.placed_now("l9_path_to_l59"), None, "the door has no position this step");

        match m.cross_toward(&[exit("l59")]) {
            Some(Crossing::Step { to, .. })
            | Some(Crossing::Probe { to, .. })
            | Some(Crossing::Seek { to }) => assert_eq!(
                to, "dead",
                "with a destination, nearest first — `far` teaches more and is a hop further out"
            ),
            other => panic!("expected a step into the forest, got {other:?}"),
        }
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
        m.fold(&inside_dump(
            "l10",
            "l10_path_to_l19",
            "Road to Gipsyville crypt",
            vec![node("l10sub6", "Ulrome guard post")],
            vec![exit("l19")],
        ));
        assert_eq!(m.entered_from.as_deref(), Some("l19"), "the road names where it came from");
        m.fold(&inside_dump(
            "l10",
            "l10sub6",
            "Ulrome guard post",
            vec![node("l10_path_to_l19", "Road to Gipsyville crypt")],
            vec![exit("l19")],
        ));
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
        m.fold(&inside_dump(
            "l10",
            "l10_path_to_l19",
            "Road to Gipsyville crypt",
            vec![node("l10sub6", "Ulrome guard post")],
            vec![exit("l19")],
        ));
        assert_eq!(m.exit_toward(&[exit("l19")]), Some("l19".into()));
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
        m.fold(&inside_dump(
            "l10",
            "l10sub6",
            "Ulrome guard post",
            vec![node("l10sub7", "Ulrome east guard post")],
            vec![exit("l19")],
        ));
        m.fold(&inside_dump(
            "l10",
            "l10sub7",
            "Ulrome east guard post",
            vec![
                node("l10sub6", "Ulrome guard post"),
                node("l10_path_to_l19", "Road to Gipsyville"),
            ],
            vec![exit("l19")],
        ));
        m.fold(&inside_dump(
            "l10",
            "l10sub6",
            "Ulrome guard post",
            vec![node("l10sub7", "Ulrome east guard post")],
            vec![exit("l19")],
        ));
        // Not a jump to the exit — the neighbour on the way to it. Clicking the exit itself is what
        // moved the screen 0.015 and stalled a live run.
        assert_eq!(
            m.cross_toward(&[exit("l19")]),
            Some(Crossing::Step { to: "l10sub7".into(), toward: "l10_path_to_l19".into() })
        );
    }

    /// The `l4` loop of 2026-08-16, and the write-off that breaks it.
    ///
    /// A frontier stays a frontier while the game withholds a neighbour (`is_frontier` is
    /// `!visited || hidden > 0`), so standing on one need not retire it — which is how the crossing
    /// kept electing `l4sub14` while the steer kept pushing back off it. Writing it off has to be
    /// enough on its own to send the crossing somewhere else, because that is all
    /// `navigate::LOOP_WRITE_OFF` does.
    ///
    /// **Two neighbours, not one, and that is the whole fixture.** Written with a single neighbour
    /// this test fails, and correctly: `cross_toward` ends in an *unfiltered* pick
    /// (`place.neighbours.iter().min()`) on the reasoning that standing still in a subworld is worse
    /// than any one bad step. So a write-off redirects a crossing that has somewhere else to go and
    /// does not strand one that has not — see the note on the residual case in
    /// `navigate::LOOP_WRITE_OFF`.
    #[test]
    fn a_frontier_that_is_written_off_sends_the_crossing_elsewhere() {
        let mut m = WorldMap::new();
        // No route to any exit: the state that leaves crossing to the frontier walk. Both
        // neighbours are named and neither is stood on, so both are frontiers.
        m.fold(&inside_dump(
            "l10",
            "l10sub6",
            "Ulrome guard post",
            vec![
                node("l10sub7", "Ulrome east guard post"),
                node("l10sub8", "Ulrome south guard post"),
            ],
            vec![exit("l19")],
        ));
        let chosen = match m.cross_toward(&[exit("l19")]) {
            Some(Crossing::Probe { to, .. }) | Some(Crossing::Seek { to }) => to,
            other => panic!("the premise: a frontier is probed, got {other:?}"),
        };

        assert!(!m.is_written_off(&chosen));
        m.abandon(&chosen);
        assert!(m.is_written_off(&chosen), "and a repeat write-off can be told from a fresh one");

        let after = m.cross_toward(&[exit("l19")]);
        match after {
            Some(Crossing::Probe { to, .. }) | Some(Crossing::Seek { to }) => assert_ne!(
                to, chosen,
                "written off and still chosen, so the guard would change nothing"
            ),
            other => panic!("still a crossing, just a different one: {other:?}"),
        }
    }

    #[test]
    fn standing_on_the_road_out_means_leave() {
        let mut m = WorldMap::new();
        m.fold(&inside_dump(
            "l10",
            "l10_path_to_l19",
            "Road to Gipsyville crypt",
            vec![node("l10sub7", "Ulrome east guard post")],
            vec![exit("l19")],
        ));
        assert_eq!(m.cross_toward(&[exit("l19")]), Some(Crossing::Leave { to: "l19".into() }));
    }

    #[test]
    fn an_unknown_interior_is_explored_rather_than_refused() {
        // The normal early state: fog reveals one hop, so on arrival there is no route to anywhere.
        // Refusing here is what left a run standing in a village with nothing to do.
        let mut m = WorldMap::new();
        m.fold(&inside_dump(
            "l10",
            "l10sub6",
            "Ulrome guard post",
            vec![node("l10sub7", "Ulrome east guard post")],
            vec![exit("l19")],
        ));
        assert_eq!(
            m.cross_toward(&[exit("l19")]),
            Some(Crossing::Probe { to: "l10sub7".into(), toward: "l10_path_to_l19".into() })
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
    fn leaving_a_subworld_picks_the_exit_nearest_the_target() {
        use crate::observe::adjacency::Exit;
        let mut m = WorldMap::new();
        // Outside: goal — near — container, and a far dead end.
        m.fold(&dump(
            "near",
            "a road",
            vec![node("goal", "Grim Barrow — level 4 crypt"), node("cave", "a forest")],
        ));
        m.fold(&dump("far", "elsewhere", vec![node("cave", "a forest")]));
        let mut inside = dump("cavesub1", "a clearing", vec![]);
        inside.subworld = Some(("cave".into(), "a forest".into()));
        inside.exits = vec![
            Exit { x: 0.0, y: 0.0, to_key: "far".into(), to_heading: "elsewhere".into() },
            Exit { x: 0.0, y: 0.0, to_key: "near".into(), to_heading: "a road".into() },
        ];
        m.fold(&inside);
        assert_eq!(
            m.exit_toward(&inside.exits).as_deref(),
            Some("near"),
            "the exit that gets us closer"
        );
    }

    /// **The console says it first, and the save only confirms it hours later.**
    ///
    /// The mist event writes `lost_woods_known_<key>` and enters the subworld in one `onSelect`
    /// (`overworld/events/arrived/lost_woods.lua:23-27`), but `mainSaveData` is written on screen
    /// **exit** — so for a crossing the flag does not reach us until the whole woods has been
    /// walked. `e3` on 2026-08-21 was 69 steps of that.
    ///
    /// The end state has to be identical to the one the save produces, which is what this pins:
    /// the same node, the same wall, from either channel.
    #[test]
    fn a_lost_woods_named_by_the_event_walls_off_exactly_as_the_save_would() {
        // The short way through the woods, before either channel has said anything.
        let mut m = two_routes();
        assert_eq!(m.next_hop().unwrap().step, "woods", "the premise: the woods is the short way");

        m.mark_lost_woods("woods");
        let hop = m.next_hop().unwrap();
        assert_eq!(hop.step, "a", "the long way round, from the console alone");
        assert_eq!(hop.plan.target, "goal", "and the goal is untouched");

        // **Both flags, and they are not the same fact.** `avoid` keeps us out; `in_lost_woods` is
        // what `far_hop_inside` reads to refuse a fast hop through fog, and it has to be true from
        // the first dump inside rather than the second.
        let p = m.get("woods").expect("the woods is on the map");
        assert!(p.avoid, "a wall for routing");
        // `in_lost_woods` is the flag `far_hop_inside` refuses on — pinned against a map that is
        // actually standing inside one by `a_forest_crosses_in_one_press_but_a_lost_woods_does_not`,
        // which is where that link belongs. Asserting it from out here would only re-measure
        // `clear_air` returning `None` because we are on the surface.
        assert!(p.in_lost_woods, "and fog for the fast hop");

        // The save channel, on an untouched copy of the same map, must reach the same place.
        let mut by_save = two_routes();
        by_save.apply_save(
            &crate::game::save::parse(
                r#"return { overworld = { areaFlags = { hell = 0, lost_woods_known_woods = 1 } } }"#,
            )
            .unwrap(),
        );
        assert_eq!(
            by_save.next_hop().unwrap().step,
            "a",
            "the save gets to the same answer, only later"
        );

        // Idempotent, which is the state after any restart that read the save.
        m.mark_lost_woods("woods");
        assert_eq!(m.next_hop().unwrap().step, "a");
    }

    /// An unvisited subworld container counts as hostile, because a bandit camp is an ordinary
    /// forest until you are standing in it (`overworld/generators/world.lua:466-475`).
    /// Standing on a node that blocks going onward: fight it, hurt or not.
    ///
    /// **This test used to assert the opposite**, and the reversal is a scope decision rather than a
    /// discovery. Backing out was legal — `canTravelToDirect` needs one endpoint complete and the
    /// node behind us is — and it was introduced after a run at 0 health stepped onto this very
    /// node, a level 6 guard post, and fought it.
    ///
    /// What backing out then cost, in order: the `l9sub16` <-> `l9sub11` cycle; a guard against that
    /// cycle which could never fire; a replacement guard that took the fight on the second visit
    /// anyway, so the rule bought one wasted round trip; and finally a run stopped in front of a
    /// **level 1** spider nest at 1/20, twenty-four steps into a forest it never crossed. The dev
    /// called it off: for the MVP, stick to the path and fight through what is on it.
    ///
    /// The protection that replaces it is upstream, in destination choice — `Risk`,
    /// `hostile_to_enter` and `next_target`'s two passes still keep a hurt run from *choosing* to
    /// walk into hostile ground.
    #[test]
    fn blocked_by_a_fight_we_take_it_hurt_or_not() {
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
        assert_eq!(m.cross_toward(&exits), Some(Crossing::Fight { at: "l10sub11".into() }));

        // Hurt: the same answer. Stepping back to `l10sub10` is still legal -- it is complete, and
        // that is what used to be taken -- but the crossing no longer offers it, because a detour
        // that ends in the same fight two steps later is not a detour.
        m.note_health_level(crate::rest::Health { current: 0, max: 12 });
        assert_eq!(
            m.cross_toward(&exits),
            Some(Crossing::Fight { at: "l10sub11".into() }),
            "the path goes through it"
        );
    }

    /// The door's position steers a crossing its key cannot route.
    ///
    /// Rebuilt from `l2` on 2026-08-09 (`spike-run-raw.log:2185-2210`), where this cost 22 of the
    /// run's 62 steps. The exits section gives the door at `(1479, -130)` and never gives its key, so
    /// `l2_path_to_l1` is not a node anything can route to — and the frontier walk, choosing between
    /// two equally near unvisited neighbours, falls to the lower key and picks the one pointing away.
    ///
    /// The coordinates are the real ones for the door and for `l2sub13`; the two candidates are
    /// placed to put the alphabet and the door in opposition, which is the whole question.
    #[test]
    fn a_door_we_cannot_route_to_still_says_which_way_to_go() {
        let door = Exit {
            x: 1479.0,
            y: -130.0,
            to_key: "l1".into(),
            to_heading: "Cowlam — level 7 crypt".into(),
        };
        let at = |key: &str, x: f64, y: f64| Node {
            key: key.into(),
            heading: "Dotterel Hedge house".into(),
            x,
            y,
            connections: 4,
        };
        let mut m = WorldMap::new();
        // One step inside. No measure yet, so this dump is the frontier walk's -- see `cross_toward`.
        m.fold(&inside_dump(
            "l2",
            "l2sub7",
            "Dotterel Hedge house",
            vec![at("l2sub13", 926.0, 413.0), at("l2sub4", 840.0, 636.0)],
            vec![door.clone()],
        ));
        assert!(
            matches!(m.cross_toward(&[door.clone()]), Some(Crossing::Probe { .. })),
            "nothing to descend on until a move has been measured"
        );

        // Arrived at `l2sub13`, which the dump above placed -- so the measure rolls forward.
        m.fold(&inside_dump(
            "l2",
            "l2sub13",
            "Dotterel Hedge chapel",
            vec![
                at("l2sub12", 1045.0, 679.0),
                at("l2sub22", 1300.0, 100.0),
                at("l2sub7", 700.0, 700.0),
            ],
            vec![door.clone()],
        ));
        assert!(
            m.first_step_toward("l2sub13", "l2_path_to_l1", false).is_none(),
            "the door has no edges, which is the whole predicament"
        );

        // **The aim survived #57 even though the arm did not.** This asserted a `Crossing::Steer`
        // until then; the door's printed position is now the `doorward` term in the frontier
        // ranking, so the same fixture picks the same node for the same reason — and picks it from
        // among nodes still worth visiting, which the steer never checked.
        //
        // All three candidates are one hop away and none is paved, so `!is_paved` and `hops` tie and
        // `doorward` decides: `l2sub22` at (1300, 100) is much the nearest to a door at (1479, -130).
        // `l2sub12` won on the alphabet under the old key, which is what made this fixture worth
        // keeping.
        match m.cross_toward(&[door]) {
            Some(Crossing::Probe { to, toward }) => {
                assert_eq!(toward, "l2_path_to_l1");
                assert_eq!(
                    to, "l2sub22",
                    "toward the door; `l2sub12` is nearer the front of the alphabet"
                );
            }
            other => panic!("expected the walk to head doorward, got {other:?}"),
        }
    }

    /// **A steer will not walk back into the node the frontier walk just left.** Task #57.
    ///
    /// The `l63_plaza` ↔ `l63xrd60x-183` bounce of the 1519Z run, three clean laps inside Upton
    /// Braken, and the shape is *two convergence arguments defeating each other*.
    ///
    /// Each arm is sound alone. `Crossing::Steer` carries a monotonic ceiling on the gap to the
    /// door, so a steer must strictly improve on the last one. `Crossing::Probe` holds its frontier
    /// target in `probing_toward`, which is what makes "the BFS distance strictly decreases" true
    /// rather than merely plausible. **Neither device constrains the other arm**, and they
    /// alternate: the frontier walk steps *away* from the door to somewhere worth learning, and from
    /// there the steer sees the node we just left as an improvement and goes back.
    ///
    /// A one-step memory is the cheap half of the cure — the dev's call, 2026-08-21 — and it belongs
    /// on the steer alone. The frontier arm returns `first_step_toward`, a real route, and routes
    /// sometimes have to pass back through a node; the steer is the arm that picks by straight-line
    /// guess with no route at all, and *that* is the one that needs to remember.
    #[test]
    fn a_steer_does_not_walk_back_into_the_node_the_probe_just_left() {
        let door = Exit {
            x: 1479.0,
            y: -130.0,
            to_key: "l43".into(),
            to_heading: "Wold Newton — level 4 crypt".into(),
        };
        let at = |key: &str, x: f64, y: f64| Node {
            key: key.into(),
            heading: "Upton Braken house".into(),
            x,
            y,
            connections: 4,
        };

        let mut m = WorldMap::new();
        // Standing on the plaza, which sits close to the door. Nothing measured yet, so this is the
        // frontier walk — and it steps *away*, to somewhere that can still teach us something.
        m.fold(&inside_dump(
            "l63",
            "l63_plaza",
            "Upton Braken plaza",
            vec![at("l63xrd", 200.0, 900.0), at("l63sub3", 260.0, 940.0)],
            vec![door.clone()],
        ));
        let away = m.cross_toward(&[door.clone()]);
        assert!(
            matches!(away, Some(Crossing::Probe { .. }) | Some(Crossing::Seek { .. })),
            "the frontier walk goes first, got {away:?}"
        );

        // Arrived at the crossroads. The plaza is now much the nearest thing to the door, and
        // stepping back to it is an improvement by every measure the steer has.
        m.fold(&inside_dump(
            "l63",
            "l63xrd",
            "Upton Braken crossroads",
            vec![at("l63_plaza", 1400.0, -100.0), at("l63sub3", 260.0, 940.0)],
            vec![door.clone()],
        ));

        // **And there is now only one arm to answer.** Before #57 this had to accept either a steer
        // that had learned not to go back, or a hand-down to the frontier walk. With the steer folded
        // into the ranking there is nothing that *can* nominate the plaza: it has been stood on and
        // its neighbours named, so `nothing_left_to_reveal` retires it, and being the nearest thing
        // to the door no longer buys it a second look.
        let step = m.cross_toward(&[door]).and_then(|c| match c {
            Crossing::Step { to, .. } | Crossing::Probe { to, .. } | Crossing::Seek { to } => {
                Some(to)
            }
            _ => None,
        });
        assert_ne!(step.as_deref(), Some("l63_plaza"), "that is the node we were just standing on");
    }

    /// **A pocket pointing the wrong way is walked out of, not sat in.**
    ///
    /// Straight-line distance knows nothing of walls, so a crossing routinely reaches ground where
    /// every way on is *further* from the door than where it stands. Before #57 that was the steer
    /// declining and handing to the frontier walk; now it is one ranking, and the property to hold is
    /// the same — the crossing keeps moving, toward whatever is still worth learning, rather than
    /// stalling because nothing improves.
    ///
    /// The name and shape are kept from `a_steer_that_does_not_gain_ground_yields_to_exploring`,
    /// because the fixture is a real one: `l2` on 2026-08-09, the village that cost 22 of a run's 62
    /// steps.
    #[test]
    fn a_pocket_pointing_away_from_the_door_is_still_walked_out_of() {
        let door = Exit {
            x: 1479.0,
            y: -130.0,
            to_key: "l1".into(),
            to_heading: "Cowlam — level 7 crypt".into(),
        };
        let at = |key: &str, x: f64, y: f64| Node {
            key: key.into(),
            heading: "Dotterel Hedge house".into(),
            x,
            y,
            connections: 4,
        };
        let mut m = WorldMap::new();
        m.fold(&inside_dump(
            "l2",
            "l2sub7",
            "Dotterel Hedge house",
            vec![at("l2sub13", 926.0, 413.0)],
            vec![door.clone()],
        ));
        m.cross_toward(&[door.clone()]);
        // Standing on `l2sub13`, and every way on is further from the door than it was: a pocket
        // pointing the wrong way, which straight-line distance cannot see coming.
        m.fold(&inside_dump(
            "l2",
            "l2sub13",
            "Dotterel Hedge chapel",
            vec![at("l2sub12", 200.0, 900.0), at("l2sub9", 150.0, 1000.0)],
            vec![door.clone()],
        ));
        match m.cross_toward(&[door]) {
            Some(Crossing::Probe { .. }) | Some(Crossing::Seek { .. }) => {}
            other => panic!("expected the walk to carry on rather than stall, got {other:?}"),
        }
    }

    /// A leaf is not worth the two steps it costs, so the crossing does not offer one.
    ///
    /// `e1sub65`, `e1sub67` and `e1sub84` were each entered and immediately left in the run of
    /// 2026-08-09, six of its twenty-three steps. Every dump that named them said `connections: 1`,
    /// so this was knowable before walking in.
    #[test]
    fn a_dead_end_is_not_somewhere_to_explore() {
        let leaf = |key: &str| Node {
            key: key.into(),
            heading: "Howden Timberland forest".into(),
            x: 0.0,
            y: 0.0,
            connections: 1,
        };
        let mut m = WorldMap::new();
        m.fold(&Adjacency {
            subworld: Some(("e1".into(), "Howden Timberland — level 2 lost woods".into())),
            exits: Vec::new(),
            ..dump(
                "e1sub69",
                "Howden Timberland forest",
                vec![leaf("e1sub67"), node("e1sub75", "Howden Timberland forest")],
            )
        });
        assert!(m.get("e1sub67").unwrap().nothing_left_to_reveal());
        assert!(!m.get("e1sub75").unwrap().nothing_left_to_reveal());
        match m.cross_toward(&[]) {
            Some(Crossing::Seek { to }) | Some(Crossing::Probe { to, .. }) => {
                assert_eq!(to, "e1sub75", "the leaf has nothing behind it");
            }
            other => panic!("expected a step past the leaf, got {other:?}"),
        }
    }

    /// The regression itself: thick fog is not a dead end on the **first** step either.
    ///
    /// `cross_toward` returned `None` here and the driver ended the run with
    /// `inside e1 with no crossing plan`. There was never nothing to do — three neighbours were
    /// named in the same dump that hid the exits.
    #[test]
    fn a_fogged_arrival_explores_instead_of_giving_up() {
        let mut m = a_lost_woods();
        // **The chest is put out of the way on purpose.** `e1sub1` is an unopened chest, and since
        // task #18 that is an errand — `cross_toward` would head straight for it and this test would
        // pass without the fog fallback existing at all. Completing it takes the errand off the
        // board and leaves the question the test is named for: three neighbours, no exits, no route.
        m.entry("e1sub1").completed = true;
        match m.cross_toward(&[]) {
            Some(Crossing::Seek { to }) | Some(Crossing::Probe { to, .. }) => {
                assert_eq!(to, "e1sub3", "the crossroads: paved outranks the rest");
            }
            other => panic!("fog is not a dead end, got {other:?}"),
        }
    }

    /// The chest the run of 2026-08-16 walked past, twice.
    ///
    /// `l4sub11 Bainton Clump — level 1 chest`, printed in the adjacency dump one hop from where we
    /// stood (`spike-run-20260816-0402Z.log:2207`), and the crossing walked to the exit instead.
    /// [`Goal::Chest`] had landed that morning and could not have helped: it plans over surface
    /// nodes, and there are no chests on the surface — see [`WorldMap::chest_inside`].
    ///
    /// Inside a subworld the destination comes from `cross_toward`, which knew the inn and the
    /// general store by name and treated everything else as "head for the door".
    #[test]
    fn a_chest_inside_a_forest_is_a_destination_and_not_scenery() {
        let mut m = a_lost_woods();
        assert!(m.get("e1sub1").unwrap().is_chest(), "the fixture's chest must be a chest");

        // Where the crossing puts its next foot, whichever kind of move it decided on. `Seek` is one
        // of the answers here — it is what a crossing with no destination does — so this cannot
        // insist on `Step`, and the positive case checks the variant separately.
        let step_to = |c: Option<Crossing>| match c {
            Some(Crossing::Step { to, .. })
            | Some(Crossing::Probe { to, .. })
            | Some(Crossing::Seek { to }) => to,
            other => panic!("expected a move, got {other:?}"),
        };

        // Standing on the plaza, an unopened chest one hop away, nothing else asking for us.
        let plan = m.cross_toward(&[]);
        assert!(
            matches!(&plan, Some(Crossing::Step { toward, .. }) if toward == "e1sub1"),
            "a chest one hop away is a destination we have a route to, got {plan:?}"
        );
        assert_eq!(step_to(plan), "e1sub1", "and the step is onto it");

        // **Not while hurt.** Opening one is a fight (`forest.lua:30-39`), and the dev's rule names
        // rest as the exception: *when the goal is not rest and a chest is visible, detour to it.*
        let mut hurt = a_lost_woods();
        hurt.wants_rest = true;
        assert_ne!(
            step_to(hurt.cross_toward(&[])),
            "e1sub1",
            "a fight is not a rest, so a hurt run does not walk to a chest"
        );

        // And an opened one is scenery again — `getAreaButtons` offers nothing at all on a complete
        // chest (`forest.lua:186-188`).
        let mut done = a_lost_woods();
        done.entry("e1sub1").completed = true;
        assert_ne!(step_to(done.cross_toward(&[])), "e1sub1", "an emptied chest is not an errand");
    }

    /// The `l10` ↔ `l18` ping-pong: inside and outside must not disagree about what we have done.
    ///
    /// Ulrome, 2026-08-16, the third such loop in two days. Consecutive lines of the report:
    ///
    /// ```text
    /// 7. l18 -> **l10** (for start, CloseAnomaly)
    /// 9. crossing `l10` toward `l10_path_to_l18` … door choice: Heart -> l32
    /// ```
    ///
    /// Outside, the errand was the anomaly and the way there ran through the village. Inside, it was
    /// a heart at `l32`, whose nearest door was the one we had just come in by. Each answer undid
    /// the other, and each lap paid to enter a village under attack.
    ///
    /// The cause was not the ranking and not the errand ordering: `choose_exit`'s outside vantage
    /// was a **hand-assembled** map that dropped `heart_bought`, so from inside the village the
    /// planner had forgotten every shop it had ever emptied. The save says otherwise —
    /// `l32_shops.generalStoreStock.inventoryHash.healthBuff.stock = 0` in
    /// `checkpoints/ping-pong-l10-l18`.
    ///
    /// The test is written against the *symptom* — that the two vantage points agree — rather than
    /// against `heart_bought` specifically, because the next field to be forgotten will not be that
    /// one.
    #[test]
    fn the_view_from_inside_a_village_remembers_what_the_view_from_outside_does() {
        use crate::observe::adjacency::Exit;
        let door = |to: &str| Exit { x: 0.0, y: 0.0, to_key: to.into(), to_heading: "".into() };

        let mut m = WorldMap::default();
        for (a, b) in [("l10", "l18"), ("l10", "l7"), ("l18", "l32")] {
            m.entry(a).neighbours.insert(b.into());
            m.entry(b).neighbours.insert(a.into());
        }
        m.entry("l10").heading = "Ulrome — level 6 village".into();
        m.entry("l18").heading = "Stanningholme crypt".into();
        m.entry("l32").heading = "Enthorpe village".into();
        m.entry("l7").heading = "Greenoak Backwoods campfire".into();
        m.entry("l10sub7").parent = Some("l10".into());
        m.here = Some("l10sub7".into());
        // Enthorpe's shelf is empty and the purse is full — the exact state of the checkpoint.
        m.apply_save(
            &crate::game::save::parse(
                "return { player = { gold = 294 }, overworld = { areaFlags = { hell = 0.1,
                     l32_shops = { generalStoreStock = { inventoryHash = {
                         healthBuff = { stock = 0, type = \"healthBuff\" } } } } },
                     completedAreas = { l18 = true, l32 = true } } }",
            )
            .unwrap(),
        );
        assert!(
            m.wants_a_heart(),
            "294 gold is over the floor, so the errand is live in principle"
        );
        assert!(m.heart_is_spent("l32"), "and the save says that shelf is bare");

        // The question the crossing asks, and the question the surface asks, must not disagree about
        // an errand that is already done.
        let (chosen, _, note) =
            m.choose_exit(&[door("l18"), door("l7")]).expect("two doors is not no doors");
        assert!(
            !note.starts_with("Heart"),
            "the view from inside forgot a shop it had emptied: {note}"
        );
        assert_ne!(
            chosen, "l18",
            "and so it left by the door it came in through, which is the ping-pong"
        );
    }

    /// How far one press may carry us, and every reason the chain stops.
    ///
    /// The dev, 2026-08-16: *make it possible for the navigator to directly hop to distant, visited
    /// nodes as long as the corruption hasn't cut off the path.* Corruption is precisely what cuts
    /// it: `setAreaIncomplete` (`overworldview.lua:179-206`) takes completion from a node **and
    /// every road out of it**, and completion on one end of an edge is what `canTravelToDirect`
    /// needs (`:1316-1321`).
    #[test]
    fn one_press_reaches_as_far_as_the_cleared_road_goes() {
        let chain = |cleared: &str, headings: Vec<(&str, &str)>| {
            let mut m = WorldMap::default();
            for (a, b) in [("a", "b"), ("b", "c"), ("c", "d")] {
                m.entry(a).neighbours.insert(b.into());
                m.entry(b).neighbours.insert(a.into());
            }
            for (k, h) in headings {
                m.entry(k).heading = h.into();
            }
            m.here = Some("a".into());
            m.apply_save(
                &crate::game::save::parse(&format!(
                    "return {{ overworld = {{ areaFlags = {{ hell = 0.1 }},
                         completedAreas = {{ {cleared} }} }} }}"
                ))
                .unwrap(),
            );
            m
        };

        // Every node cleared: the whole chain is one press.
        assert_eq!(
            chain("a = true, b = true, c = true, d = true", vec![]).far_hop("a", "d").as_deref(),
            Some("d")
        );
        // `c` uncleared breaks the edge `c -> d`: `b -> c` is still legal because `b` is complete, so
        // the press stops at `c` — which is where an ordinary step would have taken us next anyway,
        // two hops on.
        assert_eq!(chain("a = true, b = true", vec![]).far_hop("a", "d").as_deref(), Some("c"));
        // Nothing cleared past `a`: the first edge is legal, the second is not, and one hop is not a
        // multi-hop — so the caller is told nothing and steps as it always did.
        assert_eq!(chain("a = true", vec![]).far_hop("a", "d"), None);

        // **A level 4+ node in the way stops the chain short of itself**, because the walk is not a
        // teleport: `core.arriveAt` runs at every node on the path (`overworldview.lua:1210-1216`),
        // so striding through a level 5 crypt takes its fight, and pre-anomaly opens the portal too.
        //
        // The dev's rule, 2026-08-17: *make sure the in-game traveller doesn't cross over a level 4+
        // node if it's in between.* So the chain reaches `b`, which is adjacent to `a`, and one hop
        // is not a multi-hop — the caller is told nothing and steps as it always did, which is where
        // `gentler_ground_remains` and the health gate get their say.
        //
        // This used to answer `Some("c")` on the reasoning "up to it, never through it". That is
        // only defensible when the node is the next ordinary step anyway; several hops along it
        // commits the run to a fight nothing chose.
        let mut shut =
            chain("a = true, b = true, c = true, d = true", vec![("c", "Crockey — level 5 crypt")]);
        shut.hell = Some(0.0);
        assert_eq!(shut.far_hop("a", "d"), None, "stops short of it, and never lands on it");

        // **And with the portal already open.** The old guard was `shut && triggers_anomaly()`, so
        // this case chained straight through a level 5 crypt. A fight we did not choose is a fight
        // we did not choose whether or not the anomaly is open.
        let mut open =
            chain("a = true, b = true, c = true, d = true", vec![("c", "Crockey — level 5 crypt")]);
        open.hell = Some(0.1);
        assert_eq!(
            open.far_hop("a", "d"),
            None,
            "the portal's state is not what makes it dangerous"
        );

        // A level *3* node is ordinary ground and the chain runs the whole way, which is what keeps
        // this a rule about danger rather than a rule against headings.
        assert_eq!(
            chain(
                "a = true, b = true, c = true, d = true",
                vec![("c", "Rookdale — level 3 crypt")]
            )
            .far_hop("a", "d")
            .as_deref(),
            Some("d")
        );
    }

    /// Leaving a town in one press, and the two things that still stop it.
    ///
    /// The dev, 2026-08-16: *the multi-hop in that town still didn't work when we were trying to
    /// leave town after resting at the inn.* It could not: `far_hop` refuses outright inside any
    /// subworld. See [`WorldMap::far_hop_inside`] for why that was too broad — `thickFog` is set in
    /// exactly one file in the game, and the 0.015 measurement that seemed to back the rule was a
    /// **corrupted** interior with nothing complete to travel from.
    #[test]
    fn leaving_a_settlement_is_one_press_when_the_way_is_clear() {
        // Inn -> two crossroads -> the road out, the walk that prompted this.
        let town = |cleared: &str, headings: Vec<(&str, &str)>| {
            let mut m = WorldMap::default();
            for (a, b) in
                [("l25sub2", "l25xrd1"), ("l25xrd1", "l25xrd2"), ("l25xrd2", "l25_path_to_l4")]
            {
                m.entry(a).neighbours.insert(b.into());
                m.entry(b).neighbours.insert(a.into());
            }
            m.entry("l25").heading = "Aike town".into();
            for k in ["l25sub2", "l25xrd1", "l25xrd2", "l25_path_to_l4"] {
                m.entry(k).parent = Some("l25".into());
            }
            for (k, h) in headings {
                m.entry(k).heading = h.into();
            }
            m.here = Some("l25sub2".into());
            m.apply_save(
                &crate::game::save::parse(&format!(
                    "return {{ overworld = {{ areaFlags = {{ hell = 0.1 }},
                         completedAreas = {{ {cleared} }} }} }}"
                ))
                .unwrap(),
            );
            m
        };
        let all = "l25sub2 = true, l25xrd1 = true, l25xrd2 = true, l25_path_to_l4 = true";

        assert_eq!(
            town(all, vec![]).far_hop_inside("l25sub2", "l25_path_to_l4").as_deref(),
            Some("l25_path_to_l4"),
            "three hops of inn-to-door in one press"
        );

        // **And it is never a node the dump names**, which is what made the first call site dead.
        //
        // `far_chain` ends in `!can_step_is_adjacent`, so the key it returns is by construction not
        // a neighbour of where we stand — while `Adjacency::nodes` is the dump's *adjacent
        // connections*, which is exactly where `fold` builds `neighbours` from. Looking for one in
        // the other can never match, so the far hop inside a settlement could not fire at all, and
        // three live runs crossed uncorrupted villages hop by hop with the feature "landed".
        //
        // The position that does exist for it is in `Adjacency::exits`, which prints every road out
        // of the container at any distance. See the crossing branch in `navigate::drive`.
        let m = town(all, vec![]);
        let far = m.far_hop_inside("l25sub2", "l25_path_to_l4").expect("the hop above");
        assert!(
            !m.get("l25sub2").unwrap().neighbours.contains(&far),
            "a far hop that is adjacent is not a far hop — so it is never in the dump's node list"
        );
        // The surface entry point still refuses inside, which is what keeps a fogged forest from
        // trying this. Same map, same route, different rule.
        assert_eq!(town(all, vec![]).far_hop("l25sub2", "l25_path_to_l4"), None);

        // **A level 6 guard post on the way stops the chain short of itself**, by the same rule the
        // surface hop uses: arrivals fire at every node on the path, and leaving a town after a rest
        // is the one errand deliberately not looking for a fight. The chain gets as far as
        // `l25xrd1`, which is adjacent, so the crossing steps as it always did.
        let guarded = town(
            "l25sub2 = true, l25xrd1 = true",
            vec![("l25xrd2", "Aike guard post — level 6 crypt")],
        );
        assert_eq!(guarded.far_hop_inside("l25sub2", "l25_path_to_l4"), None);

        // **Corruption on the container does not decide this, and used to.** The dev, 2026-08-22:
        // *the corruption is only a one-time clearing of a node's visited state. Once we re-visit
        // the nodes, they become fast-hoppable again.* So a corrupted town whose route has been
        // re-cleared hops exactly like any other — the flag says nothing about the ground in front
        // of us.
        let mut corrupt = town(all, vec![]);
        corrupt.entry("l25").corrupted = true;
        assert_eq!(
            corrupt.far_hop_inside("l25sub2", "l25_path_to_l4").as_deref(),
            Some("l25_path_to_l4"),
            "a re-cleared route hops whether or not the container was ever corrupted"
        );

        // **What corruption actually costs is completion, one node at a time** — and that is the
        // `guarded` case above, already covered by `can_travel_direct`. Same corrupted container,
        // route not re-cleared: refused, and refused for the reason that can stop being true.
        let mut wiped = town("l25sub2 = true", vec![]);
        wiped.entry("l25").corrupted = true;
        assert_eq!(wiped.far_hop_inside("l25sub2", "l25_path_to_l4"), None);
    }

    /// A forest crosses in one press too — and only a **lost woods** does not.
    ///
    /// The dev, 2026-08-17: *the forest navigator still isn't taking advantage of fast-hops for
    /// visited nodes.* It was restricted to settlements on the grounds that a heading cannot tell a
    /// forest from a lost woods. True from outside, and irrelevant here: the mist event writes
    /// `lost_woods_known_<key>` in the same `onSelect` that calls `enterSubworld`
    /// (`overworld/events/arrived/lost_woods.lua:23-27`), so by the time we are inside one the
    /// heading already says `lost woods` and [`Place::in_lost_woods`] carries it.
    #[test]
    fn a_forest_crosses_in_one_press_but_a_lost_woods_does_not() {
        let forest = |container_heading: &str| {
            let mut m = WorldMap::default();
            for (a, b) in [("l4sub1", "l4sub2"), ("l4sub2", "l4_path_to_l25")] {
                m.entry(a).neighbours.insert(b.into());
                m.entry(b).neighbours.insert(a.into());
            }
            m.entry("l4").heading = container_heading.into();
            for k in ["l4sub1", "l4sub2", "l4_path_to_l25"] {
                m.entry(k).parent = Some("l4".into());
            }
            m.here = Some("l4sub1".into());
            m.apply_save(
                &crate::game::save::parse(
                    "return { overworld = { areaFlags = { hell = 0.1 }, completedAreas = {
                         l4sub1 = true, l4sub2 = true, l4_path_to_l25 = true } } }",
                )
                .unwrap(),
            );
            m
        };

        assert_eq!(
            forest("Bainton Clump — level 1 forest")
                .far_hop_inside("l4sub1", "l4_path_to_l25")
                .as_deref(),
            Some("l4_path_to_l25"),
            "an ordinary forest is ordinary ground"
        );

        // The one that fogs. `in_lost_woods` is set by `fold` from the container's live heading;
        // set directly here because this test is about the gate and not about folding.
        let mut lost = forest("Howden Timberland — level 2 lost woods");
        lost.entry("l4").in_lost_woods = true;
        assert_eq!(lost.far_hop_inside("l4sub1", "l4_path_to_l25"), None);

        // **And corruption does not**, which is the correction of 2026-08-22. `setAreaIncomplete`
        // takes completion away once; re-clearing gives it back. A container flag cannot express
        // that, and while it tried, the 2002Z run walked `l4` and `l10` a node at a time from end to
        // end — both corrupted, so every chain came back empty and nothing said why.
        let mut corrupt = forest("Bainton Clump — level 1 forest");
        corrupt.entry("l4").corrupted = true;
        assert_eq!(
            corrupt.far_hop_inside("l4sub1", "l4_path_to_l25").as_deref(),
            Some("l4_path_to_l25"),
            "a corrupted forest whose route is clear hops like any other"
        );
    }

    /// **#80.** The chain offers the shorter hops too, so an unclickable far node is not the end.
    ///
    /// `far_hop_inside` answers with the furthest node and nothing else, and the driver's only
    /// recourse when that node was off the clickable map was to give up the hop entirely and take a
    /// single step. Live 2026-08-22 in `l4` toward `l4_path_to_shrine1`: refused six times running,
    /// the door travellable in one press throughout, one refusal 86 px past the right edge with its
    /// y already on screen.
    ///
    /// The dev, 2026-08-22: *fast-hop to the farthest node on the path that's still visible without
    /// panning.* That needs the whole list, furthest first — which is what the walker was computing
    /// all along and throwing away at the last line.
    #[test]
    fn the_fast_hop_chain_offers_the_shorter_hops_too() {
        let mut m = WorldMap::new();
        for (a, b) in [("l25sub2", "x1"), ("x1", "x2"), ("x2", "l25_path_to_l4")] {
            m.entry(a).neighbours.insert(b.into());
            m.entry(b).neighbours.insert(a.into());
        }
        m.entry("l25").heading = "Aike town".into();
        for k in ["l25sub2", "x1", "x2", "l25_path_to_l4"] {
            m.entry(k).parent = Some("l25".into());
            m.entry(k).heading = "Aike road".into();
        }
        m.here = Some("l25sub2".into());
        m.apply_save(
            &crate::game::save::parse(
                "return { overworld = { areaFlags = { hell = 0.1 }, completedAreas = {
                     l25sub2 = true, x1 = true, x2 = true, l25_path_to_l4 = true } } }",
            )
            .unwrap(),
        );

        let chain = m.far_hop_chain_inside("l25sub2", "l25_path_to_l4");
        assert_eq!(
            chain,
            vec!["l25_path_to_l4".to_string(), "x2".to_string()],
            "furthest first, and `x1` is left out because a one-hop hop is an ordinary step"
        );
        assert_eq!(
            m.far_hop_inside("l25sub2", "l25_path_to_l4").as_deref(),
            chain.first().map(String::as_str),
            "the single answer is still the head of the list"
        );
    }

    /// **#80.** A probe is a step along a route, and therefore has a chain the driver can hop.
    ///
    /// The fast hop hung off `Crossing::Step` alone, on the grounds that the other arms exist
    /// *because no route is known*. That is true of the **door** and false of the walk: a probe
    /// holds a frontier ([`WorldMap::frontier_target`]) and steps toward it by
    /// `first_step_toward`, which is a route by any definition. Over the run of 2026-08-22, 42
    /// probes asked for no hop at all; every move in Bainton Clump was one.
    ///
    /// The chain reaches the frontier itself, which surprised me and is right: `canTravelToDirect`
    /// takes **either** end being complete (`overworldview.lua:1316-1321`), so a cleared road one
    /// step short of an unwalked node still carries us onto it in one press. Four hops for one
    /// click, at a position the run of 2026-08-22 spent four.
    #[test]
    fn a_probe_holds_a_frontier_with_a_chain_to_hop_along() {
        let mut m = WorldMap::new();
        for (a, b) in [("l25sub2", "x1"), ("x1", "x2"), ("x2", "x3"), ("x3", "frontier")] {
            m.entry(a).neighbours.insert(b.into());
            m.entry(b).neighbours.insert(a.into());
        }
        m.entry("l25").heading = "Aike town".into();
        for k in ["l25sub2", "x1", "x2", "x3", "frontier"] {
            m.entry(k).parent = Some("l25".into());
            m.entry(k).heading = "Aike road".into();
        }
        // Everything walked but the frontier, which is what makes it one.
        for k in ["l25sub2", "x1", "x2", "x3"] {
            m.entry(k).visited = true;
        }
        m.entry("frontier").connections = 3;
        m.here = Some("l25sub2".into());
        m.apply_save(
            &crate::game::save::parse(
                "return { overworld = { areaFlags = { hell = 0.1 }, completedAreas = {
                     l25sub2 = true, x1 = true, x2 = true, x3 = true } } }",
            )
            .unwrap(),
        );

        let mv = m.cross_toward(&[exit("l4")]).expect("a move inside the town");
        let step = match &mv {
            Crossing::Probe { to, .. } | Crossing::Seek { to } => to.clone(),
            other => panic!("expected a probe toward the frontier, got {other:?}"),
        };
        assert_eq!(step, "x1", "the premise: one hop of a four-hop walk");
        let target = m.frontier_target().expect("a probe holds its frontier").to_string();
        assert_eq!(target, "frontier");

        let chain = m.far_hop_chain_inside("l25sub2", &target);
        assert_eq!(
            chain,
            vec!["frontier".to_string(), "x3".to_string(), "x2".to_string()],
            "all the way to the frontier, because the road up to it is cleared"
        );
    }

    /// **#84, the price on the rule below.** A woodland shrine is worth a step aside, not a search.
    ///
    /// The dev, 2026-08-22: *I only care to do a minor shrine errand if we are adjacent to it* —
    /// and, offered two hops as the condition, *two hops is still not low enough for a minor shrine.
    /// One hop or less should be the condition.* It does not retract the Beeford Hedge ruling that
    /// put minor shrines in `errand_inside` at all; it says what they are worth once they are there.
    /// A prayer is the whole payload, and it moves [`SHRINES_BEFORE_THE_ANOMALY`] not at all.
    ///
    /// The two fixtures differ in one edge. Note the shrine is **unvisited and incomplete** in both,
    /// which is why the measure has to be [`WorldMap::hops_from`]: `distances` would price the step
    /// to it at `CROSSING`, and the first cut of this rule — written against `distances` — refused
    /// every minor shrine in the game while every test still passed.
    #[test]
    fn a_woodland_shrine_is_an_errand_next_door_and_not_two_hops_out() {
        let woods = |shrine_at: &str| {
            let mut m = WorldMap::new();
            m.fold(&inside_dump(
                "l9",
                "l9sub1",
                "Saltagh Park road",
                vec![
                    node("l9sub4", "Saltagh Park road"),
                    node(shrine_at, "Saltagh Park woodland shrine"),
                ],
                vec![exit("l19")],
            ));
            m
        };

        // One hop: the shrine is a neighbour of where we stand.
        let near = woods("l9sub2");
        assert!(near.get("l9sub2").unwrap().is_shrine(), "the premise: the heading names it");
        assert_eq!(
            near.errand_inside("l9").map(|p| p.key.as_str()),
            Some("l9sub2"),
            "a woodland shrine next door is a step aside worth taking"
        );

        // Two hops: the same shrine, one edge further out, reached through `l9sub4`.
        let mut far = WorldMap::new();
        far.fold(&inside_dump(
            "l9",
            "l9sub1",
            "Saltagh Park road",
            vec![node("l9sub4", "Saltagh Park road")],
            vec![exit("l19")],
        ));
        far.fold(&inside_dump(
            "l9",
            "l9sub4",
            "Saltagh Park road",
            vec![
                node("l9sub1", "Saltagh Park road"),
                node("l9sub2", "Saltagh Park woodland shrine"),
            ],
            vec![],
        ));
        far.fold(&inside_dump(
            "l9",
            "l9sub1",
            "Saltagh Park road",
            vec![node("l9sub4", "Saltagh Park road")],
            vec![exit("l19")],
        ));
        assert!(far.get("l9sub2").unwrap().is_shrine(), "the control is the same shrine");
        assert_eq!(far.errand_inside("l9"), None, "two hops is not low enough");
    }

    /// **#84.** The shrine that can be consecrated outranks the one that happens to be nearer.
    ///
    /// `shrine_inside` ranked on `(distance, key)` alone, so a woodland shrine one hop nearer took
    /// the errand from the plaza — and only the key tiebreak, which favours `_plaza` at *equal*
    /// distance, made that look right in the fixtures. A minor shrine pays a prayer; the plaza is
    /// the only thing in a shrine forest that can move [`SHRINES_BEFORE_THE_ANOMALY`].
    #[test]
    fn the_consecratable_shrine_outranks_the_nearer_one() {
        let mut m = WorldMap::new();
        m.fold(&inside_dump(
            "shrine7",
            "shrine7sub1",
            "Cottam Boscage road",
            vec![
                node("shrine7sub2", "Cottam Boscage woodland shrine"),
                node("shrine7sub3", "Cottam Boscage road"),
            ],
            vec![exit("l55")],
        ));
        m.fold(&inside_dump(
            "shrine7",
            "shrine7sub3",
            "Cottam Boscage road",
            vec![
                node("shrine7sub1", "Cottam Boscage road"),
                node("shrine7_plaza", "Cottam Boscage shrine"),
            ],
            vec![],
        ));
        m.fold(&inside_dump(
            "shrine7",
            "shrine7sub1",
            "Cottam Boscage road",
            vec![
                node("shrine7sub2", "Cottam Boscage woodland shrine"),
                node("shrine7sub3", "Cottam Boscage road"),
            ],
            vec![exit("l55")],
        ));

        let hops = m.hops_from("shrine7sub1");
        assert_eq!(
            hops.get("shrine7sub2").copied(),
            Some(1),
            "the premise: the minor one is nearer"
        );
        assert_eq!(hops.get("shrine7_plaza").copied(), Some(2), "and the plaza is further out");
        assert!(
            m.get("shrine7_plaza").unwrap().can_be_consecrated(),
            "and it is the one that counts"
        );

        assert_eq!(
            m.shrine_inside("shrine7").map(|p| p.key.as_str()),
            Some("shrine7_plaza"),
            "the consecratable shrine is the errand, near or not"
        );
    }

    /// **#84.** A shrine forest holds a run inside until it has found the shrine it came for.
    ///
    /// Live 2026-08-22: five hops to `shrine7`, *Cottam Boscage — level 9 forest*, in and straight
    /// out to `l55` on the next step, with the plan already moved on to `shrine3`. `next_target`
    /// excludes the node we are standing on, so arriving at the container satisfied the goal and
    /// the errand evaporated — and `shrine_inside` could not hold us, because on the first step
    /// inside there is nothing yet to find: `isRevealed` is
    /// `corrupt or areaIsComplete or areaFlag(key..'_plaza_explored')`
    /// (`overworld/locations/shrine_forest_raw.lua:5`).
    ///
    /// The premise of the whole guard is that the key is enough to know the shrine is in there —
    /// see [`WorldMap::seeking_a_shrine`] for the two lines of Lua that make that true.
    #[test]
    fn a_shrine_forest_is_not_left_before_its_shrine_has_been_found() {
        let mut m = WorldMap::new();
        m.fold(&inside_dump(
            "shrine7",
            "shrine7sub1",
            "Cottam Boscage road",
            vec![
                node("shrine7sub2", "Cottam Boscage road"),
                node("shrine7_path_to_l55", "Road to Wetwang"),
            ],
            vec![exit("l55")],
        ));

        assert!(m.seeking_a_shrine("shrine7"), "the plaza is fogged, so we are still looking");
        assert_eq!(m.shrine_inside("shrine7"), None, "the premise: there is nothing found yet");
        match m.cross_toward(&[exit("l55")]) {
            Some(Crossing::Step { to, .. })
            | Some(Crossing::Probe { to, .. })
            | Some(Crossing::Seek { to }) => assert_ne!(
                to, "shrine7_path_to_l55",
                "we came for the shrine and have not seen it; the road out is not the answer"
            ),
            other => panic!("expected a step inside the forest, got {other:?}"),
        }

        // The moment the plaza is on the map at all, this has nothing left to add: `shrine_inside`
        // answers for it, and `errand_inside` is what holds us.
        m.fold(&inside_dump(
            "shrine7",
            "shrine7sub2",
            "Cottam Boscage road",
            vec![node("shrine7_plaza", "Cottam Boscage shrine")],
            vec![],
        ));
        assert!(!m.seeking_a_shrine("shrine7"), "found; the guard steps out of the way");
        assert_eq!(
            m.errand_inside("shrine7").map(|p| p.key.as_str()),
            Some("shrine7_plaza"),
            "and the errand is the shrine itself"
        );

        // A forest that is not a shrine forest is never searched for one, and a shrine the driver
        // has already had its go at ends the search — the clause every one of these guards carries.
        assert!(!m.seeking_a_shrine("l9"), "an ordinary forest promises no shrine");
        let mut given_up = WorldMap::new();
        given_up.abandon("shrine7_plaza");
        assert!(
            !given_up.seeking_a_shrine("shrine7"),
            "we tried; the fog is not what is hiding it"
        );
    }

    /// A shrine inside a forest is an errand, not something to walk past. — the dev, 2026-08-17
    ///
    /// Live 2026-08-16 2103Z, steps 11 to 13: the run travelled to `shrine1` under `Goal::Shrine`,
    /// entered it, and crossed straight out the far side. The shrine is a **subnode**, the goal was
    /// satisfied by standing on the container, and `next_target` excludes `here` — so the errand
    /// evaporated on arrival rather than failing.
    #[test]
    fn a_shrine_inside_a_forest_is_stopped_at_before_the_crossing_leaves() {
        let forest = |flags: &str| {
            let mut m = WorldMap::new();
            m.fold(&inside_dump(
                "shrine1",
                "shrine1sub1",
                "Gripthorpe Brush road",
                vec![
                    node("shrine1sub2", "Gripthorpe Brush woodland shrine"),
                    node("shrine1_path_to_l5", "Road to Dalton Copse"),
                ],
                vec![exit("l5")],
            ));
            m.entry("shrine1").heading = "Gripthorpe Brush — level 1 forest".into();
            m.apply_save(
                &crate::game::save::parse(&format!(
                    "return {{ overworld = {{ areaFlags = {{ {flags} }} }} }}"
                ))
                .unwrap(),
            );
            m
        };

        // The heading is what makes it a shrine — `woodland shrine` ends in the word, so the same
        // `is_shrine` that names a surface `shrineN` names this.
        let m = forest("hell = 0");
        assert!(m.get("shrine1sub2").unwrap().is_shrine(), "the premise");
        assert_eq!(
            m.errand_inside("shrine1").map(|p| p.key.as_str()),
            Some("shrine1sub2"),
            "we came here for the shrine; standing on the forest is not having been"
        );

        // Prayed at already — `<key>_used` is the game's own record — so there is nothing to stop
        // for and the crossing is a crossing again.
        assert_eq!(forest("hell = 0, shrine1sub2_used = 1").errand_inside("shrine1"), None);

        // **`used`, not `completed`.** A shrine with no fight is complete on arrival (`e799771`), so
        // asking that would skip every peaceful shrine there is.
        let mut done = forest("hell = 0");
        done.entry("shrine1sub2").completed = true;
        assert!(done.errand_inside("shrine1").is_some(), "completed says nothing about praying");

        // A shrine the driver gave up on is still `!used`, because nothing was ever prayed. Without
        // the abandoned clause this errand would send the run back to it for ever — the thirty-step
        // `l10 -> shrine2 -> l10` bounce, in a subworld.
        let mut given_up = forest("hell = 0");
        given_up.abandon("shrine1sub2");
        assert_eq!(given_up.errand_inside("shrine1"), None);

        // **With the portal open it is still an errand.** Task #71, the dev 2026-08-21: *at Beeford
        // Hedge, the minor shrine should've been an errand.*
        //
        // This asserted `None` until then, on the reading that *consecrating is the only thing on
        // offer once `hell ~= 0`, and that is `worth_consecrating_here` on arrival*. The first half
        // is true of a major shrine and false of this one. `showPrayButton` leads with
        // `not shrineLocation.majorShrine` (`shrine.lua:97-102`), which short-circuits the whole
        // `hell` clause — so a minor shrine's prayer is available on identical terms before and
        // after the portal opens. Nothing about it changes, and the errand was switching off anyway.
        assert_eq!(
            forest("hell = 0.1").errand_inside("shrine1").map(|p| p.key.as_str()),
            Some("shrine1sub2"),
            "a minor shrine owes a prayer whether the portal is open or shut"
        );

        // And the prayer is what it is for, so a used one is still nothing to stop at — the same
        // clause as above, now carrying the open-portal case too.
        assert_eq!(forest("hell = 0.1, shrine1sub2_used = 1").errand_inside("shrine1"), None);
    }

    /// **The whole frontier under level 4 first, and only then the trigger.**
    ///
    /// The dev, 2026-08-16: *we don't have the warrior's gear to carry us through difficult crypts,
    /// so instead of beelining for the nearest level 4 node, I now want the pre-anomaly navigator to
    /// visit every node that is lower than level 4, so that we only visit a level 4 node when the
    /// entire frontier is at least level 4.*
    ///
    /// **Half of this rule was retired on 2026-08-22 and this test still passes** — which is worth
    /// saying out loud, because it now passes for a different reason than it used to. `Explore` no
    /// longer *refuses* a trigger node; it ranks against one below distance. Every candidate here is
    /// one hop from `start`, so the tiebreak decides and the answers are unchanged. What the dev
    /// retired is the sweep, and the sweep only shows itself when a gentle frontier is **further
    /// away** than a trigger node — which is
    /// [`tests::exploring_takes_the_nearer_trigger_node_and_the_gentler_of_two_equals`].
    ///
    /// The [`Goal::OpenAnomaly`] gate below is untouched, and that is what the last two assertions
    /// pin: the planner still never goes and opens the portal on purpose while gentler ground is
    /// left to learn from.
    #[test]
    fn the_trigger_waits_until_nothing_gentler_is_left() {
        let mut m = WorldMap::new();
        m.fold(&dump(
            "start",
            "camp",
            vec![
                node("l4", "Grim Barrow — level 4 crypt"),
                node("l2", "Quiet Glade meadow"),
                node("l3", "Fangfoss — level 2 crypt"),
            ],
        ));

        // Two nodes under the bar. Whichever is chosen, it is not the level 4 one, and the errand
        // is not the trigger.
        let plan = m.next_target().expect("a plan");
        assert_ne!(plan.target, "l4", "the level 4 crypt is not the first stop");
        assert_ne!(plan.reason, Goal::OpenAnomaly);

        // Walk the meadow. The level 2 crypt is still under the bar, so still first.
        walked_out(&mut m, "l2");
        let plan = m.next_target().expect("a plan");
        assert_eq!(plan.target, "l3", "a level 2 crypt is gentler ground too");

        // Walk that as well and the frontier is level 4 and above, which is when the rule lets go.
        walked_out(&mut m, "l3");
        let plan = m.next_target().expect("a plan");
        assert_eq!(plan.reason, Goal::OpenAnomaly);
        assert_eq!(plan.target, "l4");

        // **And the bar is lifted the moment the portal opens**, or it would keep a run wandering
        // instead of finishing. `l4` stops being a trigger then, so the check is on the predicate.
        let mut open = WorldMap::new();
        open.fold(&dump("start", "camp", vec![node("l2", "Quiet Glade meadow")]));
        open.hell = Some(0.1);
        assert!(!open.gentler_ground_remains("start", &|_: &Place| true));
    }

    /// **A refused door drops out of the ranking, and the next-best is taken instead.**
    ///
    /// The l31/l47 ping-pong of 2026-08-24, in the smallest shape that produces it. Standing in
    /// `Langtoft Forest` the run logged, four laps running:
    ///
    /// ```text
    /// door: Rest -> l9; doors l26=12 l44=6 l47=5 | reason nearest to the target
    /// ```
    ///
    /// `l47` is genuinely the nearest door — and from `l47` the surface router's first hop back to
    /// `l9` is `l31`. The ranking is not wrong, so the fix is not in the ranking.
    ///
    /// The second half is the control that matters: refusing the *only* door must not leave the run
    /// with nowhere to go, for the same reason the level-4 gentle pass is dropped when it empties.
    #[test]
    fn a_door_that_hands_us_back_is_dropped_and_the_next_best_taken() {
        let mut m = WorldMap::new();
        let door = |to: &str| Exit { x: 0.0, y: 0.0, to_key: to.into(), to_heading: "".into() };
        let exits = [door("l47"), door("l44")];
        m.fold(&inside_dump("l31", "l31sub1", "Langtoft Forest road", vec![], exits.to_vec()));

        // Whatever the ranking picks first, refusing it must move the choice to the other door.
        let first = m.choose_exit(&exits).expect("a door").0;
        m.refuse_door("l31", &first);
        let second = m.choose_exit(&exits).expect("still a door").0;
        assert_ne!(second, first, "a refused door was chosen again");

        // And refusing the last one leaves the run a door rather than none: being inside with
        // nowhere named is worse than any one door.
        m.refuse_door("l31", &second);
        assert!(
            m.choose_exit(&exits).is_some(),
            "refusing every door must fall back rather than strand the crossing"
        );
    }

    /// **The sweep, retired.** The dev, 2026-08-22: *retire the pre-anomaly phase of avoiding level
    /// 4+ nodes*, and asked whether the portal should then be opened deliberately, *only stop the
    /// exhaustive sweep*.
    ///
    /// So exploring expands outward from where we stand, and the old rule survives as a tiebreak
    /// rather than a veto. Both halves are pinned here, and they need different fixtures: the sweep
    /// only shows itself when the gentle frontier is **further** than the trigger node, and the
    /// tiebreak only when they are the same distance.
    #[test]
    fn exploring_takes_the_nearer_trigger_node_and_the_gentler_of_two_equals() {
        // Nearer trigger against a further meadow. `mid` is walked and fully named, so it is not
        // itself a frontier and cannot win.
        let mut m = WorldMap::new();
        m.fold(&dump(
            "start",
            "camp",
            vec![node("l4", "Grim Barrow — level 4 crypt"), node("mid", "Quiet Glade meadow")],
        ));
        m.fold(&dump(
            "mid",
            "Quiet Glade meadow",
            vec![node("start", "camp"), node("far", "Dane meadow")],
        ));
        m.here = Some("start".into());
        m.entry("mid").visited = true;
        m.entry("mid").hidden = Some(0);

        assert!(m.get("l4").unwrap().triggers_anomaly(), "the premise: level 4 opens the portal");
        assert!(!m.get("far").unwrap().triggers_anomaly(), "and the far frontier is gentle");
        let plan = m.next_target().expect("a plan");
        assert_eq!(
            plan.target, "l4",
            "one hop against two: exploring expands outward, it does not sweep the gentle ground first"
        );

        // Equal distance, and the tiebreak has its say. Same map with the meadow moved next door.
        let mut level = WorldMap::new();
        level.fold(&dump(
            "start",
            "camp",
            vec![node("l4", "Grim Barrow — level 4 crypt"), node("beside", "Quiet Glade meadow")],
        ));
        level.here = Some("start".into());
        let plan = level.next_target().expect("a plan");
        assert_eq!(
            plan.target, "beside",
            "at equal distance the one that does not open the portal wins"
        );

        // **And it is the level, not a boolean.** The dev, 2026-08-22: *use "node level" as the
        // second-highest (prefer to choose the lower level).* Both of these open the portal, so the
        // `triggers_anomaly` tiebreak this replaced would have tied them and let the key decide —
        // and the key says `deep`.
        let mut two_fights = WorldMap::new();
        two_fights.fold(&dump(
            "start",
            "camp",
            vec![
                node("deep", "Yokefleet — level 5 crypt"),
                node("deeper", "Cowlam — level 9 crypt"),
            ],
        ));
        two_fights.here = Some("start".into());
        assert!(
            two_fights.get("deep").unwrap().triggers_anomaly()
                && two_fights.get("deeper").unwrap().triggers_anomaly(),
            "the premise: a boolean cannot separate these"
        );
        assert_eq!(
            two_fights.next_target().expect("a plan").target,
            "deep",
            "level 5 before level 9, at the same distance"
        );
    }

    /// **The decision that killed the run of 2026-08-16**, and the exploring case it must not break.
    ///
    /// Standing inside Stillingfleet with the portal open. The anomaly is `start`, the adventure
    /// began there, and the road back — `l39`, the door we came in by — is four hops from it while
    /// `l60` is six. The measured answer is to turn round. The run took `l60`, a level 7 crypt, and
    /// died in it, because the anti-backtracking rule overrode the ranking on the grounds that `l60`
    /// was unvisited.
    ///
    /// That argument belongs to exploring, and the second half of this test is the control: with no
    /// errand naming a destination, an unvisited door is still preferred over the room we just left.
    #[test]
    fn the_door_we_came_in_by_loses_a_tie_and_wins_a_measurement() {
        // Two doors out of `l52`, both one hop from the anomaly at `start`, and we came in by
        // `l39`. This is the 2026-08-22 2002Z shape: `doors l10=16 l1=16`, tied, and every
        // remaining term preferring the way back.
        let build = || {
            let mut m = WorldMap::new();
            ready_for_the_anomaly(&mut m);
            m.hell = Some(0.1);
            m.fold(&dump(
                "start",
                "Cottam campfire",
                vec![
                    node("l39", "Eight Timberland — level 4 forest"),
                    node("l60", "Asselby — level 7 crypt"),
                ],
            ));
            m.fold(&dump(
                "l39",
                "Eight Timberland — level 4 forest",
                vec![node("l52", "Stillingfleet village")],
            ));
            m.fold(&dump(
                "l60",
                "Asselby — level 7 crypt",
                vec![node("l52", "Stillingfleet village")],
            ));
            m.fold(&dump(
                "l52",
                "Stillingfleet village",
                vec![
                    node("l39", "Eight Timberland — level 4 forest"),
                    node("l60", "Asselby — level 7 crypt"),
                ],
            ));
            m.fold(&inside_dump(
                "l52",
                "l52sub1",
                "Stillingfleet general store",
                vec![],
                vec![exit("l39"), exit("l60")],
            ));
            m.entered_from = Some("l39".into());
            // **The entrance has to be the door that would otherwise win**, or this proves nothing.
            // That is the live shape exactly: `l1` was the door we came in by *and* the one we had
            // just cleared, so `risk` ranked it `Free` against an uncleared alternative, and the key
            // order agreed. Both terms below distance pointed at the way back.
            // Both cleared, so `distances` prices the two routes alike. An unfought crypt costs
            // CROSSING, which separates them on the *first* term and the tie-break is never reached
            // — the trap this fixture fell into on its first draft.
            m.entry("l39").completed = true;
            m.entry("l60").completed = true;
            m
        };

        let m = build();
        assert_eq!(
            m.next_target().map(|p| p.reason),
            Some(Goal::CloseAnomaly),
            "the control: a measured errand, or the ranked branch never runs"
        );
        assert_eq!(
            m.get("l39").unwrap().risk(),
            m.get("l60").unwrap().risk(),
            "the premise: risk cannot separate them, so the key would decide"
        );
        assert!("l39" < "l60", "and the key alone picks the door we came in by");
        let door = |m: &WorldMap| m.choose_exit(&[exit("l39"), exit("l60")]).map(|(d, _, _)| d);
        assert_eq!(
            door(&m).as_deref(),
            Some("l60"),
            "tied on distance, so the door we did not come in by wins"
        );

        // **And it is only a tie-break.** Put `start` one hop closer through the entrance and the
        // measurement takes it back — which is what keeps this from becoming the override that
        // walked a run away from the anomaly on 2026-08-16.
        let mut nearer = build();
        nearer.fold(&dump(
            "l39",
            "Eight Timberland — level 4 forest",
            vec![node("start", "Cottam campfire")],
        ));
        nearer.entry("l60").neighbours.remove("start");
        nearer.entry("start").neighbours.remove("l60");
        assert_eq!(
            door(&nearer).as_deref(),
            Some("l39"),
            "a measured shorter route through the entrance is still taken"
        );
    }

    #[test]
    fn a_named_destination_is_worth_walking_back_towards() {
        let build = || {
            let mut m = WorldMap::new();
            ready_for_the_anomaly(&mut m);
            // The road walked in: start -> l19 -> l39 -> l52, and l60 hanging off l52.
            m.fold(&dump(
                "start",
                "Cottam campfire",
                vec![node("l19", "Gipsyville — level 2 crypt")],
            ));
            m.fold(&dump(
                "l19",
                "Gipsyville — level 2 crypt",
                vec![node("l39", "Eight Timberland — level 4 forest")],
            ));
            m.fold(&dump(
                "l39",
                "Eight Timberland — level 4 forest",
                vec![node("l52", "Stillingfleet village")],
            ));
            m.fold(&dump(
                "l52",
                "Stillingfleet village",
                vec![
                    node("l39", "Eight Timberland — level 4 forest"),
                    node("l60", "Asselby — level 7 crypt"),
                ],
            ));
            m.fold(&inside_dump(
                "l52",
                "l52sub1",
                "Stillingfleet general store",
                vec![],
                vec![exit("l39"), exit("l60")],
            ));
            m.entered_from = Some("l39".into());
            m
        };

        // The portal open, so the anomaly is the errand and `start` is where it is.
        let mut m = build();
        m.hell = Some(0.1);
        assert_eq!(
            m.next_target().map(|p| p.reason),
            Some(Goal::CloseAnomaly),
            "the control: the errand really is the anomaly, or this proves nothing about it"
        );
        let (door, why, _) = m.choose_exit(&[exit("l39"), exit("l60")]).expect("two doors");
        assert_eq!(door, "l39", "four hops from the portal, against six the other way");
        assert_eq!(why.why(), "nearest to the target", "and ranked, not fallen back on");

        // **The control: exploring, where the old rule's reasoning holds.** An unvisited door is
        // unmeasured rather than worse, and the room we just left has nothing more to teach us.
        //
        // A separate map, because the one above cannot produce this errand: with the portal shut,
        // `l39` is a level 4 forest and therefore an `OpenAnomaly` candidate, so the plan still
        // names a destination. Every level here is under the trigger's bar, the purse is empty and
        // the bar is full, which leaves exploring as the only thing left to want.
        // The container is `l9` because the fixture helper re-heads every other one as
        // "Ulrome — level 6 village" (`container_heading`), and a level 6 node is an `OpenAnomaly`
        // candidate — which would name a destination and defeat the point of this half.
        let mut m = WorldMap::new();
        m.fold(&dump("start", "Cottam campfire", vec![node("l19", "Gipsyville — level 2 crypt")]));
        m.fold(&dump(
            "l19",
            "Gipsyville — level 2 crypt",
            vec![
                node("f1", "Frontier — level 1 forest"),
                node("l39", "Eight Timberland — level 2 forest"),
            ],
        ));
        m.fold(&dump(
            "l39",
            "Eight Timberland — level 2 forest",
            vec![node("l9", "Saltagh Park — level 1 forest")],
        ));
        m.fold(&dump(
            "l9",
            "Saltagh Park — level 1 forest",
            vec![
                node("l39", "Eight Timberland — level 2 forest"),
                node("l60", "Asselby — level 2 crypt"),
            ],
        ));
        m.fold(&inside_dump(
            "l9",
            "l9sub1",
            "Saltagh clearing",
            vec![],
            vec![exit("l39"), exit("l60")],
        ));
        m.entered_from = Some("l39".into());
        m.hell = Some(0.0);
        m.gold = 0;
        m.note_health_level(crate::rest::Health { current: 20, max: 20 });
        assert_eq!(
            m.next_target().map(|p| p.reason),
            Some(Goal::Explore),
            "the control only controls if the errand really is exploring"
        );
        // The door, not the reason. On this map the two agree — exploring *targets* the unwalked
        // node, so ranking reaches `l60` on its own and the override never has to fire. That is
        // worth knowing rather than working around: the override only matters where the two
        // disagree, and under `Explore` they rarely can.
        assert_eq!(
            m.choose_exit(&[exit("l39"), exit("l60")]).map(|(d, _, _)| d),
            Some("l60".to_string()),
            "with nothing named to walk to, the unwalked door is still where exploring goes"
        );
    }

    /// **The shelf comes before the bed, and only when the shelf has been found.**
    ///
    /// The dev, 2026-08-16: *once we are in a settlement with that goal, buy as many healthBuffs as
    /// possible while still topping up your health at the inn before we leave the settlement.*
    ///
    /// The reason is arithmetic rather than taste: a heart raises maximum health and gives none, so
    /// resting first fills a bar the purchase then lengthens. `hearts_affordable` keeps the bed
    /// affordable across the whole visit, which is what makes "last" safe.
    ///
    /// The second half is the case the **previous** rule was written for and must still hold: a run
    /// at 2/20 with the inn found and no store on the map yet goes to bed, exactly as before.
    #[test]
    fn a_found_shelf_is_visited_before_the_bed_and_an_unfound_one_is_not() {
        let both = |gold: i64, health: crate::rest::Health| {
            let mut m = WorldMap::new();
            m.fold(&dump("here", "camp", vec![node("l11", "Rowlston Covert village")]));
            m.fold(&inside_dump(
                "l11",
                "l11sub1",
                "Rowlston Covert road",
                vec![
                    node("l11sub2", "Rowlston Covert general store"),
                    node("l11sub3", "The Wobbly Cat inn"),
                ],
                vec![],
            ));
            m.gold = gold;
            m.note_health_level(health);
            m
        };

        // Hurt, with both in the same village and the price in hand: the shelf first.
        let mut m = both(HEART_FLOOR, crate::rest::Health { current: 2, max: 20 });
        assert!(m.wants_rest(), "the bed is genuinely wanted, or this proves nothing");
        assert!(m.wants_a_heart());
        match m.cross_toward(&[]) {
            Some(Crossing::Step { toward, .. }) | Some(Crossing::Probe { toward, .. }) => {
                assert_eq!(toward, "l11sub2", "the store, with the bed still paid for after it")
            }
            other => panic!("expected a step toward the store, got {other:?}"),
        }
        assert_eq!(m.hearts_affordable(), 1, "and exactly one night is held back");

        // **The case the old rule existed for.** Same village, same wound, no store on the map:
        // straight to bed, unchanged.
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("l11", "Rowlston Covert village")]));
        m.fold(&inside_dump(
            "l11",
            "l11sub1",
            "Rowlston Covert road",
            vec![node("l11sub3", "The Wobbly Cat inn")],
            vec![],
        ));
        m.gold = HEART_FLOOR;
        m.note_health_level(crate::rest::Health { current: 2, max: 20 });
        match m.cross_toward(&[]) {
            Some(Crossing::Step { toward, .. }) | Some(Crossing::Probe { toward, .. }) => {
                assert_eq!(toward, "l11sub3", "a shelf we cannot see does not outrank a bed we can")
            }
            other => panic!("expected a step toward the inn, got {other:?}"),
        }

        // And below the floor there is no heart errand at all, so the bed wins on its own terms.
        let mut m = both(HEART_FLOOR - 1, crate::rest::Health { current: 2, max: 20 });
        assert!(!m.wants_a_heart());
        match m.cross_toward(&[]) {
            Some(Crossing::Step { toward, .. }) | Some(Crossing::Probe { toward, .. }) => {
                assert_eq!(toward, "l11sub3", "a pound short of the goal is not the goal")
            }
            other => panic!("expected a step toward the inn, got {other:?}"),
        }
    }

    /// Inside the village, the errand is the general store.
    ///
    /// The rest errand walks to the inn; the heart errand walks to the store. They share every step
    /// of getting there, which is why #18's generalisation arrives one errand at a time rather than
    /// as an abstraction invented around a single case.
    #[test]
    fn the_heart_errand_walks_to_the_general_store() {
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("l11", "Rowlston Covert village")]));
        m.fold(&inside_dump(
            "l11",
            "l11sub1",
            "Rowlston Covert road",
            vec![
                node("l11sub2", "Rowlston Covert general store"),
                node("l11sub3", "Rowlston Covert house"),
            ],
            vec![],
        ));
        m.hell = Some(0.1);
        m.gold = HEART_FLOOR;

        match m.cross_toward(&[]) {
            Some(Crossing::Step { to, toward }) | Some(Crossing::Probe { to, toward }) => {
                assert_eq!(toward, "l11sub2", "the store is what we came in for");
                assert_eq!(to, "l11sub2", "and it is one hop away");
            }
            other => panic!("expected a step toward the store, got {other:?}"),
        }

        // Standing on it, the crossing is over and the errand begins.
        m.fold(&inside_dump(
            "l11",
            "l11sub2",
            "Rowlston Covert general store",
            vec![node("l11sub1", "Rowlston Covert road")],
            vec![],
        ));
        assert!(
            matches!(m.cross_toward(&[]), Some(Crossing::Arrive { .. })),
            "arrived at the counter"
        );

        // And once bought, the village stops being an errand at all.
        m.bought_the_heart("l11");
        assert!(
            !matches!(m.cross_toward(&[]), Some(Crossing::Arrive { .. })),
            "an emptied store is not somewhere to stand about in"
        );
    }

    /// Walking away from the door does not re-open the ground in front of it. **The `l40` cycle,
    /// now prevented structurally rather than by a measure.**
    ///
    /// This asserted a high-water mark until #57: the steer's ceiling was the nearest we had *ever*
    /// been on a crossing, not wherever we last stood, because the frontier walk moves us away
    /// routinely and a ceiling that followed us out re-licensed the same forward steer every time.
    ///
    /// The measure is gone with the arm that read it, and the property it protected now falls out of
    /// the candidate filter instead. A node we have stood on and fully named satisfies
    /// `nothing_left_to_reveal` — `neighbours.len() >= connections` — and the frontier walk excludes
    /// it. There is nothing to re-open, because it stopped being a destination the moment it ran out
    /// of things to teach. That is the same fact that made the steer keep choosing `l63_plaza` in the
    /// 1519Z run: the steer had no such filter and the walk always did.
    #[test]
    fn walking_away_from_the_door_does_not_reopen_the_ground_in_front_of_it() {
        let mut m = WorldMap::new();
        let door = crate::observe::adjacency::Exit {
            x: 0.0,
            y: 0.0,
            to_key: "l36".into(),
            to_heading: "Wawne crypt".into(),
        };
        let near = |k: &str, x: f64, conn: u32| Node {
            key: k.into(),
            heading: "Fosholme Growth road".into(),
            x,
            y: 0.0,
            connections: conn,
        };

        // In along the road. `l40sub24` is two-way and both its roads get named, so once we have
        // stood on it there is nothing left there.
        m.fold(&inside_dump(
            "l40",
            "l40sub17",
            "Fosholme Growth road",
            vec![near("l40sub24", 100.0, 2)],
            vec![door.clone()],
        ));
        m.crossing_to = Some(("l36".into(), Goal::Explore));
        m.fold(&inside_dump(
            "l40",
            "l40sub24",
            "Fosholme Growth road",
            vec![near("l40sub17", 500.0, 3), near("l40sub25", 300.0, 3)],
            vec![door.clone()],
        ));
        assert!(
            m.get("l40sub24").unwrap().nothing_left_to_reveal(),
            "the premise: two declared roads and both named"
        );

        // Out to the far node, which has a road onward we have never walked. `l40sub24` sits between
        // us and the door and is much the nearest thing to it — exactly the shape that used to
        // re-open — while `l40sub30` is further from the door and still has something to give.
        m.fold(&inside_dump(
            "l40",
            "l40sub25",
            "Fosholme Growth road",
            vec![near("l40sub24", 100.0, 2), near("l40sub30", 900.0, 3)],
            vec![door.clone()],
        ));
        let step = m.cross_toward(&[door]).and_then(|c| match c {
            Crossing::Step { to, .. } | Crossing::Probe { to, .. } | Crossing::Seek { to } => {
                Some(to)
            }
            _ => None,
        });
        assert_eq!(
            step.as_deref(),
            Some("l40sub30"),
            "the retired node is nearer the door and is not a destination; the live one is"
        );

        // **And the distinction the first draft of this test missed.** The walk returns a *step*
        // along a route to a target, so a retired node can still be crossed as a waypoint — that is
        // the arm being allowed a real route, and it is why the memory that #57 deleted belonged on
        // the steer rather than here. What must not happen is `l40sub24` being the *target*.
        assert_ne!(
            m.probing_toward.as_ref().map(|(_, k)| k.as_str()),
            Some("l40sub24"),
            "nothing left to teach means it is not somewhere we set off for"
        );
    }

    /// Colden Brake, 2026-08-16, and it is the decision that opened the anomaly.
    ///
    /// ```text
    /// 78. crossing `e2` toward `e2_path_to_l41` (nearest to the target) via `e2_path_to_l41`
    ///   door choice: Heart -> l25; doors l20=1 l38=2 l41=1
    /// ```
    ///
    /// `l20` is a level 2 crypt already cleared. `l41` is Crockey, a level 5 crypt. Both one hop
    /// from the target. `min_by_key` keeps the first minimum and the game printed `l41` first, so a
    /// coin toss with no coin sent a run into a level 5 crypt on the way to a shop — and arriving
    /// there fired `You feel the ground rumble` (steps 79-81), which is the whole adventure's
    /// difficulty settled by the order of a print statement.
    ///
    /// Two rules now stand between that and a repeat, and the test asserts the outcome rather than
    /// which of them did it, because either alone is enough and both are wanted.
    #[test]
    fn a_door_onto_an_unfought_level_5_crypt_loses_to_a_cleared_one() {
        use crate::observe::adjacency::Exit;
        let door = |to: &str| Exit { x: 0.0, y: 0.0, to_key: to.into(), to_heading: "".into() };

        let mut m = WorldMap::default();
        for (a, b) in [("e2", "l20"), ("e2", "l38"), ("e2", "l41"), ("l20", "l25"), ("l41", "l25")]
        {
            m.entry(a).neighbours.insert(b.into());
            m.entry(b).neighbours.insert(a.into());
        }
        m.entry("l20").heading = "Keyingham — level 2 crypt".into();
        m.entry("l38").heading = "Bellasize Regrowth — level 4 forest".into();
        m.entry("l41").heading = "Crockey — level 5 crypt".into();
        m.entry("l25").heading = "Aike shrine".into();
        m.entry("e2sub6").parent = Some("e2".into());
        m.here = Some("e2sub6".into());
        m.apply_save(
            &crate::game::save::parse(
                "return { overworld = { areaFlags = { hell = 0 },
                     completedAreas = { l20 = true, e2 = true } } }",
            )
            .unwrap(),
        );
        assert_eq!(m.inside(), Some("e2"), "the fixture must be standing inside the village");

        // The exits in the order the game printed them, which is the order that decided it.
        let (chosen, why, note) = m
            .choose_exit(&[door("l41"), door("l38"), door("l20")])
            .expect("three doors is not no doors");
        assert_eq!(chosen, "l20", "the cleared level 2 crypt, not the unfought level 5 one");
        assert_eq!(why.why(), "nearest to the target", "and by ranking, not by falling back");
        assert!(
            note.contains("gentle doors only"),
            "the log must say the rule was in force: {note}"
        );
    }

    /// The level 4 rule at a door, where it costs something.
    ///
    /// The tie above is the easy case — a cleared node is both gentler *and* safer, so two
    /// independent rules agree. Here the trigger door is genuinely nearer, and obeying the dev's
    /// rule means walking further. That is the trade they asked for: *only visit a level 4 node when
    /// the entire frontier is at least level 4.*
    ///
    /// The control is the **errand**: when the target is the trigger node itself, the rule has to
    /// let us through the door to it or it deadlocks the very sequencing it exists for. That is
    /// [`Goal::OpenAnomaly`], and it is a state a run reaches every time — once nothing gentler is
    /// left, opening the portal is the plan.
    ///
    /// Two other controls were tried here and both were wrong, which is worth leaving written down:
    ///
    /// - **portal open.** Proved nothing, because opening it changed the errand too: with
    ///   `hell ~= 0` a shrine then needed `reachable_without_a_fight`, `l41` was the fight in the
    ///   way, and the target moved out from under the comparison. **That filter was removed on
    ///   2026-08-21 (#70)**, so the mechanism named here no longer exists — the control is still
    ///   the wrong one, because `worth_a_trip` and the corruption filter also turn on `hell`, but
    ///   do not go looking for the route test to explain it.
    /// - **`l41` cleared.** Green, and impossible: clearing a level 5 crypt means arriving at it,
    ///   and arriving is what opens the portal, so `hell == 0` and a completed level 5 node cannot
    ///   both hold. The dev caught it. See the note where `Place::opens_the_anomaly` used to be.
    #[test]
    fn a_nearer_door_loses_to_a_gentler_one_unless_the_trigger_is_the_errand() {
        use crate::observe::adjacency::Exit;
        let door = |to: &str| Exit { x: 0.0, y: 0.0, to_key: to.into(), to_heading: "".into() };

        let world = |cleared: &str| {
            let mut m = WorldMap::default();
            // `l41` reaches the shrine in one hop; `l20` takes three. Distance alone says `l41`.
            for (a, b) in [
                ("e2", "l20"),
                ("e2", "l41"),
                ("l41", "shrine3"),
                ("l20", "l9"),
                ("l9", "l8"),
                ("l8", "shrine3"),
            ] {
                m.entry(a).neighbours.insert(b.into());
                m.entry(b).neighbours.insert(a.into());
            }
            m.entry("l20").heading = "Keyingham — level 2 crypt".into();
            m.entry("l41").heading = "Crockey — level 5 crypt".into();
            m.entry("shrine3").heading = "Gembling shrine".into();
            m.entry("e2sub6").parent = Some("e2".into());
            m.here = Some("e2sub6".into());
            m.apply_save(
                &crate::game::save::parse(&format!(
                    "return {{ overworld = {{ areaFlags = {{ hell = 0 }},
                         completedAreas = {{ e2 = true, {cleared} }} }} }}"
                ))
                .unwrap(),
            );
            m
        };

        let doors = [door("l41"), door("l20")];
        assert_eq!(
            world("").choose_exit(&doors).map(|(k, _, _)| k).as_deref(),
            Some("l20"),
            "three hops through gentle ground beats one hop through a level 5 crypt"
        );

        // The control. Nothing gentle left: `l20` is cleared, visited, and its one connection is
        // accounted for, so it reveals nothing; `l41` is the only frontier there is. The planner's
        // answer is to open the portal, and the door to the node it named must not be refused.
        let mut only_way_on = WorldMap::default();
        for (a, b) in [("e2", "l20"), ("e2", "l41")] {
            only_way_on.entry(a).neighbours.insert(b.into());
            only_way_on.entry(b).neighbours.insert(a.into());
        }
        only_way_on.entry("l20").heading = "Keyingham — level 2 crypt".into();
        only_way_on.entry("l20").visited = true;
        only_way_on.entry("l20").connections = 1;
        only_way_on.entry("l41").heading = "Crockey — level 5 crypt".into();
        only_way_on.entry("e2sub6").parent = Some("e2".into());
        only_way_on.here = Some("e2sub6".into());
        only_way_on.apply_save(
            &crate::game::save::parse(
                "return { overworld = { areaFlags = { hell = 0 },
                     completedAreas = { e2 = true, l20 = true } } }",
            )
            .unwrap(),
        );
        let (chosen, _, note) = only_way_on.choose_exit(&doors).expect("two doors is not no doors");
        assert_eq!(chosen, "l41", "the errand is to open the portal, so the door to it must open");
        assert!(
            note.starts_with("OpenAnomaly -> l41"),
            "the control only means anything if the errand really is the trigger: {note}"
        );
        assert!(
            !note.contains("gentle doors only"),
            "and the rule must report itself as lifted, not merely overruled: {note}"
        );
    }

    /// A village whose every exit is a level 4+ node still has to be left.
    ///
    /// The rule is a preference expressed as a filter, and every filter in `choose_exit` is dropped
    /// when it would leave nothing — being inside a subworld with no door named is worse than any
    /// one door. Without this the rule would turn a perfectly ordinary map into a stall, which is
    /// how `inside {container} with no crossing plan` ended runs before.
    #[test]
    fn the_level_4_door_rule_never_leaves_a_village_with_no_way_out() {
        use crate::observe::adjacency::Exit;
        let door = |to: &str| Exit { x: 0.0, y: 0.0, to_key: to.into(), to_heading: "".into() };

        let mut m = WorldMap::default();
        for (a, b) in [("e2", "l41"), ("e2", "l45"), ("l41", "shrine3")] {
            m.entry(a).neighbours.insert(b.into());
            m.entry(b).neighbours.insert(a.into());
        }
        m.entry("l41").heading = "Crockey — level 5 crypt".into();
        m.entry("l45").heading = "Bessingby — level 5 crypt".into();
        m.entry("shrine3").heading = "Gembling shrine".into();
        m.entry("e2sub6").parent = Some("e2".into());
        m.here = Some("e2sub6".into());
        m.apply_save(
            &crate::game::save::parse(
                "return { overworld = { areaFlags = { hell = 0 },
                     completedAreas = { e2 = true } } }",
            )
            .unwrap(),
        );

        assert_eq!(
            m.choose_exit(&[door("l41"), door("l45")]).map(|(k, _, _)| k).as_deref(),
            Some("l41"),
            "no gentle door exists, so the rule lifts rather than stalling the crossing"
        );
    }

    /// Which door out of the forest, asked the way the dev asked it.
    ///
    /// Standing inside `l62`, three exits open: `shrine7`, which is finished; `l40` to the
    /// south-east; `l57` to the south-west, on a road already walked and reaching the shrine we
    /// want. The planner took `shrine7`, walked to a dead end, came back, and then took `l40` — the
    /// one exit whose road it would have to carve, and the one whose exit node has stalled a run
    /// three times.
    ///
    /// It was not choosing badly. It was not choosing at all: `next_target` plans from `here`, `here`
    /// was an interior node, no interior node reaches the surface in this map, so the ranked branch
    /// was skipped every time and `SafestOfWhatIsLeft` picked the door.
    #[test]
    fn the_door_out_of_a_forest_is_chosen_from_outside_it() {
        use crate::observe::adjacency::Exit;
        let door = |to: &str| Exit {
            x: 0.0,
            y: 0.0,
            to_key: to.into(),
            to_heading: format!("{to} heading"),
        };

        let mut m = WorldMap::default();
        for (a, b) in [("l62", "shrine7"), ("l62", "l40"), ("l62", "l57"), ("l57", "shrine9")] {
            m.entry(a).neighbours.insert(b.into());
            m.entry(b).neighbours.insert(a.into());
        }
        m.entry("shrine9").heading = "Somewhere shrine".into();
        // Completion comes from the save and nowhere else — since 2026-08-16 `apply_save`
        // clears what the save no longer lists, because corruption takes completion away.
        // Setting it by hand here used to survive the fold; now it is stated where the game
        // states it.
        // Inside the forest, on an interior node that borders nothing on the surface — which is the
        // whole of the problem this test is about.
        m.entry("l62sub18").parent = Some("l62".into());
        m.here = Some("l62sub18".into());
        m.apply_save(
            &crate::game::save::parse(
                "return { overworld = { areaFlags = { hell = 0.1 },
                     completedAreas = { l62_path_to_l57 = true, shrine9 = true } } }",
            )
            .unwrap(),
        );
        assert_eq!(m.inside(), Some("l62"), "the fixture must actually be inside the forest");

        // `choose_exit` rather than `exit_toward`, because the reason is the point: picking `l57`
        // by luck out of the safety fallback would pass the first assertion and fix nothing.
        let (chosen, why, _) = m
            .choose_exit(&[door("shrine7"), door("l40"), door("l57")])
            .expect("three doors is not no doors");
        assert_eq!(
            chosen, "l57",
            "the door that leads to the shrine, not the one that is merely safe"
        );
        assert_eq!(why.why(), "nearest to the target", "and it was ranked, not fallen back on");
    }

    /// Task #96: the two plans printed on a crossing's door line are measured from two places.
    ///
    /// The line reads `door: <door_note> | reason <why> | plan now <plan>`, and the entry was filed
    /// because the halves contradicted each other. Live 2026-08-22 2159Z, inside `l4`:
    ///
    /// ```text
    /// door: Heart -> l1; doors l10=1 l1=0 l25=2 shrine1=7 | reason nearest to the target
    ///                                                    | plan now Heart -> l4_path_to_l25
    /// ```
    ///
    /// `l4_path_to_l25` is an *interior* node, and a surface errand naming one looked like a leak.
    /// It was not. `door_note` is written by [`WorldMap::choose_exit`] from
    /// [`WorldMap::plan_from_out_here`] — the container's vantage — and answered `l1`, against which
    /// `l1=0` is exactly right. `plan now` printed the driver's own `next_target`, planned from
    /// `here`, which in a subworld is an interior node; from there the heart errand finds no shop it
    /// can route to and falls through to `probe_toward_the_unknown`, which can only nominate what
    /// its own component contains. Both answers were correct and neither was the other's.
    ///
    /// This fixture reproduces the pair down to the node name: `Heart -> l9_path_to_l1` from inside,
    /// `Heart -> l1` from out here. The door line now prints the second, so the two halves are
    /// comparable again and a disagreement means what #51 wanted it to mean — a stale commitment.
    #[test]
    fn the_two_plans_on_a_door_line_are_measured_from_two_different_places() {
        let (mut m, _, _) = a_forest_with_two_doors();
        // Over the price of a heart with the bed money still behind it, which is the whole of
        // `wants_a_heart`. `l1 Cowlam village` is the shelf; `l19` is a campfire and sells nothing.
        m.gold = crate::overworld::HEART_FLOOR;
        assert_eq!(m.inside(), Some("l9"), "the fixture must actually be inside the forest");

        let inside = m.next_target().expect("the heart errand is live from both vantages");
        assert_eq!(inside.reason, Goal::Heart);
        assert_eq!(
            m.places.get(&inside.target).and_then(|p| p.parent.as_deref()),
            Some("l9"),
            "planned from `here`, and `here` reaches nothing but the inside of the forest"
        );

        let out_here = m.plan_from_out_here().expect("the container can route to the village");
        assert_eq!(out_here.reason, Goal::Heart, "the same errand, and that is the point");
        assert_eq!(out_here.target, "l1", "the shop itself, which is what the doors are ranked on");
    }

    /// Inside a subworld the route test is not asked, because it would only measure our own model.
    ///
    /// Nothing links an interior node to its container, so `distances` from in here reaches no
    /// surface node at all and every surface errand fails a route test for a reason that has nothing
    /// to do with the world. Gating on it traded a campfire out the west door at 1 of 20 health for
    /// whichever interior node happened to be walkable.
    #[test]
    fn inside_a_subworld_every_surface_errand_survives_the_route_test() {
        let (mut m, _, _) = a_forest_with_two_doors();
        m.note_health_level(crate::rest::Health { current: 1, max: 20 });
        assert!(!m.can_route_to("l19"), "no edge joins the interior to the surface");
        let plan = m.next_target().unwrap();
        assert_eq!(
            plan.reason,
            Goal::Rest,
            "the exits are the route, and `cross_toward` walks them"
        );
        // `l1`, the village. `l19` is a campfire, nearer and free, and not a rest site until
        // arriving at one does something — see `rest::CAMPFIRE_REST_IS_BUILT`.
        assert_eq!(plan.target, "l1");
    }

    #[test]
    fn which_door_we_are_crossing_to_is_decided_once() {
        // **How bad this was, measured rather than argued.** Inside a subworld, `distances(here)`
        // reaches no surface node at all -- the interior and the overworld are separate components
        // of our graph, since nothing links a subworld node to its container. So `exit_toward`'s
        // ranking-by-distance silently never applies while crossing, and every crossing was decided
        // by its last-resort fallback: *the first exit in the dump that is not the entrance*.
        //
        // Which means the door was a function of what the current pan happened to print. Exits print
        // only while visible (`overworldview.lua:1044`), so a pan that showed one road and not the
        // other reversed the crossing -- and the next pan reversed it back. Not an argmax that could
        // flip; no argmax at all.
        let (mut m, dane, cowlam) = a_forest_with_two_doors();

        // Both roads in sight: pick one, and record it. `l1` wins the fallback ranking -- both
        // doors are `Risk::Free` and it sorts first -- which is a arbitrary-but-stable choice, and
        // being stuck with it is exactly what this test is about.
        match m.cross_toward(&[dane.clone(), cowlam.clone()]) {
            Some(Crossing::Step { to, toward }) => {
                assert_eq!(toward, "l9_path_to_l1");
                assert_eq!(to, "l9sub2", "one hop along the route, re-derived every step");
            }
            other => panic!("expected a step toward the road out, got {other:?}"),
        }

        // The signal is there: asked afresh with only the near road in sight, the chooser turns the
        // run around. This is the pre-fix answer, and it is what a live run acted on.
        assert_eq!(m.exit_toward(&[dane.clone()]), Some("l19".into()), "unguarded, fog flips it");

        // The crossing does not turn around.
        match m.cross_toward(&[dane.clone()]) {
            Some(Crossing::Step { toward, .. }) => assert_eq!(toward, "l9_path_to_l1"),
            other => panic!("expected the same door, got {other:?}"),
        }
        // Nor when the pan shows no way out whatever. `exit_toward` gives up on an empty list and
        // `cross_toward` used to hand the driver a `None`, which it reports as "no crossing plan"
        // and ends the run -- over a pan that simply had no exit in shot.
        assert_eq!(m.exit_toward(&[]), None, "the chooser has nothing to go on");
        match m.cross_toward(&[]) {
            Some(Crossing::Step { toward, .. }) => assert_eq!(toward, "l9_path_to_l1"),
            other => panic!("fog is not a dead end, got {other:?}"),
        }
    }

    #[test]
    fn a_change_of_errand_releases_the_door() {
        // The other half, and the half that keeps this from being blind commitment. Taking a fight
        // in the woods is not our own movement rearranging an argmax -- it is the world changing,
        // and the campfire we were walking away from is now the point.
        let (mut m, dane, cowlam) = a_forest_with_two_doors();
        assert_eq!(m.next_target().map(|p| p.reason), Some(Goal::Explore));
        m.cross_toward(&[dane.clone(), cowlam.clone()]);

        m.note_health_level(crate::rest::Health { current: 1, max: 20 });
        assert_eq!(m.next_target().map(|p| p.reason), Some(Goal::Rest), "the errand changed");
        match m.cross_toward(&[dane, cowlam]) {
            Some(Crossing::Step { toward, .. }) => assert_eq!(toward, "l9_path_to_l1", "the bed"),
            other => panic!("expected the door to the village, got {other:?}"),
        }
    }

    /// #51: a plan naming **another door of this same container** releases the commitment, and a
    /// target merely wandering inside does not.
    ///
    /// Both halves come from live logs. The `l4` loop of the 1043Z run held `l4_path_to_l1` for ten
    /// steps while the errand read `Heart -> l4_path_to_l10` — the door it was actually asking for,
    /// and one the log itself called "not on any route we know". The 1752Z run, with the door
    /// diagnostic in, showed the opposite case: inside `l21` the target churned `l21sub7`,
    /// `l21sub9`, `l21sub8`, `l21sub10` step by step while the door stayed `e1`, because the plan is
    /// recomputed from `here` and `here` changes every step of a crossing. Dropping the commitment
    /// on *that* would re-derive the door every step and lose the whole point of having one.
    ///
    /// [`WorldMap::committed_exit`] is called directly rather than through `cross_toward`, because
    /// the case needs the planner to nominate a specific door and the fixture would have to be built
    /// backwards from the planner's ranking to arrange it. What is asserted here is the rule; that
    /// `cross_toward` hands it `next_target().target` is one line at the call site.
    #[test]
    fn a_plan_naming_another_door_of_the_same_subworld_releases_the_commitment() {
        let (mut m, dane, cowlam) = a_forest_with_two_doors();
        m.cross_toward(&[dane, cowlam]);
        let (door, goal) = m.crossing_to.clone().expect("the crossing committed to a door");
        assert_eq!(door, "l1", "the fixture's stable choice — see the test above");

        let held = |target: &str| m.committed_exit("l9", Some(&goal), Some(target));
        assert_eq!(
            held("l9sub1").as_deref(),
            Some("l1"),
            "wandering inside is not a contradiction"
        );
        assert_eq!(held("l9_path_to_l1").as_deref(), Some("l1"), "asking for the door we hold");
        assert_eq!(held("l9_path_to_l19"), None, "the other door — this is the `l4` loop");
        assert_eq!(
            held("l4_path_to_l10").as_deref(),
            Some("l1"),
            "a door of some other subworld says nothing about this crossing"
        );
    }

    #[test]
    fn writing_off_the_road_releases_the_door() {
        // `abandoned` is the driver's record of having had its go, and the only evidence available
        // in here that a door is genuinely no good. Fog is not evidence; this is.
        let (mut m, dane, cowlam) = a_forest_with_two_doors();
        m.cross_toward(&[dane.clone(), cowlam.clone()]);
        m.abandon("l9_path_to_l1");
        match m.cross_toward(&[dane, cowlam]) {
            Some(Crossing::Step { toward, .. } | Crossing::Probe { toward, .. }) => assert_eq!(
                toward, "l9_path_to_l19",
                "re-derived, and `exit_toward` skips the road we wrote off"
            ),
            other => panic!("expected a fresh choice, got {other:?}"),
        }
    }

    #[test]
    fn the_door_does_not_outlive_the_forest() {
        // A commitment is to *this* crossing. Re-entering is the one moment the interior is allowed
        // to have changed under us -- `subworld::Rules::edges_survive_reentry` exists because it can
        // re-roll -- so last visit's choice is worth nothing.
        let (mut m, dane, cowlam) = a_forest_with_two_doors();
        m.cross_toward(&[dane.clone(), cowlam.clone()]);
        assert!(m.crossing_to.is_some());
        m.fold(&dump("l19", "Dane village", vec![node("l9", "Saltagh Park — level 1 forest")]));
        assert_eq!(m.crossing_to, None, "out on the surface, the planner gets its say again");
    }

    #[test]
    fn walking_into_the_dark_still_avoids_the_other_exits() {
        // `l9`, live: crossing toward `l9_path_to_l19` with no known route, the fallback took any
        // neighbour and chose `l9_path_to_l1`. The run left the forest, arrived at a level 7
        // corrupted crypt, and was robbed of all 763 gold by a highwayman on the way.
        let exits = vec![
            Exit { x: 0.0, y: 0.0, to_key: "l19".into(), to_heading: "Dane village".into() },
            Exit {
                x: 0.0,
                y: 0.0,
                to_key: "l1".into(),
                to_heading: "Cowlam — level 7 crypt".into(),
            },
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
            Some(Crossing::Probe { to, .. }) => {
                assert_ne!(to, "l9_path_to_l1", "that is a way OUT, by the wrong door");
                assert_eq!(to, "l9sub26", "the road, and unvisited");
            }
            other => panic!("expected an explore step, got {other:?}"),
        }
    }

    #[test]
    fn a_hurt_run_in_a_village_heads_for_the_inn_rather_than_the_way_out() {
        // The whole point of entering. `cross_toward` only ever knew how to head for an exit, so a
        // rest errand walked in one door and out the other.
        let mut m = inside_a_village(
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
        let mut m = inside_a_village(
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
        let mut m = inside_a_village(
            ("l10sub1", "Ulrome well"),
            vec![node("l10sub4", "The Wobbly Cat inn"), node("l10_path_to_l7", "Road to Greenoak")],
            9,
        );
        match m.cross_toward(&[exit("l19"), exit("l7")]) {
            Some(Crossing::Step { toward, .. }) | Some(Crossing::Probe { toward, .. }) => {
                assert!(
                    toward.starts_with("l10_path_to_"),
                    "heading out, not to the bar: {toward}"
                );
            }
            other => panic!("expected an exit crossing, got {other:?}"),
        }
    }

    /// **#99.** A `Seek` has no destination, and the log has to say which of the four reasons that
    /// is. Three of them are errands the fog is hiding; the fourth is the way out.
    ///
    /// The live case is `Store`. Inside Boreas on 2026-08-23, on a `Heart` errand with the general
    /// store unrevealed, twelve steps printed *no way out of `l59` in sight* — and the search ended
    /// by finding the shop, which is what it had been for all along.
    /// **#100, and the two rulings it was read as a contradiction between.**
    ///
    /// The dev, 2026-08-23: *degree for the inn or store, and distance (only if known) for the
    /// exit.* One map, one standing position, one pair of candidates — and the only thing that
    /// differs between the two halves is what the run is looking for.
    ///
    /// `l10sub2` is one hop away with a single unrevealed connection. `l10sub9` is two hops away
    /// with five. Degree wants the far one; distance wants the near one; `is_paved` is deliberately
    /// true for both, so the term above them cannot decide it.
    ///
    /// **Neither half has an exit in the dump**, which is the state this is about: an empty exits
    /// list is fog, not a dead end, so `dest` is `None` in both and the old `dest.is_some()` cut
    /// sent both to degree.
    #[test]
    fn a_fogged_search_ranks_by_what_it_is_searching_for() {
        // Two hops of interior, folded from the far end so the far node is known but unvisited.
        let woods = |gold: i64, health: i64| {
            let mut m = WorldMap::new();
            m.fold(&dump("l19", "Gipsyville crypt", vec![node("l10", "Ulrome village")]));
            // From `l10sub3`, which names the six-way node beyond it.
            m.fold(&inside_dump(
                "l10",
                "l10sub3",
                "Ulrome road",
                vec![
                    Node { connections: 6, ..node("l10sub9", "Ulrome road") },
                    node("l10sub1", "Ulrome road"),
                ],
                vec![],
            ));
            // And back to where we stand, which names the two-way node beside us.
            m.fold(&inside_dump(
                "l10",
                "l10sub1",
                "Ulrome road",
                vec![node("l10sub2", "Ulrome road"), node("l10sub3", "Ulrome road")],
                vec![],
            ));
            m.apply_save(
                &crate::game::save::parse(&format!("return {{ player = {{ gold = {gold} }} }}"))
                    .unwrap(),
            );
            m.note_health_level(crate::rest::Health { current: health, max: 20 });
            m
        };

        // **The positive controls.** Both candidates are frontier, both paved, and the two terms
        // genuinely disagree — without this the test could pass on a map where only one node was
        // ever eligible.
        let m = woods(HEART_FLOOR, 20);
        for k in ["l10sub2", "l10sub9"] {
            let p = m.get(k).expect(k);
            assert!(p.is_paved(), "{k} must be paved, or `is_paved` decides this and not the key");
            assert!(!p.visited, "{k} must still be frontier");
        }
        assert_eq!(m.get("l10sub2").unwrap().connections, 2, "one unrevealed connection");
        assert_eq!(m.get("l10sub9").unwrap().connections, 6, "five unrevealed connections");

        // **Searching a village for a store the fog hides.** Degree leads, so the six-way node two
        // hops out wins and the first step is toward it. The rule of 2026-08-15.
        let mut shopping = woods(HEART_FLOOR, 20);
        assert_eq!(shopping.searching_for("l10"), Searching::Store);
        let step = match shopping.cross_toward(&[]) {
            Some(Crossing::Seek { to }) | Some(Crossing::Probe { to, .. }) => to,
            other => panic!("expected a search, got {other:?}"),
        };
        assert_eq!(step, "l10sub3", "toward the six-way node, which is what a search is for");

        // **The same woods with nothing to buy: now it is a crossing, and the way out is what we
        // are looking for.** Nearest first, so the branch at our feet is finished before a richer
        // one further off is opened. The ruling of 2026-08-22, and *find the path and stick to it*.
        let mut crossing = woods(HEART_FLOOR - 1, 20);
        assert_eq!(crossing.searching_for("l10"), Searching::Exit);
        let step = match crossing.cross_toward(&[]) {
            Some(Crossing::Seek { to }) | Some(Crossing::Probe { to, .. }) => to,
            other => panic!("expected a search, got {other:?}"),
        };
        assert_eq!(step, "l10sub2", "the near branch, not the rich one two hops away");
    }

    #[test]
    fn a_search_with_no_destination_says_which_of_the_four_it_is() {
        // A village with no inn and no store drawn yet, which is the state all three village
        // answers share. What separates them is what the run wants.
        let village = |gold, health| {
            let mut m = inside_a_village(
                ("l10sub1", "Ulrome well"),
                vec![
                    node("l10sub2", "Ulrome house"),
                    node("l10_path_to_l19", "Road to Gipsyville"),
                    node("l10_path_to_l7", "Road to Greenoak"),
                ],
                gold,
            );
            m.note_health_level(crate::rest::Health { current: health, max: 20 });
            m
        };

        // **The positive control for the whole test**: this really is the branch that prints the
        // line. Without it every assertion below is about a function nothing calls.
        let mut shopping = village(HEART_FLOOR, 20);
        assert_eq!(
            shopping.cross_toward(&[exit("l19"), exit("l7")]),
            Some(Crossing::Seek { to: "l10sub2".into() }),
            "a full purse and a fogged store is a search with no destination"
        );
        assert_eq!(shopping.searching_for("l10"), Searching::Store);
        assert_eq!(shopping.searching_for("l10").what(), "its general store");

        // Hurt and a pound short of the shelf: the bed is the errand.
        let resting = village(HEART_FLOOR - 1, 1);
        assert_eq!(resting.searching_for("l10"), Searching::Inn);

        // Both wanted at once, which is an ordinary state rather than a corner: the search serves
        // both, and the answer is the guard that would have held us inside first.
        let both = village(HEART_FLOOR, 1);
        assert_eq!(both.searching_for("l10"), Searching::Inn, "the `leaving_to` order decides");

        // Nothing wanted, so the same fogged village is a plain crossing.
        let passing = village(HEART_FLOOR - 1, 20);
        assert_eq!(passing.searching_for("l10"), Searching::Exit);

        // And the third errand, which has no purse in it at all: a shrine forest whose plaza no
        // dump has drawn. Keeping the purse empty rules the other two out at their first line.
        let mut shrine = WorldMap::new();
        shrine.fold(&dump("l19", "Gipsyville crypt", vec![node("shrine1", "Gransmoor shrine")]));
        shrine.fold(&inside_dump(
            "shrine1",
            "shrine1sub1",
            "a glade",
            vec![node("shrine1sub2", "a glade")],
            vec![exit("l19")],
        ));
        shrine.apply_save(&crate::game::save::parse("return { player = { gold = 0 } }").unwrap());
        assert_eq!(shrine.searching_for("shrine1"), Searching::Shrine);

        // The negative control: the plaza on the map at all ends that search, and the container is
        // a crossing again.
        shrine.entry("shrine1_plaza").heading = "Gransmoor shrine".into();
        assert_eq!(shrine.searching_for("shrine1"), Searching::Exit);
    }

    #[test]
    fn a_village_whose_inn_is_still_fogged_is_searched_not_left() {
        // `store_inn` lands on whichever `<parent>sub<N>` the generator reaches first
        // (`village.lua:685`), so unlike an exit road there is no key to build and no route to plan:
        // the inn has to be seen. Until it is, the one move that must NOT be made is the exit —
        // which is exactly what the old fallback reached for, the entrance road being the nearest
        // thing to walk to.
        let mut m = inside_a_village(
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
        // **Under the heart's floor on purpose.** This is a test about the inn, and at 763 gold the
        // village would also be carrying a live heart errand — which since 2026-08-16 searches a
        // village whatever the anomaly is doing, so the run would keep looking for a general store
        // and the abandoned inn would prove nothing. `HEART_FLOOR - 1` is over `INN_COST`, so the
        // rest errand is real and the shopping one is not.
        let mut m = inside_a_village(
            ("l10sub1", "Ulrome well"),
            vec![node("l10sub4", "The Wobbly Cat inn"), node("l10_path_to_l7", "Road to Greenoak")],
            HEART_FLOOR - 1,
        );
        m.abandon("l10sub4");
        match m.cross_toward(&[exit("l19"), exit("l7")]) {
            Some(Crossing::Step { toward, .. }) | Some(Crossing::Probe { toward, .. }) => {
                assert!(toward.starts_with("l10_path_to_"), "back to crossing: {toward}");
            }
            other => panic!("expected an exit crossing, got {other:?}"),
        }
    }

    #[test]
    fn a_healthy_run_crosses_a_village_without_stopping_at_the_bar() {
        // The inn is only a destination while a rest is wanted. Otherwise a village is a subworld
        // like any other and the exit is the whole objective.
        //
        // Purse under `HEART_FLOOR`, so the bar is the only thing that could stop us — see the note
        // in `an_inn_we_have_already_tried_stops_the_search`, and the test below for what a full
        // purse does instead.
        let mut m = inside_a_village(
            ("l10sub1", "Ulrome well"),
            vec![node("l10sub4", "The Wobbly Cat inn"), node("l10_path_to_l7", "Road to Greenoak")],
            HEART_FLOOR - 1,
        );
        m.note_health_level(crate::rest::Health { current: 20, max: 20 });
        assert!(!m.wants_rest());
        match m.cross_toward(&[exit("l19"), exit("l7")]) {
            Some(Crossing::Step { toward, .. }) | Some(Crossing::Probe { toward, .. }) => {
                assert!(toward.starts_with("l10_path_to_"), "straight through: {toward}");
            }
            other => panic!("expected an exit crossing, got {other:?}"),
        }
    }

    /// **A full purse turns a village into an errand, with the anomaly still shut.**
    ///
    /// The dev, 2026-08-16, after the first adventure ever started from a cleared profile rested in
    /// a village with over 110 gold and walked past the shelf: *we should want the healthBuff
    /// regardless of anomaly state.*
    ///
    /// The control is the pair: the same village, the same health, the same exits, and the only
    /// difference a pound either side of [`HEART_FLOOR`]. Without it this would pass on a map that
    /// never crossed anything.
    #[test]
    fn a_village_is_searched_for_its_store_whatever_the_anomaly_is_doing() {
        let village = |gold| {
            let mut m = inside_a_village(
                ("l10sub1", "Ulrome well"),
                vec![
                    node("l10sub4", "The Wobbly Cat inn"),
                    node("l10_path_to_l7", "Road to Greenoak"),
                ],
                gold,
            );
            m.note_health_level(crate::rest::Health { current: 20, max: 20 });
            m
        };

        let mut poor = village(HEART_FLOOR - 1);
        assert!(!poor.anomaly_is_open().unwrap_or(false), "the portal is shut in both");
        assert!(!poor.wants_a_heart());
        match poor.cross_toward(&[exit("l19"), exit("l7")]) {
            Some(Crossing::Step { toward, .. }) | Some(Crossing::Probe { toward, .. }) => {
                assert!(toward.starts_with("l10_path_to_"), "a pound short: straight through")
            }
            other => panic!("expected an exit crossing, got {other:?}"),
        }

        let mut rich = village(HEART_FLOOR);
        assert!(rich.wants_a_heart(), "the anomaly no longer has a say in this");
        assert!(
            matches!(rich.cross_toward(&[exit("l19"), exit("l7")]), Some(Crossing::Seek { .. })),
            "with the price in hand the store is worth looking for, not walking past"
        );
    }

    #[test]
    fn walked_out_ground_is_not_paced_over_again() {
        // `l9sub2` <-> `l9_plaza`, twenty laps live, until the run was stopped by hand.
        //
        // Both paved, both completed, neither blocking, and the exit road never seen — so there was
        // no route to plan and the fallback was picking a neighbour by name. From `l9sub2` the
        // smallest key is `l9_plaza` (`_` sorts below `s`); from `l9_plaza` it is `l9sub2` (a prefix
        // of `l9sub26`). Two locally-correct choices, one stable cycle.
        // The two dumps exactly as the console printed them, so the neighbour sets are the game's
        // and not a convenient invention.
        let mut m = WorldMap::new();
        m.fold(&inside_dump(
            "l9",
            "l9sub2",
            "Saltagh Park road",
            vec![
                node("l9sub4", "Saltagh Park forest"),
                node("l9_plaza", "Saltagh Park crossroads"),
                node("l9sub9", "Saltagh Park road"),
            ],
            vec![exit("l19")],
        ));
        m.fold(&inside_dump(
            "l9",
            "l9_plaza",
            "Saltagh Park crossroads",
            vec![
                node("l9sub1", "Saltagh Park road"),
                node("l9sub2", "Saltagh Park road"),
                node("l9sub3", "Saltagh Park road"),
            ],
            vec![exit("l19")],
        ));
        m.note_health_level(crate::rest::Health { current: 1, max: 20 });
        // The state that produced the loop: both of these walked, everything else still dark.
        assert!(m.get("l9_plaza").unwrap().visited && m.get("l9sub2").unwrap().visited);
        assert!(!m.get("l9sub1").unwrap().visited && !m.get("l9sub3").unwrap().visited);

        // Standing on the plaza, `l9sub2` is the one neighbour with nothing left to teach us, and it
        // is the one the run took -- twenty times.
        match m.cross_toward(&[exit("l19")]) {
            Some(Crossing::Probe { to, .. }) => {
                assert_ne!(to, "l9sub2", "that is the way we came, and it is fully walked");
                assert_eq!(to, "l9sub1", "an unwalked road, nearest and paved");
            }
            other => panic!("expected a step into the dark, got {other:?}"),
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
            Some(Crossing::Probe { to, .. }) => assert_eq!(to, "l9sub16"),
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
            Some(Crossing::Probe { to, .. }) | Some(Crossing::Step { to, .. }) => {
                assert_eq!(to, "l9sub18", "round the blocker, by the road");
            }
            other => panic!("expected a step round it, got {other:?}"),
        }
    }

    /// **And it still follows the road when there is no door to head for**, which is the arm #100
    /// added and the one the older test above cannot reach.
    ///
    /// The dev, 2026-08-23: *I'd like to stay on the paved path for regular forests, not just the
    /// lost woods.* `is_paved` leads this key for every subworld and always has — what #100 changed
    /// is the pair of terms **below** it, and a fogged forest is the case where that pair is now
    /// ordered differently than it was. So the road rule needs a guard in this arm too, or the next
    /// change to the ordering can quietly cost it.
    ///
    /// The same node as the test above, verbatim from `spike-run-raw.log:635-645`, with one thing
    /// taken away: **no exits in the dump**, which is fog rather than a dead end. `dest` is `None`,
    /// `searching_for` is `Exit`, and distance leads — so the brush one hop away is exactly what
    /// would win if paved were not above it.
    #[test]
    fn a_forest_with_no_door_in_sight_still_follows_the_road() {
        let mut m = WorldMap::new();
        m.fold(&inside_dump(
            "l9",
            "l9sub22",
            "Saltagh Park road",
            vec![
                node("l9sub7", "Saltagh Park forest"),
                node("l9sub21", "Saltagh Park forest"),
                node("l9sub10", "Saltagh Park crossroads"),
            ],
            vec![],
        ));
        m.fold(&inside_dump(
            "l9",
            "l9sub10",
            "Saltagh Park crossroads",
            vec![node("l9sub22", "Saltagh Park road"), node("l9sub12", "Saltagh Park road")],
            vec![],
        ));
        m.fold(&inside_dump(
            "l9",
            "l9sub22",
            "Saltagh Park road",
            vec![
                node("l9sub7", "Saltagh Park forest"),
                node("l9sub21", "Saltagh Park forest"),
                node("l9sub10", "Saltagh Park crossroads"),
            ],
            vec![],
        ));

        // **The positive controls.** This has to be the arm under test, and the two candidates have
        // to genuinely disagree — otherwise the assertion passes for a reason it never states.
        assert_eq!(m.searching_for("l9"), Searching::Exit, "a forest has no errand to hide");
        assert!(
            m.get("l9sub21").expect("brush").is_paved() == false,
            "the near candidate is brush"
        );
        assert!(m.get("l9sub12").expect("road").is_paved(), "the far candidate is road");

        match m.cross_toward(&[]) {
            Some(Crossing::Seek { to }) | Some(Crossing::Probe { to, .. }) => {
                assert_eq!(to, "l9sub10", "toward the unwalked road, not into the brush next door");
            }
            other => panic!("expected a fogged search along the road, got {other:?}"),
        }
    }

    /// Exploring a forest follows the road too, even when brush is nearer.
    ///
    /// **The live case, 2026-08-08.** Crossing `l9` toward `l9_path_to_l19`, which was never on the
    /// map — so `first_step_toward` returned `None` every step and the frontier fallback *was* the
    /// whole crossing, for twenty-four steps. It ranked `(fight, distance, paved, key)`, so an
    /// unvisited patch of brush one hop away beat an unvisited road two hops along, every time. The
    /// run went road, brush, back to the road, and never crossed. The dev watched it and said so.
    ///
    /// The dumps below are that node verbatim (`spike-run-raw.log:635-645`): standing on `l9sub22`,
    /// a road, with `l9sub21` and `l9sub7` unvisited brush adjacent and the unvisited road `l9sub12`
    /// two hops off through the crossroads.
    #[test]
    fn exploring_a_forest_also_sticks_to_the_road() {
        let exits =
            vec![Exit { x: 0.0, y: 0.0, to_key: "l19".into(), to_heading: "Dane village".into() }];
        let mut m = WorldMap::new();
        m.fold(&inside_dump(
            "l9",
            "l9sub22",
            "Saltagh Park road",
            vec![
                node("l9sub7", "Saltagh Park forest"),
                node("l9sub21", "Saltagh Park forest"),
                node("l9sub10", "Saltagh Park crossroads"),
            ],
            exits.clone(),
        ));
        // The crossroads has been walked; the road beyond it has not.
        m.fold(&inside_dump(
            "l9",
            "l9sub10",
            "Saltagh Park crossroads",
            vec![node("l9sub22", "Saltagh Park road"), node("l9sub12", "Saltagh Park road")],
            exits.clone(),
        ));
        m.fold(&inside_dump(
            "l9",
            "l9sub22",
            "Saltagh Park road",
            vec![
                node("l9sub7", "Saltagh Park forest"),
                node("l9sub21", "Saltagh Park forest"),
                node("l9sub10", "Saltagh Park crossroads"),
            ],
            exits.clone(),
        ));

        // Nearest-first says `l9sub21` at one hop. The rule says the road at two.
        match m.cross_toward(&exits) {
            Some(Crossing::Probe { to, toward }) => {
                assert_eq!(toward, "l9_path_to_l19", "still the committed door");
                assert_eq!(to, "l9sub10", "toward the unwalked road, not into the brush");
            }
            other => panic!("expected an explore step along the road, got {other:?}"),
        }
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

    /// **#71 and #57 crossing each other: the walk heads for the shrine, not the door.**
    ///
    /// An interaction neither task's own tests reach. #71 put interior shrines back into
    /// `errand_inside` with the portal open, and `dest` is the errand when there is one
    /// (`cross_toward`), so the destination of a crossing can now be a shrine subnode. #57 then made
    /// `doorward` rank candidates by their distance to **`dest`** — which is the door only when there
    /// is no errand.
    ///
    /// Get this wrong and the failure is silent and exactly the one #71 was filed for: the run walks
    /// out of the forest past a shrine it came for. Worth a test precisely because both halves look
    /// right in isolation.
    #[test]
    fn a_crossing_with_a_shrine_errand_heads_for_the_shrine() {
        let at = |key: &str, heading: &str, x: f64, y: f64| Node {
            key: key.into(),
            heading: heading.into(),
            x,
            y,
            connections: 3,
        };
        let door = Exit {
            x: 1500.0,
            y: 1500.0,
            to_key: "l5".into(),
            to_heading: "Dalton Copse village".into(),
        };
        let mut m = WorldMap::new();
        m.fold(&inside_dump(
            "shrine1",
            "shrine1sub1",
            "Gripthorpe Brush road",
            vec![
                at("shrine1sub2", "Gripthorpe Brush woodland shrine", 100.0, 100.0),
                at("shrine1sub5", "Gripthorpe Brush road", 1400.0, 1400.0),
            ],
            vec![door.clone()],
        ));
        m.entry("shrine1").heading = "Gripthorpe Brush — level 1 forest".into();
        m.hell = Some(0.1);

        assert_eq!(
            m.errand_inside("shrine1").map(|p| p.key.as_str()),
            Some("shrine1sub2"),
            "the premise, and it is #71: with the portal open this is still an errand"
        );

        // `shrine1sub5` sits almost on top of the door, so a crossing that had fallen back to the
        // exit would take it — which is what makes this fixture able to fail.
        match m.cross_toward(&[door]) {
            Some(Crossing::Step { to, toward }) => {
                assert_eq!(
                    toward, "shrine1sub2",
                    "the errand is the destination, not the road out"
                );
                assert_eq!(to, "shrine1sub2");
            }
            other => panic!("expected a step to the shrine, got {other:?}"),
        }
    }

    /// **The one-walk crossing does not bounce, measured against the runs that did.**
    ///
    /// The rewrite of 2026-08-21 (`ee0ca84`, `b15f33f`) deleted an arm that **six of seven**
    /// crossings in the 1519Z run ended on, on the argument that a `doorward` ranking term does the
    /// same job without a second decision-maker to disagree with. That is the change on this branch
    /// with the least evidence behind it, and it cannot be exercised without a supervised run.
    ///
    /// This is the next best thing: replay both runs' interior dumps and ask the **new**
    /// `cross_toward` what it would do at each position the game really put the player in. Every
    /// answer is therefore evaluated in a state that actually happened. The *sequence* is
    /// counterfactual — once the new code diverges the run would have gone elsewhere — so the claim
    /// is deliberately pointwise: no two nodes nominate each other.
    ///
    /// **The positive control is the same measurement on the run's own report**, which finds the
    /// documented Upton Braken bounce (`l63sub5` against `l63_plaza`) and would fail this test if
    /// the detector were vacuous.
    #[test]
    fn the_one_walk_crossing_nominates_no_pair_that_nominates_it_back() {
        let mut runs = 0;
        for stem in ["spike-run-20260821-1519Z", "spike-run-20260821-0357Z"] {
            let Ok(log) = std::fs::read_to_string(format!("{stem}.log")) else {
                eprintln!("SKIP: {stem}.log is not present");
                continue;
            };
            let lines: Vec<String> = log.lines().map(|l| l.to_string()).collect();
            let dumps = crate::observe::adjacency::Reader::new().push(&lines);
            assert!(dumps.len() > 100, "{stem}: expected a whole run, got {}", dumps.len());

            let mut m = WorldMap::new();
            // **Per visit, not per container.** Two crossings of the same subworld are two
            // journeys with two targets, and the interior may re-roll between them
            // (`subworld::Rules::edges_survive_reentry`). Merging them reports `l63`'s exit to
            // `l43` and its exit to `l35` as a bounce, when they are two errands months apart in
            // the run. A bounce is two nodes nominating each other **inside one crossing**.
            let mut visits: Vec<(String, Vec<Decision>)> = Vec::new();
            for a in &dumps {
                m.fold(a);
                let Some((container, _)) = a.subworld.clone() else {
                    visits.push((String::new(), Vec::new()));
                    continue;
                };
                if visits.last().map(|(c, _)| c.as_str()) != Some(container.as_str()) {
                    visits.push((container.clone(), Vec::new()));
                }
                // `cross_toward` mutates (it holds a target across steps), and this is a
                // counterfactual, so it must not be allowed to write back into the replay.
                let to = match m.clone().cross_toward(&a.exits) {
                    Some(Crossing::Step { to, .. }) => to,
                    Some(Crossing::Probe { to, .. }) => to,
                    Some(Crossing::Seek { to }) => to,
                    _ => continue,
                };
                visits.last_mut().expect("just pushed").1.push((a.here_key.clone(), to));
            }
            visits.retain(|(c, d)| !c.is_empty() && !d.is_empty());
            let decided: usize = visits.iter().map(|(_, d)| d.len()).sum();
            assert!(decided > 50, "{stem}: only {decided} crossing decisions to judge");
            assert!(visits.len() > 5, "{stem}: only {} crossings to judge", visits.len());

            for (container, d) in &visits {
                assert!(
                    reciprocated(d).is_empty(),
                    "{stem}, crossing `{container}`: {:?} nominate each other",
                    reciprocated(d)
                );
            }
            runs += 1;
        }
        if runs == 0 {
            return;
        }

        // The control. Without it, a detector that finds nothing proves nothing.
        let Ok(report) = std::fs::read_to_string("spike-run-20260821-1519Z.md") else {
            eprintln!("SKIP the control: the report is not present");
            return;
        };
        let made = decisions_the_run_made(&report);
        let found: Vec<_> = made
            .iter()
            .flat_map(|(c, d)| reciprocated(d).into_iter().map(move |p| (c, p)))
            .collect();
        println!("the run's own bounces: {found:?}");
        // Named exactly, not merely counted. This scraper came back empty twice for reasons that
        // had nothing to do with the crossing — first by not knowing the word `steering`, which is
        // half of the alternation a bounce is made of, then by requiring a space in ` via ` that a
        // probe line does not have. Both times it reported the old two-arm crossing as clean.
        let upton = found
            .iter()
            .any(|(c, (a, b))| c.as_str() == "l63" && a == "l63_plaza" && b == "l63xrd60x-183");
        assert!(
            upton,
            "the run's own report must still contain the Upton Braken bounce (`l63_plaza` against \
             `l63xrd60x-183`); without it this detector is measuring nothing and the assertions \
             above are worthless. Found: {found:?}"
        );
        // Two more the ledger never named: `l39sub1` against `l39sub12`, and `l64sub10` against
        // `l64sub2`. Three bounced crossings out of eleven, in the run that met the MVP.
        assert!(found.len() >= 6, "expected three bounced crossings: {found:?}");
    }

    /// A road that bends the wrong way is still the road.
    ///
    /// `l40sub25` as the dump printed it on 2026-08-15, with the `l36` door at (233, 105):
    ///
    /// ```text
    ///   l40sub24  grave   (802,  65)   570 from the door   brush   <- the steer took this
    ///   l40sub17  road   (1102, 125)   869 from the door   paved
    /// ```
    ///
    /// The road runs north-east before it turns for a door that lies west, so it was *further* from
    /// the door than we already stood, the ceiling struck it out before anything was compared, and
    /// the grave was the only survivor. "Paved outranks near" was in the sort key and never saw a
    /// candidate.
    ///
    /// The dev's rule for the MVP: always prefer paved. With the road excluded on distance and the
    /// grave excluded for being brush, the steer declines — and the frontier walk below, which is
    /// itself paved-first, goes up the road toward the part of it we have not seen.
    #[test]
    fn the_crossing_will_not_leave_the_road_for_a_shortcut() {
        let node_at = |k: &str, h: &str, x: f64, y: f64| Node {
            key: k.into(),
            heading: h.into(),
            x,
            y,
            connections: 3,
        };
        let door = crate::observe::adjacency::Exit {
            x: 233.0,
            y: 105.0,
            to_key: "l36".into(),
            to_heading: "Wawne crypt".into(),
        };

        let mut m = WorldMap::new();
        // Walked in along the road, and the road carries on past `l40sub17` into ground we have not
        // seen — which is what makes a paved frontier exist at all.
        m.fold(&inside_dump(
            "l40",
            "l40sub17",
            "Fosholme Growth road",
            vec![
                node_at("l40sub25", "Fosholme Growth road", 0.0, 0.0),
                node_at("l40sub13", "Fosholme Growth road", 1400.0, 200.0),
            ],
            vec![door.clone()],
        ));
        m.crossing_to = Some(("l36".into(), Goal::Explore));

        // Now standing on `l40sub25`, having been 600 from the door at the nearest.
        m.fold(&inside_dump(
            "l40",
            "l40sub25",
            "Fosholme Growth road",
            vec![
                node_at("l40sub24", "Fosholme Growth — level 5 grave", 802.0, 65.0),
                node_at("l40sub17", "Fosholme Growth road", 1102.0, 125.0),
            ],
            vec![door.clone()],
        ));
        // The fixture has to be the one that used to fail: the grave genuinely is nearer the door.
        let to_door = |k: &str| {
            let (x, y) = m.placed_now(k).expect("placed");
            let (dx, dy) = m.placed_now("l40_path_to_l36").expect("the door is in the frame");
            ((x - dx).powi(2) + (y - dy).powi(2)).sqrt()
        };
        assert!(to_door("l40sub24") < to_door("l40sub17"), "the shortcut is the shorter line");

        let step = m.cross_toward(&[door]).and_then(|c| match c {
            Crossing::Step { to, .. } | Crossing::Probe { to, .. } | Crossing::Seek { to } => {
                Some(to)
            }
            _ => None,
        });
        assert_eq!(
            step.as_deref(),
            Some("l40sub17"),
            "the road, even though it bends away from the door first"
        );
    }

    /// Safety decides the door only when we are looking for a bed.
    ///
    /// The dev's ruling, given first for destinations and then again for doors. Corruption *is* high
    /// node levels, so ranking risk first points a run away from the anomaly — the one place it is
    /// trying to reach. This is the disagreement in its purest form: the corrupted door is the one
    /// the objective lies behind, and the safe door leads away from it.
    ///
    /// Fallback territory on purpose. The anomaly comes from the save with no edges, so no exit is
    /// measurable by route, and this ordering is the whole of the decision.
    #[test]
    fn safety_decides_the_door_only_when_we_want_a_bed() {
        use crate::observe::adjacency::Exit;
        let door = |to: &str| Exit {
            x: 0.0,
            y: 0.0,
            to_key: to.into(),
            to_heading: format!("{to} heading"),
        };
        let at = |key: &str, heading: &str, x: f64| Node {
            key: key.into(),
            heading: heading.into(),
            x,
            y: 0.0,
            connections: 2,
        };

        let mut m = WorldMap::new();
        ready_for_the_anomaly(&mut m);
        // West lies a corrupted spider forest; east, a road already cleared. Both border `l62`.
        m.fold(&dump(
            "l62",
            "Fangfoss Chaparral — level 7 spider forest",
            vec![
                at("l57", "Harswell Coppice — level 6 spider forest", -200.0),
                at("l40", "Fosholme road", 200.0),
            ],
        ));
        m.entry("l40").completed = true;
        // Both doors have been stood on, so neither is a frontier and `Explore` has nothing to
        // offer. That leaves the anomaly as the only target, which is the state this ordering exists
        // for — and the state the live run was in.
        m.entry("l57").visited = true;
        m.entry("l40").visited = true;
        m.entry("l62sub18").parent = Some("l62".into());
        m.here = Some("l62sub18".into());
        // The anomaly is open and known only by key, and the corruption around `l57` is what gives
        // it a bearing at all — see `pos_for`.
        m.apply_save(
            &crate::game::save::parse(
                "return { overworld = { areaFlags = {
                     hell = 0.1, start_first_corrupt_time = 12, l57_first_corrupt_time = 12,
                 } } }",
            )
            .unwrap(),
        );
        assert_eq!(
            m.get("l57").map(|p| p.risk()),
            Some(Risk::Corrupt),
            "the fixture must disagree"
        );
        assert_eq!(m.get("l40").map(|p| p.risk()), Some(Risk::Free));

        let doors = [door("l57"), door("l40")];
        let (going, why, _) = m.choose_exit(&doors).expect("two doors");
        assert_eq!(
            going, "l57",
            "toward the anomaly, through the corruption, because that is where it is"
        );
        assert_eq!(why.why(), "safest, and toward the anomaly");

        // Hurt, and the same map answers the other way.
        m.note_health_level(crate::rest::Health { current: 1, max: 20 });
        assert!(m.wants_rest());
        assert_eq!(
            m.choose_exit(&doors).map(|(k, _, _)| k).as_deref(),
            Some("l40"),
            "a run that needs a bed takes the safe door, which is the whole point of the ordering"
        );
    }

    /// **The way out of a village is not an errand inside it**, however its road is named.
    ///
    /// Live 2026-08-22 0203Z, and the run's own loop trace is the shape of it:
    ///
    /// ```text
    ///   37. l32sub14 — Heart -> l32sub10
    ///   38. shrine2  — Heart -> l32
    ///   39. l32      — Heart -> l25
    ///   40. l32sub14 — Heart -> l32sub10
    /// ```
    ///
    /// Enthorpe's road out is `l32_path_to_shrine2`, heading *Road to Gransmoor shrine*. It carries
    /// `l32` as its parent, and its heading ends in the word `shrine`, so `shrine_inside` named it
    /// as the errand — a destination reached by **leaving**. The run entered the village to buy a
    /// heart, walked straight out to `shrine2`, and from there the village was again the nearest
    /// place to buy one. Four laps before the guard stopped it.
    ///
    /// `seeking_a_heart` was true throughout, which is the part worth keeping: the guard that holds
    /// a run inside a village while it looks for the shop was right and was never consulted, because
    /// `errand_at.is_some()` is tested first.
    ///
    /// The positive control is the second half. A shrine that really is inside — a woodland shrine
    /// subnode, which is the case [`WorldMap::shrine_inside`] exists for — must still be found, or
    /// this fix would have traded one silent failure for another.
    #[test]
    fn the_road_out_is_not_an_errand_inside_however_it_is_named() {
        let mut m = WorldMap::new();
        m.fold(&dump("l32", "Enthorpe village", vec![node("shrine2", "Gransmoor shrine")]));
        m.fold(&inside_dump(
            "l32",
            "l32sub14",
            "Enthorpe west guard post",
            vec![
                node("l32sub10", "Enthorpe house"),
                node("l32_path_to_shrine2", "Road to Gransmoor shrine"),
            ],
            vec![exit("shrine2")],
        ));
        m.here = Some("l32sub14".into());

        // The road is still a shrine by heading — that is the trap, and it has not gone away.
        assert!(
            m.get("l32_path_to_shrine2").unwrap().is_shrine(),
            "if the heading stopped reading as a shrine this test proves nothing"
        );
        assert_eq!(m.errand_inside("l32"), None, "the way out is not somewhere to go");
        assert_eq!(m.shrine_inside("l32").map(|p| p.key.as_str()), None);

        // A shrine that genuinely is inside is still the errand.
        m.fold(&inside_dump(
            "l32",
            "l32sub14",
            "Enthorpe west guard post",
            vec![node("l32sub9", "Enthorpe woodland shrine")],
            vec![exit("shrine2")],
        ));
        assert_eq!(
            m.errand_inside("l32").map(|p| p.key.clone()),
            Some("l32sub9".into()),
            "a woodland shrine subnode is the case `shrine_inside` was written for"
        );
    }

    /// **And with a heart wanted, the crossing searches the village instead of leaving it.**
    ///
    /// The dev's ask, 2026-08-22: *it should be searching within the settlement that we entered, not
    /// disappearing immediately because the entrance isn't adjacent to the store.* This is that,
    /// stated as a move rather than as a predicate — it is the half that could still have gone wrong
    /// after the road stopped claiming to be an errand, because "no errand and no door" has to mean
    /// *explore*, not *stall*.
    #[test]
    fn a_village_entered_for_a_heart_is_searched_and_not_left() {
        let mut m = WorldMap::new();
        m.fold(&dump("l32", "Enthorpe village", vec![node("shrine2", "Gransmoor shrine")]));
        m.fold(&inside_dump(
            "l32",
            "l32sub14",
            "Enthorpe west guard post",
            vec![
                node("l32sub10", "Enthorpe house"),
                node("l32_path_to_shrine2", "Road to Gransmoor shrine"),
            ],
            vec![exit("shrine2")],
        ));
        m.here = Some("l32sub14".into());
        // The purse the 0203Z run actually had when it started going round in circles.
        m.gold = 339;

        assert!(m.seeking_a_heart("l32"), "the fixture must want a heart, or this proves nothing");
        assert!(m.store_inside("l32").is_none(), "and must not have found the shop yet");

        let exits = vec![exit("shrine2")];
        let moved = m.cross_toward(&exits).expect("no errand and no door means explore, not stall");
        let toward = match &moved {
            Crossing::Leave { to } => panic!("left the village it came to shop in, toward `{to}`"),
            Crossing::Step { toward, .. } | Crossing::Probe { toward, .. } => Some(toward.clone()),
            _ => None,
        };
        assert_ne!(
            toward.as_deref(),
            Some("l32_path_to_shrine2"),
            "still steering at the way out: {moved:?}"
        );
    }
}
