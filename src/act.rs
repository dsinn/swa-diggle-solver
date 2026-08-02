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
/// ## The confusable state is NOT measured
///
/// `Give up` shares this slot (see `fight::FINISH`), and pressing it in place of `Finish` abandons a
/// won fight. The crop includes the lettering, so the two should separate far more than a recolour
/// would — but that is reasoning, not measurement, and no capture of `Give up` exists yet. Measure
/// it before this is used to decide anything destructive.
///
/// Measured against every screen captured so far:
///
/// ```text
///   WaitPhase, the frame it came from   1.0000
///   reward screen                       0.2777
///   combat, mid-PlayerTurn              0.2317
///   village map                         0.1080
/// ```
pub const COMBAT_FINISH: Button = Button {
    name: "combat Finish",
    template: "combat-finish.png",
    search: (1570, 914, 1586, 930),
    click: (1728, 972),
};

/// `Finish` is on screen, so a fight is sitting in `WaitPhase`. See [`COMBAT_FINISH`].
pub const COMBAT_FINISH_PRESENT: f64 = 0.55;

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
/// Measured against four live frames:
///
/// ```text
///   greyed Confirm, the frame it was cropped from   1.0000
///   ACTIVE Confirm, after an item was selected      0.7255
///   village map, same slot                          0.0068
///   combat screen, same slot                        0.1196
/// ```
///
/// The active button is the same plank and the same lettering in a different colour, so it scores
/// 0.73 against the greyed template — far too close to call with the 0.55 used elsewhere. That
/// threshold answers a *different* question perfectly well, and the two must not be conflated:
///
/// - **[`REWARD_SCREEN_PRESENT`]** — is a reward screen up at all? Either state clears it and
///   nothing else comes within 6x of it.
/// - **[`REWARD_NOTHING_PICKED`]** — is it still greyed, i.e. has our click on an item registered?
///   Sits in the gap between 0.7255 and 1.0000, and is the read-back for selecting an item.
pub const REWARD_CONFIRM: Button = Button {
    name: "reward Confirm",
    template: "reward-confirm-inactive.png",
    search: (1519, 860, 1535, 876),
    click: (1652, 918),
};

/// A reward screen is on screen, in either state. See [`REWARD_CONFIRM`].
pub const REWARD_SCREEN_PRESENT: f64 = 0.55;

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
