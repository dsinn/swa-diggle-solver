//! Live probe: can we click a *named* tile, and get exactly that tile, every time?
//!
//! The bar the user set is 100% — a mis-click in combat selects the wrong tile, which builds a word
//! the board cannot play and leaves the run wedged. So this measures rather than asserts, and it
//! checks the strong property, not the weak one:
//!
//! - weak: "something changed after I clicked" — passes even if a neighbour was selected
//! - **strong: the intended tile changed AND no other tile did**
//!
//! Off-by-one is the failure mode that matters here. A mapping wrong by more than half a tile
//! selects a neighbour silently; one wrong by less still lands correctly. Only the strong check
//! separates those.
//!
//! ## Method
//!
//! For each tile, in dump order: click it, park the cursor somewhere neutral, capture, and compare
//! the mean luminance of every tile's box against a baseline. Selected tiles darken substantially
//! (a light wood face goes dark brown), so the signal is large and local. Then click again to
//! deselect and confirm the board returns to baseline — which also leaves the fight untouched.
//!
//! The cursor is parked away from the board before every capture because hovering *also* highlights
//! a tile (`mouseHoverTile`, `tileboard.lua:130`). Capturing with the pointer still resting on the
//! tile would conflate hover with selection, and the test would pass on a mapping that only ever
//! hovers the right tile.
//!
//! **This drives the real mouse.** It never submits and never presses a key.

use diggle_solver::act;
use diggle_solver::config::Config;
use diggle_solver::game::save;
use diggle_solver::layout;
use diggle_solver::observe::log::{Console, LogMirror};
use diggle_solver::win::capture::{capture_window, Frame};
use diggle_solver::win::input::{inject_left_click, warp_cursor};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

const REPORT: &str = "spike-tile-click.md";
const FRAMES: &str = "spike-frames-live";

/// Somewhere with no hotspot: the stone wall left of the player. Parking here between actions keeps
/// hover out of the measurement.
const NEUTRAL: (i32, i32) = (300, 300);

/// Luminance change that counts as "this tile changed state". Selection is a large, obvious shift;
/// this sits well above frame-to-frame noise from the torch flicker and idle animations.
const CHANGED: f64 = 12.0;

/// Mean luminance of a box centred on `(cx, cy)`, sized to sit inside one tile.
fn tile_luma(frame: &Frame, cx: i32, cy: i32, radius: i32) -> f64 {
    let mut sum = 0f64;
    let mut n = 0f64;
    for y in (cy - radius)..=(cy + radius) {
        for x in (cx - radius)..=(cx + radius) {
            if x < 0 || y < 0 || x >= frame.width || y >= frame.height {
                continue;
            }
            let i = ((y * frame.width + x) * 4) as usize;
            let (b, g, r) =
                (frame.bgra[i] as f64, frame.bgra[i + 1] as f64, frame.bgra[i + 2] as f64);
            sum += 0.114 * b + 0.587 * g + 0.299 * r;
            n += 1.0;
        }
    }
    if n == 0.0 {
        0.0
    } else {
        sum / n
    }
}

fn all_luma(frame: &Frame, centres: &[(i32, i32)], radius: i32) -> Vec<f64> {
    centres.iter().map(|&(x, y)| tile_luma(frame, x, y, radius)).collect()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config::load(Path::new("config.toml"))?;
    std::fs::create_dir_all(FRAMES)?;
    diggle_solver::win::process::refuse_if_running("lovec.exe", &[])?;
    let save_dir = diggle_solver::game::savedir::locate(cfg.save_dir.clone(), true)?;

    let mut log = String::from("# Spike: clicking a named tile\n\n");
    let mut console = Console::take()?;
    let mut mirror = LogMirror::create(Path::new("spike-tile-click-raw.log"))?;
    let mut game = diggle_solver::game::launch::GameProcess::launch(&cfg, &console)?;
    let win = game.wait_for_window(Duration::from_secs(20))?;
    std::thread::sleep(Duration::from_secs(3));

    match act::click_when_ready(&win, &act::CONTINUE, Duration::from_secs(30)) {
        Ok(i) => log.push_str(&format!("clicked Continue (inliers {i:.3})\n")),
        Err(e) => {
            log.push_str(&format!("ABORT: no Continue: {e}\n"));
            game.close(Duration::from_secs(15));
            std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
            println!("{log}");
            return Ok(());
        }
    }

    // Resuming a fight restores turnState without re-entering onStart, so gate on the save.
    let deadline = Instant::now() + Duration::from_secs(40);
    let mut combat = None;
    while Instant::now() < deadline && combat.is_none() {
        std::thread::sleep(Duration::from_millis(400));
        if let Ok(lines) = console.read_new() {
            if !lines.is_empty() {
                mirror.write(&lines);
            }
        }
        if let Ok(t) = save::load(&save_dir.join("combatSaveData")) {
            if t.str_at("rpg.player.turnState") == Some("PlayerTurn") {
                combat = Some(t);
            }
        }
    }
    let Some(combat) = combat else {
        log.push_str("ABORT: never reached an interactive PlayerTurn\n");
        game.close(Duration::from_secs(15));
        std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
        println!("{log}");
        return Ok(());
    };
    // Tiles fall into place after the screen loads; clicking mid-fall would hit a moving target.
    std::thread::sleep(Duration::from_secs(3));

    let letters: Vec<String> = combat
        .table_at("tileboard")
        .map(|tb| {
            tb.arr
                .iter()
                .filter_map(|v| {
                    v.as_str()
                        .map(str::to_string)
                        .or_else(|| Some(v.as_table()?.arr.first()?.as_str()?.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();

    let (cw, ch) = win.client_size()?;
    let resolved = diggle_solver::geometry::Geometry::from_save(&combat, letters.len());
    for p in &resolved.problems {
        log.push_str(&format!("WARNING geometry: {p}\n"));
    }
    let geom = resolved.geometry;
    let centres = layout::tile_centres(&geom, cw, ch);
    // A box well inside the tile face, so a small mapping error still samples the right tile and a
    // large one samples the gap or a neighbour -- which is what we want to detect.
    let radius = (layout::tile_radius(cw, ch) * 0.55).round() as i32;

    log.push_str(&format!(
        "\nclient {cw}x{ch}, board {} tiles, {} columns {:?}\ntile radius {:.1}px, sample box +-{radius}px\n\n",
        geom.total_tiles(),
        geom.rows_per_col.len(),
        geom.rows_per_col,
        layout::tile_radius(cw, ch)
    ));
    log.push_str("| # | letter | centre | target Δluma | worst other Δ | verdict |\n");
    log.push_str("|---|--------|--------|--------------|---------------|--------|\n");

    let park = || -> Result<(), Box<dyn std::error::Error>> {
        let (sx, sy) = win.client_to_screen(NEUTRAL.0, NEUTRAL.1)?;
        warp_cursor(sx, sy)?;
        std::thread::sleep(Duration::from_millis(180));
        Ok(())
    };

    park()?;
    let baseline = all_luma(&capture_window(&win)?, &centres, radius);

    let mut correct = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for (i, &(cx, cy)) in centres.iter().enumerate() {
        let letter = letters.get(i).cloned().unwrap_or_else(|| "?".into());

        let (sx, sy) = win.client_to_screen(cx, cy)?;
        warp_cursor(sx, sy)?;
        std::thread::sleep(Duration::from_millis(120));
        inject_left_click(1)?;
        std::thread::sleep(Duration::from_millis(280));
        park()?;

        let now = all_luma(&capture_window(&win)?, &centres, radius);
        let deltas: Vec<f64> = now.iter().zip(&baseline).map(|(a, b)| (a - b).abs()).collect();
        let target = deltas[i];
        let worst_other = deltas
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, d)| *d)
            .fold(0.0f64, f64::max);

        // The strong check: the intended tile moved, and nothing else did.
        let ok = target > CHANGED && worst_other < CHANGED;
        if ok {
            correct += 1;
        } else {
            let who = deltas
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
                .map(|(j, d)| format!("index {j} moved most ({d:.1})"))
                .unwrap_or_default();
            failures.push(format!("tile {i} ({letter}): target {target:.1}, {who}"));
            let _ = capture_window(&win)?
                .write_png(Path::new(&format!("{FRAMES}/tile-click-fail-{i}.png")));
        }
        log.push_str(&format!(
            "| {i} | {letter} | ({cx},{cy}) | {target:.1} | {worst_other:.1} | {} |\n",
            if ok { "ok" } else { "**WRONG**" }
        ));

        // Deselect, so the next tile is measured against a clean board and the fight is left as found.
        warp_cursor(sx, sy)?;
        std::thread::sleep(Duration::from_millis(120));
        inject_left_click(1)?;
        std::thread::sleep(Duration::from_millis(280));
        park()?;
    }

    // The board must be back where it started, or something was left selected.
    let restored = all_luma(&capture_window(&win)?, &centres, radius);
    let drift = restored.iter().zip(&baseline).map(|(a, b)| (a - b).abs()).fold(0.0f64, f64::max);

    log.push_str(&format!(
        "\n## Result\n\n**{correct} of {} tiles clicked correctly**\n\nresidual drift after \
         deselecting everything: {drift:.1} (want < {CHANGED})\n",
        centres.len()
    ));
    for f in &failures {
        log.push_str(&format!("- {f}\n"));
    }

    let _ = capture_window(&win)?.write_png(Path::new(&format!("{FRAMES}/tile-click-final.png")));
    game.close(Duration::from_secs(15));
    std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
    println!("{log}");
    Ok(())
}
