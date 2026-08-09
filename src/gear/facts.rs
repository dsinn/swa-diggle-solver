//! What a modifier's condition needs to know, computed only when something asks.
//!
//! Two shapes, because the two have different lifetimes:
//!
//! - [`FightFacts`] is tallied once per turn from `combatSaveData`. Only the Raven and Whale idols
//!   need it, and each half is built only when its own idol is worn.
//! - [`Facts`] is built per candidate word and memoises each answer, so two modifiers wanting the
//!   same input pay once — the job `wordCache` does in the game (`utils/words.lua:328-332`).
//!
//! **Nothing here is precomputed across the dictionary.** Measured on a full board, 1750 of 345,128
//! words are typeable (0.51%), and `search::scored` runs only on those — so a table parallel to the
//! word list would have built 345k entries to answer 1750 questions. See
//! `search::typeable_count::only_a_fraction_of_the_dictionary_can_be_spelled_on_one_board`.

use super::table::{Dict, Fact, SUFFIXES_LONGEST_FIRST};
use crate::game::save::Table;
use crate::observe::board::Tile;
use crate::typist::Typed;
use std::cell::Cell;
use std::collections::HashMap;

/// Tallies over the words already submitted this fight.
///
/// Read from `usedWords` rather than accumulated as we go, because **we resume fights**: a save
/// picked up mid-combat already carries entries submitted before this process attached, and counters
/// we owned would start at zero and be short by exactly that history. Reading also removes the
/// lifecycle — no clear-at-end-of-combat to get right across a win, a death, an abandon or a resume,
/// because a new fight is a new `combatSaveData` with an empty list.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FightFacts {
    /// `player.turnNumber` (`rpgview.lua:158`), which the quill modifiers read.
    pub turn: i64,
    /// Whale idol. Prior words per first letter; index 26 is anything not A-Z.
    pub alliteration: Option<[u16; 27]>,
    /// Raven idol. Prior words, whole.
    pub repeats: Option<HashMap<String, u16>>,
}

impl FightFacts {
    /// Tallies from the save, building only the halves the plan will actually read.
    ///
    /// `want_alliteration` and `want_repeats` come from the compiled plan, so with neither idol worn
    /// `usedWords` is not even walked.
    pub fn from_save(save: &Table, want_alliteration: bool, want_repeats: bool) -> Self {
        let turn = save.int_at("rpg.player.turnNumber").unwrap_or(0);
        if !want_alliteration && !want_repeats {
            return FightFacts { turn, ..Default::default() };
        }
        let mut letters = [0u16; 27];
        let mut words: HashMap<String, u16> = HashMap::new();
        if let Some(list) = save.table_at("usedWords") {
            // A Lua array parses to numerically-keyed entries (`game/save.rs:171`).
            for value in list.map.values() {
                let crate::game::save::Value::Table(entry) = value else { continue };
                // `stats.repeatCount` skips entries where `v.hint` is set (`rpgstats.lua:122`); an
                // absent key reads as false in Lua, and the live saves carry `{word, damage}` only.
                if entry.path("hint").is_some() {
                    continue;
                }
                let Some(word) = entry.str_at("word") else { continue };
                let upper = word.to_ascii_uppercase();
                letters[first_letter_slot(&upper)] += 1;
                *words.entry(upper).or_insert(0) += 1;
            }
        }
        FightFacts {
            turn,
            alliteration: want_alliteration.then_some(letters),
            repeats: want_repeats.then_some(words),
        }
    }

    /// How many earlier words started with the same letter as this one.
    ///
    /// `stats.alliterationCount` (`rpgstats.lua:129-138`) is handed only `word:sub(1,1)`, which is
    /// why one 27-entry tally answers it for every candidate instead of a walk per word.
    pub fn alliteration_for(&self, word: &str) -> Option<u16> {
        self.alliteration.as_ref().map(|t| t[first_letter_slot(word)])
    }

    /// How many times this exact word has already been played.
    pub fn repeats_for(&self, word: &str) -> Option<u16> {
        self.repeats.as_ref().map(|m| m.get(word).copied().unwrap_or(0))
    }
}

fn first_letter_slot(word: &str) -> usize {
    match word.as_bytes().first() {
        Some(b) if b.is_ascii_uppercase() => (b - b'A') as usize,
        Some(b) if b.is_ascii_lowercase() => (b.to_ascii_uppercase() - b'A') as usize,
        _ => 26,
    }
}

/// Word and board facts for one candidate, each computed at most once.
pub struct Facts<'a> {
    word: &'a str,
    tiles: &'a [Tile],
    typed: Option<&'a Typed>,
    dicts: &'a [(Dict, std::collections::HashSet<String>)],
    runs: Cell<Option<u8>>,
    heterogram: Cell<Option<bool>>,
    alphabetical: Cell<Option<bool>>,
    cie: Cell<Option<bool>>,
    suffix: Cell<Option<Option<&'static str>>>,
}

impl<'a> Facts<'a> {
    pub fn new(
        word: &'a str, tiles: &'a [Tile], typed: Option<&'a Typed>,
        dicts: &'a [(Dict, std::collections::HashSet<String>)],
    ) -> Self {
        Facts {
            word,
            tiles,
            typed,
            dicts,
            runs: Cell::new(None),
            heterogram: Cell::new(None),
            alphabetical: Cell::new(None),
            cie: Cell::new(None),
            suffix: Cell::new(None),
        }
    }

    /// Letters, not tiles, and `len()` rather than a scan.
    ///
    /// `src/search.rs` admits a word only if every character is ASCII alphabetic and stores it
    /// uppercased, so the byte length **is** the letter count. The game counts the same way:
    /// `word:len()` is bytes in Lua.
    fn letters(&self) -> usize {
        self.word.len()
    }

    fn runs(&self) -> u8 {
        if let Some(v) = self.runs.get() {
            return v;
        }
        let v = count_runs(self.word);
        self.runs.set(Some(v));
        v
    }

    fn alphabetical(&self) -> bool {
        if let Some(v) = self.alphabetical.get() {
            return v;
        }
        let v = self.word.as_bytes().windows(2).all(|w| w[0] <= w[1]);
        self.alphabetical.set(Some(v));
        v
    }

    fn cie(&self) -> bool {
        if let Some(v) = self.cie.get() {
            return v;
        }
        let v = has_cie(self.word);
        self.cie.set(Some(v));
        v
    }

    /// `mostCommonCount==1 and not letterHash['.']` (`wordScoreBonusHeterogram.lua`).
    ///
    /// The hash is over the word **as submitted**, and a wildcard contributes its chosen letter in
    /// lower case — `words.wildcardCount` counts `'.'` or any byte above 90 (`utils/words.lua:151`),
    /// which is how the game tells them apart. So a wildcard `a` and a tile `A` are distinct keys
    /// there, and treating both as `A` would wrongly disqualify a heterogram.
    fn heterogram(&self) -> bool {
        if let Some(v) = self.heterogram.get() {
            return v;
        }
        let wild = self.wildcard_positions();
        let mut seen = [0u8; 128];
        let mut most = 0u8;
        for (i, b) in self.word.bytes().enumerate() {
            let key = if wild.contains(&i) { b.to_ascii_lowercase() } else { b };
            let slot = &mut seen[(key & 0x7f) as usize];
            *slot += 1;
            most = most.max(*slot);
        }
        let v = most == 1 && !self.word.contains('.');
        self.heterogram.set(Some(v));
        v
    }

    /// Which letter positions came from wildcard tiles.
    ///
    /// `Typed::wildcards` is parallel to `Typed::tiles`, so a `Some` marks a wildcard. Without a
    /// `Typed` — the offline scoring path — nothing is a wildcard, which matches plain tiles.
    fn wildcard_positions(&self) -> Vec<usize> {
        match self.typed {
            Some(t) => t
                .wildcards
                .iter()
                .enumerate()
                .filter_map(|(i, w)| w.is_some().then_some(i))
                .collect(),
            None => Vec::new(),
        }
    }

    /// `cache.wildcards` — the count `basicCache` puts there (`utils/words.lua:315`).
    ///
    /// **Not** what `wordScoreBonusWildcards.lua`'s own `wordCache` computes. That one calls
    /// `countEachWithLetter(wordTiles, letter)` with `letter` an undefined global, but it is guarded
    /// by `if cache.wildcards then return end` — and Lua treats `0` as true, so the guard always
    /// fires and the broken line never runs. The modifier works, on `basicCache`'s count.
    fn wildcards(&self) -> u8 {
        self.wildcard_positions().len() as u8
    }

    /// The longest registered suffix this word ends with, if any.
    fn suffix(&self) -> Option<&'static str> {
        if let Some(v) = self.suffix.get() {
            return v;
        }
        // `cacheSuffix` requires the suffix to be shorter than the word, so a bare `ED` is not a
        // suffixed word (`utils/effects.lua:107,109`).
        let v = SUFFIXES_LONGEST_FIRST
            .iter()
            .copied()
            .find(|s| self.word.len() > s.len() && self.word.ends_with(s));
        self.suffix.set(Some(v));
        v
    }

    /// A dictionary that was not handed to us is **unknown**, not absent. Answering "no" would
    /// silently under-rate; `Lexica::load` records its own load failures separately.
    fn in_dict(&self, which: Dict) -> Option<bool> {
        self.dicts.iter().find(|(d, _)| *d == which).map(|(_, w)| w.contains(self.word))
    }

    /// What this fact contributes, or that it cannot be judged.
    pub fn evaluate(&self, fact: Fact, fight: &FightFacts) -> Eval {
        let yes = Eval::Apply(1.0);
        match fact {
            Fact::LengthBand456 => Eval::when((4..=6).contains(&self.letters())),
            Fact::Length7 => Eval::when(self.letters() == 7),
            Fact::LengthOdd => Eval::when(self.letters() % 2 == 1),
            Fact::LengthEven => Eval::when(self.letters() % 2 == 0),
            Fact::Cie => Eval::when(self.cie()),
            Fact::Pog => Eval::when(self.word.contains("POG")),
            Fact::Heterogram => Eval::when(self.heterogram()),
            Fact::Alphabetical => Eval::when(self.alphabetical()),
            Fact::Runs => Eval::counted(self.runs() as f64),
            Fact::Wildcards => Eval::counted(self.wildcards() as f64),
            Fact::Suffix(s) => Eval::when(self.suffix() == Some(s)),
            Fact::InDict(l) => match self.in_dict(l) {
                Some(hit) => Eval::when(hit),
                None => Eval::Unknown,
            },
            Fact::Alliteration => match fight.alliteration_for(self.word) {
                Some(n) => Eval::counted(n as f64),
                None => Eval::Unknown,
            },
            Fact::RepeatWord => match fight.repeats_for(self.word) {
                Some(n) if n > 0 => Eval::Apply(n as f64 * (self.letters() as f64 + 1.0)),
                Some(_) => Eval::Skip,
                None => Eval::Unknown,
            },
            Fact::QuillTurn1 => Eval::when(fight.turn == 1),
            Fact::QuillTurnMod(n) => Eval::when(n != 0 && fight.turn % n == 0),
            Fact::Quill => yes,
            // Resolved by `Plan::apply`'s second pass, which knows the tally.
            Fact::QuillCount => Eval::Unknown,
            // TODO(board facts): the last three, stubbed deliberately.
            //
            // Each needs an input this type is not handed yet, and each is small on its own:
            //
            // - `EachHasWood` — `tiles.eachHasCat(wordTiles, 'wood')`. Needs the material→category
            //   table from `utils/tiles.lua`; `Tile::quality.material` already carries the material
            //   name, so it is a lookup away.
            // - `Adjacent` — `tileboard.tilesAreAllAdjacent(wordTiles)`. Needs `Typed::tiles` mapped
            //   through `Geometry::position`, which `Facts` could take but currently does not.
            // - `FullColumn` — `tileboard.countTilesInFullySelectedColumn(wordTiles)`. The same
            //   inputs as `Adjacent`, plus `Geometry::rows_per_col`.
            //
            // Until then they report rather than contribute, which keeps the score an honest lower
            // bound instead of a confident wrong one.
            Fact::EachHasWood | Fact::Adjacent | Fact::FullColumn => {
                let _ = self.tiles;
                Eval::Unknown
            }
        }
    }
}

/// What a condition decided.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Eval {
    /// The condition is false; the modifier contributes nothing.
    Skip,
    /// The condition holds, with this many instances before the flag's own value.
    Apply(f64),
    /// Cannot be judged here — the caller reports the flag rather than assuming zero.
    Unknown,
}

impl Eval {
    fn when(cond: bool) -> Eval {
        if cond {
            Eval::Apply(1.0)
        } else {
            Eval::Skip
        }
    }
    /// A count that is also the condition: zero means the condition is false.
    fn counted(n: f64) -> Eval {
        if n > 0.0 {
            Eval::Apply(n)
        } else {
            Eval::Skip
        }
    }
}

/// `words.countRuns` (`utils/words.lua:192-204`).
///
/// Counts the *starts* of doubled letters: the `curr_char~=prev_char` guard means `AAA` is 1, not 2.
pub fn count_runs(upper: &str) -> u8 {
    let b = upper.as_bytes();
    if b.is_empty() {
        return 0;
    }
    let mut count = 0u8;
    let mut prev: Option<u8> = None;
    let mut curr = b[0];
    for &next in &b[1..] {
        if next == curr && Some(curr) != prev {
            count += 1;
        }
        prev = Some(curr);
        curr = next;
    }
    count
}

/// `word:find'[cC][iI][eE]' or word:find'[^cC.][eE][iI]'`.
pub fn has_cie(upper: &str) -> bool {
    if upper.contains("CIE") {
        return true;
    }
    let b = upper.as_bytes();
    // The `[^cC.]` class must match a real character, so a leading `EI` does not count.
    (1..b.len().saturating_sub(1))
        .any(|i| b[i] == b'E' && b[i + 1] == b'I' && b[i - 1] != b'C' && b[i - 1] != b'.')
}
