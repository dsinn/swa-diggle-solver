//! Turn parity: the turns on which an attack simply does not land.
//!
//! One passive works this way and it is the halfling's — `immuneTurnMod2`, *"Enemies can't deal
//! damage to you on even turns"* (`items/classpassives.lua:35-48`). The game evaluates it in one
//! place:
//!
//! ```lua
//! local function entityDodged(entity)
//!     return ((entity.gearFlags.immuneTurnMod2 or entity.statusEffects.immuneTurnMod2)
//!             and player.turnNumber%2==0)
//! end
//! ```
//! (`rpgview.lua:265`)
//!
//! The dodge is total and free. `EnemyAttackDamage` reads `getPlayerStatus'immune'`, which
//! `entityDodged` answers with **-1** (`:268-273`), and the whole damage block is skipped —
//! health, toxin, bleed and burn alike (`:1766-1808`). The `-1` form is not a stack, so the
//! `elseif` that decrements a real `immune` never touches it: it costs nothing and does not run out.
//!
//! ## The three numberings, which are not the same number
//!
//! This is the only hard part, and getting it wrong inverts the answer. One turn of one fight is
//! described three ways at once:
//!
//! | Where | The player's first word reads | Why |
//! |---|---|---|
//! | The console, `Player turn n start;` | **0** | `tileboard.onPlayerTurn` logs it (`tileboard.lua:1230,1285`), and `rpgview.lua:1551` calls that **before** the increment |
//! | `rpg.player.turnNumber` in the save | **1** | `rpgview.lua:1565` increments, then `rpg:save()` on the next line |
//! | The `Turn:` box on screen | **1** | `rpginfo.lua:133` draws the live value, so it shows the post-increment one too |
//!
//! Both halves are checkable without the game running, and both are checked by the tests below:
//! every fresh fight's log in this repo opens at `Player turn 0 start`, and
//! `checkpoints/pre-hud-cap/combatSaveData` — a fight parked at its first word — records
//! `turnNumber = 1`.
//!
//! So: **the console's turn `n` is the save's turn `n + 1`**, and the halfling is safe on the
//! console's odd turns. Said the way the dev says it, counting from the console the way our own logs
//! do: *the halfling only fears enemies on even turns.* Both statements are the same statement, and
//! the first version of this code got it backwards by quoting `turnNumber` while the dev quoted the
//! log.

/// The passive and status key the game checks. Both spellings of it — a class passive writes into
/// `gearFlags`, a status effect into `statusEffects` — and `entityDodged` accepts either.
pub const IMMUNE_KEY: &str = "immuneTurnMod2";

/// Whether an entity carrying the flag dodges, given the turn as the **save** numbers it.
///
/// Transcribed rather than paraphrased: `flag and turnNumber % 2 == 0` (`rpgview.lua:265`). The
/// parity is read off `player.turnNumber` no matter whose flag it is, so this same function answers
/// for the enemy as well — see `rpg/enemies/humans.lua:193-213`, the halfling villager, which is
/// deliberately not modelled (the dev, 2026-08-15: *"we don't have to worry about fighting a
/// halfling"*).
pub fn dodges(has_flag: bool, save_turn: i64) -> bool {
    has_flag && save_turn % 2 == 0
}

/// The save's turn number for the board the console announced as `Player turn n start;`.
///
/// Exists so the off-by-one is written down once, in a place with a test, instead of being
/// rediscovered at each call site.
pub fn save_turn_of(console_turn: u32) -> i64 {
    console_turn as i64 + 1
}

/// Reads the flag from a combat save, either spelling.
pub fn has_flag(save: &crate::game::save::Table) -> bool {
    save.path(&format!("rpg.player.gearFlags.{IMMUNE_KEY}")).is_some()
        || save.path(&format!("rpg.player.statusEffects.{IMMUNE_KEY}")).is_some()
}

/// The turn the save is parked on, if it says.
pub fn save_turn(save: &crate::game::save::Table) -> Option<i64> {
    save.int_at("rpg.player.turnNumber")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Transcription check against `rpgview.lua:265`, in the save's own numbering.
    #[test]
    fn the_flag_dodges_on_even_save_turns_and_never_without_it() {
        for turn in 1..=6 {
            assert_eq!(dodges(true, turn), turn % 2 == 0, "save turn {turn}");
            assert!(!dodges(false, turn), "no flag, no dodge, at save turn {turn}");
        }
    }

    /// **The dev's sentence, in the dev's numbering.** *"Make sure the halfling also only fears
    /// enemies on even turns."* Counting from the console — which is how our own run logs count,
    /// and the only turn number this project has ever printed — an even turn is one where the
    /// attack lands, so it is exactly the turn to be afraid on.
    #[test]
    fn counted_from_the_console_the_halfling_fears_on_even_turns() {
        for console in 0..=5u32 {
            let feared = !dodges(true, save_turn_of(console));
            assert_eq!(feared, console % 2 == 0, "console turn {console}");
        }
    }

    /// The save paths, which are the part that silently reads `false` if they are wrong.
    ///
    /// Both spellings, because a class passive writes its flags into `gearFlags`
    /// (`rpgview.lua:588-597`) while a status effect of the same name lands in `statusEffects`, and
    /// `entityDodged` accepts either.
    #[test]
    fn both_spellings_of_the_flag_are_found_where_the_save_puts_them() {
        let with = |where_: &str| {
            format!(
                "return {{ rpg = {{ player = {{ turnNumber = 4, {where_} = {{ immuneTurnMod2 = 1 }} }} }} }}"
            )
        };
        for spelling in ["gearFlags", "statusEffects"] {
            let save = crate::game::save::parse(&with(spelling)).expect("should parse");
            assert!(has_flag(&save), "the flag is invisible under `{spelling}`");
            assert_eq!(save_turn(&save), Some(4));
            assert!(dodges(has_flag(&save), save_turn(&save).unwrap()));
        }
        let plain = crate::game::save::parse("return { rpg = { player = { turnNumber = 4 } } }")
            .expect("should parse");
        assert!(!has_flag(&plain), "no flag on an ordinary champion");
    }

    /// Half the off-by-one, from a real save: a fight parked at its first word records
    /// `turnNumber = 1`, not 0. If this ever reads 0 the parity above is inverted.
    #[test]
    fn a_fight_at_its_first_word_saves_turn_one() {
        let path = PathBuf::from("checkpoints/pre-hud-cap/combatSaveData");
        let Ok(src) = std::fs::read_to_string(&path) else {
            eprintln!("SKIP: {} is missing", path.display());
            return;
        };
        let save = crate::game::save::parse(&src).expect("a checkpoint should parse");
        assert_eq!(
            save_turn(&save),
            Some(1),
            "the save numbers the first word 1; the console numbers the same word 0"
        );
    }

    /// The other half: every fresh fight's console log opens at turn 0. Read from the run archive
    /// rather than asserted, because it is a claim about the game's output.
    #[test]
    fn a_fresh_fight_opens_at_console_turn_zero() {
        let Ok(log) = std::fs::read_to_string("spike-run-20260815-1913Z.log") else {
            eprintln!("SKIP: the run archive is not present");
            return;
        };
        let first = log
            .lines()
            .find_map(|l| l.split_once("Player turn ").map(|(_, r)| r.to_string()))
            .expect("the log should announce a turn");
        assert!(
            first.starts_with("0 start"),
            "a fight's first console turn should be 0, got `{}`",
            first.chars().take(12).collect::<String>()
        );
    }
}
