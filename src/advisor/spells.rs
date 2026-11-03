//! Which of the player's spells a word would cast.
//!
//! A spell is held when its gear flag is set — a spellbook sets it for good, an opened scroll for
//! the rest of the fight (`utils/items.lua:591-612`), and both land in `rpg.player.gearFlags`. Each
//! spell's own `check` decides whether a word casts it (`rpg/effects/spell/*.lua`); those are
//! reproduced here and labelled with the game's `castHeading`.
//!
//! Display only: Diggle's ranking does not model spells, and this does not change the order.

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

/// The spells the player holds this turn, ready to test words against.
pub struct Spells<'a> {
    dictionary: Vec<(&'static str, &'a HashSet<String>)>,
    turn_undead: Option<&'a HashSet<String>>,
    heal4: bool,
    vowels: bool,
    vowels_y: bool,
    uwu: bool,
}

impl<'a> Spells<'a> {
    pub fn from_save(cs: &Table, lexica: &'a Lexica) -> Self {
        let held = |flag: &str| cs.path(&format!("rpg.player.gearFlags.{flag}")).is_some();
        Spells {
            dictionary: DICTIONARY_SPELLS
                .iter()
                .filter(|(flag, ..)| held(flag))
                .filter_map(|&(_, heading, key)| Some((heading, lexica.words_for(key)?)))
                .collect(),
            turn_undead: held("spellTurnUndead").then(|| lexica.words_for("bone")).flatten(),
            heal4: held("spellHeal4"),
            vowels: held("spellAEIOU"),
            vowels_y: held("spellAEIOUY"),
            uwu: held("spellUwU"),
        }
    }

    /// The headings of the held spells `word` would cast. `letters` is one entry per tile, as
    /// typed — a ligature tile is one entry (`TH`), which is what Four heal counts.
    pub fn cast_by(&self, word: &str, letters: &[String]) -> Vec<&'static str> {
        let word = word.to_ascii_uppercase();
        let mut out: Vec<&'static str> = self
            .dictionary
            .iter()
            .filter(|(_, words)| words.contains(&word))
            .map(|(h, _)| *h)
            .collect();
        // Turn undead also needs the enemy to be undead (`TurnUndead.lua:22-25`), and the save
        // does not record the enemy's categories, so the label carries the condition.
        if self.turn_undead.is_some_and(|words| words.contains(&word)) {
            out.push("Turn (if undead)");
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
        let held = Spells::from_save(&holding(&["spellFire"]), &lex);
        assert_eq!(held.cast_by(&fire, &letters(&fire)), vec!["Fire"]);
        let none = Spells::from_save(&holding(&[]), &lex);
        assert!(none.cast_by(&fire, &letters(&fire)).is_empty());
    }

    /// Four of one letter heals (`rpg/effects/spell/Heal4.lua`), counting a ligature as one.
    #[test]
    fn four_of_a_letter_casts_four_heal() {
        let lex = lexica();
        let s = Spells::from_save(&holding(&["spellHeal4"]), &lex);
        assert_eq!(s.cast_by("AAAA", &letters("AAAA")), vec!["Four heal"]);
        let ths = vec!["TH".to_string(); 4];
        assert_eq!(s.cast_by("THTHTHTH", &ths), vec!["Four heal"], "four TH tiles");
        assert!(s.cast_by("AAAB", &letters("AAAB")).is_empty());
    }

    /// Every vowel casts the vowel spell; the greater one also wants a Y (`utils/effects.lua:42-49`).
    #[test]
    fn all_five_vowels_cast_vowels() {
        let lex = lexica();
        let s = Spells::from_save(&holding(&["spellAEIOU"]), &lex);
        assert_eq!(s.cast_by("EDUCATION", &letters("EDUCATION")), vec!["Vowels"]);
        let y = Spells::from_save(&holding(&["spellAEIOUY"]), &lex);
        assert!(y.cast_by("EDUCATION", &letters("EDUCATION")).is_empty(), "no Y");
    }

    #[test]
    fn owo_casts_uwu() {
        let lex = lexica();
        let s = Spells::from_save(&holding(&["spellUwU"]), &lex);
        assert_eq!(s.cast_by("OWO", &letters("OWO")), vec!["UwU"]);
        assert!(s.cast_by("OWU", &letters("OWU")).is_empty());
    }

    /// Turn undead needs an undead enemy, which the save does not say; the label says so.
    #[test]
    fn turn_undead_is_marked_as_needing_undead() {
        let lex = lexica();
        let bone = lex.words_for("bone").unwrap().iter().next().unwrap().clone();
        let s = Spells::from_save(&holding(&["spellTurnUndead"]), &lex);
        assert_eq!(s.cast_by(&bone, &letters(&bone)), vec!["Turn (if undead)"]);
    }
}
