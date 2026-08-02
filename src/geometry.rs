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
    /// `wordRequirementAdjacent` gear: each letter must come from a tile next to the previous one
    /// (`tileboard.lua:2196-2202`, `:2273-2274`). **Not modelled** — surfaced so a caller can refuse
    /// rather than silently search as if the whole board were reachable.
    pub adjacency_required: bool,
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
}

impl Geometry {
    pub fn total_tiles(&self) -> usize {
        self.rows_per_col.iter().sum()
    }

    pub fn corner_count(&self) -> usize {
        self.corners.len()
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

    fn flat_of(&self, (col, row): (usize, usize)) -> Option<usize> {
        if col == 0 || row == 0 || col > self.rows_per_col.len() || row > self.rows_per_col[col - 1] {
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
            Some((col, row)) => !self.locked_cols.contains(&col) && !self.locked_rows.contains(&row),
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
                if let Some(n) = key.strip_prefix("tileboardUnselectableRow").and_then(parse_index) {
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
        if geometry.corners.iter().any(|&c| geometry.flat_of(c).is_none()) {
            problems.push("a corner coordinate falls outside the board".to_string());
        }

        Resolved { geometry, problems }
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
        let base = if hexagonal {
            rows.saturating_sub(middle.abs_diff(i))
        } else {
            rows
        };
        rows_per_col.push(given.unwrap_or(base));
        // `d` is measured against the COMPUTED base, not the override: `colTileCounts` can shorten a
        // column without moving it (`tileboard.lua:92-98`).
        col_y_offsets.push(if hexagonal {
            (rows - base) as f64 * TILE_SIZE * 0.5
        } else {
            0.0
        });
    }

    Geometry {
        rows_per_col,
        tiles_y: rows,
        col_y_offsets,
        corners: shape.corners.to_vec(),
        locked_rows: BTreeSet::new(),
        locked_cols: BTreeSet::new(),
        adjacency_required: false,
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
        let save = crate::game::save::parse(r#"return { passives = { "stoutCharacter" } }"#).unwrap();
        let r = Geometry::from_save(&save, 15);
        assert!(r.problems.is_empty(), "problems: {:?}", r.problems);
        assert_eq!(r.geometry.rows_per_col, vec![3, 3, 3, 3, 3]);
        assert_eq!(r.geometry.corners, vec![(1, 1), (5, 1), (1, 3), (5, 3)]);
        assert_eq!(r.geometry.corner_indices(), vec![0, 12, 2, 14]);
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
        assert!(r.geometry.adjacency_required, "must be surfaced, it is not modelled");
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
        assert!(r.problems.is_empty(), "no complaint for a legitimately ragged board: {:?}", r.problems);
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
        let save = crate::game::save::parse(
            "return { tileboard = { \"A\", \"B\", \"C\", \"D\" } }",
        )
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
        assert!(top_of_full.1 < top_of_short.1, "the full column has a tile where column 2 has a gap");
    }
}
