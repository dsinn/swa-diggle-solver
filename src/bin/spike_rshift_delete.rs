//! Does a **posted** modifier reach `love.keyboard.isDown`, and does `rshift+Delete` clear the word?
//!
//! `clearWord` would be the ideal way to take a word back off the board: one press, whatever is on
//! it, no need to know what got selected — which after a stray is exactly what is in doubt. It is
//! bound to `delete` (`utils/defaultbinds/keyboard.lua:12`) but gated:
//!
//! ```lua
//! -- main.lua:471-480
//! and (not set._mod[key] or set._mod[key] and (not getModFun or getModFun(set._mod[key])))
//! ```
//!
//! with `_mod = { delete = 'rshift' }` and `getModFun` being `love.keyboard.isDown`. So it needs
//! Right Shift *held*. We post `WM_KEYDOWN` rather than synthesising real input, and
//! `love.keyboard.isDown` reads SDL's key-state array rather than our message queue — whether the
//! two meet is the open question. Every word this program has ever submitted is a posted
//! `WM_KEYDOWN` for Space, so SDL certainly processes posted keys; whether a posted key updates the
//! *state array* that `isDown` consults is a different claim, and it is the one being tested.
//!
//! ## Why this is four measurements and not one
//!
//! "The board did not clear" has at least three causes, and telling them apart afterwards is
//! impossible without separating them here — an empty result proves nothing until the signal is
//! shown to be there.
//!
//! 1. **Positive control.** Select tiles and confirm they read as selected. If our own reading of
//!    the board is broken, nothing below means anything and the run says so instead of guessing.
//! 2. **The gate is real.** Press Delete with no modifier. It must *not* clear. If it does, our
//!    reading of `main.lua:475` is wrong — which is worth knowing, because a bare Delete then went
//!    into a commit that was reverted for nothing.
//! 3. **The experiment.** rshift down, Delete, rshift up. Clearing means posted modifiers reach
//!    `isDown`.
//! 4. **The fallback still works.** Backspace until clear. This is what makes a negative result in
//!    (3) informative: if backspace clears a board that rshift+Delete would not, the failure is the
//!    modifier and not the instrument.
//!
//! **This drives the real mouse and keyboard.** It selects tiles, clears them, and **never presses
//! Space** — nothing is submitted, so the fight is left exactly as it was found.

use diggle_solver::act;
use diggle_solver::combat::Board;
use diggle_solver::config::Config;
use diggle_solver::game::save;
use diggle_solver::layout;
use diggle_solver::observe::log::Console;
use diggle_solver::win::input::{
    click_at, warp_cursor, Input, PostMessageInput, SC_BACK, SC_DELETE, SC_RSHIFT, VK_BACK,
    VK_DELETE, VK_RSHIFT,
};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

const REPORT: &str = "spike-rshift-delete.md";
/// Somewhere with no hotspot, so hover rendering is never inside a reading.
const NEUTRAL: (i32, i32) = (300, 300);
/// Backspaces before giving up in step 4. One per letter, with room for two-letter tiles.
const MAX_BACKSPACES: usize = 32;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config::load(Path::new("config.toml"))?;
    diggle_solver::win::process::refuse_if_running("lovec.exe", &[])?;
    let save_dir = diggle_solver::game::savedir::locate(cfg.save_dir.clone(), true)?;

    let mut log = String::from("# Spike: does a posted rshift reach `love.keyboard.isDown`?\n\n");
    let mut console = Console::take()?;
    let mut game = diggle_solver::game::launch::GameProcess::launch(&cfg, &console)?;
    let win = game.wait_for_window(Duration::from_secs(20))?;
    std::thread::sleep(Duration::from_secs(3));

    let finish = |log: &str, game: &mut diggle_solver::game::launch::GameProcess| {
        game.close(Duration::from_secs(15));
        let _ = std::fs::File::create(REPORT).and_then(|mut f| f.write_all(log.as_bytes()));
        println!("{log}");
    };

    if act::click_when_ready(&win, &act::CONTINUE, Duration::from_secs(30)).is_err() {
        log.push_str("ABORT: no Continue\n");
        finish(&log, &mut game);
        return Ok(());
    }

    // Resuming drops straight into the fight `combatSaveData` holds.
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
        finish(&log, &mut game);
        return Ok(());
    };

    let n = combat.table_at("tileboard").map(|t| t.arr.len()).unwrap_or(16);
    let resolved = diggle_solver::geometry::Geometry::from_save(&combat, n);
    for p in &resolved.problems {
        log.push_str(&format!("- geometry note: {p}\n"));
    }
    let geom = resolved.geometry;
    let (cw, ch) = win.client_size()?;
    let centres = layout::tile_centres(&geom, cw, ch);
    let board = Board::new(&win, &geom)?;
    log.push_str(&format!(
        "\n{n} tiles in the dump, columns {:?}, {} centres\n\n",
        geom.rows_per_col,
        centres.len()
    ));

    // No restless list here: this spike drives the keyboard, not a planned word, so it has no reason
    // to have read the tiles. An empty list only means every column top is watched.
    if board.wait_until_ready(Duration::from_secs(20), &[])? == diggle_solver::combat::Ready::Never
    {
        log.push_str("ABORT: board never filled and settled\n");
        finish(&log, &mut game);
        return Ok(());
    }

    let keys = PostMessageInput::new(win);
    let park = || {
        let _ = warp_cursor(NEUTRAL.0, NEUTRAL.1);
    };

    // How many tiles read as selected against the untouched board.
    let baseline = board.read()?;
    let count = |b: &Board, base: &[f64]| -> Result<usize, diggle_solver::Error> {
        Ok(b.selected_now(base)?.into_iter().filter(|c| *c).count())
    };

    // Two tiles, far apart in the dump order so a mapping error is obvious.
    let picks: Vec<usize> = if centres.len() >= 4 { vec![0, centres.len() - 1] } else { vec![0] };

    // ---- 1. Positive control: can we select, and can we see it? ----
    log.push_str("## 1. Positive control — select and read back\n\n");
    for &i in &picks {
        let (sx, sy) = win.client_to_screen(centres[i].0, centres[i].1)?;
        click_at(sx, sy)?;
        std::thread::sleep(Duration::from_millis(250));
    }
    park();
    std::thread::sleep(Duration::from_millis(300));
    let selected = count(&board, &baseline)?;
    log.push_str(&format!("clicked tiles {picks:?}; {selected} tiles read as selected\n\n"));
    if selected == 0 {
        log.push_str(
            "ABORT: nothing registered as selected, so every later reading would be meaningless. \
             The instrument is the finding here, not the modifier.\n",
        );
        finish(&log, &mut game);
        return Ok(());
    }

    // ---- 2. The gate: bare Delete must NOT clear ----
    log.push_str("## 2. Bare Delete — expected to do nothing\n\n");
    keys.focus();
    std::thread::sleep(Duration::from_millis(150));
    keys.press_key(VK_DELETE, SC_DELETE)?;
    std::thread::sleep(Duration::from_millis(600));
    let after_bare = count(&board, &baseline)?;
    log.push_str(&format!(
        "{selected} selected before, {after_bare} after.  **{}**\n\n",
        if after_bare < selected {
            "CLEARED — the `_mod` gate is not what we read it to be"
        } else {
            "unchanged, as `_mod = { delete = 'rshift' }` predicts"
        }
    ));

    // ---- 3. The experiment: rshift held, then Delete ----
    log.push_str("## 3. rshift held + Delete\n\n");
    let before_mod = count(&board, &baseline)?;
    keys.focus();
    std::thread::sleep(Duration::from_millis(150));
    keys.key_down(VK_RSHIFT, SC_RSHIFT)?;
    std::thread::sleep(Duration::from_millis(120));
    keys.press_key(VK_DELETE, SC_DELETE)?;
    std::thread::sleep(Duration::from_millis(120));
    // Released on every path below, including the early returns: a modifier left down cannot be
    // observed from outside the game.
    keys.key_up(VK_RSHIFT, SC_RSHIFT)?;
    std::thread::sleep(Duration::from_millis(600));
    let after_mod = count(&board, &baseline)?;
    let modifier_worked = after_mod == 0 && before_mod > 0;
    log.push_str(&format!(
        "{before_mod} selected before, {after_mod} after.  **{}**\n\n",
        if modifier_worked {
            "CLEARED — a posted rshift does reach `love.keyboard.isDown`"
        } else {
            "not cleared — a posted modifier does not reach the state array `isDown` reads"
        }
    ));

    // ---- 4. Backspace still works, so a negative above means the modifier ----
    log.push_str("## 4. Backspace fallback\n\n");
    let before_bs = count(&board, &baseline)?;
    if before_bs == 0 {
        log.push_str("nothing left on the board to clear; step 3 had already emptied it\n\n");
    } else {
        keys.focus();
        let mut presses = 0;
        while presses < MAX_BACKSPACES && count(&board, &baseline)? > 0 {
            keys.press_key(VK_BACK, SC_BACK)?;
            presses += 1;
            std::thread::sleep(Duration::from_millis(180));
        }
        let left = count(&board, &baseline)?;
        log.push_str(&format!(
            "{before_bs} selected, {presses} backspaces, {left} left.  **{}**\n\n",
            if left == 0 {
                "cleared — so the instrument works and step 3's result is about the modifier"
            } else {
                "NOT cleared — the reading itself is suspect, not just the modifier"
            }
        ));
    }

    log.push_str(&format!(
        "## Verdict\n\n`rshift`+`Delete` {}.\n\nNothing was submitted; no Space was ever sent.\n",
        if modifier_worked { "**works**" } else { "**does not work**" }
    ));

    // Belt and braces: the game is about to close, but a stuck modifier is exactly the kind of
    // invisible state worth clearing twice.
    let _ = keys.key_up(VK_RSHIFT, SC_RSHIFT);
    finish(&log, &mut game);
    Ok(())
}
