//! Resting at an inn: the acting half of [`crate::rest`].
//!
//! `rest.rs` decides *whether* a rest is wanted and *where* it can be had. This module knows what to
//! press once we are standing on the inn, and — more usefully — what the game says back.
//!
//! ## The whole sequence is announced on the console
//!
//! Nothing here needs a template, which is unusual for this project and worth saying plainly,
//! because the alternative was three new fingerprints against screens we have never captured:
//!
//! | Step | Line the game prints | Source |
//! |---|---|---|
//! | The inn opens | `Entered inn screen` | `ui/inn.lua:125-127` |
//! | The rest screen opens | `Rest screen` + a `Rest data` block | `ui/rest.lua:537`, `:29-40` |
//! | A rest happens | `Rested` + a `Rest data` block | `ui/rest.lua:366` |
//!
//! All three are `_VERBOSE` prints, which is the channel this whole project runs on.
//!
//! ## One press is not one rest
//!
//! `heal = math.min(getRestValue(), healthNeed)` per loop iteration (`ui/rest.lua:353`), and
//! `getRestValue` is `6*(restAdd6+1)` — so a press restores **six**, not a full bar, and a run at
//! 1/20 needs four of them at ten gold each. That is why this is a loop rather than a click, and it
//! is where the name "the rest loop" comes from.
//!
//! ## The `Rest data` block printed after a rest is one step behind
//!
//! `doRest` ends with `logRestData('Rested')` (`:366`), and its caller only *then* runs
//! `payRestCost`, `updateButtonIcon` and `updateHealthArmour` (`:387-401`). So the `healthNeed`,
//! `canRest` and `cost` in a `Rested` block are the values from **before** the rest that block is
//! announcing. Reading them as the new state would leave a run pressing until its gold ran out.
//!
//! [`presses_needed`] therefore does the arithmetic from the *first* block — the one `Rest screen`
//! prints, which is current — and the loop counts its own presses instead of asking again.
//!
//! ## The dream announces itself, in the one field that is not stale
//!
//! `doingEvent` is assigned by `doRest` itself (`:364`) immediately before the log line, so unlike
//! everything else in the block it *is* current. It holds the closure that will switch modes two
//! seconds later (`core:update`, `:414-419`), and `table.serialize` renders a function as a quoted
//! `"function: 0x…"` (`utils/table.lua:384`) — a truthy string. So a `Rested` block carrying a
//! `doingEvent` is the game telling us, in advance, that a dream is coming.
//!
//! That is what makes [`WAKE_UP`] safe to click blind. The button is `showIf = wakeUp`
//! (`overworld/events/rested.lua:110-115`) and only appears once two tiles in the dream's physics
//! simulation collide, so there is no duration to wait out and nothing to match against until it
//! arrives — but we know which screen we are on, and it is the only thing in that slot.
//!
//! ## There is no crash here. This section used to say there was.
//!
//! **RETRACTED 2026-08-08, by a live rest.** The dream ran, `Physics dream:` printed, and the game
//! carried on. Kept as a worked example of a measurement that was confidently wrong, because the
//! shape of the mistake is one this project keeps making.
//!
//! The claim was that `requireCheck` (`overworld/events/rested.lua:12-16`) indexes two unguarded
//! nils and takes the game down on the first press:
//!
//! ```lua
//! and (overworldview.areaFlag'shrinePremonition' and overworldview.areaFlag'shrinePremonitionParallax')
//! and (not persistent.unlockedModes.physicsDream or love.math.random(1,4)==1)
//! and not overworldview.areaFlag'shrinePremonitionData'.gameover
//! ```
//!
//! It was reproduced offline in vendored LuaJIT against the real save files — and reproduced only
//! because the harness built `persistent` wrongly. **`persistentSaveData` on disk contains
//! `unlockedModes`.** The reading behind it, "`persistent.unlockedModes` is never initialised
//! anywhere in the game", came from `main.lua:11` plus a grep for assignments, and never from
//! opening the file the line loads. `shrinePremonitionData` is likewise present in `mainSaveData`,
//! so `.gameover` indexes fine too.
//!
//! Two lessons, and the second is the one worth carrying:
//!
//! 1. **A grep for writes cannot tell you what a save contains.** The state came from a file, not
//!    from code, so no amount of reading code was going to find it.
//! 2. **The positive control masked the bug.** Seeding `unlockedModes` and watching the error clear
//!    "confirmed" the diagnosis — but it confirms the same thing whether the field was missing in
//!    the game or only in the harness. A control has to distinguish the hypothesis from the way the
//!    experiment could be broken, and that one could not.
//!
//! The dev rejected the offline reproduction as insufficient at the time and asked for an in-game
//! one. That instinct was right and the delay cost nothing.

use crate::game::save::{Table, Value};
use crate::win::window::ButtonSpec;
use std::time::Duration;

/// `ui/inn.lua:125-127`, printed from the inn's `onActive`. Our confirmation that `Enter` worked.
pub const ENTERED: &str = "Entered inn screen";

/// `ui/rest.lua:537`, printed from the rest screen's `onActive`, followed by a [`REST_DATA`] block.
pub const REST_SCREEN: &str = "Rest screen";

/// `ui/rest.lua:366`, printed at the end of `doRest`, followed by a [`REST_DATA`] block.
pub const RESTED: &str = "Rested";

/// The header `table.repr(…, 'Rest data')` prints (`utils/table.lua:344-350`).
///
/// The key is used verbatim at depth 0 — quoting only applies to nested keys — so the line is
/// exactly this, and the block runs to the next line that is a bare `}`.
pub const REST_DATA: &str = "Rest data = {";

/// `Rest` on the **inn** screen: `ss(1, 0.9)`, `xOffset -2`, a default 250x100 button
/// (`ui/inn.lua:55-57`).
///
/// Its neighbour `Shop` sits in the same row at `xOffset -0.75` — two button-widths away, which is
/// the whole margin, so this coordinate is not one to approximate.
pub const INN_REST: ButtonSpec =
    ButtonSpec { ss_x: 1.0, ss_y: 0.9, os_x: -2.0, os_y: 0.0, w: 250.0, h: 100.0 };

/// The back plaque, `ss(0, 0.9)` with `xOffset 1.13`, a `small` 100x100 button.
///
/// One constant for two screens: the inn and the rest screen declare it identically
/// (`ui/inn.lua:68-71`, `ui/rest.lua:517-520`), so leaving is the same press twice.
///
/// Clicked rather than pressed as Escape. Escape maps to `goBack() or options()`, and a press that
/// arrives while `activeIf` is false — during a fade, or while `doingEvent` is set — opens the
/// options menu instead, which is a screen this run has no way out of.
pub const BACK: ButtonSpec =
    ButtonSpec { ss_x: 0.0, ss_y: 0.9, os_x: 1.13, os_y: 0.0, w: 100.0, h: 100.0 };

/// `Wake up` on the dream screen: `ss(0, 0.85)`, `xOffset 0.75`, default 250x100
/// (`overworld/events/rested.lua:111-113`).
///
/// The same slot the overworld's `Travel`, `Enter` and `Combat` use, which is safe only because we
/// never click it without the console having told us a dream is running — see the module docs.
pub const WAKE_UP: ButtonSpec =
    ButtonSpec { ss_x: 0.0, ss_y: 0.85, os_x: 0.75, os_y: 0.0, w: 250.0, h: 100.0 };

/// A hard ceiling on presses at one inn, so a misread block cannot spend a run's whole purse.
///
/// It exists for the case where the arithmetic is wrong, not the case where the rest is expensive,
/// and it must stay above every number the arithmetic can legitimately produce or it stops being a
/// guard and starts being a rule.
///
/// **Raised from twelve on 2026-08-20**, when banking Well-Rested stacks gave it a second and larger
/// customer. Twelve was four times what a full bar costs from one health, so it never bound; the
/// bank then asked for twice the level, sixteen for the level 8 anomaly alone — the cap would have
/// silently capped the dev's rule three presses short, at an inn with the gold in hand and nothing
/// on screen to say why.
///
/// **Raised again from twenty on 2026-08-22**, for the same reason and a sharper one: the dev's
/// band fills a restocking visit to [`crate::rest::STACKS_TARGET`], which is twenty exactly. A
/// ceiling equal to the want is not a ceiling — it binds on the very first full restock from an
/// empty bank, and does so invisibly. Thirty leaves ten presses of daylight, which is also enough
/// for the case the guard is really for: a visit interrupted by a dream, re-entered, and asked
/// again against a save that has not been written (see [`still_wanted`]).
pub const MAX_PRESSES: usize = 30;

/// How many times to press `Rest` before accepting that the rest screen is not going to open.
///
/// **One press was not enough, measured.** Live 2026-08-09 at The Alot inn: `Entered inn screen`
/// arrived, `Rest` was pressed, and no [`REST_SCREEN`] followed in eight seconds. The run wrote the
/// inn off and walked to the next village at 7/20 to try again. The same inn had worked the run
/// before, and `l19`'s worked twice in that very run — intermittent, which is the signature of a
/// race rather than a wrong coordinate.
///
/// The cause is the one this project keeps rediscovering: [`ENTERED`] is printed from the inn's
/// `onActive` (`ui/inn.lua:125-127`), which is the screen *becoming* active, not the screen being
/// ready to take a click — and `ui/elements/button.lua:93` is a strict hit test. Announcement is not
/// readiness.
///
/// Retrying rather than sleeping first is deliberate. A fixed delay is a guess that is either too
/// short on a slow frame or wasted on every fast one, and this file already has the better pattern
/// in `wake_from_dream`: press, look for the announcement, press again. Re-pressing is safe because
/// [`REST_SCREEN`] is printed from `onActive` too, so silence for a few seconds is real evidence the
/// screen never opened rather than evidence the console is behind.
///
/// **Many short tries, not a few long ones.** `REST_SCREEN` comes from `onActive`, so once a press
/// registers the line follows within a frame or two — waiting four seconds for it only delays the
/// next attempt. Eight tries at [`REST_WAIT`] costs about the same wall-clock as three at four
/// seconds and gets nearly three times as many chances at the frame where the button is finally
/// live.
///
/// A stray press cannot do damage, which is what makes tightening safe: the rest screen's own
/// `Rest` sits at `(0.5, 0.9)` (`ui/rest.lua:504`) — screen centre, (960, 972) — while this one is
/// at (1420, 972). Press again after the screen has already opened and the click lands on nothing.
pub const REST_TRIES: usize = 8;

/// How long one [`REST_TRIES`] attempt waits for the rest screen to announce itself.
pub const REST_WAIT: Duration = Duration::from_millis(1500);

/// What the game says about the rest in front of us.
///
/// Fields are the ones `logRestData` prints (`ui/rest.lua:29-40`). Anything absent from the block is
/// nil in Lua and false here.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RestData {
    /// `getCanRest()` — gold for an inn, fuel or an unused area for a campfire.
    pub can_rest: bool,
    /// `maxHealth - health`, as of the last `updateHealthArmour`.
    pub health_need: i64,
    /// `getRestTotal()` — what one press restores.
    pub health_give: i64,
    /// A campfire rather than an inn. Free, and no subworld to cross.
    pub campfire: bool,
    /// An event is pending or running. On a `Rested` block this means a dream is two seconds away.
    pub doing_event: bool,
    /// `costText`, e.g. `-10`. `None` at a campfire, which is the point of the field.
    pub cost: Option<String>,
}

impl RestData {
    fn from_table(t: &Table) -> Self {
        // Anything that is neither nil nor `false` is truthy in Lua, which is exactly what
        // `doingEvent` needs: it arrives as a quoted `"function: 0x…"` when a dream is queued.
        let truthy =
            |k: &str| !matches!(t.get(k), None | Some(Value::Nil) | Some(Value::Bool(false)));
        RestData {
            can_rest: truthy("canRest"),
            health_need: t.int_at("healthNeed").unwrap_or(0),
            health_give: t.int_at("healthGive").unwrap_or(0),
            campfire: truthy("campfire"),
            doing_event: truthy("doingEvent"),
            cost: t.str_at("cost").map(str::to_owned),
        }
    }
}

/// The **last** `Rest data` block in `lines`, parsed.
///
/// Last rather than first because the console is a scrollback: opening the screen prints one and
/// every press prints another, and the caller always wants the newest. Feed the slice from a mark
/// taken before acting, so a block from an earlier inn cannot answer for this one.
pub fn parse_rest_data(lines: &[String]) -> Option<RestData> {
    let start = lines.iter().rposition(|l| l.trim() == REST_DATA)?;
    let end = lines[start..].iter().position(|l| l.trim() == "}")? + start;
    // The block is a Lua table literal with a named key at depth 0; the parser wants a module.
    let mut src = String::from("return {\n");
    for line in &lines[start + 1..end] {
        src.push_str(line);
        src.push('\n');
    }
    src.push_str("}\n");
    crate::game::save::parse(&src).ok().map(|t| RestData::from_table(&t))
}

/// How many times to press `Rest`, from the block the rest screen printed when it opened.
///
/// Two reasons to press, and the greater of them wins:
///
/// - **Enough to fill the bar.** `heal = min(getRestValue(), healthNeed)` per press
///   (`ui/rest.lua:353`), so this is a division, not a single click.
/// - **Enough to bank `stacks_short` Well-Rested stacks**, one per press
///   (`affectPlayerStatus(statusGive, 1)`, `:355-357`). The dev's rule of 2026-08-20, revised
///   2026-08-22; see [`crate::rest::STACKS_TARGET`], which is what a visit fills to.
///
/// Then two limits on the result:
///
/// - **What we can pay for.** `getCanRest` is a flat `getPlayerGold() >= 10` (`:49`), and the
///   button simply goes inactive when it fails — a silent stop, not a refusal we would see.
/// - **[`MAX_PRESSES`]**, so a block we misread costs a bounded amount rather than a purse.
///
/// ## `health_need <= 0` is no longer a reason to stop, and that is the change
///
/// It used to return zero at full health, which was right while healing was the only thing a rest
/// bought. It is not: the stack is granted on its own line, unconditional on the heal
/// (`ui/rest.lua:355-357`), and the inn will serve a character at full health because `getCanRest`
/// asks about gold and nothing else. So a run that needs a bank and has a full bar must still press,
/// and this returning zero was the single line standing in the way.
///
/// `health_give <= 0` still stops it, but only when there is no bank to buy either: a rest that
/// heals nothing *and* banks nothing is a press with no effect at all.
///
/// Zero whenever there is nothing to gain, including the case that matters most: `can_rest` already
/// false, which is the inn telling us it will not serve us before we have touched anything.
/// What is still to buy on this visit, discounting what has already landed on it.
///
/// **Both numbers this returns are read from the save, and the save is not written until we leave.**
/// `overworld:save()` runs in the inn's `goBack` (`ui/inn.lua:9`), so `player.gold` and
/// `player.statusEffects` are frozen at the value they had when the visit began. The rest screen's
/// own `Rest data` block is fresh — the game prints it each time the screen opens — which is why
/// `healthNeed` falls across a visit and these two do not.
///
/// That asymmetry cost a live run on 2026-08-20. Banking asked for eleven stacks, a dream took the
/// screen after three or four presses, the loop re-entered and asked again — and got eleven back,
/// six times over, because nothing it could see had changed. Twenty presses landed against a want of
/// eleven, two hundred gold went, and the character came out with **25** stacks banked for a fight
/// that wanted 16. Only [`MAX_PRESSES`] stopped it, which is a misread guard being asked to be a
/// budget.
///
/// `done` is the same counter that already clamps the visit, so the correction is arithmetic rather
/// than a second reading: each press cost [`crate::rest::INN_COST`] and banked one stack.
///
/// The gold half was wrong before the bank existed and had never bitten — `presses_needed` was
/// driven by `healthNeed`, which the console refreshes, so a stale purse only ever over-allowed a
/// press the inn would then refuse. Adding a want the console does not report is what made the
/// staleness reachable.
pub fn still_wanted(gold: i64, stacks_short: i64, done: usize) -> (i64, i64) {
    let spent = done as i64 * crate::rest::INN_COST;
    ((gold - spent).max(0), (stacks_short - done as i64).max(0))
}

pub fn presses_needed(d: &RestData, gold: i64, stacks_short: i64) -> usize {
    if !d.can_rest || d.doing_event {
        return 0;
    }
    let banking = stacks_short.max(0) as usize;
    let to_full = match d.health_need > 0 && d.health_give > 0 {
        true => ((d.health_need + d.health_give - 1) / d.health_give) as usize,
        false => 0,
    };
    // A campfire costs fuel rather than gold, and `can_rest` has already answered for the fuel.
    let affordable =
        if d.campfire { usize::MAX } else { (gold / crate::rest::INN_COST).max(0) as usize };
    to_full.max(banking).min(affordable).min(MAX_PRESSES)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    /// Exactly what `table.repr` produces for the inn at 1/20 with 763 gold.
    const OPENED: &str = "\
Rest screen
Rest data = {
    canRest = true,
    healthNeed = 19,
    healthGive = 6,
    cost = \"-10\",
}";

    #[test]
    fn the_opening_block_reads_straight_off_the_console() {
        let d = parse_rest_data(&lines(OPENED)).unwrap();
        assert!(d.can_rest);
        assert_eq!(d.health_need, 19);
        assert_eq!(d.health_give, 6);
        assert_eq!(d.cost.as_deref(), Some("-10"));
        // Absent from the block means nil in Lua, which is false here -- an inn, and no dream.
        assert!(!d.campfire);
        assert!(!d.doing_event);
    }

    #[test]
    fn the_newest_block_is_the_one_that_answers() {
        // Two rests in the same buffer. Reading the first would have us press against stale numbers
        // -- the mistake the module docs are about, in its other form.
        let text = format!(
            "{OPENED}\nRested\nRest data = {{\n    canRest = true,\n    healthNeed = 19,\n    \
             healthGive = 6,\n    cost = \"-10\",\n}}\nRested\nRest data = {{\n    canRest = \
             false,\n    healthNeed = 7,\n    healthGive = 6,\n}}"
        );
        let d = parse_rest_data(&lines(&text)).unwrap();
        assert!(!d.can_rest);
        assert_eq!(d.health_need, 7);
    }

    #[test]
    fn a_queued_dream_arrives_as_a_function() {
        // `doingEvent` is the one field in a `Rested` block that is current, and it is how we know a
        // dream is two seconds out. `table.serialize` renders a function as a quoted string.
        let text = "Rested\nRest data = {\n    canRest = true,\n    healthNeed = 13,\n    \
                    healthGive = 6,\n    doingEvent = \"function: 0x0000023c\",\n}";
        let d = parse_rest_data(&lines(text)).unwrap();
        assert!(d.doing_event);
        // And it stops the loop dead: pressing into a pending event does nothing (`ui/rest.lua:44`).
        assert_eq!(presses_needed(&d, 763, 0), 0);
    }

    #[test]
    fn a_campfire_block_has_no_cost_line() {
        // `costText = nil` when resting at a campfire (`ui/rest.lua:54-55`), so the key is simply
        // absent -- and the fuel, not the purse, is what `canRest` has already answered for.
        let text = "Rest data = {\n    canRest = true,\n    healthNeed = 19,\n    healthGive = \
                    6,\n    campfire = true,\n}";
        let d = parse_rest_data(&lines(text)).unwrap();
        assert!(d.campfire);
        assert_eq!(d.cost, None);
        assert_eq!(presses_needed(&d, 0, 0), 4, "no gold is no obstacle at a campfire");
    }

    #[test]
    fn four_presses_fill_a_bar_from_one_of_twenty() {
        // The sandbox's actual state, and the reason this is a loop: 19 missing at 6 a press.
        let d = parse_rest_data(&lines(OPENED)).unwrap();
        assert_eq!(presses_needed(&d, 763, 0), 4);
    }

    #[test]
    fn gold_caps_the_loop_before_the_bar_does() {
        // Thirty gold is three rests, whatever the bar says. The button would simply go inactive on
        // the fourth press and the run would be pressing at nothing.
        let d = parse_rest_data(&lines(OPENED)).unwrap();
        assert_eq!(presses_needed(&d, 30, 0), 3);
        assert_eq!(presses_needed(&d, 9, 0), 0, "below the inn's ten gold there is nothing to do");
    }

    #[test]
    fn nothing_to_gain_is_no_presses() {
        let full =
            RestData { can_rest: true, health_need: 0, health_give: 6, ..Default::default() };
        assert_eq!(presses_needed(&full, 763, 0), 0);
        let refused =
            RestData { can_rest: false, health_need: 19, health_give: 6, ..Default::default() };
        assert_eq!(presses_needed(&refused, 763, 0), 0);
        // `restInsomnia` gear and a zero-value rest both land here, and neither is worth a click.
        let pointless =
            RestData { can_rest: true, health_need: 19, health_give: 0, ..Default::default() };
        assert_eq!(presses_needed(&pointless, 763, 0), 0);
    }

    /// **A full bar is no longer a reason to walk out**, which is the change of 2026-08-20.
    ///
    /// The dev's rule wants Well-Rested stacks banked before a deep fight, and a stack is bought by
    /// pressing `Rest` — one per press, granted on its own line, unconditional on the heal
    /// (`ui/rest.lua:355-357`). The inn will serve a character at full health because `getCanRest`
    /// asks about gold and nothing else (`:49`). This function returning zero was the single line
    /// standing in the way.
    #[test]
    fn stacks_are_bought_at_full_health() {
        let full =
            RestData { can_rest: true, health_need: 0, health_give: 6, ..Default::default() };
        assert_eq!(presses_needed(&full, 763, 0), 0, "nothing wanted, nothing pressed — as before");
        assert_eq!(presses_needed(&full, 763, 5), 5, "five stacks short is five presses");
        // Sixteen is the level 8 anomaly's whole bank, from empty.
        assert_eq!(presses_needed(&full, 763, 16), 16);
    }

    /// Healing and banking are two reasons to press, and the **greater** one wins.
    ///
    /// Not the sum: a press that heals also banks, so buying four heals has already bought four
    /// stacks. Adding them would spend twice the gold for the same result.
    #[test]
    fn the_two_reasons_to_press_do_not_add_up() {
        let d = parse_rest_data(&lines(OPENED)).unwrap();
        assert_eq!(presses_needed(&d, 763, 0), 4, "the bar alone wants four");
        assert_eq!(presses_needed(&d, 763, 2), 4, "and two stacks come free with them");
        assert_eq!(presses_needed(&d, 763, 9), 9, "the deeper want is the one that decides");
    }

    /// **A visit spends as it goes, and the save does not say so until we leave.**
    ///
    /// The 2026-08-20 run in the smallest form that reproduces it: eleven stacks wanted, dreams
    /// breaking the visit into rounds, and every round re-reading the same frozen numbers.
    #[test]
    fn what_is_left_to_buy_falls_as_the_visit_spends() {
        // Round one: nothing landed yet, so the reading stands as taken.
        assert_eq!(still_wanted(641, 11, 0), (641, 11));

        // Four presses in — the shape of one dream-interrupted round.
        assert_eq!(still_wanted(641, 11, 4), (601, 7));

        // And the want is satisfied exactly when the presses have landed, not when the save catches
        // up. This is the assertion the live run failed: at eleven done it asked for eleven more.
        assert_eq!(still_wanted(641, 11, 11), (531, 0));

        // Never negative, so an over-long visit cannot wrap into a fresh want.
        assert_eq!(still_wanted(641, 11, 20), (441, 0));
        assert_eq!(still_wanted(30, 11, 20), (0, 0), "nor can the purse go through the floor");
    }

    /// The two together: once the want is spent, the press count is zero and the visit ends.
    #[test]
    fn a_satisfied_bank_stops_the_visit_rather_than_running_to_the_ceiling() {
        let full =
            RestData { can_rest: true, health_need: 0, health_give: 6, ..Default::default() };
        let (gold, short) = still_wanted(641, 11, 11);
        assert_eq!(
            presses_needed(&full, gold, short),
            0,
            "eleven landed against eleven wanted is done, and 20 was never the budget"
        );
        assert!(MAX_PRESSES > 11, "the fixture must not be proving the ceiling instead");
    }

    /// The purse still caps it, and the floor is still the inn's own.
    #[test]
    fn gold_caps_the_bank_exactly_as_it_caps_the_bar() {
        let full =
            RestData { can_rest: true, health_need: 0, health_give: 6, ..Default::default() };
        assert_eq!(presses_needed(&full, 30, 16), 3, "three rests is what thirty gold buys");
        assert_eq!(
            presses_needed(&full, crate::rest::INN_COST - 1, 16),
            0,
            "the dev's floor: below ten gold the errand is over"
        );
    }

    /// **The ceiling has to sit above every number the rule can ask for.**
    ///
    /// It was twelve, which never bound while healing was the only customer — four times what a
    /// full bar costs from one health. The bank then asked for twice the level, sixteen for the
    /// anomaly alone, so twelve would have capped the dev's rule four presses short at an inn with
    /// the gold in hand and nothing on screen to say why.
    ///
    /// **Strictly greater, not `>=`, since 2026-08-22.** The band fills to
    /// [`crate::rest::STACKS_TARGET`] flat, so equality would mean the cap binds exactly when the
    /// rule is satisfied for the first time — a guard that fires on the intended path is a rule
    /// wearing a guard's name. This is the assertion that keeps the two apart.
    #[test]
    fn the_press_ceiling_clears_the_deepest_bank_the_rule_can_want() {
        assert!(
            MAX_PRESSES > crate::rest::stacks_short(0) as usize,
            "a full restock wants {} presses and the cap is {MAX_PRESSES}",
            crate::rest::stacks_short(0)
        );
        let full =
            RestData { can_rest: true, health_need: 0, health_give: 6, ..Default::default() };
        assert_eq!(presses_needed(&full, 10_000, 500), MAX_PRESSES, "and it is still a ceiling");
    }

    #[test]
    fn a_truncated_block_is_no_block() {
        // The console is read while it is being written, so half a block is a normal thing to see.
        // Guessing at one would be worse than waiting for the next pump.
        assert_eq!(parse_rest_data(&lines("Rest data = {\n    canRest = true,")), None);
        assert_eq!(parse_rest_data(&lines("Rest screen")), None);
        assert_eq!(parse_rest_data(&[]), None);
    }

    #[test]
    fn the_two_buttons_we_press_are_where_the_game_puts_them() {
        use crate::win::window::button_center;
        // 1920x1080. `Rest` on the inn screen: x = 1920 + 250*(-2) = 1420, y = 1080*0.9 = 972.
        assert_eq!(button_center(&INN_REST, 1920, 1080), (1420, 972));
        // Two button-widths from `Shop` at xOffset -0.75, which is why the offset is not guessable.
        //
        // The back plaque lands a pixel left of the shrine's independently measured
        // `act::SHRINE_GOBACK.click` at (113, 972) — 100*1.13 is 112.99999999999999 in binary and
        // `button_center` truncates. Two routes to the same button agreeing to within a pixel is
        // the cross-check; a pixel inside a 100x100 hit box is not worth chasing.
        assert_eq!(button_center(&BACK, 1920, 1080), (112, 972));
        assert_eq!(crate::act::SHRINE_GOBACK.click, (113, 972));
        // `Wake up` shares the overworld's area slot: x = 0 + 250*0.75, y = 1080*0.85.
        assert_eq!(button_center(&WAKE_UP, 1920, 1080), (187, 918));
    }

    /// The arithmetic and the artwork have to agree about where `Rest` is.
    ///
    /// Two independent routes to one coordinate: this file derives it from the game's own
    /// declaration (`ui/inn.lua:55`), while `act::INN_REST` carries a template cut from a
    /// screenshot at a measured offset. Neither was computed from the other, so agreement is
    /// evidence and disagreement means one of them is wrong about the screen.
    ///
    /// It is the template that gets clicked — recognition beats arithmetic, and `click_when_ready`
    /// will not fire at all if the plaque is not there. This keeps the spec honest anyway, because
    /// it is what says *which* button the template is supposed to be showing.
    #[test]
    fn the_template_sits_where_the_arithmetic_says_it_should() {
        use crate::win::window::button_center;
        let (cx, cy) = button_center(&INN_REST, 1920, 1080);
        assert_eq!(crate::act::INN_REST.click, (cx, cy));
        // The template's top-left is the click point less half a `default` button (250x100).
        assert_eq!(crate::act::INN_REST.origin, (cx - 125, cy - 50));
        // The rest screen's own button is a different one on a different screen: `(0.5, 0.9)`
        // (`ui/rest.lua:504`). That separation is what makes a stray press at the inn's coordinate
        // harmless once the rest screen is up — 460 px apart, so it lands on nothing.
        assert_eq!(crate::act::REST_CONFIRM.click, (960, 972));
        assert!((crate::act::INN_REST.click.0 - crate::act::REST_CONFIRM.click.0).abs() > 250);
    }
}
