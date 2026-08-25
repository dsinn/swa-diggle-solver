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

use crate::act::Screen;
use crate::fight::Fight;
use crate::observe::adjacency::{self, Adjacency};
use crate::observe::affirm;
use crate::observe::event;
use crate::observe::feed::Feed;
use crate::observe::pan;
use crate::overworld::{Goal, Ground, WorldMap};
use crate::win::input::{
    click_at_in, Input, PostMessageInput, SC_NEXT, SC_SPACE, VK_NEXT, VK_SPACE,
};
use crate::win::window::GameWindow;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

mod guard;
pub use guard::LoopGuard;
use guard::{LOOP_GIVE_UP, LOOP_WRITE_OFF};

mod startup;
pub use startup::start_new_run;
use startup::{page_the_shop_to, skip_cinematic};

mod screens;
pub use screens::{answer_for, precheck, Answer, Doorway, Escape, ESCAPES};

mod drive;
pub use drive::drive;

pub const FRAMES: &str = "spike-frames-live";

/// The console's announcement that a lore screen is up (`ui/lorescreen.lua`).
///
/// Matched as a prefix rather than an equality: the line carries the screen's own text after it,
/// which is the whole point — `Lore screen:` followed by *As you're passing through Ulrome east
/// guard post…* is the game telling us exactly what is on the display. See
/// [`Run::clear_text_screen`] for the run this cost.
const LORE_LINE: &str = "Lore screen:";

/// Does this stretch of console carry a lore-screen announcement?
///
/// Split out from [`Run::clear_text_screen`] so the match itself can be tested: constructing a
/// `Run` needs a live window, and this predicate is the whole of what the run of 2026-08-22 1855Z
/// needed and did not have.
fn announces_a_lore_screen(lines: &[String]) -> bool {
    lines.iter().any(|l| l.trim_start().starts_with(LORE_LINE))
}
/// Does this dump say the game has just arrived at the node we think we are on — and that
/// nothing was raised over the top of it?
///
/// The reason strings are the game's, and there are only three: `core.arriveAt` prints
/// `didEvent and 'Arrived at location with event' or 'Arrived at location'`
/// (`overworldview.lua:1442`), the pan tween prints `Screen pan finished` (`:1255`), and a load
/// prints `World loaded`. A whole run's console carried nothing else.
///
/// **The `with event` form is deliberately excluded**, and it is the only judgement in here. An
/// arrival that raised an event is precisely the case where the map is not what is on screen, and
/// while the caller's input guard would normally catch that — answering the event is a click —
/// this does not depend on having got there first. Cheap to refuse: the fallback is the locate-me
/// that used to run unconditionally.
fn arrival_selected_us(a: &Adjacency, here: Option<&str>) -> bool {
    a.reason.trim() == "Arrived at location" && Some(a.here_key.as_str()) == here
}

/// Where the learned map is kept between runs. See [`Run::save_map_cache`].
pub const MAP_CACHE: &str = "map-cache";
const AREA_BUTTONS: crate::win::capture::Region =
    crate::win::capture::Region { nx: 0.0, ny: 0.68, nw: 0.45, nh: 0.18 };
const SHOW_AREA_BUTTONS: (i32, i32) = (32, 918);
/// `Combat` / `Travel` / `Visit` all land here — which is why the area buttons must be known to be
/// the right ones before it is clicked.
const AREA_BUTTON: (i32, i32) = (187, 918);
/// How many times [`Run::left_the_overworld`] repeats a press before accepting it will not land.
///
/// Three. One retry would cover a single swallowed release; the third exists because nothing yet
/// says the loss is independent between presses, and the cost of an extra look is a second against
/// a stalled run.
const LEAVE_TRIES: usize = 3;
/// How long one such press gets to produce its screen. Eight times the game's 0.5 s
/// `transitionDuration` (`utils/defaultconfig.lua:5`), and a deadline rather than a sleep — a screen
/// that opens at once costs nothing, so this is only ever paid by a press that did not land.
const LEAVE_LANDS_WITHIN: Duration = Duration::from_secs(4);

/// How soon after an input it is fair to judge whether the input took.
///
/// The dev, 2026-08-22: *make sure that the post-validation waits at least half a second after the
/// input because of animation time.* Right, and the game names the figure — except that the number
/// to take is **0.625 and not 0.5**. `utils/defaultconfig.lua:5` ships `transitionDuration = 0.5`,
/// but `main.lua:127` and `:193` both read `userConfig.interface.transitionDuration or 0.625`, so a
/// profile without the key animates for the longer time. Diggle never writes `userConfig` and cannot
/// assume a profile has it, so the fallback is the one that has to be covered.
///
/// A *floor* on when to look, not a sleep to spend: every caller measures it from the input and
/// sleeps only the remainder, so a press that already cost longer than this pays nothing.
const INPUT_SETTLES_BY: Duration = Duration::from_millis(625);

/// How many times to press `Enter` before accepting that the subworld will not open.
///
/// Same shape and same reasoning as [`LEAVE_TRIES`]: the overworld swallows presses, and the loss
/// looks independent between them. The corrective action for a failed post-check is another press —
/// the dev's instruction of 2026-08-22, after this branch had answered a silent console with a dead
/// run.
const ENTER_TRIES: usize = 3;

/// How long one `Enter` press gets to put a subworld on the console before it is pressed again.
///
/// Shorter than the ten seconds this replaced, because it is now a *per-attempt* budget with two
/// more attempts behind it rather than the run's last word, and because the signal is immediate: the
/// dump naming the subworld is printed on arrival, not after any walk.
const ENTER_LANDS_WITHIN: Duration = Duration::from_secs(4);

/// How many times to press `Combat` before accepting that no fight will open.
///
/// The press this most needed to be true of: `Combat did not open` has ended five runs, more than
/// any other press failure in the corpus. Three for the same reason [`LEAVE_TRIES`] is three.
const COMBAT_TRIES: usize = 3;

const EMPTY_MAP: (i32, i32) = (1750, 160);
/// Touch this file to stop the run **gracefully** at the next step boundary.
///
/// The alternatives are both worse. Killing `spike_run` — `timeout`, Ctrl-C, stopping the background
/// task — ends the process before `main` writes the report and the stop frame, so the run costs a
/// launch and returns nothing. Killing the *game* does leave a report, but it interrupts whatever
/// was mid-flight: a click, a save write, an unconfirmed reward screen (which discards the reward).
///
/// Checked at the top of the loop, between steps, where nothing is half-done — and, since
/// 2026-08-21, inside a fight and inside an inn as well, because a step can hold the mouse and
/// keyboard for minutes. **Consumed here and nowhere else**, so the subsystems that merely *ask*
/// cannot swallow half a request; see [`crate::config::stop_requested`].
use crate::config::STOP_FILE;

/// What a visit to an inn actually achieved.
///
/// **`NothingToDo` and `Failed` used to be the same `false`**, and the old code could afford that
/// because it wrote an inn off on the first empty visit either way. Counting failures instead — so a
/// swallowed `Enter` no longer costs a village — made the conflation expensive: arriving at full
/// health scored as a failure and was retried [`REST_GIVE_UP`] times, each retry hunting for a `Rest`
/// plaque that reads `+0 (full)` and matches nothing, for the full [`crate::innplay::REST_TRIES`]
/// budget. Live 2026-08-10, eight inn screens at 20/20.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rested {
    /// Gold was spent and health went up.
    Healed,
    /// The inn says there is nothing to buy — full health, or a purse it will not serve. Not a
    /// failure of anything, and the strongest signal available that the errand is over: it is the
    /// game's own `healthNeed`, read off a rest screen we had just opened, rather than our own
    /// arithmetic over a save that lags.
    NothingToDo,
    /// The visit achieved nothing and we do not know why — a click swallowed, a screen that never
    /// opened, a press that went nowhere.
    Failed,
}

/// How long to let `<key>_consecrated` reach the save before calling a consecration failed.
///
/// Not a guess at latency. Two things have to finish first, and the second is the longer: the flag
/// is written by `overworld:save()` inside the consecration beam's `onDecay` (`shrine.lua:281`), and
/// the dev reports the game then **holds the camera on the shrine effect for a few seconds** before
/// panning back to the player. Eight seconds covers that with room, and costs nothing on the happy
/// path because [`Run::confirm_consecrated`] polls and returns the moment the flag appears.
const CONSECRATE_CONFIRM: Duration = Duration::from_secs(8);

/// How long a resumed fight is left alone before the first word is played.
///
/// See the call site in [`drive`] for what it is for and why it is a fixed delay rather than a
/// measurement. Eight seconds: long enough for a boss introduction, and paid at most once per run.
const RESUME_SETTLE: Duration = Duration::from_secs(8);

/// Empty visits to one rest site before it stops being a destination.
///
/// Three, because the failures worth surviving come in ones — a click lost to a transition, a plaque
/// not yet painted — and a site that gives nothing three times running is telling us something about
/// the site rather than about the click. Each attempt is already several seconds of retries inside
/// [`Run::rest_at_inn`], so this is not free; it is cheap against what one wrong write-off costs,
/// which live was a village abandoned at 7/20.
const REST_GIVE_UP: usize = 3;

/// How long to wait for a back plaque before deciding there is not one.
///
/// Short, because this is asked in a loop whose *last* iteration is always the one that finds
/// nothing: leaving the inn costs exactly one of these at the end, every time. The plaque is chrome
/// declared with its screen rather than something that animates in, so if it is coming it is there
/// within a frame or two of the screen changing — and [`Run::back_one_screen`] has already slept
/// 600ms after the previous press before asking.
const BACK_WAIT: Duration = Duration::from_millis(1200);

/// How many back presses before we accept that this is not the inn we thought we were in.
///
/// The inn is two screens deep at most — the rest screen over the inn — so three is one spare.
/// Unbounded pressing at a screen we have misread is the failure this replaces, and it is worse than
/// the one it replaces because it never ends.
const LEAVE_INN_CLICKS: usize = 3;

/// Points tried, in order, when looking for map with no location under it.
///
/// One fixed point cannot be right everywhere — a subworld packs its nodes into a much smaller area
/// than a world map, and where the nodes fall depends on the seed. All of these are inside the map
/// area and clear of the chrome ([`hud::is_map_point`] covers the top strip and the corners), and
/// they are spread so that a map dense around one is sparse around another.
const EMPTY_MAP_CANDIDATES: [(i32, i32); 4] = [EMPTY_MAP, (1750, 800), (170, 240), (960, 170)];
/// Where the cursor is parked so it cannot hover something we are about to fingerprint.
///
/// Deliberately clear of the view's hotspot rectangle, `{0, 0, 300, height*0.8}`
/// (`overworldview.lua:1146`). The old value was (300, 300) — sitting exactly on that rectangle's
/// right edge, next door to a function named `backOutOfHotspotMapPan`. Parking on the boundary of a
/// region whose handler pans the map is not somewhere to leave a cursor for seconds at a time.
pub(super) const NEUTRAL: (i32, i32) = (760, 240);
// A step cap lived here — 45, then 20 before that. It is gone.
//
// It was never a safety measure, only a guess about how long a run *ought* to take, and it kept
// expiring on the interesting part: run 4 died on the anomaly trigger at step 20 with the cinematic
// skip below it never once exercised, and the run of 2026-08-09 was cut off mid-errand at step 45
// while walking into its third village, having just rested to full and fought through a mausoleum.
// The dev's call: while runs are still turning up new behaviour every time, an arbitrary cap costs
// discoveries and buys nothing.
//
// What actually bounds a program holding the real mouse and keyboard is time, and that stays — see
// `drive`'s `deadline`, which the caller sets and which is checked at the top of every step. So does
// `.diggle-stop`, now checked in the same place rather than partway down, so it is honoured from any
// state rather than only from the states that reach the middle of the loop.

/// One reading of the area-button slot, said in full: the score and both verdicts it answers.
///
/// Both bars are printed on every line, whichever question the caller asked, because the line that
/// gave one number and one bar was read as "some other button" when it meant "nothing pressable".
/// `Combat 0.7367, gate 0.95` is true and buries the fact that 0.7367 is the *greyed* reading and
/// 0.8566 — which the same runs printed 244 times — is a live `Travel`.
fn slot_reading(q: Option<f64>) -> String {
    let Some(q) = q else { return "  area slot: not read\n".to_string() };
    let verdict = match (q >= crate::act::AREA_BUTTON_SHOWING, q >= crate::act::AREA_BUTTON_LIVE) {
        (true, _) => "`Combat`",
        (false, true) => "a live button, not `Combat`",
        (false, false) => "nothing pressable",
    };
    format!(
        "  area slot: {verdict} ({q:.4}; `Combat` at {}, any live plank at {})\n",
        crate::act::AREA_BUTTON_SHOWING,
        crate::act::AREA_BUTTON_LIVE
    )
}

/// How many times to click a node, re-deriving its coordinates between tries, before giving up.
///
/// Three: one for the coordinate we were given, and two for coordinates the game is asked to restate
/// after the map has been re-centred. A fourth would be re-asking a question already answered twice —
/// if two fresh dumps in a row put the node somewhere a click does not select it, the problem is not
/// staleness.
/// How many times a hop may press Travel and go nowhere before the run gives up.
///
/// Three, matching every other retry on this path — `SELECT_RETRIES`, the shrine `Visit`, the
/// handover out of the overworld. The number is not measured and does not need to be: each try
/// costs a locate-me and a re-plan, and the thing it is insuring against is a swallowed press,
/// which is the single most repeated failure in this project.
const MAX_HOP_MISSES: usize = 3;

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
/// How much of the whole screen must change before an area-button press counts as landed.
///
/// Named because [`Run::click_area_button`] now weighs against it several times rather than once,
/// and a bar that is sampled repeatedly is one a reader has to be able to find.
const AREA_BUTTON_MOVED: f64 = 0.05;
// A landmark-matching drift correction lived here: lift a textured patch when a dump is adopted,
// find it again before each click, and shift the coordinate by however far the map had moved. It
// worked, and it was the wrong tool. The search cost is the search area times the template — around
// a million candidate positions against 96x96 pixels — which showed up immediately as long pauses
// between moves, paid on every click to insure against a displacement that had been seen once.
//
// Pressing the arrow answers the same question for the price of a click, because the map's position
// stops mattering once the game has just restated it. The retry below does that, and the run that
// followed cleared the exact hop the drift had killed without the correction ever firing.

/// How many times to look at the area-button slot, re-selecting between looks, before pressing anyway.
///
/// Three, matching every other retry on this path. Each look after the first costs a
/// [`Run::select_here`] — a click on empty ground and a click on the arrow, about a second — which
/// is the whole of the recovery for a slot holding another node's buttons. Two of those and then
/// the press, because [`Run::look_for_a_live_slot`] may not veto: see there.
const SLOT_LOOKS: usize = 3;

/// How long to let the strip redraw after re-selecting, before reading it again.
///
/// The area buttons are inserted synchronously by the arrow's handler
/// (`overworldview.lua:488-494`), so this is for the fade rather than for the logic. Half a second
/// is the nominal transition (`utils/defaultconfig.lua:5`); this is comfortably past it.
const SLOT_RETRY_PAUSE: Duration = Duration::from_millis(700);

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
    /// Someone asked us to stop, via [`crate::config::STOP_FILE`].
    Requested,
    AnomalyBeaten,
    // `AtShrine` used to live here: arriving at a shrine ended the run, because finishing one needed
    // a capability this program did not have. It does now, so a shrine is a detour rather than a
    // terminus — see the arrival branch in `drive`.
    /// Standing on a subworld whose interior we cannot yet clear.
    AtSubworld(String),
    /// The general store's screen is open and the buying is not written yet.
    ///
    /// A deliberate terminus, asked for by the dev: the errand drives all the way to the counter and
    /// stops there, so the shop screen can be photographed and its controls measured from a real run
    /// rather than guessed at. Everything up to here is ordinary navigation; what follows needs
    /// numbers nobody has taken.
    ///
    /// The console already does most of the reading. `shop.onActive` prints `Opened shop UI` and
    /// then the entire inventory through `table.repr` (`shop.lua:248-256`), in the same
    /// serialisation `mainSaveData` uses. So what this stop is really for is the *clicking*: whether
    /// an item's index in that dump survives to its slot in the 2x4 grid at `shop.lua:287-292`.
    AtShop(String),
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
    /// **Retired 2026-08-23, and no longer produced.** Kept for the account rather than the
    /// behaviour; delete it once nothing reads old reports.
    ///
    /// It meant: too hurt to start the fight in front of us, and no rest found before we got here.
    /// A stop rather than a failure — the run intact, no checkpoint restore needed — and the
    /// deliberate alternative to [`Stop::Died`], which is what happened the one time it was not
    /// checked: a run walked from `l41` onto a level 6 crypt at 1/20 and was killed by it.
    ///
    /// **What replaced it, and why that is not simply less careful.** The dev, 2026-08-23: *when
    /// we're truly hurt and land on a crypt, shouldn't we just go to the nearest settlement with a
    /// known safe path rather than outright stalling?* `WorldMap::next_errand` already does exactly
    /// that while `wants_rest` — safe-and-routed, then safe, then
    /// [`crate::overworld::WorldMap::easiest_hostile`] — and none of it had been asked. The gate
    /// now sets the flag and re-plans; if the flag was already set, every safer errand has been
    /// considered and the node in front of us is the least bad one on the map.
    ///
    /// The `l41` death predates `easiest_hostile`, which would have preferred a level 1 forest to
    /// that level 6 crypt. The trade is real and it is the dev's to make: a stall ends the run for
    /// certain, and a gentle fight only might.
    TooHurtToFight(String),
    /// The run is going round in circles and has stopped saying so only to itself.
    ///
    /// Carries the cycle as walked, so the report diagnoses rather than merely announces. See
    /// [`Run::sterile_here`] for why this is a stop and not a correction.
    Looping(String),
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

/// How many drags to spend fetching one node into reach before trying something else.
///
/// Scrolling is clamped to the map bounds (`clampWithinBoundsX`, `overworldview.lua:293-297`), so
/// what we ask for is an upper bound on what we get and a node far outside the window will not
/// arrive in one pull. Three, because [`pan_again`] also stops the moment a pan gains nothing —
/// which is what the map's own bound looks like from here — so this is only the cap on *productive*
/// pulls.
const PAN_ATTEMPTS: usize = 3;

/// How many times a step may be re-derived from a re-centred view when panning could not fetch it.
///
/// The run of 2026-08-09 ended on the first failure with no second thought: the road out of `l2` sat
/// at y = -130, the pan asked for `(0, 164)` and measured `(38, -26)`. Locate-me is the cure this
/// project has already proved for a measurement the game's own motion got into
/// ([`Run::needs_recentre`]), so it is worth spending here before giving up — but a node the map
/// genuinely will not show has to end the run rather than spin.
const MAX_PAN_RETRIES: usize = 2;

/// How long a pressed Combat button has to produce a screen we can name.
///
/// Generous on purpose. The cost of waiting too long is seconds; the cost of not waiting is the run,
/// because the fallback is the map path pressing the same coordinate into whatever did arrive. The
/// press has already been confirmed to land — [`Run::click_area_button`] measures the screen moving
/// — so this is only ever waiting out a transition that is known to have started.
///
/// Four seconds against a `setActiveMode` cross-fade of 0.625s (`main.lua:191-195`), which leaves
/// room for a boss introduction to draw itself. See [`RESUME_SETTLE`], which is the same problem met
/// from the other side and answered with a blunter number for want of anything to measure.
const COMBAT_OPENS_BY: Duration = Duration::from_secs(4);

/// Is another drag worth spending to fetch `at` into reach?
///
/// Three ways to answer no, and the middle one is the one that matters:
///
/// 1. **It is already in reach** — nothing to fetch.
/// 2. **The last pan gained nothing.** Scrolling is clamped to the map bounds
///    (`clampWithinBoundsX`, `overworldview.lua:293-297`) and says nothing when it clamps, so a pull
///    that moved the view less than [`pan::Shift::matters`] is the map's own edge answering. Pulling
///    again against a wall will not move it, and the honest response is to stop asking this way
///    rather than to ask louder.
/// 3. **[`PAN_ATTEMPTS`] have been spent** — a plain cap, so a view that creeps a little every time
///    without ever arriving cannot run for ever.
///
/// Split out of `drive` because it is the whole stopping rule and `drive` cannot be tested.
fn pan_again(
    at: (f64, f64), client_w: i32, client_h: i32, last: Option<pan::Shift>, spent: usize,
) -> bool {
    if spent >= PAN_ATTEMPTS || last.is_some_and(|got| !got.matters()) {
        return false;
    }
    crate::observe::hud::chrome_at(at.0 as i32, at.1 as i32, client_w, client_h).is_some()
        || pan::shift_to_reach(at, client_w, client_h).matters()
}

/// Has a dump been *counted* since the map was last invalidated?
///
/// Split out as a pure function for the same reason [`precheck`] is: the loop it lives in needs a
/// live game, so without this the rule would be a claim nothing checks. Answers only the freshness
/// half — [`Run::settled_dump`] still requires the dump to have settled, and has the account of the
/// run this cost.
///
/// Strictly greater, and that is the whole point. Equality means the newest dump is the one that was
/// already on hand when the map turned over, which is exactly the dump that must not be believed.
const fn dump_is_usable(counted: usize, stale_at: usize) -> bool {
    counted > stale_at
}

/// Does this dump place **every** neighbour off the screen?
///
/// If so the camera and the coordinates disagree, whatever the dump's reason line says, and nothing
/// in it can be clicked or panned to. Adjacent nodes are around the player by construction — they
/// cannot all be off the same edge of a settled view — so this is a geometric impossibility rather
/// than a threshold, and needs no tuning.
///
/// ## The transient it catches
///
/// Ending a fight in a lost woods runs `regenerateMap` (see [`Run::settled_dump`]), which flips every
/// `posX`/`posY`. The camera is re-centred separately, and the dump can be printed in between — new
/// positions against the old `xoffset`, which mirrors the whole map about the viewport. The run of
/// 2026-08-09 got one and believed it, because it announced itself as `Screen pan finished`:
///
/// ```text
/// Local overworld data:   Screen pan finished     e1sub35
///     Adjacent connections:
///         e1sub45   posX: -1308.48   posY: 718.33
///         e1sub39   posX: -1418.89   posY: 204.56
///         e1sub20   posX:  -909.94   posY: 347.45
/// ```
///
/// Every neighbour a thousand pixels off the left edge. The step before, from the node next door,
/// the same neighbours had read `+792` and `+1270` — same magnitudes, opposite sign. Trusting it
/// asked for a pan of 1539 px, got 451 before `clampWithinBoundsX` stopped it, and ended the run.
///
/// Deliberately not "the node I want is off-screen", which is ordinary and is what panning is for.
/// **All** of them is the impossible reading.
fn camera_is_lost(nodes: &[crate::observe::adjacency::Node], client_w: i32, client_h: i32) -> bool {
    let (w, h) = (client_w as f64, client_h as f64);
    let off = |n: &crate::observe::adjacency::Node| n.x < 0.0 || n.x > w || n.y < 0.0 || n.y > h;
    !nodes.is_empty() && nodes.iter().all(off)
}

/// Is this client point inside the window we are about to click at?
///
/// **A click outside it does not miss the button — it misses the game.** `click_at_in` converts
/// to screen coordinates and injects there, so a negative client x lands on whatever else is on
/// the desktop, at coordinates nobody chose. That it also cannot select anything is the lesser
/// half of the problem.
///
/// Live 2026-08-23, the 2335Z run: an arrival dump printed mid-glide put `l45` at
/// **(-254, 63)** and three of its four neighbours at negative x. The run clicked there, read
/// the strip as unmoved, and stopped with `selecting l45 did not register` — which is true and
/// says nothing about why.
///
/// Deliberately not [`crate::observe::hud::is_map_point`], which also excludes chrome. That is
/// the right question for *where may we click to find empty map*; this one is *can this click
/// reach the window at all*, and a node genuinely drawn under the top strip is a different
/// argument that should be made separately.
pub(crate) fn on_screen(at: (i32, i32), client_w: i32, client_h: i32) -> bool {
    (0..client_w).contains(&at.0) && (0..client_h).contains(&at.1)
}

/// `, for frontier X` when the crossing holds one, and nothing when it does not.
///
/// A suffix rather than its own line, because it qualifies the step being logged rather than
/// standing alone — and because a crossing prints one line per step and that is worth keeping.
/// Empty on the step that *chooses* a target, since the choice is recorded by the arrival that
/// follows; what this catches is the third and fourth step of a walk that is going somewhere
/// nobody would have picked.
fn aiming_at(r: &Run) -> String {
    match r.map.frontier_target() {
        Some(f) => format!(", for frontier `{f}`"),
        None => String::new(),
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
    /// How much of [`Run::log`] has already reached the terminal. See [`Run::flush_log`].
    ///
    /// Public only because `spike_run` builds a `Run` with struct-literal syntax, as every other
    /// field here does. Callers should leave it at `0` and never write to it again.
    pub logged: usize,
    /// The last `affirmative slot:` line and how many times running it has been produced.
    ///
    /// See [`Run::note_affirmative`]. Public for the same struct-literal reason as [`Run::logged`];
    /// start it at `None`.
    pub affirm_repeat: Option<(String, usize)>,
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
    /// Hops that pressed Travel and left us exactly where we were.
    ///
    /// The last unretried press on the overworld. Selection already retries with a re-centre
    /// (`SELECT_RETRIES`), and every other press in this file has learned the same lesson — the
    /// shrine `Visit`, the handover out of the overworld — but a Travel that took nowhere was a
    /// stop.
    ///
    /// **A missed click is a coordinate failure and coordinates can be renewed**, which is the
    /// argument already written at the selection loop. It applies just as well one press later: the
    /// two ways to end up here are a selection that only *looked* like it landed and a Travel the
    /// game ignored, and a locate-me is the cure for both — it settles the view and re-prints every
    /// position.
    ///
    /// Counted rather than looped in place, so the retry goes through the top of the drive loop and
    /// gets a fresh screen identification, a fresh plan, and the `needs_recentre` handling with it.
    /// Cleared by any hop that moves us.
    pub hop_misses: usize,

    /// This step took the shortcut past the pan wait, so the camera may still be gliding.
    ///
    /// The shortcut's premise is that the next action is a press at a **fixed** coordinate, where a
    /// moving view costs nothing. When the step turns out to click a *node* instead — a hop, at a
    /// coordinate derived from the world frame in screen space — the premise is gone and the click
    /// lands wherever the map has slid to. `Failed("no arrival at l31")`, 2026-08-20, at a corrupted
    /// village whose errand had moved on.
    ///
    /// So the hop settles first when this is set. The shortcut keeps its win where the premise
    /// holds, and pays the pan back on the one path that needs it.
    ///
    /// **This is also what makes the selection check trustworthy again.** That check is a screen
    /// diff over the area strip, and a diff cannot tell a strip that changed because a node was
    /// selected from one that changed because the whole view moved — so under a gliding camera it
    /// reports success for a click that selected nothing, which is why the run reached the arrival
    /// wait at all instead of retrying the selection.
    pub skipped_the_pan: bool,
    /// Shrines we have already tried to play this run.
    ///
    /// `used` is the real "this shrine is finished" flag, and it is the game's, which is why it is
    /// trusted for planning. But it is only set by a *successful* pray — so a shrine we walked into
    /// and failed to solve stays unused, the arrival branch fires again on the next iteration, and
    /// the run spends its whole budget re-entering the same puzzle. This is the difference between
    /// "there is nothing left to do here" and "we already had our go".
    pub shrines_tried: std::collections::HashSet<String>,
    /// Rest sites that gave us nothing, and how many times in a row.
    ///
    /// A rest that lands zero presses used to abandon the site immediately, which is right about
    /// termination and wrong about causes. `cross_toward` keeps returning `Arrive` at a site that
    /// stays a destination, so *something* has to write it off — but the failures that get us here
    /// are overwhelmingly transient: an `Enter` click swallowed by a transition, a `Rest` plaque not
    /// painted yet. Live 2026-08-10, one missed `Enter` at `l19sub2` wrote off a working inn and
    /// sent the run out of the village at 7/20, into a highwayman.
    ///
    /// So: count instead of condemn, and abandon at [`REST_GIVE_UP`]. Termination survives because
    /// the count only rises — a site cannot be retried for ever — while one bad click no longer
    /// costs a village. Cleared by any rest that spends something, since health rising is the
    /// monotone measure that makes coming back safe.
    pub rest_failures: std::collections::HashMap<String, usize>,
    /// Area-slot captures already taken, so a template is photographed once rather than every step.
    pub slots_captured: std::collections::HashSet<String>,
    /// How many times each [`Run::snap_screen`] tag has been used, so the second one does not
    /// overwrite the first.
    pub snaps: std::collections::HashMap<String, usize>,
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
    /// How many inputs this run has sent. Numbers the lines [`Run::tap`] writes, so a click heard
    /// at the game can be found in the report by counting.
    pub inputs: usize,
    /// [`Run::inputs`] as it stood when [`Run::latest`] was folded.
    ///
    /// The whole of what makes a dump *still true*: if nothing has been clicked or typed
    /// since it arrived, nothing can have changed the state it described. See
    /// [`Run::untouched_since_the_dump`].
    pub inputs_at_dump: usize,
    /// The loop guard's memory. See [`Run::sterile_here`] and [`LoopGuard`].
    pub guard: LoopGuard,
    /// Whether [`Run::recall_map`] has already had its answer, so it stops asking.
    pub map_recalled: bool,
    /// The last few places we stood, and what we were doing there — the report's evidence.
    ///
    /// A loop is only diagnosable from the *sequence*, and every one of the three met so far was
    /// read off consecutive steps that each looked reasonable alone.
    pub recent: std::collections::VecDeque<String>,
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
    /// We pressed an area button meaning to start a fight, and have not yet seen one.
    ///
    /// Kept only so the log can tell a fight we walked into from a fight we asked for. Nothing
    /// branches on it: both go through the same handler, which is the point — see
    /// [`Run::settle_after_mode_change`].
    pub combat_expected: bool,
    /// Does the player own the Magic turbo-snail? Read from the save by
    /// [`crate::game::save::has_turbo_snail`], and used only to pace arrival waits — the snail's
    /// exponential ease makes a long hop cost about what a short one does. See
    /// [`crate::overworld::walk_budget`].
    pub turbo_snail: bool,
    /// How far into the console we have answered [`LORE_LINE`] announcements.
    ///
    /// A mark rather than a flag, for the reason [`crate::observe::feed::Feed::seen_line_since`]
    /// gives: lore screens recur, so "have we ever seen one" is useless after the first. This says
    /// "have we seen one we have not yet pressed space at".
    pub lore_cleared_at: usize,
    /// The dump count at the moment every recorded screen position stopped being trustworthy.
    ///
    /// [`Run::settled_dump`] will not return anything counted at or before this, so a dump taken
    /// before a fight cannot be used to aim a click after one. See that function for the mechanism
    /// and for the run it cost.
    ///
    /// Raised after a fight, unconditionally rather than only in a lost woods. Returning from combat
    /// re-centres the camera in any case, and "wait for one more dump" is a far cheaper mistake than
    /// clicking a mirrored map — this project has three runs' evidence that a stale coordinate fails
    /// quietly, by selecting empty ground.
    pub positions_stale_at: usize,
    /// The camera must be put back on the player before any coordinate is believed again.
    ///
    /// ## Why waiting for a fresh dump was not enough
    ///
    /// [`Run::positions_stale_at`] stops us reading a dump printed *before* a fight. It cannot help
    /// with one printed **during** the camera's return: `loadLight` re-centres on the player
    /// (`overworldview.lua:1610-1620`) and the view is still moving while the dump goes out, so the
    /// numbers are a snapshot of a glide.
    ///
    /// Two runs, the same cause, different severities. The first read a fully mirrored map and
    /// clicked a thousand pixels wrong — loud, and [`camera_is_lost`] catches it. The second was off
    /// by 128 px: `pan_map` was asked for `(27, 0)` and measured `(28, -128)`, because the game's own
    /// motion landed inside our measurement. That moved the target *under the inventory button* and
    /// ended the run, and no impossibility test can see it, because every number involved is
    /// perfectly plausible.
    ///
    /// So the cure is not a better detector. `recentre` presses locate-me, which does
    /// `refreshAreaButtons` and `centreScreenOnPlayer` together (`overworldview.lua:488-494`) and
    /// waits for the pan it starts — it ends the glide rather than trying to read around it, and the
    /// dump it returns is settled by construction.
    ///
    /// Costs one click and one dump per fight. Cheap against a run.
    pub needs_recentre: bool,
    /// How many times the current step has been re-derived because panning could not fetch its
    /// target into reach. Cleared by any step that gets as far as clicking.
    ///
    /// Counted rather than merely retried, because the failure it answers and the failure it must
    /// not paper over look identical from here: a measurement the game's own motion got into, which
    /// a re-centre cures, and a node the map will not scroll to, which nothing does. See
    /// [`MAX_PAN_RETRIES`].
    pub pan_retries: usize,
    /// Whether the map has already been pulled back a step. See [`Run::zoom_out`], which explains
    /// why once is the only useful number: the game clamps `targetZoomMul` at `0.5`
    /// (`overworldview.lua:996`) and the default is `1`.
    pub zoomed_out: bool,
    /// Whether the world frame has been reported as inconsistent. See [`Run::pump`].
    pub frame_alarm: bool,
}

impl Run<'_> {
    pub fn pump(&mut self) {
        let new: Vec<String> = self.feed.pump().to_vec();
        for a in self.reader.push(&new) {
            self.map.fold(&a);
            // **The frame's own alarm.** Two nodes we have placed cannot both be where we think they
            // are and where this dump draws them; when they disagree, the frame is not one frame and
            // every position in it is suspect. Once, on the rising edge — a broken frame stays broken
            // and would otherwise print on every dump for the rest of the run.
            match self.map.frame_disagreement(&a) {
                Some(px) if !self.frame_alarm => {
                    self.frame_alarm = true;
                    self.log.push_str(&format!(
                        "  **the world frame disagrees with itself by {px:.0} px** at `{}` — placing \
                         is suspended and far hops will decline; see WorldMap::registration\n",
                        a.here_key
                    ));
                }
                None => self.frame_alarm = false,
                _ => {}
            }
            self.latest = Some(a);
            self.inputs_at_dump = self.inputs;
            self.dumps += 1;
        }
    }

    /// Give a mode transition time to land, then let the top of [`drive`] say what it landed on.
    ///
    /// ## What this replaced, and why the replacement is smaller
    ///
    /// Clicking an area button used to be followed by a bespoke wait for `Pregame screen:` on the
    /// console, with two inferences hung off its absence: "the button entered a subworld" and,
    /// briefly, "the fight started without one". Every one of those states already has a handler at
    /// the top of `drive` — [`crate::act::Screen::Pregame`] presses Start,
    /// [`crate::act::Screen::CombatEntered`] plays the fight out, and a subworld falls through to the
    /// map path. The wait was a second, worse copy of the loop, and it could only recognise the
    /// states its one announcement covered.
    ///
    /// Which is how it stalled: a chest's `Open` button goes through `overworld.startNewRun`
    /// (`overworld/generators/forest.lua:30-39`) straight into `setActiveMode(require'rpg')`, and
    /// `Pregame screen:` is printed from `ui/pregame.lua:91` alone. The run waited ten seconds for a
    /// line that cannot be printed on that path, then failed with the board already in the feed.
    ///
    /// So this waits for the *transition*, which is a real thing with a known duration
    /// (`setActiveMode` cross-fades for 0.625s, `main.lua:191-195`), and identifies nothing. Three
    /// ways to stop waiting, none of them an inference about which button was pressed:
    ///
    /// - a screen we can name — whatever it turns out to be;
    /// - a subworld we were not in a moment ago, because that button entered a place;
    /// - an event plaque, which owns the screen until it is answered.
    ///
    /// The timeout is not a failure and is not reported as one. It means "still the map", which is
    /// the ordinary outcome of every button that neither fights nor enters.
    fn settle_after_mode_change(&mut self, inside_before: Option<String>) {
        let by = Instant::now() + Duration::from_secs(10);
        loop {
            self.pump();
            if crate::act::identify(self.win) != crate::act::Screen::Unknown
                || self.map.inside().map(str::to_string) != inside_before
                || self.affirmative().state.is_ready()
            {
                return;
            }
            if Instant::now() >= by {
                return;
            }
            std::thread::sleep(crate::timing::POLL_BRISK);
        }
    }

    /// Puts the cursor somewhere harmless, **and clears the game's hotspot highlight**.
    ///
    /// The second half is the part a warp could not do. `main.lua:420` clears the highlight only on
    /// a move event carrying a non-zero delta, and a warp reports none — it must not, since
    /// `setHotspotHighlight` moves the pointer itself and would otherwise cancel its own highlight
    /// (`utils/input.lua:96`). So `SetCursorPos` moved the pointer off the button and left the
    /// button lit, which is exactly what a `Start` this run could not read for four seconds was.
    ///
    /// See [`crate::win::input::travel_cursor_in`]. Failures are swallowed as before: parking is a
    /// tidying step and no caller has anything better to do if the pointer will not move.
    fn park(&self) {
        if let Ok((x, y)) = self.win.client_to_screen(NEUTRAL.0, NEUTRAL.1) {
            let _ = crate::win::input::travel_cursor_in(self.win, (x, y), 4);
        }
    }

    /// Scrolls the map by roughly `want` and reports how far it **actually** moved.
    ///
    /// The drag is open-loop and clamped, so the answer comes from comparing frames rather than from
    /// the delta we asked for: lift a patch of map, drag, find the patch again, and the displacement
    /// is the shift. That needs no sprite identity and no calibration constant, and it cancels tint
    /// and scale because it compares a rendering against itself.
    ///
    /// ## Why the clamp is measured and not predicted
    ///
    /// Asked and answered on 2026-08-10, and recorded here because it is the first question anyone
    /// reworking this will have. `clampWithinBoundsX` (`overworldview.lua:293-303`) is
    ///
    /// ```lua
    /// mapsizeX = ((data.tileRadiusX or 15)*64+80)*zoomMult
    /// return math.clamp(x, -mapsizeX+width/2, mapsizeX+width/2)
    /// ```
    ///
    /// so predicting it needs `tileRadiusX/Y` for the *current* generator, `zoomMult`, and the
    /// current `xoffset`. **The last two are never printed.** There is one way in — `posX = xoffset +
    /// location.posX*zoomMult`, and `start` is at world `(0, 0)` (`world.lua:73`, which
    /// [`crate::overworld::WorldMap::pos_for`] already leans on), so a dump naming `start` gives
    /// `xoffset` outright. That is exactly the case this does not need: the clamp bites *inside*
    /// subworlds, where `start` is never in frame.
    ///
    /// So it would mean modelling three unobserved quantities to predict something already measured
    /// for free — a pull that gains nothing **is** the clamp, observed rather than inferred. See
    /// [`pan_again`].
    ///
    /// ## Zoom is the better lever, and is not used yet
    ///
    /// `core:wheelmoved` calls `setZoom`, which clamps `targetZoomMul` to `[0.5, 8]`
    /// (`overworldview.lua:996`), and `UpdateZoom` (`:1087-1097`) keeps the same world point centred
    /// while it scales. A node's screen distance from centre scales with `zoomMult` and the window
    /// does not — so zooming out *always* pulls an off-screen node inward, with no drag semantics and
    /// no patch matching to get wrong.
    ///
    /// Not done, and the cost is the reason: a zoom change rescales every printed coordinate, which
    /// invalidates [`crate::overworld::WorldMap::frame`] comparisons and the steering measure behind
    /// them, and is precisely what `registration` refuses to mix. Task #29.
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
        // **Which spot, and how textured.** Instrumentation, added ahead of the fix rather than
        // after it, because the failure this is here to explain is invisible in the log as it
        // stands: on 2026-08-15 inside `l62` every drag came back unmeasured and the run ended
        // there, and nothing recorded *what* had been tracked. `gave-up.png` shows the map drawn
        // smaller than the window with black void to the right and below, and `(1300, 250)` — one of
        // these four spots — sitting on the boundary between the two. A map/void edge is the
        // highest-variance thing on such a screen and the one thing that is not map, so `max_by`
        // would choose it every time. That is a hypothesis, not a finding; this line is what turns
        // it into one either way.
        let variance = pan::variance(&patch);
        self.log.push_str(&format!(
            "  pan patch from ({}, {}), variance {variance:.1}\n",
            taken_at.0, taken_at.1
        ));

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
        std::thread::sleep(crate::timing::MAP_DRAG_APPLIED);

        let after = crate::win::capture::capture_window(self.win).ok()?;
        // Search generously: the clamp can swallow most of the requested movement, so the patch may
        // barely have moved at all, and the honest answer to that is a small measured shift.
        let radius = (want.dx.abs().max(want.dy.abs()) as i32 + pan::PATCH).max(200);
        let got = pan::measure(&after, &patch, taken_at, want, radius);
        if got.is_none() {
            // The one outcome that ends a run, and until now it reported nothing but its own name.
            // A patch that could not be found within `radius` of where it was asked to land is
            // either a patch that was never map (see above) or a view that moved further than the
            // search allowed, and the two want opposite fixes — so record enough to tell them apart:
            // where we dragged, how far we asked to go, and how wide we looked.
            self.log.push_str(&format!(
                "  pan unmeasured: dragged ({sx}, {sy}) -> ({ex}, {ey}), wanted ({:.0}, {:.0}), \
                 searched {radius} px around the landing place\n",
                want.dx, want.dy
            ));
            // **Which of the two unmeasureds was it?** `measure` folds them together: the patch was
            // not found at all, or it was found and scored below [`pan::MIN_INLIERS`]. Those want
            // opposite fixes — a view that moved further than we looked, against a patch too flat to
            // recognise itself — and the log has never distinguished them.
            //
            // Repeating the search to find out is affordable exactly here, on the path that ends
            // runs. The wide bounds are deliberate: this asks *where did it go*, not *did it land
            // where we asked*.
            let (bx, by) = (taken_at.0, taken_at.1);
            let wide = Some((bx - 900, by - 900, bx + 900, by + 900));
            match crate::observe::template::find_at_scale_in(&after, &patch, 1.0, 2, wide) {
                Some(m) => self.log.push_str(&format!(
                    "  best match for it was ({}, {}) at {:.4}, against the {:.2} bar — a shift of \
                     ({}, {})\n",
                    m.x,
                    m.y,
                    m.inliers,
                    pan::MIN_INLIERS,
                    m.x - bx,
                    m.y - by
                )),
                None => self
                    .log
                    .push_str("  and it is nowhere within 900 px — the patch stopped existing\n"),
            }
        }
        got
    }

    /// Where this world's remembered map lives.
    ///
    /// Keyed by `overworld.seed`, because the map is a property of the world rather than of the
    /// save file — two saves of the same seed describe the same terrain, and a new seed must not
    /// inherit a stale graph. `None` when the save cannot be read, which is the same condition that
    /// makes everything else here unavailable.
    ///
    /// The terrain really is a pure function of that one integer, which is what makes the key safe:
    /// `overworld/generators/world.lua:64-65` opens `generate` with
    /// `local rng = love.math.newRandomGenerator(seed)`, and `overworldview.lua:672` calls it with
    /// `overworldData.seed`. Its only other argument that could vary, `worldSettings`, is a global
    /// that is never assigned anywhere in the game tree — `nil` at both call sites.
    ///
    /// The seed is not a fingerprint, though: `overworldview.lua:1569` sets it to
    /// `#(love.filesystem.getDirectoryItems'saveStats' or {})` — the number of recorded runs at the
    /// moment the character was made — unless `userConfig.gameplay.overworld.seed` pins one
    /// (`ui/classselection.lua:50,950`). Two consequences. A repeat of the counter is harmless,
    /// because the same integer regenerates the same world. But **every new character draws a fresh
    /// seed and therefore starts blind**; the cache only pays off across restores of one save. A
    /// cold start on a new character is the design working, not a regression.
    /// Does the save record a `healthBuff` bought at this village?
    ///
    /// `reduceStock` tallies purchases under `shopData.purchased` (`shop.lua:101-106`), and
    /// `core.save` writes the whole `shopData` back to `areaFlags[<key>_shops]` when the shop closes
    /// (`:303-309`). So this is readable only after leaving, which is why the check sits where it
    /// does — and it is the game's own record rather than an inference from a click we sent.
    fn heart_is_recorded(&self, village: &str) -> bool {
        let Ok(save) = crate::game::save::load(&self.save_dir.join("mainSaveData")) else {
            return false;
        };
        let path = format!(
            "overworld.areaFlags.{village}_shops.generalStoreStock.purchased.{}",
            crate::shopplay::HEART
        );
        save.int_at(&path).unwrap_or(0) > 0
    }

    /// Which remembered map belongs to the world we are in.
    ///
    /// ## `overworld.seed` really is the world, and a small number is not evidence otherwise
    ///
    /// Worth writing down because it was doubted and the doubt was wrong. The seed reads `0` for
    /// several different adventures and `5` for others, which looks like a bucket rather than an
    /// identifier — and I argued from that that a fresh profile could absorb a foreign map, and made
    /// `clear` delete the cache to prevent it. The dev asked whether we were sure. We were not.
    ///
    /// The map is a **pure function of this number**: `overworldview.lua:672` hands
    /// `overworldData.seed` to the generator and `overworld/generators/world.lua:65` opens with
    /// `love.math.newRandomGenerator(seed)`. So seed 0 is one specific world, reproducible, not
    /// "unseeded".
    ///
    /// The saves agree. Two adventures days apart, both seed 0, on `l10`:
    ///
    /// ```text
    /// in-village ['l1','l18','l19','l4','l7'] | ping-pong ['l1','l18','l19','l4','l7']
    /// ```
    ///
    /// — identical neighbourhoods, and every road in the older save present in the newer. They share
    /// a cache file because they are the same world, which is what the key is *for*.
    ///
    /// Two consequences worth keeping:
    ///
    /// - **A cache survives a `clear` on purpose.** A fresh profile on the same seed gets the whole
    ///   road network and every position from step one, which is the difference between a run that
    ///   can route and one that cannot. `completed` and `visited` are deliberately not restored
    ///   ([`crate::overworld::WorldMap::absorb_cache`]), so the new adventure still starts with
    ///   nothing cleared.
    /// - **A pinned seed is a repeatable world.** `ui/classselection.lua:950` lets the player fix it,
    ///   which makes a chosen map replayable for testing.
    fn map_cache_path(&self) -> Option<PathBuf> {
        let save = crate::game::save::load(&self.save_dir.join("mainSaveData")).ok()?;
        let seed = save.int_at("overworld.seed")?;
        Some(PathBuf::from(MAP_CACHE).join(format!("world-{seed}.txt")))
    }

    /// Folds in what earlier runs learned about this world, the first time the save will say which
    /// world that is. Returns the edge count, the file, and whether positions came with it.
    ///
    /// **Called until it succeeds, not once at startup.** A fresh profile has no `mainSaveData` when
    /// the run begins: the game writes the save on screen *exit* (`utils/classes.lua`), and a run
    /// that has just walked into the overworld has exited nothing yet. So
    /// [`Run::map_cache_path`] finds no seed, the cache is not found, and — before 2026-08-22 —
    /// that was the end of it. The run of 0203Z started blind on a world it had 699 places for,
    /// logged `no map remembered for this world`, and stopped 46 steps later inside a village whose
    /// road network it had already mapped in an earlier adventure.
    ///
    /// So [`Run::apply_save`] asks again on every step. The save that makes `apply_save` work at all
    /// is exactly the save that makes the seed readable, which is why this hangs off that call
    /// rather than off the step loop.
    ///
    /// **A late arrival takes the shape and leaves the coordinates**, via
    /// [`crate::overworld::WorldMap::absorb_cache_structure`] — by then this run has anchored a frame
    /// of its own, and old positions dropped into it would make the frame disagree with itself.
    ///
    /// ## The startup call has to come before the first dump, and until 2026-08-22 it did not
    ///
    /// "Nothing has been placed yet" is a claim about **ordering**, not a property of startup, and
    /// the startup path had stopped satisfying it. `spike_run` waited for a dump before reading the
    /// save, `Run::pump` folds a dump as it reads it, and a surface dump into an empty map takes
    /// `Frame::defining` and places everything in it — so `any_placed`
    /// was already true and the branch above took the structure-only arm.
    ///
    /// It read as intermittent because [`crate::overworld::WorldMap::registration`] answers `None`
    /// outright for a subworld dump: a run resuming *inside* somewhere placed nothing and kept its
    /// coordinates. 2002Z opened at `l10_path_to_l18` and logged `with their positions`; 1855Z
    /// opened at `l43` on the surface and logged `for their shape only`.
    ///
    /// The startup call is now made from `spike_run` before that loop, which nothing prevents:
    /// [`Run::map_cache_path`] loads `mainSaveData` off disk itself and wants neither a dump nor a
    /// console line. `apply_save` still asks on every step, and that is still what covers the fresh
    /// profile — this only gives the question its first honest opportunity.
    ///
    /// **Losing the coordinates is not cosmetic**, which is what makes the ordering worth a section:
    /// [`crate::overworld::WorldMap::walk_legs`] answers `None` for an unplaced node, so every
    /// surface travel falls back on [`crate::overworld::walk_budget`]'s ceiling rather than the
    /// distance it exists to price — and [`crate::overworld::WorldMap::cache_text`] writes `-` for a
    /// place with no position, so a structure-only run **writes the loss back to disk** for every
    /// node it did not personally walk past.
    pub fn recall_map(&mut self) -> Option<(usize, String, bool)> {
        if self.map_recalled {
            return None;
        }
        let path = self.map_cache_path()?;
        // Only the *reading* is retried. A world with no cache file must not re-stat it every step
        // for the length of a run, so the flag is set either way once the seed is known.
        self.map_recalled = true;
        // **A world with no cache says so, and used to say nothing at all.** #102: the flag above is
        // set as soon as the *seed* reads, so the `!map_recalled` line `spike_run` prints for a
        // fresh profile does not cover this — a run whose world simply has no file on disk printed
        // no `recalled` line and no explanation either. The 2159Z header is the evidence: the cache
        // had been deleted by hand and the report had a hole where the line belongs, which read as
        // #95 having broken rather than as an absent file.
        //
        // Logged here rather than at the call site because there are two call sites — startup and
        // every later `apply_save` — and this is the one place that knows which of the two silences
        // it is. Once only, since the flag is already set.
        //
        // The two failures are separated because they are different work: a world we have never
        // mapped is ordinary and self-repairing, and a file we cannot read is a machine to go and
        // look at.
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.log.push_str(&format!(
                    "no map remembered for this world — nothing at `{}`, so this run begins blind
",
                    path.display()
                ));
                return None;
            }
            Err(e) => {
                self.log.push_str(&format!(
                    "the map for this world is on disk and unreadable — `{}`: {e}
",
                    path.display()
                ));
                return None;
            }
        };
        let positioned = !self.map.any_placed();
        let edges = match positioned {
            true => self.map.absorb_cache(&text),
            false => self.map.absorb_cache_structure(&text),
        };
        Some((edges, path.display().to_string(), positioned))
    }

    /// Writes what this run learned, for the next one.
    ///
    /// Failures are swallowed and reported into the log rather than raised: a run that has just
    /// finished has nothing better to do about an unwritable cache, and losing the memory is a
    /// slower next run rather than a broken one.
    pub fn save_map_cache(&mut self) {
        let Some(path) = self.map_cache_path() else {
            self.log.push_str("could not read the seed — this run's map is not remembered\n");
            return;
        };
        let text = self.map.cache_text();
        let wrote = std::fs::create_dir_all(MAP_CACHE).and_then(|()| std::fs::write(&path, &text));
        match wrote {
            Ok(()) => self.log.push_str(&format!(
                "remembered {} places in `{}`\n",
                self.map.len(),
                path.display()
            )),
            Err(e) => self.log.push_str(&format!("could not write {}: {e}\n", path.display())),
        }
    }

    pub fn apply_save(&mut self) -> Option<crate::rest::Health> {
        let save = crate::game::save::load(&self.save_dir.join("mainSaveData")).ok()?;
        self.map.apply_save(&save);
        // **The snail changes the shape of every arrival wait**, so it is read wherever the save
        // is. See [`crate::overworld::walk_budget`]; losing it is not a case we have to handle,
        // because a passive cannot be sold, but reading it late only costs a longer wait.
        let snail = crate::game::save::has_turbo_snail(&save);
        if snail && !self.turbo_snail {
            self.log.push_str(
                "  the Magic turbo-snail is aboard — travel is paced by it now
",
            );
        }
        self.turbo_snail = snail;
        // **The first save is also the first chance to know which world this is.** On a fresh
        // profile there is no save at startup, so the cache could not be found then; see
        // [`Run::recall_map`]. Reaching this line at all means the seed is now readable.
        if let Some((edges, path, positioned)) = self.recall_map() {
            let how = match positioned {
                true => "with their positions",
                false => "for their shape only — this run has already anchored a frame",
            };
            self.log.push_str(&format!(
                "recalled {edges} edges from `{path}` {how}
"
            ));
        }
        // **The bank has a second home, and mid-fight it is the only one.** `mainSaveData` carries
        // no `statusEffects` while a fight is in progress; `combatSaveData` carries them under a
        // different root. Read only when the main save was silent, so the ordinary path never
        // touches a file the game deletes when a fight ends — see [`crate::fight`].
        if crate::rest::well_rested_from(&save, crate::rest::STATUS_IN_MAIN).is_none() {
            if let Ok(c) = crate::game::save::load(&self.save_dir.join("combatSaveData")) {
                if let Some(w) = crate::rest::well_rested_from(&c, crate::rest::STATUS_IN_COMBAT) {
                    self.map.note_well_rested(w);
                }
            }
        }
        crate::rest::Health::from_save(&save)
    }

    /// Waits for `<key>_consecrated` to reach the save, and reports whether it did.
    ///
    /// **The screen closing is not the signal, and believing it cost a shrine.** `consecrate`
    /// treated "the shrine screen went away" as success — but it also goes away when the stats
    /// history page opens over it. Live 2026-08-10 at `shrine3`: `shrine screen closed=true`, the
    /// loop tidily cleared a stats page it had not expected, and the shrine ended the run
    /// unconsecrated with the log claiming otherwise. `shrine2` and `shrine6` genuinely worked, so
    /// the false positive was invisible in aggregate.
    ///
    /// The flag is the game's own answer — `isConsecrated` reads exactly this key — and it is worth
    /// waiting for rather than sampling once, for two reasons that compound:
    ///
    /// * `overworld:save()` runs in the consecration beam's `onDecay` (`shrine.lua:281`), so the
    ///   write happens when the *effect* finishes, not when the click lands.
    /// * The dev, 2026-08-10: after the press the game returns to the overworld but **holds the
    ///   camera on the shrine effect for a few seconds** before panning back to the player. So the
    ///   whole confirmation window is one in which the game is animating and the map is not where a
    ///   dump would expect it — which is its own reason not to act during it.
    ///
    /// Polls rather than sleeps a fixed span, so a fast machine pays only what it needs and a slow
    /// one is not cut off early.
    fn confirm_consecrated(&mut self, key: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            self.apply_save();
            if self.map.is_consecrated(key) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(crate::timing::POLL_SAVE);
        }
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
    /// ## Nor can the *wait* be skipped, which was the second attempt
    ///
    /// Having established that the arrow press is load-bearing, the obvious salvage was to keep the
    /// press and drop the twelve-second poll for a pan dump — the dev's point being that a press at
    /// a fixed coordinate does not need the camera to have finished moving. That is true, and it is
    /// still not enough. It ended the next run too, `Failed("no arrival at l31")`:
    ///
    /// ```text
    ///   188. standing on `l3`, which is what we came for — selected it without waiting for the pan
    ///     `l31` is travellable in one press and is not in this dump — placed from the world frame
    ///     input 362: click (1487,638) — travel: select `l31`
    ///     input 363: key 0x0020 — travel: press Travel for `l31`
    ///   Failed("no arrival at l31")
    /// ```
    ///
    /// **Arriving at the node we came for does not mean we are about to enter it.** `l3` is
    /// *Skipwith Brake — level 6 village [corrupted]*: a settlement whose buildings are shut, so the
    /// errand moved on and the step became a hop. A hop clicks a **node**, at a coordinate derived
    /// from the world frame — and the frame is in screen space, so a camera still gliding puts the
    /// click on empty ground. The press was heard and nothing was selected.
    ///
    /// So the two halves cannot be separated after all, for a reason that has nothing to do with
    /// either of the mechanisms above: **what the step will do is not known when the dump is
    /// fetched**, and one of the things it may do needs settled coordinates. Any future attempt has
    /// to make the *step* declare itself first, not the arrival.
    ///
    /// ## It cannot be skipped before entering a node, and 2026-08-20 is the proof
    ///
    /// The dev asked the reasonable question: when we have already decided to *enter* the node we
    /// are standing on, the press is at a fixed coordinate, so why re-centre first? Tried, and it
    /// ended a run — `Failed("Combat did not open at l38")`, twice on the same node.
    ///
    /// The frame it saved is the whole answer. The area slot was showing a **greyed `Combat`**
    /// belonging to `Keyingham crypt` — a different node entirely — while the player stood at `l38`.
    /// Pressing it did nothing, twice.
    ///
    /// The reason is the last line of the arrow's own handler (`overworldview.lua:490-493`):
    ///
    /// ```lua
    ///     core.refreshAreaButtons(location)
    ///     core.centreScreenOnPlayer()
    ///     require'ui.elements.tooltips'.clear(nil, true)
    ///     selectedLocation = location
    /// ```
    ///
    /// **One press doing three things**, and the camera is the least of them. It sets
    /// `selectedLocation` to the *player's* location — which is what makes the slot hold our node's
    /// buttons instead of whichever node was last clicked. Travelling selects the node it clicks, so
    /// without this press the slot keeps the previous selection's buttons, and they are inert
    /// because that location is not where we are.
    ///
    /// ## The mechanism stated here was wrong, and the ruling it supports is not
    ///
    /// This used to read: *`core.arriveAt` does call `refreshAreaButtons` (`:1420-1424`), which is
    /// why this looked safe on paper. It does not touch `selectedLocation`, which is the half that
    /// matters.* Re-read against the source on 2026-08-21, the second sentence is false.
    /// `refreshAreaButtons` opens with it (`overworldview.lua:474-476`):
    ///
    /// ```lua
    /// function core.refreshAreaButtons(location)
    ///     selectedLocation = location or core.playerCurrentLocation()
    ///     selectedLocationName = (location or core.playerCurrentLocation()).key
    /// ```
    ///
    /// So an ordinary arrival does set it, to the node arrived at, and the neat explanation of the
    /// `l38` frame goes with it.
    ///
    /// **The ruling stands on the live evidence rather than on that explanation.** Skipping the
    /// arrow press ended a run twice on the same node; putting it back fixed it. That is the fact,
    /// and the dev's call — *stop reverting it, add safeguards* — was made on it.
    ///
    /// What the source does supply is two ways the slot ends up holding somebody else's buttons,
    /// either of which the arrow press cures:
    ///
    /// * **Entering or leaving a subworld clears it deliberately.** `basicSubworldZoneButtons`'
    ///   `Explore` calls `enterSubworld(..., true)` (`:441-457`), whose `noButtons` reaches
    ///   `arriveAt`'s `supressAreaButtons` branch — `selectedLocation = nil` and
    ///   `overworld.clearAreaButtons()` (`:1427-1430`). `leaveSubworld` does the same (`:643`).
    /// * **Clicking a distant node selects that node**, and fills the strip with
    ///   `travelToLocationButtons` under *its* heading (`:1478-1481`, `:1493-1494`) — which is what
    ///   a fast hop's press is.
    ///
    /// Which of those produced the greyed `Combat` at `l38` is not settled, and pinning it needs a
    /// run that logs the slot on the step before. [`Run::look_for_a_live_slot`] is the safeguard
    /// that does not depend on knowing: whatever put another node's buttons there, re-selecting is
    /// what takes them away.
    ///
    /// So the re-centre is not ceremony before an entry. It is the selection, and the pan is a side
    /// effect of it. Two things noted while getting this wrong and worth keeping:
    ///
    /// * **Stillness genuinely is not the reason.** `setInteractionEnabled(false)` has three call
    ///   sites in the game — the anomaly cinematic (`utils/events.lua:48`) and entering or leaving a
    ///   shrine (`shrine.lua:57`, `:258`). An ordinary pan leaves input enabled. The claim below
    ///   that "input during the animation goes nowhere" is not what stopped that Space.
    /// * **The slot read is diagnostic and not a gate.** The failed press was logged
    ///   `area slot: something else (Combat 0.7367, gate 0.95)` and went out anyway — the observer
    ///   said it was looking at something other than a live `Combat` and nothing acted on it. A
    ///   greyed twin scoring 0.74 against a 0.95 bar is the calibration working; the press ignoring
    ///   it is not. Fixed 2026-08-21 by [`Run::look_for_a_live_slot`].
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
    /// **The selection without the wait**, for a step whose next action is a fixed-coordinate press.
    ///
    /// The dev, 2026-08-20, on the reverted attempt to skip the whole thing: *the greyed out Combat
    /// means that the crypt was already completed. Why aren't we fixing this forward?* Right on both
    /// counts. What that failure proved is that the **arrow press** is load-bearing — it sets
    /// `selectedLocation` to the player's location (`overworldview.lua:488-494`), which is what puts
    /// our node's buttons in the slot instead of the last node we clicked. It proved nothing about
    /// the twelve-second poll for a pan dump that follows it.
    ///
    /// So the press stays and the wait goes. Entering is one press at a fixed coordinate; it does
    /// not need coordinates, and it does not need the camera to have finished moving —
    /// `setInteractionEnabled(false)` has three call sites in the game and an ordinary pan is not
    /// one of them, so input during the glide lands.
    ///
    /// Returns whether the arrow press landed, read from the slot the same way [`Run::recentre`]
    /// reads it: the arrow is *replaced* by the location's buttons, so an arrow still sitting there
    /// means the press did not take.
    /// Has anything been pressed since the newest dump was folded?
    ///
    /// `false` means the dump still describes the screen: the game changes what it is showing in
    /// response to input, or to an animation — and an animation announces itself with a dump of
    /// its own. This is the guard that lets the two below trust an arrival.
    fn untouched_since_the_dump(&self) -> bool {
        self.inputs == self.inputs_at_dump
    }

    /// **Are the area buttons already ours, without pressing anything?**
    ///
    /// The dev, watching the 1828Z run: *the re-center just before we enter Ulrome Village seems
    /// unnecessary.* It is, and `core.arriveAt` (`overworldview.lua:1418-1443`) says so in the order
    /// it does three things:
    ///
    /// ```lua
    /// core.refreshAreaButtons(location)                     -- :1432
    /// if userConfig.interface.centreMapOnArrive and ... then
    ///     core.centreScreenOnPlayer()                       -- :1439
    /// end
    /// core.verboseAdjacencyData('Arrived at location')      -- :1442
    /// ```
    ///
    /// `refreshAreaButtons` **is** `selectedLocation = location; selectedLocationName = key` plus
    /// that location's buttons (`:474-476`), and `centreMapOnArrive` ships **true**
    /// (`utils/defaultconfig.lua:13`). The dump is printed *last*, so **holding an `Arrived at
    /// location` dump for the node under our feet is the receipt that both already happened.**
    ///
    /// So [`Run::select_here`] — click an empty map point to raise the arrow, then click the arrow
    /// to refresh and re-centre — asks for work the arrival did. Worse than redundant: the first
    /// click *deselects*, clearing the buttons and inserting the arrow, so the pair destroys correct
    /// state and rebuilds it.
    ///
    /// The comment this replaces kept the arrow press because *it is the press that sets
    /// `selectedLocation` to us*. True of the arrow, and beside the point after an arrival. The run
    /// of 2026-08-20 it remembers is the case where **no arrival just happened** — out of combat,
    /// out of a mode change, after our own click at the map — and
    /// [`Run::untouched_since_the_dump`] is exactly that distinction.
    fn the_arrival_already_selected_us(&self) -> bool {
        self.untouched_since_the_dump()
            && self.latest.as_ref().is_some_and(|a| arrival_selected_us(a, self.map.here()))
    }

    /// **A settled dump we did not have to pay a pan for.**
    ///
    /// The other half of the same finding, and the half that costs a click on every surface step.
    /// The arrival's own `centreScreenOnPlayer` starts the tween, and the tween announces itself
    /// when it lands: `if offsetTransition == 1 then core.verboseAdjacencyData('Screen pan
    /// finished')` (`overworldview.lua:1254-1256`). The 1828Z console shows the pair with nothing of
    /// ours in between:
    ///
    /// ```text
    /// Arrived at location    l10   Ulrome village
    /// Screen pan finished    l10   Ulrome village
    /// ```
    ///
    /// So [`Run::recentre`]'s two clicks trigger a pan that has already run. Its *wait* was the only
    /// part ever needed — the arrival dump's own coordinates are taken mid-glide, which is why they
    /// cannot be used and why this asks for the pan-finished dump instead.
    ///
    /// ## It has to read the feed, not just [`Run::latest`]
    ///
    /// The dev, watching the 2211Z run of 2026-08-23: *at the Gipsyville crypt, it seems we did
    /// redundant locate-me again.* The pan dump was there — the console holds
    /// `Arrived at location l19` and then `Screen pan finished l19`, all five neighbours on screen,
    /// so nothing was wrong with it — and this said no anyway, because it was reading a field
    /// instead of the feed.
    ///
    /// The order is what made that inevitable. `latest` is only written by [`Run::pump`], the last
    /// pump of a surface iteration is before the event handlers, and the pan lands a **full second**
    /// after the arrival (`offsetTransition` reaches 1 at `overworldview.lua:1250-1256`), by which
    /// time the driver is already inside the 2.5-second `Finish` probe. The dump duly printed, into
    /// a buffer nobody read again before the decision. So the fix is one pump and not a wait:
    /// `Screen pan finished` is a *line*, and asking for it means reading the console.
    fn settled_dump_in_hand(&mut self) -> Option<Adjacency> {
        self.pump();
        match self.untouched_since_the_dump() {
            true => self.latest.as_ref().filter(|a| a.reason.contains("pan")).cloned(),
            false => None,
        }
    }

    fn select_here(&mut self) -> bool {
        !matches!(self.locate_me(false), Located::Failed)
    }

    fn recentre(&mut self) -> Option<Adjacency> {
        match self.locate_me(true) {
            Located::Panned(a) => Some(a),
            _ => None,
        }
    }

    /// The observer's answer, when it has one worth acting on.
    ///
    /// `Screen::Unknown` is the map — see [`crate::act::identify`], whose whole set is the screens
    /// that are *not* the map — so `Some` here means "definitely not the map" and `None` means
    /// "nothing recognised, carry on". Taking `win` rather than `&self` so it can be called from a
    /// loop that is already borrowing the log.
    fn a_screen_we_know(win: &GameWindow) -> Option<crate::act::Screen> {
        match crate::act::identify(win) {
            crate::act::Screen::Unknown => None,
            named => Some(named),
        }
    }

    fn locate_me(&mut self, wait_for_pan: bool) -> Located {
        let (cw, ch) = self.win.client_size().unwrap_or((1920, 1080));
        // **Ask what is on screen before clicking at it.** The dev, 2026-08-22, watching a class
        // unlock get four blind clicks before the observer was consulted: *if Diggle doesn't know
        // what it's doing, shouldn't it call the observer sooner?*
        //
        // `identify` does run first — at the top of the driver's loop — but it is one look, once per
        // pass, and everything downstream acts on that verdict for the rest of the iteration. A
        // screen that animates in *after* the look is invisible until the next one. Live: the
        // Woodsman's shop closed, `Unlock` faded in, and this walked the whole candidate list
        // clicking into a full-screen panel; the pass ended, the loop went round, and the observer
        // named it at **1.0000**.
        //
        // Cheap, because the map is `Screen::Unknown` — a name means definitely-not-the-map, so this
        // never refuses a real locate-me. One capture against four clicks and four half-second
        // waits, on a path that was about to take five captures anyway.
        if let Some(named) = Self::a_screen_we_know(self.win) {
            self.log.push_str(&format!("  not on the map: the observer calls this {named:?}\n"));
            return Located::Failed;
        }
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
            let _ = self.tap("locate-me: clicking empty map", ex, ey);
            self.park();
            // Watched rather than waited out, for the reason [`Run::click_area_button`] gives at
            // length: the arrow is drawn on the frame after the click, and the test for it is a
            // *crop* — so sampling it costs almost nothing and the dissolve is only how long we are
            // prepared to keep asking. A candidate that is not an empty map point still costs the
            // whole of it, exactly as before.
            let by = Instant::now() + crate::timing::SCREEN_DISSOLVE;
            let slot = loop {
                std::thread::sleep(crate::timing::POLL_SELECT);
                self.pump();
                let slot = self.read_slot(&affirm::SHOW_AREA_BUTTONS);
                if slot.state.is_ready() || Instant::now() >= by {
                    break slot;
                }
            };
            self.log.push_str(&format!(
                "  locate-me slot after clicking empty map at ({cx},{cy}): {:?} (score {:.2})\n",
                slot.state, slot.score
            ));
            if !slot.state.is_ready() {
                // Two hypotheses for this reading, and only one of them was ever considered. The old
                // comment held the first: *that point was not empty, the click selected something,
                // try elsewhere.* The second is that **we are not on the map at all** — and it is
                // the dangerous one, because every remaining candidate is then a blind click into
                // whatever is there. A road at (213, 18) once opened the character screen and the
                // run had no way back.
                //
                // So the observer is asked again rather than after the list is exhausted: a screen
                // can arrive between two of these clicks as easily as before the first.
                if let Some(named) = Self::a_screen_we_know(self.win) {
                    self.log.push_str(&format!(
                        "  candidate {} came back empty because this is {named:?}, not the map\n",
                        n + 1
                    ));
                    return Located::Failed;
                }
                // The first hypothesis, then: try elsewhere.
                continue;
            }
            let Ok((lx, ly)) = self.win.client_to_screen(SHOW_AREA_BUTTONS.0, SHOW_AREA_BUTTONS.1)
            else {
                continue;
            };
            // Counted BEFORE the click. Anything already in hand describes the map as it was, and the
            // whole point of pressing the arrow is to learn where the map is now.
            let before = self.dumps;
            let _ = self.tap("show the area buttons", lx, ly);
            self.park();
            // **Selection only.** The arrow is replaced by the location's buttons when the press
            // lands, so the slot answers directly and there is nothing to wait for.
            if !wait_for_pan {
                self.pump();
                let after = self.read_slot(&affirm::SHOW_AREA_BUTTONS);
                return match after.state.is_ready() {
                    true => Located::Failed,
                    false => Located::Selected,
                };
            }
            let by = Instant::now() + Duration::from_secs(12);
            while Instant::now() < by {
                std::thread::sleep(crate::timing::POLL_SCREEN);
                self.pump();
                if self.dumps > before {
                    if let Some(a) = self.latest.as_ref().filter(|a| a.reason.contains("pan")) {
                        return Located::Panned(a.clone());
                    }
                }
            }
            // Two very different failures reach this line, and the run that needed to tell them apart
            // could not. `mousereleased` on the arrow does `refreshAreaButtons` and
            // `centreScreenOnPlayer` together (`overworldview.lua:488-494`), so a press that lands
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
        Located::Failed
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
    /// Photograph the whole window, for a failure we cannot explain from the console alone.
    ///
    /// Added because a rest failed live and the only honest answer to "what was on screen" was that
    /// nobody had looked. `ui/inn.lua:39-66` declares `Shop` and `Rest` statically, with no
    /// `activeIf` and no variant sets, so the *source* says our coordinate cannot be wrong — but a
    /// screen nobody photographed is not evidence, and arguing from source about a live failure is
    /// how this project has been wrong before.
    /// Photographs the screen under a tag that is made unique before it is used.
    ///
    /// **A fixed name loses the evidence it was taken for.** The rest loop photographs its last
    /// failed look, and on 2026-08-11 it did so twice in one run: the second inn visit's frame
    /// overwrote the first's. The two failures had different causes — the survivor showed the rest
    /// screen, and the one destroyed was of the inn, which is the one nobody could then explain.
    /// Photographs are cheap and a run writes few of them; collisions are not worth the disk they
    /// save.
    ///
    /// **Public because the moments worth photographing include the ones before `drive` starts.**
    /// A score in a report says how badly a template matched; it cannot say what the screen was, and
    /// re-running to find out only works when the state repeats. The failure this was opened up for
    /// does not repeat: a fresh profile shows the auto-save card once, and the game sets
    /// `persistent.tutorials.autoSave` and writes it the moment it does
    /// (`ui/publishersplash.lua:50-52`), so the second look is at a different screen.
    pub fn snap_screen(&mut self, tag: &str) {
        let n = self.snaps.entry(tag.to_string()).or_insert(0);
        *n += 1;
        let tag = &if *n == 1 { tag.to_string() } else { format!("{tag}-{n}") };
        match crate::win::capture::capture_window(self.win) {
            Ok(f) => {
                let path = Path::new(FRAMES).join(format!("{tag}.png"));
                match f.write_png(&path) {
                    Ok(()) => self.log.push_str(&format!("  photographed the screen as `{tag}`\n")),
                    Err(e) => self.log.push_str(&format!("  could not write {tag}: {e}\n")),
                }
            }
            Err(e) => self.log.push_str(&format!("  could not capture {tag}: {e}\n")),
        }
    }

    /// Logs the loudest button fingerprints on whatever is currently on screen.
    ///
    /// For the case where [`crate::act::identify`] says `Unknown` and that answer is *surprising*.
    /// `Unknown` is the absence of every check passing, so it names nothing and cannot distinguish
    /// "no button is drawn" from "one is drawn and scored just under its bar" — and this project has
    /// spent two runs on that distinction already.
    ///
    /// The top few only. The full registry is long, most of it scores near zero on any given screen,
    /// and a wall of noughts in a run report is how a real reading gets missed.
    pub fn log_button_scores(&mut self) {
        let mut scored: Vec<(&str, f64)> = crate::act::ALL
            .iter()
            .filter_map(|b| crate::act::score_exact(self.win, b).ok().map(|q| (b.name, q)))
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        let top: Vec<String> = scored.iter().take(4).map(|(n, q)| format!("{n} {q:.4}")).collect();
        match top.is_empty() {
            true => self.log.push_str("  nothing could be scored — the capture itself failed\n"),
            false => self.log.push_str(&format!("  loudest fingerprints: {}\n", top.join(", "))),
        }
    }

    /// Send a click, and record what it was for.
    ///
    /// ## Why every input is logged, and why it took a disagreement to build
    ///
    /// The dev watches runs by ear and eye — clicks have a sound and a cursor jump — and on
    /// 2026-08-16 concluded from that that multi-hop travel was not working. The console said one
    /// press had walked three edges (`e2 -> l41 -> l25 -> l4` inside a single step). Both readings
    /// were honest and neither could be checked against the other, because **nothing recorded what
    /// each input was for**.
    ///
    /// They are also both right, which is the point. A multi-hop walk fires an arrival event at
    /// every node it passes (`overworldview.lua:1210-1216`), and the wait loop answers those — so
    /// clicks keep coming at each hop while the travel press happened once. Watching the inputs
    /// cannot separate the two, and neither could the report.
    ///
    /// So: a numbered line per input, with the coordinate and the reason, interleaved with the steps
    /// in the same report. "Was that a travel press or an event answer?" becomes a lookup.
    ///
    /// Kept deliberately cheap — a counter and a `push_str` — so it can stay on in every run rather
    /// than being a debug mode nobody remembers to enable.
    fn tap(&mut self, why: &str, sx: i32, sy: i32) -> bool {
        self.inputs += 1;
        self.log.push_str(&format!("  input {}: click ({sx},{sy}) — {why}\n", self.inputs));
        click_at_in(self.win, sx, sy).is_ok()
    }

    /// Click a node on the map, then **watch** the area-button strip until it changes.
    ///
    /// Returns the fraction of the strip that moved, for the caller to weigh against
    /// [`SELECT_MOVED`] exactly as it always has.
    ///
    /// ## Why this is a poll and was a 900 ms sleep
    ///
    /// The dev, 2026-08-23: *it's the main navigator driver loop where I think we can save the most
    /// time.* This is that loop's largest fixed cost — a hop pays it once to select and, when the
    /// crossing arm is the one running, again — and none of it was ever the game's.
    ///
    /// **Selecting a node is instantaneous.** `core:mousereleased` (`overworldview.lua:1472`)
    /// compares `mousePressedOn` against what is under the cursor and sets the selection; there is
    /// no animation, no mode change, and — checked against a whole run's console — **no printed
    /// line**, the three reasons a dump carries being only `World loaded`, `Arrived at location` and
    /// `Screen pan finished`. So the console cannot answer this and the strip is the only witness.
    /// It is repainted from `getAreaButtons` on the next frame.
    ///
    /// The old shape sampled that witness **once, 900 ms later**. This samples it every
    /// [`crate::timing::POLL_SELECT`] and stops at the first frame that shows the change, keeping
    /// the same 900 ms as the deadline — so a selection that fails costs exactly what it used to and
    /// one that works costs a tenth of it.
    ///
    /// ## Two things make the poll affordable
    ///
    /// [`crate::win::capture::capture_window`] is `PrintWindow` with `PW_RENDERFULLCONTENT`, which
    /// asks the game to re-render the whole client area; polling *that* would have cost more than
    /// the sleep it replaced — see the note on cropping in poll loops. [`capture_client_rect`] is a
    /// `BitBlt` of one rectangle, and [`AREA_BUTTONS`] is 45% by 18% of the window, so each sample
    /// reads about a twelfth of the pixels by the cheaper of the two paths.
    ///
    /// The crop is *exactly* [`AREA_BUTTONS`], so `diff_fraction` over the whole cropped frame is
    /// the same quantity the full-window call produced, and [`SELECT_MOVED`] did not have to move.
    ///
    /// If the rectangle cannot be established or the first capture fails, this falls back to what it
    /// replaced: sleep the whole budget and diff two full windows. Losing the optimisation must not
    /// mean losing the selection.
    fn select_and_watch(&mut self, why: &str, sx: i32, sy: i32) -> f64 {
        use crate::win::capture::{capture_client_rect, capture_window, Region};
        const WHOLE: Region = Region { nx: 0.0, ny: 0.0, nw: 1.0, nh: 1.0 };
        let budget = crate::timing::AFTER_SELECT;

        let rect = self.win.client_size().ok().map(|(cw, ch)| {
            (
                (AREA_BUTTONS.nx * cw as f64) as i32,
                (AREA_BUTTONS.ny * ch as f64) as i32,
                (AREA_BUTTONS.nw * cw as f64) as i32,
                (AREA_BUTTONS.nh * ch as f64) as i32,
            )
        });
        let cropped = rect.and_then(|(x, y, w, h)| {
            capture_client_rect(self.win, x, y, w, h).ok().map(|f| (f, (x, y, w, h)))
        });

        let Some((before, (x, y, w, h))) = cropped else {
            // The old path, verbatim, for a window we could not crop.
            let before = crate::win::capture::capture_window(self.win).ok();
            let _ = self.tap(why, sx, sy);
            std::thread::sleep(budget);
            self.pump();
            let after = capture_window(self.win).ok();
            return match (before, after) {
                (Some(b), Some(a)) => b.diff_fraction(&a, AREA_BUTTONS),
                _ => 0.0,
            };
        };

        let _ = self.tap(why, sx, sy);
        let deadline = Instant::now() + budget;
        let mut moved = 0.0;
        loop {
            std::thread::sleep(crate::timing::POLL_SELECT);
            if let Ok(now) = capture_client_rect(self.win, x, y, w, h) {
                moved = before.diff_fraction(&now, WHOLE);
                if moved > SELECT_MOVED {
                    break;
                }
            }
            if Instant::now() >= deadline {
                break;
            }
        }
        self.pump();
        moved
    }

    /// Send a key, and record what it was for. See [`Run::tap`].
    fn tap_key(&mut self, why: &str, vk: u16, sc: u16) -> bool {
        self.inputs += 1;
        self.log.push_str(&format!("  input {}: key {vk:#06x} — {why}\n", self.inputs));
        self.keys.press_key(vk, sc).is_ok()
    }

    /// Have we been here before with the run no further along than it is now?
    ///
    /// Returns how many sterile visits this node has had, counting this one.
    ///
    /// ## Why this exists at all
    ///
    /// Three ping-pongs in two days — `l19`↔`l10`, `shrine2`↔`l10`, `l18`↔`l10` — each with a
    /// different root cause, each fixed on its own, and **not one of them noticed by the program**.
    /// Every one was spotted by the dev watching the screen, and cost an evening between the
    /// watching and the diagnosis.
    ///
    /// `docs/superpowers/notes/navigation-loops.md` has said from the start that every navigation
    /// bug here reduces to a missing monotone measure or a missing memory. Each fix supplied one
    /// locally — `abandon` for shrines, `heart_bought` for shops, `crossing_to` for doors. None of
    /// them generalises, because the next loop will be about something none of them tracks.
    /// [`crate::overworld::WorldMap::progress`] is the general measure, and this is the memory.
    ///
    /// ## Why it stops rather than steers
    ///
    /// It would be easy to make this *correct* the run — write the current target off and re-plan —
    /// and that is the wrong first move. A loop is a disagreement between two pieces of reasoning,
    /// and papering over it hides the disagreement while leaving both halves wrong; the `l18` loop
    /// would have been silently absorbed and the map-copy bug behind it would still be there, ready
    /// to produce something subtler. The dev's own framing was that the fourth loop should
    /// *announce itself* rather than be found by someone watching.
    ///
    /// So: stop, name the cycle, and print the sequence that produced it. Cheap to relax later if a
    /// run is ever lost to a loop it could have walked out of.
    fn sterile_here(&mut self, here: &str) -> usize {
        let now = self.map.progress();
        self.guard.visit(here, now)
    }

    /// Is the area-button slot offering `Combat` for whatever is selected right now?
    ///
    /// The dev's rule, 2026-08-16: *make sure the Combat press is either preceded by a console check
    /// if it's available and tells you when the node is interactable, or a template inlier check so
    /// that we know when it's available.*
    ///
    /// It has to be the template. The console prints the adjacency dump and nothing else
    /// (`overworldview.lua:1025-1053`); selecting a location is silent (`:475`, `:1493`), and no line
    /// anywhere names the selected node's buttons. There is no console channel to check.
    ///
    /// Always logged, match or no match, because the score is the diagnosis. A blind press that
    /// achieves nothing and a press that lands on the wrong plank produce the same
    /// `screen moved 0.000`, and telling them apart took a screenshot read by hand — see
    /// [`crate::act::AREA_COMBAT`] for the run this cost.
    fn combat_is_on_offer(&mut self) -> bool {
        let q = self.area_slot_score();
        self.log.push_str(&slot_reading(q));
        q.is_some_and(|q| q >= crate::act::AREA_BUTTON_SHOWING)
    }

    /// The raw agreement between the area-button slot and [`crate::act::AREA_COMBAT`].
    ///
    /// One measurement, two questions: *which* button is in the slot
    /// ([`crate::act::AREA_BUTTON_SHOWING`]) and whether **any live one** is
    /// ([`crate::act::AREA_BUTTON_LIVE`]). Separated from the verdicts so a caller that wants the
    /// second does not have to re-capture to get it.
    ///
    /// A capture fault answers `None` rather than a low score, because the two want opposite fixes
    /// and reading a fault as "nothing live" would send the recovery below chasing a slot that was
    /// never looked at. Reported rather than swallowed, for the reason `wait_for` counts faults.
    fn area_slot_score(&mut self) -> Option<f64> {
        match crate::act::score_exact(self.win, &crate::act::AREA_COMBAT) {
            Ok(q) => Some(q),
            Err(e) => {
                self.log.push_str(&format!("  area slot could not be read: {e}\n"));
                None
            }
        }
    }

    /// **Look for a live area button, and re-select this node until one appears.**
    ///
    /// The slot read used to be a line in the log that nothing acted on. Step 39 of run
    /// `spike-run-20260821-0251Z` printed `area slot: something else (Combat 0.7367, gate 0.95)`,
    /// pressed, moved the screen 0.029, and stopped with `Combat did not open at l38`. **0.7367 is
    /// exactly the greyed-`Combat` reading from the frame corpus** — the observer did not merely
    /// doubt the slot, it identified the state precisely, and nothing acted on it.
    ///
    /// ## Why re-selecting is the recovery rather than waiting
    ///
    /// A slot holding another node's buttons is not a slot that is about to change on its own.
    /// [`Run::select_here`] is what puts *ours* back: it clicks empty ground, which clears the strip
    /// down to the arrow (`overworldview.lua:1482-1487`), and then presses the arrow, whose handler
    /// re-inserts the player's own buttons and sets `selectedLocation` to the player
    /// (`:488-494`, `:474-476`). That is precisely the state the greyed plank says we are not in.
    ///
    /// ## It ends in a press either way, and that is deliberate
    ///
    /// [`crate::act::AREA_BUTTON_LIVE`] is calibrated on three live planks, all short words, and
    /// `Travel`, `Open` and `Wake up` have never been captured — so a live button reading under the
    /// bar is a state the number has never seen. Refusing on it could stall a run that today merely
    /// presses and moves on, which would be trading a known fault for an unknown one.
    ///
    /// So the looks are spent, the reading is reported, and the press happens regardless. The value
    /// is entirely in the retry: the fault is a **stale** slot, and re-selecting is what clears one.
    /// Returns whether the slot ended up reading live, for the caller's log.
    fn look_for_a_live_slot(&mut self) -> bool {
        for look in 1..=SLOT_LOOKS {
            let q = self.area_slot_score();
            self.log.push_str(&slot_reading(q));
            if q.is_some_and(|q| q >= crate::act::AREA_BUTTON_LIVE) {
                return true;
            }
            if look == SLOT_LOOKS {
                break;
            }
            self.log.push_str(&format!(
                "  nothing pressable in the slot — re-selecting this node and looking again \
                 (look {} of {SLOT_LOOKS})\n",
                look + 1
            ));
            self.select_here();
            std::thread::sleep(SLOT_RETRY_PAUSE);
            self.pump();
        }
        false
    }

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
    ///
    /// ## This is a blind click, and the return value does not say otherwise
    ///
    /// `Ok(true)` means **the input was sent**, not that a button received it. Nothing here looks at
    /// the screen before pressing or after. Callers must confirm the outcome themselves — every one
    /// currently does, by waiting for the console line the target screen prints from `onActive`.
    ///
    /// That is weaker than the rule in §7 of the v2 spec, and it cost a live run: `Rest` was pressed
    /// at the right coordinate, nothing happened, and the log could not distinguish a button that
    /// was not live yet from a click that missed from the wrong screen being up. Settling it took a
    /// screenshot taken by hand.
    ///
    /// The principled fix is available and not built: the game **warps the real OS cursor** to the
    /// selected control when its hotspot highlight moves, and the inn screen carries
    /// `ui.elements.hotspot` (`ui/inn.lua:78`). So the game can be asked where its buttons are —
    /// nudge the highlight, read `GetCursorPos`, and the answer comes from the game rather than from
    /// our arithmetic. That is the same oracle `locate-me` already uses. Task #19.
    fn click_button(&mut self, spec: &crate::win::window::ButtonSpec) -> bool {
        let Ok((cw, ch)) = self.win.client_size() else { return false };
        let (x, y) = crate::win::window::button_center(spec, cw, ch);
        self.inputs += 1;
        self.log.push_str(&format!(
            "  input {}: click ({x},{y}) — a button at its declared coordinate
",
            self.inputs
        ));
        self.keys.click(x, y).is_ok()
    }

    /// Logs an `affirmative slot:` reading, folding up a run of identical ones.
    ///
    /// **The arrival wait produces these by the hundred.** It polls every 300 ms for up to 30
    /// seconds and calls [`Run::clear_text_screen`] each time, so a crossing that never arrives
    /// writes one line per poll. The run of 2026-08-17 0436Z ended with roughly five hundred
    /// consecutive copies of `affirmative slot: Absent (score 0.04, margin 0.00)` — the tail of the
    /// report was that and nothing else, and the last thing the driver actually *did* was five
    /// hundred lines above the stop.
    ///
    /// Survivable while the log only appeared at exit. Not survivable now it streams
    /// ([`Run::flush_log`]), because the noise arrives on the terminal in real time and buries the
    /// line someone is watching for.
    ///
    /// **The first reading is still logged in full**, which is what keeps the calibration argument
    /// at the call site true: a threshold that has never been measured against a live screen has to
    /// show its score for the negative case, or `Absent` cannot be told from a bar set too high.
    /// What is dropped is only the identical repeats behind it, replaced by a count when the streak
    /// ends.
    ///
    /// The count is emitted lazily, so a streak that is still open when something else logs will
    /// have its summary land after that line rather than before it. Accepted rather than solved:
    /// the alternative is routing every write in this file through one place, and the streaks this
    /// exists for are produced by a loop that logs nothing else.
    fn note_affirmative(&mut self, line: String) {
        if let Some((last, n)) = &mut self.affirm_repeat {
            if *last == line {
                *n += 1;
                return;
            }
        }
        self.close_affirmative_run();
        self.log.push_str(&line);
        self.affirm_repeat = Some((line, 1));
    }

    /// Ends a streak of identical affirmative readings, noting how many were folded away.
    ///
    /// Called at the top of every step so a count cannot span two of them, and whenever the reading
    /// itself changes.
    pub fn close_affirmative_run(&mut self) {
        if let Some((_, n)) = self.affirm_repeat.take() {
            if n > 1 {
                self.log.push_str(&format!(
                    "  … the affirmative slot said that {} more times\n",
                    n - 1
                ));
            }
        }
    }

    /// Prints whatever has been added to [`Run::log`] since the last call, and flushes it.
    ///
    /// **The log used to reach the terminal exactly once, at exit.** `spike_run` accumulates every
    /// line into one `String` and prints it in `finish`, so a run in progress showed *nothing* —
    /// and a run that is still going is precisely when someone is watching and wants to know what
    /// it thinks it is doing. The dev, 2026-08-17, watching a ping-pong with no way to see the
    /// errand driving it: *your output log should be flushed more frequently.*
    ///
    /// It is not stdout buffering, so `--nocapture`-style fixes would not have helped; the text did
    /// not exist yet. This emits the new tail per step instead, which also means a run killed
    /// part-way — Ctrl-C, a crash, a `taskkill` — leaves its reasoning on the screen rather than
    /// taking it down with the process. `finish` still writes the whole log to both files, so the
    /// archive is unchanged; it just no longer reprints what has already been shown.
    ///
    /// Byte-sliced rather than line-buffered because `log` is only ever appended to, so the cursor
    /// can only fall inside it — and every push ends in `\n`, so the boundary is never mid-character
    /// even with the em-dashes this project's log lines are full of.
    pub fn flush_log(&mut self) {
        use std::io::Write;
        if self.logged >= self.log.len() {
            return;
        }
        let fresh = &self.log[self.logged..];
        print!("{fresh}");
        let _ = std::io::stdout().flush();
        self.logged = self.log.len();
    }

    /// What has not yet been printed, so `finish` can show the tail without repeating the run.
    pub fn unflushed(&self) -> &str {
        &self.log[self.logged.min(self.log.len())..]
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
            std::thread::sleep(crate::timing::POLL_CONSOLE);
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
    fn rest_at_inn(&mut self) -> Rested {
        use crate::innplay;

        // 1. `Enter`. It is the ordinary area button, in the slot everything else uses.
        // Pressed and not watched: [`crate::innplay::ENTERED`] on the line below is the test, and
        // the diff this used to take was thrown away. The mark is claimed **before** the press so a
        // line the game prints faster than we can ask for it is still inside the window we search.
        let mark = self.feed.mark();
        self.press_area_button("Enter");
        if !self.wait_for_line(mark, innplay::ENTERED, Duration::from_secs(8)) {
            self.log.push_str("  rest: `Enter` did not open the inn\n");
            return Rested::Failed;
        }

        // 2. Rounds. Each opens the rest screen from the inn — so each gets a block that is current
        //    — and each ends back on the inn, the dream path included, since the dream's own `back`
        //    lands there.
        let mut done = 0;
        // Set by the inn telling us there is nothing to buy -- full health, or a purse it will not
        // serve. Distinct from `done == 0`, which is also what a broken visit looks like.
        let mut nothing_to_do = false;
        while done < innplay::MAX_PRESSES {
            // **The abort, before another night is bought.** Twenty presses at up to
            // `REST_TRIES * REST_WAIT` each is minutes of a run the dev has already asked to stop.
            // Breaking rather than returning a new error: what is left of the visit is leaving the
            // inn, which the code below does anyway, and the driver's own check ends the run on the
            // next iteration. See [`crate::config::stop_requested`].
            if crate::config::stop_requested() {
                self.log.push_str(&format!(
                    "  rest: stop requested after {done} press(es) — leaving the inn\n"
                ));
                break;
            }
            // Press `Rest` until the screen says it opened. See [`innplay::REST_TRIES`] for why one
            // press is not enough: the inn announces itself from `onActive`, before it can take a
            // click, and a run lost an inn to that and walked to the next village at 7/20.
            // **Wait for the button, then press it.** Not "press at the arithmetic centre and hope":
            // `click_when_ready` polls `locate` until the artwork is actually on screen, which is
            // the one thing that separates *not there yet* from *not there*. The blind version
            // failed live on 2026-08-09 and could say nothing about why.
            let mut opened = None;
            for try_n in 1..=innplay::REST_TRIES {
                let mark = self.feed.mark();
                match crate::act::click_when_ready(
                    self.win,
                    &crate::act::INN_REST,
                    innplay::REST_WAIT,
                ) {
                    Ok(q) => {
                        self.log.push_str(&format!("  rest: `Rest` found at {q:.4}\n"));
                        // **Get off the button before anything looks at it again.**
                        //
                        // A click leaves the pointer on the artwork, and `button.lua:122,159` then
                        // draws `button-up-hover.jpg` over it until a `mousemoved` says otherwise
                        // (`:223-269`). There is no timeout on that; it is a state, not an
                        // animation. `act::INN_REST`'s template is the un-hovered plaque, and the
                        // hovered one scores 0.5452 against it — see
                        // `act::a_hovered_plaque_does_not_match_its_own_template`.
                        //
                        // Live 2026-08-15 at The Quacking Duck: three presses filled the bar, the
                        // next round went looking for `Rest` to open another one, and found nothing
                        // for twelve seconds while the plaque sat in plain view. Then `leave_inn`
                        // ran the same `locate`, decided we were already outside, and left the run
                        // standing on an inn screen that the identifier went on to call a shrine.
                        //
                        // Parking is how this run keeps a hover out of a reading everywhere else —
                        // `click_area_button` has done it since the `Start` it could not read for
                        // four seconds. This path simply never did.
                        self.park();
                    }
                    Err(e) => {
                        // **Not a verdict — try again.** This used to `break`, which meant
                        // [`innplay::REST_TRIES`] covered only the "pressed it and nothing opened"
                        // case and never the one it was written for: the inn announces itself from
                        // `onActive`, *before* it can take a click. A dream re-enters the inn by
                        // exactly that route, so the round after a dream met an inn that was not
                        // ready and gave up on the first look, 1.5s in.
                        //
                        // Live 2026-08-10, and it is the same failure this function's own doc
                        // records from 2026-08-08: 1/20, one rest, a dream, then out of the inn at
                        // 7/20 with 696 gold unspent. The retry was already sitting here; one arm
                        // of it just did not reach.
                        //
                        // The cost of retrying is bounded and small — `REST_TRIES` × `REST_WAIT`,
                        // ~12s — and it is only paid when the button genuinely never comes.
                        //
                        // **RETRACTED 2026-08-15.** This used to claim the budget was spent at full
                        // health, because "the plaque reads `+0 (full)` and does not match this
                        // artwork at all". It does not: `ui/inn.lua:55` declares a constant `Rest`
                        // with no `activeIf`, and the very next visit in that same run matched it at
                        // 1.0000 with the bar already full. The twelve seconds were real and the
                        // cause was the pointer, not the health — see the `Ok` arm above. The
                        // reading was invented from the behaviour rather than taken from the screen,
                        // and the screen was one `snap_screen` away the whole time.
                        self.log.push_str(&format!(
                            "  rest: `Rest` not on screen yet (try {try_n} of {}) — {e}\n",
                            innplay::REST_TRIES
                        ));
                        if try_n == innplay::REST_TRIES {
                            self.snap_screen("rest-no-button");
                        }
                        continue;
                    }
                }
                if self.wait_for_line(mark, innplay::REST_SCREEN, innplay::REST_WAIT) {
                    opened = Some(mark);
                    break;
                }
                // Recognised and pressed, and the screen still did not open. A different failure
                // from the one above and worth its own picture: the click landed on the artwork we
                // matched, so the button was there and did not take.
                self.log.push_str(&format!(
                    "  rest: pressed a `Rest` that was on screen, but no rest screen (try {try_n} \
                     of {})\n",
                    innplay::REST_TRIES
                ));
                if try_n == 1 || try_n == innplay::REST_TRIES {
                    self.snap_screen(&format!("rest-no-screen-{try_n}"));
                }
            }
            let Some(mark) = opened else {
                break;
            };
            let Some(data) = innplay::parse_rest_data(self.feed.since(mark)) else {
                self.log.push_str("  rest: the rest screen printed no `Rest data` block\n");
                // No press of our own here: `leave_inn` walks out from wherever this is.
                break;
            };
            // **Two reasons to press, and the inn cannot tell them apart.** The block on screen
            // answers the health half; the bank half is ours, and comes from the errand the run
            // is actually on — see [`crate::overworld::WorldMap::stacks_to_buy`], which is what the
            // purse may serve once the heart's reserve is out of it.
            //
            // **Discounted by what this visit has already landed**, because both of our numbers are
            // frozen until we leave — see [`innplay::still_wanted`], and the run that banked 25 for
            // a fight that wanted 16.
            let (gold, banking) =
                innplay::still_wanted(self.map.gold(), self.map.stacks_to_buy(), done);
            let presses =
                innplay::presses_needed(&data, gold, banking).min(innplay::MAX_PRESSES - done);
            self.log.push_str(&format!(
                "  rest: {} missing, {} a press, {gold} gold, {banking} stack(s) short — pressing {presses} time(s)\n",
                data.health_need, data.health_give
            ));
            // Nothing left to buy. The only exit from this loop that is not a failure, and the
            // answer comes from the inn rather than from our own arithmetic.
            if presses == 0 {
                self.log.push_str(match data.can_rest {
                    false => "  rest: done — the inn will not serve us\n",
                    true => "  rest: done — nothing left to heal\n",
                });
                nothing_to_do = true;
                break;
            }

            // 3. Press. Space rather than the button: `Rest` declares
            //    `userFunctionName = 'affirmative'` (`ui/rest.lua:513`) and `space = 'affirmative'`
            //    (`utils/defaultbinds/keyboard.lua:13`), and reading the binding beats trusting a
            //    coordinate on a screen we have never photographed.
            let mut dreamt = false;
            let mut stalled = false;
            for _ in 0..presses {
                // **Is the button live?** This one has an `activeIf` — inactive while a dream runs
                // and whenever the inn will not serve us (`ui/rest.lua:507-510`) — so unlike every
                // other press in this file, "the screen is up" genuinely does not imply "the press
                // will do something". The console can report a rest screen; only the artwork can
                // report a live button. Matching the active version is the difference.
                if matches!(crate::act::locate(self.win, &crate::act::REST_CONFIRM), Ok(None)) {
                    self.log.push_str("  rest: `Rest` is not live on the rest screen — stopping\n");
                    self.snap_screen("rest-button-inactive");
                    stalled = true;
                    break;
                }
                let mark = self.feed.mark();
                if !self.tap_key("affirmative: space", VK_SPACE, SC_SPACE) {
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
                break;
            }
            // The dream has already put us back on the inn. Otherwise we are still on the rest
            // screen, and the next round's `Rest` press needs something to hit — so this is the one
            // place that wants a single screen back rather than the way out.
            //
            // A `false` means the inn is not confirmed underfoot, and pressing `Rest` at an unknown
            // screen is how a run spends its budget clicking at scenery. Stop, and let `leave_inn`
            // find the way out from wherever this turned out to be.
            if !dreamt && !self.back_to_inn() {
                break;
            }
        }
        self.log.push_str(&format!("  rest: {done} press(es) landed\n"));

        // 4. Out, however many screens deep that turns out to be.
        self.leave_inn();
        match (done, nothing_to_do) {
            (0, true) => Rested::NothingToDo,
            (0, false) => Rested::Failed,
            _ => Rested::Healed,
        }
    }

    /// Clicks the back plaque, if there is one, and reports whether it was there.
    ///
    /// **The plaque is the instrument, not the screen behind it.** The dev's call, and it is the
    /// right one: "the bottom-left arrow button is the correct thing to look out for and click when
    /// we wish to exit the inn". Leaving used to be decided by asking whether `Rest` was on screen,
    /// which is a question about *this* screen's purpose — and that made the exit depend on a
    /// template we had just clicked and were therefore hovering. It read absent, and the run
    /// concluded it was outside while standing in the inn.
    ///
    /// The plaque has none of that against it:
    ///
    /// - **It is the same artwork everywhere it appears.** `ui/inn.lua:68-71` and
    ///   `ui/rest.lua:517-520` both declare a `small` button at `ss(0, 0.9)`, `xOffset 1.13`, with
    ///   `ui/graphics/icons/back.png`. [`crate::act::SHRINE_GOBACK`]'s template was cut from a
    ///   *shrine*, and it scores **1.0000** against the inn frame in the corpus at exactly its own
    ///   origin — the plaque is opaque, so the room behind it never reaches the pixels.
    /// - **We never hover it before looking.** The press it follows is `Rest`, at the other end of
    ///   the screen, and [`Run::park`] runs after this click too.
    /// - **Its absence is the answer we want.** No plaque means no way back from here, which is what
    ///   "out of the inn" means. Measured 0.3437 on a screen without one, against a 0.90 bar.
    ///
    /// [`crate::act::identify`] already reached this conclusion from the other side: it returns
    /// `Screen::Shrine` for any screen with a back plaque, notes that it fired on an inn once, and
    /// says the press was right even where the label was fiction. This is that fallback made
    /// deliberate, inside the flow that needs it.
    ///
    /// `click_when_ready` locates before it clicks, so a `false` here is a *measured* absence rather
    /// than a click sent into the dark.
    fn back_one_screen(&mut self) -> bool {
        match crate::act::click_when_ready(self.win, &crate::act::SHRINE_GOBACK, BACK_WAIT) {
            Ok(_) => {
                // Off the plaque before anything reads this corner again — the next look is either
                // this function on the screen behind, or `identify`.
                self.park();
                std::thread::sleep(crate::timing::SCREEN_DISSOLVE);
                self.pump();
                true
            }
            Err(_) => false,
        }
    }

    /// Leaves the rest screen and lands back on the inn, which announces itself.
    ///
    /// The round loop needs this rather than [`Run::leave_inn`]: the next round's `Rest` press has
    /// to have an inn to hit, and going all the way out would leave it pressing at the map.
    ///
    /// Returns whether the inn is confirmed underfoot. `false` means we do not know where we are —
    /// the plaque was missing, or it was pressed and no [`crate::innplay::ENTERED`] followed — and
    /// the caller's only safe move is to stop resting and let `leave_inn` clean up.
    fn back_to_inn(&mut self) -> bool {
        let mark = self.feed.mark();
        if !self.back_one_screen() {
            self.log.push_str("  rest: no back plaque on the rest screen\n");
            return false;
        }
        if !self.wait_for_line(mark, crate::innplay::ENTERED, Duration::from_secs(6)) {
            self.log.push_str("  rest: the rest screen did not close\n");
            return false;
        }
        true
    }

    /// Presses back until there is no back plaque left to press.
    ///
    /// Counting screens was wrong twice over, in opposite directions: `leave_inn(1)` from the rest
    /// screen left the run one screen short of the map, and the same call from the inn walked it one
    /// screen too far. Neither can happen to a loop that stops on the plaque being gone.
    ///
    /// Bounded because a screen we do not understand could carry a back plaque of its own, and
    /// pressing at one for ever is worse than reporting that we are stuck in it. The inn is two
    /// screens deep at most, so anything past [`LEAVE_INN_CLICKS`] is not the inn.
    fn leave_inn(&mut self) {
        for n in 1..=LEAVE_INN_CLICKS {
            if !self.back_one_screen() {
                return;
            }
            if n == LEAVE_INN_CLICKS {
                self.log.push_str(&format!(
                    "  rest: still on a screen with a way back after {n} presses\n"
                ));
                self.snap_screen("leave-inn-stuck");
            }
        }
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
        std::thread::sleep(crate::timing::REST_DREAM);
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

    /// Aims at the area slot, says what the game thinks is selected there, presses, and parks.
    ///
    /// The press itself, with no test of whether it worked — every caller supplies its own, and the
    /// three of them differ so much that this is the whole of what they share. See
    /// [`Run::press_area_button`] for the map of which test belongs to which press.
    ///
    /// **What the game itself thinks is selected, before we click at arithmetic.** Task #19, in its
    /// read-only first form: `AREA_BUTTON` is a coordinate transcribed by hand from `ss`/`os`
    /// multipliers in the Lua, and a transcription that is wrong looks exactly like one that is
    /// right until a run dies on it. When hotspot navigation is live the game has parked the real
    /// pointer on the centre of a control it computed itself, so the two can simply be compared.
    ///
    /// Reported, never acted on. `None` is the ordinary case — any real mouse movement clears the
    /// highlight (`main.lua:420`) — and a disagreement might equally mean the game has a *different*
    /// control selected, which is information rather than an error. See [`crate::win::cursor`] for
    /// why nothing here presses a direction key to find out.
    fn aim_at_the_area_slot(&mut self, what: &str) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(miss) = crate::win::cursor::miss_by(self.win, AREA_BUTTON) {
            self.log.push_str(&format!(
                "  the game has a control selected {miss:.0} px from where `{what}` is aimed\n"
            ));
        }
        let (bx, by) = self.win.client_to_screen(AREA_BUTTON.0, AREA_BUTTON.1)?;
        if !self.tap(&format!("area button: {what}"), bx, by) {
            return Err("click failed".into());
        }
        self.park();
        Ok(())
    }

    /// Presses the area slot and **looks at nothing**.
    ///
    /// ## The third kind of press
    ///
    /// [`Run::click_area_button`] asks *did the screen change*, [`Run::left_the_overworld`] asks
    /// *where are we now*, and this one asks nothing at all — because its callers already have a
    /// better test running than either, and it begins the moment the press lands.
    ///
    /// The subworld `Travel` is the press that named it. Its verdict has been deliberately discarded
    /// ever since a frame diff over one second called a *successful* move a failure: travel opens
    /// with a walk animation that barely changes the screen in that window, so the diff read 0.002
    /// while the player was in fact walking to the well. Arrival — `here` becoming the node we aimed
    /// at — replaced it as the test. The diff stayed on regardless, paid for and unread.
    ///
    /// The inn's `Enter` is the other: it waits on [`crate::innplay::ENTERED`], a line the game
    /// prints from the inn's own `onActive`, which is the console saying the thing a diff could only
    /// guess at.
    ///
    /// ## What the watch was costing
    ///
    /// [`crate::win::capture::capture_window`] is `PrintWindow` with `PW_RENDERFULLCONTENT`,
    /// measured at a **28.5 ms** median over this window, against 4.4 ms
    /// for a `BitBlt` crop. A watched press pays one of those before the click and one per poll,
    /// and cannot reach its first look sooner than [`crate::timing::POLL_SCREEN`].
    ///
    /// So the floor, for a press whose screen has plainly moved, is a quarter-second and two full
    /// captures — and a press that never clears [`AREA_BUTTON_MOVED`] pays the whole
    /// [`crate::timing::AFTER_AREA_BUTTON`] deadline and five of them. That second case is not the
    /// rare one: of the 27 subworld `Travel` presses in the 1828Z run of 2026-08-23, **9 came in at
    /// or under the bar**, median 0.233 and quietest 0.001. At the rate the 0547Z run pressed
    /// `Travel` that is the better part of a minute spent measuring a number nobody reads.
    ///
    /// ## Why dropping the watch costs nothing
    ///
    /// Nothing was reading the verdict, and the delay it incidentally provided is not needed either.
    /// `core.travelTo` (`overworldview.lua:1394-1400`) sets a path and lets `love.update` walk it,
    /// so there is no transition to sit out; the arrival loop pumps the console on its own
    /// [`crate::timing::POLL_ARRIVAL`] tick, and the inn's wait pumps on
    /// [`crate::timing::POLL_CONSOLE`]. Neither depended on this call for either time or text.
    ///
    /// **This is not the default, and it is not the cheap option to reach for.** A press with no
    /// test of its own belongs on [`Run::click_area_button`] or [`Run::left_the_overworld`]. This
    /// one is for a caller that can name the test it is using instead — which is why
    /// `no_press_is_left_without_a_test` refuses a discarded verdict from the other two.
    fn press_area_button(&mut self, what: &str) -> bool {
        match self.aim_at_the_area_slot(what) {
            Ok(()) => {
                self.log.push_str(&format!("  pressed {what} — the caller's own test decides\n"));
                true
            }
            Err(e) => {
                self.log.push_str(&format!("  could not press {what}: {e}\n"));
                false
            }
        }
    }

    fn click_area_button(&mut self, what: &str) -> Result<bool, Box<dyn std::error::Error>> {
        let before = crate::win::capture::capture_window(self.win)?;
        self.aim_at_the_area_slot(what)?;
        // **Watched, not waited out.**
        //
        // The dev, 2026-08-23: *it's the main navigator driver loop where I think we can save the
        // most time.* The 0547Z report pressed an area button 123 times and every one of them paid a
        // flat second before anything looked.
        //
        // None of that second came from the game. `StartTransition` swaps `activeMode` and the input
        // tables immediately (`main.lua:146-176`) and `transitionTimer` drives nothing but a
        // dissolve shader in `love.draw` (`:389-391`), so the screen begins changing on the very
        // next frame however long the dissolve runs. What the second was really covering is that the
        // *test* is expensive: `capture_window` is `PrintWindow` with `PW_RENDERFULLCONTENT` over
        // the whole client area, at a 28.5 ms median.
        //
        // So the deadline stays exactly where it was and only the sampling changes. The decision is
        // untouched — the same `before` frame, the same [`crate::observe::settle::FULL`] region and
        // the same [`AREA_BUTTON_MOVED`] bar — and a press that never lands still costs what it
        // always did.
        //
        // **The presses that were paying this for nothing have left**, which is the other half:
        // subworld `Travel` and the inn's `Enter` both had a better test of their own running and
        // discarded the verdict anyway. They now go through [`Run::press_area_button`], which is
        // where the measurement lives.
        let deadline = Instant::now() + crate::timing::AFTER_AREA_BUTTON;
        let (mut moved, mut looked) = (0.0, false);
        let mut failure = None;
        loop {
            std::thread::sleep(crate::timing::POLL_SCREEN);
            self.pump();
            match crate::win::capture::capture_window(self.win) {
                Ok(after) => {
                    looked = true;
                    moved = before.diff_fraction(&after, crate::observe::settle::FULL);
                    if moved > AREA_BUTTON_MOVED {
                        break;
                    }
                }
                Err(e) => failure = Some(e),
            }
            if Instant::now() >= deadline {
                break;
            }
        }
        // Every look failing is a broken window rather than an unmoved screen, and the two want
        // opposite things of the caller — so that case keeps the error it used to raise.
        if !looked {
            return Err(failure.map(Box::from).unwrap_or_else(|| "no capture succeeded".into()));
        }
        self.log.push_str(&format!("  clicked {what}: screen moved {moved:.3}\n"));
        Ok(moved > AREA_BUTTON_MOVED)
    }

    /// Presses the area slot and does not report success until the overworld is **actually behind
    /// us**, retrying the press if it is not.
    ///
    /// ## Why this exists rather than [`Run::click_area_button`]
    ///
    /// That one confirms with a pixel diff, and `moved > 0.05` answers *did anything change*, not
    /// *where are we now*. Those come apart in both directions. A press that opens nothing still
    /// moves the screen — the button's own `down` artwork, a tooltip clearing, the map drifting —
    /// and a press that opens a screen whose first frame is a fade moves almost nothing. Both
    /// mistakes have ended runs, and the second is the one that ends them silently, because the
    /// driver walks off to work on a screen that never arrived.
    ///
    /// So the split is by *intent*, and it is the distinction to keep. A press meant to take us
    /// **off** the map (`Visit`, `Shop`, `Combat`) is an identity question and belongs here. A press
    /// meant to keep us on it is a diff question and stays with `click_area_button` — unless the
    /// caller already has a stronger test of its own running, in which case the honest thing is to
    /// take no reading at all rather than pay for one and drop it, which is
    /// [`Run::press_area_button`].
    ///
    /// ## The retry, and why it is safe
    ///
    /// The overworld demonstrably swallows presses. Live 2026-08-17 at `shrine1sub1` the dev heard
    /// the button's own click and the shrine never opened — `button_down` plays inside
    /// `mousepressed` (`ui/elements/button.lua:278-281`) and only for a shown, active button, so the
    /// press landed and the *release* went missing (`mousereleased` bails on `if not down`, and
    /// `down` belongs to the button instance, which anything rebuilding the area buttons replaces).
    ///
    /// **The screen is identified before every press, not only after.** That is what makes a retry
    /// safe to fire at all: the coordinate that opens a screen from the map is, on several of the
    /// screens it opens, the plaque that leaves again — `Visit` at (188, 918) and the shrine's own
    /// `Go back` are the same point. Pressing blind a second time into a screen that opened *late*
    /// would close it, turning a recoverable stall into a silent walk-away. Checking first also
    /// makes the call idempotent, so a caller that is already off the map pays one look and nothing
    /// else.
    ///
    /// ## The console is the instrument; the template is the fallback
    ///
    /// **The project's standing rule** — the dev, 2026-08-17: *prefer the console, it should be the
    /// robust one when there are no timing issues to work around.* A printed line is the game
    /// stating what it did. A template match is us inferring it from pixels, and it can be wrong in
    /// both directions: a screen mid-fade scores like an absent one, and a plaque shared by four
    /// screens identifies none of them. So `says` is checked first on every poll, and `landed` only
    /// backs it up.
    ///
    /// **But the two answer different questions, and that is why both are here.** The console
    /// reports an *event* — "this screen just became active" — which is exactly right for confirming
    /// a press took, and useless for asking where we already are: an event we were not listening
    /// for leaves no trace. The template reports *state*, which is what the pre-press check needs.
    /// So the console confirms the transition and the template establishes the starting position,
    /// and neither substitutes for the other.
    ///
    /// A caller with no line to wait on passes `None` and gets the template alone, which is still
    /// better than a pixel diff.
    ///
    /// ## Where this is *not* used, deliberately
    ///
    /// The inn already does exactly this with [`crate::innplay::ENTERED`], and the fight branch
    /// polls [`crate::act::identify`] with its own recovery around a pregame that can arrive as
    /// either of two screens. Converting them would be a downgrade dressed as consistency.
    fn left_the_overworld(
        &mut self, what: &str, says: Option<&str>, landed: &[crate::act::Screen],
    ) -> bool {
        for attempt in 1..=LEAVE_TRIES {
            let now = crate::act::identify(self.win);
            if landed.contains(&now) {
                if attempt > 1 {
                    self.log.push_str(&format!(
                        "  `{what}` reached {now:?} on press {attempt} — the overworld swallowed \
                         the first {}\n",
                        attempt - 1
                    ));
                }
                return true;
            }
            // Same read-only cursor oracle `click_area_button` takes, and the same reason: the
            // coordinate is transcribed from Lua multipliers, and a wrong transcription looks
            // exactly like a right one until a run dies on it.
            if let Some(miss) = crate::win::cursor::miss_by(self.win, AREA_BUTTON) {
                self.log.push_str(&format!(
                    "  the game has a control selected {miss:.0} px from `{what}`\n"
                ));
            }
            let Ok((bx, by)) = self.win.client_to_screen(AREA_BUTTON.0, AREA_BUTTON.1) else {
                self.log.push_str(&format!("  could not place the `{what}` press on screen\n"));
                return false;
            };
            // Marked **before** the press, so a line the game prints faster than we can ask for it
            // is still inside the window we go on to search.
            let mark = self.feed.mark();
            if !self.tap(&format!("area button: {what}"), bx, by) {
                self.log.push_str(&format!("  the `{what}` press did not go out\n"));
                return false;
            }
            self.park();
            let deadline = Instant::now() + LEAVE_LANDS_WITHIN;
            while Instant::now() < deadline {
                std::thread::sleep(crate::timing::POLL_BRISK);
                // Pumped first, and every time round: the console is the thing being waited on, not
                // a courtesy read on the way out.
                self.pump();
                if let Some(line) = says {
                    if self.feed.seen_line_since(mark, line) {
                        self.log
                            .push_str(&format!("  `{what}` opened it — the game said `{line}`\n"));
                        return true;
                    }
                }
                let s = crate::act::identify(self.win);
                if landed.contains(&s) {
                    // Worth saying which instrument answered. If this branch is the one that keeps
                    // firing at a screen that *does* print a line, the line is wrong or the pump is
                    // behind — and that is a fault worth seeing rather than passing silently.
                    self.log.push_str(&format!(
                        "  `{what}` opened {s:?} — recognised from the screen{}\n",
                        match says {
                            Some(l) => format!(", with no `{l}` on the console"),
                            None => String::new(),
                        }
                    ));
                    return true;
                }
            }
            self.pump();
            self.log.push_str(&format!(
                "  `{what}` press {attempt} of {LEAVE_TRIES} left us on the map (observer says \
                 {:?})\n",
                crate::act::identify(self.win)
            ));
        }
        false
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
    ///
    /// ## Settled is not the same as current, and in a lost woods the difference ends runs
    ///
    /// This used to test `reason.contains("pan")` and nothing else, against whichever dump happened
    /// to be newest. "Settled" is a claim about the camera having stopped when the numbers were
    /// printed; it says nothing about **when** they were printed, and a dump from before a fight
    /// passes the test just as well as one from after it.
    ///
    /// That is fatal in a lost woods, because every fight re-orients the map. Ending one calls
    /// `overworld.returnFromDungeon` → `overworld:loadLight` (`overworld.lua:1035-1036`) →
    /// `overworldview.loadLight` (`overworldview.lua:1610-1620`):
    ///
    /// ```lua
    /// if worldParentLocation.typeData.generatorData.lostOrientation then
    ///     core.regenerateMap()
    ///     core.warpPlayerAvatar()
    ///     core.centreScreenOnPlayer(true)
    ///     core.generateClouds()
    /// end
    /// ```
    ///
    /// and `regenerateMap` re-runs the generator's `lostOrientation` block
    /// (`overworld/generators/forest.lua:483-498`), which multiplies every `posX`/`posY` by a fresh
    /// random sign and may transpose them. So a node's screen position before a fight predicts
    /// nothing about where it is afterwards — it is frequently the *mirror image*.
    ///
    /// The run of 2026-08-09 won five fights in the woods and died to this on the sixth step after
    /// one. It clicked toward `e1sub19` using the pan dump from before the fight, the map having
    /// been flipped underneath it, and moved the screen **0.032**. It looks exactly like a missed
    /// click, which is why it was first blamed on the blind-click problem — but the click landed
    /// precisely where it was aimed. The aim came from a stale map.
    ///
    /// It survived four earlier fights only by luck: a fresh dump usually arrives before the next
    /// step needs one, so this was a race that was mostly won. `positions_stale_at` makes the wait
    /// deliberate — a dump counted before the invalidation cannot satisfy this, whatever it says
    /// about panning.
    /// Pulls the map back a step, so a node no drag can fetch is simply drawn nearer the middle.
    ///
    /// Returns whether it did anything: **once per run, and once is all the game offers.** A step
    /// out halves `targetZoomMul`, clamped to `0.5` (`overworldview.lua:996`), so from the default
    /// `1` the first press is already at the floor.
    ///
    /// Every screen coordinate we hold is invalidated by this, which is the cost task #29 records:
    /// a dump prints `xoffset + posX*zoomMult` (`:1033`), so the same node prints differently either
    /// side of the change and comparing across it is meaningless. That is not a reason to avoid the
    /// zoom, it is a reason to say so — `positions_stale_at` is the existing machinery for exactly
    /// this, and it is what a finished fight already does to the same coordinates for the same
    /// reason. The camera also moves under `UpdateZoom` (`:1087-1097`), so the view is still
    /// settling when this returns; `needs_recentre` covers that as it does everywhere else.
    fn zoom_out(&mut self) -> bool {
        if self.zoomed_out {
            return false;
        }
        self.zoomed_out = true;
        self.keys.focus();
        std::thread::sleep(crate::timing::FOCUS_SETTLE);
        if self.keys.press_extended_key(VK_NEXT, SC_NEXT).is_err() {
            self.log.push_str("  could not press pagedown to zoom out\n");
            return false;
        }
        // **Before anything is pumped**, because from here until a dump measures the new scale the
        // map must place nothing: a node placed at the wrong scale stays wrong for the whole run and
        // goes into the cache. That is what ended the run of 2026-08-16 1752Z — see
        // [`crate::overworld::WorldMap::registration`].
        self.map.zoom_changed();
        // The zoom is lerped rather than snapped (`zoomMult` toward `targetZoomMul` at `dt*10`,
        // `:1091`), so this waits for the animation rather than the keystroke.
        std::thread::sleep(crate::timing::MAP_ZOOM_SETTLED);
        self.pump();
        self.positions_stale_at = self.dumps;
        self.needs_recentre = true;
        self.log.push_str(
            "  zoomed the map out a step — every held coordinate is stale, re-centring to get \
             fresh ones\n",
        );
        true
    }

    fn settled_dump(&mut self, within: Duration) -> Option<Adjacency> {
        let by = Instant::now() + within;
        let (cw, ch) = self.win.client_size().unwrap_or((1920, 1080));
        // Two ways to be unusable, and they need different words in the log. Tracked so the
        // explanation is written once rather than on every poll.
        let mut lost = false;
        loop {
            self.pump();
            let usable = dump_is_usable(self.dumps, self.positions_stale_at);
            let candidate =
                self.latest.as_ref().filter(|a| usable && a.reason.contains("pan")).cloned();
            if let Some(a) = candidate {
                // Settled, fresh, and still not to be believed — see `camera_is_lost`.
                if !camera_is_lost(&a.nodes, cw, ch) {
                    return Some(a);
                }
                if !lost {
                    lost = true;
                    self.log.push_str(
                        "  every neighbour is off-screen — the camera has not caught up with the \
                         map, so these coordinates cannot be used\n",
                    );
                    // **And ask for the cure, rather than noticing the symptom three times.**
                    //
                    // A locate-me centres on the player by construction, so it is exactly what a
                    // camera that has not caught up needs — the caller's own doc says as much, and
                    // then gates it behind `MAX_DUMP_MISSES`. Three looks, each preceded by a wait,
                    // before the one action that would fix it.
                    //
                    // Live on 2026-08-20 entering the lost woods: the event's `Continue` calls
                    // `enterSubworld` (`overworld/events/arrived/lost_woods.lua:22-30`), so the
                    // teleport lands us inside with the view somewhere else entirely. Every dump
                    // that followed was refused for the same reason and nothing reached for the
                    // remedy. The dev, watching it: *Diggle was able to recover but after over a
                    // minute of doing nothing.*
                    //
                    // Setting the flag rather than re-centring here: this is a *reader*, called
                    // from inside polling loops, and a reader that starts clicking would act in the
                    // middle of whatever is polling it. The flag is checked at the top of the next
                    // step, which is where a locate-me belongs.
                    self.needs_recentre = true;
                }
            }
            if Instant::now() >= by {
                let reason = self.latest.as_ref().map(|a| a.reason.clone()).unwrap_or_default();
                // Says which of the two it was. "Newest is `Screen pan finished`" while refusing it
                // reads like a bug unless the staleness is named.
                let why = match (dump_is_usable(self.dumps, self.positions_stale_at), lost) {
                    (_, true) => "the camera has not caught up with the map".to_string(),
                    (true, false) => format!("newest is `{reason}`"),
                    (false, false) => "nothing since the map was re-oriented".to_string(),
                };
                self.log.push_str(&format!("  no settled dump within {within:?}; {why}\n"));
                return None;
            }
            std::thread::sleep(crate::timing::POLL_SCREEN);
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
    pub fn clear_text_screen(&mut self) -> bool {
        let first = self.affirmative();
        // Logged even when there is nothing to do. A gate that has never been calibrated against a
        // live screen must show its score for the negative case too, or `Absent` is
        // indistinguishable from a threshold set too high.
        //
        // Which is why this collapses repeats rather than dropping the negative case: the *first*
        // reading still shows its score, and only the identical ones behind it are folded up. See
        // [`Run::note_affirmative`].
        self.note_affirmative(format!(
            "  affirmative slot: {:?} (score {:.2}, margin {:.2})\n",
            first.state, first.score, first.margin
        ));
        // **The console outranks the template here, and a run died proving it.**
        //
        // 2026-08-22 1855Z stopped at `l10` with the last console line reading
        // `Lore screen: As you're passing through Ulrome east guard post you notice the sole guard
        // pensively looking into the village.` — the game naming the exact screen that was up. The
        // affirmative template scored **0.05** on it and this returned `false`, so nothing cleared
        // it; the driver then spent sixteen blind clicks and three area presses on a lore screen.
        //
        // Why the template missed is in this function's own doc: the button's position moves with
        // whether it carries a label, and `combat-no-diff.png` from that stop shows the unlabelled
        // arrow plaque hard in the bottom-right corner, away from the slot the template reads.
        // Fixing the template is worth doing and is **not** what makes this safe — a fingerprint cut
        // from one variant can always miss the next one. The console cannot: `ui/lorescreen.lua`
        // prints one line per screen, and that line is a statement by the game that a lore screen
        // exists.
        //
        // So a console line we have not yet answered counts as "there is something to clear", and
        // the press below goes out whatever the slot says.
        let lore = announces_a_lore_screen(self.feed.since(self.lore_cleared_at));
        if lore {
            self.lore_cleared_at = self.feed.mark();
            self.log.push_str("  the console says a lore screen is up, whatever the slot reads\n");
        }
        if !first.state.is_ready() && !lore {
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
            std::thread::sleep(crate::timing::FOCUS_SETTLE);
            let _ = self.tap_key("clearing a text screen: space", VK_SPACE, SC_SPACE);
            std::thread::sleep(crate::timing::AFTER_SCREEN_PRESS);
            self.pump();
            // **A second lore line means a second screen, not a failed press.** The game prints one
            // per screen and they arrive in runs — entering Ulrome printed two at once — so the
            // console distinguishes "the press did nothing" from "there is another one behind it",
            // which the slot alone never could on a screen it cannot see.
            let more = announces_a_lore_screen(self.feed.since(self.lore_cleared_at));
            if more {
                self.lore_cleared_at = self.feed.mark();
                self.log.push_str("  another lore screen behind it — pressing again\n");
                continue;
            }
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

    /// Which of the three things a run can meet at the door is on screen.
    ///
    /// **Three templates, not the registry.** [`crate::act::identify`] scores every button we know
    /// and answers "which screen is this"; at startup the question is narrower — *what do I press* —
    /// and only three answers exist. Asking narrowly is cheaper (three crops against thirty-odd) and
    /// it cannot be surprised by a fingerprint from some screen that has no business being reachable
    /// here.
    ///
    /// **The text screen is asked first, and that ordering is the point.** A lore card covers the
    /// menu, so a run that checks `Continue` first is asking about a button behind a picture. The
    /// same rule already governs arrivals — *text gates options* — and this is the same rule at the
    /// front door.
    ///
    /// `Nothing` is a real and ordinary answer: the publisher splash takes several seconds and
    /// nothing is pressable during it. It means look again, not give up.
    pub fn doorway(&mut self) -> Doorway {
        // Each read is skipped once an earlier one has answered — `||` is short-circuiting — so the
        // ordinary case costs one crop, not three.
        Doorway::from_readings(
            self.affirmative().state.is_ready(),
            matches!(
                crate::act::score_exact(self.win, &crate::act::CONTINUE),
                Ok(q) if q >= crate::act::CONTINUE_PRESENT
            ),
            // Recognised on its own artwork rather than by position, because the same slot reads
            // `Restart` when a save exists and pressing that eulogises the run. See
            // [`crate::act::MENU_START`], which measures both sides.
            || {
                matches!(
                    crate::act::score_exact(self.win, &crate::act::MENU_START),
                    Ok(q) if q >= crate::act::MENU_START_PRESENT
                )
            },
        )
    }

    /// The generic observer, as a way out rather than as an epitaph.
    ///
    /// **The dev's standing rule, said more than once:** *whenever we're confused, fall back to the
    /// generic observer with all template matches so that we can try to recover — and log a warning
    /// when this happens.*
    ///
    /// [`Run::doorway`] is deliberately narrow — three templates, because at the door only three
    /// things are worth pressing — and narrowness is exactly what leaves it with nothing to say when
    /// something else turns up. That is the moment to stop being clever and ask the whole registry.
    /// It ran only at the abort before this, which is the observer as a *post mortem*: it explained
    /// the corpse rather than saving the run.
    ///
    /// The warning is the point of the log line. Reaching here means the narrow path did not cover
    /// the screen, which is a gap in the dispatch worth seeing even on the runs that recover from it.
    ///
    /// Returns whether anything was actually pressed, so the caller can tell a recovery from a
    /// diagnosis.
    pub fn recover_by_observing(&mut self, where_: &str) -> bool {
        self.log.push_str(&format!(
            "  WARNING: confused at {where_} — falling back to the full observer\n"
        ));
        let screen = crate::act::identify(self.win);
        self.log.push_str(&format!("  observer says: {screen:?}\n"));
        self.log_button_scores();
        match answer_for(screen) {
            // The escapes are the only answers that mean anything before a run has started: they
            // are screens with a back plaque and nothing behind them we want. Everything else
            // either belongs to a phase we are not in yet or is the map, and pressing at those from
            // here would be inventing a handler rather than reusing one.
            Answer::Escape => {
                let out = self.back_one_screen();
                self.log.push_str(&format!("  escaped it: {out}\n"));
                out
            }
            other => {
                self.log.push_str(&format!(
                    "  no recovery for {screen:?} here ({other:?}) — looking again\n"
                ));
                false
            }
        }
    }

    /// Clears whatever text screen is up, by the key first and the arrow second.
    ///
    /// Both are the same control — see [`Run::click_affirmative`] — so this is one attempt at one
    /// button, not two chances at two.
    pub fn clear_the_gate(&mut self) -> bool {
        self.clear_text_screen() || self.click_affirmative()
    }

    /// Clicks the lore screen's continue arrow, for when the key does not carry.
    ///
    /// **The arrow is the same control as the key**, not a second one:
    /// `ui/lorescreen.lua:46-50` declares one button, `type = 'right'` at `ss(1, 0.9)` with
    /// `xOffset = -0.75` — the bottom-right corner — carrying both `mousereleased = buttonFunction`
    /// and `userFunctionName = 'affirmative'`. [`Run::clear_text_screen`] presses the second;
    /// this presses the first, at the position [`affirm::LORE_AFFIRMATIVE`] already describes.
    ///
    /// Kept as a fallback rather than the first move, because a key needs no coordinate to be right
    /// and a click does. It is only reached once the reading says the arrow is there and six presses
    /// of `space` have failed to move it, which is the state where the coordinate is the *better*
    /// evidence — the fingerprint has just told us what is under it.
    ///
    /// Verified like everything else: the arrow going away is the proof, not the click returning
    /// `Ok`.
    pub fn click_affirmative(&mut self) -> bool {
        if !self.affirmative().state.is_ready() {
            return false;
        }
        let Ok((cx, cy)) = self.win.button_center(&affirm::LORE_AFFIRMATIVE) else { return false };
        let Ok((sx, sy)) = self.win.client_to_screen(cx, cy) else { return false };
        for attempt in 1..=3 {
            if !self.tap("locate-me: click empty map", sx, sy) {
                self.log.push_str("  could not click the continue arrow\n");
                return false;
            }
            self.park();
            std::thread::sleep(crate::timing::AFTER_SCREEN_PRESS);
            self.pump();
            let now = self.affirmative();
            if !now.state.is_ready() {
                self.log.push_str(&format!(
                    "  the continue arrow at ({cx},{cy}) cleared it after {attempt} click(s)\n"
                ));
                return true;
            }
            self.log.push_str(&format!(
                "  click {attempt} on the continue arrow did not take (still {:?}, {:.2})\n",
                now.state, now.score
            ));
        }
        false
    }

    /// Answers an arrival event, if an unanswered one has been announced. Returns its title.
    ///
    /// Scans the whole feed rather than a window. See [`Run::answered_event`] for why the window was
    /// wrong; the short version is that the event announces itself while we are busy dismissing the
    /// text screen in front of it, so any window opened afterwards is already too late.
    /// Writes down which forest just swallowed us, while the mist dialogue is still on screen.
    ///
    /// The dev, 2026-08-21: *Diggle does not know how to recognize that they've entered the lost
    /// woods.* Half of that was the camera, fixed in `c377a63`. This is the other half: the fact
    /// itself reached us only through `lost_woods_known_*` in a save that
    /// [`crate::game::save`] cannot read until the screen it belongs to has been left — so the run
    /// crossed the whole of `e3`, 69 steps, with the answer already written in the game's memory.
    ///
    /// ## Why `here` is the right key, from the source rather than from a guess
    ///
    /// `core.arriveAt` assigns `overworldData.playerLocation = locationName` **before** it raises
    /// the arrival event, and prints the dump **after** it, tagged `Arrived at location with event`
    /// (`overworldview.lua:1420-1444`). So the freshest dump at the moment this dialogue is up is
    /// the arrival that raised it, and its `here_key` is the node the event's `requireCheck` ran
    /// against.
    ///
    /// It is not `committed_to`, which was the obvious candidate and is wrong: a fast hop fires
    /// `arriveAt` at every node on the path (`:1210-1216`), so the woods can be somewhere we were
    /// only walking through.
    ///
    /// ## Three things are checked before anything permanent is written
    ///
    /// [`crate::overworld::Place::avoid`] is a routing wall that is never re-examined, so a wrong
    /// key here costs a road for the rest of the run. The dump must be an **arrival with an event**,
    /// it must be on the **surface** — `world_evil` and the mist event both require
    /// `not location.parentNode` — and the node must read as a **forest**, since `getTypeName`
    /// prints `forest` right up until the flag this event is about to set
    /// (`lost_woods.lua:29`). Failing any of them logs and writes nothing; [`WorldMap::fold`] still
    /// learns `in_lost_woods` from the container's heading one dump later, and the save still
    /// arrives eventually. This is an accelerator, and it declines rather than guesses.
    fn record_a_lost_woods(&mut self) {
        let Some(a) = self.latest.clone() else {
            self.log.push_str("  the mists, but no dump to say where — leaving it to the save\n");
            return;
        };
        let key = a.here_key.clone();
        let forest = self
            .map
            .get(&key)
            .map(|p| p.type_is("forest") || p.type_is("lost woods"))
            .unwrap_or(false);
        if !a.reason.contains("event") || a.subworld.is_some() || !forest {
            self.log.push_str(&format!(
                "  the mists at `{key}`, but the dump does not corroborate it (`{}`, {}, {}) — \
                 leaving it to the save\n",
                a.reason,
                match a.subworld.is_some() {
                    true => "inside a subworld",
                    false => "on the surface",
                },
                match forest {
                    true => "a forest",
                    false => "not a forest",
                },
            ));
            return;
        }
        self.map.mark_lost_woods(&key);
        self.log.push_str(&format!(
            "  **`{key}` is a lost woods** — recorded now rather than when the save catches up\n"
        ));
    }

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
        self.log.push_str(&format!(
            "  event **{}**: {:?}\n",
            ev.title,
            ev.choices.iter().map(|c| c.text.clone()).collect::<Vec<_>>()
        ));
        // **The one moment the console names a lost woods**, taken before the answer rather than
        // after. See [`Run::record_a_lost_woods`].
        if crate::subworld::is_the_mist_event(&ev.title) {
            self.record_a_lost_woods();
        }
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
        let pick = ev.continue_choice().or_else(|| ev.safe_choice_avoiding_combat(hurt)).cloned();
        if let Some(c) = &pick {
            if hurt && crate::observe::event::starts_combat(&c.text) {
                // The fallback fired: every option was a fight. Said out loud because it is a
                // decision, not a default — see `Event::safe_choice_avoiding_combat`.
                self.log.push_str(&format!(
                    "  **taking a fight at {} — every option was `[Combat]`**\n",
                    health
                        .map(|h| format!("{}/{}", h.current, h.max))
                        .unwrap_or("unknown health".into())
                ));
            } else if hurt {
                self.log.push_str(&format!(
                    "  hurt ({}), so avoiding any `[Combat]` option\n",
                    health
                        .map(|h| format!("{}/{}", h.current, h.max))
                        .unwrap_or("health unreadable".into())
                ));
            }
        }
        let Some(c) = pick else {
            self.log.push_str("  left alone: more than one real choice\n");
            return Some(ev.title);
        };
        // **#82: answering a `[Combat]` choice is the announcement that a fight is starting.**
        //
        // `combat_expected` existed for exactly this and was set in three places, every one of them
        // after pressing the Combat *area button* — so a fight that began from a dialogue armed
        // nothing. What that costs is the arrival wait in [`crate::navigate::drive`], whose only
        // success condition is standing on the node we pressed Travel for, which combat makes
        // unreachable. Live 2026-08-22 a highwayman was answered with `[Combat]` mid-travel and the
        // wait read an empty affirmative slot **162 times** — 300 ms apiece against a 60 s budget,
        // so very nearly the whole of it — before the outer loop's `identify` saw `CombatEntered`.
        //
        // **Armed before the click, not after it.** The fight starts on the game's side the moment
        // the press lands, and this loop's own verification can be inconclusive; a flag set only on
        // a verified answer would be missing in precisely the case it is needed. It costs nothing to
        // be early: the top of the driver's loop takes the flag
        // (`std::mem::take`) on every pass, so a fight that did not open clears it immediately.
        if crate::observe::event::starts_combat(&c.text) {
            self.combat_expected = true;
            self.log.push_str("  that choice starts a fight — not waiting for an arrival\n");
        }
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
        // **Do not answer an event into a screen that has already replaced it.**
        //
        // When the plaque cannot be scored there is nothing to confirm the answer against, so the
        // loop below falls back to a screen-moved proxy and presses up to four times. That is
        // acceptable while the event is still up and indefensible once it is not — and the observer
        // can tell the difference for the price of one capture.
        //
        // Live 2026-08-24: the `Woodsman`'s only choice is `[Shop]`, the shop opened before we
        // pressed, `identify` had *already* named it `Screen::Shop` that same iteration, and this
        // sent four blind clicks into the shelves anyway. The dev's rule, and not a new one:
        // *validate before input.*
        //
        // Only when unverifiable. With a plaque to score, that score is the better test and this
        // would refuse answers the run can confirm for itself.
        if !verifiable {
            if let Some(named) = Self::a_screen_we_know(self.win) {
                self.log.push_str(&format!(
                    "  not clicking the choice: the plaque cannot be scored and the observer \
                     calls this {named:?}, so the event is already behind us\n"
                ));
                // `answered_event` is already stamped above, so the caller will not come back to
                // this event; `None` says only that nothing was pressed this pass.
                return None;
            }
        }
        if let Ok((cx, cy)) = self.win.client_to_screen(click_x, click_y) {
            for attempt in 1..=4 {
                let before = crate::win::capture::capture_window(self.win).ok();
                let _ = self.tap("dismiss: centre of the screen", cx, cy);
                self.park();
                std::thread::sleep(crate::timing::AFTER_SELECT);
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
                        Ok(q) => self.log.push_str(&format!(
                            "  attempt {attempt}: still on the event ({q:.4})\n"
                        )),
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
                    let _ = self.tap("answering an event choice", sx, sy);
                    self.park();
                    std::thread::sleep(crate::timing::AFTER_SCREEN_PRESS);
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
                        self.log.push_str(&format!(
                            "  left the shop on attempt {attempt} ({moved:.3})\n"
                        ));
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

/// What a locate-me achieved. Three outcomes and only the first carries coordinates.
///
/// The arrow's press does three things at once (`overworldview.lua:488-494`) — refresh the area
/// buttons, centre the screen, and set `selectedLocation` to the player. A caller that only needs
/// the selection should not be made to wait for the pan that comes with it, and a caller that needs
/// coordinates must not mistake one for the other.
enum Located {
    /// The pan finished and announced itself; the dump is current.
    Panned(Adjacency),
    /// The press landed. The selection is ours; the camera may still be moving.
    Selected,
    /// The arrow was never pressed, or the pan never arrived.
    Failed,
}

#[cfg(test)]
mod tests {

    /// **The line that ended the run of 2026-08-22 1855Z**, verbatim from its console.
    ///
    /// It was the last thing the game said, and nothing read it. The affirmative template scored
    /// 0.05 on the screen it announced — the plaque is the unlabelled arrow variant, in the corner
    /// rather than the slot — so the template gate refused to press space and the driver went on to
    /// click sixteen times into a lore screen.
    ///
    /// The tab is real: the game separates the label from the text with one, which is why this
    /// matches a prefix and not a whole line.
    #[test]
    fn the_console_line_that_cost_a_run_is_recognised_as_a_lore_screen() {
        let real = "Lore screen:\tAs you're passing through Ulrome east guard post you notice the \
                    sole guard pensively looking into the village."
            .to_string();
        assert!(super::announces_a_lore_screen(&[real]));
        // A lore screen with no text is still a lore screen, and the feed may indent.
        assert!(super::announces_a_lore_screen(&["  Lore screen: ".to_string()]));
        // **Not** any old line that mentions one: an adjacency dump must never be read as a screen.
        assert!(!super::announces_a_lore_screen(&[
            "Local overworld data:\tArrived at location\tl10sub7\tUlrome east guard post".into(),
            "and the Lore screen: is not at the start of this one".into(),
        ]));
        assert!(!super::announces_a_lore_screen(&[]));
    }

    /// Every source file the driver is made of, listed because `include_str!` cannot glob.
    ///
    /// Read by [`every_fight_the_driver_starts_can_be_aborted`] and kept current by
    /// [`every_navigate_source_is_in_the_sweep`]. The paths resolve against *this* file, so they
    /// are bare names within `src/navigate/`.
    const DRIVER_SOURCES: &[(&str, &str)] = &[
        ("mod.rs", include_str!("mod.rs")),
        ("guard.rs", include_str!("guard.rs")),
        ("startup.rs", include_str!("startup.rs")),
        ("screens.rs", include_str!("screens.rs")),
        ("drive.rs", include_str!("drive.rs")),
    ];
    use super::*;

    /// **The three reasons the game prints, and which of them means "the buttons are already ours".**
    ///
    /// The dev, watching the 1828Z run: *the re-center just before we enter Ulrome Village seems
    /// unnecessary.* It was: `core.arriveAt` calls `refreshAreaButtons(location)` (`:1432`), which is
    /// `selectedLocation = location` plus that location's buttons, and only *then* prints this dump
    /// (`:1442`). So the dump is the receipt, and clicking an empty map point to raise the arrow and
    /// clicking the arrow to put the buttons back is asking for work already done.
    ///
    /// Reason strings verbatim from `spike-run-20260823-1828Z.log`, where a whole run produced no
    /// fourth kind.
    #[test]
    fn only_a_plain_arrival_at_the_node_underfoot_means_the_buttons_are_ours() {
        let dump = |reason: &str, key: &str| Adjacency {
            reason: reason.into(),
            here_key: key.into(),
            here_heading: "Ulrome village".into(),
            subworld: None,
            nodes: Vec::new(),
            hidden: 0,
            exits: Vec::new(),
            hidden_exits: 0,
        };

        assert!(
            arrival_selected_us(&dump("Arrived at location", "l10"), Some("l10")),
            "the case the dev pointed at"
        );

        // The camera settling says nothing about the selection, and is the *other* half of the
        // finding — see `Run::settled_dump_in_hand`, which is the one that wants this reason.
        assert!(!arrival_selected_us(&dump("Screen pan finished", "l10"), Some("l10")));
        // A load has selected nothing.
        assert!(!arrival_selected_us(&dump("World loaded", "l10"), Some("l10")));
        // An arrival that raised an event: the map is not what is on screen.
        assert!(!arrival_selected_us(&dump("Arrived at location with event", "l10"), Some("l10")));

        // Arriving *somewhere else* is the stale-dump case, and it is the one that would put a press
        // on another node's buttons — the failure the arrow press was kept for.
        assert!(!arrival_selected_us(&dump("Arrived at location", "l10_path_to_l1"), Some("l10")));
        assert!(!arrival_selected_us(&dump("Arrived at location", "l10"), None));
    }

    /// **The line that reported the wrong number against the wrong bar**, and what it says now.
    ///
    /// The dead press at `l38` logged `area slot: something else (Combat 0.7367, gate 0.95)` and
    /// went out anyway. Every word of it is true and it hid the fault, because the same three words
    /// covered a live `Travel` at 0.8566 — printed 244 times across the same five runs — and a
    /// greyed plank at 0.7367. One bar cannot distinguish those, so the line now carries both.
    ///
    /// The readings are measured, not invented: the corpus figures from
    /// `act::threshold_tests::a_greyed_plank_reads_lower_than_any_live_one`, and the live ones from
    /// the `area slot:` lines of `spike-run-20260821-*.md`.
    #[test]
    fn a_slot_reading_says_which_of_the_two_bars_it_cleared() {
        // A live `Combat`: over both bars, and the only reading that authorises a `Combat` press.
        let combat = slot_reading(Some(0.9812));
        assert!(combat.contains("`Combat`"), "{combat}");
        assert!(!combat.contains("nothing pressable"), "{combat}");

        // `Travel`, as measured 244 times. Under the naming bar and over the live bar — the reading
        // the old line called "something else" and this one has to call pressable.
        let explore = slot_reading(Some(0.8566));
        assert!(explore.contains("a live button, not `Combat`"), "{explore}");

        // And the worst live reading the runs produced, a village subnode at 0.8355. It is the one
        // that decides where the bar can go, so it is pinned here as well as in the calibration.
        let worst_live = slot_reading(Some(0.8355));
        assert!(worst_live.contains("a live button, not `Combat`"), "{worst_live}");

        // The greyed plank from the frame that ended the run. Under both.
        let greyed = slot_reading(Some(0.7367));
        assert!(greyed.contains("nothing pressable"), "{greyed}");

        // **The distinction the old line could not make**, stated as an inequality rather than as
        // two strings: `Explore` and greyed `Combat` used to produce the same verdict.
        assert_ne!(
            explore.split('(').next(),
            greyed.split('(').next(),
            "a live plank and a greyed one must not read alike — that is the whole fault"
        );

        // Both bars appear on every line whichever question was asked, so a log read months later
        // does not need the source to interpret the number.
        for line in [&combat, &explore, &greyed] {
            assert!(line.contains(&format!("{}", crate::act::AREA_BUTTON_SHOWING)), "{line}");
            assert!(line.contains(&format!("{}", crate::act::AREA_BUTTON_LIVE)), "{line}");
        }

        // A capture fault is not a low score. Saying "nothing pressable" for a slot nobody managed
        // to look at would send the recovery chasing a stale selection that was never observed.
        let unread = slot_reading(None);
        assert!(unread.contains("not read"), "{unread}");
        assert!(!unread.contains("nothing pressable"), "{unread}");
    }

    /// The stopping rule for fetching a node into reach, in the state that ended a run.
    ///
    /// `l2`'s road out sat at y = -130 on 2026-08-09 and exactly one drag was spent on it. Every
    /// clause here exists because one was not the right number: too few for a node that far out, and
    /// unlimited too many for a map that has stopped moving.
    #[test]
    fn a_node_out_of_reach_is_pulled_for_until_the_map_stops_giving() {
        let (w, h) = (1920, 1080);
        let far = (1301.0, -70.0); // the road out of `l2`, above the top of the window
        let here = (960.0, 540.0); // mid-screen, nothing to fetch

        assert!(pan_again(far, w, h, None, 0), "a first pull, which is all the run ever spent");
        assert!(!pan_again(here, w, h, None, 0), "already in reach");

        // The map's own bound, which announces itself only by not moving.
        assert!(pan_again(far, w, h, Some(pan::Shift { dx: 0.0, dy: 120.0 }), 1), "still giving");
        assert!(
            !pan_again(far, w, h, Some(pan::Shift { dx: 1.0, dy: -2.0 }), 1),
            "a pull that gained nothing is the edge answering; asking louder will not help"
        );

        // And a cap, so a view that creeps without ever arriving cannot run for ever.
        assert!(
            !pan_again(far, w, h, Some(pan::Shift { dx: 0.0, dy: 20.0 }), PAN_ATTEMPTS),
            "the budget is spent"
        );
    }

    /// A dump that was already in hand when the map turned over is not evidence about the new one.
    ///
    /// The run of 2026-08-09 clicked toward `e1sub19` on coordinates printed before a fight, in a
    /// lost woods, where ending a fight re-orients the map — so the node it aimed at was somewhere
    /// between mirrored and transposed away. The screen moved 0.032 and it read like a missed click.
    ///
    /// Four earlier fights survived it because a fresh dump usually lands before the next step needs
    /// one. A rule that is usually satisfied by timing is not a rule, which is what this pins.
    #[test]
    fn a_dump_from_before_the_map_turned_over_is_not_usable() {
        // The failing case: the fight raised the watermark to the dump we already had.
        assert!(!dump_is_usable(7, 7), "same dump, new map — this is the one that clicked wrong");
        assert!(!dump_is_usable(6, 7), "older still");
        assert!(dump_is_usable(8, 7), "counted after the fight, so it describes the map we have");
        // Nothing has invalidated anything yet: the ordinary case must stay free.
        assert!(dump_is_usable(1, 0), "a first dump with no fight behind it is fine");
    }

    /// Every neighbour off the same edge is the camera being wrong, not the map being big.
    ///
    /// The numbers are the ones the game printed at `e1sub35` after a fight in the lost woods, and
    /// the step before from next door, where the same nodes read positive. See [`camera_is_lost`].
    #[test]
    fn a_dump_with_every_neighbour_off_screen_is_the_camera_not_the_map() {
        let at = |x: f64, y: f64| crate::observe::adjacency::Node {
            key: format!("n{x}"),
            heading: "Howden Timberland forest".into(),
            x,
            y,
            connections: 2,
        };
        // The dump that ended the run.
        let flipped = [at(-1308.48, 718.33), at(-1418.89, 204.56), at(-909.94, 347.45)];
        assert!(camera_is_lost(&flipped, 1920, 1080));

        // The same three from the node next door, before the fight flipped them.
        let ordinary = [at(791.82, 703.56), at(1270.18, 730.56)];
        assert!(!camera_is_lost(&ordinary, 1920, 1080));

        // One node out of reach is the ordinary state of a big subworld, and is what panning is
        // for. Ulrome prints a road at x=2109 with the rest of the village on screen.
        assert!(!camera_is_lost(&[at(2109.0, 500.0), at(800.0, 500.0)], 1920, 1080));

        // A node dead-centre with nothing else named must not read as lost.
        assert!(!camera_is_lost(&[at(960.0, 540.0)], 1920, 1080));

        // No neighbours at all says nothing about the camera; only a dump that names some can.
        assert!(!camera_is_lost(&[], 1920, 1080));
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

    /// **Every fight the driver starts must be able to end in an abort**, and the arm that lets it
    /// is one line that a fourth call site would be written without.
    ///
    /// A source-reading test, like `act`'s search-box check, because the property is about the
    /// shape of the code rather than about a value: the three `match` blocks all end in a catch-all
    /// that turns any unhandled outcome into `Stop::Fought`, so a missing arm does not fail to
    /// compile — it reports the dev's own abort as a fight that went wrong. "A rule that is only
    /// checked on one of two paths is a rule with a hole in it" is this file's own phrasing.
    #[test]
    fn every_fight_the_driver_starts_can_be_aborted() {
        let (mut starts, mut arms) = (0, 0);
        for (_, src) in DRIVER_SOURCES {
            // Only the shipping half of each file. Counting the tests as well would count this
            // test's own string literals, which makes the two totals agree for a reason that has
            // nothing to do with the driver.
            let src = src
                .split_once(
                    "
#[cfg(test)]",
                )
                .map(|(before, _)| before)
                .unwrap_or(src);
            starts += src.matches("fight.run(").count();
            arms += src.matches("o.stop_requested() => return Stop::Requested").count();
        }
        assert!(starts >= 3, "expected the driver's fight call sites, found {starts}");
        assert_eq!(
            arms, starts,
            "{starts} fight call sites but {arms} abort arms — one of them reports a stop as a \
             fight that went wrong"
        );
    }

    /// The sweep above reads a list, and a list is only as good as whatever keeps it current.
    ///
    /// `include_str!` cannot glob, so [`DRIVER_SOURCES`] is written out by hand — and the whole
    /// point of the split (#76) is that the driver goes on gaining files. A `fight.run(` that moved
    /// into a file nobody added to the list would not fail to compile and would not fail the sweep;
    /// it would quietly stop being checked, which is the same hole the sweep exists to close. So
    /// compare the list against the directory rather than trusting it.
    #[test]
    fn every_navigate_source_is_in_the_sweep() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/navigate");
        let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
            .expect("src/navigate is where this module lives")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".rs"))
            .collect();
        on_disk.sort();
        let mut listed: Vec<String> =
            DRIVER_SOURCES.iter().map(|(n, _)| (*n).to_string()).collect();
        listed.sort();
        assert_eq!(
            on_disk, listed,
            "`DRIVER_SOURCES` and `src/navigate/` disagree — a driver source outside the list is a \
             source the abort sweep never reads"
        );
    }

    /// **A click outside the window does not miss the button; it misses the game.**
    ///
    /// The four coordinates are the ones the 2335Z run of 2026-08-23 was handed for `l32`'s
    /// neighbours by an arrival dump printed mid-glide, against the four the settled dump gave for
    /// the same four nodes twenty lines later. The run clicked the first set — `l45` at (-254, 63)
    /// went to whatever was on the desktop there, the strip did not move, and the run stopped
    /// saying the selection had not registered.
    #[test]
    fn a_mid_glide_coordinate_is_not_somewhere_we_may_click() {
        let (w, h) = (1920, 1080);
        // Printed by `Arrived at location l32`, before `centreScreenOnPlayer` had finished.
        for at in [(-254, 63), (-25, 211), (-3, 393), (-225, 442)] {
            assert!(!on_screen(at, w, h), "{at:?} is off the window and was clicked anyway");
        }
        // The same four from `Screen pan finished l32`, which is what should have been used.
        for at in [(709, 210), (938, 357), (960, 540), (738, 588)] {
            assert!(on_screen(at, w, h), "{at:?} is the settled position and must be clickable");
        }
        // The edges, because a node exactly on the boundary is the case a bounds check gets wrong.
        assert!(on_screen((0, 0), w, h), "the top-left pixel is inside the window");
        assert!(!on_screen((w, 0), w, h), "one past the right edge is not");
        assert!(!on_screen((0, h), w, h), "nor one past the bottom");
        assert!(on_screen((w - 1, h - 1), w, h), "but the last pixel is");
    }

    /// **A press whose verdict is thrown away should not have been measured**, and #113 is what that
    /// costs: [`Run::click_area_button`] spends up to [`crate::timing::AFTER_AREA_BUTTON`] and five
    /// full-window `PrintWindow` captures reaching an answer, and [`Run::left_the_overworld`]
    /// presses again until it can identify the screen. Both earn that from a caller who reads them.
    /// Discarding the result says the caller does not — and the subworld `Travel` press and the
    /// inn's `Enter` each carried one for months with a better test running beside it.
    ///
    /// A source-reading test in the manner of this file's abort sweep, because the property is about
    /// the shape of the code: discarding a `#[must_use]` compiles cleanly and is exactly what an
    /// editor writes to quieten a warning without pricing it. [`Run::press_area_button`] is the
    /// honest answer when there genuinely is nothing to read.
    ///
    /// The second assertion is the positive control the first one needs. A scan that finds nothing
    /// to complain about because the method was renamed underneath it is not a passing test, it is a
    /// blind one.
    #[test]
    fn no_press_is_left_without_a_test() {
        const WATCHED: [&str; 2] = ["click_area_button", "left_the_overworld"];
        let discard = ["let _ = ", "let _: "];

        let mut offenders = Vec::new();
        let mut mentions = 0;
        for (name, src) in DRIVER_SOURCES {
            // The shipping half only, for the abort sweep's reason: this test's own list of names
            // would otherwise satisfy the control it is trying to impose.
            let src = src.split_once("\n#[cfg(test)]").map(|(before, _)| before).unwrap_or(src);
            for (i, line) in src.lines().enumerate() {
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue;
                }
                mentions += WATCHED.iter().filter(|w| code.contains(**w)).count();
                if discard.iter().any(|d| code.contains(d))
                    && WATCHED.iter().any(|w| code.contains(w))
                {
                    offenders.push(format!("{name}:{}  {}", i + 1, code));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "a watched press has its verdict discarded — press without watching instead:\n{}",
            offenders.join("\n")
        );
        // Well under the 7 that are there, because the number to defend against is **zero** — a
        // rename takes every mention with it — and pinning the exact count would fail a run of
        // ordinary churn for a reason that has nothing to do with the rule.
        assert!(
            mentions >= 4,
            "the sweep found only {mentions} watched presses to police, which means it is reading \
             for a name that has moved rather than for the thing it cares about"
        );
    }
}
