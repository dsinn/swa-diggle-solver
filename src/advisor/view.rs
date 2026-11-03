//! What the window shows, decided away from the window.
//!
//! [`View::next`] turns each [`Snapshot`] the watcher sends into the whole state of the window, so
//! everything the advisor *decides* is tested here and the binary only draws it.

use super::annotate::{rows, Row, ROW_CAP};
use super::mode::{classify, Mode, Snapshot};
use super::palette::Palette;
use crate::search::{Dictionary, Goal};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// What ranking needs from the game checkout, loaded once.
pub struct Resources {
    pub game_dir: PathBuf,
    pub dict: Dictionary,
    pub scorer: crate::score::Scorer,
    pub letters: crate::letters::Weights,
    pub palette: Palette,
}

impl Resources {
    pub fn load(game_dir: &Path) -> Result<Self, crate::Error> {
        Ok(Resources {
            game_dir: game_dir.to_path_buf(),
            dict: Dictionary::load(game_dir)?,
            scorer: crate::score::Scorer::new(game_dir)?,
            letters: crate::letters::Weights::load(game_dir)?,
            palette: Palette::load(game_dir),
        })
    }
}

/// One letter of a word, drawn on its tile's colour.
#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub letter: String,
    /// The tile's family colour — see [`super::palette::Palette::face`].
    pub colour: (u8, u8, u8),
    /// A variant material, a gem, or a tile with a border overlay: drawn with a border.
    pub special: bool,
    /// Burning tiles score nothing (`utils/tiles.lua:56-57`); drawn with a red border, never red
    /// lettering, which is hard to read on the darker tiles.
    pub burning: bool,
}

/// The cell for `letter` typed into `tile`.
pub fn cell_for(res: &Resources, tile: &crate::observe::board::Tile, letter: &str) -> Cell {
    let face = res.palette.face(Some(drawn_material(&res.scorer, tile)));
    Cell {
        letter: letter.to_string(),
        colour: face.colour,
        special: face.special || tile.quality.border.is_some(),
        burning: tile.burn().is_some(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListRow {
    pub row: Row,
    pub cells: Vec<Cell>,
}

/// The window's whole state.
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub mode: Mode,
    /// The save folder being followed, for the window title.
    pub folder: String,
    /// One line: enemy, player, goal.
    pub header: String,
    /// Diggle's own turn-log lines for this board: warnings, the status tick, the band.
    pub notes: String,
    pub rows: Vec<ListRow>,
    /// `false` greys the list: it is the last turn's, kept up while the game is between turns.
    pub live: bool,
}

impl Default for View {
    fn default() -> Self {
        View {
            mode: Mode::OutOfCombat,
            folder: String::new(),
            header: "waiting for the game to write a save".into(),
            notes: String::new(),
            rows: Vec::new(),
            live: false,
        }
    }
}

impl View {
    /// The view after `snap`, given the one before it.
    ///
    /// `peaks` is the fight's record of each enemy's highest health — see [`crate::turn::plan`] —
    /// and is cleared when the fight ends.
    pub fn next(
        prev: &View, snap: &Snapshot, res: &Resources, peaks: &mut HashMap<String, i64>,
    ) -> View {
        let folder = snap.dir.display().to_string();
        let mode = classify(snap);
        match &mode {
            Mode::OutOfCombat => {
                peaks.clear();
                View { mode, folder, header: "out of combat".into(), ..View::default() }
            }
            Mode::CombatWaiting { state } => View {
                header: format!("waiting - {state}"),
                folder,
                mode: mode.clone(),
                live: false,
                ..prev.clone()
            },
            Mode::CombatPlayerTurn => {
                let cs = snap.combat.as_ref().expect("a player turn has a save");
                match Self::turn(cs, res, peaks) {
                    Ok(v) => View { folder, ..v },
                    Err(e) => View {
                        mode,
                        folder,
                        header: format!("could not plan this turn: {e}"),
                        ..View::default()
                    },
                }
            }
        }
    }

    fn turn(
        cs: &crate::game::save::Table, res: &Resources, peaks: &mut HashMap<String, i64>,
    ) -> Result<View, crate::Error> {
        let Some(plan) = crate::turn::plan(cs, &res.game_dir, &res.letters, peaks)? else {
            return Ok(View { mode: Mode::CombatPlayerTurn, ..View::default() });
        };
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let list = crate::search::ranked(
            &res.dict,
            &res.scorer,
            &plan.tiles,
            &plan.geom,
            &plan.mods,
            plan.goal,
            &plan.picking,
            threads,
        );
        let mut notes = plan.notes.clone();
        notes.extend(crate::turn::goal_notes(cs, &plan.name, &plan.mods, plan.goal));
        if list.is_empty() {
            notes.push("nothing playable on this board".into());
        }
        let rows = rows(&plan, cs, &res.scorer, &list, ROW_CAP)
            .into_iter()
            .map(|row| {
                let cells = row
                    .letters
                    .iter()
                    .map(|(letter, i)| match plan.tiles.get(*i) {
                        Some(tile) => cell_for(res, tile, letter),
                        None => Cell {
                            letter: letter.clone(),
                            colour: super::palette::GREY,
                            special: false,
                            burning: false,
                        },
                    })
                    .collect();
                ListRow { row, cells }
            })
            .collect();
        let us = match (cs.int_at("rpg.player.health"), cs.int_at("rpg.player.maxHealth")) {
            (Some(h), Some(m)) => format!("{h}/{m}"),
            _ => "?".into(),
        };
        let mut enemy = format!("{} {}+{}hp", plan.name, plan.health, plan.armour);
        if plan.deciding_health != plan.health {
            enemy.push_str(&format!(" (aiming against {})", plan.deciding_health));
        }
        if plan.first_sighting {
            enemy.push_str(" - max hp assumed from first sighting");
        }
        Ok(View {
            mode: Mode::CombatPlayerTurn,
            folder: String::new(),
            header: format!("{enemy}  |  you {us}hp  |  {}", goal_words(plan.goal)),
            notes: notes.iter().map(|n| n.trim()).collect::<Vec<_>>().join("\r\n"),
            rows,
            live: true,
        })
    }
}

impl View {
    /// This view, with a note that the save changed but could not be read.
    ///
    /// In the notes rather than the header because the reason can be long and the header is one
    /// line. The list stays: the file is usually mid-write, and the next change replaces this.
    pub fn unreadable(&self, dir: &Path, error: &str) -> View {
        let line =
            format!("save in {} unreadable, waiting for the next write: {error}", dir.display());
        let notes = if self.notes.is_empty() { line } else { format!("{line}\r\n{}", self.notes) };
        View { notes, ..self.clone() }
    }
}

/// The material the game draws a tile in.
///
/// [`crate::score::Scorer::material_name`] answers only for letters in the material table, and
/// ligature tiles (`AE`, `TH`, `ING`) are not in it. The game falls back rather than giving up:
/// `getMaterialName` is `bomb` for a digit and `letterMaterials[letter] or 'wood'` otherwise
/// (`rpg/effects/material/default.lua`). Display only — the scorer is left as it is.
pub fn drawn_material<'a>(
    scorer: &'a crate::score::Scorer, tile: &'a crate::observe::board::Tile,
) -> &'a str {
    if let Some(m) = scorer.material_name(tile) {
        return m;
    }
    if tile.letter.parse::<f64>().is_ok() {
        "bomb"
    } else {
        "wood"
    }
}

/// The goal, as the header says it. Bands are printed inclusive, as Diggle's log prints them.
pub fn goal_words(goal: Goal) -> String {
    match goal {
        Goal::RankedKill { need } | Goal::FirstKill { need } => format!("kill: {need}+ damage"),
        Goal::Scare { need, below } => format!("scare: {need}..={} damage", below - 1),
        Goal::FrugalKill { need, below } => format!("frugal kill: {need}..={} damage", below - 1),
        Goal::MaxDamage => "hardest hit (overkill pays)".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advisor::mode::Snapshot;
    use std::path::PathBuf;

    fn res() -> Resources {
        Resources::load(&PathBuf::from("../sternly-worded-adventures")).unwrap()
    }

    fn snap(src: Option<&str>) -> Snapshot {
        Snapshot {
            dir: PathBuf::from("saves"),
            combat: src.map(|s| crate::game::save::parse(s).unwrap()),
            stamp: None,
        }
    }

    fn turn() -> String {
        std::fs::read_to_string("tests/fixtures/combatSaveData-crypt-l0.lua").unwrap()
    }

    #[test]
    fn a_player_turn_lists_live_rows_with_a_cell_per_letter() {
        let res = res();
        let mut peaks = Default::default();
        let v = View::next(&View::default(), &snap(Some(&turn())), &res, &mut peaks);
        assert_eq!(v.mode, Mode::CombatPlayerTurn);
        assert!(v.live);
        assert!(!v.rows.is_empty() && v.rows.len() <= crate::advisor::annotate::ROW_CAP);
        for r in &v.rows {
            assert_eq!(r.cells.len(), r.row.letters.len(), "{}", r.row.word);
        }
        assert_eq!(v.folder, "saves", "names the folder it follows");
        assert!(v.header.contains("first sighting"), "{}", v.header);
    }

    /// Between turns the last list stays up, greyed, rather than vanishing.
    #[test]
    fn waiting_keeps_the_last_list_but_greys_it() {
        let res = res();
        let mut peaks = Default::default();
        let live = View::next(&View::default(), &snap(Some(&turn())), &res, &mut peaks);
        let wait = turn().replace("\"PlayerTurn\"", "\"WaitPhase\"");
        let v = View::next(&live, &snap(Some(&wait)), &res, &mut peaks);
        assert_eq!(v.mode, Mode::CombatWaiting { state: "WaitPhase".into() });
        assert!(!v.live);
        assert_eq!(v.rows.len(), live.rows.len());
    }

    /// Leaving combat clears the list and forgets the fight's enemies.
    #[test]
    fn out_of_combat_clears_the_list_and_the_peaks() {
        let res = res();
        let mut peaks = Default::default();
        let live = View::next(&View::default(), &snap(Some(&turn())), &res, &mut peaks);
        let v = View::next(&live, &snap(None), &res, &mut peaks);
        assert_eq!(v.mode, Mode::OutOfCombat);
        assert!(v.rows.is_empty());
        assert!(peaks.is_empty());
    }

    /// A save that will not parse is said where it can be read in full, and the list stays.
    #[test]
    fn an_unreadable_save_is_noted_without_losing_the_list() {
        let res = res();
        let mut peaks = Default::default();
        let live = View::next(&View::default(), &snap(Some(&turn())), &res, &mut peaks);
        let v = live.unreadable(std::path::Path::new("saves"), "unexpected eof");
        assert!(
            v.notes.contains("unreadable") && v.notes.contains("unexpected eof"),
            "{}",
            v.notes
        );
        assert_eq!(v.header, live.header);
        assert_eq!(v.rows.len(), live.rows.len());
    }

    /// A letter the material table does not list is wood, and a digit is a bomb, as the game draws
    /// them (`getMaterialName`, `rpg/effects/material/default.lua`).
    #[test]
    fn unlisted_letters_are_coloured_as_the_game_draws_them() {
        let res = res();
        let tile = |l: &str| crate::observe::board::Tile::plain(l);
        assert_eq!(drawn_material(&res.scorer, &tile("AE")), "wood");
        assert_eq!(drawn_material(&res.scorer, &tile("3")), "bomb");
        assert_eq!(drawn_material(&res.scorer, &tile("Q")), "gold");
    }

    /// Braced wood is wood-coloured and bordered; a bordered tile is special; fire is its own flag.
    #[test]
    fn a_cell_shows_family_colour_special_and_fire() {
        use crate::observe::board::{Quality, Tile};
        let res = res();
        let wood = res.palette.colour(Some("wood"));
        let tile = |q: Quality| Tile { letter: "E".into(), quality: q };
        let braced = cell_for(
            &res,
            &tile(Quality { material: Some("woodbraced".into()), ..Quality::default() }),
            "E",
        );
        assert_eq!((braced.colour, braced.special, braced.burning), (wood, true, false));
        let gilt = cell_for(
            &res,
            &tile(Quality { border: Some("gold".into()), ..Quality::default() }),
            "E",
        );
        assert_eq!((gilt.colour, gilt.special), (wood, true));
        let fire = cell_for(&res, &tile(Quality { burn: Some(2), ..Quality::default() }), "E");
        assert_eq!((fire.colour, fire.special, fire.burning), (wood, false, true));
    }

    #[test]
    fn goals_read_as_words() {
        use crate::search::Goal;
        assert_eq!(goal_words(Goal::RankedKill { need: 7 }), "kill: 7+ damage");
        assert_eq!(goal_words(Goal::Scare { need: 4, below: 7 }), "scare: 4..=6 damage");
        assert_eq!(goal_words(Goal::FrugalKill { need: 4, below: 7 }), "frugal kill: 4..=6 damage");
        assert_eq!(goal_words(Goal::MaxDamage), "hardest hit (overkill pays)");
    }
}
