//! A turn's decision, built from `combatSaveData` alone.
//!
//! Extracted from `Fight::play_turn` so that two callers make the decision the same way: Diggle,
//! which then types the word, and the advisor (`src/bin/diggle_advisor.rs`), which only shows it.
//! Everything here reads the save and the game checkout and nothing else — no console, no screen
//! — which is what lets the advisor share it. The one input Diggle adds afterwards, the murder
//! backoff, is fed by a console line and so stays in [`crate::fight`].

use crate::game::save::Table;
use crate::geometry::Geometry;
use crate::observe::board::Tile;
use crate::search::{Goal, Modifiers, PlayerState};
use std::collections::HashMap;
use std::path::Path;

/// A turn's decision, before anything Diggle-only (the murder backoff) adjusts it.
pub struct TurnPlan {
    pub tiles: Vec<Tile>,
    pub geom: Geometry,
    pub mods: Modifiers,
    pub goal: Goal,
    pub picking: crate::pick::Context,
    pub player: Option<PlayerState>,
    pub name: String,
    pub health: i64,
    pub armour: i64,
    /// Health after the status already on the enemy ticks — what every goal aims against.
    pub deciding_health: i64,
    /// The highest health seen for this enemy, which stands in for its maximum.
    pub peak: i64,
    /// This save is the first this enemy has been seen in, so `peak` is only its current health.
    pub first_sighting: bool,
    /// The log lines this planning produces, in the order Diggle has always printed them.
    pub notes: Vec<String>,
}

/// What the caller wants of a turn's decision beyond what the save says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// **On a turn the enemy cannot hit back, keep an enemy that could run: neither scare nor kill.**
    ///
    /// The halfling's `immuneTurnMod2` is what makes such a turn (see [`crate::parity`]); the Turn
    /// box on screen calls them its even turns. When this is set, an enemy short of its flee
    /// threshold is hurt only as far as keeps it here, or not at all ([`crate::search::Goal::Hold`]);
    /// one already past it, which any hit would kill, is dealt nothing. See
    /// [`crate::search::Modifiers::stall`] for exactly when.
    ///
    /// Off, the turn is decided as it was before this existed: usually a scare, or the kill.
    pub stall_on_free_turns: bool,
}

impl Default for Options {
    /// Stalling on: Diggle's setting, and the advisor's until the user unticks it.
    fn default() -> Self {
        Options { stall_on_free_turns: true }
    }
}

/// Plans one player turn from a combat save. `None` when the save carries no board.
///
/// `peak_health` is the caller's per-fight record of the highest health seen per enemy name: the
/// enemy's maximum is not in the save, and the flee thresholds are defined against it.
pub fn plan(
    cs: &Table, game_dir: &Path, letters: &crate::letters::Weights,
    peak_health: &mut HashMap<String, i64>, options: Options,
) -> Result<Option<TurnPlan>, crate::Error> {
    let mut notes = Vec::new();
    let tiles = tiles_of(cs);
    if tiles.is_empty() {
        return Ok(None);
    }
    let health = cs.int_at("rpg.enemy.health").unwrap_or(0);
    let armour = cs.int_at("rpg.enemy.armour").unwrap_or(0);
    let name = cs.str_at("rpg.enemy.name").unwrap_or("?").to_string();
    let (mut mods, geom) = Modifiers::from_save(game_dir, cs, tiles.len())?;
    for p in &mods.problems {
        notes.push(format!("  WARNING {p}\n"));
    }

    // The enemy's maximum health is not in the save (`rpgview.lua:2513-2522` lists what is), and
    // the flee thresholds are all defined against it. The highest health we have seen this
    // enemy at is that maximum, because we start fights from the pregame with a fresh enemy and
    // health only falls within a fight. Keyed by name, so the next enemy in a queue starts its
    // own record rather than inheriting the last one's.
    let first_sighting = !peak_health.contains_key(&name);
    let peak = peak_health.entry(name.clone()).or_insert(health);
    *peak = (*peak).max(health);
    let peak = *peak;

    // Overkill pays gold against a mimic or under a player curse, and the excess IS the reward,
    // so that fight wants the hardest hit rather than the quickest kill. Otherwise, an enemy
    // that can be frightened is frightened rather than killed.
    let player = player_state(cs, &mods);
    // **Health after the status already on it ticks**, not the health the save reports.
    //
    // The question every goal is really asking is whether the enemy gets to act, because most
    // enemy turns hurt us. The game asks the same question the same way: `enemyCanHit`
    // (`rpgview.lua:1046-1052`) is gated on the post-status health that also decides
    // `attackEstimatedToCauseEnemyDeath` (`:1045`). An enemy on 5 that is bleeding is, for our
    // purposes, an enemy on 4.
    //
    // This is also what a scare ceiling has to respect: a word inside the direct-damage band
    // still kills if the status finishes the job, which is how a live run collected thirty
    // avoidable-murder refusals for words its own arithmetic called safe.
    let deciding_health = (health - mods.enemy_status_tick).max(0);
    // The same health and armour the goal is built from, so a rapier word is measured against the
    // thresholds it will actually meet — see `search::Modifiers::reach`.
    mods.defence = Some(crate::search::Defence { health: deciding_health, armour });
    if armour > 0 && mods.weapon.holds_rapier() {
        notes.push(format!(
            "  rapier: a word that starts and ends with the same letter ignores {name}'s {armour} armour
"
        ));
    }
    // Printed whenever it holds, because it is a per-turn fact that leaves no other trace in a
    // log. The halfling's `immuneTurnMod2` is the only thing that sets it. See [`crate::parity`].
    if mods.player_dodges {
        notes.push("  the enemy cannot land a hit this turn (immuneTurnMod2)\n".to_string());
    }
    // **Should this free turn end nothing?** Each condition is one of `search::Modifiers::stall`'s,
    // in the same order. Post-status health, because an enemy the status is about to finish dies
    // whatever we play.
    mods.stall = options.stall_on_free_turns && mods.player_dodges && deciding_health > 0;
    if mods.stall {
        notes.push(format!(
            "  a free turn: {name} is kept here, neither killed nor scared off, if the board allows\n"
        ));
    }
    if mods.enemy_status_tick != 0 {
        notes.push(format!(
            "  {name} loses {} to status before acting; aiming against {deciding_health}hp
",
            mods.enemy_status_tick
        ));
    }
    // Armour still to gain before the cap. Unknown counts as room — see the note on `picking`.
    let armour_headroom = {
        let now = cs.int_at("rpg.player.armour");
        let max = cs.int_at("rpg.player.maxHealth").map(|m| {
            // `math.ceil(maxHealth / 2)` (`rpgview.lua:66`, `overworld.lua:18`): rounded up, so
            // an odd maximum leaves one more armour to fill than `m / 2` says.
            if cs.path("rpg.player.gearFlags.maxArmourHalved").is_some() {
                (m + 1) / 2
            } else {
                m
            }
        });
        match (now, max) {
            (Some(a), Some(m)) => (m - a).max(0),
            _ => i64::MAX / 4,
        }
    };
    let armour_room = armour_headroom > 0;
    // **Built before the goal, because the goal now depends on it.** Whether the wood gear is
    // paying decides whether the Well-Rested heal is a floor or merely a rank — see
    // `search::Goal::killing_blow`, and `pick::Rank::heals_fully` for the four tiers.
    let prefs = crate::pick::Preferences::from_flags(
        |f| cs.path(&format!("rpg.player.gearFlags.{f}")).is_some(),
        // Any missing health counts as injured: the buckler heals 4, so at 19/20 the payout is
        // real and merely smaller than its cap. No threshold is invented here.
        player.as_ref().map(|p| p.vitals.missing() > 0).unwrap_or(false),
        // Bleeding cancels the heal (`rpgview.lua:1080`), and with it the reason to chase a
        // wood-only word. Read from the same `PlayerState` the rested goals already use, so
        // there is one reading of the status per turn rather than two that could disagree.
        player.as_ref().map(|p| p.bleeding).unwrap_or(false),
        armour_room,
    )
    // The damage at which overkill closes the whole deficit: `lethal + 2 * missing`, since the
    // heal is `floor(overkill/2)` capped at what is missing (`rpgview.lua:1195-1196`). `None`
    // when there is no heal to collect -- no charge, cancelled by gear, or already full -- which
    // is exactly when it must not sway a ranking.
    .healing_at(
        player
            .as_ref()
            .filter(|p| p.heals && p.vitals.missing() > 0)
            .map(|p| deciding_health + armour + 2 * p.vitals.missing()),
    )
    // What braced wood and a Diamond played last can give — see `pick::gain`. Bleed is rounded up,
    // as the advisor shows it: any bleed at all stops regen.
    .with_room(crate::pick::Room {
        armour: armour_headroom,
        // Read from the save itself, not from `player`: that exists only with the Well-Rested heal,
        // and without it every hurt player read as unhurt — no Diamond heal counted, and Diamonds
        // kept as if at full health. The user's save, 2026-11-07: 48 of 56 read as 0 missing.
        health: missing_health(cs),
        bleed: cs
            .path("rpg.player.statusEffects.bleed")
            .and_then(|v| v.as_f64())
            .filter(|b| *b > 0.0)
            .map(|b| (b.ceil() as i64).max(1))
            .unwrap_or(0),
        // The Silver chain coif's armour per silver tile, on every attack — see `pick::gains`.
        silver_armour: cs
            .path("rpg.player.gearFlags.onAttackGainArmourWordSilverCount")
            .and_then(|v| v.as_f64())
            .map(|v| v as i64)
            .unwrap_or(0),
    })
    // The gauntlet's and the scales' infinitive tiles — see `pick::Infinitives`.
    .with_infinitives(
        crate::pick::Infinitives::from_gear(
            cs.get("gear")
                .and_then(|v| v.as_table())
                .into_iter()
                .flat_map(|t| t.arr.iter())
                .filter_map(|v| v.as_str()),
            |f| cs.path(&format!("rpg.player.gearFlags.{f}")).is_some(),
        )
        .with_upgrades(|f| {
            cs.path(&format!("rpg.player.gearFlags.{f}")).and_then(|v| v.as_f64()).unwrap_or(0.0)
                as i64
        }),
    )
    // The bassanello's wildcards per run of repeated letters — see `pick::wildcards_queued`.
    .with_wildcards_per_run(
        cs.path("rpg.player.gearFlags.onSubmitWoodQueueWoodornateWildcardPerRun")
            .and_then(|v| v.as_f64())
            .map(|v| v as i64)
            .unwrap_or(0),
    )
    // Express mail: the tile queue gains rank first — see `pick::Preferences::queue_first`.
    .with_queue_first(cs.path("rpg.player.gearFlags.submitContinuesTurn").is_some());
    let goal = Goal::for_enemy(&mods, deciding_health, armour, Some(peak), player.as_ref(), prefs);
    // Built per turn rather than per fight, because the board's tile count is what the target is
    // scaled to and a board can lose tiles mid-fight — the run that died at `l50` watched its
    // board collapse from 16 tiles to one. A target built once at 16 would have been describing a
    // board that no longer existed.
    // Room for more armour, which is what decides whether `onWoodKillGainArmour` is worth
    // constraining a word for. The cap is `ceil(maxHealth / (maxArmourHalved and 2 or 1))`
    // (`overworld.lua:18`), read from the same `combatSaveData` the gear flags come from.
    //
    // Unknown counts as **room available**, the opposite of how unknown health is treated when
    // deciding whether to fight. The asymmetry is deliberate: guessing wrong about health can
    // end the save, while guessing wrong here costs a slightly worse word.
    let picking = crate::pick::Context {
        target: letters.target(tiles.len()),
        prefs,
        gems: mods.gems.clone(),
    };
    Ok(Some(TurnPlan {
        tiles,
        geom,
        mods,
        goal,
        picking,
        player,
        name,
        health,
        armour,
        deciding_health,
        peak,
        first_sighting,
        notes,
    }))
}

/// The lines describing the goal actually played, printed after any adjustment to it.
pub fn goal_notes(cs: &Table, name: &str, mods: &Modifiers, goal: Goal) -> Vec<String> {
    let mut notes = Vec::new();
    if let Goal::Scare { need, below } = goal {
        // Inclusive in the log, half-open in the code. `race_for_band` filters
        // `score >= need && score < below` (`search.rs:838`), so `below` is a ceiling the word
        // must stay under — but "aiming for 4..6" reads in English as "6 is allowed", and it
        // sent a reader hunting for a bug in the aim when the aim was correct and only this
        // sentence was wrong.
        //
        // An empty band is only ever the stall's — see `search::Goal::for_enemy` — and "aiming for
        // 1..=0" would say nothing a reader could use.
        if need >= below {
            notes.push(format!(
                "  {name} cannot be hurt without dying ({:?}); dealing nothing, not a kill\n",
                mods.nerve
            ));
        } else {
            notes.push(format!(
                "  {name} can be scared off ({:?}); aiming for {need}..={} damage, not a kill\n",
                mods.nerve,
                below - 1
            ));
        }
    }
    if let Goal::Hold { below, lethal } = goal {
        let edges = match (mods.nerve, below == lethal) {
            (Some(n), false) => format!("under {below} damage keeps it short of its {n:?}"),
            _ => format!("under {below} damage keeps it alive"),
        };
        notes.push(format!("  {name} is held: {edges}; {lethal} or more kills it\n"));
    }
    // The enemy's statuses, whenever it has any.
    //
    // Recorded because a live murder warning could not be explained without them. The game's
    // estimate is of death *possibly by status* (`rpgview.lua:915`), and
    // `getStatusEffectHealthDeltasFor` (`:1027`) folds toxin, burn and bleed into the health it
    // predicts — none of which our arithmetic models. Without this line there is no way to tell
    // "the game scored the word higher than we did" from "the enemy was already dying".
    let statuses = crate::lexica::Lexica::statuses_from_save(cs);
    if !statuses.is_empty() {
        let mut shown: Vec<String> = statuses.iter().map(|(k, v)| format!("{k}={v}")).collect();
        shown.sort();
        notes.push(format!("  {name} statuses: {}\n", shown.join(" ")));
    }
    notes
}

/// Tiles as the combat save records them.
/// The board from `combatSaveData`.
///
/// Shares [`crate::observe::board::tiles_from`] with the console reader, because the two formats are
/// the same and the copy here did not know it. It did `filter_map(as_str)`, which silently dropped
/// every structured tile -- `{ "W", { bg = "wood" } }` -- shortening the board and shifting every
/// index after it.
///
/// A live fight showed the whole failure in one line. Sixteen tiles became fifteen, the solver chose
/// indices [4, 10, 2, 6, 12, 13, 8, 1, 14, 0] meaning NATURALITY on its shortened board, and the
/// game read those same indices against the real board and answered
/// `YDIWYRIYAW  not recognised`. Nothing was submitted, the board never changed, and the fight
/// stalled replaying the same word.
///
/// The dropped quality mattered too: a wood or gold tile scored as an ordinary letter, so even a
/// board that happened to parse to the right length would have been mis-scored.
///
/// ## The saved list is the board **and then the queue**
///
/// `getDataForSave` writes the grid column by column and then appends `letterQueue`, the tiles
/// waiting to drop in (`tileboard.lua:2448-2474`). Loading puts the whole list into the queue and
/// spawns the board from its front, `columns` heights when the board is ragged
/// (`tileboard.lua:2520-2537`). So the board is the first [`crate::geometry::Geometry::total_tiles`]
/// entries, which already counts those heights, and anything past that is not on the board. Read as
/// board, a live save's twelve queued tiles became a 28-tile "board" on a 4x4 shape: every word was
/// typed against tiles that were not there, and the hazard ranking could not place the `!`.
pub fn tiles_of(save: &Table) -> Vec<crate::observe::board::Tile> {
    let mut tiles =
        save.table_at("tileboard").map(crate::observe::board::tiles_from).unwrap_or_default();
    let on_board = crate::geometry::Geometry::from_save(save, tiles.len()).geometry.total_tiles();
    tiles.truncate(on_board);
    tiles
}

/// Health the player is missing: `rpg.player.maxHealth` less `rpg.player.health`, never below
/// zero, and zero when the save does not say.
pub fn missing_health(cs: &Table) -> i64 {
    match (cs.int_at("rpg.player.health"), cs.int_at("rpg.player.maxHealth")) {
        (Some(current), Some(max)) => (max - current).max(0),
        _ => 0,
    }
}

/// The player's side of the killing-blow decision, from `combatSaveData`.
///
/// Returns `None` when nothing about overkill matters, so the search keeps its ordinary goal.
pub fn player_state(
    cs: &Table, mods: &crate::search::Modifiers,
) -> Option<crate::search::PlayerState> {
    let current = cs.int_at("rpg.player.health")?;
    let max = cs.int_at("rpg.player.maxHealth")?;
    let status = |k: &str| cs.path(&format!("rpg.player.statusEffects.{k}")).is_some();
    let gear = |k: &str| cs.path(&format!("rpg.player.gearFlags.{k}")).is_some();

    // The stack lives in statusEffects; the gear flag grants the same heal permanently
    // (`rpgview.lua:1082-1083` accepts either source).
    let granted = ["wellRestedCampfire", "wellRestedInn"].iter().any(|k| status(k) || gear(k));
    if !granted {
        return None;
    }
    // Either the player's gear or the enemy's own status can cancel it (`:1084-1085`).
    let cancelled = gear("overkillNoHeal")
        || gear("overkillHealToGold")
        || mods.overkill_no_heal
        || mods.overkill_heal_to_gold;
    Some(crate::search::PlayerState {
        vitals: crate::rested::Vitals { current, max },
        heals: !cancelled,
        bleeding: status("bleed"),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::PathBuf;

    fn game_dir() -> PathBuf {
        PathBuf::from("../sternly-worded-adventures")
    }

    /// Every committed combat save plans a turn: a board, a named enemy, and the enemy's first
    /// sighting recorded as its peak.
    #[test]
    fn plan_reads_each_committed_fixture() {
        let letters = crate::letters::Weights::load(&game_dir()).unwrap();
        for name in ["combatSaveData-crypt-l0.lua", "combatSaveData-l1-depleted.lua"] {
            let src = std::fs::read_to_string(format!("tests/fixtures/{name}")).unwrap();
            let cs = crate::game::save::parse(&src).unwrap();
            let mut peaks = std::collections::HashMap::new();
            let plan = plan(&cs, &game_dir(), &letters, &mut peaks, Options::default())
                .unwrap()
                .expect(name);
            assert!(!plan.tiles.is_empty(), "{name}");
            assert_ne!(plan.name, "?", "{name}");
            assert_eq!(peaks.get(&plan.name), Some(&plan.health), "{name}");
            assert!(plan.first_sighting, "{name}");
        }
    }

    /// The crypt fixture's enemy, renamed and given Turn undead's `terror` (`TurnUndead.lua:38-41`).
    fn terrified(name: &str) -> Goal {
        let src = std::fs::read_to_string("tests/fixtures/combatSaveData-crypt-l0.lua")
            .unwrap()
            .replacen("name = \"Amorphous\",", &format!("name = \"{name}\","), 1)
            .replacen("toxinImmune = -1,", "toxinImmune = -1,\n                terror = 1,", 1)
            .replacen(
                "health = 3,\n            attacksCycle",
                "health = 8,\n            attacksCycle",
                1,
            );
        let cs = crate::game::save::parse(&src).unwrap();
        let letters = crate::letters::Weights::load(&game_dir()).unwrap();
        plan(&cs, &game_dir(), &letters, &mut HashMap::new(), Options::default())
            .unwrap()
            .unwrap()
            .goal
    }

    /// The control: a terrified enemy that is not undead is scared, so the save does reach the
    /// scare path and the skeleton's kill below is not that path being unreachable.
    #[test]
    fn a_terrified_slime_is_scared() {
        assert!(
            matches!(terrified("Amorphous"), Goal::Scare { .. }),
            "{:?}",
            terrified("Amorphous")
        );
    }

    /// Turn undead terrifies the skeleton it hits, and that must not turn the fight into a scare:
    /// skeletons are always killed.
    #[test]
    fn a_skeleton_terrified_by_turn_undead_is_still_killed() {
        let goal = terrified("Bones McCoy");
        assert!(!matches!(goal, Goal::Scare { .. }), "{goal:?}");
    }

    /// Missing health is read from the save, Well-Rested or not. The user's save, 2026-11-07: a
    /// priest at 48 of 56 with no Well-Rested read as unhurt.
    #[test]
    fn missing_health_counts_without_well_rested() {
        let src = std::fs::read_to_string("tests/fixtures/combatSaveData-chest-chrysocolla.lua")
            .unwrap()
            .replacen("            health = 12,", "            health = 7,", 1);
        assert!(!src.contains("wellRested"), "the fixture has no Well-Rested");
        let cs = crate::game::save::parse(&src).unwrap();
        let letters = crate::letters::Weights::load(&game_dir()).unwrap();
        let plan = plan(&cs, &game_dir(), &letters, &mut HashMap::new(), Options::default())
            .unwrap()
            .unwrap();
        assert!(plan.player.is_none(), "no Well-Rested, so no player state");
        assert_eq!(plan.picking.prefs.room.health, 5, "12 - 7");
        assert_eq!(missing_health(&cs), 5);
        let bare = crate::game::save::parse("return {}").unwrap();
        assert_eq!(missing_health(&bare), 0, "a save that does not say");
    }

    /// A board-less save is not a turn to plan.
    #[test]
    fn an_empty_board_has_no_plan() {
        let letters = crate::letters::Weights::load(&game_dir()).unwrap();
        let cs =
            crate::game::save::parse("return { rpg = { enemy = { name = 'x', health = 3 } } }")
                .unwrap();
        let mut peaks = std::collections::HashMap::new();
        assert!(plan(&cs, &game_dir(), &letters, &mut peaks, Options::default())
            .unwrap()
            .is_none());
    }

    /// The crypt fixture as a halfling sees it: the enemy `terror`ised and down to 1 health, which
    /// any hit kills, and the player carrying `immuneTurnMod2` (or not) on save turn `save_turn`.
    ///
    /// Shared with the advisor's view tests, which drive the same save through the window's ranking.
    pub(crate) fn halfling_save(save_turn: i64, flag: bool) -> String {
        let src = std::fs::read_to_string("tests/fixtures/combatSaveData-crypt-l0.lua")
            .unwrap()
            .replace("\r\n", "\n");
        let out = src
            .replacen("toxinImmune = -1,", "toxinImmune = -1,\n                terror = 1,", 1)
            .replacen(
                "health = 3,\n            attacksCycle",
                "health = 1,\n            attacksCycle",
                1,
            )
            .replacen("turnNumber = 1,", &format!("turnNumber = {save_turn},"), 1);
        let out = match flag {
            true => out.replacen(
                "CampfireGivesWellRested = 1,",
                "CampfireGivesWellRested = 1,\n                immuneTurnMod2 = 1,",
                1,
            ),
            false => out,
        };
        assert_ne!(out, src, "the fixture changed shape; these edits no longer land");
        out
    }

    /// [`halfling_save`] on the free turn with a `fear` enemy at its full 12 health: 7 damage would
    /// frighten it off, so there is room to hurt it and keep it.
    pub(crate) fn held_save() -> String {
        halfling_save(2, true).replacen("terror = 1,", "fear = 1,", 1).replacen(
            "health = 1,\n            attacksCycle",
            "health = 12,\n            attacksCycle",
            1,
        )
    }

    /// **The hold, from the save.** A frightened enemy short of its threshold on the halfling's even
    /// turn is kept here rather than scared off; on the next turn, which it can hit on, it is
    /// scared as usual.
    #[test]
    fn a_halfling_holds_a_frightened_enemy_on_a_free_turn_and_scares_it_on_the_next() {
        let letters = crate::letters::Weights::load(&game_dir()).unwrap();
        let goal = |src: &str, stall: bool| {
            let cs = crate::game::save::parse(src).unwrap();
            plan(
                &cs,
                &game_dir(),
                &letters,
                &mut HashMap::new(),
                Options { stall_on_free_turns: stall },
            )
            .unwrap()
            .unwrap()
            .goal
        };
        let free = held_save();
        assert_eq!(goal(&free, true), Goal::Hold { below: 6, lethal: 11 });
        assert_eq!(goal(&free, false), Goal::Scare { need: 8, below: 11 }, "unticked: as before");
        let next = free.replacen("turnNumber = 2,", "turnNumber = 3,", 1);
        assert_ne!(next, free);
        assert_eq!(goal(&next, true), Goal::Scare { need: 8, below: 11 }, "the turn it can hit on");
    }

    /// Plans [`halfling_save`] for an enemy first seen at 8 health, so its flee threshold is known.
    fn halfling_plan(save_turn: i64, flag: bool, stall: bool) -> TurnPlan {
        let cs = crate::game::save::parse(&halfling_save(save_turn, flag)).unwrap();
        let letters = crate::letters::Weights::load(&game_dir()).unwrap();
        let mut peaks = HashMap::from([("Amorphous".to_string(), 8)]);
        plan(&cs, &game_dir(), &letters, &mut peaks, Options { stall_on_free_turns: stall })
            .unwrap()
            .unwrap()
    }

    /// **The stall, from the save.** Save turn 2 is the Turn box's 2 and the console's 1: the
    /// halfling's free turn ([`crate::parity`]). Any hit kills the 1-health enemy, so the goal is
    /// the empty band that ranks a word dealing nothing first.
    #[test]
    fn a_halfling_on_a_free_turn_stalls_rather_than_killing_a_terrified_enemy() {
        let p = halfling_plan(2, true, true);
        assert!(p.mods.player_dodges, "the parity read is the precondition");
        assert!(p.mods.stall);
        assert_eq!(p.goal, Goal::Scare { need: 1, below: 1 });
        assert!(
            p.notes.iter().any(|n| n.contains("neither killed nor scared off")),
            "{:?}",
            p.notes
        );
    }

    /// The saved tile list is the board and then the queue (`tileboard.lua:2468-2474`), so tiles
    /// past the board's size are not on it.
    #[test]
    fn the_saved_queue_is_not_read_as_board() {
        let src = std::fs::read_to_string("tests/fixtures/combatSaveData-crypt-l0.lua").unwrap();
        let cs = crate::game::save::parse(&src).unwrap();
        let board = tiles_of(&cs);
        assert_eq!(board.len(), 16, "the fixture is a full 4x4 board");
        let queued = src.replacen("    tileboard = {", "    tileboard = {\n", 1);
        let n = queued.find("    tileboard = {\n").unwrap() + "    tileboard = {\n".len();
        let close = n + queued[n..].find("\n    },").unwrap();
        let with_queue =
            format!("{}\n        \"!\",\n        \"Q\",{}", &queued[..close], &queued[close..]);
        let cs = crate::game::save::parse(&with_queue).unwrap();
        assert_eq!(tiles_of(&cs).len(), 16, "the two queued tiles are left out");
        let same = tiles_of(&cs).iter().zip(&board).all(|(a, b)| a.letter == b.letter);
        assert!(same, "and the board itself is untouched");
    }

    /// Unticked, the same turn is planned exactly as before the option existed: the kill.
    #[test]
    fn with_stalling_off_the_free_turn_kills_as_it_always_did() {
        let p = halfling_plan(2, true, false);
        assert!(p.mods.player_dodges, "still a free turn, and still reported as one");
        assert!(!p.mods.stall);
        assert!(matches!(p.goal, Goal::RankedKill { .. }), "{:?}", p.goal);
    }

    /// Only the free turn. Save turn 3 is one the enemy can hit on, and a champion without the
    /// passive has no free turns at all.
    #[test]
    fn no_stall_on_a_turn_the_enemy_can_hit_or_without_the_passive() {
        for (turn, flag) in [(3, true), (1, true), (2, false)] {
            let p = halfling_plan(turn, flag, true);
            assert!(!p.mods.stall, "save turn {turn}, flag {flag}");
            assert!(matches!(p.goal, Goal::RankedKill { .. }), "{turn}/{flag}: {:?}", p.goal);
        }
    }

    /// **An enemy that cannot run is held too.** The user's live case, 2026-11-04: John Skeleton on
    /// the halfling's even turn, every top row a kill. Killing it on the free turn hands the next
    /// enemy a turn it can hit on, exactly as scaring it would.
    #[test]
    fn a_free_turn_holds_an_enemy_that_cannot_run_rather_than_killing_it() {
        let letters = crate::letters::Weights::load(&game_dir()).unwrap();
        let goal = |src: &str, stall: bool| {
            let cs = crate::game::save::parse(src).unwrap();
            let p = plan(
                &cs,
                &game_dir(),
                &letters,
                &mut HashMap::new(),
                Options { stall_on_free_turns: stall },
            )
            .unwrap()
            .unwrap();
            assert!(p.mods.nerve.is_none(), "the fixture must have no nerve to test this");
            (p.mods.stall, p.goal)
        };
        // No `terror`, and 12 health: anything under 11 keeps it standing.
        let src = halfling_save(2, true).replacen("\n                terror = 1,", "", 1).replacen(
            "health = 1,\n            attacksCycle",
            "health = 12,\n            attacksCycle",
            1,
        );
        assert_eq!(goal(&src, true), (true, Goal::Hold { below: 11, lethal: 11 }));
        let (stall, unticked) = goal(&src, false);
        assert!(!stall && matches!(unticked, Goal::RankedKill { .. }), "{unticked:?}");
        // And the skeleton itself, by name: undead lose their nerve, and are held all the same.
        let bones = src.replacen("name = \"Amorphous\",", "name = \"John Skeleton\",", 1);
        assert_ne!(bones, src);
        assert_eq!(goal(&bones, true), (true, Goal::Hold { below: 11, lethal: 11 }));
    }
}
