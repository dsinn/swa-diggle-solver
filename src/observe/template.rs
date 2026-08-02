//! Sprite/template matching against a captured frame.
//!
//! The game's own art is readable data: `overworld/locations/graphics/*.png` holds the
//! exact images blitted onto the overworld map (225 of them), including per-state variants
//! such as `campfire1/2/3` = fresh / complete / used. Matching those against a capture
//! locates a node AND identifies its type and state in one pass, in the screen coordinates
//! the map-pan gesture actually needs (design v2 §3: on the overworld the cursor is pinned,
//! so `GetCursorPos` cannot tell us where anything is).
//!
//! This deliberately avoids the alternative — replaying `overworld/generators/world.lua`
//! from the seed — which would require bit-exact reimplementation of `love.math.noise` and
//! LOVE's RNG, whose draws are interleaved with SpriteBatch calls.
//!
//! Matching is alpha-weighted: sprites are irregular and drawn over arbitrary terrain, so
//! only pixels the template actually paints may contribute. Scoring transparent pixels
//! would mostly measure the background and would match flat ground everywhere.

/// An RGBA template loaded from a PNG on disk.
#[derive(Debug, Clone)]
pub struct Template {
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// Row-major RGBA, 4 bytes per pixel.
    pub rgba: Vec<u8>,
}

/// One match result, in frame (client) pixels.
#[derive(Debug, Clone, Copy)]
pub struct Match {
    /// Top-left of the matched rectangle.
    pub x: i32,
    pub y: i32,
    /// Centre of the matched rectangle — what map panning needs to drive to (960, 540).
    pub cx: i32,
    pub cy: i32,
    pub scale: f64,
    /// Mean absolute per-channel error over opaque template pixels, 0.0 (exact) .. 1.0.
    /// Diagnostic only — **not** the ranking key, because it is not occlusion-robust.
    pub error: f64,
    /// Fraction of opaque template pixels that agree with the frame within
    /// `INLIER_TOLERANCE`. This is the ranking key.
    pub inliers: f64,
}

impl Template {
    pub fn load(path: &std::path::Path) -> Result<Template, Box<dyn std::error::Error>> {
        let file = std::fs::File::open(path)?;
        let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
        // The game's location sprites are palette-indexed at 2 bits/pixel. EXPAND turns
        // palette and sub-byte depths into straight RGB/RGBA (applying tRNS, which is how
        // these sprites carry their transparency); STRIP_16 normalises any 16-bit art.
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info()?;
        let mut buf = vec![0u8; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buf)?;
        buf.truncate(info.buffer_size());

        // Normalise whatever the PNG happens to be into RGBA8.
        let rgba = match (info.color_type, info.bit_depth) {
            (png::ColorType::Rgba, png::BitDepth::Eight) => buf,
            (png::ColorType::Rgb, png::BitDepth::Eight) => {
                let mut v = Vec::with_capacity(buf.len() / 3 * 4);
                for px in buf.chunks_exact(3) {
                    v.extend_from_slice(&[px[0], px[1], px[2], 255]);
                }
                v
            }
            (png::ColorType::GrayscaleAlpha, png::BitDepth::Eight) => {
                let mut v = Vec::with_capacity(buf.len() / 2 * 4);
                for px in buf.chunks_exact(2) {
                    v.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
                }
                v
            }
            (png::ColorType::Grayscale, png::BitDepth::Eight) => {
                let mut v = Vec::with_capacity(buf.len() * 4);
                for px in &buf {
                    v.extend_from_slice(&[*px, *px, *px, 255]);
                }
                v
            }
            (ct, bd) => return Err(format!("unsupported PNG format {ct:?}/{bd:?}").into()),
        };

        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok(Template { name, width: info.width, height: info.height, rgba })
    }

    /// Fraction of pixels with alpha above the opacity cutoff. A template that is mostly
    /// transparent carries little evidence, so callers should treat a low value as a
    /// reason to distrust a good-looking score.
    pub fn opaque_fraction(&self, alpha_min: u8) -> f64 {
        let n = (self.width * self.height) as usize;
        if n == 0 {
            return 0.0;
        }
        let opaque = self.rgba.chunks_exact(4).filter(|p| p[3] >= alpha_min).count();
        opaque as f64 / n as f64
    }
}

/// Minimum template alpha for a pixel to contribute to the score.
pub const ALPHA_MIN: u8 = 128;

/// Summed per-channel absolute difference below which a pixel counts as agreeing
/// (~30 per channel). Ranking on the *count* of such pixels rather than on mean error is
/// what makes matching survive partial occlusion.
///
/// Overworld nodes are routinely drawn under drifting clouds while remaining fully
/// interactive — `mouseIsOverLocation` (`overworldview.lua:1285`) rejects a location only
/// when `isCloudCovered` is true, which is a per-location state, not "some cloud pixels
/// overlap the sprite". A mean-error score would let a cloud covering a third of an icon
/// outweigh a perfect fit on the visible two thirds; an inlier count simply loses the
/// occluded pixels and still reports a high fraction for the part that is visible.
pub const INLIER_TOLERANCE: i32 = 90;

/// Inlier agreement between two captures of the **same region**, by the same metric as matching.
///
/// The other direction of the same idea: instead of asking "is this artwork somewhere in the frame",
/// this asks "is this region still showing what it showed a moment ago". No template, nothing loaded
/// from disk, no scale search — the two frames are already aligned by construction, so the whole
/// thing is one pass.
///
/// Why it earns its place over a hash of the region: the score is the *fraction* of pixels that
/// agree, so a small occlusion costs a small amount. That is what makes it usable with the game's
/// own cursor drawn in the middle of the region — an exact hash would flip to "different" over a few
/// hundred pixels. Same reasoning as [`INLIER_TOLERANCE`], applied to a self-comparison.
///
/// Returns 0 for mismatched sizes rather than panicking: a caller that has resized the window
/// mid-check should see "not the same", which is true, and re-establish its reference.
pub fn inliers_between(a: &crate::win::capture::Frame, b: &crate::win::capture::Frame) -> f64 {
    if a.width != b.width || a.height != b.height {
        return 0.0;
    }
    let total = a.bgra.len() / 4;
    if total == 0 {
        return 0.0;
    }
    let agree = a
        .bgra
        .chunks_exact(4)
        .zip(b.bgra.chunks_exact(4))
        .filter(|(pa, pb)| {
            // Three channels only; the alpha byte of a BitBlt capture is not meaningful.
            let d = (pa[0] as i32 - pb[0] as i32).abs()
                + (pa[1] as i32 - pb[1] as i32).abs()
                + (pa[2] as i32 - pb[2] as i32).abs();
            d <= INLIER_TOLERANCE
        })
        .count();
    agree as f64 / total as f64
}

/// Searches `frame` for `tpl` at a single scale, returning the best match.
///
/// `step` subsamples candidate positions (1 = exhaustive). Nearest-neighbour is used for
/// template resampling: the art is pixel-art blitted through a `nearest` filter for the
/// overworld location layer (`worldUtils.setLocationTypeImageFilters(..., 'linear',
/// 'nearest')` in overworld/generators/world.lua), so nearest matches how it is drawn.
pub fn find_at_scale(
    frame: &crate::win::capture::Frame,
    tpl: &Template,
    scale: f64,
    step: i32,
) -> Option<Match> {
    find_at_scale_in(frame, tpl, scale, step, None)
}

/// As `find_at_scale`, but restricted to a search rectangle `(x0, y0, x1, y1)` of candidate
/// top-left positions. Bounding the search is what makes a step=1 exhaustive scan
/// affordable — and step=1 matters for pixel art, where being one pixel off misaligns every
/// edge in the sprite and collapses the score.
pub fn find_at_scale_in(
    frame: &crate::win::capture::Frame,
    tpl: &Template,
    scale: f64,
    step: i32,
    bounds: Option<(i32, i32, i32, i32)>,
) -> Option<Match> {
    let tw = ((tpl.width as f64) * scale).round() as i32;
    let th = ((tpl.height as f64) * scale).round() as i32;
    if tw <= 0 || th <= 0 || tw > frame.width || th > frame.height {
        return None;
    }

    // Precompute the resampled template once: (dx, dy, r, g, b) for opaque pixels only.
    let mut pts: Vec<(i32, i32, i32, i32, i32)> = Vec::new();
    for dy in 0..th {
        let sy = ((dy as f64 + 0.5) / scale).floor() as u32;
        let sy = sy.min(tpl.height - 1);
        for dx in 0..tw {
            let sx = ((dx as f64 + 0.5) / scale).floor() as u32;
            let sx = sx.min(tpl.width - 1);
            let i = ((sy * tpl.width + sx) * 4) as usize;
            if tpl.rgba[i + 3] >= ALPHA_MIN {
                pts.push((
                    dx,
                    dy,
                    tpl.rgba[i] as i32,
                    tpl.rgba[i + 1] as i32,
                    tpl.rgba[i + 2] as i32,
                ));
            }
        }
    }
    if pts.is_empty() {
        return None;
    }

    let mut best: Option<Match> = None;
    let step = step.max(1);
    let (bx0, by0, bx1, by1) = bounds.unwrap_or((0, 0, frame.width, frame.height));
    let (bx0, by0) = (bx0.max(0), by0.max(0));
    let (bx1, by1) = (bx1.min(frame.width - tw), by1.min(frame.height - th));
    let mut oy = by0;
    while oy <= by1 {
        let mut ox = bx0;
        while ox <= bx1 {
            let mut sum = 0i64;
            let mut inlier_count = 0usize;
            for &(dx, dy, tr, tg, tb) in &pts {
                let fi = (((oy + dy) * frame.width + (ox + dx)) * 4) as usize;
                // Frame is BGRA.
                let b = frame.bgra[fi] as i32;
                let g = frame.bgra[fi + 1] as i32;
                let r = frame.bgra[fi + 2] as i32;
                let d = (r - tr).abs() + (g - tg).abs() + (b - tb).abs();
                sum += d as i64;
                if d <= INLIER_TOLERANCE {
                    inlier_count += 1;
                }
            }
            let error = sum as f64 / (pts.len() as f64 * 3.0 * 255.0);
            let inliers = inlier_count as f64 / pts.len() as f64;
            // Rank by inlier fraction (occlusion-robust); mean error only breaks ties.
            let better = match best {
                None => true,
                Some(m) => inliers > m.inliers || (inliers == m.inliers && error < m.error),
            };
            if better {
                best = Some(Match {
                    x: ox,
                    y: oy,
                    cx: ox + tw / 2,
                    cy: oy + th / 2,
                    scale,
                    error,
                    inliers,
                });
            }
            ox += step;
        }
        oy += step;
    }
    best
}

/// Sweeps a set of scales and returns every scale's best match, sorted best-first.
///
/// Returning the whole sweep rather than just the winner is deliberate: a single number
/// cannot show whether the match is *distinctive*. Comparing the best against the runner-up
/// is what distinguishes "found it" from "this template matches flat ground everywhere".
pub fn sweep(
    frame: &crate::win::capture::Frame,
    tpl: &Template,
    scales: &[f64],
    step: i32,
) -> Vec<Match> {
    sweep_in(frame, tpl, scales, step, None)
}

/// As `sweep`, but restricted to a search rectangle. Bounding the search is what makes an
/// exhaustive `step=1` sweep affordable, and `step=1` is what a fair test of pixel-art
/// matching requires — a coarse step misaligns every edge and cannot distinguish "the
/// rendering differs from the file" from "we never tested the right offset".
pub fn sweep_in(
    frame: &crate::win::capture::Frame,
    tpl: &Template,
    scales: &[f64],
    step: i32,
    bounds: Option<(i32, i32, i32, i32)>,
) -> Vec<Match> {
    let mut out: Vec<Match> = scales
        .iter()
        .filter_map(|s| find_at_scale_in(frame, tpl, *s, step, bounds))
        .collect();
    out.sort_by(|a, b| {
        b.inliers
            .partial_cmp(&a.inliers)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.error.partial_cmp(&b.error).unwrap_or(std::cmp::Ordering::Equal))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::capture::Frame;

    fn solid_frame(w: i32, h: i32, b: u8, g: u8, r: u8) -> Frame {
        Frame { width: w, height: h, bgra: [b, g, r, 255].repeat((w * h) as usize) }
    }

    fn square_template(size: u32, r: u8, g: u8, b: u8, alpha: u8) -> Template {
        Template {
            name: "sq".into(),
            width: size,
            height: size,
            rgba: [r, g, b, alpha].repeat((size * size) as usize),
        }
    }

    #[test]
    fn a_region_compared_with_itself_scores_one() {
        // The positive control. Without it a broken metric that always returns 0 would look exactly
        // like "the screen changed", which is the answer the caller is waiting for.
        let f = solid_frame(30, 20, 10, 20, 30);
        assert_eq!(inliers_between(&f, &f), 1.0);
    }

    #[test]
    fn a_wholly_different_region_scores_zero() {
        let a = solid_frame(30, 20, 0, 0, 0);
        let b = solid_frame(30, 20, 255, 255, 255);
        assert_eq!(inliers_between(&a, &b), 0.0);
    }

    #[test]
    fn a_small_occlusion_costs_only_its_own_area() {
        // The cursor case, and the whole reason this is not a hash: the game warps the pointer into
        // this region itself (`snapToNearestHotspot`), so some pixels always differ.
        let a = solid_frame(20, 20, 0, 0, 0);
        let mut b = a.clone();
        for i in 0..8 {
            b.bgra[i * 4] = 255;
        }
        let score = inliers_between(&a, &b);
        assert!((score - (392.0 / 400.0)).abs() < 1e-9, "got {score}");
        assert!(score >= crate::typist::wildcard::SAME, "must still read as unchanged");
    }

    #[test]
    fn a_change_under_the_tolerance_is_not_a_change() {
        // Idle animation and torch flicker move pixels a little every frame. A metric that called
        // that "different" would report the keyboard as still open forever.
        let a = solid_frame(10, 10, 100, 100, 100);
        let b = solid_frame(10, 10, 110, 120, 125);
        assert_eq!(inliers_between(&a, &b), 1.0, "10+20+25 = 55, under the tolerance");
    }

    #[test]
    fn mismatched_sizes_read_as_different_rather_than_panicking() {
        // A resized window mid-check must invalidate the reference, not crash a live fight.
        assert_eq!(inliers_between(&solid_frame(10, 10, 0, 0, 0), &solid_frame(20, 10, 0, 0, 0)), 0.0);
    }

    /// A template painted into the frame must be found exactly, with zero error.
    #[test]
    fn finds_an_exact_patch_at_its_true_position() {
        let mut frame = solid_frame(40, 40, 0, 0, 0);
        // Paint a 6x6 red square at (10, 12). Frame is BGRA.
        for y in 12..18 {
            for x in 10..16 {
                let i = ((y * 40 + x) * 4) as usize;
                frame.bgra[i] = 0;
                frame.bgra[i + 1] = 0;
                frame.bgra[i + 2] = 255;
            }
        }
        let tpl = square_template(6, 255, 0, 0, 255);
        let m = find_at_scale(&frame, &tpl, 1.0, 1).expect("should match");
        assert_eq!((m.x, m.y), (10, 12));
        assert!(m.error < 1e-9, "exact match should score 0, got {}", m.error);
    }

    /// Centre is what map panning drives to (960,540), so it must be right.
    #[test]
    fn reports_the_centre_of_the_matched_rectangle() {
        let frame = solid_frame(40, 40, 0, 0, 0);
        let tpl = square_template(6, 0, 0, 0, 255);
        let m = find_at_scale(&frame, &tpl, 1.0, 1).expect("should match");
        assert_eq!((m.cx, m.cy), (m.x + 3, m.y + 3));
    }

    /// Fully transparent templates carry no evidence and must not report a match.
    #[test]
    fn a_fully_transparent_template_matches_nothing() {
        let frame = solid_frame(20, 20, 0, 0, 0);
        let tpl = square_template(4, 255, 0, 0, 0);
        assert!(find_at_scale(&frame, &tpl, 1.0, 1).is_none());
    }

    /// Transparent pixels must not contribute: a template whose opaque part matches but
    /// whose transparent part disagrees should still score zero.
    #[test]
    fn transparent_pixels_are_excluded_from_the_score() {
        let frame = solid_frame(20, 20, 0, 0, 0); // black
        let mut tpl = square_template(4, 255, 255, 255, 0); // white, transparent
        // Make the top-left pixel opaque black -- the only pixel that may be scored.
        tpl.rgba[0] = 0;
        tpl.rgba[1] = 0;
        tpl.rgba[2] = 0;
        tpl.rgba[3] = 255;
        let m = find_at_scale(&frame, &tpl, 1.0, 1).expect("should match");
        assert!(m.error < 1e-9, "only the opaque pixel should score, got {}", m.error);
    }

    /// The property the design depends on: a sprite with a third of it painted over
    /// (a cloud) must still be found at its true position, and must still report a high
    /// inlier fraction. Ranking on mean error would fail this.
    #[test]
    fn a_partially_occluded_sprite_is_still_found() {
        // A STRUCTURED template, not a flat colour: each row has a distinct blue value, so
        // exactly one alignment can be correct. A flat template is degenerate -- a shifted
        // placement ties on inlier count and the mean-error tie-break can then pick the
        // wrong one. Real sprites carry structure; the test should too.
        let mut tpl = square_template(9, 255, 0, 0, 255);
        for y in 0..9u32 {
            for x in 0..9u32 {
                let i = ((y * 9 + x) * 4) as usize;
                tpl.rgba[i + 2] = (y * 28) as u8; // blue ramp down the rows
            }
        }
        let mut frame = solid_frame(60, 60, 0, 0, 0);
        // Paint the template into the frame at (20, 24) -- the "sprite" on the map.
        for y in 0..9usize {
            for x in 0..9usize {
                let ti = ((y * 9 + x) * 4) as usize;
                let fi = (((24 + y) * 60 + (20 + x)) * 4) as usize;
                frame.bgra[fi] = tpl.rgba[ti + 2]; // B
                frame.bgra[fi + 1] = tpl.rgba[ti + 1]; // G
                frame.bgra[fi + 2] = tpl.rgba[ti]; // R
            }
        }
        // Occlude the top 3 rows with opaque white -- a cloud drifting over it (27/81).
        for y in 24..27 {
            for x in 20..29 {
                let i = ((y * 60 + x) * 4) as usize;
                frame.bgra[i] = 255;
                frame.bgra[i + 1] = 255;
                frame.bgra[i + 2] = 255;
            }
        }
        let m = find_at_scale(&frame, &tpl, 1.0, 1).expect("should still match");
        assert_eq!((m.x, m.y), (20, 24), "occlusion must not move the reported position");
        // Two thirds visible, so the inlier fraction should reflect that, not collapse.
        assert!(
            m.inliers > 0.6,
            "expected most of the sprite to still count as inliers, got {}",
            m.inliers
        );
    }

    #[test]
    fn opaque_fraction_counts_only_pixels_above_the_cutoff() {
        assert_eq!(square_template(2, 0, 0, 0, 255).opaque_fraction(ALPHA_MIN), 1.0);
        assert_eq!(square_template(2, 0, 0, 0, 0).opaque_fraction(ALPHA_MIN), 0.0);
    }

    /// A template larger than the frame is a caller error, not a panic.
    #[test]
    fn oversized_template_returns_none() {
        let frame = solid_frame(4, 4, 0, 0, 0);
        let tpl = square_template(8, 0, 0, 0, 255);
        assert!(find_at_scale(&frame, &tpl, 1.0, 1).is_none());
    }
}
