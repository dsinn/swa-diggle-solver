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

use crate::game::save::{parse_module, Table, Value};
use std::collections::BTreeSet;
use std::path::Path;

/// Default board: 4 columns of 4 (`tileboard.lua:71-72`), corners in the order the game lists them
/// (`tileboard.lua:64-69`) — that order is the tie-break the typist uses, so it is preserved.
const DEFAULT_CORNERS: [(usize, usize); 4] = [(1, 1), (4, 1), (1, 4), (4, 4)];

/// The board's shape and which slots are locked.
#[derive(Debug, Clone, PartialEq)]
pub struct Geometry {
    /// Tile count per column, in dump order.
    pub rows_per_col: Vec<usize>,
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
    pub fn for_passive(game_dir: &Path, passive: &str, tile_count: usize) -> Resolved {
        let save = crate::game::save::parse(&format!("return {{ passives = {{ {passive:?} }} }}"))
            .expect("a literal table always parses");
        Geometry::from_save(game_dir, &save, tile_count)
    }

    /// Derives the geometry from a `combatSaveData` table, the way `generateTileboardData` does.
    ///
    /// `tile_count` is the length of the board dump, used only as a check: a shape that does not
    /// account for exactly that many tiles is reported rather than used, because a wrong shape puts
    /// the corners on the wrong letters and that is worse than admitting ignorance.
    pub fn from_save(game_dir: &Path, save: &Table, tile_count: usize) -> Resolved {
        let mut problems = Vec::new();
        let mut geometry = Geometry::default();

        let passives: Vec<&str> = save
            .table_at("passives")
            .map(|t| t.arr.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        if !passives.is_empty() {
            match board_shapes(game_dir) {
                Ok(shapes) => {
                    // `tileboard.lua:74-89` takes the FIRST passive with boardData and breaks.
                    for key in &passives {
                        if let Some(shape) = shapes.table_at(key).and_then(|i| i.table_at("boardData"))
                        {
                            match shape_to_geometry(shape) {
                                Some(g) => geometry = g,
                                None => problems.push(format!(
                                    "passive {key} has boardData this cannot read"
                                )),
                            }
                            break;
                        }
                    }
                }
                Err(e) => problems.push(format!("could not read items/boardshapes.lua: {e}")),
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

fn board_shapes(game_dir: &Path) -> Result<Table, crate::Error> {
    let src = std::fs::read_to_string(game_dir.join("items/boardshapes.lua"))?;
    parse_module(&src)
}

/// Reads one `boardData` entry into a geometry, mirroring `tileboard.lua:76-105`.
fn shape_to_geometry(shape: &Table) -> Option<Geometry> {
    let size = shape.table_at("boardSize")?;
    let cols = size.arr.first()?.as_int()? as usize;
    let rows = size.arr.get(1)?.as_int()? as usize;
    if cols == 0 || rows == 0 {
        return None;
    }

    let hexagonal = matches!(shape.get("hexagonal"), Some(Value::Bool(true)));
    let middle = shape
        .int_at("middleCol")
        .map(|m| m as usize)
        // `math.floor((tilesX-1)/2+1)`
        .unwrap_or((cols - 1) / 2 + 1);

    // `colTileCounts[i] = boardItemData.colTileCounts[i] or baseColTileCount`, where the hexagonal
    // base narrows by distance from the middle column (`tileboard.lua:92-103`).
    let explicit = shape.table_at("colTileCounts");
    let mut rows_per_col = Vec::with_capacity(cols);
    for i in 1..=cols {
        let given = explicit.and_then(|t| t.arr.get(i - 1)).and_then(|v| v.as_int());
        let base = if hexagonal {
            rows.saturating_sub(middle.abs_diff(i))
        } else {
            rows
        };
        rows_per_col.push(given.map(|n| n as usize).unwrap_or(base));
    }

    let corners = shape
        .table_at("corners")?
        .arr
        .iter()
        .filter_map(|v| {
            let t = v.as_table()?;
            Some((t.arr.first()?.as_int()? as usize, t.arr.get(1)?.as_int()? as usize))
        })
        .collect::<Vec<_>>();
    if corners.is_empty() {
        return None;
    }

    Some(Geometry {
        rows_per_col,
        corners,
        locked_rows: BTreeSet::new(),
        locked_cols: BTreeSet::new(),
        adjacency_required: false,
    })
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
        let r = Geometry::from_save(&game_dir(), &save, 16);
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
        let r = Geometry::from_save(&game_dir(), &save, 15);
        assert!(r.problems.is_empty(), "problems: {:?}", r.problems);
        assert_eq!(r.geometry.rows_per_col, vec![3, 3, 3, 3, 3]);
        assert_eq!(r.geometry.corners, vec![(1, 1), (5, 1), (1, 3), (5, 3)]);
        assert_eq!(r.geometry.corner_indices(), vec![0, 12, 2, 14]);
    }

    #[test]
    fn a_hexagonal_board_has_ragged_columns() {
        if !present() {
            eprintln!("SKIP: game source not present");
            return;
        }
        // A 5x4 hexagonal board with colTileCounts {3,3,4,3,3} holds sixteen tiles -- exactly as
        // many as the default 4x4. The count alone cannot tell them apart, which is why the shape
        // is derived from the passive rather than guessed.
        let shapes = board_shapes(&game_dir()).unwrap();
        let hex = shapes
            .map
            .values()
            .filter_map(|v| v.as_table()?.table_at("boardData"))
            .find(|b| {
                b.table_at("colTileCounts").map(|c| c.arr.len() == 5).unwrap_or(false)
                    && matches!(b.get("hexagonal"), Some(Value::Bool(true)))
            })
            .expect("a hexagonal 5-column board exists");
        let g = shape_to_geometry(hex).expect("readable");
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
            let r = Geometry::for_passive(&game_dir(), key, 16);
            assert!(r.problems.is_empty(), "{key}: {:?}", r.problems);
            assert_eq!(r.geometry.total_tiles(), 16, "{key} should hold 16 tiles");
        }
        let default = Geometry::default();
        for key in sixteens {
            let g = Geometry::for_passive(&game_dir(), key, 16).geometry;
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
        let hex5 = Geometry::for_passive(&game_dir(), "hex5", 19).geometry;
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
        let r = Geometry::from_save(&game_dir(), &save, 20);
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
        let r = Geometry::from_save(&game_dir(), &save, 16);
        assert!(r.geometry.locked_rows.contains(&2));
        assert!(r.geometry.locked_cols.contains(&3));
        assert!(r.geometry.adjacency_required, "must be surfaced, it is not modelled");
    }
}
