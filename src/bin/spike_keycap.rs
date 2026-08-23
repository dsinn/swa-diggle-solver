//! Spike 4: combined keyboard input + screen capture against the live game.
//!
//! Supersedes Spike 2 (PrintWindow capture viability) and Spike 3 (PostMessage
//! input viability) — see task-23-brief.md. Tests the actual operating loop
//! (look, act, look again) in one run: capture works, Return advances the
//! screen, screens are distinguishable by region hash, and the physical cursor
//! is never touched. The frames saved to spike-frames/ become the start of the
//! screen-fingerprint corpus.
//!
//! Carries a REQUIRED positive control: two captures with no input in between
//! must show a small idle baseline delta. If that baseline is large, the
//! capture itself is unstable and no later delta can be trusted, so the spike
//! stops immediately rather than interpreting the keyboard results.
//!
//! Run: cargo run --bin spike_keycap -- config.toml

use diggle_solver::win::capture::{capture_window, Region, START_MENU_REGION};
use diggle_solver::{config::Config, game::launch::PipedGameProcess, win::window};
use std::collections::HashSet;
use std::io::Write;
use std::time::Duration;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, PostMessageW, WM_KEYDOWN, WM_KEYUP};

const FULL: Region = Region { nx: 0.0, ny: 0.0, nw: 1.0, nh: 1.0 };
/// How long to let the screen react (or not) before re-capturing.
const SETTLE: Duration = Duration::from_millis(1400);
/// A reaction must exceed this multiple of the idle baseline delta...
const BASELINE_MULTIPLE: f64 = 3.0;
/// ...and this absolute fraction of changed pixels.
const ABSOLUTE_FLOOR: f64 = 0.02;
/// Positive-control gate: if the idle baseline itself is at or above this, the
/// capture instrument is not trustworthy and no later delta means anything.
const BASELINE_INSTABILITY_GATE: f64 = 0.25;
/// Mean non-black fraction across all captures must exceed this.
const MIN_NONBLACK: f64 = 0.02;
/// Number of Return presses to attempt before giving up.
const MAX_STEPS: usize = 20;
/// Stop early once this many consecutive presses fail to react AND the region
/// hash hasn't moved — further presses would tell us nothing.
const STALL_WINDOW: usize = 4;

const VK_RETURN: usize = 0x0D;
const SC_RETURN: isize = 0x1C;

unsafe fn post_return(hwnd: HWND) {
    let down = LPARAM((SC_RETURN << 16) | 0x0000_0001);
    let up = LPARAM((SC_RETURN << 16) | 0xC000_0001u32 as isize);
    let _ = PostMessageW(hwnd, WM_KEYDOWN, WPARAM(VK_RETURN), down);
    std::thread::sleep(Duration::from_millis(60));
    let _ = PostMessageW(hwnd, WM_KEYUP, WPARAM(VK_RETURN), up);
}

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
    index: usize,
    pre_hash: u64,
    nonblack: f64,
    delta: f64,
    reacted: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).expect("usage: spike_keycap <config.toml>");
    let cfg = Config::load(std::path::Path::new(&path))?;

    reset_sandbox_save()?;

    let mut game = PipedGameProcess::launch(&cfg)?;

    let win = loop {
        if let Some(w) = window::find_by_pid(game.pid()) {
            break w;
        }
        if !game.is_running() {
            return Err("game exited before a window appeared".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    // Let the title/lore sequence settle.
    std::thread::sleep(Duration::from_secs(6));

    let (cw, ch) = win.client_size()?;
    let cursor_before = cursor_pos();

    // --- Required positive control ---------------------------------------
    // Two captures, no input between them. If they differ too much, the
    // capture instrument itself is untrustworthy and every later delta in
    // this run is meaningless. Stop here rather than interpret anything.
    let ctrl_a = capture_window(&win)?;
    std::thread::sleep(SETTLE);
    let ctrl_b = capture_window(&win)?;
    let baseline = ctrl_a.diff_fraction(&ctrl_b, FULL);

    std::fs::create_dir_all("spike-frames")?;
    std::fs::create_dir_all("docs/superpowers/spikes")?;
    ctrl_a.write_bmp(std::path::Path::new("spike-frames/control-a.bmp"))?;
    ctrl_b.write_bmp(std::path::Path::new("spike-frames/control-b.bmp"))?;

    if baseline >= BASELINE_INSTABILITY_GATE {
        let report = format!(
            "# Spike 4 — combined keyboard + capture\n\n\
             VERDICT: FAIL (unstable capture — instrument not trustworthy)\n\n\
             - client area: {cw}x{ch}\n\
             - idle baseline frame delta (no input, {SETTLE:?} apart): {baseline:.4}\n\
             - instability gate: {BASELINE_INSTABILITY_GATE} (baseline >= gate => FAIL)\n\n\
             The positive control failed: two captures taken with no input in between\n\
             differ by {baseline:.4}, at or above the {BASELINE_INSTABILITY_GATE} gate. This\n\
             means the capture mechanism itself is unstable (or the window is animating far\n\
             more than expected at idle), so no keyboard-reaction delta measured afterward\n\
             could be trusted to mean anything. Per the brief, stopping here rather than\n\
             running or interpreting the keyboard loop.\n",
        );
        std::fs::File::create("docs/superpowers/spikes/04-keycap.md")?
            .write_all(report.as_bytes())?;
        print!("{report}");
        let _ = game.kill();
        std::process::exit(1);
    }

    // --- Main loop: capture, post Return, capture, measure ----------------
    let mut steps: Vec<StepRecord> = Vec::new();
    let mut all_hashes: HashSet<u64> = HashSet::new();
    let mut capture_failures = 0usize;
    let mut nonblack_samples: Vec<f64> = Vec::new();
    let mut any_reacted = false;

    let mut pre = capture_window(&win)?;

    for i in 0..MAX_STEPS {
        let pre_hash = pre.region_hash(START_MENU_REGION);
        let nonblack = pre.nonblack_fraction();
        all_hashes.insert(pre_hash);
        nonblack_samples.push(nonblack);

        let bmp_path = format!("spike-frames/step-{i:02}.bmp");
        pre.write_bmp(std::path::Path::new(&bmp_path))?;

        unsafe {
            post_return(win.hwnd);
        }
        std::thread::sleep(SETTLE);

        let post = match capture_window(&win) {
            Ok(f) => f,
            Err(_) => {
                capture_failures += 1;
                steps.push(StepRecord { index: i, pre_hash, nonblack, delta: 1.0, reacted: true });
                break;
            }
        };
        let delta = pre.diff_fraction(&post, FULL);
        let reacted = delta > (baseline * BASELINE_MULTIPLE).max(ABSOLUTE_FLOOR);
        if reacted {
            any_reacted = true;
        }

        steps.push(StepRecord { index: i, pre_hash, nonblack, delta, reacted });

        // Stall detection: last STALL_WINDOW presses all failed to react AND
        // the region hash hasn't moved across that window either.
        if steps.len() >= STALL_WINDOW {
            let tail = &steps[steps.len() - STALL_WINDOW..];
            let all_no_react = tail.iter().all(|s| !s.reacted);
            let hash_unchanged = tail.iter().all(|s| s.pre_hash == tail[0].pre_hash);
            if all_no_react && hash_unchanged {
                pre = post;
                break;
            }
        }

        pre = post;
    }

    // Record the final frame's hash too, since the loop only records pre-press hashes.
    let final_hash = pre.region_hash(START_MENU_REGION);
    all_hashes.insert(final_hash);
    let final_nonblack = pre.nonblack_fraction();
    let final_idx = steps.len();
    pre.write_bmp(std::path::Path::new(&format!("spike-frames/step-{final_idx:02}.bmp")))?;

    let cursor_after = cursor_pos();

    let mean_nonblack = if nonblack_samples.is_empty() {
        0.0
    } else {
        nonblack_samples.iter().sum::<f64>() / nonblack_samples.len() as f64
    };

    // --- Pass criteria, each reported independently ------------------------
    let ok_capture = capture_failures == 0 && mean_nonblack > MIN_NONBLACK;
    let ok_keyboard = any_reacted;
    let ok_distinguishable = all_hashes.len() >= 2;
    let cursor_still = cursor_before.x == cursor_after.x && cursor_before.y == cursor_after.y;
    let ok_cursor = cursor_still;

    // Find the stall point, if any: the first index at which the reaction
    // threshold stopped clearing and never cleared again afterward.
    let stall_step = {
        let mut stall: Option<usize> = None;
        for (idx, s) in steps.iter().enumerate() {
            if !s.reacted && steps[idx..].iter().all(|s2| !s2.reacted) {
                stall = Some(s.index);
                break;
            }
        }
        stall
    };

    let mut table = String::new();
    table.push_str("| step | pre-press hash | non-black frac | post-press delta | reacted |\n");
    table.push_str("|------|----------------|----------------|-------------------|---------|\n");
    for s in &steps {
        table.push_str(&format!(
            "| {:04} | {:016x} | {:.4} | {:.4} | {} |\n",
            s.index,
            s.pre_hash,
            s.nonblack,
            s.delta,
            if s.reacted { "yes" } else { "no" }
        ));
    }
    table.push_str(&format!(
        "| {:04} (final) | {:016x} | {:.4} | (n/a, no press) | (n/a) |\n",
        final_idx, final_hash, final_nonblack
    ));

    let stall_text = match stall_step {
        Some(idx) => format!(
            "Sequence STALLED at step {idx:02}: from that point on, Return stopped producing \
             a reaction. Inspect spike-frames/step-{idx:02}.bmp (before) and \
             spike-frames/step-{:02}.bmp (after) to see the screen where it got stuck.",
            idx + 1
        ),
        None => "No stall detected — every step through the run cleared the reaction threshold, \
                  or the loop hit MAX_STEPS while still reacting."
            .to_string(),
    };

    let report = format!(
        "# Spike 4 — combined keyboard input + screen capture\n\n\
         VERDICT: capture={} keyboard={} distinguishable={} cursor={}\n\n\
         ## Positive control\n\
         - idle baseline frame delta (no input, {SETTLE:?} apart): {baseline:.4} \
           (gate: < {BASELINE_INSTABILITY_GATE}) -> instrument trustworthy\n\n\
         ## Criterion 1: capture works\n\
         - client area: {cw}x{ch}\n\
         - capture failures: {capture_failures}\n\
         - mean non-black fraction across {} samples: {mean_nonblack:.4} (need > {MIN_NONBLACK})\n\n\
         ## Criterion 2: keyboard works\n\
         - reaction threshold per step: max({BASELINE_MULTIPLE} x baseline, {ABSOLUTE_FLOOR})\n\
         - at least one Return press reacted: {any_reacted}\n\n\
         ## Criterion 3: screens are distinguishable\n\
         - distinct START_MENU_REGION hashes seen across the run: {}\n\n\
         ## Criterion 4: cursor untouched\n\
         - cursor before: ({}, {})\n\
         - cursor after:  ({}, {})\n\
         - unmoved: {cursor_still}\n\n\
         ## Stall analysis (the real goal: how far does Return alone get us?)\n\
         {stall_text}\n\n\
         ## Per-step table\n\
         {table}\n\
         Frames are left on disk in spike-frames/ as evidence and as the start of the\n\
         screen-fingerprint corpus.\n",
        if ok_capture { "PASS" } else { "FAIL" },
        if ok_keyboard { "PASS" } else { "FAIL" },
        if ok_distinguishable { "PASS" } else { "FAIL" },
        if ok_cursor { "PASS" } else { "FAIL" },
        nonblack_samples.len(),
        all_hashes.len(),
        cursor_before.x, cursor_before.y,
        cursor_after.x, cursor_after.y,
    );

    std::fs::File::create("docs/superpowers/spikes/04-keycap.md")?.write_all(report.as_bytes())?;
    print!("{report}");

    let _ = game.kill();

    let pass = ok_capture && ok_keyboard && ok_distinguishable && ok_cursor;
    if !pass {
        std::process::exit(1);
    }
    Ok(())
}
