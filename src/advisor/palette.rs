//! Tile colours, taken from the game's own tile textures.
//!
//! Each material has a sprite sheet at `ui/graphics/tiles/<material>.png`, named after the
//! material files in `rpg/effects/material/`. The colour shown for a material is the **per-channel
//! median of the sheet's opaque pixels**: the tile face is most of every cell, so the median lands
//! on it, where a mean would be dragged toward the dark lettering printed on top. Sampled from the
//! checkout rather than written down here, so a re-coloured material follows the game.

use std::collections::HashMap;
use std::path::Path;

/// The colour for a material with no texture, or none at all.
pub const GREY: (u8, u8, u8) = (128, 128, 128);

/// Pixels with less alpha than this are the sheet's background, not a tile.
const OPAQUE: u8 = 128;

#[derive(Debug, Clone, Default)]
pub struct Palette {
    colours: HashMap<String, (u8, u8, u8)>,
}

impl Palette {
    /// Samples every material the game defines. A texture that cannot be read is left out, and
    /// that material shows as [`GREY`].
    pub fn load(game_dir: &Path) -> Self {
        let mut colours = HashMap::new();
        let Ok(entries) = std::fs::read_dir(game_dir.join("rpg/effects/material")) else {
            return Palette { colours };
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().trim_end_matches(".lua").to_string();
            let png = game_dir.join("ui/graphics/tiles").join(format!("{name}.png"));
            if let Some(c) = median_colour(&png) {
                colours.insert(name, c);
            }
        }
        Palette { colours }
    }

    pub fn colour(&self, material: Option<&str>) -> (u8, u8, u8) {
        material.and_then(|m| self.colours.get(m)).copied().unwrap_or(GREY)
    }
}

fn median_colour(path: &Path) -> Option<(u8, u8, u8)> {
    let mut dec = png::Decoder::new(std::fs::File::open(path).ok()?);
    // Palette sheets expand to RGB(A); 16-bit channels are cut to 8. Either way, 8-bit samples.
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let channels = info.color_type.samples();
    let mut hist = [[0u64; 256]; 3];
    let mut n = 0u64;
    for px in buf[..info.buffer_size()].chunks_exact(channels) {
        let (rgb, alpha) = match channels {
            4 => (&px[..3], px[3]),
            3 => (&px[..3], 255),
            2 => (&px[..1], px[1]),
            _ => (&px[..1], 255),
        };
        if alpha < OPAQUE {
            continue;
        }
        for c in 0..3 {
            hist[c][rgb[c.min(rgb.len() - 1)] as usize] += 1;
        }
        n += 1;
    }
    if n == 0 {
        return None;
    }
    let median = |h: &[u64; 256]| {
        let mut seen = 0;
        h.iter().position(|&k| {
            seen += k;
            seen * 2 >= n
        })
    };
    Some((median(&hist[0])? as u8, median(&hist[1])? as u8, median(&hist[2])? as u8))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn game_dir() -> PathBuf {
        PathBuf::from("../sternly-worded-adventures")
    }

    #[test]
    fn wood_and_gold_differ() {
        let p = Palette::load(&game_dir());
        let (wood, gold) = (p.colour(Some("wood")), p.colour(Some("gold")));
        assert_ne!(wood, GREY, "wood was sampled");
        assert_ne!(gold, GREY, "gold was sampled");
        assert_ne!(wood, gold);
    }

    #[test]
    fn missing_material_is_grey() {
        let p = Palette::load(&game_dir());
        assert_eq!(p.colour(Some("no-such-material")), GREY);
        assert_eq!(p.colour(None), GREY);
    }
}
