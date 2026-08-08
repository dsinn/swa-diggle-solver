//! Measures the hurt vignette's footprint on a captured frame, offline.
//!
//! `rpgview.lua:2275-2286` draws `shaders/retina.png` through `retina-hurt.vs` whenever
//! `retinaPainVal ~= 0`, which `updateRetinaPainVal` (`:1936-1945`) switches on at
//! `healthN <= 0.25 and health <= 8`. The shader is a masked alpha blend:
//!
//! ```glsl
//! float effectVal = Texel(effectmap, texpos)[0]-(1.0-cuttoff);
//! if (effectVal <= 0.0) discard;
//! ```
//!
//! so it covers wherever the effectmap exceeds the cutoff and leaves everywhere else untouched. If
//! that is a vignette, the coverage must be **radial** — heavy at the frame edges, absent in the
//! middle — and that is a claim about pixels, checkable without the game running.
//!
//! This measures it two ways on a frame that is known to have been drawn at low health, against a
//! control frame drawn at full health:
//!
//! 1. **Redness by ring.** Mean `R - (G+B)/2` in concentric bands from the centre out. A vignette
//!    shows a monotonic ramp; a uniform tint shows a flat line, and would need a different fix.
//! 2. **Redness at each button's origin.** Which template rects actually sit in the damaged zone,
//!    since that is what decides whether recognition survives.
//!
//! Measures only. Nothing here changes a threshold or a call site.
//!
//! A third mode answers the question a candidate signal actually poses. `--rect x0,y0,x1,y1`
//! reports, for that region, both the redness delta *and* [`inliers_between`] across the two frames
//! — the same metric matching uses. That is the direct measurement of "would this region still
//! recognise itself with the vignette up", which is what any always-on combat detector needs.
//!
//! ```text
//! cargo run --bin vignette_probe -- <hurt-frame.png> [control-frame.png] [--rect x0,y0,x1,y1]...
//! ```

use diggle_solver::act;
use diggle_solver::win::capture::Frame;
use std::path::Path;

/// Concentric bands, as a fraction of the half-diagonal. Eight is enough to see a ramp without
/// making the table unreadable.
const RINGS: usize = 8;

fn load(path: &Path) -> Result<Frame, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut rdr = png::Decoder::new(file)
        .read_info()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let mut buf = vec![0; rdr.output_buffer_size()];
    let info = rdr.next_frame(&mut buf).map_err(|e| format!("{}: {e}", path.display()))?;
    let n = info.color_type.samples();
    let mut bgra = Vec::with_capacity((info.width * info.height * 4) as usize);
    for px in buf.chunks_exact(n) {
        bgra.extend_from_slice(&[px[2], px[1], px[0], 255]);
    }
    Ok(Frame { width: info.width as i32, height: info.height as i32, bgra })
}

/// How red a pixel is *relative to its own brightness*, so a bright wooden panel and a dark
/// background are comparable. The vignette adds red without adding green or blue, so this is the
/// quantity it moves and plain luminance is not.
fn redness(f: &Frame, x: i32, y: i32) -> f64 {
    let i = ((y * f.width + x) * 4) as usize;
    let (b, g, r) = (f.bgra[i] as f64, f.bgra[i + 1] as f64, f.bgra[i + 2] as f64);
    r - (g + b) / 2.0
}

/// Mean redness over a rect, clamped to the frame.
fn mean_redness(f: &Frame, x0: i32, y0: i32, x1: i32, y1: i32) -> f64 {
    let (x0, y0) = (x0.max(0), y0.max(0));
    let (x1, y1) = (x1.min(f.width), y1.min(f.height));
    if x0 >= x1 || y0 >= y1 {
        return f64::NAN;
    }
    let mut total = 0.0;
    let mut n = 0u64;
    for y in y0..y1 {
        for x in x0..x1 {
            total += redness(f, x, y);
            n += 1;
        }
    }
    total / n as f64
}

/// Distance from the frame centre, as a fraction of the half-diagonal, so 0 is dead centre and
/// ~1.0 is a corner.
fn radius(f: &Frame, x: i32, y: i32) -> f64 {
    let (cx, cy) = (f.width as f64 / 2.0, f.height as f64 / 2.0);
    let (dx, dy) = (x as f64 - cx, y as f64 - cy);
    (dx * dx + dy * dy).sqrt() / (cx * cx + cy * cy).sqrt()
}

/// Mean redness per concentric band. Sampled on a stride rather than every pixel — at 1920x1080 the
/// bands hold hundreds of thousands of pixels each and the mean is settled long before that.
fn rings(f: &Frame) -> [f64; RINGS] {
    let mut total = [0.0; RINGS];
    let mut count = [0u64; RINGS];
    let mut y = 0;
    while y < f.height {
        let mut x = 0;
        while x < f.width {
            let band = ((radius(f, x, y) * RINGS as f64) as usize).min(RINGS - 1);
            total[band] += redness(f, x, y);
            count[band] += 1;
            x += 3;
        }
        y += 3;
    }
    let mut out = [f64::NAN; RINGS];
    for i in 0..RINGS {
        if count[i] > 0 {
            out[i] = total[i] / count[i] as f64;
        }
    }
    out
}

/// `x0,y0,x1,y1`.
fn parse_rect(s: &str) -> Result<(i32, i32, i32, i32), String> {
    let n: Vec<i32> = s
        .split(',')
        .map(|p| p.trim().parse::<i32>().map_err(|e| format!("--rect {s}: {e}")))
        .collect::<Result<_, _>>()?;
    match n[..] {
        [x0, y0, x1, y1] if x0 < x1 && y0 < y1 => Ok((x0, y0, x1, y1)),
        _ => Err(format!("--rect {s}: want x0,y0,x1,y1 with x0<x1 and y0<y1")),
    }
}

fn main() -> Result<(), String> {
    let mut positional = Vec::new();
    let mut rects = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--rect" {
            let v = args.next().ok_or("--rect needs x0,y0,x1,y1")?;
            rects.push((v.clone(), parse_rect(&v)?));
        } else {
            positional.push(a);
        }
    }
    let mut positional = positional.into_iter();
    let hurt_path = positional.next().ok_or(
        "usage: vignette_probe <hurt-frame.png> [control-frame.png] [--rect x0,y0,x1,y1]...\n  \
         e.g. vignette_probe spike-frames-live/gave-up.png tests/frames/combat-chest.png",
    )?;
    let control_path = positional.next();

    let hurt = load(Path::new(&hurt_path))?;
    let control = control_path.as_deref().map(|p| load(Path::new(p))).transpose()?;
    println!("hurt frame:    {hurt_path} ({}x{})", hurt.width, hurt.height);
    match (&control, &control_path) {
        (Some(c), Some(p)) => println!("control frame: {p} ({}x{})", c.width, c.height),
        _ => println!("control frame: none — the ramp is still readable, the offset is not"),
    }

    // ---- 1. redness by ring ----
    //
    // The shape is the finding. An absolute number says little on its own, because the scene behind
    // the overlay is itself a red-lit crypt; a rising *profile* is what only a vignette produces.
    println!("\nmean redness (R - (G+B)/2) by distance from centre:");
    println!("{:<14} {:>10} {:>10} {:>10}", "band", "hurt", "control", "delta");
    println!("{}", "-".repeat(48));
    let hurt_rings = rings(&hurt);
    let control_rings = control.as_ref().map(rings);
    for i in 0..RINGS {
        let lo = i as f64 / RINGS as f64;
        let hi = (i + 1) as f64 / RINGS as f64;
        let (c, d) = match control_rings {
            Some(cr) => (format!("{:.2}", cr[i]), format!("{:+.2}", hurt_rings[i] - cr[i])),
            None => ("-".into(), "-".into()),
        };
        println!("{:<14} {:>10.2} {:>10} {:>10}", format!("{lo:.2}-{hi:.2}"), hurt_rings[i], c, d);
    }
    let ramp = hurt_rings[RINGS - 1] - hurt_rings[0];
    println!("\ncentre -> corner ramp: {ramp:+.2}");

    // ---- 2. redness where the templates actually sit ----
    //
    // A ramp only matters if the recognised rects are on the wrong end of it. `origin` is the exact
    // top-left `score_exact` compares at, so this is the region that decides each read.
    println!("\nper button, over its exact template rect at `origin`:");
    println!("{:<28} {:>12} {:>8} {:>9} {:>9} {:>8}", "button", "origin", "radius", "hurt", "control", "delta");
    println!("{}", "-".repeat(80));
    let mut rows: Vec<(f64, String)> = Vec::new();
    for b in act::ALL {
        let tpl = match diggle_solver::observe::template::Template::load(
            &Path::new("templates").join(b.template),
        ) {
            Ok(t) => t,
            Err(e) => {
                println!("{:<28} template unreadable: {e}", b.name);
                continue;
            }
        };
        let (ox, oy) = b.origin;
        let (x1, y1) = (ox + tpl.width as i32, oy + tpl.height as i32);
        let r = radius(&hurt, (ox + x1) / 2, (oy + y1) / 2);
        let h = mean_redness(&hurt, ox, oy, x1, y1);
        let (c, d) = match &control {
            Some(cf) => {
                let c = mean_redness(cf, ox, oy, x1, y1);
                (format!("{c:.2}"), format!("{:+.2}", h - c))
            }
            None => ("-".into(), "-".into()),
        };
        rows.push((
            r,
            format!(
                "{:<28} {:>12} {:>8.3} {:>9.2} {:>9} {:>8}",
                b.name,
                format!("{ox},{oy}"),
                r,
                h,
                c,
                d
            ),
        ));
    }
    // Sorted by radius, because the whole question is whether damage tracks distance from centre.
    rows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    for (_, line) in rows {
        println!("{line}");
    }

    // ---- 3. candidate signal regions ----
    //
    // `inliers` is the question, not redness: a region can shift colour and still match if the shift
    // stays inside `INLIER_TOLERANCE`. This is the same call the matcher makes, on the same two
    // frames, so the number is the signal's actual survival rate rather than a proxy for it.
    if !rects.is_empty() {
        println!("\ncandidate regions, hurt vs control:");
        println!("{:<24} {:>8} {:>9} {:>9} {:>8} {:>9}", "rect", "radius", "hurt", "control", "delta", "inliers");
        println!("{}", "-".repeat(72));
        for (label, (x0, y0, x1, y1)) in &rects {
            let r = radius(&hurt, (x0 + x1) / 2, (y0 + y1) / 2);
            let h = mean_redness(&hurt, *x0, *y0, *x1, *y1);
            let (c, d, inl) = match &control {
                Some(cf) => {
                    let c = mean_redness(cf, *x0, *y0, *x1, *y1);
                    let a = crop(&hurt, (*x0, *y0, *x1, *y1));
                    let b = crop(cf, (*x0, *y0, *x1, *y1));
                    let i = diggle_solver::observe::template::inliers_between(&a, &b);
                    (format!("{c:.2}"), format!("{:+.2}", h - c), format!("{i:.4}"))
                }
                None => ("-".into(), "-".into(), "-".into()),
            };
            println!("{label:<24} {r:>8.3} {h:>9.2} {c:>9} {d:>8} {inl:>9}");
        }
    }
    Ok(())
}

/// Crop to a rect, clamped to the frame. Mirrors `score_compare::crop`.
fn crop(f: &Frame, (x0, y0, x1, y1): (i32, i32, i32, i32)) -> Frame {
    let (x0, y0) = (x0.max(0), y0.max(0));
    let (x1, y1) = (x1.min(f.width), y1.min(f.height));
    let (w, h) = (x1 - x0, y1 - y0);
    let mut bgra = Vec::with_capacity((w * h * 4) as usize);
    for y in y0..y1 {
        let row = (y * f.width + x0) as usize * 4;
        bgra.extend_from_slice(&f.bgra[row..row + (w as usize) * 4]);
    }
    Frame { width: w, height: h, bgra }
}
