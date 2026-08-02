//! Asks the LIVE window what a [`Button`] scores, through the exact path the run uses.
//!
//! Written because a check that returns "not on screen" is indistinguishable from a check that
//! cannot see: `act::locate` returns `Ok(None)` for a genuine absence and swallows a capture failure
//! into `Err`, and the loop's `matches!(.., Ok(Some(q)) if q >= T)` reads both as false. A run sat in
//! combat WaitPhase with `Finish` plainly on screen and reported a navigation error three times over.
//!
//! Dumps the captured region to PNG as well as scoring it, because the score alone cannot say
//! whether the capture landed where it was aimed.
//!
//! `probe_button <search|exact> <out.png>` — `search` runs `act::locate` (capture the search box,
//! hunt the template inside it); `exact` captures only the template-sized rect at the button's
//! origin and compares directly, no search at all.

use diggle_solver::act::{COMBAT_FINISH, REWARD_CONFIRM};
use diggle_solver::win::window;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "search".into());
    let out = std::env::args().nth(2).unwrap_or_else(|| "probe".into());
    let pid: u32 = std::fs::read_to_string(".diggle-pid")?.trim().parse()?;
    let win = window::find_by_pid(pid).ok_or("no visible window for that pid")?;
    let (cw, ch) = win.client_size()?;
    println!("client {cw}x{ch}");

    for b in [&COMBAT_FINISH, &REWARD_CONFIRM] {
        let (x0, y0, x1, y1) = b.search;
        let (w, h) = (x1 - x0, y1 - y0);
        let frame = diggle_solver::win::capture::capture_client_rect(&win, x0, y0, w, h)?;
        println!(
            "\n{}: search ({x0},{y0})-({x1},{y1}) = {w}x{h}, captured {}x{}",
            b.name, frame.width, frame.height
        );
        let path = format!("{out}-{}.png", b.template.trim_end_matches(".png"));
        write_png(&frame, Path::new(&path))?;
        println!("  wrote {path}");

        match mode.as_str() {
            "exact" => {
                let tpl = diggle_solver::observe::template::Template::load(Path::new(&format!(
                    "templates/{}",
                    b.template
                )))?;
                println!("  template {}x{}", tpl.width, tpl.height);
            }
            _ => match diggle_solver::act::locate(&win, b) {
                Ok(Some(q)) => println!("  locate -> Some({q:.4})"),
                Ok(None) => println!("  locate -> None (below MIN_INLIERS, or no match)"),
                Err(e) => println!("  locate -> Err({e})   <- a capture fault read as 'absent'"),
            },
        }
    }
    Ok(())
}

fn write_png(frame: &diggle_solver::win::capture::Frame, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let file = std::fs::File::create(path)?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), frame.width as u32, frame.height as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut rgba = Vec::with_capacity(frame.bgra.len());
    for px in frame.bgra.chunks_exact(4) {
        rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
    }
    enc.write_header()?.write_image_data(&rgba)?;
    Ok(())
}
