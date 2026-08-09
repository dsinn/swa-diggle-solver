//! The player's gear, and what it does to a word's score.
//!
//! ## The calculation this reproduces
//!
//! `utils/words.lua:292-302` is the whole of scoring:
//!
//! ```lua
//! function words.score(word, wordTiles, getFlag)
//!     local score = 0
//!     for i, tile in ipairs(wordTiles) do
//!         score = score+require'utils.tiles'.score(tile, getFlag)
//!     end
//!     local mult, add, postAdd = 1, 0, 0
//!     if getFlag then
//!         mult, add, postAdd = words.modifiers(word, wordTiles, getFlag)
//!     end
//!     return math.floor((score+add)*mult*words.lengthScore(word)+0.5+postAdd), word
//! end
//! ```
//!
//! and `words.modifiers` (`:269-290`) accumulates the three terms across every `wordBonus`
//! modifier whose flag the player holds:
//!
//! ```lua
//! local instances = flagVal
//! if data.scoreInstances then instances = data.scoreInstances(...) end
//! preAdd = preAdd+data.scorePreAdd*instances
//! mult   = mult  +data.scoreMult  *instances
//! postAdd= postAdd+data.scorePostAdd*instances
//! ```
//!
//! **The flag's value is a count, not a boolean.** `items/lengthgear.lua:36-38` lists
//! `wordScoreBonusPreLength456` three times, so the shortsword grants `3`, and every term is scaled
//! by it. That is why the shortsword adds 3 and not 1.
//!
//! The gear multiplier then goes *into* `words.getWordBonusModifier` (`:219-242`) as its starting
//! value rather than being multiplied afterwards, because the enemy's lexicon bonuses are additive
//! on the same accumulator (`mult = mult+val-1`). Multiplying the two separately computed factors
//! would be wrong whenever both are present — see [`crate::search::Modifiers::modifier_for_base`].
//!
//! ## Why this exists at all
//!
//! `score.rs` listed gear `wordBonus` modifiers as a known divergence and argued the error was
//! safe because it can only under-rate a word. That argument holds for a goal with a floor and is
//! backwards for [`crate::search::Goal::Scare`], which has a ceiling: a word that hits harder than
//! we believe kills the enemy we were trying to frighten off.
//!
//! Measured, 2026-08-09: wearing the shortsword, we scored `AAPA` at 4 and the game scored it 7.
//! Against a highwayman on 5 health behind 1 armour that is 6 through, so the game raised its
//! avoidable-murder warning. Three-letter `AAM` took no bonus and was accepted.
//!
//! ## What is not modelled, and why that is reported rather than ignored
//!
//! Several modifiers need state this program does not carry — board adjacency, column selection,
//! the auxiliary dictionaries, quill tiles, or the fight's word history. Holding such a flag does
//! not silently under-rate here: [`Adjust::unknown`] names it, so a caller with a ceiling can
//! decline to trust the number. Under-rating is only cheap when overshooting is harmless.

use crate::game::save::Table;
use crate::observe::board::Tile;
use std::collections::HashMap;

/// The three terms a word's modifiers contribute, plus what could not be evaluated.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Adjust {
    /// Added to the tile sum **before** the multiplier and the length scale.
    pub pre_add: f64,
    /// The accumulated multiplier, starting at 1. Feeds the lexicon accumulator, not multiplied
    /// against it.
    pub mult: f64,
    /// Added **after** everything, outside the rounding of the rest.
    pub post_add: f64,
    /// Flags the player holds that this cannot evaluate. Non-empty means the score is a lower
    /// bound rather than a reading.
    pub unknown: Vec<&'static str>,
}

impl Adjust {
    /// No gear, or nothing that applies: the identity for the whole calculation.
    pub fn none() -> Self {
        Adjust { pre_add: 0.0, mult: 1.0, post_add: 0.0, unknown: Vec::new() }
    }
}

/// Modifiers this reproduces, evaluated from the word and the tiles it consumes.
///
/// Every entry is `(flag, pre, mult, post)` from the corresponding file in
/// `rpg/effects/modifiers/`. The condition and any `scoreInstances` are in [`Gear::word_bonus`],
/// because they are the part that varies.
const MODELLED: &[&str] = &[
    "wordScoreBonusPreLength456",
    "wordScoreBonusPostLength7",
    "wordScoreBonusLengthOdd",
    "wordScoreNerfLengthEven",
    "wordScoreBonusCIE",
    "wordScoreBonusPOG",
    "wordScoreBonusHeterogram",
    "wordScoreBonusAlphabetical",
    "wordScoreBonusRuns",
];

/// `wordBonus` flags that are real but need state this program does not hold.
///
/// Listed by name so that holding one is *reported* rather than quietly dropped. Grouped by what
/// they would need:
///
/// - board shape: `Adjacent` (`tileboard.tilesAreAllAdjacent`), `FullColumn`
///   (`tileboard.countTilesInFullySelectedColumn`)
/// - the auxiliary dictionaries: the six `InDict*` flags, each `require'utils.words'.dictionaries.X`
/// - tile categories: `EachHasWood` (`utils/tiles.lua` material categories)
/// - quill tiles and the turn number: the four `Quill*` flags, which co-operate through a shared
///   `cache.quills` seeded by `wordScoreBonusQuillCount` at priority -999
/// - the fight's word history: `Alliteration` and `RepeatWord` both read `playerStats.usedWords`
///   (`rpgstats.lua:129-138`), which is per-fight state we do not track
const UNMODELLED: &[&str] = &[
    "wordScoreBonusAdjacent",
    "wordScoreBonusFullColumn",
    "wordScoreBonusInDictAxe",
    "wordScoreBonusInDictFire",
    "wordScoreBonusInDictIce",
    "wordScoreBonusInDictIron",
    "wordScoreBonusInDictMetal",
    "wordScoreBonusInDictWood",
    "wordScoreBonusEachHasWood",
    "wordScoreBonusQuill1",
    "wordScoreBonusQuillCount",
    "wordScoreBonusQuillTurn1",
    "wordScoreBonusQuillTurnMod12",
    "wordScoreBonusQuillTurnMod5",
    "wordScoreBonusAlliteration",
    "wordScoreBonusRepeatWord",
    // The six suffix bonuses, `scoreMult = 0.25` for a word ending in each. Declared by
    // `effects.wordScoreBonusSuffix(...)` in `utils/effects.lua:142-159` rather than in their own
    // files, which is why the first survey of this directory counted 26 modifiers instead of 32 and
    // why these were in neither list — neither evaluated nor reported, the one combination that
    // under-rates in silence.
    //
    // `cacheSuffix` (`:105-117`) matches longest-first and requires the suffix to be shorter than
    // the word, so `BIGGEST` is `EST` rather than `ES`, and `ED` alone is not a suffixed word.
    "wordScoreBonusSuffixED",
    "wordScoreBonusSuffixER",
    "wordScoreBonusSuffixES",
    "wordScoreBonusSuffixEST",
    "wordScoreBonusSuffixING",
    "wordScoreBonusSuffixLY",
    // Cannot fire in 52.4 — `countEachWithLetter(wordTiles, letter)` compares against an undefined
    // global (`utils/tiles.lua:189-198`). Listed anyway: if the dev fixes that global, this starts
    // mattering and we would rather hear about it than not.
    "wordScoreBonusWildcards",
];

/// The player's gear flags, as the save reports them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Gear {
    flags: HashMap<String, f64>,
}

impl Gear {
    /// Reads `rpg.player.gearFlags`, which is exactly what the game passes as `getFlag`.
    ///
    /// `wordboard.lua:232` scores with `rpgview.getPlayerGearFlag`, and that is
    /// `player.gearFlags[flag]` and nothing else (`rpgview.lua:569-575`) — **not** status effects.
    /// A status-based bonus would have to come from somewhere else, so reading statuses here would
    /// invent a term the game does not apply.
    pub fn from_save(cs: &Table) -> Self {
        let mut flags = HashMap::new();
        if let Some(t) = cs.table_at("rpg.player.gearFlags") {
            for (k, v) in &t.map {
                let n = match v {
                    crate::game::save::Value::Int(i) => *i as f64,
                    crate::game::save::Value::Num(n) => *n,
                    // Lua truth: a flag present as `true` counts as one instance.
                    crate::game::save::Value::Bool(true) => 1.0,
                    _ => continue,
                };
                flags.insert(k.clone(), n);
            }
        }
        Gear { flags }
    }

    /// For tests and for a player with nothing on.
    pub fn none() -> Self {
        Gear::default()
    }

    /// Builds from pairs, for tests.
    pub fn from_pairs(pairs: &[(&str, f64)]) -> Self {
        Gear { flags: pairs.iter().map(|(k, v)| ((*k).to_string(), *v)).collect() }
    }

    fn val(&self, flag: &str) -> Option<f64> {
        // Lua treats 0 as true, so a flag present at zero still passes `if flagVal`; it simply
        // contributes nothing once multiplied. Matching that costs nothing and avoids a rule this
        // code would otherwise have invented.
        self.flags.get(flag).copied()
    }

    /// Does the player hold any gear at all that touches word score?
    pub fn touches_score(&self) -> bool {
        self.flags.keys().any(|k| {
            MODELLED.contains(&k.as_str()) || UNMODELLED.contains(&k.as_str())
        })
    }

    /// The `(preAdd, mult, postAdd)` this word earns, and anything that could not be judged.
    ///
    /// `word` is the spelled word — the game passes the string, so **letters, not tiles**, decide
    /// every length test here. A ligature tile spells two letters from one tile, which is exactly
    /// the case where the two counts diverge.
    pub fn word_bonus(&self, word: &str, tiles: &[Tile]) -> Adjust {
        let mut out = Adjust::none();
        if self.flags.is_empty() {
            return out;
        }
        let upper = word.to_uppercase();
        let letters = upper.chars().count();

        let mut apply = |flag: &str, pre: f64, mult: f64, post: f64, instances: f64| {
            if let Some(v) = self.val(flag) {
                let n = instances * v;
                out.pre_add += pre * n;
                out.mult += mult * n;
                out.post_add += post * n;
            }
        };

        // `l==4 or l==5 or l==6`, `scorePreAdd = 1`. The shortsword.
        if (4..=6).contains(&letters) {
            apply("wordScoreBonusPreLength456", 1.0, 0.0, 0.0, 1.0);
        }
        // `l==7`, `scorePostAdd = 1`.
        if letters == 7 {
            apply("wordScoreBonusPostLength7", 0.0, 0.0, 1.0, 1.0);
        }
        // Odd bonus and even nerf are separate flags, and a player can hold both.
        if letters % 2 == 1 {
            apply("wordScoreBonusLengthOdd", 0.0, 0.25, 0.0, 1.0);
        } else {
            apply("wordScoreNerfLengthEven", 0.0, -0.25, 0.0, 1.0);
        }
        // `word:find'[cC][iI][eE]' or word:find'[^cC.][eE][iI]'` — "i before e except after c",
        // scored. The second pattern excludes a preceding `c` *and* a wildcard, and requires a
        // character to be there at all, so a word starting `EI` does not match.
        if has_cie(&upper) {
            apply("wordScoreBonusCIE", 0.0, 0.0, 4.0, 1.0);
        }
        if upper.contains("POG") {
            apply("wordScoreBonusPOG", 0.0, 0.0, 1.0, 1.0);
        }
        // `mostCommonCount==1 and not letterHash['.']`: every letter distinct, no wildcard.
        if let Some(most) = most_common_count(&upper) {
            if most == 1 && !upper.contains('.') {
                apply("wordScoreBonusHeterogram", 0.0, 0.25, 0.0, 1.0);
            }
        }
        if is_alphabetical(&upper) {
            apply("wordScoreBonusAlphabetical", 0.0, 1.0, 0.0, 1.0);
        }
        // `scoreInstances` multiplies the flag value by the run count.
        let runs = count_runs(&upper);
        if runs != 0 {
            apply("wordScoreBonusRuns", 1.0, 0.0, 0.0, runs as f64);
        }

        // `wordScoreBonusWildcards` is deliberately absent.
        //
        // Its `wordCache` calls `countEachWithLetter(wordTiles, letter)` with `letter` an undefined
        // global, so it compares every tile against `nil` (`utils/tiles.lua:189-198`) and always
        // counts zero. The condition is `wildcards ~= 0`, so the modifier cannot fire in the game.
        // Implementing what it looks like it means would *over*-rate a word — the direction that
        // types a word which fails to kill.
        let _ = tiles;

        out.unknown =
            UNMODELLED.iter().copied().filter(|f| self.flags.contains_key(*f)).collect();
        out
    }
}

/// `word:find'[cC][iI][eE]' or word:find'[^cC.][eE][iI]'`, on an already-uppercased word.
fn has_cie(upper: &str) -> bool {
    if upper.contains("CIE") {
        return true;
    }
    let b: Vec<char> = upper.chars().collect();
    // The `[^cC.]` class must match a real character, so the `EI` cannot start the word.
    (1..b.len().saturating_sub(1)).any(|i| {
        b[i] == 'E' && b[i + 1] == 'I' && b[i - 1] != 'C' && b[i - 1] != '.'
    })
}

/// `words.hash`'s `max` (`utils/words.lua:160-172`): how often the commonest character appears.
fn most_common_count(upper: &str) -> Option<usize> {
    if upper.is_empty() {
        return None;
    }
    let mut counts: HashMap<char, usize> = HashMap::new();
    for c in upper.chars() {
        *counts.entry(c).or_insert(0) += 1;
    }
    counts.values().copied().max()
}

/// `words.isAlphabetical` (`utils/words.lua:130`) — the letters never step backwards.
fn is_alphabetical(upper: &str) -> bool {
    let mut last = '\0';
    for c in upper.chars() {
        if c < last {
            return false;
        }
        last = c;
    }
    true
}

/// `words.countRuns` (`utils/words.lua:192-204`).
///
/// Counts the *starts* of doubled letters, not the doubled letters: the `curr_char~=prev_char`
/// guard means `AAA` scores 1, not 2. Transcribed rather than reasoned about, because that guard is
/// exactly the kind of detail a reimplementation gets wrong.
fn count_runs(upper: &str) -> usize {
    let b: Vec<char> = upper.chars().collect();
    if b.is_empty() {
        return 0;
    }
    let mut count = 0;
    let mut prev: Option<char> = None;
    let mut curr = b[0];
    for &next in b.iter().skip(1) {
        if next == curr && Some(curr) != prev {
            count += 1;
        }
        prev = Some(curr);
        curr = next;
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The live reading this whole module exists for.
    ///
    /// `AAPA`, four letters, player wearing `wordScoreBonusPreLength456 = 3`. We scored it 4 and the
    /// game scored it 7, and the difference is this bonus applied before the length scale.
    #[test]
    fn the_shortsword_adds_three_to_a_four_letter_word() {
        let gear = Gear::from_pairs(&[("wordScoreBonusPreLength456", 3.0)]);
        let a = gear.word_bonus("AAPA", &[]);
        assert_eq!(a.pre_add, 3.0, "three stacks of scorePreAdd = 1");
        assert_eq!(a.mult, 1.0);
        assert_eq!(a.post_add, 0.0);
        assert!(a.unknown.is_empty());
    }

    /// The control from the same fight: three letters took no bonus and the game accepted it.
    #[test]
    fn a_three_letter_word_takes_no_length_bonus() {
        let gear = Gear::from_pairs(&[("wordScoreBonusPreLength456", 3.0)]);
        assert_eq!(gear.word_bonus("AAM", &[]).pre_add, 0.0);
        // And the twelve-letter word from the same log is outside the band too.
        assert_eq!(gear.word_bonus("VENEPUNCTURE", &[]).pre_add, 0.0);
    }

    #[test]
    fn the_band_is_four_to_six_letters_inclusive() {
        let gear = Gear::from_pairs(&[("wordScoreBonusPreLength456", 1.0)]);
        let pre = |w: &str| gear.word_bonus(w, &[]).pre_add;
        assert_eq!((pre("CAT"), pre("CATS"), pre("CATCH"), pre("CATCHY"), pre("CATCHER")),
                   (0.0, 1.0, 1.0, 1.0, 0.0));
    }

    #[test]
    fn odd_and_even_length_are_separate_flags_and_stack_with_each_other() {
        let odd = Gear::from_pairs(&[("wordScoreBonusLengthOdd", 2.0)]);
        assert_eq!(odd.word_bonus("CAT", &[]).mult, 1.5, "1 + 0.25*2");
        assert_eq!(odd.word_bonus("CATS", &[]).mult, 1.0, "even words get nothing from it");

        let even = Gear::from_pairs(&[("wordScoreNerfLengthEven", 1.0)]);
        assert_eq!(even.word_bonus("CATS", &[]).mult, 0.75, "1 + -0.25");
        assert_eq!(even.word_bonus("CAT", &[]).mult, 1.0);
    }

    #[test]
    fn runs_are_counted_as_starts_of_doubles_not_as_doubled_letters() {
        // `words.countRuns`: the `curr_char~=prev_char` guard stops a triple counting twice.
        assert_eq!(count_runs("AA"), 1);
        assert_eq!(count_runs("AAA"), 1);
        assert_eq!(count_runs("AABB"), 2);
        assert_eq!(count_runs("ABAB"), 0);
        assert_eq!(count_runs(""), 0);

        let gear = Gear::from_pairs(&[("wordScoreBonusRuns", 2.0)]);
        // scorePreAdd = 1, instances = runs * flagVal = 2 * 2.
        assert_eq!(gear.word_bonus("AABB", &[]).pre_add, 4.0);
    }

    #[test]
    fn cie_matches_i_before_e_except_after_c() {
        assert!(has_cie("ANCIENT"), "CIE outright");
        assert!(has_cie("WEIRD"), "EI not after C");
        assert!(!has_cie("CEILING"), "EI after C is the exception the pattern excludes");
        assert!(!has_cie("EIGHT"), "the class must match a character, so a leading EI does not");
        assert!(!has_cie("BELIEVE"), "IE is not the pattern; CIE and EI are");

        let gear = Gear::from_pairs(&[("wordScoreBonusCIE", 1.0)]);
        assert_eq!(gear.word_bonus("WEIRD", &[]).post_add, 4.0);
        assert_eq!(gear.word_bonus("CEILING", &[]).post_add, 0.0);
    }

    #[test]
    fn a_heterogram_needs_every_letter_distinct_and_no_wildcard() {
        let gear = Gear::from_pairs(&[("wordScoreBonusHeterogram", 1.0)]);
        assert_eq!(gear.word_bonus("CAT", &[]).mult, 1.25);
        assert_eq!(gear.word_bonus("CATS", &[]).mult, 1.25);
        assert_eq!(gear.word_bonus("AAPA", &[]).mult, 1.0, "repeated A");
        assert_eq!(gear.word_bonus("C.T", &[]).mult, 1.0, "a wildcard disqualifies it");
    }

    #[test]
    fn alphabetical_means_the_letters_never_step_backwards() {
        assert!(is_alphabetical("ACT"));
        assert!(is_alphabetical("AAB"), "equal letters do not step backwards");
        assert!(!is_alphabetical("CAT"));
        let gear = Gear::from_pairs(&[("wordScoreBonusAlphabetical", 1.0)]);
        assert_eq!(gear.word_bonus("ACT", &[]).mult, 2.0, "scoreMult = 1 doubles it");
    }

    /// A flag we cannot evaluate must be named, not dropped.
    ///
    /// This is the whole reason `unknown` exists: silently under-rating is safe for a goal with a
    /// floor and dangerous for one with a ceiling.
    #[test]
    fn gear_we_cannot_evaluate_is_reported_rather_than_ignored() {
        let gear = Gear::from_pairs(&[
            ("wordScoreBonusPreLength456", 3.0),
            ("wordScoreBonusInDictFire", 1.0),
            ("wordScoreBonusAdjacent", 1.0),
        ]);
        let a = gear.word_bonus("AAPA", &[]);
        assert_eq!(a.pre_add, 3.0, "what we can evaluate is still applied");
        let mut got = a.unknown.clone();
        got.sort();
        assert_eq!(got, vec!["wordScoreBonusAdjacent", "wordScoreBonusInDictFire"]);
    }

    /// Gear with nothing to do with word score must not register as an unknown.
    #[test]
    fn unrelated_gear_is_not_mistaken_for_an_unevaluated_bonus() {
        // Every one of these was on the player in the live save.
        let gear = Gear::from_pairs(&[
            ("onWoodKillGainArmour", 4.0),
            ("toxinArmour", 1.0),
            ("restAdd4Armour", 1.0),
            ("curseCollectBlood", 1.0),
        ]);
        let a = gear.word_bonus("AAPA", &[]);
        assert_eq!(a, Adjust::none());
        assert!(!gear.touches_score());
    }

    /// Every `wordBonus` flag the game declares is either evaluated or reported.
    ///
    /// The count is 32, not 26: six are built by `effects.wordScoreBonusSuffix(...)` in
    /// `utils/effects.lua` rather than declared in their own files, so a survey of
    /// `rpg/effects/modifiers/` for `type = 'wordBonus'` misses them. They were in neither list —
    /// silently contributing nothing while `unknown` reported nothing — until this test.
    #[test]
    fn no_word_bonus_flag_is_both_unevaluated_and_unreported() {
        assert_eq!(
            MODELLED.len() + UNMODELLED.len(),
            32,
            "every wordBonus modifier must be in exactly one list"
        );
        for m in MODELLED {
            assert!(!UNMODELLED.contains(m), "{m} is in both lists");
        }
        // A flag we do not evaluate must make itself heard.
        let gear = Gear::from_pairs(&[("wordScoreBonusSuffixING", 1.0)]);
        assert!(gear.touches_score(), "suffix gear affects word score");
        assert_eq!(gear.word_bonus("TESTING", &[]).unknown, vec!["wordScoreBonusSuffixING"]);
    }

    #[test]
    fn the_live_gear_set_is_read_out_of_a_save_shaped_table() {
        let save = crate::game::save::parse(
            "return { rpg = { player = { gearFlags = {\n\
             \x20 wordScoreBonusPreLength456 = 3,\n\
             \x20 onWoodKillGainArmour = 4,\n\
             } } } }",
        )
        .unwrap();
        let gear = Gear::from_save(&save);
        assert!(gear.touches_score());
        assert_eq!(gear.word_bonus("AAPA", &[]).pre_add, 3.0);
        assert_eq!(gear.word_bonus("AAM", &[]).pre_add, 0.0);
    }
}
