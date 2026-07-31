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
//! Not "does the board hold these letters" but "does typing this word work" — see [`crate::typist`].
//! The game's `textinput` is a specific greedy consumer that decides which tile each character
//! takes, and that choice sets both the tile state that scores the word and the corner count that
//! `resistCornerless` scales it by. Adjacency is not required unless the player carries
//! `wordRequirementAdjacent` gear, which [`Modifiers::from_save`] refuses to search under rather
//! than quietly assuming away.
//!
//! ## Word length
//!
//! There is **no minimum**: "a" and "I" are ordinary English words and the game accepts them. An
//! earlier version guessed at a 3-letter floor and silently dropped every one- and two-letter entry,
//! which both shrank the dictionary and hid legitimate plays. Note a single wood letter scores
//! `floor(1 * 0.4 + 0.5) = 0`, so short words are usually worthless rather than illegal — the search
//! rejects them on score, which is the honest reason, not on length.

use crate::geometry::Geometry;
use crate::observe::board::Tile;
use crate::score::Scorer;
use crate::typist::Typist;
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Shortest word the search will consider. One: the game has no minimum, and "a"/"I" are real words.
pub const MIN_WORD_LEN: usize = 1;

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

/// Everything about the enemy that changes what a word is worth.
///
/// Assembled from `combatSaveData` once per turn. The two members are opposite in character:
/// `excluded` throws words away, `resist_cornerless` changes what the survivors score.
pub struct Modifiers {
    /// Words from a lexicon this enemy resists or is immune to. `nerf * 0` is a zero-damage turn, so
    /// these are dropped outright rather than scored down (see [`crate::lexica`]).
    pub excluded: HashSet<String>,
    /// `resistCornerless` (`utils/words.lua:238-240`): the score is scaled by
    /// `cornersUsed / cornerCount`. Zero corners means zero damage.
    pub resist_cornerless: bool,
    /// Reasons the search should not be trusted. Non-empty means a score here is a guess.
    pub problems: Vec<String>,
}

impl Modifiers {
    /// Reads the enemy's statuses and the board's shape out of a save table.
    ///
    /// `tile_count` is the board dump's length, used to check the derived shape fits.
    pub fn from_save(
        game_dir: &Path,
        save: &crate::game::save::Table,
        tile_count: usize,
    ) -> Result<(Self, Geometry), crate::Error> {
        let statuses = crate::lexica::Lexica::statuses_from_save(save);
        let lexica = crate::lexica::Lexica::load(game_dir)?;
        let resolved = Geometry::from_save(game_dir, save, tile_count);

        let mut problems = resolved.problems.clone();
        problems.extend(lexica.problems().iter().cloned());
        problems.extend(lexica.unmodelled(&statuses));
        if resolved.geometry.adjacency_required {
            // Every pick would be confined to the 3x3 around the last one. Searching as if the whole
            // board were reachable would produce words that cannot be typed at all.
            problems.push("wordRequirementAdjacent gear is not modelled".into());
        }

        Ok((
            Modifiers {
                excluded: lexica.excluded_words(&statuses),
                resist_cornerless: statuses.contains_key("resistCornerless"),
                problems,
            },
            resolved.geometry,
        ))
    }

    /// A modifier set for an ordinary enemy on an ordinary board.
    pub fn none() -> Self {
        Modifiers { excluded: HashSet::new(), resist_cornerless: false, problems: Vec::new() }
    }

    /// The `mult * nerf` a word earns, given how many corners it used.
    ///
    /// `getWordBonusModifier` only ever multiplies, so an enemy without `resistCornerless` gets a
    /// flat 1. Bonuses above 1 are deliberately not claimed: we never want to call a word lethal
    /// because of a bonus we mis-read.
    pub fn modifier(&self, corners_used: usize, corner_count: usize) -> f64 {
        if !self.resist_cornerless || corner_count == 0 {
            return 1.0;
        }
        corners_used as f64 / corner_count as f64
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
    /// Longest makeable word seen. Tracked separately because the refresh rule is about LENGTH, and
    /// the highest-scoring word is not necessarily the longest — a short word of gold tiles can
    /// outscore a long one of wood. Free to collect: the only time it matters is when no lethal word
    /// was found, and that case already scans the whole dictionary.
    pub longest: Option<Found>,
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
        match &self.longest {
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
    geometry: &Geometry,
    mods: &Modifiers,
    need: i64,
    threads: usize,
) -> Outcome {
    let typist = Typist::new(tiles, geometry);
    let corner_count = geometry.corner_count();
    let words = dict.words();
    let threads = threads.max(1).min(words.len().max(1));
    let chunk = words.len().div_ceil(threads);

    let stop = AtomicBool::new(false);
    let lethal: Mutex<Option<Found>> = Mutex::new(None);
    let best: Mutex<Option<Found>> = Mutex::new(None);
    let longest: Mutex<Option<Found>> = Mutex::new(None);
    let considered = std::sync::atomic::AtomicUsize::new(0);

    std::thread::scope(|scope| {
        for (slice, part) in words.chunks(chunk).enumerate() {
            let (stop, lethal, best, longest, considered, typist, mods) =
                (&stop, &lethal, &best, &longest, &considered, &typist, mods);
            scope.spawn(move || {
                let mut seen = 0usize;
                let mut local_best: Option<Found> = None;
                let mut local_longest: Option<Found> = None;
                for word in part {
                    // Checked periodically rather than every word: an atomic load per word would
                    // dominate the loop for no benefit at this granularity.
                    if seen % 512 == 0 && stop.load(Ordering::Relaxed) {
                        break;
                    }
                    seen += 1;
                    if mods.excluded.contains(word) {
                        continue;
                    }
                    // Not "are the letters there" but "does typing this work", which also tells us
                    // exactly which tiles it eats -- and so what it is worth.
                    let Some(typed) = typist.type_word(word) else { continue };
                    let consumed: Vec<Tile> =
                        typed.tiles.iter().map(|&i| tiles[i].clone()).collect();
                    let score = scorer.score_typed(
                        &consumed,
                        word.chars().count(),
                        mods.modifier(typed.corners_used, corner_count),
                    );
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
                    if local_longest
                        .as_ref()
                        .map(|b| word.chars().count() > b.word.chars().count())
                        .unwrap_or(true)
                    {
                        local_longest = Some(Found { word: word.clone(), score, slice });
                    }
                }
                considered.fetch_add(seen, Ordering::Relaxed);
                if let Some(lb) = local_best {
                    let mut b = best.lock().unwrap();
                    if b.as_ref().map(|cur| lb.score > cur.score).unwrap_or(true) {
                        *b = Some(lb);
                    }
                }
                if let Some(ll) = local_longest {
                    let mut l = longest.lock().unwrap();
                    if l.as_ref()
                        .map(|cur| ll.word.chars().count() > cur.word.chars().count())
                        .unwrap_or(true)
                    {
                        *l = Some(ll);
                    }
                }
            });
        }
    });

    Outcome {
        lethal: lethal.into_inner().unwrap(),
        best: best.into_inner().unwrap(),
        longest: longest.into_inner().unwrap(),
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

    fn race(scorer: &Scorer, dict: &Dictionary, tiles: &[Tile], mods: &Modifiers, need: i64) -> Outcome {
        race_for_kill(dict, scorer, tiles, &Geometry::default(), mods, need, 8)
    }

    #[test]
    fn refresh_only_when_nothing_good_and_nothing_lethal() {
        let lethal = Outcome {
            lethal: Some(Found { word: "CAT".into(), score: 9, slice: 0 }),
            best: Some(Found { word: "CAT".into(), score: 9, slice: 0 }),
            longest: Some(Found { word: "CAT".into(), score: 9, slice: 0 }),
            words_considered: 1,
        };
        // A kill ends the exchange, which beats any board-quality judgement.
        assert!(!lethal.should_refresh(8), "never refresh when a lethal word exists");

        let weak = Outcome {
            lethal: None,
            best: Some(Found { word: "CAT".into(), score: 4, slice: 0 }),
            longest: Some(Found { word: "CAT".into(), score: 4, slice: 0 }),
            words_considered: 1,
        };
        assert!(weak.should_refresh(8), "3 letters is under the 8 threshold");
        assert!(!weak.should_refresh(3), "3 letters meets a threshold of 3");

        let nothing = Outcome::default();
        assert!(nothing.should_refresh(8), "no playable word at all must refresh");
    }

    #[test]
    fn refresh_keys_off_length_not_score() {
        // A short word of gold tiles can outscore a long one of wood, so `best` is the wrong field
        // for a rule that is explicitly about length. JAZZ-like cases are exactly why these are
        // tracked separately.
        let out = Outcome {
            lethal: None,
            best: Some(Found { word: "JAY".into(), score: 20, slice: 0 }),
            longest: Some(Found { word: "OATMEALS".into(), score: 12, slice: 1 }),
            words_considered: 10,
        };
        assert!(!out.should_refresh(8), "an 8-letter word meets the threshold even if not top-scoring");
        assert_eq!(out.choice().map(|f| f.word.as_str()), Some("JAY"), "still PLAY the best scorer");
    }

    #[test]
    fn a_corner_resistant_enemy_takes_no_damage_from_a_corner_free_word() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        // The failure the corner model exists to prevent. The skeleton shield boss carries
        // `resistCornerless` (`rpg/enemies/skeletons.lua:251`), and against it a word that touches no
        // corner does LITERALLY nothing -- nerf = 0/4. Reporting such a word as lethal would waste a
        // turn against a boss and could lose the run.
        let dict = Dictionary::load(&game_dir()).unwrap();
        let scorer = Scorer::new(&game_dir()).unwrap();
        let board = crypt_board();
        let geom = Geometry::default();

        let mut cornerless = Modifiers::none();
        cornerless.resist_cornerless = true;

        let out = race(&scorer, &dict, &board, &cornerless, 3);
        let played = out.choice().expect("something must be playable").word.clone();
        let typed = Typist::new(&board, &geom).type_word(&played).unwrap();
        assert!(
            typed.corners_used > 0,
            "{played} uses no corner, so it would do zero damage to this enemy"
        );
    }

    #[test]
    fn the_corner_nerf_changes_what_is_worth_playing() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        // Same board, same enemy health, different answer -- which is the whole point. If the nerf
        // made no difference to the search, it would not be modelled, merely stored.
        let dict = Dictionary::load(&game_dir()).unwrap();
        let scorer = Scorer::new(&game_dir()).unwrap();
        let board = crypt_board();

        let mut cornerless = Modifiers::none();
        cornerless.resist_cornerless = true;

        // High enough that the search exhausts and reports its true best under each rule.
        let plain_best = race(&scorer, &dict, &board, &Modifiers::none(), 100_000).best.unwrap();
        let nerfed_best = race(&scorer, &dict, &board, &cornerless, 100_000).best.unwrap();
        assert!(
            nerfed_best.score < plain_best.score,
            "the nerf can only reduce: {} vs {}",
            nerfed_best.score,
            plain_best.score
        );
    }

    #[test]
    fn a_full_corner_sweep_is_not_nerfed_at_all() {
        // nerf = 4/4 = 1. The nerf must be a fraction of the corners USED, not a flat penalty --
        // otherwise a word that sweeps every corner would still be docked.
        let mut m = Modifiers::none();
        m.resist_cornerless = true;
        assert_eq!(m.modifier(4, 4), 1.0);
        assert_eq!(m.modifier(2, 4), 0.5);
        assert_eq!(m.modifier(0, 4), 0.0, "no corners means no damage");
        // And an enemy without the status is never scaled.
        assert_eq!(Modifiers::none().modifier(0, 4), 1.0);
    }

    #[test]
    fn the_halfling_can_never_reach_more_than_half_the_corners() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        // `shortCharacter` -- "Can only reach the bottom 3 rows" -- sets tileboardUnselectableRow4
        // through Row10 (`items/classpassives.lua:25-33`). On the default board that locks row 4,
        // which holds the corners (1,4) and (4,4).
        //
        // The denominator does NOT shrink to match: `tileboard.getCornerCount` is `#corners`
        // (`tileboard.lua:117-120`) and never consults the locks. So against a corner-resistant enemy
        // a halfling is capped at 2/4 -- HALF DAMAGE, permanently, with no word able to do better.
        // That is a fact about the game, not about this code, and the search must reproduce it
        // rather than quietly assume the corners it can see are all the corners there are.
        let save = crate::game::save::parse(
            r#"return { passives = {}, rpg = { player = { gearFlags = {
                tileboardUnselectableRow4 = 1, tileboardUnselectableRow5 = 1,
            } } } }"#,
        )
        .unwrap();
        let (_, geometry) = Modifiers::from_save(&game_dir(), &save, 16).unwrap();
        assert_eq!(geometry.corner_count(), 4, "the denominator ignores the locks");

        let reachable = geometry
            .corner_indices()
            .into_iter()
            .filter(|&i| geometry.slot_selectable(i))
            .count();
        assert_eq!(reachable, 2, "only the row-1 corners are left");

        // And the search reflects it: the best word cannot exceed the half-damage ceiling.
        let dict = Dictionary::load(&game_dir()).unwrap();
        let scorer = Scorer::new(&game_dir()).unwrap();
        let board = crypt_board();
        let mut cornerless = Modifiers::none();
        cornerless.resist_cornerless = true;

        let best = race_for_kill(&dict, &scorer, &board, &geometry, &cornerless, 100_000, 8)
            .best
            .expect("something is playable");
        let typed = Typist::new(&board, &geometry).type_word(&best.word).unwrap();
        assert!(typed.corners_used <= 2, "{} used {} corners", best.word, typed.corners_used);
        assert!(cornerless.modifier(typed.corners_used, geometry.corner_count()) <= 0.5);
    }

    #[test]
    fn a_locked_row_is_not_the_same_as_a_smaller_board() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        // The tiles in a locked row are still ON the board -- they are dumped, they count toward
        // totalTileCount, they just cannot be selected. Treating the halfling's board as 4x3 would
        // shift every subsequent tile's position by one and put the corners on the wrong letters.
        let save = crate::game::save::parse(
            r#"return { passives = {}, rpg = { player = { gearFlags = {
                tileboardUnselectableRow4 = 1,
            } } } }"#,
        )
        .unwrap();
        let (mods, geometry) = Modifiers::from_save(&game_dir(), &save, 16).unwrap();
        assert!(mods.problems.is_empty(), "a 16-tile dump still fits: {:?}", mods.problems);
        assert_eq!(geometry.total_tiles(), 16);
        assert_eq!(geometry.position(3), Some((1, 4)), "the locked tile keeps its place");
    }

    #[test]
    fn the_real_crypt_enemy_needs_no_modifiers() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        // The captured fight, read the way the live loop will read it. If this ever starts reporting
        // problems, the measured board that every other test is built on is no longer understood.
        let save = crate::game::save::load(Path::new("tests/fixtures/combatSaveData-crypt-l0.lua"))
            .expect("fixture loads");
        let (mods, geometry) = Modifiers::from_save(&game_dir(), &save, 16).unwrap();
        assert!(mods.problems.is_empty(), "problems: {:?}", mods.problems);
        assert!(!mods.resist_cornerless, "Amorphous does not resist corners");
        assert!(mods.excluded.is_empty(), "no lexicon immunity");
        assert_eq!(geometry, Geometry::default());
    }

    #[test]
    fn an_immune_lexicon_removes_its_words_from_the_race() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        // `nerf * 0` is a zero-damage turn, so an immune enemy's lexicon must not be searched at all.
        // This wires `crate::lexica` into the race, which previously computed exclusions nobody read.
        let save = crate::game::save::parse(
            r#"return { passives = {}, rpg = { enemy = { statusEffects = { lexiconBonusBone = 0 } } } }"#,
        )
        .unwrap();
        let (mods, _) = Modifiers::from_save(&game_dir(), &save, 16).unwrap();
        assert!(mods.excluded.contains("AITCHBONE"), "a bone word must be excluded");

        let dict = Dictionary::load(&game_dir()).unwrap();
        let scorer = Scorer::new(&game_dir()).unwrap();
        // A board that spells a bone word and little else.
        let board = plain("AITCHBONE");
        let out = race(&scorer, &dict, &board, &mods, 100_000);
        if let Some(best) = &out.best {
            assert_ne!(best.word, "AITCHBONE", "an immune word must never be chosen");
        }
    }

    #[test]
    fn one_and_two_letter_words_are_kept() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        // The game has no minimum length -- "a" and "I" are ordinary words. An earlier 3-letter floor
        // silently dropped them.
        let dict = Dictionary::load(&game_dir()).unwrap();
        assert!(dict.words().iter().any(|w| w.chars().count() == 1), "no 1-letter words survived");
        assert!(dict.words().iter().any(|w| w.chars().count() == 2), "no 2-letter words survived");
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
        let out = race(&scorer, &dict, &crypt_board(), &Modifiers::none(), 3);
        assert!(!out.should_refresh(8), "a lethal word means no refresh");
        let found = out.lethal.clone().expect("a 3-health enemy must be killable from this board");
        assert!(found.score >= 3);
        let board = crypt_board();
        let geom = Geometry::default();
        assert!(
            Typist::new(&board, &geom).type_word(&found.word).is_some(),
            "{} cannot actually be typed",
            found.word
        );
    }

    #[test]
    fn a_race_stops_early_rather_than_scanning_everything() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        let dict = Dictionary::load(&game_dir()).unwrap();
        let scorer = Scorer::new(&game_dir()).unwrap();
        let out = race(&scorer, &dict, &crypt_board(), &Modifiers::none(), 3);
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
        let out = race(&scorer, &dict, &crypt_board(), &Modifiers::none(), 100_000);
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
        assert_eq!(MIN_WORD_LEN, 1, "the game enforces no minimum word length");
        // Sorted, which is what makes a contiguous slice an alphabetical range.
        assert!(dict.words().windows(2).all(|w| w[0] <= w[1]));
    }
}
