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
//! Priority is the map's: **health first**, then a live anomaly, then opening the anomaly, then an
//! unconsecrated shrine, then exploring. Corrupted shrines stop being destinations once the anomaly
//! is open, because they cost a fight we no longer need.
//!
//! Health leads because the anomaly does not expire and health does not return by itself, so a
//! detour is only ever a delay while skipping one can end the save.
//!
//! **While a rest is wanted, every branch also avoids hostile ground** — an uncleared corrupted
//! area, or an unvisited forest that might be a bandit camp — because a subworld under attack has
//! to be cleared before you can leave it, whatever errand took you in. Not just the branch heading
//! for the objective: arriving at a shrine through an uncleared village hurts exactly as much.
//!
//! Both are *preferences*, not guarantees. The planner runs twice — once avoiding hostile ground,
//! once not — so a hurt run with nowhere safe to go still gets on with the objective rather than
//! standing still. Exploring counts as somewhere to go, and is preferred over a hostile area,
//! because a frontier may hold the rest site we have not found yet.
//!
//! ## Where it deliberately stops
//!
//! **Consecrating.** Reaching a shrine is routing; finishing one means `Visit`, then solving the
//! word, then `Consecrate` and `Pray`. The solver is built and tested but has never seen a shrine
//! screen, and reading that grid is a capability this run does not have. Arriving at one is
//! reported and stepped over rather than faked.
//!
//! ## The anomaly fight is NOT skipped
//!
//! It is level 8, and this character came straight here, so losing is the likely outcome. That is
//! the experiment: find out how badly a rushed run loses rather than assume it. A loss cannot be
//! undone in the game — only by restoring a checkpoint.
//!
//! **Drives the real mouse and keyboard. Restore a checkpoint to rewind.**
//!
//! The loop itself lives in [`diggle_solver::navigate`]; this binary reads the config, launches the
//! game, hands over, and writes the report.

use diggle_solver::config::Config;
use diggle_solver::fight::Fight;
use diggle_solver::navigate::{drive, start_new_run, Doorway, Run, Stop, FRAMES};
use diggle_solver::observe::adjacency;
use diggle_solver::observe::affirm;
use diggle_solver::observe::feed::Feed;
use diggle_solver::observe::log::{Console, LogMirror};
use diggle_solver::overworld::WorldMap;
use diggle_solver::win::input::PostMessageInput;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The stable name, always the most recent run. Kept alongside the timestamped copy because every
/// habit and every note in `handoff/` points at it.
const REPORT: &str = "spike-run.md";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config::load(Path::new("config.toml"))?;
    // Before the game is launched and before the mouse is seized: a rejected argument must cost
    // nothing, and a run that has already opened the game has cost something.
    let args: Vec<String> = std::env::args().skip(1).collect();
    let click_frames = diggle_solver::config::click_frames_from_args(&args, cfg.debug_click_frames)
        .map_err(diggle_solver::Error::Config)?;
    std::fs::create_dir_all(FRAMES)?;
    diggle_solver::win::process::refuse_if_running("lovec.exe", &[])?;
    let save_dir = diggle_solver::game::savedir::locate(cfg.save_dir.clone(), true)?;

    let scorer = diggle_solver::score::Scorer::new(&cfg.game_dir)?;
    let dict = diggle_solver::search::Dictionary::load(&cfg.game_dir)?;
    let letters = diggle_solver::letters::Weights::load(&cfg.game_dir)?;

    // **A run must not overwrite the evidence of the last one.**
    //
    // Both files had fixed names, so each run erased its predecessor. That cost a real answer on
    // 2026-08-12: asked why `Rest` had become the goal without a rest site in sight, the run that
    // did it had already been overwritten twice and the question could not be settled at all. The
    // same night, a diagnostic frame was lost the same way and got its own fix.
    //
    // Timestamped copies are the archive; `spike-run.md` and `spike-run-raw.log` stay as the
    // "latest" names everything already refers to.
    let stamp = diggle_solver::stamp::utc(std::time::SystemTime::now());
    let raw_log = format!("spike-run-{stamp}.log");
    let archive = format!("spike-run-{stamp}.md");

    let console = Console::take()?;
    let mirror = LogMirror::create(Path::new(&raw_log))?;
    let launched = Instant::now();
    let mut game = diggle_solver::game::launch::GameProcess::launch(&cfg, &console)?;
    let win = game.wait_for_window(Duration::from_secs(20))?;
    std::thread::sleep(Duration::from_secs(3));
    let window_at = Instant::now();

    let mut r = Run {
        win: &win,
        keys: PostMessageInput::new(win),
        feed: Feed::new(console, Some(mirror)),
        reader: adjacency::Reader::new(),
        map: WorldMap::new(),
        latest: None,
        dumps: 0,
        save_dir: save_dir.clone(),
        log: String::from("# Spike: the run\n\n"),
        // Nothing shown yet, so the whole header is still owed to the terminal.
        logged: 0,
        affirm_repeat: None,
        affirm: affirm::ButtonArt::load(Path::new(&cfg.game_dir), "right")?,
        answered_event: None,
        recentre_misses: 0,
        shrines_tried: std::collections::HashSet::new(),
        rest_failures: std::collections::HashMap::new(),
        slots_captured: std::collections::HashSet::new(),
        snaps: std::collections::HashMap::new(),
        committed_to: None,
        inputs: 0,
        guard: Default::default(),
        recent: std::collections::VecDeque::new(),
        dump_misses: 0,
        pending_cinematic: false,
        combat_expected: false,
        positions_stale_at: 0,
        needs_recentre: false,
        pan_retries: 0,
        zoomed_out: false,
        frame_alarm: false,
    };

    // Timed because the startup felt slow and nobody could say which part was slow. `wait_for_window`
    // returns as soon as the HWND exists, the fixed 3 s after it is a guess, and `click_when_ready`
    // polls `locate` every 400 ms — three candidates, and guessing between them is how this project
    // wastes afternoons.
    // **A dispatch over three fingerprints, not a ladder of polls.** The dev's shape, and the
    // startup path is small enough to have it: at the door there are exactly three things worth
    // pressing, so ask which one is there and act on the answer. See [`Doorway`].
    //
    // What it replaces was two exact scores in sequence plus a lore-card attempt on a three-second
    // timer with its own counter — an order that was an accident of when each piece was added, and
    // a wait that was a guess. The card is now handled the moment it is recognised, however many
    // arrive, and the menu checks no longer run against a screen that is covering the menu.
    //
    // This was `click_when_ready(CONTINUE, 30s)` once, which polls `locate` — a *search* — four
    // times a second; on a fresh save `Continue` can never appear, so it burned the whole 30 s
    // before `Start` was even considered. Every fresh-save launch paid it. Exact origins made each
    // look three comparisons instead of thousands of offsets.
    /// How long the publisher splash is allowed to be the reason nothing is recognised.
    ///
    /// `ui/publishersplash.lua:33` advances on a frame counter and hands over at `frame>7`, so this
    /// is generous rather than tuned; anything unreadable after it is not the splash.
    const SPLASH_GRACE: Duration = Duration::from_secs(8);
    /// How often to spend a full-registry look while confused. Frequent enough to recover, rare
    /// enough that the warning it writes stays readable.
    const CONFUSED_EVERY: Duration = Duration::from_secs(5);

    let menu_at = Instant::now();
    let mut confused = Instant::now();
    let mut found: Result<f64, String> = Err("menu never rendered".into());
    let mut offers_start = false;
    let by = Instant::now() + Duration::from_secs(30);
    while Instant::now() < by {
        match r.doorway() {
            // **A fresh profile does not open on the menu, and 2026-08-16 is the proof.**
            //
            // `ui/publishersplash.lua:46-53`: once the splash is done the game shows the start menu
            // *unless* `persistent.tutorials.autoSave` is unset — in which case it pushes a lore
            // card first (`ui/graphics/text/auto-save.png`, the auto-save notice), sets the flag and
            // writes `persistentSaveData` on the spot. Clearing that file clears the flag, so the
            // first launch after `checkpoint clear` puts a card where the menu is expected. `Start`
            // scored **0.0570** and the run aborted saying the menu might not have rendered. It had,
            // one screen further on.
            //
            // Cleared by the arrow in the bottom-right corner — the dev, watching it happen. Both
            // the key and the click go at the same button; see [`Run::clear_the_gate`].
            Doorway::Text => {
                r.log.push_str("  a text screen is gating the menu
");
                if !r.clear_the_gate() {
                    r.log.push_str("  could not clear it — looking again
");
                }
            }
            Doorway::Resume => {
                found = diggle_solver::act::click_exact(
                    &win,
                    &diggle_solver::act::CONTINUE,
                    diggle_solver::act::CONTINUE_PRESENT,
                )
                .map_err(|e| e.to_string());
                break;
            }
            Doorway::Fresh => {
                offers_start = true;
                break;
            }
            // The splash is several seconds long and nothing is pressable during it, so this is the
            // ordinary answer for the first stretch of every launch — and looking again is the
            // whole response for as long as it stays ordinary.
            //
            // **After that it is confusion, and confusion has a standing answer**: the full
            // observer, with a warning. `SPLASH_GRACE` is what separates the two, and it is set from
            // the thing it is waiting out rather than from a fear of pressing too early — the splash
            // runs on a frame counter (`ui/publishersplash.lua:33`, `frame>7`), so it is over in a
            // few seconds and anything still unreadable at eight is not a splash.
            //
            // Bounded, because a fallback that fires every 150 ms would bury the log it exists to
            // write. Between attempts the loop goes back to the cheap three-template question, so a
            // screen that resolves on its own is still caught by the fast path.
            Doorway::Nothing => {
                if menu_at.elapsed() > SPLASH_GRACE && confused.elapsed() > CONFUSED_EVERY {
                    confused = Instant::now();
                    r.recover_by_observing("the start menu");
                }
                std::thread::sleep(Duration::from_millis(150));
            }
        }
    }
    let found = if offers_start { Err("no Continue".to_string()) } else { found };
    r.log.push_str(&format!(
        "launch->window+3s {:?}, then the door took {:?}\n",
        window_at.duration_since(launched),
        menu_at.elapsed()
    ));
    if found.is_err() {
        // **The dispatch already decided this; nothing is re-read here.** It used to score
        // `MENU_START` a second time to find out which kind of failure it had, which is a second
        // reading that can disagree with the first — the loop breaks on `Doorway::Fresh` and this
        // could then miss on an animation and abort a launch that was fine.
        //
        // `Start` is recognised and deliberately not pressed by [`Run::doorway`]: that slot reads
        // `Restart` when a save DOES exist, and pressing that eulogises the run. The fingerprint is
        // the whole safety argument, which is why the pressing lives in `start_new_run` behind
        // `click_exact` rather than out here.
        if offers_start {
            r.log.push_str(
                "no Continue — the menu offers `Start`, so there is no save to resume. \
                 Beginning a new run.\n",
            );
            if let Err(e) = start_new_run(&mut r, &cfg.game_dir) {
                r.log.push_str(&format!("ABORT: {e}\n"));
                return finish(&mut game, &r.log, r.unflushed(), &archive);
            }
        } else {
            // **Ask the observer before giving up.** The dev's standing rule, and this was the one
            // path that still broke it: on 2026-08-16 the run stopped here saying "the menu may not
            // have rendered" on the strength of a single number, when what was on screen was the
            // auto-save lore card — a screen the spikes have known about for weeks.
            //
            // Reaching here now means thirty seconds in which none of the three was ever
            // recognised, so a score would say nothing anyway: `identify` names the screen if we
            // have a fingerprint for it, `log_button_scores` says what came closest if we do not,
            // and the frame lets the next person look at the screen instead of re-running to see
            // it — which for a first launch is not even possible, since the game writes
            // `persistent.tutorials.autoSave` the moment it shows that card.
            r.log.push_str(
                "ABORT: no menu and no text screen within 30s — asking what IS on screen\n",
            );
            r.log.push_str(&format!("  screen: {:?}\n", diggle_solver::act::identify(&win)));
            r.log_button_scores();
            r.snap_screen("startup-no-menu");
            return finish(&mut game, &r.log, r.unflushed(), &archive);
        }
    }
    let by = Instant::now() + Duration::from_secs(40);
    while Instant::now() < by && r.latest.is_none() {
        std::thread::sleep(Duration::from_millis(300));
        r.pump();
    }
    if r.latest.is_none() {
        r.log.push_str("ABORT: no adjacency dump\n");
        return finish(&mut game, &r.log, r.unflushed(), &archive);
    }
    let mut health = r.apply_save();
    // **What earlier runs learned, before this one decides where to go.**
    //
    // The save carries which areas are complete and which are corrupted; it does not carry a single
    // edge or coordinate, because the world is generated into memory from the seed and never
    // written down. So without this a resumed run knows `shrine1` exists, is cleared and is
    // unconsecrated, and has no idea how to walk there — which is exactly what kept a free
    // consecration untaken across four runs.
    //
    // Loaded *before* the first dump registers, deliberately: `registration` anchors a new dump
    // against any node that already carries a position, so restoring the old positions first makes
    // this run adopt the earlier frame instead of inventing a fresh one.
    match r.load_map_cache() {
        Some((edges, path)) => {
            r.log.push_str(&format!("recalled {edges} edges from `{path}`\n"));
        }
        None => r.log.push_str("no map remembered for this world — starting from the save alone\n"),
    }
    // The first reading counted too, and used to be taken by hand here: `note_health` needs a before
    // and an after, so on a resumed save there is nothing for it to compare and the intent was never
    // set — which is how a run that opened at 4/12 walked straight into a village fight without once
    // considering a rest. `WorldMap::apply_save` now reads health alongside gold and fuel, so the
    // `apply_save` above has already done it.
    r.log.push_str(&format!(
        "start at **{}**, portal {:?}, anomaly open {:?}, health {:?}\n\n",
        r.map.here().unwrap_or("?"),
        r.map.anomaly().map(|p| p.key.clone()),
        r.map.anomaly_is_open(),
        health.map(|h| format!("{}/{}", h.current, h.max))
    ));
    // **The two survival conditions, stated before the run starts rather than inferred from it.**
    //
    // A reader otherwise cannot tell a run that had four shrines and ignored them from one that only
    // ever found two, nor a run that skipped the bank from one that had nothing to bank for. Both
    // numbers are free here and both decide where the first hop goes.
    r.log.push_str(&format!(
        "shrines: {} known, {} consecrated (the portal wants {}); well-rested: {} stack(s) short

",
        r.map.shrines_known(),
        r.map.consecrations(),
        diggle_solver::overworld::SHRINES_BEFORE_THE_ANOMALY,
        r.map.stacks_short_ahead(),
    ));

    let fight = Fight {
        win: &win,
        dict: &dict,
        scorer: &scorer,
        letters: &letters,
        game_dir: cfg.game_dir.clone(),
        combat_path: save_dir.join("combatSaveData"),
        frames: Some(PathBuf::from(FRAMES)),
        click_frames: click_frames.then(|| PathBuf::from(FRAMES)),
    };
    if click_frames {
        r.log.push_str(&format!(
            "**`debug_click_frames` is on**: every tile click is photographed either side, into \
             `{FRAMES}`. This costs a full-window capture per click and is not how a normal run \
             should be measured.\n\n"
        ));
    }

    // A global stop, so the report is always written. A spike killed from outside leaves a stale
    // report on disk that then reads as the current result.
    // `0` means no limit, expressed as a deadline a year out rather than as an `Option` threaded
    // through `drive`: the check stays one comparison, and a run that is still going in a year has
    // problems this would not have solved.
    let minutes = cfg.run_minutes.unwrap_or(diggle_solver::config::DEFAULT_RUN_MINUTES);
    let budget = match minutes {
        0 => Duration::from_secs(365 * 24 * 60 * 60),
        m => Duration::from_secs(m * 60),
    };
    r.log.push_str(&match minutes {
        0 => "no time limit; `.diggle-stop` ends the run\n\n".to_string(),
        m => format!("time limit {m} min; `.diggle-stop` ends the run\n\n"),
    });
    let stop = drive(&mut r, &fight, &mut health, Instant::now() + budget);
    r.log.push_str(&format!("\n## Stopped\n\n{stop:?}\n\n"));

    // **Before anything else, and on every ending including death.** What a run learned about the
    // terrain outlives the character that learned it: the seed is the same, the roads are the same,
    // and the next run should not have to rediscover them. A death is in fact the case that most
    // needs it, since that is when the map is largest and the next run starts furthest back.
    r.save_map_cache();

    // Photograph whatever beat us, once, here.
    //
    // Not on every failed match -- the observer misses constantly and by design, because screens
    // animate and a template scores near zero against a half-drawn one. Those misses are the normal
    // operation of a retry loop, and shooting them would bury the one frame that matters under
    // hundreds that do not.
    //
    // The frame that matters is the screen the run gave up on. Without it a stall is diagnosed from
    // the log's last line, which names the step that noticed the problem rather than the state that
    // caused it: three runs reported "no pan dump after locate-me" while sitting in combat with
    // `Finish` on screen, and the map path was never the fault.
    //
    // **And ask the observer what it is looking at, every single time.** The dev's rule, and this is
    // the one place every give-up in `drive` passes through, so it is the one place it can be
    // obeyed without twenty separate edits. A stop message names the step that noticed a problem;
    // `identify` names the state that caused it, and the two disagree often enough to be the whole
    // diagnosis. On 2026-08-15 a run ended `Combat did not open at l16sub5` while sitting on a
    // perfectly ordinary pregame that had simply not finished animating in.
    //
    // Diagnostic only. Nothing here decides anything — the run is already over — but the next reader
    // gets the screen's name beside the excuse instead of having to open the frame and guess.
    if !matches!(stop, Stop::AnomalyBeaten) {
        let seen = diggle_solver::act::identify(r.win);
        r.log.push_str(&format!("the observer calls the stop screen **{seen:?}**
"));
        r.log_button_scores();
        match diggle_solver::win::capture::capture_window(r.win) {
            Ok(f) => {
                let path = PathBuf::from(FRAMES).join("gave-up.png");
                match f.write_png(&path) {
                    Ok(()) => r.log.push_str(&format!("screen at the stop: `{}`\n\n", path.display())),
                    Err(e) => r.log.push_str(&format!("could not write the stop frame: {e}\n\n")),
                }
            }
            Err(e) => r.log.push_str(&format!("could not capture the stop frame: {e}\n\n")),
        }
    }
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
    // The stop itself ends a step, so a streak of identical affirmative readings can still be open
    // here — and the run that motivated the folding ended in the middle of one five hundred long.
    // Closed before the summary so the count is reported rather than silently dropped.
    r.close_affirmative_run();
    // Two different logs on purpose: the files get the whole run, the terminal gets only what
    // `Run::flush_log` has not already shown. Printing `out` here as well would repeat every step
    // a second time under the summary.
    let out = r.log.clone();
    let tail = r.unflushed().to_string();
    finish(&mut game, &out, &tail, &archive)
}

/// Writes the report and, unless asked not to, closes the game.
///
/// `DIGGLE_KEEP_OPEN=1` leaves it running on the screen that stopped the run. A stall is a *visual*
/// fact — which controls are drawn, whether anything is still animating — and closing the game
/// destroys the only copy of that evidence, forcing a fresh 15-minute run to see it again. It also
/// lets a human look at the failure directly, which has been faster than any instrument here.
///
/// Note this leaves the save unflushed: `mainSaveData` is written on screen *exit*, so a checkpoint
/// taken while the game is still up records the last screen the player left, not the current one.
/// `log` is the whole run and goes to both files; `tail` is only what the terminal has not seen
/// yet, because [`diggle_solver::navigate::Run::flush_log`] has been printing each step as it
/// happened. They were one argument until 2026-08-17, when the log started streaming.
fn finish(
    game: &mut diggle_solver::game::launch::GameProcess, log: &str, tail: &str, archive: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Both names, on every exit. `REPORT` is what habit and the handoff notes reach for; the
    // archived copy is the one still there after the next run.
    let write = |log: &str| -> std::io::Result<()> {
        std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
        std::fs::File::create(archive)?.write_all(log.as_bytes())
    };
    if std::env::var("DIGGLE_KEEP_OPEN").as_deref() == Ok("1") {
        write(log)?;
        print!("{tail}");
        println!("\n-- game left running (DIGGLE_KEEP_OPEN=1); close it before any checkpoint --");
        return Ok(());
    }
    game.close(Duration::from_secs(15));
    write(log)?;
    print!("{tail}");
    println!("-- archived as {archive} --");
    Ok(())
}
