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

/// Holds an arrow key and measures how far the map CONTENT moved, by crop-and-track.
///
/// Frame-to-frame, so the map's time-of-day tint and draw scale cancel out — the same
/// instrument `diggle selftest` validates at inliers=1.000. Returns the signed displacement.
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
