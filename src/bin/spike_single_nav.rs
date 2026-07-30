//! Single-launch hotspot navigation test, for the user to observe directly.
//!
//! One launch only. Dismisses the lore card with Return, navigates the hotspot graph to
//! the Start button, presses Return, and waits for quiescence before measuring (rather
//! than sleeping a fixed interval, which previously produced a false negative on a ~2s
//! transition).
//!
//! Mitigation under test: the game warps the REAL OS cursor via love.mouse.setPosition
//! whenever the hotspot highlight moves. We record the pointer before navigating and
//! restore it afterwards, so the user gets their cursor back.
//!
//! Run: cargo run --bin spike_single_nav -- config.toml

use diggle_solver::config::Config;
use diggle_solver::game::launch::PipedGameProcess;
use diggle_solver::observe::settle::{sample_noise_floor, wait_for_quiescence, FULL};
use diggle_solver::win::capture::capture_window;
use diggle_solver::win::input::{
    PostMessageInput, SC_DOWN, SC_LEFT, SC_RETURN, SC_RIGHT, SC_UP, VK_DOWN, VK_LEFT, VK_RETURN,
    VK_RIGHT, VK_UP,
};
use diggle_solver::win::window;
use std::path::Path;
use std::time::Duration;
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, SetCursorPos};

const START: (i32, i32) = (500, 810);
const ON_TARGET_PX: f64 = 60.0;

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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "config.toml".into());
    let label = args.next().unwrap_or_else(|| "run".into());
    // Seconds to wait after the window appears, before navigating. Gives the human time
    // to arrange the window (e.g. cover it with a fullscreen app) for this trial.
    let setup_secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(6);
    let cfg = Config::load(Path::new(&path))?;
    std::fs::create_dir_all("spike-frames-5")?;

    // Sandbox save dir only. %APPDATA%\SternlyWordedAdventures (no LOVE) is the real save.
    if let Ok(appdata) = std::env::var("APPDATA") {
        let p = Path::new(&appdata)
            .join("LOVE")
            .join("SternlyWordedAdventures")
            .join("persistentSaveData");
        let _ = std::fs::remove_file(&p);
    }

    let user_cursor = cursor();
    println!("user cursor before anything: {user_cursor:?}");

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
    println!("[{label}] window found. Waiting {setup_secs}s for you to arrange it...");
    std::thread::sleep(Duration::from_secs(setup_secs));
    let input = PostMessageInput::new(win);

    // Lore card -> start menu. Typed input does NOT move the cursor.
    let _ = wait_for_quiescence(&win, 0.02, Duration::from_secs(8))?;
    input.press_extended_key(VK_RETURN, SC_RETURN)?;
    let _ = wait_for_quiescence(&win, 0.02, Duration::from_secs(8))?;
    println!("dismissed lore card; cursor still {:?}", cursor());

    let floor = sample_noise_floor(&win, 5, Duration::from_millis(250))?;
    let before = capture_window(&win)?;
    before.write_bmp(Path::new(&format!("spike-frames-5/{label}-before.bmp")))?;

    // --- navigation: THIS is the part that moves the real pointer ---
    println!("navigating (your cursor will move now)...");
    input.press_extended_key(VK_DOWN, SC_DOWN)?;
    std::thread::sleep(Duration::from_millis(300));
    println!("  after Down:  {:?}", cursor());

    for step in 0..10 {
        let c = cursor();
        if dist(c, START) <= ON_TARGET_PX {
            break;
        }
        let (dx, dy) = (START.0 - c.0, START.1 - c.1);
        let (vk, sc, name) = if dx.abs() >= dy.abs() {
            if dx > 0 { (VK_RIGHT, SC_RIGHT, "Right") } else { (VK_LEFT, SC_LEFT, "Left") }
        } else if dy > 0 {
            (VK_DOWN, SC_DOWN, "Down")
        } else {
            (VK_UP, SC_UP, "Up")
        };
        input.press_extended_key(vk, sc)?;
        std::thread::sleep(Duration::from_millis(300));
        println!("  step {step} {name:5} -> {:?}", cursor());
    }

    let landed = cursor();
    let on = dist(landed, START) <= ON_TARGET_PX;
    println!("landed {landed:?} on_target={on} (Start centre {START:?})");

    if on {
        input.press_extended_key(VK_RETURN, SC_RETURN)?;
        // Watch for a full 10s rather than trusting any single sampling strategy.
        // A fixed sleep produced a false NEGATIVE here once, and quiescence-only
        // produced a false POSITIVE, because quiescence is trivially satisfied in the
        // instant before a transition begins.
        let mut trace = String::new();
        let mut best = 0.0f64;
        for sec in 1..=10 {
            std::thread::sleep(Duration::from_secs(1));
            let f = capture_window(&win)?;
            let d = before.diff_fraction(&f, FULL);
            if d > best {
                best = d;
            }
            trace.push_str(&format!("{sec}s={d:.3} "));
            if sec == 10 {
                f.write_bmp(Path::new(&format!("spike-frames-5/{label}-after.bmp")))?;
            }
        }
        println!("[{label}] Return pressed. floor={floor:.4}");
        println!("[{label}] 10s watch: {trace}");
        println!("[{label}] max delta = {best:.4}  -> ACTIVATED = {}", best > 0.35);
    } else {
        println!("[{label}] never reached Start; Return not pressed");
    }

    let _ = game.kill();
    std::thread::sleep(Duration::from_millis(400));

    // Give the user their pointer back.
    unsafe {
        let _ = SetCursorPos(user_cursor.0, user_cursor.1);
    }
    println!("restored cursor to {user_cursor:?} (now {:?})", cursor());
    Ok(())
}
