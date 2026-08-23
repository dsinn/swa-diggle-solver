//! Live probe: does the game agree with our scorer, and does the border path throw?
//!
//! ## The question
//!
//! `spike_tile_score` established, by running the game's own `utils/tiles.lua` under LuaJIT, that
//! `tiles.score` throws on any tile without `extra.border` — and that where it *does* run we match
//! it exactly. But the real captured board is bare strings, so live tiles have no border, and the
//! game plainly works. Three facts, mutually inconsistent, and the source will not settle it.
//!
//! ## The measurement
//!
//! `wordboard.lua:232` computes `cache.score = words.score(...)` on **selection**, not submission.
//! So merely typing a word exercises the whole scoring path. If it throws, `--console` carries the
//! traceback and we have found a blocking defect. If it does not, the preview renders and the score
//! is real.
//!
//! Two readings are taken, because they fail differently:
//!
//! 1. **A frame with the word selected.** The damage preview highlights the health bar. Per the
//!    user this preview is not gospel — status effects can skew it, and armour-only damage moves
//!    nothing — but it is the only reading available *before* committing a turn.
//! 2. **The `rpg.enemy.health` delta from `combatSaveData`**, which is exact. `rpg:save()` runs at
//!    `PlayerTurn.onStart` (`rpgview.lua:1450`), so the post-damage value lands at the start of our
//!    next turn — known timing, not a guess.
//!
//! ## Why this word
//!
//! Amorphous carries `lexiconBonusSlime = 1.5`. A word in that lexicon would be multiplied and the
//! comparison would be meaningless, so the word is checked against every lexicon first. It is also
//! chosen to score BELOW the enemy's 3 health: a kill clamps at zero and only proves "≥ 3", whereas
//! a survivor gives an exact number.
//!
//! **This drives the real mouse and keyboard.** It clicks Continue and types into the game.

use diggle_solver::act;
use diggle_solver::config::Config;
use diggle_solver::game::save;
use diggle_solver::observe::board;
use diggle_solver::observe::log::{Console, LogMirror};
use diggle_solver::win::capture::capture_window;
use diggle_solver::win::input::{type_text_injected, Input, PostMessageInput, SC_SPACE, VK_SPACE};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

const REPORT: &str = "spike-word-score.md";
const RAW_LOG: &str = "spike-word-score-raw.log";
const FRAMES: &str = "spike-frames-live";

/// Candidate words for the crypt board, cheapest first. Each must be typeable from
/// `OYCAACTPORLIGAHJ` and score under the enemy's 3 health so the enemy survives to be measured.
const CANDIDATES: &[&str] = &["AT", "OAT", "TAR", "ART", "RAT"];

/// The board's letters straight out of `combatSaveData`, which carries the same list the log dump
/// does — entries are bare strings, or `{letter, extra}` for special tiles.
fn board_letters(save: &save::Table) -> String {
    save.table_at("tileboard")
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
        .unwrap_or_default()
}

fn enemy_health(save_dir: &Path) -> Option<(i64, i64, String)> {
    let t = save::load(&save_dir.join("combatSaveData")).ok()?;
    Some((
        t.int_at("rpg.enemy.health")?,
        t.int_at("rpg.enemy.armour").unwrap_or(0),
        t.str_at("rpg.enemy.name").unwrap_or("?").to_string(),
    ))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config::load(Path::new("config.toml"))?;
    std::fs::create_dir_all(FRAMES)?;
    diggle_solver::win::process::refuse_if_running("lovec.exe", &[])?;

    let save_dir = diggle_solver::game::savedir::locate(cfg.save_dir.clone(), true)?;
    let mut log = String::from("# Spike: live word score vs our prediction\n\n");

    let before = enemy_health(&save_dir);
    log.push_str(&format!("enemy before launch: {before:?}\n\n"));

    // Pick the word offline, so the choice is auditable and not a function of what happened to work.
    let scorer = diggle_solver::score::Scorer::new(&cfg.game_dir)?;
    let geometry = diggle_solver::geometry::Geometry::default();
    let tiles: Vec<board::Tile> =
        "OYCAACTPORLIGAHJ".chars().map(|c| board::Tile::plain(&c.to_string())).collect();
    let typist = diggle_solver::typist::Typist::new(&tiles, &geometry);
    let lexica = diggle_solver::lexica::Lexica::load(&cfg.game_dir)?;

    let mut chosen = None;
    for w in CANDIDATES {
        let Some(t) = typist.type_word(w) else {
            log.push_str(&format!("- {w}: not typeable from this board\n"));
            continue;
        };
        // Any lexicon at all, not merely a resisted one: a bonus would inflate the game's answer
        // and we would read our own blind spot as a scoring error.
        let in_lexicon = lexica.contains_anywhere(w);
        let consumed: Vec<board::Tile> = t.tiles.iter().map(|&i| tiles[i].clone()).collect();
        let predicted = scorer.score_typed(&consumed, w.chars().count(), 1.0);
        log.push_str(&format!(
            "- {w}: predicted {predicted}, tiles {:?}, corners {}, in a lexicon: {in_lexicon}\n",
            t.tiles, t.corners_used
        ));
        if chosen.is_none() && !in_lexicon && predicted >= 1 && predicted < 3 {
            chosen = Some((w.to_string(), predicted));
        }
    }
    let Some((word, predicted)) = chosen else {
        log.push_str("\nABORT: no candidate is lexicon-free and scores in 1..2\n");
        std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
        println!("{log}");
        return Ok(());
    };
    log.push_str(&format!("\n**chosen: {word}, predicted {predicted}**\n\n"));
    println!("chosen: {word}, predicted {predicted}");

    let mut console = Console::take()?;
    let mut mirror = LogMirror::create(Path::new(RAW_LOG))?;
    let mut game = diggle_solver::game::launch::GameProcess::launch(&cfg, &console)?;
    let win = game.wait_for_window(Duration::from_secs(20))?;
    std::thread::sleep(Duration::from_secs(3));
    let input = PostMessageInput::new(win);
    input.focus();
    std::thread::sleep(Duration::from_millis(500));

    let mut all_lines: Vec<String> = Vec::new();
    let pump = |console: &mut Console, mirror: &mut LogMirror, sink: &mut Vec<String>| {
        if let Ok(lines) = console.read_new() {
            if !lines.is_empty() {
                mirror.write(&lines);
                sink.extend(lines);
            }
        }
    };

    // ---- reach the fight ----
    // Continue is gated on its own face: Restart is the next button along and eulogises the run.
    match act::click_when_ready(&win, &act::CONTINUE, Duration::from_secs(30)) {
        Ok(i) => log.push_str(&format!("clicked Continue (inliers {i:.3})\n")),
        Err(e) => {
            log.push_str(&format!("ABORT: could not find Continue: {e}\n"));
            if let Ok(f) = capture_window(&win) {
                let _ = f.write_png(Path::new(&format!("{FRAMES}/word-no-continue.png")));
            }
            game.close(Duration::from_secs(15));
            std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
            return Ok(());
        }
    }

    // Gate on the SAVE, not on a log dump. `tileboard.logVerbose` runs from `PlayerTurn.onStart`,
    // and resuming a saved fight restores `turnState = "PlayerTurn"` without re-entering onStart —
    // so a resumed fight is fully interactive and prints nothing. Waiting for a dump here reported
    // "never reached an interactive player turn" about a board that was on screen and waiting.
    let deadline = Instant::now() + Duration::from_secs(40);
    let mut ready = None;
    while Instant::now() < deadline && ready.is_none() {
        std::thread::sleep(Duration::from_millis(400));
        pump(&mut console, &mut mirror, &mut all_lines);
        if let Ok(t) = save::load(&save_dir.join("combatSaveData")) {
            if t.str_at("rpg.player.turnState") == Some("PlayerTurn") {
                let letters = board_letters(&t);
                if !letters.is_empty() {
                    ready = Some(letters);
                }
            }
        }
    }
    let Some(letters) = ready else {
        log.push_str("ABORT: combatSaveData never showed an interactive PlayerTurn with a board\n");
        if let Ok(f) = capture_window(&win) {
            let _ = f.write_png(Path::new(&format!("{FRAMES}/word-no-board.png")));
        }
        game.close(Duration::from_secs(15));
        std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
        println!("{log}");
        return Ok(());
    };
    // The board takes a moment to finish falling into place after the screen loads.
    std::thread::sleep(Duration::from_secs(2));
    log.push_str(&format!("\nboard from combatSaveData: {letters}\n"));
    let live = enemy_health(&save_dir);
    log.push_str(&format!("enemy at our turn: {live:?}\n\n"));

    // The board may not be the one we planned against.
    if letters != "OYCAACTPORLIGAHJ" {
        log.push_str("NOTE: board differs from the planned one; prediction may not apply\n");
    }

    // ---- type the word, WITHOUT submitting ----
    // This alone drives `words.score` through `wordboard.updateEffectValues`. If the border path
    // throws, it throws here.
    //
    // `SendInput` delivers to whatever window holds FOREGROUND, not to a handle we chose. If the
    // user has alt-tabbed away, the letters land in their terminal and the game shows an empty
    // wordboard — which would read as "typing does not work" when the truth is "typing went
    // somewhere else". So foreground is taken and then re-checked, and a failure aborts rather
    // than producing a measurement of nothing.
    input.focus();
    std::thread::sleep(Duration::from_millis(400));
    if !input.has_foreground() {
        log.push_str(
            "ABORT: the game does not have foreground; injected text would go elsewhere\n",
        );
        game.close(Duration::from_secs(15));
        std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
        println!("{log}");
        return Ok(());
    }

    let lines_before_typing = all_lines.len();
    type_text_injected(&word, Duration::from_millis(120))?;
    if !input.has_foreground() {
        log.push_str("WARNING: foreground was lost DURING typing; the selection may be partial\n");
    }
    std::thread::sleep(Duration::from_millis(600));
    pump(&mut console, &mut mirror, &mut all_lines);

    let new_output: Vec<&String> = all_lines[lines_before_typing..].iter().collect();
    let errored = new_output.iter().any(|l| {
        let l = l.to_ascii_lowercase();
        l.contains("attempt to index") || l.contains("stack traceback") || l.contains("error:")
    });
    log.push_str(&format!("console after typing ({} lines):\n```\n", new_output.len()));
    for l in new_output.iter().take(40) {
        log.push_str(l);
        log.push('\n');
    }
    log.push_str("```\n\n");
    log.push_str(&format!("**Lua error while scoring the selection: {errored}**\n\n"));

    if let Ok(f) = capture_window(&win) {
        let p = format!("{FRAMES}/word-selected-{word}.png");
        let _ = f.write_png(Path::new(&p));
        log.push_str(&format!("selection frame (damage preview): `{p}`\n\n"));
    }

    // ---- submit and measure the exact delta ----
    if !input.has_foreground() {
        log.push_str(
            "NOTE: foreground lost before submit; re-taking it
",
        );
        input.focus();
        std::thread::sleep(Duration::from_millis(400));
    }
    input.press_key(VK_SPACE, SC_SPACE)?;
    log.push_str("submitted with Space (`affirmative` -> `wordboard.submit()`, rpg.lua:228-230)\n");

    // `rpg:save()` runs at PlayerTurn.onStart, so the post-damage figure appears with the next dump.
    let deadline = Instant::now() + Duration::from_secs(30);
    let dumps_before = board::parse_dumps(&all_lines).len();
    let mut after = None;
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(400));
        pump(&mut console, &mut mirror, &mut all_lines);
        if board::parse_dumps(&all_lines).len() > dumps_before {
            std::thread::sleep(Duration::from_millis(400));
            after = enemy_health(&save_dir);
            break;
        }
    }
    if let Ok(f) = capture_window(&win) {
        let _ = f.write_png(Path::new(&format!("{FRAMES}/word-after-submit.png")));
    }

    log.push_str(&format!("\nenemy after: {after:?}\n"));
    match (live, after) {
        (Some((h0, a0, _)), Some((h1, a1, _))) => {
            let dealt = (h0 - h1) + (a0 - a1);
            log.push_str(&format!(
                "\n## Result\n\nhealth {h0} -> {h1}, armour {a0} -> {a1}, so {dealt} dealt; \
                 we predicted {predicted}\n\n**{}**\n",
                if dealt == predicted {
                    "MATCH — the scoring model is confirmed against the live game".to_string()
                } else {
                    format!("MISMATCH — off by {}", dealt - predicted)
                }
            ));
        }
        _ => log.push_str("\ncould not read the enemy on both sides of the turn\n"),
    }

    game.close(Duration::from_secs(15));
    std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
    println!("\n{log}");
    Ok(())
}
