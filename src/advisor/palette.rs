//! Tile colours, taken from the game's own tile textures.
//!
//! Each material has a sprite sheet at `ui/graphics/tiles/<material>.png`, named after the
//! material files in `rpg/effects/material/`: a 7-column grid of 118-pixel cells on a 119-pixel
//! pitch, in the order `.ABCDEF / GHIJKLM / …` (`ui/objects/tile.lua:33-38`). Two samples are
//! taken per sheet, both as per-channel medians so grain and highlights do not drag them:
//!
//! - **the face**, from the middle of the blank wildcard cell (`.`), above its `[A-Z]` caption.
//!   Not the whole sheet: that mixes in bevels, shadows and the letters, and it is what made plain,
//!   braced and inlaid wood come out nearly the same brown.
//! - **the ink**, from the stem of the `I`, which is solid lettering in every face the game uses —
//!   black on wood, gold on inlaid wood.
//!
//! Sampled from the checkout rather than written down here, so a re-coloured material follows.

use std::collections::HashMap;
use std::path::Path;

/// The colour for a material with no texture, or none at all.
pub const GREY: (u8, u8, u8) = (128, 128, 128);

/// The cell pitch and size of every tile sheet (`ui/objects/tile.lua:36`).
const PITCH: usize = 119;

/// Within the blank `.` cell at (0, 0): the face, clear of the bevel and of the caption.
const FACE: (usize, usize, usize, usize) = (25, 20, 95, 70);
/// Within the `I` cell at column 2, row 1: a narrow strip down the stem.
const INK: (usize, usize, usize, usize) = (2 * PITCH + 55, PITCH + 35, 2 * PITCH + 63, PITCH + 85);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Swatch {
    face: (u8, u8, u8),
    ink: (u8, u8, u8),
}

#[derive(Debug, Clone, Default)]
pub struct Palette {
    swatches: HashMap<String, Swatch>,
}

impl Palette {
    /// Samples every material the game defines. A texture that cannot be read is left out, and
    /// that material shows as [`GREY`].
    pub fn load(game_dir: &Path) -> Self {
        let mut swatches = HashMap::new();
        let Ok(entries) = std::fs::read_dir(game_dir.join("rpg/effects/material")) else {
            return Palette { swatches };
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().trim_end_matches(".lua").to_string();
            let png = game_dir.join("ui/graphics/tiles").join(format!("{name}.png"));
            if let Some(s) = sample(&png) {
                swatches.insert(name, s);
            }
        }
        Palette { swatches }
    }

    /// A material's face colour, or [`GREY`].
    pub fn colour(&self, material: Option<&str>) -> (u8, u8, u8) {
        material.and_then(|m| self.swatches.get(m)).map(|s| s.face).unwrap_or(GREY)
    }

    /// How a tile of `material` is drawn: its own face and ink, and whether it is special.
    ///
    /// Special means a variant or a gem, ringed in the list so it stands apart. Variants are named
    /// by suffix on their family (`categories.material` in `rpg/effects/material/*.lua`): `braced`
    /// and `ornate` wood, `bleed` and `toxin` metals. Each keeps its own colours — braced and
    /// inlaid wood look nothing like plain wood in the game, so they must not here either.
    pub fn face(&self, material: Option<&str>) -> Face {
        let swatch = material.and_then(|m| self.swatches.get(m)).copied();
        let special = material.is_some_and(|m| {
            m.starts_with("gem")
                || ["braced", "ornate", "bleed", "toxin"]
                    .iter()
                    .any(|suffix| m.strip_suffix(suffix).is_some_and(|f| !f.is_empty()))
        });
        match swatch {
            Some(s) => Face { colour: s.face, ink: readable(s.face, s.ink), special },
            None => Face { colour: GREY, ink: (0, 0, 0), special },
        }
    }
}

/// A tile's face in the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Face {
    pub colour: (u8, u8, u8),
    /// The lettering colour the game prints on this material.
    pub ink: (u8, u8, u8),
    /// A variant or gem: drawn with a ring so it stands apart from a plain tile.
    pub special: bool,
}

/// The least contrast lettering may have against its face: WCAG's 3:1 for large text, which
/// the list's lettering is.
const MIN_CONTRAST: f64 = 3.0;

/// The game's ink if it reads against `face`, else black or white, whichever reads better.
///
/// Some faces rely on effects the list cannot draw: the gems print near-black letters on a dark
/// face and lift them with a glow (`ui/graphics/tiles/gemfire_glow.png`).
fn readable(face: (u8, u8, u8), ink: (u8, u8, u8)) -> (u8, u8, u8) {
    if contrast(face, ink) >= MIN_CONTRAST {
        return ink;
    }
    let (black, white) = ((0, 0, 0), (255, 255, 255));
    if contrast(face, black) >= contrast(face, white) {
        black
    } else {
        white
    }
}

/// WCAG contrast ratio between two sRGB colours, 1 to 21.
pub fn contrast(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
    let lum = |(r, g, b): (u8, u8, u8)| {
        let lin = |c: u8| {
            let c = c as f64 / 255.0;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
    };
    let (x, y) = (lum(a), lum(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

fn sample(path: &Path) -> Option<Swatch> {
    let mut dec = png::Decoder::new(std::fs::File::open(path).ok()?);
    // Palette sheets expand to RGB(A); 16-bit channels are cut to 8. Either way, 8-bit samples.
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let channels = info.color_type.samples();
    let width = info.width as usize;
    let pixels = &buf[..info.buffer_size()];
    let median_in = |(x0, y0, x1, y1): (usize, usize, usize, usize)| -> Option<(u8, u8, u8)> {
        let mut hist = [[0u64; 256]; 3];
        let mut n = 0u64;
        for y in y0..y1 {
            for x in x0..x1 {
                let at = (y * width + x) * channels;
                let px = pixels.get(at..at + channels)?;
                for (c, h) in hist.iter_mut().enumerate() {
                    h[px[c.min(channels.saturating_sub(1)).min(2)] as usize] += 1;
                }
                n += 1;
            }
        }
        let median = |h: &[u64; 256]| {
            let mut seen = 0;
            h.iter().position(|&k| {
                seen += k;
                seen * 2 >= n
            })
        };
        Some((median(&hist[0])? as u8, median(&hist[1])? as u8, median(&hist[2])? as u8))
    };
    Some(Swatch { face: median_in(FACE)?, ink: median_in(INK)? })
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

    fn distance(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
        let d = |x: u8, y: u8| (x as f64 - y as f64).powi(2);
        (d(a.0, b.0) + d(a.1, b.1) + d(a.2, b.2)).sqrt()
    }

    fn luma((r, g, b): (u8, u8, u8)) -> f64 {
        0.299 * r as f64 + 0.587 * g as f64 + 0.114 * b as f64
    }

    /// Plain, braced and inlaid wood look nothing alike in the game, and must not here.
    #[test]
    fn wood_braced_and_inlaid_are_told_apart() {
        let p = Palette::load(&game_dir());
        let face = |m| p.face(Some(m)).colour;
        let (wood, braced, inlaid) = (face("wood"), face("woodbraced"), face("woodornate"));
        for (a, b, what) in [
            (wood, braced, "wood/braced"),
            (wood, inlaid, "wood/inlaid"),
            (braced, inlaid, "braced/inlaid"),
        ] {
            assert!(distance(a, b) >= 25.0, "{what}: {a:?} vs {b:?}");
        }
    }

    /// The letters are the texture's own: black on wood and braced wood, gold on inlaid.
    #[test]
    fn ink_is_the_textures_lettering() {
        let p = Palette::load(&game_dir());
        let ink = |m| p.face(Some(m)).ink;
        assert!(luma(ink("wood")) < 80.0, "wood ink {:?}", ink("wood"));
        assert!(luma(ink("woodbraced")) < 80.0, "braced ink {:?}", ink("woodbraced"));
        assert!(luma(ink("woodornate")) > 120.0, "inlaid ink {:?}", ink("woodornate"));
    }

    /// Where the game's lettering leans on a glow the list cannot draw — dark letters on a dark
    /// gem — the ink falls back to black or white so it stays readable.
    #[test]
    fn ink_is_always_readable_against_its_face() {
        let p = Palette::load(&game_dir());
        for m in ["gemfire", "gemice", "gemheal", "wood", "woodornate", "gold", "bad"] {
            let f = p.face(Some(m));
            assert!(contrast(f.colour, f.ink) >= 3.0, "{m}: {:?} on {:?}", f.ink, f.colour);
        }
        assert_eq!(p.face(Some("wood")).ink, p.swatches["wood"].ink, "readable ink is kept");
    }

    /// Variants and gems are ringed; plain tiles are not.
    #[test]
    fn variants_and_gems_are_marked_special() {
        let p = Palette::load(&game_dir());
        for m in ["woodbraced", "woodornate", "bronzebleed", "silvertoxin", "gemheal"] {
            assert!(p.face(Some(m)).special, "{m}");
        }
        for m in ["wood", "bronze", "silver3", "gold"] {
            assert!(!p.face(Some(m)).special, "{m}");
        }
    }

    #[test]
    fn missing_material_is_grey() {
        let p = Palette::load(&game_dir());
        assert_eq!(p.colour(Some("no-such-material")), GREY);
        assert_eq!(p.colour(None), GREY);
    }
}
