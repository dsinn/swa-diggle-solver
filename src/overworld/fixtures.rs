//! Worlds to test against, shared by every module under `overworld`.
//!
//! Split out on 2026-08-21 (#76), and it had to land before any test could follow its code: these
//! builders were used across the whole of `overworld.rs`'s 204 tests, so splitting the tests without
//! them first would have meant a copy of `dump` and `node` in every file — and a fixture that
//! disagrees with its twin is worse than a long file.
//!
//! ## These are not neutral scaffolding
//!
//! Several encode a fact about the game that a fixture is easy to get wrong, and the comments on
//! them say which. [`inside_dump`] gives a container the heading the game would really print for it,
//! because `fold` believes the container's live heading over anything learned from the surface —
//! that is how a lost woods announces itself, and handing a village heading to a forest quietly
//! disabled the rule. [`ready_for_the_anomaly`] exists because the consecration gate would otherwise
//! turn every anomaly fixture into a test of the gate, passing or failing for a reason it never
//! mentions.
//!
//! A fixture that builds a state play cannot reach proves nothing; see `HANDOFF.md`, *reachable
//! states, not just source*.

#![cfg(test)]

use super::*;
use crate::observe::adjacency::{Exit, Node};

pub(super) fn node(key: &str, heading: &str) -> Node {
    Node { key: key.into(), heading: heading.into(), x: 0.0, y: 0.0, connections: 2 }
}

/// Enough consecrated major shrines that [`SHRINES_BEFORE_THE_ANOMALY`] is satisfied.
///
/// Fixtures whose subject is the anomaly — its rank against other goals, steering toward it,
/// routing to it — say this explicitly. The gate added on 2026-08-20 would otherwise turn every
/// one of them into a test of the gate, passing or failing for a reason they never mention.
///
/// Keyed at 91 and up so it cannot collide with a shrine a fixture names itself; the game's own
/// rule is `key:sub(1,6)=='shrine'` followed by digits (`overworld/generators/world.lua:87-90`),
/// which these satisfy.
pub(super) fn ready_for_the_anomaly(m: &mut WorldMap) {
    for i in 91..91 + SHRINES_BEFORE_THE_ANOMALY {
        let p = m.entry(&format!("shrine{i}"));
        p.consecrated = true;
        // **And prayed at**, which is what makes them finished rather than merely blessed.
        // `used` is the game's `<key>_used`, set by praying, and `worth_a_trip` reads
        // `!used` — so a consecrated shrine that had never been prayed at would still be a
        // live errand, and these fixtures would head for one instead of doing what they test.
        p.used = true;
    }
}

pub(super) fn node_at(key: &str, heading: &str, x: f64, y: f64) -> Node {
    Node { key: key.into(), heading: heading.into(), x, y, connections: 2 }
}

pub(super) fn dump(here: &str, heading: &str, nodes: Vec<Node>) -> Adjacency {
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
///
/// The container's own heading is **not** a free choice: `fold` now believes it over anything
/// learned from the surface, because that is how a lost woods announces itself. So the two
/// containers these tests use get the headings the game would really print for them. Handing
/// `l9` the village heading — which this helper did for every parent until the day `fold`
/// started listening — made a forest answer `seeking_a_rest`, and two crossing tests changed
/// their answers as soon as the lie stopped being discarded.
pub(super) fn container_heading(parent: &str) -> &'static str {
    match parent {
        "l9" => "Saltagh Park — level 1 forest",
        _ => "Ulrome — level 6 village",
    }
}

pub(super) fn inside_dump(
    parent: &str, here: &str, heading: &str, nodes: Vec<Node>, exits: Vec<Exit>,
) -> Adjacency {
    Adjacency {
        subworld: Some((parent.into(), container_heading(parent).into())),
        exits,
        ..dump(here, heading, nodes)
    }
}

pub(super) fn exit(to: &str) -> Exit {
    Exit { x: 0.0, y: 0.0, to_key: to.into(), to_heading: format!("{to} heading") }
}

/// start branches two ways to the goal: a short way through `woods`, a long way round.
pub(super) fn two_routes() -> WorldMap {
    let mut m = WorldMap::new();
    m.fold(&dump("start", "camp", vec![node("woods", "Mistwood forest"), node("a", "a meadow")]));
    m.fold(&dump(
        "woods",
        "Mistwood forest",
        vec![node("start", "camp"), node("goal", "Grim Barrow — level 4 crypt")],
    ));
    m.fold(&dump("a", "a meadow", vec![node("start", "camp"), node("b", "b meadow")]));
    m.fold(&dump(
        "b",
        "b meadow",
        vec![node("a", "a meadow"), node("goal", "Grim Barrow — level 4 crypt")],
    ));
    m.here = Some("start".into());
    m
}

/// Real headings from the captured island: a campfire and a village both adjacent.
pub(super) fn hurt_at_l1() -> WorldMap {
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
    m.note_health(
        crate::rest::Health { current: 12, max: 12 },
        crate::rest::Health { current: 7, max: 12 },
    );
    // Enough for a bed and well under `HEART_FLOOR`, so the errand under test is the only one
    // live. The campfire in this fixture is deliberately kept: it is the real island's shape, and
    // since `CAMPFIRE_REST_IS_BUILT` went false it is also the thing that must *not* be chosen.
    m.gold = 50;
    m
}

/// Exploring the dark must not step out of the subworld by the wrong door.
/// Saltagh Park's shape: walk in at a crossroads with a road out either side, and explore both
/// arms. Returns the map standing back at the crossroads with the whole interior mapped.
///
/// The doors are asymmetric on purpose. `l19` is a **campfire**, so it becomes a rest site the
/// moment health drops — which is how a genuine change of errand gets expressed without
/// inventing one — while `l1` is an ordinary village, which at zero gold is no rest at all
/// (`can_rest_at` for an inn is a flat gold check).
///
/// Both are `Risk::Free`, so the unmeasurable-distance fallback ranks them by key and `l1` wins.
/// That is deliberate: it means the *committed* door and the *rest* door are different ones, so
/// a test that expects the errand to change the door is testing something.
pub(super) fn a_forest_with_two_doors() -> (WorldMap, Exit, Exit) {
    let dane = Exit { x: 0.0, y: 0.0, to_key: "l19".into(), to_heading: "Dane campfire".into() };
    let cowlam = Exit { x: 0.0, y: 0.0, to_key: "l1".into(), to_heading: "Cowlam village".into() };
    let both = vec![dane.clone(), cowlam.clone()];
    let mut m = WorldMap::new();
    m.fold(&dump("start", "The Wold portal", vec![node("l9", "Saltagh Park — level 1 forest")]));
    m.fold(&inside_dump(
        "l9",
        "l9sub0",
        "Saltagh Park crossroads",
        vec![node("l9sub1", "Saltagh Park road"), node("l9sub2", "Saltagh Park road")],
        both.clone(),
    ));
    m.fold(&inside_dump(
        "l9",
        "l9sub1",
        "Saltagh Park road",
        vec![node("l9sub0", "Saltagh Park crossroads"), node("l9_path_to_l19", "Road to Dane")],
        both.clone(),
    ));
    m.fold(&inside_dump(
        "l9",
        "l9sub2",
        "Saltagh Park road",
        vec![node("l9sub0", "Saltagh Park crossroads"), node("l9_path_to_l1", "Road to Cowlam")],
        both.clone(),
    ));
    m.fold(&inside_dump(
        "l9",
        "l9sub0",
        "Saltagh Park crossroads",
        vec![node("l9sub1", "Saltagh Park road"), node("l9sub2", "Saltagh Park road")],
        both.clone(),
    ));
    // Enough for a bed. The two doors are a campfire and a village, and since
    // `rest::CAMPFIRE_REST_IS_BUILT` went false only the village can answer a rest — so without
    // this the fixture has no rest errand at all and the tests below test nothing.
    m.gold = 50;
    (m, dane, cowlam)
}

/// The state the run of 2026-08-09 stopped in, rebuilt from `spike-run-raw.log:363-405`.
///
/// We knew `e1` from the surface as an ordinary forest, travelled onto it, and the arrival event
/// `Lost in the mists!` turned it into a lost woods. The dump taken one step later named three
/// neighbours and printed every exit as `Hidden location`, because `thickFog = true`.
pub(super) fn a_lost_woods() -> WorldMap {
    let mut m = WorldMap::new();
    m.fold(&dump(
        "l12",
        "Standing — level 2 crypt",
        vec![node("e1", "Howden Timberland — level 2 forest")],
    ));
    m.fold(&Adjacency {
        subworld: Some(("e1".into(), "Howden Timberland — level 2 lost woods".into())),
        exits: Vec::new(),
        hidden_exits: 3,
        ..dump(
            "e1_plaza",
            "Howden Timberland forest",
            vec![
                node("e1sub1", "Howden Timberland — level 2 chest"),
                node("e1sub2", "Howden Timberland forest"),
                node("e1sub3", "Howden Timberland crossroads"),
            ],
        )
    });
    m
}

/// Enthorpe, laid out on one line: two doors and three rooms between them.
///
/// The village of 2026-08-21, at the size that fits in a test. Everything is uncorrupted and
/// complete, which is what the run found and what makes a fast hop legal at all
/// ([`WorldMap::far_hop_inside`]).
///
/// ```text
///   world x:  500        600        700        800        900
///             l7 door    sub1       sub2       sub3(inn)  l1 door
/// ```
///
/// The camera pans left between dumps, which is the whole difficulty: every number the game
/// prints is `xoffset + posX*zoomMult` (`overworldview.lua:1033`), so no two dumps agree about
/// where anything is until something registers them against each other.
pub(super) fn enthorpe() -> WorldMap {
    let door = |to: &str, x: f64| Exit {
        x,
        y: 500.0,
        to_key: to.into(),
        to_heading: format!("Somewhere {to} crossroads"),
    };
    let room = |key: &str, heading: &str, x: f64| Node {
        key: key.into(),
        heading: heading.into(),
        x,
        y: 500.0,
        connections: 2,
    };
    // The doors as this dump draws them, given how far the camera has slid.
    let doors = |pan: f64| vec![door("l7", 500.0 - pan), door("l1", 900.0 - pan)];

    let mut m = WorldMap::new();
    // Walking in. Nothing is placed yet, so this dump *defines* the frame and its own numbers
    // are the frame's — which is why the world coordinates above are the ones it printed.
    m.fold(&inside_dump(
        "l32",
        "l32_path_to_l7",
        "road",
        vec![room("l32sub1", "Enthorpe house", 600.0)],
        doors(0.0),
    ));
    // One room in, camera 100 left. Three anchors: the door we are standing on, and both doors
    // from the exits section — which print at any distance and are what carry the frame.
    m.fold(&inside_dump(
        "l32",
        "l32sub1",
        "Enthorpe house",
        vec![
            room("l32_path_to_l7", "Somewhere l7 crossroads", 400.0),
            room("l32sub2", "Enthorpe house", 600.0),
        ],
        doors(100.0),
    ));
    // And another. This is the dump that learns where the inn is.
    m.fold(&inside_dump(
        "l32",
        "l32sub2",
        "Enthorpe house",
        vec![room("l32sub1", "Enthorpe house", 400.0), room("l32sub3", "Enthorpe inn", 600.0)],
        doors(200.0),
    ));
    m.apply_save(
        &crate::game::save::parse(
            "return { overworld = { areaFlags = { hell = 0.1 }, completedAreas = {
                 l32sub1 = true, l32sub2 = true, l32sub3 = true, l32_path_to_l7 = true } } }",
        )
        .unwrap(),
    );
    m
}

/// Inside Ulrome, hurt, with 763 gold — the sandbox's own state, one node further on.
///
/// `nodes` is what the dump reports as adjacent, so the inn is present or absent by the same
/// mechanism the fog uses.
pub(super) fn inside_a_village(here: (&str, &str), nodes: Vec<Node>, gold: i64) -> WorldMap {
    let mut m = WorldMap::new();
    m.fold(&dump("l19", "Gipsyville crypt", vec![node("l10", "Ulrome village")]));
    m.fold(&inside_dump("l10", here.0, here.1, nodes, vec![exit("l19"), exit("l7")]));
    m.apply_save(
        &crate::game::save::parse(&format!("return {{ player = {{ gold = {gold} }} }}")).unwrap(),
    );
    m.note_health_level(crate::rest::Health { current: 1, max: 20 });
    m
}
