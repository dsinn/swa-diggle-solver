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
//! ## What this does not model
//!
//! `wordRequirementAdjacent` gear restricts each pick to the 3×3 neighbourhood of the last
//! selection ([`crate::geometry::Geometry::adjacency_required`]). Callers must check that flag; this
//! types as if the whole board were reachable, which is true without that gear and wrong with it.

use crate::geometry::Geometry;
use crate::observe::board::Tile;

/// The game's wildcard tile.
const WILDCARD: &str = ".";

/// What typing a word actually consumed.
#[derive(Debug, Clone, PartialEq)]
pub struct Typed {
    /// Dump indices of the tiles the word used, in selection order.
    pub tiles: Vec<usize>,
    /// How many of them are corner tiles — the numerator of the `resistCornerless` nerf.
    pub corners_used: usize,
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

        for &c in chars {
            if self.step(c, &mut selected, &mut built).is_none() {
                return None;
            }
        }

        // A leftover placeholder means `getWord` yields '' -- the word cannot be submitted.
        if built.iter().any(|s| matches!(s, Slot::Ephemeral(_))) {
            return None;
        }

        let tiles: Vec<usize> = built
            .iter()
            .map(|s| match s {
                Slot::Real(i) | Slot::Wild(i, _) => *i,
                Slot::Ephemeral(_) => unreachable!("checked above"),
            })
            .collect();
        let corners_used = tiles.iter().filter(|&&i| self.geometry.is_corner(i)).count();
        Some(Typed { tiles, corners_used })
    }

    fn step(&self, c: u8, selected: &mut [bool], built: &mut Vec<Slot>) -> Option<()> {
        // Clause 1: the previous tile plus this character naming a two-character ligature tile.
        if let Some(&prev) = built.last() {
            let mut key = self.slot_letters(prev);
            key.push(c as char);
            if let Some(t) = self.find_exact(&key, selected) {
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
            if let Some(t) = self.find_exact(&key, selected) {
                self.deselect(built, selected, 2);
                self.select(t, None, selected, built);
                self.clear_ephemeral(built);
                return Some(());
            }
        }
        // Clause 3: a plain tile, then wildcards.
        let one = (c as char).to_string();
        if let Some(t) = self.find_exact(&one, selected) {
            self.select(t, None, selected, built);
            return Some(());
        }
        if let Some(t) = self.find_restricted_wildcard(c, selected) {
            self.select(t, Some(c), selected, built);
            return Some(());
        }
        if let Some(t) = self.find_plain_wildcard(selected) {
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
    fn find_exact(&self, letter: &str, selected: &[bool]) -> Option<usize> {
        for &i in &self.corner_first {
            if self.candidate(i, selected) && self.letters[i] == letter {
                return Some(i);
            }
        }
        (0..self.tiles.len()).find(|&i| self.candidate(i, selected) && self.letters[i] == letter)
    }

    /// `getUnselectedLegalRestrictedWildcardOfLetter` (`tileboard.lua:2216-2228`): a wildcard whose
    /// `extra.ligature` is a Lua pattern the typed letter matches. The patterns in the game are
    /// character classes such as `[AEIOU]` (`rpg/effects/ligature/`), so only that form and a plain
    /// literal are honoured; anything else is left alone rather than approximated.
    fn find_restricted_wildcard(&self, c: u8, selected: &[bool]) -> Option<usize> {
        (0..self.tiles.len()).find(|&i| {
            self.candidate(i, selected)
                && self.letters[i] == WILDCARD
                && self.tiles[i]
                    .quality
                    .ligature
                    .as_deref()
                    .map(|p| pattern_admits(p, c))
                    .unwrap_or(false)
        })
    }

    fn find_plain_wildcard(&self, selected: &[bool]) -> Option<usize> {
        (0..self.tiles.len()).find(|&i| {
            self.candidate(i, selected)
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

/// Wildcard tiles — SPEC, not yet implemented.
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
/// ## The sequence
///
/// 1. Click the wildcard tile, as any other tile.
/// 2. Send the intended letter as a keystroke.
/// 3. Check for the keyboard fingerprint.
/// 4. **If the keyboard is still up, send the letter again.** Read → act → re-read, as everywhere
///    else; the check is the exit condition, not a formality.
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
/// a different layout.
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
/// The board will not be pixel-identical afterwards — the wildcard has become a letter — so this is a
/// similarity judgement, not equality. Score it by inliers for the same reason everything else here
/// does: a partial mismatch should cost proportionally rather than fail outright.
///
/// ### The cursor sits in the middle of it
///
/// Clicking the wildcard leaves the pointer inside this region, and the game draws a cursor there.
/// Two independent reasons that is fine, and the check should keep both:
///
/// - **Match by inliers, not by hash.** [`crate::observe::template`] scores the *fraction* of
///   template pixels that agree, which is occlusion-robust by design — it was chosen so overworld
///   nodes still match under drifting cloud. A cursor removes its own pixels from the agreeing set
///   and nothing else; against a 675x295 region that is well under 1%, and the threshold sits at
///   0.55. An exact `Frame::region_hash` would NOT survive it, so this must not be built that way.
/// - **Park first anyway.** `Run::park` moves the pointer to (760, 240), which is outside this
///   rectangle, and costs nothing. Belt and braces: the parking makes it not arise, the inlier
///   scoring makes it harmless if parking is ever missed or the pointer is warped back by the game's
///   own hotspot navigation.
///
/// ## Why this blocks a fight today
///
/// [`Typist::type_word`] will happily plan a word through a wildcard, and `crate::combat::Board`
/// cannot place it: clicking one changed nine tiles at once in a live fight
/// (`clicking tile 0 also changed [1, 2, 4, 5, 6, 9, 10, 12, 13]`) because the board was being
/// replaced by the keyboard underneath the check. The restless-tile filter cannot help — it samples
/// before the click and cannot predict a whole-screen change — so selection verification has to
/// expect this transition specifically.
pub mod wildcard {
    /// The keyboard region, in client pixels at 1920x1080.
    ///
    /// `(x0, y0, x1, y1)`, measured from a live capture: `z`'s left edge, the keyboard's top edge,
    /// `m`'s right edge, and the bottom of the `z`-`m` row. Scale with
    /// [`crate::layout::scale`] for other window sizes.
    pub const KEYBOARD: (i32, i32, i32, i32) = (578, 605, 1253, 900);
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
        Geometry {
            rows_per_col: vec![n],
            corners: vec![(1, 1)],
            ..Geometry::default()
        }
    }

    fn typed(tiles: &[Tile], geom: &Geometry, word: &str) -> Option<Vec<usize>> {
        Typist::new(tiles, geom).type_word(word).map(|t| t.tiles)
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
        let mut g = Geometry { rows_per_col: vec![4, 4], corners: vec![(1, 1)], ..Geometry::default() };
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
}
