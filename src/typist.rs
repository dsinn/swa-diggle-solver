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
    /// Parallel to [`Typed::tiles`]: the letter that must be typed into that tile because it is a
    /// wildcard, and `None` for an ordinary tile where the click is the whole action.
    ///
    /// Kept here rather than recovered later because only the simulation knows it. A wildcard's
    /// *letter* is not a property of the board — the same blank tile becomes a different letter in
    /// every candidate word — so by the time an index reaches the clicker the information is gone.
    pub wildcards: Vec<Option<char>>,
    /// How many of them are corner tiles — the numerator of the `resistCornerless` nerf.
    pub corners_used: usize,
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
        Some(Typed { tiles, wildcards, corners_used })
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
}
