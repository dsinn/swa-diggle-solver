//! Task 25 follow-up: deterministically navigate the start menu's hotspot graph
//! to the Start button using GetCursorPos as ground truth.
//!
//! `utils/input.lua`'s `setHotspotHighlight` calls `love.mouse.setPosition`
//! whenever a new hotspot is highlighted -- it warps the REAL OS cursor onto
//! whatever control is highlighted. That means we do not need screenshots or
//! pixel diffing to know which control an arrow key selected: `GetCursorPos`
//! tells us exactly where the highlight is, in the same client-area pixel space
//! `ButtonSpec`/`button_center` already use.
//!
//! This spike presses a bounded, mixed-direction sequence of arrow keys,
//! reading the cursor after every single press, until the cursor lands inside
//! the Start button's rectangle (or the budget runs out). It never presses
//! Return during that exploration -- only once the cursor is confirmed inside
//! the rectangle does it press Return and check whether the game left the
//! start menu.
//!
//! Run: cargo run --bin spike_hotspot_graph -- config.toml

use diggle_solver::observe::settle::{self, sample_noise_floor, wait_for_quiescence};
use diggle_solver::win::capture::Frame;
use diggle_solver::win::input::{
    Input, PostMessageInput, SC_DOWN, SC_LEFT, SC_RETURN, SC_RIGHT, SC_UP, VK_DOWN, VK_LEFT,
    VK_RETURN, VK_RIGHT, VK_UP,
};
use diggle_solver::win::window::{self, GameWindow};
use diggle_solver::{config::Config, game::launch::PipedGameProcess};
use std::io::Write;
use std::path::Path;
use std::time::Duration;
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

const FRAMES_DIR: &str = "spike-frames-4";
const FLOOR_SAMPLES: usize = 5;
const FLOOR_GAP: Duration = Duration::from_millis(300);
const SETTLE_TIMEOUT: Duration = Duration::from_secs(6);
const INITIAL_GENEROUS_FLOOR: f64 = 0.5;
const INITIAL_SETTLE_TIMEOUT: Duration = Duration::from_secs(20);
/// Gap between an arrow press and reading GetCursorPos -- long enough for
/// love.mouse.setPosition to have taken effect, short enough to keep the walk
/// fast.
const ARROW_SETTLE: Duration = Duration::from_millis(250);

/// Start button: ButtonSpec{ss_x:0, ss_y:0.75, os_x:2.0, w:250, h:100}, centre
/// (500, 810) at 1920x1080 (verified independently in Task 24/25). Rectangle
/// is the button's full span, x 375..625, y 760..860.
fn in_start_rect(x: i32, y: i32) -> bool {
    (375..=625).contains(&x) && (760..=860).contains(&y)
}

/// Known/estimated button centres at 1920x1080 for labeling observed cursor
/// positions. Continue and Start are independently verified (Task 24). The
/// rest are estimates: Quit is computed from the coordinator's spec
/// (ss(1,0), 'small' 100x100, xOffset -2.63, yOffset 0.38 => (1657, 38));
/// Discord/Patreon/Arcade/Almanac/Options positions are visual estimates from
/// captured frames, not source-verified specs, and are labeled as such in the
/// report.
const KNOWN_BUTTONS: &[(&str, i32, i32)] = &[
    ("Continue", 187, 810),
    ("Start", 500, 810),
    ("Quit", 1657, 38),
    ("Patreon (est.)", 100, 42),
    ("Discord (est.)", 255, 42),
    ("Options-gear (est.)", 1857, 42),
    ("Arcade (est.)", 1420, 810),
    ("Almanac (est.)", 1732, 810),
];

fn nearest_label(x: i32, y: i32) -> (&'static str, f64) {
    let mut best = ("(unrecognized)", f64::MAX);
    for &(name, bx, by) in KNOWN_BUTTONS {
        let d = (((x - bx).pow(2) + (y - by).pow(2)) as f64).sqrt();
        if d < best.1 {
            best = (name, d);
        }
    }
    best
}

fn cursor_pos() -> POINT {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    p
}

fn reset_sandbox_save() -> Result<(), Box<dyn std::error::Error>> {
    let appdata = std::env::var("APPDATA")?;
    let sandbox = std::path::Path::new(&appdata).join("LOVE").join("SternlyWordedAdventures");
    let target = sandbox.join("persistentSaveData");
    if target.exists() {
        std::fs::remove_file(&target)?;
    }
    Ok(())
}

fn save(frame: &Frame, name: &str) -> Result<String, Box<dyn std::error::Error>> {
    let path = format!("{FRAMES_DIR}/{name}.bmp");
    frame.write_bmp(Path::new(&path))?;
    Ok(path)
}

struct Step {
    idx: usize,
    arrow: &'static str,
    before: (i32, i32),
    after: (i32, i32),
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).expect("usage: spike_hotspot_graph <config.toml>");
    let cfg = Config::load(std::path::Path::new(&path))?;

    std::fs::create_dir_all(FRAMES_DIR)?;
    reset_sandbox_save()?;

    let mut game = PipedGameProcess::launch_with_env(&cfg, &[])?;

    let win: GameWindow = loop {
        if let Some(w) = window::find_by_pid(game.pid()) {
            break w;
        }
        if !game.is_running() {
            return Err("game exited before a window appeared".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    };

    let input = PostMessageInput::new(win);

    // --- Ride out the splash/fade-in, sample the lore card's floor. ---
    wait_for_quiescence(&win, INITIAL_GENEROUS_FLOOR, INITIAL_SETTLE_TIMEOUT)?;
    let mut floor = sample_noise_floor(&win, FLOOR_SAMPLES, FLOOR_GAP)?;

    // --- Mandatory positive control: Return must dismiss the lore card. ---
    let before = wait_for_quiescence(&win, floor, SETTLE_TIMEOUT)?;
    input.press_key(VK_RETURN, SC_RETURN)?;
    let after = wait_for_quiescence(&win, floor, SETTLE_TIMEOUT)?;
    let positive_control_reacted = settle::reacted(&before, &after, floor);

    if !positive_control_reacted {
        let _ = game.kill();
        return Err("positive control (Return on lore card) did not react -- harness is broken, aborting before touching the hotspot graph".into());
    }

    // Re-sample the floor on the start menu, ride out its reveal animation.
    floor = sample_noise_floor(&win, FLOOR_SAMPLES, FLOOR_GAP)?;
    let _ = wait_for_quiescence(&win, floor, SETTLE_TIMEOUT)?;

    // --- Record the cursor BEFORE any arrow press: tells us the default
    // highlight state, if any, without spending a press on it. ---
    let initial_cursor = cursor_pos();
    let mut steps: Vec<Step> = Vec::new();

    // --- Bounded, mixed-direction exploration walk. Down/Right are weighted
    // heavier since Start is below-and-right of the top row where the default
    // highlight is expected to sit (Task 25's config E ended on the top-left
    // Patreon icon after Down,Up,Left,Right). Stops the instant the cursor
    // lands in the Start rectangle -- never presses Return during this phase. ---
    let sequence: [(&str, u16, u16); 16] = [
        ("Down", VK_DOWN, SC_DOWN),
        ("Right", VK_RIGHT, SC_RIGHT),
        ("Right", VK_RIGHT, SC_RIGHT),
        ("Left", VK_LEFT, SC_LEFT),
        ("Down", VK_DOWN, SC_DOWN),
        ("Right", VK_RIGHT, SC_RIGHT),
        ("Up", VK_UP, SC_UP),
        ("Left", VK_LEFT, SC_LEFT),
        ("Down", VK_DOWN, SC_DOWN),
        ("Right", VK_RIGHT, SC_RIGHT),
        ("Down", VK_DOWN, SC_DOWN),
        ("Right", VK_RIGHT, SC_RIGHT),
        ("Up", VK_UP, SC_UP),
        ("Down", VK_DOWN, SC_DOWN),
        ("Right", VK_RIGHT, SC_RIGHT),
        ("Left", VK_LEFT, SC_LEFT),
    ];

    let mut found_at: Option<usize> = None;
    for (i, (label, vk, sc)) in sequence.iter().enumerate() {
        let cb = cursor_pos();
        input.press_extended_key(*vk, *sc)?;
        std::thread::sleep(ARROW_SETTLE);
        let ca = cursor_pos();
        steps.push(Step { idx: i, arrow: label, before: (cb.x, cb.y), after: (ca.x, ca.y) });
        if in_start_rect(ca.x, ca.y) {
            found_at = Some(i);
            break;
        }
    }

    // --- Distinct observed positions, clustered within 20px, labeled against
    // known/estimated button centres. ---
    let mut distinct: Vec<(i32, i32)> = Vec::new();
    distinct.push((initial_cursor.x, initial_cursor.y));
    for s in &steps {
        distinct.push(s.after);
    }
    let mut clusters: Vec<(i32, i32)> = Vec::new();
    for p in &distinct {
        let matches_existing = clusters
            .iter()
            .any(|c| (((c.0 - p.0).pow(2) + (c.1 - p.1).pow(2)) as f64).sqrt() < 20.0);
        if !matches_existing {
            clusters.push(*p);
        }
    }

    // --- If found, verify still inside the rect, capture before/after, press
    // Return, and check whether the game left the start menu.
    //
    // Up to 2 attempts: `love.mousemoved` (main.lua:420) clears the hotspot
    // highlight on ANY real mouse delta, even a stray hardware jitter from a
    // human at the keyboard during our press. If that happens between our
    // posted WM_KEYDOWN and WM_KEYUP for Return, `input.keyreleasefunction`
    // finds no highlight and silently never calls `love.mousereleased` --
    // the press registers but the release never does, so nothing activates.
    // Task 24 documented exactly this class of non-deterministic drift on
    // this dev machine. A bounded retry, re-verifying (and if needed
    // re-establishing) the highlight each time, distinguishes "the mechanism
    // doesn't work" from "a stray real input broke this one attempt". ---
    const MAX_RETURN_ATTEMPTS: usize = 2;
    let mut return_pressed = false;
    let mut return_reacted = false;
    let mut return_delta = 0.0f64;
    let mut return_threshold = 0.0f64;
    let mut cursor_before_return = (0, 0);
    let mut cursor_after_keyup = (0, 0);
    let mut cursor_after_return = (0, 0);
    let mut before_bmp = String::new();
    let mut after_bmp = String::new();
    let mut attempt_log = String::new();

    if found_at.is_some() {
        for attempt in 1..=MAX_RETURN_ATTEMPTS {
            let mut cb = cursor_pos();
            if !in_start_rect(cb.x, cb.y) {
                // Highlight likely got cleared between attempts; re-run the
                // confirmed Down,Right prefix to re-land on Start.
                input.press_extended_key(VK_DOWN, SC_DOWN)?;
                std::thread::sleep(ARROW_SETTLE);
                input.press_extended_key(VK_RIGHT, SC_RIGHT)?;
                std::thread::sleep(ARROW_SETTLE);
                cb = cursor_pos();
            }
            attempt_log.push_str(&format!(
                "attempt {attempt}: cursor before Return = ({}, {}), in_start_rect={}\n",
                cb.x,
                cb.y,
                in_start_rect(cb.x, cb.y)
            ));
            if !in_start_rect(cb.x, cb.y) {
                attempt_log.push_str(&format!(
                    "attempt {attempt}: could not re-establish the Start highlight, skipping Return this attempt.\n"
                ));
                continue;
            }
            cursor_before_return = (cb.x, cb.y);

            let before_frame = wait_for_quiescence(&win, floor, SETTLE_TIMEOUT)?;
            before_bmp = save(&before_frame, &format!("hotspot-start-before-return-{attempt}"))?;
            input.press_key(VK_RETURN, SC_RETURN)?;
            return_pressed = true;
            let cak = cursor_pos();
            cursor_after_keyup = (cak.x, cak.y);
            let keyup_drift = (((cak.x - cb.x).pow(2) + (cak.y - cb.y).pow(2)) as f64).sqrt();

            let after_frame = wait_for_quiescence(&win, floor, SETTLE_TIMEOUT)?;
            after_bmp = save(&after_frame, &format!("hotspot-start-after-return-{attempt}"))?;
            return_delta = before_frame.diff_fraction(&after_frame, settle::FULL);
            return_threshold = (floor * settle::REACT_MULTIPLE).max(settle::REACT_FLOOR);
            return_reacted = settle::reacted(&before_frame, &after_frame, floor);
            let ca = cursor_pos();
            cursor_after_return = (ca.x, ca.y);

            attempt_log.push_str(&format!(
                "attempt {attempt}: cursor immediately after keyup = ({}, {}) [drift during press/release gap: {:.1}px]\n",
                cak.x, cak.y, keyup_drift
            ));
            attempt_log.push_str(&format!(
                "attempt {attempt}: delta={return_delta:.4} threshold={return_threshold:.4} reacted={return_reacted} \
                 cursor after settle=({}, {})\n",
                ca.x, ca.y
            ));

            if return_reacted {
                break;
            }
            if !game.is_running() {
                attempt_log.push_str("game exited; stopping retries.\n");
                break;
            }
            if attempt < MAX_RETURN_ATTEMPTS {
                floor = sample_noise_floor(&win, FLOOR_SAMPLES, FLOOR_GAP)?;
            }
        }
    }

    let _ = game.kill();
    std::thread::sleep(Duration::from_millis(500));

    // --- Report ---
    let mut log = String::new();
    log.push_str(&format!(
        "step -1 (initial, no press): cursor = ({}, {}) -- nearest known: {} (dist {:.1})\n",
        initial_cursor.x,
        initial_cursor.y,
        nearest_label(initial_cursor.x, initial_cursor.y).0,
        nearest_label(initial_cursor.x, initial_cursor.y).1,
    ));
    for s in &steps {
        let (label, dist) = nearest_label(s.after.0, s.after.1);
        log.push_str(&format!(
            "step {:2} arrow={:<5} before=({:4},{:4}) after=({:4},{:4}) -- nearest known: {} (dist {:.1}){}\n",
            s.idx, s.arrow, s.before.0, s.before.1, s.after.0, s.after.1, label, dist,
            if found_at == Some(s.idx) { "  <== IN START RECT" } else { "" },
        ));
    }

    let mut clusters_desc = String::new();
    for c in &clusters {
        let (label, dist) = nearest_label(c.0, c.1);
        clusters_desc.push_str(&format!(
            "- ({}, {}) -- nearest known: {} (dist {:.1})\n",
            c.0, c.1, label, dist
        ));
    }

    let sequence_desc = match found_at {
        Some(k) => {
            let seq: Vec<&str> = steps[..=k].iter().map(|s| s.arrow).collect();
            format!("FOUND after {} press(es): [{}]", k + 1, seq.join(", "))
        }
        None => format!(
            "NOT FOUND within the {}-press bounded walk from the observed initial state.",
            sequence.len()
        ),
    };

    let report = format!(
        "# Task 25 follow-up: hotspot graph navigation to Start (GetCursorPos-based)\n\n\
         positive control (Return dismisses lore card) reacted: {positive_control_reacted}\n\n\
         initial cursor (before any arrow press): ({}, {})\n\n\
         ## Full (arrow, cursor-before, cursor-after) log\n{log}\n\
         ## Distinct observed hotspot positions (clustered within 20px)\n{clusters_desc}\n\
         ## Arrow sequence that reaches Start\n{sequence_desc}\n\n\
         ## Return-on-Start result (up to {MAX_RETURN_ATTEMPTS} attempts)\n{attempt_log}\n\
         return_pressed (any attempt): {return_pressed}\n\
         cursor before final Return attempt: ({}, {})\n\
         cursor immediately after keyup (before settle): ({}, {})\n\
         cursor after settle: ({}, {})\n\
         delta: {return_delta:.4}\n\
         threshold: {return_threshold:.4}\n\
         reacted (left the start menu): {return_reacted}\n\
         before/after frames (last attempt): {before_bmp} -> {after_bmp}\n",
        initial_cursor.x,
        initial_cursor.y,
        cursor_before_return.0,
        cursor_before_return.1,
        cursor_after_keyup.0,
        cursor_after_keyup.1,
        cursor_after_return.0,
        cursor_after_return.1,
    );

    std::fs::File::create(format!("{FRAMES_DIR}/report.md"))?.write_all(report.as_bytes())?;
    print!("{report}");

    Ok(())
}
