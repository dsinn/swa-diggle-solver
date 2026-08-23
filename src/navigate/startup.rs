//! Getting a run to the overworld: the cinematic, hero select, the champion, the first shop.
//!
//! Lifted out of `navigate` on 2026-08-21 (#76), unchanged. What these four share is that they run
//! **once**, before [`drive`](super::drive)'s loop has anything to steer — and that every one of
//! them is a screen the game shows only at the start of a profile, which is why they are the least
//! watched code in the driver. `start_new_run` in particular needs a *fresh* profile to reach at
//! all, because `ui/heroselect.lua:271` eulogises the current save to begin one.

use super::{Run, NEUTRAL};
use crate::win::input::{warp_cursor, SC_RETURN, VK_RETURN};
use std::path::Path;
use std::time::{Duration, Instant};

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
pub(super) fn skip_cinematic(r: &mut Run) -> Result<(), String> {
    use crate::win::input::{SC_ESCAPE, VK_ESCAPE};
    let before = crate::win::capture::capture_window(r.win)
        .map_err(|e| format!("capture before Escape: {e}"))?;
    r.keys.focus();
    std::thread::sleep(crate::timing::FOCUS_SETTLE);
    if !r.tap_key("skipping the cinematic: escape", VK_ESCAPE, SC_ESCAPE) {
        return Err("Escape failed".to_string());
    }
    r.park();
    std::thread::sleep(crate::timing::AFTER_MODE_CHANGE);
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
    std::thread::sleep(crate::timing::AFTER_MODE_CHANGE);
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
        std::thread::sleep(crate::timing::HOVER_DWELL);
    }

    // Verified, not blind: this slot reads `Restart` when it is not `Continue`, and `Restart`
    // eulogises the run.
    crate::act::click_exact(
        r.win,
        &crate::act::CONTINUE,
        crate::act::CONTINUE_PRESENT,
    )
    .map_err(|e| format!("no Continue on the main menu: {e}"))?;
    std::thread::sleep(crate::timing::AFTER_RESUME);
    r.pump();
    Ok(())
}

/// Turns shop pages until `index` is on screen, and reports the offset actually reached.
///
/// **Every press is verified by watching the grid change, and that is the whole design.** The shop
/// has no confirmation dialogue — `ui/elements/shopitem.lua:147-165` buys on release — so clicking a
/// slot whose page we are not sure of is a purchase of whatever is sitting in it. Nor is the console
/// any help: `refreshButtons` prints nothing, and the inventory is dumped once, in `onActive`
/// (`shop.lua:248-256`). The screen is the only witness a page turn has.
///
/// The comparison is deliberately lopsided. [`inliers_between`] returns 1.0 for two identical
/// captures, and a page of different items is different artwork end to end — so the bar is set at
/// **half**, far below anything a still screen could drift to and far above what a real turn would
/// score. Being wrong in the cautious direction costs a purchase we could have made; being wrong the
/// other way spends a hundred gold on something we did not choose.
///
/// A press that changes nothing is the ordinary way the right arrow ends: `activeIf` is
/// `(relativeIndex+8)<#shopInventory` (`shop.lua:215`), so it goes inert on the last page rather
/// than refusing audibly.
///
/// **Starting from offset zero is checked, not assumed.** `relativeIndex` is a module-local that
/// outlives any one visit, so a shelf left on page two would put every click one page out. It does
/// not happen: `core.load` opens with `relativeIndex = 0` (`shop.lua:360`), and the event shop's
/// `core.tempshop` does the same (`:314`). Only ever paging right follows from that.
pub(super) fn page_the_shop_to(r: &mut Run, index: usize) -> usize {
    let want = crate::shopplay::page_of(index);
    let mut at = 0usize;
    if want == 0 {
        return at;
    }
    let (Some(region), Some((ax, ay))) = (
        crate::shopplay::grid_region(r.win),
        crate::shopplay::arrow_at(r.win, crate::shopplay::ARROW_RIGHT),
    ) else {
        r.log.push_str("  cannot place the shop's paging arrow\n");
        return at;
    };
    let shot = |r: &Run| {
        crate::win::capture::capture_client_rect(r.win, region.0, region.1, region.2, region.3).ok()
    };
    while at < want {
        let Some(before) = shot(r) else {
            r.log.push_str("  could not photograph the shelf before paging\n");
            return at;
        };
        let Ok((sx, sy)) = r.win.client_to_screen(ax, ay) else { return at };
        if !r.tap("clearing the gate", sx, sy) {
            r.log.push_str("  the paging arrow could not be clicked\n");
            return at;
        }
        r.park();
        std::thread::sleep(crate::timing::SCREEN_DISSOLVE);
        let Some(after) = shot(r) else {
            r.log.push_str("  could not photograph the shelf after paging\n");
            return at;
        };
        let same = crate::observe::template::inliers_between(&before, &after);
        if same >= SHOP_PAGE_TURNED {
            r.log.push_str(&format!(
                "  the shelf did not change after the paging arrow ({same:.4}) — stopping at \
                 offset {at}\n"
            ));
            return at;
        }
        at += crate::shopplay::PER_PAGE;
        r.log.push_str(&format!("  paged right to offset {at} (the shelf changed, {same:.4})\n"));
    }
    at
}

/// How alike two captures of the shop's grid may be and still count as the same page.
///
/// See [`page_the_shop_to`] — 0.5 is not a measured boundary between two states, it is a floor
/// chosen so that only an unmistakable change counts. Nothing here is calibrated against a
/// confusable, because the confusable is *no change at all*, which reads 1.0000.
const SHOP_PAGE_TURNED: f64 = 0.5;

/// Reads the three champion cards and returns the client x of the one to click.
///
/// See [`crate::heroselect`] for what is read and why that row. This is the part that has to survive
/// the reading failing: a screen where nothing is recognised is not an error, it is the middle card
/// — which is exactly where this function's predecessor clicked every time.
///
/// The one refusal is a screen with no playable champion on it at all, which stops the run rather
/// than starting a class the router has never been written for.
fn pick_a_champion(r: &mut Run, game_dir: &Path) -> Result<i32, String> {
    /// Reads before giving up on the screen still drawing. The cards fade in, and a card read
    /// mid-fade is a card whose icons are blended with the backdrop and match nothing — the
    /// project's most repeated bug wearing a new hat.
    const READS: usize = 3;
    let centres = crate::heroselect::card_centres();
    let middle = centres[centres.len() / 2];

    let catalogue = match crate::items::Catalogue::load(game_dir) {
        Ok(c) => c,
        Err(e) => {
            r.log.push_str(&format!("  no item catalogue ({e}) — falling back to the middle card\n"));
            return Ok(middle);
        }
    };

    let (bx, by, bw, bh) = crate::heroselect::passives_band();
    let mut cards = Vec::new();
    for attempt in 1..=READS {
        let frame = match crate::win::capture::capture_client_rect(r.win, bx, by, bw, bh) {
            Ok(f) => f,
            Err(e) => {
                r.log.push_str(&format!("  could not capture the card band ({e})\n"));
                break;
            }
        };
        cards = centres
            .iter()
            .map(|&cx| crate::heroselect::read_card(&frame, (bx, by), &catalogue, game_dir, cx))
            .collect::<Vec<_>>();
        let recognised = cards
            .iter()
            .any(|c| c.scores.iter().any(|(_, s)| *s >= crate::heroselect::MARKER_PRESENT));
        for c in &cards {
            r.log.push_str(&format!("  card {}\n", c.summary()));
        }
        if recognised || attempt == READS {
            break;
        }
        r.log.push_str("  no card recognised — the screen may still be drawing\n");
        std::thread::sleep(crate::timing::SCREEN_DISSOLVE);
    }

    if cards.is_empty() {
        r.log.push_str("  the cards could not be read — falling back to the middle card\n");
        return Ok(middle);
    }
    let i = crate::heroselect::choose(&cards)?;
    Ok(cards[i].centre_x)
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
    std::thread::sleep(crate::timing::AFTER_MODE_CHANGE);

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
    // **Which card is now read off the screen.** This used to take the middle one and say so —
    // *"picked because it needs no arithmetic, not because it is best"* — which was honest and was
    // also a coin toss. The dev has since ruled two classes out and one in, and [`crate::heroselect`]
    // names them from the art in each card's `Passives:` row. A reading that recognises nothing
    // lands back on the middle card, so the worst this can do is what it replaced.
    //
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
    let (cx, cy) = (pick_a_champion(r, game_dir)?, 400);
    let (sx, sy) =
        r.win.client_to_screen(cx, cy).map_err(|e| format!("cannot reach the card: {e}"))?;
    r.log.push_str(&format!("  choosing the champion at ({cx},{cy})\n"));
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
    std::thread::sleep(crate::timing::HOVER_DWELL);
    if !r.tap("hero select: the champion card", sx, sy) {
        return Err("click on the champion card failed".into());
    }
    std::thread::sleep(crate::timing::AFTER_SCREEN_PRESS);

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
    std::thread::sleep(crate::timing::AFTER_MODE_CHANGE);

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
                // No health reading on this path, and `false` is the safe direction: it only ever
                // demotes the Heal boon, never promotes one.
                false,
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
        std::thread::sleep(crate::timing::FOCUS_SETTLE);
        let _ = r.tap_key("hero select: confirm", VK_RETURN, SC_RETURN);
        std::thread::sleep(crate::timing::AFTER_SCREEN_PRESS);
    }
    Err(format!("never reached the overworld after {MAX_RETURNS} Returns"))
}
