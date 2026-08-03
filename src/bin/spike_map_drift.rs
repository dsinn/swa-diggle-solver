//! Asks whether the overworld map is STILL moving, with no input of any kind.
//!
//! A run died waiting sixty seconds for an arrival that never came, and the frame it left behind
//! showed the map panned hard into a corner. Two explanations fit that picture equally well: a
//! one-shot shove (a click whose release the game read as a drag) or a pan that is still running
//! because nothing ever told it to stop. `overworldview.lua:1263-1269` makes the second possible —
//! the lines that would clear `hotspotX/hotspotY` on release are commented out, so a direction that
//! is set once keeps panning until something sets it back to zero.
//!
//! Three captures a second apart separate them: a shove is over, a pan is not. Read-only on purpose
//! — injecting a click to find out would destroy the state being measured.

use diggle_solver::win::{capture, window};
use std::time::Duration;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Takes the pid on the command line: `.diggle-pid` is written by `diggle launch` alone, so after
    // a `spike_run` that launched its own copy the file names a process that no longer exists.
    let pid: u32 = match std::env::args().nth(1) {
        Some(a) => a.parse()?,
        None => std::fs::read_to_string(".diggle-pid")?.trim().parse()?,
    };
    let win = window::find_by_pid(pid).ok_or("no visible window for that pid")?;
    println!("client {:?}", win.client_size()?);

    let mut prev = capture::capture_window(&win)?;
    for n in 1..=3 {
        std::thread::sleep(Duration::from_millis(1000));
        let now = capture::capture_window(&win)?;
        let moved = prev.diff_fraction(&now, diggle_solver::observe::settle::FULL);
        println!("frame {n}: moved {moved:.4} over the last second");
        prev = now;
    }
    prev.write_png(std::path::Path::new("spike-frames-live/drift-now.png"))?;
    println!("wrote spike-frames-live/drift-now.png");
    Ok(())
}
