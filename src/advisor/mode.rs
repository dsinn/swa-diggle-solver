//! Which screen the advisor shows.
//!
//! An open set, not an on/off switch: the shrine is expected next, and adding it is a variant, an
//! arm in [`classify`], and a panel in the window, without touching the others. [`Snapshot`] is
//! everything read from disk for that reason — a shrine will want more than the combat save, and
//! the field it needs goes there rather than into every caller's signature.

use crate::game::save::Table;
use std::path::PathBuf;
use std::time::SystemTime;

/// A file's size and modification time: the only thing looked at before deciding to read it.
pub type Stamp = (u64, SystemTime);

/// What the watcher last read from the folder it is following.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// The save folder being followed.
    pub dir: PathBuf,
    /// `combatSaveData`, or `None` when the file does not exist.
    pub combat: Option<Table>,
    pub stamp: Option<Stamp>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    OutOfCombat,
    /// In a fight, but not at a point where a word can be typed. Names the game's state.
    CombatWaiting {
        state: String,
    },
    /// `PlayerTurn` with a board: the list is live.
    CombatPlayerTurn,
}

/// Decides the mode from what is on disk.
pub fn classify(s: &Snapshot) -> Mode {
    // **Temporary, until a better signal for finishing a dungeon is found.** The fight is over the
    // moment `Finish` is clicked, but nothing on disk changes then: `combatSaveData` is written at
    // `PlayerTurn.onStart` and `WaitPhase.onStart` (`rpgview.lua:1450`, `:1567`) and removed only
    // when the reward is confirmed, by `viewPostgameAndReturn` (`overworld.lua:1154`). So between
    // `Finish` and the reward this still reads as a fight, in whatever state the last write left.
    let Some(cs) = &s.combat else { return Mode::OutOfCombat };
    let state = cs.str_at("rpg.player.turnState").unwrap_or("").to_string();
    if state == "PlayerTurn" && !crate::turn::tiles_of(cs).is_empty() {
        return Mode::CombatPlayerTurn;
    }
    Mode::CombatWaiting { state }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::save::parse;
    use std::path::PathBuf;

    fn snap(src: Option<&str>) -> Snapshot {
        Snapshot { dir: PathBuf::from("x"), combat: src.map(|s| parse(s).unwrap()), stamp: None }
    }

    #[test]
    fn no_combat_save_is_out_of_combat() {
        assert_eq!(classify(&snap(None)), Mode::OutOfCombat);
    }

    #[test]
    fn a_committed_player_turn_is_the_player_turn() {
        let src = std::fs::read_to_string("tests/fixtures/combatSaveData-crypt-l0.lua").unwrap();
        assert_eq!(classify(&snap(Some(&src))), Mode::CombatPlayerTurn);
    }

    #[test]
    fn any_other_state_is_waiting_and_names_it() {
        let s = r#"return { rpg = { player = { turnState = "WaitPhase" } }, tileboard = { letters = { "A" } } }"#;
        assert_eq!(classify(&snap(Some(s))), Mode::CombatWaiting { state: "WaitPhase".into() });
    }

    /// `PlayerTurn` with nothing on the board has no words to offer.
    #[test]
    fn a_player_turn_without_a_board_is_waiting() {
        let s = r#"return { rpg = { player = { turnState = "PlayerTurn" } } }"#;
        assert_eq!(classify(&snap(Some(s))), Mode::CombatWaiting { state: "PlayerTurn".into() });
    }
}
