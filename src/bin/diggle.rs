//! `diggle` — interactive dev driver.
//!
//! One-shot commands against a game that STAYS RUNNING between invocations. This exists
//! because the old cycle (edit, build, launch, navigate back to the screen, observe, game
//! dies) cost 1-2 minutes per question and lost all state each time.
//!
//! State lives in the game itself; we just record its pid in `.diggle-pid`.
//!
//!   diggle launch            spawn the game detached and wait for its window
//!   diggle kill              terminate it
//!   diggle where             cursor, client size, fingerprint hashes
//!   diggle shot <name>       capture -> spike-frames-live/<name>.{bmp,png}
//!   diggle key <name>        return|space|backspace|up|down|left|right
//!   diggle type <text>       letters via WM_CHAR (combat tile selection)
//!   diggle nav <x> <y>       arrow-navigate toward a point, verified by GetCursorPos
//!   diggle watch <secs>      frame-delta trace (the instrument from design v2 §7)
//!   diggle travel [key]      the travel step: read the map from the log, pan the target under
//!                            the map hotspot, select it, activate Travel, confirm arrival.
//!                            Defaults to a shrine, else the lowest combat level.
//!   diggle overworld <secs>  OWNS a whole session: launches with the log channel open,
//!                            reaches the overworld, and prints every adjacency dump it sees.
//!                            Cannot be one-shot — LÖVE attaches to its PARENT's console, so
//!                            the log reader must be the process that launched the game.

use diggle_solver::config::Config;
use diggle_solver::win::capture::{capture_window, Frame, START_MENU_REGION};
use diggle_solver::win::input::{
    Input, PostMessageInput, SC_DOWN, SC_LEFT, SC_RETURN, SC_RIGHT, SC_SPACE, SC_UP, VK_DOWN,
    VK_LEFT, VK_RETURN, VK_RIGHT, VK_SPACE, VK_UP,
};
use diggle_solver::observe::template::Template;
use diggle_solver::win::window::{self, GameWindow};
use std::path::{Path, PathBuf};
use std::time::Duration;
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

const PID_FILE: &str = ".diggle-pid";
const FRAME_DIR: &str = "spike-frames-live";
const VK_BACK: u16 = 0x08;
const SC_BACK: u16 = 0x0E;
const FULL: diggle_solver::win::capture::Region = diggle_solver::win::capture::Region {
    nx: 0.0, ny: 0.0, nw: 1.0, nh: 1.0,
};

fn cursor() -> (i32, i32) {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    (p.x, p.y)
}

/// Prints to the stdout we had before taking a console. Once `Console::take` runs, `println!`
/// goes nowhere — allocating a console replaces this process's standard handles.
fn echo(console: &diggle_solver::observe::log::Console, s: String) {
    console.echo(&format!("{s}\n"));
}

fn dist(a: (i32, i32), b: (i32, i32)) -> f64 {
    (((a.0 - b.0) as f64).powi(2) + ((a.1 - b.1) as f64).powi(2)).sqrt()
}

/// Attaches to the already-running game recorded in `.diggle-pid`.
fn attach() -> Result<(u32, GameWindow), Box<dyn std::error::Error>> {
    let pid: u32 = std::fs::read_to_string(PID_FILE)
        .map_err(|_| "no .diggle-pid — run `diggle launch` first")?
        .trim()
        .parse()?;
    let win = window::find_by_pid(pid)
        .ok_or("recorded pid has no visible window — the game may have exited; run `diggle launch`")?;
    Ok((pid, win))
}

/// Writes a PNG next to the BMP so the frame can be viewed directly.
fn write_png(frame: &Frame, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let file = std::fs::File::create(path)?;
    let mut enc = png::Encoder::new(
        std::io::BufWriter::new(file),
        frame.width as u32,
        frame.height as u32,
    );
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header()?;
    // Frame is top-down BGRA; PNG wants RGBA.
    let mut rgba = Vec::with_capacity(frame.bgra.len());
    for px in frame.bgra.chunks_exact(4) {
        rgba.extend_from_slice(&[px[2], px[1], px[0], 255]);
    }
    writer.write_image_data(&rgba)?;
    Ok(())
}

/// Crops a square of the live frame into a template, for tracking where the map content goes.
///
/// Frame-to-frame matching is what makes the map layer tractable at all: the time-of-day tint and
/// draw scale cancel out because both frames come from the same session. Matching against the
/// game's own sprite files does not work (design v2 §6.3).
fn crop_patch(frame: &Frame, x: i32, y: i32, size: u32) -> Template {
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for dy in 0..size as i32 {
        for dx in 0..size as i32 {
            let i = (((y + dy) * frame.width + (x + dx)) * 4) as usize;
            rgba.extend_from_slice(&[frame.bgra[i + 2], frame.bgra[i + 1], frame.bgra[i], 255]);
        }
    }
    Template { name: "patch".into(), width: size, height: size, rgba }
}

/// How far the map content has moved since `tpl` was cropped from `(x, y)`, with the match
/// quality that produced the answer.
///
/// `None` means the patch could not be found — it panned off-screen, or the screen changed to
/// something else. The caller must NOT substitute `(0, 0)`: "did not move" and "could not be
/// measured" are different facts, and collapsing them turns a broken instrument into an
/// apparently successful no-op.
/// Matches below this are rejected outright. A genuine frame-to-frame patch match scores ~1.000;
/// the readings that produced nonsense in the first travel attempt scored 0.718 and 0.601 and
/// reported a bogus constant `-192`. The metric was there all along — not checking it is what
/// turned a self-diagnosing instrument into a silent liar.
const MIN_INLIERS: f64 = 0.95;
/// Search window around the expected displacement, in pixels. Bounding the sweep is not only a
/// speed fix (a full-frame `step=1` search of a 96x96 patch is ~1.6e10 operations and blew a
/// 5-minute timeout) — it also removes distant periodic false matches from contention.
const TRACK_RADIUS: i32 = 130;

fn track_patch(
    win: &GameWindow,
    tpl: &Template,
    x: i32,
    y: i32,
    expect: (i32, i32),
) -> Option<((i32, i32), f64, f64)> {
    let after = capture_window(win).ok()?;
    let (ex, ey) = (x + expect.0, y + expect.1);
    let bounds = (
        ex - TRACK_RADIUS,
        ey - TRACK_RADIUS,
        ex + TRACK_RADIUS,
        ey + TRACK_RADIUS,
    );
    let m = diggle_solver::observe::template::find_at_scale_in(&after, tpl, 1.0, 1, Some(bounds))?;
    if m.inliers < MIN_INLIERS {
        return None;
    }
    Some(((m.x - x, m.y - y), m.inliers, m.error))
}

const PATCH: u32 = 96;

/// Mean per-channel variance of a patch — a cheap proxy for how distinctive it is.
fn patch_variance(tpl: &Template) -> f64 {
    let n = (tpl.rgba.len() / 4) as f64;
    let mut sum = [0f64; 3];
    for px in tpl.rgba.chunks_exact(4) {
        for c in 0..3 {
            sum[c] += px[c] as f64;
        }
    }
    let mean: Vec<f64> = sum.iter().map(|s| s / n).collect();
    let mut var = 0f64;
    for px in tpl.rgba.chunks_exact(4) {
        for c in 0..3 {
            var += (px[c] as f64 - mean[c]).powi(2);
        }
    }
    var / (n * 3.0)
}

/// Coarse-then-fine search, so a wide window is affordable.
///
/// A full `step=1` sweep of a 96x96 patch over ±200 px is ~1.5e9 operations per candidate. Step 4
/// first to localise, then `step=1` in a ±6 window to get the exact offset. Pixel art demands
/// `step=1` for the FINAL answer (design v2 §6.3), not for finding the neighbourhood.
fn find_coarse_then_fine(
    frame: &Frame,
    tpl: &Template,
    x: i32,
    y: i32,
    radius: i32,
) -> Option<diggle_solver::observe::template::Match> {
    let coarse = diggle_solver::observe::template::find_at_scale_in(
        frame,
        tpl,
        1.0,
        4,
        Some((x - radius, y - radius, x + radius, y + radius)),
    )?;
    diggle_solver::observe::template::find_at_scale_in(
        frame,
        tpl,
        1.0,
        1,
        Some((coarse.x - 6, coarse.y - 6, coarse.x + 6, coarse.y + 6)),
    )
}

/// Waits until the map stops moving, and returns the settled frame.
///
/// The pan does not stop when the key is released — it coasts. Measuring on a fixed 400 ms delay
/// therefore captured residual motion, and worse, the NEXT command started from a moving map. The
/// symptom was a `Down` calibration reporting 192 px of horizontal movement left over from the
/// preceding `Right` hold, which inverted the vertical sign and made the pan loop diverge.
///
/// The threshold sits between the two regimes: ambient shimmer and cloud drift move a small
/// fraction of the frame, a pan moves nearly all of it.
fn wait_map_still(win: &GameWindow, timeout: Duration) -> Result<Frame, Box<dyn std::error::Error>> {
    const STILL: f64 = 0.03;
    let deadline = std::time::Instant::now() + timeout;
    let mut prev = capture_window(win)?;
    loop {
        std::thread::sleep(Duration::from_millis(180));
        let next = capture_window(win)?;
        if prev.diff_fraction(&next, FULL) <= STILL || std::time::Instant::now() >= deadline {
            return Ok(next);
        }
        prev = next;
    }
}

/// Calibrates a pan axis and chooses the tracking patch in ONE operation, by keeping whichever
/// candidate actually MOVED.
///
/// This replaces a chooser that demanded a patch be stationary between two untouched frames. That
/// control is backwards: a screen-fixed UI overlay passes it perfectly, while real map content
/// (animated water, drifting cloud) can fail it. It duly selected a patch at (720,378) — inside the
/// rectangle `drawHotspotHighlight` paints for the map hotspot (`utils/input.lua:113-127`, roughly
/// `rect(710, 8, 500, 532)`) — scoring inliers 1.000 with zero movement, and three runs reported
/// "the map did not pan" while the map panned fine.
///
/// The property that matters is **responsiveness**: hold the key, then take the candidate whose
/// displacement along the axis is largest and confidently matched. Returns the patch (re-cropped
/// from the post-pan frame), where it now sits, the measured displacement, and the match quality.
fn calibrate_and_pick(
    win: &GameWindow,
    input: &PostMessageInput,
    cw: i32,
    ch: i32,
    vk: u16,
    sc: u16,
    ms: u64,
) -> Result<(Template, i32, i32, (i32, i32), f64), Box<dyn std::error::Error>> {
    // Start from rest, or the "before" frame is already mid-pan.
    let a = wait_map_still(win, Duration::from_secs(3))?;
    let mut candidates: Vec<(f64, i32, i32)> = Vec::new();
    for gy in 0..5 {
        for gx in 0..6 {
            let x = cw / 8 + gx * cw / 8;
            let y = ch / 10 + gy * ch / 8;
            if x + PATCH as i32 >= cw || y + PATCH as i32 >= ch {
                continue;
            }
            candidates.push((patch_variance(&crop_patch(&a, x, y, PATCH)), x, y));
        }
    }
    candidates.sort_by(|l, r| r.0.partial_cmp(&l.0).unwrap());
    candidates.truncate(10);

    input.hold_extended_key(vk, sc, Duration::from_millis(ms))?;
    let b = wait_map_still(win, Duration::from_secs(3))?;

    let mut best: Option<(i32, Template, i32, i32, (i32, i32), f64)> = None;
    for (_, x, y) in &candidates {
        let tpl = crop_patch(&a, *x, *y, PATCH);
        let Some(m) = find_coarse_then_fine(&b, &tpl, *x, *y, 220) else { continue };
        if m.inliers < MIN_INLIERS {
            continue;
        }
        let d = (m.x - *x, m.y - *y);
        let magnitude = d.0.abs().max(d.1.abs());
        if best.as_ref().map(|(bm, ..)| magnitude > *bm).unwrap_or(true) {
            best = Some((magnitude, crop_patch(&b, m.x, m.y, PATCH), m.x, m.y, d, m.inliers));
        }
    }
    let Some((_, tpl, nx, ny, d, inl)) = best else {
        return Err("no candidate patch could be confidently re-found after the pan — the tracker \
                    cannot measure this screen"
            .into());
    };
    Ok((tpl, nx, ny, d, inl))
}

fn measure_pan(
    win: &GameWindow,
    input: &PostMessageInput,
    dir: &str,
    ms: u64,
) -> Result<(i32, i32), Box<dyn std::error::Error>> {
    let (vk, sc) = match dir {
        "up" => (VK_UP, SC_UP),
        "down" => (VK_DOWN, SC_DOWN),
        "left" => (VK_LEFT, SC_LEFT),
        _ => (VK_RIGHT, SC_RIGHT),
    };
    let (cw, ch) = win.client_size()?;
    const PATCH: u32 = 48;
    let (px, py) = (cw / 2 + 120, ch / 2 - 160);
    let before = capture_window(win)?;
    let mut rgba = Vec::with_capacity((PATCH * PATCH * 4) as usize);
    for dy in 0..PATCH as i32 {
        for dx in 0..PATCH as i32 {
            let i = (((py + dy) * before.width + (px + dx)) * 4) as usize;
            rgba.extend_from_slice(&[before.bgra[i + 2], before.bgra[i + 1], before.bgra[i], 255]);
        }
    }
    let tpl = Template { name: "cal".into(), width: PATCH, height: PATCH, rgba };
    input.hold_extended_key(vk, sc, Duration::from_millis(ms))?;
    std::thread::sleep(Duration::from_millis(400));
    let after = capture_window(win)?;
    let m = diggle_solver::observe::template::find_at_scale_in(&after, &tpl, 1.0, 1, None)
        .ok_or("calibration patch not found after pan")?;
    Ok((m.x - px, m.y - py))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("help");

    match cmd {
        "launch" => {
            let cfg = Config::load(Path::new("config.toml"))?;
            // Dropping the Child without waiting leaves the game running after we exit.
            // stderr/stdout are nulled: we never read the log (it is block-buffered and
            // unusable live — design v2 §5), and an unread pipe could eventually block.
            let child = GameProcessDetached::spawn(&cfg)?;
            std::fs::write(PID_FILE, child.to_string())?;
            print!("launched pid {child}; waiting for window");
            let mut win = None;
            for _ in 0..60 {
                if let Some(w) = window::find_by_pid(child) {
                    win = Some(w);
                    break;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            match win {
                Some(w) => {
                    let (cw, ch) = w.client_size()?;
                    println!(" -> client {cw}x{ch}");
                    // The window exists before the game renders, so an immediate
                    // capture is solid black. Wait for real pixels, otherwise the very
                    // first `shot`/`where` silently reports a black frame and a
                    // meaningless fingerprint.
                    print!("waiting for first rendered frame");
                    for _ in 0..80 {
                        if let Ok(f) = capture_window(&w) {
                            if f.nonblack_fraction() > 0.02 {
                                println!(" -> nonblack={:.4}", f.nonblack_fraction());
                                return Ok(());
                            }
                        }
                        std::thread::sleep(Duration::from_millis(250));
                    }
                    println!(" -> STILL BLACK after 20s");
                }
                None => println!(" -> NO WINDOW APPEARED"),
            }
        }
        "kill" => {
            let pid: u32 = std::fs::read_to_string(PID_FILE)?.trim().parse()?;
            std::process::Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/F"])
                .status()?;
            let _ = std::fs::remove_file(PID_FILE);
            println!("killed {pid}");
        }
        "where" => {
            let (pid, win) = attach()?;
            let (cw, ch) = win.client_size()?;
            let f = capture_window(&win)?;
            println!("pid={pid} client={cw}x{ch}");
            let (ox, oy) = win.client_origin()?;
            println!("client_origin={:?} (screen coords of client 0,0)", (ox, oy));
            let c = cursor();
            println!("cursor_screen={:?} cursor_client={:?}", c, (c.0 - ox, c.1 - oy));
            println!("fullframe={:016x} nonblack={:.4}", f.region_hash(FULL), f.nonblack_fraction());
            // START_MENU_REGION is meaningful ONLY on the start menu; elsewhere it hashes
            // whatever happens to sit there (empty background on hero select). Use
            // `diggle hash <x0> <y0> <x1> <y1>` to measure a region for THIS screen.
            println!("start_menu_region={:016x}  <- only meaningful on the start menu",
                     f.region_hash(START_MENU_REGION));
        }
        "shot" => {
            let name = args.get(1).map(|s| s.as_str()).unwrap_or("shot");
            let (_, win) = attach()?;
            std::fs::create_dir_all(FRAME_DIR)?;
            let f = capture_window(&win)?;
            let bmp = PathBuf::from(FRAME_DIR).join(format!("{name}.bmp"));
            let png = PathBuf::from(FRAME_DIR).join(format!("{name}.png"));
            f.write_bmp(&bmp)?;
            write_png(&f, &png)?;
            println!("wrote {} and {}", bmp.display(), png.display());
            println!("fullframe={:016x}", f.region_hash(FULL));
        }
        "hash" => {
            // Choose a screen's fingerprint region BY MEASUREMENT. Reports the hash plus
            // an idle-stability check, because a region overlapping animation is exactly
            // how the first FINGERPRINT_REGION went wrong (idle noise floor 0.5228).
            let n: Vec<i32> = args[1..5.min(args.len())]
                .iter().filter_map(|a| a.parse().ok()).collect();
            if n.len() != 4 {
                return Err("usage: hash <x0> <y0> <x1> <y1>  (client pixels)".into());
            }
            let (_, win) = attach()?;
            let (cw, ch) = win.client_size()?;
            let r = diggle_solver::win::capture::Region::from_px(n[0], n[1], n[2], n[3], cw, ch);
            let a = capture_window(&win)?;
            std::thread::sleep(Duration::from_millis(700));
            let b = capture_window(&win)?;
            std::thread::sleep(Duration::from_millis(700));
            let c = capture_window(&win)?;
            let h = a.region_hash(r);
            let stable = h == b.region_hash(r) && h == c.region_hash(r);
            println!("region px=({},{})-({},{})  normalized={:.4},{:.4},{:.4},{:.4}",
                     n[0], n[1], n[2], n[3], r.nx, r.ny, r.nw, r.nh);
            println!("hash={h:016x}");
            println!("idle noise floor = {:.4} (want 0.0000)", a.diff_fraction(&c, r));
            println!("stable across 3 samples = {stable}");
        }
        "key" => {
            let name = args.get(1).map(|s| s.as_str()).unwrap_or("");
            let (_, win) = attach()?;
            let input = PostMessageInput::new(win);
            // Arrows are extended keys (lParam bit 24); the rest are not.
            match name {
                "return" | "enter" => input.press_key(VK_RETURN, SC_RETURN)?,
                "space" => input.press_key(VK_SPACE, SC_SPACE)?,
                "backspace" | "back" => input.press_key(VK_BACK, SC_BACK)?,
                "up" => input.press_extended_key(VK_UP, SC_UP)?,
                "down" => input.press_extended_key(VK_DOWN, SC_DOWN)?,
                "left" => input.press_extended_key(VK_LEFT, SC_LEFT)?,
                "right" => input.press_extended_key(VK_RIGHT, SC_RIGHT)?,
                other => return Err(format!("unknown key {other:?}").into()),
            }
            println!("sent {name}; cursor={:?}", cursor());
        }
        "type" => {
            let text = args.get(1).cloned().unwrap_or_default();
            let (_, win) = attach()?;
            PostMessageInput::new(win).type_text(&text)?;
            println!("typed {text:?}");
        }
        "nav" => {
            let tx: i32 = args.get(1).ok_or("usage: nav <x> <y>")?.parse()?;
            let ty: i32 = args.get(2).ok_or("usage: nav <x> <y>")?.parse()?;
            let (_, win) = attach()?;
            let input = PostMessageInput::new(win);
            // Targets are given in CLIENT pixels (that is what the game source yields),
            // but GetCursorPos reports SCREEN pixels. Convert, or every comparison below
            // is off by the window's position on the desktop.
            let target = win.client_to_screen(tx, ty)?;
            println!("target client=({tx},{ty}) -> screen={target:?}");
            // One press establishes a highlight; without one, Return is a no-op.
            input.press_extended_key(VK_DOWN, SC_DOWN)?;
            std::thread::sleep(Duration::from_millis(300));
            println!("  Down  -> {:?}", cursor());
            for i in 0..12 {
                let c = cursor();
                if dist(c, target) <= 60.0 {
                    break;
                }
                let (dx, dy) = (target.0 - c.0, target.1 - c.1);
                let (vk, sc, n) = if dx.abs() >= dy.abs() {
                    if dx > 0 { (VK_RIGHT, SC_RIGHT, "Right") } else { (VK_LEFT, SC_LEFT, "Left") }
                } else if dy > 0 {
                    (VK_DOWN, SC_DOWN, "Down")
                } else {
                    (VK_UP, SC_UP, "Up")
                };
                input.press_extended_key(vk, sc)?;
                std::thread::sleep(Duration::from_millis(300));
                println!("  {i:2} {n:5} -> {:?}", cursor());
            }
            let landed = cursor();
            println!("landed={landed:?} target={target:?} on_target={}", dist(landed, target) <= 60.0);
        }
        "find" => {
            // Locate a game sprite in the current frame. Reports the whole scale sweep,
            // not just the winner: the gap between best and runner-up is what says whether
            // the match is DISTINCTIVE rather than merely lowest-scoring.
            let png_path = args.get(1).ok_or("usage: find <sprite.png> [step]")?;
            let step: i32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(2);
            let (_, win) = attach()?;
            let f = capture_window(&win)?;
            let tpl = Template::load(Path::new(png_path))?;
            println!(
                "template {} {}x{} opaque={:.3}",
                tpl.name,
                tpl.width,
                tpl.height,
                tpl.opaque_fraction(diggle_solver::observe::template::ALPHA_MIN)
            );
            // Optional bounds let us run a FAIR step=1 test in a region where the sprite is
            // known to be, instead of a coarse full-frame sweep that cannot separate
            // "rendering differs" from "never tested the right offset".
            let bounds = if args.len() >= 7 {
                let n: Vec<i32> = args[3..7].iter().filter_map(|a| a.parse().ok()).collect();
                if n.len() == 4 { Some((n[0], n[1], n[2], n[3])) } else { None }
            } else {
                None
            };
            let scales: Vec<f64> = if bounds.is_some() {
                // Fine sweep: 0.30 .. 2.00 in 0.05 steps.
                (6..=40).map(|i| i as f64 * 0.05).collect()
            } else {
                (2..=12).map(|i| i as f64 * 0.25).collect()
            };
            let t0 = std::time::Instant::now();
            let results =
                diggle_solver::observe::template::sweep_in(&f, &tpl, &scales, step, bounds);
            println!(
                "swept {} scales, step={step}, bounds={bounds:?}, in {:?}\n",
                scales.len(),
                t0.elapsed()
            );
            println!("  scale  inliers   error   top-left      centre");
            for m in results.iter().take(6) {
                println!(
                    "  {:5.2}   {:.3}   {:.4}   ({:4},{:4})   ({:4},{:4})",
                    m.scale, m.inliers, m.error, m.x, m.y, m.cx, m.cy
                );
            }
            if let (Some(a), Some(b)) = (results.first(), results.get(1)) {
                println!(
                    "\nbest_inliers={:.3} runner_up={:.3} margin={:.3}",
                    a.inliers, b.inliers, a.inliers - b.inliers
                );
            }
        }
        "selftest" => {
            // Positive control on the INSTRUMENT. Crops a patch out of the live frame and
            // searches for it. Must return inliers=1.000 at exactly (x,y), scale 1.00.
            // If this fails, the matcher is broken; if it passes, any failure to find a
            // real sprite is about how the game RENDERS it, not about the search.
            let x: i32 = args.get(1).ok_or("usage: selftest <x> <y> <size>")?.parse()?;
            let y: i32 = args.get(2).ok_or("usage: selftest <x> <y> <size>")?.parse()?;
            let size: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(32);
            let (_, win) = attach()?;
            let f = capture_window(&win)?;
            let mut rgba = Vec::with_capacity((size * size * 4) as usize);
            for dy in 0..size as i32 {
                for dx in 0..size as i32 {
                    let i = (((y + dy) * f.width + (x + dx)) * 4) as usize;
                    rgba.extend_from_slice(&[f.bgra[i + 2], f.bgra[i + 1], f.bgra[i], 255]);
                }
            }
            let tpl = Template { name: "crop".into(), width: size, height: size, rgba };
            let m = diggle_solver::observe::template::find_at_scale(&f, &tpl, 1.0, 1)
                .ok_or("no match at all — matcher is broken")?;
            println!("cropped {size}x{size} at ({x},{y})");
            println!(
                "best: ({:4},{:4}) scale={:.2} inliers={:.3} error={:.4}",
                m.x, m.y, m.scale, m.inliers, m.error
            );
            let exact = m.x == x && m.y == y && m.inliers > 0.999;
            println!("SELFTEST {}", if exact { "PASS" } else { "FAIL" });
        }
        "probe" => {
            // §8c fallback F1: find selectable overworld nodes WITHOUT recognising anything.
            //
            // The map is a queryable surface. `Return` at the map hotspot fires
            // love.mousepressed at the cursor, and mouseIsOverLocation (overworldview.lua:
            // 1280-1293) hit-tests whatever node sits under the SCREEN CENTRE. Selection is
            // safe -- it is not travel (:1472-1481). So: pan to an offset, press Return, and
            // read the outcome off the area-button slot.
            //
            // Classification uses NO pre-recorded hashes. The slot region covers both the
            // 250x100 area-button position (187,918) and the 64x80 showAreaButtonsButton at
            // (32,918) that appears when nothing is selected, so the three states -- nothing
            // selected / current location / some other node -- are simply three distinct
            // hashes, identified by which offsets produce them.
            let hx: i32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(96);
            let hy: i32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(96);
            let step: i32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(32);
            let (_, win) = attach()?;
            let (cw, ch) = win.client_size()?;
            let (ox, oy) = win.client_origin()?;
            let input = PostMessageInput::new(win);

            // GUARD: panning only happens while the highlight is on the map hotspot.
            let c = cursor();
            if (c.0 - ox - cw / 2).abs() > 4 || (c.1 - oy - ch / 2).abs() > 4 {
                return Err(format!(
                    "cursor at client ({},{}), not the map hotspot ({},{}). Run `diggle nav {} {}` first.",
                    c.0 - ox, c.1 - oy, cw / 2, ch / 2, cw / 2, ch / 2
                ).into());
            }

            let slot = diggle_solver::win::capture::Region::from_px(0, 860, 320, 975, cw, ch);

            // Self-calibrate direction and rate per axis instead of assuming the signs.
            // Measured rate is ~0.3 px/ms, but which key moves content which way is exactly
            // the sort of thing worth measuring once rather than getting silently backwards.
            let calib_ms = 300u64;
            let (kx, ky) = (measure_pan(&win, &input, "left", calib_ms)?, measure_pan(&win, &input, "up", calib_ms)?);
            println!("calibration: hold left {calib_ms}ms -> content d=({:+},{:+})", kx.0, kx.1);
            println!("calibration: hold up   {calib_ms}ms -> content d=({:+},{:+})", ky.0, ky.1);
            let rate_x = kx.0 as f64 / calib_ms as f64; // px per ms, signed, for "left"
            let rate_y = ky.1 as f64 / calib_ms as f64; // px per ms, signed, for "up"
            if rate_x.abs() < 0.05 || rate_y.abs() < 0.05 {
                return Err("calibration failed: map did not pan measurably".into());
            }

            // Pan content by (dx,dy) using whichever key has the right sign.
            let mut pan = |dx: i32, dy: i32| -> Result<(), Box<dyn std::error::Error>> {
                if dx != 0 {
                    let (dir, r) = if (dx as f64) * rate_x > 0.0 { ("left", rate_x) } else { ("right", -rate_x) };
                    let ms = (dx.abs() as f64 / r.abs()).round() as u64;
                    let (vk, sc) = if dir == "left" { (VK_LEFT, SC_LEFT) } else { (VK_RIGHT, SC_RIGHT) };
                    input.hold_extended_key(vk, sc, Duration::from_millis(ms.clamp(30, 4000)))?;
                }
                if dy != 0 {
                    let (dir, r) = if (dy as f64) * rate_y > 0.0 { ("up", rate_y) } else { ("down", -rate_y) };
                    let ms = (dy.abs() as f64 / r.abs()).round() as u64;
                    let (vk, sc) = if dir == "up" { (VK_UP, SC_UP) } else { (VK_DOWN, SC_DOWN) };
                    input.hold_extended_key(vk, sc, Duration::from_millis(ms.clamp(30, 4000)))?;
                }
                std::thread::sleep(Duration::from_millis(250));
                Ok(())
            };

            println!("\n  probe    offset      slot-hash");
            let mut seen: Vec<(u64, Vec<(i32, i32)>)> = Vec::new();
            let mut cur = (0i32, 0i32); // current pan offset applied to content
            let mut n = 0;
            let mut ys: Vec<i32> = Vec::new();
            let mut y = -hy;
            while y <= hy { ys.push(y); y += step; }
            for (row, &py) in ys.iter().enumerate() {
                let mut xs: Vec<i32> = Vec::new();
                let mut x = -hx;
                while x <= hx { xs.push(x); x += step; }
                if row % 2 == 1 { xs.reverse(); } // serpentine: minimise panning
                for &px in &xs {
                    // Offset (px,py) means: bring the point at (centre + (px,py)) to centre,
                    // i.e. move content by (-px,-py) relative to the untouched map.
                    let want = (-px, -py);
                    pan(want.0 - cur.0, want.1 - cur.1)?;
                    cur = want;
                    input.press_key(VK_RETURN, SC_RETURN)?;
                    std::thread::sleep(Duration::from_millis(450));
                    let f = capture_window(&win)?;
                    let h = f.region_hash(slot);
                    println!("  {n:5}  ({px:+5},{py:+5})  {h:016x}");
                    match seen.iter_mut().find(|(hh, _)| *hh == h) {
                        Some((_, v)) => v.push((px, py)),
                        None => seen.push((h, vec![(px, py)])),
                    }
                    n += 1;
                }
            }

            // Return the map to where we started, so the probe leaves no drift behind.
            pan(-cur.0, -cur.1)?;

            println!("\n{} probes, {} distinct slot states:", n, seen.len());
            seen.sort_by_key(|(_, v)| std::cmp::Reverse(v.len()));
            for (h, offs) in &seen {
                println!("  {h:016x}  x{:3}   e.g. {:?}", offs.len(), &offs[..offs.len().min(6)]);
            }
            println!(
                "\nThe most common state is almost certainly 'nothing selected'. Any state with\n\
                 few offsets, clustered together, is a NODE — its offsets are where that node\n\
                 sits relative to screen centre."
            );
        }
        "pantest" => {
            // Measures how far one held arrow press pans the overworld map.
            //
            // Uses crop-and-track rather than sprite matching: a patch cropped from the
            // frame is compared against the SAME rendering after the pan, so tint and
            // scale cancel out. `selftest` verifies this instrument returns inliers=1.000.
            let dir = args.get(1).map(|s| s.as_str()).unwrap_or("left");
            let ms: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(400);
            let (vk, sc) = match dir {
                "u" | "up" => (VK_UP, SC_UP),
                "d" | "down" => (VK_DOWN, SC_DOWN),
                "l" | "left" => (VK_LEFT, SC_LEFT),
                "r" | "right" => (VK_RIGHT, SC_RIGHT),
                other => return Err(format!("unknown direction {other:?}").into()),
            };
            let (_, win) = attach()?;
            let input = PostMessageInput::new(win);

            // GUARD (design v2 §7.1): panning only happens while the highlight is on the
            // map hotspot, where the cursor is pinned to the client centre. If it is not
            // there, arrows move a highlight instead and the measurement is meaningless.
            let (ox, oy) = win.client_origin()?;
            let c = cursor();
            let (ccx, ccy) = (c.0 - ox, c.1 - oy);
            let (cw, ch) = win.client_size()?;
            if (ccx - cw / 2).abs() > 4 || (ccy - ch / 2).abs() > 4 {
                return Err(format!(
                    "cursor at client ({ccx},{ccy}), not the map hotspot ({},{}). \
                     Run `diggle nav {} {}` first — refusing to measure.",
                    cw / 2, ch / 2, cw / 2, ch / 2
                )
                .into());
            }

            // Crop from well inside the map, away from the UI panels.
            const PATCH: u32 = 48;
            let (px, py) = (cw / 2 + 120, ch / 2 - 160);
            let before = capture_window(&win)?;
            let mut rgba = Vec::with_capacity((PATCH * PATCH * 4) as usize);
            for dy in 0..PATCH as i32 {
                for dx in 0..PATCH as i32 {
                    let i = (((py + dy) * before.width + (px + dx)) * 4) as usize;
                    rgba.extend_from_slice(&[
                        before.bgra[i + 2],
                        before.bgra[i + 1],
                        before.bgra[i],
                        255,
                    ]);
                }
            }
            let tpl = Template { name: "patch".into(), width: PATCH, height: PATCH, rgba };

            input.hold_extended_key(vk, sc, Duration::from_millis(ms))?;
            std::thread::sleep(Duration::from_millis(400)); // let momentum settle
            let after = capture_window(&win)?;

            // Search a generous band around the original position, step=1.
            let m = diggle_solver::observe::template::find_at_scale_in(
                &after,
                &tpl,
                1.0,
                1,
                Some((0, py - 400, after.width, py + 400)),
            )
            .ok_or("patch not found after pan")?;
            println!(
                "hold {dir} {ms}ms: patch ({px},{py}) -> ({},{})  d=({:+},{:+})  inliers={:.3}",
                m.x,
                m.y,
                m.x - px,
                m.y - py,
                m.inliers
            );
            if m.inliers < 0.6 {
                println!("WARNING: low inliers — the patch may have been re-rendered or left the frame");
            }
        }
        "hold" => {
            // Map panning is a HELD gesture: overworldview.hotspotDirection stores a
            // direction on key-down and hotspotDirectionRelease clears it on key-up
            // (overworldview.lua:1115-1133), with acceleration applied in core:update.
            // `key` holds for a fixed 60ms, which is useless for measuring pan distance.
            let dir = args.get(1).ok_or("usage: hold <dir> <ms>")?.as_str();
            let ms: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(500);
            let (vk, sc) = match dir {
                "u" | "up" => (VK_UP, SC_UP),
                "d" | "down" => (VK_DOWN, SC_DOWN),
                "l" | "left" => (VK_LEFT, SC_LEFT),
                "r" | "right" => (VK_RIGHT, SC_RIGHT),
                other => return Err(format!("unknown direction {other:?}").into()),
            };
            let (_, win) = attach()?;
            PostMessageInput::new(win).hold_extended_key(vk, sc, Duration::from_millis(ms))?;
            println!("held {dir} for {ms}ms; cursor={:?}", cursor());
        }
        "walk" => {
            // Enumerate a screen's hotspot graph against the ALREADY-RUNNING game.
            // spike_hotspot_graph does something similar but launches its own game and
            // resets the sandbox save, so it cannot be pointed at a live run.
            //
            // Presses a comma-separated arrow sequence, reading GetCursorPos after each
            // press, and clusters the distinct positions visited. Never presses Return —
            // exploration must not activate anything (design v2 §7.1).
            let seq = args.get(1).cloned().unwrap_or_default();
            if seq.is_empty() {
                return Err("usage: walk <dirs>   e.g. walk d,d,r,r,u,l".into());
            }
            let (_, win) = attach()?;
            let (ox, oy) = win.client_origin()?;
            let input = PostMessageInput::new(win);
            let mut visited: Vec<(i32, i32)> = Vec::new();
            let start = cursor();
            println!("start  -> screen={start:?} client={:?}", (start.0 - ox, start.1 - oy));
            visited.push(start);
            for (i, d) in seq.split(',').map(|s| s.trim()).enumerate() {
                let (vk, sc) = match d {
                    "u" | "up" => (VK_UP, SC_UP),
                    "d" | "down" => (VK_DOWN, SC_DOWN),
                    "l" | "left" => (VK_LEFT, SC_LEFT),
                    "r" | "right" => (VK_RIGHT, SC_RIGHT),
                    other => return Err(format!("unknown direction {other:?}").into()),
                };
                input.press_extended_key(vk, sc)?;
                std::thread::sleep(Duration::from_millis(280));
                let c = cursor();
                let moved = visited.last() != Some(&c);
                println!(
                    "{i:2} {d:5} -> screen={c:?} client={:?}{}",
                    (c.0 - ox, c.1 - oy),
                    if moved { "" } else { "   (no move — edge of graph)" }
                );
                visited.push(c);
            }
            // Cluster within 20px so jitter does not inflate the count.
            let mut clusters: Vec<(i32, i32)> = Vec::new();
            for p in &visited {
                if !clusters.iter().any(|c| dist(*c, *p) < 20.0) {
                    clusters.push(*p);
                }
            }
            println!("\ndistinct hotspots visited ({}):", clusters.len());
            for c in &clusters {
                println!("  client=({:4},{:4})", c.0 - ox, c.1 - oy);
            }
        }
        "save" => {
            // Dumps chosen paths from a save file. Exists so the reader is checked against the
            // REAL file rather than only against a sample I wrote myself.
            let which = args.get(1).map(|s| s.as_str()).unwrap_or("mainSaveData");
            let cfg = Config::load(Path::new("config.toml"))?;
            let dir = diggle_solver::game::savedir::locate(cfg.save_dir.clone(), true)?;
            let path = dir.join(which);
            println!("{}", path.display());
            let t = diggle_solver::game::save::load(&path)?;
            println!("top-level keys: {:?}", t.map.keys().collect::<Vec<_>>());
            for p in [
                "overworld.playerLocation",
                "overworld.seed",
                "rpg.player.turnState",
                "rpg.player.health",
                "rpg.player.turnNumber",
                "rpg.enemy.name",
                "rpg.enemy.health",
                "rpg.enemy.armour",
            ] {
                match t.path(p) {
                    Some(v) => println!("  {p} = {v:?}"),
                    None => println!("  {p} = <absent>"),
                }
            }
            if let Some(tb) = t.table_at("tileboard") {
                println!("  tileboard: {} letters, columns={:?}", tb.arr.len(),
                    tb.get("columns").and_then(|c| c.as_table()).map(|c| {
                        c.arr.iter().filter_map(|v| v.as_int()).collect::<Vec<_>>()
                    }));
            }
            println!(
                "combat in progress: {}",
                diggle_solver::game::save::combat_in_progress(&dir)
            );
        }
        "solve" => {
            // Offline: given board letters and enemy health, report what would be played. Lets the
            // search be exercised without a running game, which is how it gets checked at all.
            let letters = args.get(1).ok_or("usage: solve <letters> <health> [armour] [threads]")?;
            let health: i64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(3);
            let armour: i64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
            let threads: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(8);
            let cfg = Config::load(Path::new("config.toml"))?;
            let tiles: Vec<diggle_solver::observe::board::Tile> = letters
                .chars()
                .map(|c| diggle_solver::observe::board::Tile {
                    letter: c.to_ascii_uppercase().to_string(),
                    extra: None,
                })
                .collect();

            let t0 = std::time::Instant::now();
            let scorer = diggle_solver::score::Scorer::new(&cfg.game_dir)?;
            let dict = diggle_solver::search::Dictionary::load(&cfg.game_dir)?;
            println!("loaded {} words and letter materials in {:?}", dict.len(), t0.elapsed());
            if !scorer.unknown_materials().is_empty() {
                println!("WARNING unknown materials: {:?}", scorer.unknown_materials());
            }

            // Offline, so there is no save to read. The board SHAPE cannot be inferred from the tile
            // count -- `diamond16Board`, `tall16Board` and the default 4x4 all hold sixteen tiles and
            // put their corners in different places, and hexagonal boards have six corners rather
            // than four. So `--board <passiveKey>` names the shape, and it is always printed.
            let board_passive = args
                .iter()
                .position(|a| a == "--board")
                .and_then(|i| args.get(i + 1))
                .cloned();
            let resolved = match &board_passive {
                Some(key) => {
                    diggle_solver::geometry::Geometry::for_passive(&cfg.game_dir, key, tiles.len())
                }
                None => diggle_solver::geometry::Geometry::for_passive(
                    &cfg.game_dir,
                    "__none__",
                    tiles.len(),
                ),
            };
            let geometry = resolved.geometry;
            for p in &resolved.problems {
                println!("WARNING geometry: {p}");
            }
            println!(
                "board shape: {} ({} columns of {:?}), {} corners at dump indices {:?}",
                board_passive.as_deref().unwrap_or("default 4x4"),
                geometry.rows_per_col.len(),
                geometry.rows_per_col,
                geometry.corner_count(),
                geometry.corner_indices()
            );

            let mut mods = diggle_solver::search::Modifiers::none();
            mods.resist_cornerless = args.iter().any(|a| a == "--cornerless");
            if mods.resist_cornerless {
                println!(
                    "resistCornerless: score scaled by cornersUsed/{}",
                    geometry.corner_count()
                );
            }

            let need = health + armour;
            let t1 = std::time::Instant::now();
            let out = diggle_solver::search::race_for_kill(
                &dict, &scorer, &tiles, &geometry, &mods, need, threads,
            );
            let elapsed = t1.elapsed();
            println!(
                "board {} ({} tiles), need {need} (health {health} + armour {armour})",
                letters.to_ascii_uppercase(),
                tiles.len()
            );
            println!(
                "searched {} of {} words in {:?} across {threads} slices",
                out.words_considered,
                dict.len(),
                elapsed
            );
            match &out.lethal {
                Some(f) => println!("LETHAL: {} scores {} (slice {})", f.word, f.score, f.slice),
                None => println!("no lethal word found"),
            }
            match &out.best {
                Some(f) => println!("best:   {} scores {} (slice {})", f.word, f.score, f.slice),
                None => println!("best:   none"),
            }
            match &out.longest {
                Some(f) => println!("longest: {} ({} letters, scores {})", f.word, f.word.chars().count(), f.score),
                None => println!("longest: none"),
            }
            let threshold = tiles.len() / 2;
            println!(
                "refresh (longest < {threshold} letters)? {}  -> play {:?}",
                out.should_refresh(threshold),
                out.choice().map(|f| f.word.as_str())
            );
        }
        "findpng" => {
            // Matches a template against a SAVED frame, with no game running. This is the positive
            // control the live path lacks: if a template cropped byte-identically out of a frame
            // does not match that frame, the fault is in the matcher, not on screen.
            let tpl_path = args.get(1).ok_or("usage: findpng <template> <frame.png> [x0 y0 x1 y1]")?;
            let frame_path = args.get(2).ok_or("usage: findpng <template> <frame.png> [x0 y0 x1 y1]")?;
            let tpl = Template::load(Path::new(tpl_path))?;
            // Decode the frame into the same BGRA layout capture_window produces, so this exercises
            // exactly the comparison the live path does.
            let dec = png::Decoder::new(std::fs::File::open(frame_path)?);
            let mut rdr = dec.read_info()?;
            let mut buf = vec![0; rdr.output_buffer_size()];
            let info = rdr.next_frame(&mut buf)?;
            let n = info.color_type.samples();
            let mut bgra = Vec::with_capacity((info.width * info.height * 4) as usize);
            for px in buf.chunks_exact(n) {
                bgra.extend_from_slice(&[px[2], px[1], px[0], 255]);
            }
            let frame = Frame { width: info.width as i32, height: info.height as i32, bgra };
            println!("template {} {}x{}; frame {}x{}", tpl.name, tpl.width, tpl.height, frame.width, frame.height);
            let bounds = if args.len() >= 7 {
                let v: Vec<i32> = args[3..7].iter().filter_map(|a| a.parse().ok()).collect();
                (v.len() == 4).then(|| (v[0], v[1], v[2], v[3]))
            } else {
                None
            };
            match diggle_solver::observe::template::find_at_scale_in(&frame, &tpl, 1.0, 1, bounds) {
                Some(m) => println!(
                    "best at ({},{}) inliers {:.4} error {:.4}  bounds={bounds:?}",
                    m.x, m.y, m.inliers, m.error
                ),
                None => println!("no match at all  bounds={bounds:?}"),
            }
        }
        "croppng" => {
            // Measuring a UI bounding box means looking at it. Eyeballing a region off a
            // full-resolution screenshot is how the F1 classifier ended up hashing map and sea
            // around a panel; cropping the candidate and viewing it proves what is inside.
            let (inp, outp) = (args.get(1).ok_or("usage: croppng <in> <out> <x0> <y0> <x1> <y1>")?,
                               args.get(2).ok_or("usage: croppng <in> <out> <x0> <y0> <x1> <y1>")?);
            let n: Vec<u32> = args[3..7].iter().map(|a| a.parse().unwrap()).collect();
            let (x0, y0, x1, y1) = (n[0], n[1], n[2], n[3]);
            let decoder = png::Decoder::new(std::fs::File::open(inp)?);
            let mut reader = decoder.read_info()?;
            let mut buf = vec![0; reader.output_buffer_size()];
            let info = reader.next_frame(&mut buf)?;
            let (w, ch_count) = (info.width, info.color_type.samples());
            let (cw2, chh) = (x1 - x0, y1 - y0);
            let mut out = Vec::with_capacity((cw2 * chh * 4) as usize);
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = ((y * w + x) as usize) * ch_count;
                    out.extend_from_slice(&[buf[i], buf[i + 1], buf[i + 2], 255]);
                }
            }
            let file = std::fs::File::create(outp)?;
            let mut enc = png::Encoder::new(std::io::BufWriter::new(file), cw2, chh);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            enc.write_header()?.write_image_data(&out)?;
            println!("cropped ({x0},{y0})-({x1},{y1}) = {cw2}x{chh} -> {outp}");
        }
        "travel" => {
            // The travel step, end to end: read the map from the log, pan the chosen node under
            // the map hotspot, select it, then activate Travel.
            //
            // Every position here is MEASURED, never dead-reckoned. The log gives node positions
            // in screen pixels at the moment it printed; anything that pans the map afterwards --
            // including the arrow press `nav` uses to establish a highlight -- invalidates them.
            // So each step that could move the map is wrapped in a crop-and-track measurement and
            // the node positions are corrected by the observed displacement.
            let want = args.get(1).cloned();
            let cfg = Config::load(Path::new("config.toml"))?;
            let mut console = diggle_solver::observe::log::Console::take()?;
            let mut game = diggle_solver::game::launch::GameProcess::launch(&cfg, &console)?;
            echo(&console, format!("launched pid {}", game.pid()));
            let win = game.wait_for_window(Duration::from_secs(20))?;
            std::thread::sleep(Duration::from_secs(3));
            let (cw, ch) = win.client_size()?;
            let input = PostMessageInput::new(win);
            input.focus();
            std::thread::sleep(Duration::from_millis(500));

            let mut mirror = diggle_solver::observe::log::LogMirror::create(Path::new(&format!(
                "{FRAME_DIR}/travel-log.txt"
            )))?;

            // Reads the channel until a dump arrives, or gives up. Returns the LAST one: if
            // arrival fired an event the newest dump is the authoritative one.
            let mut collect = |console: &mut diggle_solver::observe::log::Console,
                               mirror: &mut diggle_solver::observe::log::LogMirror,
                               secs: u64|
             -> Result<Option<diggle_solver::observe::adjacency::Adjacency>, Box<dyn std::error::Error>> {
                let deadline = std::time::Instant::now() + Duration::from_secs(secs);
                let mut last = None;
                while std::time::Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(400));
                    let lines = console.read_new()?;
                    if lines.is_empty() {
                        continue;
                    }
                    mirror.write(&lines);
                    if let Some(a) = diggle_solver::observe::adjacency::parse(&lines).pop() {
                        last = Some(a);
                        break;
                    }
                }
                Ok(last)
            };

            // --- reach the overworld ---
            let nav = diggle_solver::win::nav::to_client_point(&win, &input, 187, 810, 60)?;
            if !nav.on_target {
                // Restart shares this row and eulogizes the run (`heroselect.lua:271`).
                echo(&console, format!("ABORT: highlight at {:?}, not Continue", nav.landed));
                game.close(Duration::from_secs(15));
                return Ok(());
            }
            echo(&console, format!("nav to Continue: trail {:?}", nav.trail));
            input.press_key(VK_RETURN, SC_RETURN)?;
            let Some(map) = collect(&mut console, &mut mirror, 20)? else {
                // A screenshot, because "no dump" alone cannot distinguish "still on the menu"
                // from "on the overworld but silent" -- and guessing between those is exactly
                // how this project has produced invalid verdicts before.
                let shot = format!("{FRAME_DIR}/travel-no-dump.bmp");
                if let Ok(f) = capture_window(&win) {
                    let _ = f.write_bmp(Path::new(&shot));
                }
                echo(&console, format!("ABORT: no adjacency dump after Continue; screen saved to {shot}"));
                game.close(Duration::from_secs(15));
                return Ok(());
            };
            echo(
                &console,
                format!("at {} — {}; {} visible neighbour(s), {} hidden",
                    map.here_key, map.here_heading, map.nodes.len(), map.hidden),
            );

            // --- choose a target ---
            // Shrines first (§1 objective), then the lowest combat level, then whatever exists.
            // `hidden` is reported because a nonzero count means better options may be behind
            // cloud and this choice is provisional.
            let target = match &want {
                Some(k) => map.nodes.iter().find(|n| &n.key == k).cloned(),
                None => map
                    .nodes
                    .iter()
                    .find(|n| n.type_is("shrine"))
                    .or_else(|| {
                        map.nodes.iter().min_by_key(|n| n.level().unwrap_or(u32::MAX))
                    })
                    .cloned(),
            };
            let Some(target) = target else {
                echo(&console, format!("ABORT: no target (wanted {want:?})"));
                game.close(Duration::from_secs(15));
                return Ok(());
            };
            echo(&console, format!("target {} — {} at ({:.1},{:.1})", target.key, target.heading, target.x, target.y));
            if map.hidden > 0 {
                echo(&console, format!("NOTE: {} neighbour(s) hidden by cloud — this choice is provisional", map.hidden));
            }

            // --- get the highlight onto the map hotspot ---
            // The map hotspot is one big rect (`overworldview.lua:1146-1149`) whose highlight
            // warps the cursor to the client centre — confirmed empirically, the nav lands on
            // (960,540) for a 1920x1080 client.
            //
            // Reaching it does NOT pan the map: arrow presses only pan once the highlight is
            // already on the map, and the walk stops the moment it arrives. So the node positions
            // from the dump above are still valid here. Verified rather than assumed, below.
            let to_map = diggle_solver::win::nav::to_client_point(&win, &input, cw / 2, ch / 2, 40)?;
            std::thread::sleep(Duration::from_millis(400));
            echo(&console, format!(
                "highlight -> {:?} (map hotspot {:?}, on_target {})\n  trail {:?}",
                to_map.landed, (cw / 2, ch / 2), to_map.on_target, to_map.trail
            ));
            if !to_map.on_target {
                echo(&console, "ABORT: could not put the highlight on the map hotspot".into());
                game.close(Duration::from_secs(15));
                return Ok(());
            }

            // --- calibrate BOTH axes, then pan by the amount the log implies ---
            // Which arrow moves content which way is measured, not assumed — and per axis. The
            // first attempt calibrated x only and assumed y matched the key direction; it does not.
            // Measured: Right moves content -x AND Down moves content -y, i.e. the keys move the
            // CAMERA and the content goes the other way. Assuming that symmetry would have worked,
            // but only by luck, and the run that assumed it diverged instead of converging.
            //
            // This doubles as the tracker's positive control: a deliberate hold that produces no
            // confident, measurable movement means the instrument is unusable, and panning blind
            // is worse than stopping.
            // 400 ms, not 250: the two runs whose nav caused zero drift also measured zero
            // movement from a 250 ms hold, while the run where the map was already panning during
            // nav measured a clean -75 px. That is consistent with the gesture ramping from rest
            // (`hotspotAcceleration`, `overworldview.lua:1133-1135`), so give it longer to build.
            // Kept under TRACK_RADIUS so the patch cannot leave the tracker's search window.
            let cal_ms = 400u64;
            let mut rates = [0f64; 2]; // signed px/ms for Right, then for Down
            // The patch the calibration PROVED responsive, and where it currently sits.
            let mut tracker: Option<(Template, i32, i32)> = None;
            for (axis, vk, sc, name) in
                [(0usize, VK_RIGHT, SC_RIGHT, "Right"), (1, VK_DOWN, SC_DOWN, "Down")]
            {
                // A held arrow only pans while the game has the foreground, and this process
                // allocates a console window that can steal it. Report it, and take it back:
                // a confident zero-movement reading with the window unfocused would otherwise look
                // exactly like "the highlight is not on the map".
                let fg_before = input.has_foreground();
                if !fg_before {
                    input.focus();
                    std::thread::sleep(Duration::from_millis(400));
                }
                let (t, nx, ny, d, inl) =
                    calibrate_and_pick(&win, &input, cw, ch, vk, sc, cal_ms)?;
                echo(&console, format!(
                    "  ({name} calibration: foreground before {fg_before}, after {})",
                    input.has_foreground()
                ));
                let along = if axis == 0 { d.0 } else { d.1 };
                echo(&console, format!(
                    "calibration: hold {name} {cal_ms}ms -> content moved {d:?} \
                     (patch now at ({nx},{ny}), inliers {inl:.3})"
                ));
                tracker = Some((t, nx, ny));
                if along.abs() < 20 {
                    echo(&console, format!(
                        "ABORT: a {cal_ms} ms {name} hold moved the map <20 px on its axis — the \
                         highlight may not really be on the map, or the map is at a pan limit"
                    ));
                    game.close(Duration::from_secs(15));
                    return Ok(());
                }
                rates[axis] = along as f64 / cal_ms as f64;
            }
            // Calibration itself panned the map; the node moved with it.
            let mut node_at = (
                target.x as i32 + (rates[0] * cal_ms as f64) as i32,
                target.y as i32 + (rates[1] * cal_ms as f64) as i32,
            );
            echo(&console, format!(
                "rates: Right {:+.3} px/ms, Down {:+.3} px/ms; node now {:?}",
                rates[0], rates[1], node_at
            ));

            let mut ok = false;
            for attempt in 1..=5 {
                let (dx, dy) = (cw / 2 - node_at.0, ch / 2 - node_at.1);
                if dx.abs() <= 14 && dy.abs() <= 14 {
                    ok = true;
                    break;
                }
                // Reuse the patch the calibration proved responsive, re-cropped where it now sits.
                // Re-choosing per iteration risks picking a screen-fixed overlay again.
                let f = wait_map_still(&win, Duration::from_secs(3))?;
                let (px, py) = tracker.as_ref().map(|(_, x, y)| (*x, *y)).unwrap();
                let tpl = crop_patch(&f, px, py, PATCH);
                let mut expect = (0, 0);
                for (axis, delta, pos, neg) in [
                    (0usize, dx, (VK_RIGHT, SC_RIGHT), (VK_LEFT, SC_LEFT)),
                    (1, dy, (VK_DOWN, SC_DOWN), (VK_UP, SC_UP)),
                ] {
                    if delta.abs() < 10 {
                        continue;
                    }
                    // Cap each step so the patch cannot pan outside the tracker's search window.
                    let want = delta.clamp(-TRACK_RADIUS + 20, TRACK_RADIUS - 20);
                    let rate = rates[axis];
                    let ms = ((want.abs() as f64 / rate.abs()).round() as u64).clamp(40, 3000);
                    let (k, s) = if (want as f64) * rate > 0.0 { pos } else { neg };
                    input.hold_extended_key(k, s, Duration::from_millis(ms))?;
                    wait_map_still(&win, Duration::from_secs(3))?;
                    if axis == 0 {
                        expect.0 = want;
                    } else {
                        expect.1 = want;
                    }
                }
                let Some((moved, inl, err)) = track_patch(&win, &tpl, px, py, expect) else {
                    echo(&console, format!(
                        "ABORT: no confident match on pan {attempt} (expected {expect:?}) — \
                         refusing to dead-reckon"
                    ));
                    game.close(Duration::from_secs(15));
                    return Ok(());
                };
                node_at = (node_at.0 + moved.0, node_at.1 + moved.1);
                // Follow the patch to its new home so the next iteration crops it there.
                tracker = Some((crop_patch(&f, px, py, PATCH), px + moved.0, py + moved.1));
                echo(&console, format!(
                    "  pan {attempt}: wanted {:?}, expected {expect:?}, content moved {moved:?}, \
                     node now {:?} (inliers {inl:.3}, err {err:.1})",
                    (dx, dy), node_at
                ));
            }
            if !ok {
                echo(&console, format!("ABORT: node still {:?} from centre", (cw / 2 - node_at.0, ch / 2 - node_at.1)));
                game.close(Duration::from_secs(15));
                return Ok(());
            }

            // --- select it ---
            // `Return` on the map hotspot synthesizes `love.mousepressed` at the cursor, and
            // `mouseIsOverLocation` (`:1280-1293`) hit-tests whatever node is under it. Selection
            // is not travel (`:1472-1481`), so this is safe.
            let slot = diggle_solver::win::capture::Region::from_px(0, 860, 320, 975, cw, ch);
            let pre = capture_window(&win)?;
            input.press_key(VK_RETURN, SC_RETURN)?;
            std::thread::sleep(Duration::from_millis(600));
            let post = capture_window(&win)?;
            // A fair comparison: no pan happened between these two frames, so any change in the
            // button slot is caused by the selection. This is the invariance the F1 probe got
            // wrong -- it compared frames taken at DIFFERENT pan offsets, where map and sea
            // showing around the panel changed the hash on their own.
            let selected = pre.region_hash(slot) != post.region_hash(slot);
            echo(&console, format!("Return on the map -> button slot changed: {selected}"));
            if !selected {
                echo(&console, "ABORT: nothing was selected — the node was not under the cursor".into());
                game.close(Duration::from_secs(15));
                return Ok(());
            }

            // --- activate Travel ---
            // Arrows cannot leave the map hotspot: it defines `hotspotDirection`, which consumes
            // them to pan instead (`utils/input.lua:186-190`). The way out is `goBack`, which the
            // overworld defines as `backOutOfHotspotMapPan` (`overworld.lua:1407`) -> snap to the
            // hotspot nearest (0, height) -- the bottom-left button slot.
            input.press_key(VK_BACK, SC_BACK)?;
            std::thread::sleep(Duration::from_millis(500));
            let c = cursor();
            let (ox, oy) = win.client_origin()?;
            echo(&console, format!("Backspace -> highlight at client {:?}", (c.0 - ox, c.1 - oy)));
            let to_travel = diggle_solver::win::nav::to_client_point(&win, &input, 187, 918, 60)?;
            echo(&console, format!("nav to Travel {:?} on_target {}", to_travel.landed, to_travel.on_target));
            if !to_travel.on_target {
                echo(&console, "ABORT: never reached the Travel button; nothing activated".into());
                game.close(Duration::from_secs(15));
                return Ok(());
            }
            input.press_key(VK_RETURN, SC_RETURN)?;

            // --- confirm arrival from the log ---
            // Travel is asynchronous (`travelTo` `:1394-1400` only starts a walk) and arrival can
            // fire an event (`doArriveEvent` `:1425`), so the log is the only trustworthy report
            // of where we ended up.
            match collect(&mut console, &mut mirror, 40)? {
                Some(a) => {
                    let arrived = a.here_key == target.key;
                    echo(&console, format!(
                        "\n[{}] now at {} — {}\nTRAVEL {}: wanted {}, got {}",
                        a.reason, a.here_key, a.here_heading,
                        if arrived { "SUCCEEDED" } else { "WENT SOMEWHERE ELSE" },
                        target.key, a.here_key
                    ));
                }
                None => echo(&console, "no arrival dump within 40 s — travel may not have started".into()),
            }

            let exited = game.close(Duration::from_secs(15));
            echo(&console, format!("log mirrored to {FRAME_DIR}/travel-log.txt; exited gracefully: {exited}"));
        }
        "overworld" => {
            // The one command that cannot follow the one-shot pattern. LÖVE attaches to its
            // PARENT's console (`love.cpp:562-628`), so whoever reads the log has to be the
            // process that launched the game and has to stay alive. That makes the interactive
            // `launch`/`where`/`shot` workflow and the log channel mutually exclusive: those
            // attach to a game someone else started and get no log; this owns the whole session.
            let secs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(30);
            let cfg = Config::load(Path::new("config.toml"))?;

            // Taking a console replaces our standard handles, so `println!` goes nowhere from
            // here on. `Console::echo` writes to the stdout we had beforehand.
            let mut console = diggle_solver::observe::log::Console::take()?;

            let mut game = diggle_solver::game::launch::GameProcess::launch(&cfg, &console)?;
            echo(&console, format!("launched pid {} — {}", game.pid(), diggle_solver::game::launch::build_command_line(&cfg)));
            let win = game.wait_for_window(Duration::from_secs(20))?;
            echo(&console, "window up; settling".into());
            std::thread::sleep(Duration::from_secs(3));

            let mut mirror = diggle_solver::observe::log::LogMirror::create(Path::new(
                &format!("{FRAME_DIR}/overworld-log.txt"),
            ))?;

            // With `mainSaveData` present the game boots to the start menu and prints NOTHING,
            // so the channel looks dead until something makes it talk. Continue -> world load
            // -> `verboseAdjacencyData('World loaded')` (`overworldview.lua:1607`).
            let input = PostMessageInput::new(win);
            input.focus();
            std::thread::sleep(Duration::from_millis(500));
            let nav = diggle_solver::win::nav::to_client_point(&win, &input, 187, 810, 60)?;
            echo(&console, format!("nav trail {:?}", nav.trail));
            if nav.on_target {
                input.press_key(VK_RETURN, SC_RETURN)?;
                echo(&console, "highlight on Continue — activated".into());
            } else {
                // Restart shares this row and eulogizes the run (`heroselect.lua:271`). An
                // unverified Return here is unrecoverable, so refuse and keep watching: if the
                // game is already past the menu we will still see dumps.
                echo(&console, format!(
                    "REFUSED to press Return: highlight landed at {:?}, not Continue (187,810)",
                    nav.landed
                ));
            }

            let deadline = std::time::Instant::now() + Duration::from_secs(secs);
            let mut seen = 0usize;
            while std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(500));
                let lines = console.read_new()?;
                if lines.is_empty() {
                    continue;
                }
                mirror.write(&lines);
                for a in diggle_solver::observe::adjacency::parse(&lines) {
                    seen += 1;
                    let mut out = format!(
                        "\n[{}] at {} — {}\n",
                        a.reason, a.here_key, a.here_heading
                    );
                    if let Some((k, h)) = &a.subworld {
                        out.push_str(&format!("  in subworld {k} — {h}\n"));
                    }
                    for n in &a.nodes {
                        out.push_str(&format!(
                            "  {:<8} ({:7.1},{:7.1})  conns {}  {}{}\n",
                            n.key,
                            n.x,
                            n.y,
                            n.connections,
                            n.heading,
                            match n.level() {
                                Some(l) => format!("   [combat lvl {l}]"),
                                None => "   [no combat]".into(),
                            }
                        ));
                    }
                    for e in &a.exits {
                        out.push_str(&format!(
                            "  exit     ({:7.1},{:7.1})  -> {} — {}\n",
                            e.x, e.y, e.to_key, e.to_heading
                        ));
                    }
                    if a.hidden > 0 || a.hidden_exits > 0 {
                        // Not cosmetic: a nonzero count means the map is withholding options, so
                        // "no shrine adjacent" is not yet a conclusion (§1).
                        out.push_str(&format!(
                            "  {} hidden neighbour(s), {} hidden exit(s) — cloud-covered or not \
                             yet visible\n",
                            a.hidden, a.hidden_exits
                        ));
                    }
                    echo(&console, out);
                }
            }

            let exited = game.close(Duration::from_secs(15));
            echo(&console, format!(
                "\n{seen} dump(s) parsed; log mirrored to {FRAME_DIR}/overworld-log.txt; \
                 game exited gracefully: {exited}"
            ));
        }
        "watch" => {
            let secs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(8);
            let (_, win) = attach()?;
            // Design v2 §7: observations return a TRACE, not a boolean. Read the shape —
            // rise-then-plateau is a transition, flat jitter is noise.
            let base = capture_window(&win)?;
            let mut prev = base.clone();
            println!("t     vs_base  vs_prev");
            for i in 1..=(secs * 2) {
                std::thread::sleep(Duration::from_millis(500));
                let f = capture_window(&win)?;
                println!(
                    "{:4.1}s  {:.4}   {:.4}",
                    i as f64 * 0.5,
                    base.diff_fraction(&f, FULL),
                    prev.diff_fraction(&f, FULL)
                );
                prev = f;
            }
        }
        _ => {
            println!("{}", include_str!("diggle_usage.txt"));
        }
    }
    Ok(())
}

/// Spawns the game and returns its pid, deliberately NOT retaining the Child so the
/// process outlives this command.
struct GameProcessDetached;
impl GameProcessDetached {
    fn spawn(cfg: &Config) -> Result<u32, Box<dyn std::error::Error>> {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP. Without these the game inherits
        // our console and keeps the calling shell's pipeline open, so `diggle launch`
        // never returns even though it has done its job.
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        let child = std::process::Command::new(&cfg.lovec_path)
            .arg(&cfg.game_dir)
            .arg("--verbose")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
            .spawn()?;
        Ok(child.id())
    }
}
