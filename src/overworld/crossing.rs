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

use super::{dist_or_far, exit_node_key, heading_has_combat, Door, Goal, Place, Risk, WorldMap};
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

impl WorldMap {
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
    pub(super) fn choose_exit(
        &self, exits: &[crate::observe::adjacency::Exit],
    ) -> Option<(String, Door, String)> {
        if exits.is_empty() {
            return None;
        }
        let entrance = self.entered_from.clone();
        // **Ask the question from outside the forest.**
        //
        // `next_target` plans from `self.here`, and while we are inside a subworld `here` is an
        // interior node. Nothing links an interior node to its container in this map — the two are
        // separate components, as [`WorldMap::exit_toward`]'s doc has said all along — so every
        // surface place is unroutable, `plan` returns nothing, and the whole ranked branch below is
        // skipped. What is left is `SafestOfWhatIsLeft`: a door picked for safety with **no
        // reference to where we are going**.
        //
        // That is not a rare degenerate case, it is what happens every single time we cross a
        // subworld, and it cost three runs on 2026-08-15. From inside `l62` it chose the exit to
        // `shrine7` — a dead end already consecrated, prayed at and finished — walked there, turned
        // round, re-entered the forest, and then chose `l40` to the south-east while `l57` to the
        // south-west stood open on a road already walked and reaching every remaining shrine.
        //
        // The container is a surface node with surface edges, and it is where we will be standing
        // the moment we step out. So plan from there. Every exit's `to_key` is a surface node too,
        // which is why the ranking below works at all once it is given a target it can measure.
        // ## The whole map, with one thing changed. It used to be six fields, and that was the
        // ## `l10` <-> `l18` ping-pong
        //
        // The list was `places`, `abandoned`, `roads_done`, `hell`, `wants_rest`, `gold`. Everything
        // else took `Default`, and the one that mattered was **`heart_bought`** — the record of
        // which general stores we have already emptied.
        //
        // So the planner, asked from inside Ulrome, was asked by a map that had forgotten every
        // heart the run ever bought. With 294 gold in hand `wants_a_heart` was true, the nearest
        // settlement with a shelf was `l32`, and the answer came back `Heart -> l32` — whose nearest
        // door is `l18`. Outside, the real map remembered, answered `CloseAnomaly -> start`, and
        // sent us back into the village. Three ping-pongs in two days, and the run of
        // 2026-08-16 0602Z shows the two answers alternating on consecutive lines:
        //
        // ```text
        // 7. l18 -> **l10** (for start, CloseAnomaly)
        // 9. crossing `l10` toward `l10_path_to_l18` … door choice: Heart -> l32
        // ```
        //
        // A hand-written field list is a promise to remember every future field, and this file has
        // over twenty. Copying the map entire keeps that promise by construction; the vantage point
        // is the only thing that should differ, so it is the only thing set.
        //
        // `here` being a surface node is what makes it "outside" — `inside()` reads the parent of
        // `here`, and a container has none.
        let outside = self.inside().map(|container| {
            let mut m = self.clone();
            m.here = Some(container.to_string());
            m
        });
        // **The errand is kept, not just the destination.** It decides whether the anti-backtracking
        // rule below is allowed to overrule a measured answer — see there.
        let plan = outside.as_ref().unwrap_or(self).next_target();
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
            gentle_doors_only
                && self.places.get(to).map(|p| p.triggers_anomaly()).unwrap_or(false)
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
                        (dist_or_far(&dist, &e.to_key), risk as i64, &e.to_key)
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
                parent.map(|p| !self.abandoned.contains(&exit_node_key(p, &e.to_key))).unwrap_or(true)
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
    pub(super) fn committed_exit(&self, parent: &str, errand: Option<&Goal>, target: Option<&str>) -> Option<String> {
        let (to, goal) = self.crossing_to.as_ref()?;
        if errand != Some(goal) || self.abandoned.contains(&exit_node_key(parent, to)) {
            return None;
        }
        let committed = exit_node_key(parent, to);
        let contradicted = target
            .is_some_and(|t| t.starts_with(&format!("{parent}_path_to_")) && t != committed);
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
                && self.places.get(k).map(|p| {
                    !p.avoid && !(self.wants_rest && p.is_chest() && !p.completed)
                }).unwrap_or(true)
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
        let door_now = dest.as_deref().and_then(|d| self.placed_now(d));
        let doorward = |p: &Place| -> u64 {
            match (door_now, self.placed_now(&p.key)) {
                (Some(d), Some(at)) => {
                    let g = (at.0 - d.0).powi(2) + (at.1 - d.1).powi(2);
                    // Squared throughout — it orders the same as the distance and takes no root.
                    // Clamped rather than cast blind: a NaN or an absurd coordinate must sort last,
                    // not wrap to zero and win.
                    if g.is_finite() && g >= 0.0 { g.min(u64::MAX as f64 - 1.0) as u64 } else { u64::MAX }
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
            // The doorward term is absent when the destination has no printed position — the fogged
            // inn — so that search keeps exactly the ordering it was given.
            .filter_map(|p| {
                let unrevealed = p.connections.saturating_sub(p.neighbours.len() as u32);
                hops.get(&p.key).map(|d| {
                    // **Which of `hops` and degree leads depends on whether we can see the door**,
                    // because the two rules were written for two different errands and only look
                    // like they contradict.
                    //
                    // *Crossing to a known exit.* Nearest first, so the search expands outward from
                    // where we stand and `doorward` chooses which branch at each fork — the dev's
                    // *don't backtrack until our current branch is done*. Degree decides between
                    // nodes equally near and equally doorward.
                    //
                    // *Searching for something the fog hides* — an inn we have not found, with no
                    // position to head for. Degree first, which is the rule of 2026-08-15 and the
                    // village that *got searched a cul-de-sac at a time*. `doorward` is `u64::MAX`
                    // for every candidate here, so it drops out and this is exactly the key that
                    // rule was given.
                    //
                    // Inverted rather than wrapped in `Reverse` so both orderings are one type.
                    let near = *d as u64;
                    let teaches = u64::MAX - unrevealed as u64;
                    let (lead, trail) =
                        match door_now.is_some() { true => (near, teaches), false => (teaches, near) };
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
            p.parent.as_deref() == Some(container)
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
    pub(super) fn errand_inside(&self, container: &str) -> Option<&Place> {
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
        self.places
            .values()
            .filter(|p| p.parent.as_deref() == Some(container))
            .filter(|p| p.is_shrine() && !p.used && !p.avoid)
            .filter(|p| !self.abandoned.contains(&p.key))
            .min_by_key(|p| (dist_or_far(&dist, &p.key), p.key.clone()))
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
            .filter(|p| p.parent.as_deref() == Some(container))
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
    /// Which of the two reasons a [`Crossing::Seek`] has no destination, for the log to say so.
    ///
    /// `true` is the errand — a village whose inn the fog still hides. `false` is the crossing — an
    /// exit we cannot see yet, which in a lost woods is every exit.
    pub fn seeking_an_inn(&self, container: &str) -> bool {
        self.seeking_a_rest(container)
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

    fn seeking_a_rest(&self, container: &str) -> bool {
        if !self.wants_a_bed() || self.gold < crate::rest::INN_COST {
            return false;
        }
        if !self.places.get(container).map(|p| p.has_an_inn() && p.trades()).unwrap_or(false) {
            return false;
        }
        !self
            .places
            .values()
            .any(|p| p.parent.as_deref() == Some(container) && p.is_inn() && self.abandoned.contains(&p.key))
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
    /// ## Uncorrupted, because corruption brings the fog back
    ///
    /// The dev, 2026-08-17: *once a settlement is corrupted, the thick fog re-appears, but if that
    /// hasn't happened, then there is no thick fog to guard against.* Applied to every subworld and
    /// not only settlements, because the mechanism below is not about settlements.
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
            .map(|c| !c.corrupted && !c.in_lost_woods)
            .unwrap_or(false);
        if !clear_air {
            return None;
        }
        self.far_chain(from, to, &|p: &Place| self.blocks_departure(&p.key))
    }
}
