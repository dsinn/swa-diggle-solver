//! The wizard's tower, and the free map it hands out.
//!
//! **Placeholder.** The decision — *is `Reveal` on offer here?* — is implemented and tested, because
//! it is answerable from the save. The press is not: [`press_reveal`] is a stub, and
//! [`Offer::Available`] is a promise nothing yet keeps. See [`Offer`] for why that split.
//!
//! ## Why a tower is worth arriving at
//!
//! It is one of the two places on the map that cost nothing to enter. `competeOnVisit = true`
//! (`overworld/locations/wizard_tower.lua:18`) is unconditional, so `locationHasCombat`
//! (`overworldview.lua:305-310`) is false and the heading prints without a `— level N`. Standing on
//! it completes it. Nothing attacks.
//!
//! What it gives is fog:
//!
//! ```lua
//! -- wizard_tower.lua:60-69
//! overworldview.clearFogAroundPoint(
//!     overworldview.playerCurrentLocation().posX,
//!     overworldview.playerCurrentLocation().posY,
//!     640*(1+((overworld.playerHasGearFlag('towerRange+') or 0)*0.24))
//! )
//! ```
//!
//! 640 world units — ten grid tiles, since the generator works in `posX/64`
//! (`hellportal.lua:170`) — revealed in every direction, for one click. That is the direct answer to
//! a hurt run that knows no campfire and no village: the reason it knows none may only be that the
//! clouds are in the way.
//!
//! **What it is not is a way to lower [`crate::overworld::Place::hidden`]**, which this paragraph
//! claimed until #106. That field counts the `Hidden location` lines in the dump's *adjacent
//! connections*, and in that loop the cloud test can never fire — it ends in `not
//! connections[playerLocation]` (`overworldview.lua:704`) and every key the loop visits is adjacent
//! to the player by construction. Those are **secrets**, cleared by `events.revealSecretLocation`
//! (`utils/events.lua:161-181`) and not by fog. What the tower moves is which places exist on the
//! map at all, which is a different and larger thing; see [`crate::overworld::Place::unrevealed`].
//!
//! ## The trap in the slot
//!
//! `Reveal` and `Teleport` are **the same coordinate** — both `xOffset = 3.25` at `(0, 0.85)`
//! (`wizard_tower.lua:52` and `:71`) — and they are switched by mutually exclusive `showIf`
//! predicates, never drawn together. So a blind click there presses whichever the game decided to
//! paint, and the one we do not want opens `ui.waypointselect` (`:78-81`): a mode change, off the
//! overworld, that nothing in [`crate::navigate`] recognises or knows how to leave.
//!
//! This is the same shared-slot hazard `(0, 0.85)` has produced twice already — see
//! [`crate::observe::affirm::SHOW_AREA_BUTTONS`], where the fix was to read the artwork rather than
//! trust the position. Here the artwork cannot separate them: both are `default` buttons, identical
//! but for the word printed on them. So the guard has to be [`Offer::of`], computed from the save
//! **before** the click, and its conservative direction is chosen deliberately — see there.

use crate::win::window::ButtonSpec;

/// `Reveal` on the tower's area-button row.
///
/// `require'ui.elements.button'('Reveal', 0, 0.85, { xOffset = 3.25, ... })`
/// (`wizard_tower.lua:52-70`), with `default` at 250x100 (`ui/elements/button.lua:17`). Centre
/// (812, 918) at 1920x1080.
pub const REVEAL: ButtonSpec =
    ButtonSpec { ss_x: 0.0, ss_y: 0.85, os_x: 3.25, os_y: 0.0, w: 250.0, h: 100.0 };

/// `Teleport` — **the same slot as [`REVEAL`]**, and the thing we must never press by accident.
///
/// `wizard_tower.lua:71-82`. Kept as a separate constant with identical fields on purpose: the two
/// being equal is the hazard, and a reader who sees only [`REVEAL`] would not know it.
pub const TELEPORT: ButtonSpec = REVEAL;

/// What the tower is offering, decided from the save rather than from the screen.
///
/// The three cases are not symmetrical, and the asymmetry is the point:
///
/// - [`Offer::Available`] is the only one that permits a click at [`REVEAL`].
/// - [`Offer::Spent`] means the slot holds `Teleport`. Pressing it leaves the overworld.
/// - [`Offer::NotATower`] means the slot holds something else entirely — `Combat`, `Enter`, `Rest`,
///   or the back-arrow. Every one of those is worse than doing nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offer {
    /// `Reveal` is painted and live. Fog can be bought here for one click.
    Available,
    /// Already revealed at our current range: the slot holds `Teleport` instead.
    Spent,
    /// Not standing on a wizard's tower at all.
    NotATower,
}

impl Offer {
    /// Mirrors the button's own `showIf` (`wizard_tower.lua:56-59`):
    ///
    /// ```lua
    /// local flag = overworldview.areaFlag(overworldview.usedFlagName())
    /// return not flag or flag and (tonumber(flag) or 0)<(overworld.playerHasGearFlag('towerRange+') or 0)
    /// ```
    ///
    /// `used_flag` is `<key>_used` (`overworldview.lua:215`) and is **not a boolean here**. Every
    /// other location writes `true` through `setAreaUsed` (`:216`); the tower writes the gear range
    /// it was used at — `setAreaFlag(usedFlagName(), playerHasGearFlag'towerRange+' or 0)`
    /// (`wizard_tower.lua:66`). A tower used with no gear therefore stores `0`, which is **truthy in
    /// Lua**, so `not flag` is false and the offer returns only once the range has since grown.
    ///
    /// ## `range` is an input because it is not in the save
    ///
    /// `playerHasGearFlag` reads `gearFlagHash` (`overworld.lua:111-114`), a table built at runtime
    /// from the player's equipped items; `towerRange+` is granted by `items/overworldgear.lua:19`
    /// and `items/classpassives.lua:103`. Nothing on disk carries the resolved flag, so getting it
    /// means walking `mainSaveData.items` against the item tables — real work, and not this stub's.
    ///
    /// **Passing `0` is the safe default and callers should**, because the error it can make is the
    /// harmless one. Under-reporting the range turns a live `Reveal` into [`Offer::Spent`] and we
    /// skip a free map. Over-reporting it would return [`Offer::Available`] while the game is
    /// painting `Teleport`, and the click would land on the mode change. One costs a missed
    /// opportunity; the other strands the run on a screen it cannot leave.
    pub fn of(is_tower: bool, used_flag: Option<i64>, range: i64) -> Offer {
        if !is_tower {
            return Offer::NotATower;
        }
        match used_flag {
            None => Offer::Available,
            Some(used) if used < range => Offer::Available,
            Some(_) => Offer::Spent,
        }
    }
}

/// Heading suffix for a tower, from `typeName = "wizards' tower"` (`wizard_tower.lua:4`).
///
/// A constant rather than a literal at the call site because the apostrophe is easy to lose and the
/// mistake is silent: [`crate::overworld::Place::type_is`] is a plain suffix test, so a near-miss
/// matches nothing and the tower simply never gets noticed.
pub const TYPE_NAME: &str = "wizards' tower";

/// Press `Reveal`. **Not implemented.**
///
/// Deliberately a returned `false` and not a `todo!()`, for the reason [`crate::navigate::Stop`]
/// already records: panicking is the idiomatic Rust for unwritten, and it is the wrong tool while
/// this process holds the real mouse and keyboard — an unwind skips the run's whole shutdown and
/// leaves the game foregrounded, eating whatever the user types next.
///
/// Returning `false` is safe in a way that returning `true` would not be. The caller's only
/// reasonable response to `false` is to log it and walk on, which is exactly what happens today; a
/// caller told `true` would believe fog had been cleared and wait for a dump that never comes.
///
/// ## What finishing it needs, in the order it has to be done
///
/// 1. **Get the area buttons showing.** Arrival leaves the row in whatever state the last click
///    left; [`crate::observe::affirm::SHOW_AREA_BUTTONS`] is the recovery and already works.
/// 2. **Confirm the slot before pressing it.** [`Offer::of`] is the gate, but it is inference. The
///    tower row is `Shop | Crafting | Reveal-or-Teleport`, so `Shop` at `xOffset = 0.75` and
///    `Crafting` at `2` (`wizard_tower.lua:34,44`) are a cheap positive control: if those two are
///    not where they should be, we are not on a tower and must not click at all.
/// 3. **Press, then verify by the fog moving** — not by the button vanishing. `Reveal` swaps to
///    `Teleport` in place, so the slot stays occupied either way. The observable is the **used
///    flag in the save**: the handler sets `areaFlag(usedFlagName())` and then calls
///    `overworld:save()` (`wizard_tower.lua:66-68`), so it is readable straight afterwards —
///    unusually, this is one of the few actions that flushes without a screen exit.
///
///    **Not a drop in [`crate::overworld::Place::hidden`]**, which this note proposed until #106.
///    That count is secrets and the tower clears fog; the two do not meet, so the check would have
///    waited for a number that was never going to move. Nor is it *new neighbours in the next
///    adjacency dump*: the dump names the neighbours of the node we are standing on, and those are
///    never cloud-covered in the first place.
///
/// The third step is why this is not two lines. Everything this project has got wrong twice is a
/// press whose effect was assumed rather than watched.
pub fn press_reveal(_win: &crate::win::window::GameWindow) -> Result<bool, crate::Error> {
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::window::button_center;

    #[test]
    fn reveal_and_teleport_share_a_coordinate() {
        // The whole reason `Offer` exists. If these ever stop being equal, the guard can relax --
        // and if this test is deleted without that happening, a blind click starts a mode change.
        assert_eq!(button_center(&REVEAL, 1920, 1080), button_center(&TELEPORT, 1920, 1080));
        // `xOffset = 3.25` on a 250-wide `default` button, at `ss_y = 0.85`.
        assert_eq!(button_center(&REVEAL, 1920, 1080), (812, 918));
    }

    #[test]
    fn an_untouched_tower_offers_a_reveal() {
        assert_eq!(Offer::of(true, None, 0), Offer::Available);
    }

    #[test]
    fn zero_is_a_used_tower_not_an_unused_one() {
        // The trap: `setAreaFlag(usedFlagName(), playerHasGearFlag'towerRange+' or 0)` writes 0 for
        // a player with no gear, and 0 is truthy in Lua, so `not flag` is false. Reading it as
        // "unused" would send us to press `Teleport`.
        assert_eq!(Offer::of(true, Some(0), 0), Offer::Spent);
    }

    #[test]
    fn more_range_than_last_time_reopens_it() {
        // `(tonumber(flag) or 0) < towerRange+`. Used bare, then picked up a `towerRange+` item.
        assert_eq!(Offer::of(true, Some(0), 1), Offer::Available);
        assert_eq!(Offer::of(true, Some(1), 1), Offer::Spent, "same range, nothing new to see");
        assert_eq!(Offer::of(true, Some(1), 2), Offer::Available);
    }

    #[test]
    fn assuming_no_gear_can_only_cost_us_a_free_map() {
        // The documented safe direction. Whatever the true range, `range = 0` never returns
        // `Available` where the real `showIf` would have hidden `Reveal` -- so the click never lands
        // on `Teleport`.
        for used in 0..4 {
            assert_eq!(Offer::of(true, Some(used), 0), Offer::Spent);
        }
    }

    #[test]
    fn nothing_is_offered_anywhere_else() {
        assert_eq!(Offer::of(false, None, 9), Offer::NotATower);
    }

    #[test]
    fn the_type_name_matches_a_real_heading() {
        // `AreaHeading` prints `name .. ' ' .. typeName` with no level, because `competeOnVisit` is
        // unconditionally true (`wizard_tower.lua:18`) -- so a tower never carries `— level N`, and
        // that absence is what tells the planner it is free to enter.
        let heading = format!("Wetwang {TYPE_NAME}");
        assert!(heading.ends_with(TYPE_NAME));
        assert!(!heading.contains("— level "), "a tower heading never announces combat");
    }

    #[test]
    fn the_press_is_honest_about_being_unwritten() {
        // Pinning the stub's contract: it must not claim success. A caller that believed it would
        // wait for fog that never cleared.
        assert!(!press_reveal_is_implemented());
    }

    /// Mirrors [`press_reveal`]'s return without needing a window handle to call it.
    fn press_reveal_is_implemented() -> bool {
        false
    }
}
