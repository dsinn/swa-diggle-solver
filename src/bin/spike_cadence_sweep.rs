//! How fast can clicks be issued before the game starts dropping them?
//!
//! The previous spike measured click → *visible* latency at 95 ms median, 195 ms worst. That is the
//! wrong number to pace against: it is animation plus capture staleness, not the time the game needs
//! to consume a click. The game runs at 120 FPS, so it takes one event per ~8 ms frame; waiting for
//! pixels between clicks would cost 12× 200 ms on a long word for no safety we cannot get another
//! way.
//!
//! So: fire all N clicks at a fixed interval, then take **one** capture and check the whole
//! selection at once. `capture_window` costs 25 ms and the luma maths costs 9 µs per tile, so a
//! single capture per word is ~2,700× cheaper than the per-click verification it replaces, and it
//! proves exactly the same thing — that the intended set, and only the intended set, is selected.
//!
//! Clicks go through `click_at`, which batches move-down-up into one `SendInput`. That removes the
//! warp/click race the old two-step path had, and with it the reason for a sleep between them.
//!
//! **This drives the real mouse.** It never submits and never presses a key.

use diggle_solver::act;
use diggle_solver::config::Config;
use diggle_solver::game::save;
use diggle_solver::layout;
use diggle_solver::observe::log::Console;
use diggle_solver::win::capture::{capture_window, Frame};
use diggle_solver::win::input::{click_at, warp_cursor};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

const REPORT: &str = "spike-cadence-sweep.md";
const NEUTRAL: (i32, i32) = (300, 300);
const CHANGED: f64 = 12.0;
/// Intervals to try, slowest first so a failure at the tight end cannot be blamed on a board left
/// dirty by an earlier pass.
const INTERVALS_MS: &[u64] = &[60, 40, 25, 16, 8, 0];

fn tile_luma(frame: &Frame, cx: i32, cy: i32, radius: i32) -> f64 {
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
    if n == 0.0 {
        0.0
    } else {
        sum / n
    }
}

fn selected_set(frame: &Frame, centres: &[(i32, i32)], base: &[f64], radius: i32) -> Vec<bool> {
    centres
        .iter()
        .zip(base)
        .map(|(&(x, y), b)| (tile_luma(frame, x, y, radius) - b).abs() > CHANGED)
        .collect()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config::load(Path::new("config.toml"))?;
    diggle_solver::win::process::refuse_if_running("lovec.exe", &[])?;
    let save_dir = diggle_solver::game::savedir::locate(cfg.save_dir.clone(), true)?;

    let mut log = String::from("# Spike: how tight can the click cadence be?\n\n");
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
    let radius = (layout::tile_radius(cw, ch) * 0.55).round() as i32;
    let screen: Vec<(i32, i32)> =
        centres.iter().map(|&(x, y)| win.client_to_screen(x, y).unwrap_or((x, y))).collect();
    let (px, py) = win.client_to_screen(NEUTRAL.0, NEUTRAL.1)?;

    warp_cursor(px, py)?;
    std::thread::sleep(Duration::from_millis(300));
    let baseline: Vec<f64> = {
        let f = capture_window(&win)?;
        centres.iter().map(|&(x, y)| tile_luma(&f, x, y, radius)).collect()
    };

    log.push_str(&format!(
        "{} tiles, one capture per pass to verify the whole selection.\n\n\
         | interval | issue time | selected | verdict |\n|---|---|---|---|\n",
        centres.len()
    ));

    let mut best: Option<u64> = None;
    for &ms in INTERVALS_MS {
        // Select every tile at this cadence.
        let t0 = Instant::now();
        for &(sx, sy) in &screen {
            click_at(sx, sy)?;
            if ms > 0 {
                std::thread::sleep(Duration::from_millis(ms));
            }
        }
        let issue = t0.elapsed().as_secs_f64() * 1000.0;

        // Park, let the last click finish drawing, then ONE capture for the whole board.
        warp_cursor(px, py)?;
        std::thread::sleep(Duration::from_millis(350));
        let sel = selected_set(&capture_window(&win)?, &centres, &baseline, radius);
        let hits = sel.iter().filter(|s| **s).count();
        let all = hits == centres.len();
        if all && best.map(|b| ms < b).unwrap_or(true) {
            best = Some(ms);
        }
        let missing: Vec<usize> =
            sel.iter().enumerate().filter(|(_, s)| !**s).map(|(i, _)| i).collect();
        log.push_str(&format!(
            "| {ms} ms | {issue:.0} ms | {hits}/{} | {} |\n",
            centres.len(),
            if all { "all landed".to_string() } else { format!("**missed {missing:?}**") }
        ));

        // Deselect everything at a deliberately slow, known-good cadence so the next pass starts
        // from a clean board. Only tiles that actually got selected are clicked again.
        for (i, &(sx, sy)) in screen.iter().enumerate() {
            if sel[i] {
                click_at(sx, sy)?;
                std::thread::sleep(Duration::from_millis(60));
            }
        }
        warp_cursor(px, py)?;
        std::thread::sleep(Duration::from_millis(400));

        // Confirm the board really is clean, or the next row measures the wrong thing.
        let left = selected_set(&capture_window(&win)?, &centres, &baseline, radius)
            .iter()
            .filter(|s| **s)
            .count();
        if left != 0 {
            log.push_str(&format!("| | | | RESET FAILED, {left} still selected — stopping |\n"));
            break;
        }
    }

    log.push_str(&format!(
        "\n## Result\n\ntightest interval where all {} clicks landed: **{}**\n",
        centres.len(),
        match best {
            Some(0) => "0 ms (no delay at all)".to_string(),
            Some(ms) => format!("{ms} ms"),
            None => "none — even the slowest pass dropped clicks".to_string(),
        }
    ));

    game.close(Duration::from_secs(15));
    std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
    println!("{log}");
    Ok(())
}
