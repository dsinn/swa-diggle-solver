//! Task 25: SDL hint + focus matrix for posted mouse clicks.
//!
//! Task 24 established a hard asymmetry: posted **keyboard** input works (Return
//! dismisses the lore card, cursor untouched) but posted **mouse clicks do not
//! land at all** on the Start button, whose coordinates were independently
//! verified correct and which the game does not gate on focus in its own hit-test
//! code. The likely cause is SDL declining to deliver mouse *button* events to a
//! window without real focus -- behaviour it does not apply to keyboard.
//!
//! Configurations A-D test two independent, purely-external levers, each in its
//! own fresh game launch, ending in the same measured click on the Start button:
//!
//!   A: neither lever (negative control -- must reproduce Task 24's failure)
//!   B: SDL_MOUSE_FOCUS_CLICKTHROUGH=1 only
//!   C: SetForegroundWindow only
//!   D: both
//!
//! Every A-D configuration also runs the mandatory positive control (Return on
//! the lore card) before its click, so a broken harness can never be confused
//! with a genuine negative result. Because `SetForegroundWindow` is restricted by
//! Windows (a process that doesn't own the foreground window and didn't receive
//! the last input event generally cannot steal it), C and D also record the
//! call's own return value and `GetForegroundWindow()` immediately before the
//! click, so a silent no-op reads as "inconclusive" rather than "negative".
//!
//! Configuration E is keyboard-only (no SDL hint, no SetForegroundWindow) and
//! tests a different mechanism entirely: utils/input.lua's `hotspotSelect`
//! (bound to Return) synthesizes a `love.mousepressed` call in Lua itself once a
//! hotspot highlight exists, and arrow keys are what establish that highlight
//! via `love.mouse.setPosition`. If an arrow key + Return can leave the start
//! menu, no SDL mouse-button delivery is ever required.
//!
//! Run: cargo run --bin spike_click_matrix -- config.toml

use diggle_solver::observe::settle::{self, sample_noise_floor, wait_for_quiescence};
use diggle_solver::win::capture::Frame;
use diggle_solver::win::input::{
    Input, PostMessageInput, SC_DOWN, SC_LEFT, SC_RETURN, SC_RIGHT, SC_UP, VK_DOWN, VK_LEFT,
    VK_RETURN, VK_RIGHT, VK_UP,
};
use diggle_solver::win::window::{self, ButtonSpec, GameWindow};
use diggle_solver::{config::Config, game::launch::GameProcess};
use std::io::Write;
use std::path::Path;
use std::time::Duration;
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

const FRAMES_DIR: &str = "spike-frames-3";
const FLOOR_SAMPLES: usize = 5;
const FLOOR_GAP: Duration = Duration::from_millis(300);
const SETTLE_TIMEOUT: Duration = Duration::from_secs(6);
/// Generous floor used only for the very first settle, which must ride out the
/// publisher splash / lore-card fade-in before any real floor sampling can happen.
const INITIAL_GENEROUS_FLOOR: f64 = 0.5;
const INITIAL_SETTLE_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy)]
struct MatrixConfig {
    name: &'static str,
    clickthrough_hint: bool,
    focus_before_click: bool,
}

const MATRIX: [MatrixConfig; 4] = [
    MatrixConfig { name: "A", clickthrough_hint: false, focus_before_click: false },
    MatrixConfig { name: "B", clickthrough_hint: true, focus_before_click: false },
    MatrixConfig { name: "C", clickthrough_hint: false, focus_before_click: true },
    MatrixConfig { name: "D", clickthrough_hint: true, focus_before_click: true },
];

fn cursor_pos() -> POINT {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    p
}

/// Deletes persistentSaveData from the SANDBOX save dir only
/// (%APPDATA%\LOVE\SternlyWordedAdventures\), forcing the auto-save lore card to
/// appear on next launch. Never touches %APPDATA%\SternlyWordedAdventures\ (no
/// LOVE folder), which is the user's real Steam save.
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

struct ConfigResult {
    name: &'static str,
    positive_control_reacted: bool,
    click_floor: f64,
    click_delta: f64,
    click_threshold: f64,
    click_reacted: bool,
    /// `SetForegroundWindow`'s own return value -- only Some() when this config
    /// actually calls focus(). Windows may refuse the request silently.
    focus_call_succeeded: Option<bool>,
    /// Whether the game window held OS foreground focus immediately before the
    /// click, regardless of whether/how this config tried to obtain it.
    foreground_before_click: bool,
    cursor_before: POINT,
    cursor_after: POINT,
    cursor_moved: bool,
    notes: Vec<String>,
}

fn run_one(cfg: &Config, mc: MatrixConfig) -> Result<ConfigResult, Box<dyn std::error::Error>> {
    let mut notes: Vec<String> = Vec::new();

    reset_sandbox_save()?;

    let env: Vec<(&str, &str)> = if mc.clickthrough_hint {
        vec![("SDL_MOUSE_FOCUS_CLICKTHROUGH", "1")]
    } else {
        vec![]
    };
    notes.push(format!(
        "Config {}: SDL_MOUSE_FOCUS_CLICKTHROUGH={}, focus_before_click={}",
        mc.name, mc.clickthrough_hint, mc.focus_before_click
    ));

    let mut game = GameProcess::launch_with_env(cfg, &env)?;

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

    // --- Ride out the splash/fade-in, then sample the lore card's idle floor. ---
    wait_for_quiescence(&win, INITIAL_GENEROUS_FLOOR, INITIAL_SETTLE_TIMEOUT)?;
    let mut floor = sample_noise_floor(&win, FLOOR_SAMPLES, FLOOR_GAP)?;
    notes.push(format!("Lore-card idle noise floor: {floor:.4}"));

    // --- Mandatory positive control: Return must dismiss the lore card. ---
    let before = wait_for_quiescence(&win, floor, SETTLE_TIMEOUT)?;
    let before_path = save(&before, &format!("{}-positive-control-before", mc.name))?;
    input.press_key(VK_RETURN, SC_RETURN)?;
    let after = wait_for_quiescence(&win, floor, SETTLE_TIMEOUT)?;
    let after_path = save(&after, &format!("{}-positive-control-after", mc.name))?;
    let positive_control_reacted = settle::reacted(&before, &after, floor);
    notes.push(format!(
        "Positive control (Return on lore card): reacted={positive_control_reacted} \
         ({before_path} -> {after_path})"
    ));

    // Re-sample the floor: we should now be on the start menu, whose mascot
    // animates continuously, so this floor will be much higher.
    floor = sample_noise_floor(&win, FLOOR_SAMPLES, FLOOR_GAP)?;
    notes.push(format!("Start-menu idle noise floor re-sampled: {floor:.4}"));

    // Ride out the start menu's one-time title/mascot reveal before clicking.
    let current = wait_for_quiescence(&win, floor, SETTLE_TIMEOUT)?;

    // --- Apply the focus lever, if this configuration calls for it. ---
    let mut focus_call_succeeded: Option<bool> = None;
    if mc.focus_before_click {
        let ok = input.focus();
        focus_call_succeeded = Some(ok);
        notes.push(format!(
            "Called SetForegroundWindow before the click; return value (own success/failure): {ok}."
        ));
        // Give the OS a moment to actually hand over focus.
        std::thread::sleep(Duration::from_millis(200));
    }

    // Verify OS foreground state immediately before the click, regardless of
    // whether/how this config tried to obtain it -- SetForegroundWindow can
    // silently no-op if the calling process doesn't own the foreground window
    // and didn't receive the last input event, so this check is what tells us
    // whether C/D actually tested what they claim.
    let foreground_before_click = input.has_foreground();
    notes.push(format!(
        "GetForegroundWindow() immediately before click matches game window: {foreground_before_click}."
    ));

    // --- Click Start. ---
    let spec = ButtonSpec { ss_x: 0.0, ss_y: 0.75, os_x: 2.0, os_y: 0.0, w: 250.0, h: 100.0 };
    let (bx, by) = win.button_center(&spec)?;
    notes.push(format!("Resolved Start button center: client-area pixel ({bx}, {by})."));

    let before = current;
    let before_path = save(&before, &format!("{}-before", mc.name))?;
    input.click(bx, by)?;
    let after = wait_for_quiescence(&win, floor, SETTLE_TIMEOUT)?;
    let after_path = save(&after, &format!("{}-after", mc.name))?;

    let click_delta = before.diff_fraction(&after, settle::FULL);
    let click_threshold = (floor * settle::REACT_MULTIPLE).max(settle::REACT_FLOOR);
    let click_reacted = settle::reacted(&before, &after, floor);
    notes.push(format!(
        "Click Start at ({bx}, {by}): delta={click_delta:.4} threshold={click_threshold:.4} \
         reacted={click_reacted} ({before_path} -> {after_path})"
    ));

    let cursor_after = cursor_pos();
    let cursor_moved = cursor_before.x != cursor_after.x || cursor_before.y != cursor_after.y;

    let _ = game.kill();
    // Give the OS a moment to actually tear the process down before the next
    // configuration launches a fresh one.
    std::thread::sleep(Duration::from_millis(500));

    Ok(ConfigResult {
        name: mc.name,
        positive_control_reacted,
        click_floor: floor,
        click_delta,
        click_threshold,
        click_reacted,
        focus_call_succeeded,
        foreground_before_click,
        cursor_before,
        cursor_after,
        cursor_moved,
        notes,
    })
}

/// Configuration E: no SDL hint, no SetForegroundWindow, keyboard only. Tests
/// whether an arrow key (to establish a hotspot highlight, which warps the
/// game's own cursor via `love.mouse.setPosition`) followed by Return (which
/// synthesizes `love.mousepressed` in Lua once a highlight exists, per
/// utils/input.lua's `hotspotSelect`/`setHotspotHighlight`) can leave the start
/// menu without SDL ever having to deliver a real mouse button event.
fn run_e(cfg: &Config) -> Result<ConfigResult, Box<dyn std::error::Error>> {
    let mut notes: Vec<String> = Vec::new();
    let name = "E";

    reset_sandbox_save()?;
    notes.push("Config E: keyboard-only (no SDL hint, no SetForegroundWindow).".to_string());

    let mut game = GameProcess::launch_with_env(cfg, &[])?;

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

    // --- Ride out the splash/fade-in, then sample the lore card's idle floor. ---
    wait_for_quiescence(&win, INITIAL_GENEROUS_FLOOR, INITIAL_SETTLE_TIMEOUT)?;
    let mut floor = sample_noise_floor(&win, FLOOR_SAMPLES, FLOOR_GAP)?;
    notes.push(format!("Lore-card idle noise floor: {floor:.4}"));

    // --- Mandatory positive control: Return must dismiss the lore card. ---
    let before = wait_for_quiescence(&win, floor, SETTLE_TIMEOUT)?;
    let before_path = save(&before, &format!("{name}-positive-control-before"))?;
    input.press_key(VK_RETURN, SC_RETURN)?;
    let after = wait_for_quiescence(&win, floor, SETTLE_TIMEOUT)?;
    let after_path = save(&after, &format!("{name}-positive-control-after"))?;
    let positive_control_reacted = settle::reacted(&before, &after, floor);
    notes.push(format!(
        "Positive control (Return on lore card): reacted={positive_control_reacted} \
         ({before_path} -> {after_path})"
    ));

    // Re-sample the floor now that we're on the start menu.
    floor = sample_noise_floor(&win, FLOOR_SAMPLES, FLOOR_GAP)?;
    notes.push(format!("Start-menu idle noise floor re-sampled: {floor:.4}"));

    let mut current = wait_for_quiescence(&win, floor, SETTLE_TIMEOUT)?;

    // Not applicable to E -- no SetForegroundWindow call, no click. Recorded
    // for a consistent table shape; foreground state is still worth knowing.
    let focus_call_succeeded: Option<bool> = None;
    let foreground_before_click = input.has_foreground();
    notes.push(format!(
        "GetForegroundWindow() matches game window (no focus attempt made in E): {foreground_before_click}."
    ));

    // --- Try arrow keys in order until one establishes a hotspot highlight. ---
    let candidates: [(&str, u16, u16); 4] =
        [("Down", VK_DOWN, SC_DOWN), ("Up", VK_UP, SC_UP), ("Left", VK_LEFT, SC_LEFT), ("Right", VK_RIGHT, SC_RIGHT)];

    let mut arrow_that_worked: Option<&str> = None;
    let mut arrow_delta = 0.0f64;
    let mut arrow_threshold = 0.0f64;

    for (label, vk, sc) in candidates {
        let before_arrow = current.clone();
        let before_path = save(&before_arrow, &format!("{name}-arrow-{label}-before"))?;
        input.press_extended_key(vk, sc)?;
        let after_arrow = wait_for_quiescence(&win, floor, SETTLE_TIMEOUT)?;
        let after_path = save(&after_arrow, &format!("{name}-arrow-{label}-after"))?;

        let delta = before_arrow.diff_fraction(&after_arrow, settle::FULL);
        let threshold = (floor * settle::REACT_MULTIPLE).max(settle::REACT_FLOOR);
        let reacted = settle::reacted(&before_arrow, &after_arrow, floor);
        notes.push(format!(
            "Arrow {label}: delta={delta:.4} threshold={threshold:.4} reacted={reacted} \
             ({before_path} -> {after_path})"
        ));
        current = after_arrow;

        if reacted {
            arrow_that_worked = Some(label);
            arrow_delta = delta;
            arrow_threshold = threshold;
            break;
        }
    }

    if let Some(label) = arrow_that_worked {
        notes.push(format!("Arrow {label} produced a visible change (candidate for a hotspot highlight)."));
    } else {
        notes.push(
            "No arrow key (Down, Up, Left, Right) produced a visible change under the \
             current floor. Proceeding to press Return anyway in case a highlight was \
             established without a visible pixel change at this resolution/frame timing."
                .to_string(),
        );
    }

    // --- Press Return; did the game leave the start menu? ---
    let before_return = current.clone();
    let before_path = save(&before_return, &format!("{name}-return-before"))?;
    input.press_key(VK_RETURN, SC_RETURN)?;
    let after_return = wait_for_quiescence(&win, floor, SETTLE_TIMEOUT)?;
    let after_path = save(&after_return, &format!("{name}-return-after"))?;

    let click_delta = before_return.diff_fraction(&after_return, settle::FULL);
    let click_threshold = (floor * settle::REACT_MULTIPLE).max(settle::REACT_FLOOR);
    let click_reacted = settle::reacted(&before_return, &after_return, floor);
    notes.push(format!(
        "Return after arrow ({:?}): delta={click_delta:.4} threshold={click_threshold:.4} \
         reacted={click_reacted} ({before_path} -> {after_path})",
        arrow_that_worked
    ));
    notes.push(format!(
        "(For reference, the arrow step itself measured delta={arrow_delta:.4} threshold={arrow_threshold:.4}.)"
    ));

    let cursor_after = cursor_pos();
    let cursor_moved = cursor_before.x != cursor_after.x || cursor_before.y != cursor_after.y;
    // love.mouse.setPosition warps the REAL pointer -- this is an expected cost
    // of configuration E, caused by the game itself, not a harness bug.
    notes.push(format!(
        "Cursor moved: {cursor_moved} (before=({},{}) after=({},{})). If true, this is \
         EXPECTED for E: love.mouse.setPosition (utils/input.lua) warps the real pointer \
         when a hotspot highlight is set -- not evidence of a problem.",
        cursor_before.x, cursor_before.y, cursor_after.x, cursor_after.y
    ));

    let _ = game.kill();
    std::thread::sleep(Duration::from_millis(500));

    Ok(ConfigResult {
        name,
        positive_control_reacted,
        click_floor: floor,
        click_delta,
        click_threshold,
        click_reacted,
        focus_call_succeeded,
        foreground_before_click,
        cursor_before,
        cursor_after,
        cursor_moved,
        notes,
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).expect("usage: spike_click_matrix <config.toml>");
    let cfg = Config::load(std::path::Path::new(&path))?;

    std::fs::create_dir_all(FRAMES_DIR)?;

    let mut results: Vec<ConfigResult> = Vec::new();
    let mut fatal: Option<String> = None;

    for mc in MATRIX {
        match run_one(&cfg, mc) {
            Ok(r) => {
                // Negative-control gate: if A unexpectedly succeeds, everything
                // after it is suspect. Stop immediately rather than continuing.
                if mc.name == "A" && r.click_reacted {
                    results.push(r);
                    fatal = Some(
                        "Negative control A (neither lever) UNEXPECTEDLY SUCCEEDED. \
                         Task 24's failure did not reproduce, so B/C/D results would be \
                         uninterpretable. Stopping per the brief rather than continuing."
                            .to_string(),
                    );
                    break;
                }
                results.push(r);
            }
            Err(e) => {
                fatal = Some(format!("Configuration {} errored: {e}", mc.name));
                break;
            }
        }
    }

    // Only run E if A-D didn't hit a fatal condition -- E is a distinct
    // mechanism and its harness soundness is judged by its own positive
    // control, but there's no reason to spend a launch on it if the earlier
    // controls already invalidated the run.
    if fatal.is_none() {
        match run_e(&cfg) {
            Ok(r) => results.push(r),
            Err(e) => fatal = Some(format!("Configuration E errored: {e}")),
        }
    }

    // --- Report ---
    let mut table = String::new();
    table.push_str(
        "| config | positive control | floor | delta | threshold | reacted | focus() ok | foreground before click | cursor moved |\n",
    );
    table.push_str("|---|---|---|---|---|---|---|---|---|\n");
    for r in &results {
        table.push_str(&format!(
            "| {} | {} | {:.4} | {:.4} | {:.4} | {} | {} | {} | {} |\n",
            r.name,
            if r.positive_control_reacted { "yes" } else { "NO" },
            r.click_floor,
            r.click_delta,
            r.click_threshold,
            if r.click_reacted { "yes" } else { "no" },
            match r.focus_call_succeeded {
                Some(true) => "true",
                Some(false) => "FALSE",
                None => "n/a",
            },
            if r.foreground_before_click { "yes" } else { "no" },
            if r.cursor_moved { "yes" } else { "no" },
        ));
    }

    let mut notes_all = String::new();
    for r in &results {
        notes_all.push_str(&format!(
            "\n#### Config {}\ncursor before: ({}, {})\ncursor after: ({}, {})\n",
            r.name, r.cursor_before.x, r.cursor_before.y, r.cursor_after.x, r.cursor_after.y
        ));
        for n in &r.notes {
            notes_all.push_str(&format!("- {n}\n"));
        }
    }

    let winner = results
        .iter()
        .filter(|r| r.name != "A")
        .find(|r| r.click_reacted && r.positive_control_reacted)
        .map(|r| r.name);

    let report = format!(
        "# Task 25 spike: click matrix (A-D) + hotspot-keyboard mechanism (E)\n\n\
         fatal: {:?}\n\n\
         winning configuration (first of B/C/D/E with positive control ok AND reacted): {:?}\n\n\
         ## Matrix\n{table}\n\
         ## Per-config notes\n{notes_all}\n",
        fatal, winner,
    );

    std::fs::File::create(format!("{FRAMES_DIR}/report.md"))?.write_all(report.as_bytes())?;
    print!("{report}");

    if let Some(f) = fatal {
        eprintln!("FATAL: {f}");
    }

    Ok(())
}
