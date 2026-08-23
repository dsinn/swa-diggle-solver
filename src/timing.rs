//! **Every fixed wait in the run, in one place, each with the thing it is waiting for.**
//!
//! The dev, 2026-08-23: *audit our sleep timings against the SWA source code to make sure they're
//! not unnecessarily long, and then ensure that each sleep duration is in a constant and documented
//! with justification for the value.* This module is the second half; the first half is recorded
//! against each constant, and the two values that were provably too long say so by name.
//!
//! # The four things a wait can be waiting for, and only two of them have a source
//!
//! Sorting them this way is the point of the module, because it says which numbers can be argued
//! from the game and which can only be measured:
//!
//! 1. **A frame.** The game reads input once per frame in `love.run` and integrates mouse motion
//!    per frame, so anything below one frame is coalesced away. See [`FRAME`].
//! 2. **A named animation.** `transitionDuration`, the map pan, the zoom lerp, the rest event —
//!    each is a literal in the Lua, and each is cited at the constant that waits for it.
//! 3. **Our own console scrape.** The game prints immediately; the lag is in reading the screen
//!    buffer back. Nothing in the game bounds this, so these are measured rather than derived — and
//!    every one of them would be better as a poll on the line it wants. Marked as *scrape* below.
//! 4. **Nothing at all.** Two of these existed and are now cut: [`MAP_DRAG_APPLIED`] waited out an
//!    animation the game does not have, and [`MAP_ZOOM_SETTLED`] waited more than twice the length
//!    of the one it does.
//!
//! # Three of them stopped being waits
//!
//! The dev, 2026-08-23: *it's the main navigator driver loop where I think we can save the most
//! time.* [`AFTER_SELECT`], [`AFTER_AREA_BUTTON`] and the locate-me arrow all sat in front of a test
//! that could simply be *run* — so each is now a deadline on a poll rather than a sleep, with the
//! decision, the region and the threshold left exactly as they were. Against the 0547Z report's own
//! counts — 116 selections, 130 area buttons, 39 locate-me clicks — that is somewhere around three
//! minutes a run, and a failure still costs what it always did.
//!
//! # The frame rate, and why these are not multiples of it
//!
//! `conf.lua:75` is LÖVE 11.5 and `main.lua:60` calls `love.window.setMode` without a `vsync` key,
//! so vsync stays on at its default and **the frame rate is the display's refresh rate**. This
//! project has measured 120 FPS on the dev's machine — 8.3 ms a frame — but a 60 Hz display is 16.7
//! and a remote session can be 33. So the frame-level constants below are sized for the slowest case
//! that matters rather than for the measured one: being a frame short drops a keystroke, and being
//! three frames long costs milliseconds.
//!
//! # What the game does *not* make us wait for
//!
//! Worth stating once, because several waits were sized as though the opposite were true.
//! `setActiveMode` (`main.lua:191-195`) hands the new mode to `love.graphics.captureScreenshot`,
//! whose callback runs at the end of the current frame; `StartTransition` (`:146-176`) then sets
//! `activeMode = pendingMode` and swaps the input tables **immediately**. `transitionTimer` drives
//! nothing but a dissolve shader in `love.draw` (`:389-391`), and neither `love.mousepressed` nor
//! `love.keypressed` consults it (`:422-423`, `:491`).
//!
//! **So a new screen is live and clickable one frame after the click that opened it.** The
//! transition duration is how long it takes to *look* settled, which matters to a template match
//! and not at all to a keystroke or to the console.

use std::time::Duration;

/// One frame, sized for a 30 Hz display rather than the 120 Hz this project measured.
///
/// Not used as a unit of arithmetic — the constants below are written as the numbers they are, so
/// that changing one does not silently move the others. This is here to be quoted.
pub const FRAME: Duration = Duration::from_millis(33);

// -------------------------------------------------------------------------------------------
// Driver level: pacing the events themselves, in `win::input`.
// -------------------------------------------------------------------------------------------

/// Between the steps of a synthesised cursor move or drag.
///
/// `core:mousemoved` (`overworldview.lua:1500-1517`) applies each delta to `xoffset` as it arrives,
/// and LÖVE delivers whatever the OS coalesced into the frame. Faster than a frame and the steps
/// merge into one jump, which is the thing stepping exists to avoid.
pub const CURSOR_STEP: Duration = Duration::from_millis(25);

/// After the cursor lands, before the button goes down.
///
/// The game's buttons select off hover state maintained in `update`, so the hover has to have been
/// *observed* by a frame before the press is handled — a press in the same frame as the move sees
/// no hover at all.
pub const HOVER_BEFORE_PRESS: Duration = Duration::from_millis(40);

/// How long a synthesised button or key is held down.
///
/// Two frames at 30 Hz, so press and release cannot land in one frame and cancel. It has a second
/// job as the gap between the clicks of a repeat: staying *inside* the OS double-click time, or the
/// game sees two unrelated single clicks.
pub const CLICK_HOLD: Duration = Duration::from_millis(60);

/// The `PostMessage` click path's hover dwell — longer than [`HOVER_BEFORE_PRESS`] because a posted
/// `WM_MOUSEMOVE` competes with the real cursor position rather than replacing it.
pub const POSTED_HOVER: Duration = Duration::from_millis(150);

/// The `PostMessage` click path's button hold. See [`CLICK_HOLD`].
pub const POSTED_HOLD: Duration = Duration::from_millis(80);

/// Between characters when typing at driver level.
pub const TYPE_GAP: Duration = Duration::from_millis(40);

/// Between the keystrokes of a shrine guess.
///
/// **The tightest wait in the project, and deliberately so.** `shrineview.lua`'s `remove`
/// (`:227-231`) trims one character per press and there is no animation to wait out, so the only
/// requirement is that the game see each press as its own event rather than coalescing them — one
/// frame, and this is four at the measured 120 FPS. It is paid `wordLength` times per guess and
/// several guesses per shrine, which is why it is the one place worth being tight.
///
/// The failure modes are not symmetric and the callers rely on that. A *dropped* letter leaves the
/// row short, `submit` refuses outright, and `read_row` reports a timeout — loud. A letter arriving
/// late during length inference is the one that fails silently, and that site re-reads rather than
/// trusting the first sample.
pub const KEYSTROKE_GAP: Duration = Duration::from_millis(34);

/// After `focus()`, before the keystroke it exists to deliver.
///
/// Focus is an OS-level change that the game learns of through SDL's event queue, so it needs a
/// frame in hand before a key press means anything. Five frames at 30 Hz, eighteen at the measured
/// 120.
///
/// **One value for all of them**, which is the change: the same gap was written as 120, 150, 200,
/// 250 and 300 at nine sites, and no two of those numbers were reasoned about separately. Unifying
/// shortens six of them and lengthens one.
pub const FOCUS_SETTLE: Duration = Duration::from_millis(150);

// -------------------------------------------------------------------------------------------
// Animations the game names in its own source.
// -------------------------------------------------------------------------------------------

/// A mode transition's dissolve: `transitionDuration`, plus a frame.
///
/// `utils/defaultconfig.lua:5` ships **0.5 s**. `main.lua:127`'s `or 0.625` is the fallback for a
/// missing key, not the default, and the player can change it in the options — so this is the
/// shipped value rather than a bound.
///
/// **Wait for this only in order to *look* at the new screen.** The mode itself is live a frame
/// after the click, as the module note sets out, so a keystroke or a console read needs no part of
/// it.
pub const SCREEN_DISSOLVE: Duration = Duration::from_millis(550);

/// A map drag, which the game applies as the mouse moves and never animates. **Was 300 ms — #101.**
///
/// `core:mousemoved` (`overworldview.lua:1510-1516`) writes `xoffset` and `yoffset` directly on each
/// event, and `core:mousereleased` (`:1496`) only clears the drag state. There is no tween, no
/// easing and no inertia: `offsetTransition` is cleared outright on `mousepressed` (`:1302`) and
/// again for as long as `isDragging` (`:1257`). **The pan is complete when the last motion event is
/// consumed.**
///
/// `drag_in` has already slept [`CLICK_HOLD`] before its button-up, so by the time this is reached
/// the offset has been settled for two frames at 30 Hz. What is left is one frame drawn with the new
/// offset in it, because the next thing the caller does is capture the window and measure the shift.
/// Three frames at 30 Hz is the margin on that.
pub const MAP_DRAG_APPLIED: Duration = Duration::from_millis(100);

/// The scripted map pan, `core.centreScreenOn` without `instant`.
///
/// `offsetTransition = math.min(1, offsetTransition+delta)` (`overworldview.lua:1250`) advances by
/// one *second* per second, so this tween takes **exactly 1.0 s** whatever the distance, eased at
/// both ends by `math.easeBoth`. Unlike the drag above, this one is real and has to be waited out.
pub const MAP_RECENTRE: Duration = Duration::from_millis(1000);

/// The zoom lerp settling. **Was 900 ms; the animation is less than half that.**
///
/// `UpdateZoom` (`overworldview.lua:1087-1097`) is `zoomMult = lerp(zoomMult, targetZoomMul, dt*10)`
/// with a snap to the target once the gap falls under 0.01. That is exponential decay with a 100 ms
/// time constant, so even the widest jump the clamp allows — `[0.5, 8]` at `:996` — reaches the snap
/// threshold in `ln(0.5/0.01)/10` ≈ **390 ms**.
///
/// Rounded up rather than trimmed to the arithmetic, because the lerp advances per frame and a slow
/// frame stretches it, and because the caller's next act is to place nodes at the new scale.
pub const MAP_ZOOM_SETTLED: Duration = Duration::from_millis(500);

/// Sleeping at an inn, from the click to the screen behind the dream.
///
/// Two animations end to end, and both are literals: the rest event fires at `eventTime>2`
/// (`ui/rest.lua:414-419`) and then transitions for another 2.5 s
/// (`overworld/events/rested.lua:18`, which overrides `transitionDuration` for exactly this).
/// Clicking earlier lands on the rest screen we are still looking at.
///
/// **Left at 4.5 s plus half a second rather than trimmed to the sum.** By the module's own argument
/// the inn is live at 2 s and only the dissolve takes the other 2.5, so a much shorter value would
/// probably work — but this fires once per rest, the margin costs nothing measurable, and getting it
/// wrong means pressing into the dream. The next thing the caller does is wait up to 60 s for the
/// inn to announce itself, which is where the real readiness test lives.
pub const REST_DREAM: Duration = Duration::from_millis(5000);

// -------------------------------------------------------------------------------------------
// Poll cadences. Not waits: one costs at most a single interval of extra latency, and the
// deadline belongs to the caller.
// -------------------------------------------------------------------------------------------

/// Between reads of the combat board while it is animating.
pub const POLL_BOARD: Duration = Duration::from_millis(100);

/// Between crops of the area-button strip while waiting for a map selection to show up in it.
///
/// The strip is repainted from `getAreaButtons` on the frame after `core:mousereleased` sets the
/// selection (`overworldview.lua:1472`), so this is two frames at 30 Hz and seven at the measured
/// 120. Each sample is a `BitBlt` of about a twelfth of the window — see
/// [`crate::navigate::Run::select_and_watch`] for why that is what makes polling here affordable at
/// all.
pub const POLL_SELECT: Duration = Duration::from_millis(60);

/// Between screen-buffer pumps when the console is the instrument.
pub const POLL_CONSOLE: Duration = Duration::from_millis(200);

/// Between full captures when a template or a screen fingerprint is the instrument. Slower than
/// [`POLL_CONSOLE`] because a capture costs orders of magnitude more than a buffer read.
pub const POLL_SCREEN: Duration = Duration::from_millis(250);

/// Between scores of one button's patch, which is a crop rather than a whole window.
pub const POLL_BUTTON: Duration = Duration::from_millis(120);

/// Between re-reads of `mainSaveData`.
///
/// The game writes it on screen *exit* rather than on the act, so polling faster buys nothing and
/// competes with the game's own writer for the file.
pub const POLL_SAVE: Duration = Duration::from_millis(400);

/// Between the passes of a search for a button that has not appeared yet — a whole capture and a
/// sweep, which is the most expensive poll here.
pub const POLL_LOCATE: Duration = Duration::from_millis(400);

/// Between checks that the launched process has opened a window.
pub const POLL_LAUNCH: Duration = Duration::from_millis(200);

/// Between the frame pairs of a quiescence test.
pub const POLL_QUIESCENCE: Duration = Duration::from_millis(200);

/// Between pumps while an arrival is expected. The budget that ends the wait is
/// [`crate::overworld::pace::walk_budget`]'s; this only decides how finely it is sampled.
pub const POLL_ARRIVAL: Duration = Duration::from_millis(300);

/// The brisk tier: a cheap read whose answer is expected within a step or two — a subworld entry
/// landing, an affirmative slot coming live, a save being rewritten after a fight.
pub const POLL_BRISK: Duration = Duration::from_millis(150);

/// One tile redrawing on an already-static screen.
///
/// A shrine's board does not animate a typed letter — `shrineview` paints the row from the guess
/// buffer — so the only wait is the next frame, ~8 ms at the measured 120 FPS. This leaves an order
/// of magnitude of headroom because it is cheap to be wrong in this direction: a letter that has not
/// rendered moves all four candidate boxes equally, `infer_length` declines rather than guesses, and
/// the retry loop takes another sample.
pub const TILE_REDRAW: Duration = Duration::from_millis(100);

// -------------------------------------------------------------------------------------------
// Scrape settles. **These are ours, not the game's** — it has already printed by the time the
// wait starts, and what is being waited on is our screen-buffer scrape carrying the line. Each
// would be better as a poll on the line it wants; where a caller already retries, the value is a
// budget for one attempt rather than a claim about the game.
// -------------------------------------------------------------------------------------------

/// **A deadline, not a wait.** How long [`crate::navigate::Run::select_and_watch`] keeps asking
/// whether a map selection has shown up in the area-button strip.
///
/// The selection itself is instant: `core:mousereleased` (`overworldview.lua:1472`) compares
/// `mousePressedOn` against what is under the cursor and sets it, with no animation, no mode change
/// and — checked against a whole run's console — nothing printed. It used to be slept through in
/// full; now it is polled at [`POLL_SELECT`] and a selection that works costs a tenth of this.
///
/// The number is unchanged because it is now only what a *failure* costs, and a failure was already
/// worth this much patience: the caller has `SELECT_RETRIES` attempts and re-centres between them.
pub const AFTER_SELECT: Duration = Duration::from_millis(900);

/// After a click that dismisses or advances a screen, before reading what is behind it.
/// [`SCREEN_DISSOLVE`] plus scrape.
pub const AFTER_SCREEN_PRESS: Duration = Duration::from_millis(700);

/// After a click that opens a whole new mode from a menu — the longest of these, because it also
/// waits for the mode's `onActive` to run and print.
pub const AFTER_MODE_CHANGE: Duration = Duration::from_millis(1200);

/// After resuming a saved game, which loads a world before it prints anything.
pub const AFTER_RESUME: Duration = Duration::from_millis(1500);

/// **A deadline, not a wait.** How long [`crate::navigate::Run::click_area_button`] keeps asking
/// whether an area button actually landed.
///
/// It was the most-repeated wait in a run, slept flat on every press. Two things changed that. It is
/// now *sampled* at [`POLL_SCREEN`] against the same region and the same bar, so a press whose
/// screen has plainly moved stops paying for the presses that have not — the screen begins changing
/// on the frame after the click (`main.lua:146-176`, `:389-391`), and what the second was really
/// covering is that the test is a full-window `PrintWindow` at a 28.5 ms median. And the presses
/// with a better test of their own no longer come here at all: subworld `Travel` waits on arrival
/// and the inn's `Enter` waits on the console, so both take
/// [`crate::navigate::Run::press_area_button`] and read nothing.
///
/// What is left on this deadline is the press whose *only* test is the screen moving.
pub const AFTER_AREA_BUTTON: Duration = Duration::from_millis(1000);

/// After a click on a shop shelf or its paging arrow.
pub const AFTER_SHOP_PRESS: Duration = Duration::from_millis(900);

/// A general retry pause, where the last attempt was refused and the next one is a fresh look.
pub const BEFORE_RETRY: Duration = Duration::from_secs(1);

/// A dwell long enough for the game to have registered a hover as *state* rather than as an event.
///
/// Longer than [`HOVER_BEFORE_PRESS`] because the thing being waited for is different: hero select
/// reads `bodyHover` (`herodisplay.lua:44,214-217`), a flag maintained in `update` from the cursor
/// position, and the tooltip layer uses `tooltipDelay`, which ships at **0.2 s**
/// (`utils/defaultconfig.lua:4`). A dwell shorter than that is a move, not a hover.
pub const HOVER_DWELL: Duration = Duration::from_millis(400);

#[cfg(test)]
mod tests {
    use super::*;

    /// **Each constant that claims a source is checked against the arithmetic of that source.**
    ///
    /// These do not test our code; they test that the numbers still say what the Lua says, so that
    /// trimming one later fails here rather than in a run.
    #[test]
    fn every_wait_with_a_source_still_covers_what_it_waits_for() {
        // `zoomMult = lerp(zoomMult, targetZoomMul, dt*10)` with a snap at a gap under 0.01, over
        // the widest jump `setZoom`'s `[0.5, 8]` clamp allows: exponential decay, 100 ms constant.
        let zoom_settles_in = (0.5f64 / 0.01).ln() / 10.0;
        assert!(zoom_settles_in < 0.4, "the arithmetic itself moved: {zoom_settles_in}");
        assert!(
            MAP_ZOOM_SETTLED.as_secs_f64() > zoom_settles_in,
            "{MAP_ZOOM_SETTLED:?} no longer covers a {zoom_settles_in:.3}s lerp"
        );

        // `offsetTransition + delta` per frame to 1: one second, exactly, whatever the distance.
        assert!(MAP_RECENTRE.as_secs_f64() >= 1.0, "the scripted pan is a full second");

        // `eventTime>2` then a 2.5s transition.
        assert!(REST_DREAM.as_secs_f64() >= 4.5, "the dream is 2s of event and 2.5s of transition");

        // `utils/defaultconfig.lua:5`.
        assert!(SCREEN_DISSOLVE.as_secs_f64() >= 0.5, "transitionDuration ships at 0.5s");

        // Anything meant to be seen as its own event has to outlast a frame on a 120 Hz display,
        // which is the fastest this game has been measured running.
        let fastest_frame = 1.0 / 120.0;
        for (name, d) in [
            ("KEYSTROKE_GAP", KEYSTROKE_GAP),
            ("CURSOR_STEP", CURSOR_STEP),
            ("HOVER_BEFORE_PRESS", HOVER_BEFORE_PRESS),
            ("CLICK_HOLD", CLICK_HOLD),
            ("TILE_REDRAW", TILE_REDRAW),
        ] {
            assert!(d.as_secs_f64() > fastest_frame, "{name} is inside one frame at 120 FPS");
        }

        // A dwell has to outlast `tooltipDelay`, which ships at 0.2s, or it is a move not a hover.
        assert!(HOVER_DWELL.as_secs_f64() > 0.2, "shorter than tooltipDelay is not a hover");
    }

    /// **No fixed wait may be written as a literal again**, which is the half of #101 the dev asked
    /// for: *ensure that each sleep duration is in a constant and documented with justification.*
    ///
    /// Scanned rather than trusted, because the rule is one a later edit breaks by habit and not by
    /// intent. `src/bin` is out of scope — a spike is allowed to be a spike.
    #[test]
    fn the_library_holds_no_sleep_literals() {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for e in std::fs::read_dir(dir).expect("readable").flatten() {
                let p = e.path();
                match p.is_dir() {
                    true if p.file_name().is_some_and(|n| n == "bin") => {}
                    true => walk(&p, out),
                    false if p.extension().is_some_and(|x| x == "rs") => out.push(p),
                    false => {}
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        walk(&root, &mut files);
        assert!(files.len() > 20, "the walk found almost nothing: {}", files.len());

        let mut offenders = Vec::new();
        for f in &files {
            let text = std::fs::read_to_string(f).expect("readable");
            for (i, line) in text.lines().enumerate() {
                let Some(rest) = line.split_once("sleep(").map(|(_, r)| r) else { continue };
                if rest.trim_start().starts_with("Duration::from_")
                    || rest.trim_start().starts_with("std::time::Duration::from_")
                {
                    offenders.push(format!("{}:{}  {}", f.display(), i + 1, line.trim()));
                }
            }
        }
        assert!(offenders.is_empty(), "sleep literals are back:\n{}", offenders.join("\n"));
    }
}
