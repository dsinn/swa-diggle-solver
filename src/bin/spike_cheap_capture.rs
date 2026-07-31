//! Does the cheap capture agree with the expensive one, and by how much is it cheaper?
//!
//! `capture_window` uses `PrintWindow(PW_RENDERFULLCONTENT)`, which makes the game re-render its
//! whole window into our DC — 25 ms, and the user saw the game visibly drop frames while the click
//! spikes ran. `capture_client_rect` does a `BitBlt` off the screen instead: no second render, and
//! only the board rectangle is copied.
//!
//! That is a different source of pixels, not merely a faster one, so it needs a **positive
//! control**. Speed alone would be worthless if the two disagreed — a verifier reading subtly
//! different pixels is exactly the kind of instrument this project has been burned by. So both are
//! taken back to back and compared tile by tile, on an idle board and again with tiles selected.
//!
//! **This drives the real mouse.** It selects two tiles, deselects them, and never submits.

use diggle_solver::act;
use diggle_solver::config::Config;
use diggle_solver::game::save;
use diggle_solver::layout;
use diggle_solver::observe::log::Console;
use diggle_solver::win::capture::{capture_client_rect, capture_window, Frame};
use diggle_solver::win::input::{click_at, warp_cursor};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

const REPORT: &str = "spike-cheap-capture.md";
const NEUTRAL: (i32, i32) = (300, 300);

fn luma(frame: &Frame, cx: i32, cy: i32, radius: i32) -> f64 {
    let mut sum = 0f64;
    let mut n = 0f64;
    for y in (cy - radius)..=(cy + radius) {
        for x in (cx - radius)..=(cx + radius) {
            if x < 0 || y < 0 || x >= frame.width || y >= frame.height {
                continue;
            }
            let i = ((y * frame.width + x) * 4) as usize;
            sum += 0.114 * frame.bgra[i] as f64
                + 0.587 * frame.bgra[i + 1] as f64
                + 0.299 * frame.bgra[i + 2] as f64;
            n += 1.0;
        }
    }
    if n == 0.0 { 0.0 } else { sum / n }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config::load(Path::new("config.toml"))?;
    diggle_solver::win::process::refuse_if_running("lovec.exe", &[])?;
    let save_dir = diggle_solver::game::savedir::locate(cfg.save_dir.clone(), true)?;

    let mut log = String::from("# Spike: cheap capture vs PrintWindow\n\n");
    let mut console = Console::take()?;
    let mut game = diggle_solver::game::launch::GameProcess::launch(&cfg, &console)?;
    let win = game.wait_for_window(Duration::from_secs(20))?;
    std::thread::sleep(Duration::from_secs(3));

    if act::click_when_ready(&win, &act::CONTINUE, Duration::from_secs(30)).is_err() {
        log.push_str("ABORT: no Continue\n");
        game.close(Duration::from_secs(15));
        std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
        println!("{log}");
        return Ok(());
    }
    let deadline = Instant::now() + Duration::from_secs(40);
    let mut combat = None;
    while Instant::now() < deadline && combat.is_none() {
        std::thread::sleep(Duration::from_millis(400));
        let _ = console.read_new();
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
    std::thread::sleep(Duration::from_secs(3));

    let n = combat.table_at("tileboard").map(|t| t.arr.len()).unwrap_or(16);
    let (cw, ch) = win.client_size()?;
    let geom = diggle_solver::geometry::Geometry::from_save(&combat, n).geometry;
    let centres = layout::tile_centres(&geom, cw, ch);
    let (bx, by, bw, bh) = layout::board_rect(&geom, cw, ch);
    let radius = (layout::tile_radius(cw, ch) * 0.55).round() as i32;

    let (px, py) = win.client_to_screen(NEUTRAL.0, NEUTRAL.1)?;
    warp_cursor(px, py)?;
    std::thread::sleep(Duration::from_millis(300));

    log.push_str(&format!(
        "client {cw}x{ch}; board rect {bw}x{bh} at ({bx},{by}) = {:.1}% of the window\n\n",
        (bw * bh) as f64 / (cw * ch) as f64 * 100.0
    ));

    // ---- cost ----
    let _ = capture_window(&win)?;
    let _ = capture_client_rect(&win, bx, by, bw, bh)?;
    let mut full = Vec::new();
    let mut cheap = Vec::new();
    for _ in 0..12 {
        let t = Instant::now();
        let _ = capture_window(&win)?;
        full.push(t.elapsed().as_secs_f64() * 1000.0);
        let t = Instant::now();
        let _ = capture_client_rect(&win, bx, by, bw, bh)?;
        cheap.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    full.sort_by(|a, b| a.partial_cmp(b).unwrap());
    cheap.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let (fm, cm) = (full[full.len() / 2], cheap[cheap.len() / 2]);
    log.push_str(&format!(
        "| capture | median | min | max |\n|---|---|---|---|\n\
         | `PrintWindow` full window | {fm:.1} ms | {:.1} | {:.1} |\n\
         | `BitBlt` board rect | **{cm:.1} ms** | {:.1} | {:.1} |\n\n**{:.1}x faster**\n\n",
        full[0], full[full.len() - 1], cheap[0], cheap[cheap.len() - 1], fm / cm.max(0.001)
    ));

    // ---- agreement, idle board then with tiles selected ----
    let mut worst_overall = 0f64;
    for phase in ["idle", "two tiles selected"] {
        if phase == "two tiles selected" {
            for i in [0usize, 5] {
                let (sx, sy) = win.client_to_screen(centres[i].0, centres[i].1)?;
                click_at(sx, sy)?;
                std::thread::sleep(Duration::from_millis(60));
            }
            warp_cursor(px, py)?;
            std::thread::sleep(Duration::from_millis(400));
        }

        let f = capture_window(&win)?;
        let c = capture_client_rect(&win, bx, by, bw, bh)?;
        let mut worst = 0f64;
        for &(x, y) in &centres {
            let a = luma(&f, x, y, radius);
            // The cheap frame's origin is the rect corner, so tile coordinates shift by it.
            let b = luma(&c, x - bx, y - by, radius);
            worst = worst.max((a - b).abs());
        }
        worst_overall = worst_overall.max(worst);
        log.push_str(&format!("- {phase}: worst per-tile luma difference **{worst:.2}**\n"));
    }

    // Leave the board as found.
    for i in [0usize, 5] {
        let (sx, sy) = win.client_to_screen(centres[i].0, centres[i].1)?;
        click_at(sx, sy)?;
        std::thread::sleep(Duration::from_millis(60));
    }
    warp_cursor(px, py)?;
    std::thread::sleep(Duration::from_millis(300));

    log.push_str(&format!(
        "\n## Verdict\n\n{}\n",
        if worst_overall < 1.0 {
            "The two capture paths agree to within a luma point, on an idle board and a selected \
             one. The selection threshold is 12, so the cheap path carries ~12x the margin it needs."
                .to_string()
        } else {
            format!(
                "**Disagreement of {worst_overall:.2} luma.** Not safe to swap in until explained — \
                 a verifier reading different pixels than the one it was calibrated against is worse \
                 than a slow one."
            )
        }
    ));

    game.close(Duration::from_secs(15));
    std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
    println!("{log}");
    Ok(())
}
