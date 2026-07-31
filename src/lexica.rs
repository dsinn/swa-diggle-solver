//! Lexica: themed word lists that some enemies resist or are immune to.
//!
//! This is a **correctness** concern, not a refinement. `words.getWordBonusModifier`
//! (`utils/words.lua:219-242`) walks the enemy's statuses and, for each one naming a lexicon the word
//! belongs to:
//!
//! ```lua
//! if val > 1 then mult = mult + val - 1   -- bonus
//! else            nerf = nerf * val        -- resist, and val may be 0
//! ```
//!
//! `val = 0` means **immune**: `nerf * 0` makes the word score zero. So playing a bone word against a
//! bone-immune enemy does no damage at all and wastes the turn. Ignoring lexica is therefore safe in
//! one direction only — never claiming a bonus we do not have — and unsafe in the other.
//!
//! ## The chain, and why it needs a `require` shim
//!
//! `utils/dictionaries/bone.lua` is a descriptor: `key = 'bone'`, `subkey = 'Bone'`,
//! `lexicon = require'utils.lexica.bones'`. `utils/dictionaries.lua:23` then derives the status key as
//! `'lexiconBonus' .. (subkey or key)` — so `lexiconBonusBone`. The descriptor cannot be read as plain
//! data because of that `require`, so it is evaluated with a `require` that simply returns the module
//! name, which turns the call into the string we actually want.
//!
//! Lexicon files themselves are plain data: `return { abarthrosis=1, acanthous=2, … }`, lowercase keys.

use crate::game::save::{parse, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// A themed word list and the enemy-status key that modifies it.
pub struct Lexicon {
    /// e.g. `lexiconBonusBone` — the key that appears in `rpg.enemy.statusEffects`.
    pub status_key: String,
    /// Uppercased words, to match the search's representation.
    pub words: HashSet<String>,
}

pub struct Lexica {
    lexicons: Vec<Lexicon>,
    /// Descriptors that could not be read, so a missing lexicon is visible rather than assumed absent.
    problems: Vec<String>,
}

impl Lexica {
    pub fn load(game_dir: &Path) -> Result<Self, crate::Error> {
        let dir = game_dir.join("utils/dictionaries");
        let mut lexicons = Vec::new();
        let mut problems = Vec::new();
        let entries = std::fs::read_dir(&dir)?;
        for e in entries.filter_map(|e| e.ok()) {
            let path = e.path();
            if path.extension().and_then(|s| s.to_str()) != Some("lua") {
                continue;
            }
            match Self::load_one(game_dir, &path) {
                Ok(Some(l)) => lexicons.push(l),
                Ok(None) => {}
                Err(err) => problems.push(format!("{}: {err}", path.display())),
            }
        }
        Ok(Lexica { lexicons, problems })
    }

    fn load_one(game_dir: &Path, descriptor: &Path) -> Result<Option<Lexicon>, crate::Error> {
        let src = std::fs::read_to_string(descriptor)?;
        // `require'utils.lexica.bones'` becomes the string "utils.lexica.bones", so the descriptor
        // parses as data and tells us which lexicon file to read.
        let shimmed = format!("local function require(n) return n end\n{src}");
        let table = parse(&shimmed)?;
        let Some(key) = table.str_at("key") else { return Ok(None) };
        // `utils/dictionaries.lua:23`: lexiconBonusKey defaults to 'lexiconBonus'..(subkey or key).
        let suffix = table.str_at("subkey").unwrap_or(key);
        let status_key = table
            .str_at("lexiconBonusKey")
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("lexiconBonus{suffix}"));
        let Some(module) = table.str_at("lexicon") else { return Ok(None) };

        let rel = module.replace('.', "/");
        let lex_path = game_dir.join(format!("{rel}.lua"));
        let lex_src = std::fs::read_to_string(&lex_path)?;
        let lex = parse(&lex_src)?;
        let words: HashSet<String> = lex.map.keys().map(|w| w.to_ascii_uppercase()).collect();
        if words.is_empty() {
            return Err(crate::Error::Config(format!("empty lexicon {}", lex_path.display())));
        }
        Ok(Some(Lexicon { status_key, words }))
    }

    pub fn len(&self) -> usize {
        self.lexicons.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lexicons.is_empty()
    }

    /// Descriptors that failed to load. Must be inspected: a lexicon we failed to read is one whose
    /// immunity we cannot honour, so words from it could be played into a zero-damage turn.
    pub fn problems(&self) -> &[String] {
        &self.problems
    }

    pub fn status_keys(&self) -> Vec<&str> {
        self.lexicons.iter().map(|l| l.status_key.as_str()).collect()
    }

    /// Words to exclude from the search, given the enemy's status effects.
    ///
    /// Excludes any word belonging to a lexicon whose status value is `<= 1` — resist or immune.
    /// Exclusion rather than score-scaling because the MVP question is only "does this kill", and a
    /// resisted word's reduced score is not worth modelling when skipping it is both simpler and safe.
    /// Values `> 1` are bonuses and are left alone: we never *rely* on them, so the word stays
    /// eligible on its unbonused score.
    pub fn excluded_words(&self, statuses: &HashMap<String, f64>) -> HashSet<String> {
        let mut out = HashSet::new();
        for lex in &self.lexicons {
            if let Some(v) = statuses.get(&lex.status_key) {
                if *v <= 1.0 {
                    out.extend(lex.words.iter().cloned());
                }
            }
        }
        out
    }

    /// Reads `rpg.enemy.statusEffects` into a flat map.
    pub fn statuses_from_save(save: &crate::game::save::Table) -> HashMap<String, f64> {
        let mut out = HashMap::new();
        if let Some(t) = save.table_at("rpg.enemy.statusEffects") {
            for (k, v) in &t.map {
                match v {
                    Value::Int(i) => {
                        out.insert(k.clone(), *i as f64);
                    }
                    Value::Num(n) => {
                        out.insert(k.clone(), *n);
                    }
                    Value::Bool(b) => {
                        out.insert(k.clone(), if *b { 1.0 } else { 0.0 });
                    }
                    _ => {}
                }
            }
        }
        out
    }

    /// Enemy statuses that change a word's score and that nothing here accounts for.
    ///
    /// Currently empty by construction: the only non-lexicon score status in the game is
    /// `resistCornerless`, and [`crate::search::Modifiers`] now models it from the board geometry.
    /// The function stays because the list is a property of the game, not of us — a new status
    /// belongs here the day it appears, and an empty return should mean "checked", not "never
    /// looked".
    pub fn unmodelled(&self, statuses: &HashMap<String, f64>) -> Vec<String> {
        let known: HashSet<&str> = self.status_keys().into_iter().collect();
        let mut out: Vec<String> = statuses
            .keys()
            .filter(|k| !known.contains(k.as_str()) && Self::affects_score_unmodelled(k))
            .cloned()
            .collect();
        out.sort();
        out
    }

    /// Statuses that change a word's score other than through a lexicon, and that we do not model.
    fn affects_score_unmodelled(_key: &str) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn game_dir() -> PathBuf {
        PathBuf::from("../sternly-worded-adventures")
    }

    fn present() -> bool {
        game_dir().join("utils/dictionaries/bone.lua").is_file()
    }

    #[test]
    fn loads_the_lexica_with_their_status_keys() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        let lx = Lexica::load(&game_dir()).unwrap();
        assert!(lx.problems().is_empty(), "problems: {:?}", lx.problems());
        assert!(lx.len() >= 8, "expected several lexica, got {}", lx.len());
        // Derived per `utils/dictionaries.lua:23` from subkey 'Bone'.
        assert!(
            lx.status_keys().contains(&"lexiconBonusBone"),
            "keys: {:?}",
            lx.status_keys()
        );
    }

    #[test]
    fn an_immune_enemy_excludes_that_lexicons_words() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        let lx = Lexica::load(&game_dir()).unwrap();
        // val = 0 is immunity: `nerf * 0` zeroes the score, so these words do NOTHING.
        let statuses = HashMap::from([("lexiconBonusBone".to_string(), 0.0)]);
        let excluded = lx.excluded_words(&statuses);
        assert!(!excluded.is_empty(), "bone words must be excluded when immune");
        // A word from the bone lexicon, verified present in `utils/lexica/bones.lua`.
        assert!(excluded.contains("AITCHBONE"), "a known bone word must be excluded");
    }

    #[test]
    fn a_bonus_lexicon_is_not_excluded() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        let lx = Lexica::load(&game_dir()).unwrap();
        // val > 1 is a BONUS. We never rely on it, so the word stays eligible on its plain score --
        // excluding it would throw away perfectly good words.
        let statuses = HashMap::from([("lexiconBonusBone".to_string(), 2.0)]);
        assert!(lx.excluded_words(&statuses).is_empty());
    }

    #[test]
    fn no_statuses_excludes_nothing() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        let lx = Lexica::load(&game_dir()).unwrap();
        assert!(lx.excluded_words(&HashMap::new()).is_empty());
    }

    #[test]
    fn the_real_crypt_enemy_has_no_lexicon_immunities() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        // The captured fight: "Amorphous". Its statusEffects must not silently exclude anything, or
        // the level-0 crypt search would be solving a different problem than the one we measured.
        let save = crate::game::save::load(Path::new("tests/fixtures/combatSaveData-crypt-l0.lua"))
            .expect("fixture loads");
        let statuses = Lexica::statuses_from_save(&save);
        let lx = Lexica::load(&game_dir()).unwrap();
        assert!(lx.excluded_words(&statuses).is_empty(), "statuses: {statuses:?}");
        assert!(lx.unmodelled(&statuses).is_empty(), "unmodelled: {:?}", lx.unmodelled(&statuses));
    }

    #[test]
    fn resist_cornerless_is_no_longer_unmodelled() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        // It used to be reported here, because we could not tell which tiles a word would take.
        // `crate::typist` now simulates the game's own selection, so the corner fraction is computed
        // rather than admitted to -- see `crate::search::Modifiers::modifier`.
        let lx = Lexica::load(&game_dir()).unwrap();
        let statuses = HashMap::from([("resistCornerless".to_string(), -1.0)]);
        assert!(lx.unmodelled(&statuses).is_empty());
    }
}
