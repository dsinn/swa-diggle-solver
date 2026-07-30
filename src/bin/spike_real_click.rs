//! Can we drive the overworld by warping the real cursor and injecting a REAL click?
//!
//! ## Why this is worth trying, when "clicking does not work" is a settled finding
//!
//! It is not settled — it is narrower than it sounds. `spike_click_matrix` posted
//! `WM_LBUTTONDOWN` and tried SDL hints. SDL does not take mouse buttons from posted window
//! messages, so that spike proved *posted* clicks fail. **`SendInput` was never tried.** It
//! injects below the message queue, where SDL is actually listening.
//!
//! The reason it was never tried is that it warps the user's real pointer. That was treated as a
//! hard constraint until the user pointed out we already warp it on every arrow press
//! (`input.setHotspotHighlight` -> `love.mouse.setPosition`, `utils/input.lua:94-98`), so the
//! constraint had been abandoned in practice without anyone re-deriving what it unblocked.
//!
//! ## Why it would be a large simplification
//!
//! Selecting a node currently requires panning it under the map hotspot, because `Return`
//! synthesizes `love.mousepressed` and the map hotspot's warp point is the screen centre. Panning
//! is where the travel step is stuck: after a settled pan of -122 px in x, the next measurement
//! showed +188 px back the other way, i.e. the map does not stay where it is put.
//!
//! A real click needs none of that. `core.verboseAdjacencyData` already prints each node's
//! position **in screen pixels** (`overworldview.lua:1033`), so we can aim straight at it.
//!
//! ## What this spike establishes, in order, each with a control
//!
//! 1. **The game sees our cursor at all.** Warp onto the node and look for a hover change
//!    (`mouseHoverOn`/tooltip, `overworldview.lua:13`). If nothing changes, injected motion is not
//!    reaching the game and nothing below is interpretable.
//! 2. **A single injected click selects.** The area-button slot changes only because of the
//!    selection: both frames are taken at the same camera position, so unlike the F1 probe this
//!    comparison is not confounded by panning.
//! 3. **Travel.** Click the Travel button, then wait for the log to say where we ended up.
//!    `travelTo` is asynchronous (`:1394-1400`) and arrival can fire an event (`:1425`), so the
//!    adjacency dump is the only trustworthy report.
//!
//! It also answers a question I could not settle by reading: whether cursor motion clears the
//! hotspot highlight. `main.lua:420` calls `input.setHotspotHighlight()` with no argument on any
//! nonzero-delta move, but that function indexes `hotPointHighlight[5]` *after* assigning the new
//! value (`utils/input.lua:94-106`), which would error on nil — and `hotPointHighlight` is assigned
//! nowhere else. Something has to give; a screenshot on failure will show if it is an error screen.
//!
//! Run: cargo run --release --bin spike_real_click -- config.toml
//!
//! WARNING: this genuinely takes the mouse for a few seconds. Reports to a FILE, because taking a
//! console replaces our standard handles.

use diggle_solver::config::Config;
use diggle_solver::observe::adjacency;
use diggle_solver::observe::log::{Console, LogMirror};
use diggle_solver::win::capture::{capture_window, Region};
use diggle_solver::win::input::{
    inject_left_click, warp_cursor, Input, PostMessageInput, SC_RETURN, VK_RETURN,
};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

const REPORT: &str = "spike-frames-live/real-click-report.md";

/// Reads the log until a dump arrives. Returns the last one seen.
fn collect(
    console: &mut Console,
    mirror: &mut LogMirror,
    reader: &mut adjacency::Reader,
    secs: u64,
) -> Result<Option<adjacency::Adjacency>, Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(400));
        let lines = console.read_new()?;
        if lines.is_empty() {
            continue;
        }
        mirror.write(&lines);
        // Through the stateful Reader, so a dump split across two polls is stitched rather than
        // discarded -- which is what made a successful travel look like a failure.
        if let Some(a) = reader.push(&lines).pop() {
            return Ok(Some(a));
        }
    }
    Ok(None)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg_path = std::env::args().nth(1).unwrap_or_else(|| "config.toml".into());
    std::fs::create_dir_all("spike-frames-live")?;
    let cfg = Config::load(Path::new(&cfg_path))?;
    let mut log = String::from("# Spike: real injected clicks on the overworld\n\n");

    let mut console = Console::take()?;
    let mut reader = adjacency::Reader::new();
    let mut mirror = LogMirror::create(Path::new("spike-frames-live/real-click-log.txt"))?;
    let mut game = diggle_solver::game::launch::GameProcess::launch(&cfg, &console)?;
    let win = game.wait_for_window(Duration::from_secs(20))?;
    std::thread::sleep(Duration::from_secs(3));
    let (cw, ch) = win.client_size()?;
    let (ox, oy) = win.client_origin()?;
    let input = PostMessageInput::new(win);
    input.focus();
    std::thread::sleep(Duration::from_millis(500));
    log.push_str(&format!("pid {} client {cw}x{ch} origin ({ox},{oy})\n\n", game.pid()));

    // ---- reach the overworld (keyboard, the path we know works) ----
    let nav = diggle_solver::win::nav::to_client_point(&win, &input, 187, 810, 60)?;
    if !nav.on_target {
        log.push_str(&format!("ABORT: highlight at {:?}, not Continue\n", nav.landed));
        game.close(Duration::from_secs(15));
        std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
        return Ok(());
    }
    input.press_key(VK_RETURN, SC_RETURN)?;
    let Some(map) = collect(&mut console, &mut mirror, &mut reader, 20)? else {
        log.push_str("ABORT: no adjacency dump after Continue\n");
        game.close(Duration::from_secs(15));
        std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
        return Ok(());
    };
    log.push_str(&format!(
        "at {} — {}\n\n| key | heading | screen x | screen y |\n|---|---|---|---|\n",
        map.here_key, map.here_heading
    ));
    for n in &map.nodes {
        log.push_str(&format!("| {} | {} | {:.1} | {:.1} |\n", n.key, n.heading, n.x, n.y));
    }
    let Some(target) = map.nodes.first().cloned() else {
        log.push_str("\nABORT: no visible neighbour to aim at\n");
        game.close(Duration::from_secs(15));
        std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
        return Ok(());
    };
    // The log's positions are client-space (they come from love.graphics coordinates); the cursor
    // API is screen-space. Conflating the two is a bug this project has already shipped once.
    let (tx, ty) = (target.x.round() as i32, target.y.round() as i32);
    let (sx, sy) = win.client_to_screen(tx, ty)?;
    log.push_str(&format!(
        "\naiming at {} — {} : client ({tx},{ty}) -> screen ({sx},{sy})\n\n",
        target.key, target.heading
    ));

    // ---- CONTROL 1: does the game see our injected cursor at all? ----
    let before_hover = capture_window(&win)?;
    warp_cursor(sx, sy)?;
    std::thread::sleep(Duration::from_millis(700));
    let after_hover = capture_window(&win)?;
    let hover_changed = before_hover.diff_fraction(&after_hover, Region { nx: 0.0, ny: 0.0, nw: 1.0, nh: 1.0 });
    let landed = {
        let mut p = windows::Win32::Foundation::POINT::default();
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut p);
        }
        (p.x, p.y)
    };
    log.push_str(&format!(
        "## Control 1 — the game sees the cursor\n\ncursor landed at screen {landed:?} \
         (wanted ({sx},{sy}))\nframe changed by {hover_changed:.4} after the warp\n\n\
         **{}**\n\n",
        if hover_changed > 0.002 { "CHANGED — motion is reaching the game" }
        else { "NO VISIBLE CHANGE — injected motion may not be reaching the game" }
    ));

    // ---- CONTROL 2: does a real click select the node? ----
    // Same camera position for both frames, so a change in the button slot is caused by the click
    // and nothing else. This is the invariance the F1 probe got wrong.
    let slot = Region::from_px(0, 860, 320, 975, cw, ch);
    let pre = capture_window(&win)?;
    inject_left_click(1)?;
    std::thread::sleep(Duration::from_millis(800));
    let post = capture_window(&win)?;
    let selected = pre.region_hash(slot) != post.region_hash(slot);
    let _ = post.write_bmp(Path::new("spike-frames-live/real-click-after-select.bmp"));
    log.push_str(&format!(
        "## Control 2 — one injected click selects\n\nbutton slot changed: **{selected}**\n\
         (screenshot: `spike-frames-live/real-click-after-select.bmp`)\n\n"
    ));

    // ---- The travel attempt ----
    // Only if the click registered. Clicking Travel with nothing selected is not dangerous, but it
    // would make the result uninterpretable.
    if selected {
        // Double-click the NODE, not a button. `doubleClickEffect` (`overworldview.lua:1446-1468`)
        // falls through to `core.travelTo` for any location that is not the player's own, so this
        // needs no area button and no knowledge of the panel layout — the previous attempt clicked
        // client (187,918) on the assumption the Travel button lives there and nothing happened.
        //
        // `travelTo` has NO reachability guard, which is why this is only ever aimed at a node the
        // log has just reported as an adjacent connection.
        warp_cursor(sx, sy)?;
        std::thread::sleep(Duration::from_millis(300));
        inject_left_click(2)?;
        log.push_str(&format!(
            "double-clicked the node itself at screen ({sx},{sy}) — doubleClickEffect -> travelTo\n\n"
        ));
        match collect(&mut console, &mut mirror, &mut reader, 45)? {
            Some(a) => {
                let arrived = a.here_key == target.key;
                log.push_str(&format!(
                    "## Result\n\n[{}] now at {} — {}\n\n**TRAVEL {}** (wanted {}, got {})\n",
                    a.reason,
                    a.here_key,
                    a.here_heading,
                    if arrived { "SUCCEEDED" } else { "WENT ELSEWHERE" },
                    target.key,
                    a.here_key
                ));
            }
            None => log.push_str(
                "## Result\n\nNo arrival dump within 45 s. Either travel never started, or the \
                 click landed on something inert.\n",
            ),
        }
    } else {
        log.push_str(
            "## Result\n\nStopped before the travel attempt: the click did not register, so a \
             Travel click would prove nothing.\n",
        );
    }

    // ---- capture the arrival screen, to MEASURE the progress button rather than guess it ----
    // Arriving at this crypt plays a cutscene, and the way past it is its "next"/progress button.
    // A bounding box has to be read off a real frame: the F1 classifier failed precisely because I
    // chose a region by eye that included map and sea around the panel, so panning alone changed
    // its hash. Frames are spaced out because a cutscene animates.
    log.push_str("\n## Arrival screen captures (for measuring the progress button)\n\n");
    for i in 1..=5 {
        std::thread::sleep(Duration::from_millis(1200));
        let f = capture_window(&win)?;
        let p = format!("spike-frames-live/arrival-{i}.png");
        if f.write_png(Path::new(&p)).is_ok() {
            log.push_str(&format!("- `{p}` (nonblack {:.3})\n", f.nonblack_fraction()));
        }
    }

    let exited = game.close(Duration::from_secs(15));
    log.push_str(&format!("\ngame exited gracefully: {exited}\n"));
    std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
    console.echo(&format!("done — see {REPORT}\n"));
    Ok(())
}
