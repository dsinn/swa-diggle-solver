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

/// Plans one player turn from a combat save. `None` when the save carries no board.
///
/// `peak_health` is the caller's per-fight record of the highest health seen per enemy name: the
/// enemy's maximum is not in the save, and the flee thresholds are defined against it.
pub fn plan(
    cs: &Table, game_dir: &Path, letters: &crate::letters::Weights,
    peak_health: &mut HashMap<String, i64>,
) -> Result<Option<TurnPlan>, crate::Error> {
    let mut notes = Vec::new();
    let tiles = tiles_of(cs);
    if tiles.is_empty() {
        return Ok(None);
    }
    let health = cs.int_at("rpg.enemy.health").unwrap_or(0);
    let armour = cs.int_at("rpg.enemy.armour").unwrap_or(0);
    let name = cs.str_at("rpg.enemy.name").unwrap_or("?").to_string();
    let (mods, geom) = Modifiers::from_save(&game_dir, cs, tiles.len())?;
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
    // Printed rather than acted on. The halfling's `immuneTurnMod2` is the only thing that sets
    // it, it is a per-turn fact that leaves no other trace in a log, and the case it unlocks —
    // spending a free turn rather than committing an avoidable murder — needs a goal that does
    // not exist yet. See [`crate::parity`] and `search::MIN_MEANINGFUL_DAMAGE`.
    if mods.player_dodges {
        notes.push("  the enemy cannot land a hit this turn (immuneTurnMod2)\n".to_string());
    }
    if mods.enemy_status_tick != 0 {
        notes.push(format!(
            "  {name} loses {} to status before acting; aiming against {deciding_health}hp
",
            mods.enemy_status_tick
        ));
    }
    let armour_room = {
        let now = cs.int_at("rpg.player.armour");
        let max = cs.int_at("rpg.player.maxHealth").map(|m| {
            if cs.path("rpg.player.gearFlags.maxArmourHalved").is_some() {
                m / 2
            } else {
                m
            }
        });
        match (now, max) {
            (Some(a), Some(m)) => a < m,
            _ => true,
        }
    };
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
    );
    let goal = Goal::for_enemy(&mods, deciding_health, armour, Some(peak), player.as_ref(), prefs);
    // Built per turn rather than per fight, because the board's tile count is what the target is
    // scaled to and a board can lose tiles mid-fight — the run that died at `l50` watched its
    // board collapse from 16 tiles to one. A target built once at 16 would have been describing a
    // board that no longer existed.
    // Room for more armour, which is what decides whether `onWoodKillGainArmour` is worth
    // constraining a word for. The cap is `maxHealth / (maxArmourHalved and 2 or 1)`
    // (`overworld.lua:18`), read from the same `combatSaveData` the gear flags come from.
    //
    // Unknown counts as **room available**, the opposite of how unknown health is treated when
    // deciding whether to fight. The asymmetry is deliberate: guessing wrong about health can
    // end the save, while guessing wrong here costs a slightly worse word.
    let picking = crate::pick::Context { target: letters.target(tiles.len()), prefs };
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
        notes.push(format!(
            "  {name} can be scared off ({:?}); aiming for {need}..={} damage, not a kill\n",
            mods.nerve,
            below - 1
        ));
    }
    // Its own line, because for one run this band borrowed the one above and the log said an
    // enemy with no nerve at all `can be scared off (None)` while we tried to kill it.
    if let Goal::FrugalKill { need, below } = goal {
        notes.push(format!(
            "  killing {name} frugally: {need}..={} damage keeps the rest charge\n",
            below - 1
        ));
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
pub fn tiles_of(save: &Table) -> Vec<crate::observe::board::Tile> {
    save.table_at("tileboard").map(crate::observe::board::tiles_from).unwrap_or_default()
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

    // The charge lives in statusEffects; the gear flag grants the same heal permanently
    // (`rpgview.lua:1082-1083` accepts either source).
    // **The stack count, not merely the presence.** `affectPlayerStatus` keeps these as numbers and
    // the HUD draws them as `×4`; reading them with `is_some` threw that away, and the policy above
    // was left unable to tell one charge from four. A status present but unreadable as a number
    // counts as one, which is the old behaviour and the safe direction — it hoards.
    let count = |k: &str| match status(k) {
        true => cs.int_at(&format!("rpg.player.statusEffects.{k}")).unwrap_or(1).max(1),
        false => 0,
    };
    let charges = count("wellRestedCampfire") + count("wellRestedInn");
    let consumes_charge = charges > 0;
    let granted = consumes_charge || gear("wellRestedCampfire") || gear("wellRestedInn");
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
        consumes_charge,
        charges,
        bleeding: status("bleed"),
    })
}

#[cfg(test)]
mod tests {
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
            let plan = plan(&cs, &game_dir(), &letters, &mut peaks).unwrap().expect(name);
            assert!(!plan.tiles.is_empty(), "{name}");
            assert_ne!(plan.name, "?", "{name}");
            assert_eq!(peaks.get(&plan.name), Some(&plan.health), "{name}");
            assert!(plan.first_sighting, "{name}");
        }
    }

    /// A board-less save is not a turn to plan.
    #[test]
    fn an_empty_board_has_no_plan() {
        let letters = crate::letters::Weights::load(&game_dir()).unwrap();
        let cs =
            crate::game::save::parse("return { rpg = { enemy = { name = 'x', health = 3 } } }")
                .unwrap();
        let mut peaks = std::collections::HashMap::new();
        assert!(plan(&cs, &game_dir(), &letters, &mut peaks).unwrap().is_none());
    }
}
