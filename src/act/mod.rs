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

mod buttons;
pub use buttons::*;

#[cfg(test)]
mod threshold_tests;

/// Does the bottom-right slot read `Eulogise` rather than `Finish`?
///
/// Compares both templates and takes the better match, rather than asking one of them a yes/no
/// question. Pressing this slot while it says `Eulogise` ends the run, so the useful form is "which
/// word is it", not "is it probably Finish".
///
/// Errs toward **yes** on a capture fault: a slot we could not read is not one to press.
pub fn slot_is_eulogise(win: &GameWindow) -> bool {
    let eulogise = score_exact(win, &COMBAT_EULOGISE);
    let finish = score_exact(win, &COMBAT_FINISH);
    match (eulogise, finish) {
        (Ok(e), Ok(f)) => e > f && e >= COMBAT_EULOGISE_PRESENT,
        _ => true,
    }
}

/// Which screen the game is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// A fight sitting in `WaitPhase`, waiting for `Finish`.
    CombatWaiting,
    /// A fight in progress on its **first turn**, recognised by the HUD rather than by any button.
    ///
    /// Narrow on purpose. [`COMBAT_HUD`] carries the turn numeral, so this fires on entry and not
    /// afterwards — it answers "we have just become a fight", which is the transition a run can
    /// otherwise miss entirely. A later turn falls through to [`Screen::Unknown`] exactly as before,
    /// so nothing that worked before behaves differently.
    CombatEntered,
    /// The character is dead; that slot reads `Eulogise`.
    Dead,
    /// A "Choose one:" item screen, untouched.
    ItemChoice,
    /// The postgame stats screen.
    Postgame,
    /// The character/inventory screen — a dead end for the navigator.
    Character,
    /// Hero select, recognised by its heading rather than its button. See [`HEROSELECT_HEADER`].
    HeroSelect,
    /// The main menu, offering a new run.
    MainMenu,
    /// The combat pregame, waiting for `Start`.
    Pregame,
    /// The stats history page — a dead end with no map, reachable by accident from a shrine.
    StatsHistory,
    /// A shrine screen with a live `Consecrate` waiting to be pressed. See [`SHRINE_CONSECRATE`].
    ///
    /// Unlike [`Screen::Shrine`] this one means what it says: it is matched on the button's own
    /// artwork, so it identifies both the screen and the opportunity.
    ShrineConsecrate,
    /// A shrine's word screen, identified only by its back plaque. See [`SHRINE_GOBACK`].
    Shrine,
    /// A general store, open for business. See [`SHOP_SELL`].
    Shop,
    /// A class-unlock announcement, which can interrupt a run after a fight. See [`UNLOCK_CONTINUE`].
    Unlock,
    /// None of the above. Usually the map, but also any screen with no fingerprint yet.
    Unknown,
}

impl Screen {
    /// Every variant, so callers can ask a question of all of them.
    ///
    /// Rust cannot enumerate an enum, so this is hand-written and could fall out of step. What keeps
    /// it honest is [`crate::navigate::answer_for`], whose exhaustive `match` will not compile until
    /// a newly added variant is given an answer — and whose test then checks that answer against this
    /// list. So the compiler catches the variant, and the test catches the omission from here.
    pub const ALL: &'static [Screen] = &[
        Screen::CombatWaiting,
        Screen::CombatEntered,
        Screen::Dead,
        Screen::ItemChoice,
        Screen::Postgame,
        Screen::Character,
        Screen::HeroSelect,
        Screen::MainMenu,
        Screen::Pregame,
        Screen::StatsHistory,
        Screen::ShrineConsecrate,
        Screen::Shrine,
        Screen::Shop,
        Screen::Unlock,
        Screen::Unknown,
    ];
}

/// Asks the screen what it is, before anything acts on an assumption about it.
///
/// The navigator assumes "map" and discovers otherwise several steps later, by failing. Four
/// `Absent` locate-me readings after a cleared crypt were that; so was a run that clicked into the
/// character inventory and spent the rest of its budget there. Both were knowable in one look.
///
/// Cheap enough to run every iteration only because these are [`score_exact`] comparisons — one per
/// candidate at a known offset, not a template sweep. The same check built on [`locate`] would cost
/// thousands of offsets per screen and would not be worth it.
///
/// **Order is by distinctiveness, not by likelihood.** `Dead` is tested before `CombatWaiting`
/// because they share a slot and `Eulogise` must never be mistaken for `Finish`; `Character` is
/// early because it is the one that strands a run.
/// The ordering rule between the two screens that can both claim a frame, split out so a test can
/// assert it without a window. See `tests::the_unlock_screen_is_not_hero_select`.
pub fn identify_from_scores(unlock: bool, hero_select: bool) -> Screen {
    if unlock {
        Screen::Unlock
    } else if hero_select {
        Screen::HeroSelect
    } else {
        Screen::Unknown
    }
}

pub fn identify(win: &GameWindow) -> Screen {
    let over = |b: &Button, t: f64| matches!(score_exact(win, b), Ok(q) if q >= t);
    if over(&CHARACTER_STATS, CHARACTER_STATS_PRESENT) {
        return Screen::Character;
    }
    if slot_is_eulogise(win) {
        return Screen::Dead;
    }
    if over(&COMBAT_FINISH, COMBAT_FINISH_PRESENT) {
        return Screen::CombatWaiting;
    }
    // After the two combat states that name a *button*, because those are more actionable — a
    // `WaitPhase` on turn 1 satisfies both, and `Finish` is the thing worth pressing.
    //
    // Placed this early despite being a HUD read rather than a button, because it is the most
    // distinctive fingerprint in the registry: 0.484 between the combat floor and the non-combat
    // ceiling, where the rest of this function works with margins near 0.1. It is also the only one
    // that survives the hurt vignette, which is precisely when the checks above stop working.
    if over(&COMBAT_HUD, COMBAT_HUD_PRESENT) {
        return Screen::CombatEntered;
    }
    // Both states of the one button. The hot one is not a rare edge: the game lights whatever it
    // put the pointer on, and it puts the pointer on `Start` every time this screen opens.
    if over(&PREGAME_START, PREGAME_START_PRESENT)
        || over(&PREGAME_START_HOT, PREGAME_START_PRESENT)
    {
        return Screen::Pregame;
    }
    if over(&REWARD_CONFIRM, REWARD_SCREEN_PRESENT) {
        return Screen::ItemChoice;
    }
    if over(&POSTGAME_CONTINUE, POSTGAME_CONTINUE_PRESENT) {
        return Screen::Postgame;
    }
    // `CONTINUE_HOT` is asked as well, or the menu we reach by the skip's own route — highlighted,
    // because we came through the options menu — is not recognised as the main menu at all.
    //
    // Asked **exactly**, not searched, because the position is known — see below.
    //
    // A correction, recorded because the wrong version of it was committed first: a run that logged
    // `screen: MainMenu` and then refused `Continue` at 0.3223 was blamed on searched scoring
    // matching a combat screen's statistics panel. Measurement says otherwise. On that exact frame
    // (`tests/frames/combat-chest.png`) all three menu buttons score far under their thresholds —
    // `Continue` 0.1509 searched / 0.1447 exact, `CONTINUE_HOT` 0.1080, `Start` 0.0984 — so
    // `identify` cannot have reached `MainMenu` from it. What it saw was the real main menu still
    // fading out after the launch click, which is also what 0.3223 is: a half-transparent button.
    //
    // The exact test stands on its own evidence rather than on that story. Searched scoring reports
    // the best of 289 candidate offsets, and a maximum over background inflates: `Progress` gains
    // 0.2062 on a death screen, `SHRINE_PRAY` 0.1266. On a button that is genuinely present the
    // sweep adds nothing at all — it is already sitting at the argmax — so five of five measurable
    // buttons score identically both ways. Free tightening, measured, not assumed.
    //
    // All three belong here. The main menu is a fixed layout: `Start`, `Continue` and `Restart` are
    // `default` 250x100 buttons at coordinates derived from the client size, so their positions are
    // *known*, not to be discovered. Each still carries a `search` box, but it is only ever the
    // origin grown by 8 — a vestige of when these were hand-crops with guessed origins and had to be
    // hunted for. Searching a slack box for a button whose address we can compute buys nothing and
    // costs exactly the false positive above.
    let exactly = |b: &Button, t: f64| matches!(score_exact(win, b), Ok(q) if q >= t);
    if exactly(&MENU_START, MENU_START_PRESENT)
        || exactly(&CONTINUE, CONTINUE_PRESENT)
        || exactly(&CONTINUE_HOT, CONTINUE_PRESENT)
    {
        return Screen::MainMenu;
    }
    // Last, because its slot is the options button's on the overworld and the two are only 0.31
    // apart. Anything with a fingerprint of its own should win before this is asked at all — the map
    // itself has none, but every screen tested above does, so reaching here means "not one of those",
    // which is exactly the condition under which the reading is trustworthy.
    if over(&STATS_BACK, STATS_BACK_PRESENT) {
        return Screen::StatsHistory;
    }
    // Shares `ss(1, 0.9)` with three other buttons and clears them by only 0.07, so everything with
    // a fingerprint elsewhere on the screen has been ruled out before this is asked.
    if over(&UNLOCK_CONTINUE, UNLOCK_CONTINUE_PRESENT) {
        return Screen::Unlock;
    }

    // **Hero select goes last, and is the weakest reading here.**
    //
    // It used to sit above the three checks before this one, on the reasoning that a heading is more
    // distinctive than a shared button slot. That is true in general and was false in the case that
    // mattered: the class-unlock screen scores **0.8532** against this heading, over its 0.80 bar,
    // while the unlock's own `Continue` scores **1.0000** against its 0.90 bar. Ordered the old way
    // the weaker, wrong answer won. `tests/frames/unlock-woodsman.png` is that frame — *The Woodsman
    // class is now available*, met mid-run after a road event — and every live run today reported
    // `screen: HeroSelect` on it and then failed hunting for a map.
    //
    // Demoting it costs nothing, which is the part worth stating. Hero select is reached in exactly
    // one way — [`crate::navigate::start_new_run`] clicks through it, knowing it is there because
    // `Pregame screen:` follows on the console — so it is **never identified by sight** anyway. The
    // check earns nothing where it stood and cost a run every time it fired.
    //
    // Kept rather than deleted because a run that somehow lands here needs a name for what it is
    // looking at, and by this point everything with a real fingerprint has been excluded.
    //
    // By the heading, never by the confirm button. [`HEROSELECT_CONFIRM`] answers "has a champion
    // been chosen" on a screen already known to be hero select; it cannot answer "which screen is
    // this", because `Pray` scores 0.8438 against it while genuine confirms in other classes score
    // 0.8156 and 0.8751 — no threshold separates them. See [`HEROSELECT_HEADER`].
    if over(&HEROSELECT_HEADER, HEROSELECT_HEADER_PRESENT) {
        return Screen::HeroSelect;
    }

    // **Dead last, on purpose: this is a generic "take me back" matcher, not a screen.**
    //
    // **The shrine and the inn share this plaque.** Both declare it at `ss(0, 0.9)` with the same
    // `back.png` — `ui/inn.lua:68-71` and `ui/rest.lua:517-520` against the shrine's — and the two
    // click points are (112, 972) and (113, 972), a pixel apart because `100*1.13` truncates. They
    // are the same artwork in the same slot, so nothing can tell them apart here, and half the
    // game's other screens are built the same way.
    //
    // So it is placed last deliberately, below everything that answers *which screen is this*. What
    // a match means is only: **nothing above recognised this, and there is a way back from it** —
    // which is the whole intent. When there is nothing else to do, return where you came from.
    //
    // It used to sit above [`Screen::Unlock`] and [`Screen::HeroSelect`], and that was wrong in the
    // same way hero select being high was wrong: a check that answers "is there a back plaque" was
    // pre-empting two that answer "which screen is this". Live 2026-08-09 it fired on an **inn** and
    // the run logged `screen: Shrine` and `left the shrine screen`. Pressing back was the right
    // move; the label was fiction.
    //
    // The variant is still named `Shrine`, which is a worse name than the check deserves. Nothing
    // may read it as evidence that a shrine is underfoot — `WorldMap::worth_consecrating_here`
    // answers that from the save and should stay the only thing that does.
    // **Above the back plaque, and that ordering is the whole point.**
    //
    // Both live in the same fall-through region, and the plaque wins on nothing but position. Live
    // 2026-08-12 a fight at a corrupted shrine ended leaving the shrine screen up with `Consecrate`
    // lit in the right-hand slot, and the classifier walked past it to "there is a way back from
    // here" and pressed back. It saw the exit and not the offer.
    //
    // This is the same shape as the note on [`SHRINE_GOBACK`] below: a check that answers *is there
    // a back plaque* must never pre-empt one that answers *what is this screen for*.
    if over(&SHRINE_CONSECRATE, SHRINE_CONSECRATE_PRESENT) {
        return Screen::ShrineConsecrate;
    }
    // Asked before the generic back-plaque checks below, for the reason written above them: a shop
    // has its own back arrow, and a test that answers "there is a way out of here" must never
    // pre-empt one that answers "what is this screen for".
    if over(&SHOP_SELL, SHOP_SELL_PRESENT) {
        return Screen::Shop;
    }
    if over(&SHRINE_GOBACK, SHRINE_GOBACK_PRESENT) {
        return Screen::Shrine;
    }
    Screen::Unknown
}

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
    let frame =
        crate::win::capture::capture_client_rect(win, ox, oy, tpl.width as i32, tpl.height as i32)?;
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
        std::thread::sleep(crate::timing::POLL_BUTTON);
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

/// Where the first choice plaque is, and how well it matched.
///
/// `options` is how many choices the console listed, which is what fixes the vertical band — see
/// [`event_choice_search`]. The centre is in **client** pixels.
///
/// ## The position is the point, not a by-product
///
/// The first version of this returned only a score, and the click went to a coordinate derived from
/// the console's `posX`. That coordinate was wrong — see [`crate::observe::event::Choice::click_point`]
/// — and the derivation needed to know whether the event had a portrait, which nothing on the console
/// says.
///
/// None of that inference is necessary. **The template match already knows where the button is**: it
/// is a picture of the plaque, so finding it is finding the plaque. Reading `cx`/`cy` off the match
/// closes the loop the same way every other press in this project is closed — the thing that confirms
/// the control is present is the same thing that says where to press it. No layout arithmetic, no
/// portrait detection, and it cannot drift from the game's own positioning because it is measured
/// from the pixels the game drew.
///
/// The centre is offset by the crop's own padding, which is the one piece of bookkeeping left: the
/// template is the plaque plus 15 px on every side, so its centre and the plaque's centre coincide.
pub fn event_plaque_find(
    win: &GameWindow, options: usize,
) -> Result<Option<crate::observe::template::Match>, crate::Error> {
    let tpl = cached_template(&EVENT_CHOICE)?;
    let (x0, y0, x1, y1) = event_choice_search(options);
    let frame = crate::win::capture::capture_client_rect(win, x0, y0, x1 - x0, y1 - y0)?;
    // `find_at_scale_in` reports inside the cropped frame, so the box origin goes back on.
    Ok(find_at_scale_in(&frame, &tpl, 1.0, 1, None).map(|mut m| {
        m.x += x0;
        m.y += y0;
        m.cx += x0;
        m.cy += y0;
        m
    }))
}

/// Just the score, for callers that only need to know whether the plaque is there.
pub fn event_plaque_score(win: &GameWindow, options: usize) -> Result<f64, crate::Error> {
    Ok(event_plaque_find(win, options)?.map(|m| m.inliers).unwrap_or(0.0))
}

/// Verifies a button at its known origin, then clicks it — no search.
///
/// [`click`] goes through [`locate`], which sweeps the template across a slack region. For a button
/// whose position is exact that is waste, and it is the same correction already applied to
/// detection: one comparison, not hundreds.
///
/// **Returns the score; it does not prove the press did anything.** That distinction has cost this
/// project three separate bugs in a day. `Start` was clicked at 1.0000 and the game stayed on the
/// main menu, so every later click landed on menu background. Callers that need the press to have
/// *worked* must watch for the consequence themselves — see [`wait_until_gone`].
pub fn click_exact(win: &GameWindow, button: &Button, threshold: f64) -> Result<f64, crate::Error> {
    let q = score_exact(win, button)?;
    if q < threshold {
        return Err(crate::Error::Win32(format!(
            "refusing to click {}: scored {q:.4}, below {threshold:.2}. The layout may have changed, \
             or this is not the screen we think it is.",
            button.name
        )));
    }
    let (sx, sy) = win.client_to_screen(button.click.0, button.click.1)?;
    crate::win::input::click_at_in(win, sx, sy)?;
    Ok(q)
}

/// Waits until a button STOPS matching — i.e. the screen it belongs to has gone.
///
/// The read-back for a click that should navigate away. `click_exact` can only say the button was
/// there when we pressed it; this says the press was acted on.
pub fn wait_until_gone(
    win: &GameWindow, button: &Button, threshold: f64, timeout: std::time::Duration,
) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        match score_exact(win, button) {
            Ok(q) if q < threshold => return true,
            _ => {}
        }
        std::thread::sleep(crate::timing::POLL_BUTTON);
    }
    false
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
    win: &GameWindow, button: &Button, timeout: std::time::Duration,
) -> Result<f64, crate::Error> {
    let deadline = std::time::Instant::now() + timeout;
    let started = std::time::Instant::now();
    let mut attempts = 0usize;
    let mut spent_looking = std::time::Duration::ZERO;
    // **The loop yields the score rather than filling in a variable declared above it.** There is no
    // way out of here that is not either a match or an error, so a `best` seeded with 0.0 was a
    // value no caller could ever receive — and a reader had to prove that for themselves.
    let best = loop {
        let probe = std::time::Instant::now();
        let result = locate(win, button);
        spent_looking += probe.elapsed();
        attempts += 1;
        match result {
            Ok(Some(inliers)) => {
                // Separates the two explanations for a slow start: a button that took a long time to
                // APPEAR, versus a check that is slow to run. Guessing between them once already
                // produced a fix for the wrong one.
                eprintln!(
                    "{} found after {:?} over {attempts} attempts ({:?} of that inside locate)",
                    button.name,
                    started.elapsed(),
                    spent_looking
                );
                break inliers;
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
        std::thread::sleep(crate::timing::POLL_LOCATE);
    };
    let (sx, sy) = win.client_to_screen(button.click.0, button.click.1)?;
    // One batched, position-carrying, game-checked click. The old warp -> sleep(250) -> positionless
    // click was a race this module's own primitive was written to remove: anything that moved the
    // cursor inside that gap - the user, or the game's hotspot navigation, which warps the real OS
    // cursor - sent a hardware-level click somewhere else entirely.
    crate::win::input::click_at_in(win, sx, sy)?;
    Ok(best)
}
