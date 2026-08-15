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

/// The extra the game sometimes staples to **one** of the offered items.
///
/// ## Where these come from, and why there are exactly three
///
/// `overworld.lua:1119-1127` rolls `rarity = random(0,9)` and, on `rarity <= 2`, picks a single
/// lucky item out of the drops and hangs `getRewardBoons(rng, luckyKey, rarity==0 and 'rare' or
/// 'uncommon', class)` on it. `getRewardBoons` (`:640-656`) joins the source item's own declared
/// boons with everything whose `sources` name `<type>RewardBoon` or `rewardBoon` at that rarity,
/// filters for class and usefulness, and returns **one**. The whole pool in `items/`:
///
/// ```text
///   gearSlotsBuff   Gear item slot   gearRewardBoon = 'rare'       items/ephemeral.lua:69
///   gold50          Pile of 50 gold  rewardBoon = 'uncommon'       items/ephemeral.lua:103
///   healthHeal4     Heal 4           rewardBoon = 'uncommon'       items/ephemeral.lua:37
/// ```
///
/// So the gear slot is *rare* and only ever attaches to a gear-type item; the other two are
/// *uncommon*. Anything else is an item declaring its own `uncommonRewardBoons`, which is what
/// [`Boon::Other`] covers.
///
/// ## The policy is the dev's, 2026-08-15
///
/// > If there is a gear slot, eagerly take the bonus gear slot. Then the bonus reward only becomes a
/// > tiebreaker: 50 gold always holds value, whereas the heal only holds value if we are below max
/// > health. The consumable holds the least value of the bonuses.
///
/// [`Boon::GearSlot`] is therefore not a tiebreaker at all — see [`Boon::is_eager`]. The rest rank
/// by [`Boon::worth`] and only separate offers that the kind ranking has already tied.
///
/// The game agrees about the heal, which is worth noting because it is a rule arrived at twice
/// independently: `healthHeal4.isUseless` is `health + 2 >= maxHealth` (`items/ephemeral.lua:41-45`),
/// and `filterUselessBoons` drops it before the roll. So a heal boon on offer is *already* one the
/// game thought we could use — this ranking is the second, stricter opinion.
///
/// # ⚠ This policy is NOT HOOKED UP
///
/// Nothing supplies it. [`choose`] builds an empty boon list, every offer reads "no boon", and the
/// pick is decided entirely by [`PREFERRED_KINDS`] exactly as it was before any of this existed. The
/// code below is a decision waiting for an input, and it is deliberately not pretending otherwise.
///
/// **The console does not carry the attachment.** `ui/itemselection.lua:413-428` prints each item's
/// key, name and screen position and nothing else; the boon is held in `extras.bonuses[item]` and
/// never logged. The game is never modified, so that is the end of the console route.
///
/// **The screen does carry it, and the dev has ruled that route out.** `:102-114` draws the boon
/// item's own 32x32 icon at 2x inside the item button and `:395` puts a lens flare over the lucky
/// pedestal, so three templates and the button transform would read it. The dev, 2026-08-15: *I'd
/// prefer not to rely on the rendering.* Recorded here so nobody re-derives it as a fresh idea and
/// spends an evening on it — it is a known option that was considered and declined.
///
/// **What is left** is deriving the roll: `overworld.lua:1120` seeds
/// `love.math.newRandomGenerator(seed * location.seed * 1000)` when `playerData.overworld.seedDrops`
/// is set, and every draw after that is arithmetic we could in principle reproduce. That is a real
/// route and a deep one, and it is not being started on a guess about whether it is wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boon {
    /// `gearSlotsBuff` — a permanent extra gear slot. Taken eagerly, whatever it is stapled to.
    GearSlot,
    /// `gold50` — fifty gold, which is always worth something.
    Gold,
    /// `healthHeal4` — four health, worth nothing at all when we are already full.
    Heal,
    /// Anything else: an item's own declared boon, which is a consumable in practice.
    Other,
}

impl Boon {
    /// Which item key this is, as the game names it.
    pub fn from_key(key: &str) -> Boon {
        match key {
            "gearSlotsBuff" => Boon::GearSlot,
            "gold50" => Boon::Gold,
            "healthHeal4" => Boon::Heal,
            _ => Boon::Other,
        }
    }

    /// Does this override the item ranking rather than merely break its ties?
    ///
    /// Only the gear slot. It is the one boon that changes what the *rest of the run* can equip:
    /// `playerHasGearEquipped` is `has <= getPlayerGearSlotCount()` (`overworld.lua:307-310`) with a
    /// default of three, so every piece of gear past the third is carried and inert. A slot converts
    /// dead weight already in the bag into working gear, which no single item on a pedestal can do.
    pub fn is_eager(self) -> bool {
        self == Boon::GearSlot
    }

    /// Tiebreak value, **higher is better**, given whether we are below full health.
    ///
    /// `hurt` is the whole of the heal's worth: at full health `affectPlayerHealth(4)` does nothing
    /// at all, so it ranks under the consumable rather than over it.
    pub fn worth(self, hurt: bool) -> u8 {
        match self {
            Boon::GearSlot => 4,
            Boon::Gold => 3,
            Boon::Heal if hurt => 2,
            Boon::Other => 1,
            // Nothing is gained, so it loses to the consumable we might at least drink later.
            Boon::Heal => 0,
        }
    }
}

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

/// Is a "Choose one:" screen up, untouched?
///
/// Cheap: one 250x100 comparison at a known offset, no search — see [`crate::act::score_exact`].
/// A capture fault reads as absent, which is right for a poll that runs every iteration and wrong
/// for a decision, so nothing destructive hangs off this.
pub fn on_screen(win: &GameWindow) -> bool {
    matches!(crate::act::score_exact(win, &crate::act::REWARD_CONFIRM),
        Ok(q) if q >= crate::act::REWARD_SCREEN_PRESENT)
}

/// Is `Confirm` still greyed — i.e. has nothing been selected?
///
/// The game's own answer, since `activeIf` *is* the selection (`ui/itemselection.lua:274`). This is
/// the sole gate on whether a click registered; see [`choose`] for why the pedestal probe that used
/// to accompany it was removed rather than recalibrated.
///
/// Needs its own threshold, above [`crate::act::REWARD_SCREEN_PRESENT`]: the active button is the
/// same plank and lettering in another colour and scores 0.7255 against the greyed template, so a
/// threshold set to *detect the screen* would also call a live button greyed.
pub fn nothing_picked(win: &GameWindow) -> bool {
    matches!(crate::act::score_exact(win, &crate::act::REWARD_CONFIRM),
        Ok(q) if q >= crate::act::REWARD_NOTHING_PICKED)
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
/// `hurt` is the one thing a boon ranking needs that the screen cannot say: the Heal boon is worth
/// nothing at full health. Callers hold a health reading already; passing `false` when they do not
/// is the safe direction, since it only ever demotes the heal.
pub fn choose(
    win: &GameWindow, feed: &mut Feed, keys: &PostMessageInput, game_dir: &std::path::Path,
    log: &mut String, deadline: Instant, hurt: bool,
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

    // **NOT HOOKED UP.** This is always empty, so every branch below it is inert and the pick is
    // exactly what it was before boons existed. See [`Boon`] for the policy, which is built and
    // tested; what is missing is any way to learn *which* offer carries *which* boon.
    let boons: Vec<(String, Boon)> = Vec::new();
    let boon_on = |key: &str| boons.iter().find(|(k, _)| k == key).map(|(_, b)| *b);

    // Eager, and ahead of everything: a gear slot is not a tiebreak, it is a change to what the
    // whole bag can equip. The dev's word for it was "eagerly".
    let eager: Vec<&Offer> =
        found.iter().filter(|(k, _, _)| boon_on(k).is_some_and(Boon::is_eager)).collect();

    let best = found.iter().map(|(k, _, _)| rank(kind_of(k).as_deref())).min().unwrap_or(0);
    let by_kind: Vec<&Offer> =
        found.iter().filter(|(k, _, _)| rank(kind_of(k).as_deref()) == best).collect();
    // Only *then* the boon, and only between offers the kind ranking already called equal — which
    // is what "the bonus reward only becomes a tiebreaker" means.
    let shortlist: Vec<&Offer> = match eager.is_empty() {
        false => eager,
        true => {
            let top = by_kind
                .iter()
                .map(|(k, _, _)| boon_on(k).map(|b| b.worth(hurt)).unwrap_or(0))
                .max()
                .unwrap_or(0);
            match top {
                0 => by_kind,
                _ => by_kind
                    .into_iter()
                    .filter(|(k, _, _)| {
                        boon_on(k).map(|b| b.worth(hurt)).unwrap_or(0) == top
                    })
                    .collect(),
            }
        }
    };

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

    // `Confirm` going live is the whole check.
    //
    // There used to be a second one: sample the pedestal's luminance before and after, and require
    // *both* it and `Confirm`. That cost a live run its reward. The pedestal moved 10.3 against a
    // threshold of 12.0 borrowed from `combat::CHANGED` — a constant measured for *tile* selections,
    // which move 42–141 — so a screen that had been successfully selected from was abandoned three
    // attempts running while `Confirm` sat there live and saying so.
    //
    // Recalibrating it was the obvious repair and the wrong one. Ask what the check was *for*: it
    // distinguished "our pedestal reacted" from "the click landed on the one next door". But the
    // coordinates are not ours to get wrong — `ui/itemselection.lua:419` prints where the game drew
    // each item, and we click exactly that. Mis-aiming is not a failure mode of reading a number out
    // of a log. The check was guarding a door with no building behind it, and paying a capture, a
    // probe and a second calibration story for the privilege.
    //
    // If "which item" ever does need verifying, the feed answers it after the fact and for free —
    // `items.give(item, 'reward')` names what was granted. Pixels are the wrong instrument for a
    // question the game answers in words.
    let (sx, sy) = win.client_to_screen(ix, iy)?;

    let mut picked = false;
    for attempt in 1..=PICK_ATTEMPTS {
        click_at(sx, sy)?;
        park(win);
        std::thread::sleep(Duration::from_millis(600));
        let live = !nothing_picked(win);
        log.push_str(&format!(
            "  attempt {attempt}: Confirm {}\n",
            if live { "live — selection registered" } else { "still greyed" }
        ));
        if live {
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
mod boon_tests {
    use super::*;

    /// The dev's ordering, 2026-08-15, stated as a policy rather than a set of numbers.
    #[test]
    fn a_gear_slot_is_taken_eagerly_and_the_rest_only_break_ties() {
        assert!(Boon::GearSlot.is_eager(), "a slot changes what the whole bag can equip");
        for b in [Boon::Gold, Boon::Heal, Boon::Other] {
            assert!(!b.is_eager(), "{b:?} is a tiebreaker, not an override");
        }
    }

    #[test]
    fn gold_always_holds_value_and_the_heal_only_when_hurt() {
        // Hurt: gold, then the heal, then the consumable.
        assert!(Boon::Gold.worth(true) > Boon::Heal.worth(true));
        assert!(Boon::Heal.worth(true) > Boon::Other.worth(true));
        // Full: the heal restores nothing, so it drops under the consumable rather than merely
        // level with it. `affectPlayerHealth(4)` at full health is the whole reason.
        assert!(Boon::Other.worth(false) > Boon::Heal.worth(false));
        assert_eq!(Boon::Heal.worth(false), 0);
        // And gold is unmoved by our health, which is what "always holds value" means.
        assert_eq!(Boon::Gold.worth(true), Boon::Gold.worth(false));
    }

    /// The keys are the game's, and a boon we do not recognise must not outrank one we do.
    #[test]
    fn the_three_known_boons_are_named_from_the_game() {
        assert_eq!(Boon::from_key("gearSlotsBuff"), Boon::GearSlot);
        assert_eq!(Boon::from_key("gold50"), Boon::Gold);
        assert_eq!(Boon::from_key("healthHeal4"), Boon::Heal);
        // An item's own `uncommonRewardBoons` -- a consumable in practice.
        assert_eq!(Boon::from_key("healthPotion"), Boon::Other);
        assert!(Boon::Gold.worth(true) > Boon::Other.worth(true));
    }
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
