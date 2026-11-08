//! Where each tile sits on the board.
//!
//! The `--verbose` dump is a flat list, but it is not shapeless: `getDataForSave`
//! (`tileboard.lua:2445-2455`) walks `tilegrid` **column-major**, `for col ... for row`, so entry
//! *i* is at a position that follows directly from the column heights. Recovering that position is
//! what makes two things possible at all:
//!
//! - **`resistCornerless`** (`utils/words.lua:238-240`) scales a word's damage by
//!   `selectedCornerTiles / cornerCount`, which reaches **zero** for a word that touches no corner.
//!   Against the skeleton shield boss (`rpg/enemies/skeletons.lua:251`) an unmodelled corner nerf
//!   does not merely mis-rank words, it reports lethal damage for a word that will do none.
//! - **Which tile the typist takes** when several carry the same letter, because
//!   `getUnselectedLegalTileWithLetter` (`tileboard.lua:2269-2292`) scans the **corners first**.
//!
//! ## The shape cannot be inferred from the tile count
//!
//! Sixteen entries is a 4×4 board — or a hexagonal 5×4 with `colTileCounts = {3,3,4,3,3}`
//! (`items/boardshapes.lua:63-65`), which is also sixteen. So the geometry is derived the way the
//! game derives it (`tileboard.lua:70-90`): default 4×4, overridden by the **first** passive that
//! carries `boardData`. Guessing from the count would be a coin flip on a corner-nerf fight.

use crate::game::save::Table;
use std::collections::BTreeSet;

/// Default board: 4 columns of 4 (`tileboard.lua:71-72`), corners in the order the game lists them
/// (`tileboard.lua:64-69`) — that order is the tie-break the typist uses, so it is preserved.
const DEFAULT_CORNERS: [(usize, usize); 4] = [(1, 1), (4, 1), (1, 4), (4, 4)];

/// Tile edge length in game units (`tileboard.lua:51`). Everything on the board is a multiple of it.
pub const TILE_SIZE: f64 = 118.0;
/// The board object's frame above and below the tiles (`tileboard.lua:48-49`).
pub const HEADING_DEPTH: f64 = 79.0;
pub const FOOTER_DEPTH: f64 = 79.0;

/// The board's shape and which slots are locked.
#[derive(Debug, Clone, PartialEq)]
pub struct Geometry {
    /// Tile count per column, in dump order.
    pub rows_per_col: Vec<usize>,
    /// The board's nominal height in tiles — `boardSize[2]`, which is NOT `rows_per_col.max()` on a
    /// hexagonal board. It sets the drawn height, so it is needed to place tiles on screen.
    pub tiles_y: usize,
    /// Vertical offset per column in game units (`tileboard.lua:96-98`). Non-zero only on hexagonal
    /// boards, where short columns are centred rather than bottom-aligned.
    pub col_y_offsets: Vec<f64>,
    /// Corner coordinates as 1-based `(col, row)`, in the game's own order.
    pub corners: Vec<(usize, usize)>,
    /// Rows locked by `tileboardUnselectableRow<N>` gear flags (`tileboard.lua:409-412`).
    pub locked_rows: BTreeSet<usize>,
    /// Columns locked by `tileboardUnselectableCol<N>` (`tileboard.lua:414-417`).
    pub locked_cols: BTreeSet<usize>,
    /// `wordRequirementAdjacent` gear (directThinker, the letter-chaining curse): each tile must be
    /// next to the previous one ([`Geometry::tiles_adjacent`]), and typing looks for each letter only
    /// around the last tile (`tileboard.lua:2196-2202`, `:2273-2274`). [`crate::typist`] models both.
    pub adjacency_required: bool,
    /// A hexagonal board, where adjacency loses two diagonals ([`Geometry::tiles_adjacent`]).
    pub hexagonal: bool,
    /// `middleColI` (`tileboard.lua:81`): which side of it a tile is on decides which two.
    pub middle_col: usize,
}

impl Default for Geometry {
    fn default() -> Self {
        Geometry {
            rows_per_col: vec![4; 4],
            tiles_y: 4,
            col_y_offsets: vec![0.0; 4],
            corners: DEFAULT_CORNERS.to_vec(),
            locked_rows: BTreeSet::new(),
            locked_cols: BTreeSet::new(),
            adjacency_required: false,
            hexagonal: false,
            middle_col: 2,
        }
    }
}

/// A geometry plus whatever we could not resolve while building it.
#[derive(Debug, Clone)]
pub struct Resolved {
    pub geometry: Geometry,
    /// Non-empty means the shape is a guess. A caller in a `resistCornerless` fight must treat its
    /// scores as unreliable rather than acting on them.
    pub problems: Vec<String>,
    /// Things worth saying that are **not** faults — the board being legitimately ragged, and what
    /// that costs. Kept apart from [`Resolved::problems`] because the two want opposite responses:
    /// a problem means the geometry is a guess and scores cannot be trusted, while a note means the
    /// geometry is right and the board is simply in a worse state than a full one.
    pub notes: Vec<String>,
}

impl Geometry {
    pub fn total_tiles(&self) -> usize {
        self.rows_per_col.iter().sum()
    }

    pub fn corner_count(&self) -> usize {
        self.corners.len()
    }

    /// How many tiles sit in the columns `tiles` uses every tile of:
    /// `tileboard.countTilesInFullySelectedColumn` (`tileboard.lua:892-909`), which the Totem
    /// (`wordScoreBonusFullColumn`) pays half a point of pre-add on, per tile.
    ///
    /// Per column as the board holds it, so a short column on a hexagonal or ragged board is full
    /// at its own height, and an empty column is never full (`rowsData[1]` is nil).
    pub fn full_column_tiles(&self, tiles: &[usize]) -> usize {
        let mut start = 0;
        let mut count = 0;
        for &rows in &self.rows_per_col {
            if rows > 0 && (start..start + rows).all(|i| tiles.contains(&i)) {
                count += rows;
            }
            start += rows;
        }
        count
    }

    /// `(col, row)` of dump entry `flat`, both 1-based. `None` past the end of the board.
    pub fn position(&self, flat: usize) -> Option<(usize, usize)> {
        let mut seen = 0;
        for (i, &rows) in self.rows_per_col.iter().enumerate() {
            if flat < seen + rows {
                return Some((i + 1, flat - seen + 1));
            }
            seen += rows;
        }
        None
    }

    /// Dump indices of the corner tiles, **in the game's corner order** — the order
    /// `getUnselectedLegalTileWithLetter` tries them in, which decides which of two identical
    /// letters a typed word consumes.
    pub fn corner_indices(&self) -> Vec<usize> {
        self.corners.iter().filter_map(|&c| self.flat_of(c)).collect()
    }

    /// `tileboard.tilesAreAdjacent(prev, next)` (`tileboard.lua:500-521`), by dump index.
    ///
    /// Within one column and one row of index either way — on `(col, row)` indices, so a short
    /// column's tiles neighbour the bottoms of the columns beside it. A hexagonal board then drops
    /// two of those eight, chosen by which side of the middle column `prev` is on, with
    /// `x = x1 - x2` and `y = y2 - y1` (the game's own signs, `:505`):
    ///
    /// - left of the middle, `leftHexNotAdj`: `(y, x)` of `(-1, -1)` and `(1, 1)`;
    /// - right of it, `rightHexNotAdj`: `(-1, 1)` and `(1, -1)`;
    /// - on it, `midColNotAdj`: `(1, -1)` and `(1, 1)` (`:488-498`).
    pub fn tiles_adjacent(&self, prev: usize, next: usize) -> bool {
        let (Some((x1, y1)), Some((x2, y2))) = (self.position(prev), self.position(next)) else {
            return false;
        };
        let (x, y) = (x1 as i64 - x2 as i64, y2 as i64 - y1 as i64);
        if x.abs() > 1 || y.abs() > 1 {
            return false;
        }
        if !self.hexagonal {
            return true;
        }
        let excluded: [(i64, i64); 2] = match x1.cmp(&self.middle_col) {
            std::cmp::Ordering::Less => [(-1, -1), (1, 1)],
            std::cmp::Ordering::Greater => [(-1, 1), (1, -1)],
            std::cmp::Ordering::Equal => [(1, -1), (1, 1)],
        };
        !excluded.contains(&(y, x))
    }

    fn flat_of(&self, (col, row): (usize, usize)) -> Option<usize> {
        if col == 0 || row == 0 || col > self.rows_per_col.len() || row > self.rows_per_col[col - 1]
        {
            return None;
        }
        Some(self.rows_per_col[..col - 1].iter().sum::<usize>() + row - 1)
    }

    pub fn is_corner(&self, flat: usize) -> bool {
        self.position(flat).map(|p| self.corners.contains(&p)).unwrap_or(false)
    }

    /// `tileboard.slotIsSelectable` minus the tile's own `unselectable` flag, which the caller
    /// already reads off the dump.
    pub fn slot_selectable(&self, flat: usize) -> bool {
        match self.position(flat) {
            Some((col, row)) => {
                !self.locked_cols.contains(&col) && !self.locked_rows.contains(&row)
            }
            None => false,
        }
    }

    /// Derives the geometry from a named board-shape passive, for offline use.
    ///
    /// Goes through [`Geometry::from_save`] rather than around it, so the offline path cannot drift
    /// from the one the live loop uses.
    pub fn for_passive(passive: &str, tile_count: usize) -> Resolved {
        let save = crate::game::save::parse(&format!("return {{ passives = {{ {passive:?} }} }}"))
            .expect("a literal table always parses");
        Geometry::from_save(&save, tile_count)
    }

    /// Derives the geometry from a `combatSaveData` table, the way `generateTileboardData` does.
    ///
    /// `tile_count` is the length of the board dump, used only as a check: a shape that does not
    /// account for exactly that many tiles is reported rather than used, because a wrong shape puts
    /// the corners on the wrong letters and that is worse than admitting ignorance.
    pub fn from_save(save: &Table, tile_count: usize) -> Resolved {
        let mut problems = Vec::new();
        let mut notes: Vec<String> = Vec::new();
        let mut geometry = Geometry::default();

        let passives: Vec<&str> = save
            .table_at("passives")
            .map(|t| t.arr.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        // `tileboard.lua:74-89` takes the FIRST passive with boardData and breaks. Read from the
        // baked table, not from the game file: this runs every combat turn and must not depend on
        // evaluating Lua. `crate::tables` carries a test that the baked copy still matches source.
        for key in &passives {
            if let Some(shape) = crate::tables::shape(key) {
                geometry = shape_to_geometry(shape);
                break;
            }
        }

        if let Some(flags) = save.table_at("rpg.player.gearFlags") {
            for key in flags.map.keys() {
                if let Some(n) = key.strip_prefix("tileboardUnselectableRow").and_then(parse_index)
                {
                    geometry.locked_rows.insert(n);
                } else if let Some(n) =
                    key.strip_prefix("tileboardUnselectableCol").and_then(parse_index)
                {
                    geometry.locked_cols.insert(n);
                } else if key == "wordRequirementAdjacent" {
                    geometry.adjacency_required = true;
                }
            }
        }

        // A ragged board: the save states the current column heights, and only when they no longer
        // add up to the full board.
        //
        // `tileboard.lua:2457-2467` writes `letters.columns` **only** `if #letters ~= totalTileCount`,
        // each entry `#tilegrid[i]`. That is why it has never appeared before, and why its absence
        // has always been safe to ignore.
        //
        // It has to be honoured rather than warned about: a tile that falls off the bottom leaves a
        // column one short, and the flat array is column-major bottom-to-top, so every index after
        // that column addresses the wrong cell. Row 0 is the bottom row here (see
        // `crate::layout::tile_centres`), which is what makes a short column fill from the bottom
        // and leave its gap at the top — matching the board exactly as drawn.
        //
        // `col_y_offsets` is deliberately left alone. On a hexagonal board that offset centres a
        // column that is *nominally* short, which is a property of the board's shape and not of how
        // many tiles happen to be sitting in it right now.
        if let Some(cols) = save.table_at("tileboard").and_then(|t| t.map.get("columns")) {
            if let Some(t) = cols.as_table() {
                let heights: Vec<usize> =
                    t.arr.iter().filter_map(|v| v.as_int()).map(|n| n.max(0) as usize).collect();
                if !heights.is_empty() {
                    if heights.len() == geometry.rows_per_col.len() {
                        geometry.rows_per_col = heights;
                    } else {
                        problems.push(format!(
                            "save lists {} column heights but the shape has {} columns",
                            heights.len(),
                            geometry.rows_per_col.len()
                        ));
                    }
                }
            }
        }

        if geometry.total_tiles() != tile_count {
            problems.push(format!(
                "board shape accounts for {} tiles but the dump has {tile_count}",
                geometry.total_tiles()
            ));
        }
        // **A corner that is no longer on the board is a state, not a fault.**
        //
        // The dev, 2026-08-15: *there's a possibility that we will have a bomb tile or an
        // exclamation tile cause our board to be not full when we encounter the shield boss.*
        // Exactly right, and the anomaly run of that evening printed this warning on three of its
        // nine turns while playing perfectly well.
        //
        // Corners are fixed when the board is built — `tileboard.lua:86` sets `data.corners` once
        // and nothing moves them — while the columns shorten as tiles fall off. So a `!` tile
        // reaching the bottom, or a bomb going off, can take the tile a corner names and the
        // coordinate simply stops resolving. The game behaves the same way and does not treat it as
        // an error: `getSelectedCornerTileCount` (`:122-131`) walks the corner list and indexes the
        // grid, and a missing tile is one it cannot count.
        //
        // **Which matters because of the shield boss.** `resistCornerless` scales damage by
        // `getSelectedCornerTileCount(word) / getCornerCount()` (`utils/words.lua:238-240`), and the
        // denominator is `#corners` — the full set, whether or not the tiles are still there. So a
        // board that has lost a corner tile **cannot reach full damage against that enemy at all**,
        // and no word will ever be found that does. That is the game's rule, faithfully; what would
        // be a fault is not knowing we are in it, so it is reported as a ceiling rather than as a
        // complaint.
        let missing = geometry.corners.iter().filter(|&&c| geometry.flat_of(c).is_none()).count();
        if missing > 0 {
            notes.push(format!(
                "{missing} of {} corner tiles have fallen off the board; against `resistCornerless` \
                 the most any word can do is {:.0}% damage",
                geometry.corners.len(),
                100.0 * (geometry.corners.len() - missing) as f64 / geometry.corners.len() as f64
            ));
        }

        Resolved { geometry, problems, notes }
    }
}

fn parse_index(s: &str) -> Option<usize> {
    s.parse().ok()
}

/// Turns a baked shape into a geometry, mirroring `tileboard.lua:76-105`.
fn shape_to_geometry(shape: &crate::tables::Shape) -> Geometry {
    let (cols, rows) = (shape.cols, shape.rows);
    let hexagonal = shape.hexagonal;
    // `math.floor((tilesX-1)/2+1)` when the file omits middleCol; 0 is the baked "omitted" marker.
    let middle = if shape.middle_col == 0 { (cols - 1) / 2 + 1 } else { shape.middle_col };
    let explicit = shape.col_tile_counts;
    let mut rows_per_col = Vec::with_capacity(cols);
    let mut col_y_offsets = Vec::with_capacity(cols);
    for i in 1..=cols {
        let given = explicit.get(i - 1).copied();
        let base = if hexagonal { rows.saturating_sub(middle.abs_diff(i)) } else { rows };
        rows_per_col.push(given.unwrap_or(base));
        // `d` is measured against the COMPUTED base, not the override: `colTileCounts` can shorten a
        // column without moving it (`tileboard.lua:92-98`).
        col_y_offsets.push(if hexagonal { (rows - base) as f64 * TILE_SIZE * 0.5 } else { 0.0 });
    }

    Geometry {
        rows_per_col,
        tiles_y: rows,
        col_y_offsets,
        corners: shape.corners.to_vec(),
        locked_rows: BTreeSet::new(),
        locked_cols: BTreeSet::new(),
        adjacency_required: false,
        hexagonal,
        middle_col: middle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Totem's count on the user's `hex4` board, 2026-11-07, columns of 3, 4, 3 and 2:
    /// `ANHYDRASES` took tiles 0-9, every tile of the first three columns, 10 in all, and the game
    /// dealt 39 where a score without them said 28.
    #[test]
    fn full_columns_count_at_their_own_height() {
        let g = Geometry::for_passive("hex4", 12).geometry;
        assert_eq!(g.rows_per_col, vec![3, 4, 3, 2]);
        assert_eq!(g.full_column_tiles(&[7, 4, 0, 1, 2, 3, 8, 5, 6, 9]), 10);
        assert_eq!(g.full_column_tiles(&[10, 11]), 2, "the short column");
        assert_eq!(g.full_column_tiles(&[3, 4, 5]), 0, "three of four");
        assert_eq!(g.full_column_tiles(&[]), 0);
    }
    use std::path::PathBuf;

    fn game_dir() -> PathBuf {
        PathBuf::from("../sternly-worded-adventures")
    }

    fn present() -> bool {
        game_dir().join("items/boardshapes.lua").is_file()
    }

    #[test]
    fn the_default_board_maps_flat_indices_column_major() {
        // `getDataForSave` walks columns then rows, so entry 4 ends column 1 and entry 5 opens
        // column 2. Getting this backwards would rotate the board and put corners on wrong letters.
        let g = Geometry::default();
        assert_eq!(g.total_tiles(), 16);
        assert_eq!(g.position(0), Some((1, 1)));
        assert_eq!(g.position(3), Some((1, 4)));
        assert_eq!(g.position(4), Some((2, 1)));
        assert_eq!(g.position(15), Some((4, 4)));
        assert_eq!(g.position(16), None);
    }

    #[test]
    fn corner_indices_follow_the_games_corner_order() {
        // The order matters: it is the order `getUnselectedLegalTileWithLetter` tries corners in,
        // so it decides which of two identical letters a word consumes.
        let g = Geometry::default();
        assert_eq!(g.corner_indices(), vec![0, 12, 3, 15], "(1,1) (4,1) (1,4) (4,4)");
        assert!(g.is_corner(0) && g.is_corner(15));
        assert!(!g.is_corner(5));
    }

    #[test]
    fn a_locked_row_or_column_makes_its_slots_unselectable() {
        let mut g = Geometry::default();
        g.locked_rows.insert(1);
        g.locked_cols.insert(4);
        assert!(!g.slot_selectable(0), "row 1 is locked");
        assert!(!g.slot_selectable(13), "column 4 is locked");
        assert!(g.slot_selectable(5), "(2,2) is untouched");
    }

    #[test]
    fn the_real_crypt_save_gives_the_default_board() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        // `passives = { "bedroll" }` — no board item, so the shape is the default 4x4 and nothing
        // is left unresolved. If this ever reports a problem, the corner model is not trustworthy.
        let save = crate::game::save::load(std::path::Path::new(
            "tests/fixtures/combatSaveData-crypt-l0.lua",
        ))
        .expect("fixture loads");
        let r = Geometry::from_save(&save, 16);
        assert!(r.problems.is_empty(), "problems: {:?}", r.problems);
        assert_eq!(r.geometry, Geometry::default());
    }

    #[test]
    fn a_board_shape_passive_changes_the_corners() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        // `stoutCharacter` is the 5x3 board (`items/boardshapes.lua:12-27`). Its corners are at
        // column 5, which does not even exist on the default board.
        let save =
            crate::game::save::parse(r#"return { passives = { "stoutCharacter" } }"#).unwrap();
        let r = Geometry::from_save(&save, 15);
        assert!(r.problems.is_empty(), "problems: {:?}", r.problems);
        assert_eq!(r.geometry.rows_per_col, vec![3, 3, 3, 3, 3]);
        assert_eq!(r.geometry.corners, vec![(1, 1), (5, 1), (1, 3), (5, 3)]);
        assert_eq!(r.geometry.corner_indices(), vec![0, 12, 2, 14]);
    }

    /// `tileboard.tilesAreAdjacent` (`tileboard.lua:500-521`): the eight neighbours on a square board,
    /// by `(col, row)` index — so a short column's tiles sit against the bottoms of its neighbours.
    #[test]
    fn square_adjacency_is_the_eight_neighbours() {
        let g = Geometry::default(); // 4x4; dump index = (col-1)*4 + (row-1)
        let at = |col: usize, row: usize| (col - 1) * 4 + (row - 1);
        assert!(g.tiles_adjacent(at(1, 1), at(2, 2)), "diagonal");
        assert!(g.tiles_adjacent(at(2, 2), at(2, 3)));
        assert!(g.tiles_adjacent(at(3, 3), at(2, 3)));
        assert!(!g.tiles_adjacent(at(1, 1), at(1, 3)), "two rows apart");
        assert!(!g.tiles_adjacent(at(1, 1), at(3, 1)), "two columns apart");
        assert!(!g.tiles_adjacent(at(1, 1), 99), "off the board");
    }

    /// On a hexagonal board two of the eight box neighbours are not neighbours, and which two
    /// depends on whether the PREVIOUS tile is left of, right of, or on the middle column
    /// (`leftHexNotAdj`, `rightHexNotAdj`, `midColNotAdj`, `tileboard.lua:488-498`), with
    /// `x = x1 - x2` and `y = y2 - y1`.
    #[test]
    fn hexagonal_adjacency_drops_two_diagonals_by_side() {
        let g = shape_to_geometry(crate::tables::shape("hex5").expect("hex5 is baked"));
        assert!(g.hexagonal);
        let mid = g.middle_col;
        let flat = |col: usize, row: usize| g.flat_of((col, row)).unwrap();
        // Left of the middle: x = -1 (moving right) with y = -1 is out, with y = +1 is in.
        let left = mid - 1;
        assert!(!g.tiles_adjacent(flat(left, 2), flat(left + 1, 1)));
        assert!(g.tiles_adjacent(flat(left, 2), flat(left + 1, 3)));
        // Right of the middle: x = +1 (moving left) with y = -1 is out, with y = +1 is in.
        let right = mid + 1;
        assert!(!g.tiles_adjacent(flat(right, 2), flat(right - 1, 1)));
        assert!(g.tiles_adjacent(flat(right, 2), flat(right - 1, 3)));
        // On the middle column, both diagonals with y = +1 are out; straight up is in.
        assert!(!g.tiles_adjacent(flat(mid, 2), flat(mid - 1, 3)));
        assert!(!g.tiles_adjacent(flat(mid, 2), flat(mid + 1, 3)));
        assert!(g.tiles_adjacent(flat(mid, 2), flat(mid - 1, 1)));
        assert!(g.tiles_adjacent(flat(mid, 2), flat(mid, 3)));
    }

    #[test]
    fn a_hexagonal_board_has_ragged_columns() {
        // A 5x4 hexagonal board with colTileCounts {3,3,4,3,3} holds sixteen tiles -- exactly as
        // many as the default 4x4. The count alone cannot tell them apart, which is why the shape
        // comes from the passive rather than a guess. Read from the baked table, since that is what
        // the runtime uses; `crate::tables` is where it is checked against the game.
        let hex = crate::tables::shape("hench").expect("hench is baked");
        let g = shape_to_geometry(hex);
        assert_eq!(g.rows_per_col, vec![3, 3, 4, 3, 3]);
        assert_eq!(g.total_tiles(), 16, "same tile count as the default board");
        assert_ne!(g.corner_indices(), Geometry::default().corner_indices());
    }

    #[test]
    fn sixteen_tiles_does_not_identify_a_board() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        // Three shapes hold exactly sixteen tiles and disagree about where the corners are -- and
        // `hex4` is even 4x4. The tile count carries no information about the shape, which is why the
        // geometry is read from the passive instead of inferred. Their own names are the control:
        // `diamond16Board` and `tall16Board` say sixteen, so a derivation that produced any other
        // number would be wrong about the column heights.
        let sixteens = ["diamond16Board", "tall16Board"];
        for key in sixteens {
            let r = Geometry::for_passive(key, 16);
            assert!(r.problems.is_empty(), "{key}: {:?}", r.problems);
            assert_eq!(r.geometry.total_tiles(), 16, "{key} should hold 16 tiles");
        }
        let default = Geometry::default();
        for key in sixteens {
            let g = Geometry::for_passive(key, 16).geometry;
            assert_ne!(g.corner_indices(), default.corner_indices(), "{key} corners must differ");
        }
    }

    #[test]
    fn corner_counts_are_not_always_four() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        // The `resistCornerless` nerf is `used / cornerCount`, so a hardcoded 4 would mis-scale
        // every word on a hexagonal board -- reporting 4/4 = full damage for a word that used four
        // of six corners and actually does two thirds.
        let hex5 = Geometry::for_passive("hex5", 19).geometry;
        assert_eq!(hex5.corner_count(), 6, "hexagonal boards have six corners");
        assert_eq!(Geometry::default().corner_count(), 4);
    }

    #[test]
    fn a_shape_that_does_not_fit_the_dump_is_reported() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        // Silently accepting a mismatch would place corners on the wrong letters, and a corner nerf
        // computed from wrong corners is worse than no corner model at all.
        let save = crate::game::save::parse(r#"return { passives = {} }"#).unwrap();
        let r = Geometry::from_save(&save, 20);
        assert!(!r.problems.is_empty(), "a 20-tile dump does not fit a 4x4 board");
    }

    #[test]
    fn gear_flags_lock_rows_and_columns_and_flag_adjacency() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        let save = crate::game::save::parse(
            r#"return { passives = {}, rpg = { player = { gearFlags = {
                tileboardUnselectableRow2 = 1,
                tileboardUnselectableCol3 = 1,
                wordRequirementAdjacent = 1,
            } } } }"#,
        )
        .unwrap();
        let r = Geometry::from_save(&save, 16);
        assert!(r.geometry.locked_rows.contains(&2));
        assert!(r.geometry.locked_cols.contains(&3));
        assert!(r.geometry.adjacency_required, "the typist confines itself on this flag");
    }
}

#[cfg(test)]
mod ragged_board_tests {
    use super::*;

    /// The live 15-tile board from a village inn fight, with its `columns` field.
    ///
    /// On screen it was drawn as, top row first:
    ///
    /// ```text
    /// C  .  S  L
    /// T  Y  I  E
    /// R  A  P  C
    /// R  C  I  A
    /// ```
    ///
    /// One tile had fallen off the bottom of column 2, leaving that column three tall with its gap
    /// at the TOP.
    fn live_save() -> crate::game::save::Table {
        crate::game::save::parse(
            "return {\n\
             \x20 tileboard = {\n\
             \x20   \"R\", \"R\", \"T\", \"C\",\n\
             \x20   \"C\", \"A\", \"Y\",\n\
             \x20   \"I\", \"P\", \"I\", \"S\",\n\
             \x20   \"A\", \"C\", \"E\", \"L\",\n\
             \x20   columns = { 4, 3, 4, 4 },\n\
             \x20 },\n\
             }\n",
        )
        .expect("parses")
    }

    #[test]
    fn the_saves_column_heights_are_honoured() {
        let r = Geometry::from_save(&live_save(), 15);
        assert_eq!(r.geometry.rows_per_col, vec![4, 3, 4, 4]);
        assert_eq!(r.geometry.total_tiles(), 15);
        assert!(
            r.problems.is_empty(),
            "no complaint for a legitimately ragged board: {:?}",
            r.problems
        );
    }

    #[test]
    fn indices_land_in_the_right_columns() {
        let g = Geometry::from_save(&live_save(), 15).geometry;
        // Column-major, bottom-to-top. Verified letter by letter against the drawn board.
        assert_eq!(g.position(0), Some((1, 1)), "first R, bottom of column 1");
        assert_eq!(g.position(3), Some((1, 4)), "C, top of column 1");
        assert_eq!(g.position(4), Some((2, 1)), "C, bottom of the SHORT column");
        assert_eq!(g.position(6), Some((2, 3)), "Y, top of the short column - row 4 is empty");
        assert_eq!(g.position(7), Some((3, 1)), "I, bottom of column 3");
        assert_eq!(g.position(8), Some((3, 2)), "the wood P");
        assert_eq!(g.position(14), Some((4, 4)), "L, top of column 4");
        assert_eq!(g.position(15), None, "there is no sixteenth tile");
    }

    #[test]
    fn a_full_board_is_unaffected() {
        // No `columns` key at all, which is the normal case -- the game only writes it when the
        // board is short (tileboard.lua:2457).
        let save =
            crate::game::save::parse("return { tileboard = { \"A\", \"B\", \"C\", \"D\" } }")
                .expect("parses");
        let r = Geometry::from_save(&save, 16);
        assert_eq!(r.geometry.rows_per_col, vec![4, 4, 4, 4]);
    }

    #[test]
    fn the_short_column_puts_its_gap_at_the_top() {
        // Row 0 is the bottom (crate::layout::tile_centres), so column 2's three tiles occupy the
        // bottom three cells. The proof that matters: the top of the short column is HIGHER on
        // screen than the bottom, and there is no tile above it.
        let g = Geometry::from_save(&live_save(), 15).geometry;
        let centres = crate::layout::tile_centres(&g, 1920, 1080);
        assert_eq!(centres.len(), 15);
        let bottom_of_short = centres[4]; // C
        let top_of_short = centres[6]; // Y
        assert!(top_of_short.1 < bottom_of_short.1, "row 3 sits above row 1");
        // And column 1, which is full, reaches higher still than the short column's top.
        let top_of_full = centres[3]; // C
        assert!(
            top_of_full.1 < top_of_short.1,
            "the full column has a tile where column 2 has a gap"
        );
    }

    /// A board that has lost a corner is playable, not broken — and against the shield boss it has
    /// a ceiling.
    ///
    /// The dev, 2026-08-15: *there's a possibility that we will have a bomb tile or an exclamation
    /// tile cause our board to be not full when we encounter the shield boss.* The anomaly run that
    /// evening printed `WARNING a corner coordinate falls outside the board` on three of nine turns
    /// while playing perfectly well, which is a warning that trains you to ignore warnings.
    ///
    /// Two assertions, and the second is the one with teeth: the geometry is **not** a guess (so
    /// scores stay trustworthy), and the ceiling against `resistCornerless` is said out loud —
    /// because `getCornerCount` is `#corners` whether or not the tiles are still there
    /// (`utils/words.lua:238-240`), so a lost corner caps damage below full for the whole fight and
    /// no word will ever be found that reaches it.
    #[test]
    fn a_corner_that_fell_off_is_a_ceiling_and_not_a_fault() {
        // Fifteen tiles on a board whose shape is four columns of four: the last column is a tile
        // short, which is exactly what a `!` tile reaching the bottom leaves behind.
        let save = crate::game::save::parse(
            r#"return { passives = {}, rpg = { player = { gearFlags = {} } },
                 tileboard = { columns = { 4, 4, 4, 3 } } }"#,
        )
        .unwrap();
        let r = Geometry::from_save(&save, 15);
        assert_eq!(r.geometry.rows_per_col, vec![4, 4, 4, 3], "the save's own heights");
        assert!(r.problems.is_empty(), "a ragged board is not a broken one: {:?}", r.problems);
        // The default shape corners include (4,4), which no longer exists.
        let gone = r.geometry.corners.iter().filter(|&&c| r.geometry.flat_of(c).is_none()).count();
        assert!(gone > 0, "the fixture must actually lose a corner, or it tests nothing");
        assert_eq!(r.notes.len(), 1, "and it is reported: {:?}", r.notes);
        assert!(r.notes[0].contains("resistCornerless"), "named for the fight it matters in");
        // The denominator does not shrink with the board — that is the game's rule and the whole
        // reason this is worth saying.
        assert_eq!(r.geometry.corner_count(), r.geometry.corners.len());
    }
}
