//! The layer between a word's score and the damage it actually does.
//!
//! `words.score` is **not** damage. `rpgview.estimateDamage` passes the score through
//! `getWeaponBonusElementalDamageValues` (`rpgview.lua:868-912`) before comparing it to health, and
//! that function reads six gear flags which live nowhere near the modifier system.
//!
//! ## Why this matters in both directions
//!
//! - **`bonusBastard` removes 6 damage** from a seven-letter word. Ignoring it means a word we score
//!   at 20 lands 14, so a kill aimed at 20 leaves the enemy alive with a free turn. This is the one
//!   place gear makes us **over**-estimate, and `score.rs`'s stated "we can only under-rate"
//!   guarantee does not cover it.
//! - **The same flags add bleed and toxin**, which reach `estimates[currentEnemy]` and then
//!   `getStatusEffectHealthDeltasFor` (`rpgview.lua:1027`), deciding `enemySurvives` and so
//!   `attackEstimatedToCauseEnemyDeath` (`:1045`) — the avoidable-murder path.
//!
//! Neither is reachable from `rpg/effects/modifiers/`, so the gear table's parity test says nothing
//! about them. They are transcribed here by hand and cited line by line.
//!
//! ## The status model is deferred to post-MVP, as one decision
//!
//! What is modelled here is the single tick that decides **this** turn: does the enemy survive to
//! act? That is a damage question and it belongs in the MVP. Everything needing status tracked
//! *over* turns is deferred together rather than gap by gap — see [`deferred_status_model`].

use super::Gear;
use crate::observe::board::Tile;

/// What the weapon layer does to one word's attack.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct WeaponBonus {
    /// Added to the word's score to get direct damage. Negative for the flags that trade damage for
    /// status, which is all of the ones that touch it.
    pub damage_delta: i64,
    /// Stacks applied to the enemy by this attack.
    pub toxin: i64,
    pub burn: i64,
    pub bleed: i64,
}

/// Which of these flags the player has, resolved once per fight.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Weapon {
    bastard: bool,
    cei4toxin: bool,
    burning_tiles: bool,
    toxin_attack: i64,
    burn_attack: i64,
    bleed_attack: i64,
    /// `bonusMindfogToxin` is held but unmodelled — the `!` count includes the tile queue.
    pub mindfog_unmodelled: bool,
}

impl Weapon {
    /// Reads the flags from gear **and** the player's statuses.
    ///
    /// Unlike the `wordBonus` modifiers, three of these check a status effect as well as a gear flag
    /// (`rpgview.lua:892, 898, 904`), so the status half cannot be skipped here the way it is in
    /// [`Gear::from_save`].
    pub fn from_save(gear: &Gear, save: &crate::game::save::Table) -> Self {
        let status = |k: &str| save.int_at(&format!("rpg.player.statusEffects.{k}")).unwrap_or(0);
        let flag = |k: &str| gear.get(k).unwrap_or(0.0) as i64;
        Weapon {
            bastard: gear.get("bonusBastard").is_some(),
            cei4toxin: gear.get("bonusCEI4toxin").is_some(),
            burning_tiles: gear.get("bonusBurningTiles").is_some(),
            // `player.statusEffects.toxinAttack or gearFlag'toxinAttack'` — either grants it, and the
            // bonus is `#word` regardless of how many stacks (`:896`).
            toxin_attack: (status("toxinAttack") != 0 || flag("toxinAttack") != 0) as i64,
            burn_attack: (status("burnAttack") != 0 || flag("burnAttack") != 0) as i64,
            // Bleed is the exception: the two sources ADD rather than either granting it (`:905`).
            bleed_attack: status("bleedAttack") + flag("bleedAttack"),
            mindfog_unmodelled: gear.get("bonusMindfogToxin").is_some(),
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == Weapon::default()
    }

    /// The adjustment for one word.
    ///
    /// `letters` is the word's letter count, which is what the game tests — `#word` in Lua is bytes,
    /// and our words are ASCII.
    pub fn bonus_for(&self, word: &str, tiles: &[Tile]) -> WeaponBonus {
        let mut b = WeaponBonus::default();
        let letters = word.len() as i64;
        if self.bastard && letters == 7 {
            b.damage_delta -= 6;
            b.bleed += 3;
            b.toxin += 3;
        }
        if self.cei4toxin && cei_for_weapon(word) {
            b.damage_delta -= 4;
            b.toxin += 4;
        }
        if self.burning_tiles {
            // `words.burningTileCount` counts tiles in the WORD carrying `extra.burn`
            // (`utils/words.lua:206-213`) — not burning tiles on the board.
            b.burn += tiles.iter().filter(|t| t.burn().is_some()).count() as i64;
        }
        if self.toxin_attack != 0 {
            b.toxin += letters;
        }
        if self.burn_attack != 0 {
            b.burn += letters;
        }
        b.bleed += self.bleed_attack;
        b
    }
}

/// `word:find'CIE' or word:find'[^a-zC.]EI'` (`rpgview.lua:877`).
///
/// **Not the same pattern as `wordScoreBonusCIE`**, which uses `[^cC.]`. This one also excludes
/// every lowercase letter, and a lowercase letter is how the game spells a wildcard's chosen letter
/// (`utils/words.lua:151` counts bytes above 90 as wildcards). So an `EI` immediately after a
/// wildcard earns the word bonus but not this one.
fn cei_for_weapon(word: &str) -> bool {
    if word.contains("CIE") {
        return true;
    }
    let b = word.as_bytes();
    (1..b.len().saturating_sub(1)).any(|i| {
        b[i] == b'E'
            && b[i + 1] == b'I'
            && !b[i - 1].is_ascii_lowercase()
            && b[i - 1] != b'C'
            && b[i - 1] != b'.'
    })
}

/// The health an enemy loses to status at the start of its next turn.
///
/// This is the term that decides whether it acts at all. `startOfTurnStatusEffectsFor` runs
/// **before** `doStartOfTurnStatusDecayFor` (`rpgview.lua:1610-1612`), so freshly applied stacks pay
/// their full value before decaying, and `getStatusEffectHealthDeltasFor` routes the damage to
/// health unless `[status]Armour` is set (`:1297-1298`) — so it **bypasses armour**.
///
/// `applied` is what this attack adds, which is why the same word can be inside a scare band on
/// direct damage and still kill.
///
/// Per-status damage is `baseStatusDamage` (`:1260-1273`): bleed is a flat 1 whatever the stack
/// count, toxin is the stack count itself, burn is `ceil(maxHealth/16)`, ice is 1. `[status]Weak`
/// doubles each and `[status]Immune` zeroes it (`:1262-1284`), both of which are cheap enough to
/// model. `[status]Resist` is not: it subtracts a gear or status amount with a floor of `min(1,
/// base)`, and the amount lives on gear we do not read. See [`unmodelled_status`].
pub fn status_tick(
    enemy_statuses: &std::collections::HashMap<String, f64>, applied: WeaponBonus,
    max_health: Option<i64>,
) -> i64 {
    let stacks = |k: &str| enemy_statuses.get(k).copied().unwrap_or(0.0) as i64;
    let has = |k: &str| enemy_statuses.contains_key(k);
    let scale = |s: &str| if has(&format!("{s}Weak")) { 2 } else { 1 };
    let mut total = 0;
    if stacks("bleed") + applied.bleed > 0 && !has("bleedImmune") {
        // Flat, regardless of how many stacks are on it.
        total += scale("bleed");
    }
    if !has("toxinImmune") {
        // The count IS the damage, so existing stacks and ours add.
        total += (stacks("toxin") + applied.toxin).max(0) * scale("toxin");
    }
    if stacks("burn") + applied.burn > 0 && !has("burnImmune") {
        // Unknown maximum health means we cannot size this; treat it as the minimum rather than
        // inventing a number, which keeps the estimate on the cautious side for a kill.
        let divisor = if has("burnWeak") { 8.0 } else { 16.0 };
        total += max_health.map(|m| (m as f64 / divisor).ceil() as i64).unwrap_or(1);
    }
    if stacks("ice") > 0 && !has("iceImmune") {
        total += scale("ice");
    }
    total
}

/// What of the deferred status model is actually in play, as one line rather than several.
///
/// ## The deferral, and why it is one decision
///
/// **The dev's call:** status logic beyond the deciding tick is post-MVP, on complexity. Reporting
/// each piece separately made three unrelated-looking gaps out of one agreed boundary, and a turn
/// log full of "not modelled" reads like an oversight rather than a plan.
///
/// What sits behind the line:
///
/// - **`[status]Resist`.** Subtracts `entity.gearFlags[status..'Resist']` plus a status point,
///   floored at `min(1, base)` (`rpgview.lua:1282-1284`). The gear amount is not something we read,
///   so its size is unknown rather than merely fiddly.
/// - **`bonusMindfogToxin`**, the Mindfog dagger. It adds toxin per `!` tile counted across the
///   whole board *including the queue* (`tileboard.getLetterCount('!', true)`,
///   `tileboard.lua:317-329`), and the queue is not in the save. The dev's reasoning for letting this
///   one go: the build is strong for a human, who must find playable words under pressure and is glad
///   of damage that arrives without one — and worth much less to a solver that rapidly scans the
///   entire dictionary every turn and can usually just find the better word.
/// - **The status a word *applies*, over later turns.** The tick modelled here is only the next one.
///
/// Returned only when something is genuinely in play: a `bleedResist` on an enemy that is not
/// bleeding changes nothing, and a problem nobody can act on is noise.
pub fn deferred_status_model(
    enemy_statuses: &std::collections::HashMap<String, f64>, weapon: &Weapon,
) -> Option<String> {
    let mut live: Vec<String> = Vec::new();
    for s in ["bleed", "toxin", "burn", "ice"] {
        let key = format!("{s}Resist");
        let active = enemy_statuses.get(s).copied().unwrap_or(0.0) != 0.0;
        if active && enemy_statuses.contains_key(&key) {
            live.push(key);
        }
    }
    if weapon.mindfog_unmodelled {
        live.push("bonusMindfogToxin".into());
    }
    (!live.is_empty())
        .then(|| format!("status model deferred post-MVP; in play here: {}", live.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn bastard() -> Weapon {
        Weapon { bastard: true, ..Weapon::default() }
    }

    /// The trade the dev asked to be scored properly.
    ///
    /// `-6` alone reads as ruinous. In turn-deciding terms on a clean enemy it is `-6 + 1 (bleed,
    /// flat) + 3 (toxin, three fresh stacks)` — a net **-2**.
    #[test]
    fn the_bastard_sword_costs_two_not_six_on_a_clean_enemy() {
        let b = bastard().bonus_for("BASTARD", &[]);
        assert_eq!(b.damage_delta, -6);
        assert_eq!((b.bleed, b.toxin), (3, 3));
        let tick = status_tick(&HashMap::new(), b, Some(20));
        assert_eq!(tick, 4, "1 flat bleed + 3 toxin stacks");
        assert_eq!(b.damage_delta + tick, -2);
    }

    /// And it gets better against an enemy already carrying toxin, because toxin damage is the
    /// whole stack count rather than a flat rate.
    #[test]
    fn the_bastard_sword_breaks_even_against_an_already_poisoned_enemy() {
        let b = bastard().bonus_for("BASTARD", &[]);
        let mut enemy = HashMap::new();
        enemy.insert("toxin".to_string(), 2.0);
        let tick = status_tick(&enemy, b, Some(20));
        assert_eq!(tick, 6, "1 bleed + (2 existing + 3 applied) toxin");
        assert_eq!(b.damage_delta + tick, 0);
    }

    #[test]
    fn only_a_seven_letter_word_pays_the_bastard_trade() {
        for w in ["BASTAR", "BASTARDS"] {
            assert_eq!(bastard().bonus_for(w, &[]), WeaponBonus::default(), "{w}");
        }
    }

    /// Bleed is flat: one stack and ten stacks both tick for 1.
    /// `Immune` zeroes a status and `Weak` doubles it — both cheap enough that reporting them
    /// instead would have been noise. The level-0 crypt enemy carries `bleedWeak` and `toxinImmune`,
    /// which is what caught this.
    #[test]
    fn immune_zeroes_a_status_and_weak_doubles_it() {
        let mut e = HashMap::new();
        e.insert("bleed".to_string(), 1.0);
        e.insert("toxin".to_string(), 3.0);
        assert_eq!(status_tick(&e, WeaponBonus::default(), Some(20)), 4, "1 bleed + 3 toxin");
        e.insert("bleedWeak".to_string(), 1.0);
        assert_eq!(status_tick(&e, WeaponBonus::default(), Some(20)), 5, "bleed doubles to 2");
        e.insert("toxinImmune".to_string(), 1.0);
        assert_eq!(
            status_tick(&e, WeaponBonus::default(), Some(20)),
            2,
            "toxin contributes nothing"
        );
        assert!(
            deferred_status_model(&e, &Weapon::default()).is_none(),
            "Weak and Immune are modelled, not deferred"
        );
    }

    /// A `Resist` on a status the enemy does not have changes nothing, so it is not worth saying.
    #[test]
    fn only_a_resist_on_an_active_status_is_reported() {
        let mut e = HashMap::new();
        e.insert("bleedResist".to_string(), 1.0);
        assert!(
            deferred_status_model(&e, &Weapon::default()).is_none(),
            "not bleeding, so it cannot matter"
        );
        e.insert("bleed".to_string(), 2.0);
        assert!(deferred_status_model(&e, &Weapon::default())
            .expect("now it is in play")
            .contains("bleedResist"));
    }

    /// The Mindfog dagger is part of the same deferral, not a separate gap.
    #[test]
    fn the_mindfog_dagger_is_deferred_with_the_rest_of_the_status_model() {
        let w = Weapon { mindfog_unmodelled: true, ..Weapon::default() };
        let note = deferred_status_model(&HashMap::new(), &w).expect("in play");
        assert!(note.contains("bonusMindfogToxin"));
        assert!(note.contains("deferred post-MVP"), "one decision, not an oversight: {note}");
    }

    #[test]
    fn bleed_is_flat_and_toxin_is_the_stack_count() {
        let mut one = HashMap::new();
        one.insert("bleed".to_string(), 1.0);
        let mut ten = HashMap::new();
        ten.insert("bleed".to_string(), 10.0);
        assert_eq!(status_tick(&one, WeaponBonus::default(), Some(20)), 1);
        assert_eq!(status_tick(&ten, WeaponBonus::default(), Some(20)), 1);

        let mut toxic = HashMap::new();
        toxic.insert("toxin".to_string(), 4.0);
        assert_eq!(status_tick(&toxic, WeaponBonus::default(), Some(20)), 4);
    }

    /// The weapon's CIE pattern excludes wildcards, which the word-bonus one does not.
    #[test]
    fn the_weapon_cei_pattern_is_stricter_than_the_word_bonus_one() {
        assert!(cei_for_weapon("ANCIENT"));
        assert!(cei_for_weapon("WEIRD"));
        assert!(!cei_for_weapon("CEILING"));
        // A wildcard is spelled lower case, and `[^a-zC.]` excludes it.
        assert!(!cei_for_weapon("WaEID"), "no EI here at all");
        assert!(!cei_for_weapon("WaEI"), "the EI follows a wildcard letter");
        assert!(super::super::facts::has_cie("WaEI"), "the word bonus still counts it");
    }

    #[test]
    fn burning_tiles_are_counted_in_the_word_not_on_the_board() {
        let w = Weapon { burning_tiles: true, ..Weapon::default() };
        let mut burning = Tile::plain("A");
        burning.quality.burn = Some(1);
        let tiles = vec![burning, Tile::plain("B")];
        assert_eq!(w.bonus_for("AB", &tiles).burn, 1);
    }

    #[test]
    fn bleed_attack_adds_its_two_sources_while_toxin_attack_does_not() {
        // `:905` sums the status and the gear flag; `:896` grants a flat `#word` from either.
        let save = crate::game::save::parse(
            "return { rpg = { player = { statusEffects = { bleedAttack = 2, toxinAttack = 1 } } } }",
        )
        .unwrap();
        let gear = Gear::from_pairs(&[("bleedAttack", 3.0)]);
        let w = Weapon::from_save(&gear, &save);
        let b = w.bonus_for("CAT", &[]);
        assert_eq!(b.bleed, 5, "2 from the status plus 3 from the gear");
        assert_eq!(b.toxin, 3, "one grant, worth the word's length");
    }
}
