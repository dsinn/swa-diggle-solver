//! Diagnostic probe: which start-menu buttons does Return actually activate?
//!
//! Established already: arrow keys move the game's hotspot highlight (it warps the real
//! OS cursor via love.mouse.setPosition, so GetCursorPos is ground truth), and Return
//! calls love.mousepressed inside Lua, bypassing SDL mouse delivery. Return activated
//! the Patreon icon but NOT Start, reproducibly. Both use `mousereleased` and both end
//! in setActiveMode, so that is not the difference.
//!
//! Part 1 proves by measurement (not by eye) that the highlight lands on Start.
//! Part 2 discriminates: Arcade goes straight to setActiveMode, whereas Start routes
//! through classes.checkForCallbacksAndSetActive -> heroselect.load{} first.
//!
//! Run: cargo run --bin spike_button_probe -- config.toml

use diggle_solver::config::Config;
use diggle_solver::game::launch::PipedGameProcess;
use diggle_solver::observe::settle::{reacted, sample_noise_floor, wait_for_quiescence, FULL};
use diggle_solver::win::capture::{capture_window, Region};
use diggle_solver::win::input::{
    PostMessageInput, SC_DOWN, SC_LEFT, SC_RETURN, SC_RIGHT, SC_UP, VK_DOWN, VK_LEFT, VK_RETURN,
    VK_RIGHT, VK_UP,
};
use diggle_solver::win::window::{self, GameWindow};
use std::io::Write;
use std::path::Path;
use std::time::Duration;
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

const SETTLE: Duration = Duration::from_millis(1200);
/// Cursor must land within this many pixels of a button centre to count as "on" it.
const ON_TARGET_PX: f64 = 60.0;
const MAX_PRESSES: usize = 14;

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

/// Pixel rect -> normalized Region, for cropping a capture to one button.
fn rect(x0: i32, y0: i32, x1: i32, y1: i32, w: i32, h: i32) -> Region {
    Region {
        nx: x0 as f64 / w as f64,
        ny: y0 as f64 / h as f64,
        nw: (x1 - x0) as f64 / w as f64,
        nh: (y1 - y0) as f64 / h as f64,
    }
}

struct Session {
    game: PipedGameProcess,
    win: GameWindow,
    input: PostMessageInput,
}

/// Fresh launch, cleared sandbox save, Return past the lore card, parked on the start
/// menu. Returns the session plus the noise floor measured ON the start menu.
fn start_menu(cfg: &Config) -> Result<(Session, f64), Box<dyn std::error::Error>> {
    // Sandbox only. %APPDATA%\SternlyWordedAdventures (no LOVE) is the real save.
    if let Ok(appdata) = std::env::var("APPDATA") {
        let p = Path::new(&appdata)
            .join("LOVE")
            .join("SternlyWordedAdventures")
            .join("persistentSaveData");
        let _ = std::fs::remove_file(&p);
    }

    let mut game = PipedGameProcess::launch(cfg)?;
    let win = loop {
        if let Some(w) = window::find_by_pid(game.pid()) {
            break w;
        }
        if !game.is_running() {
            return Err("game exited before a window appeared".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    std::thread::sleep(Duration::from_secs(6));
    let input = PostMessageInput::new(win);

    // Positive control: Return must dismiss the lore card. If it does not, this run is void.
    let _ = wait_for_quiescence(&win, 0.02, Duration::from_secs(8))?;
    let floor_lore = sample_noise_floor(&win, 4, Duration::from_millis(250))?;
    let before = capture_window(&win)?;
    input.press_extended_key(VK_RETURN, SC_RETURN)?;
    std::thread::sleep(SETTLE);
    let after = capture_window(&win)?;
    let control_ok = reacted(&before, &after, floor_lore);
    println!(
        "  positive control (Return on lore card): reacted={control_ok} delta={:.4} floor={floor_lore:.4}",
        before.diff_fraction(&after, FULL)
    );
    if !control_ok {
        return Err("positive control FAILED - run is void".into());
    }

    let _ = wait_for_quiescence(&win, 0.02, Duration::from_secs(8))?;
    let floor = sample_noise_floor(&win, 5, Duration::from_millis(250))?;
    Ok((Session { game, win, input }, floor))
}

/// Walk the hotspot graph toward `target`, choosing the arrow that most reduces
/// distance. Returns the presses made and whether we landed on target.
fn navigate_to(s: &Session, target: (i32, i32)) -> Result<(Vec<&'static str>, bool), Box<dyn std::error::Error>> {
    let mut log = Vec::new();
    // One press to establish a highlight at all; without one, Return is a no-op.
    s.input.press_extended_key(VK_DOWN, SC_DOWN)?;
    std::thread::sleep(Duration::from_millis(300));
    log.push("Down");

    for _ in 0..MAX_PRESSES {
        let c = cursor();
        if dist(c, target) <= ON_TARGET_PX {
            return Ok((log, true));
        }
        let dx = target.0 - c.0;
        let dy = target.1 - c.1;
        let (vk, sc, name) = if dx.abs() >= dy.abs() {
            if dx > 0 { (VK_RIGHT, SC_RIGHT, "Right") } else { (VK_LEFT, SC_LEFT, "Left") }
        } else if dy > 0 {
            (VK_DOWN, SC_DOWN, "Down")
        } else {
            (VK_UP, SC_UP, "Up")
        };
        s.input.press_extended_key(vk, sc)?;
        std::thread::sleep(Duration::from_millis(300));
        log.push(name);
        // A press that does not move the cursor means no hotspot that way; try the
        // perpendicular axis next time by nudging.
        if cursor() == c {
            let (vk2, sc2, n2) = if dx.abs() >= dy.abs() {
                (VK_DOWN, SC_DOWN, "Down")
            } else {
                (VK_RIGHT, SC_RIGHT, "Right")
            };
            s.input.press_extended_key(vk2, sc2)?;
            std::thread::sleep(Duration::from_millis(300));
            log.push(n2);
        }
    }
    Ok((log, dist(cursor(), target) <= ON_TARGET_PX))
}

fn press_return_and_measure(
    s: &Session, floor: f64, tag: &str,
) -> Result<(f64, f64, bool), Box<dyn std::error::Error>> {
    let before = capture_window(&s.win)?;
    before.write_bmp(Path::new(&format!("spike-frames-4/{tag}-before.bmp")))?;
    s.input.press_extended_key(VK_RETURN, SC_RETURN)?;
    std::thread::sleep(SETTLE);
    let after = capture_window(&s.win)?;
    after.write_bmp(Path::new(&format!("spike-frames-4/{tag}-after.bmp")))?;
    let delta = before.diff_fraction(&after, FULL);
    let threshold = (floor * 4.0).max(0.02);
    Ok((delta, threshold, reacted(&before, &after, floor)))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).unwrap_or_else(|| "config.toml".into());
    let cfg = Config::load(Path::new(&path))?;
    std::fs::create_dir_all("spike-frames-4")?;
    let mut out = String::new();

    // ---------- PART 1: prove the highlight lands on Start, by measurement ----------
    println!("=== PART 1: does the highlight visibly land on Start? ===");
    let (s, floor) = start_menu(&cfg)?;
    let (cw, ch) = s.win.client_size()?;
    let start_rect = rect(375, 760, 625, 860, cw, ch);
    let patreon_rect = rect(64, 0, 164, 88, cw, ch);

    let pre = capture_window(&s.win)?;
    pre.write_bmp(Path::new("spike-frames-4/p1-before-nav.bmp"))?;
    let (nav, on) = navigate_to(&s, (500, 810))?;
    let post = capture_window(&s.win)?;
    post.write_bmp(Path::new("spike-frames-4/p1-after-nav.bmp"))?;

    let d_start = pre.diff_fraction(&post, start_rect);
    let d_patreon = pre.diff_fraction(&post, patreon_rect);
    let c = cursor();
    out.push_str(&format!(
        "PART 1\n  nav={nav:?} on_target={on} cursor={c:?} (Start centre 500,810)\n  \
         Start-rect diff   = {d_start:.4}  (expect CHANGE: hover art appears)\n  \
         Patreon-rect diff = {d_patreon:.4}  (expect ~0: untouched)\n  floor={floor:.4}\n"
    ));
    println!("{out}");
    let mut s = s;
    let _ = s.game.kill();

    // ---------- PART 2: which buttons does Return activate? ----------
    // Discord: ss(0,0) small 100x100 xOffset 2.63 yOffset 0.38 -> (263,38). Positive
    //   control for the "mousereleased -> ui.confirm" class that is known to work.
    // Arcade: ss(1,0.75) default 250x100 xOffset -2 -> (1420,810). Goes STRAIGHT to
    //   setActiveMode with no mode.load() -> the discriminating case.
    // Start:  (500,810). Known negative; re-confirm under identical conditions.
    let targets: [(&str, (i32, i32)); 3] = [
        ("discord", (263, 38)),
        ("arcade", (1420, 810)),
        ("start", (500, 810)),
    ];
    out.push_str("\nPART 2\n");
    for (tag, target) in targets {
        println!("=== PART 2: {tag} target {target:?} ===");
        let (s2, floor2) = start_menu(&cfg)?;
        let (nav, on) = navigate_to(&s2, target)?;
        let landed = cursor();
        let line = if !on {
            format!(
                "  {tag:8} target={target:?} landed={landed:?} nav={nav:?} \
                 NOT ON TARGET - Return not pressed\n"
            )
        } else if tag == "start" {
            // Start is the known failure. Watch for 12s rather than 1.2s, in case the
            // unlock-screen chain (heroselect.load -> unlockedCheck -> modesequence)
            // simply takes longer than one settle to appear.
            let before = capture_window(&s2.win)?;
            before.write_bmp(Path::new("spike-frames-4/start-before.bmp"))?;
            s2.input.press_extended_key(VK_RETURN, SC_RETURN)?;
            let mut trace = String::new();
            let mut best = 0.0f64;
            for sec in 1..=12 {
                std::thread::sleep(Duration::from_secs(1));
                let f = capture_window(&s2.win)?;
                let d = before.diff_fraction(&f, FULL);
                if d > best {
                    best = d;
                }
                trace.push_str(&format!("{sec}s={d:.3} "));
                f.write_bmp(Path::new(&format!("spike-frames-4/start-t{sec:02}.bmp")))?;
            }
            format!(
                "  {tag:8} target={target:?} landed={landed:?} nav={nav:?} floor={floor2:.4}\n    \
                 12s watch: {trace}\n    max delta over 12s = {best:.4}\n"
            )
        } else {
            let (delta, threshold, react) = press_return_and_measure(&s2, floor2, tag)?;
            // Clamp: floor*4 can exceed 1.0, which no diff can ever clear. That is what
            // made discord's genuine 0.9997 activation read as reacted=false.
            let usable = threshold.min(0.5);
            format!(
                "  {tag:8} target={target:?} landed={landed:?} nav={nav:?} \
                 floor={floor2:.4} delta={delta:.4} threshold={threshold:.4} \
                 clamped={usable:.4} reacted={react} reacted_clamped={}\n",
                delta > usable
            )
        };
        print!("{line}");
        out.push_str(&line);
        let mut s2 = s2;
        let _ = s2.game.kill();
        std::thread::sleep(Duration::from_secs(2));
    }

    std::fs::create_dir_all("docs/superpowers/spikes")?;
    std::fs::File::create("docs/superpowers/spikes/05-button-probe.md")?
        .write_all(out.as_bytes())?;
    println!("\n===== SUMMARY =====\n{out}");
    Ok(())
}
