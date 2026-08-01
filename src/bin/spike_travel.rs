//! One travel step: read the map, choose a hop, take it, and handle whatever answers.
//!
//! This is the first live exercise of [`diggle_solver::overworld`] and
//! [`diggle_solver::observe::event`]. Everything it does was derived from source and has never been
//! run, so the point is as much to find out where that derivation is wrong as to travel.
//!
//! The sequence, and why each step is where it is:
//!
//! 1. Fold the `Local overworld data:` dump the world-load prints (`overworldview.lua:1607`).
//! 2. Apply `mainSaveData` for `completedAreas` and the `hell` flag.
//! 3. Ask the map for a hop. Always one hop — see [`diggle_solver::subworld`].
//! 4. Click the neighbour. `core:mousereleased` (`:1470-1483`) selects it and raises the Travel
//!    buttons; it does **not** travel.
//! 5. Confirm the selection took, by watching the strip where the area buttons and the location
//!    name appear. Nothing is printed, so a frame diff is the only instrument.
//! 6. Press Space. `travelToLocationButtons` holds exactly one button, and `insertAreaButtons`
//!    gives a lone button `affirmative` (`overworld.lua:1507-1509`), so Space *is* Travel — with
//!    the game's own `activeIf = canTravelToIndirect` still deciding whether the move is legal.
//! 7. Wait for either an arrival dump or an event screen. An arrival can raise a beggar, a
//!    highwayman, or the anomaly itself, and the overworld does not come back until it is answered.
//!
//! Step 5 is not a nicety. `basicCombatZoneButtons` (`overworldview.lua:414`) puts a single
//! **Combat** button in that same slot when standing at a combat node, and it becomes `affirmative`
//! by the same rule — so Space without a confirmed selection starts a fight instead of travelling.
//!
//! **Drives the real mouse and sends Space. Never presses Escape and never submits a word.**
//!
//! Restore `checkpoint restore pre-anomaly` first if a previous run moved the world.

use diggle_solver::config::Config;
use diggle_solver::observe::adjacency;
use diggle_solver::observe::event;
use diggle_solver::observe::log::{Console, LogMirror};
use diggle_solver::overworld::WorldMap;
use diggle_solver::win::input::Input;
use diggle_solver::win::window::GameWindow;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

const REPORT: &str = "spike-travel.md";
/// The strip holding the area buttons and the selected location's name (`overworld.lua:1514` puts
/// the title at 0.05, 0.75). Watched to prove a click actually selected something.
const AREA_BUTTONS: diggle_solver::win::capture::Region =
    diggle_solver::win::capture::Region { nx: 0.0, ny: 0.68, nw: 0.45, nh: 0.18 };
/// Somewhere with no hotspot, so hover is never in a reading.
const NEUTRAL: (i32, i32) = (300, 300);
/// "Locate me" (`showAreaButtonsButton`, `overworldview.lua:483-494`): a `right` button, 64x80, at
/// screen-space (0, 0.85) with an xOffset of 0.5 — so x = 64*0.5 = 32, y = 1080*0.85 = 918. It sits
/// at a different x from the Travel/Combat slot (187) and the two are never shown together.
const LOCATE_ME: (i32, i32) = (32, 918);
/// Map with no node on it, clicked to clear the area buttons so "locate me" appears. Top-right,
/// away from both the node cluster and the bottom-left button bar.
const EMPTY_MAP: (i32, i32) = (1750, 160);

fn park(win: &GameWindow) {
    if let Ok((x, y)) = win.client_to_screen(NEUTRAL.0, NEUTRAL.1) {
        let _ = diggle_solver::win::input::warp_cursor(x, y);
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config::load(Path::new("config.toml"))?;
    diggle_solver::win::process::refuse_if_running("lovec.exe", &[])?;
    let save_dir = diggle_solver::game::savedir::locate(cfg.save_dir.clone(), true)?;

    let mut log = String::from("# Spike: one travel step\n\n");
    let mut console = Console::take()?;
    let mut mirror = LogMirror::create(Path::new("spike-travel-raw.log"))?;
    let mut game = diggle_solver::game::launch::GameProcess::launch(&cfg, &console)?;
    let win = game.wait_for_window(Duration::from_secs(20))?;
    std::thread::sleep(Duration::from_secs(3));

    let mut lines: Vec<String> = Vec::new();
    let mut reader = adjacency::Reader::new();
    let mut map = WorldMap::new();
    let mut latest: Option<adjacency::Adjacency> = None;

    macro_rules! pump {
        () => {
            if let Ok(new) = console.read_new() {
                if !new.is_empty() {
                    mirror.write(&new);
                    for a in reader.push(&new) {
                        map.fold(&a);
                        latest = Some(a);
                    }
                    lines.extend(new);
                }
            }
        };
    }

    if diggle_solver::act::click_when_ready(
        &win,
        &diggle_solver::act::CONTINUE,
        Duration::from_secs(30),
    )
    .is_err()
    {
        log.push_str("ABORT: no Continue on the main menu\n");
        return finish(&mut game, &log);
    }
    log.push_str("clicked Continue\n\n");

    // The world-load dump is the map's first and only free gift; everything after it has to be
    // earned by moving.
    let deadline = Instant::now() + Duration::from_secs(40);
    while Instant::now() < deadline && latest.is_none() {
        std::thread::sleep(Duration::from_millis(300));
        pump!();
    }
    let Some(first) = latest.clone() else {
        log.push_str("ABORT: no adjacency dump after the world loaded\n");
        return finish(&mut game, &log);
    };
    if let Ok(save) = diggle_solver::game::save::load(&save_dir.join("mainSaveData")) {
        map.apply_save(&save);
    }
    log.push_str(&format!(
        "## Start\n\nat **{}** ({}), reason `{}`, {} places known, anomaly available: {:?}\n\n",
        first.here_key,
        first.here_heading,
        first.reason,
        map.len(),
        map.anomaly_available()
    ));
    for n in &first.nodes {
        log.push_str(&format!(
            "- `{}` {} at ({:.0},{:.0}), level {:?}\n",
            n.key,
            n.heading,
            n.x,
            n.y,
            n.level()
        ));
    }
    if first.hidden > 0 {
        log.push_str(&format!("- plus **{} hidden**\n", first.hidden));
    }

    let Some(hop) = map.next_hop() else {
        log.push_str("\nno hop proposed — nothing left worth travelling to\n");
        return finish(&mut game, &log);
    };
    log.push_str(&format!(
        "\n## Plan\n\nstep to **{}**, heading for **{}** ({:?})\n\n",
        hop.step, hop.plan.target, hop.plan.reason
    ));

    // --- get coordinates we can actually trust ---
    //
    // The world-load dump is printed BEFORE the view finishes settling: in it, `start` sits at
    // (960,534) — dead centre — even though the player is at l1. Centring on the player happens
    // afterwards, and where it is `instant` no pan ever finishes, so no second dump is printed.
    // Clicking those stale coordinates is how the previous run selected the wrong node and then sat
    // on an inactive `Combat` button.
    //
    // The fix is to make the view move on our terms. Clicking empty map takes the `else` branch of
    // `core:mousereleased` (`overworldview.lua:1484-1487`), which clears the area buttons and
    // inserts `showAreaButtonsButton` — "locate me". That button centres with
    // `centreScreenOnPlayer()` and **no** `instant` flag (`:491`), so it animates, and a finished
    // animation fires `verboseAdjacencyData('Screen pan finished')` (`:1255`). That dump is current
    // by construction.
    let mut fresh = first.clone();
    {
        let (ex, ey) = win.client_to_screen(EMPTY_MAP.0, EMPTY_MAP.1)?;
        diggle_solver::win::input::click_at(ex, ey)?;
        std::thread::sleep(Duration::from_millis(600));
        pump!();
        let (lx, ly) = win.client_to_screen(LOCATE_ME.0, LOCATE_ME.1)?;
        diggle_solver::win::input::click_at(lx, ly)?;
        park(&win);
        let deadline = Instant::now() + Duration::from_secs(12);
        let mut panned = None;
        while Instant::now() < deadline && panned.is_none() {
            std::thread::sleep(Duration::from_millis(250));
            pump!();
            if let Some(a) = latest.as_ref() {
                if a.reason.contains("pan") {
                    panned = Some(a.clone());
                }
            }
        }
        match panned {
            Some(a) => {
                log.push_str(&format!(
                    "re-centred: fresh dump `{}` at **{}**, coordinates now current\n\n",
                    a.reason, a.here_key
                ));
                fresh = a;
            }
            None => log.push_str(
                "**no pan dump after locate-me** — falling back to the world-load coordinates, \
                 which may be stale\n\n",
            ),
        }
    }

    // Coordinates are only valid for the dump that printed them.
    let Some(target) = fresh.nodes.iter().find(|n| n.key == hop.step) else {
        log.push_str("ABORT: the hop is not among the nodes on screen — cannot aim at it\n");
        return finish(&mut game, &log);
    };
    let (sx, sy) = win.client_to_screen(target.x as i32, target.y as i32)?;

    // --- select, and PROVE it took ---
    //
    // Selecting raises the area-button bar and writes the location's name at (0.05, 0.75)
    // (`overworld.lua:1514`). Nothing is printed, so a frame diff over that strip is the only
    // instrument available — and it is load-bearing rather than a nicety, because of what comes
    // next.
    let floor = diggle_solver::observe::settle::sample_noise_floor(
        &win,
        6,
        Duration::from_millis(150),
    )?;
    park(&win);
    std::thread::sleep(Duration::from_millis(300));
    let before = diggle_solver::win::capture::capture_window(&win)?;
    diggle_solver::win::input::click_at(sx, sy)?;
    // Deliberately no park here: `insertAreaButtons` ends with `snapToNearestHotspot`
    // (`overworld.lua:1515`), so the game moves the cursor itself and fighting it would only race.
    std::thread::sleep(Duration::from_millis(900));
    pump!();
    let after = diggle_solver::win::capture::capture_window(&win)?;
    let moved = before.diff_fraction(&after, AREA_BUTTONS);
    let selected = moved > (floor * 4.0).max(0.01);
    log.push_str(&format!(
        "clicked `{}` at ({:.0},{:.0}); area-button strip moved {moved:.4} (noise floor {floor:.4}) \
         -> selected: **{selected}**\n",
        target.key, target.x, target.y
    ));
    let _ = after.write_png(Path::new("spike-frames-live/travel-selected.png"));

    if !selected {
        log.push_str(
            "\nABORT: the selection did not register, so Space is not safe to send.\n\n\
             `basicCombatZoneButtons` (`overworldview.lua:414`) puts a single **Combat** button in \
             the same slot when standing at a combat node, and a lone area button is given \
             `affirmative` (`overworld.lua:1508`). Pressing Space without a confirmed selection \
             would therefore start a fight instead of travelling.\n",
        );
        return finish(&mut game, &log);
    }

    // --- travel ---
    //
    // `travelToLocationButtons` holds exactly one button, and `insertAreaButtons` gives a lone
    // button `affirmative` (`overworld.lua:1507-1509`). So Space *is* Travel here, with the game's
    // own `activeIf = canTravelToIndirect` still deciding whether the move is legal. That beats
    // aiming at the button: no coordinates to get wrong.
    log.push_str("selection confirmed -> Space (affirmative = Travel)\n");
    let keys = diggle_solver::win::input::PostMessageInput::new(win);
    keys.focus();
    std::thread::sleep(Duration::from_millis(200));
    keys.press_key(
        diggle_solver::win::input::VK_SPACE,
        diggle_solver::win::input::SC_SPACE,
    )?;

    // --- wait for whatever answers ---
    // An arrival dump means we moved. An event means something intercepted us and the overworld is
    // gone until it is answered.
    let before = map.here().map(|s| s.to_string());
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut handled_event = false;
    let mut arrived = false;
    while Instant::now() < deadline && !arrived {
        std::thread::sleep(Duration::from_millis(300));
        let mark = lines.len();
        pump!();

        if !handled_event {
            if let Some(ev) = event::parse_events(&lines[mark.saturating_sub(40)..]).pop() {
                log.push_str(&format!(
                    "\n## Event: {}\n\n{}\n\nchoices: {:?}\n",
                    ev.title,
                    ev.text,
                    ev.choices.iter().map(|c| &c.text).collect::<Vec<_>>()
                ));
                // Only act when there is no decision to make. A highwayman's pay-or-refuse is a
                // policy question this spike has no business answering.
                // Never `choices[0]`: "forced" is not the same as safe -- a lone option can be the
                // robbery itself, and a corrupted village can put "Kill him" first.
                let pick = ev.continue_choice().or_else(|| ev.safe_choice());
                match pick {
                    Some(c) => {
                        let (cx, cy) = win.client_to_screen(c.x, c.y)?;
                        diggle_solver::win::input::click_at(cx, cy)?;
                        park(&win);
                        log.push_str(&format!("  took `{}` at ({},{})\n", c.text, c.x, c.y));
                        handled_event = true;
                    }
                    None => {
                        log.push_str("  **left alone**: more than one real choice, no policy yet\n");
                        break;
                    }
                }
            }
        }

        if let Some(now) = map.here() {
            if Some(now.to_string()) != before {
                arrived = true;
            }
        }
    }

    log.push_str("\n## Result\n\n");
    match (arrived, latest.as_ref()) {
        (true, Some(a)) => {
            log.push_str(&format!(
                "**arrived at `{}`** ({}), reason `{}`\n\n{} places known after folding\n",
                a.here_key,
                a.here_heading,
                a.reason,
                map.len()
            ));
            if let Some(next) = map.next_hop() {
                log.push_str(&format!(
                    "next: step to `{}` heading for `{}` ({:?})\n",
                    next.step, next.plan.target, next.plan.reason
                ));
            }
        }
        _ => log.push_str(&format!(
            "**did not arrive** within the deadline (event handled: {handled_event})\n"
        )),
    }

    finish(&mut game, &log)
}

fn finish(
    game: &mut diggle_solver::game::launch::GameProcess, log: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    game.close(Duration::from_secs(15));
    std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
    println!("{log}");
    Ok(())
}
