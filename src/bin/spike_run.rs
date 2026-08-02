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
use diggle_solver::overworld::{Goal, WorldMap};
use diggle_solver::win::input::{click_at_in, warp_cursor, Input, PostMessageInput, SC_SPACE, VK_SPACE};
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
        for (n, &(cx, cy)) in EMPTY_MAP_CANDIDATES.iter().enumerate() {
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
    };

    // Timed because the startup felt slow and nobody could say which part was slow. `wait_for_window`
    // returns as soon as the HWND exists, the fixed 3 s after it is a guess, and `click_when_ready`
    // polls `locate` every 400 ms — three candidates, and guessing between them is how this project
    // wastes afternoons.
    let menu_at = Instant::now();
    let found = diggle_solver::act::click_when_ready(
        &win, &diggle_solver::act::CONTINUE, Duration::from_secs(30),
    );
    r.log.push_str(&format!(
        "launch->window+3s {:?}, then Continue took {:?}\n",
        window_at.duration_since(launched),
        menu_at.elapsed()
    ));
    if found.is_err() {
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
    let stop = drive(&mut r, &fight, &mut health, Instant::now() + Duration::from_secs(900));
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
                Crossing::Fight { .. } => unreachable!("handled above"),
            };
            r.log.push_str(&format!("{step}. {what}\n"));
            // A node can be adjacent and still be somewhere we must not click. The dump reports
            // positions in screen space regardless of visibility, so an exit can sit off-screen or
            // under the HUD — and clicking one at (213, 18) opened the character screen, after which
            // the area-button coordinate meant `Stats` and the run spent its whole budget there.
            if let Ok((cw, ch)) = r.win.client_size() {
                if let Some(chrome) = diggle_solver::observe::hud::chrome_at(at.0 as i32, at.1 as i32, cw, ch) {
                    return Stop::Failed(format!(
                        "{what}: its position ({:.0}, {:.0}) is {chrome}, not map — \
                         reaching it needs selection by graph, not by pixel",
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
                if r.affirmative().state.is_ready() || r.map.inside().is_some() {
                    // A live affirmative or a subworld dump: whatever that button did, it was not
                    // starting a fight.
                    break;
                }
                r.pump();
                if r.feed.seen_since(mark, "Pregame screen:") {
                    pregame = true;
                    r.pregame_seen = true;
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
