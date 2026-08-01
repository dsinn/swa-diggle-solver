//! Travel repeatedly: fold the map, choose a hop, take it, handle what answers, repeat.
//!
//! The single-hop version proved the mechanics ([`spike_travel`]). This repeats them, which is the
//! only way to reach several things that cannot be summoned on demand:
//!
//! - **Arrival events.** Beggars and highwaymen are random, so `observe::event` has never seen real
//!   output. Travelling until one appears is the only test there is.
//! - **A level > 3 node**, which is what opens the anomaly. None is visible from the start.
//! - **The rest policy**, which only engages once something has actually hurt us.
//!
//! ## Why it stops at a combat node
//!
//! `canTravelToDirect` (`overworldview.lua:1316-1321`) requires one of the two endpoints to be
//! complete:
//!
//! ```lua
//! and (core.areaOrExitToComplete(location1.key, location2.key)
//!      or core.areaOrExitToComplete(location2.key, location1.key))
//! ```
//!
//! So from an incomplete combat node the only legal move is back the way we came. A combat node
//! cannot be walked past — it has to be fought — and fighting is a separate proven capability that
//! this loop deliberately does not invoke. Reaching one is a **successful** outcome here, and the
//! point at which the two halves need joining.
//!
//! Restore with `checkpoint restore pre-anomaly` to replay from a known island.
//!
//! **Drives the real mouse and sends Space.** Never presses Escape, never submits a word, and never
//! starts a fight.

use diggle_solver::config::Config;
use diggle_solver::observe::adjacency::{self, Adjacency};
use diggle_solver::observe::event;
use diggle_solver::observe::log::{Console, LogMirror};
use diggle_solver::overworld::WorldMap;
use diggle_solver::win::input::Input;
use diggle_solver::win::window::GameWindow;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const REPORT: &str = "spike-travel-loop.md";
const MAX_HOPS: usize = 8;
/// See `spike_travel`: the strip where area buttons and the location name appear.
const AREA_BUTTONS: diggle_solver::win::capture::Region =
    diggle_solver::win::capture::Region { nx: 0.0, ny: 0.68, nw: 0.45, nh: 0.18 };
const LOCATE_ME: (i32, i32) = (32, 918);
const EMPTY_MAP: (i32, i32) = (1750, 160);
const NEUTRAL: (i32, i32) = (300, 300);

/// Why the loop stopped. Several of these are successes.
#[derive(Debug)]
enum Stop {
    /// Standing at a combat node, which cannot be passed without fighting.
    CombatNode(String),
    /// An event offered a real decision and this spike has no policy for it.
    UndecidedEvent(String),
    NoPlan,
    HopFailed(String),
    Exhausted,
}

struct Session {
    console: Console,
    mirror: LogMirror,
    lines: Vec<String>,
    reader: adjacency::Reader,
    map: WorldMap,
    latest: Option<Adjacency>,
    save_dir: PathBuf,
    log: String,
}

impl Session {
    fn pump(&mut self) {
        if let Ok(new) = self.console.read_new() {
            if !new.is_empty() {
                self.mirror.write(&new);
                for a in self.reader.push(&new) {
                    self.map.fold(&a);
                    self.latest = Some(a);
                }
                self.lines.extend(new);
            }
        }
    }

    /// Health, gold, fuel and completion, which decide whether we need a rest and where.
    fn apply_save(&mut self) -> Option<diggle_solver::rest::Health> {
        let save = diggle_solver::game::save::load(&self.save_dir.join("mainSaveData")).ok()?;
        self.map.apply_save(&save);
        diggle_solver::rest::Health::from_save(&save)
    }

    /// Forces a dump whose coordinates are current.
    ///
    /// The world-load dump is printed before the view settles, and where centring is `instant` no
    /// pan ever finishes, so no corrected dump follows. Clicking empty map raises "locate me"
    /// (`overworldview.lua:1484-1487`), which centres *without* `instant` (`:491`) — an animated pan,
    /// and a finished pan prints (`:1255`).
    fn recentre(&mut self, win: &GameWindow) -> Option<Adjacency> {
        let (ex, ey) = win.client_to_screen(EMPTY_MAP.0, EMPTY_MAP.1).ok()?;
        let _ = diggle_solver::win::input::click_at(ex, ey);
        std::thread::sleep(Duration::from_millis(500));
        self.pump();
        let (lx, ly) = win.client_to_screen(LOCATE_ME.0, LOCATE_ME.1).ok()?;
        let _ = diggle_solver::win::input::click_at(lx, ly);
        park(win);
        let deadline = Instant::now() + Duration::from_secs(12);
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(250));
            self.pump();
            if let Some(a) = self.latest.as_ref() {
                if a.reason.contains("pan") {
                    return Some(a.clone());
                }
            }
        }
        None
    }
}

fn park(win: &GameWindow) {
    if let Ok((x, y)) = win.client_to_screen(NEUTRAL.0, NEUTRAL.1) {
        let _ = diggle_solver::win::input::warp_cursor(x, y);
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config::load(Path::new("config.toml"))?;
    diggle_solver::win::process::refuse_if_running("lovec.exe", &[])?;
    let save_dir = diggle_solver::game::savedir::locate(cfg.save_dir.clone(), true)?;

    let console = Console::take()?;
    let mirror = LogMirror::create(Path::new("spike-travel-loop-raw.log"))?;
    let mut game = diggle_solver::game::launch::GameProcess::launch(&cfg, &console)?;
    let win = game.wait_for_window(Duration::from_secs(20))?;
    std::thread::sleep(Duration::from_secs(3));

    let mut s = Session {
        console,
        mirror,
        lines: Vec::new(),
        reader: adjacency::Reader::new(),
        map: WorldMap::new(),
        latest: None,
        save_dir,
        log: String::from("# Spike: multi-hop travel\n\n"),
    };

    if diggle_solver::act::click_when_ready(
        &win,
        &diggle_solver::act::CONTINUE,
        Duration::from_secs(30),
    )
    .is_err()
    {
        s.log.push_str("ABORT: no Continue\n");
        return finish(&mut game, &s.log);
    }

    let deadline = Instant::now() + Duration::from_secs(40);
    while Instant::now() < deadline && s.latest.is_none() {
        std::thread::sleep(Duration::from_millis(300));
        s.pump();
    }
    if s.latest.is_none() {
        s.log.push_str("ABORT: no adjacency dump after the world loaded\n");
        return finish(&mut game, &s.log);
    }
    let mut health = s.apply_save();
    s.log.push_str(&format!(
        "start at **{}**, health {:?}\n\n| # | from | step | goal | why | result |\n|---|---|---|---|---|---|\n",
        s.map.here().unwrap_or("?"),
        health.map(|h| format!("{}/{}", h.current, h.max))
    ));

    let stop = run(&mut s, &win, &mut health);
    s.log.push_str(&format!("\n## Stopped\n\n{stop:?}\n\n"));
    s.log.push_str(&format!(
        "{} places known; here **{}**; wants rest: {}\n",
        s.map.len(),
        s.map.here().unwrap_or("?"),
        s.map.wants_rest()
    ));
    let known: Vec<String> = s
        .map
        .places()
        .map(|p| {
            format!(
                "- `{}` {}{}{}",
                p.key,
                if p.heading.is_empty() { "(unheaded)" } else { &p.heading },
                if p.completed { " [done]" } else { "" },
                if p.visited { " [visited]" } else { "" }
            )
        })
        .collect();
    s.log.push_str(&known.join("\n"));

    let out = s.log.clone();
    finish(&mut game, &out)
}

fn run(
    s: &mut Session, win: &GameWindow, health: &mut Option<diggle_solver::rest::Health>,
) -> Stop {
    for hop_no in 1..=MAX_HOPS {
        // A combat node cannot be left except back the way we came, so stop and hand over.
        if let Some(here) = s.map.here().map(|h| h.to_string()) {
            if let Some(p) = s.map.get(&here) {
                if p.has_combat() && !p.completed {
                    return Stop::CombatNode(format!("{} ({})", p.key, p.heading));
                }
            }
        }

        let Some(fresh) = s.recentre(win) else {
            return Stop::HopFailed("no pan dump after locate-me".into());
        };
        let Some(hop) = s.map.next_hop() else { return Stop::NoPlan };
        let from = s.map.here().unwrap_or("?").to_string();
        let Some(target) = fresh.nodes.iter().find(|n| n.key == hop.step).cloned() else {
            s.log.push_str(&format!(
                "| {hop_no} | {from} | {} | {} | {:?} | not on screen |\n",
                hop.step, hop.plan.target, hop.plan.reason
            ));
            return Stop::HopFailed(format!("{} is not among the visible nodes", hop.step));
        };

        // Select, and prove it: a lone area button becomes `affirmative`, and at a combat node that
        // lone button is **Combat**. Space without a confirmed selection starts a fight.
        let before = match diggle_solver::win::capture::capture_window(win) {
            Ok(f) => f,
            Err(e) => return Stop::HopFailed(e.to_string()),
        };
        let (sx, sy) = match win.client_to_screen(target.x as i32, target.y as i32) {
            Ok(p) => p,
            Err(e) => return Stop::HopFailed(e.to_string()),
        };
        let _ = diggle_solver::win::input::click_at(sx, sy);
        std::thread::sleep(Duration::from_millis(900));
        s.pump();
        let after = match diggle_solver::win::capture::capture_window(win) {
            Ok(f) => f,
            Err(e) => return Stop::HopFailed(e.to_string()),
        };
        if before.diff_fraction(&after, AREA_BUTTONS) <= 0.01 {
            s.log.push_str(&format!(
                "| {hop_no} | {from} | {} | {} | {:?} | selection did not take |\n",
                hop.step, hop.plan.target, hop.plan.reason
            ));
            return Stop::HopFailed(format!("selecting {} did not register", hop.step));
        }

        let keys = diggle_solver::win::input::PostMessageInput::new(*win);
        keys.focus();
        std::thread::sleep(Duration::from_millis(200));
        if keys
            .press_key(diggle_solver::win::input::VK_SPACE, diggle_solver::win::input::SC_SPACE)
            .is_err()
        {
            return Stop::HopFailed("could not send Space".into());
        }
        park(win);

        // Wait for the move, or for whatever intercepted it.
        let deadline = Instant::now() + Duration::from_secs(45);
        let mut arrived = false;
        let mut note = String::new();
        while Instant::now() < deadline && !arrived {
            std::thread::sleep(Duration::from_millis(300));
            let mark = s.lines.len().saturating_sub(60);
            s.pump();
            if let Some(ev) = event::parse_events(&s.lines[mark..]).pop() {
                // Never `choices[0]`: a corrupted village can put "Kill him" first.
        let pick = ev.continue_choice().or_else(|| ev.safe_choice()).cloned();
                match pick {
                    Some(c) => {
                        if let Ok((cx, cy)) = win.client_to_screen(c.x, c.y) {
                            let _ = diggle_solver::win::input::click_at(cx, cy);
                            park(win);
                        }
                        note = format!("event `{}` -> `{}`", ev.title, c.text);
                    }
                    None => {
                        s.log.push_str(&format!(
                            "| {hop_no} | {from} | {} | {} | {:?} | event `{}` |\n",
                            hop.step, hop.plan.target, hop.plan.reason, ev.title
                        ));
                        return Stop::UndecidedEvent(format!(
                            "{}: {:?}",
                            ev.title,
                            ev.choices.iter().map(|c| c.text.clone()).collect::<Vec<_>>()
                        ));
                    }
                }
            }
            if s.map.here().map(|h| h != from).unwrap_or(false) {
                arrived = true;
            }
        }

        if !arrived {
            s.log.push_str(&format!(
                "| {hop_no} | {from} | {} | {} | {:?} | did not arrive |\n",
                hop.step, hop.plan.target, hop.plan.reason
            ));
            return Stop::HopFailed(format!("no arrival at {}", hop.step));
        }

        // Health decides whether the next plan is a detour to rest.
        let now = s.apply_save();
        if let (Some(b), Some(a)) = (*health, now) {
            s.map.note_health(b, a);
            s.map.rested(a);
        }
        *health = now;
        s.log.push_str(&format!(
            "| {hop_no} | {from} | {} | {} | {:?} | arrived{}{} |\n",
            hop.step,
            hop.plan.target,
            hop.plan.reason,
            now.map(|h| format!(", {}/{} hp", h.current, h.max)).unwrap_or_default(),
            if note.is_empty() { String::new() } else { format!(", {note}") }
        ));
    }
    Stop::Exhausted
}

fn finish(
    game: &mut diggle_solver::game::launch::GameProcess, log: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    game.close(Duration::from_secs(15));
    std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
    println!("{log}");
    Ok(())
}
