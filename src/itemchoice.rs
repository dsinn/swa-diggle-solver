//! The "Choose one:" screen — picking an item from a row of pedestals.
//!
//! **Not a combat screen.** It lived in [`crate::fight`] because that is where we first met it, but
//! `ui.itemselection` is constructed from seven places and only one of them is a fight:
//!
//! | site | what it is |
//! |---|---|
//! | `utils/world.lua:1289` | loot after clearing a combat zone |
//! | `overworld.lua:1167` | the postgame drop |
//! | `overworld/locations/capital.lua:123` | a shop |
//! | `items/potionaugmentgear.lua:105` | using a potion |
//! | `ui/classselection.lua:335` | choosing a class |
//! | `ui/modesequence.lua:23` | a scripted sequence |
//! | `ui/itemselection.lua:308` | its own reroll, which rebuilds the screen |
//!
//! So a run can meet this screen without having fought anything, and the code that gets off it does
//! not belong behind a `Fight`.
//!
//! ## The screen has to be *seen*, not inferred
//!
//! It gates everything behind it. While it is up there is no map, no area buttons and no
//! affirmative, so a driver that does not check for it fails every later step looking for a screen
//! nobody looked for — which is exactly what four `Absent` locate-me readings at 0.23 turned out to
//! be after a cleared crypt.
//!
//! ## Identification is from the console, and only from the console
//!
//! `ui/itemselection.lua:419` prints each item's key and screen position. **Nothing on the screen is
//! read to find them** — not the pedestals, not the labels. That is worth stating plainly because it
//! sets the failure mode: with no block in the feed there is nothing to click, and no way off a
//! screen whose only exit is `Confirm`, which `activeIf = selection` (`:274`) keeps dead until
//! something is chosen.
//!
//! The keys the console prints are enough to *choose* as well as to click, because every item
//! declares its kind in the game's own source — see [`PREFERRED_KINDS`] for the policy and
//! [`crate::items`] for how the kinds are read.

use crate::observe::feed::Feed;
use crate::win::input::{click_at, warp_cursor, Input, PostMessageInput, SC_SPACE, VK_SPACE};
use crate::win::window::GameWindow;
use std::time::{Duration, Instant};

/// Somewhere with no hotspot, so a hover highlight is never inside a reading.
const NEUTRAL: (i32, i32) = (300, 300);
/// Attempts at selecting an item before giving up.
const PICK_ATTEMPTS: usize = 3;

/// One item on offer: its key, and where the game says it is drawn.
pub type Offer = (String, i32, i32);

/// Kinds we can make use of, best first. Anything not listed ranks below all of them.
///
/// ## Why passive beats gear
///
/// Both apply on their own — `items.give` routes `gear` to `overworld.givePlayerGear` and `passive`
/// to `overworld.givePlayerPassive` (`utils/items.lua:46-63`), and neither needs the player to do
/// anything afterwards. The difference is capacity:
///
/// - `givePlayerPassive` (`overworld.lua:264-270`) appends to `playerData.passives` and regenerates
///   the flag hash. There is no limit. The tenth passive works exactly like the first.
/// - `givePlayerGear` (`overworld.lua:312-323`) appends to `playerData.gear`, but
///   `playerHasGearEquipped` is `has and has <= getPlayerGearSlotCount()` (`:307-310`), and that
///   count defaults to **3** (`:325-327`). Gear past the third slot is carried, not equipped — inert
///   until something reorders it, which Diggle cannot do.
///
/// So a fourth piece of gear may well do nothing, while a fourth passive certainly does something.
///
/// Everything else needs to be *used*: consumables, potions and scrolls sit in the item bar waiting
/// for a click Diggle never makes. Taking one is close to taking nothing, which is why they rank
/// below both — not because they are bad items.
///
/// **Known refinement, deliberately not taken yet:** once fewer than three gear slots are free, gear
/// is worth no more than a consumable, and the free-slot count is readable from `mainSaveData.gear`
/// the way [`crate::rest::fuel_from_save`] reads `items`. Left out because that save is written on
/// screen *exit*, so mid-run it can lag exactly the reward that changes it — and a wrong slot count
/// would demote gear at the moment gear is the only thing on offer.
pub const PREFERRED_KINDS: &[&str] = &["passive", "gear"];

/// Where a kind sits in [`PREFERRED_KINDS`]. **Lower is better**; unknown kinds tie for last.
///
/// Unknown is not a failure state. It covers an item the scan could not attribute (see
/// [`crate::items`]) as well as one that is genuinely a potion, and both deserve the same answer:
/// take it only if nothing better is offered.
pub fn rank(kind: Option<&str>) -> usize {
    kind.and_then(|k| PREFERRED_KINDS.iter().position(|p| *p == k))
        .unwrap_or(PREFERRED_KINDS.len())
}

/// Parses the offers out of the latest `Item selection:` block.
///
/// ## Whitespace, not tabs
///
/// Lua's `print` separates with `\t`, so tabs are what the game emits — but they are not what we
/// read, and splitting on one produced a single part, ended the block on its first row, and reported
/// "no offers parsed" against a screen showing three items.
///
/// Not a quirk of one capture either: the feed scrapes the console **screen buffer** with
/// `ReadConsoleOutputCharacterW` (`observe/log.rs:132`), and conhost renders a tab by advancing the
/// cursor and filling cells with spaces. There is no tab character in a screen buffer to find, so
/// splitting on one could never have worked against this source.
///
/// Whitespace splitting is right for either form because the shape carries the meaning: the key is
/// one token, the last two are coordinates, and the name between them may contain spaces (it usually
/// does — `Weird veiny beige thumb`) and is never needed.
///
/// Takes the **latest** block, so a driver reusing one feed across screens cannot collect an earlier
/// one's items.
pub fn offers(lines: &[String]) -> Vec<Offer> {
    let mut out = Vec::new();
    let Some(start) = lines.iter().rposition(|l| l.contains("Item selection:")) else {
        return out;
    };
    for line in lines.iter().skip(start + 1) {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 4 {
            break; // the block has ended
        }
        let (Ok(x), Ok(y)) = (parts[parts.len() - 2].parse(), parts[parts.len() - 1].parse()) else {
            break;
        };
        out.push((parts[0].to_string(), x, y));
    }
    out
}

/// Is a "Choose one:" screen up, in either state?
///
/// Cheap enough to poll: a 250x100 template over a 16x16 search box. See
/// [`crate::act::REWARD_CONFIRM`] for why the greyed capture identifies the screen whether or not
/// anything has been picked.
pub fn on_screen(win: &GameWindow) -> bool {
    matches!(
        crate::act::locate(win, &crate::act::REWARD_CONFIRM),
        Ok(Some(q)) if q >= crate::act::REWARD_SCREEN_PRESENT
    )
}

/// Is `Confirm` still greyed — i.e. has nothing been selected?
///
/// The game's own answer, since `activeIf` *is* the selection (`ui/itemselection.lua:274`), rather
/// than a pixel heuristic about the pedestal. Needs its own threshold: the active button is the same
/// plank and lettering in another colour and scores 0.73 against the greyed template, so the 0.55
/// that identifies the screen would call a live button greyed.
pub fn nothing_picked(win: &GameWindow) -> bool {
    matches!(
        crate::act::locate(win, &crate::act::REWARD_CONFIRM),
        Ok(Some(q)) if q >= crate::act::REWARD_NOTHING_PICKED
    )
}

fn park(win: &GameWindow) {
    if let Ok((x, y)) = win.client_to_screen(NEUTRAL.0, NEUTRAL.1) {
        let _ = warp_cursor(x, y);
    }
}

/// What happened on the screen.
#[derive(Debug, Clone, PartialEq)]
pub enum Chosen {
    /// Picked and confirmed. Carries the item key.
    Took(String),
    /// The screen is up but the feed holds no block for it, so there is nothing to click.
    NoOffers,
    /// Offers existed but no click would select one.
    CouldNotSelect,
}

/// Picks an item and presses `Confirm`.
///
/// Does **not** handle whatever comes next — a postgame, a shop returning to the map — because that
/// differs per caller. This gets as far as a confirmed selection and stops.
pub fn choose(
    win: &GameWindow, feed: &mut Feed, keys: &PostMessageInput, game_dir: &std::path::Path,
    log: &mut String, deadline: Instant,
) -> Result<Chosen, crate::Error> {
    // Poll rather than parse a single pump: the screen being up and its block reaching the feed are
    // not the same instant, and this can be entered a whole step after the screen appeared.
    let mut found = Vec::new();
    let by = deadline.min(Instant::now() + Duration::from_secs(5));
    while Instant::now() < by {
        feed.pump();
        found = offers(feed.lines());
        if !found.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    if found.is_empty() {
        log.push_str("  item screen up but no offers in the feed; nothing to click\n");
        return Ok(Chosen::NoOffers);
    }

    let _ = crate::observe::settle::wait_for_quiescence(win, 0.02, Duration::from_secs(8));

    // Rank by kind, then break the tie at random.
    //
    // A failed catalogue load is not fatal: every offer then ranks unknown, they all tie, and the
    // pick falls back to exactly the arbitrary choice this used to make unconditionally. Worth
    // saying out loud in the log, because "took the wrong-looking item" and "could not read the
    // game's items directory" look identical from the outside otherwise.
    let catalogue = match crate::items::Catalogue::load(game_dir) {
        Ok(c) => Some(c),
        Err(e) => {
            log.push_str(&format!("  could not read the item catalogue ({e}); picking at random\n"));
            None
        }
    };
    let kind_of = |key: &str| catalogue.as_ref().and_then(|c| c.kind(key)).map(|s| s.to_string());
    log.push_str(&format!(
        "  offered: {}\n",
        found
            .iter()
            .map(|(k, _, _)| format!("{k} ({})", kind_of(k).unwrap_or_else(|| "?".into())))
            .collect::<Vec<_>>()
            .join(", ")
    ));

    let best = found.iter().map(|(k, _, _)| rank(kind_of(k).as_deref())).min().unwrap_or(0);
    let shortlist: Vec<&Offer> =
        found.iter().filter(|(k, _, _)| rank(kind_of(k).as_deref()) == best).collect();

    // Seeded from the clock so repeated runs do not always take the same position when the shortlist
    // has more than one item — an arbitrary-but-fixed choice would hide a click that only ever works
    // on one pedestal.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0);
    let (key, ix, iy) = shortlist[nanos % shortlist.len()].clone();
    log.push_str(&format!(
        "  picking **{key}** ({}) at ({ix},{iy}), best of {} offers\n",
        kind_of(&key).unwrap_or_else(|| "unknown kind".into()),
        found.len()
    ));

    // Two independent confirmations, because they answer different questions.
    //
    // The pedestal's luminance says *this* item reacted, which is what separates a landed click from
    // one that hit the next pedestal along. `Confirm` losing its greyed form says the game agrees a
    // selection exists, which is authoritative but says nothing about which item.
    let probe = (ix - 90, iy - 90, 180, 180);
    let luma_at = || -> Result<f64, crate::Error> {
        crate::win::capture::capture_client_rect(win, probe.0, probe.1, probe.2, probe.3)
            .map(|f| crate::combat::luma(&f, probe.2 / 2, probe.3 / 2, 60))
    };
    let before = luma_at()?;
    let (sx, sy) = win.client_to_screen(ix, iy)?;

    let mut picked = false;
    for attempt in 1..=PICK_ATTEMPTS {
        click_at(sx, sy)?;
        park(win);
        std::thread::sleep(Duration::from_millis(600));
        let after = luma_at()?;
        let moved = (after - before).abs() > crate::combat::CHANGED;
        let live = !nothing_picked(win);
        log.push_str(&format!(
            "  attempt {attempt}: pedestal luma {before:.1} -> {after:.1} ({}), Confirm {}\n",
            if moved { "changed" } else { "unchanged" },
            if live { "live" } else { "still greyed" }
        ));
        if moved && live {
            picked = true;
            break;
        }
    }
    if !picked {
        return Ok(Chosen::CouldNotSelect);
    }

    // Space rather than a click: the button declares `userFunctionName = 'affirmative'` and
    // `activeIf = selection`, so it cannot fire before something is selected — the guard is the
    // game's own, and we have just confirmed the condition it tests.
    keys.focus();
    std::thread::sleep(Duration::from_millis(300));
    keys.press_key(VK_SPACE, SC_SPACE)?;
    Ok(Chosen::Took(key))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(|l| l.to_string()).collect()
    }

    /// The rows exactly as they reach us, copied from a live raw log — tab-expanded by the console,
    /// column-padded, with a three-word item name in the middle column.
    #[test]
    fn reads_the_rows_in_the_form_the_console_actually_delivers() {
        let got = offers(&lines(
            "Item selection:\n\
             \x20       armourLeatherGloves     Blacksmith gloves       480     540\n\
             \x20       iBeforeEBad     Weird veiny beige thumb 960     540\n\
             \x20       goldIdol        Gold idol       1440    540",
        ));
        assert_eq!(got.len(), 3, "this screen was showing three items");
        assert_eq!(got[0], ("armourLeatherGloves".into(), 480, 540));
        // The one that matters: a name with spaces must not be read as extra columns.
        assert_eq!(got[1], ("iBeforeEBad".into(), 960, 540));
        assert_eq!(got[2], ("goldIdol".into(), 1440, 540));
    }

    /// Tabs must work too — the parser should not care which form it is handed.
    #[test]
    fn tab_separated_rows_still_parse() {
        let got = offers(&lines(
            "Item selection:\n\
             \tphoenixFeather\tPhoenix feather\t480\t540\n\
             \tgildedTetraTeabag\tGilded tetra teabag\t1440\t540\n\
             ach_something\talready achieved",
        ));
        assert_eq!(got.len(), 2, "the trailing non-row ends the block");
        assert_eq!(got[0], ("phoenixFeather".into(), 480, 540));
    }

    #[test]
    fn takes_the_latest_block_not_the_first() {
        // A second screen must not pick up the previous one's offers.
        let got = offers(&lines(
            "Item selection:\n\
             \toldThing\tOld\t480\t540\n\
             noise\n\
             Item selection:\n\
             \tnewThing\tNew\t960\t540",
        ));
        assert_eq!(got, vec![("newThing".to_string(), 960, 540)]);
    }

    #[test]
    fn an_announcement_with_no_rows_yields_nothing() {
        assert!(offers(&lines("Item selection:")).is_empty());
    }

    #[test]
    fn passive_outranks_gear_outranks_everything_else() {
        assert!(rank(Some("passive")) < rank(Some("gear")));
        assert!(rank(Some("gear")) < rank(Some("consumable")));
        assert_eq!(rank(Some("potion")), rank(Some("scroll")), "unusable kinds tie");
        assert_eq!(rank(None), rank(Some("potion")), "an unreadable kind is no worse than a potion");
    }

    /// The screen we actually met, keys and all: gloves are `passive`, the idol is `gear`, and the
    /// thumb is a `consumable`. The gloves win outright — no tie, so no randomness involved.
    #[test]
    fn the_live_screen_picks_the_gloves() {
        let offered = ["armourLeatherGloves", "iBeforeEBad", "goldIdol"];
        let kind = |k: &str| match k {
            "armourLeatherGloves" => Some("passive"),
            "goldIdol" => Some("gear"),
            _ => Some("consumable"),
        };
        let best = offered.iter().map(|k| rank(kind(k))).min().unwrap();
        let short: Vec<&str> =
            offered.iter().copied().filter(|k| rank(kind(k)) == best).collect();
        assert_eq!(short, vec!["armourLeatherGloves"]);
    }

    /// With no catalogue every kind reads unknown, so nothing is preferred and the shortlist is the
    /// whole screen — the behaviour this had before a policy existed, rather than a stall.
    #[test]
    fn an_unreadable_catalogue_leaves_every_offer_eligible() {
        let offered = ["a", "b", "c"];
        let best = offered.iter().map(|_| rank(None)).min().unwrap();
        let short = offered.iter().filter(|_| rank(None) == best).count();
        assert_eq!(short, 3);
    }
}
