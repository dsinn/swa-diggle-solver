//! The board's target letter distribution, and how far a board is from it.
//!
//! Picking the *first* word that kills leaves the board to chance. What is left behind decides every
//! later turn: a board that has drifted to `QXJVZ` has no words in it whatever the dictionary says,
//! and the run discovers that several turns later as `NoPlayableWord`. This is the other half of
//! choosing a word — not what it does to the enemy, but what it leaves us standing on.
//!
//! ## The target is the game's own table
//!
//! `utils/letters.lua:3-30` carries `LetterWeights`, 26 entries summing to ~100 — the frequencies the
//! game itself refills from. Reading them rather than restating them is the same rule the scorer
//! follows: the numbers are data in the game's files and can change under us.
//!
//! Normalised so the weights sum to the **tile count of the board**, they become "how many `E`s
//! should a board this size be carrying". A 16-tile board wants about 1.72 `E`s and 0.03 `Q`s.
//! Fractional targets are the point — rounding them would throw away exactly the signal that
//! distinguishes two candidates.
//!
//! ## Deviation, and why absolute rather than squared
//!
//! [`deviation`] is the L1 distance: the sum over A-Z of `|remaining - target|`. Squared error would
//! punish one badly-wrong letter far more than five slightly-wrong ones, and there is no evidence
//! that is the right trade here. L1 also has a plain reading — roughly "how many tiles are in the
//! wrong place" — which matters for a number that will be read in logs while diagnosing a run.
//!
//! Lower is better. It is a *tiebreak*, never a reason to pass up a kill.

use std::path::Path;

/// A-Z, indexed by `letter as u8 - b'A'`.
pub const ALPHABET: usize = 26;

/// The game's relative letter frequencies, as read from its own table.
#[derive(Debug, Clone, PartialEq)]
pub struct Weights {
    /// Raw weights in dump order, indexed A-Z. Letters absent from the table stay at zero.
    by_letter: [f64; ALPHABET],
}

impl Weights {
    /// Reads `LetterWeights` from `utils/letters.lua`.
    ///
    /// Deliberately not a Lua evaluation. The file is a module that builds functions around the
    /// table, so evaluating it means running it; the table itself is plain `{'E', 10.77},` rows and a
    /// line scan reads them without pulling in an interpreter. The count is asserted, so a format
    /// change fails here instead of silently yielding a lopsided target.
    pub fn load(game_dir: &Path) -> Result<Self, crate::Error> {
        let src = std::fs::read_to_string(game_dir.join("utils/letters.lua"))?;
        Self::parse(&src)
    }

    /// Splitting `load` from the parse is what lets this be tested without the game present.
    pub fn parse(src: &str) -> Result<Self, crate::Error> {
        let mut by_letter = [0.0f64; ALPHABET];
        let mut seen = 0usize;
        let mut inside = false;
        for line in src.lines() {
            let t = line.trim();
            if !inside {
                // `FirstLetterWeights` is a different table for a different purpose — the first
                // letter of a word, not the board's composition — and sits directly below this one.
                // Anchoring on the exact name is what keeps them apart.
                inside = t.starts_with("local LetterWeights");
                continue;
            }
            if t == "}" {
                break;
            }
            let Some((letter, weight)) = read_row(t) else { continue };
            let i = (letter as u8 - b'A') as usize;
            if by_letter[i] != 0.0 {
                return Err(crate::Error::Config(format!("letter {letter} appears twice")));
            }
            by_letter[i] = weight;
            seen += 1;
        }
        if seen != ALPHABET {
            return Err(crate::Error::Config(format!(
                "read {seen} letter weights, expected {ALPHABET}; \
                 utils/letters.lua no longer looks the way this parse expects"
            )));
        }
        Ok(Weights { by_letter })
    }

    /// How many of each letter a board of `tiles` tiles ought to be carrying.
    ///
    /// Scaled so the target sums to `tiles`, which is what makes the deviation comparable to a count
    /// of misplaced tiles rather than to a percentage.
    ///
    /// Cheap enough to call per fight and not per candidate — 26 multiplications — but not per word,
    /// which is why callers hold the result. A board cannot change size mid-fight, so one computation
    /// covers the whole search.
    pub fn target(&self, tiles: usize) -> Target {
        let total: f64 = self.by_letter.iter().sum();
        let mut want = [0.0f64; ALPHABET];
        if total > 0.0 {
            let scale = tiles as f64 / total;
            for (i, w) in self.by_letter.iter().enumerate() {
                want[i] = w * scale;
            }
        }
        Target { want }
    }
}

/// `{'E', 10.7782867889039},` — the letter and its weight, or nothing if this is not such a row.
fn read_row(line: &str) -> Option<(char, f64)> {
    let inner = line.trim().strip_prefix('{')?;
    let (letter_part, rest) = inner.split_once(',')?;
    let letter = letter_part.trim().trim_matches('\'').chars().next()?;
    if !letter.is_ascii_uppercase() {
        return None;
    }
    let weight: f64 = rest.trim().trim_end_matches(',').trim_end_matches('}').trim().parse().ok()?;
    Some((letter, weight))
}

/// The letter counts a board of a given size ought to have.
///
/// `Default` is every target at zero, which is not a board any size wants — it exists so callers
/// that do not rank (a `FirstKill` or `MaxDamage` search) can supply a context without loading the
/// game's tables for one they will never read.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Target {
    want: [f64; ALPHABET],
}

impl Target {
    /// How far `counts` sits from this target: the sum of `|counts - want|` over A-Z.
    ///
    /// Lower is better. Zero is unreachable in practice — the targets are fractional and counts are
    /// whole — so the floor is the rounding residue, not zero, and only differences between
    /// candidates mean anything.
    pub fn deviation(&self, counts: &[usize; ALPHABET]) -> f64 {
        self.want.iter().zip(counts).map(|(w, &c)| (c as f64 - w).abs()).sum()
    }

    /// The target for one letter, for logs and tests.
    pub fn want(&self, letter: char) -> f64 {
        index_of(letter).map(|i| self.want[i]).unwrap_or(0.0)
    }

    pub fn total(&self) -> f64 {
        self.want.iter().sum()
    }
}

/// A-Z to 0-25. Anything else — a wildcard `.`, an unselectable `!`, a digit — has no place in the
/// distribution and is counted by nobody.
pub fn index_of(letter: char) -> Option<usize> {
    let c = letter.to_ascii_uppercase();
    c.is_ascii_uppercase().then(|| (c as u8 - b'A') as usize)
}

/// Letter counts for a whole board.
///
/// Non-letter tiles are skipped rather than bucketed somewhere: a `!` is not a letter the board is
/// short of, and counting it as one would make hazard tiles look like distribution problems.
///
/// ## A ligature counts as every letter it carries
///
/// This read `chars().next()` until 2026-08-17, which is one letter per tile — and a tile's letter
/// is a *string*, because ligatures are real tiles carrying more than one. `slimes.lua:27` builds an
/// ash as `{'AE', {ligature = 'ash'}}`, so the board reports the two characters `AE`; `ui/objects/tile.lua:40-51`
/// lists the rest — `QU`, `TH`, `SS`, `ED`, `ES`, `LY`, `ING`.
///
/// So every ligature was being counted as its **first letter alone**: an ash read as a plain `A`,
/// and the distribution believed the board held a common vowel where it held a tile few words can
/// spend. `QU` read as a `Q` with its `U` invisible, which is the opposite error — the `U` is
/// exactly what makes a `Q` playable.
///
/// The dev's rule, 2026-08-17: *ligatures should increment the counts for each letter that it is
/// made of.* Iterating the characters is also what makes three-letter ligatures need no further
/// thought — `ING` adds one each to `I`, `N` and `G` by the same loop, with nothing to extend when
/// `-est` and `-ing` suffix tiles arrive.
pub fn counts_of<'a>(letters: impl IntoIterator<Item = &'a str>) -> [usize; ALPHABET] {
    let mut counts = [0usize; ALPHABET];
    for l in letters {
        for c in l.chars() {
            if let Some(i) = index_of(c) {
                counts[i] += 1;
            }
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn game_dir() -> PathBuf {
        PathBuf::from("../sternly-worded-adventures")
    }

    fn present() -> bool {
        game_dir().join("utils/letters.lua").is_file()
    }

    /// A trimmed stand-in with the real file's shape, including the second table below it.
    const SRC: &str = "local letters = {}\n\
        \n\
        local LetterWeights = {\n\
        \x20   {'E', 50.0},\n\
        \x20   {'A', 30.0},\n\
        \x20   {'B', 20.0},\n\
        }\n\
        \n\
        local FirstLetterWeights = {\n\
        \x20   {'S', 99.0},\n\
        }\n";

    #[test]
    fn a_short_table_is_refused_rather_than_silently_used() {
        // Three letters is not an alphabet. Accepting it would produce a target that says the board
        // should be half `E`, which would then quietly drive every tiebreak.
        let err = Weights::parse(SRC).unwrap_err();
        assert!(format!("{err:?}").contains("read 3 letter weights"), "got {err:?}");
    }

    #[test]
    fn the_target_sums_to_the_board_size() {
        let Some(w) = present().then(|| Weights::load(&game_dir()).unwrap()) else {
            eprintln!("SKIP: game source not present");
            return;
        };
        for tiles in [1usize, 12, 16, 25] {
            let t = w.target(tiles);
            assert!(
                (t.total() - tiles as f64).abs() < 1e-9,
                "target for {tiles} tiles sums to {}",
                t.total()
            );
        }
    }

    #[test]
    fn the_common_letters_outrank_the_rare_ones() {
        let Some(w) = present().then(|| Weights::load(&game_dir()).unwrap()) else {
            eprintln!("SKIP: game source not present");
            return;
        };
        let t = w.target(16);
        // Straight from `utils/letters.lua`: E is the heaviest at 10.78 and Q the lightest at 0.17.
        assert!(t.want('E') > t.want('A'), "E {} vs A {}", t.want('E'), t.want('A'));
        assert!(t.want('A') > t.want('Q'), "A {} vs Q {}", t.want('A'), t.want('Q'));
        assert!(t.want('Q') > 0.0, "every letter should want something");
    }

    #[test]
    fn a_board_matching_the_target_deviates_less_than_one_that_does_not() {
        let Some(w) = present().then(|| Weights::load(&game_dir()).unwrap()) else {
            eprintln!("SKIP: game source not present");
            return;
        };
        let t = w.target(16);
        // Sixteen `Q`s is the worst board the alphabet allows; a spread of common letters is not.
        let awful = counts_of(std::iter::repeat("Q").take(16));
        let decent = counts_of(
            ["E", "A", "I", "S", "O", "R", "N", "T", "L", "C", "U", "D", "P", "M", "G", "H"],
        );
        assert!(
            t.deviation(&decent) < t.deviation(&awful),
            "decent {} should beat awful {}",
            t.deviation(&decent),
            t.deviation(&awful)
        );
    }

    /// A ligature tile is worth every letter printed on it, not just the first.
    ///
    /// The dev's rule, 2026-08-17. Until then this counted `chars().next()`, so an ash read as a
    /// lone `A` — the distribution believed the board held a common vowel where it held a tile
    /// almost nothing can spend — and a `QU` read as a `Q` with its `U` invisible, which is the
    /// same bug pointing the other way, since the `U` is what makes the `Q` playable.
    ///
    /// `ING` is here as the forward-looking half: three-letter suffix tiles are expected, and the
    /// character loop that fixes the two-letter case has to cover them with nothing added.
    #[test]
    fn a_ligature_counts_as_every_letter_it_carries() {
        let at = |c: char| index_of(c).unwrap();

        let ash = counts_of(["AE"]);
        assert_eq!(ash[at('A')], 1, "the ash carries an A");
        assert_eq!(ash[at('E')], 1, "**and an E, which the first-character read threw away**");
        assert_eq!(ash.iter().sum::<usize>(), 2, "and nothing else");

        let qu = counts_of(["QU"]);
        assert_eq!((qu[at('Q')], qu[at('U')]), (1, 1), "a QU is a Q and the U that makes it usable");

        // Three characters, for the suffix tiles that do not exist here yet.
        let ing = counts_of(["ING"]);
        assert_eq!((ing[at('I')], ing[at('N')], ing[at('G')]), (1, 1, 1));
        assert_eq!(ing.iter().sum::<usize>(), 3);

        // Mixed with plain tiles, which is what a real board looks like.
        let board = counts_of(["E", "AE", "T"]);
        assert_eq!(board[at('E')], 2, "one plain E and the ash's E");
        assert_eq!(board.iter().sum::<usize>(), 4);
    }

    #[test]
    fn non_letters_are_not_counted_as_letters() {
        // `!` is unselectable and `.` is a wildcard. Neither is a letter the board is short of, and
        // bucketing them anywhere would make a hazard tile read as a distribution problem.
        let counts = counts_of(["E", "!", ".", "3", "E"]);
        assert_eq!(counts[index_of('E').unwrap()], 2);
        assert_eq!(counts.iter().sum::<usize>(), 2);
    }

    #[test]
    fn the_real_table_has_every_letter_exactly_once() {
        let Some(w) = present().then(|| Weights::load(&game_dir()).unwrap()) else {
            eprintln!("SKIP: game source not present");
            return;
        };
        // Every letter carries weight, so no letter can be targeted at zero -- which would make a
        // board full of it look perfect.
        let t = w.target(16);
        for c in 'A'..='Z' {
            assert!(t.want(c) > 0.0, "{c} has no weight");
        }
    }
}
