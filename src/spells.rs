//! The player's spells: which a word would cast, and how hard each would hit.
//!
//! A spell is held when its gear flag is set — a spellbook sets it for good, an opened scroll for
//! the rest of the fight (`utils/items.lua:591-612`), and both land in `rpg.player.gearFlags`. Each
//! spell's own `check` decides whether a word casts it (`rpg/effects/spell/*.lua`); those are
//! reproduced here and labelled with the game's `castHeading`.
//!
//! ## Casting is a choice
//!
//! Submitting a word that qualifies for a held spell does not cast it. The game lists the plain
//! attack first and each qualifying spell after it (`wordboard.lua:166-200`), and offers them as
//! buttons (`ui/elements/rpgcast.lua:238-242`, opened from `wordboard.lua:483-487`). So a spell
//! never takes damage away — the plain attack is always on offer — and [`SpellBook::best_cast`] is
//! the better of the two.
//!
//! **Only the advisor has a book.** [`crate::search::Modifiers::spells`] is `None` for Diggle,
//! which never presses the cast buttons: scoring a word by a spell it will not cast would aim it at
//! damage it cannot deal.

use crate::game::save::Table;
use crate::lexica::Lexica;
use std::collections::HashSet;

/// The spells whose check is "the word is in this dictionary": flag, heading, dictionary key.
const DICTIONARY_SPELLS: [(&str, &str, &str); 5] = [
    ("spellFire", "Fire", "fire"),      // rpg/effects/spell/Fire.lua:18
    ("spellIce", "Ice", "ice"),         // Ice.lua:18
    ("spellMetal", "Metal", "metal"),   // Metal.lua:17
    ("spellVampire", "Drain", "blood"), // Vampire.lua:21
    ("spellSwears", "Curse", "swears"), // Swears.lua:22
];

/// The spells the player holds this turn, and what decides their damage.
#[derive(Debug, Clone, Default)]
pub struct SpellBook {
    dictionary: Vec<(&'static str, HashSet<String>)>,
    turn_undead: Option<HashSet<String>>,
    /// Is the enemy undead? `None` when it cannot be told, which Turn treats as "no".
    undead: Option<bool>,
    heal4: bool,
    vowels: bool,
    vowels_y: bool,
    uwu: bool,
    /// `spellMetalDamage`: Metal hits for what the armour it grants does not absorb.
    metal_damage: bool,
    missing_armour: i64,
    /// Power per school — arcane, divine, profane — as `passiveSpellEffectBoost` sums it.
    power: [f64; 3],
}

#[derive(Clone, Copy)]
enum School {
    Arcane,
    Divine,
    Profane,
}

impl SpellBook {
    /// `undead` is whether the enemy is undead, from [`crate::advisor::bestiary::Bestiary`].
    pub fn from_save(cs: &Table, lexica: &Lexica, undead: Option<bool>) -> Self {
        let held = |flag: &str| cs.path(&format!("rpg.player.gearFlags.{flag}")).is_some();
        let number = |path: String| cs.path(&path).and_then(|v| v.as_f64()).unwrap_or(0.0);
        // `utils/effects.lua:21-32`: strength 1, plus 5% per gear stack and 1% per status stack.
        let power = |school: &str| {
            1.0 + number(format!("rpg.player.gearFlags.{school}SpellEffectBonus5")) * 0.05
                + number(format!("rpg.player.statusEffects.{school}SpellEffectBonus1")) * 0.01
        };
        // `rpgview.lua:174-177`, with the cap `fight.rs` reads the same way.
        let max_armour = cs.int_at("rpg.player.maxHealth").map(|m| {
            if cs.path("rpg.player.gearFlags.maxArmourHalved").is_some() {
                m / 2
            } else {
                m
            }
        });
        let armour = cs.int_at("rpg.player.armour").unwrap_or(0);
        SpellBook {
            dictionary: DICTIONARY_SPELLS
                .iter()
                .filter(|(flag, ..)| held(flag))
                .filter_map(|&(_, heading, key)| Some((heading, lexica.words_for(key)?.clone())))
                .collect(),
            turn_undead: held("spellTurnUndead")
                .then(|| lexica.words_for("bone").cloned())
                .flatten(),
            undead,
            heal4: held("spellHeal4"),
            vowels: held("spellAEIOU"),
            vowels_y: held("spellAEIOUY"),
            uwu: held("spellUwU"),
            metal_damage: held("spellMetalDamage"),
            missing_armour: max_armour.map(|m| (m - armour).clamp(0, m)).unwrap_or(0),
            power: [power("arcane"), power("divine"), power("profane")],
        }
    }

    /// Is Turn undead held, with the enemy's nature unknown? Then its doubling is not counted.
    pub fn turn_undead_unknown(&self) -> bool {
        self.turn_undead.is_some() && self.undead.is_none()
    }

    /// `passiveSpellEffectBoost` (`utils/effects.lua:21-32`), rounded half up as `math.round` is.
    fn boost(&self, value: i64, school: School) -> i64 {
        (value as f64 * self.power[school as usize] + 0.5).floor() as i64
    }

    /// The best damage `word` can deal, given its plain-attack damage `base`, and the spell to
    /// cast for it — `None` when the plain attack is as good as any spell.
    ///
    /// Each spell's damage is its `estimate` (`rpg/effects/spell/*.lua`): Fire, Ice and Drain hit
    /// for the boosted damage, Curse and Turn for double, Metal for what its armour does not absorb
    /// and only with its upgrade. Four heal, Vowels and UwU deal none, so they never win here.
    pub fn best_cast(&self, word: &str, base: i64) -> (i64, Option<&'static str>) {
        let word = word.to_ascii_uppercase();
        let mut best = (base, None);
        let mut offer = |damage: i64, heading: &'static str| {
            if damage > best.0 {
                best = (damage, Some(heading));
            }
        };
        for (heading, words) in &self.dictionary {
            if !words.contains(&word) {
                continue;
            }
            match *heading {
                "Fire" | "Ice" => offer(self.boost(base, School::Arcane), heading),
                "Drain" => offer(self.boost(base, School::Profane), heading),
                "Curse" => offer(self.boost(base * 2, School::Profane), heading),
                "Metal" if self.metal_damage => {
                    offer((self.boost(base, School::Arcane) - self.missing_armour).max(0), heading)
                }
                _ => {}
            }
        }
        if self.undead == Some(true) && self.turn_undead.as_ref().is_some_and(|w| w.contains(&word))
        {
            offer(self.boost(base * 2, School::Divine), "Turn");
        }
        best
    }

    /// The headings of the held spells `word` would cast. `letters` is one entry per tile, as
    /// typed — a ligature tile is one entry (`TH`), which is what Four heal counts.
    pub fn qualifying(&self, word: &str, letters: &[String]) -> Vec<&'static str> {
        let word = word.to_ascii_uppercase();
        let mut out: Vec<&'static str> = self
            .dictionary
            .iter()
            .filter(|(_, words)| words.contains(&word))
            .map(|(h, _)| *h)
            .collect();
        // Turn undead also needs the enemy to be undead (`TurnUndead.lua:22-25`).
        if self.turn_undead.as_ref().is_some_and(|words| words.contains(&word)) {
            match self.undead {
                Some(true) => out.push("Turn"),
                Some(false) => {}
                None => out.push("Turn (if undead)"),
            }
        }
        // `Heal4.lua:17-34`: four tiles of one letter, or a four-letter word of one letter.
        let four_tiles =
            letters.len() == 4 && letters.iter().all(|l| l.eq_ignore_ascii_case(&letters[0]));
        let four_letters = word.len() == 4 && word.bytes().all(|b| b == word.as_bytes()[0]);
        if self.heal4 && (four_tiles || four_letters) {
            out.push("Four heal");
        }
        // `utils/effects.lua:42-49`: every vowel, and Y too for the greater spell. The word is
        // already a dictionary word, which is the check's other half.
        let has = |c: char| word.contains(c);
        let vowels = "AEIOU".chars().all(has);
        if (self.vowels && vowels) || (self.vowels_y && vowels && has('Y')) {
            out.push("Vowels");
        }
        // `UwU.lua:14-16`: three letters, the outer two the same one of O, U, Q, V, a W between.
        let b = word.as_bytes();
        if self.uwu && b.len() == 3 && b[1] == b'W' && b[0] == b[2] && b"OUQV".contains(&b[0]) {
            out.push("UwU");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::save::parse;

    fn lexica() -> crate::lexica::Lexica {
        crate::lexica::Lexica::load(std::path::Path::new("../sternly-worded-adventures")).unwrap()
    }

    fn holding(flags: &[&str]) -> crate::game::save::Table {
        let flags: Vec<String> = flags.iter().map(|f| format!("{f} = 1")).collect();
        parse(&format!(
            "return {{ rpg = {{ player = {{ gearFlags = {{ {} }} }} }} }}",
            flags.join(", ")
        ))
        .unwrap()
    }

    fn letters(word: &str) -> Vec<String> {
        word.chars().map(|c| c.to_string()).collect()
    }

    /// A word from a spell's dictionary casts it, but only when the spell is held.
    #[test]
    fn a_dictionary_word_casts_its_spell_only_when_held() {
        let lex = lexica();
        let fire = lex.words_for("fire").unwrap().iter().next().unwrap().clone();
        let held = SpellBook::from_save(&holding(&["spellFire"]), &lex, None);
        assert_eq!(held.qualifying(&fire, &letters(&fire)), vec!["Fire"]);
        let none = SpellBook::from_save(&holding(&[]), &lex, None);
        assert!(none.qualifying(&fire, &letters(&fire)).is_empty());
    }

    /// Four of one letter heals (`rpg/effects/spell/Heal4.lua`), counting a ligature as one.
    #[test]
    fn four_of_a_letter_casts_four_heal() {
        let lex = lexica();
        let s = SpellBook::from_save(&holding(&["spellHeal4"]), &lex, None);
        assert_eq!(s.qualifying("AAAA", &letters("AAAA")), vec!["Four heal"]);
        let ths = vec!["TH".to_string(); 4];
        assert_eq!(s.qualifying("THTHTHTH", &ths), vec!["Four heal"], "four TH tiles");
        assert!(s.qualifying("AAAB", &letters("AAAB")).is_empty());
    }

    /// Every vowel casts the vowel spell; the greater one also wants a Y (`utils/effects.lua:42-49`).
    #[test]
    fn all_five_vowels_cast_vowels() {
        let lex = lexica();
        let s = SpellBook::from_save(&holding(&["spellAEIOU"]), &lex, None);
        assert_eq!(s.qualifying("EDUCATION", &letters("EDUCATION")), vec!["Vowels"]);
        let y = SpellBook::from_save(&holding(&["spellAEIOUY"]), &lex, None);
        assert!(y.qualifying("EDUCATION", &letters("EDUCATION")).is_empty(), "no Y");
    }

    #[test]
    fn owo_casts_uwu() {
        let lex = lexica();
        let s = SpellBook::from_save(&holding(&["spellUwU"]), &lex, None);
        assert_eq!(s.qualifying("OWO", &letters("OWO")), vec!["UwU"]);
        assert!(s.qualifying("OWU", &letters("OWU")).is_empty());
    }

    fn bone(lex: &crate::lexica::Lexica) -> String {
        lex.words_for("bone").unwrap().iter().next().unwrap().clone()
    }

    /// Turn undead doubles a bone word's damage against an undead enemy, and only then
    /// (`TurnUndead.lua:22-25,32-34`).
    #[test]
    fn turn_undead_doubles_only_against_the_undead() {
        let lex = lexica();
        let w = bone(&lex);
        let undead = SpellBook::from_save(&holding(&["spellTurnUndead"]), &lex, Some(true));
        assert_eq!(undead.best_cast(&w, 44), (88, Some("Turn")));
        assert_eq!(undead.qualifying(&w, &letters(&w)), vec!["Turn"]);
        let living = SpellBook::from_save(&holding(&["spellTurnUndead"]), &lex, Some(false));
        assert_eq!(living.best_cast(&w, 44), (44, None));
        assert!(living.qualifying(&w, &letters(&w)).is_empty(), "no Turn against the living");
        let unknown = SpellBook::from_save(&holding(&["spellTurnUndead"]), &lex, None);
        assert_eq!(unknown.best_cast(&w, 44), (44, None), "unknown counts as not undead");
    }

    /// Curse doubles (`Swears.lua:40-41`); Fire keeps the damage (`Fire.lua:30-31`); a heal spell
    /// deals none, so the plain attack stays the better choice (`Heal4.lua:62-63`).
    #[test]
    fn each_spell_offers_its_own_damage() {
        let lex = lexica();
        let swear = lex.words_for("swears").unwrap().iter().next().unwrap().clone();
        let curse = SpellBook::from_save(&holding(&["spellSwears"]), &lex, None);
        assert_eq!(curse.best_cast(&swear, 10), (20, Some("Curse")));
        let fire = lex.words_for("fire").unwrap().iter().next().unwrap().clone();
        let book = SpellBook::from_save(&holding(&["spellFire"]), &lex, None);
        assert_eq!(book.best_cast(&fire, 10), (10, None), "a tie stays the plain attack");
        let heal = SpellBook::from_save(&holding(&["spellHeal4"]), &lex, None);
        assert_eq!(heal.best_cast("AAAA", 10), (10, None));
    }

    /// A school's power boost raises its spells' damage: 5% per `<school>SpellEffectBonus5`
    /// stack, 1% per status stack, rounded half up (`utils/effects.lua:21-32`).
    #[test]
    fn a_school_boost_raises_spell_damage() {
        let lex = lexica();
        let fire = lex.words_for("fire").unwrap().iter().next().unwrap().clone();
        let cs = parse(
            "return { rpg = { player = { gearFlags = { spellFire = 1, arcaneSpellEffectBonus5 = 2 },              statusEffects = { arcaneSpellEffectBonus1 = 5 } } } }",
        )
        .unwrap();
        let book = SpellBook::from_save(&cs, &lex, None);
        assert_eq!(book.best_cast(&fire, 20), (23, Some("Fire")), "20 x 1.15 = 23");
    }

    /// Metal deals damage only with its upgrade, and then only what the armour it grants does not
    /// absorb (`Metal.lua:35-38`) — so it beats the plain attack only when spell power outweighs
    /// the missing armour.
    #[test]
    fn metal_hits_only_with_its_upgrade_and_past_missing_armour() {
        let lex = lexica();
        let metal = lex.words_for("metal").unwrap().iter().next().unwrap().clone();
        let plain = SpellBook::from_save(&holding(&["spellMetal"]), &lex, None);
        assert_eq!(plain.best_cast(&metal, 10), (10, None), "no upgrade, no damage");
        let save = |boost: u32| {
            parse(&format!(
                "return {{ rpg = {{ player = {{ gearFlags = {{ spellMetal = 1, spellMetalDamage = 1,                  arcaneSpellEffectBonus5 = {boost} }}, maxHealth = 20, armour = 15 }} }} }}"
            ))
            .unwrap()
        };
        let unboosted = SpellBook::from_save(&save(0), &lex, None);
        assert_eq!(unboosted.best_cast(&metal, 30), (30, None), "25 after armour loses to 30");
        let boosted = SpellBook::from_save(&save(2), &lex, None);
        assert_eq!(boosted.best_cast(&metal, 100), (105, Some("Metal")), "110 less 5");
    }

    /// Turn undead needs an undead enemy, which the save does not say; the label says so.
    #[test]
    fn turn_undead_is_marked_as_needing_undead() {
        let lex = lexica();
        let bone = lex.words_for("bone").unwrap().iter().next().unwrap().clone();
        let s = SpellBook::from_save(&holding(&["spellTurnUndead"]), &lex, None);
        assert_eq!(s.qualifying(&bone, &letters(&bone)), vec!["Turn (if undead)"]);
    }
}
