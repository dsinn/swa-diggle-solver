//! Game data, baked in.
//!
//! These are the numbers Diggle needs to place tiles and score words. They were **derived once**
//! from the game's own files and written down here, so the runtime never evaluates Lua to learn a
//! board shape or a material score. Nothing on the hot path parses `items/boardshapes.lua`.
//!
//! ## Why this is safe to hardcode, and how it stays honest
//!
//! Baked constants drift silently when the game changes — that is the real cost, and it is paid for
//! by the tests beside each table. When the game source is present, they re-derive the same values
//! from it and assert the baked copy still matches. So the runtime is fast and source-free, while a
//! version bump that moves a corner or reprices a material fails the suite instead of mis-clicking a
//! tile in combat.
//!
//! Regenerate with `cargo run --bin gen_tables` and paste the output back here.
//!
//! Derived from **v52.3**.

/// One board template from `items/boardshapes.lua`, keyed by the passive that grants it.
#[derive(Debug, Clone, Copy)]
pub struct Shape {
    pub key: &'static str,
    pub cols: usize,
    pub rows: usize,
    pub hexagonal: bool,
    /// `middleCol`, or 0 when the file omits it and the default `floor((cols-1)/2+1)` applies.
    pub middle_col: usize,
    /// Explicit per-column heights; empty means "derive from the hexagonal base".
    pub col_tile_counts: &'static [usize],
    /// 1-based `(col, row)`, **in the game's order** — the order the typist tries corners in.
    pub corners: &'static [(usize, usize)],
}

/// Every alternate board in the game. The default 4×4 is not here: it is
/// [`crate::geometry::Geometry::default`], since it applies when no passive grants a shape.
pub const SHAPES: &[Shape] = &[
    Shape { key: "diamond16Board", cols: 7, rows: 4, hexagonal: true, middle_col: 0, col_tile_counts: &[], corners: &[(1,1),(4,1),(4,4),(7,1)] },
    Shape { key: "erudite", cols: 5, rows: 5, hexagonal: false, middle_col: 0, col_tile_counts: &[], corners: &[(1,1),(5,1),(1,5),(5,5)] },
    Shape { key: "hench", cols: 5, rows: 4, hexagonal: true, middle_col: 3, col_tile_counts: &[3,3,4,3,3], corners: &[(1,1),(3,1),(5,1),(1,3),(5,3)] },
    Shape { key: "hex4", cols: 4, rows: 4, hexagonal: true, middle_col: 2, col_tile_counts: &[], corners: &[(1,1),(2,1),(4,1),(1,3),(2,4),(4,2)] },
    Shape { key: "hex5", cols: 5, rows: 5, hexagonal: true, middle_col: 3, col_tile_counts: &[], corners: &[(1,1),(3,1),(5,1),(1,3),(3,5),(5,3)] },
    Shape { key: "stoutCharacter", cols: 5, rows: 3, hexagonal: false, middle_col: 0, col_tile_counts: &[], corners: &[(1,1),(5,1),(1,3),(5,3)] },
    Shape { key: "tall16Board", cols: 3, rows: 6, hexagonal: true, middle_col: 0, col_tile_counts: &[], corners: &[(1,1),(2,1),(3,1),(1,5),(2,6),(3,5)] },
];

pub fn shape(key: &str) -> Option<&'static Shape> {
    SHAPES.iter().find(|s| s.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::save::{parse_module, Value};
    use std::path::PathBuf;

    fn game_dir() -> PathBuf {
        PathBuf::from("../sternly-worded-adventures")
    }

    #[test]
    fn the_baked_shapes_still_match_the_game() {
        let path = game_dir().join("items/boardshapes.lua");
        if !path.is_file() {
            eprintln!("SKIP: game source not present");
            return;
        }
        // The whole point of baking: this is the only place the game file is read, and it is a test.
        // If a version bump moves a corner, this fails rather than letting combat click a wrong tile.
        let src = std::fs::read_to_string(&path).unwrap();
        let shapes = parse_module(&src).unwrap();

        let mut seen = 0usize;
        for (key, item) in &shapes.map {
            let Some(b) = item.as_table().and_then(|i| i.table_at("boardData")) else { continue };
            let Some(size) = b.table_at("boardSize") else { continue };
            seen += 1;

            let baked = shape(key).unwrap_or_else(|| panic!("{key} is in the game but not baked"));
            assert_eq!(baked.cols, size.arr[0].as_int().unwrap() as usize, "{key} cols");
            assert_eq!(baked.rows, size.arr[1].as_int().unwrap() as usize, "{key} rows");
            assert_eq!(
                baked.hexagonal,
                matches!(b.get("hexagonal"), Some(Value::Bool(true))),
                "{key} hexagonal"
            );
            assert_eq!(
                baked.middle_col,
                b.int_at("middleCol").unwrap_or(0) as usize,
                "{key} middleCol"
            );

            let ctc: Vec<usize> = b
                .table_at("colTileCounts")
                .map(|t| t.arr.iter().filter_map(|v| v.as_int()).map(|n| n as usize).collect())
                .unwrap_or_default();
            assert_eq!(baked.col_tile_counts, ctc.as_slice(), "{key} colTileCounts");

            let corners: Vec<(usize, usize)> = b
                .table_at("corners")
                .map(|t| {
                    t.arr
                        .iter()
                        .filter_map(|v| {
                            let t = v.as_table()?;
                            Some((t.arr[0].as_int()? as usize, t.arr[1].as_int()? as usize))
                        })
                        .collect()
                })
                .unwrap_or_default();
            // Order matters, not just membership: it is the typist's corner tie-break.
            assert_eq!(baked.corners, corners.as_slice(), "{key} corners (order matters)");
        }
        assert_eq!(seen, SHAPES.len(), "the game has {seen} board shapes, we baked {}", SHAPES.len());
    }

    #[test]
    fn corner_counts_are_four_to_six() {
        // A sanity floor on the baked data itself, independent of the game file: the
        // `resistCornerless` denominator comes from here, and a zero would divide by nothing.
        for s in SHAPES {
            assert!(
                (4..=6).contains(&s.corners.len()),
                "{} has {} corners",
                s.key,
                s.corners.len()
            );
        }
    }
}
