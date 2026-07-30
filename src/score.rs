//! Scoring words.
//!
//! ## Why this is a reimplementation and not the game's own code
//!
//! Running `words.score` through mlua got as far as loading `utils/words.lua` and its 78 modifier
//! files, via a custom `require`, a `love.filesystem` shim, a `utf8` shim, a `buildDrawDataTable`
//! stub, a deep `love` stub, and per-file `pcall` loading of the modifiers. Then it hit the wall that
//! actually matters:
//!
//! `utils/tiles.lua:39-50` does `local border = tiles.getBorderData(tile)` and then `border.score`,
//! and `getBorderData` returns **nil** unless `tile.extra.border` is set. Live tiles carry
//! `extra.border` and `extra.bg`; the serialised board dump does **not** — the game reconstructs them
//! when it loads a board. So exact scoring would require replicating how tiles acquire their material
//! and border anyway. The mlua route was not avoiding reimplementation, only relocating it.
//!
//! Version drift being acceptable, this computes the score directly. What it does NOT do is hardcode
//! the game's *data*: the letter → material table is read from `rpg/effects/material/default.lua` at
//! runtime, because that is the part most likely to change and the cheapest to keep honest.
//!
//! ## The formula, from the game
//!
//! ```text
//! words.score       (utils/words.lua:292-295)  floor((sum + add) * mult * lengthScore + 0.5 + postAdd)
//! words.lengthScore (utils/words.lua:215-217)  0.2 * (len + 1)
//! ```
//!
//! With no gear, `add = 0`, `mult = 1`, `postAdd = 0`.
//!
//! ## Known divergence, stated rather than discovered later
//!
//! - **Gear `wordBonus` modifiers are ignored.** ~26 modifier files adjust `mult`/`add`/`postAdd`
//!   under conditions. At MVP the player's `gear` is empty, so they contribute nothing; as gear
//!   accumulates this under-estimates. That is the accepted drift.
//! - **Enemy status lexicon bonuses** (`words.getWordBonusModifier`) are ignored.
//! - **Tile state is honoured where it would flatter us**: `burn` zeroes a tile's score
//!   (`utils/tiles.lua:56-57`), so [`Scorer::score_tiles`] applies it. Ignoring burn would
//!   *over*-estimate, and an over-estimate types a word that fails to kill — handing the enemy a free
//!   turn. Under-estimating merely costs search.
//!
//! Net direction: this can under-rate a word but should not over-rate one, so a word it calls lethal
//! should be lethal, while a word it calls non-lethal might still kill.

use crate::observe::board::Tile;
use std::collections::HashMap;
use std::path::Path;

/// Material score values, from `rpg/effects/material/<name>.lua` (`score = …`) in 52.3.
///
/// Hardcoded because reading them means loading 30+ material modules that pull in engine code — the
/// same wall that ended the mlua attempt. Unknown materials fall back to `wood` and are reported by
/// [`Scorer::unknown_materials`] rather than silently scored: a new high-value material treated as
/// wood would quietly under-rate every word containing it.
const MATERIAL_SCORES: &[(&str, f64)] = &[
    ("wood", 1.0),
    ("wood0", 0.0),
    ("bronze", 1.5),
    ("silver2", 2.0),
    ("silver3", 3.0),
    ("gold", 10.0),
    ("bad", 0.0),
];

pub struct Scorer {
    /// Letter (uppercase) → material name, read from the game's own data file.
    letter_material: HashMap<char, String>,
    material_score: HashMap<String, f64>,
    unknown: std::cell::RefCell<Vec<String>>,
}

impl Scorer {
    /// Reads the letter → material table from the game directory.
    ///
    /// `rpg/effects/material/default.lua` assigns `local letterMaterials = { … }` rather than
    /// returning it, so `return letterMaterials` is appended before parsing it as data.
    pub fn new(game_dir: &Path) -> Result<Self, crate::Error> {
        let path = game_dir.join("rpg/effects/material/default.lua");
        let src = std::fs::read_to_string(&path)?;
        // Only the leading table literal is wanted; the rest of the file builds draw data.
        let cut = src.find("\n}").map(|i| i + 2).unwrap_or(src.len());
        let table = crate::game::save::parse(&format!("{}\nreturn letterMaterials", &src[..cut]))?;

        let mut letter_material = HashMap::new();
        for (k, v) in &table.map {
            if let (Some(c), Some(m)) = (k.chars().next(), v.as_str()) {
                letter_material.insert(c.to_ascii_uppercase(), m.to_string());
            }
        }
        if letter_material.is_empty() {
            return Err(crate::Error::Config(format!(
                "no letter materials parsed from {}",
                path.display()
            )));
        }
        Ok(Scorer {
            letter_material,
            material_score: MATERIAL_SCORES.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            unknown: std::cell::RefCell::new(Vec::new()),
        })
    }

    /// Base score of a single letter, before length and tile state.
    pub fn letter_score(&self, letter: char) -> f64 {
        let up = letter.to_ascii_uppercase();
        let Some(material) = self.letter_material.get(&up) else {
            self.unknown.borrow_mut().push(format!("letter {up}"));
            return 1.0;
        };
        match self.material_score.get(material) {
            Some(s) => *s,
            None => {
                self.unknown.borrow_mut().push(format!("material {material}"));
                1.0
            }
        }
    }

    /// The game's length multiplier: `0.2 * (len + 1)` (`utils/words.lua:215-217`).
    pub fn length_score(len: usize) -> f64 {
        0.2 * (len as f64 + 1.0)
    }

    /// Scores a word from its letters alone, assuming plain tiles.
    pub fn score_word(&self, word: &str) -> i64 {
        let sum: f64 = word.chars().map(|c| self.letter_score(c)).sum();
        (sum * Self::length_score(word.chars().count()) + 0.5).floor() as i64
    }

    /// Scores a word against the specific tiles it will consume, honouring tile state.
    pub fn score_tiles(&self, tiles: &[Tile]) -> i64 {
        let sum: f64 = tiles
            .iter()
            .map(|t| {
                if t.burn().unwrap_or(0) > 0 {
                    0.0
                } else {
                    t.letter.chars().map(|c| self.letter_score(c)).sum()
                }
            })
            .sum();
        (sum * Self::length_score(tiles.len()) + 0.5).floor() as i64
    }

    /// Would this score kill the enemy?
    ///
    /// The captured crypt save has `rpg.enemy.armour` **absent** rather than 0, so callers must map a
    /// missing key to zero; the signature takes a plain `i64` to make that the caller's explicit
    /// `unwrap_or(0)` rather than a hidden default here.
    pub fn is_lethal(score: i64, enemy_health: i64, enemy_armour: i64) -> bool {
        score >= enemy_health + enemy_armour
    }

    /// Letters or materials encountered that this scorer does not know.
    ///
    /// Should be empty. Non-empty means the game gained a material and every word using it is being
    /// under-rated — drift that is invisible unless something reports it.
    pub fn unknown_materials(&self) -> Vec<String> {
        let mut v = self.unknown.borrow().clone();
        v.sort();
        v.dedup();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn game_dir() -> PathBuf {
        PathBuf::from("../sternly-worded-adventures")
    }

    /// Skips loudly when the game source is absent, so the suite runs elsewhere without a silent pass.
    fn scorer() -> Option<Scorer> {
        if !game_dir().join("rpg/effects/material/default.lua").is_file() {
            eprintln!("SKIP: game source not present at {}", game_dir().display());
            return None;
        }
        Some(Scorer::new(&game_dir()).expect("letter materials should load"))
    }

    fn plain(word: &str) -> Vec<Tile> {
        word.chars().map(|c| Tile { letter: c.to_string(), extra: None }).collect()
    }

    #[test]
    fn reads_the_letter_materials_from_the_game() {
        let Some(s) = scorer() else { return };
        // Spot values straight out of `rpg/effects/material/default.lua`.
        assert_eq!(s.letter_score('E'), 1.0, "E is wood");
        assert_eq!(s.letter_score('C'), 1.5, "C is bronze");
        assert_eq!(s.letter_score('G'), 2.0, "G is silver2");
        assert_eq!(s.letter_score('W'), 3.0, "W is silver3");
        assert_eq!(s.letter_score('J'), 10.0, "J is gold");
        assert!(s.unknown_materials().is_empty(), "unknown: {:?}", s.unknown_materials());
    }

    #[test]
    fn matches_a_hand_computed_score() {
        // GRAPH on the real crypt board: G(2) R(1) A(1) P(1.5) H(2) = 7.5; length 5 -> 0.2*6 = 1.2;
        // 7.5*1.2 = 9.0; floor(9.0+0.5) = 9. Worked by hand from the game's formula, so this pins the
        // arithmetic rather than restating the implementation.
        let Some(s) = scorer() else { return };
        assert_eq!(s.score_word("GRAPH"), 9);
    }

    #[test]
    fn length_multiplier_follows_the_game() {
        // Epsilon rather than exact: `0.2 * 6` is 1.2000000000000002 in IEEE doubles. Deliberately
        // NOT "corrected" to exact arithmetic — Lua computes it in doubles too, so reproducing the
        // float behaviour is what keeps us in step with the game at the floor() boundary.
        for (len, want) in [(1usize, 0.4), (5, 1.2), (9, 2.0)] {
            let got = Scorer::length_score(len);
            assert!((got - want).abs() < 1e-9, "length {len}: got {got}, want {want}");
        }
    }

    #[test]
    fn a_longer_word_of_the_same_letters_scores_higher() {
        let Some(s) = scorer() else { return };
        assert!(s.score_word("CATTLE") > s.score_word("CAT"));
    }

    #[test]
    fn a_burning_tile_contributes_nothing() {
        // `utils/tiles.lua:56-57` zeroes a burning tile. Honoured because ignoring it would
        // OVER-estimate, and an over-estimate types a word that fails to kill.
        let Some(s) = scorer() else { return };
        let mut tiles = plain("JJ");
        let mut extra = crate::game::save::Table::default();
        extra.map.insert("burn".into(), crate::game::save::Value::Int(3));
        tiles[1].extra = Some(extra);
        // J(10) + burning J(0) = 10; length 2 -> 0.6; floor(6.0+0.5) = 6.
        assert_eq!(s.score_tiles(&tiles), 6);
        assert!(s.score_tiles(&tiles) < s.score_tiles(&plain("JJ")));
    }

    #[test]
    fn lethality_treats_absent_armour_as_zero() {
        // The captured crypt save had `rpg.enemy.armour` ABSENT, not 0.
        assert!(Scorer::is_lethal(9, 3, 0), "9 damage kills 3 health");
        assert!(!Scorer::is_lethal(2, 3, 0));
        assert!(!Scorer::is_lethal(9, 3, 7), "armour absorbs first");
    }

    #[test]
    fn the_real_crypt_board_can_kill_the_real_enemy() {
        // End to end on captured data: board O Y C A A C T P O R L I G A H J versus "Amorphous",
        // health 3, armour absent.
        let Some(s) = scorer() else { return };
        let score = s.score_word("GRAPH");
        assert!(
            Scorer::is_lethal(score, 3, 0),
            "GRAPH scores {score}, which should kill a 3-health enemy"
        );
    }
}
