//! Replays `spike_run::Run::recentre` by hand, one click at a time, and photographs each step.
//!
//! A run died waiting for an arrival that never came, and the frame it left behind showed the map
//! shoved several hundred pixels off the node coordinates the game had just printed. Two claims were
//! tangled together in that failure and this separates them:
//!
//! 1. **The driver's own bookkeeping is wrong.** `Run::latest` is sticky (`spike_run.rs:189`), and
//!    `recentre` waits for "a dump whose reason contains `pan`" without first consuming the one
//!    already sitting there. That is a certainty from the code, not a theory — the run printed four
//!    `Screen pan finished` lines while `recentre` reported success six times.
//! 2. **Something moved the map with no announcement.** `Screen pan finished` is printed only when an
//!    animated `offsetTransition` completes (`overworldview.lua:1249-1256`); the drag and hotspot
//!    paths write `xoffset` directly (`:1263-1265`) and say nothing. So silence does not mean
//!    stillness, and the log cannot tell us which of them ran.
//!
//! Claim 2 is what this probe is for. It asks the live game the only question that settles it: press
//! the arrow and watch whether the player ends up in the middle of the screen.
//! `showAreaButtonsButton.mousereleased` (`overworldview.lua:485-494`) calls `refreshAreaButtons`
//! *and* `centreScreenOnPlayer`, so a working press has two visible consequences, and photographing
//! both tells us whether the press was ignored, half-applied, or applied and then undone.
//!
//! Deliberately NOT a loop. The failure is being reproduced, not driven past.

use diggle_solver::win::input::click_at_in;
use diggle_solver::win::{capture, window};
use std::path::Path;
use std::time::Duration;

/// The arrow, and the point `recentre` uses to raise it. Copied rather than shared so this probe
/// keeps testing the coordinates the run actually used even if the run's constants move.
const SHOW_AREA_BUTTONS: (i32, i32) = (32, 918);
const EMPTY_MAP: (i32, i32) = (1750, 160);
const NEUTRAL: (i32, i32) = (760, 240);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let pid: u32 = match std::env::args().nth(1) {
        Some(a) => a.parse()?,
        None => std::fs::read_to_string(".diggle-pid")?.trim().parse()?,
    };
    let win = window::find_by_pid(pid).ok_or("no visible window for that pid")?;
    let dir = Path::new("spike-frames-live");
    let shot = |name: &str| -> Result<capture::Frame, Box<dyn std::error::Error>> {
        let f = capture::capture_window(&win)?;
        f.write_png(&dir.join(format!("recentre-{name}.png")))?;
        Ok(f)
    };
    let park = || {
        if let Ok((x, y)) = win.client_to_screen(NEUTRAL.0, NEUTRAL.1) {
            let _ = diggle_solver::win::input::warp_cursor(x, y);
        }
    };

    let before = shot("0-before")?;
    println!("captured the starting frame");

    let (ex, ey) = win.client_to_screen(EMPTY_MAP.0, EMPTY_MAP.1)?;
    click_at_in(&win, ex, ey)?;
    park();
    std::thread::sleep(Duration::from_millis(700));
    let raised = shot("1-arrow-raised")?;
    println!(
        "after clicking empty map at {EMPTY_MAP:?}: moved {:.4}",
        before.diff_fraction(&raised, diggle_solver::observe::settle::FULL)
    );

    let (ax, ay) = win.client_to_screen(SHOW_AREA_BUTTONS.0, SHOW_AREA_BUTTONS.1)?;
    click_at_in(&win, ax, ay)?;
    park();

    // Sampled rather than slept through: `centreScreenOnPlayer` animates, so the interesting part is
    // whether the map moves at all and whether it then stops. One frame two seconds later cannot
    // tell a pan that never started from one that ran and was undone.
    let mut prev = raised.clone();
    for n in 1..=6 {
        std::thread::sleep(Duration::from_millis(400));
        let now = capture::capture_window(&win)?;
        println!(
            "  {}ms after the arrow: moved {:.4} since the last look",
            n * 400,
            prev.diff_fraction(&now, diggle_solver::observe::settle::FULL)
        );
        prev = now;
    }
    prev.write_png(&dir.join("recentre-2-after-arrow.png"))?;
    println!(
        "total movement across the arrow press: {:.4}",
        raised.diff_fraction(&prev, diggle_solver::observe::settle::FULL)
    );
    println!("wrote spike-frames-live/recentre-*.png");
    Ok(())
}
