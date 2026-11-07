//! Simulating what the game does when you type a word.
//!
//! ## Why a simulation and not a segmentation
//!
//! An earlier version asked "can the board's letters be cut into this word?" and answered with a
//! backtracking segmenter. That is the wrong question. `rpg.textinput` (`rpg.lua:801-858`) is a
//! specific greedy consumer, and it decides **which** tile each character takes:
//!
//! 1. If the previous tile's letter plus the new character names an unselected tile, that ligature
//!    tile is taken and the previous selection is undone (`wordboard.select` toggles,
//!    `wordboard.lua:132`). Then the same test for a three-character ligature.
//! 2. Otherwise a tile whose letter is exactly the character, else a restricted wildcard, else a
//!    plain wildcard.
//! 3. Otherwise, if some multi-letter tile *contains* the character, an **ephemeral placeholder** is
//!    selected — the mechanism that lets you type `S`, `T` into an `ST` tile. A word still holding a
//!    placeholder at the end is not a word: `wordboard.getWord` returns `''`
//!    (`wordboard.lua:277`).
//!
//! and `getUnselectedLegalTileWithLetter` (`tileboard.lua:2269-2292`) scans **corners first**, then
//! column-major. Two tiles can carry the same letter and different consequences — one burning and
//! worth nothing, one in a corner and worth everything against `resistCornerless` — so "the word is
//! makeable" was never the whole answer. Which tiles it eats is the answer.
//!
//! ## The letter-chaining curse
//!
//! `wordRequirementAdjacent` gear (directThinker) confines typing to the 3×3 box around the last
//! tile selected ([`crate::geometry::Geometry::adjacency_required`]):
//!
//! - the exact-letter lookup and both wildcard lookups search only that box, corners first and then
//!   column-major, as without the curse (`tileboard.lua:2269-2292`, `:2216-2245`); the "could a
//!   ligature tile absorb this letter" check does not (`:2182-2192`);
//! - the box moves only when the selection is refreshed with a real tile last
//!   (`wordboard.lua:265-267`), so a ligature is looked up around the tile it is about to replace,
//!   and a placeholder never moves it;
//! - the first letter has no box, so it comes from the whole board, corners first — **greedily**:
//!   if that tile strands the rest of the word, the game does not try another;
//! - on submit, every tile must be next to the one before ([`Geometry::tiles_adjacent`],
//!   `rpg/effects/modifiers/wordRequirementAdjacent.lua`), which a ligature swap can break.
//!
//! So the words this accepts are exactly the ones the keyboard can chain — fewer than a mouse could
//! drag, which is the user's call (2026-11-03): they type.
//!
//! **Which tiles fill those words is a different call, and it went the other way** (2026-11-06):
//! the user clicks the whole word, so any tile with the right letter may fill a slot. Typing still
//! decides which words exist and how they split into tiles; [`Typist::placements`] then offers each
//! other choice of tile, and the search scores them all.

use crate::geometry::Geometry;
use crate::observe::board::Tile;

/// The game's wildcard tile.
const WILDCARD: &str = ".";

/// The most placements [`Typist::placements`] returns for one word, the typed one included.
///
/// A bound on the work, not a reason: with twins merged into classes a real board stays far below
/// it, and the typed placement is always first, so a word cut short is never scored worse than
/// typing would.
pub const MAX_PLACEMENTS: usize = 64;

/// What typing a word actually consumed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Typed {
    /// Dump indices of the tiles the word used, in selection order.
    pub tiles: Vec<usize>,
    /// Parallel to [`Typed::tiles`]: the letter that must be typed into that tile because it is a
    /// wildcard, and `None` for an ordinary tile where the click is the whole action.
    ///
    /// Kept here rather than recovered later because only the simulation knows it. A wildcard's
    /// *letter* is not a property of the board — the same blank tile becomes a different letter in
    /// every candidate word — so by the time an index reaches the clicker the information is gone.
    pub wildcards: Vec<Option<char>>,
    /// How many of them are corner tiles — the numerator of the `resistCornerless` nerf.
    pub corners_used: usize,
    /// Every tile is next to the one before it (`tileboard.tilesAreAllAdjacent`) — what
    /// `wordScoreBonusAdjacent` pays on, and what the chaining curse requires.
    pub adjacent: bool,
}

impl Typed {
    /// The word as a sequence of actions: which tile, and what to type into it.
    pub fn steps(&self) -> impl Iterator<Item = (usize, Option<char>)> + '_ {
        self.tiles.iter().copied().zip(self.wildcards.iter().copied())
    }

    /// Does placing this word involve the on-screen keyboard at all?
    pub fn uses_a_wildcard(&self) -> bool {
        self.wildcards.iter().any(Option::is_some)
    }
}

/// A board prepared for repeated typing.
///
/// Built once per turn and reused across the whole dictionary, so the per-word cost is a scan of the
/// tiles and nothing else.
pub struct Typist<'a> {
    tiles: &'a [Tile],
    geometry: &'a Geometry,
    /// Uppercased tile letters, so the hot loop compares bytes it already owns.
    letters: Vec<String>,
    /// Corner dump indices in the game's corner order — the order the game tries them in.
    corner_first: Vec<usize>,
    /// Slots excluded by a locked row or column, or by the tile's own `unselectable`.
    usable: Vec<bool>,
}

/// What [`Typist::place`] carries down its walk unchanged, and the keys it has already kept.
struct Walk<'w> {
    wants: &'w [String],
    options: &'w [Vec<usize>],
    members: &'w [Vec<usize>],
    seen: &'w mut std::collections::HashSet<Vec<(usize, String)>>,
}

/// One entry of the word being built. Mirrors `wordTiles`.
#[derive(Clone, Copy)]
enum Slot {
    /// A real board tile.
    Real(usize),
    /// A wildcard standing in for a typed letter, which is still a real tile.
    Wild(usize, u8),
    /// The placeholder from clause 3 — no tile behind it.
    Ephemeral(u8),
}

impl<'a> Typist<'a> {
    pub fn new(tiles: &'a [Tile], geometry: &'a Geometry) -> Self {
        let letters: Vec<String> = tiles.iter().map(|t| t.letter.to_ascii_uppercase()).collect();
        let usable: Vec<bool> = (0..tiles.len())
            .map(|i| tiles[i].selectable() && geometry.slot_selectable(i))
            .collect();
        // Corners that are off the end of the dump are dropped rather than panicked on; a mismatch
        // is already reported by `Geometry::from_save`.
        let corner_first: Vec<usize> =
            geometry.corner_indices().into_iter().filter(|&i| i < tiles.len()).collect();
        Typist { tiles, geometry, letters, corner_first, usable }
    }

    pub fn geometry(&self) -> &Geometry {
        self.geometry
    }

    /// Types `word` and reports what it consumed, or `None` if the game would not accept it.
    pub fn type_word(&self, word: &str) -> Option<Typed> {
        let upper = word.to_ascii_uppercase();
        let chars = upper.as_bytes();
        let mut selected = vec![false; self.tiles.len()];
        let mut built: Vec<Slot> = Vec::with_capacity(chars.len());
        // The tile the curse's box is centred on: `lastSelectedCol/Row`. None until a real tile is
        // selected, and then only ever moved to a real tile — see the module notes.
        let mut last: Option<usize> = None;

        for &c in chars {
            self.step(c, &mut selected, &mut built, last)?;
            if let Some(Slot::Real(i) | Slot::Wild(i, _)) = built.last() {
                if self.geometry.adjacency_required {
                    last = Some(*i);
                }
            }
        }

        // A leftover placeholder means `getWord` yields '' -- the word cannot be submitted.
        if built.iter().any(|s| matches!(s, Slot::Ephemeral(_))) {
            return None;
        }

        // **Does the selection spell what we asked for?** Every step above succeeded, which is not
        // the same thing, and this is the check that says so.
        //
        // Each clause consumes one character and can only *add* a tile, so it is tempting to treat
        // "no step returned None" as proof. It is not: clause 1 and clause 2 also DELETE, and
        // `clear_ephemeral` deletes placeholders they never touched -- faithfully, because
        // `rpg.lua:824` does exactly that via `wordboard.lua:86-93`. A placeholder parked several
        // characters back can therefore be swept by a ligature completing much later, and the
        // character it stood for leaves the word without any step failing.
        //
        // Live 2026-08-11: `UNEQUIVOCALLY` on a board whose only U was inside a QU tile came back
        // as a clean 11-tile selection spelling NEQUIVOCALLY. The typist was asked whether it could
        // type the word and answered about something else. The fight stalled and the run ended.
        //
        // Rebuilt the way the game reads it -- `wordboard.getWord` concatenates `letterTemp or
        // letter` (`wordboard.lua:275-283`), which is precisely [`Typist::slot_letters`] -- so this
        // compares against the string the game itself would submit rather than against our idea of
        // one.
        let spelled: String = built.iter().map(|&s| self.slot_letters(s)).collect();
        if spelled != upper {
            return None;
        }

        let tiles: Vec<usize> = built
            .iter()
            .map(|s| match s {
                Slot::Real(i) | Slot::Wild(i, _) => *i,
                Slot::Ephemeral(_) => unreachable!("checked above"),
            })
            .collect();
        // The game lower-cases the letter before storing it (`onscreenKeypress`, `rpg.lua:664-666`),
        // and `textinput` matches on `%a` either way, so the case here is cosmetic -- but sending
        // what the game will store keeps a log line comparable with a `letterTemp` dump.
        let wildcards: Vec<Option<char>> = built
            .iter()
            .map(|s| match s {
                Slot::Wild(_, c) => Some((*c as char).to_ascii_lowercase()),
                _ => None,
            })
            .collect();
        let corners_used = tiles.iter().filter(|&&i| self.geometry.is_corner(i)).count();
        let adjacent =
            !tiles.is_empty() && tiles.windows(2).all(|p| self.geometry.tiles_adjacent(p[0], p[1]));
        if self.geometry.adjacency_required && !adjacent {
            // Typed, but refused on submit: "not contiguous".
            return None;
        }
        Some(Typed { tiles, wildcards, corners_used, adjacent })
    }

    /// Every way of **clicking** `word` with the segmentation typing found: [`Typist::type_word`]'s
    /// tiles first, then each other choice of tile for its slots.
    ///
    /// The user's call, 2026-11-06: *they click the whole word* rather than type it, so which of two
    /// `B` tiles comes first is theirs to choose. That matters because some materials pay only in
    /// one position: the serrated tiles (`*toxin`, `*bleed`), `gemfire` and `gemice` as the
    /// **first** tile (`active = wordTiles[1]==tile`), and `woodbraced`, `gemheal` and `gemice` as
    /// the **last** (`wordTiles[#wordTiles]==tile`), `rpg/effects/material/*.lua`. Typing always
    /// takes the game's first match in its corner-first scan, so a board with `BREAKABLY` on it and
    /// a `silvertoxin` second `B` scored the word without its toxin, though one click would add it.
    ///
    /// A click is never refused for position — `wordboard.select` takes any selectable tile; the
    /// chaining curse checks adjacency only on submit, which is checked here as typing checks it.
    ///
    /// **What varies is the tile behind each slot, not the slots.** A slot holding `CH` takes another
    /// `CH` tile, never a `C` and an `H`; a single letter takes any tile with that letter or a
    /// wildcard that admits it. So a word typing cannot make is still not offered — a segmentation
    /// search is a separate, larger change.
    ///
    /// **Tiles that cannot be told apart are not permuted.** Without the curse, two tiles with the
    /// same letter, the same quality and the same corner standing score identically wherever they
    /// go, so they share one class and the class takes its tiles in typing's order — which keeps the
    /// typed placement exactly as it was and keeps the count small. What that gives up is the
    /// purely positional tiebreak between twins ([`crate::pick::hazard_fall`], the board's
    /// deviation, `wordScoreBonusAdjacent`). Under the curse adjacency is everything, so every tile
    /// is its own class. At most [`MAX_PLACEMENTS`] are returned either way.
    pub fn placements(&self, word: &str) -> Vec<Typed> {
        let Some(typed) = self.type_word(word) else { return Vec::new() };
        // What each slot must read as: the tile's letters, or the letter typed into a wildcard.
        let wants: Vec<String> = typed
            .steps()
            .map(|(i, wild)| match wild {
                Some(c) => c.to_ascii_uppercase().to_string(),
                None => self.letters[i].clone(),
            })
            .collect();

        // Classes of interchangeable tiles. Under the curse every tile is its own.
        let mut class_of = vec![usize::MAX; self.tiles.len()];
        let mut reps: Vec<usize> = Vec::new();
        for (i, class) in class_of.iter_mut().enumerate() {
            if !self.usable[i] {
                continue;
            }
            let twin = (!self.geometry.adjacency_required)
                .then(|| {
                    reps.iter().position(|&r| {
                        self.tiles[r] == self.tiles[i]
                            && self.geometry.is_corner(r) == self.geometry.is_corner(i)
                    })
                })
                .flatten();
            *class = match twin {
                Some(c) => c,
                None => {
                    reps.push(i);
                    reps.len() - 1
                }
            };
        }
        // Each class's tiles in the order they are handed out: the ones typing used, as it used
        // them, then the rest in the game's scan order.
        let mut members: Vec<Vec<usize>> = vec![Vec::new(); reps.len()];
        let scan = typed.tiles.iter().chain(&self.corner_first).copied().chain(0..self.tiles.len());
        for i in scan {
            if class_of[i] != usize::MAX && !members[class_of[i]].contains(&i) {
                members[class_of[i]].push(i);
            }
        }
        // The classes that can fill each slot, the one typing chose first.
        // A wildcard stands in for a slot typing filled with a real letter only if it is a special
        // one: a plain wildcard is worth nothing (`wood0`) and costs a hoarded tile, so it never
        // beats the letter, and offering it roughly doubled the work on a board with three of them.
        let special =
            |q: &crate::observe::board::Quality| q.material.is_some() || q.border.is_some();
        let fits = |rep: usize, want: &str, real_slot: bool| -> bool {
            let l = &self.letters[rep];
            if l == want {
                return true;
            }
            if l != WILDCARD || want.len() != 1 {
                return false;
            }
            if real_slot && !special(&self.tiles[rep].quality) {
                return false;
            }
            match self.tiles[rep].quality.ligature.as_deref() {
                None => true,
                Some(p) => pattern_admits(p, want.as_bytes()[0]),
            }
        };
        let ends = |j: usize| j == 0 || j + 1 == wants.len();
        let options: Vec<Vec<usize>> = wants
            .iter()
            .zip(&typed.tiles)
            .enumerate()
            .map(|(j, (want, &chosen))| {
                let own = class_of[chosen];
                // Mid-word, a wildcard for a real letter changes only what the word spends, and on
                // a board with three bordered wildcards that alone doubled the ranking's time
                // (0.21s to 0.43s, measured 2026-11-06). The ends are where a gem wildcard pays.
                let real_slot = self.letters[chosen] != WILDCARD;
                if real_slot && !ends(j) {
                    return std::iter::once(own)
                        .chain((0..reps.len()).filter(|&c| {
                            c != own
                                && self.letters[reps[c]] != WILDCARD
                                && fits(reps[c], want, true)
                        }))
                        .collect();
                }
                std::iter::once(own)
                    .chain((0..reps.len()).filter(|&c| c != own && fits(reps[c], want, real_slot)))
                    .collect()
            })
            .collect();

        let own: Vec<usize> = typed.tiles.iter().map(|&i| class_of[i]).collect();
        let mut seen = std::collections::HashSet::from([self.score_key(&own, &wants)]);
        let mut out = vec![typed.clone()];
        let mut used = vec![0usize; reps.len()];
        let mut seq: Vec<usize> = Vec::with_capacity(wants.len());
        let mut walk =
            Walk { wants: &wants, options: &options, members: &members, seen: &mut seen };
        self.place(&mut walk, &mut used, &mut seq, &mut out);
        out
    }

    /// What a placement's worth depends on, as classes and the letter each slot reads as: every
    /// tile as a multiset, and which one is first and which last — the only positions a material
    /// pays in (see [`Typist::placements`]). Two placements with the same key score alike, so only
    /// the first is kept. Under the curse the order is adjacency, so the whole sequence is the key.
    fn score_key(&self, seq: &[usize], wants: &[String]) -> Vec<(usize, String)> {
        let pairs: Vec<(usize, String)> = seq.iter().copied().zip(wants.iter().cloned()).collect();
        if self.geometry.adjacency_required || pairs.len() < 3 {
            return pairs;
        }
        let (first, last) = (pairs[0].clone(), pairs[pairs.len() - 1].clone());
        let mut middle = pairs[1..pairs.len() - 1].to_vec();
        middle.sort();
        std::iter::once(first).chain(std::iter::once(last)).chain(middle).collect()
    }

    /// [`Typist::placements`]' depth-first walk over class sequences.
    fn place(
        &self, walk: &mut Walk, used: &mut [usize], seq: &mut Vec<usize>, out: &mut Vec<Typed>,
    ) {
        let (wants, options, members) = (walk.wants, walk.options, walk.members);
        if out.len() >= MAX_PLACEMENTS {
            return;
        }
        let j = seq.len();
        if j == wants.len() {
            if !walk.seen.insert(self.score_key(seq, wants)) {
                return;
            }
            // The classes become tiles in hand-out order, so the sequence typing chose rebuilds its
            // own placement — which is already first in `out`.
            let mut taken = vec![0usize; members.len()];
            let tiles: Vec<usize> = seq
                .iter()
                .map(|&c| {
                    taken[c] += 1;
                    members[c][taken[c] - 1]
                })
                .collect();
            if tiles == out[0].tiles {
                return;
            }
            let wildcards = tiles
                .iter()
                .zip(wants)
                .map(|(&i, w)| {
                    (self.letters[i] == WILDCARD)
                        .then(|| w.as_bytes()[0].to_ascii_lowercase() as char)
                })
                .collect();
            let corners_used = tiles.iter().filter(|&&i| self.geometry.is_corner(i)).count();
            let adjacent = tiles.windows(2).all(|p| self.geometry.tiles_adjacent(p[0], p[1]));
            if self.geometry.adjacency_required && !adjacent {
                return;
            }
            out.push(Typed { tiles, wildcards, corners_used, adjacent });
            return;
        }
        for &c in &options[j] {
            if used[c] >= members[c].len() {
                continue;
            }
            // Under the curse each class is one tile, so the chain can be pruned as it is built.
            if self.geometry.adjacency_required {
                if let Some(&prev) = seq.last() {
                    if !self.geometry.tiles_adjacent(members[prev][0], members[c][0]) {
                        continue;
                    }
                }
            }
            used[c] += 1;
            seq.push(c);
            self.place(walk, used, seq, out);
            seq.pop();
            used[c] -= 1;
        }
    }

    fn step(
        &self, c: u8, selected: &mut [bool], built: &mut Vec<Slot>, last: Option<usize>,
    ) -> Option<()> {
        // Clause 1: the previous tile plus this character naming a two-character ligature tile.
        if let Some(&prev) = built.last() {
            let mut key = self.slot_letters(prev);
            key.push(c as char);
            if let Some(t) = self.find_exact(&key, selected, last) {
                self.deselect(built, selected, 1);
                self.select(t, None, selected, built);
                self.clear_ephemeral(built);
                return Some(());
            }
        }
        // Clause 2: the two previous tiles plus this character -- the ING tile.
        if built.len() >= 2 {
            let mut key = self.slot_letters(built[built.len() - 2]);
            key.push_str(&self.slot_letters(built[built.len() - 1]));
            key.push(c as char);
            if let Some(t) = self.find_exact(&key, selected, last) {
                self.deselect(built, selected, 2);
                self.select(t, None, selected, built);
                self.clear_ephemeral(built);
                return Some(());
            }
        }
        // Clause 3: a plain tile, then wildcards.
        let one = (c as char).to_string();
        if let Some(t) = self.find_exact(&one, selected, last) {
            self.select(t, None, selected, built);
            return Some(());
        }
        if let Some(t) = self.find_restricted_wildcard(c, selected, last) {
            self.select(t, Some(c), selected, built);
            return Some(());
        }
        if let Some(t) = self.find_plain_wildcard(selected, last) {
            self.select(t, Some(c), selected, built);
            return Some(());
        }
        // Clause 4: a placeholder, valid only if some multi-letter tile could still absorb it.
        if self.letter_is_in_selectable_ligature(c, selected) {
            built.push(Slot::Ephemeral(c));
            return Some(());
        }
        None
    }

    fn slot_letters(&self, s: Slot) -> String {
        match s {
            Slot::Real(i) => self.letters[i].clone(),
            // A wildcard reads as the letter typed into it (`letterTemp`), not as '.'.
            Slot::Wild(_, c) | Slot::Ephemeral(c) => (c as char).to_string(),
        }
    }

    /// Removes the last `n` entries, releasing any real tiles they held.
    fn deselect(&self, built: &mut Vec<Slot>, selected: &mut [bool], n: usize) {
        for _ in 0..n {
            if let Some(s) = built.pop() {
                if let Slot::Real(i) | Slot::Wild(i, _) = s {
                    selected[i] = false;
                }
            }
        }
    }

    fn select(&self, tile: usize, typed: Option<u8>, selected: &mut [bool], built: &mut Vec<Slot>) {
        selected[tile] = true;
        built.push(match typed {
            Some(c) => Slot::Wild(tile, c),
            None => Slot::Real(tile),
        });
    }

    fn clear_ephemeral(&self, built: &mut Vec<Slot>) {
        built.retain(|s| !matches!(s, Slot::Ephemeral(_)));
    }

    /// `getUnselectedLegalTileWithLetter` with `allowPartial` unset: an exact letter match, corners
    /// tried first and then column-major, which is dump order.
    ///
    /// Under the chaining curse, only within the box around `last` — corners in it first, then the
    /// rest of it column-major, which is still dump order.
    fn find_exact(&self, letter: &str, selected: &[bool], last: Option<usize>) -> Option<usize> {
        for &i in &self.corner_first {
            if self.reachable(i, selected, last) && self.letters[i] == letter {
                return Some(i);
            }
        }
        (0..self.tiles.len())
            .find(|&i| self.reachable(i, selected, last) && self.letters[i] == letter)
    }

    /// `getUnselectedLegalRestrictedWildcardOfLetter` (`tileboard.lua:2216-2228`): a wildcard whose
    /// `extra.ligature` is a Lua pattern the typed letter matches. The patterns in the game are
    /// character classes such as `[AEIOU]` (`rpg/effects/ligature/`), so only that form and a plain
    /// literal are honoured; anything else is left alone rather than approximated.
    fn find_restricted_wildcard(
        &self, c: u8, selected: &[bool], last: Option<usize>,
    ) -> Option<usize> {
        (0..self.tiles.len()).find(|&i| {
            self.reachable(i, selected, last)
                && self.letters[i] == WILDCARD
                && self.tiles[i]
                    .quality
                    .ligature
                    .as_deref()
                    .map(|p| pattern_admits(p, c))
                    .unwrap_or(false)
        })
    }

    fn find_plain_wildcard(&self, selected: &[bool], last: Option<usize>) -> Option<usize> {
        (0..self.tiles.len()).find(|&i| {
            self.reachable(i, selected, last)
                && self.letters[i] == WILDCARD
                && self.tiles[i].quality.ligature.is_none()
        })
    }

    /// `tileboard.letterIsInSelectableLigatureTile` (`tileboard.lua:2182-2192`).
    fn letter_is_in_selectable_ligature(&self, c: u8, selected: &[bool]) -> bool {
        (0..self.tiles.len()).any(|i| {
            self.candidate(i, selected)
                && self.letters[i].len() > 1
                && self.letters[i].as_bytes().contains(&c)
        })
    }

    fn candidate(&self, i: usize, selected: &[bool]) -> bool {
        self.usable[i] && !selected[i]
    }

    /// [`Typist::candidate`], and inside the chaining curse's box around `last`: within one column
    /// and one row of index (`tileboard.lua:2273-2274`). The box, not true adjacency — on a
    /// hexagonal board typing can still pick a tile the submit check then refuses, as in the game.
    fn reachable(&self, i: usize, selected: &[bool], last: Option<usize>) -> bool {
        if !self.candidate(i, selected) {
            return false;
        }
        match last {
            None => true,
            Some(l) => match (self.geometry.position(l), self.geometry.position(i)) {
                (Some((c1, r1)), Some((c2, r2))) => c1.abs_diff(c2) <= 1 && r1.abs_diff(r2) <= 1,
                _ => false,
            },
        }
    }
}

/// Does a Lua pattern from `extra.ligature` admit this letter?
///
/// Deliberately narrow: `[AEIOU]`-style classes and plain literals only. A pattern this does not
/// understand returns false, so an unrecognised restricted wildcard is treated as unusable — which
/// costs a word we might have played, rather than claiming one we cannot.
fn pattern_admits(pattern: &str, c: u8) -> bool {
    let p = pattern.to_ascii_uppercase();
    if let Some(class) = p.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        return !class.contains('%') && !class.contains('-') && class.as_bytes().contains(&c);
    }
    p.len() == 1 && p.as_bytes()[0] == c
}

/// Wildcard tiles.
///
/// A wildcard is a blank tile that can stand for any letter. It reads as `.` in the board dump, and
/// the search already treats it correctly as a free letter — a live fight planned `DROPLETS` through
/// index 0 while index 0 printed as `.`. What is missing is the actuation.
///
/// ## Click it, then type — never type alone
///
/// The obvious shortcut is to skip the click and just type the letter. That is wrong, and the reason
/// is worth keeping: **if the letter you want already exists on the board, the ordinary tile wins**.
/// Type `E` while a real `E` is present and the game spends that one, not the wildcard. Items exist
/// that make *which* tile you spend matter, so this is a correctness problem rather than a
/// shortcut — the click is what selects the wildcard specifically, and the keystroke only decides
/// what it becomes.
///
/// There is a second typed path, found while implementing this and worth recording so it is not
/// re-discovered as a shortcut: `rpg.textinput` selects a wildcard for **any punctuation character**
/// (`rpg.lua:809-811`), `wordboard.select(tileboard.getUnselectedLegalTileWithLetter'.')`. That
/// avoids the click, but it takes whichever wildcard the game's own corner-first scan reaches first.
/// With two wildcards on the board and a reason to want the second — a corner, a material — there is
/// no way to say so. The click can. So the click stays.
///
/// ## The sequence
///
/// 1. Click the wildcard tile, as any other tile.
/// 2. Wait for the keyboard to appear. **This is the confirmation that the click landed**, and it
///    replaces the usual luminance check, which cannot work here: `rpg.showKeyboard` calls
///    `tileboard.hide'fade'` (`rpg.lua:695`), so every tile changes at once — that is exactly the
///    `clicking tile 0 also changed [1, 2, 4, ...]` failure below.
/// 3. Send the intended letter as text.
/// 4. Check for the keyboard fingerprint.
/// 5. **If the keyboard is still up, send the letter again.** Read → act → re-read, as everywhere
///    else; the check is the exit condition, not a formality.
///
/// Letters go out as **text**, not key events, on the same grounds as ordinary tile selection:
/// `keyboardOpen` is handled inside `rpg.textinput` (`rpg.lua:802-807`), which forwards to
/// `onscreenKeypress`. `love.keypressed` does not lead there.
///
/// ### Two cases where no keyboard appears, and neither is an error
///
/// `wordboard.select` only opens it when the tile has no letter yet **and** the player lacks the
/// `submitWildcardRegex` gear flag (`wordboard.lua:140-143`). With that gear a wildcard selects like
/// any other tile and the letter is decided at submission instead. So "clicked, no keyboard, tile is
/// selected" is a success, not a timeout — and it is distinguishable from a missed click, which
/// leaves the tile unselected too.
///
/// ### The retry has to be bounded
///
/// `onscreenKeypress` (`rpg.lua:663-672`) applies the tile's own `ligature` restriction before
/// accepting: a restricted wildcard silently ignores a letter its pattern does not admit, and the
/// keyboard stays up. Resending forever would hang the turn. A bounded retry turns that into a
/// reported failure, which is what it is — [`Typist`] should not have proposed the letter.
///
/// ## Recognising the screen
///
/// Choosing a wildcard's letter replaces the tile board with an on-screen QWERTY keyboard, so the
/// board fingerprint cannot be used — there is no board. [`KEYBOARD`] is the region measured from a
/// live capture at 1920x1080: bounded left by the `z` key, right by the `m` key, and top by the
/// keyboard's upper edge. It is filled almost entirely with opaque wooden keys, which is what makes
/// it stable against whatever fight scene is drawn behind.
///
/// The wildcard itself shows `[A-Z]` as a placeholder near the top of the screen while the keyboard
/// is up, which is a second signal if one is ever wanted.
///
/// ### Recognising it without a stored template
///
/// Do not template-match a captured keyboard. The game supports alternative keyboard layouts, so the
/// glyph on any given key is not fixed, and anything keyed on letter positions fails for a player on
/// a different layout. `ui/objects/keyboard.lua:5-17` carries five of them, and they differ in row
/// *width* as well as in glyphs — `qwerty`'s top row is 11 keys, `dvorak`'s is 8 — with each row
/// centred by `xOffset = -(#letterset/2)+(i-0.5)`. So neither the letters nor the key positions are
/// fixed; only "a wide block of wooden keys sits where the board was" is.
///
/// Compare the region against **itself** instead. Capture it once immediately before clicking the
/// wildcard — that is the tile board — and again after sending the letter. Back near the board
/// reference means the keyboard has gone; still far from it means the letter did not take and should
/// be sent again. Nothing is stored on disk, nothing depends on a layout, and the reference is
/// seconds old from the same session and the same window.
///
/// The two screens are easy to tell apart in that comparison because the keyboard is much the wider:
/// ten keys spanning roughly 1095 px against a tile board of about 520 px. Inside a 675 px region the
/// edges are keys in one case and plain background in the other, so the difference is concentrated
/// exactly where it is easiest to measure. An earlier draft of this note called for measuring key
/// pitch to tell wooden-squares-with-letters apart; that was more machinery than the problem needs.
///
/// ### Measured, not assumed
///
/// Two live captures of this exact region, one with the keyboard up and one taken a moment after the
/// wildcard was committed, scored with the same inlier metric the check will use
/// (`cargo run --bin inlier_probe`):
///
/// ```text
///        kb-region.png vs kb-region.png          inliers 1.0000
///        kb-region.png vs after-letter-kb.png    inliers 0.2592
///  after-letter-kb.png vs after-letter-kb.png    inliers 1.0000
/// ```
///
/// So keyboard against tile board is **0.26**, against a self-comparison control of 1.00. The two
/// screens are nowhere near each other and the threshold has a wide gap to sit in — around 0.5 leaves
/// room on both sides. The self-comparisons are the positive control: they prove the metric responds
/// at all, so 0.26 is a real separation rather than a broken measurement.
///
/// The board will not be pixel-identical afterwards — the wildcard has become a letter — so this is a
/// similarity judgement, not equality. Score it by inliers for the same reason everything else here
/// does: a partial mismatch should cost proportionally rather than fail outright.
///
/// ### The cursor sits in the middle of it
///
/// Clicking the wildcard leaves the pointer inside this region, and the game draws a cursor there.
/// Worse, we do not even control where it ends up: `showKeyboard` and `hideKeyboard` both finish with
/// `snapToNearestHotspot` (`rpg.lua:691`, `rpg.lua:711`), which warps the pointer onto whichever key
/// or tile is nearest. Parking beforehand therefore does not settle the question — the game moves it
/// back. Two independent reasons that is fine, and the check should keep both:
///
/// - **Match by inliers, not by hash.** [`crate::observe::template`] scores the *fraction* of
///   template pixels that agree, which is occlusion-robust by design — it was chosen so overworld
///   nodes still match under drifting cloud. A cursor removes its own pixels from the agreeing set
///   and nothing else; against a 675x295 region that is well under 1%, and the threshold sits at
///   0.55. An exact `Frame::region_hash` would NOT survive it, so this must not be built that way.
/// - **Nothing else in the region moves.** Both captures are of the same static screen furniture, so
///   the cursor is the only mover and it is a rounding error against 675x295.
///
/// ## What used to happen instead
///
/// [`Typist::type_word`] will happily plan a word through a wildcard, and `crate::combat::Board`
/// could not place it: clicking one changed nine tiles at once in a live fight
/// (`clicking tile 0 also changed [1, 2, 4, 5, 6, 9, 10, 12, 13]`) because the board was being
/// replaced by the keyboard underneath the check. The restless-tile filter cannot help — it samples
/// before the click and cannot predict a whole-screen change — so selection verification has to
/// expect this transition specifically, which is what
/// [`crate::combat::Board::select_word`] now does.
pub mod wildcard {
    /// The keyboard region, in client pixels at 1920x1080.
    ///
    /// `(x0, y0, x1, y1)`, measured from a live capture: `z`'s left edge, the keyboard's top edge,
    /// `m`'s right edge, and the bottom of the `z`-`m` row.
    pub const KEYBOARD: (i32, i32, i32, i32) = (578, 605, 1253, 900);

    /// Inlier score at or above which the region is showing what the reference showed.
    ///
    /// Placed in the measured gap: keyboard against tile board scored **0.26**, and either against
    /// itself **1.00**. Halfway leaves room for the cursor, the fade, and whatever the fight scene
    /// does behind the board, on both sides.
    pub const SAME: f64 = 0.55;

    /// Where the keyboard block is anchored, in normalized client coordinates.
    ///
    /// `xPos or 0.5` and `yPos = 0.7`, from the call site (`rpg.lua:685`) and the defaults
    /// (`ui/objects/keyboard.lua:44-45`). The game positions by a *fraction of the client* and then
    /// adds offsets in game units multiplied by the scale — it does not letterbox — which is the same
    /// model [`crate::win::window::button_center`] implements. Getting this wrong would only show on
    /// a non-16:9 window, where the region would drift off the keys and every check would read
    /// "changed".
    const ANCHOR: (f64, f64) = (0.5, 0.7);

    /// [`KEYBOARD`] as `(x, y, w, h)` for a given client size, clipped to it.
    pub fn region(client_w: i32, client_h: i32) -> (i32, i32, i32, i32) {
        let s = crate::layout::scale(client_w, client_h);
        let (x0, y0, x1, y1) = KEYBOARD;
        // The measured rectangle re-expressed as offsets from the anchor, at native size.
        let (ax, ay) = (1920.0 * ANCHOR.0, 1080.0 * ANCHOR.1);
        let x = (client_w as f64 * ANCHOR.0 + (x0 as f64 - ax) * s).round() as i32;
        let y = (client_h as f64 * ANCHOR.1 + (y0 as f64 - ay) * s).round() as i32;
        let w = ((x1 - x0) as f64 * s).round() as i32;
        let h = ((y1 - y0) as f64 * s).round() as i32;
        let x = x.clamp(0, client_w.max(1) - 1);
        let y = y.clamp(0, client_h.max(1) - 1);
        (x, y, w.min(client_w - x).max(1), h.min(client_h - y).max(1))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn at_native_size_the_region_is_the_measured_rectangle() {
            assert_eq!(region(1920, 1080), (578, 605, 675, 295));
        }

        #[test]
        fn it_scales_with_the_window() {
            // Half size: the anchor stays at the same fraction of the client and the offsets halve.
            // 540*0.7 = 378, plus (605-756)*0.5 = -75.5, which rounds away from zero to 303.
            assert_eq!(region(960, 540), (289, 303, 338, 148));
        }

        #[test]
        fn a_taller_window_moves_the_anchor_rather_than_stretching_the_region() {
            // 1920x1200: scale stays 1.0 (width-limited), but the anchor is 0.7 of 1200 = 840, so
            // the whole block sits 84px lower while keeping its size.
            let (x, y, w, h) = region(1920, 1200);
            assert_eq!((x, w, h), (578, 675, 295));
            assert_eq!(y, 605 + 84);
        }

        #[test]
        fn the_region_never_leaves_the_client_area() {
            // A window smaller than the region must still yield a capturable rectangle, because the
            // alternative is a BitBlt of a rectangle that is partly off-screen.
            let (x, y, w, h) = region(320, 200);
            assert!(x >= 0 && y >= 0 && w >= 1 && h >= 1);
            assert!(x + w <= 320 && y + h <= 200);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::save::{Table, Value};
    use crate::observe::board::Quality;

    fn plain(letters: &str) -> Vec<Tile> {
        letters.chars().map(|c| Tile::plain(&c.to_string())).collect()
    }

    fn tiles_of(strings: &[&str]) -> Vec<Tile> {
        strings.iter().map(|s| Tile::plain(s)).collect()
    }

    fn special(letter: &str, key: &str, value: Value) -> Tile {
        let mut extra = Table::default();
        extra.map.insert(key.into(), value);
        Tile { letter: letter.into(), quality: Quality::from_extra(&extra) }
    }

    /// A geometry with one column, so dump order is the only order and corners are trivial.
    fn strip(n: usize) -> Geometry {
        Geometry { rows_per_col: vec![n], corners: vec![(1, 1)], ..Geometry::default() }
    }

    fn typed(tiles: &[Tile], geom: &Geometry, word: &str) -> Option<Vec<usize>> {
        Typist::new(tiles, geom).type_word(word).map(|t| t.tiles)
    }

    /// A 4x4 board with the letter-chaining curse. Dump index is `(col-1)*4 + (row-1)`; `board`
    /// names all sixteen tiles in that order.
    fn chained(board: &str) -> (Vec<Tile>, Geometry) {
        let g = Geometry { adjacency_required: true, ..Geometry::default() };
        let t = if board.contains(',') {
            tiles_of(&board.split(',').collect::<Vec<_>>())
        } else {
            plain(board)
        };
        (t, g)
    }

    /// Typing only looks around the last tile (`tileboard.lua:2273-2274`): a word whose letters
    /// are on the board but not next to each other cannot be typed.
    #[test]
    fn the_chaining_curse_types_only_through_neighbours() {
        // C(1,1) A(1,2) T(1,3), and a second T far away at (4,4).
        let (t, g) = chained("CATQQQQQQQQQQQQT");
        assert_eq!(typed(&t, &g, "CAT"), Some(vec![0, 1, 2]));
        let w = Typist::new(&t, &g).type_word("CAT").unwrap();
        assert!(w.adjacent);
        // C(1,1) A(1,2), and the only T at (4,4).
        let (t, g) = chained("CAQQQQQQQQQQQQQT");
        assert_eq!(typed(&t, &g, "CAT"), None, "T is out of reach of A");
        let free = Geometry::default();
        let w = Typist::new(&t, &free).type_word("CAT").expect("no curse, any tile");
        assert!(!w.adjacent, "typeable without the curse, but not a chain");
    }

    /// The first letter is taken from the whole board, corners first, and greedily: when that
    /// choice strands the word, the game does not go back for another A.
    #[test]
    fn the_chaining_curse_is_greedy_about_the_first_letter() {
        // A in the corner (1,1), another A at (3,3), B at (4,4) — next to the second A only.
        let (t, g) = chained("AQQQQQQQQQAQQQQB");
        assert_eq!(typed(&t, &g, "AB"), None, "the corner A is taken, and B is not beside it");
        assert_eq!(typed(&t, &g, "BA"), Some(vec![15, 10]), "from B, the A beside it");
    }

    /// The wildcard lookups are confined too (`tileboard.lua:2216-2245`).
    #[test]
    fn the_chaining_curse_confines_wildcards() {
        // C(1,1) A(1,2) and a wildcard at (4,4).
        let (t, g) = chained("CAQQQQQQQQQQQQQ.");
        assert_eq!(typed(&t, &g, "CAT"), None);
        // ...and one at (2,3), beside A.
        let (t, g) = chained("CAQQQQ.QQQQQQQQQ");
        assert_eq!(typed(&t, &g, "CAT"), Some(vec![0, 1, 6]));
    }

    /// A ligature is looked up around the tile it replaces, before that tile is released, so it
    /// can land out of reach of the tile before — and the game refuses the word on submit
    /// (`wordRequirementAdjacent.lua`'s `tilesAreAllAdjacent`).
    #[test]
    fn the_chaining_curse_checks_the_whole_chain_after_a_ligature() {
        // A(1,1), T(2,2), TH(3,3): typing A, T, H swaps T for TH, leaving A and TH apart.
        let board = "A,Q,Q,Q,Q,T,Q,Q,Q,Q,TH,Q,Q,Q,Q,Q";
        let (t, g) = chained(board);
        assert_eq!(typed(&t, &g, "ATH"), None);
        // With TH beside A as well, at (2,1), the swap keeps the chain.
        let (t, g) = chained("A,Q,Q,Q,TH,T,Q,Q,Q,Q,Q,Q,Q,Q,Q,Q");
        assert_eq!(typed(&t, &g, "ATH"), Some(vec![0, 4]));
    }

    #[test]
    fn a_plain_word_takes_the_letters_it_names() {
        let t = plain("CAT");
        let g = strip(3);
        assert_eq!(typed(&t, &g, "CAT"), Some(vec![0, 1, 2]));
        assert_eq!(typed(&t, &g, "ACT"), Some(vec![1, 0, 2]), "selection follows the word");
        assert_eq!(typed(&t, &g, "CATS"), None, "no S on the board");
        assert_eq!(typed(&t, &g, "AA"), None, "only one A");
    }

    #[test]
    fn corners_are_taken_before_other_tiles_with_the_same_letter() {
        // THE central fact for `resistCornerless`: the game scans corners first
        // (`tileboard.lua:2276-2283`), so a duplicated letter is drawn from the corner. A model that
        // took the first tile in dump order would under-count corners and under-rate every word.
        let t = plain("AAAAAAAAAAAAAAAA");
        let g = Geometry::default();
        let out = Typist::new(&t, &g).type_word("AA").unwrap();
        assert_eq!(out.tiles, vec![0, 12], "corner (1,1) then corner (4,1)");
        assert_eq!(out.corners_used, 2);
    }

    #[test]
    fn a_word_can_miss_the_corners_entirely() {
        // The zero case, and the reason `resistCornerless` is dangerous rather than merely
        // inconvenient: nerf = 0/4 multiplies the damage to nothing. Corners here are indices
        // 0, 12, 3, 15 -- given Q/Z/X/J, so a word of A and B cannot touch one.
        let t = plain("QAABZAAAAAAAXAAJ");
        let g = Geometry::default();
        assert_eq!(
            g.corner_indices().iter().map(|&i| t[i].letter.as_str()).collect::<Vec<_>>(),
            ["Q", "X", "B", "J"],
            "the board really does keep A off every corner"
        );
        let out = Typist::new(&t, &g).type_word("AA").unwrap();
        assert_eq!(out.corners_used, 0);
        assert!(!out.tiles.iter().any(|&i| g.is_corner(i)), "tiles: {:?}", out.tiles);

        // And the contrast: a word that names a corner letter is drawn from the corner.
        let with_corner = Typist::new(&t, &g).type_word("BAA").unwrap();
        assert_eq!(with_corner.corners_used, 1, "the B at index 3 is corner (1,4)");
    }

    #[test]
    fn a_ligature_tile_cannot_be_split() {
        // A TH tile supplies T and H only together. Counting it as a bare T would find words the
        // board can never play.
        let t = tiles_of(&["TH", "E", "N"]);
        let g = strip(3);
        assert_eq!(typed(&t, &g, "THEN"), Some(vec![0, 1, 2]));
        assert_eq!(typed(&t, &g, "TEN"), None, "the T is locked inside TH");
        assert_eq!(typed(&t, &g, "HEN"), None);
    }

    #[test]
    fn typing_into_a_ligature_uses_the_placeholder_then_completes_it() {
        // `rpg.lua:853-858` selects an ephemeral placeholder for the T, and `rpg.lua:816-825`
        // replaces it when the H arrives. Without the placeholder step, TH would be untypeable.
        let t = tiles_of(&["TH", "E"]);
        let g = strip(2);
        assert_eq!(typed(&t, &g, "THE"), Some(vec![0, 1]), "two tiles for a three-letter word");
    }

    #[test]
    fn a_dangling_placeholder_is_not_a_word() {
        // Typing T alone leaves an ephemeral tile, and `wordboard.getWord` returns '' for that --
        // there is nothing to submit.
        let t = tiles_of(&["TH", "E"]);
        let g = strip(2);
        assert_eq!(typed(&t, &g, "TE"), None, "the T never resolves to a tile");
    }

    #[test]
    fn a_completed_ligature_does_not_excuse_an_earlier_dangling_placeholder() {
        // The board that ended the run of 2026-08-11, verbatim from the console, and the word the
        // search chose. `UNEQUIVOCALLY` needs two U's; the board's only U is locked inside the QU
        // tile, so it is unspellable — but every step still succeeded:
        //
        //   U -> no U tile; clause 4 parks a placeholder, because QU *contains* a U
        //   N, E -> plain tiles
        //   Q -> no Q tile either; clause 4 parks a second placeholder
        //   U -> clause 1 completes "Q"+"U" as the QU tile and calls `clear_ephemeral`,
        //        which sweeps BOTH placeholders — including the leading U, three steps back
        //
        // That is faithful to the game: `rpg.lua:824` calls `wordboard.clearEphemeralTiles`, and
        // `wordboard.lua:86-93` removes every ephemeral in `wordTiles`, not just the absorbed one.
        // So the guard on leftover placeholders cannot catch this — by the time it looks, there are
        // none. What came back was a real, typeable selection that spells NEQUIVOCALLY, which the
        // game's own `getWord` would hand over and no dictionary would accept. The fight stalled
        // there and the run ended.
        let t = tiles_of(&[
            "QU", "E", "R", "L", "W", "N", "A", "A", "I", "C", "L", "Y", "L", "L", "O", "V",
        ]);
        let g = strip(16);
        assert_eq!(typed(&t, &g, "UNEQUIVOCALLY"), None, "the board has one U and it is inside QU");

        // The negative control, so this cannot be passed by refusing ligatures outright: the same
        // board still plays a word that genuinely uses the QU tile.
        assert!(typed(&t, &g, "QUILL").is_some(), "QU-I-L-L is spellable and must stay typeable");
    }

    #[test]
    fn the_ligature_clause_beats_two_single_tiles() {
        // Clause 1 fires before clause 3, so typing T then H takes the TH tile and RELEASES the T
        // it had just selected. A segmenter that merely checked availability would report the word
        // as using three tiles when the game uses two -- different score, different corner count.
        let t = tiles_of(&["T", "H", "TH", "E"]);
        let g = strip(4);
        assert_eq!(typed(&t, &g, "THE"), Some(vec![2, 3]), "took the TH ligature, freeing the T");
    }

    #[test]
    fn a_three_letter_ligature_is_absorbed_by_the_second_clause() {
        let t = tiles_of(&["K", "I", "N", "ING", "S"]);
        let g = strip(5);
        // K-I-N-G: I and N select singly, then G triggers the ING clause and frees them.
        assert_eq!(typed(&t, &g, "KINGS"), Some(vec![0, 3, 4]));
    }

    #[test]
    fn greedy_absorption_is_self_correcting_when_the_ligature_is_used_up() {
        // Once the AB tile is selected, clause 1 stops firing and the single tiles are used --
        // which is why the game needs no backtracking and neither does this.
        let t = tiles_of(&["AB", "A", "B"]);
        let g = strip(3);
        assert_eq!(typed(&t, &g, "ABAB"), Some(vec![0, 1, 2]));
    }

    #[test]
    fn an_unselectable_tile_is_not_available() {
        let mut t = plain("AB");
        t[1] = special("B", "unselectable", Value::Int(2));
        let g = strip(2);
        assert_eq!(typed(&t, &g, "AB"), None, "B is unselectable");
        assert_eq!(typed(&t, &g, "A"), Some(vec![0]));
    }

    #[test]
    fn a_locked_column_removes_its_tiles() {
        let t = plain("AAAABBBB");
        let mut g =
            Geometry { rows_per_col: vec![4, 4], corners: vec![(1, 1)], ..Geometry::default() };
        g.locked_cols.insert(2);
        assert_eq!(typed(&t, &g, "AB"), None, "every B sits in the locked column");
        assert_eq!(typed(&t, &g, "AA"), Some(vec![0, 1]));
    }

    #[test]
    fn a_plain_wildcard_takes_any_letter() {
        let t = tiles_of(&["A", "B", "."]);
        let g = strip(3);
        assert_eq!(typed(&t, &g, "ABC"), Some(vec![0, 1, 2]));
        assert_eq!(typed(&t, &g, "ABCD"), None, "one wildcard cannot cover two missing letters");
    }

    #[test]
    fn the_letter_for_each_wildcard_comes_back_aligned_with_its_tile() {
        // The clicker walks `tiles` and `wildcards` together, so a misalignment would type a
        // letter into the wrong tile -- or type nothing and leave the keyboard up, stalling the
        // fight. Ordinary tiles must carry `None`, not a placeholder.
        let t = tiles_of(&["A", "B", "."]);
        let g = strip(3);
        let typed = Typist::new(&t, &g).type_word("ABC").unwrap();
        assert_eq!(typed.tiles, vec![0, 1, 2]);
        assert_eq!(typed.wildcards, vec![None, None, Some('c')]);
        assert!(typed.uses_a_wildcard());
        assert_eq!(typed.steps().collect::<Vec<_>>(), vec![(0, None), (1, None), (2, Some('c'))]);
    }

    #[test]
    fn a_word_with_no_wildcard_asks_for_no_typing_at_all() {
        let t = tiles_of(&["A", "B"]);
        let typed = Typist::new(&t, &strip(2)).type_word("AB").unwrap();
        assert!(!typed.uses_a_wildcard());
        assert_eq!(typed.wildcards, vec![None, None]);
    }

    #[test]
    fn a_wildcard_dropped_by_a_ligature_does_not_leave_a_letter_behind() {
        // Clause 1 undoes the previous selection when a ligature tile wins. If `wildcards` were
        // built from anything but the final `built`, the abandoned wildcard's letter would survive
        // into the plan and be typed at a tile that is not a wildcard.
        let t = tiles_of(&["T", "TH", "."]);
        let g = strip(3);
        let typed = Typist::new(&t, &g).type_word("TH").unwrap();
        assert_eq!(typed.tiles, vec![1], "the TH tile, not T plus a wildcard");
        assert_eq!(typed.wildcards, vec![None]);
    }

    #[test]
    fn a_restricted_wildcard_only_takes_letters_its_pattern_admits() {
        // `getUnselectedLegalRestrictedWildcardOfLetter` matches the typed letter against
        // `extra.ligature`, and the game's patterns are classes like `[AEIOU]`.
        let mut t = tiles_of(&["B", "."]);
        t[1] = special(".", "ligature", Value::Str("[AEIOU]".into()));
        let g = strip(2);
        assert_eq!(typed(&t, &g, "BE"), Some(vec![0, 1]), "E is in the class");
        assert_eq!(typed(&t, &g, "BZ"), None, "Z is not, and there is no plain wildcard");
    }

    #[test]
    fn a_wildcard_reads_as_the_letter_typed_into_it() {
        // `letterTemp` is what the ligature clauses compare against (`rpg.lua:819`), so a wildcard
        // standing in for T can complete a TH tile.
        let t = tiles_of(&[".", "TH"]);
        let g = strip(2);
        // Typing T takes the wildcard; typing H then finds "TH" and swaps to the real tile.
        assert_eq!(typed(&t, &g, "TH"), Some(vec![1]));
    }

    /// Typing takes the first B in scan order; a click can start the word on the other one, and
    /// that is what [`Typist::placements`] offers second. The user's `BREAKABLY`, 2026-11-06.
    #[test]
    fn placements_offer_the_other_twin_first() {
        let mut t = plain("BOB");
        t[2].quality.material = Some("silvertoxin".into());
        let g = strip(3);
        let all = Typist::new(&t, &g).placements("BO");
        assert_eq!(all[0], Typist::new(&t, &g).type_word("BO").unwrap(), "typing's own first");
        assert_eq!(
            all.iter().map(|p| p.tiles.clone()).collect::<Vec<_>>(),
            [vec![0, 1], vec![2, 1]]
        );
        // Both Bs in the word: either may start it.
        let both: Vec<Vec<usize>> =
            Typist::new(&t, &g).placements("BOB").into_iter().map(|p| p.tiles).collect();
        assert_eq!(both, [vec![0, 1, 2], vec![2, 1, 0]]);
    }

    /// Twins — same letter, same quality, same corner standing — score alike wherever they go, so
    /// they are not permuted: sixteen plain As give only corner-or-not per slot.
    #[test]
    fn placements_do_not_permute_twins() {
        let t = plain("AAAAAAAAAAAAAAAA");
        let g = Geometry::default();
        let all = Typist::new(&t, &g).placements("AA");
        assert_eq!(all.len(), 4, "{all:?}");
        assert_eq!(all[0].tiles, vec![0, 12], "typing's corners first");
        assert_eq!(all.iter().map(|p| p.corners_used).collect::<Vec<_>>(), [2, 1, 1, 0]);
    }

    /// A special wildcard can stand in for a letter typing found a real tile for, and is then typed
    /// into; a plain one is not offered, since it is worth nothing and costs a hoarded tile.
    #[test]
    fn placements_can_use_a_special_wildcard_instead() {
        let mut t = plain("A..");
        t[2].quality.material = Some("gemfire".into());
        let g = strip(3);
        let all = Typist::new(&t, &g).placements("A");
        assert_eq!(all.len(), 2, "the gem wildcard, never the plain one: {all:?}");
        assert_eq!(all[1].tiles, vec![2]);
        assert_eq!(all[1].wildcards, vec![Some('a')]);
    }
}
