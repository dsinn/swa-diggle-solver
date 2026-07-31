//! The combat loop: fight until the dungeon is clear, then reach the reward screen.
//!
//! Everything under this is already measured, so this is wiring rather than discovery:
//!
//! - tile positions derived from the game's hotspot maths — 16/16 correct, zero cross-talk
//! - clicks confirmed one at a time (`combat::Board::select_word`), so no gap ever opens
//! - scoring confirmed live against the game (health 3 → 2 against a prediction of 1)
//! - the board rect captured with `BitBlt`, 4.4 ms and pixel-identical to `PrintWindow`
//!
//! ## Turn gating
//!
//! `combatSaveData` is the authority for *whose turn it is*, because `rpg:save()` runs at
//! `PlayerTurn.onStart` (`rpgview.lua:1450`) — the exact moment control returns to us.
//!
//! It is **not** authority for whether the board has settled. I first assumed it was, reasoning that
//! `PlayerPreTurn.shouldEnd` is `tileboard.boardIsStatic()` so `PlayerTurn` implies static tiles.
//! That holds on a normal transition and fails on a **resume**: loading a mid-combat save restores
//! `turnState` directly, skipping the gate, while the tiles are still dropping in. The first live run
//! of this loop clicked into a half-empty falling board and reported the click as hitting four wrong
//! tiles. `Board::wait_until_static` is the fix, and it runs before any baseline is taken.
//!
//! Two things the save cannot tell us, both read from the `--console` channel instead: the board
//! itself while a turn is in progress, and `"Item selection:"` (`ui/itemselection.lua:415`), which
//! is how the reward screen announces itself — and it prints each item WITH screen coordinates.
//!
//! ## Safety
//!
//! - `Finish` is clicked only with `turnState == WaitPhase` confirmed from the save. Normalized
//!   (0.72, 0.9) is never clicked: `Hint`, `Fight on!`, `Cancel` and `Attack!` all share that slot.
//! - Both buttons are fingerprinted on the first legitimate `WaitPhase`, which is the only moment
//!   they render.
//!
//! **This drives the real mouse and keyboard.**

use diggle_solver::combat::Board;
use diggle_solver::config::Config;
use diggle_solver::game::save::{self, Table};
use diggle_solver::geometry::Geometry;
use diggle_solver::observe::board as boardparse;
use diggle_solver::observe::log::{Console, LogMirror};
use diggle_solver::search::{self, Dictionary, Modifiers};
use diggle_solver::win::capture::capture_window;
use diggle_solver::win::input::{Input, PostMessageInput, SC_SPACE, VK_SPACE};
use diggle_solver::win::window::{ButtonSpec, GameWindow};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

const REPORT: &str = "spike-combat.md";
const FRAMES: &str = "spike-frames-live";
/// `Finish` / `Give up` share this slot; `Finish` is safe only in `WaitPhase`.
const FINISH: ButtonSpec =
    ButtonSpec { ss_x: 0.9, ss_y: 0.9, os_x: 0.0, os_y: 0.0, w: 300.0, h: 100.0 };
/// Somewhere with no hotspot, to keep hover out of the readings.
const NEUTRAL: (i32, i32) = (300, 300);
const MAX_TURNS: usize = 40;

fn tiles_of(save: &Table) -> Vec<diggle_solver::observe::board::Tile> {
    save.table_at("tileboard")
        .map(|tb| {
            tb.arr
                .iter()
                .filter_map(|v| {
                    if let Some(s) = v.as_str() {
                        return Some(diggle_solver::observe::board::Tile::plain(s));
                    }
                    let t = v.as_table()?;
                    let letter = t.arr.first()?.as_str()?.to_string();
                    let quality = t
                        .arr
                        .get(1)
                        .and_then(|v| v.as_table())
                        .map(diggle_solver::observe::board::Quality::from_extra)
                        .unwrap_or_default();
                    Some(diggle_solver::observe::board::Tile { letter, quality })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn park(win: &GameWindow) {
    if let Ok((x, y)) = win.client_to_screen(NEUTRAL.0, NEUTRAL.1) {
        let _ = diggle_solver::win::input::warp_cursor(x, y);
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config::load(Path::new("config.toml"))?;
    std::fs::create_dir_all(FRAMES)?;
    diggle_solver::win::process::refuse_if_running("lovec.exe", &[])?;
    let save_dir = diggle_solver::game::savedir::locate(cfg.save_dir.clone(), true)?;
    let combat_path = save_dir.join("combatSaveData");

    let mut log = String::from("# Spike: the combat loop\n\n");
    let scorer = diggle_solver::score::Scorer::new(&cfg.game_dir)?;
    let dict = Dictionary::load(&cfg.game_dir)?;
    log.push_str(&format!("loaded {} words\n\n", dict.len()));

    let mut console = Console::take()?;
    let mut mirror = LogMirror::create(Path::new("spike-combat-raw.log"))?;
    let mut game = diggle_solver::game::launch::GameProcess::launch(&cfg, &console)?;
    let win = game.wait_for_window(Duration::from_secs(20))?;
    std::thread::sleep(Duration::from_secs(3));
    let keys = PostMessageInput::new(win);
    keys.focus();

    let mut lines: Vec<String> = Vec::new();
    let mut pump = |c: &mut Console, m: &mut LogMirror, sink: &mut Vec<String>| {
        if let Ok(new) = c.read_new() {
            if !new.is_empty() {
                m.write(&new);
                sink.extend(new);
            }
        }
    };

    if diggle_solver::act::click_when_ready(
        &win,
        &diggle_solver::act::CONTINUE,
        Duration::from_secs(30),
    )
    .is_err()
    {
        log.push_str("ABORT: no Continue\n");
        game.close(Duration::from_secs(15));
        std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
        println!("{log}");
        return Ok(());
    }
    log.push_str("clicked Continue\n\n");

    let mut turns = 0usize;
    let mut fingerprinted = false;
    let mut reached_rewards = false;
    let deadline = Instant::now() + Duration::from_secs(300);

    while Instant::now() < deadline && turns < MAX_TURNS && !reached_rewards {
        pump(&mut console, &mut mirror, &mut lines);
        if lines.iter().any(|l| l.contains("Item selection:")) {
            reached_rewards = true;
            break;
        }
        let Ok(cs) = save::load(&combat_path) else {
            std::thread::sleep(Duration::from_millis(300));
            continue;
        };
        let state = cs.str_at("rpg.player.turnState").unwrap_or("").to_string();

        match state.as_str() {
            "WaitPhase" | "SmokebombWaitPhase" => {
                // The only moment Finish and Fight on! both render. Capture before clicking.
                if !fingerprinted {
                    if let Ok(f) = capture_window(&win) {
                        let _ = f.write_png(Path::new(&format!("{FRAMES}/waitphase-buttons.png")));
                        log.push_str(&format!(
                            "captured WaitPhase buttons -> `{FRAMES}/waitphase-buttons.png`\n"
                        ));
                    }
                    fingerprinted = true;
                }
                let (fx, fy) = win.button_center(&FINISH)?;
                let (sx, sy) = win.client_to_screen(fx, fy)?;
                log.push_str(&format!("WaitPhase confirmed -> clicking Finish at ({fx},{fy})\n"));
                diggle_solver::win::input::click_at(sx, sy)?;
                std::thread::sleep(Duration::from_secs(2));
            }
            "PlayerTurn" => {
                turns += 1;
                let tiles = tiles_of(&cs);
                if tiles.is_empty() {
                    std::thread::sleep(Duration::from_millis(300));
                    continue;
                }
                let health = cs.int_at("rpg.enemy.health").unwrap_or(0);
                let armour = cs.int_at("rpg.enemy.armour").unwrap_or(0);
                let name = cs.str_at("rpg.enemy.name").unwrap_or("?").to_string();
                let (mods, geom) = Modifiers::from_save(&cfg.game_dir, &cs, tiles.len())?;
                for p in &mods.problems {
                    log.push_str(&format!("  WARNING {p}\n"));
                }

                let need = health + armour;
                let out = search::race_for_kill(&dict, &scorer, &tiles, &geom, &mods, need, 8);
                let letters: String = tiles.iter().map(|t| t.letter.as_str()).collect();
                let Some(found) = out.choice().cloned() else {
                    log.push_str(&format!("turn {turns}: nothing playable on {letters}\n"));
                    break;
                };

                let typist = diggle_solver::typist::Typist::new(&tiles, &geom);
                let Some(typed) = typist.type_word(&found.word) else {
                    log.push_str(&format!("turn {turns}: {} is not realizable\n", found.word));
                    break;
                };

                log.push_str(&format!(
                    "turn {turns}: {name} {health}+{armour}hp, board {letters}\n  \
                     play **{}** (scores {}, tiles {:?}, {} corners)\n",
                    found.word, found.score, typed.tiles, typed.corners_used
                ));

                let board = Board::new(&win, &geom)?;
                park(&win);
                // The physics animation must finish first. `turnState == PlayerTurn` does NOT imply
                // a settled board when the fight was RESUMED from a save: the state is restored
                // directly, skipping `PlayerPreTurn`'s `boardIsStatic()` gate, while the tiles are
                // still dropping in. A baseline taken then describes a board that no longer exists.
                if !board.wait_until_ready(Duration::from_secs(20))? {
                    log.push_str("  board never filled/settled -- not clicking into a moving board
");
                    break;
                }
                match board.select_word(&typed.tiles) {
                    Ok(()) => {
                        park(&win);
                        std::thread::sleep(Duration::from_millis(150));
                        keys.focus();
                        std::thread::sleep(Duration::from_millis(150));
                        keys.press_key(VK_SPACE, SC_SPACE)?;
                        log.push_str("  selected and submitted\n");
                    }
                    Err(e) => {
                        log.push_str(&format!("  SELECTION FAILED: {e}\n"));
                        if let Ok(f) = capture_window(&win) {
                            let _ = f
                                .write_png(Path::new(&format!("{FRAMES}/combat-select-fail.png")));
                        }
                        break;
                    }
                }

                // Wait for the turn to move on: anything but PlayerTurn, or a changed board.
                let turn_deadline = Instant::now() + Duration::from_secs(20);
                while Instant::now() < turn_deadline {
                    std::thread::sleep(Duration::from_millis(250));
                    pump(&mut console, &mut mirror, &mut lines);
                    if lines.iter().any(|l| l.contains("Item selection:")) {
                        break;
                    }
                    if let Ok(next) = save::load(&combat_path) {
                        let s = next.str_at("rpg.player.turnState").unwrap_or("");
                        let letters_now: String =
                            tiles_of(&next).iter().map(|t| t.letter.clone()).collect();
                        if s != "PlayerTurn" || letters_now != letters {
                            break;
                        }
                    } else {
                        break; // the file went away: combat is over
                    }
                }
            }
            other => {
                if !other.is_empty() {
                    // PlayerPreTurn, EnemyTurn, EnemyDying: the game is animating. Waiting on the
                    // save is enough -- PlayerTurn only begins once the board is static.
                    std::thread::sleep(Duration::from_millis(300));
                } else {
                    std::thread::sleep(Duration::from_millis(300));
                }
            }
        }
    }

    pump(&mut console, &mut mirror, &mut lines);
    let items: Vec<&String> = lines
        .iter()
        .skip_while(|l| !l.contains("Item selection:"))
        .take(8)
        .collect();
    log.push_str(&format!(
        "\n## Result\n\nturns played: {turns}\nreached the reward screen: **{reached_rewards}**\n"
    ));
    if !items.is_empty() {
        log.push_str("\n```\n");
        for l in items {
            log.push_str(l);
            log.push('\n');
        }
        log.push_str("```\n");
    }
    if let Ok(f) = capture_window(&win) {
        let _ = f.write_png(Path::new(&format!("{FRAMES}/combat-final.png")));
    }

    game.close(Duration::from_secs(15));
    std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
    println!("{log}");
    Ok(())
}
