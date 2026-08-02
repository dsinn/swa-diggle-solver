//! Clicking a button only after confirming it IS that button.
//!
//! Actuation is now injected mouse clicks (design v2 §3), which means every action is a bare screen
//! coordinate — and a coordinate is a promise about layout that nothing checks. Two facts make that
//! dangerous rather than merely sloppy:
//!
//! - **`Restart` sits beside `Continue`** on the start menu, and with `mainSaveData` present it
//!   eulogises the current run (`ui/heroselect.lua:271`). A 300 px error destroys the save.
//! - **Buttons share slots.** `Hint` and `Fight on!` are both at normalized (0.72, 0.9); `Finish`
//!   and `Give up` are both at (0.9, 0.9) (`rpg.lua:504`/`:531`, `:561`/`:571`). The same pixel is a
//!   different button depending on state, so position can never identify one.
//!
//! So a click is gated on a template match of the button's own face, including its label text —
//! the label is what distinguishes `Continue` from `Restart`. A layout change makes this **refuse**
//! rather than click the wrong thing, which is the failure mode we want.
//!
//! The templates are cropped from verified live frames (`diggle croppng`), strictly inside the
//! button face: a box that catches the surrounding scene would match unreliably, which is the same
//! error that produced the F1 probe's phantom states.

use crate::observe::template::{find_at_scale_in, Template};
use crate::win::capture::capture_window;
use crate::win::window::GameWindow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A button we are willing to click, described by how to *recognise* it.
pub struct Button {
    pub name: &'static str,
    /// Template file, relative to the repo's `templates/` directory.
    pub template: &'static str,
    /// Where to look for it, in client pixels, with slack for minor layout drift.
    pub search: (i32, i32, i32, i32),
    /// Where to click once recognised, in client pixels. Not necessarily the template's centre —
    /// the progress button is clipped by the right screen edge, so its geometric centre is
    /// off-screen.
    pub click: (i32, i32),
}

/// Combat's `Finish`, which ends a cleared fight.
///
/// `ss(0.9, 0.9)` with the 300x100 `default` size — centre (1728, 972), matching the coordinate a
/// live run already clicks (`WaitPhase -> Finish at (1728,972)`).
///
/// ## What it does and does not prove
///
/// It proves **`WaitPhase`**, not "a fight is happening". The button is drawn only once the enemies
/// are done; mid-`PlayerTurn` the slot is empty, which is why a combat capture from turn 2 scores
/// 0.2317 here. A general "are we in combat" check needs a different signal — the tile board.
///
/// That is still the signal worth having, because `WaitPhase` is exactly the state a run gets
/// stranded in: resuming a save drops us straight into it with no overworld dump to be had, and the
/// run then fails looking for a map. See [`crate::fight::Fight::run`], which is built to join a
/// fight already in progress.
///
/// ## `Give up` is the SAME BUTTON, not a neighbouring one
///
/// This is worse than "shares a slot". `rpg.lua:592-597` is a single `ui.elements.button` whose
/// label is a function:
///
/// ```lua
/// return gameover and 'Eulogise'
///     or (rpgview.getPlayerHealth() or 0) <= 0 and (rpgview.fixedEnemiesRemaining() or 1)>0
///        and 'Give up'
///     or 'Finish'
/// ```
///
/// Same object, same plank artwork, same position, same size. Only the glyphs differ — so every
/// pixel outside the lettering matches perfectly, and the inlier fraction cannot fall below the
/// share of the crop the text occupies. Pressing it while it reads `Give up` eulogises the run;
/// while it reads `Eulogise`, likewise.
///
/// ## Measured, against the whole saved-frame corpus
///
/// ```text
///   Finish, spike-frames-live/now.png              1.0000  err 0.0000
///   Finish, spike-frames-live/combat-stalled.png   1.0000  err 0.0000   independent, 2 days apart
///   'Adventure!' plank, 16-selected.png            0.7843  err 0.0769   <- nearest confusable
///   Finish under a hover tooltip, waitphase-*.png  0.5794  err 0.1718
///   plain overworld terrain, overworld-campfire    0.5653  err 0.1646   <- no button at all
/// ```
///
/// Two things that measurement settles, neither of which was visible from the old negative controls
/// (a reward screen at 0.2777, a village map at 0.1080):
///
/// 1. **A different word on the same style of plank scores 0.7843.** That is the empirical stand-in
///    for `Give up`, and the old 0.55 sat *below* it — the threshold could not have separated them.
/// 2. **Flat brown map terrain scores 0.5653**, also above 0.55. The template is a low-contrast
///    wooden plank and `INLIER_TOLERANCE` is 90 across three channels, about 30 each, which brown
///    ground clears against brown wood. The village map measured 0.1080 only because that particular
///    map was dark green.
///
/// A true `Finish` scores **1.0000 with zero error, reproducibly, on independent captures**. So the
/// threshold belongs just under that, not just over the noise.
pub const COMBAT_FINISH: Button = Button {
    name: "combat Finish",
    template: "combat-finish.png",
    search: (1570, 914, 1586, 930),
    click: (1728, 972),
};

/// `Finish` is on screen, so a fight is sitting in `WaitPhase`. See [`COMBAT_FINISH`].
///
/// **0.90, not 0.55.** Deliberately high, because the errors are not symmetric: failing to see a
/// `Finish` costs one loop iteration, while mistaking `Give up` for it eulogises the run. Sits 0.10
/// below both measured true positives and 0.12 above the nearest confusable.
///
/// It also rejects a `Finish` occluded by a hover tooltip (0.5794). That is intended and nearly free
/// — the tooltip only renders under the cursor, and every reader parks at `NEUTRAL` first.
///
/// ## This threshold is not the real guarantee
///
/// `Give up` has never been captured — the run has never reached zero health — so 0.7843 is a proxy
/// from a different button, not a measurement of the thing itself. The guarantee comes from the
/// source instead: **both** paths that render `Give up` require `getPlayerHealth() <= 0`
/// (`rpg.lua:576` and `:594`), and `Eulogise` requires `gameover`. With health above zero this slot
/// can only say `Finish`. Gate the press on health, and the fingerprint stops being load-bearing.
pub const COMBAT_FINISH_PRESENT: f64 = 0.90;

/// The reward screen's `Confirm`, captured in its **greyed** state.
///
/// `ui/itemselection.lua:272` — `button('Confirm', 0.5, 0.85, { xOffset = 2.768, activeIf =
/// function() return selection end })`, a `default` button (250x100), giving centre (1652, 918).
///
/// ## Why a captured crop, when [`crate::observe::affirm`] insists on shipped artwork
///
/// The `default` type ships as `button-*.jpg` — only the arrow and tab types are PNG
/// (`ui/elements/button.lua:16-28`) — and a JPEG has no alpha, so matching one scores the backdrop
/// it was authored against along with the button.
///
/// Here that objection does not apply, and specifically so: `fancyboard.png` is drawn at the same
/// 0.5/0.85 anchor immediately before it (`:271`), and the icon overlay is `getIcon(selection)`,
/// which does not exist until something is chosen. The greyed Confirm is therefore the same pixels
/// on every reward screen — a capture of it is the composition the game always draws, not whatever
/// happened to be behind it once.
///
/// ## Two questions, two thresholds
///
/// Measured across the whole saved-frame corpus, not just the four frames this started with:
///
/// ```text
///   greyed Confirm, post-crypt.png                 1.0000  err 0.0000
///   plain overworld terrain, overworld-campfire    0.7709  err 0.1143   <- no screen at all
///   ACTIVE Confirm, reward-selected.png            0.7255  err 0.0967
///   a village crossing, crossing-stall.png         0.5965  err 0.1180
///   dark-green village map, same slot              0.0068
///   combat screen, same slot                       0.1196
/// ```
///
/// **Read the second and third rows together.** Blank brown map ground scores *higher* against this
/// template than a real reward screen with a selection made. That is an inversion, and it means no
/// single inlier threshold can both accept every true screen and reject terrain — the ordering
/// itself is wrong, so there is no cut point to find.
///
/// The cause is the same one that bit [`COMBAT_FINISH`]: the crop is a low-contrast wooden plank and
/// `INLIER_TOLERANCE` is 90 summed over three channels, roughly 30 each, which brown ground clears
/// against brown wood. The original negative controls hid it because both were dark screens — a
/// green village map and a combat backdrop. The confusable was never another *screen*; it was the
/// ground.
///
/// Two questions, and only the first can be answered from this template:
///
/// - **[`REWARD_SCREEN_PRESENT`]** — is a reward screen up at all? Answerable, but only for the
///   greyed state, and only well above the terrain score.
/// - **[`REWARD_NOTHING_PICKED`]** — is it still greyed, i.e. did our click register? Unaffected:
///   it is asked *while the screen is known to be up*, where terrain is not among the candidates.
pub const REWARD_CONFIRM: Button = Button {
    name: "reward Confirm",
    template: "reward-confirm-inactive.png",
    search: (1519, 860, 1535, 876),
    click: (1652, 918),
};

/// A reward screen is on screen. See [`REWARD_CONFIRM`] for why this is 0.90 and not 0.55.
///
/// At 0.55 this fired on plain overworld ground (0.7709). That is not a near miss: the loop asks it
/// every iteration, and a `true` sends the run into [`crate::itemchoice::choose`], which finds no
/// offers in the feed and reports a screen it cannot clear. A blank patch of map would have ended
/// the run.
///
/// **Narrowed on purpose: this now means "up and untouched", not "up in either state".** The greyed
/// screen measures 1.0000 and terrain 0.7709, so 0.90 separates them; the *active* state at 0.7255
/// is below the cut and is no longer recognised here. That is the right trade for the one caller
/// this has — the loop asks on arrival, before anything has been picked, and
/// [`crate::itemchoice::choose`] confirms its own selection through
/// [`REWARD_NOTHING_PICKED`] and leaves via Space within the same call. A screen with a selection
/// already made is not a state the run produces, and closing the game at one rewinds it to
/// `WaitPhase` rather than restoring it.
///
/// Still worth replacing with a signal that is not brown: the `Choose one:` heading is dark text on
/// light board and would not have this failure mode. That needs a capture, so it waits for the game.
pub const REWARD_SCREEN_PRESENT: f64 = 0.90;

/// Still greyed, so nothing has been selected yet. Placed midway between the measured 0.7255 for the
/// active button and 1.0000 for the greyed one.
pub const REWARD_NOTHING_PICKED: f64 = 0.86;

/// Start menu `Continue`. Measured on 52.3 at 1920x1080; `Restart` is the adjacent button at
/// x≈500 and eulogises the run, which is exactly why this is verified rather than assumed.
pub const CONTINUE: Button = Button {
    name: "Continue",
    template: "continue-button.png",
    search: (60, 745, 350, 880),
    click: (190, 812),
};

/// The bottom-right plaque that advances a cutscene or dialogue.
pub const PROGRESS: Button = Button {
    name: "Progress",
    template: "progress-button.png",
    search: (1760, 865, 1920, 1055),
    click: (1855, 960),
};

/// Below this, refuse. A correct button face on a settled screen matches ~1.000; the live cutscene
/// run measured 1.000, 1.000, 0.987, 1.000 and then 0.599 once the screen changed to something else.
/// 0.85 sits well clear of that 0.599.
pub const MIN_INLIERS: f64 = 0.85;

fn template_path(name: &str) -> PathBuf {
    Path::new("templates").join(name)
}

/// Is this button on screen right now? Returns its match quality.
///
/// `Ok(None)` means "not present", which is ordinary and actionable — it is how a caller detects
/// that a screen has moved on. An `Err` means the template could not be loaded or the screen could
/// not be captured, which is a fault.
pub fn locate(win: &GameWindow, button: &Button) -> Result<Option<f64>, crate::Error> {
    // Cached across calls. `click_when_ready` polls four times a second, and re-reading and
    // re-decoding the same PNG from disk each time is pure waste -- templates never change during a
    // run.
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<&'static str, Template>>> =
        std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let tpl = {
        let mut map = cache.lock().unwrap();
        if !map.contains_key(button.template) {
            let t = Template::load(&template_path(button.template))
                .map_err(|e| crate::Error::Config(format!("template {}: {e}", button.template)))?;
            map.insert(button.template, t);
        }
        map[button.template].clone()
    };
    // Capture only the search box, not the whole window.
    //
    // The buttons this locates do not move, and `click_when_ready` polls four times a second while
    // the game loads. Grabbing 1920x1080 to look at a 290x135 corner of it is ~50x the pixels for
    // the same answer. Nothing needs translating back into window space because this returns the
    // match's QUALITY and never its position.
    let (x0, y0, x1, y1) = button.search;
    let frame = crate::win::capture::capture_client_rect(win, x0, y0, x1 - x0, y1 - y0)?;
    Ok(find_at_scale_in(&frame, &tpl, 1.0, 1, None)
        .filter(|m| m.inliers >= MIN_INLIERS)
        .map(|m| m.inliers))
}

/// Clicks the button, but **only** if it is verifiably there.
///
/// Returns the match quality on success. Errors rather than clicking anything when the button is not
/// recognised: with `Restart` one button away and `Fight on!` sharing a slot with `Hint`, clicking on
/// an unverified guess is not a recoverable mistake.
pub fn click(win: &GameWindow, button: &Button) -> Result<f64, crate::Error> {
    let Some(inliers) = locate(win, button)? else {
        return Err(crate::Error::Win32(format!(
            "refusing to click {}: not confidently recognised in {:?}. The layout may have changed, \
             or the screen is not the one expected.",
            button.name, button.search
        )));
    };
    let (sx, sy) = win.client_to_screen(button.click.0, button.click.1)?;
    // One batched, position-carrying, game-checked click. The old warp -> sleep(250) -> positionless
    // click was a race this module's own primitive was written to remove: anything that moved the
    // cursor inside that gap - the user, or the game's hotspot navigation, which warps the real OS
    // cursor - sent a hardware-level click somewhere else entirely.
    crate::win::input::click_at_in(win, sx, sy)?;
    Ok(inliers)
}

/// Waits for the button to appear, then clicks it.
///
/// This is the primitive callers should reach for. A fixed sleep before clicking is not equivalent:
/// the first crypt run slept 3 s after the window appeared, matched against a menu that had not
/// rendered yet, and refused — correctly, but for a reason that looked like a layout change. The
/// screenshot taken moments later showed `Continue` exactly where expected, and an offline match on
/// that frame scored 1.0000.
///
/// So the distinction that matters is **"not there yet" versus "not there"**, and only waiting can
/// tell them apart. Returns the match quality, or errors if the button never appears.
pub fn click_when_ready(
    win: &GameWindow,
    button: &Button,
    timeout: std::time::Duration,
) -> Result<f64, crate::Error> {
    let deadline = std::time::Instant::now() + timeout;
    let started = std::time::Instant::now();
    let mut best = 0.0f64;
    let mut attempts = 0usize;
    let mut spent_looking = std::time::Duration::ZERO;
    loop {
        let probe = std::time::Instant::now();
        let result = locate(win, button);
        spent_looking += probe.elapsed();
        attempts += 1;
        match result {
            Ok(Some(inliers)) => {
                best = inliers;
                // Separates the two explanations for a slow start: a button that took a long time to
                // APPEAR, versus a check that is slow to run. Guessing between them once already
                // produced a fix for the wrong one.
                eprintln!(
                    "{} found after {:?} over {attempts} attempts ({:?} of that inside locate)",
                    button.name,
                    started.elapsed(),
                    spent_looking
                );
                break;
            }
            Ok(None) => {}
            // A capture can legitimately fail while the window is still coming up.
            Err(e) if std::time::Instant::now() >= deadline => return Err(e),
            Err(_) => {}
        }
        if std::time::Instant::now() >= deadline {
            return Err(crate::Error::Win32(format!(
                "{} never appeared within {timeout:?} (searched {:?})",
                button.name, button.search
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
    let (sx, sy) = win.client_to_screen(button.click.0, button.click.1)?;
    // One batched, position-carrying, game-checked click. The old warp -> sleep(250) -> positionless
    // click was a race this module's own primitive was written to remove: anything that moved the
    // cursor inside that gap - the user, or the game's hotspot navigation, which warps the real OS
    // cursor - sent a hardware-level click somewhere else entirely.
    crate::win::input::click_at_in(win, sx, sy)?;
    Ok(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continue_and_restart_do_not_share_a_search_box() {
        // Restart's face is centred near x=500 on 52.3. If CONTINUE's search box reached it, a
        // match could land on the wrong button and the click point would still be Continue's --
        // giving false confidence rather than a refusal.
        const RESTART_X: i32 = 500;
        assert!(
            CONTINUE.search.2 < RESTART_X - 100,
            "CONTINUE search box must stop well short of Restart, got {:?}",
            CONTINUE.search
        );
    }

    #[test]
    fn click_points_lie_inside_their_search_boxes() {
        for b in [&CONTINUE, &PROGRESS] {
            let (x0, y0, x1, y1) = b.search;
            assert!(
                b.click.0 >= x0 && b.click.0 <= x1 && b.click.1 >= y0 && b.click.1 <= y1,
                "{} click point {:?} outside search box {:?}",
                b.name,
                b.click,
                b.search
            );
        }
    }

    #[test]
    fn the_forbidden_slot_is_not_reachable_through_any_button() {
        // Normalized (0.72, 0.9) is `Hint` or `Fight on!` depending on state, and Fight on commits
        // to another enemy. No Button may target it. At 1920x1080 that is about (1382, 972).
        const FORBIDDEN: (i32, i32) = (1382, 972);
        for b in [&CONTINUE, &PROGRESS] {
            let d = ((b.click.0 - FORBIDDEN.0).pow(2) + (b.click.1 - FORBIDDEN.1).pow(2)) as f64;
            assert!(d.sqrt() > 120.0, "{} clicks too close to the Fight on! slot", b.name);
        }
    }
}

/// Threshold regression tests, run against saved frames rather than a live game.
///
/// These pin the numbers that the doc comments above quote. Both thresholds were once set from
/// negative controls that happened to be dark screens, and both turned out to admit plain brown map
/// ground — a failure invisible until something scored the templates against *every* frame on disk
/// instead of the handful that motivated them.
///
/// The frames are gitignored (`/spike-frames*/`), so these skip where they are absent, as the
/// game-source tests do. The templates themselves are tracked, so a template edit still gets checked
/// wherever a corpus exists.
#[cfg(test)]
mod threshold_tests {
    use super::*;
    use crate::win::capture::Frame;

    fn frame(name: &str) -> Option<Frame> {
        let path = PathBuf::from("spike-frames-live").join(name);
        let dec = png::Decoder::new(std::fs::File::open(path).ok()?);
        let mut rdr = dec.read_info().ok()?;
        let mut buf = vec![0; rdr.output_buffer_size()];
        let info = rdr.next_frame(&mut buf).ok()?;
        let n = info.color_type.samples();
        // The same BGRA layout `capture_window` produces, so this exercises the live comparison.
        let mut bgra = Vec::with_capacity((info.width * info.height * 4) as usize);
        for px in buf.chunks_exact(n) {
            bgra.extend_from_slice(&[px[2], px[1], px[0], 255]);
        }
        Some(Frame { width: info.width as i32, height: info.height as i32, bgra })
    }

    /// Scores a button's template against a saved frame, in its own search box.
    fn score(button: &Button, name: &str) -> Option<f64> {
        let f = frame(name)?;
        let tpl = Template::load(&PathBuf::from("templates").join(button.template)).ok()?;
        find_at_scale_in(&f, &tpl, 1.0, 1, Some(button.search)).map(|m| m.inliers)
    }

    #[test]
    fn finish_is_told_apart_from_another_plank_and_from_bare_ground() {
        let Some(real) = score(&COMBAT_FINISH, "now.png") else {
            eprintln!("SKIP: frame corpus not present");
            return;
        };
        // Two independent captures two days apart, both exact. The threshold sits under these.
        assert!(real >= COMBAT_FINISH_PRESENT, "a real Finish scored {real:.4}");
        let older = score(&COMBAT_FINISH, "combat-stalled.png").unwrap();
        assert!(older >= COMBAT_FINISH_PRESENT, "an older real Finish scored {older:.4}");

        // The nearest confusable available: a wooden plank button reading `Adventure!`. `Give up` is
        // the *same button object* as Finish (`rpg.lua:592-597`) so it would score at least this
        // well, and pressing it eulogises the run.
        let other_plank = score(&COMBAT_FINISH, "16-selected.png").unwrap();
        assert!(
            other_plank < COMBAT_FINISH_PRESENT,
            "a different word on a plank scored {other_plank:.4}, at or above the threshold"
        );

        // Blank brown map, no button in the slot at all.
        let ground = score(&COMBAT_FINISH, "overworld-campfire.png").unwrap();
        assert!(ground < COMBAT_FINISH_PRESENT, "bare map ground scored {ground:.4}");
    }

    #[test]
    fn a_reward_screen_is_told_apart_from_bare_ground() {
        let Some(real) = score(&REWARD_CONFIRM, "post-crypt.png") else {
            eprintln!("SKIP: frame corpus not present");
            return;
        };
        assert!(real >= REWARD_SCREEN_PRESENT, "a real greyed Confirm scored {real:.4}");
        let ground = score(&REWARD_CONFIRM, "overworld-campfire.png").unwrap();
        assert!(ground < REWARD_SCREEN_PRESENT, "bare map ground scored {ground:.4}");
    }

    /// Pins the inversion itself, because it is the reason [`REWARD_SCREEN_PRESENT`] cannot simply be
    /// tuned down again: bare ground scores **higher** than a real reward screen whose item has been
    /// selected. Anyone lowering the threshold to catch the active state will re-admit the map.
    #[test]
    fn bare_ground_outranks_a_selected_reward_screen() {
        let Some(ground) = score(&REWARD_CONFIRM, "overworld-campfire.png") else {
            eprintln!("SKIP: frame corpus not present");
            return;
        };
        let selected = score(&REWARD_CONFIRM, "reward-selected.png").unwrap();
        assert!(
            ground > selected,
            "expected the inversion to still hold: ground {ground:.4} vs selected {selected:.4}. \
             If this now fails, the template or the metric changed and REWARD_SCREEN_PRESENT can \
             be reconsidered."
        );
    }
}
