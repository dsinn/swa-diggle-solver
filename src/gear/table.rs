//! Every `wordBonus` modifier the game declares, and what its condition reads.
//!
//! Transcribed from `rpg/effects/modifiers/*.lua`, one entry per modifier. The constants
//! (`scorePreAdd`, `scoreMult`, `scorePostAdd`, `priority`) are copied verbatim; the conditions and
//! `scoreInstances` become a [`Fact`], because those are arbitrary Lua and cannot be transcribed
//! mechanically.
//!
//! ## Thirty-two, not twenty-six
//!
//! Six are produced by `effects.wordScoreBonusSuffix(...)` (`utils/effects.lua:142-159`) rather than
//! declared in their own files, so a survey of the modifier directory for `type = 'wordBonus'`
//! misses them entirely — which is exactly what happened the first time. `every_modifier_is_known`
//! in this module re-derives the set from the game and fails when it disagrees, so the next one the
//! dev adds is a red test rather than a silent under-estimate.

/// Which of the game's auxiliary dictionaries a word is checked against.
///
/// `utils/dictionaries/*.lua` names the dictionary and points at the lexicon that backs it; the two
/// names differ, and the lexicon name is the one [`crate::lexica`] loads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dict {
    /// `axe` → `utils.lexica.lumberjack`
    Axe,
    /// `fire` → `utils.lexica.fire`
    Fire,
    /// `ice` → `utils.lexica.frost`
    Ice,
    /// `iron` → `utils.lexica.iron`
    Iron,
    /// `metal` → `utils.lexica.metals`
    Metal,
    /// `wood` → `utils.lexica.wood`
    Wood,
}

impl Dict {
    /// The dictionary's key, which is what [`crate::lexica::Lexica::words_for`] takes. **Not** the
    /// lexicon file it loads — `axe` reads `utils.lexica.lumberjack` — so keying on the file name
    /// would miss every one of them.
    pub fn key(self) -> &'static str {
        match self {
            Dict::Axe => "axe",
            Dict::Fire => "fire",
            Dict::Ice => "ice",
            Dict::Iron => "iron",
            Dict::Metal => "metal",
            Dict::Wood => "wood",
        }
    }
}

/// What a modifier's condition looks at.
///
/// One variant per distinct question, not one per modifier — four length modifiers share three
/// variants because they ask different things of the same number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fact {
    /// `l==4 or l==5 or l==6`
    LengthBand456,
    /// `l==7`
    Length7,
    /// `l%2==1`
    LengthOdd,
    /// `l%2==0`
    LengthEven,
    /// `word:find'[cC][iI][eE]' or word:find'[^cC.][eE][iI]'`
    Cie,
    /// `word:find'[pP][oO][gG]'`
    Pog,
    /// `mostCommonCount==1 and not letterHash['.']`
    Heterogram,
    /// `words.isAlphabetical(word)`
    Alphabetical,
    /// `countRuns(word)`, and the count is the instance multiplier.
    Runs,
    /// `cache.wildcards`, and the count is the instance multiplier.
    Wildcards,
    /// `cache.suffix==<this>`, longest match wins.
    Suffix(&'static str),
    /// `dictionaries.X.find(word)`
    InDict(Dict),
    /// `tiles.eachHasCat(wordTiles, 'wood')`
    EachHasWood,
    /// `tileboard.tilesAreAllAdjacent(wordTiles)`
    Adjacent,
    /// `tileboard.countTilesInFullySelectedColumn(wordTiles)`, the count multiplies instances.
    FullColumn,
    /// A quill modifier with no condition of its own — it only feeds the counter.
    Quill,
    /// `cache.quills>0`, and the tally multiplies instances. Runs at priority -999 to seed it.
    QuillCount,
    /// `cache.turn==1`
    QuillTurn1,
    /// `cache.turn % n == 0`
    QuillTurnMod(i64),
    /// `stats.alliterationCount(word)>0`, and the count multiplies instances.
    Alliteration,
    /// `stats.repeatCount(word)>0`; instances are `repeats*(length+1)`.
    RepeatWord,
}

/// One modifier, as the game declares it.
#[derive(Debug, Clone, Copy)]
pub struct ModifierSpec {
    pub flag: &'static str,
    pub pre_add: f64,
    pub mult: f64,
    pub post_add: f64,
    pub priority: i64,
    pub fact: Fact,
    /// Does this modifier feed `cache.quills`? `categories = {quill=true}` in the Lua.
    pub quill_category: bool,
}

const fn m(
    flag: &'static str, pre_add: f64, mult: f64, post_add: f64, priority: i64, fact: Fact,
) -> ModifierSpec {
    ModifierSpec { flag, pre_add, mult, post_add, priority, fact, quill_category: false }
}

const fn quill(
    flag: &'static str, pre_add: f64, mult: f64, priority: i64, fact: Fact,
) -> ModifierSpec {
    ModifierSpec { flag, pre_add, mult, post_add: 0.0, priority, fact, quill_category: true }
}

/// All 32, in no particular order — [`crate::gear::Plan`] sorts by `priority` when it compiles.
pub const MODIFIERS: &[ModifierSpec] = &[
    m("wordScoreBonusPreLength456", 1.0, 0.0, 0.0, 10, Fact::LengthBand456),
    m("wordScoreBonusPostLength7", 0.0, 0.0, 1.0, 10, Fact::Length7),
    m("wordScoreBonusLengthOdd", 0.0, 0.25, 0.0, 10, Fact::LengthOdd),
    m("wordScoreNerfLengthEven", 0.0, -0.25, 0.0, 10, Fact::LengthEven),
    m("wordScoreBonusCIE", 0.0, 0.0, 4.0, 10, Fact::Cie),
    m("wordScoreBonusPOG", 0.0, 0.0, 1.0, 10, Fact::Pog),
    m("wordScoreBonusHeterogram", 0.0, 0.25, 0.0, 9, Fact::Heterogram),
    m("wordScoreBonusAlphabetical", 0.0, 1.0, 0.0, 40, Fact::Alphabetical),
    m("wordScoreBonusRuns", 1.0, 0.0, 0.0, 10, Fact::Runs),
    m("wordScoreBonusWildcards", 0.5, 0.0, 0.0, 10, Fact::Wildcards),
    m("wordScoreBonusEachHasWood", 0.0, 0.05, 0.0, 40, Fact::EachHasWood),
    m("wordScoreBonusAdjacent", 0.0, 0.25, 0.0, 10, Fact::Adjacent),
    m("wordScoreBonusFullColumn", 0.5, 0.0, 0.0, 10, Fact::FullColumn),
    m("wordScoreBonusAlliteration", 0.0, 0.075, 0.0, 10, Fact::Alliteration),
    m("wordScoreBonusRepeatWord", 0.0, 0.1, 0.0, 10, Fact::RepeatWord),
    m("wordScoreBonusInDictAxe", 0.0, 0.05, 0.0, 10, Fact::InDict(Dict::Axe)),
    m("wordScoreBonusInDictFire", 0.0, 0.05, 0.0, 10, Fact::InDict(Dict::Fire)),
    m("wordScoreBonusInDictIce", 0.0, 0.05, 0.0, 10, Fact::InDict(Dict::Ice)),
    m("wordScoreBonusInDictIron", 0.0, 0.05, 0.0, 10, Fact::InDict(Dict::Iron)),
    m("wordScoreBonusInDictMetal", 0.0, 0.05, 0.0, 10, Fact::InDict(Dict::Metal)),
    m("wordScoreBonusInDictWood", 0.0, 0.05, 0.0, 10, Fact::InDict(Dict::Wood)),
    // The suffix six. `priority` is assigned by a counter as `utils/effects.lua` loads, so the exact
    // numbers are load-order dependent; they all sit above 9 and none of them interact, so the
    // relative order among them cannot change a score.
    m("wordScoreBonusSuffixED", 0.0, 0.25, 0.0, 10, Fact::Suffix("ED")),
    m("wordScoreBonusSuffixER", 0.0, 0.25, 0.0, 10, Fact::Suffix("ER")),
    m("wordScoreBonusSuffixES", 0.0, 0.25, 0.0, 10, Fact::Suffix("ES")),
    m("wordScoreBonusSuffixEST", 0.0, 0.25, 0.0, 10, Fact::Suffix("EST")),
    m("wordScoreBonusSuffixING", 0.0, 0.25, 0.0, 10, Fact::Suffix("ING")),
    m("wordScoreBonusSuffixLY", 0.0, 0.25, 0.0, 10, Fact::Suffix("LY")),
    // The quill family. `QuillCount` seeds the tally at priority -999 and reads it back; the other
    // three each add one to it when their own turn condition holds, and contribute a multiplier
    // besides. See `Plan::apply` for why this needs two passes.
    quill("wordScoreBonusQuillCount", 3.0, 0.0, -999, Fact::QuillCount),
    quill("wordScoreBonusQuill1", 1.0, 0.0, 10, Fact::Quill),
    quill("wordScoreBonusQuillTurn1", 0.0, 1.0, 10, Fact::QuillTurn1),
    quill("wordScoreBonusQuillTurnMod12", 0.0, 1.0, 10, Fact::QuillTurnMod(12)),
    quill("wordScoreBonusQuillTurnMod5", 0.0, 0.4, 10, Fact::QuillTurnMod(5)),
];

/// Every suffix the game registers, longest first.
///
/// `cacheSuffix` (`utils/effects.lua:105-117`) scans from the longest candidate down and takes the
/// first hit, so `EST` must be tested before `ES` or `BIGGEST` scores as an `ES` word. The set is
/// the union of what both factories register — `onSubmitSuffixShiftEffect` adds its `from` and
/// `wordScoreBonusSuffix` adds its own — which happens to be the same six.
pub const SUFFIXES_LONGEST_FIRST: &[&str] = &["EST", "ING", "ED", "ER", "ES", "LY"];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn game_dir() -> std::path::PathBuf {
        std::path::PathBuf::from("../sternly-worded-adventures")
    }

    /// The table matches the game, or this fails.
    ///
    /// Re-derives the set of `wordBonus` flags from the source rather than trusting the transcription
    /// — a modifier added, removed or renamed in a game update lands here instead of silently
    /// contributing nothing. Both declaration styles are covered: a literal `type = 'wordBonus'` in
    /// the modifier's own file, and the `effects.wordScoreBonusSuffix(...)` factory.
    #[test]
    fn every_modifier_is_known() {
        let dir = game_dir().join("rpg/effects/modifiers");
        if !dir.is_dir() {
            return;
        }
        let mut found: HashSet<String> = HashSet::new();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().map(|e| e != "lua").unwrap_or(true) {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            if text.contains("type = 'wordBonus'") {
                let flag = text
                    .lines()
                    .find_map(|l| l.trim().strip_prefix("flag = '"))
                    .and_then(|l| l.split('\'').next())
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        // `local flag = '...'` above the table, as `wordScoreBonusEachWood` does.
                        text.lines()
                            .find_map(|l| l.trim().strip_prefix("local flag = '"))
                            .and_then(|l| l.split('\'').next())
                            .expect("a wordBonus modifier must name its flag")
                            .to_string()
                    });
                found.insert(flag);
            } else if let Some(rest) = text.split("wordScoreBonusSuffix('").nth(1) {
                let suffix = rest.split('\'').next().unwrap();
                found.insert(format!("wordScoreBonusSuffix{suffix}"));
            }
        }
        let known: HashSet<String> = MODIFIERS.iter().map(|s| s.flag.to_string()).collect();
        let mut missing: Vec<_> = found.difference(&known).cloned().collect();
        let mut extra: Vec<_> = known.difference(&found).cloned().collect();
        missing.sort();
        extra.sort();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "the game and this table disagree.\n  in the game, not here: {missing:?}\n  \
             here, not in the game: {extra:?}"
        );
        assert_eq!(MODIFIERS.len(), 32, "thirty-two, and six of them come from a factory");
    }

    /// A longer suffix must be tried before a shorter one it ends with.
    #[test]
    fn suffixes_are_ordered_longest_first() {
        for (i, a) in SUFFIXES_LONGEST_FIRST.iter().enumerate() {
            for b in &SUFFIXES_LONGEST_FIRST[i + 1..] {
                assert!(!a.ends_with(*b) || a.len() >= b.len(), "{b} would shadow {a}");
                assert!(a.len() >= b.len(), "{a} before {b} breaks longest-first");
            }
        }
    }

    #[test]
    fn the_quill_family_is_flagged_and_the_counter_is_seeded_first() {
        let quills: Vec<_> = MODIFIERS.iter().filter(|s| s.quill_category).collect();
        assert_eq!(quills.len(), 5, "four contributors plus the counter itself");
        let counter = quills.iter().find(|s| s.fact == Fact::QuillCount).unwrap();
        for other in quills.iter().filter(|s| s.fact != Fact::QuillCount) {
            assert!(
                counter.priority < other.priority,
                "the counter must be seeded before {} adds to it",
                other.flag
            );
        }
    }
}
