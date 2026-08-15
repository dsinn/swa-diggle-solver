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
use crate::fight::Fight;
use crate::observe::adjacency::{self, Adjacency};
use crate::observe::affirm;
use crate::observe::event;
use crate::observe::feed::Feed;
use crate::observe::pan;
use crate::overworld::{Goal, WorldMap};
use crate::win::input::{
    click_at_in, warp_cursor, Input, PostMessageInput, SC_NEXT, SC_RETURN, SC_SPACE, VK_NEXT,
    VK_RETURN, VK_SPACE,
};
use crate::win::window::GameWindow;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const FRAMES: &str = "spike-frames-live";
/// Where the learned map is kept between runs. See [`Run::save_map_cache`].
pub const MAP_CACHE: &str = "map-cache";
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
const EMPTY_MAP_CANDIDATES: [(i32, i32); 4] =
    [EMPTY_MAP, (1750, 800), (170, 240), (960, 170)];
/// Where the cursor is parked so it cannot hover something we are about to fingerprint.
///
/// Deliberately clear of the view's hotspot rectangle, `{0, 0, 300, height*0.8}`
/// (`overworldview.lua:1146`). The old value was (300, 300) — sitting exactly on that rectangle's
/// right edge, next door to a function named `backOutOfHotspotMapPan`. Parking on the boundary of a
/// region whose handler pans the map is not somewhere to leave a cursor for seconds at a time.
const NEUTRAL: (i32, i32) = (760, 240);
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
    // **Not an exit — the one entry here that presses on rather than backs out.**
    //
    // A fight at a corrupted shrine ends leaving the shrine screen up with `Consecrate` lit, and
    // live 2026-08-12 the run pressed back instead: the classifier had no check for the button, so
    // the back plaque answered first and the offer went unseen. `Consecrate` is only ever drawn when
    // it will do something -- `majorShrine and hell ~= 0` (`shrine.lua:92-95`) -- so finding it lit
    // on a screen we are already standing on means the trip and any fight are paid for and the only
    // thing left is to take it.
    //
    // The mechanism is the same as every other escape: press the button, look again. What differs is
    // the intent, and the reason it can share the mechanism is that pressing `Consecrate` also
    // leaves — `shrine.lua:288` ends in `setActiveMode(overworld)`.
    // **The `Consecrate` entry was removed on 2026-08-15, and the premise above is why.**
    //
    // "`Consecrate` is only ever drawn when it will do something" is not what the source says. The
    // gate is `showConsecrateButton` = `ShowAGoodButton() and majorShrine` (`shrine.lua:92-95`), and
    // `ShowAGoodButton()` is `hasWon() and not heretic` (`:36-40`) — so the button is drawn **greyed
    // until the shrine's word is solved**, and being on screen says nothing about being pressable.
    //
    // Live 2026-08-15 at `shrine1`: this entry fired, logged `left the shrine screen, by
    // consecrating it`, and one step later the shrine driver found the slot at **0.8564** — which
    // `SHRINE_CONSECRATE_PRESENT`'s own table names as the greyed state — and reported
    // `shrine: left unconsecrated`. The run then believed it had failed, and because `worth_a_trip`
    // is `!p.used` and consecrating never sets `used`, it walked four hops to `shrine2` and on
    // toward `shrine6`, out of the corruption it was supposed to be closing.
    //
    // Consecration belongs to the shrine driver, which solves the word first and therefore knows the
    // button is earned before it presses. One actor per action: an escape route exists to get *off*
    // a screen, and the moment it also performs the objective, two things race for the same button
    // and the loser reports a failure that did not happen.
    // Normally the moment after `Pray`, where the slot now holds a greyed `Consecrate` and the only
    // thing left to do is leave. `shrineplay::play` deliberately stops at the Pray press and hands
    // the aftermath back here, so this is the ordinary exit rather than an error path.
    Escape {
        screen: Screen::Shrine,
        button: &crate::act::SHRINE_GOBACK,
        threshold: crate::act::SHRINE_GOBACK_PRESENT,
        what: "the shrine screen",
    },
    // **The same exit, for the screen that names the button.**
    //
    // Removing the old `Consecrate` entry was right and left a hole: `identify` reports
    // `ShrineConsecrate` in preference to `Shrine` whenever the slot is occupied, so the more
    // specific variant shadowed the escape above and the run had no answer for a screen it was
    // standing on. `answer_for` said `Elsewhere("the shrine driver")`, but the shrine driver enters
    // from the *map* by pressing `Visit` — it has no way to take over a screen we are already on.
    //
    // Live 2026-08-15, restored at `shrine1` mid-fight: the fight finished, the game left the shrine
    // screen up, and the run spent every step alternating `ShrineConsecrate` -> locate-me clicks
    // that landed on the shrine's own chrome -> `StatsHistory` -> back -> `ShrineConsecrate`, until
    // it stopped with `no pan dump after locate-me, 4 times over`. Seven steps, no progress, and a
    // stop message about panning.
    //
    // **This presses `Go back`, not `Consecrate`,** which is the whole difference from the entry
    // that was removed. Leaving is always safe and always available; taking the consecration
    // requires knowing the word is solved, which only the shrine driver knows. The invariant holds —
    // nothing outside that driver presses `Consecrate` — and the screen stops being a trap.
    Escape {
        screen: Screen::ShrineConsecrate,
        button: &crate::act::SHRINE_GOBACK,
        threshold: crate::act::SHRINE_GOBACK_PRESENT,
        what: "the shrine screen, leaving the slot alone",
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
        // `Shrine` is the escape **fallback**, and still has to be: the check behind it is "there is
        // a back plaque here", which has fired on an inn. But `drive` looks first, and if the *save*
        // says a shrine underfoot is worth consecrating it plays it where it stands rather than
        // pressing back — see the branch above `ESCAPES`. The escape is what happens when the save
        // says there is nothing here, which is the only case it was ever right for.
        Screen::Character | Screen::StatsHistory | Screen::Shrine => Answer::Escape,
        // Not escaped any more: pressing `Consecrate` is the shrine driver's job, because only it
        // has solved the word that makes the button pressable. See the note where its `Escape` used
        // to be.
        Screen::ShrineConsecrate => Answer::Escape,
        // `drive` buys at it and leaves; see the store arrival branch. Not an escape, because
        // leaving before buying is the one outcome the whole trip was against.
        Screen::Shop => Answer::Bespoke,
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
    /// `refreshAreaButtons` and `centreScreenOnPlayer` together (`overworldview.lua:485-494`) and
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
            std::thread::sleep(Duration::from_millis(150));
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
        std::thread::sleep(Duration::from_millis(300));

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

    fn map_cache_path(&self) -> Option<PathBuf> {
        let save = crate::game::save::load(&self.save_dir.join("mainSaveData")).ok()?;
        let seed = save.int_at("overworld.seed")?;
        Some(PathBuf::from(MAP_CACHE).join(format!("world-{seed}.txt")))
    }

    /// Folds in what earlier runs learned about this world. Returns the edge count and the file.
    pub fn load_map_cache(&mut self) -> Option<(usize, String)> {
        let path = self.map_cache_path()?;
        let text = std::fs::read_to_string(&path).ok()?;
        let edges = self.map.absorb_cache(&text);
        Some((edges, path.display().to_string()))
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
            Ok(()) => self
                .log
                .push_str(&format!("remembered {} places in `{}`\n", self.map.len(), path.display())),
            Err(e) => self.log.push_str(&format!("could not write {}: {e}\n", path.display())),
        }
    }

    pub fn apply_save(&mut self) -> Option<crate::rest::Health> {
        let save = crate::game::save::load(&self.save_dir.join("mainSaveData")).ok()?;
        self.map.apply_save(&save);
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
            std::thread::sleep(Duration::from_millis(400));
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
    fn snap_screen(&mut self, tag: &str) {
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
        let top: Vec<String> =
            scored.iter().take(4).map(|(n, q)| format!("{n} {q:.4}")).collect();
        match top.is_empty() {
            true => self.log.push_str("  nothing could be scored — the capture itself failed\n"),
            false => self.log.push_str(&format!("  loudest fingerprints: {}\n", top.join(", "))),
        }
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
    fn rest_at_inn(&mut self) -> Rested {
        use crate::innplay;

        // 1. `Enter`. It is the ordinary area button, in the slot everything else uses.
        let mark = self.feed.mark();
        let _ = self.click_area_button("Enter");
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
                match crate::act::click_when_ready(self.win, &crate::act::INN_REST, innplay::REST_WAIT)
                {
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
                std::thread::sleep(Duration::from_millis(600));
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
        // **What the game itself thinks is selected, before we click at arithmetic.** Task #19, in
        // its read-only first form: `AREA_BUTTON` is a coordinate transcribed by hand from `ss`/`os`
        // multipliers in the Lua, and a transcription that is wrong looks exactly like one that is
        // right until a run dies on it. When hotspot navigation is live the game has parked the real
        // pointer on the centre of a control it computed itself, so the two can simply be compared.
        //
        // Reported, never acted on. `None` is the ordinary case — any real mouse movement clears the
        // highlight (`main.lua:420`) — and a disagreement might equally mean the game has a
        // *different* control selected, which is information rather than an error. See
        // [`crate::win::cursor`] for why nothing here presses a direction key to find out.
        if let Some(miss) = crate::win::cursor::miss_by(self.win, AREA_BUTTON) {
            self.log.push_str(&format!(
                "  the game has a control selected {miss:.0} px from where `{what}` is aimed
"
            ));
        }
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
        std::thread::sleep(Duration::from_millis(120));
        if self.keys.press_extended_key(VK_NEXT, SC_NEXT).is_err() {
            self.log.push_str("  could not press pagedown to zoom out\n");
            return false;
        }
        // The zoom is lerped rather than snapped (`zoomMult` toward `targetZoomMul` at `dt*10`,
        // `:1091`), so this waits for the animation rather than the keystroke.
        std::thread::sleep(Duration::from_millis(900));
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
        // **Sit still for eight seconds before touching anything.**
        //
        // Resuming drops us into a fight the game is still opening. On 2026-08-12 that meant a boss
        // introduction — the combat HUD up, the enemy named across the screen, and no board drawn
        // yet — and the first clicks went into it and were discarded. Nothing downstream could tell:
        // the readiness check samples tile centres for brightness, and on that card they sample sky
        // and sea, which are brighter than any tile. Sixteen of sixteen slots read as occupied on a
        // screen with no board on it, so the wait it was supposed to perform never happened.
        //
        // A fixed delay, not a cleverer measurement. Measuring this properly means telling a tile
        // from scenery, and two attempts at that on 2026-08-12 both shipped regressions that ended
        // runs at the *start* of ordinary fights — the dev's call was to stop patching and take the
        // blunt instrument. Eight seconds covers the introduction, costs eight seconds once per
        // resumed fight, and cannot be fooled by what is on screen because it does not look.
        //
        // Scoped to the resume path deliberately. A fight entered by walking into it is announced on
        // the console and already handled; this is the one entry the run does not watch happen.
        std::thread::sleep(RESUME_SETTLE);
        r.log.push_str(&format!("  waited {RESUME_SETTLE:?} for the fight to finish opening\n"));
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

    for step in 1.. {
        if Instant::now() >= deadline {
            return Stop::Exhausted;
        }
        // Checked here, at the top, and not partway down where it used to sit: with no step cap
        // above it, this and the deadline are the only two ways a run ends that are not the run's
        // own decision, and a `continue` from a screen handler must not be able to skip either.
        if Path::new(STOP_FILE).exists() {
            let _ = std::fs::remove_file(STOP_FILE);
            r.log.push_str(&format!("{step}. stop requested — ending cleanly\n"));
            return Stop::Requested;
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
        let mut screen = crate::act::identify(r.win);
        // **One look is not enough right after we asked for a fight.**
        //
        // `identify` is a set of `score_exact` comparisons, and mid-transition every template scores
        // low — the same "a number below the threshold is not the same as the screen having moved
        // on" that [`crate::act::score_exact`] warns about and that [`crate::act::wait_for`] exists
        // for. Asking once and calling it Unknown commits this iteration to the map path.
        //
        // Live 2026-08-14 at `l16sub14`: the press landed (the screen moved 0.975), the single look
        // came back Unknown, the map path re-derived the same step and pressed the same coordinate
        // into the pregame, where it means nothing — 0.011 of movement — and the run stopped with
        // `Combat did not open`. `spike-frames-live/gave-up.png` is that pregame, and its Start
        // button scores **1.0000** against a 0.90 bar, on both the exact and searched paths. The
        // fingerprint was never the problem; nothing asked it a second time.
        //
        // Scoped to `combat_expected`, which until now was carried only so the log could say whether
        // a fight was walked into or asked for. Everywhere else Unknown is the ordinary answer —
        // the map is Unknown — so polling for a name would cost every iteration the full timeout.
        // Here we have just pressed a button whose whole purpose is to leave the map, so Unknown is
        // a transition rather than a verdict, and it is worth waiting out.
        if screen == crate::act::Screen::Unknown && r.combat_expected {
            let by = Instant::now() + COMBAT_OPENS_BY;
            while Instant::now() < by {
                // **Take the cursor back before looking.** Opening a screen moves the real mouse:
                // `input.setHotspotHighlight` calls `love.mouse.setPosition(unpack(hotspot))` and
                // hides the pointer (`utils/input.lua:94-96`), so the game parks it on that screen's
                // hotspot — which on the pregame is `Start`. The button then draws in its hover art,
                // and every template in the registry is cropped from the resting art.
                //
                // That is what four seconds of `Unknown` was, live 2026-08-14 at `l16sub5`: the
                // screen was up and unmistakable to a human, and unreadable to us because we were
                // holding a picture of it in a state the game had moved it out of. The frame that
                // seemed to disprove this scored 1.0000 only because the *next* press parked the
                // cursor before the capture — a photograph taken after the evidence had gone.
                //
                // Parking is already how this run keeps a hover out of a reading — see [`NEUTRAL`],
                // chosen to sit clear of the view's own hotspot rectangle. Doing it inside the loop
                // rather than once, because the warp follows the screen: whatever arrives during
                // these four seconds may grab the pointer again on the way in.
                r.park();
                std::thread::sleep(Duration::from_millis(150));
                r.pump();
                screen = crate::act::identify(r.win);
                if screen != crate::act::Screen::Unknown {
                    break;
                }
            }
            // Cleared either way: a press that opened nothing within the window is a press that
            // failed, and leaving the flag up would make the *next* iteration wait all over again.
            if screen == crate::act::Screen::Unknown {
                r.combat_expected = false;
                r.log.push_str(&format!(
                    "  pressed Combat and no screen arrived within {COMBAT_OPENS_BY:?} — treating \
                     it as still the map\n"
                ));
                // **Photograph and score before moving on**, because this branch has already been
                // wrong once about what it was looking at.
                //
                // Live 2026-08-14 at `l16sub5`: the console printed `Pregame screen:` — its last
                // line — so the screen was constructed, this poll ran its full four seconds naming
                // nothing, and `gave-up.png` taken moments later scores **1.0000** for
                // `PREGAME_START`. Three facts that cannot all be about the same instant, and no
                // capture existed from inside the window to say which one moved.
                //
                // A number here settles it next time: a Start at 0.89 is a threshold or a hover
                // state, a Start at 0.02 is a screen that was not drawn yet, and those want
                // opposite fixes.
                //
                // Kept now that the cursor is taken back above, because the park is a counter to one
                // mechanism rather than a proof there is only one. If this line ever prints a Start
                // in the eighties again, the answer is the game's own hover art — `ButtonArt` in
                // [`crate::observe::affirm`] already loads all five states from the game's files,
                // and no hand-cropped template would be needed.
                r.snap_screen("combat-no-screen");
                r.log_button_scores();
            }
        }
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
        //
        // ## Now the only way into a fight, planned or not
        //
        // The planned path used to play its own fight out immediately after pressing the area
        // button, on the strength of having seen `Pregame screen:`. That made two fight handlers
        // whose only difference was how they had found out, and the one with the announcement could
        // not recognise a door that does not make one. Pressing the button now just `continue`s, so
        // both arrive here — and `combat_expected` is kept only so the log can still say which.
        if screen == crate::act::Screen::CombatEntered {
            r.log.push_str(match std::mem::take(&mut r.combat_expected) {
                true => "  the fight is up — playing it out\n",
                false => "  in combat and had not noticed — playing it out\n",
            });
            let mut fl = String::new();
            let outcome = fight.run(
                &mut r.feed,
                &r.keys,
                &mut fl,
                deadline.min(Instant::now() + Duration::from_secs(400)),
            );
            r.log.push_str(&fl.lines().map(|l| format!("    {l}\n")).collect::<String>());
            match outcome {
                Ok(o) if o.cleared() => {
                    r.log.push_str(&format!("  fight finished: {o:?}\n"));
                    // Every screen position we hold predates the map the fight just handed back.
                    // In a lost woods that is literal: `loadLight` re-orients it. See
                    // [`Run::settled_dump`].
                    r.positions_stale_at = r.dumps;
                    // And the camera is still returning to the player, so even a dump printed after
                    // this one describes a view in motion. See [`Run::needs_recentre`].
                    r.needs_recentre = true;
                    // Bookkeeping every fight needs. Skipping it is how a run walks out of a fight at
                    // 1/20 and never considers resting, because nothing recorded the loss.
                    let now = r.apply_save();
                    if let (Some(b), Some(a)) = (health.clone(), now) {
                        r.map.note_health(b, a);
                        r.map.rested(a);
                    }
                    *health = now;
                    continue;
                }
                // Reported as a fight that went wrong, not as a map failure. The whole point of this
                // branch is that "no pan dump after locate-me" was never the truth about this state.
                Ok(o) if o.fatal() => return Stop::Died(format!("{o:?}")),
                Ok(other) => return Stop::Fought(format!("{other:?}")),
                Err(e) => return Stop::Failed(format!("could not play the fight out: {e}")),
            }
        }
        // **A shrine screen after a shrine fight is not a dead end, it is the handoff.**
        //
        // The dev, 2026-08-15: "Upon completing the combat at shrine1, we should immediately
        // Consecrate instead of having to rely on the fallback."
        //
        // Right, and the game agrees — `overworld.lua:1070-1079` wires the postgame screen's way
        // back to a freshly loaded shrine when the scenario was a shrine, so dismissing the rewards
        // lands us *on* the shrine with `directFromCombat = true`. Escaping that and walking back in
        // through `Visit` is refusing a door the game opened.
        //
        // The save decides, not the screen. [`crate::act::Screen::Shrine`] means no more than
        // "there is a way back from here" — it has fired on an inn — so what makes this safe is
        // `worth_consecrating_here`, which reads `_consecrated` and the corruption flags and is
        // documented as the only thing allowed to answer "is a shrine underfoot".
        //
        // Re-read first: `completed` reaches the save when the *screen* is exited, so the flag from
        // the fight we have just won can be one read behind. Same reason, same fix, as the arrival
        // branch's own re-read.
        if screen == crate::act::Screen::Shrine {
            r.apply_save();
            let here = r.map.here().map(str::to_string);
            if let Some(key) = here.filter(|k| {
                r.map.worth_consecrating_here(k) && !r.shrines_tried.contains(k)
            }) {
                r.log.push_str(&format!(
                    "  the fight left us on `{key}`'s shrine screen — consecrating here rather \
                     than walking back in\n"
                ));
                r.shrines_tried.insert(key.clone());
                r.map.abandon(&key);
                let anomaly_open = r.map.anomaly_is_open().unwrap_or(false);
                match crate::shrineplay::play(r.win, &r.keys, anomaly_open, true) {
                    Ok(played) => {
                        r.log.push_str(&played.log);
                        if played.consecrated && !r.confirm_consecrated(&key, CONSECRATE_CONFIRM) {
                            r.log.push_str(
                                "  shrine: the screen closed but `_consecrated` never landed\n",
                            );
                        }
                    }
                    Err(e) => r.log.push_str(&format!("  shrine failed: {e}\n")),
                }
                r.apply_save();
                continue;
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
            // A fight just ended: settle the camera deliberately rather than reading around it.
            // Nothing here is a fallback — if locate-me cannot be made to work we would rather take
            // the miss and retry than aim at a moving view. See [`Run::needs_recentre`].
            if r.needs_recentre {
                let (cw, ch) = r.win.client_size().unwrap_or((1920, 1080));
                // `recentre` leaves its answer in `latest`, which is where `settled_dump` reads
                // from, so there is nothing to carry across by hand. The flag clears only on
                // success: a locate-me that did not take has settled nothing.
                if r.recentre().is_some_and(|a| !camera_is_lost(&a.nodes, cw, ch)) {
                    r.needs_recentre = false;
                    r.dump_misses = 0;
                }
            }
            let polled = r.settled_dump(Duration::ZERO).or_else(|| {
                // `recentre` clicks the map to force a pan dump, so it is worth one try rather than
                // three: a run that is not on the map at all should not be clicking at it repeatedly.
                //
                // It is also the cure for a camera that has not caught up, because locate-me centres
                // on the player by construction. Its answer is held to the same test anyway — an
                // invariant that is only checked on one of two paths into the same variable is a
                // rule with a hole in it.
                let (cw, ch) = r.win.client_size().unwrap_or((1920, 1080));
                (r.dump_misses + 1 >= MAX_DUMP_MISSES)
                    .then(|| r.recentre())
                    .flatten()
                    .filter(|a| !camera_is_lost(&a.nodes, cw, ch))
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
            // The overworld gets the settling for free — it re-centres every step, and `recentre`
            // already refuses a dump older than its own click. What it did not get is the sanity
            // check on the answer, and there is no reason the overworld camera is more trustworthy
            // than a subworld's; a locate-me caught mid-glide reads the same either side.
            //
            // Treated as a miss rather than a stop, which is what this branch already does with a
            // locate-me that did not answer: go round, re-identify, try again.
            let (cw, ch) = r.win.client_size().unwrap_or((1920, 1080));
            match r.recentre().filter(|a| !camera_is_lost(&a.nodes, cw, ch)) {
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
            if p.is_shrine() {
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
        // **One shrine branch, and it always brings the typist.**
        //
        // The dev's rule: solve the word as soon as we enter an unsolved shrine. There used to be a
        // second branch below for a shrine that was `used` but unconsecrated, and it called
        // `shrineplay::consecrate` — which opens the screen, looks for a live `Consecrate` and has
        // no typist at all. Whenever the word was not already won that branch could only fail, and
        // on 2026-08-15 it did: two subworlds and three fights to reach `shrine5`, greyed button,
        // `left unconsecrated`, nothing typed.
        //
        // `play` handles every state the slot can be in — active `Consecrate` spends the solve,
        // `Pray` claims the blessing, an empty slot means the word and so the solver runs — so there
        // is nothing for a second entry point to add. See its doc for the four-state table.
        //
        // The condition is the union of what the two branches covered: an unused shrine is worth
        // entering for its blessing, and a used one is worth entering while the anomaly is open and
        // it is still unconsecrated.
        // **A shrine's `completed` is one save read behind us when we arrive.**
        //
        // The guard below wants `completed` because the area slot holds `Visit` only when the area
        // is complete — press it early and the click starts a fight instead of opening the word. For
        // a shrine with no fight on it that flag lands on *arrival*, so the map we planned the hop
        // with predates it and the branch silently declines.
        //
        // Live 2026-08-15, twice, and it read as two different bugs. At `shrine5` on the first run
        // the play branch declined and the consecrate branch took it instead — with no typist, so
        // nothing was solved. On the second run the branches had been merged, so declining meant
        // doing *nothing at all*: the run crossed three fights to reach `shrine5`, arrived, and left
        // for `shrine7` in the same step. The dev watched both.
        //
        // One extra save read, only when standing on a shrine we think is unfinished. Everything
        // else here already re-reads after acting; this is the one place that had to read *before*.
        let place = match place.as_ref().filter(|p| p.is_shrine() && !p.completed) {
            Some(_) => {
                r.apply_save();
                r.map.get(&here).cloned()
            }
            None => place,
        };
        if let Some(p) = place
            .as_ref()
            .filter(|p| p.is_shrine() && p.completed)
            .filter(|p| !p.used || r.map.worth_consecrating_here(&p.key))
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
            // The portal decides which button a solve produces, so the answer travels into `play`
            // rather than being discovered by a failed match afterwards. Unknown reads as closed —
            // the conservative direction, since it only costs the older `Pray` attempt.
            let anomaly_open = r.map.anomaly_is_open().unwrap_or(false);
            // Reached from the map, so the shrine still has to be opened with `Visit`.
            match crate::shrineplay::play(r.win, &r.keys, anomaly_open, false) {
                Ok(played) => {
                    let log = played.log.clone();
                    r.log.push_str(&log);
                    if played.consecrated && !r.confirm_consecrated(&key, CONSECRATE_CONFIRM) {
                        r.log.push_str(
                            "  shrine: **the screen closed but `_consecrated` never landed** —                              not consecrated after all
",
                        );
                    }
                    if !played.prayed && !played.consecrated {
                        // Not fatal, and deliberately not a stop: the blessing is a bonus, and a run
                        // that cannot claim it should still get on with the anomaly. It is logged
                        // loudly because a shrine we walked to and failed to use is a wasted trip.
                        r.log.push_str(&format!(
                            "  shrine: left un{}\n",
                            if anomaly_open { "consecrated" } else { "prayed" }
                        ));
                    }
                }
                Err(e) => r.log.push_str(&format!("  shrine failed: {e}\n")),
            }
            // Re-read before anything plans on it. `_used` reaches the save when the shrine screen
            // is *exited*, which the driver has just done, so this is the first moment the flag is
            // readable — see the standing note that a stale read here is timing, not failure.
            r.apply_save();
            // **Was a blessing actually left behind?** Only the save can answer, and the answer is
            // not "did we press Pray this visit". A shrine solved before the anomaly opened was
            // prayed at then — with the portal shut that is the only reward a shrine offers — so it
            // owes the consecration alone and no `Pray` appears after the beam. Judging that by the
            // press would report a correct, complete visit as a failure.
            //
            // Worth a loud line when it is real, because `_used` is what `worth_a_trip` reads: a
            // shrine that stays unused stays a destination, and the run will walk back to it instead
            // of going to the anomaly.
            if let Some(p) = r.map.get(&key).filter(|p| !p.used) {
                let _ = p;
                r.log.push_str(
                    "  shrine: still **unused** after the visit — a blessing is owed here, and \
                     the planner will keep choosing this shrine until it is claimed\n",
                );
            }
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
        // **Standing on a shrine and doing nothing is the end of it as a destination.**
        //
        // The structural backstop, and the more important half of the fix above. `abandon` existed
        // already but was only ever called from inside the branches that *act*, so any shrine the
        // driver declined stayed a perfectly good target forever and the planner kept re-choosing
        // it. That is a loop whatever the reason for declining, so the guard belongs here — where
        // "we arrived and nothing happened" is known — rather than being added case by case as each
        // new reason to decline appears.
        if place.as_ref().map(|p| p.is_shrine()).unwrap_or(false) {
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
                // **Which errand are we standing on?** The two share this branch because reaching
                // them is identical; what happens next is not.
                if r.map.get(at).map(|p| p.is_general_store()).unwrap_or(false) {
                    r.log.push_str(&format!("{step}. at **{at}** in `{container}` - the general store
"));
                    // The same press as every other area button, confirmed the same way. `Shop` sits
                    // in the slot `Visit` and `Combat` share (`village.lua:312-320`), so this is
                    // machinery that already works.
                    if !matches!(r.click_area_button("Shop"), Ok(true)) {
                        r.map.abandon(at);
                        r.log.push_str("  the shop did not open - writing the store off
");
                        continue;
                    }
                    // The console is the instrument here, not the screen: the pump is what carries
                    // `Opened shop UI` and the inventory behind it.
                    std::thread::sleep(Duration::from_millis(1200));
                    r.pump();

                    // **The game told us the stock; the layout tells us where it is drawn.**
                    // Neither is a guess and neither is a template — see `crate::shopplay`.
                    let inv = crate::shopplay::inventory(r.feed.lines());
                    r.log.push_str(&format!("  the store lists {} items
", inv.len()));
                    let bought = match crate::shopplay::index_of(&inv, crate::shopplay::HEART) {
                        None => {
                            r.log.push_str("  no `healthBuff` in stock — nothing to buy here
");
                            false
                        }
                        Some(i) => match crate::shopplay::slot_at(r.win, i, 0) {
                            // Paging is not written: `relativeIndex` steps by eight
                            // (`shop.lua:205-232`) and a heart has never yet been anywhere but the
                            // first page. Refusing to click is the safe half of that — there is no
                            // confirmation dialogue on this screen, so a slot we cannot place is a
                            // purchase of whatever else is sitting in it.
                            None => {
                                r.log.push_str(&format!(
                                    "  the heart is item {i}, which is off the first page — paging                                      is not implemented, so nothing is pressed
"
                                ));
                                false
                            }
                            Some((x, y)) => {
                                // **Empty the shelf, do not take one off it.** The dev's
                                // correction, 2026-08-15: settlements stock more than one heart and
                                // the single purchase was leaving them behind.
                                //
                                // Bounded by two numbers and the smaller wins. The shop printed the
                                // stock when it opened, and `hearts_affordable` is the purse with
                                // `INN_COST` held back — a reserve over the whole visit, not just
                                // the first item, so a shelf of four cannot spend us out of a bed.
                                //
                                // Same slot every time. `reduceStock` decrements in place
                                // (`shop.lua:101-106`) and the grid is rebuilt from the same list,
                                // so the heart does not move while we are buying it; and a click on
                                // a sold-out item does nothing, because `mousereleased` checks
                                // stock before it takes the gold
                                // (`ui/elements/shopitem.lua:147-165`). Overshooting is therefore
                                // harmless, which is what makes clicking to a count safe without a
                                // fresh dump between presses — there isn't one, the inventory is
                                // printed on open only.
                                let stock = crate::shopplay::stock_of(&inv, crate::shopplay::HEART);
                                let want = stock.min(r.map.hearts_affordable()).max(0);
                                r.log.push_str(&format!(
                                    "  the heart is item {i}, at ({x}, {y}) — {stock} in stock, \
                                     {} gold, buying {want}\n",
                                    r.map.gold()
                                ));
                                match r.win.client_to_screen(x, y) {
                                    Ok((sx, sy)) => {
                                        for _ in 0..want {
                                            let _ = click_at_in(r.win, sx, sy);
                                            std::thread::sleep(Duration::from_millis(700));
                                        }
                                        r.pump();
                                        want > 0
                                    }
                                    Err(e) => {
                                        r.log.push_str(&format!("  could not aim at it: {e}
"));
                                        false
                                    }
                                }
                            }
                        },
                    };

                    // Leave whatever happened. The back arrow is the mirror of `Sell` at the other
                    // end of the same bar, measured off the frame this screen was first captured on.
                    if let Ok((bx, by)) = r.win.client_to_screen(1755, 916) {
                        let _ = click_at_in(r.win, bx, by);
                        std::thread::sleep(Duration::from_millis(900));
                        r.pump();
                    }

                    // **Confirmed from the save, not from the press.** `core.save` writes the whole
                    // `shopData` back to `areaFlags[<key>_shops]` when the shop closes
                    // (`shop.lua:303-309`), and `reduceStock` records what was bought under
                    // `purchased` (`:101-106`). So leaving is what makes the purchase readable, and
                    // this is the first moment it can be checked.
                    r.apply_save();
                    let spent = r.heart_is_recorded(&container);
                    r.log.push_str(&format!(
                        "  heart bought={bought}, and the save {}
",
                        match spent {
                            true => "agrees",
                            false => "does not show it yet",
                        }
                    ));
                    // Written off either way: a store that did not sell us one this time will not
                    // sell us one next time either, and standing here again is the bounce every
                    // other errand in this file has already had to learn not to do.
                    r.map.abandon(at);
                    if spent || bought {
                        r.map.bought_the_heart(&container);
                    }
                    continue;
                }
                r.log.push_str(&format!("{step}. at **{at}** in `{container}` — resting\n"));
                let rested = r.rest_at_inn();
                // **Written off only if it gave us nothing.** This used to abandon before trying,
                // which is what turned one missed press into "walk to the next village at 7/20":
                // the inn stopped being a destination, `seeking_a_rest` went false, and the crossing
                // routed out of the village.
                //
                // Abandoning still has to happen on failure, or `cross_toward` returns `Arrive`
                // here for ever. But a *partial* rest is progress, not failure — health went up, so
                // coming back round to the same inn is a loop with a monotone measure under it,
                // which is the one kind that terminates. It ends when the bar is full (`wants_rest`
                // clears) or the purse is empty (`inn_inside`'s gold gate stops nominating it), and
                // **A failure is counted, not taken as a verdict** — see [`Run::rest_failures`] —
                // and "nothing to buy" is not a failure at all. Collapsing those two is what made
                // arriving at full health cost three visits.
                match rested {
                    Rested::Healed => {
                        r.rest_failures.remove(at);
                    }
                    Rested::NothingToDo => {
                        r.rest_failures.remove(at);
                        // **The errand is over, and the inn just said so.** `wants_rest` otherwise
                        // clears only on *reading* full health, and the read that would do it is not
                        // on disk until we have left — `overworld:save()` runs in the inn's `goBack`
                        // (`ui/inn.lua:9`) — so the decision to come back is taken before the
                        // evidence lands. Live 2026-08-10: healed to full, walked out, walked
                        // straight back in, and opened the rest screen again to be told
                        // `healthNeed = 0`. This is that same `healthNeed`, read one screen earlier.
                        r.map.rest_errand_over();
                    }
                    Rested::Failed => {
                        let n = r.rest_failures.entry(at.clone()).or_insert(0);
                        *n += 1;
                        if *n >= REST_GIVE_UP {
                            r.log.push_str(&format!(
                                "  rest: {at} has given us nothing {n} times — writing it off\n"
                            ));
                            r.map.abandon(at);
                        } else {
                            r.log.push_str(&format!(
                                "  rest: nothing landed at {at} ({n} of {REST_GIVE_UP}) — trying again\n"
                            ));
                        }
                    }
                }
                // Re-read before anything plans on it: `overworld:save()` runs in the inn's
                // `goBack` (`ui/inn.lua:9`), so leaving is the moment the new health is readable.
                // This is what clears `wants_rest` and lets the run get on with the anomaly.
                if let Some(h) = r.apply_save() {
                    r.log.push_str(&format!(
                        "  health is now {}/{}{}\n",
                        h.current,
                        h.max,
                        match rested {
                            Rested::Healed => "",
                            Rested::NothingToDo => " (nothing left to buy)",
                            Rested::Failed => " (nothing was spent)",
                        }
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
                Crossing::Step { to, toward } => match fresh.nodes.iter().find(|n| &n.key == to) {
                    // The door's reason is printed because three runs in a row turned on which
                    // branch chose it, and the line looked identical in all of them.
                    Some(n) => (
                        format!(
                            "crossing `{container}` toward `{toward}` ({}) via `{to}`",
                            r.map.door_reason().map(|d| d.why()).unwrap_or("held from earlier")
                        ),
                        (n.x, n.y),
                    ),
                    None => return Stop::Failed(format!("{to} is not adjacent on screen from {here}")),
                },
                // **Separated from `Step`, which it used to share a line with.** The two mean
                // opposite things — one is a hop along a route, the other is a walk into the dark
                // because no route exists — and printing them alike cost a whole run's diagnosis.
                //
                // Live in `l2` on 2026-08-09: twenty-two consecutive lines read `crossing l2 toward
                // l2_path_to_l1 ... via l2subN`, which reads as a considered route across a village.
                // Every one of them was this branch. The exits section of a dump gives a door's
                // POSITION but never its key (`overworldview.lua:1041-1047`), so
                // `l2_path_to_l1` was not a node in our graph until a dump finally named it as a
                // neighbour — at the second-to-last step. The run explored the whole village because
                // routing to the door was not something it could do, and the log said otherwise.
                Crossing::Probe { to, toward } => match fresh.nodes.iter().find(|n| &n.key == to) {
                    Some(n) => (
                        format!("`{toward}` is not on any route we know — probing `{container}` via `{to}`"),
                        (n.x, n.y),
                    ),
                    None => return Stop::Failed(format!("{to} is not adjacent on screen from {here}")),
                },
                // No route, but the dump on screen right now says which way the door lies. Its own
                // line for the same reason `Explore` got one: it is a third decision procedure, and
                // one that will be wrong in a way the others are not — straight-line distance knows
                // nothing about walls, so a steer that repeats without arriving is the signature to
                // look for.
                Crossing::Steer { to, toward } => match fresh.nodes.iter().find(|n| &n.key == to) {
                    Some(n) => (
                        format!("steering `{container}` toward where `{toward}` is printed, via `{to}`"),
                        (n.x, n.y),
                    ),
                    None => return Stop::Failed(format!("{to} is not adjacent on screen from {here}")),
                },
                // Its own line, and deliberately not either of the two above. A search with no
                // destination at all is a third thing.
                //
                // Two ways to have no destination, and the log used to name only one of them. An inn
                // we have not found is the errand case; an exit the fog has not shown us is the
                // crossing case, and a lost woods makes it the common one. Reporting the second as
                // `searching e1 for its inn` had the run looking for a bar in the woods.
                Crossing::Seek { to } => match fresh.nodes.iter().find(|n| &n.key == to) {
                    Some(n) => (
                        match r.map.seeking_an_inn(&container) {
                            true => format!("searching `{container}` for its inn via `{to}`"),
                            false => format!("no way out of `{container}` in sight — probing via `{to}`"),
                        },
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
                // **One drag was one attempt too few.** [`pan_again`] carries the stopping rule: it
                // gives up the moment a pull gains nothing, which is what the map's own bound looks
                // like from here, and caps the rest.
                let mut spent = 0usize;
                let mut last = None;
                // A drag that happened but could not be measured. Not a failure by itself — see the
                // shared recovery below — but it does end this pulling loop, because `at` no longer
                // describes anything: the view moved by an unknown amount.
                let mut unmeasured = false;
                while pan_again(at, cw, ch, last, spent) {
                    let want = pan::shift_to_reach(at, cw, ch);
                    let Some(got) = r.pan_map(want) else {
                        unmeasured = true;
                        break;
                    };
                    at = pan::moved(at, got);
                    r.log.push_str(&format!(
                        "  panned by ({:.0}, {:.0}) of ({:.0}, {:.0}) wanted; `{}` now at ({:.0}, {:.0})\n",
                        got.dx, got.dy, want.dx, want.dy, what, at.0, at.1
                    ));
                    (last, spent) = (Some(got), spent + 1);
                }
                // **A pan that could not deliver is not the end of a run.** It used to be, and the
                // run of 2026-08-09 ended exactly here: the road out of `l2` sat at y = -130, the
                // pan asked for `(0, 164)` and measured `(38, -26)`, and that was that.
                //
                // That measurement has a documented shape — see [`Run::needs_recentre`], where a pan
                // asked for `(27, 0)` and measured `(28, -128)` because the game's own motion landed
                // inside our measurement. The cure recorded there is not a better detector but
                // locate-me, which ends the motion instead of reading around it and returns a dump
                // settled by construction. So take that cure here too rather than a second guess at
                // the same coordinate: re-centre, and derive the step again from a view we know is
                // still.
                //
                // Bounded, because a node the map genuinely will not show must still end the run
                // rather than spin.
                //
                // **Two ways in, and they want the same cure.** A pan that measured cleanly but did
                // not fetch the node is the case this branch was written for. A pan that could not
                // be **measured** is the other, and it used to end the run on the spot — even though
                // `pan::measure` returns `None` rather than a guess *precisely* so the caller can
                // re-establish position, and says so in its own doc. Ending the run was the one
                // response that never did. Live 2026-08-14: the road out of `l24` sat at (1381,
                // 1230), the first drag came back unmeasured, and that was the run.
                //
                // Locate-me answers both, because it does not read the view — it *ends* the motion
                // and asks the game where the player is. An unknown position is exactly what it
                // repairs, and re-centring also moves the target, so a node that no drag could fetch
                // may simply be on screen afterwards.
                if unmeasured || pan_again(at, cw, ch, None, 0) {
                    // **Before spending a retry: make the map smaller.**
                    //
                    // Dragging has two failure modes here and zooming answers both. A node below the
                    // window comes back inside it because everything halves toward the centre; and a
                    // patch that will not correlate stops mattering, because the fresh dump after
                    // the zoom carries new coordinates for everything rather than a measured delta.
                    // Task #29, and the dev's call after three runs died on the same exit.
                    //
                    // One press does the whole job. `pagedown` is bound to `scrollDown5`
                    // (`utils/defaultbinds/keyboard.lua:30`), which is `love.wheelmoved(0, -5)`
                    // (`main.lua:467`), and the overworld's handler is
                    // `core.setZoom(y > 0)` (`overworldview.lua:1529-1531`) — one step whatever the
                    // magnitude. A step out halves `targetZoomMul` and it is clamped at `0.5`
                    // (`:996`), so from the default `1` a single press reaches the floor and further
                    // presses are silent no-ops. That is why this fires once per run and not once
                    // per retry.
                    if r.zoom_out() {
                        continue;
                    }
                    if r.pan_retries >= MAX_PAN_RETRIES {
                        return Stop::Failed(match unmeasured {
                            true => format!(
                                "{what}: last seen at ({:.0}, {:.0}); the pan could not be measured \
                                 after {MAX_PAN_RETRIES} re-centres — position is unknown",
                                at.0, at.1
                            ),
                            false => format!(
                                "{what}: still out of reach at ({:.0}, {:.0}) after panning and \
                                 {MAX_PAN_RETRIES} re-centres",
                                at.0, at.1
                            ),
                        });
                    }
                    r.pan_retries += 1;
                    r.needs_recentre = true;
                    r.log.push_str(&match unmeasured {
                        // Deliberately "last seen at": after an unmeasured drag the coordinate is
                        // where the node *was*, and reporting it as current is how a reader ends up
                        // debugging a number that stopped meaning anything.
                        true => format!(
                            "  `{what}` was last seen at ({:.0}, {:.0}) and the pan could not be \
                             measured — re-centring to re-establish position (try {} of \
                             {MAX_PAN_RETRIES})\n",
                            at.0, at.1, r.pan_retries
                        ),
                        false => format!(
                            "  `{what}` is at ({:.0}, {:.0}) and panning did not fetch it — \
                             re-centring and deriving the step again (try {} of \
                             {MAX_PAN_RETRIES})\n",
                            at.0, at.1, r.pan_retries
                        ),
                    });
                    continue;
                }
                r.pan_retries = 0;
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
            // **Corruption is a level, not a locked door — `completed` is what says whether a fight
            // is still owed.** A corrupted village that has been cleared has its inn back; it is a
            // harder fight than an ordinary village and that is the whole of the difference.
            //
            // The planner already knew this — its rest-site filter reads `(!p.corrupted ||
            // p.completed)` (`overworld::plan`) — and this line did not, so the two disagreed: the
            // planner would route to a cleared corrupted village *for the rest* and the driver would
            // arrive and decline to take it. A disagreement between planner and driver is the shape
            // that produced the `shrine1 -> l10 -> shrine1` bounce, and the same omission is what
            // left `shrine1` unconsecrated for four runs — a filter testing `corrupted` without
            // asking `completed`.
            // **Two reasons to walk into a village now.** A bed, and a heart -- see
            // `WorldMap::wants_a_heart`. The gate is otherwise identical, corruption clause and all:
            // corruption is a level rather than a locked door, and a cleared corrupted village has
            // its shops back.
            //
            // **`is_settlement`, not `type_is("village")`.** A town is a settlement too, and the
            // planner's heart filter has always known that while this line did not — which is the
            // whole of the `l28 <-> l27` bounce the dev stopped by hand. See [`Place::is_settlement`].
            //
            // **And a bed is now worth stopping for whenever we are passing one.** The dev's rule,
            // 2026-08-15: *if we're at a settlement, have less than full health, and have the gold
            // to rest at the inn, go rest.* That is a weaker condition than `wants_rest`, which
            // waits for half health or a four-point drop — deliberately, because those are the bars
            // for making a *detour*. Standing in the doorway is not a detour, and ten gold for six
            // health before the next fight is the cheapest trade on the board.
            let top_up = r.map.top_up_at(&p.key);
            let rest_here = p.is_settlement()
                && (!p.corrupted || p.completed)
                && ((r.map.wants_rest() && r.map.gold() >= crate::rest::INN_COST)
                    || top_up
                    || (r.map.wants_a_heart() && !r.map.heart_is_spent(&p.key)));
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
            // What that button opens is not ours to predict, so nothing here tries to.
            //
            // `getLocationButtons` tests `typeData.subworld` BEFORE `basicCombatZone`
            // (`overworldview.lua:462-467`), so the button labelled for a fight enters a forest or a
            // village instead whenever the node is one -- and their headings read exactly like
            // fights. A chest's button is `Open`, and goes straight into combat with no pregame at
            // all. Three outcomes from one press, and this used to try to tell them apart from a
            // single console announcement, which is why a chest ended a run.
            //
            // Now it presses, waits for the transition, and lets the top of the loop identify what
            // arrived -- where `Screen::Pregame` and `Screen::CombatEntered` already have handlers
            // that were being duplicated here. Being inside a subworld needs no handler at all: the
            // map path deals with it.
            let inside_before = r.map.inside().map(str::to_string);
            r.snap_area_slot("combat-live");
            if !matches!(r.click_area_button("Combat"), Ok(true)) {
                // **A screen diff is a worse witness than the observer, so ask the observer.**
                //
                // `click_area_button` judges a press by how much the window changed one second
                // later. That is a reasonable *first* question and a terrible last one: the pregame
                // animates in, and a frame caught before it starts is identical to the map it
                // replaced. The dev has said the observer belongs here as the fallback, and they are
                // right — `identify` names the screen whatever the diff happened to catch.
                //
                // Live 2026-08-15 at `l16sub5`: `clicked Combat: screen moved 0.002`, run over.
                // `gave-up.png` from that stop is unmistakably the pregame — `Bursall Hedge — level
                // 2 road` across the top, `Start` at the bottom — with the scene behind it still
                // black because it had not finished rendering. The press had landed. Nothing looked.
                //
                // Bounded by the same [`COMBAT_OPENS_BY`] the other post-press re-look uses, so a
                // press that genuinely did nothing still ends the run rather than spinning.
                let by = Instant::now() + COMBAT_OPENS_BY;
                let mut opened = None;
                while Instant::now() < by {
                    let s = crate::act::identify(r.win);
                    if matches!(s, crate::act::Screen::Pregame | crate::act::Screen::CombatEntered) {
                        opened = Some(s);
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
                match opened {
                    Some(s) => {
                        r.log.push_str(&format!(
                            "  the diff saw nothing but the observer found {s:?} — the press landed                              after all
"
                        ));
                        r.combat_expected = true;
                        continue;
                    }
                    None => {
                        r.snap_screen("combat-no-diff");
                        r.log_button_scores();
                    }
                }
                return Stop::Failed(format!("Combat did not open at {here}"));
            }
            r.combat_expected = true;
            r.settle_after_mode_change(inside_before);
            continue;
        }

        let Some(hop) = hop else { return Stop::NoPlan };
        let Some(target) = fresh.nodes.iter().find(|n| n.key == hop.step).cloned() else {
            return Stop::Failed(format!("{} is not on screen from {here}", hop.step));
        };
        // The suffix says whether this step is *on the way* or merely *the way it lies*. Without it
        // a run heading nowhere reads exactly like a run heading somewhere -- five identical
        // `(for start, Anomaly)` lines were read as a journey and were five guesses.
        r.log.push_str(&format!(
            "{step}. {here} -> **{}** (for {}, {:?}){}\n",
            hop.step,
            hop.plan.target,
            hop.plan.reason,
            match hop.routed {
                true => "",
                false => " — no route there; stepping the way it lies",
            }
        ));
        // **Whether exploring was actually steered, and by what.** An `Explore` line looks identical
        // whether the corruption pulled the choice or nearest-unvisited did, which is how a steering
        // rule that had stopped working went unnoticed. `placed of total` is the honest part: a
        // bearing that can measure two candidates out of nine is barely steering at all.
        if let Some((toward, placed, total)) = &hop.plan.steered_by {
            r.log.push_str(&format!(
                "  steering toward `{toward}` ({placed} of {total} candidates placed)
"
            ));
        } else if matches!(
            hop.plan.reason,
            crate::overworld::Goal::Explore | crate::overworld::Goal::RouteTo(_)
        ) {
            // Both shapes of exploring, because the silent one is the interesting one: a `RouteTo`
            // with nothing to aim by is a hop that knows its errand and is walking at random.
            r.log.push_str("  not steered — exploring by hops alone
");
        }
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
            // Fresh coordinates, from a dump that is now required to be newer than the arrow press —
            // and to describe a view the player is actually in. Retrying a failed selection against
            // a camera that has not caught up spends all `SELECT_RETRIES` aiming at the same wrong
            // place, which reads in the log as the click not registering.
            let (cw, ch) = r.win.client_size().unwrap_or((1920, 1080));
            match r.recentre().filter(|a| !camera_is_lost(&a.nodes, cw, ch)) {
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
        }
        *health = now;
        let _ = Goal::Explore;
    }
    Stop::Exhausted
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Both ways into a fight must be answered by the loop, not by whoever pressed the button.
    ///
    /// This is the invariant the chest broke. The pregame and the first turn of combat are the two
    /// states an area button can produce, and for as long as the caller handled them itself it could
    /// only recognise the door it knew about — so `Open`, which announces nothing, stalled a run with
    /// the board already in the feed.
    ///
    /// [`Answer::Elsewhere`] is the failure this guards against: it means "some other component owns
    /// this", which for these two would put the knowledge back at a call site. A run can arrive on
    /// either screen without having pressed anything, so neither has a single owner to give it to.
    #[test]
    fn the_two_faces_of_entering_combat_are_both_answered_by_the_loop() {
        for s in [Screen::Pregame, Screen::CombatEntered] {
            let answer = answer_for(s);
            assert!(
                matches!(answer, Answer::Bespoke | Answer::Fight),
                "{s:?} must be handled at the top of `drive`, not delegated: got {answer:?}"
            );
            assert!(precheck(s).is_none(), "{s:?} must not stop the run");
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
    fn a_lit_consecrate_is_answered_by_pressing_it_not_by_leaving() {
        use crate::act::{Screen, SHRINE_CONSECRATE, SHRINE_GOBACK};

        // Both plaques are on the shrine screen at the same time, in slots that do not overlap --
        // `Consecrate` on the right, the back arrow on the left. So `identify` can satisfy either
        // check on the same frame and the ONLY thing separating them is which is asked first. That
        // is what makes the ordering load-bearing rather than incidental, and on 2026-08-12 the back
        // plaque won and a consecration was thrown away.
        let (cx0, _, cx1, _) = SHRINE_CONSECRATE.search;
        let (bx0, _, bx1, _) = SHRINE_GOBACK.search;
        assert!(cx0 > bx1 || bx0 > cx1, "the two slots must be distinct for both to be present");

        // And **nothing on the escape path may press it**. The button is drawn greyed until the
        // shrine's word is solved (`ShowAGoodButton()`, `shrine.lua:36-40`), so an actor that has
        // not solved it cannot know whether pressing does anything — which is how a consecration
        // came to be reported twice, once as done and once as failed. The shrine driver owns it.
        //
        // The screen still needs a way out, which is the part the first version of this got wrong.
        // Removing the entry altogether left `ShrineConsecrate` with no answer at all, and since
        // `identify` prefers it to `Shrine` whenever the slot is occupied, it shadowed the ordinary
        // shrine escape and trapped a run for its whole length. So: an escape, pressing **Go back**.
        let entry = ESCAPES
            .iter()
            .find(|e| e.screen == Screen::ShrineConsecrate)
            .expect("the screen must have a way out or it is a trap");
        assert_eq!(
            entry.button.name, SHRINE_GOBACK.name,
            "leaving is always safe; pressing `Consecrate` needs a solved word and only the shrine              driver knows that"
        );
        assert!(matches!(answer_for(Screen::ShrineConsecrate), Answer::Escape));
    }

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

    /// **And the other direction, which is the one that traps runs.**
    ///
    /// `answer_for` is exhaustive over [`Screen`], so a new variant cannot be added without somebody
    /// deciding what happens. That check has a blind side: deciding `Answer::Escape` and then not
    /// writing the [`ESCAPES`] entry compiles, passes every test, and produces a screen the run
    /// recognises and cannot leave.
    ///
    /// Which is not hypothetical. `ShrineConsecrate` was moved off `Escape` on 2026-08-15 for a good
    /// reason and back onto it later the same day for a better one; in between, `identify` preferred
    /// it to `Shrine` whenever the slot was occupied, so it shadowed the ordinary shrine exit and a
    /// live run spent its whole length bouncing between that screen and the stats page.
    ///
    /// Two lists agreeing by hand is the shape of half the faults in this file. This is the pair that
    /// can be checked, so it is checked.
    #[test]
    fn every_screen_that_says_escape_has_somewhere_to_go() {
        for &s in Screen::ALL {
            if matches!(answer_for(s), Answer::Escape) {
                assert!(
                    ESCAPES.iter().any(|e| e.screen == s),
                    "{s:?} is answered by escaping and has no escape — it is a trap"
                );
            }
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
