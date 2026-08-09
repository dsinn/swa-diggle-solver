//! The navigator: the loop that decides what to do with whatever is on screen.
//!
//! Moved here from `bin/spike_run.rs` unchanged. It lived in the binary for as long as it was one
//! spike among many, and by the time it was the main event it had reached 2,226 lines carrying two
//! tests, against 363 in the library — which is to say the most failure-prone code in the project
//! was the least reachable by anything but a live run. Every regression in it has been found by
//! driving the real game, and the comments below are largely the record of those runs.
//!
//! The binary keeps what a binary is for: reading config, launching the game, and writing the
//! report. [`drive`] and [`Run`] are the parts worth testing.
//!
//! ## The shape of one step
//!
//! 1. **Ask what screen this is** ([`crate::act::identify`]), before acting on any assumption about
//!    it. The navigator used to assume "map" and discover otherwise several steps later by failing.
//! 2. **Answer whatever is not the map** — a dead end to leave, an event to decide, a fight to play.
//! 3. **Only then plan a hop**, and take one.
//!
//! Step 1 is the one that keeps being skipped and keeps costing runs. [`ESCAPES`] turns the part of
//! step 2 that is purely "press the button that leaves" into a table, so that "is every dead end
//! answered" is a question with an answer rather than a chain of near-identical branches. Screens
//! needing more than a button — a fight to play, a menu worth a second look — keep their own code.
//!
//! ## Before you add anything that stops the run walking in circles
//!
//! **Read `docs/superpowers/notes/navigation-loops.md` first.** Seven two-node cycles have been
//! found and fixed here, and **two of the fixes were guards whose trigger condition could never
//! occur** — including one in this file that counted consecutive retreats, a state the system cannot
//! reach. Both looked exactly like the anti-cycling measure they were named after.
//!
//! The short version, so this file states its own rule: only two things stop a loop — **explicit
//! memory of what we already tried** ([`Run::committed_to`], [`Run::shrines_tried`],
//! `WorldMap::crossing_to`, `WorldMap::abandon`) and **a monotone progress
//! measure**. A ranking is neither: a stable preference between two alternating states *is* a cycle.
//! If you are adding a guard, state the condition that trips it and prove that condition is
//! reachable.
//!
//! And check the ranking runs at all before improving it. The seventh cycle was `exit_toward`
//! ranking a subworld's exits by distance to the target — from inside the subworld, where our graph
//! reaches no surface node, so the ranking was dead code and the door came from the fallback beneath
//! it. Nothing in the routing said so; only measuring did.

use crate::act::{Button, Screen};
use crate::fight::{Fight, Outcome};
use crate::observe::adjacency::{self, Adjacency};
use crate::observe::affirm;
use crate::observe::event;
use crate::observe::feed::Feed;
use crate::observe::pan;
use crate::overworld::{Goal, WorldMap};
use crate::win::input::{
    click_at_in, warp_cursor, Input, PostMessageInput, SC_RETURN, SC_SPACE, VK_RETURN, VK_SPACE,
};
use crate::win::window::GameWindow;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const FRAMES: &str = "spike-frames-live";
const AREA_BUTTONS: crate::win::capture::Region =
    crate::win::capture::Region { nx: 0.0, ny: 0.68, nw: 0.45, nh: 0.18 };
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
/// A budget, not a target. Run 4 reached the anomaly trigger on step 20 and died there — the cap
/// expired on the exact step the interesting part began, so the cinematic skip below it had never
/// run live. Three shrines and two fights cost that run its whole budget; getting past the trigger
/// and into the anomaly needs roughly twice as much.
///
/// This bounds a program that holds the real mouse and keyboard, so it stays finite. Drop
/// `.diggle-stop` in the working directory to end a run early — it is read between steps, where
/// nothing is half-done.
const MAX_STEPS: usize = 45;

/// How many times to click a node, re-deriving its coordinates between tries, before giving up.
///
/// Three: one for the coordinate we were given, and two for coordinates the game is asked to restate
/// after the map has been re-centred. A fourth would be re-asking a question already answered twice —
/// if two fresh dumps in a row put the node somewhere a click does not select it, the problem is not
/// staleness.
const SELECT_RETRIES: usize = 3;
/// How much of the area-button strip must change for a click to count as having selected something.
///
/// **This threshold is not calibrated and is deliberately unchanged from the value that failed.** It
/// was 0.01 when a run clicked empty ocean, passed this check anyway, and pressed Space into an empty
/// selection. Raising it blind risks the opposite failure — refusing a selection that did happen —
/// so it keeps its old value until a live measurement says what a real `Travel` and a real miss
/// actually score. The retry above is what makes the weak check survivable in the meantime: a miss
/// that slips through now costs one arrival wait rather than the run.
const SELECT_MOVED: f64 = 0.01;
// A landmark-matching drift correction lived here: lift a textured patch when a dump is adopted,
// find it again before each click, and shift the coordinate by however far the map had moved. It
// worked, and it was the wrong tool. The search cost is the search area times the template — around
// a million candidate positions against 96x96 pixels — which showed up immediately as long pauses
// between moves, paid on every click to insure against a displacement that had been seen once.
//
// Pressing the arrow answers the same question for the price of a click, because the map's position
// stops mattering once the game has just restated it. The retry below does that, and the run that
// followed cleared the exact hop the drift had killed without the correction ever firing.

/// How many consecutive locate-me misses to sit through before calling it a stall.
///
/// Three, each a second apart. A screen transition is half a second nominally
/// (`utils/defaultconfig.lua:5`) and the loop re-identifies between attempts, so three covers a slow
/// fade several times over while still failing promptly when the map really is not there.
const RECENTRE_RETRIES: usize = 3;

/// `PartialEq` is for the tests: comparing what [`precheck`] returns against what it should is the
/// only way to check `drive`'s wiring without a running game in front of it.
#[derive(Debug, PartialEq)]
pub enum Stop {
    /// Someone asked us to stop, via [`STOP_FILE`].
    Requested,
    AnomalyBeaten,
    // `AtShrine` used to live here: arriving at a shrine ended the run, because finishing one needed
    // a capability this program did not have. It does now, so a shrine is a detour rather than a
    // terminus — see the arrival branch in `drive`.
    /// Standing on a subworld whose interior we cannot yet clear.
    AtSubworld(String),
    NoPlan,
    Failed(String),
    Fought(String),
    /// The character died. Distinct from [`Stop::Fought`] because it is not a fight that went badly,
    /// it is the end of the save — nothing after it can be retried, and the sandbox needs a
    /// checkpoint restore before another run means anything.
    Died(String),
    /// A screen [`crate::act::identify`] recognises and nothing answers. See [`Answer::Unanswered`].
    ///
    /// Deliberately a stop and not a `todo!()`. Panicking is the idiomatic Rust for "not written
    /// yet", and it is the wrong tool while this process holds the real mouse and keyboard: an
    /// unwind skips [`Stop`]'s whole shutdown — the report, the `gave-up.png` capture of the screen
    /// that beat us, and closing the game — and leaves the window foregrounded silently eating
    /// whatever the user types next. A named stop loses nothing and keeps all of it.
    Unanswered(Screen),
    /// Too hurt to start the fight in front of us, and no rest was found before we got here.
    ///
    /// A stop, not a failure: the run is intact and a checkpoint restore is not needed. It is the
    /// deliberate alternative to [`Stop::Died`], which is what happened the one time this was not
    /// checked — a run walked from `l41` onto a level-6 crypt at 1/20 and was killed by it.
    TooHurtToFight(String),
    Exhausted,
}

/// A screen that is a dead end, and the one button that leaves it.
///
/// Three of these were hand-written branches identical to the line — click one known button, settle,
/// log, `continue`, and on refusal stop with "stuck on X". The only thing that varied between them
/// was which button and what to call it, which is data.
///
/// Not every screen fits here and none is forced to. A fight has to be played, the main menu needs a
/// retry because a highlighted `Continue` scores differently from the template it was cut from. Those
/// stay as their own branches; this covers the ones where leaving *is* the whole response.
/// How many looks to take inside a subworld before concluding we are stuck.
///
/// Each look is a full [`crate::act::identify`] pass plus a console pump, so three of them across
/// two seconds is a real search of the screen rather than a formality. If none of them recognises
/// anything and no dump arrives, the run genuinely has nothing to go on and should say so.
const MAX_DUMP_MISSES: usize = 3;

/// How long to leave between looks. See [`MAX_DUMP_MISSES`].
///
/// A second, because what we are waiting out is a screen transition -- a fight rendering, a dialogue
/// fading in -- and those resolve in well under that. Long enough not to spin, short enough that a
/// screen appearing mid-wait is caught almost immediately rather than after eight seconds.
const DUMP_RETRY_PAUSE: Duration = Duration::from_secs(1);

pub struct Escape {
    /// What [`crate::act::identify`] calls it.
    pub screen: Screen,
    pub button: &'static Button,
    pub threshold: f64,
    /// Names the screen in the log line and in the failure, e.g. `the shrine screen`.
    pub what: &'static str,
}

/// Every dead-end screen, and how to get off it.
///
/// The prose on each entry is the reason it exists. Two of the three were reached *by accident* by a
/// live run, and the failure each produced pointed somewhere else entirely — which is worth more to
/// the next reader than the four lines of clicking it replaces.
pub const ESCAPES: &[Escape] = &[
    // From here the area-button coordinate means `Stats`, so a run that does not notice presses
    // `Stats` instead of travelling. The way out is the back arrow in the bottom-right corner.
    Escape {
        screen: Screen::Character,
        button: &crate::act::CHARACTER_BACK,
        threshold: crate::act::CHARACTER_BACK_PRESENT,
        what: "the character screen",
    },
    // Reached by accident straight after finishing a shrine and clearing the text screen behind it.
    // Like the character screen it is a dead end with no map, and the failure it produced was
    // thoroughly misleading: four locate-me probes at 0.15 and a stop reading `no pan dump after
    // locate-me`, which describes the map being unreadable rather than absent, and sends you looking
    // at panning.
    Escape {
        screen: Screen::StatsHistory,
        button: &crate::act::STATS_BACK,
        threshold: crate::act::STATS_BACK_PRESENT,
        what: "the stats history page",
    },
    // Normally the moment after `Pray`, where the slot now holds a greyed `Consecrate` and the only
    // thing left to do is leave. `shrineplay::play` deliberately stops at the Pray press and hands
    // the aftermath back here, so this is the ordinary exit rather than an error path.
    Escape {
        screen: Screen::Shrine,
        button: &crate::act::SHRINE_GOBACK,
        threshold: crate::act::SHRINE_GOBACK_PRESENT,
        what: "the shrine screen",
    },
];

/// Where a recognised screen gets answered.
///
/// The point of naming this at all is [`answer_for`]'s `match`, which is exhaustive: a variant added
/// to [`Screen`] does not compile until somebody decides what happens when the game shows it.
///
/// That is the check this project was missing. `Screen` had no in-combat variant, so a run that
/// entered a fight from an overworld event fell through to "assume map" and spent its whole budget
/// probing for one; the fix cost a live run and a dead character to find. Under an exhaustive match
/// the same omission is a build failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// One of the [`ESCAPES`]: press its button and look again.
    Escape,
    /// [`drive`] hands it to [`crate::fight::Fight::run`].
    Fight,
    /// [`drive`] has a branch of its own for it.
    Bespoke,
    /// Answered before `drive`'s screen check ever sees it. The string names what does it, so that
    /// "handled" is a claim with an address rather than a shrug.
    Elsewhere(&'static str),
    /// The ordinary case: no fingerprint, so get on with planning a hop.
    Map,
    /// Recognised by [`crate::act::identify`] and answered by nobody.
    ///
    /// Not a placeholder to be filled in silently — [`tests::the_unanswered_screens_are_only_the_ones
    /// _we_have_admitted_to`] pins the list, so shrinking it is deliberate and growing it is loud.
    Unanswered,
}

/// What answers each screen, as of now.
///
/// Exhaustive by construction. Prefer `Elsewhere` with a name over `Bespoke` when the answer is not
/// in [`drive`] at all: several screens never reach the screen check because something upstream has
/// already dealt with them, and recording *where* is the difference between this being a map and
/// being a wish.
pub const fn answer_for(screen: Screen) -> Answer {
    match screen {
        Screen::Character | Screen::StatsHistory | Screen::Shrine => Answer::Escape,
        // The one `drive` plays out itself, because nothing else is watching for it.
        Screen::CombatEntered => Answer::Fight,
        // Reached through the affirmative slot rather than through `identify`: `drive` waits on
        // `Finish` and hands the fight over on `waited.found()`.
        Screen::CombatWaiting => Answer::Elsewhere("drive's wait on the Finish slot"),
        // Both are inside a fight's own loop -- `take_reward` for the item screen, and the postgame
        // dismissal after it. `drive` never sees either.
        Screen::ItemChoice => Answer::Elsewhere("Fight::take_reward"),
        Screen::Postgame => Answer::Elsewhere("Fight::run, after the reward"),
        // Clicked through by `start_new_run`, which knows it is there because `Pregame screen:`
        // arrives on the console afterwards. It is never identified by sight.
        Screen::HeroSelect => Answer::Elsewhere("start_new_run"),
        Screen::MainMenu | Screen::Pregame | Screen::Unlock => Answer::Bespoke,
        Screen::Unknown => Answer::Map,
        // Death has no answer in the navigator. `Outcome::Died` catches it from the console *during*
        // a fight (`fight.rs`), which is where it has always happened so far -- but a run standing on
        // a death screen outside one would fall through to the map path and probe for a map that is
        // not there. The fingerprint for it exists (`slot_is_eulogise`); nothing consults it.
        //
        // Left honest rather than quietly mapped to `Bespoke`. It is a real gap and the test below
        // makes sure it stays visible.
        Screen::Dead => Answer::Unanswered,
    }
}

/// What [`drive`] must do about a screen before anything else looks at it.
///
/// Split out as a pure function so the wiring is testable: without it, "an unanswered screen stops
/// the run" would be a claim about a loop that needs a live game to enter. [`answer_for`] is a map,
/// and a map nothing reads is a comment.
///
/// Only [`Answer::Unanswered`] stops. [`Answer::Elsewhere`] deliberately does not: seeing one of
/// those in `drive` means the component that owns it has already finished — a reward screen still up
/// after a fight, say — and those resolve on the next iteration rather than being errors.
pub fn precheck(screen: Screen) -> Option<Stop> {
    match answer_for(screen) {
        Answer::Unanswered => Some(Stop::Unanswered(screen)),
        Answer::Escape | Answer::Fight | Answer::Bespoke | Answer::Elsewhere(_) | Answer::Map => None,
    }
}

pub struct Run<'a> {
    pub win: &'a GameWindow,
    pub keys: PostMessageInput,
    pub feed: Feed,
    pub reader: adjacency::Reader,
    pub map: WorldMap,
    pub latest: Option<Adjacency>,
    /// How many adjacency dumps have been parsed, ever.
    ///
    /// [`Run::latest`] is sticky — it is set when a dump arrives and never cleared — so "is there a
    /// dump in hand whose reason mentions a pan" is answered `true` by a dump that arrived minutes
    /// ago. `recentre` asked exactly that question after pressing the arrow and so could return
    /// coordinates from before the press, having proved nothing about the press at all. A run died on
    /// it: six recentres reported success while the console printed five `Screen pan finished` lines
    /// all told, three of them caused by arrivals rather than by the arrow.
    ///
    /// A monotonic count fixes it without any timestamp bookkeeping: take the count before the click,
    /// and only a dump that pushes it higher can answer for that click.
    pub dumps: usize,
    pub save_dir: PathBuf,
    pub log: String,
    /// The game's own `right-*.png` artwork, for reading the affirmative slot off the screen.
    ///
    /// This replaced a tally of `Lore screen:` console lines. The tally could only ever answer "how
    /// many text screens were constructed", when what a press needs to know is "is a live control on
    /// the screen right now" — and it was open-loop, so our count and the game's reality could only
    /// drift. See [`crate::observe::affirm`].
    pub affirm: affirm::ButtonArt,
    /// Consecutive locate-me probes that produced no dump. Reset by any success.
    ///
    /// Consecutive is the point: a miss while a screen fades in is ordinary and self-correcting, and
    /// only a run of them means the map is genuinely not coming.
    pub recentre_misses: usize,
    /// Shrines we have already tried to play this run.
    ///
    /// `used` is the real "this shrine is finished" flag, and it is the game's, which is why it is
    /// trusted for planning. But it is only set by a *successful* pray — so a shrine we walked into
    /// and failed to solve stays unused, the arrival branch fires again on the next iteration, and
    /// the run spends its whole budget re-entering the same puzzle. This is the difference between
    /// "there is nothing left to do here" and "we already had our go".
    pub shrines_tried: std::collections::HashSet<String>,
    /// Area-slot captures already taken, so a template is photographed once rather than every step.
    pub slots_captured: std::collections::HashSet<String>,
    /// Consecutive turns inside a subworld with no usable adjacency dump.
    ///
    /// Reset on every dump that does arrive, so this counts a *run* of misses rather than a total.
    /// See where it is incremented for why a miss is a reason to look at the screen rather than to
    /// give up.
    pub dump_misses: usize,
    /// The destination the last hop was taken **for**, as opposed to the node it stepped to.
    ///
    /// Exists to close a blind spot that every planner branch shares: [`WorldMap::next_target`]
    /// excludes `here`, so the moment we arrive somewhere, that place stops being a reason to be
    /// there. For a node whose whole value is the thing we do on it — a fight — that means arriving
    /// and immediately re-planning to somewhere else, then being sent straight back.
    ///
    /// `must_fight_here` did not catch it, because it asks whether leaving is *legal*
    /// (`canTravelToDirect` needs one endpoint complete, `overworldview.lua:1316-1321`) and leaving a
    /// level 1 forest with two cleared neighbours is perfectly legal. Legal to leave and pointless to
    /// leave are different questions, and only the first was being asked.
    ///
    /// So the run remembers what it set out for. Arriving at it means acting on it.
    pub committed_to: Option<String>,
    /// An answered event has started the anomaly cinematic and it has not been skipped yet.
    ///
    /// A flag rather than a return value, because *which* caller answers the rumble is an accident of
    /// timing. `handle_event` is called from three places and only one of them looks at what it
    /// returns; the arrival wait — the loop most likely to be running when the ground rumbles, since
    /// the trigger fires mid-hop — calls it bare and discards the title. So the event was answered,
    /// `answered_event` was set, the outer loop's call correctly reported nothing new, and the skip
    /// that this whole capability exists for never ran. Recording the fact on the run makes the
    /// trigger independent of who noticed it.
    pub pending_cinematic: bool,
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
    pub answered_event: Option<usize>,
    /// Have we already been through the pregame screen in this save?
    ///
    /// It appears once per save — the item-selection screen before the first fight — and never
    /// again. So after the first one there is nothing to wait FOR, and the wait can collapse to a
    /// single immediate look. The branch decision is deliberately unchanged: "no pregame" still has
    /// to be distinguished from "that button entered a subworld", and conflating the two would send
    /// a real fight down the subworld path.
    pub pregame_seen: bool,
}

impl Run<'_> {
    pub fn pump(&mut self) {
        let new: Vec<String> = self.feed.pump().to_vec();
        for a in self.reader.push(&new) {
            self.map.fold(&a);
            self.latest = Some(a);
            self.dumps += 1;
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
        let before = crate::win::capture::capture_window(self.win).ok()?;

        // Clear of the chrome, and spread so that a map blank around one is textured around another.
        let spots = [(760, 300), (1100, 500), (500, 620), (1300, 250)];
        let (patch, taken_at) = spots
            .iter()
            .filter(|(x, y)| crate::observe::hud::is_map_point(*x, *y, cw, ch))
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
        if crate::win::input::drag_in(self.win, from, to, 8).is_err() {
            return None;
        }
        std::thread::sleep(Duration::from_millis(300));

        let after = crate::win::capture::capture_window(self.win).ok()?;
        // Search generously: the clamp can swallow most of the requested movement, so the patch may
        // barely have moved at all, and the honest answer to that is a small measured shift.
        let radius = (want.dx.abs().max(want.dy.abs()) as i32 + pan::PATCH).max(200);
        pan::measure(&after, &patch, taken_at, want, radius)
    }

    pub fn apply_save(&mut self) -> Option<crate::rest::Health> {
        let save = crate::game::save::load(&self.save_dir.join("mainSaveData")).ok()?;
        self.map.apply_save(&save);
        crate::rest::Health::from_save(&save)
    }

    /// Clicks empty map to raise "locate me", clicks it, and waits for the pan to FINISH.
    ///
    /// The waiting is the point. Locate-me centres without `instant` (`overworldview.lua:491`), so
    /// it animates, and input during the animation goes nowhere — that is what made a Space vanish
    /// against a visible Combat button. `Screen pan finished` (`:1255`) is the signal.
    ///
    /// ## What the signal does NOT cover
    ///
    /// That line is printed only when an animated `offsetTransition` completes
    /// (`overworldview.lua:1249-1256`). Three other paths move the map and say nothing: the drag and
    /// hotspot writes (`:1263-1265`) set `xoffset` directly, and `UpdateZoom` recentres with
    /// `instant` (`:1095`). So silence here means "no animated pan finished", never "the map has not
    /// moved", and a dump is evidence about the instant it was printed rather than a standing fact.
    ///
    /// The arrow being on screen is likewise not evidence that it can be pressed. It carries
    /// `activeIf = core.getInteractionEnabled` (`:487`) and is *shown* by `clearToShowAreaButton`
    /// (`:497-502`), which is exactly what `setInteractionEnabled(false)` calls — so the moments it
    /// is most visible include the moments it is inert. Pressing it then does nothing at all, which
    /// is indistinguishable from a press that worked unless a fresh dump is demanded afterwards.
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
            if !crate::observe::hud::is_map_point(cx, cy, cw, ch) {
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
            // Counted BEFORE the click. Anything already in hand describes the map as it was, and the
            // whole point of pressing the arrow is to learn where the map is now.
            let before = self.dumps;
            let _ = click_at_in(self.win, lx, ly);
            self.park();
            let by = Instant::now() + Duration::from_secs(12);
            while Instant::now() < by {
                std::thread::sleep(Duration::from_millis(250));
                self.pump();
                if self.dumps > before {
                    if let Some(a) = self.latest.as_ref().filter(|a| a.reason.contains("pan")) {
                        return Some(a.clone());
                    }
                }
            }
            // Two very different failures reach this line, and the run that needed to tell them apart
            // could not. `mousereleased` on the arrow does `refreshAreaButtons` and
            // `centreScreenOnPlayer` together (`overworldview.lua:485-494`), so a press that lands
            // *replaces* the arrow with the location's buttons. Reading the slot one more time
            // therefore says which half went wrong: an arrow still sitting there was never pressed,
            // an arrow that has gone was pressed and the pan simply went unannounced — and only the
            // second leaves us holding coordinates worth anything.
            let after = self.read_slot(&affirm::SHOW_AREA_BUTTONS);
            self.log.push_str(&format!(
                "  no pan finished after locate-me (candidate {n}); arrow is now {:?} ({:.2}) — {}\n",
                after.state,
                after.score,
                if after.state.is_ready() {
                    "the press did not land"
                } else {
                    "the press landed but no pan was announced"
                }
            ));
        }
        None
    }

    /// Clicks the lone area button, having first proved the strip is showing something.
    /// Photographs the area-button slot, for building fingerprints of what can appear in it.
    ///
    /// The slot is **chrome, not map**: a `default` button at normalized `(0, 0.85)`, so it stays put
    /// however far the map has panned. That is the whole reason it is worth reading — every other
    /// arrival and selection check we have is downstream of a map coordinate, and map coordinates
    /// have now moved silently twice, taking a run with them each time.
    ///
    /// `Combat`, `Attack`, `Enter`, `Gather`, `Rest`, `Open`, `Wake up` and `Visit` all land here
    /// (`affirm.rs:115-119`), and a finished node shows its verb *greyed* rather than showing
    /// nothing. So the fingerprints have to be told apart from each other and from their own greyed
    /// forms — the nearest confusable state is never the background.
    ///
    /// `tag` records what the game's own state says should be in the slot, so the capture arrives
    /// already labelled rather than needing to be identified afterwards.
    fn snap_area_slot(&mut self, tag: &str) {
        // `default` is 250x100 (`ui/elements/button.lua:17`), and [`AREA_BUTTON`] is its centre.
        let (x, y) = (AREA_BUTTON.0 - 125, AREA_BUTTON.1 - 50);
        if let Ok(f) = crate::win::capture::capture_client_rect(self.win, x, y, 250, 100) {
            let path = Path::new(FRAMES).join(format!("slot-{tag}.png"));
            match f.write_png(&path) {
                Ok(()) => self.log.push_str(&format!("  captured the area slot as `{tag}`\n")),
                Err(e) => self.log.push_str(&format!("  could not write the {tag} slot: {e}\n")),
            }
        }
    }

    /// Clicks a button at the position the game's own layout arithmetic puts it.
    ///
    /// For screens with no fingerprint, where the coordinate is derived from the button's
    /// declaration rather than found by looking. The caller is responsible for knowing which screen
    /// is up — every one of these coordinates means something else on the map.
    fn click_button(&mut self, spec: &crate::win::window::ButtonSpec) -> bool {
        let Ok((cw, ch)) = self.win.client_size() else { return false };
        let (x, y) = crate::win::window::button_center(spec, cw, ch);
        self.keys.click(x, y).is_ok()
    }

    /// Pumps the console until a line **equal to** `line` appears, or the deadline passes.
    ///
    /// Equality rather than substring, because these are short wordy announcements — `Rested`,
    /// `Rest screen` — and the console also carries item names and event prose. See
    /// [`crate::observe::feed::Feed::seen_line_since`], and [`crate::fight::GAME_OVER`] for the
    /// false positive that made the distinction worth having.
    fn wait_for_line(&mut self, mark: usize, line: &str, within: Duration) -> bool {
        let by = Instant::now() + within;
        loop {
            self.pump();
            if self.feed.seen_line_since(mark, line) {
                return true;
            }
            if Instant::now() >= by {
                return false;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// Enters the inn we are standing on, rests until full or out of money, and comes back out.
    ///
    /// Every step is confirmed by the console rather than by pixels — see [`crate::innplay`] for the
    /// three lines the game prints and why one press is not one rest. Returns whether any gold was
    /// actually spent on health; the caller re-reads the save either way, because "it did not work"
    /// and "we were already full" both end here and only the save can tell them apart.
    ///
    /// ## Why this is a loop of rounds and not a loop of presses
    ///
    /// **A dream ends the rest screen.** `doRest` can queue an event, and the event's `back` returns
    /// to the *inn*, not to the screen we were pressing on — so every press after a dream lands on
    /// nothing. Live 2026-08-08: 1/20, four presses planned, the first one dreamt, and the run left
    /// at 7/20 with 775 gold still in its pocket and `1 of 4 press(es) landed` in the log. The dream
    /// was detected and woken from correctly; it was treated as the end of the errand rather than an
    /// interruption to it.
    ///
    /// So each **round** opens the rest screen fresh, reads a block, and presses until it is done or
    /// something takes the screen away. Waking from a dream starts another round.
    ///
    /// ## The stop condition is the game's own, not our arithmetic
    ///
    /// "Until full or broke" is `canRest && healthNeed > 0` read from a **freshly opened** rest
    /// screen, which is the one place that block can be trusted: `doRest` logs before its caller
    /// runs `payRestCost`/`updateHealthArmour` (`ui/rest.lua:387-401`), so the block printed *after*
    /// a rest is one step behind, while the one printed on opening is current. `canRest` is the
    /// game's own `getPlayerGold() >= 10` (`:49`), so the money question is answered by the inn
    /// rather than by a gold figure of ours — which matters, because the save is only written on
    /// screen exit and ours goes stale the moment we start spending.
    ///
    /// That staleness can still make [`crate::innplay::presses_needed`] over-count *within* a round.
    /// It is bounded: the button goes inactive, no `Rested` arrives, the round stops, and the next
    /// round's `canRest` answers false.
    fn rest_at_inn(&mut self) -> bool {
        use crate::innplay;

        // 1. `Enter`. It is the ordinary area button, in the slot everything else uses.
        let mark = self.feed.mark();
        let _ = self.click_area_button("Enter");
        if !self.wait_for_line(mark, innplay::ENTERED, Duration::from_secs(8)) {
            self.log.push_str("  rest: `Enter` did not open the inn\n");
            return false;
        }

        // 2. Rounds. Each opens the rest screen from the inn — so each gets a block that is current
        //    — and each ends back on the inn, the dream path included, since the dream's own `back`
        //    lands there.
        let mut done = 0;
        while done < innplay::MAX_PRESSES {
            // Press `Rest` until the screen says it opened. See [`innplay::REST_TRIES`] for why one
            // press is not enough: the inn announces itself from `onActive`, before it can take a
            // click, and a run lost an inn to that and walked to the next village at 7/20.
            let mut opened = None;
            for try_n in 1..=innplay::REST_TRIES {
                let mark = self.feed.mark();
                if !self.click_button(&innplay::INN_REST) {
                    self.log.push_str("  rest: could not click `Rest`\n");
                    break;
                }
                if self.wait_for_line(mark, innplay::REST_SCREEN, Duration::from_secs(4)) {
                    opened = Some(mark);
                    break;
                }
                self.log.push_str(&format!(
                    "  rest: the rest screen did not open (try {try_n} of {})\n",
                    innplay::REST_TRIES
                ));
            }
            let Some(mark) = opened else {
                break;
            };
            let Some(data) = innplay::parse_rest_data(self.feed.since(mark)) else {
                self.log.push_str("  rest: the rest screen printed no `Rest data` block\n");
                self.leave_inn(1);
                break;
            };
            let gold = self.map.gold();
            let presses = innplay::presses_needed(&data, gold).min(innplay::MAX_PRESSES - done);
            self.log.push_str(&format!(
                "  rest: {} missing, {} a press, {gold} gold — pressing {presses} time(s)\n",
                data.health_need, data.health_give
            ));
            // Nothing left to buy. The only exit from this loop that is not a failure, and the
            // answer comes from the inn rather than from our own arithmetic.
            if presses == 0 {
                self.log.push_str(match data.can_rest {
                    false => "  rest: done — the inn will not serve us\n",
                    true => "  rest: done — nothing left to heal\n",
                });
                self.leave_inn(1);
                break;
            }

            // 3. Press. Space rather than the button: `Rest` declares
            //    `userFunctionName = 'affirmative'` (`ui/rest.lua:513`) and `space = 'affirmative'`
            //    (`utils/defaultbinds/keyboard.lua:13`), and reading the binding beats trusting a
            //    coordinate on a screen we have never photographed.
            let mut dreamt = false;
            let mut stalled = false;
            for _ in 0..presses {
                let mark = self.feed.mark();
                if self.keys.press_key(VK_SPACE, SC_SPACE).is_err() {
                    self.log.push_str("  rest: the Space press failed\n");
                    stalled = true;
                    break;
                }
                if !self.wait_for_line(mark, innplay::RESTED, Duration::from_secs(8)) {
                    // The press was swallowed, the button went inactive, or the game is no longer
                    // there. Stopping beats pressing harder at something that is not answering.
                    self.log.push_str("  rest: no `Rested` after the press — stopping here\n");
                    stalled = true;
                    break;
                }
                done += 1;
                // The only field in a `Rested` block that is current, and it is a two-second
                // warning that the screen is about to be taken away from us.
                if innplay::parse_rest_data(self.feed.since(mark))
                    .map(|d| d.doing_event)
                    .unwrap_or(false)
                {
                    self.log.push_str("  rest: a dream is queued — waking from it\n");
                    self.wake_from_dream();
                    dreamt = true;
                    break;
                }
            }
            if stalled {
                self.leave_inn(1);
                break;
            }
            // The dream has already put us back on the inn. Otherwise we are still on the rest
            // screen, and the next round's `Rest` press needs something to hit.
            if !dreamt {
                self.leave_inn(1);
            }
        }
        self.log.push_str(&format!("  rest: {done} press(es) landed\n"));

        // 4. Out of the inn itself.
        self.leave_inn(1);
        done > 0
    }

    /// Presses the back plaque `screens` times, confirming the inn's own announcement in between.
    ///
    /// The rest screen and the inn declare the plaque identically (`ui/inn.lua:68-71`,
    /// `ui/rest.lua:517-520`), so leaving is the same press twice — and the first of the two lands
    /// back on the inn, which says so on the console. That gives the sequence a checkpoint in the
    /// middle instead of two blind clicks in a row.
    fn leave_inn(&mut self, screens: usize) {
        for i in 0..screens {
            let mark = self.feed.mark();
            if !self.click_button(&crate::innplay::BACK) {
                self.log.push_str("  rest: could not click the back plaque\n");
                return;
            }
            // Only the first press has something to confirm. The second lands on the overworld,
            // which announces nothing until it is asked for a dump — the main loop's job.
            if i + 1 < screens && !self.wait_for_line(mark, crate::innplay::ENTERED, Duration::from_secs(6))
            {
                self.log.push_str("  rest: the rest screen did not close\n");
                return;
            }
            std::thread::sleep(Duration::from_millis(600));
        }
        self.pump();
    }

    /// Clicks `Wake up` until the inn announces itself again.
    ///
    /// Blind clicking, and safe only because the console has already told us which screen we are on
    /// — `doingEvent` in the `Rested` block is assigned before the log line (`ui/rest.lua:364-366`),
    /// so it is the one field there that is not a step behind.
    ///
    /// A loop rather than a wait because the button's arrival cannot be predicted: `showIf` returns
    /// `wakeUp`, which is set from an `onCollisionCallbacks` handler
    /// (`overworld/events/rested.lua:56-70`) — when two tiles in the dream's physics simulation
    /// collide. There is no duration to wait out, so we press at intervals and let the game ignore
    /// the presses that land early.
    ///
    /// The exit is the game's, not ours: the dream's `back` runs `setActiveMode(nextMode)` (`:80`),
    /// `nextMode` is the inn mode the rest screen was loaded over, and the inn prints
    /// [`crate::innplay::ENTERED`] from its `onActive`.
    fn wake_from_dream(&mut self) {
        let mark = self.feed.mark();
        // The event fires two seconds into the update loop (`ui/rest.lua:414-419`) and then
        // transitions for another 2.5 (`overworld/events/rested.lua:18`). Clicking before that
        // would land on the rest screen we are still looking at.
        std::thread::sleep(Duration::from_secs(5));
        let by = Instant::now() + Duration::from_secs(60);
        loop {
            if self.wait_for_line(mark, crate::innplay::ENTERED, Duration::from_secs(2)) {
                self.log.push_str("  rest: woke up\n");
                return;
            }
            if Instant::now() >= by {
                self.log.push_str("  rest: still dreaming after a minute\n");
                return;
            }
            self.click_button(&crate::innplay::WAKE_UP);
        }
    }

    fn click_area_button(&mut self, what: &str) -> Result<bool, Box<dyn std::error::Error>> {
        let before = crate::win::capture::capture_window(self.win)?;
        let (bx, by) = self.win.client_to_screen(AREA_BUTTON.0, AREA_BUTTON.1)?;
        click_at_in(self.win, bx, by)?;
        self.park();
        std::thread::sleep(Duration::from_millis(1000));
        self.pump();
        let after = crate::win::capture::capture_window(self.win)?;
        let moved = before.diff_fraction(&after, crate::observe::settle::FULL);
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
    fn read_slot(&self, spec: &crate::win::window::ButtonSpec) -> affirm::Reading {
        let absent = affirm::Reading { state: affirm::State::Absent, score: 0.0, margin: 0.0 };
        let Ok((cw, ch)) = self.win.client_size() else { return absent };
        let (x, y, w, h) = affirm::ButtonArt::crop_rect(spec, cw, ch);
        match crate::win::capture::capture_client_rect(self.win, x, y, w, h) {
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
        // disagree, and that is worth seeing in the log.
        //
        // **Returns false, because nothing was cleared.** This used to return `true`, which was
        // survivable only by accident: the caller's `continue` re-entered an inner loop that gave up
        // after ten tries. Once that `continue` was corrected to re-run the screen check, the lie
        // became a hard stall — a live run sat on the overworld reading a phantom affirmative at 0.62
        // and burned every remaining step re-clearing a text screen that was not there.
        //
        // The honest answer to "did you clear a text screen" is no, and the caller can then fall
        // through to the map path, which is where a run standing on the map belongs.
        self.log.push_str("  affirmative still live after 6 presses — giving up on it\n");
        false
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
        if ev.title.to_ascii_lowercase().contains("rumble") {
            self.pending_cinematic = true;
        }
        self.log.push_str(&format!("  event **{}**: {:?}\n", ev.title,
            ev.choices.iter().map(|c| c.text.clone()).collect::<Vec<_>>()));
        // Read straight from the save rather than plumbed in, because `handle_event` is called from
        // three places and only one of them is holding a health reading. A pure load: deliberately
        // not `apply_save`, which would fold the save into the map as a side effect of answering a
        // dialogue.
        //
        // `mainSaveData` is written on screen EXIT, so this can lag by a node — see
        // [`Run::apply_save`]. That is tolerable here: the question is "are we badly hurt", which
        // does not turn on the last few points, and the reading that mattered in the case this
        // exists for was already `health = 1` two screens earlier.
        let health = crate::game::save::load(&self.save_dir.join("mainSaveData"))
            .ok()
            .and_then(|s| crate::rest::Health::from_save(&s));
        // Below half, by `rest::health_is_low` — the game's own `fear` line (`rpgview.lua:1643`),
        // not a number invented here. Unknown health counts as hurt: the reading is missing exactly
        // when the save is mid-rewrite, and guessing "fine" is how this went wrong the first time.
        let hurt = health.map(crate::rest::health_is_low).unwrap_or(true);
        // Never `choices[0]`: a corrupted village can put "Kill him" first. And never a `[Combat]`
        // option while hurt, which is a different axis and needs asking separately.
        let pick = ev
            .continue_choice()
            .or_else(|| ev.safe_choice_avoiding_combat(hurt))
            .cloned();
        if let Some(c) = &pick {
            if hurt && crate::observe::event::starts_combat(&c.text) {
                // The fallback fired: every option was a fight. Said out loud because it is a
                // decision, not a default — see `Event::safe_choice_avoiding_combat`.
                self.log.push_str(&format!(
                    "  **taking a fight at {} — every option was `[Combat]`**\n",
                    health.map(|h| format!("{}/{}", h.current, h.max)).unwrap_or("unknown health".into())
                ));
            } else if hurt {
                self.log.push_str(&format!(
                    "  hurt ({}), so avoiding any `[Combat]` option\n",
                    health.map(|h| format!("{}/{}", h.current, h.max)).unwrap_or("health unreadable".into())
                ));
            }
        }
        let Some(c) = pick else {
            self.log.push_str("  left alone: more than one real choice\n");
            return Some(ev.title);
        };
        // `onActive` announces a screen at the start of its transition, so settle before clicking
        // and verify, or the click lands on a screen that is still fading in.
        let _ = crate::observe::settle::wait_for_quiescence(self.win, 0.02, Duration::from_secs(8));
        // Taken BEFORE the click, because the shop announces itself inside the wait that follows it.
        // A mark taken afterwards is already past its own answer — the same mistake this project has
        // now made in four places, so it is called out rather than left to be rediscovered.
        let mark = self.feed.mark();
        // **Verified by the plaque going away, not by the screen changing.**
        //
        // This loop used to accept `diff_fraction > 0.05` as proof the answer landed. That is an
        // indirect proxy for the wrong thing — plenty moves on this screen without the event being
        // answered — and a live run at `l9sub22` showed what it costs. It logged `answered
        // Woodsman at Saltagh Park road`, both plaques were still on screen, and the run went off
        // looking for a map dump that could not come because it was not on the map. It failed eight
        // seconds later with `inside a subworld with no settled dump`: a symptom three steps
        // downstream of the cause, pointing at a function that was working correctly.
        //
        // `act::EVENT_CHOICE` answers the question directly. The count comes from the console's own
        // choice list, which is what fixes the plaque's position.
        let options = ev.choices.len();
        // **The positive control, taken before anything is clicked.**
        //
        // `act::EVENT_CHOICE` is one named event's plaque, for the reasons its doc gives, so it
        // scores near zero on a different event — and an absence read without a matching presence
        // would say "answered" for every event we have no template for. That is the original bug
        // wearing the fix's clothes.
        //
        // So: unless the plaque is *seen* first, its later absence proves nothing, and this falls
        // back to the old screen-diff and says which one it used. An unverifiable answer is reported
        // as unverified rather than as success.
        // **Filtered by the threshold, not merely by `Some`.** `find_at_scale_in` returns the best
        // alignment it found, however bad — 0.6154 on an event we have no template for — and the
        // first version of this took its position whenever it was `Some`. So the log said `located`
        // while the coordinate came from a wrong alignment of the Woodsman's plaque against a
        // different event's. It happened to land somewhere plausible, which is worse than landing
        // nowhere: a lucky coordinate is indistinguishable from a correct one until it is not.
        let found = crate::act::event_plaque_find(self.win, options)
            .ok()
            .flatten()
            .filter(|m| m.inliers >= crate::act::EVENT_CHOICE_PRESENT);
        let watched = found.as_ref().map(|m| m.inliers).unwrap_or(0.0);
        let verifiable = found.is_some();
        if !verifiable {
            self.log.push_str(&format!(
                "  no template for this event's plaque ({watched:.4}) — answering unverified\n"
            ));
        }
        let mut answered = false;
        // **Where to click, measured rather than derived.**
        //
        // The console's `posX` is the button's anchor and omits `xOffset`, so on an event with a
        // portrait it names the plaque's left edge — outside the hit test, which is strict. See
        // `Choice::click_point` for that derivation and the live evidence.
        //
        // But deriving it at all is the weaker move. The template match above *is* a picture of the
        // plaque, so it already knows where the plaque is; `cx`/`cy` come straight off the pixels the
        // game drew. That needs no portrait detection, no layout arithmetic, and cannot drift from
        // the game's own positioning. The console still supplies the ORDER of the choices — which is
        // what it is good at — and the pitch between plaques converts that to the n-th one.
        //
        // The derived coordinate stays as the fallback for events we have no template for, where
        // there is nothing to measure.
        let index = ev.choices.iter().position(|k| k.text == c.text).unwrap_or(0);
        let (click_x, click_y) = match (found.as_ref(), self.win.client_size()) {
            (Some(m), Ok((_, ch))) => {
                (m.cx, m.cy + index as i32 * crate::act::event_choice_pitch(ch))
            }
            (_, Ok((cw, ch))) => c.click_point(cw, ch),
            (_, Err(_)) => (c.x, c.y),
        };
        self.log.push_str(&format!(
            "  clicking choice {} at ({click_x},{click_y}) — {}\n",
            index + 1,
            if found.is_some() { "located" } else { "derived from the console" }
        ));
        if let Ok((cx, cy)) = self.win.client_to_screen(click_x, click_y) {
            for attempt in 1..=4 {
                let before = crate::win::capture::capture_window(self.win).ok();
                let _ = click_at_in(self.win, cx, cy);
                self.park();
                std::thread::sleep(Duration::from_millis(900));
                self.pump();
                if verifiable {
                    match crate::act::event_plaque_score(self.win, options) {
                        Ok(q) if q < crate::act::EVENT_CHOICE_PRESENT => {
                            self.log.push_str(&format!(
                                "  answer took on attempt {attempt} ({watched:.4} -> {q:.4})\n"
                            ));
                            answered = true;
                            break;
                        }
                        Ok(q) => self
                            .log
                            .push_str(&format!("  attempt {attempt}: still on the event ({q:.4})\n")),
                        // A capture fault is not evidence either way, so it neither confirms nor
                        // retries into a loop — the count runs out honestly.
                        Err(e) => self.log.push_str(&format!("  attempt {attempt}: {e}\n")),
                    }
                } else {
                    // The old proxy, kept only for events we cannot yet watch. It is weak — plenty
                    // moves on this screen without the event being answered — which is exactly why
                    // it is no longer the primary check.
                    let moved = before
                        .as_ref()
                        .zip(crate::win::capture::capture_window(self.win).ok())
                        .map(|(b, a)| b.diff_fraction(&a, crate::observe::settle::FULL))
                        .unwrap_or(0.0);
                    if moved > 0.05 {
                        answered = true;
                        break;
                    }
                }
            }
        }
        // A `[Shop]` choice is often the *safe* branch — the Woodsman's alternative was
        // `[Combat] - "The book hungers for blood."` — so answering an event correctly can land us in
        // a shop UI, which is not the map and does not become the map on its own. A live run stalled
        // exactly there.
        //
        // Nothing here buys: `crate::buyer::wanted` returns an empty list on purpose, so the shop is
        // a pass-through. Leaving is the whole interaction.
        if self.feed.seen_since(mark, crate::act::SHOP_OPENED) {
            let stock = Vec::new();
            let gold = crate::game::save::load(&self.save_dir.join("mainSaveData"))
                .ok()
                .and_then(|s| s.int_at("player.gold"))
                .unwrap_or(0);
            let buying = crate::buyer::wanted(gold, &stock);
            self.log.push_str(&format!(
                "  shop opened with {gold} gold — buying {} item(s), leaving\n",
                buying.len()
            ));
            let (bx, by) = crate::act::SHOP_BACK;
            let mut left = false;
            for attempt in 1..=3 {
                if let Ok((sx, sy)) = self.win.client_to_screen(bx, by) {
                    let before = crate::win::capture::capture_window(self.win).ok();
                    let _ = click_at_in(self.win, sx, sy);
                    self.park();
                    std::thread::sleep(Duration::from_millis(700));
                    self.pump();
                    // The shop's own backdrop is a full-screen shelf, so leaving moves most of the
                    // window. A weak proxy, and said to be one — the strong check would be a
                    // template for the shelf, which is worth cutting the first time this misbehaves.
                    let moved = before
                        .as_ref()
                        .zip(crate::win::capture::capture_window(self.win).ok())
                        .map(|(b, a)| b.diff_fraction(&a, crate::observe::settle::FULL))
                        .unwrap_or(0.0);
                    if moved > 0.05 {
                        self.log
                            .push_str(&format!("  left the shop on attempt {attempt} ({moved:.3})\n"));
                        left = true;
                        break;
                    }
                }
            }
            if !left {
                self.log.push_str("  **could not leave the shop**\n");
            }
        }
        if !answered && verifiable {
            // Deliberately not a stop: the caller's next move is to look at the screen, and an event
            // we could not dismiss will be found there. What must not happen is this reporting
            // success, which is the whole bug.
            self.log.push_str("  **the event is still on screen** — four attempts, none took\n");
        }
        Some(ev.title)
    }
}

/// Skips the anomaly-opening cinematic by reloading the world from the main menu.
///
/// Proven in `spike_anomaly` and, until now, only ever run there — capability built in a spike and
/// never wired into the thing that needs it, the same way the shrine solver sat unused. A live run
/// reached `You feel the ground rumble` with no way past what follows it.
///
/// The sequence is `Escape -> options -> Menu -> main menu -> Continue`. The game never closes: the
/// world is already on disk, so reloading it *is* the skip. The game's own source says so —
/// `-- so we can save right away and main menu skip` — which is what makes this an intended path
/// rather than an exploit.
///
/// ## The one sanctioned `Escape`
///
/// `Escape` is bound to `backOptions` (`utils/defaultbinds/keyboard.lua:9`), so on a screen with no
/// `goBack` it opens the options menu — and a run that does not know it is in there will keep
/// pressing at a map that is no longer on screen. This is the single place this program sends it,
/// and it is safe here only because the next click is already decided.
///
/// Escape doing nothing is a real outcome, not a failure to retry: input is disabled while the beams
/// play. It is reported so the caller can carry on rather than press blindly into a cutscene.
fn skip_cinematic(r: &mut Run) -> Result<(), String> {
    use crate::win::input::{SC_ESCAPE, VK_ESCAPE};
    let before = crate::win::capture::capture_window(r.win)
        .map_err(|e| format!("capture before Escape: {e}"))?;
    r.keys.focus();
    std::thread::sleep(Duration::from_millis(300));
    r.keys.press_key(VK_ESCAPE, SC_ESCAPE).map_err(|e| format!("Escape: {e}"))?;
    r.park();
    std::thread::sleep(Duration::from_millis(1200));
    r.pump();
    let after = crate::win::capture::capture_window(r.win)
        .map_err(|e| format!("capture after Escape: {e}"))?;
    let moved = before.diff_fraction(&after, crate::observe::settle::FULL);
    if moved < 0.05 {
        return Err(format!("Escape moved the screen {moved:.3} — options did not open"));
    }

    // `Menu`: a `small` 100x100 at ss(1, 0), xOffset -2.63, yOffset 0.38 (`ui/options.lua:333-337`),
    // so (1657, 38) at 1920x1080. Red, top right.
    let (mx, my) =
        r.win.client_to_screen(1657, 38).map_err(|e| format!("Menu coords: {e}"))?;
    crate::win::input::click_at(mx, my).map_err(|e| format!("Menu click: {e}"))?;
    r.park();
    std::thread::sleep(Duration::from_millis(1500));
    r.pump();

    // Park before scoring, or we fingerprint our own cursor. The click that opened this menu leaves
    // the pointer wherever it landed, and the main menu's `Continue` carries a hover state — a
    // brighter button (`hover_alpha`, `ui/elements/button.lua:83`) plus a "Load previously saved
    // data." tooltip drawn underneath it. Neither is in the template, which was captured cold, so a
    // hovered button scored 0.5726 against a 0.90 bar and `click_exact` refused a button that was
    // genuinely there. The refusal was correct — `Restart` is the neighbour and it eulogises the run
    // — but the reading was ours to get right.
    //
    // `NEUTRAL` is empty backdrop on this screen as well as on the map, which is the only property
    // required of it here.
    if let Ok((px, py)) = r.win.client_to_screen(NEUTRAL.0, NEUTRAL.1) {
        let _ = crate::win::input::warp_cursor(px, py);
        std::thread::sleep(Duration::from_millis(400));
    }

    // Verified, not blind: this slot reads `Restart` when it is not `Continue`, and `Restart`
    // eulogises the run.
    crate::act::click_exact(
        r.win,
        &crate::act::CONTINUE,
        crate::act::CONTINUE_PRESENT,
    )
    .map_err(|e| format!("no Continue on the main menu: {e}"))?;
    std::thread::sleep(Duration::from_millis(1500));
    r.pump();
    Ok(())
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
pub fn start_new_run(r: &mut Run, game_dir: &Path) -> Result<(), String> {
    /// Enough Returns for the unlock chain, which is longer on a profile with unlocks pending.
    const MAX_RETURNS: usize = 16;
    let mark = r.feed.mark();

    // Verified click: this slot reads `Restart` when a save exists, and that eulogises the run.
    // `act::click` refuses rather than guessing, which is the entire safety argument.
    let q = crate::act::click_exact(
        r.win,
        &crate::act::MENU_START,
        crate::act::MENU_START_PRESENT,
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
    if !crate::act::wait_until_gone(
        r.win,
        &crate::act::MENU_START,
        crate::act::MENU_START_PRESENT,
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
    let seen = crate::act::wait_for(
        r.win,
        &crate::act::HEROSELECT_CONFIRM,
        crate::act::HEROSELECT_CONFIRM_PRESENT,
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
    let cq = crate::act::click_exact(
        r.win,
        &crate::act::HEROSELECT_CONFIRM,
        crate::act::HEROSELECT_CONFIRM_PRESENT,
    )
    .map_err(|e| format!("could not click the confirm button: {e}"))?;
    r.log.push_str(&format!("  confirmed the champion ({cq:.4})\n"));
    if !crate::act::wait_until_gone(
        r.win,
        &crate::act::HEROSELECT_CONFIRM,
        crate::act::HEROSELECT_CONFIRM_PRESENT,
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
            let picked = crate::itemchoice::choose(
                r.win,
                &mut r.feed,
                &r.keys,
                game_dir,
                &mut il,
                deadline,
            );
            r.log.push_str(&il.lines().map(|l| format!("    {l}\n")).collect::<String>());
            return match picked {
                Ok(crate::itemchoice::Chosen::Took(k)) => {
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

pub fn drive(
    r: &mut Run, fight: &Fight, health: &mut Option<crate::rest::Health>, deadline: Instant,
) -> Stop {
    // Are we already in a fight? The save says so outright.
    //
    // `combatSaveData` exists only while a fight is live — the game deletes it when combat ends,
    // which is why `fight.rs:306` reads an unreadable file as "combat is over". So its presence is
    // the question answered, not a hint to be corroborated. `checkpoint.rs:131` already trusts it for
    // exactly this, and `Fight` was built to join a fight in progress (`fight.rs:10`); the only thing
    // missing was anyone asking at startup.
    //
    // Asked here, before a single frame is captured, because the alternative is inferring it from
    // pixels *through* the main menu still fading out after the launch click — the transient that
    // defeated two resumes, scoring 0.3223 one run and 0.5985 the next. A file on disk has no
    // animation.
    //
    // Startup is also the safe end of the standing rule about this file: never read it while the
    // game is deleting it, which is the moment a reward is confirmed or postgame is dismissed.
    // Nothing is in flight here.
    if fight.combat_path.is_file() {
        r.log.push_str("0. resuming a fight already in progress
");
        let mut fl = String::new();
        let outcome =
            fight.run(&mut r.feed, &r.keys, &mut fl, deadline.min(Instant::now() + Duration::from_secs(300)));
        r.log.push_str(&fl.lines().map(|l| format!("    {l}
")).collect::<String>());
        match outcome {
            Ok(o) if o.cleared() => {
                r.log.push_str(&format!("  resumed fight finished: {o:?}
"));
                let now = r.apply_save();
                if let (Some(b), Some(a)) = (health.clone(), now) {
                    r.map.note_health(b, a);
                    r.map.rested(a);
                    r.map.note_health_level(a);
                }
                *health = now;
            }
            // Not a stop dressed as success: an unresumable fight is worth reporting as itself, so a
            // run that cannot rejoin says so rather than wandering onto the map path and failing to
            // find a map that was never there.
            Ok(o) if o.fatal() => return Stop::Died(format!("resumed: {o:?}")),
            Ok(other) => return Stop::Fought(format!("resumed: {other:?}")),
            Err(e) => return Stop::Failed(format!("could not resume the fight: {e}")),
        }
    }

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
        let screen = crate::act::identify(r.win);
        if screen != crate::act::Screen::Unknown {
            r.log.push_str(&format!("{step}. screen: {screen:?}\n"));
        }
        // A screen we can name and cannot act on. Stopping here is the whole point: the alternative
        // is what this loop used to do with one, which is fall through to the map path and spend the
        // remaining budget probing for a map that is not there, then report the probing as the
        // failure. `Screen::Dead` is the one that qualifies today.
        if let Some(stop) = precheck(screen) {
            r.log.push_str(&format!(
                "  **`{screen:?}` is recognised and nothing answers it** — stopping here rather \
                 than treating it as the map\n"
            ));
            return stop;
        }
        // A fight we did not know we were in. Tested first, because every branch below assumes there
        // is a map underneath the screen, and in combat there is not.
        //
        // This is the gap that stranded a run at `l41`. `handle_event` answered a `Stump in the road`
        // by taking `[Combat] - Cut it down.`, combat started, and nothing on the overworld path
        // watches for that — so the navigator went on probing for a map, four blind locate-me clicks
        // deep, on a combat screen. The console had said `Player turn 0 start` with the whole board
        // in it; nobody was listening, and no button-shaped fingerprint could have helped, because
        // the player was at 1/20 health and the hurt vignette had taken the affirmative slot with it.
        //
        // Recognised by the HUD instead, which the vignette does not reach. See [`act::COMBAT_HUD`].
        if screen == crate::act::Screen::CombatEntered {
            r.log.push_str("  in combat and had not noticed — playing it out\n");
            let mut fl = String::new();
            let outcome = fight.run(
                &mut r.feed,
                &r.keys,
                &mut fl,
                deadline.min(Instant::now() + Duration::from_secs(300)),
            );
            r.log.push_str(&fl.lines().map(|l| format!("    {l}\n")).collect::<String>());
            match outcome {
                Ok(o) if o.cleared() => {
                    r.log.push_str(&format!("  unplanned fight finished: {o:?}\n"));
                    // Same bookkeeping the planned paths do. Skipping it is how a run walks out of a
                    // fight at 1/20 and never considers resting, because nothing recorded the loss.
                    let now = r.apply_save();
                    if let (Some(b), Some(a)) = (health.clone(), now) {
                        r.map.note_health(b, a);
                        r.map.rested(a);
                        r.map.note_health_level(a);
                    }
                    *health = now;
                    continue;
                }
                // Reported as a fight that went wrong, not as a map failure. The whole point of this
                // branch is that "no pan dump after locate-me" was never the truth about this state.
                Ok(o) if o.fatal() => return Stop::Died(format!("unplanned: {o:?}")),
                Ok(other) => return Stop::Fought(format!("unplanned: {other:?}")),
                Err(e) => return Stop::Failed(format!("could not play an unplanned fight: {e}")),
            }
        }
        // Every dead end that is left by pressing one button. See [`ESCAPES`] for which, and why
        // each is in the list.
        if let Some(esc) = ESCAPES.iter().find(|e| e.screen == screen) {
            match crate::act::click_exact(r.win, esc.button, esc.threshold) {
                Ok(q) => {
                    r.park();
                    std::thread::sleep(Duration::from_millis(900));
                    r.pump();
                    r.log.push_str(&format!("  left {} ({q:.4})\n", esc.what));
                }
                Err(e) => return Stop::Failed(format!("stuck on {}: {e}", esc.what)),
            }
            continue;
        }
        // The main menu, which we reach on purpose (the cinematic skip reloads the world through it)
        // and by accident (anything that strands us outside the game). Either way the way out is the
        // same, and it belongs here rather than only inside `skip_cinematic`: that function owned the
        // click, so when its one attempt was refused the run had no second look, fell through to the
        // map path, and blind-probed a menu — which is how it kept opening the almanac.
        //
        // A second look is worth having because the refusal is often transient. Arriving from the
        // options menu leaves `Continue` highlighted, and a highlighted button is not the button the
        // template was cut from: it scored 0.5726 against a 0.90 bar on first arrival, and cleanly on
        // the way back. `click_exact` refusing was right — `Restart` is the neighbour, and it
        // eulogises the run — but a refusal is a reason to look again, not to stop.
        if screen == crate::act::Screen::MainMenu {
            r.park();
            std::thread::sleep(Duration::from_millis(600));
            r.pump();
            // Both renderings, same origin and same click: whichever matches, the action is
            // identical. The plain template is asked first because it is the ordinary case; the
            // highlighted one exists because arriving through the options menu — the skip's own
            // route — leaves the button lit, and that state had no template until it stalled a run.
            let mut hit = crate::act::click_exact(
                r.win,
                &crate::act::CONTINUE,
                crate::act::CONTINUE_PRESENT,
            );
            if hit.is_err() {
                hit = crate::act::click_exact(
                    r.win,
                    &crate::act::CONTINUE_HOT,
                    crate::act::CONTINUE_PRESENT,
                );
            }
            match hit {
                Ok(q) => {
                    r.log.push_str(&format!("{step}. resumed from the main menu ({q:.4})
"));
                    std::thread::sleep(Duration::from_millis(1500));
                    r.pump();
                }
                // Deliberately not a stop. The next iteration re-identifies and tries again, which is
                // exactly what turns a highlighted button into an ordinary one.
                Err(e) => {
                    r.log.push_str(&format!("{step}. main menu, Continue refused: {e}
"));
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
            continue;
        }
        // A class unlock, which arrives unannounced after a fight — a live run was handed "The
        // Cultist class is now available." on the way back from a level 3 crypt. Dismissing it is the
        // whole interaction; the unlock itself is a profile-wide reward that needs nothing from us.
        if screen == crate::act::Screen::Unlock {
            match crate::act::click_exact(
                r.win,
                &crate::act::UNLOCK_CONTINUE,
                crate::act::UNLOCK_CONTINUE_PRESENT,
            ) {
                Ok(q) => {
                    r.park();
                    std::thread::sleep(Duration::from_millis(900));
                    r.pump();
                    r.log.push_str(&format!("  dismissed a class unlock ({q:.4})\n"));
                }
                Err(e) => return Stop::Failed(format!("stuck on the unlock screen: {e}")),
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
        if screen == crate::act::Screen::Pregame {
            r.pregame_seen = true;
            match crate::act::click_exact(
                r.win,
                &crate::act::PREGAME_START,
                crate::act::PREGAME_START_PRESENT,
            ) {
                Ok(q) => r.log.push_str(&format!("{step}. pregame — started the encounter ({q:.4})
")),
                Err(e) => return Stop::Failed(format!("pregame Start refused: {e}")),
            }
            r.park();
            if !crate::act::wait_until_gone(
                r.win,
                &crate::act::PREGAME_START,
                crate::act::PREGAME_START_PRESENT,
                Duration::from_secs(8),
            ) {
                return Stop::Failed("clicked pregame Start but it is still on screen".into());
            }
            std::thread::sleep(Duration::from_millis(800));
            r.pump();
            continue;
        }
        // Skipped here rather than at the call site that answered it, so any caller can arm it.
        // Checked before the screen is identified because the cinematic is precisely a state where
        // identification fails: `utils/events.lua:45-48` pans the camera to (0,0) and disables
        // interaction, which leaves locate-me inert and the map unreadable. The run this came from
        // spent its last four steps asking a panning, uninteractable map where it was.
        if r.pending_cinematic {
            r.pending_cinematic = false;
            match skip_cinematic(r) {
                Ok(()) => r.log.push_str(&format!("{step}. skipped the anomaly cinematic
")),
                Err(e) => r.log.push_str(&format!("{step}. could not skip the cinematic: {e}
")),
            }
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
        if crate::itemchoice::on_screen(r.win) {
            r.log.push_str(&format!("{step}. a `Choose one:` screen is up
"));
            let mut il = String::new();
            let picked = crate::itemchoice::choose(
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
                Ok(crate::itemchoice::Chosen::Took(key)) => {
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
        // (see [`crate::act::COMBAT_FINISH`]), which is precisely the state that strands us.
        //
        // `Fight::run` is built to join a fight already in progress, so it needs no special entry:
        // it reads the save, sees `WaitPhase`, and clicks Finish itself.
        // Polled, not asked once. Resuming a save lands here while the crypt is still fading in,
        // and a template scores near zero against a half-drawn screen -- so a single low reading
        // means "not yet", not "not there". Asking once is what sent three runs down the map path
        // with `Finish` about to appear behind them.
        let waited = crate::act::wait_for(
            r.win,
            &crate::act::COMBAT_FINISH,
            crate::act::COMBAT_FINISH_PRESENT,
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
                Ok(o) if o.fatal() => return Stop::Died(format!("{o:?}")),
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

        // Dismissing something changes the screen, so go back to the top and look again rather than
        // carrying on as if the map were underneath.
        //
        // This used to `continue` the *inner* loop, which only ever meant "clear another text
        // screen" — the screen check at the top of the iteration was never re-run. A live run walked
        // straight through that gap: it finished a shrine, cleared one text screen, and proceeded
        // into the map path on the stats history page, failing four locate-me probes and stopping
        // with `no pan dump after locate-me`. Both new screen handlers existed by then and neither
        // was ever asked, because asking happens at the top and the run never got back there.
        let mut dismissed = false;
        for _ in 0..10 {
            if r.clear_text_screen() {
                r.log.push_str(&format!("{step}. cleared a text screen\n"));
                dismissed = true;
                break;
            }
            match r.handle_event() {
                Some(title) => {
                    r.log.push_str(&format!("{step}. answered `{title}`\n"));
                    // Answering the rumble is what *starts* the cinematic, so this is the moment to
                    // skip it — see `skip_cinematic`.
                    if title.to_ascii_lowercase().contains("rumble") {
                        match skip_cinematic(r) {
                            Ok(()) => r.log.push_str("  skipped the anomaly cinematic\n"),
                            Err(e) => r.log.push_str(&format!("  cinematic skip failed: {e}\n")),
                        }
                    }
                    dismissed = true;
                    break;
                }
                None => break,
            }
        }
        if dismissed {
            continue;
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
        // **Poll, do not wait.** Ask for a dump with no timeout at all, and if there is not one, go
        // back to the top of the loop — where [`crate::act::identify`] gets another look — after a
        // second. Three times, then give up.
        //
        // The eight-second block this replaces is what made the observer useless at exactly the
        // wrong moment. `identify` runs at the top of each iteration and is perfectly stateless;
        // `act::COMBAT_HUD` scores **1.0000** on the frame the last run died holding. But the fight
        // had not rendered when the screen was checked, and by the time it had, the iteration was
        // committed to waiting for a dump that could never come and then exiting. One look, taken
        // too early, and no second chance for eight seconds.
        //
        // A dump also cannot arrive while an event dialogue is up, or a shop, or a class unlock —
        // every stall `inside a subworld with no settled dump` has ever reported was one of those,
        // and the message named `settled_dump`, which was working correctly every time.
        //
        // Polling makes the observer the thing that runs often and the dump the thing that is merely
        // checked for. A screen that appears mid-wait is now seen within a second instead of being
        // missed entirely.
        let fresh = if inside_now {
            // `Duration::ZERO` is one pump and one look — see `settled_dump`, which tests its
            // deadline after checking, so a zero budget still gets a full attempt.
            let polled = r.settled_dump(Duration::ZERO).or_else(|| {
                // `recentre` clicks the map to force a pan dump, so it is worth one try rather than
                // three: a run that is not on the map at all should not be clicking at it repeatedly.
                (r.dump_misses + 1 >= MAX_DUMP_MISSES).then(|| r.recentre()).flatten()
            });
            match polled {
                Some(a) => {
                    r.dump_misses = 0;
                    a
                }
                None => {
                    r.dump_misses += 1;
                    if r.dump_misses >= MAX_DUMP_MISSES {
                        return Stop::Failed(format!(
                            "inside `{}`: no dump, and no screen we recognise, {} looks over",
                            r.map.inside().unwrap_or("?"),
                            r.dump_misses
                        ));
                    }
                    r.log.push_str(&format!(
                        "{step}. no dump inside `{}` — asking the screen again (look {} of {})\n",
                        r.map.inside().unwrap_or("?"),
                        r.dump_misses + 1,
                        MAX_DUMP_MISSES
                    ));
                    std::thread::sleep(DUMP_RETRY_PAUSE);
                    continue;
                }
            }
        } else {
            match r.recentre() {
                Some(a) => {
                    r.recentre_misses = 0;
                    a
                }
                // **Not a stop on the first miss.** A failed locate-me means "no map answered", and
                // the commonest reason by far is that a screen is still arriving — the run that
                // produced this branch stopped on a stats history page caught mid-fade, with the
                // previous screen's furniture still drawn over the top of it. Giving up on the first
                // look diagnoses a transition as a dead end.
                //
                // So: wait a second and go round again. Going round is what matters more than the
                // second — it re-runs `identify` at the top of the loop, which is where a screen
                // that is not the map gets recognised and handled. Retrying the probe in place would
                // only ask the same question of the same wrong screen.
                None => {
                    r.recentre_misses += 1;
                    if r.recentre_misses <= RECENTRE_RETRIES {
                        r.log.push_str(&format!(
                            "{step}. no pan dump — waiting for a transition (miss {} of {})\n",
                            r.recentre_misses, RECENTRE_RETRIES
                        ));
                        std::thread::sleep(Duration::from_secs(1));
                        continue;
                    }
                    return Stop::Failed(format!(
                        "no pan dump after locate-me, {} times over",
                        r.recentre_misses
                    ));
                }
            }
        };
        let here = r.map.here().unwrap_or("?").to_string();
        let place = r.map.get(&here).cloned();

        // A shrine we are standing on whose fight is already won and whose blessing is unclaimed.
        //
        // Taken **on arrival, whatever the errand was**. An uncorrupted shrine is strictly
        // beneficial: the walk is already paid for, and `Pray` hands over wildcard tiles
        // (`doPray` -> `blessings.wildcardRewards`, `shrine.lua:126-131`) which are exactly what a
        // hurt run wants before its next fight. The objective is not disturbed — this returns to the
        // map and the next `next_target` picks up where it left off.
        //
        // `completed` is the shrine's *area*, i.e. its combat, and it is what decides whether the
        // overworld slot holds `Visit` or `Combat` (`overworld/locations/shrine.lua:64,76`). It is
        // NOT the word: `shrineView.hasWon()` is the guess list restored from
        // `<key>subs` (`shrine.lua:495`), which is why a shrine can be `[done]` with the puzzle
        // untouched — which is exactly the state the live run found shrine1 in.
        //
        // `used` is `<key>_used` (`overworldview.lua:215-218`), set by praying, and is the only
        // thing that gates `Pray` once the word is solved (`showPrayButton`, `shrine.lua:98-102`).
        // So "complete and unused" is precisely "there is something here worth doing".
        // The greyed forms, which are the states a fingerprint is most likely to be fooled by: the
        // verb is still drawn, just at half alpha over a different base image
        // (`ui/elements/button.lua:128,131`). A threshold measured only against a live button would
        // read "finished" as "ready" — the failure that a shared slot punishes hardest, because the
        // coordinate that means `Visit` on one node means `Combat` on the next.
        //
        // "Spent" is a different flag for each verb, and conflating them produced two byte-identical
        // captures labelled as opposites. A crypt's `Combat` greys out when its area is `completed`.
        // A shrine's `Visit` does not: `completed` is the shrine's *combat*, and the shrine stays
        // visitable until `used` records that something was prayed for.
        if let Some((p, tag)) = place.as_ref().and_then(|p| {
            if p.type_is("shrine") {
                p.used.then_some((p, "visit-spent"))
            } else {
                p.completed.then_some((p, "combat-spent"))
            }
        }) {
            let _ = p;
            if !r.slots_captured.contains(tag) {
                r.slots_captured.insert(tag.to_string());
                r.snap_area_slot(tag);
            }
        }
        if let Some(p) = place
            .as_ref()
            .filter(|p| p.type_is("shrine") && p.completed && !p.used)
            .filter(|p| !r.shrines_tried.contains(&p.key))
        {
            let key = p.key.clone();
            r.log.push_str(&format!("{step}. at **{key}** — playing the shrine\n"));
            // Standing on an unused shrine, so the slot is showing a live `Visit`.
            r.snap_area_slot("visit-live");
            // Marked before the attempt, not after: a play that panics, times out, or leaves us on
            // an unexpected screen must still count as having had its go, or the guard protects
            // nothing in exactly the cases it exists for.
            r.shrines_tried.insert(key.clone());
            // The planner has to be told as well, and this is the whole reason `abandon` exists.
            // `shrines_tried` only stops us *entering* the shrine again; it says nothing about
            // whether the shrine is worth walking to, and a shrine left unprayed is still unused as
            // far as the save is concerned. With only half the story, the planner kept choosing the
            // one shrine the arrival branch would always decline, and the run ping-ponged between it
            // and the cleared crypt on the way to the next.
            r.map.abandon(&key);
            match crate::shrineplay::play(r.win, &r.keys) {
                Ok(played) => {
                    let log = played.log.clone();
                    r.log.push_str(&log);
                    if !played.prayed {
                        // Not fatal, and deliberately not a stop: the blessing is a bonus, and a run
                        // that cannot claim it should still get on with the anomaly. It is logged
                        // loudly because a shrine we walked to and failed to use is a wasted trip.
                        r.log.push_str("  shrine: left unprayed\n");
                    }
                }
                Err(e) => r.log.push_str(&format!("  shrine failed: {e}\n")),
            }
            // Re-read before anything plans on it. `_used` reaches the save when the shrine screen
            // is *exited*, which the driver has just done, so this is the first moment the flag is
            // readable — see the standing note that a stale read here is timing, not failure.
            r.apply_save();
            continue;
        }

        // A shrine we are standing on that is already used but not consecrated.
        //
        // This branch is why `worth_a_trip`'s `!consecrated` clause is honest. It used to promise a
        // trip that nothing could fulfil: `WorldMap::worth_consecrating_here` existed and was called
        // from nowhere, so the planner routed to a used shrine, `drive` declined it — the play
        // branch above needs `!used` — and the planner sent it straight back. A live run bounced
        // `l10 -> shrine2 -> l10` twelve times and died of it.
        //
        // An uncorrupted shrine is strictly worth it: consecrating costs no fight and is what the
        // `shrineKarma` economy pays out on. The gate is the map's, not this function's, because it
        // is the map that knows whether a *corrupted* one is merely on the way.
        if r.map.worth_consecrating_here(&here) && !r.shrines_tried.contains(&here) {
            r.log.push_str(&format!("{step}. at **{here}** — consecrating\n"));
            // Same discipline as the play branch: marked before the attempt, both here and on the
            // planner, so an attempt that panics or times out still counts as having had its go.
            r.shrines_tried.insert(here.clone());
            r.map.abandon(&here);
            // The artwork capture lives inside `consecrate`, not here: `snap_area_slot` photographs
            // the OVERWORLD slot, and `Consecrate` is on the shrine screen. Calling it from here
            // produced a picture of `Visit` filed under `consecrate-live`.
            match crate::shrineplay::consecrate(r.win, &r.keys) {
                Ok(did) => {
                    r.log.push_str(&did.log.clone());
                    if !did.done {
                        r.log.push_str("  shrine: left unconsecrated\n");
                    }
                }
                Err(e) => r.log.push_str(&format!("  consecrate failed: {e}\n")),
            }
            r.apply_save();
            continue;
        }

        // **Standing on a shrine and doing nothing is the end of it as a destination.**
        //
        // The structural backstop, and the more important half of the fix above. `abandon` existed
        // already but was only ever called from inside the branches that *act*, so any shrine the
        // driver declined stayed a perfectly good target forever and the planner kept re-choosing
        // it. That is a loop whatever the reason for declining, so the guard belongs here — where
        // "we arrived and nothing happened" is known — rather than being added case by case as each
        // new reason to decline appears.
        if place.as_ref().map(|p| p.type_is("shrine")).unwrap_or(false) {
            r.map.abandon(&here);
        }

        // A wizard's tower we are standing on that still has fog to sell. **Placeholder** —
        // `tower::press_reveal` is a stub, so this logs the opportunity and walks on.
        //
        // Wired up now, ahead of the press, because the decision is the part that can be got wrong
        // silently. `Reveal` and `Teleport` share a coordinate (`wizard_tower.lua:52,71`), so the
        // gate has to be computed rather than clicked at — see `tower::Offer`.
        //
        // `used` is a bool here and the flag is a number there, which loses the one case
        // `tower::Offer` can otherwise recover: a tower used before we picked up `towerRange+` is
        // offered again at the wider range (`wizard_tower.lua:56-59`). Mapping true to `Some(0)`
        // with `range = 0` collapses that to `Spent`, which is the safe direction and the same one
        // `Offer::of` documents — we skip a free map rather than press the mode change.
        if let Some(p) = place.as_ref().filter(|p| p.type_is(crate::tower::TYPE_NAME)) {
            let offer = crate::tower::Offer::of(true, p.used.then_some(0), 0);
            if offer == crate::tower::Offer::Available {
                r.log.push_str(&format!(
                    "{step}. at **{}** — `Reveal` is on offer here (NOT IMPLEMENTED, walking on)\n",
                    p.key
                ));
                match crate::tower::press_reveal(r.win) {
                    Ok(true) => {
                        r.apply_save();
                    }
                    Ok(false) => {}
                    Err(e) => r.log.push_str(&format!("  reveal failed: {e}\n")),
                }
            }
        }

        // Inside a subworld: walk to the exit rather than reaching for it.
        //
        // A `Fight` verdict deliberately falls through to the combat handling below, because it is
        // not a detour — `canTravelToDirect` refuses to move off an incomplete node, so clearing the
        // one underfoot is the only legal move available.
        let mut crossing = None;
        // The crossing said this node has to be dealt with. Carried to the fight branch rather than
        // re-derived there from `can_step`, which answers a *different* question — "is any move off
        // this node legal" — and says yes whenever a neighbour is complete, which after a retreat is
        // always true. That gap is why a `Fight` verdict could fall through the fight branch and out
        // the other side.
        let mut must_clear_here = false;
        if let Some(container) = r.map.inside().map(|s| s.to_string()) {
            // Bound rather than matched in place: `cross_toward` records the door it chose, so the
            // borrow it takes would still be live in the arms that call into the map.
            let verdict = r.map.cross_toward(&fresh.exits);
            match verdict {
                Some(crate::overworld::Crossing::Fight { at }) => {
                    r.log.push_str(&format!(
                        "{step}. inside `{container}` — `{at}` must be cleared before we can leave it\n"
                    ));
                    must_clear_here = true;
                }
                Some(mv) => crossing = Some((container, mv)),
                None => return Stop::Failed(format!("inside {container} with no crossing plan")),
            }
        }
        if let Some((container, mv)) = crossing {
            use crate::overworld::Crossing;
            // Standing on the inn we crossed the village for. Nothing to click on the map — the
            // errand is the whole reason we are in here.
            //
            // `abandon` first, and unconditionally, for the same reason the shrine branches do it:
            // this is the driver's record of having had its go. Without it a rest that fails to open
            // a screen leaves the inn a perfectly good destination, `cross_toward` routes straight
            // back to it, and the run spends its budget walking between the gate and the bar. The
            // planner and the driver must not disagree about what is still worth walking to.
            if let Crossing::Arrive { at } = &mv {
                r.log.push_str(&format!("{step}. at **{at}** in `{container}` — resting\n"));
                r.map.abandon(at);
                let rested = r.rest_at_inn();
                // Re-read before anything plans on it: `overworld:save()` runs in the inn's
                // `goBack` (`ui/inn.lua:9`), so leaving is the moment the new health is readable.
                // This is what clears `wants_rest` and lets the run get on with the anomaly.
                if let Some(h) = r.apply_save() {
                    r.log.push_str(&format!(
                        "  health is now {}/{}{}\n",
                        h.current,
                        h.max,
                        if rested { "" } else { " (nothing was spent)" }
                    ));
                }
                continue;
            }
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
                // Its own line, and deliberately not the one above. `Step` and `Explore` already
                // print identically, which made a wrong-door guess read like a considered route in
                // the logs; a search with no destination at all would have been a third.
                Crossing::Seek { to } => match fresh.nodes.iter().find(|n| &n.key == to) {
                    Some(n) => (
                        format!("searching `{container}` for its inn via `{to}`"),
                        (n.x, n.y),
                    ),
                    None => return Stop::Failed(format!("{to} is not adjacent on screen from {here}")),
                },
                Crossing::Fight { .. } | Crossing::Arrive { .. } => {
                    unreachable!("handled above")
                }
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
                let needs_pan = crate::observe::hud::chrome_at(at.0 as i32, at.1 as i32, cw, ch)
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
                    crate::observe::hud::chrome_at(at.0 as i32, at.1 as i32, cw, ch)
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
        if let Some(p) = place.as_ref() {
            // **A village we are hurt in front of is never a dead end.** Entering it is the errand,
            // not a detour on the way to somewhere else — so the "nowhere left to go" test must not
            // be allowed to answer for it. Without this the run reaches the rest it planned and
            // stops on the doorstep in exactly the state the rest was for: `next_target` excludes
            // `here`, and at low health with every remaining node hostile there may be no second
            // choice for it to name.
            //
            // ## This is also the ONLY way we learn a village can be entered at all
            //
            // `subworld_container` is set from a dump taken *inside* (`overworld.rs`), so it can
            // never be true for a village we have not already been in. Standing on the very rest
            // stop we crossed a forest to reach, the driver therefore knew nothing about it, walked
            // away, and `next_target` — which excludes `here` — nominated the next village along.
            // Live 2026-08-08: `l19 -> l27 (for l27, Rest)`, `l27 -> l19 (for l19, Rest)`, four
            // round trips between two adjacent villages, having just crossed `l9` to get to one.
            // Cycle number eight, and the first with a cause outside the routing.
            //
            // A village heading settles it where a forest's cannot. The deliberate rule against
            // inferring a container from its heading — `a_container_is_learned_from_being_inside_
            // it_not_from_its_heading` — exists because `Eight Timberland — level 4 forest` reads
            // exactly like a fight. `Dane village` does not: `village.lua:5,72` is
            // `typeName = 'village'` with `subworld = 'village'`, and `getLocationButtons`
            // (`overworldview.lua:461-468`) returns the subworld button set for it. So the area
            // button below is `Enter`, not `Combat`.
            //
            // **Corrupted is excluded**, and that is the whole of the risk here: `getAreaButtons`
            // is consulted *before* `subworld`, and a village under attack replaces the set
            // (`village.lua:371-395`). We do not know what those buttons are, so we do not press
            // them on spec.
            //
            // ## This is the narrow version of a general capability
            //
            // "Am I hurt, and is this a village" is the wrong question in the long run. The right
            // one is **does this container hold anything I currently want** — which is the same
            // question `WorldMap::cross_toward` answers inside, and answers just as narrowly, with
            // `inn_inside` and `seeking_a_rest` written around the inn specifically.
            //
            // The generic form is an *errand*: a predicate over interior places plus what to do on
            // arriving at one. `Goal::Rest` names the inn (`village.lua:341`); a shop errand names
            // the shop subnode and `buyer::wanted`, which already exists as a deliberate
            // pass-through stub. `Crossing::Arrive` would carry the errand and the driver dispatch
            // on it, instead of `Arrive` meaning "the inn" by construction.
            //
            // Deliberately not built yet — the MVP has one errand, and inventing the abstraction
            // around a single case is how you get the wrong abstraction. Task #18, raised by the dev
            // for the post-MVP shop work.
            let rest_here = r.map.wants_rest()
                && r.map.gold() >= crate::rest::INN_COST
                && p.type_is("village")
                && !p.corrupted;
            if p.subworld_container || rest_here {
                let heading_for = r.map.next_hop().map(|h| h.plan.target);
                let stuck_here =
                    !rest_here && heading_for.as_deref().map(|t| t == here).unwrap_or(true);
                if stuck_here {
                    r.log.push_str(&format!(
                        "{step}. `{here}` ({}) is the destination and is a subworld — clearing it \
                         from the inside is not implemented\n",
                        p.heading
                    ));
                    return Stop::AtSubworld(here);
                }
                match rest_here {
                    true => r.log.push_str(&format!(
                        "{step}. `{here}` ({}) is the rest we came for — entering it\n",
                        p.heading
                    )),
                    false => r.log.push_str(&format!(
                        "{step}. `{here}` is a subworld on the way to `{}` — entering to cross it\n",
                        heading_for.as_deref().unwrap_or("?")
                    )),
                }
                // **Press it here, rather than falling through and hoping.**
                //
                // Falling through worked only by accident, and only for a forest: the area button is
                // pressed further down inside the *fight* branch, which fires on `has_combat() &&
                // !completed`. A forest container's heading carries a level, so it qualified and the
                // `Combat` press turned out to enter the subworld. `Dane village` carries no level
                // and is already complete, so that branch could never fire — the run logged
                // "entering to cross it" and then travelled onward, which is the `l19 <-> l27`
                // bounce with a sentence of narration on top.
                //
                // `Enter` and `Combat` are the same click either way: `click_area_button` presses a
                // fixed position (`AREA_BUTTON`) and the label only reaches the log.
                //
                // **Only the rest case takes this path.** A container we are crossing still falls
                // through to the fight branch, because that is the code that crossed `l9` live on
                // 2026-08-08 and it handles an outcome this does not: the press may open a *pregame*
                // rather than a subworld, and telling those apart is the loop down there. Here the
                // node is a completed, uncorrupted village — there is no fight for it to start.
                if rest_here {
                    let inside_before = r.map.inside().map(str::to_string);
                    if !matches!(r.click_area_button("Enter"), Ok(true)) {
                        return Stop::Failed(format!("Enter did nothing at {here}"));
                    }
                    // Confirm by the *change*, never by "we are in a subworld" — inside a village
                    // that is already true before the click, and asking it that way is what once had
                    // a run report that it had entered somewhere it was standing in. Announcement is
                    // not readiness: the press is not done until the world says it moved.
                    let by = Instant::now() + Duration::from_secs(10);
                    loop {
                        r.pump();
                        if r.map.inside().map(str::to_string) != inside_before {
                            r.log.push_str(&format!(
                                "  inside `{}` now\n",
                                r.map.inside().unwrap_or("?")
                            ));
                            break;
                        }
                        if Instant::now() >= by {
                            return Stop::Failed(format!("no subworld after entering {here}"));
                        }
                        std::thread::sleep(Duration::from_millis(200));
                    }
                    continue;
                }
            }
        }
        // Ask where we are GOING before deciding to fight. The first live run had these the other
        // way round: it saw an unfinished fight underfoot and cleared it unconditionally, when the
        // route ran straight back the way we came. `canTravelToDirect` needs one endpoint complete,
        // and the node behind us always is -- so leaving was legal all along, and the fight it
        // picked was one it walked into for nothing.
        let hop = r.map.next_hop();
        // Three ways this node has to be dealt with rather than walked past.
        //
        // 0. **The crossing said so** — `must_clear_here`, set above. `can_step` cannot answer this
        //    one: it asks whether *a* move off the node is legal, and after a retreat the answer is
        //    always yes, because the node we came from is complete.
        // 1. Leaving is illegal -- the original test, and the one that matters inside a subworld.
        // 2. **We came here on purpose.** `next_target` excludes `here`, so a node's own reason for
        //    existing evaporates the instant we stand on it; without this the run arrives at the
        //    fight it chose, re-plans to somewhere else, and is routed back. Legal to leave and
        //    pointless to leave are different questions and only the first was being asked.
        let arrived_at_target = r.committed_to.as_deref() == Some(here.as_str());
        let must_fight_here = arrived_at_target
            || must_clear_here
            || match hop.as_ref() {
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
            // Nothing used to ask how much health was left before starting a fight, and a run found
            // out what that costs: it cleared `l41`, came out at **1 of 20**, and the planner — with
            // no campfire or village anywhere in its 22 known places — fell through to its documented
            // "get on with the objective" behaviour, walked to `l50`, and clicked Combat on a level-6
            // crypt. Nine turns later the console printed `game over`.
            //
            // The health-first priority was not broken. It had nowhere to route to, and "nowhere to
            // rest" is not a reason to fight anyway.
            //
            // Deliberately blunt: `rest::health_is_low` is below half, so this also refuses fights a
            // healthier judgement would take. That is the safe direction while nothing here models
            // enemy strength — the heading carries a level (`level 6 crypt`) and weighing it against
            // health is the better rule, once something reads it. Unknown health counts as hurt, for
            // the same reason it does when answering an event.
            //
            // ## Three exemptions, and the third is the one the dev added on 2026-08-08
            //
            // The **anomaly** is exempt. It is the objective, it is level 8 against whoever arrives,
            // and losing to it is a documented expected outcome rather than an accident to prevent.
            //
            // A **forest** is exempt, because entering one is not committing to a fight. A forest is
            // a subworld whose interior nodes are peaceful or not individually
            // (`competeOnVisit = subnodeIsPeaceful`, `forest.lua:109,123,137,…`), so crossing one
            // can cost nothing. `Risk::Forest` and not `type_is("forest")`: a **corrupted** forest
            // ranks `Corrupt`, and corruption puts the whole interior under attack
            // (`village.lua:371-395`).
            //
            // **A fight the crossing demands is exempt**, which is `must_clear_here` — we are inside
            // a subworld, standing on something that blocks departure, and the path goes through it.
            // The alternative is not "keep the health": it is to stop where we stand. A live run did
            // exactly that in front of a **level 1** spider nest at 1/20, twenty-four steps into a
            // forest it never crossed, and the dev's verdict was that occasionally bypassing combat
            // nodes has cost more than it is worth. For the MVP we stick to the path and fight
            // through what is on it.
            //
            // This is the same argument the deleted `no_way_round` made and could barely reach: it
            // needed us to have backed out of the node once and been routed straight back. Backing
            // out is gone — `WorldMap::cross_toward` has the account — so the observation is made
            // where it was always available, at the point the crossing says the node is the move.
            //
            // What is left gated is the case the gate was built for: **choosing** to walk onto
            // hostile ground while hurt. A run cleared `l41`, came out at 1 of 20, had no rest site
            // among its 22 known places, walked to `l50` and clicked Combat on a level 6 crypt. That
            // is `arrived_at_target`, not a crossing, and it still stops.
            //
            // The trade-off is real: a crossing that leads onto a level 4 guard post now takes it at
            // 1/20. Weighing the node's level against health is the better rule and is still not
            // implemented — see the open question on `easiest_hostile`.
            let enterable = p.risk() == crate::overworld::Risk::Forest;
            let too_hurt = health.map(crate::rest::health_is_low).unwrap_or(true)
                && !enterable
                && !must_clear_here;
            if too_hurt && !is_anomaly {
                let hp = health
                    .map(|h| format!("{}/{}", h.current, h.max))
                    .unwrap_or_else(|| "unreadable".into());
                r.log.push_str(&format!(
                    "{step}. **not fighting `{here}` ({}) at {hp}** — stopping instead of dying\n",
                    p.heading
                ));
                return Stop::TooHurtToFight(format!("{here} ({}) at {hp}", p.heading));
            }
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
            r.snap_area_slot("combat-live");
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
                Ok(o) if o.fatal() => return Stop::Died(format!("{o:?}")),
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
        // What this hop is *for*, so that arriving there is recognised as arriving. See
        // [`Run::committed_to`].
        r.committed_to = Some(hop.plan.target.clone());

        // Select, and prove it: the lone area button is `affirmative`, and at a combat node that
        // button is Combat. Space without a confirmed selection starts a fight.
        //
        // Retried, because a missed click is a *coordinate* failure and coordinates can be renewed.
        // The node position comes from an adjacency dump, and a dump describes the map at the instant
        // it was printed — see `recentre` for the three ways the map moves without announcing it. So
        // when a click lands on empty ground the useful response is not to give up but to ask the
        // game where things are now, which is exactly what pressing the arrow does.
        //
        // A miss is cheap to detect and ruinous to miss: `affirmative` acts on
        // `overworldview.getMousePressedOn()` (`overworld.lua:1355-1357`), so with nothing selected
        // the Space that follows has no subject and travel never starts. The run that taught us this
        // then sat in the arrival wait for its full sixty seconds and died, having printed "the
        // affirmative slot is empty" two hundred times on the way.
        let mut selected = false;
        let mut at = (target.x as i32, target.y as i32);
        for attempt in 1..=SELECT_RETRIES {
            let Ok(before) = crate::win::capture::capture_window(r.win) else {
                return Stop::Failed("capture failed".into());
            };
            let Ok((sx, sy)) = r.win.client_to_screen(at.0, at.1) else {
                return Stop::Failed("coordinate conversion failed".into());
            };
            let _ = click_at_in(r.win, sx, sy);
            std::thread::sleep(Duration::from_millis(900));
            r.pump();
            let Ok(after) = crate::win::capture::capture_window(r.win) else {
                return Stop::Failed("capture failed".into());
            };
            let moved = before.diff_fraction(&after, AREA_BUTTONS);
            if moved > SELECT_MOVED {
                selected = true;
                break;
            }
            r.log.push_str(&format!(
                "  selecting {} at ({}, {}) moved the strip {moved:.4}, attempt {attempt} of {SELECT_RETRIES}\n",
                hop.step, at.0, at.1
            ));
            if attempt == SELECT_RETRIES {
                break;
            }
            // Fresh coordinates, from a dump that is now required to be newer than the arrow press.
            match r.recentre() {
                Some(a) => match a.nodes.iter().find(|n| n.key == hop.step) {
                    Some(n) => {
                        at = (n.x as i32, n.y as i32);
                        r.log.push_str(&format!(
                            "  re-centred; `{}` is now at ({}, {})\n",
                            hop.step, at.0, at.1
                        ));
                    }
                    None => {
                        r.log.push_str(&format!(
                            "  re-centred, but `{}` is not in the new dump\n",
                            hop.step
                        ));
                        break;
                    }
                },
                None => {
                    r.log.push_str("  re-centre produced no fresh dump\n");
                    break;
                }
            }
        }
        if !selected {
            return Stop::Failed(format!(
                "selecting {} did not register after {SELECT_RETRIES} attempts",
                hop.step
            ));
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `Screen::ALL` has to actually list every variant, or every test below silently checks less
    /// than it claims.
    ///
    /// The compiler cannot prove this — Rust will not enumerate an enum — so what it proves instead
    /// is the half that matters: `answer_for` is an exhaustive match, so a new variant cannot reach
    /// this test without someone having decided what answers it. This catches the *other* omission,
    /// forgetting to add it here afterwards, by requiring the two to agree in size.
    #[test]
    fn every_screen_is_listed_exactly_once() {
        let mut seen: Vec<Screen> = Vec::new();
        for &s in Screen::ALL {
            assert!(!seen.contains(&s), "{s:?} appears twice in Screen::ALL");
            seen.push(s);
        }
        // Every escape's screen must be in the list, which is the cheapest way to notice the list
        // going stale in the direction that matters.
        for e in ESCAPES {
            assert!(Screen::ALL.contains(&e.screen), "{:?} is escapable but not in Screen::ALL", e.screen);
        }
    }

    /// The escape table and the answer map must agree about which screens are escapable.
    ///
    /// They are two statements of the same fact written in different places, which is exactly the
    /// shape that drifts. Either one alone would be believed.
    #[test]
    fn the_escape_table_and_the_answer_map_agree() {
        for e in ESCAPES {
            assert_eq!(
                answer_for(e.screen),
                Answer::Escape,
                "{:?} is in ESCAPES but answer_for does not call it an escape",
                e.screen
            );
        }
        for &s in Screen::ALL {
            if answer_for(s) == Answer::Escape {
                assert!(
                    ESCAPES.iter().any(|e| e.screen == s),
                    "{s:?} is answered as an escape but has no ESCAPES entry, so `drive` will fall \
                     through it to the map path"
                );
            }
        }
    }

    /// No two escapes may claim the same screen: the lookup takes the first and the second would be
    /// dead code that reads as though it were live.
    #[test]
    fn no_two_escapes_claim_the_same_screen() {
        for (i, a) in ESCAPES.iter().enumerate() {
            for b in &ESCAPES[i + 1..] {
                assert_ne!(a.screen, b.screen, "{:?} is claimed twice", a.screen);
            }
        }
    }

    /// Every escape's button must be in `act::ALL`, which is what subjects it to the registry's own
    /// invariants — chiefly that its search box can contain its template, the mismatch that once let
    /// a button measure perfectly offline while never matching in a live run.
    #[test]
    fn every_escape_button_is_in_the_registry() {
        for e in ESCAPES {
            assert!(
                crate::act::ALL.iter().any(|b| std::ptr::eq(*b, e.button)),
                "{} is escaped by a button outside act::ALL",
                e.what
            );
        }
    }

    /// The screens nothing answers, written down.
    ///
    /// This is the test the project actually needed. `Screen` had no in-combat variant, so a run that
    /// entered a fight from an overworld event fell through to "assume map", spent its budget probing
    /// for one, and the gap was found by a live run and a dead character. A list that has to be
    /// edited on purpose turns that into a failing assertion.
    ///
    /// Shrinking this list is the goal. Growing it should require saying so here.
    #[test]
    fn the_unanswered_screens_are_only_the_ones_we_have_admitted_to() {
        let unanswered: Vec<Screen> =
            Screen::ALL.iter().copied().filter(|&s| answer_for(s) == Answer::Unanswered).collect();
        assert_eq!(
            unanswered,
            vec![Screen::Dead],
            "the set of screens nothing answers has changed; if that is deliberate, say so here"
        );
    }

    /// The map has to be *read* by something, or it is a comment that looks like a control.
    ///
    /// This is the half [`answer_for`] could not give on its own: it can be exhaustive and correct
    /// and still change nothing about what the run does. `precheck` is what `drive` actually calls,
    /// so testing it tests the wiring rather than the intention.
    #[test]
    fn an_unanswered_screen_stops_the_run_and_every_other_screen_does_not() {
        for &s in Screen::ALL {
            match answer_for(s) {
                Answer::Unanswered => assert_eq!(
                    precheck(s),
                    Some(Stop::Unanswered(s)),
                    "{s:?} is unanswered but `drive` would carry on past it"
                ),
                _ => assert!(
                    precheck(s).is_none(),
                    "{s:?} has an answer, so `drive` must not stop on it"
                ),
            }
        }
    }

    /// `Elsewhere` screens must never stop the run.
    ///
    /// Worth its own assertion rather than leaving it to the sweep above, because it is the tempting
    /// mistake: they are not handled *here*, which reads like "not handled". Seeing one in `drive`
    /// means the component that owns it has just finished — a reward screen still up after a fight —
    /// and the next iteration clears it. Stopping would turn ordinary transitions into dead runs.
    #[test]
    fn a_screen_answered_elsewhere_is_not_a_reason_to_stop() {
        let elsewhere: Vec<Screen> = Screen::ALL
            .iter()
            .copied()
            .filter(|&s| matches!(answer_for(s), Answer::Elsewhere(_)))
            .collect();
        assert!(!elsewhere.is_empty(), "the sweep is vacuous if nothing is answered elsewhere");
        for s in elsewhere {
            assert!(precheck(s).is_none(), "{s:?} is answered elsewhere but `drive` stops on it");
        }
    }

    /// Every point we are willing to click looking for empty map must actually be map.
    ///
    /// The search retries until the locate-me arrow appears, so a bad candidate is not fatal — but a
    /// candidate sitting on chrome would press the character screen or the menu, which is a click
    /// this project has already made once by hand.
    #[test]
    fn every_empty_map_candidate_is_inside_the_map_area() {
        for (x, y) in EMPTY_MAP_CANDIDATES {
            assert!(
                crate::observe::hud::is_map_point(x, y, 1920, 1080),
                "({x}, {y}) is chrome: {:?}",
                crate::observe::hud::chrome_at(x, y, 1920, 1080)
            );
        }
    }

    /// The coordinate we click for locate-me must be the one its ButtonSpec resolves to, or we read
    /// one slot and press another.
    #[test]
    fn the_click_point_matches_the_slot_we_read() {
        assert_eq!(
            crate::win::window::button_center(&affirm::SHOW_AREA_BUTTONS, 1920, 1080),
            SHOW_AREA_BUTTONS
        );
    }
}
