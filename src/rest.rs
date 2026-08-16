//! Health, and when to go and get it back.
//!
//! Combat is the only thing that costs health and the only thing health is for, so the policy is
//! simple: after a node that cost us real health, top up before taking the next fight. The user set
//! the bar at **four or more** lost at a single overworld node.
//!
//! Health is read straight from `mainSaveData.player` (`health`, `maxHealth`) — no vision needed.
//!
//! ## Where resting happens, and what it costs
//!
//! `ui/rest.lua` serves both, and they are not priced the same:
//!
//! - **Inn**, inside a village: `costText = '-10'` and `golddisplay.add(-10)` (`:59`, `:117`) — ten
//!   gold. Reached by entering the village subworld, walking to the `store_inn` subnode, `Enter`
//!   (`overworld/generators/village.lua:361-368`), then `Rest` (`ui/inn.lua:55`).
//! - **Campfire**: `costIcon` is a campfire when `overworldview.areaUnused()` and firewood
//!   otherwise, with **`costText = nil`** (`ui/rest.lua:54-55`) — no gold at all.
//!
//! So a campfire is the cheaper rest and needs no subworld, no shop, and no walking to a subnode.
//! The user asked for villages; campfires are worth preferring where one is adjacent, and this
//! module ranks them accordingly. Both restore `min(healthNeed, healthGive)` where
//! `healthNeed = maxHealth - health` (`:159-161`), so neither is guaranteed to be a full heal.
//!
//! Resting also sets `lastRested` (`:265`), which is what clears the `tiredness` status
//! (`overworldview.lua:225-233`) — a second reason not to put it off indefinitely.
//!
//! ## The dream, which is why resting is not a fire-and-forget action
//!
//! Resting can drop into the `Physics dream` (`overworld/events/rested.lua:10-17`). It is random and
//! becomes rarer with exposure — gated on `love.math.random(1,4)==1` the first times and then
//! `random(1, physicsDream)==1` — so it will not show up reliably in testing and must be handled
//! whenever it does.
//!
//! Two properties make it hostile to the patterns used everywhere else in this project:
//!
//! - **`Wake up` is bound to `goBack`, not `affirmative`** (`:112-116`). Space will not dismiss it,
//!   and Escape — the other `goBack` binding — is forbidden here, since it maps to
//!   `goBack() or options()` and can strand a run in the options menu. So this button must be
//!   **clicked**, at [`crate::innplay::WAKE_UP`].
//! - **It is not there when the dream starts.** `showIf = function() return wakeUp end`, and outside
//!   the `alreadyCorrupt` case `wakeUp` only becomes true from an `onCollisionCallbacks` handler
//!   (`:56-70`) — that is, when two tiles in the dream's physics simulation collide. There is no
//!   fixed duration to wait out. The button's arrival has to be *observed*.
//!
//! What can be known in advance is that a dream is *coming*: `doRest` assigns `doingEvent` and then
//! logs it (`:364-366`), so the console announces the queued event two seconds before the screen
//! changes. [`crate::innplay`] acts on that, and the pressing lives there — this module stays
//! decision-only.

/// Health lost at one overworld node that sends us looking for a rest.
///
/// The user's number. Below it, the detour costs more time than the health is worth.
pub const REST_THRESHOLD: i64 = 4;

/// The player's health, from `mainSaveData.player`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Health {
    pub current: i64,
    pub max: i64,
}

impl Health {
    pub fn from_save(save: &crate::game::save::Table) -> Option<Self> {
        Some(Health {
            current: save.int_at("player.health")?,
            max: save.int_at("player.maxHealth")?,
        })
    }

    /// How much a full rest would restore.
    pub fn missing(&self) -> i64 {
        (self.max - self.current).max(0)
    }

    pub fn is_full(&self) -> bool {
        self.missing() <= 0
    }
}

/// Did this node cost enough health to be worth resting off?
///
/// Takes the readings from either side of the node rather than a single "damage" number, because
/// health can also go *up* — items, effects, a rest we already took — and a naive subtraction of a
/// larger figure from a smaller one would report a huge loss.
pub fn should_rest(before: Health, after: Health) -> bool {
    let lost = before.current - after.current;
    lost >= REST_THRESHOLD && !after.is_full()
}

/// Is health low enough that resting is the priority, whatever route got us here?
///
/// [`should_rest`] is a **delta** rule — "that node was expensive" — and it has a blind spot it
/// cannot see out of: it only fires when we watched health fall. A run that *resumes* at 4/12 has no
/// before-reading to compare against, so nothing sets the intent and the navigator walks into the
/// next fight at a third health. That is exactly what a live run did.
///
/// This is the absolute rule that closes it: below half, seek a rest, however we arrived.
///
/// Half is the game's own line, not one invented here. `rpgview.lua:1643` makes an enemy `fear`able
/// at `health*2 < maxHealth`, so it is the threshold the game itself treats as "this one is in
/// trouble" — the same arithmetic, strict, so an exact half is not yet low.
pub fn health_is_low(now: Health) -> bool {
    now.current * 2 < now.max
}

/// What the inn charges (`ui/rest.lua:49`, `getPlayerGold() >= 10`).
pub const INN_COST: i64 = 10;

/// **Is resting at a campfire actually implemented?** No, and until it is, one is not a rest site.
///
/// The dev's call, 2026-08-16: *the campfire stalled the run; we can sidestep this by not trying to
/// rest there, and mark it post-MVP.*
///
/// The planner has always known a campfire is the *better* rest — no subworld to cross, no subnode
/// to walk to, no gold ([`Site::rank`]) — and the driver has never had a handler for arriving at
/// one. So `Goal::Rest` at a campfire is a trip that cannot pay: the run walks there, nothing
/// happens, `wants_rest` is still true, and the planner picks the next site along.
/// `spike-run-20260816-0802Z.md` steps 15-16 are exactly that, `l1 -> l7 (for l7, Rest)` followed
/// immediately by `l7 -> l32 (for l32, Rest)`.
///
/// It was a wasted trip before and it is a **run-ender now**, because nothing is achieved on such a
/// lap — `WorldMap::progress` does not move — so `LoopGuard` will end the run at the fourth one.
/// That is the guard doing its job, and the right response is to stop nominating a destination we
/// cannot use rather than to relax the guard.
///
/// One constant rather than a deletion so the reversal is one word. What re-enabling needs is an
/// arrival handler: `Rest` on a campfire is an ordinary area button, and the cost side is already
/// read — [`fuel`] totals the firewood and `areaUnused` says whether the first rest is free.
pub const CAMPFIRE_REST_IS_BUILT: bool = false;

/// A place that can restore health.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Site {
    /// Free while the area is unused, then firewood.
    Campfire,
    /// Inside a village. Ten gold, and a walk across a subworld to reach it.
    Inn,
}

impl Site {
    /// Higher is better. A campfire beats an inn — no subworld to enter, no subnode to cross, no
    /// gold — and this is a plain preference because [`can_rest_at`] has already ruled out the
    /// campfires we could not actually use.
    pub fn rank(self) -> u8 {
        match self {
            Site::Campfire => 2,
            Site::Inn => 1,
        }
    }
}

/// Which kind of rest site a heading names, if any.
pub fn site(heading: &str) -> Option<Site> {
    let h = heading.trim_end();
    if h.ends_with("campfire") {
        Some(Site::Campfire)
    } else if h.ends_with("village") || h.ends_with("inn") {
        Some(Site::Inn)
    } else {
        None
    }
}

/// Item groups whose stacked variants count as campfire fuel.
///
/// `items/fuel.lua` builds each as `('%s%d'):format(group, i)` for `i` in 1..5, with
/// `usefulness.campfirerest = 'fuel'` and `craft.fill = i`. So the trailing digit *is* the fill
/// value, which is what `getPlayerCampfireFuelCount` sums (`overworld.lua:483-492`).
const FUEL_GROUPS: &[&str] = &["scrapwood", "firewood", "charcoal"];

/// How much campfire fuel the save says we carry.
///
/// Mirrors `getPlayerCampfireFuelCount`: walk `playerData.items`, keep the ones that are fuel, and
/// add up their `craft.fill`. Anything nonzero makes a campfire usable even where the area has
/// already been used.
pub fn fuel_count(items: &[String]) -> i64 {
    items
        .iter()
        .filter_map(|it| {
            let group = FUEL_GROUPS.iter().find(|g| it.starts_with(**g))?;
            it[group.len()..].parse::<i64>().ok()
        })
        .sum()
}

/// Fuel carried, read from `mainSaveData.items`.
pub fn fuel_from_save(save: &crate::game::save::Table) -> i64 {
    let Some(items) = save.table_at("items") else { return 0 };
    let keys: Vec<String> =
        items.arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect();
    fuel_count(&keys)
}

/// Can we actually rest here, given what we are carrying?
///
/// `getCanRest` (`ui/rest.lua:43-50`) gates the two differently:
///
/// ```lua
/// if campfire then
///     return location.type=='campfire' and areaUnused() or getPlayerCampfireFuelCount()~=0
/// end
/// return overworld.getPlayerGold()>=10
/// ```
///
/// So an inn is a straight gold check, while a campfire is free only while the area is **unused**
/// and wants fuel thereafter. Both halves are knowable from the save, so neither is a gamble:
/// `areaHasBeenUsed(name)` is exactly `areaFlag(name..'_used')` (`overworldview.lua:215-218`), and
/// those flags live in `mainSaveData.overworld.areaFlags`, while fuel is counted by [`fuel_count`].
///
/// Never walk to a campfire hoping. Without fuel, a campfire we have already used restores nothing,
/// and the trip is spent for it — the real save has `start_used = true`, so the nearest campfire on
/// the current island is exactly that case. Go only when we carry fuel, or when the flag says for
/// certain it is untouched.
///
/// `restInsomnia` gear blocks resting outright (`:45`) and is the one part we genuinely cannot see.
pub fn can_rest_at(site: Site, gold: i64, fuel: i64, area_unused: bool) -> bool {
    match site {
        Site::Campfire => CAMPFIRE_REST_IS_BUILT && (fuel > 0 || area_unused),
        Site::Inn => gold >= INN_COST,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hp(current: i64, max: i64) -> Health {
        Health { current, max }
    }

    #[test]
    fn four_lost_is_the_bar() {
        assert!(should_rest(hp(12, 12), hp(8, 12)), "exactly four qualifies");
        assert!(!should_rest(hp(12, 12), hp(9, 12)), "three does not");
        assert!(should_rest(hp(12, 12), hp(2, 12)));
    }

    #[test]
    fn there_is_no_point_resting_at_full_health() {
        // Lost five, then something healed us back. A rest would restore nothing.
        assert!(!should_rest(hp(12, 12), hp(12, 12)));
    }

    #[test]
    fn gaining_health_is_never_a_reason_to_rest() {
        // The reading can rise -- a potion, an effect, a rest already taken. Subtracting the wrong
        // way round would read that as a huge loss and send us off to heal after being healed.
        assert!(!should_rest(hp(4, 12), hp(11, 12)));
    }

    #[test]
    fn a_campfire_outranks_a_village() {
        // Free and immediate, against ten gold and a walk across a subworld.
        assert!(site("Cottam campfire").unwrap().rank() > site("Ulrome village").unwrap().rank());
        // Real headings from the captured dump.
        assert_eq!(site("Greenoak Backwoods campfire"), Some(Site::Campfire));
        assert_eq!(site("Ulrome village"), Some(Site::Inn));
        assert_eq!(site("Weedley Copse — level 0 crypt"), None);
        assert_eq!(site("Bainton Clump — level 1 forest"), None);
    }

    #[test]
    fn an_inn_is_useless_without_ten_gold() {
        // `getCanRest` is a flat `getPlayerGold() >= 10`, so walking to a village with nine gold
        // buys a wasted trip and no health.
        assert!(!can_rest_at(Site::Inn, 9, 0, true));
        assert!(can_rest_at(Site::Inn, 10, 0, false));
        assert!(can_rest_at(Site::Inn, 107, 0, false));
    }

    /// A campfire is not a rest site while the press for one is unwritten.
    ///
    /// See [`CAMPFIRE_REST_IS_BUILT`]. The cost side is still modelled and still correct — it is the
    /// *arriving* that does nothing — so this asserts both halves: the gate is shut, and what it is
    /// shutting is a judgement that would otherwise be right.
    #[test]
    fn a_campfire_is_not_a_rest_site_until_arriving_at_one_does_something() {
        assert!(!can_rest_at(Site::Campfire, 0, 0, true), "unused and free, and still not offered");
        assert!(!can_rest_at(Site::Campfire, 0, 3, false), "firewood does not change it either");
        assert!(!can_rest_at(Site::Campfire, 999, 0, false), "and a used one never could");

        // The judgement underneath, unchanged and ready for the day the press exists: a used
        // campfire with no firewood restores nothing, either of the other two states does.
        let cost_side = |fuel, unused| fuel > 0 || unused;
        assert!(!cost_side(0, false), "used, no wood — the walk would pay nothing");
        assert!(cost_side(0, true), "known unlit-and-unused is enough");
        assert!(cost_side(3, false), "so is firewood");
    }

    #[test]
    fn a_usable_campfire_always_beats_paying() {
        // Ranking is a plain preference now that `can_rest_at` has already discarded the campfires
        // we could not use. Whenever both survive that filter, the free one wins.
        assert!(Site::Campfire.rank() > Site::Inn.rank());
    }

    #[test]
    fn fuel_is_the_trailing_digit_of_a_stack() {
        // `items/fuel.lua` builds `scrapwood1..5` etc. with `craft.fill = i`.
        assert_eq!(fuel_count(&["scrapwood3".into()]), 3);
        assert_eq!(fuel_count(&["firewood5".into(), "charcoal2".into()]), 7);
        // Real inventory contents from the sandbox save: neither is fuel.
        assert_eq!(fuel_count(&["healthPotion4".into(), "antivenom4".into()]), 0);
    }

    #[test]
    fn fuel_reads_out_of_the_save() {
        let save = crate::game::save::parse(
            r#"return { items = { "healthPotion4", "firewood2" }, player = { gold = 3 } }"#,
        )
        .unwrap();
        assert_eq!(fuel_from_save(&save), 2);
    }

    #[test]
    fn health_reads_out_of_the_save() {
        let save = crate::game::save::parse(
            r#"return { player = { health = 8, maxHealth = 12, gold = 107 } }"#,
        )
        .unwrap();
        let h = Health::from_save(&save).unwrap();
        assert_eq!(h.missing(), 4);
        assert!(!h.is_full());
    }
}
