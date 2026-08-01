//! Open the anomaly, and skip the cinematic the way the game intends.
//!
//! The one-way door. Arriving at a surface node of level > 3 raises `You feel the ground rumble`
//! (`overworld/events/arrived/world_evil.lua`), whose single `Continue` calls `hellOpens`.
//!
//! ## Why this does not watch the beams
//!
//! `hellOpens` (`utils/events.lua:11-115`) sets `hell` to 0.1, regenerates the map, and then plays
//! a beam per shrine at `initDelay 3.5 + 2s` intervals with `setInteractionEnabled(false)`
//! throughout — the better part of twenty seconds during which nothing we send is read. But before
//! any of that it writes the post-anomaly world to disk:
//!
//! ```lua
//! local initStates = {} -- so we can save right away and main menu skip
//! ...
//! overworld:save()
//! ```
//!
//! The comment is the game telling us the skip is intended. So: dismiss the text, wait for `hell`
//! to appear in the save, then take the main menu and load it back.
//!
//! **The application is never closed to do this.** Reaching the menu in-game is Escape ->
//! `backOptions` (`utils/defaultbinds/keyboard.lua:9`) -> the options screen's `Menu` button
//! (`ui/options.lua:333-337`) -> `Continue`. Escape is otherwise a key this project does not send,
//! and it is sent here only because the click that follows it is already decided.
//!
//! **Drives the real mouse and sends Space. Spends the island's anomaly trigger — restore a
//! checkpoint to undo.**

use diggle_solver::config::Config;
use diggle_solver::observe::adjacency;
use diggle_solver::observe::event;
use diggle_solver::observe::feed::Feed;
use diggle_solver::observe::log::{Console, LogMirror};
use diggle_solver::overworld::WorldMap;
use diggle_solver::win::input::Input;
use diggle_solver::win::window::GameWindow;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

const REPORT: &str = "spike-anomaly.md";
const AREA_BUTTONS: diggle_solver::win::capture::Region =
    diggle_solver::win::capture::Region { nx: 0.0, ny: 0.68, nw: 0.45, nh: 0.18 };
const LOCATE_ME: (i32, i32) = (32, 918);
const EMPTY_MAP: (i32, i32) = (1750, 160);
const NEUTRAL: (i32, i32) = (300, 300);

fn park(win: &GameWindow) {
    if let Ok((x, y)) = win.client_to_screen(NEUTRAL.0, NEUTRAL.1) {
        let _ = diggle_solver::win::input::warp_cursor(x, y);
    }
}

/// Reads the `hell` flag out of `mainSaveData`. A float — `setHellValue(0.1)`.
fn hell_of(main: &Path) -> Option<f64> {
    let t = diggle_solver::game::save::load(main).ok()?;
    t.table_at("overworld.areaFlags")?.get("hell")?.as_f64()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config::load(Path::new("config.toml"))?;
    diggle_solver::win::process::refuse_if_running("lovec.exe", &[])?;
    let save_dir = diggle_solver::game::savedir::locate(cfg.save_dir.clone(), true)?;
    let main_save = save_dir.join("mainSaveData");

    let mut log = String::from("# Spike: opening the anomaly\n\n");
    let console = Console::take()?;
    let mirror = LogMirror::create(Path::new("spike-anomaly-raw.log"))?;
    let mut feed = Feed::new(console, Some(mirror));

    log.push_str(&format!("hell before: {:?}\n\n", hell_of(&main_save)));

    // ---- phase 1: travel onto the trigger and dismiss the text ----
    let mut game = diggle_solver::game::launch::GameProcess::launch(&cfg, feed_console(&feed))?;
    let win = game.wait_for_window(Duration::from_secs(20))?;
    std::thread::sleep(Duration::from_secs(3));
    let keys = diggle_solver::win::input::PostMessageInput::new(win);

    if diggle_solver::act::click_when_ready(&win, &diggle_solver::act::CONTINUE, Duration::from_secs(30)).is_err() {
        log.push_str("ABORT: no Continue\n");
        return finish(&mut game, &log);
    }

    let mut reader = adjacency::Reader::new();
    let mut map = WorldMap::new();
    let mut latest = None;
    let deadline = Instant::now() + Duration::from_secs(40);
    while Instant::now() < deadline && latest.is_none() {
        std::thread::sleep(Duration::from_millis(300));
        for a in reader.push(feed.pump()) {
            map.fold(&a);
            latest = Some(a);
        }
    }
    if latest.is_none() {
        log.push_str("ABORT: no adjacency dump\n");
        return finish(&mut game, &log);
    }
    if let Ok(save) = diggle_solver::game::save::load(&main_save) {
        map.apply_save(&save);
    }

    let Some(hop) = map.next_hop() else {
        log.push_str("ABORT: no hop proposed\n");
        return finish(&mut game, &log);
    };
    log.push_str(&format!(
        "at **{}**; step to **{}** for **{}** ({:?})\n\n",
        map.here().unwrap_or("?"),
        hop.step,
        hop.plan.target,
        hop.plan.reason
    ));

    // Fresh coordinates: the world-load dump predates the view settling.
    let (ex, ey) = win.client_to_screen(EMPTY_MAP.0, EMPTY_MAP.1)?;
    diggle_solver::win::input::click_at(ex, ey)?;
    std::thread::sleep(Duration::from_millis(500));
    for a in reader.push(feed.pump()) {
        map.fold(&a);
        latest = Some(a);
    }
    let (lx, ly) = win.client_to_screen(LOCATE_ME.0, LOCATE_ME.1)?;
    diggle_solver::win::input::click_at(lx, ly)?;
    park(&win);
    let pan_by = Instant::now() + Duration::from_secs(12);
    let mut fresh = None;
    while Instant::now() < pan_by && fresh.is_none() {
        std::thread::sleep(Duration::from_millis(250));
        for a in reader.push(feed.pump()) {
            map.fold(&a);
            if a.reason.contains("pan") {
                fresh = Some(a.clone());
            }
            latest = Some(a);
        }
    }
    let Some(fresh) = fresh.or(latest) else {
        log.push_str("ABORT: no usable dump\n");
        return finish(&mut game, &log);
    };
    let Some(target) = fresh.nodes.iter().find(|n| n.key == hop.step).cloned() else {
        log.push_str(&format!("ABORT: {} is not on screen\n", hop.step));
        return finish(&mut game, &log);
    };

    let before = diggle_solver::win::capture::capture_window(&win)?;
    let (sx, sy) = win.client_to_screen(target.x as i32, target.y as i32)?;
    diggle_solver::win::input::click_at(sx, sy)?;
    std::thread::sleep(Duration::from_millis(900));
    feed.pump();
    let after = diggle_solver::win::capture::capture_window(&win)?;
    if before.diff_fraction(&after, AREA_BUTTONS) <= 0.01 {
        log.push_str("ABORT: selection did not register, so Space is not safe\n");
        return finish(&mut game, &log);
    }
    keys.focus();
    std::thread::sleep(Duration::from_millis(200));
    keys.press_key(diggle_solver::win::input::VK_SPACE, diggle_solver::win::input::SC_SPACE)?;
    park(&win);
    log.push_str(&format!("travelling to **{}**\n", hop.step));

    // ---- the rumble ----
    let mark = feed.mark();
    let by = Instant::now() + Duration::from_secs(60);
    let mut dismissed = false;
    let mut seen_event = None;
    while Instant::now() < by && !dismissed {
        std::thread::sleep(Duration::from_millis(300));
        feed.pump();
        if let Some(ev) = event::parse_events(feed.since(mark)).pop() {
            log.push_str(&format!("\n## Event\n\n**{}**\n\n{}\n\nchoices: {:?}\n", ev.title, ev.text,
                ev.choices.iter().map(|c| &c.text).collect::<Vec<_>>()));
            seen_event = Some(ev.title.clone());
            // A single forced Continue -- `world_evil.lua:26-29`.
            if let Some(c) = ev.choices.first() {
                // `onActive` announces the screen at the START of its transition, so the first
                // attempt clicked into a screen that was still fading in and the button — visibly
                // present the whole time — never took it. Same shape as the pan: an announcement
                // says a screen exists, never that it is ready.
                let _ = diggle_solver::observe::settle::wait_for_quiescence(
                    &win,
                    0.02,
                    Duration::from_secs(8),
                );
                let (cx, cy) = win.client_to_screen(c.x, c.y)?;
                for attempt in 1..=4 {
                    let before = diggle_solver::win::capture::capture_window(&win)?;
                    diggle_solver::win::input::click_at(cx, cy)?;
                    park(&win);
                    std::thread::sleep(Duration::from_millis(900));
                    feed.pump();
                    let after = diggle_solver::win::capture::capture_window(&win)?;
                    let moved = before.diff_fraction(&after, diggle_solver::observe::settle::FULL);
                    log.push_str(&format!(
                        "  `{}` at ({},{}), attempt {attempt}: screen moved {moved:.3}\n",
                        c.text, c.x, c.y
                    ));
                    // Dismissing it starts the cinematic, which is anything but subtle.
                    if moved > 0.05 {
                        dismissed = true;
                        break;
                    }
                }
                if !dismissed {
                    let _ = diggle_solver::win::capture::capture_window(&win)
                        .map(|f| f.write_png(Path::new("spike-frames-live/anomaly-stuck.png")));
                }
            }
        }
    }
    if !dismissed {
        log.push_str(&format!("\n**no event appeared** (seen: {seen_event:?}) — did we arrive?\n"));
        return finish(&mut game, &log);
    }

    // `hellOpens` writes the post-anomaly world before the beams start, so the save is the signal
    // that it is safe to skip them.
    let by = Instant::now() + Duration::from_secs(45);
    let mut opened = None;
    while Instant::now() < by && opened.is_none() {
        std::thread::sleep(Duration::from_millis(400));
        feed.pump();
        opened = hell_of(&main_save).filter(|h| *h != 0.0);
    }
    log.push_str(&format!("\nhell after dismissing: **{opened:?}**\n"));
    if opened.is_none() {
        log.push_str("**the anomaly did not open** — not skipping anything\n");
        return finish(&mut game, &log);
    }

    // ---- phase 2: the skip, in-game ----
    //
    // Escape -> options -> `Menu` -> main menu -> Continue. The game never closes: the world is
    // already on disk, so reloading it from the main menu is the skip, and it is the path the
    // "main menu skip" comment describes.
    log.push_str("\n## Skip\n\nEscape to options\n");
    let before_opts = diggle_solver::win::capture::capture_window(&win)?;
    keys.focus();
    std::thread::sleep(Duration::from_millis(300));
    keys.press_key(diggle_solver::win::input::VK_ESCAPE, diggle_solver::win::input::SC_ESCAPE)?;
    park(&win);
    std::thread::sleep(Duration::from_millis(1200));
    feed.pump();
    let after_opts = diggle_solver::win::capture::capture_window(&win)?;
    let moved = before_opts.diff_fraction(&after_opts, diggle_solver::observe::settle::FULL);
    log.push_str(&format!("screen changed by {moved:.3} after Escape\n"));
    let _ = after_opts.write_png(Path::new("spike-frames-live/anomaly-options.png"));
    if moved < 0.05 {
        log.push_str("**Escape did nothing** — interaction is probably disabled during the beams\n");
        return finish(&mut game, &log);
    }

    // `Menu`, a `small` (100x100) button at ss (1, 0) with xOffset -2.63, yOffset 0.38
    // (`ui/options.lua:333-337`), so (1657, 38) at 1920x1080. Red, top right.
    let (mx, my) = win.client_to_screen(1657, 38)?;
    diggle_solver::win::input::click_at(mx, my)?;
    park(&win);
    std::thread::sleep(Duration::from_millis(1500));
    feed.pump();
    let _ = diggle_solver::win::capture::capture_window(&win)
        .map(|f| f.write_png(Path::new("spike-frames-live/anomaly-mainmenu.png")));
    log.push_str("clicked Menu\n");

    if diggle_solver::act::click_when_ready(&win, &diggle_solver::act::CONTINUE, Duration::from_secs(30)).is_err() {
        log.push_str("ABORT: no Continue on the main menu\n");
        return finish(&mut game, &log);
    }
    log.push_str("continued from the main menu\n");

    let mut reader = adjacency::Reader::new();
    let mut map = WorldMap::new();
    let by = Instant::now() + Duration::from_secs(40);
    let mut arrived = None;
    while Instant::now() < by && arrived.is_none() {
        std::thread::sleep(Duration::from_millis(300));
        for a in reader.push(feed.pump()) {
            map.fold(&a);
            arrived = Some(a);
        }
    }
    if let Ok(save) = diggle_solver::game::save::load(&main_save) {
        map.apply_save(&save);
    }
    log.push_str(&format!(
        "\n## Result\n\nback in the world at **{}**, hell **{:?}**, anomaly available: {:?}\n\n{} places known\n",
        map.here().unwrap_or("?"),
        hell_of(&main_save),
        map.anomaly_available(),
        map.len()
    ));
    for p in map.places() {
        log.push_str(&format!(
            "- `{}` {}{}\n",
            p.key,
            if p.heading.is_empty() { "(unheaded)" } else { &p.heading },
            if p.corrupted { " [corrupted]" } else { "" }
        ));
    }
    if let Some(plan) = map.next_target() {
        log.push_str(&format!("\nnext: **{}** ({:?})\n", plan.target, plan.reason));
    }
    let _ = arrived;
    finish(&mut game, &log)
}

/// The console the game must attach to. `Feed` owns it, and launching needs a reference.
fn feed_console(feed: &Feed) -> &Console {
    feed.console()
}

fn finish(
    game: &mut diggle_solver::game::launch::GameProcess, log: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    game.close(Duration::from_secs(15));
    std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
    println!("{log}");
    Ok(())
}
