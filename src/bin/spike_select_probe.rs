//! Measures what "the click selected a node" actually looks like, against what a miss looks like.
//!
//! `spike_run` decides a node click worked by diffing the area-button strip and asking for more than
//! 1% of it to change. That threshold has never been measured against anything. A run clicked empty
//! ocean, passed the check, pressed Space into an empty selection — `affirmative` acts on
//! `overworldview.getMousePressedOn()` (`overworld.lua:1355-1357`) — and then waited a full minute
//! for an arrival that was never started.
//!
//! The honest objection to simply raising the number is that a miss is not "nothing happens": it
//! *deselects*, which slides the area-button panel out and the arrow in, and that moves plenty of
//! pixels too. So the strip diff may not separate the two states at any threshold, and the useful
//! answer may be that the affirmative slot has to be read instead — a selected node puts `Travel`
//! there, a miss puts nothing.
//!
//! So this measures both readings in both states, back to back, on the same map:
//!
//! - **hit**: click the node whose coordinates the last dump gave.
//! - **miss**: click open water, which is the nearest confusable state — not a blank screen, and not
//!   a state where nothing happened at all.
//!
//! Selecting is inert on its own: travel needs Space or a double click
//! (`overworldview.lua:1446-1468`), and this sends neither.

use diggle_solver::observe::affirm;
use diggle_solver::win::capture::Region;
use diggle_solver::win::input::click_at_in;
use diggle_solver::win::{capture, window};
use std::path::Path;
use std::time::Duration;

const AREA_BUTTONS: Region = Region { nx: 0.0, ny: 0.68, nw: 0.45, nh: 0.18 };
const NEUTRAL: (i32, i32) = (760, 240);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let pid: u32 =
        args.next().ok_or("usage: spike_select_probe <pid> <node_x> <node_y>")?.parse()?;
    let nx: i32 = args.next().ok_or("need node x")?.parse()?;
    let ny: i32 = args.next().ok_or("need node y")?.parse()?;

    let cfg = diggle_solver::config::Config::load(Path::new("config.toml"))?;
    let win = window::find_by_pid(pid).ok_or("no visible window for that pid")?;
    let art = affirm::ButtonArt::load(Path::new(&cfg.game_dir), "right")?;
    let (cw, ch) = win.client_size()?;
    let dir = Path::new("spike-frames-live");

    let park = || {
        if let Ok((x, y)) = win.client_to_screen(NEUTRAL.0, NEUTRAL.1) {
            let _ = diggle_solver::win::input::warp_cursor(x, y);
        }
    };
    let read_affirmative = || -> affirm::Reading {
        let (x, y, w, h) = affirm::ButtonArt::crop_rect(&affirm::LORE_AFFIRMATIVE, cw, ch);
        match capture::capture_client_rect(&win, x, y, w, h) {
            Ok(f) => art.read_cropped(&f, &affirm::LORE_AFFIRMATIVE, (cw, ch), (x, y)),
            Err(_) => affirm::Reading { state: affirm::State::Absent, score: 0.0, margin: 0.0 },
        }
    };

    // Open water, far from any node and clear of the chrome. This is the state a stale coordinate
    // actually lands in, so it is what the threshold has to be told apart from.
    let miss_at = (1500, 300);

    for (what, at) in [("hit ", (nx, ny)), ("miss", miss_at)] {
        let before = capture::capture_window(&win)?;
        let (sx, sy) = win.client_to_screen(at.0, at.1)?;
        click_at_in(&win, sx, sy)?;
        park();
        std::thread::sleep(Duration::from_millis(900));
        let after = capture::capture_window(&win)?;
        let moved = before.diff_fraction(&after, AREA_BUTTONS);
        let r = read_affirmative();
        println!(
            "{what} at ({:4}, {:4}): strip moved {moved:.4} | affirmative {:?} score {:.4} margin {:.4}",
            at.0, at.1, r.state, r.score, r.margin
        );
        after.write_png(&dir.join(format!("select-{}.png", what.trim())))?;
    }
    println!("wrote spike-frames-live/select-*.png");
    Ok(())
}
