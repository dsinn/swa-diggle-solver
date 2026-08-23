//! Shrine solving: the game's Wordle.
//!
//! A shrine picks a hidden word and gives `maxGuesses` attempts, colouring each guess green/yellow/
//! grey. Diggle has to find it without looking — the answer *is* written to the save
//! (`setAreaFlag(dataKey..'word', word)`, `shrineview.lua:266`) and is derivable from the world seed
//! via `words.getRandomShrine`, and **both are off limits**. What is fair game is the word lists
//! themselves, which are static game data no different from a board shape.
//!
//! ## The three sets
//!
//! Answers come from `shrineDicts[length]` — one lexicon per length, 4 through 7
//! (`utils/words.lua:21-26`). `shrineDifficulty` bands them by an ngram commonness score with strict
//! inequalities, `easy = {1, huge}` and `hard = {0, 1}`, so the bands partition each file and
//! [`Band::Wild`] is both together.
//!
//! Guesses are validated against the **whole** dictionary (`shrineview.lua:261`), not the shrine
//! list. Probe words that cannot be the answer are therefore legal, which is the tool that breaks
//! endgame clusters like `?ills`.
//!
//! Sizes as of game v52.4, for a sense of scale only — nothing asserts them, because the game's
//! lexica and dictionary move between versions. `gen_shrine_data` prints the current figures.
//!
//! | L | guesses | easy | hard | wild | legal guesses |
//! |---|---|---|---|---|---|
//! | 4 | 8 | 1,323 | 2,430 | 3,753 | 6,102 |
//! | 5 | 6 | 1,677 | 5,514 | 7,191 | 13,865 |
//! | 6 | 6 | 2,385 | 9,736 | 12,121 | 25,373 |
//! | 7 | 6 | 2,614 | 13,660 | 16,274 | 38,111 |
//!
//! The budget is comfortable: `maxGuesses = clamp(ceil(30/L), 6, 10)` (`shrineview.lua:39`). Longer
//! words are *easier* despite the larger list, because 3^7 = 2,187 feedback patterns collapse a
//! candidate set far faster than 3^5 = 243.
//!
//! ## Policy
//!
//! Greedy expected-remaining-size over a prefiltered guess pool. For each candidate guess, partition
//! the live candidates by the feedback that guess would produce and score `Σ n_p² / |S|`, **omitting
//! the all-green bucket** — that omission is what gives a guess which could itself be the answer its
//! `1/|S|` chance of ending the game now, and leaving it in quietly turns the solver into a pure
//! information maximiser that never tries to win.
//!
//! Bucket *count* is deliberately only a tie-break. It is the best-performing greedy heuristic in the
//! literature, but it saturates at 3^L: with 16,274 candidates against 2,187 patterns every sane
//! guess fills every bucket and the score goes constant. That is precisely the opening position here.
//!
//! Measured over every word in every band by `shrine_selfplay` — results and the two fixes that got
//! it there are in that binary's header. Zero puzzles exceed the game's budget; slowest turn 30 ms.
//!
//! ## Hard mode is gear, and weaker than Wordle's
//!
//! `shrineSubmitHard` (`shrineview.lua:236-255`) is a **gear flag**, not a property of the shrine —
//! it only applies when the player is carrying the item. Two rules, both checked against the
//! *immediately previous* guess (`submitions[#submitions-1]`) rather than the whole history:
//! revealed letter counts must be retained (`min(answerCount, prevCount) <= thisCount`), and greens
//! must stay in place.
//!
//! A violation is **rejected, not penalised**: `bad` skips the entire submit branch, so no guess is
//! consumed and the shrine merely shakes and taunts (`shrineview.lua:277-281`). It is therefore a
//! filter on the guess pool whose worst case is one wasted round trip.
//!
//! **Not implemented.** Because it looks back only one guess, it is strictly weaker than the Wordle
//! rule, so anything legal under Wordle hard mode is legal here — the loophole is real but cheaper
//! to ignore than to model. If it is ever implemented, it belongs in [`Solver::pool`] as a filter,
//! not as a different policy.
//!
//! ## Gear that reveals letters
//!
//! Also not modelled, and also information we are simply leaving on the table:
//!
//! - `shrine<X>Known` (`shrineview.lua:93-101`) pre-reveals whether letter X is in the answer. With
//!   `i` nil, `updateButtonsForletter` resolves to yellow-if-present, else grey.
//! - `shrineLeastCommonWrongConsonantKnown` (`102-114`) reveals N *absent* consonants, walking the
//!   fixed list `qjxzvwkfbhgmpdcltnrs`.
//!
//! Both are real constraints on the candidate set, readable from gear rather than off the screen.
//! Ignoring them costs guesses, never correctness.
//!
//! ## Staleness is the failure mode that matters
//!
//! The lists are baked, and baked lists rot when the game's lexica change. A stale *answer* list
//! fails silently and totally: the true word gets eliminated and the shrine becomes unwinnable with
//! no error anywhere. That is why lists arrive through [`WordSource`] rather than being read from
//! the constants directly. Deferred past the MVP, in increasing order of effort:
//!
//! 1. record a checksum of each source lexicon beside the baked data and verify it at startup when
//!    the game source is reachable, so staleness is *loud*;
//! 2. regenerate through `gen_shrine_data`, making a game update a one-command refresh;
//! 3. read `utils/lexica/shrine*.lua` and `utils/dictionary.lua` directly as a fallback when the
//!    baked data is absent or fails its checksum.
//!
//! ## Getting to a shrine, and finishing it
//!
//! Solving the word is the middle of a five-step sequence, not the whole of it. All of it is
//! button-driven and none of it is announced, so each step is confirmed by what appears next.
//!
//! 1. **Clear its combat.** A shrine node carries two area buttons in the same slot,
//!    `Combat` while `currentAreaIsNotcomplete` and `Visit` once complete
//!    (`overworld/locations/shrine.lua:61-73`). There are two of them, so `insertAreaButtons` gives
//!    neither `affirmative` — they must be clicked, at the usual (0, 0.85)+0.75 slot.
//! 2. **Visit**, which opens the shrine screen.
//! 3. **Solve**, which is what this module is for. `shrineView.hasWon()` gates everything after.
//! 4. **Consecrate** — `button('Consecrate', 1.0, 0.9, {xOffset = -0.75})` (`shrine.lua:241`),
//!    calling `overworldview.consecrate`, which sets `<key>_consecrated` and bumps `shrineKarma`
//!    (`overworldview.lua:321-327`). That flag is how [`crate::overworld`] knows the shrine is done.
//! 5. **Pray** — `button('Pray', 1.0, 0.9, {xOffset = -0.75})` (`:296`), the **same slot**, which is
//!    why it only becomes reachable once Consecrate has gone away. `showPrayButton` additionally
//!    wants `areaUnused(key)`, so praying is lost if the area has been used first.
//!
//! Two conditions on step 4 shape the whole run, and neither is guessable from the map:
//!
//! - **`hell ~= 0`.** `showConsecrateButton` (`:93-96`) reduces to
//!   `(hell ~= 0 and not consecrated) or areaHasBeenUsed(key)`, so consecrating is essentially
//!   impossible until the anomaly has been opened. Doing shrines "first" is not a strategy, it is a
//!   dead end — which is why [`crate::overworld::Goal::Shrine`] ranks below opening it.
//! - **`majorShrine`.** Consecrate never appears at a minor shrine at all. Those offer only `Pray`,
//!   which `showPrayButton` allows for a non-major shrine whatever the `hell` value. Nothing in
//!   `AreaHeading` distinguishes the two, so the live sequence has to try Consecrate and fall back
//!   to Pray rather than decide in advance.
//!
//! Both right-hand buttons resolve to roughly (1732, 972) at 1920x1080 — adjacent to the combat
//! `Finish` slot, and worth aiming carefully.
//!
//! ## Still to do, live
//!
//! None of the above has been run, and the solver has never seen a real shrine screen. Reading the
//! grid needs the word length (the column count) and the colours; the difficulty band may not be
//! readable without the seed, in which case [`Band::Wild`] is the safe default and still clears the
//! budget at every length.

use std::collections::HashMap;

/// Longest shrine word. `getLength` (`shrineview.lua:17-20`) clamps to 4..7.
pub const MAX_LEN: usize = 7;
/// Shortest shrine word. `shrineDicts` has no `[3]`, despite `maxGuesses` allowing for one.
pub const MIN_LEN: usize = 4;

/// How many guesses a shrine allows, per `shrineview.lua:39`.
///
/// `hasLost` is `#submitions > maxGuesses` and `submitions` carries a trailing in-progress `''`, so
/// all `maxGuesses` attempts are real — the last one still counts.
pub fn max_guesses(length: usize) -> usize {
    (30usize.div_ceil(length)).clamp(6, 10)
}

/// Which commonness band the shrine draws its answer from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    /// `ngram > 1` — common words.
    Easy,
    /// `0 < ngram < 1` — rare words.
    Hard,
    /// The whole per-length lexicon. Also what an unrecognised difficulty falls back to, since
    /// `shrineDifficulty[difficulty]` is then nil and `getRandomShrine` skips the filter entirely.
    Wild,
}

/// Where word lists come from.
///
/// The MVP has exactly one implementation, [`Baked`], and that is the point of the trait rather than
/// an argument against it: baked tables go stale when the game's lexica change, and a stale *answer*
/// list fails silently and totally — the true word gets eliminated and the shrine becomes
/// unwinnable. Taking lists through a provider is what lets a checksum-verified or
/// read-from-source implementation drop in later without touching the solver. See the contingency
/// tiers in the ledger.
pub trait WordSource {
    fn answers(&self, length: usize, band: Band) -> Result<WordList, crate::Error>;
    fn guesses(&self, length: usize) -> Result<WordList, crate::Error>;
}

/// Word lists embedded at build time, generated by `gen_shrine_data`.
///
/// Sanitised to keys only — the game's dictionary ships 30 MB of Wiktionary definitions we have no
/// use for, exactly as [`crate::search::Dictionary`] strips them for combat.
pub struct Baked;

macro_rules! banded {
    ($len:expr, $band:expr) => {
        match ($len, $band) {
            (4, Band::Easy) => include_str!("../data/shrine-answers-4-easy.txt"),
            (4, Band::Hard) => include_str!("../data/shrine-answers-4-hard.txt"),
            (5, Band::Easy) => include_str!("../data/shrine-answers-5-easy.txt"),
            (5, Band::Hard) => include_str!("../data/shrine-answers-5-hard.txt"),
            (6, Band::Easy) => include_str!("../data/shrine-answers-6-easy.txt"),
            (6, Band::Hard) => include_str!("../data/shrine-answers-6-hard.txt"),
            (7, Band::Easy) => include_str!("../data/shrine-answers-7-easy.txt"),
            (7, Band::Hard) => include_str!("../data/shrine-answers-7-hard.txt"),
            _ => "",
        }
    };
}

impl WordSource for Baked {
    fn answers(&self, length: usize, band: Band) -> Result<WordList, crate::Error> {
        let text: Vec<&str> = match band {
            Band::Easy | Band::Hard => vec![banded!(length, band)],
            Band::Wild => vec![banded!(length, Band::Easy), banded!(length, Band::Hard)],
        };
        if text.iter().any(|t| t.is_empty()) {
            return Err(crate::Error::Config(format!("no shrine answers for length {length}")));
        }
        let mut list = WordList::new(length);
        for chunk in text {
            list.extend(chunk, length)?;
        }
        list.sort();
        Ok(list)
    }

    fn guesses(&self, length: usize) -> Result<WordList, crate::Error> {
        let text = match length {
            4 => include_str!("../data/shrine-guesses-4.txt"),
            5 => include_str!("../data/shrine-guesses-5.txt"),
            6 => include_str!("../data/shrine-guesses-6.txt"),
            7 => include_str!("../data/shrine-guesses-7.txt"),
            _ => {
                return Err(crate::Error::Config(format!("no shrine guesses for length {length}")))
            }
        };
        let mut list = WordList::new(length);
        list.extend(text, length)?;
        Ok(list)
    }
}

/// Fixed-width lowercase words, packed end to end.
///
/// One allocation and no indirection, so bucketing a whole candidate set stays cache-friendly.
#[derive(Clone)]
pub struct WordList {
    length: usize,
    data: Vec<u8>,
}

impl WordList {
    pub fn new(length: usize) -> Self {
        WordList { length, data: Vec::new() }
    }

    fn extend(&mut self, text: &str, length: usize) -> Result<(), crate::Error> {
        for line in text.lines() {
            let w = line.trim();
            if w.is_empty() {
                continue;
            }
            if w.len() != length || !w.bytes().all(|b| b.is_ascii_lowercase()) {
                return Err(crate::Error::Config(format!(
                    "bad word {w:?} in a length-{length} list"
                )));
            }
            self.data.extend_from_slice(w.as_bytes());
        }
        Ok(())
    }

    fn sort(&mut self) {
        let mut words: Vec<&[u8]> = (0..self.len()).map(|i| self.get(i)).collect();
        words.sort_unstable();
        let sorted: Vec<u8> = words.concat();
        self.data = sorted;
    }

    pub fn length(&self) -> usize {
        self.length
    }

    pub fn len(&self) -> usize {
        self.data.len() / self.length
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn get(&self, i: usize) -> &[u8] {
        &self.data[i * self.length..(i + 1) * self.length]
    }

    pub fn word(&self, i: usize) -> String {
        String::from_utf8_lossy(self.get(i)).into_owned()
    }

    pub fn position(&self, word: &str) -> Option<usize> {
        let w = word.as_bytes();
        (0..self.len()).find(|&i| self.get(i) == w)
    }

    pub fn iter(&self) -> impl Iterator<Item = &[u8]> {
        (0..self.len()).map(move |i| self.get(i))
    }
}

/// The colouring a guess earns against an answer, packed base-3, least significant digit first.
///
/// 0 grey, 1 yellow, 2 green. Fits a `u16` for every shrine length (3^7 = 2,187).
pub type Pattern = u16;

/// Every position green — the terminal pattern.
pub fn solved(length: usize) -> Pattern {
    3u16.pow(length as u32) - 1
}

/// The game's colouring rule, from `shrineview.lua:151` and `164-175`.
///
/// Greens are counted across the whole guess first (`hashWord`, lines 133-143); the draw loop then
/// walks left to right awarding yellow only while `thisHash[letter] < wordHash[letter]`. That is the
/// standard two-pass rule — greens claim their letters, leftover yellows are handed out left to
/// right and capped at the answer's count of that letter — but it is the single most common place a
/// Wordle solver goes quietly wrong, so it is pinned by tests rather than assumed.
pub fn feedback(guess: &[u8], answer: &[u8]) -> Pattern {
    debug_assert_eq!(guess.len(), answer.len());
    let mut spare = [0u8; 26];
    let mut marks = [0u8; MAX_LEN];
    for (i, (&g, &a)) in guess.iter().zip(answer.iter()).enumerate() {
        if g == a {
            marks[i] = 2;
        } else {
            spare[(a - b'a') as usize] += 1;
        }
    }
    for (i, &g) in guess.iter().enumerate() {
        if marks[i] == 2 {
            continue;
        }
        let l = (g - b'a') as usize;
        if spare[l] > 0 {
            spare[l] -= 1;
            marks[i] = 1;
        }
    }
    let mut code: Pattern = 0;
    for i in (0..guess.len()).rev() {
        code = code * 3 + marks[i] as Pattern;
    }
    code
}

/// Renders a pattern as `GY.` for logging and tests.
pub fn show(pattern: Pattern, length: usize) -> String {
    let mut p = pattern;
    let mut out = String::with_capacity(length);
    for _ in 0..length {
        out.push(match p % 3 {
            2 => 'G',
            1 => 'Y',
            _ => '.',
        });
        p /= 3;
    }
    out
}

/// Parses `GY.` back into a pattern. For tests and for reading a colouring off the screen.
pub fn parse_pattern(s: &str) -> Option<Pattern> {
    let mut code: Pattern = 0;
    for c in s.chars().rev() {
        let d = match c.to_ascii_uppercase() {
            'G' => 2,
            'Y' => 1,
            '.' | '_' | 'X' => 0,
            _ => return None,
        };
        code = code * 3 + d;
    }
    Some(code)
}

/// How many live candidates to score against before sampling kicks in.
///
/// Bucket statistics are being *estimated*; past a few thousand candidates the sampling error is far
/// below the difference between one reasonable heuristic and another, while the cost keeps growing.
/// The full candidate set is still filtered exactly — only the scoring is sampled, so this can cost
/// guess quality but never correctness.
const SCORE_SAMPLE: usize = 4096;
/// How many non-candidate probes survive the cheap prefilter.
const PROBE_POOL: usize = 2048;
/// Below this many live candidates, an exact search replaces the greedy score.
///
/// One-ply greedy is myopic by construction: it maximises how much the *next* colouring narrows the
/// set, with no notion that a bucket of eight `_aded` words is a far worse place to stand than a
/// bucket of eight unrelated ones. That is where the whole greedy-versus-optimal gap lives, and with
/// a handful of candidates the full recursion is affordable.
const ENDGAME_MAX: usize = 150;
/// Deepest exact search, in guesses.
///
/// Cost explodes with depth, not with candidate count: searching four guesses ahead from 40
/// candidates took **35 seconds** on one puzzle, while the same search three ahead is milliseconds.
/// Measured on `faded`, the one word in 71,268 that greedy could not finish — and the depth-4 search
/// there was pure waste, since the line that actually rescued it was a better *third*-from-last
/// guess, well inside this cap.
const ENDGAME_DEPTH: usize = 3;
/// How many discriminating probes the exact search branches on at each node.
const ENDGAME_BRANCH: usize = 64;
/// Nodes the exact search may expand before giving up and letting greedy answer.
///
/// A guard, not a tuning knob. The recursion is bounded in principle by the depth limit, but a wide
/// shallow set could still cost more than a turn is worth; running out means we fall back, never
/// that we stall.
const ENDGAME_NODES: usize = 2_000_000;
/// Below this many live candidates, every legal word is scored and the prefilter is skipped.
///
/// The prefilter ranks probes by how much of the live set their letters cover, which is a good proxy
/// **until the live set is a one-letter cluster**. Against `bowed/cowed/…/wowed` every candidate
/// shares `owed`, so probes are ranked by containing `o`, `w`, `e`, `d` — precisely the letters that
/// cannot separate them — and the words that discriminate the first letter are ranked last. Self-play
/// found exactly this: `coved`, `wowed` and `zagging` were the only three puzzles in 71,268 that ran
/// past the budget. A small candidate set makes the full pool affordable, so the bias is not worth
/// being clever about.
const EXACT_POOL_MAX: usize = 64;

/// The first guess, precomputed per `(length, band)` by `shrine_selfplay`.
///
/// Turn 1 is the only turn where the candidate set is the entire list, and scoring it live costs
/// seconds — for an answer that never changes, since the opener depends only on the word list. The
/// literature's finding is that *which* good opener you pick is worth about a thousandth of a guess,
/// while picking a bad one measurably worsens the tail; these are the ones the greedy score itself
/// chose, so they cost nothing in play.
///
/// Regenerate with `cargo run --release --bin shrine_selfplay` when the lexica change.
const OPENERS: &[(usize, Band, &str)] = &[
    (4, Band::Easy, "lare"),
    (4, Band::Hard, "toea"),
    (4, Band::Wild, "lare"),
    (5, Band::Easy, "raise"),
    (5, Band::Hard, "raise"),
    (5, Band::Wild, "raise"),
    (6, Band::Easy, "tanier"),
    (6, Band::Hard, "lanier"),
    (6, Band::Wild, "tanier"),
    (7, Band::Easy, "saltier"),
    (7, Band::Hard, "saltier"),
    (7, Band::Wild, "saltier"),
];

/// A shrine in progress.
pub struct Solver {
    length: usize,
    band: Band,
    answers: WordList,
    guesses: WordList,
    /// Indices into `answers` still consistent with every colouring seen.
    live: Vec<u32>,
    history: Vec<(String, Pattern)>,
}

impl Solver {
    pub fn new(source: &dyn WordSource, length: usize, band: Band) -> Result<Self, crate::Error> {
        let answers = source.answers(length, band)?;
        let guesses = source.guesses(length)?;
        let live: Vec<u32> = (0..answers.len() as u32).collect();
        Ok(Solver { length, band, answers, guesses, live, history: Vec::new() })
    }

    /// Derives the opening guess from the word list instead of reading [`OPENERS`].
    ///
    /// This is what regenerates the baked table, so it must not consult it — otherwise the constant
    /// would simply confirm itself and drift would be invisible.
    pub fn compute_opener(&self) -> Option<String> {
        if self.live.is_empty() {
            return None;
        }
        Some(self.best_guess())
    }

    pub fn length(&self) -> usize {
        self.length
    }

    /// How many words could still be the answer.
    pub fn remaining(&self) -> usize {
        self.live.len()
    }

    /// Start a fresh shrine on the same word lists.
    ///
    /// Rebuilding a [`Solver`] re-parses and re-sorts nearly a megabyte of words, which swamps the
    /// actual solving when playing many puzzles back to back.
    pub fn reset(&mut self) {
        self.live.clear();
        self.live.extend(0..self.answers.len() as u32);
        self.history.clear();
    }

    /// The live candidates, for reporting.
    pub fn candidates(&self) -> Vec<String> {
        self.live.iter().map(|&i| self.answers.word(i as usize)).collect()
    }

    pub fn history(&self) -> &[(String, Pattern)] {
        &self.history
    }

    /// Is this word one the shrine will accept at all?
    ///
    /// A rejected guess costs no attempt — `submit` skips the whole branch and the shrine just shakes
    /// (`shrineview.lua:277-281`) — but it does cost a round trip, so we never knowingly send one.
    pub fn is_legal(&self, word: &str) -> bool {
        self.guesses.position(word).is_some()
    }

    /// Narrow the candidate set by a colouring the shrine gave us.
    pub fn observe(&mut self, guess: &str, pattern: Pattern) {
        let g = guess.as_bytes().to_vec();
        self.live.retain(|&i| feedback(&g, self.answers.get(i as usize)) == pattern);
        self.history.push((guess.to_string(), pattern));
    }

    /// The next word to play.
    ///
    /// Returns `None` only when the candidate set has been emptied, which means a colouring was
    /// misread or the baked answer list is stale. That is worth surfacing rather than papering over
    /// with an arbitrary guess: every remaining attempt would be wasted.
    pub fn propose(&self) -> Option<String> {
        // Nothing has been ruled out yet, so this is the precomputed opener.
        if self.history.is_empty() {
            if let Some(&(_, _, w)) =
                OPENERS.iter().find(|&&(l, b, _)| l == self.length && b == self.band)
            {
                return Some(w.to_string());
            }
        }
        match self.live.len() {
            0 => None,
            // Nothing can separate two candidates better than guessing one of them: it wins outright
            // half the time and leaves exactly one word otherwise.
            1..=2 => Some(self.answers.word(self.live[0] as usize)),
            n => {
                if n <= ENDGAME_MAX {
                    if let Some(g) = self.endgame() {
                        return Some(g);
                    }
                }
                Some(self.best_guess())
            }
        }
    }

    /// Guesses left before the shrine is lost.
    pub fn budget_left(&self) -> usize {
        max_guesses(self.length).saturating_sub(self.history.len())
    }

    /// Exact search: the guess that solves every remaining candidate within the guesses we have
    /// left, choosing the one with the lowest total cost among those that can.
    ///
    /// Returns `None` when no guess can guarantee it, or when the node guard trips — in both cases
    /// greedy answers instead, which is no worse than not having tried.
    fn endgame(&self) -> Option<String> {
        let depth = self.budget_left();
        if depth < 2 {
            return None;
        }
        // Searching deeper than this costs seconds per turn for no measured benefit; greedy holds
        // the early game and the exact search takes over for the endgame it is meant to fix.
        if depth > ENDGAME_DEPTH {
            return None;
        }
        let live: Vec<u32> = self.live.clone();
        let pool = self.endgame_pool(&live);
        let mut memo = HashMap::new();
        let mut nodes = 0usize;

        let mut best: Option<(u32, usize)> = None;
        for &gi in &pool {
            let Some(total) = self.solve_cost(&live, gi, depth, &pool, &mut memo, &mut nodes)
            else {
                continue;
            };
            if best.is_none_or(|(b, _)| total < b) {
                best = Some((total, gi));
            }
        }
        best.map(|(_, gi)| String::from_utf8_lossy(self.guesses.get(gi)).into_owned())
    }

    /// Total guesses to finish every word in `live` if we play `gi` now, or `None` if that cannot be
    /// done within `depth`.
    fn solve_cost(
        &self, live: &[u32], gi: usize, depth: usize, pool: &[usize],
        memo: &mut HashMap<(Vec<u32>, usize), Option<u32>>, nodes: &mut usize,
    ) -> Option<u32> {
        let g = self.guesses.get(gi);
        let win = solved(self.length);
        let mut buckets: HashMap<Pattern, Vec<u32>> = HashMap::new();
        for &i in live {
            buckets.entry(feedback(g, self.answers.get(i as usize))).or_default().push(i);
        }
        // A guess that cannot tell any two candidates apart makes no progress and would recurse
        // forever on the same set.
        if buckets.len() == 1 && !buckets.contains_key(&win) {
            return None;
        }
        // Every candidate pays for this guess.
        let mut total = live.len() as u32;
        for (pattern, rest) in buckets {
            if pattern == win {
                continue;
            }
            total += self.best_cost(&rest, depth - 1, pool, memo, nodes)?;
        }
        Some(total)
    }

    /// Cheapest total over all guesses for this candidate set, within `depth`.
    fn best_cost(
        &self, live: &[u32], depth: usize, pool: &[usize],
        memo: &mut HashMap<(Vec<u32>, usize), Option<u32>>, nodes: &mut usize,
    ) -> Option<u32> {
        match live.len() {
            0 => return Some(0),
            // One candidate left is one guess: name it.
            1 => return Some(1),
            _ => {}
        }
        if depth == 0 {
            return None;
        }
        // With one guess left, only a single candidate can be guaranteed.
        if depth == 1 {
            return None;
        }
        *nodes += 1;
        if *nodes > ENDGAME_NODES {
            return None;
        }
        let key = (live.to_vec(), depth);
        if let Some(&cached) = memo.get(&key) {
            return cached;
        }
        let mut best: Option<u32> = None;
        for &gi in pool {
            if let Some(t) = self.solve_cost(live, gi, depth, pool, memo, nodes) {
                if best.is_none_or(|b| t < b) {
                    best = Some(t);
                }
                // 2n-1 is the floor: every candidate pays for this guess, and all but the one we
                // happened to name pays for at least one more. Nothing can beat it, so stop.
                if best == Some(2 * live.len() as u32 - 1) {
                    break;
                }
            }
        }
        memo.insert(key, best);
        best
    }

    /// Words worth branching on in the exact search: every candidate, plus the probes that split
    /// this particular set best.
    ///
    /// Ranked by discrimination over the live set itself — bucket count first, then expected
    /// remaining size — rather than by the letter-frequency proxy [`Solver::pool`] uses. That proxy
    /// is what fails on one-letter clusters, and this is the search meant to rescue them.
    fn endgame_pool(&self, live: &[u32]) -> Vec<usize> {
        let win = solved(self.length);
        let live_words: std::collections::HashSet<&[u8]> =
            live.iter().map(|&i| self.answers.get(i as usize)).collect();
        let mut pool: Vec<usize> = Vec::new();
        let mut ranked: Vec<(usize, u64, usize)> = Vec::with_capacity(self.guesses.len());
        let mut counts: HashMap<Pattern, u32> = HashMap::with_capacity(64);
        for gi in 0..self.guesses.len() {
            let g = self.guesses.get(gi);
            counts.clear();
            for &i in live {
                *counts.entry(feedback(g, self.answers.get(i as usize))).or_insert(0) += 1;
            }
            if counts.len() == 1 && !counts.contains_key(&win) {
                continue; // separates nothing
            }
            if live_words.contains(g) {
                pool.push(gi);
                continue;
            }
            let spread: u64 = counts.values().map(|&n| (n as u64) * (n as u64)).sum();
            ranked.push((counts.len(), spread, gi));
        }
        // Most buckets first; among equals, the flattest split.
        ranked.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        pool.extend(ranked.iter().take(ENDGAME_BRANCH).map(|&(_, _, gi)| gi));
        pool
    }

    fn best_guess(&self) -> String {
        let sample = self.sample();
        let pool = self.pool();
        let mut best: Option<(f64, usize, bool, usize)> = None; // (score, -buckets, is_candidate, idx)
        let mut chosen = 0usize;
        let live_set: std::collections::HashSet<&[u8]> =
            self.live.iter().map(|&i| self.answers.get(i as usize)).collect();

        let mut counts: HashMap<Pattern, u32> = HashMap::with_capacity(512);
        for &gi in &pool {
            let g = self.guesses.get(gi);
            counts.clear();
            for &si in &sample {
                *counts.entry(feedback(g, self.answers.get(si as usize))).or_insert(0) += 1;
            }
            let all_green = solved(self.length);
            let total: f64 = sample.len() as f64;
            // Expected size of the set we would be left with. The all-green bucket is omitted
            // because that branch ends the game -- which is exactly the credit a guess that could
            // itself be the answer deserves.
            let expected: f64 = counts
                .iter()
                .filter(|(&p, _)| p != all_green)
                .map(|(_, &n)| (n as f64) * (n as f64))
                .sum::<f64>()
                / total;
            let buckets = counts.len();
            let is_candidate = live_set.contains(g);
            let key = (expected, usize::MAX - buckets, !is_candidate, gi);
            if best.is_none_or(|b| key < b) {
                best = Some(key);
                chosen = gi;
            }
        }
        String::from_utf8_lossy(self.guesses.get(chosen)).into_owned()
    }

    /// Candidates to score against — all of them until the set is large enough that sampling is
    /// cheaper than certainty.
    fn sample(&self) -> Vec<u32> {
        if self.live.len() <= SCORE_SAMPLE {
            return self.live.clone();
        }
        let stride = self.live.len() / SCORE_SAMPLE;
        self.live.iter().step_by(stride.max(1)).copied().collect()
    }

    /// Guesses worth scoring: every live candidate, plus the most promising probes.
    ///
    /// Live candidates are never dropped, so the win-now option is always on the table. Probes are
    /// ranked by how much of the live set their letters touch — a cheap proxy that only has to avoid
    /// discarding a good word, since the real scoring runs afterwards.
    fn pool(&self) -> Vec<usize> {
        // Few enough candidates that scoring everything is cheap, and the prefilter's blind spot
        // (see EXACT_POOL_MAX) is exactly where the endgame lives.
        if self.live.len() <= EXACT_POOL_MAX {
            return (0..self.guesses.len()).collect();
        }
        let mut present = [0u32; 26];
        let mut positional = vec![[0u32; 26]; self.length];
        for &i in &self.live {
            let w = self.answers.get(i as usize);
            let mut seen = [false; 26];
            for (j, &b) in w.iter().enumerate() {
                let l = (b - b'a') as usize;
                positional[j][l] += 1;
                if !seen[l] {
                    seen[l] = true;
                    present[l] += 1;
                }
            }
        }

        let live_words: std::collections::HashSet<&[u8]> =
            self.live.iter().map(|&i| self.answers.get(i as usize)).collect();
        let mut probes: Vec<(u32, usize)> = Vec::with_capacity(self.guesses.len());
        let mut pool: Vec<usize> = Vec::new();
        for gi in 0..self.guesses.len() {
            let w = self.guesses.get(gi);
            if live_words.contains(w) {
                pool.push(gi);
                continue;
            }
            let mut seen = [false; 26];
            let mut score = 0u32;
            for (j, &b) in w.iter().enumerate() {
                let l = (b - b'a') as usize;
                score += positional[j][l];
                if !seen[l] {
                    seen[l] = true;
                    // Distinct letters are what shrink the set; a repeat buys much less.
                    score += present[l];
                }
            }
            probes.push((score, gi));
        }
        let keep = PROBE_POOL.min(probes.len());
        if keep > 0 {
            // Partition around the keep-th best rather than sorting: the order within the kept
            // probes is irrelevant, only membership matters.
            let nth = keep - 1;
            probes.select_nth_unstable_by(nth, |a, b| b.0.cmp(&a.0));
            pool.extend(probes[..keep].iter().map(|&(_, gi)| gi));
        }
        pool
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fb(guess: &str, answer: &str) -> String {
        show(feedback(guess.as_bytes(), answer.as_bytes()), guess.len())
    }

    #[test]
    fn greens_and_greys() {
        assert_eq!(fb("crane", "crane"), "GGGGG");
        assert_eq!(fb("abcd", "wxyz"), "....");
    }

    #[test]
    fn a_present_letter_elsewhere_is_yellow() {
        assert_eq!(fb("stone", "notes"), "YYYYY");
        assert_eq!(fb("crane", "nacre"), "YYYYG");
    }

    // The duplicate-letter cases below are the ones that separate a correct implementation from a
    // plausible one, worked by hand against `shrineview.lua:164-175`.

    #[test]
    fn a_repeated_guess_letter_is_capped_at_the_answers_count() {
        // guess l-l-a-m-a, answer l-a-p-s-e. The answer's only 'l' is claimed by the green at 0, so
        // the second 'l' gets nothing. Only the first of the two 'a's is paid.
        assert_eq!(fb("llama", "lapse"), "G.Y..");
        // guess a-d-d-e-d, answer d-r-e-a-d. Three 'd's guessed against two in the answer: the green
        // at 4 takes one, the LEFTMOST remaining 'd' takes the other, and the third greys.
        assert_eq!(fb("added", "dread"), "YY.YG");
    }

    #[test]
    fn greens_claim_their_letters_before_any_yellow() {
        // guess e-e-r-i-e, answer r-e-s-i-t. One 'e' in the answer and it is green at position 1, so
        // both other 'e's grey. A naive left-to-right pass would paint position 0 yellow -- the
        // classic bug this whole test exists to catch.
        assert_eq!(fb("eerie", "resit"), ".GYG.");
        // Same shape, minimal: the answer's single 'a' is spoken for by the green.
        assert_eq!(fb("aabbb", "xaxxx"), ".G...");
    }

    #[test]
    fn yellows_are_handed_out_left_to_right() {
        // Two 'o's in the guess, one in the answer: the leftmost non-green 'o' takes it.
        assert_eq!(fb("oozes", "stole"), "Y..YY");
    }

    #[test]
    fn patterns_round_trip() {
        for s in ["GGGGG", ".....", "GY.YG", "..G.."] {
            let p = parse_pattern(s).unwrap();
            assert_eq!(show(p, s.len()), s);
        }
        assert_eq!(parse_pattern("GGGGG").unwrap(), solved(5));
    }

    #[test]
    fn the_guess_budget_matches_the_game() {
        // maxGuesses = clamp(ceil(30/L), 6, 10)
        assert_eq!(max_guesses(4), 8);
        assert_eq!(max_guesses(5), 6);
        assert_eq!(max_guesses(6), 6);
        assert_eq!(max_guesses(7), 6);
    }

    #[test]
    fn the_baked_lists_are_present_and_usable_at_every_length_and_band() {
        // Deliberately no expected sizes. The game's lexica and dictionary move between versions —
        // v52.4 added `clanker`, which shifted the 7-letter guess list by one — and pinning the
        // counts here only ever asserted that `data/` agreed with a literal in this file. Neither
        // side is the game, so the check could not detect the drift it looked like it was guarding,
        // while every legitimate version bump forced an edit. Drift is caught by re-running
        // `gen_shrine_data`, which reads the game and rewrites `data/`.
        //
        // What is worth asserting is that the embedded data is there and loads. `WordList::extend`
        // (`shrine.rs:234-246`) already rejects a word of the wrong length or with a non-lowercase
        // byte, so a load that succeeds and yields a non-empty list of the right width is the
        // usable condition — and it catches the real risk, a truncated or absent `data/` file.
        for len in MIN_LEN..=MAX_LEN {
            let mut banded = Vec::new();
            for band in [Band::Easy, Band::Hard] {
                let list = Baked
                    .answers(len, band)
                    .unwrap_or_else(|e| panic!("answers {len} {band:?} did not load: {e}"));
                assert!(!list.is_empty(), "answers {len} {band:?} loaded empty");
                assert_eq!(list.length(), len, "answers {len} {band:?} has the wrong width");
                banded.push(list.len());
            }

            let wild = Baked
                .answers(len, Band::Wild)
                .unwrap_or_else(|e| panic!("answers {len} Wild did not load: {e}"));
            // Wild takes a second path through `answers` — it concatenates both files rather than
            // reading one — so it is worth confirming it picks up every word and drops none.
            assert_eq!(
                wild.len(),
                banded[0] + banded[1],
                "wild {len} should be exactly easy plus hard"
            );

            let guesses =
                Baked.guesses(len).unwrap_or_else(|e| panic!("guesses {len} did not load: {e}"));
            assert!(!guesses.is_empty(), "guesses {len} loaded empty");
            assert_eq!(guesses.length(), len, "guesses {len} has the wrong width");
        }
    }

    #[test]
    fn every_answer_is_a_legal_guess() {
        // If an answer could not be typed, the shrine would be unwinnable. Worth checking rather
        // than assuming: the two lists come from different files.
        for len in MIN_LEN..=MAX_LEN {
            let answers = Baked.answers(len, Band::Wild).unwrap();
            let guesses = Baked.guesses(len).unwrap();
            let legal: std::collections::HashSet<&[u8]> = guesses.iter().collect();
            let missing: Vec<String> = answers
                .iter()
                .filter(|w| !legal.contains(*w))
                .map(|w| String::from_utf8_lossy(w).into_owned())
                .collect();
            assert!(missing.is_empty(), "length {len}: answers not in the guess list: {missing:?}");
        }
    }

    #[test]
    fn observing_narrows_to_the_answer() {
        let mut s = Solver::new(&Baked, 5, Band::Easy).unwrap();
        let before = s.remaining();
        let answer = "crane";
        // Feed it real colourings until it commits.
        for _ in 0..max_guesses(5) {
            let g = s.propose().expect("candidates remain");
            if g == answer {
                return;
            }
            let p = feedback(g.as_bytes(), answer.as_bytes());
            s.observe(&g, p);
            assert!(s.remaining() < before, "each guess must narrow the set");
            assert!(s.remaining() > 0, "the true answer must survive every filter");
        }
        panic!("did not find {answer} within the budget; history {:?}", s.history());
    }

    // ## Two replays here, and the sweep behind them lives in a binary — the dev's question, 2026-08-17
    //
    // *Do the tests run fast enough to cover the entire dictionaries, or should we hardcode the
    // "last guess" answers?* Both were measured, and the split follows the numbers:
    //
    // | | debug | release | runs |
    // |---|---|---|---|
    // | `shrine_selfplay` — every word in every band | — | ~90 s, threaded | on demand |
    // | `the_answers_with_no_slack_…` — all 346 | 129 s | 7.5 s | on demand |
    // | `the_hardest_word_of_each_shrine_…` — 8 words | 3 s | — | **always** |
    //
    // `cargo test` is a **debug** build and the suite is thirteen seconds, so the full replay cannot
    // live in it. The eight-word one costs nothing in wall clock, because it runs alongside the rest.
    //
    // **The exhaustive sweep is `shrine_selfplay` and always was** — it plays every word in every
    // band and reports the mean, the worst case and anything over budget, threaded. What it did not
    // report until now is how many answers land *exactly on* the budget, which is the number that
    // decides all of this; it writes them to `data/shrine-hardest.txt`.
    //
    // **The at-risk set is small and exact**: 346 answers across eight configurations win on the
    // *last* allowed guess. The other four have a guess in hand, so a misread costs them a turn
    // rather than the shrine — which is why the pinned list is those eight and not all twelve.
    //
    // **What a pinned list cannot do, and why the sweep stays the authority.** It only knows
    // *yesterday's* hard words. Change an opener, `ENDGAME_MAX`, `best_guess` or the dictionaries and
    // some other word becomes the hardest, and both tests here will pass while it goes over budget.
    // That is the same trap as a baked `OPENERS` table confirming itself — see
    // [`Solver::compute_opener`], which exists so the constant cannot. **After touching any of those,
    // rerun `shrine_selfplay`**, which regenerates the list and fails loudly on a real regression.

    /// Every `(length, band, answer)` in the generated zero-slack list.
    fn hardest() -> Vec<(usize, &'static str, &'static str)> {
        include_str!("../data/shrine-hardest.txt")
            .lines()
            .map(str::trim_end)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(|l| {
                let mut f = l.split('\t');
                let (Some(len), Some(band), Some(word)) = (f.next(), f.next(), f.next()) else {
                    panic!("malformed line in shrine-hardest.txt: {l:?}");
                };
                (len.parse().expect("a length"), band, word)
            })
            .collect()
    }

    fn replay(words: &[(usize, &str, &str)]) -> usize {
        let mut solvers: HashMap<(usize, &str), Solver> = HashMap::new();
        let mut played = 0usize;
        for &(len, band, answer) in words {
            let b = match band {
                "Easy" => Band::Easy,
                "Hard" => Band::Hard,
                "Wild" => Band::Wild,
                other => panic!("unknown band {other:?}"),
            };
            // One solver per configuration, reset between words: rebuilding re-parses nearly a
            // megabyte of word lists, which would swamp the solving itself. See [`Solver::reset`].
            let s = solvers.entry((len, band)).or_insert_with(|| {
                Solver::new(&Baked, len, b).expect("word lists for every configuration")
            });
            s.reset();
            let budget = max_guesses(len);
            let mut solved_in = None;
            for turn in 1..=budget {
                let guess = s.propose().unwrap_or_else(|| {
                    panic!("{answer} ({len} {band}): candidates emptied on guess {turn}")
                });
                if guess == answer {
                    solved_in = Some(turn);
                    break;
                }
                s.observe(&guess, feedback(guess.as_bytes(), answer.as_bytes()));
            }
            assert_eq!(
                solved_in,
                Some(budget),
                "{answer} ({len} {band}) is listed as needing exactly {budget}; \
                 regenerate data/shrine-hardest.txt if the solver changed"
            );
            played += 1;
        }
        played
    }

    /// One zero-slack word per configuration, on every `cargo test`.
    ///
    /// The cheap half of the split described on [`the_answers_with_no_slack_are_still_solved_in_time`]:
    /// every `(length, band)` that has no guess in hand is exercised to its full depth, so gross
    /// breakage in the opener table, the endgame search or the word lists shows up in the ordinary
    /// suite rather than only when someone remembers to run the slow one.
    #[test]
    fn the_hardest_word_of_each_shrine_is_still_solved_in_time() {
        let all = hardest();
        let mut seen: Vec<(usize, &str)> = Vec::new();
        let sample: Vec<(usize, &str, &str)> = all
            .iter()
            .filter(|(len, band, _)| {
                let fresh = !seen.contains(&(*len, band));
                if fresh {
                    seen.push((*len, band));
                }
                fresh
            })
            .copied()
            .collect();
        assert_eq!(replay(&sample), 8, "eight configurations win on the last guess");
    }

    /// All 346 of them, which is 7.5 s in release and over two minutes in debug.
    #[test]
    #[ignore = "the full zero-slack list; run with --release after touching the solver"]
    fn the_answers_with_no_slack_are_still_solved_in_time() {
        let played = replay(&hardest());
        assert!(played > 300, "the hardest-word list looks truncated: {played} words");
    }

    #[test]
    fn every_configuration_has_a_legal_opener() {
        for len in MIN_LEN..=MAX_LEN {
            for band in [Band::Easy, Band::Hard, Band::Wild] {
                let s = Solver::new(&Baked, len, band).unwrap();
                let opener = s.propose().expect("an opener");
                assert!(s.is_legal(&opener), "{len} {band:?}: {opener:?} is not a legal guess");
                assert_eq!(opener.len(), len);
                // It must come from the table rather than a live scan -- computing it costs seconds.
                assert!(
                    OPENERS.iter().any(|&(l, b, w)| l == len && b == band && w == opener),
                    "{len} {band:?} fell through to a computed opener"
                );
            }
        }
    }

    #[test]
    fn a_two_candidate_set_guesses_a_candidate() {
        let mut s = Solver::new(&Baked, 5, Band::Easy).unwrap();
        s.live.truncate(2);
        // A non-empty history is what takes it off the opener and into the policy.
        s.history.push(("xxxxx".into(), 0));
        let g = s.propose().unwrap();
        assert!(s.candidates().contains(&g), "with two left, guess one of them");
    }
}
