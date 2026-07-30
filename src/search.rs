//! Finding a word that kills the enemy.
//!
//! Per the MVP scope: **a race, not an optimisation.** Threads take contiguous alphabetical slices of
//! the dictionary and the first to find a lethal word wins; the rest stop. Optimal play is not the
//! goal, and for non-boss enemies a killing word is usually easy to find.
//!
//! Contiguous slices are deliberately not a balanced partition — a slice whose initial letters are
//! absent from the board yields nothing at all. That is fine for a race (any thread can win) and it
//! keeps the split trivial, but it is worth knowing that thread load is wildly uneven by design.
//!
//! ## What makes a word playable
//!
//! Just having the letters. Adjacency is **not** required: `cache.isAdjacent`
//! (`wordboard.lua:270`) is consumed only for drawing (`tileboard.lua:2033`), so a word may use tiles
//! anywhere on the board. Wildcard tiles (`"."`, material `wood0`, score 0) substitute for any
//! missing letter.
//!
//! ## Assumption flagged rather than buried
//!
//! [`MIN_WORD_LEN`] is 3. I have not found the game's minimum-length check, so this is a guess chosen
//! to be safe: submitting a rejected word would waste a turn, while skipping valid two-letter words
//! only costs a little search. Worth verifying against a live fight.

use crate::observe::board::Tile;
use crate::score::Scorer;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Shortest word the search will consider. See the module note — an unverified assumption.
pub const MIN_WORD_LEN: usize = 3;

/// The game's wildcard tile, which takes any letter.
const WILDCARD: &str = ".";

pub struct Dictionary {
    /// Uppercased, A-Z only, in file order — which is roughly alphabetical, so a contiguous slice is
    /// a contiguous alphabetical range.
    words: Vec<String>,
}

impl Dictionary {
    /// Reads the game's dictionary.
    ///
    /// `utils/dictionary.lua` is 345k lines of `word="definition",`, with keys occasionally bracketed
    /// (`["'un"]="…"`) when they are not valid Lua identifiers. Scanned line-by-line in Rust rather
    /// than evaluated as Lua: the definitions are megabytes of text we have no use for, and building
    /// a 345k-entry Lua table to throw away the values would be pure waste.
    pub fn load(game_dir: &Path) -> Result<Self, crate::Error> {
        let path = game_dir.join("utils/dictionary.lua");
        let text = std::fs::read_to_string(&path)?;
        let mut words = Vec::with_capacity(350_000);
        for line in text.lines() {
            let line = line.trim();
            let key = if let Some(rest) = line.strip_prefix("[\"") {
                rest.split_once("\"]").map(|(k, _)| k)
            } else {
                line.split_once('=').map(|(k, _)| k.trim())
            };
            let Some(key) = key else { continue };
            // Only pure alphabetic words are playable: the dictionary also holds entries with
            // apostrophes, hyphens and spaces, none of which are on the board.
            if key.len() >= MIN_WORD_LEN && key.chars().all(|c| c.is_ascii_alphabetic()) {
                words.push(key.to_ascii_uppercase());
            }
        }
        words.sort_unstable();
        words.dedup();
        if words.is_empty() {
            return Err(crate::Error::Config(format!(
                "no words parsed from {}",
                path.display()
            )));
        }
        Ok(Dictionary { words })
    }

    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    pub fn words(&self) -> &[String] {
        &self.words
    }
}

/// What the board can supply.
///
/// **Tiles are not letters.** A tile's `letter` may be a 2- or 3-character **ligature**
/// (`tileboard.lua:1512-1515` matches `word:sub(i,i+1)` and `word:sub(i,i+2)`), and a ligature cannot
/// be split — a `TH` tile supplies `T` and `H` only together and only where the word has them
/// adjacent. An earlier version of this took `letter.chars().next()`, which would have counted a `TH`
/// tile as a bare `T` and happily "found" words it could never play.
///
/// So availability is a segmentation problem, not a multiset comparison: the word must be cuttable
/// into available tile strings, each tile used at most once.
#[derive(Debug, Clone, Default)]
pub struct Supply {
    /// Uppercased tile strings, one per selectable tile. Multi-character entries are ligatures.
    tiles: Vec<String>,
    wildcards: usize,
}

impl Supply {
    /// Builds the supply from the board, excluding unselectable tiles.
    pub fn from_tiles(tiles: &[Tile]) -> Self {
        let mut s = Supply::default();
        for t in tiles.iter().filter(|t| t.selectable()) {
            if t.letter == WILDCARD {
                s.wildcards += 1;
            } else {
                s.tiles.push(t.letter.to_ascii_uppercase());
            }
        }
        s
    }

    /// Longest tile string in the supply, which bounds the segmentation search.
    fn max_tile_len(&self) -> usize {
        self.tiles.iter().map(|t| t.len()).max().unwrap_or(1)
    }

    /// Can this word be formed? Returns the tile indices consumed, in word order.
    ///
    /// Tries the **longest** match at each position first, mirroring the game's greedy absorption of
    /// ligature tiles (`getUnselectedLegalTileWithLetter`, `tileboard.lua:2266`). That ordering matters
    /// beyond mere availability: which tiles get consumed determines the score once tile state such as
    /// burn is involved.
    pub fn segment(&self, word: &str) -> Option<Vec<usize>> {
        let w = word.to_ascii_uppercase();
        let bytes = w.as_bytes();
        let mut used = vec![false; self.tiles.len()];
        let mut wild_left = self.wildcards;
        let mut chosen = Vec::with_capacity(w.len());
        if self.walk(bytes, 0, &mut used, &mut wild_left, &mut chosen) {
            Some(chosen)
        } else {
            None
        }
    }

    fn walk(
        &self,
        word: &[u8],
        at: usize,
        used: &mut Vec<bool>,
        wild_left: &mut usize,
        chosen: &mut Vec<usize>,
    ) -> bool {
        if at == word.len() {
            return true;
        }
        let remaining = word.len() - at;
        // Longest first, so ligatures are preferred exactly as the game prefers them.
        let longest = self.max_tile_len().min(remaining);
        for len in (1..=longest).rev() {
            let piece = &word[at..at + len];
            for i in 0..self.tiles.len() {
                if used[i] || self.tiles[i].len() != len || self.tiles[i].as_bytes() != piece {
                    continue;
                }
                used[i] = true;
                chosen.push(i);
                if self.walk(word, at + len, used, wild_left, chosen) {
                    return true;
                }
                chosen.pop();
                used[i] = false;
            }
        }
        // A wildcard covers exactly one character.
        if *wild_left > 0 {
            *wild_left -= 1;
            // usize::MAX marks a wildcard rather than a real tile index.
            chosen.push(usize::MAX);
            if self.walk(word, at + 1, used, wild_left, chosen) {
                return true;
            }
            chosen.pop();
            *wild_left += 1;
        }
        false
    }

    /// Convenience predicate.
    pub fn can_make(&self, word: &str) -> bool {
        self.segment(word).is_some()
    }
}

/// A word the search found worth reporting.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub word: String,
    pub score: i64,
    /// Which dictionary slice found it — useful for seeing whether the split is pulling its weight.
    pub slice: usize,
}

/// The result of a search.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    /// The first lethal word any thread found. Not the best — deliberately.
    pub lethal: Option<Found>,
    /// Highest-scoring word seen, for when nothing is lethal.
    pub best: Option<Found>,
    pub words_considered: usize,
}

impl Outcome {
    /// Should the board be refreshed rather than played?
    ///
    /// The user's MVP rule: refresh when the longest word found is shorter than half the board. Cheap
    /// to evaluate because the `--verbose` board dump is truncated to `totalTileCount`, so the dump's
    /// own length is the threshold ([`crate::observe::board::BoardDump::refresh_threshold`]).
    ///
    /// A lethal word always wins over refreshing: killing the enemy ends the exchange, which beats any
    /// board-quality consideration.
    pub fn should_refresh(&self, threshold: usize) -> bool {
        if self.lethal.is_some() {
            return false;
        }
        match &self.best {
            Some(b) => b.word.chars().count() < threshold,
            // Nothing playable at all is the strongest case for a refresh.
            None => true,
        }
    }

    /// What to actually play: the lethal word if there is one, else the best found.
    pub fn choice(&self) -> Option<&Found> {
        self.lethal.as_ref().or(self.best.as_ref())
    }
}

/// Races `threads` slices of the dictionary for a word that kills.
///
/// `need` is the damage required — enemy health plus armour, with an absent armour key treated as
/// zero by the caller (the captured save omits it entirely).
///
/// Stops every thread as soon as one finds a lethal word, so the cost is usually a small fraction of
/// the dictionary. The best-scoring word is still tracked, because the fallback needs it and the scan
/// that produced it was already paid for.
pub fn race_for_kill(
    dict: &Dictionary,
    scorer: &Scorer,
    tiles: &[Tile],
    need: i64,
    threads: usize,
) -> Outcome {
    let supply = Supply::from_tiles(tiles);
    let words = dict.words();
    let threads = threads.max(1).min(words.len().max(1));
    let chunk = words.len().div_ceil(threads);

    let stop = AtomicBool::new(false);
    let lethal: Mutex<Option<Found>> = Mutex::new(None);
    let best: Mutex<Option<Found>> = Mutex::new(None);
    let considered = std::sync::atomic::AtomicUsize::new(0);

    std::thread::scope(|scope| {
        for (slice, part) in words.chunks(chunk).enumerate() {
            let (stop, lethal, best, considered, supply) =
                (&stop, &lethal, &best, &considered, &supply);
            scope.spawn(move || {
                let mut seen = 0usize;
                let mut local_best: Option<Found> = None;
                for word in part {
                    // Checked periodically rather than every word: an atomic load per word would
                    // dominate the loop for no benefit at this granularity.
                    if seen % 512 == 0 && stop.load(Ordering::Relaxed) {
                        break;
                    }
                    seen += 1;
                    if !supply.can_make(word) {
                        continue;
                    }
                    let score = scorer.score_word(word);
                    if score >= need {
                        let found = Found { word: word.clone(), score, slice };
                        let mut l = lethal.lock().unwrap();
                        // First writer wins, so the result does not depend on thread scheduling
                        // any more than it has to.
                        if l.is_none() {
                            *l = Some(found);
                        }
                        stop.store(true, Ordering::Relaxed);
                        break;
                    }
                    if local_best.as_ref().map(|b| score > b.score).unwrap_or(true) {
                        local_best = Some(Found { word: word.clone(), score, slice });
                    }
                }
                considered.fetch_add(seen, Ordering::Relaxed);
                if let Some(lb) = local_best {
                    let mut b = best.lock().unwrap();
                    if b.as_ref().map(|cur| lb.score > cur.score).unwrap_or(true) {
                        *b = Some(lb);
                    }
                }
            });
        }
    });

    Outcome {
        lethal: lethal.into_inner().unwrap(),
        best: best.into_inner().unwrap(),
        words_considered: considered.into_inner(),
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
        game_dir().join("utils/dictionary.lua").is_file()
    }

    fn plain(letters: &str) -> Vec<Tile> {
        letters.chars().map(|c| Tile { letter: c.to_string(), extra: None }).collect()
    }

    /// The actual level-0 crypt board, from `tests/fixtures/combatSaveData-crypt-l0.lua`.
    fn crypt_board() -> Vec<Tile> {
        plain("OYCAACTPORLIGAHJ")
    }

    #[test]
    fn supply_respects_letter_counts() {
        let s = Supply::from_tiles(&plain("AB"));
        assert!(s.can_make("AB"));
        assert!(!s.can_make("ABB"), "only one B is on the board");
        assert!(!s.can_make("ABC"));
    }

    #[test]
    fn unselectable_tiles_are_not_available() {
        // An unselectable tile cannot form part of a word, so counting it would produce words that
        // cannot actually be played.
        let mut tiles = plain("AB");
        let mut extra = crate::game::save::Table::default();
        extra.map.insert("unselectable".into(), crate::game::save::Value::Int(2));
        tiles[1].extra = Some(extra);
        let s = Supply::from_tiles(&tiles);
        assert!(s.can_make("AAA") == false);
        assert!(!s.can_make("AB"), "B is unselectable");
    }

    #[test]
    fn refresh_only_when_nothing_good_and_nothing_lethal() {
        let lethal = Outcome {
            lethal: Some(Found { word: "CAT".into(), score: 9, slice: 0 }),
            best: Some(Found { word: "CAT".into(), score: 9, slice: 0 }),
            words_considered: 1,
        };
        // A kill ends the exchange, which beats any board-quality judgement.
        assert!(!lethal.should_refresh(8), "never refresh when a lethal word exists");

        let weak = Outcome {
            lethal: None,
            best: Some(Found { word: "CAT".into(), score: 4, slice: 0 }),
            words_considered: 1,
        };
        assert!(weak.should_refresh(8), "3 letters is under the 8 threshold");
        assert!(!weak.should_refresh(3), "3 letters meets a threshold of 3");

        let nothing = Outcome::default();
        assert!(nothing.should_refresh(8), "no playable word at all must refresh");
    }

    #[test]
    fn a_ligature_tile_cannot_be_split() {
        // The user's catch: a tile may hold 2-3 letters that cannot be separated. Counting a "TH"
        // tile as a bare "T" would find words the board cannot play.
        let tiles = vec![
            Tile { letter: "TH".into(), extra: None },
            Tile { letter: "E".into(), extra: None },
            Tile { letter: "N".into(), extra: None },
        ];
        let s = Supply::from_tiles(&tiles);
        assert!(s.can_make("THEN"), "TH + E + N");
        assert!(!s.can_make("TEN"), "the T is locked inside the TH ligature");
        assert!(!s.can_make("NET"), "same, and the H has nowhere to go");
        assert!(!s.can_make("HEN"), "H is not separately available either");
    }

    #[test]
    fn a_ligature_is_preferred_over_single_tiles() {
        // Longest-match-first mirrors the game's greedy absorption
        // (`getUnselectedLegalTileWithLetter`). Which tiles are consumed decides the score once tile
        // state such as burn is in play, so the ORDER is not merely cosmetic.
        let tiles = vec![
            Tile { letter: "TH".into(), extra: None },
            Tile { letter: "T".into(), extra: None },
            Tile { letter: "H".into(), extra: None },
            Tile { letter: "E".into(), extra: None },
        ];
        let s = Supply::from_tiles(&tiles);
        let seg = s.segment("THE").expect("THE is makeable");
        assert_eq!(seg, vec![0, 3], "took the TH ligature, not T + H");
    }

    #[test]
    fn a_three_letter_ligature_works() {
        let tiles = vec![
            Tile { letter: "ING".into(), extra: None },
            Tile { letter: "S".into(), extra: None },
            Tile { letter: "K".into(), extra: None },
        ];
        let s = Supply::from_tiles(&tiles);
        // The ligature can sit anywhere in the word, as long as its letters are contiguous.
        assert!(s.can_make("KINGS"), "K + ING + S");
        assert!(s.can_make("SKING"), "S + K + ING");
        // But its letters are never available individually.
        assert!(!s.can_make("SINK"), "I and N exist only inside ING");
        assert!(!s.can_make("GIN"), "GIN would need ING split and reordered");
    }

    #[test]
    fn backtracking_finds_a_valid_cut_when_the_greedy_one_fails() {
        // Greedy alone is not enough: taking "AB" first leaves nothing for the second A, so the
        // search must back off to the single tiles.
        let tiles = vec![
            Tile { letter: "AB".into(), extra: None },
            Tile { letter: "A".into(), extra: None },
            Tile { letter: "B".into(), extra: None },
            Tile { letter: "A".into(), extra: None },
        ];
        let s = Supply::from_tiles(&tiles);
        assert!(s.can_make("ABA"), "AB + A, or A + B + A");
        // Greedy would take AB then AB again; only one AB tile exists, so it must fall back.
        assert!(s.can_make("ABAB"), "AB + A + B");
    }

    #[test]
    fn wildcards_still_cover_a_single_missing_letter() {
        let s = Supply::from_tiles(&plain("AB."));
        assert!(s.can_make("ABC"), "the wildcard supplies C");
        assert!(!s.can_make("ABCD"), "one wildcard cannot cover two missing letters");
    }

    #[test]
    fn finds_a_lethal_word_on_the_real_crypt_board() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        let dict = Dictionary::load(&game_dir()).unwrap();
        let scorer = Scorer::new(&game_dir()).unwrap();
        // The real fight: "Amorphous", 3 health, armour absent.
        let out = race_for_kill(&dict, &scorer, &crypt_board(), 3, 8);
        assert!(!out.should_refresh(8), "a lethal word means no refresh");
        let found = out.lethal.clone().expect("a 3-health enemy must be killable from this board");
        assert!(found.score >= 3);
        let supply = Supply::from_tiles(&crypt_board());
        assert!(supply.can_make(&found.word), "{} is not makeable", found.word);
    }

    #[test]
    fn a_race_stops_early_rather_than_scanning_everything() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        let dict = Dictionary::load(&game_dir()).unwrap();
        let scorer = Scorer::new(&game_dir()).unwrap();
        let out = race_for_kill(&dict, &scorer, &crypt_board(), 3, 8);
        // The whole point of racing: a trivially killable enemy must not cost a full dictionary scan.
        assert!(
            out.words_considered < dict.len(),
            "considered {} of {} words -- the race did not stop early",
            out.words_considered,
            dict.len()
        );
    }

    #[test]
    fn an_unkillable_enemy_yields_a_best_word_and_no_lethal() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        let dict = Dictionary::load(&game_dir()).unwrap();
        let scorer = Scorer::new(&game_dir()).unwrap();
        // Absurd health, so the search must exhaust and fall back rather than reporting nothing.
        let out = race_for_kill(&dict, &scorer, &crypt_board(), 100_000, 8);
        assert!(out.lethal.is_none());
        let best = out.best.expect("must still report the best word it found");
        assert!(best.score > 0);
        assert_eq!(out.words_considered, dict.len(), "no lethal word means a full scan");
    }

    #[test]
    fn the_dictionary_looks_like_a_dictionary() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        let dict = Dictionary::load(&game_dir()).unwrap();
        assert!(dict.len() > 100_000, "expected a large word list, got {}", dict.len());
        assert!(dict.words().iter().all(|w| w.chars().all(|c| c.is_ascii_uppercase())));
        assert!(dict.words().iter().all(|w| w.len() >= MIN_WORD_LEN));
        // Sorted, which is what makes a contiguous slice an alphabetical range.
        assert!(dict.words().windows(2).all(|w| w[0] <= w[1]));
    }
}
