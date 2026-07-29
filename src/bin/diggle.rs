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
            println!("cursor={:?}", cursor());
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
            let target = (tx, ty);
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
