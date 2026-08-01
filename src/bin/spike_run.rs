//! The run: head for the anomaly, fighting what blocks the way and taking shrines that are cheap.
//!
//! One process, one game launch, one map. Every earlier spike did a single step and closed the
//! game, which is why entering a fight and clearing it took two runs and why Diggle's accumulated
//! map died between them.
//!
//! Each iteration:
//!
//! 1. Re-centre, for coordinates that match what is on screen. The world-load dump predates the
//!    view settling, and where centring is `instant` no corrected dump ever follows.
//! 2. If we are standing on an unfinished combat node, fight it — that is the only way past
//!    (`canTravelToDirect` needs one endpoint complete, `overworldview.lua:1316-1321`).
//! 3. Otherwise take one hop toward whatever [`WorldMap::next_target`] wants, handling any arrival
//!    event on the way.
//!
//! Priority is the map's: a live anomaly, then health, then opening the anomaly, then an
//! unconsecrated shrine, then exploring. Corrupted shrines stop being destinations once the anomaly
//! is open, because they cost a fight we no longer need.
//!
//! ## Where it deliberately stops
//!
//! **Consecrating.** Reaching a shrine is routing; finishing one means `Visit`, then solving the
//! word, then `Consecrate` and `Pray`. The solver is built and tested but has never seen a shrine
//! screen, and reading that grid is a capability this run does not have. Arriving at one is
//! reported and stepped over rather than faked.
//!
//! **The anomaly fight itself.** Level 8, and losing it ends the run. Reaching it is the milestone
//! here.
//!
//! **Drives the real mouse and keyboard. Restore a checkpoint to rewind.**

use diggle_solver::config::Config;
use diggle_solver::fight::{Fight, Outcome};
use diggle_solver::observe::adjacency::{self, Adjacency};
use diggle_solver::observe::event;
use diggle_solver::observe::feed::Feed;
use diggle_solver::observe::log::{Console, LogMirror};
use diggle_solver::overworld::{Goal, WorldMap};
use diggle_solver::win::input::{click_at, warp_cursor, Input, PostMessageInput, SC_SPACE, VK_SPACE};
use diggle_solver::win::window::GameWindow;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const REPORT: &str = "spike-run.md";
const FRAMES: &str = "spike-frames-live";
const AREA_BUTTONS: diggle_solver::win::capture::Region =
    diggle_solver::win::capture::Region { nx: 0.0, ny: 0.68, nw: 0.45, nh: 0.18 };
const LOCATE_ME: (i32, i32) = (32, 918);
/// `Combat` / `Travel` / `Visit` all land here — which is why the area buttons must be known to be
/// the right ones before it is clicked.
const AREA_BUTTON: (i32, i32) = (187, 918);
const EMPTY_MAP: (i32, i32) = (1750, 160);
const NEUTRAL: (i32, i32) = (300, 300);
const MAX_STEPS: usize = 14;

#[derive(Debug)]
enum Stop {
    AnomalyReached,
    AnomalyBeaten,
    AtShrine(String),
    /// Standing on a subworld whose interior we cannot yet clear.
    AtSubworld(String),
    NoPlan,
    Failed(String),
    Fought(String),
    Exhausted,
}

struct Run<'a> {
    win: &'a GameWindow,
    keys: PostMessageInput,
    feed: Feed,
    reader: adjacency::Reader,
    map: WorldMap,
    latest: Option<Adjacency>,
    save_dir: PathBuf,
    log: String,
}

impl Run<'_> {
    fn pump(&mut self) {
        let new: Vec<String> = self.feed.pump().to_vec();
        for a in self.reader.push(&new) {
            self.map.fold(&a);
            self.latest = Some(a);
        }
    }

    fn park(&self) {
        if let Ok((x, y)) = self.win.client_to_screen(NEUTRAL.0, NEUTRAL.1) {
            let _ = warp_cursor(x, y);
        }
    }

    fn apply_save(&mut self) -> Option<diggle_solver::rest::Health> {
        let save = diggle_solver::game::save::load(&self.save_dir.join("mainSaveData")).ok()?;
        self.map.apply_save(&save);
        diggle_solver::rest::Health::from_save(&save)
    }

    /// Clicks empty map to raise "locate me", clicks it, and waits for the pan to FINISH.
    ///
    /// The waiting is the point. Locate-me centres without `instant` (`overworldview.lua:491`), so
    /// it animates, and input during the animation goes nowhere — that is what made a Space vanish
    /// against a visible Combat button. `Screen pan finished` (`:1255`) is the signal.
    fn recentre(&mut self) -> Option<Adjacency> {
        let (ex, ey) = self.win.client_to_screen(EMPTY_MAP.0, EMPTY_MAP.1).ok()?;
        let _ = click_at(ex, ey);
        std::thread::sleep(Duration::from_millis(500));
        self.pump();
        let (lx, ly) = self.win.client_to_screen(LOCATE_ME.0, LOCATE_ME.1).ok()?;
        let _ = click_at(lx, ly);
        self.park();
        let by = Instant::now() + Duration::from_secs(12);
        while Instant::now() < by {
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

    /// Clicks the lone area button, having first proved the strip is showing something.
    fn click_area_button(&mut self, what: &str) -> Result<bool, Box<dyn std::error::Error>> {
        let before = diggle_solver::win::capture::capture_window(self.win)?;
        let (bx, by) = self.win.client_to_screen(AREA_BUTTON.0, AREA_BUTTON.1)?;
        click_at(bx, by)?;
        self.park();
        std::thread::sleep(Duration::from_millis(1000));
        self.pump();
        let after = diggle_solver::win::capture::capture_window(self.win)?;
        let moved = before.diff_fraction(&after, diggle_solver::observe::settle::FULL);
        self.log.push_str(&format!("  clicked {what}: screen moved {moved:.3}\n"));
        Ok(moved > 0.05)
    }

    /// Answers an arrival event, if one is up. Returns its title.
    fn handle_event(&mut self, mark: usize) -> Option<String> {
        let ev = event::parse_events(self.feed.since(mark)).pop()?;
        self.log.push_str(&format!("  event **{}**: {:?}\n", ev.title,
            ev.choices.iter().map(|c| c.text.clone()).collect::<Vec<_>>()));
        // Never `choices[0]`: a corrupted village can put "Kill him" first.
        let pick = ev.continue_choice().or_else(|| ev.safe_choice()).cloned();
        let Some(c) = pick else {
            self.log.push_str("  left alone: more than one real choice\n");
            return Some(ev.title);
        };
        // `onActive` announces a screen at the start of its transition, so settle before clicking
        // and verify, or the click lands on a screen that is still fading in.
        let _ = diggle_solver::observe::settle::wait_for_quiescence(self.win, 0.02, Duration::from_secs(8));
        if let Ok((cx, cy)) = self.win.client_to_screen(c.x, c.y) {
            for _ in 0..4 {
                let before = diggle_solver::win::capture::capture_window(self.win).ok()?;
                let _ = click_at(cx, cy);
                self.park();
                std::thread::sleep(Duration::from_millis(900));
                self.pump();
                let after = diggle_solver::win::capture::capture_window(self.win).ok()?;
                if before.diff_fraction(&after, diggle_solver::observe::settle::FULL) > 0.05 {
                    break;
                }
            }
        }
        Some(ev.title)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config::load(Path::new("config.toml"))?;
    std::fs::create_dir_all(FRAMES)?;
    diggle_solver::win::process::refuse_if_running("lovec.exe", &[])?;
    let save_dir = diggle_solver::game::savedir::locate(cfg.save_dir.clone(), true)?;

    let scorer = diggle_solver::score::Scorer::new(&cfg.game_dir)?;
    let dict = diggle_solver::search::Dictionary::load(&cfg.game_dir)?;

    let console = Console::take()?;
    let mirror = LogMirror::create(Path::new("spike-run-raw.log"))?;
    let mut game = diggle_solver::game::launch::GameProcess::launch(&cfg, &console)?;
    let win = game.wait_for_window(Duration::from_secs(20))?;
    std::thread::sleep(Duration::from_secs(3));

    let mut r = Run {
        win: &win,
        keys: PostMessageInput::new(win),
        feed: Feed::new(console, Some(mirror)),
        reader: adjacency::Reader::new(),
        map: WorldMap::new(),
        latest: None,
        save_dir: save_dir.clone(),
        log: String::from("# Spike: the run\n\n"),
    };

    if diggle_solver::act::click_when_ready(&win, &diggle_solver::act::CONTINUE, Duration::from_secs(30)).is_err() {
        r.log.push_str("ABORT: no Continue\n");
        return finish(&mut game, &r.log);
    }
    let by = Instant::now() + Duration::from_secs(40);
    while Instant::now() < by && r.latest.is_none() {
        std::thread::sleep(Duration::from_millis(300));
        r.pump();
    }
    if r.latest.is_none() {
        r.log.push_str("ABORT: no adjacency dump\n");
        return finish(&mut game, &r.log);
    }
    let mut health = r.apply_save();
    r.log.push_str(&format!(
        "start at **{}**, hell {:?}, anomaly available {:?}, health {:?}\n\n",
        r.map.here().unwrap_or("?"),
        r.map.anomaly().map(|p| p.key.clone()),
        r.map.anomaly_available(),
        health.map(|h| format!("{}/{}", h.current, h.max))
    ));

    let fight = Fight {
        win: &win,
        dict: &dict,
        scorer: &scorer,
        game_dir: cfg.game_dir.clone(),
        combat_path: save_dir.join("combatSaveData"),
        frames: Some(PathBuf::from(FRAMES)),
    };

    // A global stop, so the report is always written. A spike killed from outside leaves a stale
    // report on disk that then reads as the current result.
    let stop = drive(&mut r, &fight, &mut health, Instant::now() + Duration::from_secs(600));
    r.log.push_str(&format!("\n## Stopped\n\n{stop:?}\n\n"));
    r.log.push_str(&format!(
        "at **{}**; {} places known; anomaly {:?}{}; beaten: {}\n\n",
        r.map.here().unwrap_or("?"),
        r.map.len(),
        r.map.anomaly().map(|p| p.key.clone()),
        if r.map.anomaly_is_assumed() { " (assumed)" } else { "" },
        r.map.anomaly_beaten()
    ));
    for p in r.map.places() {
        r.log.push_str(&format!(
            "- `{}` {}{}{}{}\n",
            p.key,
            if p.heading.is_empty() { "(unheaded)" } else { &p.heading },
            if p.completed { " [done]" } else { "" },
            if p.corrupted { " [corrupted]" } else { "" },
            if p.consecrated { " [consecrated]" } else { "" },
        ));
    }
    let out = r.log.clone();
    finish(&mut game, &out)
}

fn drive(
    r: &mut Run, fight: &Fight, health: &mut Option<diggle_solver::rest::Health>, deadline: Instant,
) -> Stop {
    for step in 1..=MAX_STEPS {
        if Instant::now() >= deadline {
            return Stop::Exhausted;
        }
        if r.map.anomaly_beaten() {
            return Stop::AnomalyBeaten;
        }
        let Some(fresh) = r.recentre() else {
            return Stop::Failed("no pan dump after locate-me".into());
        };
        let here = r.map.here().unwrap_or("?").to_string();
        let place = r.map.get(&here).cloned();

        // A shrine we could finish: routing brought us here, but consecrating needs a capability
        // this run does not have, so say so rather than pretend.
        if r.map.worth_consecrating_here(&here) {
            r.log.push_str(&format!("{step}. at **{here}** — consecrating is not implemented\n"));
            return Stop::AtShrine(here);
        }

        // Inside a subworld we cannot navigate: leave by the exit that gets us nearest the target.
        // Nothing here knows how to cross a forest, and standing in one is the worst place to be.
        if let Some(container) = r.map.inside().map(|s| s.to_string()) {
            let Some(want) = r.map.exit_toward(&fresh.exits) else {
                return Stop::Failed(format!("inside {container} with no exit in this dump"));
            };
            let Some(e) = fresh.exits.iter().find(|e| e.to_key == want).cloned() else {
                return Stop::Failed(format!("exit to {want} not on screen"));
            };
            r.log.push_str(&format!("{step}. inside `{container}` — leaving toward `{want}`\n"));
            let Ok((ex, ey)) = r.win.client_to_screen(e.x as i32, e.y as i32) else {
                return Stop::Failed("coordinate conversion failed".into());
            };
            let _ = click_at(ex, ey);
            std::thread::sleep(Duration::from_millis(900));
            r.pump();
            if !matches!(r.click_area_button("Travel (exit)"), Ok(true)) {
                return Stop::Failed(format!("could not take the exit toward {want}"));
            }
            std::thread::sleep(Duration::from_secs(3));
            r.pump();
            continue;
        }

        // Standing on an unfinished fight: it cannot be walked past.
        //
        // `subworld_container` is the guard that stopped this walking into a forest. A container's
        // heading carries a level and reads exactly like a fight, but its area button ENTERS the
        // subworld (`getLocationButtons` tests `typeData.subworld` first). Clearing one means
        // fighting through its interior, which is a capability this run does not have.
        if let Some(p) = place.as_ref().filter(|p| p.subworld_container) {
            r.log.push_str(&format!(
                "{step}. `{here}` ({}) is a subworld — clearing it from the inside is not implemented\n",
                p.heading
            ));
            return Stop::AtSubworld(here);
        }
        // Ask where we are GOING before deciding to fight. The first live run had these the other
        // way round: it saw an unfinished fight underfoot and cleared it unconditionally, when the
        // route ran straight back the way we came. `canTravelToDirect` needs one endpoint complete,
        // and the node behind us always is -- so leaving was legal all along, and the fight it
        // picked was one it walked into for nothing.
        let hop = r.map.next_hop();
        let must_fight_here = match hop.as_ref() {
            Some(h) => !r.map.can_step(&here, &h.step),
            None => true,
        };
        if let Some(p) = place
            .as_ref()
            .filter(|p| p.has_combat() && !p.completed && must_fight_here)
        {
            let is_anomaly = r.map.anomaly().map(|a| a.key == p.key).unwrap_or(false);
            if is_anomaly {
                r.log.push_str(&format!("{step}. **the anomaly** at `{here}` ({})\n", p.heading));
                return Stop::AnomalyReached;
            }
            r.log.push_str(&format!("{step}. fighting `{here}` ({})\n", p.heading));
            if !matches!(r.click_area_button("Combat"), Ok(true)) {
                return Stop::Failed(format!("Combat did not open at {here}"));
            }
            // The pregame announces itself; Space there is `Start`.
            let mark = r.feed.mark();
            let by = Instant::now() + Duration::from_secs(30);
            let mut pregame = false;
            while Instant::now() < by && !pregame {
                std::thread::sleep(Duration::from_millis(300));
                r.pump();
                pregame = r.feed.seen_since(mark, "Pregame screen:");
            }
            if !pregame {
                // No pregame does not mean failure. `getLocationButtons` tests `typeData.subworld`
                // BEFORE `basicCombatZone` (`overworldview.lua:462-467`), so on a forest or a
                // village that button ENTERED the place instead of starting a fight -- and their
                // headings (`Eight Timberland — level 4 forest`, `Ulrome — level 6 village`) read
                // exactly like fights. Record what we just learned and let the next iteration deal
                // with being inside; failing here left the run standing in a village.
                r.pump();
                if let Some(container) = r.map.inside().map(|s| s.to_string()) {
                    r.log.push_str(&format!(
                        "  no pregame — that button entered `{container}`, which is a subworld
"
                    ));
                    continue;
                }
                return Stop::Failed(format!("no pregame and no subworld at {here}"));
            }
            r.keys.focus();
            std::thread::sleep(Duration::from_millis(400));
            if r.keys.press_key(VK_SPACE, SC_SPACE).is_err() {
                return Stop::Failed("could not send Start".into());
            }
            std::thread::sleep(Duration::from_secs(2));

            let mut log = String::new();
            let outcome = fight.run(
                &mut r.feed,
                &r.keys,
                &mut log,
                deadline.min(Instant::now() + Duration::from_secs(240)),
            );
            r.log.push_str(&log.lines().map(|l| format!("    {l}\n")).collect::<String>());
            match outcome {
                Ok(Outcome::Cleared { turns, reward }) => {
                    r.log.push_str(&format!("  cleared in {turns} turns, took {reward:?}\n"));
                }
                Ok(other) => return Stop::Fought(format!("{other:?}")),
                Err(e) => return Stop::Failed(e.to_string()),
            }
            let now = r.apply_save();
            if let (Some(b), Some(a)) = (*health, now) {
                r.map.note_health(b, a);
                r.map.rested(a);
            }
            *health = now;
            continue;
        }

        let Some(hop) = hop else { return Stop::NoPlan };
        let Some(target) = fresh.nodes.iter().find(|n| n.key == hop.step).cloned() else {
            return Stop::Failed(format!("{} is not on screen from {here}", hop.step));
        };
        r.log.push_str(&format!(
            "{step}. {here} -> **{}** (for {}, {:?})\n",
            hop.step, hop.plan.target, hop.plan.reason
        ));

        // Select, and prove it: the lone area button is `affirmative`, and at a combat node that
        // button is Combat. Space without a confirmed selection starts a fight.
        let Ok(before) = diggle_solver::win::capture::capture_window(r.win) else {
            return Stop::Failed("capture failed".into());
        };
        let Ok((sx, sy)) = r.win.client_to_screen(target.x as i32, target.y as i32) else {
            return Stop::Failed("coordinate conversion failed".into());
        };
        let _ = click_at(sx, sy);
        std::thread::sleep(Duration::from_millis(900));
        r.pump();
        let Ok(after) = diggle_solver::win::capture::capture_window(r.win) else {
            return Stop::Failed("capture failed".into());
        };
        if before.diff_fraction(&after, AREA_BUTTONS) <= 0.01 {
            return Stop::Failed(format!("selecting {} did not register", hop.step));
        }
        r.keys.focus();
        std::thread::sleep(Duration::from_millis(200));
        if r.keys.press_key(VK_SPACE, SC_SPACE).is_err() {
            return Stop::Failed("could not send Travel".into());
        }
        r.park();

        let mark = r.feed.mark();
        let by = Instant::now() + Duration::from_secs(60);
        let mut arrived = false;
        while Instant::now() < by && !arrived {
            std::thread::sleep(Duration::from_millis(300));
            r.pump();
            r.handle_event(mark);
            arrived = r.map.here().map(|h| h != here).unwrap_or(false);
        }
        if !arrived {
            return Stop::Failed(format!("no arrival at {}", hop.step));
        }
        let now = r.apply_save();
        if let (Some(b), Some(a)) = (*health, now) {
            r.map.note_health(b, a);
            r.map.rested(a);
        }
        *health = now;
        let _ = Goal::Explore;
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
