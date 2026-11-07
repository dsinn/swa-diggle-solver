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
//! ## Unless the game casts it for you
//!
//! With the `alwaysCastSpell` option — **on by default** (`utils/defaultconfig.lua:24`), and on in
//! the user's config — a word that qualifies for **exactly one** spell has the plain attack taken
//! off the list, and the spell is what it does (`wordboard.lua:205-207`). Then there is no choice:
//! a metal word with Book of metal hits for what its armour does not absorb, and a word that casts
//! Four heal or Vowels hits for nothing. With two or more spells the plain attack stays, first.
//! See [`SpellBook::attack`].
//!
//! The user's report, 2026-11-07: words the advisor called kills fell short. The olexa idol puts
//! metal words near the top of the list on their plain damage, and the game cast Metal instead.
//!
//! **Diggle chooses too**, the user's call, 2026-11-07: *I intend for Diggle to cast a spell in
//! most cases, so stub it out*. Its book is built from the save with [`SpellBook::choose`] on, so it
//! is scored as casting the best spell. **Pressing the cast button is not written yet** — what it
//! takes is worked out in [`crate::cast`], with [`SpellBook::cast_slot`] naming the slot — and until
//! it is, Diggle plays the plain attack where a spell scored higher, and lands short of what it aimed
//! for. And the advisor's user is assumed to cast the spell its Spell column names first.

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
    power: Power,
    /// `userConfig.gameplay.alwaysCastSpell`: a word with exactly one spell casts it. The game's
    /// default is on; see [`always_cast_from_config`].
    pub always_cast: bool,
    /// Whether the player picks among the cast buttons: the hardest of them, when on. On for the
    /// advisor and, stubbed, for Diggle (see the module notes); off scores the first on the list —
    /// the plain attack, unless the game removed it.
    pub choose: bool,
}

/// The gear flag a spell is keyed by in `rpg/effects/spell/*.lua` (`key = 'spellFire'`), from the
/// heading the panel shows. Both vowel spells are headed `Vowels`; this names the first.
fn flag_of(heading: &str) -> &'static str {
    match heading {
        "Fire" => "spellFire",
        "Ice" => "spellIce",
        "Metal" => "spellMetal",
        "Drain" => "spellVampire",
        "Curse" => "spellSwears",
        "Turn" | "Turn (if undead)" => "spellTurnUndead",
        "Four heal" => "spellHeal4",
        "Vowels" => "spellAEIOU",
        "UwU" => "spellUwU",
        _ => "",
    }
}

/// Every spell flag this module models: the ten in `rpg/effects/spell/` as of v53.7.
pub const MODELLED_SPELLS: [&str; 10] = [
    "spellAEIOU",
    "spellAEIOUY",
    "spellFire",
    "spellHeal4",
    "spellIce",
    "spellMetal",
    "spellSwears",
    "spellTurnUndead",
    "spellUwU",
    "spellVampire",
];

/// Spells the save holds that this module cannot model: a key from the game's own
/// `rpg/effects/spell/*.lua`, read now rather than written down, that is not in
/// [`MODELLED_SPELLS`]. Non-empty means a word's panel can hold options we do not know of, so its
/// slots — and its damage — cannot be trusted. Future game versions and mods
/// (`mods.getActiveDirs`, `utils/enumerate.lua:62`) are what add them.
pub fn unmodelled_spells(game_dir: &std::path::Path, cs: &Table) -> Vec<String> {
    let Ok(dir) = std::fs::read_dir(game_dir.join("rpg/effects/spell")) else { return Vec::new() };
    let mut out: Vec<String> = dir
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("lua") {
                return None;
            }
            // Read as text, not evaluated: the files call into the game at load
            // (`words.dictionaries.blood.find`, `Vampire.lua:21`), which no shim survives. The key
            // is a literal on its own line; `hashRequire` falls back to the file name without one.
            let src = std::fs::read_to_string(&path).ok()?;
            let key = src.lines().find_map(|l| {
                let rest = l.trim_start().strip_prefix("key")?.trim_start().strip_prefix('=')?;
                let rest = rest.trim_start();
                let q = rest.chars().next().filter(|c| *c == '\'' || *c == '"')?;
                rest[1..].split(q).next().map(str::to_string)
            });
            Some(key.unwrap_or_else(|| path.file_stem().unwrap().to_string_lossy().into_owned()))
        })
        .filter(|k| !MODELLED_SPELLS.contains(&k.as_str()))
        .filter(|k| cs.path(&format!("rpg.player.gearFlags.{k}")).is_some())
        .collect();
    out.sort();
    out
}

/// `gameplay.alwaysCastSpell` from the game's `userConfig`, in the save folder. The game's default,
/// on (`utils/defaultconfig.lua:24`), when the file or the key is missing.
pub fn always_cast_from_config(save_dir: &std::path::Path) -> bool {
    std::fs::read_to_string(save_dir.join("userConfig"))
        .ok()
        .and_then(|src| crate::game::save::parse(&src).ok())
        .and_then(|cfg| cfg.path("gameplay.alwaysCastSpell").and_then(|v| v.as_bool()))
        .unwrap_or(true)
}

/// A school of magic, as `passiveSpellEffectBoost` names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum School {
    Arcane,
    Divine,
    Profane,
}

/// Power per school — arcane, divine, profane — as `passiveSpellEffectBoost` sums it
/// (`utils/effects.lua:21-32`): strength 1, plus 5% per `<school>SpellEffectBonus5` gear stack
/// and 1% per `<school>SpellEffectBonus1` status stack. Adept is ten gear stacks of the arcane
/// one (`items/classpassives.lua:50-86`), so 1.5.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Power([f64; 3]);

impl Default for Power {
    fn default() -> Self {
        Power([1.0; 3])
    }
}

impl Power {
    pub fn from_save(cs: &Table) -> Self {
        let number = |path: String| cs.path(&path).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let power = |school: &str| {
            1.0 + number(format!("rpg.player.gearFlags.{school}SpellEffectBonus5")) * 0.05
                + number(format!("rpg.player.statusEffects.{school}SpellEffectBonus1")) * 0.01
        };
        Power([power("arcane"), power("divine"), power("profane")])
    }

    /// `passiveSpellEffectBoost`, rounded half up as `math.round` is (`utils/maths.lua:24`).
    pub fn boost(&self, value: i64, school: School) -> i64 {
        (value as f64 * self.0[school as usize] + 0.5).floor() as i64
    }
}

/// The school a word's own tiles make it a spell of, if any: `wordboard.isTileSpellEffectWord`
/// (`wordboard.lua:346-356`), the first tile in word order whose **active** material or ligature
/// has a `spell` category.
///
/// A plain attack that is one is boosted by that school's [`Power`] (`wordboard.lua:611-612`), so
/// a Chrysocolla that starts a word makes it an arcane spell and, with Adept, hits half as hard
/// again. The tiles with a school, and when each is active (`rpg/effects/material/*.lua`,
/// `rpg/effects/ligature/multiO.lua`):
///
/// ```text
///   gemfire  Corundum      arcane  first
///   gemice   Chrysocolla   arcane  first or last
///   gemheal  Diamond       divine  last
///   multiO   Multiocular O divine  anywhere
/// ```
///
/// None of them is active while `noCastSpell` is held (`wordboard.lua:103-118`), which the caller
/// checks; see [`crate::search::Modifiers::tile_spells`].
pub fn tile_school(
    scorer: &crate::score::Scorer, consumed: &[crate::observe::board::Tile],
) -> Option<School> {
    let last = consumed.len().checked_sub(1)?;
    consumed.iter().enumerate().find_map(|(i, t)| {
        let material = match scorer.material_name(t) {
            Some("gemfire") if i == 0 => Some(School::Arcane),
            Some("gemice") if i == 0 || i == last => Some(School::Arcane),
            Some("gemheal") if i == last => Some(School::Divine),
            _ => None,
        };
        material
            .or_else(|| (t.quality.ligature.as_deref() == Some("multiO")).then_some(School::Divine))
    })
}

impl SpellBook {
    /// `undead` is whether the enemy is undead, from [`crate::advisor::bestiary::Bestiary`].
    pub fn from_save(cs: &Table, lexica: &Lexica, undead: Option<bool>) -> Self {
        let held = |flag: &str| cs.path(&format!("rpg.player.gearFlags.{flag}")).is_some();
        // `rpgview.lua:174-177`, with the cap `fight.rs` reads the same way.
        let max_armour = cs.int_at("rpg.player.maxHealth").map(|m| {
            // `math.ceil(maxHealth / 2)` (`rpgview.lua:66`, `overworld.lua:18`): rounded up, so
            // an odd maximum leaves one more armour to fill than `m / 2` says.
            if cs.path("rpg.player.gearFlags.maxArmourHalved").is_some() {
                (m + 1) / 2
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
            power: Power::from_save(cs),
            always_cast: true,
            choose: true,
        }
    }

    /// Is Turn undead held, with the enemy's nature unknown? Then its doubling is not counted.
    pub fn turn_undead_unknown(&self) -> bool {
        self.turn_undead.is_some() && self.undead.is_none()
    }

    fn boost(&self, value: i64, school: School) -> i64 {
        self.power.boost(value, school)
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

    /// Every held spell `word` qualifies for, **one entry per spell** as the game lists them
    /// (`wordboard.lua:180-203`), each with the damage it deals from the plain score `base`.
    ///
    /// Damage is each spell's `cast` (`rpg/effects/spell/*.lua`): Fire, Ice and Drain the boosted
    /// score, Curse and Turn double it first, Metal only what its armour does not absorb and only
    /// with its upgrade — otherwise nothing — and Four heal, both Vowels and UwU nothing at all.
    /// Turn needs the enemy to be undead (`TurnUndead.lua:22-25`); an enemy that cannot be told is
    /// counted as not.
    pub fn offers(&self, word: &str, letters: &[String], base: i64) -> Vec<(&'static str, i64)> {
        self.offers_flagged(word, letters, base).into_iter().map(|(_, h, d)| (h, d)).collect()
    }

    /// [`SpellBook::offers`], each with the gear flag the game keys the spell by — what orders the
    /// "Submit as" panel (see [`SpellBook::submit_list`]).
    fn offers_flagged(
        &self, word: &str, letters: &[String], base: i64,
    ) -> Vec<(&'static str, &'static str, i64)> {
        let word = word.to_ascii_uppercase();
        let mut out = Vec::new();
        for (heading, words) in &self.dictionary {
            if !words.contains(&word) {
                continue;
            }
            let damage = match *heading {
                "Fire" | "Ice" => self.boost(base, School::Arcane),
                "Drain" => self.boost(base, School::Profane),
                "Curse" => self.boost(base * 2, School::Profane),
                "Metal" if self.metal_damage => {
                    (self.boost(base, School::Arcane) - self.missing_armour).max(0)
                }
                _ => 0,
            };
            out.push((flag_of(heading), *heading, damage));
        }
        if self.undead == Some(true) && self.turn_undead.as_ref().is_some_and(|w| w.contains(&word))
        {
            out.push(("spellTurnUndead", "Turn", self.boost(base * 2, School::Divine)));
        }
        let four_tiles =
            letters.len() == 4 && letters.iter().all(|l| l.eq_ignore_ascii_case(&letters[0]));
        let four_letters = word.len() == 4 && word.bytes().all(|b| b == word.as_bytes()[0]);
        if self.heal4 && (four_tiles || four_letters) {
            out.push(("spellHeal4", "Four heal", 0));
        }
        // `utils/effects.lua:42-49`, each its own spell: AEIOU wants every vowel, AEIOUY a Y too.
        let has = |c: char| word.contains(c);
        let vowels = "AEIOU".chars().all(has);
        if self.vowels && vowels {
            out.push(("spellAEIOU", "Vowels", 0));
        }
        if self.vowels_y && vowels && has('Y') {
            out.push(("spellAEIOUY", "Vowels", 0));
        }
        let b = word.as_bytes();
        if self.uwu && b.len() == 3 && b[1] == b'W' && b[0] == b[2] && b"OUQV".contains(&b[0]) {
            out.push(("spellUwU", "UwU", 0));
        }
        out
    }

    /// The "Submit as" panel's options for `word`, in the game's order: the plain attack (`None`)
    /// first, then each qualifying spell by its flag (`Some("spellMetal")`).
    ///
    /// `getMagicWordPowers` (`wordboard.lua:168-209`) puts the attack first and walks the spells
    /// with `sortedpairs`, which sorts the keys — the flags, `spellAEIOU` … `spellVampire` — as
    /// strings (`utils/table.lua:489-500`), so the order is alphabetical by flag, not by heading.
    /// With [`SpellBook::always_cast`] and one spell, the attack is gone and the list is that spell.
    /// One entry means no panel: Enter submits it (`wordboard.lua:483-490`).
    pub fn submit_list(&self, word: &str, letters: &[String]) -> Vec<Option<&'static str>> {
        let mut spells: Vec<&'static str> =
            self.offers_flagged(word, letters, 0).into_iter().map(|(f, ..)| f).collect();
        spells.sort_unstable();
        if self.always_cast && spells.len() == 1 {
            return vec![Some(spells[0])];
        }
        std::iter::once(None).chain(spells.into_iter().map(Some)).collect()
    }

    /// Where the option for `cast` (a heading, as [`SpellBook::attack`] returns it; `None` for the
    /// plain attack) sits on the panel: `(slot, options)`, slot from 0. `None` when no panel opens
    /// — the word has one option, and Enter alone plays it — or the option is not on the list.
    pub fn cast_slot(
        &self, word: &str, letters: &[String], cast: Option<&str>,
    ) -> Option<(usize, usize)> {
        let list = self.submit_list(word, letters);
        if list.len() < 2 {
            return None;
        }
        let want = cast.map(flag_of);
        list.iter().position(|o| *o == want).map(|i| (i, list.len()))
    }

    /// What submitting `word` actually does: its damage, and the spell that deals it — `None` for
    /// the plain attack, which deals `plain` (the score with any boost the word's own tiles give,
    /// see [`crate::search::scored_cast`]); `base` is the score a spell is handed.
    ///
    /// - **Exactly one spell, with [`SpellBook::always_cast`]**: that spell, whatever it deals.
    /// - **Otherwise, choosing**: the hardest of the plain attack and every spell, a tie to the
    ///   plain attack.
    /// - **Otherwise**: the plain attack, first on the list.
    pub fn attack(
        &self, word: &str, letters: &[String], base: i64, plain: i64,
    ) -> (i64, Option<&'static str>) {
        let offers = self.offers(word, letters, base);
        if self.always_cast && offers.len() == 1 {
            return (offers[0].1, Some(offers[0].0));
        }
        if !self.choose {
            return (plain, None);
        }
        offers.into_iter().fold((plain, None), |best, (heading, damage)| {
            if damage > best.0 {
                (damage, Some(heading))
            } else {
                best
            }
        })
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

    fn gem(letter: &str, material: &str) -> crate::observe::board::Tile {
        let mut t = crate::observe::board::Tile::plain(letter);
        t.quality.material = Some(material.into());
        t
    }

    /// Each school tile counts only where it is active, and the first in word order decides.
    #[test]
    fn a_tile_makes_a_spell_only_where_it_is_active() {
        let sc = crate::score::Scorer::new(std::path::Path::new("../sternly-worded-adventures"))
            .unwrap();
        let p = crate::observe::board::Tile::plain;
        let school = |t: &[crate::observe::board::Tile]| tile_school(&sc, t);
        assert_eq!(school(&[gem("F", "gemice"), p("I"), p("R")]), Some(School::Arcane));
        assert_eq!(school(&[p("I"), p("R"), gem("F", "gemice")]), Some(School::Arcane), "last too");
        assert_eq!(school(&[p("D"), gem("F", "gemice"), p("T")]), None, "not mid-word");
        assert_eq!(school(&[p("A"), gem("T", "gemfire")]), None, "Corundum only first");
        assert_eq!(school(&[gem("T", "gemheal"), p("A")]), None, "Diamond only last");
        assert_eq!(school(&[p("A"), gem("T", "gemheal")]), Some(School::Divine));
        assert_eq!(
            school(&[gem("A", "gemfire"), gem("T", "gemheal")]),
            Some(School::Arcane),
            "the first tile's school"
        );
        let mut o = p("O");
        o.quality.ligature = Some("multiO".into());
        assert_eq!(school(&[p("N"), o, p("T")]), Some(School::Divine), "Multiocular O, anywhere");
        assert_eq!(school(&[]), None);
    }

    /// Adept is ten stacks of `arcaneSpellEffectBonus5`: 1.5x, rounded half up.
    #[test]
    fn adept_makes_arcane_half_as_strong_again() {
        let cs =
            parse("return { rpg = { player = { gearFlags = { arcaneSpellEffectBonus5 = 10 } } } }")
                .unwrap();
        let power = Power::from_save(&cs);
        assert_eq!(power.boost(20, School::Arcane), 30);
        assert_eq!(power.boost(15, School::Arcane), 23, "22.5 rounds up");
        assert_eq!(power.boost(20, School::Divine), 20, "only its own school");
        assert_eq!(Power::default().boost(20, School::Arcane), 20);
    }

    /// The user's report, 2026-11-07: with `alwaysCastSpell` on, a metal word under Book of metal
    /// *is* the Metal spell — the plain attack is taken away — so it hits for what the armour it
    /// grants does not absorb, however hard the plain attack would have hit.
    #[test]
    fn a_lone_spell_is_cast_for_you() {
        let lex = lexica();
        let metal = lex.words_for("metal").unwrap().iter().next().unwrap().clone();
        let cs = parse(
            "return { rpg = { player = { gearFlags = { spellMetal = 1, spellMetalDamage = 1, \
             arcaneSpellEffectBonus5 = 10 }, maxHealth = 20, armour = 12 } } }",
        )
        .unwrap();
        let mut book = SpellBook::from_save(&cs, &lex, None);
        book.choose = true;
        let l = letters(&metal);
        // 20 boosted to 30, less the 8 armour it fills first.
        assert_eq!(book.attack(&metal, &l, 20, 25), (22, Some("Metal")), "under the plain 25");
        book.always_cast = false;
        assert_eq!(book.attack(&metal, &l, 20, 25), (25, None), "with a choice, the plain attack");
        assert_eq!(book.attack(&metal, &l, 20, 18), (22, Some("Metal")), "or Metal, if harder");
        // Diggle cannot choose: the plain attack, unless the game removed it.
        book.choose = false;
        assert_eq!(book.attack(&metal, &l, 20, 18), (18, None));
        book.always_cast = true;
        assert_eq!(book.attack(&metal, &l, 20, 25), (22, Some("Metal")));
        // A word with no spell is the plain attack, always.
        assert_eq!(book.attack("ZZZQ", &letters("ZZZQ"), 20, 25), (25, None));
    }

    /// A lone spell that deals nothing takes the whole hit with it; a second spell gives the
    /// choice back, plain attack included (`wordboard.lua:205`).
    #[test]
    fn a_lone_healing_spell_deals_nothing_and_two_spells_give_the_choice_back() {
        let lex = lexica();
        let one = SpellBook {
            choose: true,
            ..SpellBook::from_save(&holding(&["spellHeal4"]), &lex, None)
        };
        assert_eq!(one.attack("AAAA", &letters("AAAA"), 10, 10), (0, Some("Four heal")));
        let swear = lex.words_for("swears").unwrap().iter().next().unwrap().clone();
        let both = SpellBook {
            choose: true,
            ..SpellBook::from_save(&holding(&["spellSwears", "spellUwU"]), &lex, None)
        };
        assert_eq!(both.offers(&swear, &letters(&swear), 10).len(), 1, "UwU needs OWO");
        let two = SpellBook {
            choose: true,
            ..SpellBook::from_save(&holding(&["spellAEIOU", "spellAEIOUY"]), &lex, None)
        };
        let w = "EDUCATIONALLY";
        assert_eq!(two.offers(w, &letters(w), 10).len(), 2, "each vowel spell is its own entry");
        assert_eq!(two.attack(w, &letters(w), 10, 10), (10, None), "so the plain attack stays");
    }

    /// Halved armour caps at `ceil(maxHealth / 2)` (`rpgview.lua:66`): with 21 health, 11, so at 5
    /// armour Metal fills 6 before it hits — one more than flooring to 10 would say.
    #[test]
    fn halved_armour_rounds_its_cap_up() {
        let lex = lexica();
        let metal = lex.words_for("metal").unwrap().iter().next().unwrap().clone();
        let cs = parse(
            "return { rpg = { player = { gearFlags = { spellMetal = 1, spellMetalDamage = 1,              maxArmourHalved = 1 }, maxHealth = 21, armour = 5 } } }",
        )
        .unwrap();
        let book = SpellBook::from_save(&cs, &lex, None);
        assert_eq!(book.offers(&metal, &letters(&metal), 20), vec![("Metal", 14)]);
    }

    /// The panel lists the attack, then the spells alphabetically by flag — `spellMetal` before
    /// `spellTurnUndead`, though `Metal` and `Turn` would sort the same way by heading, and
    /// `spellFire` before `spellIce`.
    #[test]
    fn the_panel_lists_the_attack_then_spells_by_flag() {
        let lex = lexica();
        let metal = lex.words_for("metal").unwrap();
        let fire = lex.words_for("fire").unwrap();
        let ice = lex.words_for("ice").unwrap();
        // The first in alphabetical order, not whatever the set yields first: a word that is also a
        // metal word would put Metal on the panel too, and a set's order changes between runs.
        let first = |pick: &dyn Fn(&String) -> bool, set: &HashSet<String>| {
            let mut v: Vec<&String> = set.iter().filter(|w| pick(w)).collect();
            v.sort();
            v.first().map(|w| (*w).clone())
        };
        let both = first(&|w| ice.contains(w) && !metal.contains(w), fire)
            .expect("a word both fire and ice, not metal");
        let both = &both;
        let book =
            SpellBook::from_save(&holding(&["spellFire", "spellIce", "spellMetal"]), &lex, None);
        let l = letters(both);
        assert_eq!(
            book.submit_list(both, &l),
            vec![None, Some("spellFire"), Some("spellIce")],
            "{both}"
        );
        assert_eq!(book.cast_slot(both, &l, Some("Ice")), Some((2, 3)));
        assert_eq!(book.cast_slot(both, &l, None), Some((0, 3)), "the attack, first");
        // One spell under alwaysCastSpell: no panel; Enter casts it.
        let only = first(&|w| !fire.contains(w) && !ice.contains(w), metal).unwrap();
        let only = &only;
        assert_eq!(book.submit_list(only, &letters(only)), vec![Some("spellMetal")]);
        assert_eq!(book.cast_slot(only, &letters(only), Some("Metal")), None);
        // Without it, the attack stays and the panel opens.
        let mut chooser = book.clone();
        chooser.always_cast = false;
        assert_eq!(chooser.cast_slot(only, &letters(only), Some("Metal")), Some((1, 2)));
        assert!(MODELLED_SPELLS.windows(2).all(|w| w[0] < w[1]), "kept in the game's order");
    }

    /// The game's own spell folder is read for keys we do not model; today there are none.
    #[test]
    fn every_spell_the_game_ships_is_modelled() {
        let game = std::path::Path::new("../sternly-worded-adventures");
        // The save holds every spell the game ships, written out here rather than taken from
        // `MODELLED_SPELLS`, so a spell dropped from that list is one held and unmodelled.
        let shipped = [
            "spellAEIOU",
            "spellAEIOUY",
            "spellFire",
            "spellHeal4",
            "spellIce",
            "spellMetal",
            "spellSwears",
            "spellTurnUndead",
            "spellUwU",
            "spellVampire",
        ];
        let all: Vec<String> = shipped.iter().map(|f| format!("{f} = 1")).collect();
        let extra = "spellFromAFutureVersion = 1";
        let cs = parse(&format!(
            "return {{ rpg = {{ player = {{ gearFlags = {{ {}, {extra} }} }} }} }}",
            all.join(", ")
        ))
        .unwrap();
        assert!(unmodelled_spells(game, &cs).is_empty(), "the folder holds only what we model");
        // And a flag that is not a spell file is not reported: only the folder decides.
        assert!(!unmodelled_spells(game, &cs).contains(&"spellFromAFutureVersion".to_string()));
    }

    /// The setting is the user's, read from the save folder; missing means the game's default, on.
    #[test]
    fn always_cast_is_read_from_the_user_config() {
        let dir = std::env::temp_dir().join(format!("diggle-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(always_cast_from_config(&dir), "no file: the default");
        let write = |v: &str| {
            std::fs::write(
                dir.join("userConfig"),
                format!("return {{ gameplay = {{ alwaysCastSpell = {v} }} }}"),
            )
            .unwrap()
        };
        write("false");
        assert!(!always_cast_from_config(&dir));
        write("true");
        assert!(always_cast_from_config(&dir));
        let _ = std::fs::remove_dir_all(&dir);
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
