//! Where each tile is **on screen**.
//!
//! This is the piece that makes mouse input possible, and it is deliberately *derived* rather than
//! measured. Hardcoding a tile-index → pixel table would be correct for the default 4×4 and wrong
//! for every other template: `items/boardshapes.lua` has 5×3, 5×5, three hexagonal shapes with
//! ragged columns, and a 7×4 diamond. Clicking a hardcoded grid on one of those would select the
//! wrong tile silently, which in combat means typing a word the board cannot play.
//!
//! ## The chain
//!
//! Two functions compose, both from the game:
//!
//! ```text
//! generateHotspot     (tileboard.lua:20-27)   tile (col,row) -> normalized offset in the board object
//! input.hotspotScreen (utils/input.lua:80-87) normalized offset -> screen pixels
//! ```
//!
//! and the board object is created as `require'tileboard'(0.5, 1, 0, -0.5, true)` (`rpg.lua:283`),
//! which fixes the four multipliers: `ssXmult = 0.5`, `ssYmult = 1`, `osXmult = 0`, `osYmult = -0.5`.
//! Substituting those collapses the general form to:
//!
//! ```text
//! x = w/2 + boardWidth  * hx * scale
//! y = h   - boardHeight * (0.5 - hy) * scale
//! ```
//!
//! with `scale = min(w/1920, h/1080)` (the default `scaleType`, `utils/input.lua:3-14`).
//!
//! ## Checked against a real frame
//!
//! On a 1920×1080 client with the default board this puts the bottom-left tile at (783, 942) and the
//! top-right at (1137, 588). Both land on the tiles in a captured combat frame, which is the only
//! reason to believe the derivation at all.

use crate::geometry::{Geometry, FOOTER_DEPTH, HEADING_DEPTH, TILE_SIZE};

/// The board object's drawn size in game units.
///
/// Width is `tileSize * tilesX`; height adds the frame above and below the tiles
/// (`tileboard.lua:107-109`). Height uses `tiles_y`, the board's *nominal* height, not the tallest
/// column — on a hexagonal board those differ and the shorter one would shift every tile.
pub fn board_size(g: &Geometry) -> (f64, f64) {
    let width = TILE_SIZE * g.rows_per_col.len() as f64;
    let height = TILE_SIZE * g.tiles_y as f64 + HEADING_DEPTH + FOOTER_DEPTH;
    (width, height)
}

/// `min(w/1920, h/1080)`, the default `scaleType` (`utils/input.lua:13`).
pub fn scale(client_w: i32, client_h: i32) -> f64 {
    (client_w as f64 / 1920.0).min(client_h as f64 / 1080.0)
}

/// The centre of every tile, in **client** pixels, indexed the same way the board dump is.
///
/// Column-major with row 1 at the bottom, matching [`Geometry::position`].
pub fn tile_centres(g: &Geometry, client_w: i32, client_h: i32) -> Vec<(i32, i32)> {
    let (bw, bh) = board_size(g);
    let s = scale(client_w, client_h);
    let mut out = Vec::with_capacity(g.total_tiles());

    for (ci, &rows) in g.rows_per_col.iter().enumerate() {
        // `generateHotspot` takes 0-based indices.
        let x0 = ci as f64;
        let y_offset = g.col_y_offsets.get(ci).copied().unwrap_or(0.0);
        for ri in 0..rows {
            let y0 = ri as f64;
            let hx = (TILE_SIZE / 2.0 + TILE_SIZE * x0) / bw - 0.5;
            let hy = (bh - (FOOTER_DEPTH + y_offset + TILE_SIZE / 2.0 + TILE_SIZE * y0)) / bh - 0.5;
            let x = client_w as f64 / 2.0 + bw * hx * s;
            let y = client_h as f64 - bh * (0.5 - hy) * s;
            out.push((x.round() as i32, y.round() as i32));
        }
    }
    out
}

/// Half the on-screen width of a tile — the radius within which a click still lands on it.
///
/// Useful as a tolerance: a mapping that is off by less than this still clicks the right tile, and
/// one that is off by more will silently select a neighbour.
pub fn tile_radius(client_w: i32, client_h: i32) -> f64 {
    TILE_SIZE * scale(client_w, client_h) / 2.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn game_dir() -> PathBuf {
        PathBuf::from("../sternly-worded-adventures")
    }

    #[test]
    fn matches_a_real_combat_frame() {
        // Measured off `spike-frames-live/word-selected-AT.png`, a genuine 1920x1080 combat frame.
        // These two corners pin both the origin and the scale; getting either wrong moves them in
        // opposite directions, so agreeing on both is not a coincidence.
        let c = tile_centres(&Geometry::default(), 1920, 1080);
        assert_eq!(c.len(), 16);
        assert_eq!(c[0], (783, 942), "bottom-left, dump index 0");
        assert_eq!(c[15], (1137, 588), "top-right, dump index 15");
    }

    #[test]
    fn tiles_are_one_tile_apart() {
        // A uniform pitch of exactly TILE_SIZE at scale 1. If the pitch were wrong the corners could
        // still match while everything between them drifted.
        let c = tile_centres(&Geometry::default(), 1920, 1080);
        assert_eq!(c[1].1 - c[0].1, -(TILE_SIZE as i32), "row 2 is one tile ABOVE row 1");
        assert_eq!(c[4].0 - c[0].0, TILE_SIZE as i32, "column 2 is one tile right");
        assert_eq!(c[0].0, c[3].0, "a column shares an x");
        assert_eq!(c[0].1, c[4].1, "a row shares a y");
    }

    #[test]
    fn the_board_is_centred_horizontally() {
        let c = tile_centres(&Geometry::default(), 1920, 1080);
        let left = c[0].0 as f64;
        let right = c[15].0 as f64;
        assert!(((left + right) / 2.0 - 960.0).abs() < 1.0, "centre {left}..{right}");
    }

    #[test]
    fn scaling_the_window_scales_the_layout() {
        // `scaleType` defaults to min(w/1920, h/1080), so a half-size window halves the offsets from
        // the anchor. The anchor is bottom-centre, which is why x tracks w/2 and y tracks h.
        let full = tile_centres(&Geometry::default(), 1920, 1080);
        let half = tile_centres(&Geometry::default(), 960, 540);
        assert!(((960.0 - full[0].0 as f64) / 2.0 - (480.0 - half[0].0 as f64)).abs() < 1.0);
        assert!(((1080.0 - full[0].1 as f64) / 2.0 - (540.0 - half[0].1 as f64)).abs() < 1.0);
    }

    #[test]
    fn a_different_board_shape_moves_every_tile() {
        if !game_dir().join("items/boardshapes.lua").is_file() {
            eprintln!("SKIP: game source not present");
            return;
        }
        // The user's point: each template has its own pixel positions. A 5x3 board is wider and
        // shorter, so a layout hardcoded for 4x4 would click the wrong tiles on it.
        let stout = crate::geometry::Geometry::for_passive("stoutCharacter", 15).geometry;
        let c = tile_centres(&stout, 1920, 1080);
        assert_eq!(c.len(), 15);
        assert_eq!(stout.rows_per_col.len(), 5);
        let default = tile_centres(&Geometry::default(), 1920, 1080);
        assert_ne!(c[0], default[0], "a 5-wide board starts further left");
        // Still centred, and still one tile apart.
        assert!(((c[0].0 + c[14].0) as f64 / 2.0 - 960.0).abs() < 1.0);
        assert_eq!(c[3].0 - c[0].0, TILE_SIZE as i32, "5x3: index 3 opens column 2");
    }

    #[test]
    fn a_hexagonal_board_offsets_its_short_columns() {
        if !game_dir().join("items/boardshapes.lua").is_file() {
            eprintln!("SKIP: game source not present");
            return;
        }
        // `colPosYOffsets` centres short columns instead of bottom-aligning them
        // (`tileboard.lua:96-98`). Ignoring it would put every tile in a short column half a tile
        // too low -- within a tile's height, so it would select the WRONG NEIGHBOUR rather than
        // missing, which is the failure that never announces itself.
        let tall = crate::geometry::Geometry::for_passive("tall16Board", 16).geometry;
        assert!(tall.col_y_offsets.iter().any(|&o| o != 0.0), "expected offsets: {:?}", tall.col_y_offsets);
        let c = tile_centres(&tall, 1920, 1080);
        assert_eq!(c.len(), 16);
        // Column 1 is short (5 of 6) and therefore raised relative to the full middle column.
        let col1_bottom = c[0].1;
        let col2_bottom = c[tall.rows_per_col[0]].1;
        assert!(col1_bottom < col2_bottom, "short column should sit higher: {col1_bottom} vs {col2_bottom}");
    }
}
