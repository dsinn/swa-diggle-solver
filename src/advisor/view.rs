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
    /// The game's word lists, for the spells a word would cast.
    pub lexica: crate::lexica::Lexica,
    /// Enemy names to enemy types, for whether Turn undead applies.
    pub bestiary: super::bestiary::Bestiary,
}

impl Resources {
    pub fn load(game_dir: &Path) -> Result<Self, crate::Error> {
        Ok(Resources {
            game_dir: game_dir.to_path_buf(),
            dict: Dictionary::load(game_dir)?,
            scorer: crate::score::Scorer::new(game_dir)?,
            letters: crate::letters::Weights::load(game_dir)?,
            palette: Palette::load(game_dir),
            lexica: crate::lexica::Lexica::load(game_dir)?,
            bestiary: super::bestiary::Bestiary::load(game_dir),
        })
    }
}

/// One letter of a word, drawn on its tile's colour.
#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub letter: String,
    /// The tile's face colour — see [`super::palette::Palette::face`].
    pub colour: (u8, u8, u8),
    /// The lettering colour the game prints on this tile, made readable where it leans on a glow.
    pub ink: (u8, u8, u8),
    /// Typed into a wildcard: drawn in a handwriting face, as the game scribbles them.
    pub wildcard: bool,
    /// The ring a special tile is drawn with: its metal border's colour, else its material's
    /// (see [`super::palette::Face::ring`]). `None` for a plain tile.
    pub ring: Option<(u8, u8, u8)>,
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
        ink: face.ink,
        wildcard: false,
        ring: res.palette.border(tile.quality.border.as_deref()).or(face.ring),
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
    /// The save file being followed, for the window title — which also says when it is an arcade
    /// challenge's rather than the normal run's.
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
        options: crate::turn::Options,
    ) -> View {
        let folder = snap.dir.join(&snap.file).display().to_string();
        let mode = classify(snap);
        match &mode {
            Mode::OutOfCombat => {
                peaks.clear();
                // No header: the banner already says it.
                View { mode, folder, header: String::new(), ..View::default() }
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
                let always_cast = crate::spells::always_cast_from_config(&snap.dir);
                match Self::turn(cs, res, peaks, options, always_cast) {
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
        options: crate::turn::Options, always_cast: bool,
    ) -> Result<View, crate::Error> {
        let started = std::time::Instant::now();
        let Some(mut plan) = crate::turn::plan(cs, &res.game_dir, &res.letters, peaks, options)?
        else {
            return Ok(View { mode: Mode::CombatPlayerTurn, ..View::default() });
        };
        // The player can cast, so the search scores each word by its best option — see
        // `crate::spells`. Diggle's own plan leaves this `None`.
        let mut book =
            crate::spells::SpellBook::from_save(cs, &res.lexica, res.bestiary.undead(&plan.name));
        book.always_cast = always_cast;
        book.choose = true;
        plan.mods.spells = Some(book.clone());
        let planned = started.elapsed();
        let ranking = std::time::Instant::now();
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
        let ranked = ranking.elapsed();
        let annotating = std::time::Instant::now();
        let mut notes = plan.notes.clone();
        notes.extend(crate::turn::goal_notes(cs, &plan.name, &plan.mods, plan.goal));
        if book.turn_undead_unknown() {
            notes.push(format!(
                "cannot tell whether {} is undead; Turn undead's double damage is not counted",
                plan.name
            ));
        }
        if list.is_empty() {
            notes.push("nothing playable on this board".into());
        }
        let rows = rows(&plan, cs, &res.scorer, &book, &list, ROW_CAP)
            .into_iter()
            .map(|row| {
                let cells = row
                    .letters
                    .iter()
                    .map(|l| {
                        let cell = match plan.tiles.get(l.tile) {
                            Some(tile) => cell_for(res, tile, &l.text),
                            None => Cell {
                                letter: l.text.clone(),
                                colour: super::palette::GREY,
                                ink: (0, 0, 0),
                                wildcard: false,
                                ring: None,
                                burning: false,
                            },
                        };
                        Cell { wildcard: l.wildcard, ..cell }
                    })
                    .collect();
                ListRow { row, cells }
            })
            .collect();
        super::telemetry::note(
            "rank",
            format_args!(
                "turn {} vs {} on {} tiles ({} wildcards), goal {:?}: plan {}, search {} ({} candidates, {threads} threads), rows {}",
                cs.int_at("rpg.player.turnNumber").unwrap_or(-1),
                plan.name,
                plan.tiles.len(),
                plan.tiles.iter().filter(|t| t.is_wildcard()).count(),
                plan.goal,
                super::telemetry::ms(planned),
                super::telemetry::ms(ranked),
                list.len(),
                super::telemetry::since(annotating),
            ),
        );
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
        // Empty only when stalling: any hit kills, and the enemy cannot hit back this turn.
        Goal::Scare { need, below } if need >= below => "spare: deal nothing".into(),
        Goal::Scare { need, below } => format!("scare: {need}..={} damage", below - 1),
        // A free turn: keep it here, short of its flee threshold. See `search::Goal::Hold`.
        Goal::Hold { below, .. } if below <= 1 => "hold: deal nothing, don't scare".into(),
        Goal::Hold { below, .. } => format!("hold: under {below} damage, don't scare"),
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
            file: "bogglingChallenge".into(),
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
        let v = View::next(
            &View::default(),
            &snap(Some(&turn())),
            &res,
            &mut peaks,
            Default::default(),
        );
        assert_eq!(v.mode, Mode::CombatPlayerTurn);
        assert!(v.live);
        assert!(!v.rows.is_empty() && v.rows.len() <= crate::advisor::annotate::ROW_CAP);
        for r in &v.rows {
            assert_eq!(r.cells.len(), r.row.letters.len(), "{}", r.row.word);
        }
        assert_eq!(
            v.folder,
            PathBuf::from("saves").join("bogglingChallenge").display().to_string(),
            "names the file it follows"
        );
        assert!(v.header.contains("first sighting"), "{}", v.header);
    }

    /// Between turns the last list stays up, greyed, rather than vanishing.
    #[test]
    fn waiting_keeps_the_last_list_but_greys_it() {
        let res = res();
        let mut peaks = Default::default();
        let live = View::next(
            &View::default(),
            &snap(Some(&turn())),
            &res,
            &mut peaks,
            Default::default(),
        );
        let wait = turn().replace("\"PlayerTurn\"", "\"WaitPhase\"");
        let v = View::next(&live, &snap(Some(&wait)), &res, &mut peaks, Default::default());
        assert_eq!(v.mode, Mode::CombatWaiting { state: "WaitPhase".into() });
        assert!(!v.live);
        assert_eq!(v.rows.len(), live.rows.len());
    }

    /// Leaving combat clears the list and forgets the fight's enemies.
    #[test]
    fn out_of_combat_clears_the_list_and_the_peaks() {
        let res = res();
        let mut peaks = Default::default();
        let live = View::next(
            &View::default(),
            &snap(Some(&turn())),
            &res,
            &mut peaks,
            Default::default(),
        );
        let v = View::next(&live, &snap(None), &res, &mut peaks, Default::default());
        assert_eq!(v.mode, Mode::OutOfCombat);
        assert!(v.rows.is_empty());
        // Not the previous fight's header, and not the default's "waiting for a save" either.
        assert_eq!(v.header, "", "the banner says out of combat; the header must not repeat it");
        assert!(peaks.is_empty());
    }

    /// A save that will not parse is said where it can be read in full, and the list stays.
    #[test]
    fn an_unreadable_save_is_noted_without_losing_the_list() {
        let res = res();
        let mut peaks = Default::default();
        let live = View::next(
            &View::default(),
            &snap(Some(&turn())),
            &res,
            &mut peaks,
            Default::default(),
        );
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

    /// Braced wood has its own face, black lettering and a ring; a gilt tile is ringed gold; fire
    /// is its own flag.
    #[test]
    fn a_cell_shows_its_own_colours_ring_and_fire() {
        use crate::observe::board::{Quality, Tile};
        let res = res();
        let wood = res.palette.face(Some("wood")).colour;
        let tile = |q: Quality| Tile { letter: "E".into(), quality: q };
        let braced = cell_for(
            &res,
            &tile(Quality { material: Some("woodbraced".into()), ..Quality::default() }),
            "E",
        );
        assert_eq!(braced.colour, res.palette.face(Some("woodbraced")).colour);
        assert_eq!(braced.ring, Some(super::super::palette::SPECIAL), "a plain dark ring");
        assert!(
            braced.ink.0 < 80,
            "braced wood is lettered black, as in the game: {:?}",
            braced.ink
        );
        let gilt = cell_for(
            &res,
            &tile(Quality { border: Some("gold".into()), ..Quality::default() }),
            "E",
        );
        assert_eq!((gilt.colour, gilt.ring), (wood, res.palette.border(Some("gold"))));
        let fire = cell_for(&res, &tile(Quality { burn: Some(2), ..Quality::default() }), "E");
        assert_eq!((fire.colour, fire.ring, fire.burning), (wood, None, true));
    }

    #[test]
    fn a_held_spell_shows_on_the_words_that_cast_it() {
        let res = res();
        let held = turn().replacen(
            "turnState = \"PlayerTurn\",",
            "turnState = \"PlayerTurn\", gearFlags = { spellFire = 1 },",
            1,
        );
        let v = View::next(
            &View::default(),
            &snap(Some(&held)),
            &res,
            &mut Default::default(),
            Default::default(),
        );
        let fire = res.lexica.words_for("fire").unwrap();
        let mut cast = 0;
        for r in &v.rows {
            assert_eq!(
                r.row.spells.iter().any(|s| s.text == "🔥"),
                fire.contains(&r.row.word),
                "{}",
                r.row.word
            );
            cast += r.row.spells.len();
        }
        assert!(cast > 0, "no row cast Fire, so this proves nothing");
        let plain = View::next(
            &View::default(),
            &snap(Some(&turn())),
            &res,
            &mut Default::default(),
            Default::default(),
        );
        assert!(plain.rows.iter().all(|r| r.row.spells.is_empty()), "no spell held, none shown");
    }

    /// The bug a user found on a real save: against an undead enemy, a bone word with Turn undead
    /// held deals double, can kill, and must rank and read as such — `ACETABULUM` had been shown at
    /// half its damage, below words that could not kill.
    #[test]
    fn turn_undead_doubles_a_bone_word_against_a_skeleton() {
        let res = res();
        let board: String = "ACETABULUMQXZJVK".chars().map(|c| format!("\"{c}\", ")).collect();
        let save = |spell: bool| {
            let mut s = turn().replacen("\"Amorphous\"", "\"Bones McCoy\"", 1);
            let a = s.find("tileboard = {").unwrap();
            let b = a + s[a..].find('}').unwrap();
            s.replace_range(a..=b, &format!("tileboard = {{ {board} }}"));
            if spell {
                s = s.replacen(
                    "turnState = \"PlayerTurn\",",
                    "turnState = \"PlayerTurn\", gearFlags = { spellTurnUndead = 1 },",
                    1,
                );
            }
            s
        };
        let row = |v: &View| v.rows.iter().find(|r| r.row.word == "ACETABULUM").cloned();
        let plain = View::next(
            &View::default(),
            &snap(Some(&save(false))),
            &res,
            &mut Default::default(),
            Default::default(),
        );
        let cast = View::next(
            &View::default(),
            &snap(Some(&save(true))),
            &res,
            &mut Default::default(),
            Default::default(),
        );
        let (plain, cast) = (row(&plain).expect("spellable"), row(&cast).expect("spellable"));
        assert!(plain.row.score > 0);
        assert_eq!(cast.row.score, plain.row.score * 2);
        assert_eq!(cast.row.spells.first().map(|s| s.text.as_str()), Some("💀"));
        assert!(plain.row.spells.is_empty());
    }

    /// The user's fight, 2026-11-03: the rapier against an enemy behind 54 armour. `SINGES` starts
    /// and ends with S, so it goes straight to health and kills, where without the rapier it would
    /// only dent the armour.
    #[test]
    fn a_rapier_word_kills_through_armour() {
        let res = res();
        let board: String = "SINGESCATHQQQQQQ".chars().map(|c| format!("\"{c}\", ")).collect();
        let save = |rapier: bool| {
            let mut s = turn().replacen("health = 3,", "health = 5, armour = 54,", 1);
            let a = s.find("tileboard = {").unwrap();
            let b = a + s[a..].find('}').unwrap();
            s.replace_range(a..=b, &format!("tileboard = {{ {board} }}"));
            if rapier {
                s = s.replacen(
                    "gearFlags = {",
                    "gearFlags = { damageArmourPiercingEndsMatch = 1,",
                    1,
                );
            }
            s
        };
        let view = |rapier: bool| {
            View::next(
                &View::default(),
                &snap(Some(&save(rapier))),
                &res,
                &mut Default::default(),
                Default::default(),
            )
        };
        let row = |v: &View, w: &str| v.rows.iter().find(|r| r.row.word == w).cloned();
        let (plain, rapier) = (view(false), view(true));
        let singes = row(&rapier, "SINGES").expect("typeable");
        assert!(singes.row.score > 5, "SINGES must out-damage 5 health: {}", singes.row.score);
        assert!(singes.row.pierces);
        assert_eq!(singes.row.tier, crate::search::Tier::InBand);
        assert_eq!(singes.row.effect, "kills");
        assert!(
            rapier.rows[0].row.pierces,
            "the top row is a rapier kill: {}",
            rapier.rows[0].row.word
        );
        assert!(rapier.notes.contains("rapier"), "{}", rapier.notes);

        let singes = row(&plain, "SINGES").expect("typeable");
        assert!(!singes.row.pierces);
        assert_eq!(singes.row.tier, crate::search::Tier::Fallback);
        assert!(!plain.notes.contains("rapier"));
    }

    /// The bassanello, the user's ask, 2026-11-06: a wood-only word with a run of repeated
    /// letters queues a wildcard per run, so it ranks above an equal word without one, and the
    /// Gain column says so.
    #[test]
    fn the_bassanellos_wildcards_rank_and_show() {
        let res = res();
        // Wood letters only, so every word on it is wood-only: TOSS and LESS have a run, and plenty
        // of words have none.
        let board: String = "TOSSERLINEATQQQQ".chars().map(|c| format!("\"{c}\", ")).collect();
        let save = |worn: bool| {
            let mut s = turn();
            let a = s.find("tileboard = {").unwrap();
            let b = a + s[a..].find('}').unwrap();
            s.replace_range(a..=b, &format!("tileboard = {{ {board} }}"));
            if worn {
                s = s.replacen(
                    "gearFlags = {",
                    "gearFlags = { onSubmitWoodQueueWoodornateWildcardPerRun = 1,",
                    1,
                );
            }
            s
        };
        let view = |worn: bool| {
            View::next(
                &View::default(),
                &snap(Some(&save(worn))),
                &res,
                &mut Default::default(),
                Default::default(),
            )
        };
        let runs = |w: &str| crate::gear::facts::count_runs(w);
        let worn = view(true);
        assert!(!worn.rows.is_empty());
        for r in &worn.rows {
            let n = runs(&r.row.word) as i64;
            assert_eq!(r.row.gain.wildcards, n, "{}", r.row.word);
            let text = crate::advisor::annotate::gain_text(r.row.gain);
            assert_eq!(text.contains(&format!("+{n} ⬜")), n > 0, "{}: {text}", r.row.word);
        }
        assert!(
            runs(&worn.rows[0].row.word) > 0,
            "the top row queues a wildcard: {}",
            worn.rows[0].row.word
        );
        assert!(view(false).rows.iter().all(|r| r.row.gain.wildcards == 0), "not worn");
    }

    /// The Staff of the dragon, the user's ask, 2026-11-07: a fire word queues a Corundum, which
    /// ranks above the infinitives, and the Gain column shows it.
    #[test]
    fn the_staff_of_the_dragons_corundum_ranks_and_shows() {
        let res = res();
        let fire = res.lexica.words_for("fire").unwrap().clone();
        let board: String = "COALHEATSIREBURN".chars().map(|c| format!("\"{c}\", ")).collect();
        let save = |worn: bool| {
            let mut s = turn();
            let a = s.find("tileboard = {").unwrap();
            let b = a + s[a..].find('}').unwrap();
            s.replace_range(a..=b, &format!("tileboard = {{ {board} }}"));
            if worn {
                s = s.replacen(
                    "gearFlags = {",
                    "gearFlags = { onSubmitFireDictWordQueueGemfire = 1,",
                    1,
                );
            }
            s
        };
        let view = |worn: bool| {
            View::next(
                &View::default(),
                &snap(Some(&save(worn))),
                &res,
                &mut Default::default(),
                Default::default(),
            )
        };
        let worn = view(true);
        assert!(worn.rows.iter().any(|r| !fire.contains(&r.row.word)), "plain words compete");
        for r in &worn.rows {
            let n = i64::from(fire.contains(&r.row.word));
            assert_eq!(r.row.gain.gems, n, "{}", r.row.word);
            let text = crate::advisor::annotate::gain_text(r.row.gain);
            assert_eq!(text.contains("+1 💎"), n > 0, "{}: {text}", r.row.word);
        }
        let top = &worn.rows[0].row.word;
        assert!(fire.contains(top), "the top row is a fire word: {top}");
        let bare = view(false);
        assert!(bare.rows.iter().all(|r| r.row.gain.gems == 0), "not worn");
        let plain = &bare.rows[0].row.word;
        assert!(!fire.contains(plain), "the control: unworn, the top row is not one: {plain}");
    }

    /// The user's chest, 2026-11-06: a priest with Adept (`arcaneSpellEffectBonus5 = 10`) and one
    /// Chrysocolla F on the board. `FIRWOOD` started on it is an arcane spell, 20 x 1.5 = 30, and
    /// its 4 ice tick 1 before the chest acts: 31 in all, past `DRIFTWOOD`'s 30, which uses the F
    /// mid-word where it does nothing. Before, `DRIFTWOOD` was listed first and `FIRWOOD` at 20.
    #[test]
    fn a_chrysocolla_first_casts_arcane_and_freezes() {
        let res = res();
        let src =
            std::fs::read_to_string("tests/fixtures/combatSaveData-chest-chrysocolla.lua").unwrap();
        let v = View::next(
            &View::default(),
            &snap(Some(&src)),
            &res,
            &mut Default::default(),
            Default::default(),
        );
        let row = |w: &str| v.rows.iter().position(|r| r.row.word == w).expect(w);
        let (fir, drift) = (row("FIRWOOD"), row("DRIFTWOOD"));
        let f = &v.rows[fir];
        assert_eq!(f.row.letters[0].tile, 14, "FIRWOOD starts on the Chrysocolla");
        assert_eq!((f.row.score, f.row.afflictions.ice), (30, 1), "boosted, and the ice ticks");
        let d = &v.rows[drift];
        assert_eq!((d.row.score, d.row.afflictions.total()), (30, 0), "mid-word, it does nothing");
        assert!(fir < drift, "FIRWOOD is listed above DRIFTWOOD");
        // Ending on it is active too: an arcane spell, but the ice comes only from the front.
        let whiff = &v.rows[row("WHIFF")];
        assert_eq!(whiff.row.letters.last().map(|l| l.tile), Some(14));
        assert_eq!((whiff.row.score, whiff.row.afflictions.ice), (24, 0), "16 x 1.5");
    }

    /// Boggling anxiety: a 5x5 board (erudite) under the letter-chaining curse with the chain
    /// bonus. Every word listed must chain tile to tile, every one earns the bonus, and the list must
    /// arrive well inside the challenge's twenty-second turn.
    #[test]
    fn boggling_anxiety_lists_only_chained_words_in_time() {
        let res = res();
        let board: String =
            "TRAINSOLETCAPIDMUNESHOWER".chars().map(|c| format!("\"{c}\", ")).collect();
        let save = |curse: bool, bonus: bool| {
            let mut s = turn().replacen("health = 3,", "health = 200,", 1);
            let a = s.find("tileboard = {").unwrap();
            let b = a + s[a..].find('}').unwrap();
            s.replace_range(a..=b, &format!("tileboard = {{ {board} }}"));
            s = s.replacen("\"bedroll\",", "\"erudite\", \"bedroll\",", 1);
            if curse {
                s = s.replacen("gearFlags = {", "gearFlags = { wordRequirementAdjacent = 1,", 1);
            }
            if bonus {
                s = s.replacen("gearFlags = {", "gearFlags = { wordScoreBonusAdjacent = 1,", 1);
            }
            s
        };
        let view = |curse: bool, bonus: bool| {
            View::next(
                &View::default(),
                &snap(Some(&save(curse, bonus))),
                &res,
                &mut Default::default(),
                Default::default(),
            )
        };
        let started = std::time::Instant::now();
        let cursed = view(true, true);
        let took = started.elapsed();
        let free = view(false, false);
        let unpaid = view(true, false);
        assert!(!cursed.notes.contains("Adjacent"), "{}", cursed.notes);
        assert!(!cursed.rows.is_empty());
        let geom = crate::geometry::Geometry::for_passive("erudite", 25).geometry;
        assert_eq!(geom.total_tiles(), 25);
        for r in &cursed.rows {
            let tiles: Vec<usize> = r.row.letters.iter().map(|l| l.tile).collect();
            assert!(
                tiles.windows(2).all(|p| geom.tiles_adjacent(p[0], p[1])),
                "{} does not chain: {tiles:?}",
                r.row.word
            );
        }
        // The curse removes words, and the bonus raises the ones that are left.
        assert!(
            cursed.rows.len() < free.rows.len()
                || free.rows.len() == crate::advisor::annotate::ROW_CAP
        );
        let unpaid_score =
            |w: &str| unpaid.rows.iter().find(|r| r.row.word == w).map(|r| r.row.score);
        let shared: Vec<(i64, i64)> = cursed
            .rows
            .iter()
            .filter_map(|r| Some((r.row.score, unpaid_score(&r.row.word)?)))
            .collect();
        assert!(!shared.is_empty(), "no word on both lists to compare");
        assert!(shared.iter().all(|(c, f)| c >= f), "the chain bonus never lowers a score");
        assert!(shared.iter().any(|(c, f)| c > f), "and it raises some: {shared:?}");
        eprintln!("boggling turn ranked in {took:?}, {} rows", cursed.rows.len());
        assert!(took < std::time::Duration::from_secs(5), "{took:?}");
    }

    #[test]
    fn goals_read_as_words() {
        use crate::search::Goal;
        assert_eq!(goal_words(Goal::RankedKill { need: 7 }), "kill: 7+ damage");
        assert_eq!(goal_words(Goal::Scare { need: 4, below: 7 }), "scare: 4..=6 damage");
        assert_eq!(goal_words(Goal::MaxDamage), "hardest hit (overkill pays)");
        assert_eq!(goal_words(Goal::Scare { need: 1, below: 1 }), "spare: deal nothing");
        assert_eq!(
            goal_words(Goal::Hold { below: 4, lethal: 9 }),
            "hold: under 4 damage, don't scare"
        );
        assert_eq!(
            goal_words(Goal::Hold { below: 1, lethal: 9 }),
            "hold: deal nothing, don't scare"
        );
    }

    /// The hold in the window: on the free turn the top row hurts the enemy without frightening it
    /// off, and every scare is listed below the words that keep it here.
    #[test]
    fn a_free_turn_lists_a_chip_above_every_scare() {
        use crate::search::Tier;
        let res = res();
        let v = View::next(
            &View::default(),
            &snap(Some(&crate::turn::tests::held_save())),
            &res,
            &mut Default::default(),
            Default::default(),
        );
        assert!(v.header.contains("hold: under 6 damage"), "{}", v.header);
        let top = &v.rows.first().expect("a ranked list").row;
        assert_eq!(top.tier, Tier::Under, "{} first", top.word);
        assert!(top.score >= 1 && top.score < 6, "{} scores {}", top.word, top.score);
        let first_scare = v.rows.iter().position(|r| r.row.tier == Tier::Flees);
        let last_keep =
            v.rows.iter().rposition(|r| matches!(r.row.tier, Tier::Under | Tier::Stall));
        if let (Some(s), Some(k)) = (first_scare, last_keep) {
            assert!(k < s, "a scare at row {s} is above a word that keeps it, at row {k}");
            assert_eq!(v.rows[s].row.effect, "scares - on a free turn");
        }
        assert!(v.rows.iter().all(|r| r.row.tier != Tier::InBand), "no word is aimed at a scare");
    }

    /// **The checkbox, end to end.** One halfling save on a free turn, a terrified enemy on 1
    /// health: ticked, the top row deals nothing and says why; unticked, it kills, as the advisor
    /// did before the box existed. Both lists hold the same words — only the order differs.
    #[test]
    fn the_stall_box_decides_whether_a_free_turn_spares_or_kills() {
        use crate::search::Tier;
        let res = res();
        let save = crate::turn::tests::halfling_save(2, true);
        let view = |stall: bool| {
            let mut peaks = HashMap::from([("Amorphous".to_string(), 8)]);
            View::next(
                &View::default(),
                &snap(Some(&save)),
                &res,
                &mut peaks,
                crate::turn::Options { stall_on_free_turns: stall },
            )
        };
        let (on, off) = (view(true), view(false));

        let top = &on.rows.first().expect("a ranked list").row;
        assert_eq!(top.tier, Tier::Stall, "{} first", top.word);
        assert_eq!(top.score, 0);
        assert_eq!(top.effect, "stalls - it cannot hit you");
        assert!(on.header.contains("spare: deal nothing"), "{}", on.header);
        // The kills are still there, just below.
        assert!(on.rows.iter().any(|r| r.row.tier == Tier::Overshoot), "no kill listed at all");

        let top = &off.rows.first().expect("a ranked list").row;
        assert_eq!(top.effect, "kills", "{} first, unticked", top.word);
        assert!(off.rows.iter().all(|r| r.row.tier != Tier::Stall));
        assert!(off.header.contains("kill"), "{}", off.header);
    }
}
