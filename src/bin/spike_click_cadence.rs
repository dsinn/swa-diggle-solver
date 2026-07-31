//! How fast can we click tiles, and how cheaply can we confirm each one landed?
//!
//! Three numbers, all measured rather than assumed:
//!
//! 1. **Capture cost.** `PrintWindow(PW_RENDERFULLCONTENT)` forces the window to redraw into a DC,
//!    so it is a fixed price per call and cannot be asked for a sub-rectangle. If it is expensive,
//!    the lever is *fewer* captures, not smaller ones.
//! 2. **Luma cost.** The arithmetic over one tile's box. Expected to be noise next to the capture,
//!    but "expected" is how this project has been wrong before.
//! 3. **Click → visible latency.** Click a tile, then capture in a tight loop until that tile's
//!    luminance moves. This is the real answer to "what cadence?" — the game runs at 120 FPS
//!    (8.3 ms/frame), but the number that matters is how many frames pass before a click is both
//!    processed and drawn, plus whatever the capture path adds.
//!
//! The point is to replace a guessed sleep with a measured one. The current spike uses 120 ms before
//! the click and 280 ms after, which were picked to be obviously safe, not to be right.
//!
//! **This drives the real mouse.** It never submits and never presses a key.

use diggle_solver::act;
use diggle_solver::config::Config;
use diggle_solver::game::save;
use diggle_solver::layout;
use diggle_solver::observe::log::Console;
use diggle_solver::win::capture::{capture_client_rect, capture_window, Frame};
use diggle_solver::win::input::{inject_left_click, warp_cursor};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

const REPORT: &str = "spike-click-cadence.md";
const NEUTRAL: (i32, i32) = (300, 300);
const CHANGED: f64 = 12.0;
/// How long to keep polling for a click to become visible before calling it lost.
const PATIENCE: Duration = Duration::from_millis(1500);

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
    if n == 0.0 { 0.0 } else { sum / n }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config::load(Path::new("config.toml"))?;
    diggle_solver::win::process::refuse_if_running("lovec.exe", &[])?;
    let save_dir = diggle_solver::game::savedir::locate(cfg.save_dir.clone(), true)?;

    let mut log = String::from("# Spike: click cadence and verification cost\n\n");
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

    let n_tiles = combat.table_at("tileboard").map(|t| t.arr.len()).unwrap_or(16);
    let (cw, ch) = win.client_size()?;
    let geom = diggle_solver::geometry::Geometry::from_save(&combat, n_tiles).geometry;
    let centres = layout::tile_centres(&geom, cw, ch);
    let radius = (layout::tile_radius(cw, ch) * 0.55).round() as i32;

    // ---- 1. capture cost ----
    let mut cap_times = Vec::new();
    let mut frame = capture_window(&win)?; // warm up, so the first alloc is not counted
    for _ in 0..12 {
        let t = Instant::now();
        frame = capture_window(&win)?;
        cap_times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    cap_times.sort_by(|a, b| a.partial_cmp(b).unwrap());

    // ---- 2. luma cost, one tile and the whole board ----
    let t = Instant::now();
    for _ in 0..1000 {
        std::hint::black_box(tile_luma(&frame, centres[0].0, centres[0].1, radius));
    }
    let one_tile_us = t.elapsed().as_secs_f64() * 1000.0; // 1000 iterations -> us per call
    let t = Instant::now();
    for _ in 0..100 {
        for &(x, y) in &centres {
            std::hint::black_box(tile_luma(&frame, x, y, radius));
        }
    }
    // 100 passes over the whole board -> microseconds per pass.
    let all_tiles_us = t.elapsed().as_secs_f64() * 1_000_000.0 / 100.0;

    log.push_str(&format!(
        "## Costs\n\n\
         | what | time |\n|---|---|\n\
         | `capture_window` median | **{:.1} ms** |\n\
         | `capture_window` min / max | {:.1} / {:.1} ms |\n\
         | luma, one tile ({}x{} px box) | {one_tile_us:.1} µs |\n\
         | luma, all {} tiles | {all_tiles_us:.1} µs |\n\n",
        cap_times[cap_times.len() / 2],
        cap_times[0],
        cap_times[cap_times.len() - 1],
        radius * 2 + 1,
        radius * 2 + 1,
        centres.len()
    ));

    // ---- 3. click -> visible latency ----
    // Poll as fast as the capture allows and record when the tile first moves. Both the select and
    // the deselect are timed: if they differ, the animation is asymmetric and the slower one sets
    // the cadence.
    log.push_str("## Click → visible latency\n\n| tile | select | deselect |\n|---|---|---|\n");
    let park = || -> Result<(), Box<dyn std::error::Error>> {
        let (sx, sy) = win.client_to_screen(NEUTRAL.0, NEUTRAL.1)?;
        warp_cursor(sx, sy)?;
        Ok(())
    };

    let (bx, by, bw, bh) = layout::board_rect(&geom, cw, ch);
    log.push_str(&format!(
        "
Latency polled with the CHEAP capture ({bw}x{bh} BitBlt), so the floor is ~4 ms rather          than the ~25 ms `PrintWindow` imposed on the earlier run.

"
    ));
    let mut latencies = Vec::new();
    let sample: Vec<usize> = (0..centres.len()).step_by(3).collect();
    for &i in &sample {
        let (cx, cy) = centres[i];
        let (sx, sy) = win.client_to_screen(cx, cy)?;
        let mut row = Vec::new();

        for phase in ["select", "deselect"] {
            park()?;
            std::thread::sleep(Duration::from_millis(200));
            let before =
                tile_luma(&capture_client_rect(&win, bx, by, bw, bh)?, cx - bx, cy - by, radius);

            warp_cursor(sx, sy)?;
            let t0 = Instant::now();
            inject_left_click(1)?;

            let mut seen = None;
            while t0.elapsed() < PATIENCE {
                let f = capture_client_rect(&win, bx, by, bw, bh)?;
                if (tile_luma(&f, cx - bx, cy - by, radius) - before).abs() > CHANGED {
                    seen = Some(t0.elapsed());
                    break;
                }
            }
            row.push(match seen {
                Some(d) => {
                    let ms = d.as_secs_f64() * 1000.0;
                    latencies.push(ms);
                    format!("{ms:.0} ms")
                }
                None => {
                    // The cursor sits ON the tile here, so hover is included -- a miss means the
                    // click genuinely did not register, not that we failed to see it.
                    format!("**LOST** ({phase})")
                }
            });
        }
        log.push_str(&format!("| {i} | {} | {} |\n", row[0], row[1]));
    }

    // Leave the board clean.
    park()?;
    std::thread::sleep(Duration::from_millis(300));

    latencies.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if !latencies.is_empty() {
        let median = latencies[latencies.len() / 2];
        let worst = latencies[latencies.len() - 1];
        log.push_str(&format!(
            "\nmedian **{median:.0} ms**, worst **{worst:.0} ms** over {} clicks\n\n\
             Note these are bounded BELOW by the capture: the poll cannot see a change sooner than \
             one capture takes, so the true input-to-draw latency is smaller than the figure above.\n",
            latencies.len()
        ));
    } else {
        log.push_str("\nno latencies recorded\n");
    }

    game.close(Duration::from_secs(15));
    std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
    println!("{log}");
    Ok(())
}
