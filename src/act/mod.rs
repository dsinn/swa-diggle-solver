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
            "refusing to click {}: scored {q:.4}, below {threshold:.2}. The layout may have changed,              or this is not the screen we think it is.",
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
        std::thread::sleep(std::time::Duration::from_millis(120));
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
    win: &GameWindow,
    button: &Button,
    timeout: std::time::Duration,
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
        std::thread::sleep(std::time::Duration::from_millis(400));
    };
    let (sx, sy) = win.client_to_screen(button.click.0, button.click.1)?;
    // One batched, position-carrying, game-checked click. The old warp -> sleep(250) -> positionless
    // click was a race this module's own primitive was written to remove: anything that moved the
    // cursor inside that gap - the user, or the game's hotspot navigation, which warps the real OS
    // cursor - sent a hardware-level click somewhere else entirely.
    crate::win::input::click_at_in(win, sx, sy)?;
    Ok(best)
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

    /// Loads a corpus frame.
    ///
    /// `tests/frames`, **not** `spike-frames-live`. These tests used to read the live spike output
    /// directory, which no one had decided on — the captures happened to be there and the tests
    /// reached for them. Runs write that directory under fixed names, so a run can overwrite the
    /// evidence a threshold test rests on, and one did: a stall inside the anomaly replaced
    /// `combat-stalled.png` with a `PlayerTurn` frame containing no `Finish` at all, and the test
    /// failed on a change that had nothing to do with it.
    ///
    /// The corpus is a fixture, so it lives with the tests and is tracked. Frames are captured once
    /// and copied in deliberately; nothing the driver does at runtime can reach them.
    fn frame(name: &str) -> Option<Frame> {
        let path = PathBuf::from("tests").join("frames").join(name);
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

    /// Scores a button the way [`identify`] does: the template-sized rect at `origin`, no sweep.
    ///
    /// The two paths are not interchangeable, and reporting one while the run uses the other is how
    /// a regression test certifies a bug — see [`score`]'s note. `identify` calls [`score_exact`] for
    /// every screen it names, so this is the measurement that speaks to what a run saw.
    fn score_at_origin(button: &Button, name: &str) -> Option<f64> {
        let f = frame(name)?;
        let tpl = Template::load(&PathBuf::from("templates").join(button.template)).ok()?;
        let (ox, oy) = button.origin;
        let rect = (ox, oy, ox + tpl.width as i32, oy + tpl.height as i32);
        find_at_scale_in(&crop(&f, rect), &tpl, 1.0, 1, None).map(|m| m.inliers)
    }

    /// Measures the frame a run stopped on with `Start` plainly on screen.
    ///
    /// 2026-08-14 at `l16sub14`: the run pressed Combat, the screen moved 0.975, and the next look
    /// did not report `Screen::Pregame`. It re-derived the same step, pressed the same coordinate a
    /// second time into a screen where it means nothing (0.011 of movement), and stopped with
    /// `Combat did not open`. `spike-frames-live/gave-up.png` is the pregame, copied here.
    ///
    /// This is here to say which half is at fault, because an absent fingerprint and a fingerprint
    /// nobody asked about produce the same log line and want opposite fixes.
    #[test]
    fn the_pregame_a_run_stopped_on_is_recognisable() {
        let Some(exact) = score_at_origin(&PREGAME_START, "pregame-graveyard.png") else {
            eprintln!("SKIP: frame corpus not present");
            return;
        };
        // Both paths, because they answer different questions and only one is `identify`'s.
        let searched = score(&PREGAME_START, "pregame-graveyard.png").unwrap();
        assert!(
            exact >= PREGAME_START_PRESENT,
            "a resting Start scored {exact:.4} at its origin (searched: {searched:.4}), under the \
             {PREGAME_START_PRESENT:.2} bar"
        );
        eprintln!("PREGAME_START on the give-up frame: exact {exact:.4}, searched {searched:.4}");
        if let Some(e2) = score_at_origin(&PREGAME_START, "pregame-graveyard-2.png") {
            let s2 = score(&PREGAME_START, "pregame-graveyard-2.png").unwrap();
            eprintln!("PREGAME_START on the second give-up frame: exact {e2:.4}, searched {s2:.4}");
        }
        // The resting template must NOT claim a hot button — if it did, the pair would be
        // indistinguishable and the second template pointless.
        for f in ["pregame-hot.png", "pregame-hot-2.png"] {
            let cold = score_at_origin(&PREGAME_START, f).unwrap();
            assert!(cold < PREGAME_START_PRESENT, "{f}: resting template scored {cold:.4} on a hot button");
        }
    }

    /// The highlighted `Start`, on the two frames a run photographed itself failing to read.
    ///
    /// Both are pregames the run could not name — `l14` and `l48_plaza`, 2026-08-14 — and on both
    /// the resting template scored 0.4972. That number is the whole argument for a second template
    /// rather than a lower bar: it sits *under* impostors the same frames carry, so no threshold
    /// separates a hot pregame from a screen that is not a pregame.
    #[test]
    fn a_highlighted_start_is_still_the_pregame() {
        let Some(hot) = score_at_origin(&PREGAME_START_HOT, "pregame-hot.png") else {
            eprintln!("SKIP: frame corpus not present");
            return;
        };
        assert!(hot >= PREGAME_START_PRESENT, "the frame it was cut from scored {hot:.4}");

        // The one that matters: a *different* pregame, at a different location, with a different
        // backdrop. Scoring 1.0000 against its own source frame would prove only that cropping works.
        let other = score_at_origin(&PREGAME_START_HOT, "pregame-hot-2.png").unwrap();
        assert!(
            other >= PREGAME_START_PRESENT,
            "a second hot pregame scored {other:.4}, under the {PREGAME_START_PRESENT:.2} bar"
        );

        // And the hot template must not claim a resting button, or the two would be one.
        for f in ["pregame-graveyard.png", "pregame-graveyard-2.png"] {
            let cold = score_at_origin(&PREGAME_START_HOT, f).unwrap();
            assert!(cold < PREGAME_START_PRESENT, "{f}: hot template scored {cold:.4} on a resting button");
        }
        eprintln!("PREGAME_START_HOT: source {hot:.4}, independent {other:.4}");
    }

    #[test]
    fn finish_is_told_apart_from_another_plank_and_from_bare_ground() {
        let Some(real) = score(&COMBAT_FINISH, "now.png") else {
            eprintln!("SKIP: frame corpus not present");
            return;
        };
        // Two independent captures two days apart, both exact. The threshold sits under these.
        assert!(real >= COMBAT_FINISH_PRESENT, "a real Finish scored {real:.4}");
        // `combat-stalled.png` was the second of those two captures and is **gone**: it lived in the
        // live spike directory, and a stall inside the anomaly overwrote it with a `PlayerTurn`
        // frame that contains no `Finish` at all. The directory was git-excluded, so there is
        // nothing to restore. Skipped rather than deleted, because the gap is worth seeing — a
        // second independent capture is what made the 1.0000 from the first one meaningful, and this
        // assertion comes back the moment one is captured into `tests/frames`.
        match score(&COMBAT_FINISH, "combat-stalled.png") {
            Some(older) => {
                assert!(older >= COMBAT_FINISH_PRESENT, "an older real Finish scored {older:.4}")
            }
            None => eprintln!("SKIP: combat-stalled.png awaits recapture"),
        }

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

    /// The confusable that mattered all along, finally measured rather than estimated.
    ///
    /// `Eulogise` occupies the identical slot and plank as `Finish`; only the word differs. A single
    /// template cannot separate them — 0.8527 for the word swap against 0.8639 for `Finish` merely
    /// going greyed — so the test is that **argmax over the two templates** does.
    #[test]
    fn finish_and_eulogise_are_told_apart_by_comparing_both_templates() {
        let Some(f_on_finish) = score(&COMBAT_FINISH, "now.png") else {
            eprintln!("SKIP: frame corpus not present");
            return;
        };
        let e_on_finish = score(&COMBAT_EULOGISE, "now.png").unwrap();
        let f_on_death = score(&COMBAT_FINISH, "eulogise-at-death.png").unwrap();
        let e_on_death = score(&COMBAT_EULOGISE, "eulogise-at-death.png").unwrap();

        assert!(f_on_finish > e_on_finish, "{f_on_finish:.4} vs {e_on_finish:.4} on a Finish frame");
        assert!(e_on_death > f_on_death, "{e_on_death:.4} vs {f_on_death:.4} on the death screen");

        // Both margins comfortably wider than anything a single cut point could offer.
        let margin = (f_on_finish - e_on_finish).min(e_on_death - f_on_death);
        assert!(margin > 0.10, "argmax margin only {margin:.4}");

        // And the death screen must not clear the Finish bars at all.
        assert!(f_on_death < COMBAT_FINISH_PRESENT, "Eulogise scored {f_on_death:.4} as Finish");
        assert!(f_on_death < COMBAT_FINISH_ACTIVE);
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

    /// The combat HUD separates "in a fight" from every non-combat screen in the corpus, and does it
    /// on the frame where every *other* fingerprint had failed.
    ///
    /// `combat-turn1-hurt.png` is that frame: the run that entered combat from an overworld event and
    /// never noticed, at `health = 1` of 20, with the hurt vignette at 1.4 of 1.5. Six buttons
    /// sharing the affirmative slot were unreadable on it. This corner was not.
    #[test]
    fn the_combat_hud_is_told_apart_from_every_screen_that_is_not_a_fight() {
        let Some(hurt) = score(&COMBAT_HUD, "combat-turn1-hurt.png") else {
            eprintln!("SKIP: frame corpus not present");
            return;
        };
        assert!(
            hurt >= COMBAT_HUD_PRESENT,
            "turn 1 under a full-strength vignette scored {hurt:.4}, below the threshold — the one \
             state this fingerprint exists for"
        );

        // The gap is the finding, so it is asserted rather than trusted. Every one of these is a
        // screen the navigator can legitimately be sitting on, and none may read as a fight.
        for name in ["16-selected.png", "post-crypt.png", "reward-selected.png", "overworld-campfire.png"] {
            let q = score(&COMBAT_HUD, name).unwrap();
            assert!(q < COMBAT_HUD_PRESENT, "{name} scored {q:.4}, at or above the threshold");
            // Not merely under the bar — nowhere near it. A non-combat screen creeping up towards
            // 0.99 would mean the crop had started matching wood grain rather than the HUD.
            assert!(q < 0.75, "{name} scored {q:.4}, close enough to the bar to be worth re-measuring");
        }
    }

    /// The HUD crop is **biome-independent**, which is what makes it a screen fingerprint rather
    /// than a picture of one fight.
    ///
    /// `combat-turn1-hurt.png` is a crypt at 1/20 under a full vignette — the frame the template was
    /// cut from. `combat-forest-turn1.png` is a spider forest, a different parallax, different
    /// lighting, red vines across the backdrop, a different enemy. Both score 1.0000, so the crop
    /// carries only the `?` button and the turn plaque and none of the scene behind them.
    ///
    /// Worth pinning because 0.99 is the tightest bar in this module, and a crop that had picked up
    /// any backdrop would pass on its own biome and fail everywhere else — which would look exactly
    /// like the bug this frame came from, and is not it.
    #[test]
    fn the_combat_hud_reads_the_same_in_a_different_biome() {
        let Some(forest) = score(&COMBAT_HUD, "combat-forest-turn1.png") else {
            eprintln!("SKIP: frame corpus not present");
            return;
        };
        let crypt = score(&COMBAT_HUD, "combat-turn1-hurt.png").unwrap_or(0.0);
        assert!(forest >= COMBAT_HUD_PRESENT, "a forest fight scored {forest:.4}");
        assert!(crypt >= COMBAT_HUD_PRESENT, "the crypt frame scored {crypt:.4}");

        // And nothing else claims that frame, so `identify` reaching it returns `CombatEntered`.
        assert!(score(&COMBAT_FINISH, "combat-forest-turn1.png").unwrap_or(0.0) < COMBAT_FINISH_PRESENT);
        assert!(score(&CHARACTER_STATS, "combat-forest-turn1.png").unwrap_or(0.0) < CHARACTER_STATS_PRESENT);
    }

    /// The class-unlock screen must not read as hero select. **Both layers are asserted.**
    ///
    /// Met live: a road event unlocked the Woodsman mid-run, and every run that day reported
    /// `screen: HeroSelect` and then failed hunting for a map that was not there.
    #[test]
    fn the_unlock_screen_is_not_hero_select() {
        let Some(unlock) = score(&UNLOCK_CONTINUE, "unlock-woodsman.png") else {
            eprintln!("SKIP: frame corpus not present");
            return;
        };
        let hero = score(&HEROSELECT_HEADER, "unlock-woodsman.png").unwrap_or(0.0);
        eprintln!("  unlock {unlock:.4} / hero-select heading {hero:.4}");

        // Layer one: the unlock's own button is exact.
        assert!(unlock >= UNLOCK_CONTINUE_PRESENT, "the real Continue scored {unlock:.4}");

        // Layer two: the heading no longer clears its bar on this frame. It DID at the old 0.80 --
        // 0.8532 -- which is why the bar moved. Asserted from both sides so neither the raise nor the
        // measurement can be quietly undone.
        assert!(hero > 0.80, "the impostor that forced the raise scored {hero:.4}, once above 0.80");
        assert!(hero < HEROSELECT_HEADER_PRESENT, "still reads as hero select at {hero:.4}");

        // Layer three, and the one that survives an impostor we have not met yet: ordering. Asserted
        // on explicit booleans rather than on these scores, because the point is what `identify` does
        // when BOTH fire -- which is the situation a louder future impostor puts us in.
        assert_eq!(
            super::identify_from_scores(true, true),
            Screen::Unlock,
            "with both firing, the more specific fingerprint has to win"
        );
        assert_eq!(super::identify_from_scores(false, true), Screen::HeroSelect);
    }

    /// The event plaque separates "an event is still up" from every screen that has no event on it.
    #[test]
    fn the_event_plaque_is_told_apart_from_every_screen_without_one() {
        // Scored through the computed band, the way live code does it -- two options on that frame.
        let scan = |name: &str, options: usize| -> Option<f64> {
            let f = frame(name)?;
            let tpl = Template::load(&PathBuf::from("templates").join(EVENT_CHOICE.template)).ok()?;
            find_at_scale_in(&crop(&f, event_choice_search(options)), &tpl, 1.0, 1, None)
                .map(|m| m.inliers)
        };
        let Some(real) = scan("event-woodsman.png", 2) else {
            eprintln!("SKIP: frame corpus not present");
            return;
        };
        assert!(real >= EVENT_CHOICE_PRESENT, "the Woodsman's own event scored {real:.4}");
        eprintln!("  event-woodsman.png (n=2): {real:.4}");
        // The count has to be right for the band to land on the plaque. A wrong `n` is a miss, not a
        // near-miss, and that is worth pinning: it is what makes the console's choice list load-
        // bearing rather than incidental.
        for n in [1, 3, 4] {
            let q = scan("event-woodsman.png", n).unwrap_or(0.0);
            assert!(q < EVENT_CHOICE_PRESENT, "the right plaque at the wrong n={n} scored {q:.4}");
        }

        // `overworld-campfire.png` is the one that matters. The map's area button is wood too, so a
        // bare-wood template is exactly the crop that could match it -- and would then report an
        // event on every overworld screen, freezing the run for ever. 400 px is wider than the 250 px
        // `default` button, which is what makes that impossible rather than merely unlikely.
        for name in
            ["overworld-campfire.png", "post-crypt.png", "combat-turn1-hurt.png", "16-selected.png"]
        {
            // Every option count the band can take, since a run asks this question with whatever
            // count the console last reported -- including on a screen that has no event at all.
            for n in 1..=4 {
                let q = scan(name, n).unwrap_or(0.0);
                assert!(
                    q < EVENT_CHOICE_PRESENT,
                    "{name} scored {q:.4} at n={n}, at or above the threshold"
                );
                eprintln!("  {name} (n={n}): {q:.4}");
            }
        }
    }

    /// **This crop is no longer turn-specific, and that is now the point.**
    ///
    /// It used to assert the opposite — that a later turn scores *below* [`COMBAT_HUD_PRESENT`],
    /// because the template carries the numeral `1` and the check was meant as an entry signal. That
    /// held at a 0.99 bar, and the old assertion's own message said what to do if the numeral
    /// stopped mattering: say so here rather than leave the doc claiming it does. This is that.
    ///
    /// What changed: a live fight at turn 1 scored **0.9820** and missed the 0.99 bar, so `identify`
    /// returned `Unknown` and the run stood on the map path in front of a full board until it gave
    /// up. The bar is [`MAX_PRESENT`] now, and a two-character numeral is too small a part of a
    /// 225x100 region to hold turn 11 below it.
    ///
    /// So the contract is "a fight is on screen", not "a fight just started" — which is the more
    /// useful question anyway. The **gap to the nearest non-combat frame** is what the check really
    /// lives on, and if that ever closes the answer is a crop without the numeral, never a lower
    /// threshold.
    #[test]
    fn any_turn_reads_as_combat_and_stands_clear_of_what_is_not() {
        let Some(turn_11) = score(&COMBAT_HUD, "combat-chest.png") else {
            eprintln!("SKIP: frame corpus not present");
            return;
        };
        let turn_1 = score(&COMBAT_HUD, "combat-turn1-hurt.png").unwrap();
        for (name, q) in [("turn 1", turn_1), ("turn 11", turn_11)] {
            assert!(
                q >= COMBAT_HUD_PRESENT,
                "{name} scored {q:.4}, below the bar — a fight this misses is a fight the run walks \
                 past, which is exactly what 0.99 did on 2026-08-09"
            );
        }
        let nearest_non_combat = score(&COMBAT_HUD, "16-selected.png").unwrap();
        assert!(
            turn_11 - nearest_non_combat > 0.3,
            "a later turn ({turn_11:.4}) should stand well clear of the nearest non-combat screen \
             ({nearest_non_combat:.4}); that margin is the whole of this check"
        );
    }

    /// **No threshold may be tighter than the frame variation nobody has measured.**
    ///
    /// The rule behind [`MAX_PRESENT`], enforced rather than trusted. Two bars in this file were set
    /// near 1.0 with a note that exact bounds made it free, and the tighter of them missed a live
    /// fight by 0.008 — a run stood in front of a full board reporting a navigation error.
    ///
    /// A cap is not a substitute for measurement, and this test does not pretend otherwise: the
    /// per-button docs carry the true positive and the nearest confusable, and those gaps are what
    /// make a threshold correct. This only guarantees that whatever is chosen leaves at least 0.05
    /// of room for a background, an animation or a biome we have not photographed.
    ///
    /// The list is written out because there is no way to enumerate consts, which is the same
    /// weakness [`ALL`] has — a threshold added and not listed here is checked by nothing. Add it.
    #[test]
    fn every_threshold_leaves_room_for_a_frame_we_have_not_seen() {
        let bars = [
            ("COMBAT_FINISH", COMBAT_FINISH_PRESENT),
            ("COMBAT_FINISH_ACTIVE", COMBAT_FINISH_ACTIVE),
            ("COMBAT_HUD", COMBAT_HUD_PRESENT),
            ("COMBAT_EULOGISE", COMBAT_EULOGISE_PRESENT),
            ("REWARD_SCREEN", REWARD_SCREEN_PRESENT),
            ("MENU_START", MENU_START_PRESENT),
            ("HEROSELECT_CONFIRM", HEROSELECT_CONFIRM_PRESENT),
            ("POSTGAME_CONTINUE", POSTGAME_CONTINUE_PRESENT),
            ("CHARACTER_STATS", CHARACTER_STATS_PRESENT),
            ("CHARACTER_BACK", CHARACTER_BACK_PRESENT),
            ("PREGAME_START", PREGAME_START_PRESENT),
            ("EVENT_CHOICE", EVENT_CHOICE_PRESENT),
            ("UNLOCK_CONTINUE", UNLOCK_CONTINUE_PRESENT),
            ("HEROSELECT_HEADER", HEROSELECT_HEADER_PRESENT),
            ("SHRINE_GOBACK", SHRINE_GOBACK_PRESENT),
            ("STATS_BACK", STATS_BACK_PRESENT),
            ("SHRINE_PRAY", SHRINE_PRAY_PRESENT),
            ("CONTINUE", CONTINUE_PRESENT),
        ];
        for (name, bar) in bars {
            assert!(
                bar <= MAX_PRESENT,
                "{name}_PRESENT is {bar:.2}, above the {MAX_PRESENT:.2} cap. A bar that tight \
                 asserts a region is pixel-identical across frames nobody has sampled; if the \
                 separation genuinely needs it, the crop is wrong, not the cap"
            );
        }
    }

    #[test]
    fn praying_cannot_be_fooled_by_the_button_it_replaces() {
        // The cap above is the ceiling; this is the floor, and it is the one that has teeth.
        //
        // `shrineplay::claim_blessing` presses `Pray` on this threshold alone, with no second
        // opinion, and the rect it reads is shared with `Consecrate`. The dangerous neighbour is not
        // an *active* Consecrate — that state means the blessing is genuinely not claimable yet, and
        // pressing it there would spend the solve early — it is the greyed one, measured live at
        // `shrine1` on 2026-08-15 while the word was still unsolved.
        //
        // Lowering the bar past that figure does not merely make a match noisier: it makes "the
        // blessing is ready" indistinguishable from "the word has not been solved", which is the
        // pair this whole slot exists to separate.
        const GREYED_CONSECRATE: f64 = 0.8564;
        assert!(
            SHRINE_PRAY_PRESENT > GREYED_CONSECRATE,
            "SHRINE_PRAY_PRESENT is {SHRINE_PRAY_PRESENT:.4}, at or below the {GREYED_CONSECRATE:.4} \
             a greyed `Consecrate` scores against Pray's artwork — an unsolved shrine would read as \
             a claimable blessing"
        );
    }

    /// **A button under the pointer is not the button we have a template for.**
    ///
    /// `ui/elements/button.lua:122,159` draws `<img>-up-hover.jpg` over `<img>-up.jpg` at
    /// `hover_alpha`, and `hover` is set purely from `mousemoved` (`:223-269`) — so a plaque keeps
    /// its hover artwork for as long as the pointer sits inside it, with nothing to time out. Every
    /// template in this file was cut with the pointer somewhere else.
    ///
    /// `inn-rest-hovered.png` is the live frame from 2026-08-15 at The Quacking Duck, captured after
    /// the run had clicked `Rest` and left the pointer on it. The plaque is plainly drawn, at the
    /// coordinate the arithmetic predicts, and it scores **0.5452** — so far under [`MIN_INLIERS`]
    /// that no threshold could split the two. The run read that as "the inn is not there yet", spent
    /// [`crate::innplay::REST_TRIES`] × 1.5s hunting it, and then `leave_inn`'s presence check —
    /// the same `locate` — concluded it was already out of the inn while standing in it.
    ///
    /// The fix is not a looser bar or a second template: it is to move the pointer off the artwork
    /// before reading it, which [`crate::navigate::Run::park`] already exists to do. This test is
    /// what says the hazard is real, and it is the reason that call is not optional.
    #[test]
    fn a_hovered_plaque_does_not_match_its_own_template() {
        let Some(hovered) = score(&INN_REST, "inn-rest-hovered.png") else {
            eprintln!("SKIP: frame corpus not present");
            return;
        };
        assert!(
            hovered < MIN_INLIERS,
            "`inn Rest` scores {hovered:.4} on the hovered frame, at or above MIN_INLIERS \
             {MIN_INLIERS:.4}. If this now passes, the hover artwork or the metric changed — and \
             the parking in `rest_at_inn` was justified by this measurement."
        );
    }

    /// The back plaque is one button wearing several screens, and that is what makes it the exit.
    ///
    /// [`SHRINE_GOBACK`]'s template was cut from a **shrine**. This scores it against an **inn**,
    /// through [`score_at_origin`] because that is the path `identify` and `click_when_ready` take.
    /// `ui/inn.lua:68-71` and `ui/rest.lua:517-520` declare the same `small` button at `ss(0, 0.9)`,
    /// `xOffset 1.13`, with the same `back.png` icon, and the plaque is opaque — so the two rooms
    /// behind them never reach the pixels.
    ///
    /// This is the measurement [`crate::navigate::Run::back_one_screen`] rests on: leaving the inn
    /// is keyed on this plaque rather than on `Rest`, because the plaque is not the button we just
    /// clicked and so is never the one we are hovering.
    #[test]
    fn the_back_plaque_is_the_same_button_at_a_shrine_and_at_an_inn() {
        let Some(q) = score_at_origin(&SHRINE_GOBACK, "inn-rest-hovered.png") else {
            eprintln!("SKIP: frame corpus not present");
            return;
        };
        // Held to [`SHRINE_GOBACK_PRESENT`] rather than to the [`MIN_INLIERS`] that `locate` — and
        // so `back_one_screen` — actually gates on. The stricter bar is deliberate: this is a claim
        // that the two screens share one piece of artwork, not merely that they are close enough to
        // pass. Measured 1.0000, so there is nothing being squeezed through here.
        assert!(
            q >= SHRINE_GOBACK_PRESENT,
            "the shrine's back plaque scores {q:.4} on an inn screen, under the \
             {SHRINE_GOBACK_PRESENT:.4} this test holds it to. If this fails the two screens no \
             longer share the artwork, and leaving the inn needs a template of its own."
        );
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

    /// [`AREA_BUTTON_SHOWING`] sits in a gap that was measured, against the states it can be
    /// mistaken for.
    ///
    /// Every area button is the same plank at the same coordinate, so background agreement is total
    /// and only the lettering separates them — a threshold guessed here would be a threshold picked
    /// out of the air. The three confusables are the ones a crossing actually meets: `Explore` (the
    /// corrupted forest we are trying to enter), `Visit` (a settlement subnode), and `Combat` greyed
    /// out (offered but not pressable).
    ///
    /// The slot captures are 250x100, exactly template-sized, so [`score`] and `score_at_origin`
    /// would both be measuring the same single comparison. `crop` is given the whole frame.
    #[test]
    fn the_combat_plank_is_separable_from_the_planks_it_shares_a_slot_with() {
        let tpl = Template::load(&PathBuf::from("templates").join(AREA_COMBAT.template)).unwrap();
        let against = |name: &str| -> Option<f64> {
            let f = frame(name)?;
            assert_eq!(
                (f.width, f.height),
                (tpl.width as i32, tpl.height as i32),
                "{name} must be a slot-sized capture for this comparison to mean anything"
            );
            find_at_scale_in(&f, &tpl, 1.0, 1, None).map(|m| m.inliers)
        };
        let Some(explore) = against("area-explore.png") else {
            eprintln!("SKIP: frame corpus not present");
            return;
        };
        let visit = against("area-visit.png").unwrap();
        let greyed = against("area-combat-greyed.png").unwrap();
        let worst = explore.max(visit).max(greyed);
        assert!(
            worst < AREA_BUTTON_SHOWING,
            "the nearest confusable must fall under the gate: Explore {explore:.4}, \
             Visit {visit:.4}, greyed Combat {greyed:.4}, gate {AREA_BUTTON_SHOWING}"
        );
        // A margin rather than a bare inequality: a gate resting a thousandth above the loudest
        // wrong answer is a gate that the next capture moves. This one has 0.07 under it.
        assert!(
            AREA_BUTTON_SHOWING - worst > 0.05,
            "the gap is too thin to trust: worst confusable {worst:.4} vs gate {AREA_BUTTON_SHOWING}"
        );
    }

    /// [`AREA_BUTTON_LIVE`] separates a **pressable** plank from a greyed one, whatever it says.
    ///
    /// The other half of the same measurement, and the one that would have saved the two dead
    /// presses of 2026-08-20. `Explore` and `Visit` are live buttons wearing different words and
    /// score 0.87 and 0.86; a greyed `Combat` — the same word as the template — scores 0.74. So
    /// greying costs more agreement than the lettering does, and one bar can tell live from dead
    /// across all three.
    ///
    /// The corpus is three live planks and one greyed one. The live side is much better sampled by
    /// the run logs — see [`AREA_BUTTON_LIVE`] for all 298 readings, the worst live one of which is
    /// **0.8355**, and for why the caller still may not veto a press on this bar.
    ///
    /// Both populations are asserted, corpus and live, because the gate has to clear both and the
    /// live figures are what moved it from 0.80 to 0.79.
    #[test]
    fn a_greyed_plank_reads_lower_than_any_live_one() {
        let tpl = Template::load(&PathBuf::from("templates").join(AREA_COMBAT.template)).unwrap();
        let against = |name: &str| -> Option<f64> {
            find_at_scale_in(&frame(name)?, &tpl, 1.0, 1, None).map(|m| m.inliers)
        };
        let Some(explore) = against("area-explore.png") else {
            eprintln!("SKIP: frame corpus not present");
            return;
        };
        let visit = against("area-visit.png").unwrap();
        let greyed = against("area-combat-greyed.png").unwrap();

        // The ordering is the claim: every live plank above the bar, the greyed one below it.
        // The template scores 1.0000 against itself and cannot be the minimum, so it is left out.
        let worst_live = explore.min(visit);
        assert!(
            worst_live > AREA_BUTTON_LIVE,
            "a live plank must read as live: Explore {explore:.4}, Visit {visit:.4},              gate {AREA_BUTTON_LIVE}"
        );
        assert!(
            greyed < AREA_BUTTON_LIVE,
            "a greyed plank must not: {greyed:.4} vs gate {AREA_BUTTON_LIVE}"
        );
        // A margin on both sides, as above. A bar resting a thousandth off either population is a
        // bar the next capture moves.
        assert!(
            worst_live - AREA_BUTTON_LIVE > 0.05 && AREA_BUTTON_LIVE - greyed > 0.05,
            "the gap is too thin to trust: worst live {worst_live:.4}, greyed {greyed:.4}, \
             gate {AREA_BUTTON_LIVE}"
        );
        // **The live populations**, from the `area slot:` lines of `spike-run-20260821-*.md`. Not
        // re-measurable here — they are readings of screens nobody captured — so they are written
        // down as the numbers the runs printed, which is what a threshold moved by them owes the
        // next person to touch it.
        const LIVE_TRAVEL: f64 = 0.8566; // x244, every crossing press in five runs
        const LIVE_OPEN: f64 = 0.8458; // x6, a chest with nothing guarding it
        const LIVE_WORST: f64 = 0.8355; // x5, a village subnode
        const GREYED_AT_L38: f64 = 0.7367; // x1, the reading that ended run 0251Z
        for live in [LIVE_TRAVEL, LIVE_OPEN, LIVE_WORST] {
            assert!(live > AREA_BUTTON_LIVE, "{live:.4} is a live plank and must read as one");
        }
        assert!(GREYED_AT_L38 < AREA_BUTTON_LIVE, "and the one that ended a run must not");
        // The same 0.05 either side, which is what took the gate from 0.80 down to 0.79: at 0.80
        // the worst live reading had only 0.0355 under it.
        assert!(LIVE_WORST - AREA_BUTTON_LIVE > 0.04, "live margin");
        assert!(AREA_BUTTON_LIVE - GREYED_AT_L38 > 0.05, "greyed margin");
        // The greyed reading agrees with the corpus **exactly**, which is what makes the live logs
        // usable as calibration at all rather than as anecdote.
        assert!((GREYED_AT_L38 - greyed).abs() < 1e-4, "corpus {greyed:.4} vs live {GREYED_AT_L38}");

        // And it is strictly the looser of the two bars, which is what makes `Combat` imply `live`.
        assert!(AREA_BUTTON_LIVE < AREA_BUTTON_SHOWING);
    }
}
