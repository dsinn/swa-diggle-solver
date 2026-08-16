//! Start a fight from the overworld — the one link in the chain never tested.
//!
//! Everything else exists: travel arrives at a node, and `spike_combat` clears a dungeon and takes
//! the reward. What has never run is the step between them. Every combat run so far **resumed** an
//! existing `combatSaveData`; none has begun one.
//!
//! Three buttons in a row, each guarded:
//!
//! 1. **Combat.** `basicCombatZoneButtons` (`overworldview.lua:414`) is a single button, and
//!    `insertAreaButtons` gives a lone button `affirmative` (`overworld.lua:1507-1509`) — so Space
//!    starts the fight. It shares that slot with Travel, which is why the buttons have to be the
//!    *current location's* before Space is sent. "Locate me" does exactly that:
//!    `core.refreshAreaButtons(location)` then `centreScreenOnPlayer()` (`:488-491`).
//!    `activeIf = core.currentAreaIsNotcomplete`, so a finished crypt will not restart.
//! 2. **Start**, on the pregame screen, which announces itself as `Pregame screen:`
//!    (`ui/pregame.lua:91`).
//! 3. Nothing else — this spike stops the moment a fight is under way and leaves the clearing to
//!    `spike_combat`, which already does it.
//!
//! **Drives the real mouse and sends Space. Starts a fight, deliberately.**

use diggle_solver::config::Config;
use diggle_solver::observe::adjacency;
use diggle_solver::observe::log::{Console, LogMirror};
use diggle_solver::overworld::WorldMap;
use diggle_solver::win::input::Input;
use diggle_solver::win::window::GameWindow;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

const REPORT: &str = "spike-enter-combat.md";
const AREA_BUTTONS: diggle_solver::win::capture::Region =
    diggle_solver::win::capture::Region { nx: 0.0, ny: 0.68, nw: 0.45, nh: 0.18 };
const LOCATE_ME: (i32, i32) = (32, 918);
/// `Combat`, from `basicCombatZoneButtons` (`overworldview.lua:415`): a default 250x100 button at
/// screen-space (0, 0.85) with xOffset 0.75, so (187, 918) at 1920x1080. The same slot `Travel`
/// occupies — which is exactly why the area buttons must be the current location's before it is hit.
const COMBAT_BUTTON: (i32, i32) = (187, 918);
const EMPTY_MAP: (i32, i32) = (1750, 160);
const NEUTRAL: (i32, i32) = (300, 300);

fn park(win: &GameWindow) {
    if let Ok((x, y)) = win.client_to_screen(NEUTRAL.0, NEUTRAL.1) {
        let _ = diggle_solver::win::input::warp_cursor(x, y);
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config::load(Path::new("config.toml"))?;
    diggle_solver::win::process::refuse_if_running("lovec.exe", &[])?;
    let save_dir = diggle_solver::game::savedir::locate(cfg.save_dir.clone(), true)?;
    let combat_path = save_dir.join("combatSaveData");

    let mut log = String::from("# Spike: start a fight from the overworld\n\n");
    let mut console = Console::take()?;
    let mut mirror = LogMirror::create(Path::new("spike-enter-combat-raw.log"))?;
    let mut game = diggle_solver::game::launch::GameProcess::launch(&cfg, &console)?;
    let win = game.wait_for_window(Duration::from_secs(20))?;
    std::thread::sleep(Duration::from_secs(3));

    let mut lines: Vec<String> = Vec::new();
    let mut reader = adjacency::Reader::new();
    let mut map = WorldMap::new();
    let mut latest = None;
    // **Read at three later points**, but this macro expands at several call sites and the last of
    // them has nothing after it — so that one expansion assigns a value no line reads, and the lint
    // reports the macro body rather than the site.
    macro_rules! pump {
        () => {
            if let Ok(new) = console.read_new() {
                if !new.is_empty() {
                    mirror.write(&new);
                    for a in reader.push(&new) {
                        map.fold(&a);
                        #[allow(unused_assignments)]
                        {
                            latest = Some(a);
                        }
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
        log.push_str("ABORT: no Continue\n");
        return finish(&mut game, &log);
    }

    let deadline = Instant::now() + Duration::from_secs(40);
    while Instant::now() < deadline && latest.is_none() {
        std::thread::sleep(Duration::from_millis(300));
        pump!();
    }
    if latest.is_none() {
        log.push_str("ABORT: no adjacency dump\n");
        return finish(&mut game, &log);
    }
    if let Ok(save) = diggle_solver::game::save::load(&save_dir.join("mainSaveData")) {
        map.apply_save(&save);
    }

    let here = map.here().unwrap_or("?").to_string();
    let Some(place) = map.get(&here).cloned() else {
        log.push_str("ABORT: no record of where we are\n");
        return finish(&mut game, &log);
    };
    log.push_str(&format!(
        "at **{}** ({}), level {:?}, completed {}\n\n",
        place.key,
        place.heading,
        place.level(),
        place.completed
    ));
    if !place.has_combat() || place.completed {
        log.push_str("ABORT: not standing on an unfinished combat node — nothing to start\n");
        return finish(&mut game, &log);
    }

    // Make the area buttons the CURRENT location's, so the lone `affirmative` is Combat and not
    // Travel. Locate-me does the refresh and the centring together.
    let (ex, ey) = win.client_to_screen(EMPTY_MAP.0, EMPTY_MAP.1)?;
    diggle_solver::win::input::click_at(ex, ey)?;
    std::thread::sleep(Duration::from_millis(600));
    let before = diggle_solver::win::capture::capture_window(&win)?;
    let (lx, ly) = win.client_to_screen(LOCATE_ME.0, LOCATE_ME.1)?;
    diggle_solver::win::input::click_at(lx, ly)?;
    park(&win);
    // Wait for the pan to FINISH, not for a guessed interval. Locate-me centres with
    // `centreScreenOnPlayer()` and no `instant` flag (`overworldview.lua:491`), so it animates, and
    // input during that animation goes nowhere. The first attempt slept 1200 ms and fired into a
    // moving screen; `spike_travel` worked because it waited for this dump and this one did not.
    // `verboseAdjacencyData('Screen pan finished')` (`:1255`) is the exact signal.
    let pan_deadline = Instant::now() + Duration::from_secs(12);
    let mut panned = false;
    while Instant::now() < pan_deadline && !panned {
        std::thread::sleep(Duration::from_millis(250));
        pump!();
        panned = latest.as_ref().map(|a| a.reason.contains("pan")).unwrap_or(false);
    }
    log.push_str(&format!("pan finished: **{panned}**\n"));
    let after = diggle_solver::win::capture::capture_window(&win)?;
    let moved = before.diff_fraction(&after, AREA_BUTTONS);
    log.push_str(&format!("locate-me: area strip moved {moved:.4}\n"));
    let _ = after.write_png(Path::new("spike-frames-live/enter-combat-buttons.png"));
    if moved <= 0.01 {
        log.push_str("ABORT: the area buttons never appeared, so Space has no known meaning\n");
        return finish(&mut game, &log);
    }

    // Click Combat rather than pressing Space.
    //
    // Space *should* reach it — a crypt is `basicCombatZone = true`
    // (`overworld/locations/crypt.lua:12`) so it uses `basicCombatZoneButtons`, which holds exactly
    // one button, and `insertAreaButtons` gives a lone button `affirmative`
    // (`overworld.lua:1507-1509`). Clicking is preferred anyway: it does not depend on that rule
    // holding for every location type, and a click that lands is provable from the screen. Position
    // comes from the same declaration Travel uses, `('Combat', 0, 0.85, {xOffset = 0.75})` on a
    // default 250x100 button, so x = 250*0.75 and y = 1080*0.85.
    let (cx, cy) = win.client_to_screen(COMBAT_BUTTON.0, COMBAT_BUTTON.1)?;
    diggle_solver::win::input::click_at(cx, cy)?;
    park(&win);
    log.push_str(&format!("clicked Combat at {COMBAT_BUTTON:?}\n"));
    let keys = diggle_solver::win::input::PostMessageInput::new(win);

    // The pregame announces itself; Space there is `Start` and is already verified safe.
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut pregame = false;
    while Instant::now() < deadline && !pregame {
        std::thread::sleep(Duration::from_millis(300));
        pump!();
        pregame = lines.iter().any(|l| l.contains("Pregame screen:"));
    }
    log.push_str(&format!("reached the pregame: **{pregame}**\n"));
    if !pregame {
        log.push_str("ABORT: Combat did not open the pregame\n");
        return finish(&mut game, &log);
    }

    keys.focus();
    std::thread::sleep(Duration::from_millis(400));
    keys.press_key(
        diggle_solver::win::input::VK_SPACE,
        diggle_solver::win::input::SC_SPACE,
    )?;
    log.push_str("sent Space (Start)\n");

    // A fight is under way once the game writes a combat save. That file is the handover to
    // `spike_combat`, which resumes exactly this state.
    let deadline = Instant::now() + Duration::from_secs(40);
    let mut started = false;
    while Instant::now() < deadline && !started {
        std::thread::sleep(Duration::from_millis(400));
        pump!();
        started = combat_path.is_file();
    }
    log.push_str(&format!("\n## Result\n\ncombat under way: **{started}**\n"));
    if started {
        if let Ok(t) = diggle_solver::game::save::load(&combat_path) {
            log.push_str(&format!(
                "turnState `{}`, enemy health {:?}\n\nHand over to `spike_combat` to clear it.\n",
                t.str_at("rpg.player.turnState").unwrap_or("?"),
                t.int_at("rpg.enemy.health")
            ));
        }
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
