//! What each row of the advisor's list says about its word, beyond the score.
//!
//! Display only. Nothing here changes the order of the list — that is [`crate::search::ranked`]'s
//! — so a heal or a bonus shown here that Diggle's ranking does not weigh (the Diamond heal is
//! one) informs the player without moving a row.

use crate::game::save::Table;
use crate::observe::board::Tile;
use crate::score::Scorer;
use crate::search::{Candidate, Goal, Modifiers, Tier, MIN_MEANINGFUL_DAMAGE};
use crate::turn::TurnPlan;

/// The most rows the list is filled with. Enough to scroll through; cheap to rebuild every turn.
pub const ROW_CAP: usize = 500;

/// One row of the list.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub word: String,
    pub score: i64,
    pub tier: Tier,
    /// The enemy's lexicon multiplier for this word, when it is not 1.
    pub bonus: Option<Bonus>,
    /// Health this word is expected to give back. Zero for most words.
    pub heal: i64,
    pub effect: &'static str,
    /// Each letter as typed, with the board index of the tile it goes into.
    pub letters: Vec<Letter>,
    /// The headings of the held spells this word would cast (`Fire`, `Four heal`).
    pub spells: Vec<String>,
    /// The rapier carries this word's damage past the enemy's armour — see
    /// [`crate::search::Modifiers::pierces`].
    pub pierces: bool,
}

/// The enemy's lexicon bonus for `word`, when it is not 1, and which lexicons it comes from.
///
/// The same accumulator as [`Modifiers::modifier_for_base`] — lexicons add, `mult + val - 1` per
/// match (`utils/words.lua:219-242`) — started from 1 and without the corner nerf, which is a
/// property of the board rather than of the word.
pub fn bonus_of(mods: &Modifiers, word: &str) -> Option<Bonus> {
    let mut mult = 1.0;
    let mut names = Vec::new();
    for (i, (words, v)) in mods.bonuses.iter().enumerate() {
        if words.contains(word) {
            mult += v - 1.0;
            names.extend(mods.bonus_names.get(i).cloned());
        }
    }
    ((mult - 1.0f64).abs() > 1e-9).then_some(Bonus { mult, names })
}

/// A word's lexicon bonus: the multiplier, and the lexicons it came from (`Bone`, `Slime`).
#[derive(Debug, Clone, PartialEq)]
pub struct Bonus {
    pub mult: f64,
    pub names: Vec<String>,
}

impl std::fmt::Display for Bonus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "x{:.2}", self.mult)?;
        // Just the emoji: they say which lexicon, and the user found the names noise. A lexicon
        // with no emoji keeps its name, so nothing drops out of sight.
        if !self.names.is_empty() {
            let marks: String = self
                .names
                .iter()
                .map(|n| lexicon_emoji(n).map(str::to_string).unwrap_or_else(|| format!(" {n}")))
                .collect();
            write!(f, " {}", marks.trim_start())?;
        }
        Ok(())
    }
}

/// What the player can get back this turn, and what decides it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Heals {
    /// Damage that kills: post-status health plus armour, the figure every goal aims against.
    pub need: i64,
    pub missing: i64,
    pub bleed: i64,
    /// The Well-Rested overkill heal is live: [`crate::search::PlayerState::heals`].
    pub rested: bool,
    /// `onWoodKillGainHealth` is worn.
    pub wood_gear: bool,
}

impl Heals {
    pub fn from_save(plan: &TurnPlan, cs: &Table) -> Self {
        let current = cs.int_at("rpg.player.health").unwrap_or(0);
        let max = cs.int_at("rpg.player.maxHealth").unwrap_or(current);
        Heals {
            need: plan.deciding_health + plan.armour,
            missing: (max - current).max(0),
            bleed: bleed_of(cs),
            rested: plan.player.as_ref().is_some_and(|p| p.heals),
            wood_gear: cs.path("rpg.player.gearFlags.onWoodKillGainHealth").is_some(),
        }
    }

    /// Health `c` is expected to restore, capped at what is missing.
    ///
    /// Three sources, summed:
    ///
    /// - **Well-Rested overkill**: `floor(overkill / 2)` on a kill (`rpgview.lua:1195-1196`,
    ///   [`crate::rested::heal_from`]), skipped while bleeding (`:1080`).
    /// - **Wood-kill gear**: 4 on a kill made only of wood tiles, unless bleeding
    ///   (`onWoodKillGainHealth`, see [`crate::pick::Preferences::from_flags`]).
    /// - **A Diamond tile played last**: `3 × gems in the word`, less any bleed, kill or not
    ///   (`rpg/effects/material/gemheal.lua`, its `activate`). Diggle's ranking does not model this.
    ///
    /// A kill is judged as the goals judge it, against post-status health, so this agrees with
    /// Diggle's arithmetic rather than with a second reading of the same save. And on the word's
    /// reach, so a rapier word's overkill is counted past health alone, as `rpgview.lua:1207` does.
    pub fn of(&self, tiles: &[Tile], scorer: &Scorer, c: &Candidate) -> i64 {
        let kills = c.reach >= self.need;
        let mut heal = 0;
        if self.rested && self.bleed == 0 && kills {
            heal += crate::rested::heal_from(c.reach, self.need);
        }
        if self.wood_gear
            && self.bleed == 0
            && kills
            && crate::pick::wood_only(tiles, &c.typed.tiles, scorer)
        {
            heal += 4;
        }
        let material = |i: &usize| tiles.get(*i).and_then(|t| scorer.material_name(t));
        if c.typed.tiles.last().and_then(material) == Some("gemheal") {
            let gems = c
                .typed
                .tiles
                .iter()
                .filter(|i| material(i).is_some_and(|m| m.starts_with("gem")))
                .count() as i64;
            heal += (3 * gems - self.bleed).max(0);
        }
        heal.min(self.missing)
    }
}

/// The player's bleed, as a whole number of stacks, and never zero while any is present.
///
/// Rounded **up** because presence is what cancels the heals (`rpgview.lua:1080`) — Diggle reads it
/// as presence too, in [`crate::turn::player_state`] — and a fraction rounded down to zero would
/// show a heal the game will not give.
pub fn bleed_of(cs: &Table) -> i64 {
    match cs.path("rpg.player.statusEffects.bleed").and_then(|v| v.as_f64()) {
        Some(b) if b > 0.0 => (b.ceil() as i64).max(1),
        _ => 0,
    }
}

/// The few words describing what playing this does, given the goal it was ranked for.
///
/// `kill` is the damage that kills, so a word outside the band can still say it kills — a frugal
/// kill's overshoot, or any hit under `MaxDamage`.
///
/// **Blank when the enemy simply survives.** A hit that neither kills, scares nor breaks a scare's
/// ceiling has nothing to say beyond its damage, which has its own column; the user found a
/// `chip` label there to be noise.
///
/// `score` is the damage, which decides whether the word lands at all; `reach` is the damage as the
/// goal measures it ([`crate::search::Modifiers::reach`]), which decides whether it kills.
pub fn effect(goal: Goal, tier: Tier, score: i64, reach: i64, kill: i64) -> &'static str {
    if score < MIN_MEANINGFUL_DAMAGE {
        return "no damage";
    }
    match (tier, goal) {
        (Tier::InBand, Goal::Scare { .. }) => "scares",
        (Tier::InBand, Goal::FrugalKill { .. }) => "frugal kill",
        (Tier::InBand, _) => "kills",
        (Tier::Under, _) => "",
        (Tier::Overshoot, _) => "overshoots - would kill",
        (Tier::Fallback, _) if reach >= kill => "kills",
        (Tier::Fallback, _) => "",
    }
}

/// Each letter as it is typed, against the board index of its tile. A wildcard shows the letter
/// typed into it, upper-cased like every other tile (the typist reports it in lower case).
pub fn letters_of(tiles: &[Tile], c: &Candidate) -> Vec<Letter> {
    c.typed
        .steps()
        .map(|(i, wild)| {
            let letter = match wild {
                Some(ch) => ch.to_ascii_uppercase().to_string(),
                None => tiles
                    .get(i)
                    .map(|t| match t.quality.ligature.as_deref().and_then(historical_glyph) {
                        Some(glyph) => glyph.to_string(),
                        None => t.letter.to_ascii_uppercase(),
                    })
                    .unwrap_or_default(),
            };
            Letter { text: letter, tile: i, wildcard: wild.is_some() }
        })
        .collect()
}

/// One letter of a candidate word, as typed.
#[derive(Debug, Clone, PartialEq)]
pub struct Letter {
    pub text: String,
    /// Board index of the tile it is typed into.
    pub tile: usize,
    /// Typed into a wildcard, which the game draws as handwriting.
    pub wildcard: bool,
}

/// The letter the game draws for a ligature tile, when that is a historical glyph.
///
/// The game picks a tile's face from its ligature sheet by `extra.ligature` first
/// (`ui/objects/tile.lua:40-73,97`), and these six cells of `ui/graphics/tiles/*_ligatures.png` are
/// letters in their own right rather than letters run together. Each counts as the letters the
/// tile carries (`rpg/effects/ligature/thorn.lua` and its neighbours). Digraphs drawn as ordinary
/// letters — `Th`, `Qu`, `ing` — are not in the list and show as text.
fn historical_glyph(ligature: &str) -> Option<&'static str> {
    Some(match ligature {
        "thorn" => "Þ",
        "eth" => "Ð",
        "ash" => "Æ",
        "ethel" => "Œ",
        "eszett" => "ß",
        "multiO" => "ꙮ",
        _ => return None,
    })
}

/// The Spell column: the spell to cast for the damage shown, first, then any other held spell
/// the word qualifies for. Each is its emoji alone — the user's call, the emoji say which — and
/// only a spell without one (UwU) is written out.
pub fn spell_column(cast: Option<&'static str>, qualifying: Vec<&'static str>) -> Vec<String> {
    let mark = |heading: &str| spell_emoji(heading).unwrap_or(heading).to_string();
    let mut out: Vec<String> = cast.map(mark).into_iter().collect();
    out.extend(qualifying.into_iter().filter(|q| Some(*q) != cast).map(mark));
    out
}

/// The emoji for a lexicon, by the name a bonus carries (`Bone`, from `lexiconBonusBone`).
///
/// The user's choices, 2026-11-02 and -03: fire, cold, bone, wood, spider, slime, metal, and ❗ for
/// profanity — the mark on the profane book itself, and profane spells do produce `!` tiles. The
/// rest are filled in to match; the dictionaries are `utils/dictionaries/*.lua`.
pub fn lexicon_emoji(name: &str) -> Option<&'static str> {
    Some(match name {
        "Fire" => "🔥",
        "Ice" => "❄️",
        "Bone" => "💀",
        "Wood" => "🪵",
        "Spider" => "🕷️",
        "Swears" => "❗",
        "Axe" => "🪓",
        "Blood" => "🩸",
        "Iron" => "⛓️",
        "Metal" => "🤘",
        "Slime" => "🍮",
        _ => return None,
    })
}

/// The emoji for a spell, by its `castHeading`. A spell checked against a dictionary shares that
/// dictionary's emoji, so a Bone bonus and casting Turn read as the same thing. UwU has none: the
/// user's call, its name is already the picture.
pub fn spell_emoji(heading: &str) -> Option<&'static str> {
    match heading {
        "Fire" => lexicon_emoji("Fire"),
        "Ice" => lexicon_emoji("Ice"),
        "Metal" => lexicon_emoji("Metal"),
        "Drain" => lexicon_emoji("Blood"),
        "Curse" => lexicon_emoji("Swears"),
        "Turn" | "Turn (if undead)" => lexicon_emoji("Bone"),
        "Four heal" => Some("💚"),
        "Vowels" => Some("🔤"),
        _ => None,
    }
}

/// The list's rows, at most `cap` of them, in the order given.
pub fn rows(
    plan: &TurnPlan, cs: &Table, scorer: &Scorer, spells: &crate::spells::SpellBook,
    cands: &[Candidate], cap: usize,
) -> Vec<Row> {
    let heals = Heals::from_save(plan, cs);
    // One entry per tile, as the game's spell checks see them: a ligature is one tile (`TH`), and
    // a wildcard is the letter typed into it.
    let tile_letters = |c: &Candidate| -> Vec<String> {
        c.typed
            .steps()
            .map(|(i, wild)| match wild {
                Some(ch) => ch.to_ascii_uppercase().to_string(),
                None => {
                    plan.tiles.get(i).map(|t| t.letter.to_ascii_uppercase()).unwrap_or_default()
                }
            })
            .collect()
    };
    cands
        .iter()
        .take(cap)
        .map(|c| Row {
            word: c.word.clone(),
            score: c.score,
            tier: c.tier,
            bonus: bonus_of(&plan.mods, &c.word),
            heal: heals.of(&plan.tiles, scorer, c),
            effect: effect(plan.goal, c.tier, c.score, c.reach, heals.need),
            pierces: c.pierces,
            letters: letters_of(&plan.tiles, c),
            spells: spell_column(c.cast, spells.qualifying(&c.word, &tile_letters(c))),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observe::board::{Quality, Tile};
    use crate::search::{Candidate, Goal, Modifiers, Tier};
    use crate::typist::Typed;
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn scorer() -> crate::score::Scorer {
        crate::score::Scorer::new(&PathBuf::from("../sternly-worded-adventures")).unwrap()
    }

    fn cand(word: &str, score: i64, tier: Tier, tiles: Vec<usize>) -> Candidate {
        let n = tiles.len();
        Candidate {
            word: word.into(),
            score,
            reach: score,
            pierces: false,
            tier,
            rank: None,
            typed: Typed { tiles, wildcards: vec![None; n], corners_used: 0, adjacent: false },
            cast: None,
        }
    }

    fn heals(need: i64, missing: i64) -> Heals {
        Heals { need, missing, bleed: 0, rested: false, wood_gear: false }
    }

    #[test]
    fn bonus_is_none_at_one() {
        assert_eq!(bonus_of(&Modifiers::none(), "CAT"), None);
    }

    /// Lexicons add on one accumulator (`utils/words.lua:219-242`): two 1.5x make 2.0x.
    #[test]
    fn bonus_adds_lexicons() {
        let mut mods = Modifiers::none();
        let set: HashSet<String> = ["CAT".to_string()].into();
        mods.bonuses = vec![(set.clone(), 1.5), (set, 1.5)];
        mods.bonus_names = vec!["Fire".into(), "Ice".into()];
        let both = Bonus { mult: 2.0, names: vec!["Fire".into(), "Ice".into()] };
        assert_eq!(bonus_of(&mods, "CAT"), Some(both.clone()));
        assert_eq!(both.to_string(), "x2.00 🔥❄️", "the emoji say which, without words");
        let unknown = Bonus { mult: 1.5, names: vec!["Mystery".into()] };
        assert_eq!(unknown.to_string(), "x1.50 Mystery", "a lexicon with no emoji keeps its name");
        assert_eq!(bonus_of(&mods, "DOG"), None);
    }

    /// `floor(overkill / 2)`, capped at what is missing (`rpgview.lua:1195-1196`).
    #[test]
    fn overkill_heals_half_capped_at_missing() {
        let tiles: Vec<Tile> = "CAT".chars().map(|c| Tile::plain(&c.to_string())).collect();
        let h = Heals { rested: true, ..heals(5, 3) };
        let s = scorer();
        assert_eq!(h.of(&tiles, &s, &cand("CAT", 9, Tier::InBand, vec![0, 1, 2])), 2);
        assert_eq!(h.of(&tiles, &s, &cand("CAT", 20, Tier::InBand, vec![0, 1, 2])), 3);
        assert_eq!(h.of(&tiles, &s, &cand("CAT", 4, Tier::Fallback, vec![0, 1, 2])), 0);
    }

    /// A rapier word's damage goes to health alone, so its overkill — and the heal — are counted
    /// from its reach: 45 into an enemy on 42 behind 54 armour overkills by 3, not by -51.
    #[test]
    fn a_rapier_kill_heals_from_its_reach() {
        let tiles: Vec<Tile> = "SINGES".chars().map(|c| Tile::plain(&c.to_string())).collect();
        let h = Heals { rested: true, ..heals(96, 10) };
        let c = Candidate {
            reach: 99,
            pierces: true,
            ..cand("SINGES", 45, Tier::InBand, vec![0, 1, 2, 3, 4, 5])
        };
        assert_eq!(h.of(&tiles, &scorer(), &c), 1, "floor(3 / 2)");
    }

    /// Bleeding skips the heal branch (`rpgview.lua:1080`).
    #[test]
    fn no_overkill_heal_while_bleeding() {
        let tiles: Vec<Tile> = "CAT".chars().map(|c| Tile::plain(&c.to_string())).collect();
        let h = Heals { rested: true, bleed: 2, ..heals(5, 3) };
        assert_eq!(h.of(&tiles, &scorer(), &cand("CAT", 20, Tier::InBand, vec![0, 1, 2])), 0);
    }

    /// `onWoodKillGainHealth`: 4 on a wood-only kill (`items/woodaugmentgear.lua`).
    #[test]
    fn a_wood_only_kill_heals_four_with_the_gear() {
        let tiles: Vec<Tile> = "EATZ".chars().map(|c| Tile::plain(&c.to_string())).collect();
        let h = Heals { wood_gear: true, ..heals(3, 10) };
        let s = scorer();
        assert_eq!(h.of(&tiles, &s, &cand("EAT", 3, Tier::InBand, vec![0, 1, 2])), 4);
        assert_eq!(h.of(&tiles, &s, &cand("ZEA", 12, Tier::InBand, vec![3, 0, 1])), 0, "Z is gold");
        assert_eq!(h.of(&tiles, &s, &cand("EAT", 2, Tier::Fallback, vec![0, 1, 2])), 0, "no kill");
    }

    /// A Diamond last: `3 × gems − bleed` (`rpg/effects/material/gemheal.lua`), kill or not.
    #[test]
    fn a_diamond_played_last_heals_three_per_gem() {
        let mut tiles: Vec<Tile> = "CAT".chars().map(|c| Tile::plain(&c.to_string())).collect();
        tiles[2] = Tile {
            letter: "T".into(),
            quality: Quality { material: Some("gemheal".into()), ..Quality::default() },
        };
        let s = scorer();
        let c = cand("CAT", 1, Tier::Fallback, vec![0, 1, 2]);
        assert_eq!(heals(100, 10).of(&tiles, &s, &c), 3);
        assert_eq!(Heals { bleed: 1, ..heals(100, 10) }.of(&tiles, &s, &c), 2);
        assert_eq!(heals(100, 2).of(&tiles, &s, &c), 2, "capped at what is missing");
        let first = cand("TCA", 1, Tier::Fallback, vec![2, 0, 1]);
        assert_eq!(heals(100, 10).of(&tiles, &s, &first), 0, "only when last");
    }

    /// Any bleed is bleeding, as Diggle reads it, even one the save writes as a fraction.
    #[test]
    fn a_fractional_bleed_still_counts() {
        let cs = |b: &str| {
            crate::game::save::parse(&format!(
                "return {{ rpg = {{ player = {{ statusEffects = {{ bleed = {b} }} }} }} }}"
            ))
            .unwrap()
        };
        assert_eq!(bleed_of(&cs("2.5")), 3);
        assert_eq!(bleed_of(&cs("0.25")), 1);
        assert_eq!(bleed_of(&cs("2")), 2);
        assert_eq!(bleed_of(&crate::game::save::parse("return {}").unwrap()), 0);
    }

    /// A ligature the game draws as a historical letter shows that letter
    /// (`ui/objects/tile.lua:40-73`, `ui/graphics/tiles/*_ligatures.png`); others stay as text.
    #[test]
    fn historical_ligatures_show_their_glyph() {
        let lig = |letter: &str, ligature: Option<&str>| Tile {
            letter: letter.into(),
            quality: Quality { ligature: ligature.map(str::to_string), ..Quality::default() },
        };
        let tiles = vec![
            lig("TH", Some("thorn")),
            lig("TH", Some("eth")),
            lig("AE", Some("ash")),
            lig("OE", Some("ethel")),
            lig("SS", Some("eszett")),
            lig("O", Some("multiO")),
            lig("ING", None),
            lig("TH", None),
            lig("QU", None),
        ];
        let c = cand("X", 1, Tier::Fallback, (0..tiles.len()).collect());
        let shown: Vec<String> = letters_of(&tiles, &c).into_iter().map(|l| l.text).collect();
        assert_eq!(shown, ["Þ", "Ð", "Æ", "Œ", "ß", "ꙮ", "ING", "TH", "QU"]);
    }

    /// The spell to cast leads, with its emoji; other qualifying spells follow; a spell sharing a
    /// dictionary shows that dictionary's emoji.
    #[test]
    fn the_spell_column_leads_with_the_cast_and_its_emoji() {
        assert_eq!(spell_column(Some("Turn"), vec!["Turn", "Four heal"]), vec!["💀", "💚"]);
        assert_eq!(spell_column(None, vec!["Fire"]), vec!["🔥"]);
        assert!(spell_column(None, vec![]).is_empty());
        assert_eq!(spell_column(None, vec!["UwU"]), vec!["UwU"], "UwU is its own label");
        assert_eq!(spell_column(Some("Metal"), vec!["Metal"]), vec!["🤘"]);
        assert_eq!(lexicon_emoji("Slime"), Some("🍮"));
        assert_eq!(lexicon_emoji("Swears"), Some("❗"), "the profane book's own mark");
    }

    /// Every dictionary the game ships has an emoji, so a bonus never shows a bare name by accident.
    #[test]
    fn every_lexicon_has_an_emoji() {
        let lex =
            crate::lexica::Lexica::load(&PathBuf::from("../sternly-worded-adventures")).unwrap();
        let statuses: std::collections::HashMap<String, f64> =
            lex.status_keys().iter().map(|k| (k.to_string(), 2.0)).collect();
        let names = lex.bonus_names(&statuses);
        assert!(names.len() >= 10, "{names:?}");
        for n in names {
            assert!(lexicon_emoji(&n).is_some(), "{n}");
        }
    }

    #[test]
    fn effects_match_goal_and_tier() {
        let kill = 5;
        let e = |goal: Goal, tier: Tier, score: i64| effect(goal, tier, score, score, kill);
        assert_eq!(e(Goal::RankedKill { need: 5 }, Tier::InBand, 6), "kills");
        assert_eq!(
            e(Goal::RankedKill { need: 5 }, Tier::Fallback, 3),
            "",
            "survives: nothing to say"
        );
        assert_eq!(e(Goal::Scare { need: 2, below: 4 }, Tier::InBand, 3), "scares");
        assert_eq!(
            e(Goal::Scare { need: 2, below: 4 }, Tier::Under, 1),
            "",
            "a safe hit under a scare"
        );
        assert_eq!(
            e(Goal::Scare { need: 2, below: 4 }, Tier::Overshoot, 9),
            "overshoots - would kill"
        );
        assert_eq!(e(Goal::Scare { need: 2, below: 4 }, Tier::Fallback, 0), "no damage");
        assert_eq!(e(Goal::FrugalKill { need: 5, below: 7 }, Tier::InBand, 5), "frugal kill");
        assert_eq!(e(Goal::FrugalKill { need: 5, below: 7 }, Tier::Fallback, 9), "kills");
        assert_eq!(e(Goal::MaxDamage, Tier::Fallback, 9), "kills");
        // A rapier word: it kills on its reach, and lands or not on its damage.
        assert_eq!(effect(Goal::MaxDamage, Tier::Fallback, 3, 9, kill), "kills");
        assert_eq!(effect(Goal::MaxDamage, Tier::Fallback, 0, 4, kill), "no damage");
    }

    /// The row shows each letter against the tile it is typed into — wildcards as their letter.
    #[test]
    fn letters_name_the_tile_each_one_comes_from() {
        let tiles: Vec<Tile> = "C.T".chars().map(|c| Tile::plain(&c.to_string())).collect();
        let mut c = cand("CAT", 1, Tier::Fallback, vec![0, 1, 2]);
        c.typed.wildcards = vec![None, Some('a'), None];
        let l = |text: &str, tile, wildcard| Letter { text: text.into(), tile, wildcard };
        assert_eq!(
            letters_of(&tiles, &c),
            vec![l("C", 0, false), l("A", 1, true), l("T", 2, false)],
            "the wildcard is flagged, so it can be drawn as the game draws it"
        );
    }
}
