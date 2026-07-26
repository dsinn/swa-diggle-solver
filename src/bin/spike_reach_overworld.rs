//! Task 24: act-verify loop with an adaptive noise floor, plus posted mouse
//! clicks, driving the game from a fresh save as far towards the overworld as
//! it will go.
//!
//! Supersedes Spike 4 (Task 23)'s flawed stall detector: that spike measured
//! its idle baseline ONCE, on the lore card, and reused it forever. The start
//! menu animates a mascot continuously, so every later frame read as
//! "reacted" and it pressed Return 20 times at the same screen. Here the
//! noise floor is re-sampled after every transition (see
//! `diggle_solver::observe::settle`), so ambient animation sits inside the
//! floor and a genuine transition towers over it.
//!
//! Also tests the first posted **mouse click** against this game: Spike 4
//! stalled at the start menu because its buttons declare no
//! `userFunctionName`, so Return does nothing there. Getting past it needs a
//! real (posted) left-click on the Start button.
//!
//! Run: cargo run --bin spike_reach_overworld -- config.toml

use diggle_solver::observe::settle::{self, sample_noise_floor, wait_for_quiescence};
use diggle_solver::win::capture::Frame;
use diggle_solver::win::input::{Input, PostMessageInput, SC_RETURN, VK_RETURN};
use diggle_solver::win::window::{self, ButtonSpec, GameWindow};
use diggle_solver::{config::Config, game::launch::GameProcess};
use std::io::Write;
use std::path::Path;
use std::time::Duration;
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
use windows::Win32::Foundation::POINT;

const FRAMES_DIR: &str = "spike-frames-2";
/// How many samples sample_noise_floor takes per screen, and how far apart.
const FLOOR_SAMPLES: usize = 5;
const FLOOR_GAP: Duration = Duration::from_millis(300);
/// Ceiling on how long wait_for_quiescence will wait for an animation to finish.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(6);
/// Generous floor used only for the very first settle, which must ride out
/// the publisher splash / lore-card fade-in before any real floor sampling
/// can happen.
const INITIAL_GENEROUS_FLOOR: f64 = 0.5;
const INITIAL_SETTLE_TIMEOUT: Duration = Duration::from_secs(20);
/// Up to this many further Return presses after the Start click.
const MAX_FURTHER_STEPS: usize = 12;

fn cursor_pos() -> POINT {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    p
}

/// Deletes persistentSaveData from the SANDBOX save dir only
/// (%APPDATA%\LOVE\SternlyWordedAdventures\), forcing the auto-save lore card
/// to appear on next launch. Never touches %APPDATA%\SternlyWordedAdventures\
/// (no LOVE folder), which is the user's real Steam save.
fn reset_sandbox_save() -> Result<(), Box<dyn std::error::Error>> {
    let appdata = std::env::var("APPDATA")?;
    let sandbox = std::path::Path::new(&appdata).join("LOVE").join("SternlyWordedAdventures");
    let target = sandbox.join("persistentSaveData");
    if target.exists() {
        std::fs::remove_file(&target)?;
    }
    Ok(())
}

struct StepRecord {
    label: String,
    floor: f64,
    delta: f64,
    threshold: f64,
    reacted: bool,
    before_bmp: String,
    after_bmp: String,
}

fn record_step(
    steps: &mut Vec<StepRecord>,
    label: &str,
    floor: f64,
    before: &Frame,
    after: &Frame,
    before_bmp: String,
    after_bmp: String,
) -> bool {
    let delta = before.diff_fraction(after, settle::FULL);
    let threshold = (floor * settle::REACT_MULTIPLE).max(settle::REACT_FLOOR);
    let r = settle::reacted(before, after, floor);
    steps.push(StepRecord {
        label: label.to_string(),
        floor,
        delta,
        threshold,
        reacted: r,
        before_bmp,
        after_bmp,
    });
    r
}

fn save(frame: &Frame, name: &str) -> Result<String, Box<dyn std::error::Error>> {
    let path = format!("{FRAMES_DIR}/{name}.bmp");
    frame.write_bmp(Path::new(&path))?;
    Ok(path)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).expect("usage: spike_reach_overworld <config.toml>");
    let cfg = Config::load(std::path::Path::new(&path))?;

    std::fs::create_dir_all(FRAMES_DIR)?;
    std::fs::create_dir_all(".superpowers/sdd/2026-07-25-diggle-solver-milestone-1")?;

    reset_sandbox_save()?;

    let mut game = GameProcess::launch(&cfg)?;

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
    let cursor_before = cursor_pos();

    let mut steps: Vec<StepRecord> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    // --- Step 2: ride out the splash/fade-in with a generous floor, then
    // sample the lore card's real idle floor. ------------------------------
    let mut current = wait_for_quiescence(&win, INITIAL_GENEROUS_FLOOR, INITIAL_SETTLE_TIMEOUT)?;
    let mut floor = sample_noise_floor(&win, FLOOR_SAMPLES, FLOOR_GAP)?;
    let lore_floor = floor;
    let lore_before_path = save(&current, "00-lore-before")?;
    notes.push(format!(
        "Initial settle: generous floor {INITIAL_GENEROUS_FLOOR}, timeout {INITIAL_SETTLE_TIMEOUT:?}. \
         Lore-card idle noise floor sampled ({FLOOR_SAMPLES} samples, {FLOOR_GAP:?} apart): {lore_floor:.4}. \
         Frame saved: {lore_before_path}"
    ));

    // --- Step 3: press Return to dismiss the lore card. --------------------
    let before = current.clone();
    input.press_key(VK_RETURN, SC_RETURN)?;
    let after = wait_for_quiescence(&win, floor, SETTLE_TIMEOUT)?;
    let after_path = save(&after, "01-lore-after-return")?;
    let before_path = lore_before_path.clone();
    let r = record_step(
        &mut steps,
        "Return (dismiss lore card)",
        floor,
        &before,
        &after,
        before_path,
        after_path,
    );
    current = after;
    if !r {
        notes.push("WARNING: Return on the lore card did not register as a reaction.".into());
    }

    // Re-sample the floor: we should now be on the start menu, whose mascot
    // animates continuously, so this floor will be much higher.
    floor = sample_noise_floor(&win, FLOOR_SAMPLES, FLOOR_GAP)?;
    notes.push(format!(
        "Start-menu idle noise floor re-sampled after the lore-card transition: {floor:.4} \
         (compare to lore card's {lore_floor:.4} -- the mascot's ambient animation)."
    ));

    // The start menu has a one-time title/mascot reveal animation on top of the
    // steady-state mascot loop. Ride that out (using the floor we just sampled,
    // which is itself already the steady-state ambient level) before clicking,
    // so the click lands on a screen that isn't itself mid-transient.
    current = wait_for_quiescence(&win, floor, SETTLE_TIMEOUT)?;

    // --- Step 4: click Start. This is the new capability under test. -------
    let spec = ButtonSpec { ss_x: 0.0, ss_y: 0.75, os_x: 2.0, os_y: 0.0, w: 250.0, h: 100.0 };
    let (bx, by) = win.button_center(&spec)?;
    notes.push(format!("Resolved Start button center: client-area pixel ({bx}, {by})."));

    let before = current.clone();
    let before_path = save(&before, "02-startmenu-before-click")?;
    input.click(bx, by)?;
    let after = wait_for_quiescence(&win, floor, SETTLE_TIMEOUT)?;
    let after_path = save(&after, "03-startmenu-after-click")?;
    let mut click_reacted = record_step(
        &mut steps,
        &format!("Click Start at ({bx}, {by})"),
        floor,
        &before,
        &after,
        before_path,
        after_path,
    );
    current = after;

    if click_reacted {
        notes.push("Posted mouse click on Start REACTED on the first attempt -- this is the capability that stalled Spike 4.".into());
    } else {
        notes.push(format!(
            "First posted click on Start did NOT react (delta below max({}x{floor:.4}, {})). \
             Retrying once more before concluding, given how consequential this measurement is.",
            settle::REACT_MULTIPLE, settle::REACT_FLOOR,
        ));
        let before2 = current.clone();
        let before2_path = save(&before2, "03b-startmenu-before-click-retry")?;
        input.click(bx, by)?;
        let after2 = wait_for_quiescence(&win, floor, SETTLE_TIMEOUT)?;
        let after2_path = save(&after2, "03c-startmenu-after-click-retry")?;
        let retry_reacted = record_step(
            &mut steps,
            &format!("Click Start RETRY at ({bx}, {by})"),
            floor,
            &before2,
            &after2,
            before2_path,
            after2_path,
        );
        current = after2;
        click_reacted = retry_reacted;
        notes.push(format!("Retry click reacted: {retry_reacted}."));
    }

    if click_reacted {
        floor = sample_noise_floor(&win, FLOOR_SAMPLES, FLOOR_GAP)?;
        notes.push(format!("Noise floor re-sampled on the screen after the Start click: {floor:.4}."));
    } else {
        notes.push("Posted mouse click on Start did NOT react on either attempt -- floor left unchanged for the next step.".into());
    }

    // --- Step 5: up to MAX_FURTHER_STEPS further Return presses, stopping
    // after two consecutive non-reactions. ----------------------------------
    let mut consecutive_fails: usize = 0;
    let mut stalled_at: Option<usize> = None;
    for i in 0..MAX_FURTHER_STEPS {
        let before = current.clone();
        let before_name = format!("{:02}-before-return-{i:02}", 4 + i);
        let before_path = save(&before, &before_name)?;

        input.press_key(VK_RETURN, SC_RETURN)?;
        let after = wait_for_quiescence(&win, floor, SETTLE_TIMEOUT)?;
        let after_name = format!("{:02}-after-return-{i:02}", 4 + i);
        let after_path = save(&after, &after_name)?;

        let r = record_step(
            &mut steps,
            &format!("Return (further step {i:02})"),
            floor,
            &before,
            &after,
            before_path,
            after_path,
        );
        current = after;

        if r {
            consecutive_fails = 0;
            floor = sample_noise_floor(&win, FLOOR_SAMPLES, FLOOR_GAP)?;
        } else {
            consecutive_fails += 1;
            if consecutive_fails >= 2 {
                stalled_at = Some(i);
                notes.push(format!(
                    "Two consecutive Return presses failed to react at further-step {i:02}. Stopping per brief."
                ));
                break;
            }
        }
    }

    let cursor_after = cursor_pos();
    let cursor_unchanged = cursor_before.x == cursor_after.x && cursor_before.y == cursor_after.y;

    let _ = game.kill();

    // --- Report -------------------------------------------------------------
    let mut table = String::new();
    table.push_str("| # | action | floor | delta | threshold | reacted | before | after |\n");
    table.push_str("|---|--------|-------|-------|-----------|---------|--------|-------|\n");
    for (idx, s) in steps.iter().enumerate() {
        table.push_str(&format!(
            "| {idx:02} | {} | {:.4} | {:.4} | {:.4} | {} | {} | {} |\n",
            s.label,
            s.floor,
            s.delta,
            s.threshold,
            if s.reacted { "yes" } else { "no" },
            s.before_bmp,
            s.after_bmp,
        ));
    }

    let progress_desc = if steps.len() >= 2 && steps[1].reacted {
        "lore card -> start menu -> beyond start menu (click worked)"
    } else if !steps.is_empty() && steps[0].reacted {
        "lore card -> start menu (stalled at start menu)"
    } else {
        "stalled at lore card"
    };

    let report = format!(
        "# Task 24 spike: reach overworld\n\n\
         cursor before: ({}, {})\n\
         cursor after:  ({}, {})\n\
         cursor unchanged: {cursor_unchanged}\n\n\
         start-menu Start button resolved pixel: see notes\n\n\
         progress summary: {progress_desc}\n\n\
         stalled at further-step: {:?}\n\n\
         ## Notes\n{}\n\n\
         ## Per-step table\n{table}\n",
        cursor_before.x, cursor_before.y, cursor_after.x, cursor_after.y,
        stalled_at,
        notes.iter().map(|n| format!("- {n}\n")).collect::<String>(),
    );

    std::fs::File::create(format!("{FRAMES_DIR}/report.md"))?.write_all(report.as_bytes())?;
    print!("{report}");

    Ok(())
}
