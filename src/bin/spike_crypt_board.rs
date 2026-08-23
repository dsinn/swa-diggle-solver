//! Get into the crypt and capture a REAL board dump plus a real `combatSaveData`.
//!
//! Everything downstream of here — the mlua scoring harness, the board parser, the lethality test —
//! depends on the actual shape of two artifacts I have so far only read the *writers* of. Reading a
//! writer tells you what a function returns; it does not tell you what `table.serialize` puts on the
//! wire, how it wraps across console rows, or which fields are present in practice. So this captures
//! raw and commits the bytes.
//!
//! Route, all of it already verified:
//! 1. Keyboard-navigate to Continue and press Return (`win::nav`, then `Return`).
//! 2. Read the adjacency dump, aim at the adjacent node, and double-click it — injected clicks
//!    reach SDL where posted ones do not (`spike_real_click`).
//! 3. Click the progress button through the arrival cutscene, detected by TEMPLATE MATCH rather than
//!    by clicking a coordinate blind (`templates/progress-button.png`).
//! 4. Wait for `combatSaveData` to appear — it is written by `rpg.save`, and `PlayerTurn.onStart`
//!    calls it (`rpgview.lua:1567`), so its existence means combat has begun and it is our turn.
//!
//! SAFETY, from the phase analysis:
//! - **Normalized (0.72, 0.9) is a forbidden coordinate.** `Hint` and `Fight on!` share it
//!   (`rpg.lua:504`, `:531`), and `Fight on!` commits to another enemy. Nothing here clicks it.
//! - `Finish` is at (0.9, 0.9), shared with `Give up`, and is only correct in `WaitPhase`. This spike
//!   does not click it either: it captures and leaves.
//! - No default keybind maps `fightOn` (`utils/defaultbinds/keyboard.lua`), so no key sent here can
//!   trigger it.
//!
//! WARNING: injects real mouse input, so it takes the pointer for a while.
//!
//! Run: cargo run --release --bin spike_crypt_board -- config.toml

use diggle_solver::config::Config;
use diggle_solver::observe::adjacency;
use diggle_solver::observe::log::{Console, LogMirror};
use diggle_solver::win::capture::capture_window;
use diggle_solver::win::input::{inject_left_click, warp_cursor, Input, PostMessageInput};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

const REPORT: &str = "spike-frames-live/crypt-board-report.md";
const RAW_LOG: &str = "spike-frames-live/crypt-board-log.txt";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg_path = std::env::args().nth(1).unwrap_or_else(|| "config.toml".into());
    std::fs::create_dir_all("spike-frames-live")?;
    let cfg = Config::load(Path::new(&cfg_path))?;
    let save_dir = diggle_solver::game::savedir::locate(cfg.save_dir.clone(), true)?;
    let combat_save = save_dir.join("combatSaveData");
    // Start from a known state: a stale file from an earlier run would look like instant success.
    let had_stale = combat_save.exists();

    let mut log = String::from("# Spike: real crypt board dump\n\n");
    log.push_str(&format!(
        "save dir: `{}`\nstale combatSaveData present before we start: {had_stale}\n\n",
        save_dir.display()
    ));

    let mut console = Console::take()?;
    let mut mirror = LogMirror::create(Path::new(RAW_LOG))?;
    let mut reader = adjacency::Reader::new();
    let mut game = diggle_solver::game::launch::GameProcess::launch(&cfg, &console)?;
    let win = game.wait_for_window(Duration::from_secs(20))?;
    std::thread::sleep(Duration::from_secs(3));
    let input = PostMessageInput::new(win);
    input.focus();
    std::thread::sleep(Duration::from_millis(500));

    // Drains the console into the mirror and returns any completed adjacency dumps.
    let pump = |console: &mut Console,
                mirror: &mut LogMirror,
                reader: &mut adjacency::Reader|
     -> Result<Vec<adjacency::Adjacency>, Box<dyn std::error::Error>> {
        let lines = console.read_new()?;
        if lines.is_empty() {
            return Ok(Vec::new());
        }
        mirror.write(&lines);
        Ok(reader.push(&lines))
    };

    // ---- 1. reach the overworld ----
    // CLICKED, not navigated. Keyboard hotspot nav failed here on the previous attempt — the
    // highlight never left (-1271,578) — and it is the wrong tool anyway now that injected clicks
    // work. The click is gated on recognising Continue's own face, because `Restart` is the next
    // button along and eulogises the run (`ui/heroselect.lua:271`).
    match diggle_solver::act::click_when_ready(
        &win,
        &diggle_solver::act::CONTINUE,
        Duration::from_secs(30),
    ) {
        Ok(inliers) => log.push_str(&format!("clicked Continue (inliers {inliers:.3})\n\n")),
        Err(e) => {
            log.push_str(&format!("ABORT: {e}\n"));
            if let Ok(f) = capture_window(&win) {
                let _ = f.write_png(Path::new("spike-frames-live/crypt-no-continue.png"));
            }
            game.close(Duration::from_secs(15));
            std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
            return Ok(());
        }
    }

    let mut map = None;
    let deadline = Instant::now() + Duration::from_secs(25);
    while Instant::now() < deadline && map.is_none() {
        std::thread::sleep(Duration::from_millis(400));
        map = pump(&mut console, &mut mirror, &mut reader)?.pop();
    }
    let Some(map) = map else {
        log.push_str("ABORT: no adjacency dump after Continue\n");
        game.close(Duration::from_secs(15));
        std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
        return Ok(());
    };
    let Some(target) = map.nodes.iter().find(|n| n.has_combat()).cloned() else {
        log.push_str("ABORT: no adjacent COMBAT node to enter — nothing to fight\n");
        game.close(Duration::from_secs(15));
        std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
        return Ok(());
    };
    log.push_str(&format!(
        "at {} — {}\ntarget {} — {} (level {:?}) at ({:.1},{:.1})\n\n",
        map.here_key,
        map.here_heading,
        target.key,
        target.heading,
        target.level(),
        target.x,
        target.y
    ));

    // ---- 2. travel there ----
    let (sx, sy) = win.client_to_screen(target.x.round() as i32, target.y.round() as i32)?;
    warp_cursor(sx, sy)?;
    std::thread::sleep(Duration::from_millis(500));
    inject_left_click(2)?; // doubleClickEffect -> travelTo
    log.push_str(&format!("double-clicked {} at screen ({sx},{sy})\n\n", target.key));

    let mut arrived = false;
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline && !arrived {
        std::thread::sleep(Duration::from_millis(400));
        for a in pump(&mut console, &mut mirror, &mut reader)? {
            if a.here_key == target.key {
                arrived = true;
                log.push_str(&format!("[{}] arrived at {}\n\n", a.reason, a.here_key));
            }
        }
    }
    if !arrived {
        log.push_str("ABORT: never saw an arrival dump for the target\n");
        game.close(Duration::from_secs(15));
        std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
        return Ok(());
    }

    // ---- 3. advance to combat, driven by what the LOG says the screen is ----
    //
    // The first attempt pattern-matched the cutscene plaque and clicked it until combat started.
    // That got through four `Lore screen:` pages and then stalled at inliers 0.599 for a minute,
    // because the next screen is not a cutscene at all: the log said `Pregame screen: nil`.
    //
    // The log announces every screen it enters, which is a far better signal than guessing from
    // pixels. So dispatch on the announcement:
    //   - `Lore screen:`    -> click the progress plaque (matched, never a blind coordinate)
    //   - `Pregame screen:` -> press Space. The `Start` button declares
    //     `userFunctionName = 'affirmative'` (`ui/pregame.lua:142-148`), which is exactly the
    //     condition under which Space is safe (design v2 §3.1). Note `End demo` shares that slot
    //     on demo builds but declares NO userFunctionName, so Space cannot trigger it.
    log.push_str("## Advancing to combat\n\n");
    let mut clicks = 0;
    let mut screen = String::new();
    let mut stuck = 0;
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        let lines = console.read_new()?;
        if !lines.is_empty() {
            mirror.write(&lines);
            let _ = reader.push(&lines);
            for l in &lines {
                let t = l.trim();
                if let Some(name) = t.split(" screen:").next().filter(|_| t.contains(" screen:")) {
                    screen = name.to_string();
                    log.push_str(&format!("- log announces screen: `{screen}`\n"));
                    stuck = 0;
                }
            }
        }
        if combat_save.exists() {
            log.push_str("- combatSaveData appeared\n");
            break;
        }

        if screen == "Pregame" {
            input.press_key(0x20, 0x39)?; // Space -> affirmative -> Start
            log.push_str("- pressed Space on the pregame screen\n");
            std::thread::sleep(Duration::from_millis(2000));
            stuck += 1;
        } else {
            // Through the verified-click helper, so the plaque is recognised before anything is
            // clicked rather than after.
            match diggle_solver::act::click(&win, &diggle_solver::act::PROGRESS) {
                Ok(inliers) => {
                    clicks += 1;
                    log.push_str(&format!(
                        "- click {clicks}: plaque verified, inliers {inliers:.3}\n"
                    ));
                    std::thread::sleep(Duration::from_millis(1400));
                    stuck = 0;
                }
                Err(_) => {
                    stuck += 1;
                    if stuck % 8 == 1 {
                        // Screenshot when stuck, not only on success: "no match" is uninterpretable
                        // without seeing what is actually on screen, and the first attempt spent a
                        // minute reporting 0.599 with no way to tell why.
                        let p = format!("spike-frames-live/crypt-stuck-{stuck}.png");
                        if let Ok(f) = capture_window(&win) {
                            let _ = f.write_png(Path::new(&p));
                        }
                        log.push_str(&format!("- stuck, screen=`{screen}`, saved `{p}`\n"));
                    }
                    std::thread::sleep(Duration::from_millis(1200));
                }
            }
        }
        if stuck > 30 {
            log.push_str("- giving up on advancing\n");
            break;
        }
    }

    // ---- 4. capture the artifacts ----
    log.push_str(&format!("\n## Combat\n\ncombatSaveData appeared: {}\n\n", combat_save.exists()));
    if combat_save.exists() {
        // Give the first PlayerTurn a moment to log its board, then drain.
        for _ in 0..12 {
            std::thread::sleep(Duration::from_millis(500));
            let _ = pump(&mut console, &mut mirror, &mut reader)?;
        }
        let copy = Path::new("spike-frames-live/combatSaveData.lua");
        std::fs::copy(&combat_save, copy)?;
        log.push_str(&format!("copied to `{}`\n\n", copy.display()));
        match diggle_solver::game::save::load(&combat_save) {
            Ok(t) => {
                log.push_str(&format!(
                    "top-level keys: {:?}\n\n",
                    t.map.keys().collect::<Vec<_>>()
                ));
                for p in [
                    "rpg.player.turnState",
                    "rpg.player.health",
                    "rpg.player.turnNumber",
                    "rpg.enemy.name",
                    "rpg.enemy.health",
                    "rpg.enemy.armour",
                    "rpg.scenario",
                ] {
                    log.push_str(&format!("- `{p}` = {:?}\n", t.path(p)));
                }
                if let Some(tb) = t.table_at("tileboard") {
                    log.push_str(&format!(
                        "\ntileboard: {} entries; columns={:?}\nletters: {:?}\n",
                        tb.arr.len(),
                        tb.get("columns").and_then(|c| c.as_table()).map(|c| c
                            .arr
                            .iter()
                            .filter_map(|v| v.as_int())
                            .collect::<Vec<_>>()),
                        tb.arr
                    ));
                }
                if let Some(gf) = t.table_at("rpg.player.gearFlags") {
                    log.push_str(&format!(
                        "\ngearFlags: {:?}\n",
                        gf.map.keys().collect::<Vec<_>>()
                    ));
                } else {
                    log.push_str("\ngearFlags: <absent>\n");
                }
            }
            Err(e) => log.push_str(&format!("save parse FAILED: {e}\n")),
        }
        if let Ok(f) = capture_window(&win) {
            let _ = f.write_png(Path::new("spike-frames-live/crypt-combat.png"));
            log.push_str("\nscreenshot: `spike-frames-live/crypt-combat.png`\n");
        }
    }

    log.push_str(&format!("\nraw log mirrored to `{RAW_LOG}` — the primary artifact\n"));
    // Deliberately does NOT click Finish or Fight on. Capture and leave.
    let exited = game.close(Duration::from_secs(15));
    log.push_str(&format!("game exited gracefully: {exited}\n"));
    std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
    console.echo(&format!("done — {REPORT}\n"));
    Ok(())
}
