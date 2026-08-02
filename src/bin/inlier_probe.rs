//! Measures the inlier agreement between two same-sized PNG crops.
//!
//! Calibration, not gameplay. A threshold picked by reasoning about how different two screens
//! "should" look is a guess; this reports what they actually score, so the number in the code can be
//! chosen from data — the same way the affirmative gate's 0.55 was placed in a measured gap.
//!
//! Scores by inliers rather than mean error, matching [`diggle_solver::observe::template`]: the
//! fraction of pixels agreeing within `INLIER_TOLERANCE`. That is the metric the real check will use,
//! so the numbers here transfer directly.

use diggle_solver::observe::template::{Template, INLIER_TOLERANCE};

fn inliers(a: &Template, b: &Template) -> f64 {
    if a.width != b.width || a.height != b.height {
        eprintln!("size mismatch: {}x{} vs {}x{}", a.width, a.height, b.width, b.height);
        return f64::NAN;
    }
    let mut agree = 0usize;
    let mut total = 0usize;
    for (pa, pb) in a.rgba.chunks_exact(4).zip(b.rgba.chunks_exact(4)) {
        let d = (pa[0] as i32 - pb[0] as i32).abs()
            + (pa[1] as i32 - pb[1] as i32).abs()
            + (pa[2] as i32 - pb[2] as i32).abs();
        if d <= INLIER_TOLERANCE {
            agree += 1;
        }
        total += 1;
    }
    agree as f64 / total.max(1) as f64
}

fn short(p: &str) -> String {
    std::path::Path::new(p)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        return Err("usage: inlier_probe <a.png> <b.png> [more.png ...]".into());
    }
    let loaded: Vec<(String, Template)> = args
        .iter()
        .map(|p| Template::load(std::path::Path::new(p)).map(|t| (p.clone(), t)))
        .collect::<Result<_, _>>()?;
    for i in 0..loaded.len() {
        for j in i..loaded.len() {
            println!(
                "{:>28} vs {:<28} inliers {:.4}",
                short(&loaded[i].0),
                short(&loaded[j].0),
                inliers(&loaded[i].1, &loaded[j].1)
            );
        }
    }
    Ok(())
}
