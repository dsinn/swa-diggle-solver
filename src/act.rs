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
#[allow(unused_imports)]
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
    ///
    /// **This is a capture rectangle, not an anchor box.** [`locate`] grabs exactly this region and
    /// searches the template *inside* it, so it must be at least as large as the template in both
    /// axes — otherwise there is nowhere for the template to sit and the match can never succeed.
    /// The convention is the button's own rectangle grown by a slack margin on every side.
    ///
    /// Easy to get wrong, and it was: `COMBAT_FINISH` and `REWARD_CONFIRM` were both authored as
    /// `top-left ± 8`, a 16x16 box, which reads perfectly sensibly if you have `diggle findpng` in
    /// mind — its `bounds` argument constrains where the template's **top-left corner** may land, so
    /// 16x16 means "within 8 px of here" and is right. Same numbers, two incompatible meanings, and
    /// every offline measurement went through `findpng` while the live path silently returned
    /// `None` forever. [`tests::every_search_box_can_contain_its_template`] now makes that
    /// impossible to reintroduce.
    pub search: (i32, i32, i32, i32),
    /// The template's exact top-left in client pixels, for [`score_exact`].
    ///
    /// Redundant with `search` only in appearance: `search` is a region grown by slack, this is the
    /// one offset the template can actually occupy. Kept explicit rather than derived so the slack
    /// can change without silently moving where we look.
    pub origin: (i32, i32),
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
///   greyed Finish at 0 health, finish-at-zero-*.png 0.8380 err 0.0743   <- see below
///   Finish under a hover tooltip, waitphase-*.png  0.5794  err 0.1718
///   plain overworld terrain, overworld-campfire    0.5653  err 0.1646   <- no button at all
/// ```
///
/// ## Two questions, and only one of them has a clean answer
///
/// A live run reached 0 health with the fight won. The slot read `Finish`, drawn **inactive**
/// (`activeIf` is `WaitPhase or SmokebombWaitPhase or PlayerTurn`), and scored 0.8380 — while a
/// *different word* on a plank scores 0.7843. Fifty-four thousandths apart, one sample each.
///
/// So "is this word `Finish` or `Give up`" does **not** separate on this template: changing the
/// button's state costs about as much as changing its word, and any threshold between those two
/// numbers would be fitting noise.
///
/// "Is an active `Finish` on screen" separates enormously, though — 0.8380 inactive against 1.0000
/// active, later confirmed at 0.9880 under the red low-health tint. That is the question
/// [`COMBAT_FINISH_ACTIVE`] asks, and it is the one worth asking: an inactive button cannot be
/// clicked anyway, so waiting for an active one costs nothing and yields the strongest reading the
/// template can give.
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
    // The 300x100 button spans (1578,922)-(1878,1022); this is that grown by 8 px of slack.
    search: (1570, 914, 1886, 1030),
    origin: (1578, 922),
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

/// `Finish` is not merely on screen but **drawn active**, i.e. clickable. See [`COMBAT_FINISH`].
///
/// Separate from [`COMBAT_FINISH_PRESENT`] because it answers a harder question and is used for a
/// riskier decision — pressing at zero health, where the wrong label ends the run.
///
/// The two states are far apart, which is what makes this usable: an active plank measured **1.0000**
/// on two independent captures, the greyed one **0.8380**. 0.97 admits only a near-exact match, and
/// a word swap should cost far more than 3% of the crop.
///
/// Confirmed live: a run reached zero health with the area cleared, and the active `Finish` — under the
/// red low-health tint that washes the whole scene — measured **0.9880**. So the bar is clearable in
/// the exact condition it exists for, with margin, and is not merely theoretical.
///
/// The other half is still an estimate. An active `Give up` has never been captured, so the bound
/// on it is arithmetic — roughly 7% of the crop is lettering — rather than a measurement.
/// [`crate::fight::Fight::finish`] logs the best score whenever this bar is missed, so the first run
/// that meets a real `Give up` replaces the estimate with a number.
pub const COMBAT_FINISH_ACTIVE: f64 = 0.97;

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
    // The 250x100 button spans (1527,868)-(1777,968); this is that grown by 8 px of slack.
    search: (1519, 860, 1785, 976),
    origin: (1527, 868),
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

/// The postgame stats screen's `Continue`.
///
/// `ui/postgame.lua:69` — `button('Continue', 1, 0.85, { xOffset = -0.75 })`, a `default` button
/// (250x100), giving centre (1732.5, 918) and a span of x 1607–1857, y 868–968.
///
/// ## Cropped to stop short of the lore arrow
///
/// [`crate::observe::affirm::LORE_AFFIRMATIVE`] is `ss(1.0, 0.9)` with `os_x -0.75`, 64x80 — centre
/// (1872, 972), spanning **x 1840–1904**. The `Continue` plank runs to 1857, so the two overlap by
/// about 17 px, and a naive crop of the whole button would put arrow pixels inside this template and
/// `Continue` pixels inside the arrow's slot. Two readers sharing pixels is how one screen starts
/// answering for another.
///
/// So the template is cut at **x 1830**, ten pixels clear of the arrow, and keeps only the lettering
/// — which is the discriminating part anyway. The plank's wood grain is what every other brown thing
/// on screen also has.
///
/// ## Measured
///
/// ```text
///   the postgame itself           1.0000  err 0.0000
///   plain overworld terrain       0.6798  err 0.1291   <- nearest confusable, as ever
///   a reward screen               0.5337  err 0.1788
///   'Adventure!' plank            0.2953  err 0.2615
///   greyed Finish at 0 health     0.2862  err 0.2130
///   combat WaitPhase              0.2587  err 0.2560
/// ```
///
/// The last two matter because the combat `Finish` plank spans 1578–1878 and so sits right across
/// this region. It scores 0.26 — the lettering is what carries the signal, exactly as intended.
pub const POSTGAME_CONTINUE: Button = Button {
    name: "postgame Continue",
    template: "postgame-continue.png",
    // 220x92 template at (1610,872), grown by 8 — and hard-stopped at 1838, under the arrow's 1840.
    search: (1602, 864, 1838, 972),
    origin: (1610, 872),
    click: (1732, 918),
};

/// The postgame is on screen. See [`POSTGAME_CONTINUE`].
///
/// 0.90, sitting 0.22 above the nearest confusable and 0.10 below the true positive.
pub const POSTGAME_CONTINUE_PRESENT: f64 = 0.90;

/// Every button, so the invariant tests cover all of them.
///
/// Exists because they did not. The geometry tests enumerated `[&CONTINUE, &PROGRESS]` by hand, so
/// the two buttons added later were checked by nothing — including the check that would have caught
/// their search boxes being smaller than their own templates. A hand-written list silently stops
/// covering the thing you just added, which is the moment coverage matters most.
pub const ALL: &[&Button] =
    &[&CONTINUE, &PROGRESS, &COMBAT_FINISH, &REWARD_CONFIRM, &POSTGAME_CONTINUE];

/// Start menu `Continue`. Measured on 52.3 at 1920x1080; `Restart` is the adjacent button at
/// x≈500 and eulogises the run, which is exactly why this is verified rather than assumed.
pub const CONTINUE: Button = Button {
    name: "Continue",
    template: "continue-button.png",
    search: (60, 745, 350, 880),
    origin: (68, 753),
    click: (190, 812),
};

/// The bottom-right plaque that advances a cutscene or dialogue.
pub const PROGRESS: Button = Button {
    name: "Progress",
    template: "progress-button.png",
    search: (1760, 865, 1920, 1055),
    origin: (1768, 873),
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
/// Scores a button where it **is**, with no search at all.
///
/// [`locate`] sweeps the template across a slack region. That is worth paying for while the start
/// menu is still laying itself out, but it is waste for a button whose position is known exactly:
/// these are anchored at fixed normalized coordinates and the client is a fixed 1920x1080, so there
/// is precisely one offset that can ever match. Capturing the template-sized rect at that offset
/// turns a 17x17 sweep into a single comparison.
///
/// The slack was not buying safety either. If the client size ever changed, every `click` point
/// would already be wrong by the same amount, so a match found 8 px away would be a match we must
/// not act on.
///
/// Returns the raw score, so callers can log it. **A number below the threshold is not the same as
/// "the screen has moved on"** — mid-animation everything scores low — which is why the loop guards
/// poll this rather than asking once.
pub fn score_exact(win: &GameWindow, button: &Button) -> Result<f64, crate::Error> {
    let tpl = cached_template(button)?;
    let (ox, oy) = button.origin;
    let frame = crate::win::capture::capture_client_rect(win, ox, oy, tpl.width as i32, tpl.height as i32)?;
    // The frame is exactly template-sized, so there is one candidate offset and this is a direct
    // comparison — reusing the matcher rather than hand-rolling one keeps alpha handling identical.
    Ok(find_at_scale_in(&frame, &tpl, 1.0, 1, None).map(|m| m.inliers).unwrap_or(0.0))
}

/// Polls [`score_exact`] until the button appears, and reports the best score if it never does.
///
/// Single-shot recognition is wrong for anything reached through a transition. The screen spends
/// hundreds of milliseconds fading, sliding and animating, and during that time *every* template
/// scores low — so "did not match" carries no information until the screen has had a chance to
/// settle. A run resumed straight into combat, asked once while the crypt was still fading in, got
/// a low score for a `Finish` that was about to be plainly visible, and fell through to the map
/// path. Three times, with the same misleading "no pan dump after locate-me" each time.
///
/// Returns `Ok(Some(score))` on success and `Ok(None)` on a genuine timeout, with `best` telling the
/// caller how close it came — that is what separates "never rendered" from "nearly, but the
/// threshold is wrong", and the two want opposite fixes.
///
/// Capture faults are counted, not swallowed: `locate` folds them into `Err` and the loop's
/// `matches!` turned both into a plain `false`, which is how a blind check looked exactly like an
/// absent button.
pub fn wait_for(
    win: &GameWindow, button: &Button, threshold: f64, timeout: std::time::Duration,
) -> WaitResult {
    let deadline = std::time::Instant::now() + timeout;
    let mut best = 0.0f64;
    let mut faults = 0usize;
    let mut looks = 0usize;
    loop {
        match score_exact(win, button) {
            Ok(q) => {
                looks += 1;
                best = best.max(q);
                if q >= threshold {
                    return WaitResult { score: Some(q), best: q, looks, faults };
                }
            }
            Err(_) => faults += 1,
        }
        if std::time::Instant::now() >= deadline {
            return WaitResult { score: None, best, looks, faults };
        }
        std::thread::sleep(std::time::Duration::from_millis(120));
    }
}

/// What [`wait_for`] saw. `best` and `faults` exist so a miss is diagnosable from the log alone.
#[derive(Debug, Clone, Copy)]
pub struct WaitResult {
    /// The score that cleared the threshold, if one did.
    pub score: Option<f64>,
    /// The highest score seen, whether or not it cleared.
    pub best: f64,
    /// How many times the screen was read.
    pub looks: usize,
    /// Reads that failed outright. Nonzero means we were blind, not that the button was absent.
    pub faults: usize,
}

impl WaitResult {
    pub fn found(&self) -> bool {
        self.score.is_some()
    }
}

/// Loads a button's template, cached across calls.
fn cached_template(button: &Button) -> Result<Template, crate::Error> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<&'static str, Template>>> =
        std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut map = cache.lock().unwrap();
    if !map.contains_key(button.template) {
        let t = Template::load(&template_path(button.template))
            .map_err(|e| crate::Error::Config(format!("template {}: {e}", button.template)))?;
        map.insert(button.template, t);
    }
    Ok(map[button.template].clone())
}

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

    /// A search box smaller than its own template can never match, and the failure is silent:
    /// [`locate`] returns `Ok(None)`, which every caller reads as the ordinary "not on screen".
    ///
    /// Both `COMBAT_FINISH` and `REWARD_CONFIRM` shipped this way and nothing noticed for four
    /// commits, because every measurement went through `diggle findpng`, whose `bounds` argument
    /// means the opposite thing — where the template's top-left may land, rather than what to
    /// capture. The offline numbers were all correct and all irrelevant.
    #[test]
    fn every_search_box_can_contain_its_template() {
        for b in ALL {
            let tpl = Template::load(&template_path(b.template))
                .unwrap_or_else(|e| panic!("{}: {e}", b.name));
            let (w, h) = (b.search.2 - b.search.0, b.search.3 - b.search.1);
            assert!(
                w >= tpl.width as i32 && h >= tpl.height as i32,
                "{}: search box is {w}x{h} but the template is {}x{} — locate() captures only the \
                 box, so this can never match",
                b.name,
                tpl.width,
                tpl.height
            );
        }
    }

    /// `origin` and `search` describe the same button two ways, so they have to agree: the template
    /// placed at `origin` must sit wholly inside `search`.
    ///
    /// Only `COMBAT_FINISH` and `REWARD_CONFIRM` have origins measured off live frames; the other
    /// two are nominal, since nothing calls [`score_exact`] on them. This check is what would catch
    /// a nominal value being badly wrong if that ever changed.
    #[test]
    fn each_origin_agrees_with_its_search_box() {
        for b in ALL {
            let tpl = Template::load(&template_path(b.template)).unwrap();
            let (ox, oy) = b.origin;
            assert!(
                ox >= b.search.0
                    && oy >= b.search.1
                    && ox + tpl.width as i32 <= b.search.2
                    && oy + tpl.height as i32 <= b.search.3,
                "{}: template at origin {:?} ({}x{}) does not fit inside search {:?}",
                b.name,
                b.origin,
                tpl.width,
                tpl.height,
                b.search
            );
        }
    }

    /// A button that is *not* the bottom-right arrow must not read the arrow's pixels.
    ///
    /// [`crate::observe::affirm::LORE_AFFIRMATIVE`] is `ss(1.0, 0.9)`, `os_x -0.75`, 64x80 — centre
    /// (1872, 972), spanning x 1840–1904, y 932–1012. The postgame `Continue` is a 250x100 plank
    /// running to x 1857, so the two genuinely overlap on screen, and a template cropped to the whole
    /// button would put arrow pixels inside it while putting `Continue` pixels inside the arrow's
    /// slot. Two readers sharing pixels is how one screen starts answering for another.
    /// [`POSTGAME_CONTINUE`] is therefore cut at 1830, ten clear of the arrow.
    ///
    /// Asserted for this button only, rather than as a blanket rule, because the blanket rule is not
    /// true. [`PROGRESS`] clicks (1855, 960) — inside the arrow slot — because it is not a neighbour
    /// of that arrow, it **is** that arrow, read by template instead of by shipped artwork.
    /// [`COMBAT_FINISH`]'s 300x100 crop also runs to 1878, and that overlap is pre-existing,
    /// measured, and harmless in practice: combat WaitPhase and a lore screen are different screens,
    /// so the two elements are never drawn together. Re-cropping it would invalidate every threshold
    /// on it. Recorded as a follow-up rather than fixed here.
    #[test]
    fn the_postgame_continue_stops_clear_of_the_lore_arrow() {
        const ARROW_LEFT: i32 = 1872 - 32; // 1840
        assert!(
            POSTGAME_CONTINUE.search.2 <= ARROW_LEFT,
            "postgame Continue searches to x={}, into the arrow slot at x>={ARROW_LEFT}",
            POSTGAME_CONTINUE.search.2
        );
        // And the template itself, not just the slack, must clear it.
        let tpl = Template::load(&template_path(POSTGAME_CONTINUE.template)).unwrap();
        assert!(POSTGAME_CONTINUE.origin.0 + tpl.width as i32 <= ARROW_LEFT);
    }

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
        for b in ALL {
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
        for b in ALL {
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

    /// Crops a frame to a client rectangle, standing in for `capture_client_rect`.
    fn crop(f: &Frame, (x0, y0, x1, y1): (i32, i32, i32, i32)) -> Frame {
        let (w, h) = (x1 - x0, y1 - y0);
        let mut bgra = Vec::with_capacity((w * h * 4) as usize);
        for y in y0..y1 {
            let row = (y * f.width + x0) as usize * 4;
            bgra.extend_from_slice(&f.bgra[row..row + (w as usize) * 4]);
        }
        Frame { width: w, height: h, bgra }
    }

    /// Scores a button's template against a saved frame **the way [`locate`] does**: capture only
    /// the search box, then search the template inside that crop with no further bounds.
    ///
    /// Deliberately not `find_at_scale_in(whole_frame, .., Some(button.search))`. That is what
    /// `diggle findpng` does, and it treats `search` as an anchor box rather than a capture
    /// rectangle — the exact mismatch that let a 16x16 search box on a 300x100 template measure
    /// perfectly offline while never once matching in the live run. A regression test that measures
    /// through the wrong path is worse than none, because it certifies the bug.
    fn score(button: &Button, name: &str) -> Option<f64> {
        let f = frame(name)?;
        let tpl = Template::load(&PathBuf::from("templates").join(button.template)).ok()?;
        find_at_scale_in(&crop(&f, button.search), &tpl, 1.0, 1, None).map(|m| m.inliers)
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

        // A *differently composed* WaitPhase: `Fight on!` beside it, a lit brazier and an open chest
        // in the scene. Worth its own assertion because the other two scoring exactly 1.0000 was
        // weak evidence — a template scores 1.0000 against the rendering it was cropped from, which
        // says nothing about how the button looks when the screen around it differs. This is the
        // frame the run was actually sitting on when the check failed.
        let composed = score(&COMBAT_FINISH, "waitphase-with-fighton.png").unwrap();
        assert!(
            composed >= COMBAT_FINISH_PRESENT,
            "a live WaitPhase with Fight on! beside it scored {composed:.4}"
        );

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
