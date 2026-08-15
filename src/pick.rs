//! Choosing *which* killing word to play, when more than one kills.
//!
//! [`crate::search::Goal::FirstKill`] took whichever lethal word a thread happened to find first.
//! That is the right answer to "end the exchange" and the wrong answer to "play the fight", because
//! the word also decides what the board looks like afterwards and what the turn is worth beyond the
//! damage. Four things separate two words that both kill:
//!
//! 1. **A wood-only kill pays out**, when the gear for it is held and the payout is not wasted.
//! 2. **Gold and wildcards are held back.** A board's gold tiles are the damage it has in reserve for
//!    the turn a big hit is needed, so a kill that spends one where a plainer word would also have
//!    killed has thrown that reserve away. See [`hoarded`].
//! 3. **Hazard tiles want to reach the bottom.** `tileboard.lua:178` is the game telling the player
//!    so directly: *"I'm sorry. Your special tile got unlucky. Get it to the bottom to get rid of
//!    it."* A `!` tile *"falls off if it's at the bottom of the board at the start of your turn"*
//!    (`:180`), so clearing tiles beneath one is how it leaves.
//! 4. **What is left should look like a board you can play.** See [`crate::letters`].
//!
//! In that order, and the order is the point: 1 and 2 are about value, 3 and 4 are hygiene. A
//! tiebreak never outranks a payout, and none of the four ever outranks killing.
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
    /// Damage at which a killing blow's overkill covers the **whole** health deficit, if the
    /// Well-Rested heal is available and wanted. `None` when there is no heal to collect.
    ///
    /// `lethal + 2 * missing`, because the heal is `floor(overkill/2)` capped at the deficit
    /// (`rpgview.lua:1195-1196`). See [`Rank::heals_fully`] for why this is a rank and not a floor.
    pub heal_at: Option<i64>,
}

impl Preferences {
    /// Reads the wood-kill gear out of `gearFlags` and decides whether any of it is worth
    /// constraining the word for **right now**.
    ///
    /// **Flags, not item names**, because flags are what the save carries. Five items touch this and
    /// they are not interchangeable:
    ///
    /// ```text
    ///   Wooden idol          woodenIdol             health
    ///   Wooden buckler       armourBucklerWood              armour
    ///   Wooden idol buckler  armourBucklerIdolWood  health  armour
    ///   Braced buckler       armourTargeIdol        health  armour  braced
    ///   Targe                armourTarge                            braced
    /// ```
    ///
    /// (`items/woodaugmentgear.lua:85,117,154`, `items/idoltarge.lua:4`,
    /// `items/specialtilegear.lua:4`.)
    ///
    /// ## Three payouts, three different reasons they might be worthless
    ///
    /// This used to treat health and armour as one thing behind a single "would the heal land"
    /// gate, which is wrong for every item that grants armour without health — including the
    /// **Wooden buckler**, the one a live run was actually carrying. Its description is
    /// *"Defeating an enemy with a word that only contains wooden tiles gives you 4 armour"*: no
    /// heal is involved, so gating it on being injured and not bleeding withheld it for two reasons
    /// that do not apply.
    ///
    /// - **`onWoodKillGainHealth`** heals 4 *"unless you're bleeding"*, and `rpgview.lua:1080` skips
    ///   the heal branch outright while `bleed` is set. At full health the heal is thrown away. So
    ///   it pays only when **injured and not bleeding**.
    /// - **`onWoodKillGainArmour`** is not a heal. Bleeding does not cancel it and full health does
    ///   not waste it — but armour caps at `maxHealth / (maxArmourHalved and 2 or 1)`
    ///   (`overworld.lua:18`), so it pays only while there is **room for more armour**.
    /// - **`onWoodKillQueueWoodbraced`** queues a braced wooden tile and cares about none of the
    ///   above. It pays **always**.
    ///
    /// Any one of the three is reason enough, so they are OR-ed rather than ranked: the word is
    /// constrained the same way whichever payout is collecting.
    pub fn from_flags(
        has_flag: impl Fn(&str) -> bool, injured: bool, bleeding: bool, armour_room: bool,
    ) -> Self {
        let heals = has_flag("onWoodKillGainHealth") && injured && !bleeding;
        let armours = has_flag("onWoodKillGainArmour") && armour_room;
        let braced = has_flag("onWoodKillQueueWoodbraced");
        Preferences { wood_only_pays: heals || armours || braced, heal_at: None }
    }

    /// Records the damage at which the Well-Rested heal covers the whole deficit.
    ///
    /// Separate from [`Preferences::from_flags`] because the two answer to different things: the
    /// flags are gear, this is the player's health and charges, and only the search knows the
    /// enemy's. See [`Rank::heals_fully`].
    pub fn healing_at(self, damage: Option<i64>) -> Self {
        Preferences { heal_at: damage, ..self }
    }
}

/// Everything the ranking needs that does not change between candidates.
///
/// Built once per turn and borrowed by every thread. The [`Target`] in particular is worth hoisting:
/// a board cannot change size mid-fight, so its target is computed once and read by every one of the
/// tens of thousands of candidates a scan considers.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Context {
    pub target: Target,
    pub prefs: Preferences,
}

/// How good a lethal candidate is, beyond the fact that it kills.
///
/// Compared in field order: wood-only first, then what it spends from the board's good tiles, then
/// hazard fall, then deviation. Not `Ord`, because two fields are `f64` and a total order over floats
/// would be a lie — [`Rank::better_than`] is the comparison, and it is deliberately the only one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rank {
    /// Every tile used was wood, and the gear pays for that. Higher is better.
    pub wood_only: bool,
    /// This blow overkills deeply enough for the Well-Rested heal to close the whole deficit.
    ///
    /// **Below `wood_only`, and that ordering is the dev's rule**, 2026-08-15: *first check if
    /// there's a wood-only kill that triggers both the Well-Rested heal and the armour bonus;
    /// otherwise go for the best wood-only kill, and only if that's not possible, go for the
    /// Well-Rested heal.* Two booleans in that order give exactly those four tiers:
    ///
    /// ```text
    ///   wood_only  heals_fully
    ///     true       true       both payouts -- the one to want
    ///     true       false      the gear's armour or health, no heal
    ///     false      true       the heal alone
    ///     false      false      an ordinary kill
    /// ```
    ///
    /// It is a **rank** rather than a threshold on purpose. Asking for the heal as a floor is what
    /// the search used to do, and a floor cannot express "I would rather have the wood word that
    /// heals nothing": every candidate under it is discarded before anything compares them. The
    /// floor drops to plain lethal whenever the wood payout is live, and this field carries the heal
    /// into the comparison instead — see [`crate::search::Goal::killing_blow`].
    pub heals_fully: bool,
    /// What this word spends from the board's stock of tiles worth keeping. **Lower** is better.
    ///
    /// See [`hoarded`]. Ranked below `wood_only`, which is a payout the gear actually pays, and above
    /// both hygiene terms: a kill is a kill, so of two words that both end the fight, the one that
    /// does not burn the board's biggest hitter is the better trade.
    ///
    /// Note the two do **not** subsume each other, though it looks as if they might. A wildcard's
    /// material is `wood0` (`rpg/effects/material/default.lua:4`), which `wood_only` accepts, so a
    /// wood-only kill can still be spending one.
    pub hoarded: f64,
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
        if self.heals_fully != other.heals_fully {
            return self.heals_fully;
        }
        if self.hoarded != other.hoarded {
            return self.hoarded < other.hoarded;
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

/// A wildcard: the letter is `.` until something is typed into it.
///
/// `letterMaterials['.'] = 'wood0'` and `score.wood0 = 0`
/// (`rpg/effects/material/default.lua:4,38`), so a wildcard is worth **nothing** on its own and
/// nothing in the scorer has any reason to keep one. What makes it precious is the other half: it
/// takes whatever letter we ask for, so it is the tile that turns a word we cannot spell into one we
/// can. That value does not appear in any score, which is why it has to be named here.
const WILDCARD: &str = ".";

/// What a wildcard counts for when deciding what a word spends. See [`hoarded`].
///
/// The dev's number, 2026-08-10, and the reasoning is in the size of it: **1, not 10.** A wildcard is
/// worth conserving, but nothing like as much as a gold tile — so this is set low enough that it can
/// never argue a gold tile into the fire, and high enough that between two words which spend the same
/// gold, the one that also burns a wildcard loses.
const WILDCARD_WORTH: f64 = 1.0;

/// The material a word should not be spending unless the kill needs it.
const GOLD: &str = "gold";

/// What this word spends from the board's stock of tiles worth keeping. Lower is better.
///
/// The dev's rule, 2026-08-09: **do not spend a solid gold tile until we cannot land a kill without
/// it.** Gold is what a board holds for the moment a big hit is needed, and spending a `Q` on a kill
/// a wood word would also have made is the waste this exists to stop.
///
/// ## Solid gold, and that means the material, not the border
///
/// [`Scorer::material_name`] resolves the two cases in one call — an explicit `bg` where gear has
/// upgraded the tile, and otherwise the material the letter implies, which makes **Z, X, J and Q**
/// gold by nature (`rpg/effects/material/default.lua:25-28`). The dev settled that both are hoarded:
/// the point of the rule is damage held in reserve, and a natural `Q` is the biggest reserve there
/// is. A *bordered* gold tile is a different thing — `rpg/effects/border/gold.lua` is an overlay that
/// can sit on any material — and is deliberately not counted.
///
/// ## Cost is the tile's own worth, which buys the post-MVP refinement for nothing
///
/// A gold tile costs what [`Scorer::tile_score`] says it is worth, so `Z = 10` and `Q = 40` are not
/// the same expenditure. That was raised as a later task — *minimise the value of the gold spent* —
/// and falls out of pricing preciousness at value rather than counting tiles. Nothing was added for
/// it; a flat cost would have been the same amount of code and less true.
///
/// A tile has one preciousness, so the two reasons are `max`ed rather than summed: a wildcard that
/// gear has turned gold is priced as gold, which it is.
pub fn hoarded(tiles: &[Tile], consumed: &[usize], scorer: &Scorer) -> f64 {
    consumed
        .iter()
        .filter_map(|&i| tiles.get(i))
        .map(|t| {
            let gold = match scorer.material_name(t) {
                Some(GOLD) => scorer.tile_score(t),
                _ => 0.0,
            };
            let wild = match t.letter == WILDCARD {
                true => WILDCARD_WORTH,
                false => 0.0,
            };
            gold.max(wild)
        })
        .sum()
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
    target: &Target, prefs: Preferences, damage: i64,
) -> Rank {
    Rank {
        wood_only: prefs.wood_only_pays && wood_only(tiles, consumed, scorer),
        heals_fully: prefs.heal_at.is_some_and(|at| damage >= at),
        hoarded: hoarded(tiles, consumed, scorer),
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

    /// What a word spends, read off the game's own tables rather than asserted from memory.
    ///
    /// The dev's rule is about *solid gold*, so the fixture has to prove the scorer agrees which
    /// letters those are before the costs mean anything.
    #[test]
    fn gold_letters_cost_what_they_are_worth_and_plain_ones_cost_nothing() {
        let Some(sc) = scorer() else { return };
        let tiles = board(&["Q", "Z", "E", "A"]);
        // If this is wrong the fixture is wrong, not the code, and every assertion below would be
        // passing for the wrong reason.
        for (i, letter) in ["Q", "Z"].iter().enumerate() {
            assert_eq!(sc.material_name(&tiles[i]).as_deref(), Some(GOLD), "{letter} is solid gold");
        }
        assert_eq!(sc.material_name(&tiles[2]).as_deref(), Some("wood"), "E is not");

        assert_eq!(hoarded(&tiles, &[2, 3], &sc), 0.0, "a word of plain letters spends nothing");
        // `score.Q = 40`, `score.Z` falls through to `score.gold = 10`
        // (`rpg/effects/material/default.lua:33-41`). So the two are not interchangeable, which is
        // the whole of the deferred "spend the cheapest gold" refinement, arriving for free.
        let q = hoarded(&tiles, &[0], &sc);
        let z = hoarded(&tiles, &[1], &sc);
        assert!(z > 0.0 && q > z, "Q ({q}) should cost more to spend than Z ({z})");
        assert_eq!(hoarded(&tiles, &[0, 1], &sc), q + z, "and spending both costs both");
    }

    /// A wildcard is worth nothing to the scorer and something to us.
    ///
    /// `letterMaterials['.'] = 'wood0'` with `score.wood0 = 0`, so nothing in the scoring model has
    /// any reason to keep one — its value is that it takes whatever letter we ask for. The dev's
    /// number is **1**: enough to break a tie, never enough to argue a gold tile into the fire.
    #[test]
    fn a_wildcard_is_worth_keeping_but_never_more_than_gold() {
        let Some(sc) = scorer() else { return };
        let tiles = board(&[".", "Z", "E"]);
        assert_eq!(sc.tile_score(&tiles[0]), 0.0, "the game scores a wildcard at nothing");
        assert_eq!(hoarded(&tiles, &[0], &sc), WILDCARD_WORTH, "and we still would rather keep it");

        // The point of the number being small: burning a wildcard must never look worse than
        // burning gold, or the rule would spend the reserve to protect the convenience.
        assert!(
            hoarded(&tiles, &[0], &sc) < hoarded(&tiles, &[1], &sc),
            "a wildcard has to cost less than the cheapest gold tile"
        );
        assert_eq!(hoarded(&tiles, &[2], &sc), 0.0, "and a plain letter is free to spend");
    }

    /// The dev's four tiers, 2026-08-15, in the order they stated them.
    ///
    /// > if a Well-Rested heal only heals for 3 while a wood-only kill provides 4 armour, we should
    /// > first check if there's a wood-only kill that triggers both the Well-Rested heal and armour
    /// > bonus; otherwise, go for the best wood-only kill, and only if that's not possible, go for
    /// > the Well-Rested heal.
    ///
    /// Deliberately asserted with the *other* fields set against each tier — the heal-only word is
    /// given a spotless board and the wood word a wasteful one — because that is what proves the
    /// order is the priority and not an accident of the hygiene terms.
    #[test]
    fn a_wood_kill_outranks_a_heal_it_cannot_also_collect() {
        let tier = |wood: bool, heal: bool, hoarded: f64| Rank {
            wood_only: wood,
            heals_fully: heal,
            hoarded,
            hazard_fall: 0,
            deviation: 0.0,
        };
        let both = tier(true, true, 99.0);
        let wood_only_kill = tier(true, false, 99.0);
        let heal_only = tier(false, true, 0.0);
        let ordinary = tier(false, false, 0.0);

        assert!(both.better_than(&wood_only_kill), "both payouts beat one, spendthrift or not");
        assert!(wood_only_kill.better_than(&heal_only), "the gear's payout outranks the heal");
        assert!(heal_only.better_than(&ordinary), "and a heal still beats no payout at all");
        // Transitive, so the whole order holds rather than just its neighbours.
        assert!(both.better_than(&ordinary));
        assert!(wood_only_kill.better_than(&ordinary));
        assert!(!heal_only.better_than(&wood_only_kill), "strictly, in one direction only");
    }

    /// With no heal to collect, the field must not sway anything.
    #[test]
    fn the_heal_rank_is_inert_when_there_is_no_heal() {
        let prefs = Preferences { wood_only_pays: true, heal_at: None };
        // `rank` is what sets the field, and `heal_at: None` is how the caller says the heal is
        // unavailable -- no charge, cancelled by gear, or already at full health.
        assert!(!prefs.heal_at.is_some_and(|at| 9999 >= at), "no threshold, so nothing clears it");
        let with = Preferences::default().healing_at(Some(20));
        assert!(with.heal_at.is_some_and(|at| 20 >= at), "exactly the threshold heals fully");
        assert!(!with.heal_at.is_some_and(|at| 19 >= at), "a point short does not");
    }

    /// Two words that both kill: the one that keeps the good tiles wins.
    ///
    /// The rule in the shape it is actually used — a comparison between candidates, not a score.
    #[test]
    fn between_two_kills_the_one_that_spends_less_wins() {
        let plain = Rank { wood_only: false, heals_fully: false, hoarded: 0.0, hazard_fall: 0, deviation: 9.0 };
        let golden = Rank { wood_only: false, heals_fully: false, hoarded: 40.0, hazard_fall: 0, deviation: 0.0 };
        assert!(plain.better_than(&golden), "a tidier board is not worth a Q");
        assert!(!golden.better_than(&plain));

        // Below the payout, though. `wood_only` is gear paying out for real, and this is a saving.
        let paid = Rank { wood_only: true, heals_fully: false, hoarded: 40.0, hazard_fall: 0, deviation: 9.0 };
        assert!(paid.better_than(&plain), "a payout in hand outranks a tile kept back");

        // And the wildcard case the dev asked for: same gold either way, so do not also burn one.
        let gold_only = Rank { wood_only: false, heals_fully: false, hoarded: 10.0, hazard_fall: 0, deviation: 0.0 };
        let gold_and_wild = Rank { wood_only: false, heals_fully: false, hoarded: 11.0, hazard_fall: 0, deviation: 0.0 };
        assert!(gold_only.better_than(&gold_and_wild), "no reason to spend a wildcard as well");
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

    /// The five wood items, each by the flags it actually carries. Named after the items rather
    /// than after "plain" and "braced", because lumping them cost a live run its buckler.
    #[test]
    fn each_wood_item_pays_for_its_own_reasons() {
        let idol = |f: &str| f == "onWoodKillGainHealth";
        let buckler = |f: &str| f == "onWoodKillGainArmour";
        let idol_buckler = |f: &str| idol(f) || buckler(f);
        let braced = |f: &str| idol_buckler(f) || f == "onWoodKillQueueWoodbraced";
        // (injured, bleeding, armour_room)
        let hurt = (true, false, true);
        let full = (false, false, true);
        let bleeding = (true, true, true);
        let capped = (false, false, false);
        // Injured, so the heal would land, but armour has nowhere to go. The state that separates
        // the two payouts -- without it, "armour full" and "not injured" are never seen apart.
        let capped_but_hurt = (true, false, false);
        let pays = |g: &dyn Fn(&str) -> bool, (i, b, a): (bool, bool, bool)| {
            Preferences::from_flags(g, i, b, a).wood_only_pays
        };

        // The **Wooden idol** is a heal and only a heal: injured and not bleeding, or nothing.
        assert!(pays(&idol, hurt));
        assert!(!pays(&idol, full), "a heal at full health is thrown away");
        assert!(!pays(&idol, bleeding), "rpgview.lua:1080 skips the heal branch");
        assert!(pays(&idol, capped_but_hurt), "armour being full says nothing about a heal");

        // The **Wooden buckler** is armour and only armour -- the one a live run was carrying, and
        // the case the old single gate got wrong in both directions.
        assert!(pays(&buckler, full), "4 armour is worth having at full health");
        assert!(pays(&buckler, bleeding), "bleeding does not cancel armour");
        assert!(!pays(&buckler, capped), "but a full armour bar does");

        // The **Wooden idol buckler** has both, so either reason is enough.
        assert!(pays(&idol_buckler, full), "the armour still pays");
        assert!(pays(&idol_buckler, capped_but_hurt), "and at full armour the heal does");
        assert!(!pays(&idol_buckler, capped), "neither payout is available: no heal wanted, no room");

        // The **Braced buckler** adds a queued tile, which cares about none of it.
        for state in [hurt, full, bleeding, capped] {
            assert!(pays(&braced, state), "onWoodKillQueueWoodbraced always pays");
        }
    }

    /// The single case that made the split necessary, stated on its own.
    ///
    /// A live run carried `armourBucklerWood` at 1/20 and the preference *was* on -- but only
    /// because it happened to be injured. At full health, or while bleeding, the old rule would have
    /// declined 4 free armour for reasons that belong to a heal the item does not grant.
    #[test]
    fn armour_is_not_a_heal_and_is_not_gated_like_one() {
        let buckler = |f: &str| f == "onWoodKillGainArmour";
        assert!(Preferences::from_flags(buckler, false, true, true).wood_only_pays);
        // The only thing that switches it off is having nowhere to put the armour.
        assert!(!Preferences::from_flags(buckler, true, false, false).wood_only_pays);
    }

    /// The Targe pays for ending a word on braced wood, not for a wood-only word, so it must not
    /// switch this on. Its flags are `tileRefillSpecialWoodbraced` / `tileQueueSpecialWoodbraced`
    /// (`items/specialtilegear.lua:31-34`) and neither is a wood-*kill* flag.
    #[test]
    fn the_targe_alone_does_not_make_wood_only_worth_chasing() {
        let targe = |f: &str| f == "tileRefillSpecialWoodbraced" || f == "tileQueueSpecialWoodbraced";
        // No combination of health and bleeding makes it pay, because it has no wood-kill flag at all.
        for injured in [true, false] {
            for bleeding in [true, false] {
                for room in [true, false] {
                    assert!(!Preferences::from_flags(targe, injured, bleeding, room).wood_only_pays);
                }
            }
        }
    }

    #[test]
    fn a_payout_outranks_tidier_letters_and_wood_outranks_a_falling_hazard() {
        let wood = Rank { wood_only: true, heals_fully: false, hoarded: 0.0, hazard_fall: 0, deviation: 99.0 };
        let hazard = Rank { wood_only: false, heals_fully: false, hoarded: 0.0, hazard_fall: 5, deviation: 1.0 };
        assert!(wood.better_than(&hazard), "a wood-only kill outranks any number of falls");

        let falls = Rank { wood_only: false, heals_fully: false, hoarded: 0.0, hazard_fall: 2, deviation: 50.0 };
        let tidy = Rank { wood_only: false, heals_fully: false, hoarded: 0.0, hazard_fall: 1, deviation: 0.0 };
        assert!(falls.better_than(&tidy), "a falling hazard outranks a tidier board");

        let a = Rank { wood_only: false, heals_fully: false, hoarded: 0.0, hazard_fall: 1, deviation: 3.0 };
        let b = Rank { wood_only: false, heals_fully: false, hoarded: 0.0, hazard_fall: 1, deviation: 4.0 };
        assert!(a.better_than(&b), "with the payouts equal, lower deviation wins");
        assert!(!b.better_than(&a));
        assert!(!a.better_than(&a), "better_than is strict");
    }
}
