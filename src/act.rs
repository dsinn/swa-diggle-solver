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
/// `ss(0.9, 0.9)` with the 250x100 `default` size — centre (1728, 972), matching the coordinate a
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
/// All figures are for the **exact** 250x100 button rect. An earlier crop was 300x100 and so carried
/// 25 px of scene down each side; that made it score 1.0000 on two frames of the *same crypt* and
/// would have made it scene-dependent anywhere else. `default` is 250x100
/// (`ui/elements/button.lua:17`), so that is what the template is.
///
/// ```text
///   Finish, now.png                         1.0000  err 0.0000
///   Finish, combat-stalled.png              1.0000  err 0.0000   independent, 2 days apart
///   Finish + `Fight on!`, different scene   1.0000  err 0.0000   <- scene-independent, as intended
///   greyed Finish at 0 health               0.8639  err 0.0720
///   `Eulogise`, the same slot at death      0.8527  err 0.0537   <- the real confusable
///   'Adventure!' plank                      0.8228  err 0.0696
///   plain overworld terrain                 0.6784  err 0.1271
///   postgame Continue                       0.3327  err 0.2352
/// ```
///
/// ## Two questions, and only one of them has a clean answer
///
/// "Is this word `Finish` or `Eulogise`" does **not** separate on one template: 0.8527 for a word
/// swap against 0.8639 for a mere state change is eleven thousandths, and any cut point between
/// them would be fitting noise. [`slot_is_eulogise`] compares the two templates instead, which
/// separates the same pair by 0.147.
///
/// "Is an **active** `Finish` on screen" separates cleanly — 1.0000 against 0.8639 greyed — and that
/// is what [`COMBAT_FINISH_ACTIVE`] asks. An inactive button cannot be clicked anyway, so waiting
/// for an active one costs nothing and gives the strongest reading available.
///
/// Note the tightened crop made terrain rejection *worse*, 0.5653 to 0.6784: the wide version had
/// dark scene edges that brown ground could not match, and the exact one is all brown wood. That is
/// a fair price for a template that means the same thing in every scene, and the thresholds sit far
/// above it either way.
pub const COMBAT_FINISH: Button = Button {
    name: "combat Finish",
    template: "combat-finish.png",
    // `default` is 250x100 (`ui/elements/button.lua:17`) centred at (1728,972), so the button is
    // exactly (1603,922)-(1853,1022). This is that grown by 8 px of slack -- no scene included.
    search: (1595, 914, 1861, 1030),
    origin: (1603, 922),
    click: (1728, 972),
};

/// `Finish` is on screen, so a fight is sitting in `WaitPhase`. See [`COMBAT_FINISH`].
///
/// **0.95.** Raised from 0.90 once the bounds became exact: with the template cropped to the button
/// itself, a genuine `Finish` scores 1.0000, so the bar can sit far above every confusable — the
/// greyed button (0.8639), `Eulogise` (0.8527), the `Adventure!` plank (0.8228) and bare terrain
/// (0.6784) — without ever risking a true positive.
pub const COMBAT_FINISH_PRESENT: f64 = 0.95;

/// The combat HUD's top-left corner: the `?` button and the turn counter beside it.
///
/// Answers the question [`COMBAT_FINISH`] cannot — **"are we in a fight at all"**, rather than "is
/// this fight waiting to be finished". `Finish` is drawn only in `WaitPhase`, so mid-`PlayerTurn`
/// there is no combat fingerprint anywhere on the registry, and a run that lands in one has nothing
/// to recognise. That is not hypothetical: a live run entered combat from an overworld event, never
/// noticed, and spent its whole budget probing for a map (`spike-run-raw.log:59-73`).
///
/// **Recognition only — never clicked.** `?` opens a help overlay we have no way out of. `click` is
/// the template centre because the field is not optional, not because anything should press it.
///
/// ## Why this corner, when everything else on that screen was unreadable
///
/// The frame that stranded that run had the hurt vignette at 1.4 of a 1.5 maximum
/// (`rpgview.lua:1936-1945`, at `health = 1` of 20). That overlay is a *masked* blend — the shader
/// discards below its cutoff (`shaders/retina-hurt.vs`) — so it eats inward from the edges and
/// leaves a clean middle. Measured on that frame, in bands of distance from centre: nothing inside
/// r≈0.38, then +31 to +40 mean redness outside it, and **+58 over the affirmative slot**, which is
/// why six buttons sharing (1603,922) all became unreadable at once.
///
/// This region survives it completely. The `?` sub-rect scores **1.0000** on that same frame against
/// a full-health combat frame from a different fight *and a different game version* — not one pixel
/// over tolerance. The HUD layer draws over the overlay, not under it.
///
/// ## Measured, hurt frame vs the saved-frame corpus
///
/// ```text
///   combat-chest.png (turn 11)              1.0000   the reference
///   now.png                                 0.9750
///   waitphase-with-fighton.png              0.9750
///   gave-up.png (turn 1, vignette at 1.4)   0.9616   <- the state this exists for
///   eulogise-at-death.png                   0.8894   <- dead, and usefully distinguishable
///   16-selected.png                         0.4257   <- nearest non-combat
///   post-crypt.png                          0.4194
///   reward-selected.png                     0.4194
///   overworld-campfire.png                  0.4041
/// ```
///
/// A gap of **0.46** between the combat floor and the non-combat ceiling — the widest separation of
/// any fingerprint in this registry.
///
/// ## The turn counter is inside the crop on purpose, and bounds its use
///
/// Including it is what buys the separation: the `?` alone drops to 0.61-0.63 on non-combat frames,
/// against 0.4041-0.4257 for the pair. But it also means the crop is **turn-specific**. The template
/// is cut at turn 1, so this is an *entry* signal — "we have just become a fight" — and the whole
/// 0.9616-vs-1.0000 shortfall above is the numeral, `11` against `1`, nothing else.
///
/// Reusing it past turn 1 needs a lower bar than [`COMBAT_HUD_PRESENT`], or a crop with the numeral
/// excluded. Do not simply relax the threshold and assume it still means the same thing.
pub const COMBAT_HUD: Button = Button {
    name: "combat HUD",
    template: "combat-hud.png",
    // The `?` button and the turn plaque, (60,0)-(285,100), grown by 8 px of slack. Clamped at the
    // top edge: the plaque is flush with y=0, so there is no room to grow upwards.
    search: (52, 0, 293, 108),
    origin: (60, 0),
    click: (172, 50),
};

/// A fight is in progress and on its **first turn**. See [`COMBAT_HUD`].
///
/// **0.99, not 1.00.** Turn 1 against a turn-1 template genuinely measures 1.0000, and the region is
/// immune to the vignette that breaks everything else — so a perfect bar would cost nothing *if the
/// HUD is truly static*. That has been checked on one frame, and one frame is not a steady state:
/// if the `?` plaque so much as pulses, an exact bar fails intermittently, which is the worst way
/// for a detector to be wrong. The 0.01 is slack for that, not tolerance for a changed numeral —
/// the nearest thing below is 0.8894, so it costs no separation at all.
pub const COMBAT_HUD_PRESENT: f64 = 0.99;

/// `Finish` is not merely on screen but **drawn active**, i.e. clickable. See [`COMBAT_FINISH`].
///
/// Separate from [`COMBAT_FINISH_PRESENT`] because it answers a harder question and gates a riskier
/// decision — pressing at zero health, where the wrong label ends the run.
///
/// **0.97, and high on purpose.** Now that the template is the exact button rect rather than the
/// button plus 25 px of scene, every genuine match measures **1.0000** — three independent frames,
/// two scenes, zero error. A threshold near 1.0 therefore costs nothing on the true positives while
/// putting 0.106 between itself and the nearest confusable.
///
/// The asymmetry settles it: refusing a real `Finish` costs one loop iteration, pressing `Eulogise`
/// ends the run. When one side of a mistake is recoverable and the other is not, the bar belongs
/// just under the true positive rather than midway to the noise.
///
/// One caveat kept honest: the 0.9880 reading taken live at low health was against the *old*
/// 300x100 crop, whose outer margin the red tint barely touched. The exact crop is all plank, so the
/// same tint must cost somewhat more, and no frame of an **active** `Finish` at low health was ever
/// saved to measure it. If a live run is ever refused here at, say, 0.96, that is this caveat
/// arriving — and [`crate::fight::Fight::finish`] logs the best score precisely so it arrives as a
/// number rather than a mystery.
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

/// `Eulogise` — the same slot as [`COMBAT_FINISH`], when the character is dead.
///
/// Captured from a live death screen ("!! YOU DIED !!", `l10sub11`, 0/12 health). Cropped at exactly
/// [`COMBAT_FINISH`]'s geometry so the two are directly comparable: same 300x100 at (1578, 922).
///
/// ## This retires an estimate
///
/// [`COMBAT_FINISH_ACTIVE`] was set to 0.97 on the arithmetic that lettering is "roughly 7% of the
/// crop", with the note that no `Give up` or `Eulogise` had ever been captured. Now one has, and the
/// answer is symmetric and comfortable:
///
/// ```text
///                              vs finish tpl   vs eulogise tpl
///   the death screen              0.8535          1.0000
///   Finish, now.png               1.0000          0.8535
///   Finish, combat-stalled.png    1.0000          0.8535
///   Finish + Fight on!            1.0000          0.8535
///   greyed Finish at 0 health     0.8380          0.8174
///   'Adventure!' plank            0.7843          0.8012
///   postgame Continue             0.2777          0.3714
///   plain overworld terrain       0.5653          0.5451
/// ```
///
/// A word swap costs **0.1465**, not the ~0.07 I estimated — so 0.97 rejects `Eulogise` with 0.12 to
/// spare, and even 0.90 would have held. The guess was conservative in the right direction, which is
/// luck rather than method; the number is now measured.
///
/// ## Argmax beats a threshold here
///
/// Two templates for one slot turn a threshold question into a classification: whichever scores
/// higher is the word on the button. Every row above separates by at least 0.146 that way, against
/// margins as thin as 0.02 for any single cut point. [`slot_is_eulogise`] uses it.
pub const COMBAT_EULOGISE: Button = Button {
    name: "combat Eulogise",
    template: "combat-eulogise.png",
    search: (1595, 914, 1861, 1030),
    origin: (1603, 922),
    click: (1728, 972),
};

/// The dead-character slot is showing. See [`COMBAT_EULOGISE`].
///
/// 0.97: true positive 1.0000, nearest confusable 0.8527 — the same reasoning as
/// [`COMBAT_FINISH_ACTIVE`]. Exact bounds make a near-1.0 bar free.
pub const COMBAT_EULOGISE_PRESENT: f64 = 0.97;

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

/// The main menu's `Start` — a **new** run, only present when no save exists.
///
/// `default` 250x100 centred at (500, 810), so exactly (375,760)-(625,860).
///
/// ## The same slot says `Restart` when a save exists, and that eulogises
///
/// This is the hazard the module header opens with, and it is the Finish/Eulogise pattern again:
/// one position, two words, and one of them destroys a run. Both sides are measured:
///
/// ```text
///   `Start`, fresh save (no mainSaveData)   1.0000  err 0.0000
///   `Restart`, menu with a save present     0.8619  err 0.0503   <- eulogises if pressed
///   plain overworld terrain                 0.5400  err 0.1865
///   a postgame screen                       0.0898  err 0.3753
///   combat WaitPhase                        0.0832  err 0.2949
/// ```
///
/// A word swap costs 0.138 here, in line with the 0.147 measured for Finish vs Eulogise. So
/// [`MENU_START_PRESENT`] at 0.95 separates them with 0.09 to spare, and the run refuses rather than
/// eulogises if it ever meets the wrong one.
pub const MENU_START: Button = Button {
    name: "menu Start",
    template: "menu-start.png",
    search: (367, 752, 633, 868),
    origin: (375, 760),
    click: (500, 810),
};

/// The menu is offering a **new** run, not `Restart`. See [`MENU_START`].
///
/// 0.95: `Start` measures 1.0000, `Restart` 0.8619.
pub const MENU_START_PRESENT: f64 = 0.95;

/// Hero select's confirm button — proof that a champion **is** selected.
///
/// `ui/heroselect.lua:327-336`, `ss(1, 0.9)`, `xOffset -0.75`, `default` 250x100 → exactly
/// (1608,922)-(1858,1022). The fourth distinct word to occupy this corner, after `Finish`,
/// `Eulogise` and the postgame `Continue`.
///
/// ## It is a read-back, not a screen detector
///
/// `showIf = function() return selectedIndex and selectedHeroes[selectedIndex] end` — the button
/// **does not exist** until a champion is picked. So its presence is exactly the confirmation that a
/// click on a hero card landed, which is the one thing the driver could not otherwise know.
///
/// ## The caption changes with the class, so this asks presence, not identity
///
/// The label is `classes[...].availableMessageShort or 'Start'`. Three classes measured live:
///
/// ```text
///   `Fight!`     (the template itself)   1.0000  err 0.0000
///   `Arise`      (another class)         0.8751  err 0.0438
///   `Adventure!` (another class)         0.8158  err 0.0740   <- lowest caption seen
///   ---------------------------------------------------------  a 0.67-wide gap
///   NO champion selected                 0.1488  err 0.3612   <- the case to reject
///   the main menu                        0.1059  err 0.3788
/// ```
///
/// **0.55, in the middle of the gap.** Two earlier attempts hugged the caption side — 0.80 off an
/// estimate, then 0.70 after `Adventure!` came in at 0.8158 — and both were the wrong shape of
/// answer. The question here is *does this button exist*, not *which word is on it*, and absence
/// does not score 0.8; it scores 0.15. A bar at 0.55 leaves 0.27 under the lowest caption seen and
/// 0.40 over absence, so an unseen caption longer than `Adventure!` still passes comfortably.
///
/// Note how differently the caption cost behaves from the other slot-sharing buttons: 0.147 for
/// Finish-vs-Eulogise and 0.138 for Start-vs-Restart, but 0.184 for `Adventure!`-vs-`Fight!`. Those
/// were swaps between words of *similar length*; this one nearly doubles the glyph run. Any estimate
/// of "what a word swap costs" is really an estimate about length.
///
/// **Do not use this as a general screen check** — and at 0.55 that matters more, not less. Combat's
/// `Finish` scores 0.8912 at this rect and bare terrain 0.6615, both well over the bar. Harmless
/// while this is only ever asked on hero select, where neither can be on screen; a bug anywhere else.
pub const HEROSELECT_CONFIRM: Button = Button {
    name: "hero select confirm",
    template: "heroselect-confirm.png",
    search: (1600, 914, 1866, 1030),
    origin: (1608, 922),
    click: (1733, 972),
};

/// A champion is selected, so hero select can be confirmed. See [`HEROSELECT_CONFIRM`].
pub const HEROSELECT_CONFIRM_PRESENT: f64 = 0.55;

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

/// The character/inventory screen's `Stats` button.
///
/// A dead end if the run lands there by accident: its area-button coordinate means `Stats`, so the
/// navigator's usual click does nothing useful and the run spends its budget in a menu. That has now
/// happened twice, and until this template existed nothing could even say it had happened.
///
/// `Stats` is unusually distinctive because no other screen puts a plank in the bottom-LEFT corner:
///
/// ```text
///   the character screen   1.0000  err 0.0000
///   overworld terrain      0.2952  err 0.2545
///   combat WaitPhase       0.2902  err 0.2483
///   the main menu          0.1567  err 0.2814
///   postgame               0.1145  err 0.3895
///   hero select            0.1101  err 0.3878
/// ```
pub const CHARACTER_STATS: Button = Button {
    name: "character Stats",
    template: "character-stats.png",
    search: (137, 912, 403, 1028),
    origin: (145, 920),
    click: (270, 970),
};

/// The character screen is up. See [`CHARACTER_STATS`]. 0.70 — nothing else comes near 0.30.
pub const CHARACTER_STATS_PRESENT: f64 = 0.70;

/// The character screen's return arrow, bottom-right — the way out.
///
/// A `small` 100x100 at (1674,917)-(1774,1017). Fingerprinted so that leaving the character screen
/// is a verified click like every other, rather than a bare coordinate. The first version of the
/// escape clicked (1730, 968) on faith, which was the only click in that change asking the observer
/// nothing — the exact habit behind the day's failures.
///
/// ```text
///   the character screen, two captures   1.0000  err 0.0000
///   plain overworld terrain              0.6847  err 0.1285
///   combat WaitPhase                     0.6721  err 0.1295
///   hero select                          0.6425  err 0.1371
///   postgame                             0.3023  err 0.2597
///   the main menu                        0.1056  err 0.3819
/// ```
///
/// **0.90.** The arrow itself does not vary — fixed icon, no text, no per-class form, no inactive
/// state on this screen — and two independent captures measured exactly 1.0000 with zero error, so
/// the question is really "is this arrow present" rather than "how close is close enough".
///
/// 0.99 was tried on that reasoning and is a shade too literal. A pixel-exact bar assumes the
/// *capture* is as stable as the sprite, and it is not: the frame arrives through a live grab, and
/// the plank behind the arrow can pick up a hover tint or a frame of animation from elsewhere on
/// screen. 0.90 still leaves 0.22 to the nearest confusable at 0.6847 — an enormous margin by the
/// standards of every other threshold here — while not betting the run on a capture being byte-clean.
///
/// The confusables cluster near 0.68 because they are all brown planks in roughly this corner; none
/// of them carries the arrow. A rejection stops the run with "stuck on the character screen", which
/// is loud and the safe direction.
pub const CHARACTER_BACK: Button = Button {
    name: "character back",
    template: "character-back.png",
    search: (1666, 909, 1782, 1025),
    origin: (1674, 917),
    click: (1724, 967),
};

/// The character screen's exit is on screen. See [`CHARACTER_BACK`].
pub const CHARACTER_BACK_PRESENT: f64 = 0.90;

/// The combat pregame's `Start`, cropped to the part of it that is **on screen**.
///
/// `ui/pregame.lua:142` — `button('Start', 0.5, 1, { yOffset = -0.38 })`, a `default` 250x100 centred
/// at (960, 1042). That runs to y=1092 on a 1080-tall client, so the bottom 12 px do not exist.
///
/// I had treated that as a reason not to fingerprint it, and it is not: cropping the **visible**
/// rect (835,992)-(1085,1080) gives a perfectly ordinary 250x88 template. A clipped button is only
/// awkward if you insist on cropping its nominal size.
///
/// ```text
///   the pregame               1.0000  err 0.0000
///   plain overworld terrain   0.7777  err 0.0996   <- nearest, as ever
///   hero select               0.7277  err 0.0971
///   combat WaitPhase          0.1138  err 0.3152
///   the character screen      0.0733  err 0.3922
///   postgame                  0.0585  err 0.4298
///   the main menu             0.0581  err 0.4334
/// ```
///
/// 0.90: 0.10 under the true positive, 0.12 over the nearest confusable.
///
/// **Preferred over the console announcement**, which is what this replaced. `Pregame screen:`
/// proves the screen was *constructed*, not that it is *up* — the same "announcement is not
/// readiness" trap in another costume — and it offers no way to tell whether the press worked. A
/// fingerprint does both: presence gates the click, and disappearance confirms it.
pub const PREGAME_START: Button = Button {
    name: "pregame Start",
    template: "pregame-start.png",
    search: (827, 984, 1093, 1080),
    origin: (835, 992),
    // Inside the visible part, deliberately not the geometric centre at (960, 1042).
    click: (960, 1035),
};

/// The combat pregame is up. See [`PREGAME_START`].
pub const PREGAME_START_PRESENT: f64 = 0.90;

/// An event's choice plaque — the fingerprint for **"this event is still on screen"**.
///
/// Answering an event is a click, and until now nothing checked that the click landed.
/// `handle_event` pressed a choice and logged `answered`, and a live run at `l9sub22` proved what
/// that is worth: the press did not take, the Woodsman's two options stayed up, and the run went
/// looking for a map dump that could not come because it was not on the map. It waited eight seconds
/// and failed with `inside a subworld with no settled dump` — a symptom three steps downstream of the
/// cause, naming a function that was working correctly. The frame is
/// `tests/frames/event-woodsman.png`, and both plaques are plainly still there.
///
/// This is the project's most repeated bug in its usual costume: announcement taken for readiness.
///
/// ## Scene-specific, and that is the design rather than a shortcut
///
/// Three things vary independently on this screen, and between them they rule out every generic
/// crop:
///
/// - **The choice text** is different on every event (`ui/eventscreen.lua:119`).
/// - **The backdrop** is `bgblurry` of the player's own location (`:31-36`), so it changes with where
///   the character is standing, not with which event fired.
/// - **An icon** may occupy a plaque's left end — `icon = icon, textAlign = icon and 'left'`
///   (`:101,104`), as the `[Combat]` choice does in that very frame.
///
/// What is left that is text-free, backdrop-free and icon-free is flat wood, and flat wood carries
/// no information: the first cut was a 400x34 strip of it and scored **0.8065** against
/// `overworld-campfire.png`, because `INLIER_TOLERANCE` is a per-pixel budget of 90 across three
/// channels and the map's brown dirt is about as smooth and about as brown. Adding the plaque's top
/// edge brought that to 0.5936 but bought the backdrop problem with it.
///
/// So this is the **whole first plaque of one named event**, 1000x150 plus 15 px of padding. Every
/// one of those variables is nailed down by being specific, and the crop is 40x the area of the
/// strip it replaces. It answers "is the Woodsman's shop offer still up", not "is some event up".
///
/// ## Which is safe only because absence is never trusted on its own
///
/// A template this specific scores near zero on a *different* event — and "no plaque" would then read
/// as "answered", which is the original bug wearing the fix's clothes. [`event_plaque_score`] is
/// therefore called **before** the click as well as after: the before-score is the positive control,
/// and unless it clears the bar, the after-score proves nothing and `handle_event` says so instead of
/// claiming success. Adding a template per event widens what can be verified; nothing silently
/// degrades in the meantime.
///
/// ## Position is computed, not searched for
///
/// `ui/eventscreen.lua:117-119` puts a choice at `buttonX = img and 0.075 or 0.5` with
/// `xOffset = img and 0.5 or 0`, and `buttonY = 0.55 + optionOffset + actualI*0.15`, where
/// `optionOffset = -(visibleChoices*0.075)` (`:17`). The first draft handed that to [`locate`] as an
/// 800x660 box — 251,000 candidate positions of a 13,600-pixel template, and it did not finish.
/// Substituting `optionOffset` collapses the vertical unknown to arithmetic:
///
/// ```text
///   the first choice of n:  y = 1080 * (0.70 - 0.075n)
/// ```
///
/// and **n is known** — the console prints the choice list, which is where the answer comes from in
/// the first place. See [`event_choice_search`].
pub const EVENT_CHOICE: Button = Button {
    name: "Woodsman shop plaque",
    template: "event-woodsman-shop.png",
    // The two-option case, so the struct is complete and
    // `every_search_box_can_contain_its_template` covers it. Live code calls `event_choice_search`.
    search: (121, 500, 1167, 688),
    origin: (129, 504),
    // Unused. The answer goes through the console's choice list, which knows which option is which,
    // and nothing should ever pick a plaque by this coordinate.
    click: (644, 594),
};

/// Where the **first** choice plaque sits, given how many options the console listed.
///
/// See [`EVENT_CHOICE`] for the derivation. The band is the computed position plus 8 px of slack
/// each way, which covers rounding in the game's own layout without reopening the search.
pub fn event_choice_search(options: usize) -> (i32, i32, i32, i32) {
    let n = options.max(1) as f64;
    // The crop starts 90 px above the plaque's centre line: 75 for half its height, 15 of padding.
    let y = (1080.0 * (0.70 - 0.075 * n)) as i32 - 90;
    // Slack of 8 px in x and 4 in y, not a region to search. Both axes are known: the plaque's
    // left edge is fixed for a named event (this one has a portrait, so `buttonX = 0.075`), and the
    // count fixes y. The slack is for the game's own rounding, nothing more -- 99 candidate
    // positions rather than the 1,207 a lazier box costs, and every one of those extra positions is
    // another chance for a wrong alignment to score well.
    (129 - 8, y - 4, 129 + 1030 + 8, y + 180 + 4)
}

/// The Woodsman's shop plaque is painted. See [`EVENT_CHOICE`].
///
/// Worst case over n = 1..4, since a run asks with whatever count the console last reported:
///
/// ```text
///   the Woodsman's own event, n=2   1.0000
///   overworld-campfire.png          0.6389
///   combat-turn1-hurt.png           0.5085
///   post-crypt.png                  0.2658
/// ```
///
/// The same event scored at the **wrong** n lands under 0.75 as well, which is asserted rather than
/// assumed: the band misses the plaque entirely, so the console's choice count is load-bearing and
/// not incidental.
///
/// **0.75**. The 1.0000 is a self-match — the template was cut from that frame — so it is an
/// upper bound rather than a typical score, and the same event at a different location will read
/// lower because of the backdrop in the padding. The bar is placed low in the gap for that reason,
/// and because the two errors are not equal: reading a real plaque as gone is the original bug, while
/// reading a non-event as a plaque costs four retries and a log line.
pub const EVENT_CHOICE_PRESENT: f64 = 0.75;

/// A solved shrine's `Pray`, which collects the blessing.
///
/// `ss(1.0, 0.9)` with `xOffset = -0.75` and the 250x100 `default` size (`shrine.lua:296-301`), so
/// the rect is exactly (1608,922)-(1858,1022) and the centre is (1733,972). Confirmed live: the
/// button rendered on that rect to the pixel.
///
/// ## The slot is shared, and the neighbour appears the moment you use it
///
/// `Consecrate` (`shrine.lua:241`), `Read` (`:302`) and `Desecrate` (`:320`) all sit at the same
/// `ss(1.0, 0.9)`, `xOffset -0.75`. This is not a hypothetical: praying **immediately** swaps the
/// slot to a greyed `Consecrate`, because `showConsecrateButton` (`shrine.lua:93-96`) admits the
/// button once `areaHasBeenUsed(key)` is true, while `activeIf` keeps it inactive at `hell == 0`.
/// A live capture one click apart shows `Pray`, then `Consecrate` in the same 250x100 rect.
///
/// So clicking this slot blind would press whatever happens to be there. Read it first.
///
/// ## Measured, one click apart on the same screen
///
/// ```text
///   Pray vs itself                        1.0000
///   Pray vs greyed Consecrate             0.8130   <- the real confusable
///   Pray vs the empty slot, pre-win       0.3542
///   Pray vs the empty slot, mid-puzzle    0.3552
/// ```
///
/// The empty-slot figures matter as much as the confusable: before `shrineView.hasWon()` the slot
/// holds nothing at all (`showPrayButton`, `shrine.lua:98-102`), so "no button" is the state we are
/// in for the whole puzzle and it must not read as a match.
pub const SHRINE_PRAY: Button = Button {
    name: "shrine Pray",
    template: "shrine-pray.png",
    search: (1600, 914, 1866, 1030),
    origin: (1608, 922),
    click: (1733, 972),
};

/// The class-unlock screen's `Continue`.
///
/// `ui/unlockscreen.lua` announces itself on the console as `Unlock screen:`, and it appears
/// mid-run — a live run finished a level 3 crypt and was shown *"The Cultist class is now
/// available."* Until now that was handled only inside `start_new_run`, which sends Returns through
/// the unlock chain on the way to hero select; encountered *during* a run it read as `Unknown`, which
/// the loop treats as "probably the map".
///
/// ## A fourth tenant of `ss(1, 0.9)`
///
/// This slot is the busiest in the game. Measured against every other occupant on the same exact
/// 250x100 rect:
///
/// ```text
///   the hero select confirm      0.8300
///   the shrine's `Pray`          0.8248
///   a greyed `Consecrate`        0.6445
///   the empty slot               0.1272
/// ```
///
/// So the bar has to clear 0.83, and [`UNLOCK_CONTINUE_PRESENT`] does. What makes that safe rather
/// than merely tight is that the *screens* are mutually exclusive and each is identified by something
/// outside this slot: hero select by its heading, the shrine by its back plaque. This is the last
/// question asked, not the first.
pub const UNLOCK_CONTINUE: Button = Button {
    name: "unlock Continue",
    template: "unlock-continue.png",
    search: (1600, 914, 1866, 1030),
    origin: (1608, 922),
    click: (1733, 972),
};

/// The class-unlock screen is up. See [`UNLOCK_CONTINUE`].
///
/// **0.90.** Above the 0.8300 hero-select confirm and the 0.8248 `Pray` that share the rect, and well
/// under a genuine reading — the label is a fixed string on a fixed plank, so a real one scores near
/// 1.0 the way `Pray` does. The margin below is 0.07, which is thinner than this project likes; it is
/// acceptable only because the two impostors live on screens that are recognised by other means
/// first, and it should be re-measured the moment a live unlock screen gives a real number.
pub const UNLOCK_CONTINUE_PRESENT: f64 = 0.90;

/// The hero select screen's heading, `Choose your champion:`.
///
/// `ui.objects.text.object("Choose your champion:", 0.5, 0.095, {font = 'title_type'})`
/// (`ui/heroselect.lua:316`), so it is centred at (960, 103) at 1920x1080. The template is a 700x100
/// band around it, cropped from a live capture.
///
/// ## Why the heading and not the button
///
/// [`HEROSELECT_CONFIRM`] cannot identify this screen, and the reason is measured rather than
/// suspected. Its label changes with the champion's class, and the shrine's `Pray` sits in the same
/// `ss(1, 0.9)` slot wearing the same plank:
///
/// ```text
///   genuine confirm, the class the template came from   1.0000
///   genuine confirm, a third class                      0.8751
///   the shrine's Pray                                   0.8438   <- in between
///   genuine confirm, a second class                     0.8156
/// ```
///
/// `Pray` lands **strictly inside** the range of genuine confirms. Any threshold admitting the
/// 0.8156 class admits `Pray` too, and any threshold rejecting `Pray` rejects that class — so no
/// number exists that separates them, and raising the bar was not an available fix. A live run was
/// told it was on hero select while standing in a shrine it had just prayed at.
///
/// The heading has none of that: it is a fixed string, drawn before any choice is made, and it does
/// not share its region with a control.
///
/// ## Measured
///
/// ```text
///   hero select, no champion chosen    1.0000
///   hero select, second class          0.9989
///   hero select, third class           0.9984
///   hero select, first class           0.9152
///   -------------------------------------------  gap
///   stats history                      0.4218
///   the main menu                      0.3519
///   a shrine                           0.1037
///   the overworld                      0.0225
/// ```
///
/// Note the first row. The button reads 0.1414 with no champion selected — it is *absent* until one
/// is chosen — so it could never have recognised the screen we most need to recognise: the one we
/// have just arrived on and not yet acted upon.
///
/// **Never clicked.** This is a text object with no handler; `click` is filled in only because
/// [`Button`] requires it, and the hero card click lives in `start_new_run`.
pub const HEROSELECT_HEADER: Button = Button {
    name: "hero select heading",
    template: "heroselect-header.png",
    search: (602, 47, 1318, 163),
    origin: (610, 55),
    click: (960, 105),
};

/// The hero select screen is up. See [`HEROSELECT_HEADER`].
///
/// ```text
///   genuine readings    cluster at 0.99, weakest 0.9152
///   the class unlock    0.8532                            tests/frames/unlock-woodsman.png
///   loudest impostor known before that   0.42
/// ```
///
/// **0.90**, raised from 0.80. The old bar sat in a gap 0.49 wide and was placed near the impostor
/// end of it on purpose, since the genuine readings clustered at 0.99. Then the class-unlock screen
/// turned up at **0.8532** — twice as loud as anything previously seen — and cleared it.
///
/// ## This is the second layer, and the weaker one
///
/// The real fix is ordering: [`identify`] asks about [`UNLOCK_CONTINUE`], which scores 1.0000 on
/// that frame, before it asks about this. A threshold cannot be the fix, because impostors are
/// unbounded — 0.8532 was unpredicted, and the next one could be 0.92. Raising the bar only removes
/// the impostors we have actually met.
///
/// ## Why raising it is nearly free, and the margin below is not the usual worry
///
/// 0.90 leaves just **0.0152** below the weakest genuine reading, which would normally be
/// unacceptably thin — and the doc for that outlier says it is "the sort of thing a cursor or a
/// notification produces", so a real hero select *can* be depressed by a transient overlay and land
/// under this bar.
///
/// That is affordable here, and only here, because the two errors are wildly unequal. Hero select is
/// reached in exactly one way — [`crate::navigate::start_new_run`] clicks through it, knowing it is
/// there from `Pregame screen:` on the console — so it is **never identified by sight**, and a false
/// negative costs nothing at all. A false positive, as today proved, costs the run.
pub const HEROSELECT_HEADER_PRESENT: f64 = 0.90;

/// The shrine word screen's `Go back`, bottom left.
///
/// `button('', 0.0, 0.9, {type = 'small', xOffset = 1.13})` (`shrine.lua:231-240`) with `small` at
/// 100x100 (`ui/elements/button.lua:19`), so the centre is `0 + 100*1.13 = 113`, `1080*0.9 = 972`
/// and the rect is exactly (63,922)-(163,1022).
///
/// Replaces a blind click at (120,968), which worked but proved nothing. It is `activeIf backMode`
/// and `showIf shrineLocation`, so it is absent on a shrine reached any other way — and leaving the
/// run parked inside a shrine is how the next iteration reads a map that is not on screen.
///
/// ## What it does and does not prove
///
/// It proves **a back plaque is at the bottom left**, not that we are on a shrine. The art is the
/// shared `ui/graphics/icons/back.png` on the shared `small` button, and `ss(0, 0.9)` is a popular
/// place to put one. So this is a *confirmation* that the button we intend to press is really there,
/// used only once we already believe we are on the shrine screen — never as a screen identifier.
///
/// ## Measured
///
/// ```text
///   an independent shrine frame, mid-puzzle, keyboard up   1.0000
///   the overworld at the same rect                         0.3437
/// ```
///
/// The positive control is worth more than usual here: it is a *different frame* from a different
/// moment — before the word was solved, with the on-screen keyboard up and a different scene state —
/// rather than the template compared with itself. 1.0000 across that gap is what makes 0.90 safe.
/// The shop's back arrow, which is how a run that buys nothing gets out again.
///
/// `shop.lua:180-187`: `button('', 0.5, 0.85, { type = 'small', icon = back.png, xOffset = 7.67 })`,
/// and `small` is 100x100 (`ui/elements/button.lua:19`). So the centre is
/// `0.5*1920 + 100*7.67 = 1727`, `0.85*1080 = 918`.
///
/// A bare coordinate rather than a [`Button`] with a template, because there is nothing here worth
/// matching: the icon is a 100x100 arrow shared with several screens, and the screen it sits on is
/// identified from the console instead — `core.onActive` prints `Opened shop UI` (`shop.lua:253`),
/// which is unambiguous and free.
///
/// **`activeIf = backMode`** (`:184`), and `backMode` is set when the shop was opened from somewhere
/// to go back to. Reached through a road event's `[Shop]` choice that is satisfied, so this is
/// pressable — but it is why the press is confirmed rather than assumed.
pub const SHOP_BACK: (i32, i32) = (1727, 918);

/// The console line `core.onActive` prints when a shop UI opens (`shop.lua:253`).
pub const SHOP_OPENED: &str = "Opened shop UI";

pub const SHRINE_GOBACK: Button = Button {
    name: "shrine Go back",
    template: "shrine-goback.png",
    search: (55, 914, 171, 1030),
    origin: (63, 922),
    click: (113, 972),
};

/// The shrine's back plaque is on screen. See [`SHRINE_GOBACK`]. **0.90**, against 0.3437 absent.
pub const SHRINE_GOBACK_PRESENT: f64 = 0.90;

/// The stats history page's return plaque, top right.
///
/// Reached by accident, which is the whole reason it needs a fingerprint: a live run finished a
/// shrine, cleared a text screen, and found itself here — then failed four locate-me probes at 0.15
/// each and stopped with `no pan dump after locate-me`, because it was looking for a map on a screen
/// that has none.
///
/// **Clipped by the top screen edge.** The visible face is (1756,0)-(1855,86); the button itself is
/// `small`, i.e. 100x100, so its upper 13 px are off-screen. The template is the visible part and the
/// click point is the centre of *that*, the same accommodation [`PROGRESS`] makes for being clipped
/// on the right.
///
/// ## The slot belongs to the options menu everywhere else
///
/// This is not a quiet corner. The overworld draws its **options** button on almost exactly this
/// rect, and the two are the same wood carrying different icons — measured at **0.6917** against each
/// other. Options is a screen this project treats as a trap: `Escape` is normally never sent
/// precisely because `backOptions` can strand a run in there with no map to read. So this button must
/// never be pressed on the strength of position alone.
///
/// ```text
///   the page's own return plaque       1.0000
///   the overworld's options button     0.6917   <- same slot, same material, different icon
/// ```
pub const STATS_BACK: Button = Button {
    name: "stats history back",
    template: "stats-back.png",
    search: (1748, 0, 1864, 95),
    origin: (1756, 0),
    click: (1806, 43),
};

/// The stats history page is up. See [`STATS_BACK`].
///
/// **0.90**, which is 0.21 clear of the options button that shares the slot. The project's rule of
/// thumb is that swapping the glyph on an otherwise identical button costs 0.138-0.184, so 0.6917 is
/// about what an icon swap should score and the bar is set above the whole band rather than just
/// above the one measurement.
pub const STATS_BACK_PRESENT: f64 = 0.90;

/// `Pray` is on screen, so the word is solved and the blessing is unclaimed. See [`SHRINE_PRAY`].
///
/// **0.92**, placed in the measured gap between a real `Pray` at 1.0000 and the greyed `Consecrate`
/// that replaces it at 0.8130 — 0.107 of margin below, nothing above.
///
/// The last open case is now **measured**, and the prediction held. An **active** `Consecrate`,
/// which only exists once `hell ~= 0`, was estimated at ~0.82 from the rule of thumb that a pure
/// word swap costs 0.138-0.184 scaling by length. Consecrating `shrine2` and `shrine3` on
/// 2026-08-08 scored **0.8560** on both — 0.036 above the estimate, and 0.064 below this threshold,
/// so `Pray` and an active `Consecrate` are correctly told apart here.
///
/// That margin is thinner than the others on this slot, which is why nothing consecrates on the
/// strength of it: identifying `Consecrate` positively needs its own artwork, and
/// [`SHRINE_SLOT_OCCUPIED`] plus a save-derived gate is what stands in until there is a template.
/// `spike-frames-live/slot-consecrate-live.png` is the capture to cut one from.
pub const SHRINE_PRAY_PRESENT: f64 = 0.92;

/// **Something** is in the shared right-hand slot, without claiming which button it is.
///
/// A deliberately weaker question than [`SHRINE_PRAY_PRESENT`], and the only one a template can
/// honestly answer for `Consecrate`: we have never captured one, so there is nothing to match it
/// against. What [`SHRINE_PRAY`]'s artwork *can* separate is occupied from empty, because a
/// same-size `default` button in the same rect scores far above bare background:
///
/// ```text
///   greyed `Consecrate`                   0.8130   measured
///   the empty slot, pre-win               0.3542   measured
///   the empty slot, mid-puzzle            0.3552   measured
///   an ACTIVE `Consecrate`                ~0.82    PREDICTED, see SHRINE_PRAY_PRESENT
/// ```
///
/// **0.60**, in the middle of a 0.458 gap between the two measured states. Wide enough that the
/// predicted figure being wrong by a lot still lands on the right side.
///
/// ## This is half a check, and the other half is the save
///
/// Occupancy alone must never authorise a click here — the slot's whole hazard is that four buttons
/// share it. It is paired with a save-derived gate in [`crate::shrineplay::consecrate`]: the game's
/// own `showPrayButton` requires `areaUnused` (`shrine.lua:98-102`), so at a shrine whose `_used`
/// flag is set, `Pray` **cannot** be the occupant; `Read` and `Desecrate` both need the
/// `spellSwears` gear flag (`:107,113`). The save says which button must be there; this says a
/// button is there at all. Neither is sufficient alone.
pub const SHRINE_SLOT_OCCUPIED: f64 = 0.60;

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
    /// A shrine's word screen, identified only by its back plaque. See [`SHRINE_GOBACK`].
    Shrine,
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
        Screen::Shrine,
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
    if over(&PREGAME_START, PREGAME_START_PRESENT) {
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
    // Also last, and for a weaker reason than the others: this is a back plaque at the bottom left,
    // and `ss(0, 0.9)` with the shared `back.png` is a popular place to put one. It is a genuine
    // screen identifier only because everything with a distinctive fingerprint has already been
    // ruled out above — so what it really means is "not one of those, and something here still wants
    // dismissing". Good enough to get off a screen, and explicitly not good enough to conclude a
    // shrine is underfoot.
    if over(&SHRINE_GOBACK, SHRINE_GOBACK_PRESENT) {
        return Screen::Shrine;
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
    Screen::Unknown
}

/// Every button, so the invariant tests cover all of them.
///
/// Exists because they did not. The geometry tests enumerated `[&CONTINUE, &PROGRESS]` by hand, so
/// the two buttons added later were checked by nothing — including the check that would have caught
/// their search boxes being smaller than their own templates. A hand-written list silently stops
/// covering the thing you just added, which is the moment coverage matters most.
pub const ALL: &[&Button] =
    &[
    &CONTINUE,
    &PROGRESS,
    &COMBAT_FINISH,
    &COMBAT_EULOGISE,
    &COMBAT_HUD,
    &REWARD_CONFIRM,
    &POSTGAME_CONTINUE,
    &MENU_START,
    &HEROSELECT_CONFIRM,
    &CHARACTER_STATS,
    &CHARACTER_BACK,
    &PREGAME_START,
    &SHRINE_PRAY,
    &SHRINE_GOBACK,
    &STATS_BACK,
    &HEROSELECT_HEADER,
    &UNLOCK_CONTINUE,
];

/// Start menu `Continue`. Measured on 52.3 at 1920x1080; `Restart` is the adjacent button at
/// x≈500 and eulogises the run, which is exactly why this is verified rather than assumed.
pub const CONTINUE: Button = Button {
    name: "menu Continue",
    template: "continue-button.png",
    // `default` 250x100 centred at (190, 812) -> exactly (65,762)-(315,862), grown by 8 of slack.
    // Was a 205x63 hand-crop with a guessed "nominal" origin, which is why it could not use
    // `score_exact` and had to be searched for.
    search: (57, 754, 323, 870),
    origin: (65, 762),
    click: (190, 812),
};

/// The same `Continue`, rendered highlighted.
///
/// Not a different button: identical origin, identical click. `default` buttons draw a hover layer
/// over the base image (`ui/elements/button.lua:83,128`), and reaching the main menu *through the
/// options menu* leaves this one highlighted — which is exactly the route the anomaly skip takes. So
/// the state the run meets is the one state no template covered: it scored 0.5726 against
/// [`CONTINUE`] on arrival, and matched cleanly only after a detour had cleared the highlight.
///
/// Alternative match rather than a lowered threshold. Dropping [`CONTINUE`] to 0.57 would admit
/// anything vaguely button-shaped in a slot whose neighbour, `Restart`, eulogises the run. Two
/// tight templates for two real renderings keeps the bar high for both.
///
/// Cut from `spike-frames-live/mainmenu-hover.png` at the button's own origin, so it is position-
/// locked in the same way: `score_exact` compares only the 250x100 rect at (65, 762), which is why
/// no threshold here can ever reach across to `Restart` at x≈500.
pub const CONTINUE_HOT: Button = Button {
    name: "menu Continue (highlighted)",
    template: "continue-button-hot.png",
    search: (57, 754, 323, 870),
    origin: (65, 762),
    click: (190, 812),
};

/// A **live** `Continue`, i.e. there is a save to resume.
///
/// Measured on the exact 250x100 crop:
///
/// ```text
///   live `Continue`, a save present     1.0000  err 0.0000
///   GREYED `Continue`, fresh save       0.7642  err 0.0936   <- must be rejected
///   plain overworld terrain             0.5138  err 0.2037
///   combat WaitPhase                    0.1548  err 0.3020
/// ```
///
/// 0.90 sits 0.10 under the true positive and 0.14 over the greyed one. The greyed case is the whole
/// point: on a fresh save the button is drawn but dead, so "is it there" is the wrong question and
/// "is it live" is the right one.
pub const CONTINUE_PRESENT: f64 = 0.90;

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

/// How far apart two choice plaques sit, in client pixels at a given client height.
///
/// `buttonY` advances by `actualI*0.15` per visible choice (`ui/eventscreen.lua:119`), so the pitch
/// is 15% of the client height — 162 px at 1080. This is what turns "where is the first plaque" into
/// "where is the n-th", which is all the position bookkeeping that remains once the match supplies
/// the anchor.
pub fn event_choice_pitch(client_h: i32) -> i32 {
    (client_h as f64 * 0.15).round() as i32
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

    /// Pins the crop's **turn-specificity**, which is a limitation rather than a defect.
    ///
    /// The template carries the numeral `1`, so a fight on a later turn scores below
    /// [`COMBAT_HUD_PRESENT`] — by design, since this is an entry signal. What must stay true is that
    /// it lands far above the non-combat frames rather than falling in among them: that gap is what a
    /// future "still in combat, any turn" check would be built on, and if it closes, the crop needs
    /// the numeral excluded rather than the threshold lowered.
    #[test]
    fn a_later_turn_reads_below_the_entry_bar_but_far_above_any_non_combat_screen() {
        let Some(turn_11) = score(&COMBAT_HUD, "combat-chest.png") else {
            eprintln!("SKIP: frame corpus not present");
            return;
        };
        assert!(
            turn_11 < COMBAT_HUD_PRESENT,
            "turn 11 scored {turn_11:.4}, at or above an entry-only bar — if the numeral has stopped \
             mattering, say so here rather than leaving the doc claiming it does"
        );
        let nearest_non_combat = score(&COMBAT_HUD, "16-selected.png").unwrap();
        assert!(
            turn_11 - nearest_non_combat > 0.3,
            "a later turn ({turn_11:.4}) should stand well clear of the nearest non-combat screen \
             ({nearest_non_combat:.4}); the margin is what a turn-independent check would use"
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
}
