//! Scoring words.
//!
//! ## Why this is a reimplementation and not the game's own code
//!
//! Running `words.score` through mlua got as far as loading `utils/words.lua` and its 78 modifier
//! files, via a custom `require`, a `love.filesystem` shim, a `utf8` shim, a `buildDrawDataTable`
//! stub, a deep `love` stub, and per-file `pcall` loading of the modifiers, before the engine
//! dependencies made it not worth continuing. Version drift being acceptable, this computes the
//! score directly — but it reads the game's **data** rather than restating it: letter materials,
//! material scores, border scores and ligature bonuses all come from the game's own files.
//!
//! ## The formula
//!
//! ```text
//! words.score       (utils/words.lua:292-302)  floor((sum + add) * mult * lengthScore + 0.5 + postAdd)
//! words.lengthScore (utils/words.lua:215-217)  0.2 * (#word + 1)
//! tiles.score       (utils/tiles.lua:45-95)    per tile, below
//! ```
//!
//! Two details in there cost real damage if you get them wrong, and an earlier version got both:
//!
//! - **`lengthScore` counts LETTERS, not tiles.** `words.score` is handed the word *string*. A
//!   three-letter `ING` tile lengthens the word by three while occupying one tile.
//! - **`default.getScore` checks the letter before the material**: `score[letter] or
//!   score[letterMaterials[letter]] or 1` (`rpg/effects/material/default.lua:47-49`). The `score`
//!   table carries `X = 20, J = 30, Q = 40` alongside the material entries, so those three letters
//!   are worth far more than the `gold = 10` their material implies. Scoring J as 10 under-rates
//!   every word containing it by twenty points before the length multiplier.
//!
//! That same fallthrough is why a **ligature tile scores 1**: `score["TH"]` and
//! `letterMaterials["TH"]` are both absent, so the `or 1` catches it. A ligature is worth less than
//! the letters it spells, not more.
//!
//! ## Known divergence, stated rather than discovered later
//!
//! - **Gear `wordBonus` modifiers are ignored** — ~26 files adjusting `mult`/`add`/`postAdd` under
//!   conditions. At MVP the player's `gear` is empty; as gear accumulates this under-estimates.
//! - **Enemy lexicon bonuses are ignored** (see [`crate::lexica`], which handles the resist half,
//!   the half that can reach zero).
//! - **`bonusBurnTileMult` gear is not read**, so a burning tile is scored at zero rather than half.
//!
//! Every one of those pushes the same way: this can under-rate a word, not over-rate one. That
//! direction is chosen deliberately. An over-estimate types a word that fails to kill and hands the
//! enemy a free turn; an under-estimate only costs search.

use crate::game::save::{parse, parse_module, Table};
use crate::observe::board::Tile;
use std::collections::HashMap;
use std::path::Path;

/// The ratio `tiles.score` blends a material with a border by (`utils/tiles.lua:7`).
const MAT_BORDER_RATIO: f64 = 0.75;

/// A material's contribution, as its data file declares it.
#[derive(Debug, Clone, Default)]
struct Material {
    score: Option<f64>,
    score_mult: Option<f64>,
}

pub struct Scorer {
    /// `letterMaterials` — letter (uppercase) → material name.
    letter_material: HashMap<String, String>,
    /// The `score` table from `default.lua`, keyed by **material names and specific letters alike**.
    default_score: HashMap<String, f64>,
    /// Material name → its file's `score`/`scoreMult`.
    materials: HashMap<String, Material>,
    /// Border name → `score`. `iron` has none, and a border without a score contributes nothing.
    borders: HashMap<String, Option<f64>>,
    /// Ligature effect name → `scoreAdd`.
    ligature_bonus: HashMap<String, f64>,
    /// Mutex rather than RefCell: the search shares `&Scorer` across threads, and RefCell is not
    /// Sync. Contention is irrelevant because this only ever records a drift warning.
    unknown: std::sync::Mutex<Vec<String>>,
}

impl Scorer {
    /// Loads every scoring table the game keeps on disk.
    pub fn new(game_dir: &Path) -> Result<Self, crate::Error> {
        let (letter_material, default_score) = load_default_material(game_dir)?;
        let materials = load_materials(game_dir)?;
        let borders = load_scored_dir(game_dir.join("rpg/effects/border"))?;
        let ligature_bonus = load_ligature_bonuses(game_dir)?;

        if letter_material.is_empty() || default_score.is_empty() {
            return Err(crate::Error::Config(
                "rpg/effects/material/default.lua yielded no letter materials".into(),
            ));
        }
        Ok(Scorer {
            letter_material,
            default_score,
            materials,
            borders,
            ligature_bonus,
            unknown: std::sync::Mutex::new(Vec::new()),
        })
    }

    fn note_unknown(&self, what: String) {
        if let Ok(mut v) = self.unknown.lock() {
            if !v.contains(&what) {
                v.push(what);
            }
        }
    }

    /// `default.getScore(letter)` — `score[letter] or score[letterMaterials[letter]] or 1`.
    ///
    /// Takes the whole tile letter, which may be a ligature. The `or 1` is not a fallback for our
    /// ignorance; it is the game's own answer for anything it has no entry for.
    pub fn letter_score(&self, letter: &str) -> f64 {
        let up = letter.to_ascii_uppercase();
        if let Some(s) = self.default_score.get(&up) {
            return *s;
        }
        match self.letter_material.get(&up) {
            Some(m) => match self.default_score.get(m) {
                Some(s) => *s,
                None => {
                    self.note_unknown(format!("material {m}"));
                    1.0
                }
            },
            // Ligatures and punctuation land here, exactly as they do in the game.
            None => 1.0,
        }
    }

    /// The game's length multiplier: `0.2 * (#word + 1)`, over the word's **letters**.
    pub fn length_score(letters: usize) -> f64 {
        0.2 * (letters as f64 + 1.0)
    }

    /// One tile's contribution, following `tiles.score` (`utils/tiles.lua:45-95`).
    pub fn tile_score(&self, tile: &Tile) -> f64 {
        let bg = tile.extra.as_ref().and_then(|e| e.str_at("bg"));
        // `materials[tile.extra.bg] or default` -- only `default` carries getScore/getMaterial.
        let bg_material = bg.and_then(|b| {
            let m = self.materials.get(b);
            if m.is_none() {
                self.note_unknown(format!("material {b}"));
            }
            m
        });

        // getMaterialData: an explicit bg wins; otherwise default.getMaterial maps the letter.
        let material = match bg_material {
            Some(m) => Some(m),
            None => self
                .letter_material
                .get(&tile.letter.to_ascii_uppercase())
                .and_then(|name| self.materials.get(name)),
        };

        let mut score_mult = material
            .and_then(|m| m.score_mult)
            .or_else(|| bg_material.and_then(|m| m.score_mult))
            .unwrap_or(1.0);

        // A burning tile is worth nothing. Modelled because ignoring it would OVER-estimate.
        if tile.burn().is_some() {
            score_mult = 0.0;
        }
        if let Some(carbon) = tile.extra.as_ref().and_then(|e| e.int_at("carbon")) {
            score_mult *= 0.9f64.powi(carbon as i32);
        }
        if score_mult == 0.0 {
            return 0.0;
        }

        // `if rawMaterial.getScore` -- true only when bg is unset, i.e. rawMaterial is `default`.
        let material_score = match bg_material {
            Some(m) => m.score,
            None => Some(self.letter_score(&tile.letter)),
        };

        let border_score = tile
            .extra
            .as_ref()
            .and_then(|e| e.str_at("border"))
            .and_then(|b| match self.borders.get(b) {
                Some(s) => *s,
                None => {
                    self.note_unknown(format!("border {b}"));
                    None
                }
            });

        let mut score = match (material_score, border_score) {
            (Some(m), Some(b)) => m * MAT_BORDER_RATIO + b * (1.0 - MAT_BORDER_RATIO),
            (m, b) => m.unwrap_or(1.0) + b.unwrap_or(0.0),
        };
        if let Some(add) = tile
            .extra
            .as_ref()
            .and_then(|e| e.str_at("ligature"))
            .and_then(|l| self.ligature_bonus.get(l))
        {
            score += add;
        }
        score * score_mult
    }

    /// Scores a word from its letters alone, assuming plain untouched tiles.
    ///
    /// Only correct when the tiles really are plain; [`Scorer::score_typed`] is what the search uses,
    /// because it knows which tiles the word will actually consume.
    pub fn score_word(&self, word: &str) -> i64 {
        let tiles: Vec<Tile> =
            word.chars().map(|c| Tile { letter: c.to_string(), extra: None }).collect();
        self.score_typed(&tiles, word.chars().count(), 1.0)
    }

    /// The real thing: the tiles a typed word consumes, the word's letter count, and the enemy's
    /// multiplier.
    ///
    /// `modifier` is `mult * nerf` from `words.getWordBonusModifier` — for us, the
    /// `resistCornerless` fraction. It multiplies the whole sum, so a zero there is a zero score no
    /// matter how good the letters are.
    pub fn score_typed(&self, tiles: &[Tile], letters: usize, modifier: f64) -> i64 {
        let sum: f64 = tiles.iter().map(|t| self.tile_score(t)).sum();
        (sum * modifier * Self::length_score(letters) + 0.5).floor() as i64
    }

    /// Would this score kill the enemy?
    ///
    /// The captured crypt save has `rpg.enemy.armour` **absent** rather than 0, so callers must map
    /// a missing key to zero; the signature takes a plain `i64` to make that the caller's explicit
    /// `unwrap_or(0)` rather than a hidden default here.
    pub fn is_lethal(score: i64, enemy_health: i64, enemy_armour: i64) -> bool {
        score >= enemy_health + enemy_armour
    }

    /// Materials, borders or letters encountered that this scorer does not know.
    ///
    /// Should be empty. Non-empty means the game gained something and words using it are being
    /// mis-rated — drift that is invisible unless something reports it.
    pub fn unknown_materials(&self) -> Vec<String> {
        let mut v = self.unknown.lock().map(|g| g.clone()).unwrap_or_default();
        v.sort();
        v.dedup();
        v
    }
}

/// Reads `letterMaterials` and `score` out of `default.lua`.
///
/// Both are file-locals under a table of functions, so the literal is cut at the module's own
/// `return` and a data-only return is appended.
fn load_default_material(game_dir: &Path) -> Result<(HashMap<String, String>, HashMap<String, f64>), crate::Error> {
    let path = game_dir.join("rpg/effects/material/default.lua");
    let src = std::fs::read_to_string(&path)?;
    let cut = src.find("\nreturn").ok_or_else(|| {
        crate::Error::Config(format!("{} has no return statement", path.display()))
    })?;
    let table = parse(&format!(
        "{}\nreturn {{ letterMaterials = letterMaterials, score = score }}",
        &src[..cut]
    ))?;

    let mut letter_material = HashMap::new();
    if let Some(t) = table.table_at("letterMaterials") {
        for (k, v) in &t.map {
            if let Some(m) = v.as_str() {
                letter_material.insert(k.to_ascii_uppercase(), m.to_string());
            }
        }
    }
    // Keys are taken VERBATIM. The `score` table mixes uppercase letter keys (`X`, `J`, `Q`) with
    // lowercase material names (`wood`, `gold`), and Lua indexes it case-sensitively with both.
    // Normalising the case collapses the two namespaces and every letter falls through to the `or 1`.
    let mut score = HashMap::new();
    if let Some(t) = table.table_at("score") {
        for (k, v) in &t.map {
            if let Some(n) = v.as_f64() {
                score.insert(k.clone(), n);
            }
        }
    }
    Ok((letter_material, score))
}

fn load_materials(game_dir: &Path) -> Result<HashMap<String, Material>, crate::Error> {
    let mut out = HashMap::new();
    for (name, table) in read_lua_dir(game_dir.join("rpg/effects/material"))? {
        if name == "default" {
            continue;
        }
        out.insert(
            name,
            Material {
                score: table.path("score").and_then(|v| v.as_f64()),
                score_mult: table.path("scoreMult").and_then(|v| v.as_f64()),
            },
        );
    }
    Ok(out)
}

fn load_scored_dir(dir: std::path::PathBuf) -> Result<HashMap<String, Option<f64>>, crate::Error> {
    Ok(read_lua_dir(dir)?
        .into_iter()
        .map(|(name, t)| (name, t.path("score").and_then(|v| v.as_f64())))
        .collect())
}

fn load_ligature_bonuses(game_dir: &Path) -> Result<HashMap<String, f64>, crate::Error> {
    Ok(read_lua_dir(game_dir.join("rpg/effects/ligature"))?
        .into_iter()
        .filter_map(|(name, t)| Some((name, t.path("scoreAdd")?.as_f64()?)))
        .collect())
}

/// Loads every `.lua` in a directory as data, keyed by `data.key` or the file stem — the same rule
/// `enumerate.hashRequire` uses (`utils/enumerate.lua:60-74`).
fn read_lua_dir(dir: std::path::PathBuf) -> Result<Vec<(String, Table)>, crate::Error> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir)?.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("lua") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        let src = std::fs::read_to_string(&path)?;
        // A file that will not parse is skipped rather than fatal: the directories hold draw data
        // and effect code beside the numbers, and one unreadable entry must not take the run down.
        let Ok(table) = parse_module(&src) else { continue };
        let key = table.str_at("key").unwrap_or(stem).to_string();
        out.push((key, table));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::save::Value;
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
        Some(Scorer::new(&game_dir()).expect("scoring tables should load"))
    }

    fn tile(letter: &str) -> Tile {
        Tile { letter: letter.into(), extra: None }
    }

    fn with(letter: &str, key: &str, value: Value) -> Tile {
        let mut extra = Table::default();
        extra.map.insert(key.into(), value);
        Tile { letter: letter.into(), extra: Some(extra) }
    }

    #[test]
    fn reads_the_letter_materials_from_the_game() {
        let Some(s) = scorer() else { return };
        assert_eq!(s.letter_score("E"), 1.0, "E is wood");
        assert_eq!(s.letter_score("C"), 1.5, "C is bronze");
        assert_eq!(s.letter_score("G"), 2.0, "G is silver2");
        assert_eq!(s.letter_score("W"), 3.0, "W is silver3");
        assert_eq!(s.letter_score("Z"), 10.0, "Z is gold, with no letter entry of its own");
        assert!(s.unknown_materials().is_empty(), "unknown: {:?}", s.unknown_materials());
    }

    #[test]
    fn the_three_premium_letters_beat_their_material() {
        // `default.getScore` is `score[letter] or score[letterMaterials[letter]] or 1`, and the
        // score table carries X/J/Q entries ABOVE the gold material's 10
        // (`rpg/effects/material/default.lua:31-42`). An earlier version returned 10 for all three,
        // under-rating every word containing them -- by 30 raw points for a Q, before length.
        let Some(s) = scorer() else { return };
        assert_eq!(s.letter_score("X"), 20.0);
        assert_eq!(s.letter_score("J"), 30.0);
        assert_eq!(s.letter_score("Q"), 40.0);
        assert!(s.letter_score("J") > s.letter_score("Z"), "J beats a plain gold letter");
    }

    #[test]
    fn a_ligature_tile_scores_one_not_the_sum_of_its_letters() {
        // Neither `score["TH"]` nor `letterMaterials["TH"]` exists, so the `or 1` catches it. Summing
        // T(1) + H(2) would over-rate the tile by two -- and over-rating is the direction that gets
        // the player killed.
        let Some(s) = scorer() else { return };
        assert_eq!(s.letter_score("TH"), 1.0);
        assert_eq!(s.letter_score("ING"), 1.0);
        assert!(s.letter_score("TH") < s.letter_score("T") + s.letter_score("H"));
    }

    #[test]
    fn matches_a_hand_computed_score() {
        // GRAPH: G(2) R(1) A(1) P(1.5) H(2) = 7.5; length 5 -> 0.2*6 = 1.2; 7.5*1.2 = 9.0;
        // floor(9.0+0.5) = 9. Worked by hand from the game's formula, so this pins the arithmetic
        // rather than restating the implementation.
        let Some(s) = scorer() else { return };
        assert_eq!(s.score_word("GRAPH"), 9);
    }

    #[test]
    fn length_multiplier_follows_the_game() {
        // Epsilon rather than exact: `0.2 * 6` is 1.2000000000000002 in IEEE doubles. Deliberately
        // NOT "corrected" to exact arithmetic -- Lua computes it in doubles too, so reproducing the
        // float behaviour is what keeps us in step at the floor() boundary.
        for (len, want) in [(1usize, 0.4), (5, 1.2), (9, 2.0)] {
            let got = Scorer::length_score(len);
            assert!((got - want).abs() < 1e-9, "length {len}: got {got}, want {want}");
        }
    }

    #[test]
    fn length_counts_letters_not_tiles() {
        // `words.score` is handed the word STRING, so an ING tile lengthens the word by three while
        // occupying one tile. Scoring by tile count would shrink the multiplier from 0.2*(5+1) to
        // 0.2*(3+1) -- a third of the damage lost on every ligature word.
        let Some(s) = scorer() else { return };
        let tiles = [tile("K"), tile("ING"), tile("S")];
        let by_letters = s.score_typed(&tiles, 5, 1.0);
        let by_tiles = s.score_typed(&tiles, 3, 1.0);
        assert!(by_letters > by_tiles, "{by_letters} vs {by_tiles}");
        // K(3) + ING(1) + S(1) = 5; length 5 -> 1.2; 6.0; floor(6.5) = 6.
        assert_eq!(by_letters, 6);
    }

    #[test]
    fn a_burning_tile_contributes_nothing() {
        // `utils/tiles.lua:56-57` zeroes scoreMult for a burning tile.
        let Some(s) = scorer() else { return };
        let tiles = [tile("J"), with("J", "burn", Value::Int(3))];
        // J(30) + burning J(0) = 30; length 2 -> 0.6; 18.0; floor(18.5) = 18.
        assert_eq!(s.score_typed(&tiles, 2, 1.0), 18);
        assert!(s.score_typed(&tiles, 2, 1.0) < s.score_typed(&[tile("J"), tile("J")], 2, 1.0));
    }

    #[test]
    fn an_explicit_material_overrides_the_letters_own() {
        // With `extra.bg` set, `rawMaterial` is that material and its `getScore` is absent, so the
        // letter's premium does not apply -- a gold-backed J scores 10, not 30.
        let Some(s) = scorer() else { return };
        assert_eq!(s.tile_score(&with("J", "bg", Value::Str("gold".into()))), 10.0);
        assert_eq!(s.tile_score(&tile("J")), 30.0, "without bg, the letter entry wins");
        assert_eq!(s.tile_score(&with("E", "bg", Value::Str("gold".into()))), 10.0);
    }

    #[test]
    fn a_scored_border_blends_with_the_material() {
        // `utils/tiles.lua:78-82`: when both exist the score is 0.75 material + 0.25 border. A plain
        // tile has no border at all and takes the additive branch, which is why plain letters score
        // exactly their material.
        let Some(s) = scorer() else { return };
        let mut extra = Table::default();
        extra.map.insert("bg".into(), Value::Str("wood".into()));
        extra.map.insert("border".into(), Value::Str("gold".into()));
        let t = Tile { letter: "E".into(), extra: Some(extra) };
        // wood(1)*0.75 + gold border(10)*0.25 = 3.25
        assert!((s.tile_score(&t) - 3.25).abs() < 1e-9, "got {}", s.tile_score(&t));
    }

    #[test]
    fn an_unscored_border_leaves_the_material_alone() {
        // `rpg/effects/border/iron.lua` declares no score, so it takes the additive branch and adds
        // nothing. This is also why plain tiles -- which carry no border key -- score cleanly.
        let Some(s) = scorer() else { return };
        let mut extra = Table::default();
        extra.map.insert("bg".into(), Value::Str("wood".into()));
        extra.map.insert("border".into(), Value::Str("iron".into()));
        let t = Tile { letter: "E".into(), extra: Some(extra) };
        assert_eq!(s.tile_score(&t), 1.0);
    }

    #[test]
    fn the_enemy_modifier_scales_the_whole_word() {
        // `getWordBonusModifier` returns `mult*nerf`, and `words.score` multiplies the summed tiles
        // by it. A nerf of zero -- an immune lexicon, or a corner-resistant enemy the word missed --
        // is a zero score however good the letters were.
        let Some(s) = scorer() else { return };
        let full = s.score_word("GRAPH");
        let tiles: Vec<Tile> = "GRAPH".chars().map(|c| tile(&c.to_string())).collect();
        assert_eq!(s.score_typed(&tiles, 5, 1.0), full);
        assert!(s.score_typed(&tiles, 5, 0.5) < full);
        assert_eq!(s.score_typed(&tiles, 5, 0.0), 0, "a zero nerf means zero damage");
    }

    #[test]
    fn lethality_treats_absent_armour_as_zero() {
        assert!(Scorer::is_lethal(9, 3, 0), "9 damage kills 3 health");
        assert!(!Scorer::is_lethal(2, 3, 0));
        assert!(!Scorer::is_lethal(9, 3, 7), "armour absorbs first");
    }

    #[test]
    fn every_material_the_letters_name_is_known() {
        // A letter mapped to a material we cannot score would silently fall back to 1. Checking the
        // whole alphabet means a new material shows up here rather than as quiet under-damage.
        let Some(s) = scorer() else { return };
        for c in 'A'..='Z' {
            let v = s.letter_score(&c.to_string());
            assert!(v > 0.0, "{c} scored {v}");
        }
        assert!(s.unknown_materials().is_empty(), "unknown: {:?}", s.unknown_materials());
    }
}
