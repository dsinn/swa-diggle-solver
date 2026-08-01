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
    /// How many neighbours the game refused to describe, from the dump taken **at** this node.
    /// `None` until we have stood here — a neighbour's dump cannot tell us this.
    pub hidden: Option<usize>,
    /// The subworld this sits inside, if any. Anomaly eligibility turns on this being `None`.
    pub parent: Option<String>,
    /// Have we stood on it and taken a dump?
    pub visited: bool,
    /// From `mainSaveData.overworld.completedAreas`.
    pub completed: bool,
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

    pub fn type_is(&self, type_name: &str) -> bool {
        self.heading.trim_end().ends_with(type_name)
    }

    /// Would arriving here open the anomaly?
    ///
    /// `overworld/events/arrived/world_evil.lua:15-21` requires a node with **no parent** (not
    /// inside a subworld), with combat, of level **> 3**. The remaining conditions in that check —
    /// `node_has_no_followups`, the `hell` flag, and a heretic/blood-curse exclusion — are not
    /// properties of the node's heading, so they are answered by [`WorldMap::anomaly_available`]
    /// and by the run itself rather than guessed at here.
    pub fn triggers_anomaly(&self) -> bool {
        self.parent.is_none() && self.level().unwrap_or(0) > 3
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
    /// Where the last dump said we were.
    here: Option<String>,
    /// `areaFlags.hell` — zero means the anomaly has not opened and the trigger is still live.
    hell: Option<i64>,
}

/// Where to head, and why. The reason is carried so a route can be explained rather than just taken.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub target: String,
    pub reason: Goal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Goal {
    /// The anomaly is open and this is it. Everything else can wait.
    Anomaly,
    /// A level > 3 surface node: arriving opens the anomaly.
    OpenTheAnomaly,
    /// Somewhere unseen, to grow the map toward one of the above.
    Explore,
}

impl WorldMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn here(&self) -> Option<&str> {
        self.here.as_deref()
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

    /// Has the anomaly already opened? `None` when no save has been read yet.
    pub fn anomaly_available(&self) -> Option<bool> {
        self.hell.map(|h| h == 0)
    }

    /// Folds one adjacency dump into the map.
    ///
    /// Everything here is additive except the fields a dump is authoritative for. A neighbour's
    /// entry is created if unseen, but its `hidden` count and `visited` flag are left alone: only a
    /// dump taken *at* a node can speak to those.
    pub fn fold(&mut self, a: &Adjacency) {
        let parent = a.subworld.as_ref().map(|(k, _)| k.clone());
        self.here = Some(a.here_key.clone());

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
            if place.parent.is_none() {
                place.parent = p;
            }
            place.neighbours.insert(a.here_key.clone());
            self.entry(&a.here_key).neighbours.insert(n.key.clone());
        }

        // Exits lead OUT of the subworld, so what they name belongs to the parent world, not here.
        for e in &a.exits {
            let place = self.entry(&e.to_key);
            if place.heading.is_empty() {
                place.heading = e.to_heading.clone();
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
            for key in t.map.keys() {
                self.entry(key).completed = true;
            }
        }
        self.hell = save
            .table_at("overworld.areaFlags")
            .and_then(|f| f.get("hell"))
            .and_then(|v| v.as_int());
        if let Some(loc) = save.str_at("overworld.playerLocation") {
            self.here.get_or_insert_with(|| loc.to_string());
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
    pub fn next_target(&self) -> Option<Plan> {
        let here = self.here.as_deref().unwrap_or("");

        if let Some(p) = self.places.values().find(|p| p.type_is("anomaly") && !p.completed) {
            return Some(Plan { target: p.key.clone(), reason: Goal::Anomaly });
        }

        // Only worth aiming at while the trigger is unspent. `anomaly_available` is `None` before
        // any save has been read, and an unknown flag is treated as still available: the cost of
        // going there needlessly is a wasted trip, while skipping it could strand the run.
        if self.anomaly_available().unwrap_or(true) {
            let mut candidates: Vec<&Place> = self
                .places
                .values()
                .filter(|p| p.key != here && p.triggers_anomaly())
                .collect();
            // Any qualifying node does; prefer one we have not cleared, then take the lowest level
            // for a predictable arrival, then by key so the choice is stable across runs.
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

        // Nothing qualifying is known yet, so grow the map. Unvisited places first — standing on one
        // is what produces a dump — and among those prefer the ones still hiding neighbours.
        let mut frontier: Vec<&Place> =
            self.places.values().filter(|p| p.key != here && p.is_frontier()).collect();
        frontier.sort_by(|a, b| {
            a.visited
                .cmp(&b.visited)
                .then(b.hidden.unwrap_or(0).cmp(&a.hidden.unwrap_or(0)))
                .then(a.key.cmp(&b.key))
        });
        frontier.first().map(|p| Plan { target: p.key.clone(), reason: Goal::Explore })
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
        m.fold(&dump("start", "camp", vec![node("l4", "Grim Barrow — level 4 crypt")]));
        m.hell = Some(1); // the anomaly already opened
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Explore, "chasing a spent trigger wastes the run");
    }

    #[test]
    fn the_anomaly_itself_outranks_everything() {
        let mut m = WorldMap::new();
        m.fold(&dump(
            "start",
            "camp",
            vec![node("l4", "Grim Barrow — level 4 crypt"), node("hp", "The Rift anomaly")],
        ));
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Anomaly);
        assert_eq!(plan.target, "hp");
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
        m.hell = Some(1);
        // Everything is visited, nothing hides neighbours, the anomaly is spent: no target.
        assert_eq!(m.next_target(), None, "walking on would be pointless, and saying so is the answer");
    }
}
