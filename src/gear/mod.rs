//! The player's gear, and what it does to a word's score.
//!
//! ## The calculation this reproduces
//!
//! `utils/words.lua:292-302`:
//!
//! ```lua
//! return math.floor((score+add)*mult*words.lengthScore(word)+0.5+postAdd), word
//! ```
//!
//! where `add`, `mult` and `postAdd` come from `words.modifiers` (`:269-290`), which accumulates
//! across every `wordBonus` modifier whose flag the player holds:
//!
//! ```lua
//! local instances = flagVal
//! if data.scoreInstances then instances = data.scoreInstances(...) end
//! preAdd = preAdd+data.scorePreAdd*instances
//! mult   = mult  +data.scoreMult  *instances
//! postAdd= postAdd+data.scorePostAdd*instances
//! ```
//!
//! Three details are load-bearing:
//!
//! - **The flag's value is a count, not a boolean.** `items/lengthgear.lua:36-38` lists
//!   `wordScoreBonusPreLength456` three times, so the shortsword grants `3` and every term scales by
//!   it. That is why it adds 3 and not 1.
//! - **`preAdd` joins the tile sum**, so it is scaled by both the multiplier and `lengthScore`;
//!   `postAdd` lands after the rounding and is scaled by neither.
//! - **The gear multiplier is the starting value for the lexicon accumulator**, not a separate
//!   factor — see [`crate::search::Modifiers::modifier_for_base`].
//!
//! ## Why this exists
//!
//! `score.rs` listed gear `wordBonus` modifiers as a known divergence and argued the error was safe
//! because it can only under-rate. That holds for a goal with a floor and is backwards for
//! [`crate::search::Goal::Scare`], which has a ceiling: a word that hits harder than we believe kills
//! the enemy we were trying to frighten off.
//!
//! Measured live, 2026-08-09: wearing the shortsword we scored `AAPA` at 4 and the game scored it 7.
//! Against a highwayman on 5 health behind 1 armour that is 6 through, so the game raised its
//! avoidable-murder warning — thirty times running.

pub mod facts;
pub mod table;
pub mod weapon;

pub use facts::{Facts, FightFacts};
pub use table::{Dict, Fact, ModifierSpec, MODIFIERS};
pub use weapon::{status_tick, Weapon, WeaponBonus};

use facts::Eval;
use std::collections::HashMap;

/// The three terms a word's modifiers contribute, plus what could not be evaluated.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Adjust {
    /// Added to the tile sum **before** the multiplier and the length scale.
    pub pre_add: f64,
    /// The accumulated multiplier, starting at 1. Feeds the lexicon accumulator.
    pub mult: f64,
    /// Added after everything, outside the rounding of the rest.
    pub post_add: f64,
    /// Flags held that could not be evaluated. Non-empty means the score is a lower bound.
    pub unknown: Vec<&'static str>,
}

impl Adjust {
    /// The identity: no gear, or nothing that applies.
    pub fn none() -> Self {
        Adjust { pre_add: 0.0, mult: 1.0, post_add: 0.0, unknown: Vec::new() }
    }
}

/// The player's gear flags, as the save reports them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Gear {
    flags: HashMap<String, f64>,
}

impl Gear {
    /// Reads `rpg.player.gearFlags`, which is exactly what the game passes as `getFlag`.
    ///
    /// `wordboard.lua:232` scores with `rpgview.getPlayerGearFlag`, and that is
    /// `player.gearFlags[flag]` and nothing else (`rpgview.lua:569-575`) — **not** status effects, so
    /// reading statuses here would invent a term the game does not apply.
    pub fn from_save(save: &crate::game::save::Table) -> Self {
        let mut flags = HashMap::new();
        if let Some(t) = save.table_at("rpg.player.gearFlags") {
            for (k, v) in &t.map {
                let n = match v {
                    crate::game::save::Value::Int(i) => *i as f64,
                    crate::game::save::Value::Num(n) => *n,
                    crate::game::save::Value::Bool(true) => 1.0,
                    _ => continue,
                };
                flags.insert(k.clone(), n);
            }
        }
        Gear { flags }
    }

    pub fn none() -> Self {
        Gear::default()
    }

    /// For tests.
    pub fn from_pairs(pairs: &[(&str, f64)]) -> Self {
        Gear { flags: pairs.iter().map(|(k, v)| ((*k).to_string(), *v)).collect() }
    }

    pub fn get(&self, flag: &str) -> Option<f64> {
        self.flags.get(flag).copied()
    }

    /// The modifiers this player actually has, in priority order.
    pub fn compile(&self) -> Plan {
        let mut active: Vec<Active> = MODIFIERS
            .iter()
            .filter_map(|spec| self.flags.get(spec.flag).map(|&value| Active { spec, value }))
            .collect();
        // `rpg/effects/modifiers.lua:22-26` sorts each set by priority, which is what puts the quill
        // counter's -999 ahead of everything that feeds it.
        active.sort_by_key(|a| a.spec.priority);
        Plan { active }
    }
}

#[derive(Debug, Clone, Copy)]
struct Active {
    spec: &'static ModifierSpec,
    value: f64,
}

/// The modifiers in play this fight, resolved once.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    active: Vec<Active>,
}

impl Plan {
    /// Nothing the player wears touches word score.
    pub fn is_empty(&self) -> bool {
        self.active.is_empty()
    }

    /// Does the plan read the Whale idol's tally?
    pub fn wants_alliteration(&self) -> bool {
        self.active.iter().any(|a| a.spec.fact == Fact::Alliteration)
    }

    /// Does the plan read the Raven idol's tally?
    pub fn wants_repeats(&self) -> bool {
        self.active.iter().any(|a| a.spec.fact == Fact::RepeatWord)
    }

    /// Which auxiliary dictionaries this plan will ask about, so the caller can hand over only
    /// those word sets rather than the whole of [`crate::lexica::Lexica`].
    pub fn dicts_needed(&self) -> Vec<Dict> {
        let mut out = Vec::new();
        for a in &self.active {
            if let Fact::InDict(d) = a.spec.fact {
                if !out.contains(&d) {
                    out.push(d);
                }
            }
        }
        out
    }

    /// Flags whose fact this build cannot evaluate for any word, for reporting once per fight
    /// rather than once per candidate.
    pub fn unevaluable(&self) -> Vec<&'static str> {
        self.active
            .iter()
            .filter(|a| {
                matches!(a.spec.fact, Fact::EachHasWood | Fact::Adjacent | Fact::FullColumn)
            })
            .map(|a| a.spec.flag)
            .collect()
    }

    /// The `(preAdd, mult, postAdd)` this word earns, and anything unjudgeable.
    ///
    /// ## Two passes, because the quill family is coupled
    ///
    /// `wordScoreBonusQuillCount` seeds `cache.quills = 0` at priority **-999** — before everything —
    /// and the other three quill modifiers each add one to that counter when their own turn condition
    /// holds. `QuillCount` then reads the final tally through `scoreInstances` as
    /// `cache.quills * flagVal`. So a counter written by later-priority modifiers is read by an
    /// earlier-priority one, inside a single word's evaluation, and one ordered pass would hand
    /// `QuillCount` a zero.
    ///
    /// The counter only ever counts modifiers the player **holds**: `words.rawScore:329-332` runs
    /// each `wordCache` under `if getFlag(data.flag)`, so an unworn quill contributes nothing to it.
    pub fn apply(&self, facts: &Facts, fight: &FightFacts) -> Adjust {
        let mut out = Adjust::none();
        if self.active.is_empty() {
            return out;
        }
        let mut quills = 0.0f64;
        for a in &self.active {
            if a.spec.quill_category && a.spec.fact != Fact::QuillCount {
                if let Eval::Apply(_) = facts.evaluate(a.spec.fact, fight) {
                    quills += 1.0;
                }
            }
        }
        for a in &self.active {
            let eval = match a.spec.fact {
                // Resolved here rather than in `Facts`, which cannot see the plan.
                Fact::QuillCount => {
                    if quills > 0.0 {
                        Eval::Apply(quills)
                    } else {
                        Eval::Skip
                    }
                }
                other => facts.evaluate(other, fight),
            };
            match eval {
                Eval::Skip => {}
                Eval::Apply(instances) => {
                    let n = instances * a.value;
                    out.pre_add += a.spec.pre_add * n;
                    out.mult += a.spec.mult * n;
                    out.post_add += a.spec.post_add * n;
                }
                Eval::Unknown => out.unknown.push(a.spec.flag),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts_for<'a>(word: &'a str) -> Facts<'a> {
        Facts::new(word, &[], None, &[])
    }

    fn plain(gear: &Gear, word: &str) -> Adjust {
        gear.compile().apply(&facts_for(word), &FightFacts::default())
    }

    /// The live reading this module exists for.
    #[test]
    fn the_shortsword_adds_three_to_a_four_letter_word() {
        let gear = Gear::from_pairs(&[("wordScoreBonusPreLength456", 3.0)]);
        let a = plain(&gear, "AAPA");
        assert_eq!((a.pre_add, a.mult, a.post_add), (3.0, 1.0, 0.0));
        assert!(a.unknown.is_empty());
    }

    #[test]
    fn a_three_letter_word_takes_no_length_bonus() {
        let gear = Gear::from_pairs(&[("wordScoreBonusPreLength456", 3.0)]);
        assert_eq!(plain(&gear, "AAM").pre_add, 0.0);
        assert_eq!(plain(&gear, "VENEPUNCTURE").pre_add, 0.0);
    }

    #[test]
    fn the_band_is_four_to_six_letters_inclusive() {
        let gear = Gear::from_pairs(&[("wordScoreBonusPreLength456", 1.0)]);
        let pre = |w: &str| plain(&gear, w).pre_add;
        assert_eq!(
            (pre("CAT"), pre("CATS"), pre("CATCH"), pre("CATCHY"), pre("CATCHER")),
            (0.0, 1.0, 1.0, 1.0, 0.0)
        );
    }

    #[test]
    fn odd_and_even_length_are_separate_flags() {
        let odd = Gear::from_pairs(&[("wordScoreBonusLengthOdd", 2.0)]);
        assert_eq!(plain(&odd, "CAT").mult, 1.5, "1 + 0.25*2");
        assert_eq!(plain(&odd, "CATS").mult, 1.0);
        let even = Gear::from_pairs(&[("wordScoreNerfLengthEven", 1.0)]);
        assert_eq!(plain(&even, "CATS").mult, 0.75);
    }

    #[test]
    fn runs_scale_the_instances() {
        let gear = Gear::from_pairs(&[("wordScoreBonusRuns", 2.0)]);
        // scorePreAdd = 1, instances = runs * flagVal = 2 * 2
        assert_eq!(plain(&gear, "AABB").pre_add, 4.0);
        assert_eq!(plain(&gear, "ABAB").pre_add, 0.0);
    }

    #[test]
    fn cie_is_i_before_e_except_after_c() {
        let gear = Gear::from_pairs(&[("wordScoreBonusCIE", 1.0)]);
        assert_eq!(plain(&gear, "ANCIENT").post_add, 4.0);
        assert_eq!(plain(&gear, "WEIRD").post_add, 4.0);
        assert_eq!(plain(&gear, "CEILING").post_add, 0.0, "EI after C is the exception");
        assert_eq!(plain(&gear, "EIGHT").post_add, 0.0, "a leading EI has no preceding char");
    }

    #[test]
    fn a_heterogram_needs_every_letter_distinct() {
        let gear = Gear::from_pairs(&[("wordScoreBonusHeterogram", 1.0)]);
        assert_eq!(plain(&gear, "CATS").mult, 1.25);
        assert_eq!(plain(&gear, "AAPA").mult, 1.0);
    }

    /// Longest match, and never the whole word.
    #[test]
    fn the_longest_registered_suffix_wins() {
        let est = Gear::from_pairs(&[("wordScoreBonusSuffixEST", 1.0)]);
        let es = Gear::from_pairs(&[("wordScoreBonusSuffixES", 1.0)]);
        assert_eq!(plain(&est, "BIGGEST").mult, 1.25);
        assert_eq!(plain(&es, "BIGGEST").mult, 1.0, "EST shadows ES");
        assert_eq!(plain(&es, "MOVES").mult, 1.25);
        let ed = Gear::from_pairs(&[("wordScoreBonusSuffixED", 1.0)]);
        assert_eq!(plain(&ed, "ED").mult, 1.0, "the suffix must be shorter than the word");
    }

    /// The counter is fed by the quill modifiers that are worn and whose turn condition holds.
    #[test]
    fn the_quill_counter_is_seeded_then_read() {
        let gear = Gear::from_pairs(&[
            ("wordScoreBonusQuillCount", 1.0),
            ("wordScoreBonusQuill1", 1.0),
            ("wordScoreBonusQuillTurn1", 1.0),
        ]);
        let plan = gear.compile();
        // Turn 1: the unconditional quill and the turn-1 quill both count.
        let t1 = plan.apply(&facts_for("CAT"), &FightFacts { turn: 1, ..Default::default() });
        assert_eq!(t1.pre_add, 3.0 * 2.0 + 1.0, "QuillCount 3*2 quills, plus Quill1's own 1");
        assert_eq!(t1.mult, 2.0, "QuillTurn1 contributes scoreMult 1");
        // Turn 2: only the unconditional one.
        let t2 = plan.apply(&facts_for("CAT"), &FightFacts { turn: 2, ..Default::default() });
        assert_eq!(t2.pre_add, 3.0 + 1.0);
        assert_eq!(t2.mult, 1.0);
    }

    /// An unworn quill must not feed the counter.
    #[test]
    fn only_worn_quills_are_counted() {
        let gear = Gear::from_pairs(&[("wordScoreBonusQuillCount", 1.0)]);
        let a =
            gear.compile().apply(&facts_for("CAT"), &FightFacts { turn: 1, ..Default::default() });
        assert_eq!(a.pre_add, 0.0, "no quill contributors worn, so the tally is zero");
    }

    #[test]
    fn gear_we_cannot_evaluate_is_reported() {
        let gear = Gear::from_pairs(&[
            ("wordScoreBonusPreLength456", 3.0),
            ("wordScoreBonusAdjacent", 1.0),
        ]);
        let a = plain(&gear, "AAPA");
        assert_eq!(a.pre_add, 3.0, "what we can evaluate is still applied");
        assert_eq!(a.unknown, vec!["wordScoreBonusAdjacent"]);
    }

    #[test]
    fn unrelated_gear_compiles_to_an_empty_plan() {
        let gear = Gear::from_pairs(&[("onWoodKillGainArmour", 4.0), ("toxinArmour", 1.0)]);
        assert!(gear.compile().is_empty());
        assert_eq!(plain(&gear, "AAPA"), Adjust::none());
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
        assert_eq!(plain(&gear, "AAPA").pre_add, 3.0);
    }
}
