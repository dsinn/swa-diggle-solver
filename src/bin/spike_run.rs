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

use diggle_solver::config::Config;
use diggle_solver::fight::{Fight, Outcome};
use diggle_solver::observe::adjacency::{self, Adjacency};
use diggle_solver::observe::affirm;
use diggle_solver::observe::event;
use diggle_solver::observe::feed::Feed;
use diggle_solver::observe::log::{Console, LogMirror};
use diggle_solver::observe::pan;
use diggle_solver::overworld::{Goal, WorldMap};
use diggle_solver::win::input::{
    click_at_in, warp_cursor, Input, PostMessageInput, SC_RETURN, SC_SPACE, VK_RETURN, VK_SPACE,
};
use diggle_solver::win::window::GameWindow;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const REPORT: &str = "spike-run.md";
const FRAMES: &str = "spike-frames-live";
const AREA_BUTTONS: diggle_solver::win::capture::Region =
    diggle_solver::win::capture::Region { nx: 0.0, ny: 0.68, nw: 0.45, nh: 0.18 };
const SHOW_AREA_BUTTONS: (i32, i32) = (32, 918);
/// `Combat` / `Travel` / `Visit` all land here — which is why the area buttons must be known to be
/// the right ones before it is clicked.
const AREA_BUTTON: (i32, i32) = (187, 918);
const EMPTY_MAP: (i32, i32) = (1750, 160);
/// Touch this file to stop the run **gracefully** at the next step boundary.
///
/// The alternatives are both worse. Killing `spike_run` — `timeout`, Ctrl-C, stopping the background
/// task — ends the process before `main` writes the report and the stop frame, so the run costs a
/// launch and returns nothing. Killing the *game* does leave a report, but it interrupts whatever
/// was mid-flight: a click, a save write, an unconfirmed reward screen (which discards the reward).
///
/// Checked at the top of the loop, between steps, where nothing is half-done. Consumed on read so a
/// stale file cannot end the next run before it starts.
const STOP_FILE: &str = ".diggle-stop";

/// Points tried, in order, when looking for map with no location under it.
///
/// One fixed point cannot be right everywhere — a subworld packs its nodes into a much smaller area
/// than a world map, and where the nodes fall depends on the seed. All of these are inside the map
/// area and clear of the chrome ([`hud::is_map_point`] covers the top strip and the corners), and
/// they are spread so that a map dense around one is sparse around another.
const EMPTY_MAP_CANDIDATES: [(i32, i32); 4] =
    [EMPTY_MAP, (1750, 800), (170, 240), (960, 170)];
/// Where the cursor is parked so it cannot hover something we are about to fingerprint.
///
/// Deliberately clear of the view's hotspot rectangle, `{0, 0, 300, height*0.8}`
/// (`overworldview.lua:1146`). The old value was (300, 300) — sitting exactly on that rectangle's
/// right edge, next door to a function named `backOutOfHotspotMapPan`. Parking on the boundary of a
/// region whose handler pans the map is not somewhere to leave a cursor for seconds at a time.
const NEUTRAL: (i32, i32) = (760, 240);
const MAX_STEPS: usize = 20;

#[derive(Debug)]
enum Stop {
    /// Someone asked us to stop, via [`STOP_FILE`].
    Requested,
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
    /// The game's own `right-*.png` artwork, for reading the affirmative slot off the screen.
    ///
    /// This replaced a tally of `Lore screen:` console lines. The tally could only ever answer "how
    /// many text screens were constructed", when what a press needs to know is "is a live control on
    /// the screen right now" — and it was open-loop, so our count and the game's reality could only
    /// drift. See [`diggle_solver::observe::affirm`].
    affirm: affirm::ButtonArt,
    /// Feed line index of the last `Event:` block we acted on.
    ///
    /// Identity, not a tally. The previous design read `feed.since(mark)` with the mark taken at the
    /// top of the loop, which threw away any event announced during the *previous* iteration's pump
    /// — and that is precisely when they arrive, because clearing the text screen in front of the
    /// event is what causes the event to announce itself. Proof from a live log: the two
    /// `Lore screen:` lines and the `Event:` line landed at 119-121, all before the next mark.
    ///
    /// Keying on the block's position makes the read idempotent and self-resyncing: a newer event
    /// has a larger index, the same event has the same index, and nothing has to stay in step with a
    /// count that can drift.
    answered_event: Option<usize>,
    /// Have we already been through the pregame screen in this save?
    ///
    /// It appears once per save — the item-selection screen before the first fight — and never
    /// again. So after the first one there is nothing to wait FOR, and the wait can collapse to a
    /// single immediate look. The branch decision is deliberately unchanged: "no pregame" still has
    /// to be distinguished from "that button entered a subworld", and conflating the two would send
    /// a real fight down the subworld path.
    pregame_seen: bool,
    /// Feed line index of the last `Pregame screen:` we answered.
    ///
    /// Identity rather than a tally, for the same reason [`Run::answered_event`] is: a count and the
    /// game's reality can only drift, while an index is idempotent and self-resyncing.
    answered_pregame: Option<usize>,
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

    /// Scrolls the map by roughly `want` and reports how far it **actually** moved.
    ///
    /// The drag is open-loop and clamped, so the answer comes from comparing frames rather than from
    /// the delta we asked for: lift a patch of map, drag, find the patch again, and the displacement
    /// is the shift. That needs no sprite identity and no calibration constant, and it cancels tint
    /// and scale because it compares a rendering against itself.
    ///
    /// Several patch positions are tried because a patch of flat void matches equally well
    /// everywhere, and would report whichever candidate the sweep reached first. `None` means the
    /// map moved by an unknown amount, which the caller must treat as losing its position entirely —
    /// a pan that happened but was not measured is worse than one that never happened.
    fn pan_map(&mut self, want: pan::Shift) -> Option<pan::Shift> {
        let (cw, ch) = self.win.client_size().ok()?;
        let before = diggle_solver::win::capture::capture_window(self.win).ok()?;

        // Clear of the chrome, and spread so that a map blank around one is textured around another.
        let spots = [(760, 300), (1100, 500), (500, 620), (1300, 250)];
        let (patch, taken_at) = spots
            .iter()
            .filter(|(x, y)| diggle_solver::observe::hud::is_map_point(*x, *y, cw, ch))
            .filter_map(|&(x, y)| pan::patch_from(&before, x, y, pan::PATCH).map(|t| (t, (x, y))))
            .max_by(|a, b| pan::variance(&a.0).total_cmp(&pan::variance(&b.0)))?;

        // Drag from a point that leaves room to travel in the wanted direction, and stay inside the
        // window: an endpoint outside it is refused outright by `drag_in`'s window guard.
        let (sx, sy) = (
            (cw / 2 - want.dx.round() as i32 / 2).clamp(160, cw - 160),
            (ch / 2 - want.dy.round() as i32 / 2).clamp(160, ch - 160),
        );
        let (ex, ey) = (
            (sx + want.dx.round() as i32).clamp(4, cw - 4),
            (sy + want.dy.round() as i32).clamp(4, ch - 4),
        );
        let from = self.win.client_to_screen(sx, sy).ok()?;
        let to = self.win.client_to_screen(ex, ey).ok()?;
        if diggle_solver::win::input::drag_in(self.win, from, to, 8).is_err() {
            return None;
        }
        std::thread::sleep(Duration::from_millis(300));

        let after = diggle_solver::win::capture::capture_window(self.win).ok()?;
        // Search generously: the clamp can swallow most of the requested movement, so the patch may
        // barely have moved at all, and the honest answer to that is a small measured shift.
        let radius = (want.dx.abs().max(want.dy.abs()) as i32 + pan::PATCH).max(200);
        pan::measure(&after, &patch, taken_at, want, radius)
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
    ///
    /// ## Why the middle step is *read* and not merely clicked
    ///
    /// [`affirm::SHOW_AREA_BUTTONS`] shares its slot with the current location's area buttons. Clicking
    /// (32, 918) without looking presses whichever of the two is there, and on a combat node the
    /// other one is `Combat`. Reading the slot first costs one 76x92 crop and turns "the click did
    /// nothing" into "the arrow was not there", which is a different bug with a different fix.
    ///
    /// The empty-space click is retried across several candidates because "empty" is a property of
    /// the map, not of the coordinate: `core:mousereleased` only restores the button when the
    /// release was over **no location** (`:1479-1485`), and one fixed point cannot be empty on every
    /// map. Each attempt is checked, so this converges rather than hoping.
    fn recentre(&mut self) -> Option<Adjacency> {
        let (cw, ch) = self.win.client_size().unwrap_or((1920, 1080));
        for (n, &(cx, cy)) in EMPTY_MAP_CANDIDATES.iter().enumerate() {
            // Actually check the point is on the map before clicking it.
            //
            // The list's own comment says these are "inside the map area and clear of the chrome
            // ([`hud::is_map_point`] covers the top strip and the corners)" — but nothing tested it,
            // so the claim was documentation rather than behaviour. A fresh map paid for that: one
            // of these clicks opened the character inventory, after which the area-button coordinate
            // means `Stats` and the run has no way back. That failure is described three comments
            // up in this same file, from a previous occurrence.
            if !diggle_solver::observe::hud::is_map_point(cx, cy, cw, ch) {
                self.log.push_str(&format!("  skipping ({cx},{cy}): not a map point\n"));
                continue;
            }
            let Ok((ex, ey)) = self.win.client_to_screen(cx, cy) else { continue };
            let _ = click_at_in(self.win, ex, ey);
            self.park();
            std::thread::sleep(Duration::from_millis(500));
            self.pump();
            let slot = self.read_slot(&affirm::SHOW_AREA_BUTTONS);
            self.log.push_str(&format!(
                "  locate-me slot after clicking empty map at ({cx},{cy}): {:?} (score {:.2})\n",
                slot.state, slot.score
            ));
            if !slot.state.is_ready() {
                // That point was not empty: the click selected something, which leaves the slot
                // showing that location's buttons instead. Try elsewhere.
                continue;
            }
            let Ok((lx, ly)) = self.win.client_to_screen(SHOW_AREA_BUTTONS.0, SHOW_AREA_BUTTONS.1) else { continue };
            let _ = click_at_in(self.win, lx, ly);
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
            self.log.push_str(&format!("  no pan finished after locate-me (candidate {n})\n"));
        }
        None
    }

    /// Clicks the lone area button, having first proved the strip is showing something.
    fn click_area_button(&mut self, what: &str) -> Result<bool, Box<dyn std::error::Error>> {
        let before = diggle_solver::win::capture::capture_window(self.win)?;
        let (bx, by) = self.win.client_to_screen(AREA_BUTTON.0, AREA_BUTTON.1)?;
        click_at_in(self.win, bx, by)?;
        self.park();
        std::thread::sleep(Duration::from_millis(1000));
        self.pump();
        let after = diggle_solver::win::capture::capture_window(self.win)?;
        let moved = before.diff_fraction(&after, diggle_solver::observe::settle::FULL);
        self.log.push_str(&format!("  clicked {what}: screen moved {moved:.3}\n"));
        Ok(moved > 0.05)
    }

    /// The most recent dump whose coordinates have stopped moving.
    ///
    /// Arrival prints the dump *while the camera is still panning* — every hop produces a pair,
    /// `Arrived at location` and then `Screen pan finished`, and only the second has settled
    /// coordinates. Using the first is what put a click 42 px short of the Ulrome well: the node was
    /// chosen correctly, `Travel` was genuinely available, and the click selected nothing because the
    /// map had kept moving after the numbers were printed.
    ///
    /// `recentre` has always waited for `reason.contains("pan")`, which is why the overworld never
    /// showed this. Inside a subworld there is no locate-me to force the pan, so we wait for the one
    /// the arrival already started.
    ///
    /// Returns `None` rather than falling back to the arrival dump. A stale coordinate does not fail
    /// loudly — it clicks empty ground and reports that nothing happened, which is three runs'
    /// evidence that guessing here is worse than stopping.
    fn settled_dump(&mut self, within: Duration) -> Option<Adjacency> {
        let by = Instant::now() + within;
        loop {
            self.pump();
            if let Some(a) = self.latest.as_ref().filter(|a| a.reason.contains("pan")) {
                return Some(a.clone());
            }
            if Instant::now() >= by {
                let reason = self.latest.as_ref().map(|a| a.reason.clone()).unwrap_or_default();
                self.log.push_str(&format!(
                    "  no settled dump within {within:?}; newest is `{reason}`\n"
                ));
                return None;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    /// Reads the affirmative slot in the bottom-right corner.
    fn affirmative(&self) -> affirm::Reading {
        self.read_slot(&affirm::LORE_AFFIRMATIVE)
    }

    /// Reads any `right`-artwork slot from a crop of just that slot.
    ///
    /// One reader for both corners: the lore screen's continue arrow and the overworld's locate-me
    /// arrow are the same five images, differing only in where they sit.
    fn read_slot(&self, spec: &diggle_solver::win::window::ButtonSpec) -> affirm::Reading {
        let absent = affirm::Reading { state: affirm::State::Absent, score: 0.0, margin: 0.0 };
        let Ok((cw, ch)) = self.win.client_size() else { return absent };
        let (x, y, w, h) = affirm::ButtonArt::crop_rect(spec, cw, ch);
        match diggle_solver::win::capture::capture_client_rect(self.win, x, y, w, h) {
            Ok(f) => self.affirm.read_cropped(&f, spec, (cw, ch), (x, y)),
            Err(_) => absent,
        }
    }

    /// Clears a text screen that is gating everything behind it, if one is up.
    ///
    /// Text comes **before** options: while a lore screen is up, the event's choices do not exist
    /// yet, so `parse_events` finds nothing, the map is not on screen, and everything downstream
    /// stalls. A live run sat at a village gate for exactly this reason.
    ///
    /// The loop is read → act → **re-read**, which is the shape every input in this project should
    /// have. The exit condition is the control no longer being pressable, observed rather than
    /// assumed, so a press swallowed by a fade costs one more iteration instead of stranding the run.
    /// Text screens also arrive in runs — entering Ulrome printed two at once — and that needs no
    /// special handling here: the next read simply finds the next one.
    ///
    /// Space rather than a click, because the button declares `userFunctionName = 'affirmative'`
    /// (`ui/lorescreen.lua:49`) and `space = 'affirmative'` (`utils/defaultbinds/keyboard.lua:13`).
    /// Reading the binding beats assuming a coordinate, and this button's position moves with
    /// whether it carries a label.
    fn clear_text_screen(&mut self) -> bool {
        let first = self.affirmative();
        // Logged even when there is nothing to do. A gate that has never been calibrated against a
        // live screen must show its score for the negative case too, or `Absent` is
        // indistinguishable from a threshold set too high.
        self.log.push_str(&format!(
            "  affirmative slot: {:?} (score {:.2}, margin {:.2})\n",
            first.state, first.score, first.margin
        ));
        if !first.state.is_ready() {
            return false;
        }
        for attempt in 1..=6 {
            // No quiescence wait at all any more.
            //
            // It was insurance against pressing into a transition, but a lore screen animates its
            // text in over a blurred, drifting backdrop, so quiescence at 0.02 is never reached and
            // the full timeout was burned on every press — twice per screen, on every screen. The
            // button fingerprint is a better readiness signal than stillness ever was: it says the
            // control is painted and live, and the re-read after the press says whether it took.
            self.keys.focus();
            std::thread::sleep(Duration::from_millis(150));
            let _ = self.keys.press_key(VK_SPACE, SC_SPACE);
            std::thread::sleep(Duration::from_millis(700));
            self.pump();
            let now = self.affirmative();
            if !now.state.is_ready() {
                self.log.push_str(&format!("  cleared after {attempt} press(es)\n"));
                return true;
            }
            self.log.push_str(&format!(
                "  press {attempt} did not take (still {:?}, score {:.2})\n",
                now.state, now.score
            ));
        }
        // Reported, not swallowed: still-ready after six presses means the reading and the binding
        // disagree, and that is worth seeing in the log rather than looping forever.
        self.log.push_str("  affirmative still live after 6 presses — giving up on it\n");
        true
    }

    /// Answers an arrival event, if an unanswered one has been announced. Returns its title.
    ///
    /// Scans the whole feed rather than a window. See [`Run::answered_event`] for why the window was
    /// wrong; the short version is that the event announces itself while we are busy dismissing the
    /// text screen in front of it, so any window opened afterwards is already too late.
    fn handle_event(&mut self) -> Option<String> {
        let at = self.feed.lines().iter().rposition(|l| l.starts_with("Event:"))?;
        if self.answered_event == Some(at) {
            return None;
        }
        let tail: Vec<String> = self.feed.lines()[at..].to_vec();
        // Recorded before acting, not after: a click that half-lands must not leave us free to
        // answer the same event again on the next pass.
        self.answered_event = Some(at);
        let ev = event::parse_events(&tail).pop()?;
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
                let _ = click_at_in(self.win, cx, cy);
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
        save_dir: save_dir.clone(),
        log: String::from("# Spike: the run\n\n"),
        affirm: affirm::ButtonArt::load(Path::new(&cfg.game_dir), "right")?,
        answered_event: None,
        pregame_seen: false,
        answered_pregame: None,
    };

    // Timed because the startup felt slow and nobody could say which part was slow. `wait_for_window`
    // returns as soon as the HWND exists, the fixed 3 s after it is a guess, and `click_when_ready`
    // polls `locate` every 400 ms — three candidates, and guessing between them is how this project
    // wastes afternoons.
    // One tight loop over BOTH menu buttons, breaking on whichever appears.
    //
    // This was `click_when_ready(CONTINUE, 30s)`, which polls `locate` — a *search* — four times a
    // second, and on a fresh save `Continue` can never appear, so it burned the whole 30 s before
    // `Start` was even considered. Every fresh-save launch paid it.
    //
    // Both buttons now have exact origins, so each look is two comparisons rather than thousands of
    // offsets, and the loop exits the moment either is recognised.
    let menu_at = Instant::now();
    let mut found: Result<f64, String> = Err("menu never rendered".into());
    let mut offers_start = false;
    let by = Instant::now() + Duration::from_secs(30);
    while Instant::now() < by {
        let cont = diggle_solver::act::score_exact(&win, &diggle_solver::act::CONTINUE);
        if matches!(cont, Ok(q) if q >= diggle_solver::act::CONTINUE_PRESENT) {
            found = diggle_solver::act::click_exact(
                &win,
                &diggle_solver::act::CONTINUE,
                diggle_solver::act::CONTINUE_PRESENT,
            )
            .map_err(|e| e.to_string());
            break;
        }
        let start = diggle_solver::act::score_exact(&win, &diggle_solver::act::MENU_START);
        if matches!(start, Ok(q) if q >= diggle_solver::act::MENU_START_PRESENT) {
            offers_start = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    let found = if offers_start { Err("no Continue".to_string()) } else { found };
    r.log.push_str(&format!(
        "launch->window+3s {:?}, then Continue took {:?}\n",
        window_at.duration_since(launched),
        menu_at.elapsed()
    ));
    if found.is_err() {
        // No `Continue` means no save — so the menu is offering `Start` instead. Say which, because
        // "no Continue" alone cannot distinguish "fresh save, ready to start a new run" from "the
        // menu never rendered", and those want opposite responses.
        //
        // Recognised, deliberately not pressed. That slot reads `Restart` when a save DOES exist and
        // pressing it eulogises the run, so the fingerprint is the whole safety argument — and past
        // it lies hero select, which nothing in this tree can drive yet. Better to stop here naming
        // the reason than to click into a screen we cannot leave.
        match diggle_solver::act::score_exact(&win, &diggle_solver::act::MENU_START) {
            Ok(q) if q >= diggle_solver::act::MENU_START_PRESENT => {
                r.log.push_str(&format!(
                    "no Continue — the menu offers `Start` ({q:.4}), so there is no save to \
                     resume. Beginning a new run.\n"
                ));
                if let Err(e) = start_new_run(&mut r, &cfg.game_dir) {
                    r.log.push_str(&format!("ABORT: {e}\n"));
                    return finish(&mut game, &r.log);
                }
            }
            Ok(q) => {
                r.log.push_str(&format!(
                    "ABORT: no Continue, and no `Start` either (best {q:.4}) — the menu may not \
                     have rendered\n"
                ));
                return finish(&mut game, &r.log);
            }
            Err(e) => {
                r.log.push_str(&format!("ABORT: no Continue; could not read `Start`: {e}\n"));
                return finish(&mut game, &r.log);
            }
        }
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
    // The first reading counts too. `note_health` needs a before and an after, so on a resumed save
    // there is nothing for it to compare and the intent is never set — which is how a run that
    // opened at 4/12 walked straight into a village fight without once considering a rest.
    if let Some(h) = health {
        r.map.note_health_level(h);
    }
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
    let stop = drive(&mut r, &fight, &mut health, Instant::now() + Duration::from_secs(900));
    r.log.push_str(&format!("\n## Stopped\n\n{stop:?}\n\n"));

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
    if !matches!(stop, Stop::AnomalyBeaten) {
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
    let out = r.log.clone();
    finish(&mut game, &out)
}

/// Starts a fresh run: press `Start`, get through hero select, clear the pregame.
///
/// ## Hero select is silent, so this does not try to see it
///
/// Grepping every `print("` in the game's `ui/`, neither `heroselect.lua` nor `classselection.lua`
/// announces itself. But the screens around them do — `ui/unlockscreen.lua:32` prints
/// `Unlock screen:` and `ui/pregame.lua:91` prints `Pregame screen:` — and
/// `heroselect.load -> unlockedCheck -> modesequence` is the order.
///
/// So the selection screen needs no fingerprint of its own. **`Pregame screen:` is the success
/// test**, and that is what this waits for. `spike_reach_overworld` did something similar but
/// stopped on "two consecutive non-reactions", which is a stall detector standing in for a success
/// test — it could not tell arriving from giving up. Here the non-reaction count only decides when
/// to abandon; arrival is decided by the game saying so.
///
/// ## The default class, on purpose
///
/// Return takes whatever is selected. Choosing a class *well* is a separate question needing its own
/// evidence, and nothing in the MVP depends on it — a run that starts is worth more than a run that
/// starts as the right class.
fn start_new_run(r: &mut Run, game_dir: &Path) -> Result<(), String> {
    /// Enough Returns for the unlock chain, which is longer on a profile with unlocks pending.
    const MAX_RETURNS: usize = 16;
    let mark = r.feed.mark();

    // Verified click: this slot reads `Restart` when a save exists, and that eulogises the run.
    // `act::click` refuses rather than guessing, which is the entire safety argument.
    let q = diggle_solver::act::click_exact(
        r.win,
        &diggle_solver::act::MENU_START,
        diggle_solver::act::MENU_START_PRESENT,
    )
    .map_err(|e| format!("could not click `Start`: {e}"))?;
    r.log.push_str(&format!("  clicked `Start` ({q:.4})\n"));

    // Confirm the press was ACTED ON, not merely that the button was there.
    //
    // Three runs died on this. `Start` reported 1.0000 every time and the game stayed on the main
    // menu, so the champion click that followed landed on empty menu background — which is why three
    // different card positions all read back 0.15 and looked like a hitbox problem. It was not; we
    // were never on the champion screen at all.
    //
    // The menu going away is the consequence to watch for, and it is exactly what
    // `wait_until_gone` asks.
    if !diggle_solver::act::wait_until_gone(
        r.win,
        &diggle_solver::act::MENU_START,
        diggle_solver::act::MENU_START_PRESENT,
        Duration::from_secs(8),
    ) {
        return Err("clicked `Start` but the main menu is still up — the press did not take".into());
    }
    r.log.push_str("  the menu cleared\n");
    // Whatever comes next needs a moment to draw before anything is clicked on it.
    std::thread::sleep(Duration::from_millis(1200));

    // Hero select is CLICK-to-choose, not Return-to-accept.
    //
    // A first cut drove Return here and sent sixteen of them into a screen that ignores every one —
    // the same trap `spike_reach_overworld`'s header records for the start menu, whose buttons
    // "declare no `userFunctionName`, so Return does nothing there". There is no default champion to
    // accept; a card has to be picked.
    //
    // `ui/elements/herodisplay` is placed at `(0.5, 0.5)` with an x-index of
    // `i-((#selectedHeroes-1)/2)-1` (`ui/heroselect.lua:355`), so with three heroes the cards sit at
    // screen centre and one card-width either side. Measured on a live screen: 508, 958, 1408 —
    // centre +/- 450.
    //
    // Then `Space`, because `heroselect.lua:335` binds `mousereleased = userFunctions.affirmative`.
    // Select-then-confirm, exactly like the reward screen.
    //
    // **The middle card, and the reason is honesty about what we can read.** Today's three were
    // 6/12, 20/20 and 4/4 — a spread that decides runs, and health has ended every run this project
    // has attempted. Choosing on it means reading those numbers off the screen, which is OCR and is
    // not built. The middle card is picked because it needs no arithmetic, not because it is best;
    // that it was the 20/20 Warrior today is luck and must not be mistaken for a policy.
    // Aim at the NAME band, deliberately low on the card.
    //
    // The card body is a generous target, but it is not uniform: each card carries two `small`
    // buttons along its top — "Randomise cosmetics" and "Save hero card", at `yOffset -5.4`
    // (`ui/heroselect.lua:357-380`), which render around y=268. Hitting either does something
    // plausible-looking and useless, and the first attempt came back with three recoloured champions
    // and no selection, so that is not hypothetical.
    //
    // Aim at the **character sprite**, not the card and not the name.
    //
    // y=520 — the name band — was clicked and read back at 0.1543, i.e. nothing selected. The card
    // body at large is not the target: `herodisplay` tracks `bodyHover` (`:44`, `:214-217`) and
    // paints a highlight from it, so the *body* is the hover region and the caption below it is
    // inert. On a live screen the sprite occupies roughly y 250-480, so y=400 is its middle.
    //
    // Still clear of the two `small` buttons along the card top — "Randomise cosmetics" and "Save
    // hero card" at `yOffset -5.4` (`ui/heroselect.lua:357-380`), rendering near y=268 — which an
    // earlier attempt hit, coming back with three recoloured champions and no selection.
    const CARD_SPACING: i32 = 450;
    let (cx, cy) = (960, 400);
    let (sx, sy) =
        r.win.client_to_screen(cx, cy).map_err(|e| format!("cannot reach the card: {e}"))?;
    r.log.push_str(&format!("  choosing the middle champion at ({cx},{cy})\n"));
    // The click's own result, not discarded. The previous version logged "choosing…" and threw the
    // `Result` away with `let _ =`, so the line recorded an *intention* — it could not distinguish a
    // click that landed from one that was refused. That is the whole reason the first attempt was
    // hard to diagnose.
    // Hover FIRST, and do not park until the read-back is done.
    //
    // Two positions were tried — the name band and the sprite — and both read back 0.15, i.e. no
    // selection. Two different places failing identically says the variable is not *where* we
    // clicked. What our click has that a human's does not is a cursor that arrives and leaves in the
    // same instant: `click_at_in` moves, presses and releases, and `park()` warped away immediately
    // afterwards.
    //
    // `herodisplay` selects off `bodyHover` (`:44`, `:214-217`), a flag maintained in `update` from
    // the cursor position. A hover state the game has never had a frame to observe cannot be true
    // when the release is handled — so the dwell before the click is what makes it real, and not
    // parking before the read-back is what stops us erasing it again.
    warp_cursor(sx, sy).map_err(|e| format!("cannot move the cursor to the card: {e}"))?;
    std::thread::sleep(Duration::from_millis(400));
    click_at_in(r.win, sx, sy).map_err(|e| format!("click on the champion card failed: {e}"))?;
    std::thread::sleep(Duration::from_millis(700));

    // Read back: the confirm button only exists once `selectedIndex` is set
    // (`ui/heroselect.lua:333`), so its appearance IS the proof that the click selected something.
    let seen = diggle_solver::act::wait_for(
        r.win,
        &diggle_solver::act::HEROSELECT_CONFIRM,
        diggle_solver::act::HEROSELECT_CONFIRM_PRESENT,
        Duration::from_secs(4),
    );
    if !seen.found() {
        return Err(format!(
            "clicked the champion card but no confirm button appeared (best {:.4} over {} looks, \
             {} capture faults) — nothing was selected",
            seen.best, seen.looks, seen.faults
        ));
    }
    r.log.push_str(&format!("  champion selected — confirm reads live ({:.4})\n", seen.best));
    r.park();

    // Click the confirm button and verify IT went too.
    //
    // Space was tried first, on the grounds that the button declares
    // `userFunctionName = 'affirmative'`. It did not take — sixteen Returns after it changed nothing
    // — and rather than theorise about why, this uses the same shape that has worked everywhere else
    // today: click a verified button, then watch for it to disappear. A press whose consequence is
    // never checked is the single most expensive habit in this codebase.
    let cq = diggle_solver::act::click_exact(
        r.win,
        &diggle_solver::act::HEROSELECT_CONFIRM,
        diggle_solver::act::HEROSELECT_CONFIRM_PRESENT,
    )
    .map_err(|e| format!("could not click the confirm button: {e}"))?;
    r.log.push_str(&format!("  confirmed the champion ({cq:.4})\n"));
    if !diggle_solver::act::wait_until_gone(
        r.win,
        &diggle_solver::act::HEROSELECT_CONFIRM,
        diggle_solver::act::HEROSELECT_CONFIRM_PRESENT,
        Duration::from_secs(8),
    ) {
        return Err("clicked confirm but hero select is still up — the press did not take".into());
    }
    r.log.push_str("  hero select cleared\n");
    std::thread::sleep(Duration::from_millis(1200));
    let _ = CARD_SPACING;

    let deadline = Instant::now() + Duration::from_secs(120);
    for i in 1..=MAX_RETURNS {
        r.pump();
        // **`World loaded` is the arrival. The pregame is a COMBAT screen and belongs elsewhere.**
        //
        // This first waited on `Pregame screen:` and aborted after sixteen Returns, on a run that had
        // already arrived — `World loaded  start  Water campfire` was sitting in the feed.
        //
        // The reason is not that the pregame is merely optional here. It is
        // `core.startCombatPregame` (`overworldview.lua:514`), raised from the combat area button
        // (`:423`) when a fight is entered. It is not part of starting a character at all, so no
        // number of Returns on the hero-select chain could ever produce it: the run has to walk into
        // a fight first. Waiting for it here was waiting for a screen from a different phase of the
        // game.
        //
        // `World loaded` is the adjacency dump, and it is what the caller waits for next anyway.
        if r.feed.seen_since(mark, "World loaded") {
            r.log.push_str(&format!("  reached the overworld after {i} Return(s)\n"));
            return Ok(());
        }
        if r.feed.seen_since(mark, "Pregame screen:") {
            r.log.push_str(&format!("  reached the pregame after {i} Return(s)\n"));
            r.pregame_seen = true;
            // The pregame IS an item-selection screen, and clearing it is what lets the overworld
            // load — so the adjacency dump the caller waits for cannot arrive until this is done.
            let mut il = String::new();
            let picked = diggle_solver::itemchoice::choose(
                r.win,
                &mut r.feed,
                &r.keys,
                game_dir,
                &mut il,
                deadline,
            );
            r.log.push_str(&il.lines().map(|l| format!("    {l}\n")).collect::<String>());
            return match picked {
                Ok(diggle_solver::itemchoice::Chosen::Took(k)) => {
                    r.log.push_str(&format!("  pregame: took **{k}**\n"));
                    Ok(())
                }
                Ok(other) => Err(format!("pregame screen could not be cleared: {other:?}")),
                Err(e) => Err(format!("pregame screen: {e}")),
            };
        }
        if Instant::now() >= deadline {
            return Err(format!("neither `World loaded` nor `Pregame screen:` within 120s ({i} Returns sent)"));
        }
        // Logged because the unlock chain's length varies with `persistentSaveData`, so how many of
        // these it takes is the one number that says which path the profile went down.
        if r.feed.seen_since(mark, "Unlock screen:") && i == 1 {
            r.log.push_str("  unlock screens present — this profile has unlocks pending\n");
        }
        r.keys.focus();
        std::thread::sleep(Duration::from_millis(250));
        let _ = r.keys.press_key(VK_RETURN, SC_RETURN);
        std::thread::sleep(Duration::from_millis(900));
    }
    Err(format!("never reached the overworld after {MAX_RETURNS} Returns"))
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
        // Ask what is on screen before acting on the assumption that it is the map.
        //
        // The navigator assumes "map" and finds out otherwise several steps later, by failing —
        // four `Absent` locate-me readings after a cleared crypt, and a run that clicked into the
        // character inventory and had no way back. Both were one look away from being known.
        //
        // Only screens that need naming are logged. The map is the ordinary case and reporting it
        // every iteration would bury everything else.
        let screen = diggle_solver::act::identify(r.win);
        if screen != diggle_solver::act::Screen::Unknown {
            r.log.push_str(&format!("{step}. screen: {screen:?}\n"));
        }
        if screen == diggle_solver::act::Screen::Character {
            // A dead end: from here the area-button coordinate means `Stats`. The way out is the
            // back arrow in the bottom-right corner.
            match diggle_solver::act::click_exact(
                r.win,
                &diggle_solver::act::CHARACTER_BACK,
                diggle_solver::act::CHARACTER_BACK_PRESENT,
            ) {
                Ok(q) => {
                    r.park();
                    std::thread::sleep(Duration::from_millis(900));
                    r.pump();
                    r.log.push_str(&format!(
                        "  left the character screen via the return arrow ({q:.4})\n"
                    ));
                }
                Err(e) => return Stop::Failed(format!("stuck on the character screen: {e}")),
            }
            continue;
        }
        // The combat pregame: recognised by its `Start`, and the click validated by that button
        // going away.
        //
        // This first watched the console for `Pregame screen:` (`ui/pregame.lua:91`) and pressed
        // Space. That worked, but it could only ever prove the screen had been *constructed* — the
        // announcement-is-not-readiness trap again — and gave no way to tell whether the press took.
        // The fingerprint answers both halves, which is why it is worth the crop.
        if screen == diggle_solver::act::Screen::Pregame {
            r.pregame_seen = true;
            match diggle_solver::act::click_exact(
                r.win,
                &diggle_solver::act::PREGAME_START,
                diggle_solver::act::PREGAME_START_PRESENT,
            ) {
                Ok(q) => r.log.push_str(&format!("{step}. pregame — started the encounter ({q:.4})
")),
                Err(e) => return Stop::Failed(format!("pregame Start refused: {e}")),
            }
            r.park();
            if !diggle_solver::act::wait_until_gone(
                r.win,
                &diggle_solver::act::PREGAME_START,
                diggle_solver::act::PREGAME_START_PRESENT,
                Duration::from_secs(8),
            ) {
                return Stop::Failed("clicked pregame Start but it is still on screen".into());
            }
            std::thread::sleep(Duration::from_millis(800));
            r.pump();
            continue;
        }
        if Path::new(STOP_FILE).exists() {
            let _ = std::fs::remove_file(STOP_FILE);
            r.log.push_str(&format!("{step}. stop requested — ending cleanly
"));
            return Stop::Requested;
        }
        // An event can be up at the top of any iteration, and while one is it owns the screen —
        // clicking the map does nothing and `recentre` times out with "no pan dump". Entering a
        // subworld raises one immediately, which is how a live run got stuck at a village gate it
        // had just walked through. Answer it first, then look at the map.
        r.pump();
        // Text, then options, then whatever the choice raises next.
        //
        // Looped here rather than by falling back to the outer loop, because these chain: a choice
        // commonly leads to another lore screen, so a village gate can be text, text, choice, text
        // before the map is reachable. Re-entering the outer loop for each would spend a quarter of
        // the run's twenty steps standing still — and the step budget exists to stop *wandering*,
        // not to ration reading.
        //
        // Order matters: text gates options. While a lore screen is up the choices do not exist yet,
        // so `parse_events` would find nothing and the map would not be on screen either.
        // A reward screen gates everything behind it exactly as a lore screen does, and it is not
        // always the fight loop that has to deal with it: a fight can end, hand back, and leave the
        // screen standing. Until it is dismissed there is no map, no area buttons and no
        // affirmative, so every later step fails looking for a screen nobody checked for. That is
        // what four `Absent` locate-me readings at 0.23 turned out to be after a cleared crypt.
        //
        // Asked here, in the loop, rather than only where a fight happens to end — same reason
        // dialogue and lore detection live here rather than at a village gate.
        if diggle_solver::itemchoice::on_screen(r.win) {
            r.log.push_str(&format!("{step}. a `Choose one:` screen is up
"));
            let mut il = String::new();
            let picked = diggle_solver::itemchoice::choose(
                r.win,
                &mut r.feed,
                &r.keys,
                &fight.game_dir,
                &mut il,
                deadline.min(Instant::now() + Duration::from_secs(45)),
            );
            r.log.push_str(&il.lines().map(|l| format!("    {l}
")).collect::<String>());
            match picked {
                Ok(diggle_solver::itemchoice::Chosen::Took(key)) => {
                    r.log.push_str(&format!("  took **{key}**
"));
                    // The screen closes into whatever built it -- a postgame after a fight, the map
                    // after a shop -- so let the next pass work out where we are rather than
                    // assuming. It is dismissed; that was the blocking part.
                    std::thread::sleep(Duration::from_millis(800));
                    r.pump();
                }
                Ok(other) => {
                    return Stop::Failed(format!(
                        "a `Choose one:` screen is up and cannot be cleared: {other:?}"
                    ))
                }
                Err(e) => return Stop::Failed(format!("item screen: {e}")),
            }
        }

        // A fight waiting to be finished gates the map exactly as an item screen does, and a save
        // resumed mid-combat drops us straight into one — the run then demanded an overworld dump,
        // found none, and stopped. Twice, on two different saves.
        //
        // Asked visually rather than from `combatSaveData`, whose existence proves nothing: it is
        // the whole RUN's save, removed only at death or postgame (`overworld.lua:968`, `:1154`), so
        // it is present all the time. `Finish` is drawn only in `WaitPhase`
        // (see [`diggle_solver::act::COMBAT_FINISH`]), which is precisely the state that strands us.
        //
        // `Fight::run` is built to join a fight already in progress, so it needs no special entry:
        // it reads the save, sees `WaitPhase`, and clicks Finish itself.
        // Polled, not asked once. Resuming a save lands here while the crypt is still fading in,
        // and a template scores near zero against a half-drawn screen -- so a single low reading
        // means "not yet", not "not there". Asking once is what sent three runs down the map path
        // with `Finish` about to appear behind them.
        let waited = diggle_solver::act::wait_for(
            r.win,
            &diggle_solver::act::COMBAT_FINISH,
            diggle_solver::act::COMBAT_FINISH_PRESENT,
            Duration::from_millis(2500),
        );
        if !waited.found() && (waited.best > 0.3 || waited.faults > 0) {
            // Only worth a line when it was close or we were blind; a flat miss on the overworld is
            // the ordinary case and would drown the log.
            r.log.push_str(&format!(
                "{step}. no Finish (best {:.2} over {} looks, {} capture faults)\n",
                waited.best, waited.looks, waited.faults
            ));
        }
        if waited.found() {
            r.log.push_str(&format!("{step}. a fight is waiting to be finished
"));
            let mut fl = String::new();
            let outcome = fight.run(
                &mut r.feed,
                &r.keys,
                &mut fl,
                deadline.min(Instant::now() + Duration::from_secs(300)),
            );
            r.log.push_str(&fl.lines().map(|l| format!("    {l}
")).collect::<String>());
            match outcome {
                Ok(o) if o.cleared() => r.log.push_str(&format!("  {o:?}
")),
                Ok(other) => return Stop::Fought(format!("{other:?}")),
                Err(e) => return Stop::Failed(e.to_string()),
            }
            let now = r.apply_save();
            if let (Some(b), Some(a)) = (*health, now) {
                r.map.note_health(b, a);
                r.map.rested(a);
                r.map.note_health_level(a);
            }
            *health = now;
            continue;
        }

        for _ in 0..10 {
            if r.clear_text_screen() {
                r.log.push_str(&format!("{step}. cleared a text screen\n"));
                continue;
            }
            match r.handle_event() {
                Some(title) => r.log.push_str(&format!("{step}. answered `{title}`\n")),
                None => break,
            }
        }

        // Locate-me is an overworld control and does not work in a subworld — confirmed live. It is
        // also unnecessary there: the dump fires on every *arrival* (`overworldview.lua:1442`), and
        // inside a subworld we arrive every hop, so the coordinates are already as fresh as they get.
        //
        // The catch is that staleness is silent. An animated pan announces itself when
        // `offsetTransition` completes (`:1253-1255`), but a mouse **drag** writes `xoffset` directly
        // with no transition and no dump (`:1256-1259`) — so a dragged map invalidates every
        // coordinate we hold without saying so. Hence: act on a dump immediately, never hold one
        // across an action, and verify what the click did.
        // Inside a subworld the dump normally arrives free, because we arrive every hop. It does NOT
        // arrive on the hop that never happened: a run resumed inside a subworld has only the
        // `World loaded` dump, whose coordinates are unusable.
        //
        // `verboseAdjacencyData` prints `xoffset+location.posX*zoomMult` (`:1033`), and that dump is
        // emitted at `:1607` — *before* the camera is placed. Measured live: it reported the well at
        // (960, 520) while the well was drawn at (755, 600). Every entry is off by the same
        // translation, which happens to be the player's own world position, and that is the one
        // coordinate the dump never prints. So it cannot be corrected from the dump alone.
        //
        // Locate-me is the way out, and the earlier note here — that it "does not work in a
        // subworld" — was wrong about the cause. The button is simply not on screen: it shares its
        // slot with the location's area buttons. Clicking empty map restores it
        // (`core:mousereleased`, `:1479-1485`), and that branch tests only whether a location was
        // under the release, not which world we are in.
        let inside_now = r.map.inside().is_some();
        let fresh = if inside_now {
            match r.settled_dump(Duration::from_secs(8)).or_else(|| r.recentre()) {
                Some(a) => a,
                None => return Stop::Failed("inside a subworld with no settled dump".into()),
            }
        } else {
            match r.recentre() {
                Some(a) => a,
                None => return Stop::Failed("no pan dump after locate-me".into()),
            }
        };
        let here = r.map.here().unwrap_or("?").to_string();
        let place = r.map.get(&here).cloned();

        // A shrine we could finish: routing brought us here, but consecrating needs a capability
        // this run does not have, so say so rather than pretend.
        if r.map.worth_consecrating_here(&here) {
            r.log.push_str(&format!("{step}. at **{here}** — consecrating is not implemented\n"));
            return Stop::AtShrine(here);
        }

        // Inside a subworld: walk to the exit rather than reaching for it.
        //
        // A `Fight` verdict deliberately falls through to the combat handling below, because it is
        // not a detour — `canTravelToDirect` refuses to move off an incomplete node, so clearing the
        // one underfoot is the only legal move available.
        let mut crossing = None;
        if let Some(container) = r.map.inside().map(|s| s.to_string()) {
            match r.map.cross_toward(&fresh.exits) {
                Some(diggle_solver::overworld::Crossing::Fight { at }) => r.log.push_str(&format!(
                    "{step}. inside `{container}` — `{at}` must be cleared before we can leave it\n"
                )),
                Some(mv) => crossing = Some((container, mv)),
                None => return Stop::Failed(format!("inside {container} with no crossing plan")),
            }
        }
        if let Some((container, mv)) = crossing {
            use diggle_solver::overworld::Crossing;
            let (what, at) = match &mv {
                Crossing::Leave { to } => {
                    match fresh.exits.iter().find(|e| &e.to_key == to) {
                        Some(e) => (format!("leaving `{container}` for `{to}`"), (e.x, e.y)),
                        None => return Stop::Failed(format!("exit to {to} not on screen")),
                    }
                }
                Crossing::Step { to, toward } | Crossing::Explore { to, toward } => {
                    match fresh.nodes.iter().find(|n| &n.key == to) {
                        Some(n) => (format!("crossing `{container}` toward `{toward}` via `{to}`"), (n.x, n.y)),
                        None => return Stop::Failed(format!("{to} is not adjacent on screen from {here}")),
                    }
                }
                Crossing::Retreat { to } => match fresh.nodes.iter().find(|n| &n.key == to) {
                    Some(n) => (
                        format!("hurt and blocked in `{container}` — backing out via `{to}`"),
                        (n.x, n.y),
                    ),
                    None => return Stop::Failed(format!("{to} is not adjacent on screen from {here}")),
                },
                Crossing::Fight { .. } => unreachable!("handled above"),
            };
            r.log.push_str(&format!("{step}. {what}\n"));
            // A node can be adjacent and still be somewhere we must not click. The dump reports
            // positions in screen space regardless of visibility, so an exit can sit off-screen or
            // under the HUD — and clicking one at (213, 18) opened the character screen, after which
            // the area-button coordinate meant `Stats` and the run spent its whole budget there.
            // Not fatal any more: the map can be scrolled until the node is somewhere clickable.
            // Ulrome is bigger than the window — from `l10sub6` the road to l19 sits at (169, 24)
            // under the inventory button, the road to l7 at x=2109, and two more below y=1660.
            //
            // The pan is measured rather than assumed. Scrolling is silent and clamped to the map
            // bounds (`clampWithinBoundsX`, `overworldview.lua:293-297`), so the delta we asked for
            // is an upper bound on the delta we got, and nothing announces the difference.
            let mut at = at;
            if let Ok((cw, ch)) = r.win.client_size() {
                let needs_pan = diggle_solver::observe::hud::chrome_at(at.0 as i32, at.1 as i32, cw, ch)
                    .is_some()
                    || pan::shift_to_reach(at, cw, ch).matters();
                if needs_pan {
                    let want = pan::shift_to_reach(at, cw, ch);
                    match r.pan_map(want) {
                        Some(got) => {
                            at = pan::moved(at, got);
                            r.log.push_str(&format!(
                                "  panned by ({:.0}, {:.0}) of ({:.0}, {:.0}) wanted; `{}` now at ({:.0}, {:.0})\n",
                                got.dx, got.dy, want.dx, want.dy, what, at.0, at.1
                            ));
                        }
                        None => {
                            return Stop::Failed(format!(
                                "{what}: at ({:.0}, {:.0}), out of reach, and the pan could not be \
                                 measured — position is now unknown",
                                at.0, at.1
                            ))
                        }
                    }
                }
                if let Some(chrome) =
                    diggle_solver::observe::hud::chrome_at(at.0 as i32, at.1 as i32, cw, ch)
                {
                    return Stop::Failed(format!(
                        "{what}: still {chrome} at ({:.0}, {:.0}) after panning",
                        at.0, at.1
                    ));
                }
            }
            let Ok((ax, ay)) = r.win.client_to_screen(at.0 as i32, at.1 as i32) else {
                return Stop::Failed("coordinate conversion failed".into());
            };
            let _ = click_at_in(r.win, ax, ay);
            std::thread::sleep(Duration::from_millis(900));
            r.pump();
            let _ = r.click_area_button("Travel (subworld)");
            // Arrival, not pixels.
            //
            // A frame diff over one second called a *successful* move a failure: travel begins with
            // a walk animation that barely changes the screen in that window, so the verdict was
            // 0.002 while the player was in fact walking to the well. The run then reported the
            // village as uncrossable while standing somewhere new.
            //
            // `here` changing is the game's own statement that we moved, and the overworld path has
            // always used it. Text is cleared inside the wait because arrival raises lore and events,
            // and those hold back the dump that would tell us we arrived.
            let by = Instant::now() + Duration::from_secs(30);
            let mut arrived = false;
            while Instant::now() < by && !arrived {
                std::thread::sleep(Duration::from_millis(300));
                r.pump();
                r.clear_text_screen();
                r.handle_event();
                arrived = r.map.here().map(|h| h != here).unwrap_or(false);
            }
            if !arrived {
                return Stop::Failed(format!("no arrival after: {what}"));
            }
            r.log.push_str(&format!("  arrived at `{}`\n", r.map.here().unwrap_or("?")));
            continue;
        }

        // Standing on an unfinished fight: it cannot be walked past.
        //
        // `subworld_container` is the guard that stopped this walking into a forest. A container's
        // heading carries a level and reads exactly like a fight, but its area button ENTERS the
        // subworld (`getLocationButtons` tests `typeData.subworld` first). Clearing one means
        // fighting through its interior, which is a capability this run does not have.
        // ...but only when the subworld is where we are trying to GET TO. Standing on one we are
        // merely walking past is not a reason to stop.
        //
        // This branch used to fire on any container underfoot, and it ended a run that was doing
        // nothing wrong: at 0 health the planner correctly chose the `l7` campfire, the route there
        // crosses Ulrome, and arriving at Ulrome killed the run on the spot. The machinery to cross
        // a subworld already existed and had worked one step earlier in that very log —
        // `l10sub7` out to `l18` — because `cross_toward` runs when we are *inside*. We simply
        // never got inside, having stopped on the doorstep.
        //
        // Falling through instead lets the code below click the area button, which enters the
        // subworld (`getLocationButtons` tests `typeData.subworld` before `basicCombatZone`), after
        // which the next iteration is "inside" and the crossing logic takes over.
        if let Some(p) = place.as_ref().filter(|p| p.subworld_container) {
            let heading_for = r.map.next_hop().map(|h| h.plan.target);
            let stuck_here = heading_for.as_deref().map(|t| t == here).unwrap_or(true);
            if stuck_here {
                r.log.push_str(&format!(
                    "{step}. `{here}` ({}) is the destination and is a subworld — clearing it from \
                     the inside is not implemented\n",
                    p.heading
                ));
                return Stop::AtSubworld(here);
            }
            r.log.push_str(&format!(
                "{step}. `{here}` is a subworld on the way to `{}` — entering to cross it\n",
                heading_for.as_deref().unwrap_or("?")
            ));
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
            // The anomaly is fought like anything else. It is level 8 against a character that came
            // straight here, so losing is the likely outcome — and finding out how badly is the
            // point. A loss cannot be undone in the game, only by restoring a checkpoint.
            let is_anomaly = r.map.anomaly().map(|a| a.key == p.key).unwrap_or(false);
            r.log.push_str(&format!(
                "{step}. fighting {}`{here}` ({})\n",
                if is_anomaly { "**THE ANOMALY** " } else { "" },
                p.heading
            ));
            // Marked BEFORE the click, because `click_area_button` pumps: it sleeps a second and
            // drains the console to measure the screen. The pregame announces itself inside that
            // window, so a mark taken afterwards is already past its own answer.
            //
            // This is the third place the same mistake appeared — the lore counter, the event
            // window, and here. A mark is only meaningful if nothing between it and the read can
            // consume the feed, and almost every helper here consumes the feed.
            let mark = r.feed.mark();
            // Which subworld we were in *before* the click, so that "we are in one" and "this button
            // put us in one" stay distinguishable. Inside a village they are not the same thing at
            // all, and conflating them is what stopped this run: see the loop below.
            let inside_before = r.map.inside().map(str::to_string);
            if !matches!(r.click_area_button("Combat"), Ok(true)) {
                return Stop::Failed(format!("Combat did not open at {here}"));
            }
            // The pregame announces itself; Space there is `Start`.
            // Cheapest, most local signal first.
            //
            // This used to poll for `Pregame screen:` for thirty seconds and only then conclude the
            // button had entered a subworld — inferring one outcome from the *absence* of the
            // other's announcement. Entering a village never prints a pregame, so it always paid the
            // full timeout, and that was the delay before the first text screen of a village.
            //
            // A text screen answers immediately, and reading its button is a 76x92 crop compared
            // against four templates. So ask that first, every pass, and let the console answer only
            // the case it is actually needed for. The remaining timeout covers "the fight is still
            // starting", which is a real wait rather than an inferred one — and ten seconds is
            // generous for a screen that announces itself on `onActive`.
            // Nothing to wait for once it has been used: one pass, then move on.
            let by = if r.pregame_seen {
                Instant::now()
            } else {
                Instant::now() + Duration::from_secs(10)
            };
            let mut pregame = false;
            loop {
                // Pump and test the POSITIVE signal first. The other two branches are inferences
                // from what has not happened, and an inference must never pre-empt an announcement
                // that is already sitting unread in the feed — which is exactly what went wrong:
                // `Pregame screen:` was printed, and this loop returned before ever pumping.
                r.pump();
                if r.feed.seen_since(mark, "Pregame screen:") {
                    pregame = true;
                    r.pregame_seen = true;
                    break;
                }
                // A live affirmative, or a subworld we were NOT in a moment ago: whatever that
                // button did, it was not starting a fight.
                //
                // `inside().is_some()` was the test here, and inside a village it is true before the
                // click as well as after — so it fired every time, on the first iteration, and the
                // run reported "that button entered `l10`" while standing in `l10` already. The
                // pregame it had just opened stayed on screen, and the next step's click found no
                // area button at all. Only a *change* is evidence.
                if r.affirmative().state.is_ready()
                    || r.map.inside().map(str::to_string) != inside_before
                {
                    break;
                }
                if Instant::now() >= by {
                    break;
                }
                std::thread::sleep(Duration::from_millis(150));
            }
            if !pregame {
                // No pregame does not mean failure. `getLocationButtons` tests `typeData.subworld`
                // BEFORE `basicCombatZone` (`overworldview.lua:462-467`), so on a forest or a
                // village that button ENTERED the place instead of starting a fight -- and their
                // headings (`Eight Timberland — level 4 forest`, `Ulrome — level 6 village`) read
                // exactly like fights. Record what we just learned and let the next iteration deal
                // with being inside; failing here left the run standing in a village.
                r.pump();
                let inside_now = r.map.inside().map(str::to_string);
                if inside_now != inside_before {
                    if let Some(container) = inside_now {
                        r.log.push_str(&format!(
                            "  no pregame — that button entered `{container}`, which is a subworld\n"
                        ));
                        continue;
                    }
                }
                return Stop::Failed(format!(
                    "no pregame at {here} and still inside {inside_before:?}"
                ));
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
                deadline.min(Instant::now() + Duration::from_secs(400)),
            );
            r.log.push_str(&log.lines().map(|l| format!("    {l}\n")).collect::<String>());
            match outcome {
                // A win with nothing on offer is still a win, and the node is still cleared. Treating
                // it as a stop would end the run on the ordinary case of a reward roll coming up
                // empty (`utils/world.lua:1287`).
                // A win with nothing on offer is still a win, and the node is still cleared. Whatever
                // screen it left up is not dismissed by the fight -- the top of this loop already
                // presses the bottom-right affirmative for lore screens, and a post-combat screen is
                // the same thing wearing a different backdrop. One place that knows how to press it.
                Ok(Outcome::ClearedWithoutReward { turns }) => {
                    r.log.push_str(&format!("  cleared in {turns} turns, no reward offered\n"));
                }
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
                r.map.note_health_level(a);
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
        let _ = click_at_in(r.win, sx, sy);
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

        let by = Instant::now() + Duration::from_secs(60);
        let mut arrived = false;
        while Instant::now() < by && !arrived {
            std::thread::sleep(Duration::from_millis(300));
            r.pump();
            // Text before options here too. Arrival is detected from an adjacency dump, and a lore
            // screen holds that dump back — so without this the loop would spin out its full 60 s
            // waiting for a map that cannot be drawn until the text is gone.
            r.clear_text_screen();
            r.handle_event();
            arrived = r.map.here().map(|h| h != here).unwrap_or(false);
        }
        if !arrived {
            return Stop::Failed(format!("no arrival at {}", hop.step));
        }
        let now = r.apply_save();
        if let (Some(b), Some(a)) = (*health, now) {
            r.map.note_health(b, a);
            r.map.rested(a);
            r.map.note_health_level(a);
        }
        *health = now;
        let _ = Goal::Explore;
    }
    Stop::Exhausted
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
fn finish(
    game: &mut diggle_solver::game::launch::GameProcess, log: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var("DIGGLE_KEEP_OPEN").as_deref() == Ok("1") {
        std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
        println!("{log}");
        println!("\n-- game left running (DIGGLE_KEEP_OPEN=1); close it before any checkpoint --");
        return Ok(());
    }
    game.close(Duration::from_secs(15));
    std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
    println!("{log}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every point we are willing to click looking for empty map must actually be map.
    ///
    /// The search retries until the locate-me arrow appears, so a bad candidate is not fatal — but a
    /// candidate sitting on chrome would press the character screen or the menu, which is a click
    /// this project has already made once by hand.
    #[test]
    fn every_empty_map_candidate_is_inside_the_map_area() {
        for (x, y) in EMPTY_MAP_CANDIDATES {
            assert!(
                diggle_solver::observe::hud::is_map_point(x, y, 1920, 1080),
                "({x}, {y}) is chrome: {:?}",
                diggle_solver::observe::hud::chrome_at(x, y, 1920, 1080)
            );
        }
    }

    /// The coordinate we click for locate-me must be the one its ButtonSpec resolves to, or we read
    /// one slot and press another.
    #[test]
    fn the_click_point_matches_the_slot_we_read() {
        assert_eq!(
            diggle_solver::win::window::button_center(&affirm::SHOW_AREA_BUTTONS, 1920, 1080),
            SHOW_AREA_BUTTONS
        );
    }
}
