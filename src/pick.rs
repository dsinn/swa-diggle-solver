//! Choosing *which* killing word to play, when more than one kills.
//!
//! [`crate::search::Goal::FirstKill`] took whichever lethal word a thread happened to find first.
//! That is the right answer to "end the exchange" and the wrong answer to "play the fight", because
//! the word also decides what the board looks like afterwards and what the turn is worth beyond the
//! damage. Three things separate two words that both kill:
//!
//! 1. **A wood-only kill pays out**, when the gear for it is held and the payout is not wasted.
//! 2. **Hazard tiles want to reach the bottom.** `tileboard.lua:178` is the game telling the player
//!    so directly: *"I'm sorry. Your special tile got unlucky. Get it to the bottom to get rid of
//!    it."* A `!` tile *"falls off if it's at the bottom of the board at the start of your turn"*
//!    (`:180`), so clearing tiles beneath one is how it leaves.
//! 3. **What is left should look like a board you can play.** See [`crate::letters`].
//!
//! In that order, and the order is the point: 1 and 2 are payouts, 3 is hygiene. A tiebreak never
//! outranks a payout, and none of the three ever outranks killing.
//!
//! ## Everything here is a tiebreak among words that already kill
//!
//! None of this decides *whether* to kill. A candidate that does not kill never reaches these
//! functions, and no amount of tidy letters or falling hazards makes a non-lethal word preferable to
//! a lethal one.

use crate::letters::{self, Target, ALPHABET};
use crate::observe::board::Tile;
use crate::score::Scorer;

/// The material prefix that counts as wood.
///
/// A prefix rather than an equality test because the Targe family introduces **braced wood**
/// (`tileRefillSpecialWoodbraced`), which is wood for the purposes of a wood-only kill and is not
/// spelled `wood`. Matching the prefix covers both and any further wood variant the game adds.
const WOOD: &str = "wood";

/// The material a bomb tile carries (`rpg/effects/material/bomb.lua`).
const BOMB: &str = "bomb";

/// What the player's gear and condition make worth chasing this turn.
///
/// Derived once per turn rather than per candidate: it depends on the save, not on the word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Preferences {
    /// Is a wood-only kill worth constraining the word for?
    pub wood_only_pays: bool,
}

impl Preferences {
    /// Reads the two buckler behaviours out of `gearFlags`, and decides whether either is worth
    /// anything right now.
    ///
    /// **Flags, not item names.** Both bucklers set `onWoodKillGainArmour` and `onWoodKillGainHealth`
    /// four times each (`items/woodaugmentgear.lua:154`, `items/idoltarge.lua:4`), and flags are what
    /// the save carries. The Targe is deliberately *not* included: it sets only
    /// `tileRefillSpecialWoodbraced` / `tileQueueSpecialWoodbraced` and pays for **ending a word on a
    /// braced wooden tile**, which is a different objective and not modelled here.
    ///
    /// ## Why full health switches the plain buckler off
    ///
    /// The Wooden idol buckler's payout is *"heals you for an additional 4, unless you're bleeding,
    /// and gives you 4 armour"*. At full health the heal is thrown away, and constraining the word to
    /// wood costs damage — so it stops being worth chasing.
    ///
    /// The Braced buckler keeps its value regardless, because it also carries
    /// `onWoodKillQueueWoodbraced`: it queues a braced wooden tile on the kill, and that does not
    /// care how much health is missing. That flag is the only thing separating the two in the save.
    pub fn from_flags(has_flag: impl Fn(&str) -> bool, injured: bool) -> Self {
        let wood_kill_gear = has_flag("onWoodKillGainHealth") || has_flag("onWoodKillGainArmour");
        let pays_regardless = has_flag("onWoodKillQueueWoodbraced");
        Preferences { wood_only_pays: wood_kill_gear && (injured || pays_regardless) }
    }
}

/// How good a lethal candidate is, beyond the fact that it kills.
///
/// Compared in field order: wood-only first, then hazard fall, then deviation. Not `Ord`, because
/// `deviation` is an `f64` and a total order over floats would be a lie — [`Rank::better_than`] is
/// the comparison, and it is deliberately the only one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rank {
    /// Every tile used was wood, and the gear pays for that. Higher is better.
    pub wood_only: bool,
    /// Total slots every hazard tile on the board would fall by. Higher is better.
    pub hazard_fall: usize,
    /// Distance of the resulting board from its target letter distribution. **Lower** is better.
    pub deviation: f64,
}

impl Rank {
    /// Strictly better, by the stated priority order.
    ///
    /// The two payouts are compared before the hygiene term, and equality on both is what lets
    /// deviation decide. Written as an explicit chain rather than a tuple compare so that the
    /// direction of each term is visible at the point it is applied — `deviation` runs the other way
    /// from the other two, which is exactly the kind of thing a tuple hides.
    pub fn better_than(&self, other: &Rank) -> bool {
        if self.wood_only != other.wood_only {
            return self.wood_only;
        }
        if self.hazard_fall != other.hazard_fall {
            return self.hazard_fall > other.hazard_fall;
        }
        self.deviation < other.deviation
    }
}

/// Is this tile a hazard that wants to reach the bottom of its column?
///
/// Two kinds, named separately by the request and both true here:
///
/// - **Exclamation** — letter `!`, *"Unselectable tile. Falls off if it's at the bottom of the board
///   at the start of your turn"* (`tileboard.lua:180`).
/// - **Bomb** — material `bomb`, *"Inert bomb tile; will explode and destroy edge touching tiles if
///   detonated by something else"* (`rpg/effects/material/bomb.lua:12-13`).
///
/// Digit tiles are deliberately excluded even though `tileboard.lua:181` says a number *"acts like a
/// ! tile"*. They leave by counting down to zero and popping, not by reaching the bottom, so falling
/// does not get rid of one and counting it here would reward a move that achieves nothing.
pub fn is_hazard(tile: &Tile, scorer: &Scorer) -> bool {
    tile.letter == "!" || scorer.material_name(tile).is_some_and(|m| m == BOMB)
}

/// Total number of slots the board's hazard tiles would fall, if `consumed` were removed.
///
/// A tile falls by the number of removed tiles **below** it in its own column. `layout.rs:55` fixes
/// the direction: *"Column-major with row 1 at the bottom"*, so "below" is a strictly smaller row.
///
/// Summed over hazards rather than maximised over them, which is what makes two hazards in one
/// column worth twice as much as one — removing a single tile under both moves both, and that turn
/// has done twice the work.
///
/// A hazard that is itself consumed contributes nothing: it is gone, and a gone tile cannot fall.
/// That understates such a move rather than overstating it, which is the safe direction — the tile
/// left the board, which is the outcome the falling was for.
pub fn hazard_fall(
    tiles: &[Tile], geometry: &crate::geometry::Geometry, consumed: &[usize], scorer: &Scorer,
) -> usize {
    // Positions of everything being removed, so each hazard is one pass over a small list.
    let removed: Vec<(usize, usize)> =
        consumed.iter().filter_map(|&i| geometry.position(i)).collect();
    let mut total = 0;
    for (i, tile) in tiles.iter().enumerate() {
        if !is_hazard(tile, scorer) || consumed.contains(&i) {
            continue;
        }
        let Some((col, row)) = geometry.position(i) else { continue };
        total += removed.iter().filter(|&&(c, r)| c == col && r < row).count();
    }
    total
}

/// Does this word use nothing but wood?
///
/// Empty is not wood-only: a word that used no tiles could not have killed anything, and answering
/// `true` for it would make an impossible candidate outrank every real one.
pub fn wood_only(tiles: &[Tile], consumed: &[usize], scorer: &Scorer) -> bool {
    !consumed.is_empty()
        && consumed.iter().all(|&i| {
            tiles.get(i).and_then(|t| scorer.material_name(t)).is_some_and(|m| m.starts_with(WOOD))
        })
}

/// Letter counts of what would be left standing.
pub fn remaining_counts(tiles: &[Tile], consumed: &[usize]) -> [usize; ALPHABET] {
    letters::counts_of(
        tiles
            .iter()
            .enumerate()
            .filter(|(i, _)| !consumed.contains(i))
            .map(|(_, t)| t.letter.as_str()),
    )
}

/// The full ranking for one candidate.
pub fn rank(
    tiles: &[Tile], geometry: &crate::geometry::Geometry, consumed: &[usize], scorer: &Scorer,
    target: &Target, prefs: Preferences,
) -> Rank {
    Rank {
        wood_only: prefs.wood_only_pays && wood_only(tiles, consumed, scorer),
        hazard_fall: hazard_fall(tiles, geometry, consumed, scorer),
        deviation: target.deviation(&remaining_counts(tiles, consumed)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Geometry;
    use std::path::PathBuf;

    fn scorer() -> Option<Scorer> {
        let dir = PathBuf::from("../sternly-worded-adventures");
        if !dir.join("rpg/effects/material/default.lua").is_file() {
            eprintln!("SKIP: game source not present");
            return None;
        }
        Some(Scorer::new(&dir).expect("scoring tables load"))
    }

    /// Two columns of three. Flat indices are column-major with row 1 at the bottom
    /// (`layout.rs:55`), so column 1 is 0,1,2 bottom-to-top and column 2 is 3,4,5.
    fn two_columns_of_three() -> Geometry {
        Geometry { rows_per_col: vec![3, 3], ..Default::default() }
    }

    fn board(letters: &[&str]) -> Vec<Tile> {
        letters.iter().map(|l| Tile::plain(l)).collect()
    }

    /// The case from the request, verbatim: one column holding two exclamation tiles, and a
    /// candidate that removes a single tile beneath both. Each hazard moves down one slot, so the
    /// move is worth **2** rather than 1.
    #[test]
    fn two_hazards_over_one_removed_tile_count_twice() {
        let Some(sc) = scorer() else { return };
        let g = two_columns_of_three();
        // Column 1, bottom to top: E, !, ! — so removing the E drops both hazards one slot each.
        let tiles = board(&["E", "!", "!", "A", "B", "C"]);
        assert_eq!(hazard_fall(&tiles, &g, &[0], &sc), 2);
    }

    #[test]
    fn only_tiles_below_a_hazard_in_its_own_column_move_it() {
        let Some(sc) = scorer() else { return };
        let g = two_columns_of_three();
        // Column 1: E, !, R. Column 2: A, B, C.
        let tiles = board(&["E", "!", "R", "A", "B", "C"]);
        // Below it, same column: the hazard falls one.
        assert_eq!(hazard_fall(&tiles, &g, &[0], &sc), 1);
        // Above it, same column: nothing moves.
        assert_eq!(hazard_fall(&tiles, &g, &[2], &sc), 0);
        // A whole other column: nothing moves.
        assert_eq!(hazard_fall(&tiles, &g, &[3, 4, 5], &sc), 0);
        // Both tiles under it would be two slots, but only one is under it.
        assert_eq!(hazard_fall(&tiles, &g, &[0, 2], &sc), 1);
    }

    #[test]
    fn a_board_with_no_hazards_scores_zero_however_much_is_removed() {
        let Some(sc) = scorer() else { return };
        let g = two_columns_of_three();
        let tiles = board(&["E", "A", "R", "T", "H", "S"]);
        assert_eq!(hazard_fall(&tiles, &g, &[0, 1, 2, 3, 4, 5], &sc), 0);
    }

    /// Wood is the material of ordinary letters, so this has to be checked through the scorer rather
    /// than through `quality.material`, which is `None` on almost every tile.
    #[test]
    fn wood_only_reads_the_letter_implied_material_not_just_an_explicit_one() {
        let Some(sc) = scorer() else { return };
        let tiles = board(&["E", "A", "R"]);
        let all_wood = tiles.iter().all(|t| sc.material_name(t).is_some_and(|m| m.starts_with(WOOD)));
        // If this is false the fixture is wrong, not the code -- and the assertion below would then
        // be passing for the wrong reason.
        assert!(all_wood, "expected plain letters to resolve to wood: {:?}",
            tiles.iter().map(|t| sc.material_name(t)).collect::<Vec<_>>());
        assert!(wood_only(&tiles, &[0, 1, 2], &sc));
        assert!(!wood_only(&tiles, &[], &sc), "an empty word cannot have killed anything");
    }

    #[test]
    fn the_plain_buckler_stops_mattering_at_full_health_and_the_braced_one_does_not() {
        let plain = |f: &str| f == "onWoodKillGainHealth" || f == "onWoodKillGainArmour";
        let braced = |f: &str| plain(f) || f == "onWoodKillQueueWoodbraced";

        // Injured: both pay, because the heal lands.
        assert!(Preferences::from_flags(plain, true).wood_only_pays);
        assert!(Preferences::from_flags(braced, true).wood_only_pays);
        // Unhurt: the plain buckler's heal is thrown away, so constraining the word is not worth it.
        assert!(!Preferences::from_flags(plain, false).wood_only_pays);
        // The braced one still queues a braced wooden tile, which does not care about health.
        assert!(Preferences::from_flags(braced, false).wood_only_pays);
    }

    /// The Targe pays for ending a word on braced wood, not for a wood-only word, so it must not
    /// switch this on. Its flags are `tileRefillSpecialWoodbraced` / `tileQueueSpecialWoodbraced`
    /// (`items/specialtilegear.lua:31-34`) and neither is a wood-*kill* flag.
    #[test]
    fn the_targe_alone_does_not_make_wood_only_worth_chasing() {
        let targe = |f: &str| f == "tileRefillSpecialWoodbraced" || f == "tileQueueSpecialWoodbraced";
        assert!(!Preferences::from_flags(targe, true).wood_only_pays);
        assert!(!Preferences::from_flags(targe, false).wood_only_pays);
    }

    #[test]
    fn a_payout_outranks_tidier_letters_and_wood_outranks_a_falling_hazard() {
        let wood = Rank { wood_only: true, hazard_fall: 0, deviation: 99.0 };
        let hazard = Rank { wood_only: false, hazard_fall: 5, deviation: 1.0 };
        assert!(wood.better_than(&hazard), "a wood-only kill outranks any number of falls");

        let falls = Rank { wood_only: false, hazard_fall: 2, deviation: 50.0 };
        let tidy = Rank { wood_only: false, hazard_fall: 1, deviation: 0.0 };
        assert!(falls.better_than(&tidy), "a falling hazard outranks a tidier board");

        let a = Rank { wood_only: false, hazard_fall: 1, deviation: 3.0 };
        let b = Rank { wood_only: false, hazard_fall: 1, deviation: 4.0 };
        assert!(a.better_than(&b), "with the payouts equal, lower deviation wins");
        assert!(!b.better_than(&a));
        assert!(!a.better_than(&a), "better_than is strict");
    }
}
