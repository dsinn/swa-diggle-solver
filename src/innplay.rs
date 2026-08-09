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
/// Twelve is four times what a full bar costs from one health at the base rate, so it never binds in
/// ordinary play; it exists for the case where the arithmetic is wrong rather than the case where
/// the rest is expensive.
pub const MAX_PRESSES: usize = 12;

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
        let truthy = |k: &str| !matches!(t.get(k), None | Some(Value::Nil) | Some(Value::Bool(false)));
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
/// Three limits, and each has bitten something in this project before:
///
/// - **Enough to fill the bar.** `heal = min(getRestValue(), healthNeed)` per press
///   (`ui/rest.lua:353`), so this is a division, not a single click.
/// - **What we can pay for.** `getCanRest` is a flat `getPlayerGold() >= 10` (`:49`), and the
///   button simply goes inactive when it fails — a silent stop, not a refusal we would see.
/// - **[`MAX_PRESSES`]**, so a block we misread costs a bounded amount rather than a purse.
///
/// Zero whenever there is nothing to gain, including the case that matters most: `can_rest` already
/// false, which is the inn telling us it will not serve us before we have touched anything.
pub fn presses_needed(d: &RestData, gold: i64) -> usize {
    if !d.can_rest || d.doing_event || d.health_need <= 0 || d.health_give <= 0 {
        return 0;
    }
    let to_full = ((d.health_need + d.health_give - 1) / d.health_give) as usize;
    // A campfire costs fuel rather than gold, and `can_rest` has already answered for the fuel.
    let affordable =
        if d.campfire { usize::MAX } else { (gold / crate::rest::INN_COST).max(0) as usize };
    to_full.min(affordable).min(MAX_PRESSES)
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
        assert_eq!(presses_needed(&d, 763), 0);
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
        assert_eq!(presses_needed(&d, 0), 4, "no gold is no obstacle at a campfire");
    }

    #[test]
    fn four_presses_fill_a_bar_from_one_of_twenty() {
        // The sandbox's actual state, and the reason this is a loop: 19 missing at 6 a press.
        let d = parse_rest_data(&lines(OPENED)).unwrap();
        assert_eq!(presses_needed(&d, 763), 4);
    }

    #[test]
    fn gold_caps_the_loop_before_the_bar_does() {
        // Thirty gold is three rests, whatever the bar says. The button would simply go inactive on
        // the fourth press and the run would be pressing at nothing.
        let d = parse_rest_data(&lines(OPENED)).unwrap();
        assert_eq!(presses_needed(&d, 30), 3);
        assert_eq!(presses_needed(&d, 9), 0, "below the inn's ten gold there is nothing to do");
    }

    #[test]
    fn nothing_to_gain_is_no_presses() {
        let full = RestData { can_rest: true, health_need: 0, health_give: 6, ..Default::default() };
        assert_eq!(presses_needed(&full, 763), 0);
        let refused =
            RestData { can_rest: false, health_need: 19, health_give: 6, ..Default::default() };
        assert_eq!(presses_needed(&refused, 763), 0);
        // `restInsomnia` gear and a zero-value rest both land here, and neither is worth a click.
        let pointless =
            RestData { can_rest: true, health_need: 19, health_give: 0, ..Default::default() };
        assert_eq!(presses_needed(&pointless, 763), 0);
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
}
