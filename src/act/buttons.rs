//! Every button we are willing to click, and the score at which we believe we have found it.
//!
//! Lifted out of `act` on 2026-08-21 (#76). This file is **data and its provenance**: a
//! [`Button`] says where to look and where to press, and the `_PRESENT` constant beside each one
//! says how well the template must match before a press is allowed. The prose is most of the
//! weight and is the point — nearly every threshold here was moved at least once, and the comment
//! records the frames it was measured against and the confusable it has to clear.
//!
//! Two conventions worth knowing before editing:
//!
//! - A threshold lives **immediately below the button it guards**, not in a table of its own, so
//!   that a measurement and the thing it measures cannot drift apart.
//! - A bar is set against the nearest state it could be mistaken for, never against bare
//!   background. Two of these were once set from negative controls that happened to be dark
//!   screens, and both admitted plain brown map ground.
//!
//! The machinery that reads all this — [`identify`](super::identify), [`locate`](super::locate),
//! [`score_exact`](super::score_exact) and the click helpers — stays in the parent.

// Named by the doc comments throughout; none of them is called from here.
#[allow(unused_imports)]
use super::{
    click_when_ready, event_plaque_score, identify, locate, score_exact, slot_is_eulogise,
};

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
/// **0.95, and the 0.99 it replaces cost a run.**
///
/// The old value came with the right reasoning and the wrong number: turn 1 against a turn-1
/// template measures 1.0000, the region is immune to the vignette, *but one frame is not a steady
/// state*, so 0.01 was left as slack in case the HUD moved. Live 2026-08-09 it moved by **0.018**.
///
/// Leaving a village walked into a highwayman, the fight opened at turn 1, and this scored
/// **0.9820** — measured offline against `spike-frames-live/gave-up.png` with `inlier_probe`, so it
/// is a number and not a story. `identify` returned `Unknown`, the driver stayed on the map path,
/// and the run spent its retries clicking empty ground at (1750, 160) while a full board sat on
/// screen. Word count 0, kill count 0.
///
/// The template was cut in a crypt and this fight was in a night swamp; the background shows around
/// the `?` and turn plaques, which is the variation nobody had sampled.
///
/// **0.95 costs no separation.** The nearest non-combat frame in the corpus scores 0.8894, so the
/// gap between "a fight" and "not a fight" is still 0.06 wide on the low side and 0.03 on the high.
/// What it does give up is the old promise that this fires *only* on turn 1 — a two-character
/// numeral is a fraction of a 225x100 region, so a later turn may now match too. That is the right
/// trade: the question worth answering is "is a fight on screen", and answering it only on the
/// first turn is how a run walks past one.
pub const COMBAT_HUD_PRESENT: f64 = 0.95;

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
///
/// **That caveat is why this is [`MAX_PRESENT`] and no longer 0.97.** It was written predicting a
/// refusal around 0.96 from a tint nobody had photographed, and the same shape of prediction came
/// true elsewhere the same week: [`COMBAT_HUD_PRESENT`] sat at 0.99 on identical reasoning and
/// missed a live fight at 0.9820, leaving a run stranded in front of a full board. Anticipating the
/// failure in a comment is not the same as leaving room for it.
///
/// The separation is unharmed: 1.0000 against 0.8639 greyed leaves 0.086 below the bar, and the
/// `Finish`-versus-`Eulogise` question is settled by [`slot_is_eulogise`]'s argmax rather than here.
pub const COMBAT_FINISH_ACTIVE: f64 = MAX_PRESENT;

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
/// True positive 1.0000, nearest confusable 0.8527, so anything from 0.90 to 0.99 separates. It was
/// 0.97 on the reasoning that "exact bounds make a near-1.0 bar free" — the same reasoning that put
/// [`COMBAT_HUD_PRESENT`] at 0.99, where a live fight scored 0.9820 and was missed. Free is the
/// wrong word for it: a bar that tight buys no separation and spends the margin that absorbs a
/// background nobody sampled.
///
/// Held at [`MAX_PRESENT`] now, with 0.12 still between it and the confusable. The real
/// discrimination here is argmax between the two templates ([`slot_is_eulogise`]), not this bar.
pub const COMBAT_EULOGISE_PRESENT: f64 = MAX_PRESENT;

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

/// The same button with the game's **hotspot highlight** on it.
///
/// ## Why a second template rather than a lower bar
///
/// Opening the pregame moves the pointer onto `Start` and lights it:
/// `input.setHotspotHighlight` calls `love.mouse.setPosition(unpack(hotspot))`
/// (`utils/input.lua:94-96`). The highlighted artwork scores **0.4972** against [`PREGAME_START`],
/// measured twice — at `l14` and at `l48_plaza` on 2026-08-14, identical to four decimals, which is
/// what one artwork in one state looks like.
///
/// No threshold can cover that. 0.4972 sits *below* impostors already seen on other screens — the
/// hero-select heading at 0.6049 and `Progress` at 0.5193 on those very frames — so a bar low enough
/// to admit a hot `Start` would admit two screens that are not the pregame at all. The states differ
/// by more than the screens do, which is precisely the case for matching both rather than loosening
/// one.
///
/// The same argument, and the same shape, as [`CONTINUE_HOT`]: a state we cannot summon on demand,
/// cropped out of a saved frame with `crop_template` rather than captured live.
///
/// Cut from `spike-frames-live/combat-no-screen-5.png`, the run's own photograph of a pregame it
/// could not name. Kept as `tests/frames/pregame-hot.png` so the pair is measurable.
pub const PREGAME_START_HOT: Button = Button {
    name: "pregame Start (hot)",
    template: "pregame-start-hot.png",
    search: (827, 984, 1093, 1080),
    origin: (835, 992),
    click: (960, 1035),
};

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

/// The console line `onActive` prints when the shrine screen becomes active (`shrine.lua:431-432`).
///
/// **Unconditional, and that is what makes it usable.** The `print` sits one line *above*
/// `if not activeGameTypeIs'main' then return end`, so it announces every activation of the mode —
/// including the return trip after a consecration beam hands the screen back via `setActiveMode`.
///
/// Preferred over [`SHRINE_GOBACK`] for confirming that a `Visit` press landed, because that plaque
/// answers "is there a way out of here" and several screens have one; this answers "the shrine
/// screen just opened" and nothing else prints it. The trailing colon is the game's, not a typo —
/// [`crate::observe::feed::Feed::seen_line_since`] matches whole lines.
pub const SHRINE_SCREEN: &str = "Shrine screen:";

/// A back plaque at `ss(0, 0.9)` — **and the shrine is only one of the screens that has one**.
///
/// The inn declares the same button from the same `back.png` (`ui/inn.lua:68-71`), so does the rest
/// screen (`ui/rest.lua:517-520`), and the click points land a pixel apart at (113, 972) and
/// (112, 972) — `100*1.13` is `112.99999999999999` and `button_center` truncates. Two routes to one
/// button, which is a useful cross-check (`innplay::the_two_buttons_we_press_are_where_the_game_puts
/// _them` pins it) and a warning: **this template cannot tell a shrine from an inn.**
///
/// That is why `identify` asks it last, below everything that names a screen. It is a generic
/// "take me back" matcher — the answer to *there is nothing else to do here, how do I leave* — and
/// never evidence about what the screen is. See the comment at its call site.
/// `Consecrate`, in the shared right-hand slot. Same rect as [`SHRINE_PRAY`], which is the point:
/// the slot holds one of several buttons and this identifies *which*, where occupancy could not.
///
/// Cut from `spike-frames-live/slot-consecrate-live.png`, captured by hand on 2026-08-10 because the
/// state cannot be summoned on demand — it needs a solved, unconsecrated, uncorrupted shrine with
/// the portal open.
pub const SHRINE_CONSECRATE: Button = Button {
    name: "shrine Consecrate",
    template: "shrine-consecrate.png",
    search: (1600, 914, 1866, 1030),
    origin: (1608, 922),
    click: (1733, 972),
};

/// An **active** `Consecrate` is on screen and will do something if pressed.
///
/// Measured against every state we hold a frame for:
///
/// | frame | inliers |
/// |---|---|
/// | active `Consecrate` | **1.0000** |
/// | greyed `Consecrate` | **0.8564** |
/// | inn screen | 0.8073 |
/// | death / overworld | 0.7976 |
/// | combat board | 0.7865 |
/// | murder warning | 0.1175 |
///
/// **0.95**, with 0.0936 of margin above the nearest confusable — a greyed `Consecrate`, which is
/// the same word in the same rect and differs only by tint. That is wider than the 0.107 gap
/// [`SHRINE_PRAY_PRESENT`] was set on, and the dev's figure: 0.95 has historically survived the
/// cursor accidentally sitting over a button, which is the failure a tighter threshold invites.
///
/// **The 1.0000 is a positive control and nothing more** — the template was cut from that exact
/// frame, so it is guaranteed and proves only that the matcher works. What is *not* yet measured is
/// an active `Consecrate` at a different shrine, against different scenery. If that comes in low, the
/// top end is what moves, not this reasoning.
///
/// Note what this cannot do, because the temptation is real: it does not separate `Consecrate` from
/// `Pray` by strength of evidence alone. The `Pray` artwork scores 0.8560 against an *active*
/// `Consecrate` — four ten-thousandths from what this template scores against a *greyed* one — so
/// the two questions must be asked with their own templates, never inferred from one score.
pub const SHRINE_CONSECRATE_PRESENT: f64 = 0.95;

/// The general store's `Sell` plaque, which is how we know the shop screen is up.
///
/// Cut from `spike-frames-live/shop-open.png`, the frame the run of 2026-08-15 stopped on at
/// Rowlston Covert with `Stop::AtShop`. The plaque is chrome — fixed position, no state — which is
/// exactly what a screen fingerprint wants, and it is the only large text on the screen that is not
/// an item name.
///
/// **Nothing presses it.** Selling is not something this program does; this identifies the screen
/// and no more. The item grid is aimed at arithmetically (see [`crate::shopplay`]) and the back
/// arrow at the other end of the bar is what leaves.
pub const SHOP_SELL: Button = Button {
    name: "shop Sell",
    template: "shop-sell.png",
    search: (128, 852, 418, 980),
    origin: (148, 872),
    click: (273, 916),
};

/// The shop screen is up. See [`SHOP_SELL`].
///
/// **0.90**, the same bar as the other chrome plaques and for the same reason: it is a fixed image
/// over a fixed background, so a genuine match scores near 1.0 and the only thing the threshold has
/// to survive is capture noise. Unmeasured against a confusable, because there is no other `Sell` in
/// the game's chrome — if one turns up, this needs re-measuring against it rather than lowering.
pub const SHOP_SELL_PRESENT: f64 = 0.90;

/// The **same plank** as [`SHOP_SELL`], reading `Inventory`, which is what a shop that does not buy
/// puts there.

/// `shop.lua:188-200` defines the two at one anchor and switches them on `canSellTo`:
///
/// ```lua
/// require'ui.elements.button'('Sell',      0.5, 0.85, { xOffset = -2.768, showIf = function() return canSellTo end }),
/// require'ui.elements.button'('Inventory', 0.5, 0.85, { xOffset = -2.768, showIf = function() return not canSellTo end }),
/// ```
///
/// **This cost a run.** The `Woodsman` at `Bempton Silva road` sells and does not buy, so his shop
/// drew `Inventory`; [`SHOP_SELL`] scored **0.8061** against its 0.90 bar, `identify` answered
/// `Unknown`, and the driver hunted for a map that was not there until it gave up
/// (2211Z, 2026-08-23). The plank artwork is identical and only the word differs, which is exactly
/// why it scores high enough to look like noise rather than a different screen.
///
/// **Third time for this shape.** `Reveal`/`Teleport` share the tower's slot
/// ([`crate::tower::REVEAL`]) and the overworld's `(0, 0.85)` has done it before
/// ([`crate::observe::affirm::SHOW_AREA_BUTTONS`]). There the hazard is pressing the wrong one;
/// here it is failing to recognise the screen at all. A shared anchor with a `showIf` switch is
/// worth checking for whenever a fingerprint is a plank with a word on it.
///
/// Cut from `spike-frames-live/gave-up.png`, the frame that run stopped on, at the identical rect
/// [`SHOP_SELL`] uses.
pub const SHOP_INVENTORY: Button =
    Button { name: "shop Inventory", template: "shop-inventory.png", ..SHOP_SELL };

pub const SHRINE_GOBACK: Button = Button {
    name: "shrine Go back",
    template: "shrine-goback.png",
    search: (55, 914, 171, 1030),
    origin: (63, 922),
    click: (113, 972),
};

/// The shrine's back plaque is on screen. See [`SHRINE_GOBACK`]. **0.90**, against 0.3437 absent.
pub const SHRINE_GOBACK_PRESENT: f64 = 0.90;

/// `Cancel` on the **avoidable-murder confirmation**, which is a modal over the combat board.
///
/// `wordboard.lua:499-508`: submitting an attack routes through
///
/// ```lua
/// if userConfig.interface.murderWarning and not murderConfirm and looksLikeAvoidableFirstMurder()
/// ```
///
/// and `looksLikeAvoidableFirstMurder` (`:488-497`) needs `getEstimationEnemyAboutToDie()` **and**
/// the enemy carrying `fear`, `terror` or `caution`. So it appears exactly when we are about to kill
/// something we could have frightened off — the case [`crate::search::Goal::Scare`] exists to avoid,
/// which means it firing is evidence our damage model and the game's estimate have disagreed.
///
/// The disagreement has at least two sources and we do not need to tell them apart to back out
/// safely: the game's estimate is of *death*, "possibly by status" (`rpgview.lua:915`), so a
/// damage-over-time effect can finish an enemy our arithmetic left alive; and `killing_blow`'s
/// frugal branch deliberately aims at `lethal..lethal+2` to control overkill, which is a kill even
/// though it is spelled as a band.
///
/// Cut from `spike-frames-live/gave-up.png`, which is the frame a live run stopped on with
/// `BoardNeverSettled` — a modal is what a board that never settles looks like from underneath.
/// The plaque is opaque, so the blurred scene behind the dialog does not reach the template.
///
/// ## This does not detect the dialog, and must not be used to
///
/// It used to. A single `locate` fired straight after the Space press, and it missed a live one —
/// not because the artwork or the search box are wrong, but because `setActiveMode`
/// (`main.lua:191-195`) cross-fades for **0.625s** and the look landed mid-fade. Measured against
/// the frame that run stopped on, this template scores **1.0000** at its own origin. A threshold
/// change would not have helped and neither would a better crop.
///
/// Detection is the console's job now — see [`crate::fight::Fight::back_out_of_murder`]. What is
/// left here is *confirmation*: once the feed says the dialog opened, this is polled until it stops
/// matching, which is how we know the Backspace landed. That is the role a template is good at,
/// because by then we already know what should be on screen and are willing to wait out the fade.
pub const MURDER_CANCEL: Button = Button {
    name: "murder Cancel",
    template: "murder-cancel.png",
    search: (984, 564, 1251, 676),
    origin: (992, 572),
    click: (1117, 620),
};

/// The overworld area-button slot showing **`Combat`**.
///
/// The one thing this answers: *is the selected node one we must fight rather than walk onto?*
///
/// A forest subnode holding enemies offers `Combat` **and nothing else** —
/// `return subnodeHasEnemies(location) and combatButton or location.typeData.areaButtons`
/// (`overworld/generators/forest.lua:86-87`), where `subnodeHasEnemies` reads the parent's area flag
/// (`:6-11`). There is no `Travel` on that node to press. So a run that presses `Travel` at the slot
/// coordinate presses nothing, nothing moves, and no console line says why.
///
/// That is exactly how the run of 2026-08-16 ended. `l4` was a level 1 spider forest when we first
/// crossed it and every subnode was peaceful; we came back after the anomaly opened, corruption had
/// put enemies back on the roads, and steps 97-101 walked `l4sub13 -> l4sub5 -> l4sub1 -> l4_plaza
/// -> l4sub3` pressing `Travel` at a slot that read `Combat`, then stopped with `no arrival`. The
/// stop frame (`spike-frames-live/gave-up.png`) shows `Bainton Clump — level 6 crossroads` selected
/// with `Combat` in the slot, which is where this template is cut from.
///
/// ## Why a template and not the console
///
/// Because the console cannot answer it. `overworldview.lua` prints the adjacency dump
/// (`:1025-1053`) and nothing else — no line names the selected location's buttons, and selection
/// itself (`:475`, `:1493`) is silent. The dev asked for a console check *or* a template check; only
/// one of the two exists.
///
/// ## Calibrated against the confusable planks, not against the map
///
/// Every area button is the same wooden plank at the same coordinate with different lettering, so
/// the background matches perfectly and only the glyphs can separate them — the same trap as
/// `Finish` versus `Give up` on [`COMBAT_FINISH`]. Measured with `inlier_probe` against the slot
/// captures a live run left behind:
///
/// ```text
/// area-combat.png vs itself                  1.0000
/// area-combat.png vs Explore                 0.8731
/// area-combat.png vs Visit                   0.8616
/// area-combat.png vs Combat, greyed out      0.7367
/// ```
///
/// [`AREA_BUTTON_SHOWING`] sits at 0.95, in the middle of the measured gap. Note the greyed `Combat`
/// scoring *lowest* of the three: an inactive button is a bigger pixel difference than a different
/// word, which is the right way round — a `Combat` we cannot press is not a `Combat` worth pressing.
pub const AREA_COMBAT: Button = Button {
    name: "area Combat",
    template: "area-combat.png",
    search: (54, 860, 320, 976),
    origin: (62, 868),
    click: (187, 918),
};

/// How well the area-button slot must match before we believe what it says.
///
/// Placed in the gap measured on [`AREA_COMBAT`]: nearest confusable 0.8731, exact 1.0000.
pub const AREA_BUTTON_SHOWING: f64 = 0.95;

/// How well the slot must match [`AREA_COMBAT`] before we believe **a live area button** is in it,
/// whatever word is written on the plank.
///
/// A second, much lower bar on the same measurement, and it answers a different question. The gate
/// above asks *which* button; this asks *whether one is pressable at all*, which is the question the
/// dead press at `l38` turned on — the slot held a greyed `Combat`, the observer scored it
/// **0.7367**, and the press went out against a bar of 0.95 that could not tell that reading from
/// any other miss.
///
/// **Greying is a bigger pixel difference than the lettering is**, which is what makes one number
/// able to answer both questions.
///
/// ## Measured twice: on the frame corpus, and on 298 readings a run actually logged
///
/// The corpus, same probe as [`AREA_BUTTON_SHOWING`]:
///
/// ```text
/// live Combat  (the template itself)  1.0000
/// live Explore                        0.8731
/// live Visit                          0.8616
/// greyed Combat                       0.7367
/// ```
///
/// And every `area slot:` line the runs of 2026-08-17 to 08-21 printed, which is a far better
/// picture than four captures and was sitting unread in `spike-run-*.md`:
///
/// ```text
///   23 x 1.0000   live `Combat`
///   16 x 0.8731   live `Explore`
///  244 x 0.8566   live `Travel`, from every crossing press in five runs
///    6 x 0.8458   live `Open`, a chest with nothing guarding it
///    5 x 0.8355   live, a village subnode — `Enter` or `Attack`   <- the worst live reading
/// ----------------------------------------------------------------  the gap
///    1 x 0.7367   the greyed `Combat` at `l38` that ended run 0251Z
///    3 x <0.27    nothing recognisable at all
/// ```
///
/// So the populations are cleanly separated by about 0.10, and 0.79 is near the midpoint of
/// **0.7367** and **0.8355** (0.7861): 0.053 over the greyed reading and 0.046 under the worst live
/// one. Both beat the 0.05 margin against the corpus figures that
/// `a_greyed_plank_reads_lower_than_any_live_one` holds it to.
///
/// ## It is still a warning and not a veto
///
/// **This paragraph used to say `Travel` and `Open` had never been captured. They had — 250 times
/// between them, in logs nobody had looked at.** What remains true is thinner: the live population
/// is five words, `Rest` and `Wake up` are not among them, and the only greyed sample is `Combat`.
/// A greyed *different* word must score lower than a greyed `Combat` — the lettering disagrees as
/// well as the alpha — so the gate is safe in that direction, and it is the live side that is
/// unbounded below.
///
/// So a reading under the bar makes the caller look again and re-select, and then press regardless.
/// Being wrong costs the press we would have made anyway, which is the only direction a bar with an
/// open-ended population may fail in. What would close it is a greyed `Visit` and a greyed `Travel`
/// in the corpus; the live side is now well enough sampled to stop worrying about.
pub const AREA_BUTTON_LIVE: f64 = 0.79;

/// The inn's `Rest`, on the inn screen.
///
/// `button('Rest', 1, 0.9, { xOffset = -2 })` (`ui/inn.lua:55`) with the 250x100 `default` size, so
/// the centre is (1420, 972) and the template's top-left is (1295, 922). Cross-checked against
/// [`crate::innplay::INN_REST`], which computes the same centre from the same declaration — two
/// routes to one coordinate, the way [`SHRINE_GOBACK`] is cross-checked against `innplay::BACK`.
///
/// **Why this exists at all.** The press used to be blind: compute the centre, fire, and hope. It
/// failed live on 2026-08-09 and the log could not say whether the button was absent, not yet live,
/// or somewhere else — "were we clicking blindly rather than letting the observer tell us the button
/// exists" was the dev's question, and the answer was yes. [`click_when_ready`] is the primitive
/// that was always the right one here: it separates *not there yet* from *not there*, which is
/// precisely the distinction that failure turned on.
///
/// Cut from a screenshot the dev took by hand, because the state cannot be summoned on demand from
/// a test — same provenance as `continue-button-hot.png`, and the reason `crop_template` exists. The
/// plaque is opaque, so the inn's interior behind it does not reach the template.
pub const INN_REST: Button = Button {
    name: "inn Rest",
    template: "inn-rest.png",
    search: (1287, 914, 1553, 1030),
    origin: (1295, 922),
    click: (1420, 972),
};

/// The rest screen's own `Rest`, which is a **readiness check** and not just a location.
///
/// `button('Rest', 0.5, 0.9)` (`ui/rest.lua:504`) — screen centre, (960, 972). Note this is a
/// different button from [`INN_REST`] on a different screen, which is what makes a stray press at
/// the inn's coordinate harmless once the rest screen is up: (1420, 972) is empty here.
///
/// Unlike most buttons in this file, this one has a live `activeIf`:
///
/// ```lua
/// activeIf = function()
///     if doingEvent then return end
///     return canRest
/// end
/// ```
///
/// So it is drawn inactive while a dream is running and whenever the inn will not serve us — and
/// matching the *active* artwork answers "may we press Space now", which nothing else can. The
/// console says a rest screen exists; it does not say the button on it is live.
///
/// The `-10` in the template is the inn's fixed price (`crate::rest::INN_COST`, `ui/rest.lua:49`),
/// so it is part of the artwork rather than a variable. A campfire charges fuel and would need its
/// own template; this is only ever used at an inn.
pub const REST_CONFIRM: Button = Button {
    name: "rest Rest",
    template: "rest-confirm.png",
    search: (827, 914, 1093, 1030),
    origin: (835, 922),
    click: (960, 972),
};

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

/// Every button, so the invariant tests cover all of them.
///
/// Exists because they did not. The geometry tests enumerated `[&CONTINUE, &PROGRESS]` by hand, so
/// the two buttons added later were checked by nothing — including the check that would have caught
/// their search boxes being smaller than their own templates. A hand-written list silently stops
/// covering the thing you just added, which is the moment coverage matters most.
pub const ALL: &[&Button] = &[
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
    &PREGAME_START_HOT,
    &SHRINE_PRAY,
    &SHRINE_CONSECRATE,
    &SHRINE_GOBACK,
    &SHOP_SELL,
    &SHOP_INVENTORY,
    &STATS_BACK,
    &HEROSELECT_HEADER,
    &UNLOCK_CONTINUE,
    &INN_REST,
    &REST_CONFIRM,
    &MURDER_CANCEL,
    &AREA_COMBAT,
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

/// **No `_PRESENT` bar may sit above this.** The dev's rule, and it is about frame variation rather
/// than about any one screen.
///
/// A threshold near 1.0 asserts that a region is pixel-identical between the frame the template was
/// cut from and every frame it will ever meet. That has only ever been checked on one frame, and one
/// frame is not a steady state: backgrounds differ by biome, plaques animate, the vignette moves.
/// Twice now a bar was set near 1.0 with the note that "exact bounds make it free" — and on
/// 2026-08-09 the tighter of the two, [`COMBAT_HUD_PRESENT`] at 0.99, missed a live fight at 0.9820
/// and the run walked past a full board reporting a navigation error.
///
/// 0.95 costs nothing anywhere it applies. Every threshold in this file has its true positive at
/// 1.0000 and its nearest confusable measured, and the tightest of those gaps is still 0.12 wide.
/// What the cap buys is that the *slack* is always larger than the frame-to-frame variation nobody
/// has sampled yet.
///
/// It is a floor on slack, not a substitute for measurement: a threshold still has to sit in a
/// measured gap against the nearest state it could be mistaken for. `every_threshold_leaves_room_
/// for_a_frame_we_have_not_seen` enforces the cap; the per-button docs carry the gaps.
pub const MAX_PRESENT: f64 = 0.95;

/// How far apart two choice plaques sit, in client pixels at a given client height.
///
/// `buttonY` advances by `actualI*0.15` per visible choice (`ui/eventscreen.lua:119`), so the pitch
/// is 15% of the client height — 162 px at 1080. This is what turns "where is the first plaque" into
/// "where is the n-th", which is all the position bookkeeping that remains once the match supplies
/// the anchor.
pub fn event_choice_pitch(client_h: i32) -> i32 {
    (client_h as f64 * 0.15).round() as i32
}

#[cfg(test)]
mod tests {
    use super::*;
    // The template loader stays with the machinery in the parent; these tests read templates
    // to check each search box can actually contain the one it is looking for.
    use crate::act::template_path;
    use crate::observe::template::Template;

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

    /// Buttons allowed within the forbidden radius, and the reason each is allowed.
    ///
    /// **`inn Rest` — (1420, 972), 38 px from the slot.** The game puts it there; `ui/inn.lua:55`
    /// is `('Rest', 1, 0.9, { xOffset = -2 })` and combat's `Hint`/`Fight on!` is normalized
    /// (0.72, 0.9). Nothing about that is ours to move.
    ///
    /// What makes it safe is that this button is **only ever clicked through
    /// [`click_when_ready`]**, which will not fire until the `Rest` plaque itself matches at
    /// [`MIN_INLIERS`]. A combat screen cannot produce that match: the artwork is a different
    /// plaque with different lettering. So the coordinate is only reached on a screen that has
    /// already identified itself, which is the property this test is a blunt proxy for.
    ///
    /// **What would invalidate this.** Any caller that clicks `INN_REST.click` directly, or reaches
    /// that coordinate through `Run::click_button`, which fires at arithmetic and recognises
    /// nothing. If you are adding one, you are removing this exemption's only justification.
    const RECOGNISED_ONLY: &[&str] = &["inn Rest"];

    #[test]
    fn the_forbidden_slot_is_not_reachable_through_any_button() {
        // Normalized (0.72, 0.9) is `Hint` or `Fight on!` depending on state, and Fight on commits
        // to another enemy. No Button may target it. At 1920x1080 that is about (1382, 972).
        const FORBIDDEN: (i32, i32) = (1382, 972);
        for b in ALL {
            if RECOGNISED_ONLY.contains(&b.name) {
                continue;
            }
            let d = ((b.click.0 - FORBIDDEN.0).pow(2) + (b.click.1 - FORBIDDEN.1).pow(2)) as f64;
            assert!(d.sqrt() > 120.0, "{} clicks too close to the Fight on! slot", b.name);
        }
    }

    /// The exemption list is not a place to park a button that merely fails the test.
    ///
    /// Every name on it must exist, so a rename cannot silently turn an exemption into a
    /// no-op — which would leave the guard passing for a button nobody is checking.
    #[test]
    fn every_exempt_button_still_exists() {
        for name in RECOGNISED_ONLY {
            assert!(
                ALL.iter().any(|b| &b.name == name),
                "{name} is exempt from the forbidden-slot check but is not a Button any more"
            );
        }
    }
}
