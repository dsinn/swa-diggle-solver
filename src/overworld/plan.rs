//! Where to go next, and why — the goal ladder.
//!
//! Split out of `overworld.rs` on 2026-08-21 (#76). This is the module that decides; everything
//! around it supplies facts. [`WorldMap::plan`] is the ladder itself and [`WorldMap::next_target`]
//! its entry point, with [`WorldMap::next_hop`] turning a chosen target into the single step that
//! serves it.
//!
//! ## The ladder is ordered, and the order is the design
//!
//! Each rung declines for a stated reason before the next is considered, so a run that ends up
//! exploring has been refused by every errand above it. That is what makes the log readable: a hop
//! prints the goal it serves, and the goal is a claim about which rungs said no.
//!
//! ## Two survival conditions sit inside it
//!
//! `SHRINES_BEFORE_THE_ANOMALY` consecrations gate the anomaly rung, and the Well-Rested bank gates
//! a deliberate fight — [`WorldMap::stacks_short_ahead`] is how deep the want is,
//! [`WorldMap::stacks_to_buy`] how much of it the purse may serve, and they are **not** the same
//! number. Printing one where the other was meant is how a contradiction went unread for a whole
//! run; see the note on [`WorldMap::well_rested`].
//!
//! ## A gate with no release is a stall
//!
//! Every condition here has an escape at the foot of the ladder, reached only when the frontier is
//! exhausted. The loop guard is not a substitute: it ends a run four laps later having learned
//! nothing.

use super::{
    dist_or_far, Place, WorldMap, ANOMALY_KEY, HEART_COST, HEART_FLOOR,
    SHRINES_BEFORE_THE_ANOMALY,
};
// Doc links only. See the note in `place.rs`: the arguments came across intact rather than being
// requalified to suit the file layout.
#[allow(unused_imports)]
use super::Risk;
use std::collections::{BTreeMap, BTreeSet};

/// Where to head, and why. The reason is carried so a route can be explained rather than just taken.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub target: String,
    pub reason: Goal,
    /// What direction exploring was steered by, when it was steered at all.
    ///
    /// `(bearing, placed, total)` — the node being aimed at, how many frontier candidates could be
    /// measured against it, and how many there were. Recorded because steering has three ways to do
    /// nothing and the log could not tell them apart: no bearing at all, a bearing nothing can place,
    /// and a bearing sitting on the node we are already standing on. All three come out as
    /// `Goal::Explore` and a nearest-unvisited hop, which is what a run that had never heard of
    /// steering would do — so "we fixed this" and "it went the wrong way" were both true and neither
    /// was checkable. Live 2026-08-12 a run wandered out of the corruption into a bandit camp and
    /// this is the field that would have said why.
    pub steered_by: Option<(String, usize, usize)>,
}

/// Why a hop is being taken.
///
/// **A label, not a control switch.** Nothing branches on it except the log; it exists so that a
/// report can be read afterwards and say what the run believed it was doing.
///
/// `Copy` was dropped when [`Goal::RouteTo`] arrived. It is not free — every match site had to be
/// looked at — but the alternative was a parallel enum of "goals that can be sought", kept in step
/// with this one by hand, which is the shape that produced `Screen::ALL` and its keeper test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Goal {
    /// The anomaly is open and this is it. Everything else can wait.
    ///
    /// Named against [`Goal::OpenAnomaly`]: one *triggers* the portal, this one *finishes* it. The
    /// pair used to be `OpenTheAnomaly` and a bare `Anomaly`, which read as a step and its subject
    /// rather than as two ends of the same job.
    CloseAnomaly,
    /// A level > 3 surface node: arriving opens the anomaly.
    OpenAnomaly,
    /// Health is down and somewhere nearby can restore it.
    Rest,
    /// **Not hurt — under-banked.** An inn, to buy Well-Rested stacks before a deliberate deep fight.
    ///
    /// Distinct from [`Goal::Rest`] in what it is answering. `Rest` is a response to damage already
    /// taken and stops when the bar is full; this is preparation for damage not yet taken, it is
    /// bought at full health, and it stops when the bank is deep enough for the fight ahead
    /// ([`crate::rest::stacks_wanted`]).
    ///
    /// The game permits it: `getCanRest` for an inn is a flat `getPlayerGold() >= 10`
    /// (`ui/rest.lua:49`) with no health condition, and `doRest` grants the stack on its own line,
    /// separate from the heal (`:355-357`). So a run at full health can and should keep paying.
    StockUp,
    /// A village whose general store sells a `Heart` — four maximum health for a hundred gold.
    ///
    /// Ranked with the shrine detours rather than with [`Goal::Rest`]: it is not a response to being
    /// hurt, it is preparation bought while the road is free. See [`WorldMap::wants_a_heart`].
    Heart,
    /// An unopened treasure chest. Task #16.
    ///
    /// Ranked with the other detours rather than with exploring, and for the same reason as
    /// [`Goal::Heart`]: it is not a response to anything, it is loot taken while the road is free.
    /// **Opening it is a fight** — `Open` calls `scenarios.chest`
    /// (`overworld/generators/forest.lua:30-39`) — so this only fires while a rest is not wanted,
    /// which is the dev's own condition: *when the goal is not rest and a chest is visible, detour
    /// to it, open it, then rejoin the path.*
    Chest,
    /// A shrine we have not consecrated.
    ///
    /// Ranked below [`Goal::OpenAnomaly`] because consecrating is *impossible* until the anomaly
    /// is open — `showConsecrateButton` requires `hell ~= 0` (`shrine.lua:93-96`). Once it is open,
    /// corruption resets areas to incomplete, so a shrine needing a fight only earns a detour if it
    /// already lies on the route; one that is merely awaiting a `Visit` stays cheap.
    ///
    /// ## An UNCORRUPTED shrine is strictly beneficial, and that is worth stating
    ///
    /// It costs no fight, and the payoff is combat-relevant. The full chain, verified in source:
    ///
    /// ```text
    ///   ShowAGoodButton      hasWon() and not heretic            shrine.lua:36-40
    ///   Consecrate           + majorShrine + hell ~= 0           shrine.lua:92-95
    ///   Pray                 + areaUnused + (hell == 0 or consecrated or desecrated)
    ///                                                            shrine.lua:98-103
    ///   -> wildcardRewards   queues 3 gold-bordered, 3 silver,   utils/blessings.lua:95-110
    ///                        or 1 gold WILDCARD tile
    /// ```
    ///
    /// **Every shrine is a major one.** `overworld/generators/world.lua:86-89` sets
    /// `majorShrine = true` for any location whose key starts with `shrine`, so there is no minor
    /// shrine to pray at without consecrating first. With the anomaly already open — `hell ~= 0` —
    /// that ordering is satisfied, and solve → Consecrate → Pray runs straight through.
    ///
    /// Wildcards are the mechanic Diggle handles best and has exercised live. So an uncorrupted
    /// shrine is the cheapest way to make the *next* fight winnable, which is precisely what a run
    /// at low health needs — and why this outranks [`Goal::EasiestHostile`] rather than being a
    /// luxury deferred until healthy.
    Shrine,
    /// Somewhere unseen, to grow the map toward one of the above.
    Explore,
    /// We know where we want to go and not how to get there, so we explore toward it.
    ///
    /// Carries the goal that was declined, because that is what the hop is really *for* and the log
    /// had no way to say it. `Explore` used to cover this case too, which is how a reader concluded
    /// that exploring meant the anomaly had not opened yet — while the run had in fact been unable to
    /// route to an open portal for its entire length.
    ///
    /// **Not the same question as whether it can steer.** Knowing the destination is one thing;
    /// having a position to aim at is another, and [`Plan::steered_by`] answers that separately. It
    /// can be `None` while this is `Some`, and that combination is exactly the failure that walked a
    /// run out of the corruption on 2026-08-12.
    RouteTo(Box<Goal>),
    /// Hurt, and **everywhere** is hostile — so take the cheapest fight on the map.
    ///
    /// The last resort of [`WorldMap::next_target`]'s first pass. Distinct from the goals above
    /// because it is not pursuing any of them: it is choosing where to bleed. Carries the level so
    /// a caller can log what it is walking into.
    EasiestHostile { level: Option<u32> },
}

/// What we can say about getting somewhere without taking a fight.
///
/// Three answers rather than two, because the missing one was doing damage. See
/// [`WorldMap::access_without_a_fight`] for the dev's correction that produced it: a `false` that
/// meant "a fight is in the way" and a `false` that meant "I have never looked over there" are not
/// the same claim, and the second is not ours to make.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// A route exists over recorded edges and takes no fight.
    Free,
    /// Every route we hold takes a fight, and the fight-free region around us is closed — every
    /// place in it has all the roads the game says it has.
    Blocked,
    /// No route over what we hold, but the fight-free region touches roads we have not looked down,
    /// so it may not end where our edges do. Go and see.
    Unknown,
}

/// One move: the adjacent node to travel to, and the plan it serves.
#[derive(Debug, Clone, PartialEq)]
pub struct Hop {
    pub step: String,
    pub plan: Plan,
    /// True when this step is on a route to [`Plan::target`], false when it is a guess in its
    /// direction — [`WorldMap::next_hop`]'s fallback for a target nothing can reach.
    ///
    /// Carried purely so the log can tell them apart, and #24 is why. Five consecutive travel lines
    /// reading `(for start, Anomaly)` were read as five steps of a journey to the anomaly; they were
    /// five guesses at a node with no edges, and the run ended in a crypt nobody had chosen.
    /// Route-gating makes this the rare case rather than the usual one, which is exactly why it
    /// needs to be visible when it does happen.
    pub routed: bool,
}

impl WorldMap {
    /// Where to go next.
    ///
    /// Priority follows the objective directly. If the anomaly is open, go to it — nothing else
    /// shortens the run. Otherwise head for a surface node of level > 3, because *arriving* there
    /// opens the anomaly and no fight is required to do it. Failing that, explore, preferring
    /// frontiers that could still reveal such a node.
    ///
    /// Returns `None` when there is nothing left worth travelling to, which is a real answer and not
    /// a failure: it means the map is fully explored with no trigger found, and the caller has to
    /// widen the search rather than keep walking.
    /// Where to go next, with the Well-Rested bank taken into account.
    ///
    /// Two layers, deliberately separate. [`WorldMap::next_errand`] answers *what this run wants to
    /// do*, which is the whole ladder and every rule already written into it. This one asks one
    /// further question of the answer: **is what we are walking into a fight we should not take
    /// with an empty bank?** — and if so, buys stacks first.
    ///
    /// A wrapper rather than another rung, because the condition is not about a destination's
    /// merit. Every branch of the ladder can name a deep fight, and a rung would have to be
    /// repeated in each of them; asking afterwards catches all of them once.
    pub fn next_target(&self) -> Option<Plan> {
        self.next_errand().map(|p| self.bank_first(p))
    }

    /// **Uncorrupted major shrines still available to consecrate**, folded the same way
    /// [`WorldMap::consecrations`] folds them so a shrine and its plaza never count twice.
    ///
    /// The denominator of [`WorldMap::corrupted_shrines_are_needed`]. `abandoned` and `avoid` are
    /// excluded because a shrine the driver gave up on is not a shrine we can still bank — leaving
    /// it in would hold the corrupted ones off the list for ever on the strength of one we cannot
    /// finish.
    fn clean_shrines_left(&self) -> usize {
        self.places
            .values()
            .filter(|p| p.can_be_consecrated() && !p.consecrated && !p.corrupted && !p.avoid)
            .filter(|p| !self.abandoned.contains(&p.key))
            .map(|p| p.key.strip_suffix("_plaza").unwrap_or(&p.key))
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    }

    /// **Has the free supply run out with the bar still short?** Task #74.
    ///
    /// The dev, 2026-08-21: *a corrupted shrine should become a target when it is the last to
    /// satisfy the requirement. That also means that if two shrines are in the corruption, then they
    /// become targets after two consecrations of uncorrupted shrines. Generalize this to any number
    /// of corrupted shrines.*
    ///
    /// Both sentences describe this one condition. The second is what rules out the other reading:
    /// *we can already see we will need them* would admit a corrupted shrine at nought
    /// consecrations whenever the clean ones are too few, and the dev put the moment at **after**
    /// the clean ones are spent. Take the free ones first, always — a corrupted shrine costs a fight
    /// that can end the run, and a free consecration banked before it is one kept.
    ///
    /// **Both halves are load-bearing.** Without the bar test this becomes the shrine-chase of
    /// 2026-08-15 again, since the clean supply is empty once the bar is met and every corrupted
    /// shrine would turn back into a detour on the way to the portal.
    ///
    /// The count is over shrines we **know about**, which is sound only because this is reached with
    /// the portal open, and opening it reveals every shrine in the world — the beams, and skipping
    /// the cutscene does not skip the revelation (`shrine.lua`'s `initStates`, *so we can save right
    /// away and main menu skip*). Before the portal there is no corruption to weigh anyway.
    fn corrupted_shrines_are_needed(&self) -> bool {
        self.consecrations() < SHRINES_BEFORE_THE_ANOMALY && self.clean_shrines_left() == 0
    }

    /// **Buy Well-Rested stacks before walking into a deliberate deep fight.**
    ///
    /// Returns `plan` untouched unless every clause of the dev's rule holds, in which case the trip
    /// becomes [`Goal::StockUp`] at the nearest inn:
    ///
    /// - the destination is a fight we would be **taking on purpose**
    ///   ([`Place::deliberate_fight_level`], which excludes forests for the dev's reason);
    /// - it is deep enough to be worth the errand ([`crate::rest::worth_banking_for`]);
    /// - the bank is short of twice its level ([`crate::rest::stacks_short`]);
    /// - **there is still ten gold**, the dev's floor and the inn's own gate — below it the
    ///   requirement is unmeetable rather than merely unmet, and holding the run at it would stall;
    /// - and a bed can actually be reached.
    ///
    /// The last two are what stop this looping. Every other clause is a fact about the world that
    /// the errand itself changes: each rest spends ten gold and adds one stack, so the run walks
    /// toward the condition it is testing and either satisfies it or runs out of money.
    ///
    /// Note it does **not** consult `wants_rest`. That flag is about damage taken; this is about
    /// damage to come, and the two are independent — a run at full health with an empty bank is
    /// exactly the case this exists for.
    /// How many more Well-Rested stacks a trip to `target` wants. Zero unless every clause holds.
    ///
    /// Split out of [`WorldMap::bank_first`] because the inn needs the same number: the planner uses
    /// it to decide *whether to go*, and [`crate::navigate::Run::rest_at_inn`] uses it to decide
    /// **how many times to press `Rest`** once it is there. Two answers from one function, so a
    /// run cannot set off for stacks it will not then buy.
    fn stacks_short_for(&self, target: &str) -> i64 {
        let Some(level) = self.places.get(target).and_then(Place::deliberate_fight_level) else {
            return 0;
        };
        match crate::rest::worth_banking_for(level) {
            true => crate::rest::stacks_short(self.well_rested, level),
            false => 0,
        }
    }

    /// **How short the bank is for the deepest deliberate fight we know about.**
    ///
    /// The *inn's* question, and deliberately a different one from [`WorldMap::bank_first`]'s.
    /// That one asks "is the place I am walking to a fight I should bank for", because it is
    /// deciding whether to make a special trip. This one asks "are stacks wanted at all", because
    /// by the time it is asked we are already standing in the village and the only decision left is
    /// whether to spend ten gold on the way past.
    ///
    /// Asking the ladder again here would answer with the inn we are standing in — whose
    /// `deliberate_fight_level` is `None` — and the count would collapse to zero exactly where it
    /// is needed. The deepest known fight is the honest stand-in: the anomaly is on the map from
    /// the moment the portal opens, and a crypt we have seen does not stop being a crypt because
    /// this step is heading elsewhere.
    ///
    /// ## Why this terminates, which is the part that matters
    ///
    /// It is the one question this file has been burned by before — a bed that stays wanted is a
    /// loop, and `docs/superpowers/notes/navigation-loops.md` catalogues six of them. **Every rest
    /// moves both terms in the same direction**: one stack banked, ten gold gone. So the condition
    /// is strictly monotone under its own errand and ends either when the bank is deep enough or
    /// when the purse drops below [`crate::rest::INN_COST`] — the dev's floor, and the inn's own
    /// gate. Neither exit depends on anything we cannot see.
    pub fn stacks_short_ahead(&self) -> i64 {
        self.places
            .values()
            .filter_map(Place::deliberate_fight_level)
            .filter(|l| crate::rest::worth_banking_for(*l))
            .max()
            .map(|l| crate::rest::stacks_short(self.well_rested, l))
            .unwrap_or(0)
    }

    /// **Which fight [`WorldMap::stacks_short_ahead`] is pricing.** For the log.
    ///
    /// The shortfall alone is not readable: `16 stack(s) short` is `2 x 8 - 0` and equally
    /// `2 x 9 - 2`, and the run of 2026-08-21 1519Z printed 16 at startup and 0 at every rest with
    /// three thousand gold in the purse and the same level 8 anomaly on the map throughout. Two
    /// numbers cannot be told apart by their difference. Naming the node and the bank beside it
    /// makes the line answer for itself.
    ///
    /// Reproduced from `map-cache/world-0.txt` alone: 699 places, deepest `start`
    /// (`Cottam - level 8 anomaly`), and `stacks_short_ahead` is exactly 16 with an empty bank. So
    /// the *shortfall* was never in doubt; what the line could not say was what it thought the
    /// bank held.
    pub fn deepest_fight(&self) -> Option<(&str, u32)> {
        self.places
            .values()
            .filter_map(|p| p.deliberate_fight_level().map(|l| (l, p.key.as_str())))
            .filter(|(l, _)| crate::rest::worth_banking_for(*l))
            .max()
            .map(|(l, k)| (k, l))
    }

    /// Is there still a heart on a shelf somewhere we have not written off?
    ///
    /// Mirrors the filter in [`WorldMap::plan`]'s heart branch, and must keep mirroring it: this is
    /// what reserves the gold that branch will want, so a disagreement would hoard for an errand
    /// that can never fire. The standing assumption is the dev's — every village's general store
    /// starts with one — so the interesting state is which no longer do.
    fn a_heart_is_still_for_sale(&self) -> bool {
        self.places.values().any(|p| {
            p.stocks_a_heart()
                && !p.avoid
                && !self.heart_bought.contains(&p.key)
                && !self.abandoned.contains(&p.key)
        })
    }

    /// **Gold that banking may not touch.**
    ///
    /// The dev's ruling, 2026-08-20: *hearts have priority over the Well-Rested stacks.* Both are
    /// preparation bought with the same purse, and left to compete a sixteen-stack bank at ten gold
    /// a stack empties a hundred-gold heart's budget on the way past — which trades four **maximum**
    /// health, permanent for the rest of the run, for a bank that a single fight spends.
    ///
    /// [`HEART_FLOOR`] rather than [`HEART_COST`], because that is the bar the heart errand itself
    /// nominates on: the price *plus a night's bed*, which the dev raised it to on 2026-08-15 after
    /// watching the errand spend a run down to nothing. Reserving less would leave a purse that can
    /// buy the heart and not survive it.
    ///
    /// Zero once no heart is left to buy — hoarding for an errand that cannot fire is just a run
    /// that never banks.
    ///
    /// **Health is not subject to this.** `wants_rest` keeps its own plain [`crate::rest::INN_COST`]
    /// gate in [`WorldMap::wants_a_bed`]: the ruling ranks two kinds of *preparation*, and being
    /// hurt outranks both.
    fn heart_reserve(&self) -> i64 {
        match self.a_heart_is_still_for_sale() {
            true => HEART_FLOOR,
            false => 0,
        }
    }

    /// **How many stacks to actually buy**, after the heart's reserve is taken out of the purse.
    ///
    /// [`WorldMap::stacks_short_ahead`] is how deep the want is; this is how much of it the purse
    /// may serve. Everything that spends on stacks asks *this* one — the planner deciding whether
    /// the trip is worth making, and the inn deciding how many times to press — so a run cannot set
    /// off for stacks it will then refuse to pay for.
    ///
    /// The dev's original floor survives inside it: with no heart left to reserve for, this is
    /// positive exactly when the purse holds [`crate::rest::INN_COST`], which is the inn's own gate
    /// and the point below which the requirement is unmeetable rather than merely unmet.
    pub fn stacks_to_buy(&self) -> i64 {
        let spendable = (self.gold - self.heart_reserve()).max(0);
        self.stacks_short_ahead().min(spendable / crate::rest::INN_COST)
    }

    /// **Is a bed wanted at all**, for either of the two reasons there are?
    ///
    /// [`WorldMap::wants_rest`] is damage already taken; [`WorldMap::stacks_short_ahead`] is damage
    /// to come. The village-crossing rules ask this rather than `wants_rest` alone, or a run sent to
    /// an inn by [`WorldMap::bank_first`] would walk into the village at full health, find nothing
    /// that wanted a bed, and walk out again — which is the exact shape of the campfire stall
    /// recorded at [`crate::rest::CAMPFIRE_REST_IS_BUILT`], and it ends the same way, with the loop
    /// guard.
    ///
    /// The gold gate is left where it already was in each caller: both of them check it, and both
    /// check it against the same [`crate::rest::INN_COST`].
    pub fn wants_a_bed(&self) -> bool {
        self.wants_rest || self.top_up || self.stacks_to_buy() > 0
    }

    fn bank_first(&self, plan: Plan) -> Plan {
        // **Both questions, and they are not the same one.** `stacks_short_for` asks whether the
        // place we are walking to is a fight worth banking for; `stacks_to_buy` asks whether the
        // purse may serve it once the heart's reserve is out. A trip taken on the first alone would
        // arrive at an inn that then declined to press.
        if self.stacks_short_for(&plan.target) == 0 || self.stacks_to_buy() == 0 {
            return plan;
        }
        let here = self.here.as_deref().unwrap_or("");
        let dist = self.distances(here);
        // The route test only. Hostile ground is the rest goal's concern, and this errand is taken
        // at whatever health we happen to have — refusing a bed for being awkward to reach would
        // leave the fight itself as the alternative.
        let ok = |p: &Place| self.can_route_to(&p.key);
        match self.best_rest_site(here, &dist, &ok) {
            Some(bed) if bed.key != plan.target => {
                Plan { target: bed.key.clone(), reason: Goal::StockUp, steered_by: None }
            }
            _ => plan,
        }
    }

    fn next_errand(&self) -> Option<Plan> {
        // While a rest is wanted, EVERY branch skips areas that cost a fight to leave — not just the
        // one heading for the objective. A plan is a plan whatever its reason, and arriving at a
        // shrine or a frontier through an uncleared corrupted village hurts exactly as much as
        // arriving at an anomaly trigger that way.
        //
        // Two passes, so the rule cannot strand the run: if avoiding hostile ground leaves nothing
        // at all to do, the second pass drops the restriction and we get on with it.
        if self.wants_rest {
            if let Some(plan) = self.plan(true, true) {
                return Some(plan);
            }
            if let Some(plan) = self.plan(true, false) {
                return Some(plan);
            }
            // Nowhere safe left on the map. Rather than fall straight through to the objective —
            // which is ordered by *usefulness* and can put a level 8 anomaly first — take the
            // cheapest fight there is.
            if let Some(plan) = self.easiest_hostile() {
                return Some(plan);
            }
        }
        // **Not hurt, so hostility is not a filter — and note which argument is which.**
        //
        // `plan(skip_hostile, need_route)`. Both calls here pass `skip_hostile = false`, so the
        // dev's standing rule already holds where targets are chosen: risk ordering belongs to the
        // rest goal and nothing else. It is counterproductive on the way to the anomaly, because
        // corruption *is* high node levels, so skipping hostile ground points a run away from the
        // one place it is trying to reach.
        //
        // The two passes differ on `need_route`, which is a different question entirely: prefer a
        // target we can already reach, and fall back to one we cannot rather than stranding.
        // Written down because the pair reads like a hostility cascade at a glance, and on
        // 2026-08-15 it fooled me into "fixing" a rule that was not broken.
        self.plan(false, true).or_else(|| self.plan(false, false))
    }

    /// Is there a known route to `key` that takes **no fight at all** on the way?
    ///
    /// The dev's rule, stated many times and finally written down here: *go toward the anomaly
    /// unless there is an accessible shrine that does not require combat*. A shrine is worth the
    /// detour because it is free — it pays in wildcard tiles and costs only walking — and the moment
    /// a fight is on the way it is not free, it is the anomaly's budget spent early.
    ///
    /// **Combat, not crypts.** The first version of this tested `type_is("crypt")`, because the run
    /// that prompted it had died in one. That is the example, not the rule, and the difference is
    /// most of a map: `l22` is a bandit camp, `l24` a level 3 spider forest, `l40` a level 5
    /// graveyard — every one of them a fight, none of them a crypt, and all four sat on the route
    /// this function called free on 2026-08-15. The test is [`heading_has_combat`], which is the
    /// same one the rest of this file uses to know a fight when it sees one.
    ///
    /// **Only an unfinished fight counts**, because `completed` is what says whether one is still
    /// owed — the same reading as everywhere else here.
    ///
    /// **The destination is not exempt**, unlike [`WorldMap::step_avoiding`]'s blocked set. There,
    /// refusing to path to where we were told to go would be failure; here the question *is*
    /// whether going costs a fight, and a shrine whose own node still has to be fought for costs one
    /// — `Swanland — level 6 shrine` is a real heading from this very save.
    ///
    /// Breadth-first over the same edges as [`WorldMap::distances`] and deliberately *not* weighted:
    /// the question is whether such a route exists at all, not which one is cheapest.
    fn reachable_without_a_fight(&self, from: &str, to: &str) -> bool {
        matches!(self.access_without_a_fight(from, to), Access::Free)
    }

    /// Does this place have roads we have never looked down?
    ///
    /// `connections` is the **game's** degree for the node, printed beside it in every dump
    /// (`connections: 4`), while `neighbours` is what we have actually seen named. A gap between them
    /// is not a guess: it is the game telling us there is more here than we have recorded.
    ///
    /// Deliberately not [`Place::is_frontier`], which is the other candidate and would be wrong.
    /// That reads `!visited || hidden > 0`, and `visited` means "stood here **this run**" and is
    /// pointedly not restored from the cache — so on a resumed run every place is a frontier and the
    /// answer degenerates to "always". This one survives a restart because both halves of it do.
    fn has_unexplored_roads(&self, key: &str) -> bool {
        self.places
            .get(key)
            // **And we have not already stood there.** Task #73, the dev 2026-08-21: *one probe per
            // node, remembered.*
            //
            // Standing on a node is the act that makes the game name its neighbours, so a gap that
            // survives a visit is one visiting cannot close — most often a **secret** neighbour,
            // which `verboseAdjacencyData` prints as `Hidden location` and never names until a tower
            // reveals it (`locationIsVisible`, `overworldview.lua:554-556`; task #14 is the reveal).
            // Counting it as unexplored makes the node a permanent magnet: the errand nominates it,
            // `next_target` excludes `here` so the pull vanishes on arrival, exploring hops away, and
            // from over there it is a candidate again.
            //
            // **A ranking was tried first and was not enough.** `probe_toward_the_unknown` sorts on
            // `(visited, dist)`, which decides between candidates — and with a single candidate it
            // decides nothing. One is the ordinary case: the `l11` / `l13` bounce of the 1519Z run
            // went three full laps *with that ranking in the binary* before `LOOP_WRITE_OFF` broke
            // it.
            //
            // This is the stricter reading of the dev's earlier correction, not a retreat from it:
            // *it should be the navigator's responsibility to probe and build the cache, not to
            // assume from the cache.* Probing is exactly what a visit is. Concluding **after** one is
            // the responsibility discharged; concluding before it is the thing they objected to.
            //
            // `visited` is set only by standing somewhere **this run** and is pointedly not restored
            // from the map cache, so a resumed run probes afresh — the right side to err on, since a
            // tower may have revealed the neighbour while we were away.
            .map(|p| !p.visited && p.connections as usize > p.neighbours.len())
            .unwrap_or(false)
    }

    /// [`WorldMap::reachable_without_a_fight`], with the two kinds of "no" kept apart.
    ///
    /// ## The dev's correction, 2026-08-15
    ///
    /// > It should be the navigator's responsibility to probe and build the in-memory cache of which
    /// > nodes are accessible without combat, not to assume from the cache that they are
    /// > inaccessible without combat.
    ///
    /// Right, and the bare `bool` is what made that impossible to honour: it collapsed *I know a
    /// fight is in the way* into the same answer as *I have no edges over there at all*, and the
    /// heart errand read both as "there is no heart to be had for free". Live 2026-08-15 the map held
    /// `l28 Enholmes town` with **four** declared connections and exactly **one** recorded — three
    /// roads never looked down — and the run reported it unreachable as a fact.
    ///
    /// [`Access::Unknown`] is that case: nothing in the fight-free region reaches the target, but the
    /// region touches a node with unexplored roads, so it may not end where our edges do.
    ///
    /// ## What is *not* a source of pessimism, measured
    ///
    /// Recalled headings are not, and this is worth stating because it is the natural suspicion.
    /// `AreaHeading` prints the level only when `locationHasCombat`, and that is
    ///
    /// ```lua
    /// function core.locationHasCombat(location)
    ///     if core.areaIsComplete(location.key) then return false end
    ///     return not core.locationIsCompleteOnVisit(location)
    /// end
    /// ```
    ///
    /// (`overworldview.lua:305-310`). `locationIsCompleteOnVisit` is a property of the location's
    /// *type* and does not change; `areaIsComplete` is a save flag we re-read every step and already
    /// AND into [`Place::may_be_a_fight`]. So a recalled `— level N` can only stop being true through
    /// the one input we already take live. Corruption moves it the other way and arrives in the save
    /// too. Of the four surface nodes the 2026-08-15 run named live, four agreed with the cache.
    ///
    /// The edges are the whole of the gap, which is why this counts roads and not headings.
    fn access_without_a_fight(&self, from: &str, to: &str) -> Access {
        let costly = |k: &str| {
            self.places.get(k).map(|p| !p.completed && p.may_be_a_fight()).unwrap_or(false)
        };
        if costly(to) {
            return Access::Blocked;
        }
        let mut seen: BTreeSet<&str> = [from].into_iter().collect();
        let mut queue: std::collections::VecDeque<&str> = [from].into();
        while let Some(k) = queue.pop_front() {
            if k == to {
                return Access::Free;
            }
            let Some(p) = self.places.get(k) else { continue };
            for n in &p.neighbours {
                if seen.contains(n.as_str()) || costly(n) {
                    continue;
                }
                seen.insert(n);
                queue.push_back(n);
            }
        }
        // The target is out of reach over the edges we hold. Whether that is a fact about the map or
        // a fact about our ignorance depends on whether the region we just walked has any way out we
        // have not tried.
        match seen.iter().any(|k| self.has_unexplored_roads(k)) {
            true => Access::Unknown,
            false => Access::Blocked,
        }
    }

    /// The nearest place inside the fight-free region around `from` that still has roads we have not
    /// looked down — somewhere to go and *find out*, rather than a target in itself.
    ///
    /// Walks the same region [`WorldMap::access_without_a_fight`] does, so a probe it returns is
    /// reachable on the same terms as the errand that wanted one: no fight to get there.
    fn probe_toward_the_unknown(
        &self,
        from: &str,
        dist: &BTreeMap<String, usize>,
    ) -> Option<&Place> {
        let costly = |k: &str| {
            self.places.get(k).map(|p| !p.completed && p.may_be_a_fight()).unwrap_or(false)
        };
        let mut seen: BTreeSet<&str> = [from].into_iter().collect();
        let mut queue: std::collections::VecDeque<&str> = [from].into();
        while let Some(k) = queue.pop_front() {
            let Some(p) = self.places.get(k) else { continue };
            for n in &p.neighbours {
                if seen.contains(n.as_str()) || costly(n) {
                    continue;
                }
                seen.insert(n);
                queue.push_back(n);
            }
        }
        seen.into_iter()
            .filter(|k| *k != from && self.has_unexplored_roads(k))
            .filter(|k| !self.abandoned.contains(*k))
            .filter_map(|k| self.places.get(k))
            // **Somewhere we have not stood first, and only then somewhere we have.**
            //
            // The dev, 2026-08-21: *shouldn't the heart errand keep searching the frontier?* Yes,
            // and re-nominating a node it has already stood on is how it stopped.
            //
            // `has_unexplored_roads` compares the game's declared `connections` against the
            // neighbours we have seen named, and being *at* a node is what makes the game name them.
            // So a node we have stood on whose gap is still open has a gap that visiting cannot
            // close — most often a **secret** neighbour, which `verboseAdjacencyData` prints as
            // `Hidden location` and never names until a tower reveals it (`locationIsVisible`,
            // `overworldview.lua:554-556`). Probing it again buys nothing and the errand at the far
            // end nominates the way back. Live 2026-08-21, `l11 Argham crossroads` against `l5
            // Dalton Copse`, two full laps before the write-off broke it:
            //
            // ```text
            //   6. l11 -> **l5**  (for l39, Explore)
            //   7. l5  -> **l11** (for l11, Heart)
            //   8. l11 -> **l5**  (for l39, Explore)
            //   9. l5  -> **l11** (for l11, Heart)
            //  10. `l11` is written off — stood on 2 times with nothing learned
            // ```
            //
            // **A preference and not a filter**, which is the whole of the dev's earlier correction:
            // *it should be the navigator's responsibility to probe and build the cache, not to
            // assume from the cache.* Excluding visited nodes outright was tried and it broke
            // `a_heart_behind_the_fog_is_probed_rather_than_written_off`, where the one candidate has
            // been stood on and going anyway is right — there is nothing else to try, and a probe
            // that refuses the only road left has stopped searching. Ranked, that fixture is
            // unchanged and the bounce still ends: with two candidates the unvisited one wins.
            //
            // `visited` is set only by standing somewhere **this run**, so a resumed run probes
            // afresh — the right side to err on, since the gap may have closed while we were away.
            .min_by_key(|p| (p.visited, dist_or_far(dist, &p.key)))
    }

    /// Could [`WorldMap::next_hop`] actually set off toward `key`, over the edges we have recorded?
    ///
    /// The question `plan` failed to ask, and the run of 2026-08-09 is what that cost. Every travel
    /// goal it printed read `(for start, Anomaly)` — five of five — while the map held `start` with
    /// no heading and no edges at all, learned from `start_first_corrupt_time` in the save and never
    /// seen. The anomaly branch outranks the shrine branch and does not yield, so `shrine6 Borsea
    /// shrine` — heading known, unused, unconsecrated, uncorrupted, and reachable — was never
    /// considered. `next_hop` found no route, fell through to its any-frontier fallback, and the run
    /// wandered onto a level 6 crypt and died there while the log claimed it was going somewhere.
    ///
    /// So three costs, and only the first is cosmetic: the log lies, a reachable objective is
    /// skipped, and the fallback picks fights nobody chose.
    ///
    /// ## Unreachable and unmeasured look the same, and this does not try to tell them apart
    ///
    /// A node we have never stood beside has no edges, so "no route to it" is also what "we have not
    /// mapped the way yet" looks like — [`Arrival::Unknown`] exists because conflating two such
    /// states put a run at 1/20 in front of a crypt. This deliberately does not distinguish them.
    /// It reports what `next_hop` can do *now*, and the caller uses it as a preference with a second
    /// pass behind it, so a target we merely have not mapped is demoted rather than discarded.
    ///
    /// Asked with `shun` false, which is `next_hop`'s second attempt: a route that has to cross a
    /// lost woods we escaped is a poor route and still a route. The rest of the exclusions —
    /// abandoned nodes, chests while hurt — are shared with [`WorldMap::step_avoiding`] by calling
    /// it, rather than restated here where the two could drift apart.
    ///
    /// Standing on the target counts. `first_step_toward` answers `None` for `from == to` because
    /// there is no step to take, which is not the same as being unable to get there, and treating it
    /// as unreachable would make the planner walk away from somewhere it had just arrived.
    pub(super) fn can_route_to(&self, key: &str) -> bool {
        let Some(here) = self.here.as_deref() else { return true };
        key == here || self.first_step_toward(here, key, false).is_some()
    }

    /// Does going here cost a fight, on **either** axis?
    ///
    /// The exact complement of `plan`'s `ok`, so that the two passes of [`WorldMap::next_target`]
    /// tile the map with no gap and no overlap: everywhere `plan(true)` refuses,
    /// [`WorldMap::easiest_hostile`] will consider, and nowhere is refused by both.
    /// `the_two_passes_between_them_consider_every_place` pins that.
    fn owes_a_fight(p: &Place) -> bool {
        p.hostile_to_enter() || !p.arrival().is_free()
    }

    /// The cheapest fight on the map, for when being hurt has run out of safe options.
    ///
    /// Two reasons this beats going at the objective while hurt, and the second is the better one:
    ///
    /// 1. It is the fight least likely to end the run.
    /// 2. **Winning it may be what unlocks a rest.** A corrupted village's inn is locked behind the
    ///    village being cleared (`overworld/generators/village.lua:371-395`), so clearing the
    ///    cheapest one is a route to the rest we could not otherwise reach — not merely the least
    ///    bad way to keep moving.
    ///
    /// **The anomaly is excluded, and an unknown level sorts LAST.** Both because of the same trap:
    /// the anomaly's heading is `The Rift anomaly`, with no `— level N` in it, so [`Place::level`]
    /// returns `None` for the hardest fight on the map. Treating an absent level as zero ranked it
    /// as the gentlest option available — the exact opposite of this function's purpose, and it
    /// would have sent a run at a third health straight into the level 8 fight it was trying to
    /// avoid.
    ///
    /// So: unknown means unknown, and goes to the back. And the anomaly is not a stepping stone
    /// towards a rest under any reading — it is the objective, and reaching it is what the whole
    /// first pass exists to defer.
    ///
    /// ## [`Risk`] first, then level — **deferred as post-MVP, not settled**
    ///
    /// This used to sort on level alone, which cannot express the dev's ranking at all: a level 5
    /// forest and a level 5 crypt are not the same errand, and the forest is much the better one.
    /// See [`Risk`] for what separates them.
    ///
    /// Putting the class ahead of the level is a choice with a cost, and it is worth being plain
    /// about it: **a level 7 forest outranks a level 1 crypt.** The argument for it is that the
    /// forest's fights are individually declinable — a route across peaceful subnodes may cost
    /// nothing at all — while the crypt's one fight is compulsory, so the forest's *expected* cost
    /// can be zero where the crypt's never is. The argument against is that when it does bite, it
    /// bites at level 7, and at 1/20 health any level is fatal.
    ///
    /// Neither argument wins on the evidence available, and settling it wants live data on what a
    /// forest crossing actually costs — which no run has yet produced. The ordering follows the
    /// dev's read of the game until then, deliberately and with this written down. Swapping the two
    /// `.then` clauses is the whole change.
    ///
    /// ## Not route-gated, unlike [`WorldMap::plan`], and that is a decision
    ///
    /// #24 made every branch of the planner refuse a target nothing can reach. This one is exempt,
    /// because here the two orderings pull against each other: preferring what we can route to means
    /// taking a reachable level 6 crypt over an unreachable level 1 forest, and this function exists
    /// precisely to *not* do that — it is reached only while hurt, with nowhere safe left, which is
    /// the state where picking the wrong fight ends the run.
    ///
    /// So an unreachable cheapest fight is still named, and `next_hop` steps its way. Nothing has
    /// measured which of the two costs more, and the safe direction while hurt is the gentle fight.
    fn easiest_hostile(&self) -> Option<Plan> {
        let here = self.here.as_deref().unwrap_or("");
        let mut candidates: Vec<&Place> = self
            .places
            .values()
            .filter(|p| p.key != here && !p.avoid && Self::owes_a_fight(p))
            .filter(|p| p.key != ANOMALY_KEY && !p.type_is("anomaly"))
            .collect();
        candidates.sort_by(|a, b| {
            a.risk()
                .cmp(&b.risk())
                .then(
                    a.remembered_level()
                        .unwrap_or(u32::MAX)
                        .cmp(&b.remembered_level().unwrap_or(u32::MAX)),
                )
                .then(a.key.cmp(&b.key))
        });
        candidates
            .first()
            .map(|p| Plan { target: p.key.clone(), reason: Goal::EasiestHostile { level: p.remembered_level() }, steered_by: None })
    }

    /// The planner proper. `skip_hostile` excludes anywhere a fight is owed, on either axis;
    /// `need_route` excludes anywhere [`WorldMap::next_hop`] could not actually set off toward.
    ///
    /// **Both axes, and that is the fix.** It used to consult [`Place::hostile_to_enter`] alone,
    /// which asks whether a subworld will hold us in — so a crypt, which is not a subworld and
    /// cannot trap anybody, sailed through the hurt pass and a run at 1/20 was routed onto a level 6
    /// one under [`Goal::Explore`]. Meanwhile `shrine5` sat on the same map printing
    /// `Gembling shrine` with no level at all, free to walk onto.
    ///
    /// ## Why `need_route` is a pass over the whole ladder and not a patch on one branch
    ///
    /// A branch that cannot make progress used to outrank every branch that could. The anomaly is
    /// the case that showed it — see [`WorldMap::can_route_to`] for the live run — but nothing about
    /// it is specific to the anomaly: a shrine in a component we have not linked up yet, or a
    /// frontier node known only by key from `completedAreas`, suppress the branches below them in
    /// exactly the same way. So the question is asked of every candidate, in `ok`, alongside the
    /// hostility one.
    ///
    /// It is a *preference*, never a filter on the map: [`WorldMap::next_target`] runs this twice
    /// and the second pass drops the requirement. Nothing routable is a real state — a fresh launch
    /// standing nowhere is one — and a run that refuses to name a target it cannot yet reach has
    /// stopped, which is worse than aiming at one and stepping the way it lies.
    ///
    /// ## And it is asked on the SURFACE ONLY
    ///
    /// Inside a subworld our graph is **known** to be disconnected: nothing links an interior node
    /// to its container, so `distances` from inside reaches no surface node at all — the limitation
    /// [`WorldMap::exit_toward`] is written around. Every surface target therefore fails a route
    /// test from in there, for a reason that is about our model and not about the world, and gating
    /// on it would hand the run to whichever interior frontier happened to be walkable.
    ///
    /// Which is not hypothetical: it turned "I am at 1 of 20 health and there is a campfire out the
    /// west door" into `Goal::Explore`, and the door commitment follows the errand. The route test
    /// is a statement about what [`WorldMap::next_hop`] can do, and while we are inside, `next_hop`
    /// is not what moves us — [`WorldMap::cross_toward`] is, and it reaches the surface through
    /// exits rather than edges.
    /// **The best place to sleep**, by site then distance then key — or `None` if none can serve.
    ///
    /// Extracted so the two errands that want a bed cannot drift apart. [`Goal::Rest`] asks because
    /// health is down; [`Goal::StockUp`] asks because the Well-Rested bank is short of what the next
    /// deliberate deep fight wants. They differ in *why* and not at all in *where*, and this file
    /// has been bitten twice by two predicates that were meant to agree and quietly stopped —
    /// see [`Place::is_shrine`] and the `l28 <-> l27` bounce.
    ///
    /// `ok` is the caller's own admissibility test, so the hostile-ground and route rules stay where
    /// they are decided rather than being re-derived here.
    fn best_rest_site<'a>(
        &'a self, here: &str, dist: &BTreeMap<String, usize>, ok: &dyn Fn(&Place) -> bool,
    ) -> Option<&'a Place> {
        let mut sites: Vec<(&Place, crate::rest::Site)> = self
            .places
            .values()
            // A corrupted rest stop is locked, not lost. Corruption swaps a location's type for
            // its `corruptType` and puts the place under attack, so the inn sits behind
            // `underAttackAreaButtons` or `destroyedAreaButtons`
            // (`overworld/generators/village.lua:371-395`) — while the heading still ends in
            // "village", which is why nothing else here notices. Met live as
            // `Ulrome — level 6 village [corrupted]`.
            //
            // **Verified, and it is a race.** The inn has three button sets
            // (`overworld/generators/village.lua:360-393`) and only one of them can serve:
            //
            // ```text
            //   areaButtons            Enter -> ui.inn             the innkeeper, rest works
            //   underAttackAreaButtons Enter -> ui.building_empty  an empty shell, no rest
            //   destroyedAreaButtons   a loot button only          the inn is gone for good
            // ```
            //
            // So corruption does not merely gate the inn, it starts a clock: clear the village
            // in time and the real inn comes back, leave it and the inn is destroyed
            // permanently. Confirmed by the dev, 2026-08-12, and it replaces the guess that used
            // to sit here — which had the release right and knew nothing about the deadline.
            //
            // `completed` is that release and the filter is correct for it. **The gap is the
            // third state**: nothing here distinguishes a village still under attack from one
            // whose inn is already rubble, because both are `corrupted` and neither is
            // `completed`. Today that costs nothing — both are excluded — but a run that clears
            // a long-corrupted village expecting a bed will find a loot pile, and `completed`
            // will say it may sleep there. See task #26, which wants the loot anyway.
            .filter(|p| p.key != here && !p.avoid && (!p.corrupted || p.completed) && ok(p))
            // A settlement under attack or lost has no bed — see [`Place::trades`]. Campfires
            // are unaffected by it and excluded elsewhere entirely
            // (`rest::CAMPFIRE_REST_IS_BUILT`).
            .filter(|p| !p.has_an_inn() || p.trades())
            .filter_map(|p| crate::rest::site(&p.heading).map(|s| (p, s)))
            .filter(|(p, s)| crate::rest::can_rest_at(*s, self.gold, self.fuel, !p.used))
            .collect();
        // **Site, then distance, then key.** The middle term is new, and its absence is the
        // whole of the dev's question of 2026-08-15: *"after we completed the level 7 crypt, why
        // did the navigator choose such a distant village to rest at instead of the ones we
        // bought healthBuffs from?"*
        //
        // Because the tie-break was `pa.key.cmp(&pb.key)` and nothing else. Two inns of equal
        // rank were separated **alphabetically**, so the run walked four hops to `l11` while
        // `l19` — rested at earlier in that same run, and two hops away through `l9` — lost the
        // comparison to a string. `spike-run-20260815-1913Z.md` steps 49-53 are that walk. It is
        // not a near miss either: `l100` sorts before `l11`, which sorts before `l2`.
        //
        // Distance is in hops rather than anything cleverer, because hops are what we have —
        // see #21. Unreachable sites sort last rather than being dropped: `next_target` runs the
        // whole ladder a second time with the route requirement lifted, and a bed we cannot yet
        // plot a course to is still better than no bed at all.
        //
        // The key stays as the final tie-break so the choice is deterministic, which several
        // tests depend on. **It is no longer ordering anything that matters.**
        let far = |p: &Place| dist.get(&p.key).copied().unwrap_or(usize::MAX);
        sites.sort_by(|(pa, sa), (pb, sb)| {
            sb.rank()
                .cmp(&sa.rank())
                .then(far(pa).cmp(&far(pb)))
                .then(pa.key.cmp(&pb.key))
        });
        sites.first().map(|(p, _)| *p)
    }

    fn plan(&self, skip_hostile: bool, need_route: bool) -> Option<Plan> {
        let here = self.here.as_deref().unwrap_or("");
        let need_route = need_route && self.inside().is_none();
        // Both halves are deliberately last in every filter chain below: `can_route_to` runs a
        // search, and the cheap tests in front of it decide most candidates for free.
        let ok = |p: &Place| {
            !(skip_hostile && Self::owes_a_fight(p)) && (!need_route || self.can_route_to(&p.key))
        };

        // **Health first, ahead of everything including a live anomaly.**
        //
        // This used to sit below the anomaly branch, on the reading that nothing shortens the run
        // like going straight at the objective. That is true and it is also how a run dies: the
        // anomaly is level 8, and arriving at it on a third health converts "the objective" into
        // "the end of the save". The anomaly does not expire, and health does not recover by
        // itself — so the detour is only ever a delay, while skipping it can be terminal.
        //
        // Ranked by site — a campfire costs nothing and needs no subworld — then by whether we can
        // actually rest there, which for an inn means carrying the ten gold it charges.
        //
        // Falls through when nothing qualifies, which is the common case on a corrupted map: every
        // inn there is locked behind its village being cleared. So this is a preference, not a
        // guarantee, and a run with no reachable rest still gets on with the objective rather than
        // standing still.
        //
        // Health before exploration: walking into the next fight hurt is how a run ends. Ranked by
        // site — a campfire costs nothing and needs no subworld — then by whether we can actually
        // rest there, which for an inn means carrying the ten gold it charges.
        //
        // Hoisted above the rest branch so both can use it. It used to be computed below, which is
        // why the bed was the one thing on this ladder chosen with no idea how far away it was.
        let dist = self.distances(here);
        if self.wants_rest {
            if let Some(p) = self.best_rest_site(here, &dist, &ok) {
                return Some(Plan { target: p.key.clone(), reason: Goal::Rest, steered_by: None });
            }
        }


        let anomaly_open = self.anomaly_is_open().unwrap_or(false);
        // A shrine still worth walking to. `Pray` retires on `areaUnused`; `Consecrate` needs
        // `hell ~= 0`, so an open portal makes an unconsecrated shrine live again whatever its
        // `_used` flag says. See the fuller account at the closed-portal branch below.
        //
        // **`corrupted` is a bill, not a verdict, and `completed` says whether it is still owed.**
        // The filter reads `corrupted && !completed` for the same reason
        // [`WorldMap::worth_consecrating_here`] does, and the two must agree: a shrine whose fight we
        // have already won costs nothing more to consecrate than an uncorrupted one. Live
        // 2026-08-14, they did not agree. `shrine1` sat `[done] [corrupted]` and unconsecrated for a
        // whole run — the arrival test would have consecrated it on sight, while this filter dropped
        // it before `ok` could even ask for a route, so the run never went. Dropping only the
        // `completed` clause here is what let the pair drift apart.
        //
        // **And the pair drifted again on 2026-08-17, the other way round.**
        // [`Place::can_be_consecrated`] was added to the arrival test and not to this one. The
        // consecration clause below is `!consecrated`, which for a **minor** shrine is true and stays
        // true for ever: `Consecrate` requires `majorShrine` (`shrine.lua:93-96`), so a woodland
        // shrine can never take the flag that would retire it. A prayed-at woodland shrine would
        // therefore have become a permanent `Goal::Shrine` the moment the portal opened — the same
        // shape as the `shrine1 -> l10 -> shrine1` bounce recorded at the closed-portal branch, but
        // with nothing that could ever end it, because arrival would decline the trip this filter
        // kept proposing.
        //
        // So the consecration half is gated on the consecration being *possible*. The prayer half is
        // not, and must not be: `showPrayButton` leads with `not shrineLocation.majorShrine`
        // (`shrine.lua:98-101`), so an unprayed minor shrine is a real errand whatever the portal is
        // doing.
        let worth_a_trip =
            |p: &Place| !p.used || (anomaly_open && p.can_be_consecrated() && !p.consecrated);
        // Hoisted: it walks every place, and asking it once per candidate would make the shrine
        // branch quadratic in the size of the map for an answer that cannot change inside one pass.
        let corrupted_are_needed = anomaly_open && self.corrupted_shrines_are_needed();
        let pick_shrine = || {
            self.places
                .values()
                .filter(|p| p.key != here && !p.avoid && p.is_shrine())
                // **A surface destination, so nothing with a parent.** [`Place::is_shrine`] answers
                // by key *or* heading, and only the key arm excludes subnodes — its own doc says
                // matching them "would make the planner target a node whose parent is where it
                // actually wants to go". The heading arm re-admits exactly what the key arm keeps
                // out, as soon as a dump names one: `Gripthorpe Brush woodland shrine` ends in
                // `shrine`, so `shrine1sub1` and `shrine1_plaza` are both candidates here.
                //
                // Mostly hidden today, and hidden is not fixed. Our graph has no edge across a
                // subworld boundary (see [`WorldMap::plan`]'s note), so `dist` scores an interior
                // node `usize::MAX` and `ok`'s route test drops it — but `plan` runs a **second pass
                // with `need_route` off**, and there the route test is not there to catch it. What is
                // left is a target `next_hop` cannot step toward.
                //
                // The right entry to an interior shrine is its container, and once inside,
                // [`WorldMap::shrine_inside`] and [`WorldMap::worth_consecrating_here`] own it. This
                // says that in the filter instead of relying on a distance that happens to sort last.
                .filter(|p| p.parent.is_none())
                .filter(|p| !self.abandoned.contains(&p.key))
                .filter(|p| worth_a_trip(p))
                // **With the portal shut, a shrine that would open it is not a free errand.**
                //
                // The dev, 2026-08-17, watching a run at 52/52 walk into `shrine7`, *Cottam Boscage
                // — level 9 forest*: *why did we even enter the lv9 forest instead of immediately
                // backtracking? That was nearly suicidal.*
                //
                // Both filters below are written `!anomaly_open || …`, so **before the portal opens
                // they are no-ops** and nothing at all priced a shrine's cost. `worth_a_trip`
                // reduces to `!used`, which a never-prayed level 9 forest satisfies as happily as a
                // level 1 one, and `Goal::Shrine` walked us into the fight.
                //
                // Arriving is also what *opened* the anomaly, which is the second half of the harm:
                // `world_evil.lua:15-18` fires on `locationHasCombat and level > 3`, so the errand
                // did not merely pick an expensive fight, it ended the phase it belonged to.
                //
                // ## `triggers_anomaly()` alone, and the missing `&& !completed` is the point
                //
                // This first read `triggers_anomaly() && !completed`, which is the predicate removed
                // from [`Place`] as guarding an unreachable state — and I cited that note as having
                // been about the whole rule. It is not. The dev, 2026-08-17:
                //
                // > There was apparently code to check whether a lv4+ node was considered "gentle"
                // > as internally, that is the case when it's already been cleared. However, that
                // > specific situation cannot happen because no nodes with levels are considered
                // > cleared to begin with.
                //
                // The unreachable state is **level 4+, cleared, portal still shut**. Clearing needs
                // arriving and arriving opens the portal, so while `hell == 0` a level 4+ node is
                // uncompleted without exception. `!completed` is therefore *always true* here, and
                // writing it says the opposite — that a cleared one is a case worth admitting.
                //
                // So the rule is the trigger and nothing else. That also leaves nothing for the note
                // on [`Place`] to disagree with: it retired a redundant clause, and this adds a
                // filter at a caller that never had one.
                .filter(|p| anomaly_open || !p.triggers_anomaly())
                // **With the portal open, a shrine has to be cheap or it is not a destination.**
                //
                // The dev's rule, 2026-08-15: target only unconsecrated, uncorrupted shrines that
                // can be reached without fighting a crypt on the way. A corrupted shrine is not a
                // target at all — consecrating one is something we do *if we happen to be there*,
                // having gone through it for another reason, and never a reason to make the trip.
                //
                // What this replaces was a filter that admitted any corrupted shrine whose fight was
                // already won, and ranked purely on distance. It sent the run of 2026-08-15 from
                // `shrine5` through `l50`, `l41`, `l51` and `l35` into `l49` — Yokefleet, a level 6
                // crypt — chasing another shrine, with the anomaly untouched. Every hop of it read
                // `RouteTo(Shrine)`.
                //
                // The cost is bounded and the benefit is not: a shrine bought with a crypt fight is
                // no longer cheap preparation, it is the level 8 fight's budget spent early.
                //
                // **Unless it is the only way left to reach the bar** — task #74, and the exception
                // the gate makes necessary. `SHRINES_BEFORE_THE_ANOMALY` requires consecrations the
                // world may not be able to supply cleanly, and a rule that refuses corrupted shrines
                // outright would leave such a world exploring an exhausted frontier until the
                // release fires. See [`WorldMap::corrupted_shrines_are_needed`], which admits them
                // only once the clean supply is spent *and* the bar is still short — so the free
                // ones are always taken first, and none of this reopens once the bar is met.
                .filter(|p| !anomaly_open || !p.consecrated)
                //
                // **And it must be able to advance the bar**, or the exception pays a fight for
                // nothing. `is_shrine` answers by heading as well as by key, so a minor shrine is a
                // candidate here — and a minor shrine can never be consecrated
                // ([`Place::can_be_consecrated`]), so no number of them moves `consecrations`. A
                // corrupted one is a fight whose reward the gate cannot spend.
                .filter(|p| {
                    !anomaly_open
                        || !p.corrupted
                        || (corrupted_are_needed && p.can_be_consecrated())
                })
                // **The route's cost is NOT a filter, and that is task #70.**
                //
                // There was a `reachable_without_a_fight` here, and it is gone. The dev, 2026-08-21:
                // *shrines should never be blocked by level unless the anomaly has not yet opened.*
                //
                // It cannot stay, because it does not price anything — `may_be_a_fight` is
                // `heading_has_combat || corrupted`, a **boolean**, so a level 1 forest disqualified
                // a route exactly as hard as a level 9 crypt. The heading is where the level comes
                // from at all (`AreaHeading` prints `— level N` only when `locationHasCombat`), so
                // the number was parsed and then thrown away.
                //
                // Live, 1519Z, and measured rather than recalled — see
                // [`tests::the_nearest_shrine_of_the_1519z_world_is_one_the_old_filter_refused`],
                // which replays that run's own map cache. With the portal open from step 0 and
                // seven shrines already known, the run's **first** shrine goal is step 68 of 178,
                // and both shrines it took were ones it happened to be standing beside. `shrine5`
                // — *Harswell Coppice, level 7 forest*, twelve hops out against the forty-two to
                // the pair it took — was never a target at all, because this filter refused it.
                // Put the filter back and the planner picks the shrine at forty-two: thirty extra
                // hops, at the state the run was actually in.
                //
                // The log carries **zero** `RouteTo(Shrine)` lines in those 178 steps, which is
                // what pins it on this filter rather than on `ok`'s route test: a candidate that
                // survived to `ok` and failed there would have said so.
                //
                // **What replaced it is the gate, not nothing.** The 2026-08-15 rule this reverses
                // was written when a shrine was optional reward, and paying a crypt for one spent the
                // level 8 fight's budget early. [`SHRINES_BEFORE_THE_ANOMALY`] makes four of them a
                // precondition — see the `CloseAnomaly` branch, which already argues that *the
                // shrines are not a reward to collect on the way, they are the preparation the fight
                // needs*. Preparation we are required to buy cannot also be refused for costing
                // something.
                //
                // The pre-anomaly filter above is untouched and is a different rule: `triggers_anomaly`
                // refuses a level 4+ shrine while `hell == 0` because arriving there is what *opens*
                // the portal, which is not a question about cost at all.
                .filter(|p| ok(p))
                .min_by_key(|p| dist_or_far(&dist, &p.key))
        };

        // **With the portal live, a reachable shrine outranks the portal itself** — the dev's call,
        // 2026-08-10, and the argument is already written under [`Goal::Shrine`]. Consecrating is
        // *only* possible while `hell ~= 0` (`shrine.lua:93-96`), so the window is exactly now; it
        // costs no fight at an uncorrupted shrine, nor at a corrupted one whose fight is already
        // won, which is exactly the pair the filter above admits; and it pays in gold- and
        // silver-bordered wildcard tiles
        // (`utils/blessings.lua:95-110`), the mechanic this solver handles best.
        //
        // So the ordering is not "reward before objective", it is **cheap preparation before a level
        // 8 fight, during the only window in which it can be bought**. `ok` still demands a known
        // route, so this can only ever choose a shrine we can actually walk to.
        if anomaly_open {
            if let Some(p) = pick_shrine() {
                return Some(Plan { target: p.key.clone(), reason: Goal::Shrine, steered_by: None });
            }
        }

        // **A chest, if one is standing about unopened.** Task #16, and the dev's rule verbatim:
        // *when the goal is not rest and a chest is visible, detour to it, open it, then rejoin the
        // path.*
        //
        // ## This branch has never fired and cannot, in v52.4. The rule lives in `chest_inside`
        //
        // `next_target` plans over **surface** nodes, and there are no chests on the surface:
        // `typeName = 'chest'` occurs in exactly two files, `overworld/generators/forest.lua:178`
        // and `bandit_camp_forest.lua:201`, and both build subnodes off `parentNode.subnodeCount`.
        // Every chest in the game is inside a forest.
        //
        // It is kept rather than deleted because it is correct, cheap, and would be wanted the day a
        // generator places one outdoors — and deleted-and-rewritten is how this rule got written
        // twice already. But **do not read its existence as the chest detour working**: that is
        // [`WorldMap::chest_inside`], reached through [`WorldMap::errand_inside`] while crossing.
        // The tests under this branch use surface fixtures and prove only that the branch is wired.
        //
        // `wants_rest` is the whole of "the goal is not rest", and it is the right gate rather than a
        // cautious one: `Open` starts a combat scenario (`overworld/generators/forest.lua:30-39`),
        // so a chest is a fight, and a fight is the last thing a hurt run should detour for. The
        // rest branch above has already claimed those runs anyway.
        //
        // `ok` still demands a route, and `abandon` still writes one off, so this cannot nominate a
        // chest we could not reach or have already given up on. Nearest first — a chest is worth the
        // walk, not any length of walk.
        if !self.wants_rest {
            let chest = self
                .places
                .values()
                .filter(|p| p.key != here && p.is_chest() && !p.completed && !p.avoid)
                .filter(|p| !self.abandoned.contains(&p.key) && ok(p))
                .min_by_key(|p| (dist_or_far(&dist, &p.key), p.key.clone()));
            if let Some(p) = chest {
                return Some(Plan { target: p.key.clone(), reason: Goal::Chest, steered_by: None });
            }
        }

        // **A heart, if one can be had for nothing but walking.**
        //
        // The dev's rule, 2026-08-15, given the evening a run finally died going the right way:
        // two level 6 corrupted crypts back to back, and the second ran the board dry against a
        // five-deep queue. Four maximum health for a hundred gold is the cheapest preparation
        // available, and the anomaly is a level 8 fight.
        //
        // **Outside the `anomaly_open` block, unlike the shrine detour above it** — the dev,
        // 2026-08-16: *we should want the healthBuff regardless of anomaly state.* The shrine really
        // is windowed, because consecrating needs `hell ~= 0` (`shrine.lua:93-96`) and cannot be
        // bought early at any price. A heart has no such window, and the fights that *open* the
        // anomaly are the level 6 ones that killed the run this rule came from. See
        // [`WorldMap::wants_a_heart`] for how the misreading survived: the shut state had never been
        // played until an adventure started from a cleared profile.
        //
        // Above the anomaly and above `OpenAnomaly`, so the preparation happens before the fight it
        // is preparation for, in both states.
        //
        // Every remaining clause is the dev's: the gold must be *over* the price, the village must
        // be reachable **without combat** (the same test the shrine detour uses, for the same reason
        // — a detour paid for with a fight is not a detour, it is the fight), and a village whose
        // store we have already emptied is not a destination. `has_heart` is the standing assumption
        // the dev set: every village's general store starts with one.
        if self.wants_a_heart() {
            let shops: Vec<&Place> = self
                .places
                .values()
                .filter(|p| p.key != here && !p.avoid && p.stocks_a_heart())
                .filter(|p| !self.heart_bought.contains(&p.key) && !self.abandoned.contains(&p.key))
                .collect();
            let heart = shops
                .iter()
                .filter(|p| self.reachable_without_a_fight(here, &p.key))
                .min_by_key(|p| dist_or_far(&dist, &p.key));
            if let Some(p) = heart {
                return Some(Plan {
                    target: p.key.clone(),
                    reason: Goal::Heart,
                    steered_by: None,
                });
            }
            // **"I have not looked" is not "there is nothing there", and the difference is a
            // probe.** The dev's correction, 2026-08-15: the navigator's job is to go and find
            // out which nodes are reachable without combat, not to conclude from an incomplete
            // cache that none are.
            //
            // So a shop we cannot route to for free, whose [`Access`] is `Unknown` rather than
            // `Blocked`, sends us to the nearest place with roads we have never looked down —
            // still inside the fight-free region, so the probe costs walking and nothing else.
            // Whatever it reveals lands in the map, and the next pass either routes to the shop
            // or downgrades it to `Blocked` on evidence.
            //
            // Still `Goal::Heart`, because that is what the walk is *for*, and a log that said
            // `Explore` here would hide the errand that chose it.
            if shops
                .iter()
                .any(|p| self.access_without_a_fight(here, &p.key) == Access::Unknown)
            {
                if let Some(p) = self.probe_toward_the_unknown(here, &dist) {
                    return Some(Plan {
                        target: p.key.clone(),
                        reason: Goal::Heart,
                        steered_by: None,
                    });
                }
            }
        }

        // The anomaly is hostile by construction, so `ok` skips it too on the first pass. That is
        // the point rather than an oversight: it is a level 8 fight, and walking into it below half
        // health is the single most expensive thing this run can do. On the first pass we would
        // rather go exploring — which is also how an unknown rest site gets found — and the second
        // pass takes it when there is genuinely nothing else.
        // **And [`SHRINES_BEFORE_THE_ANOMALY`] consecrations before it**, the dev's rule of 2026-08-20 and the
        // reason this branch can now decline.
        //
        // A consecration is bought with walking and pays in gold- and silver-bordered wildcards
        // (`utils/blessings.lua:95-110`); the anomaly is a level 8 fight that the run of that
        // morning reached and lost at turn 24. So the shrines are not a reward to collect on the
        // way, they are the preparation the fight needs, and taking the portal early spends a
        // character that was never made ready.
        //
        // Declining here drops through to `OpenAnomaly` and then to exploring — *which is exactly
        // what the dev asked for*: "we need to keep exploring if the anomaly opens before we've
        // revealed 4 shrines." The shrine branch above this one has already had its chance and is
        // reached first, so a shrine we can reach is taken in preference to more exploring.
        //
        // **The release is at the foot of this function, not here.** A gate with no release is a
        // stall: a world that places too few shrines, or puts them behind ground we cannot
        // cross, would leave the run walking a frontier it has already exhausted for ever. See
        // there for why it is the last thing tried rather than a clause in this condition.
        if self.consecrations() >= SHRINES_BEFORE_THE_ANOMALY {
            if let Some(p) = self.anomaly().filter(|p| ok(p)) {
                return Some(Plan { target: p.key.clone(), reason: Goal::CloseAnomaly, steered_by: None });
            }
        }



        // Opening the anomaly comes BEFORE shrines, and not only because it is the objective:
        // **consecration is impossible until it is open.** `showConsecrateButton`
        // (`shrine.lua:93-96`) needs `hell ~= 0`, so a run that saved its shrines for first would
        // arrive at every one of them unable to finish it.
        //
        // Only worth aiming at while the trigger is unspent — `anomaly_is_open` is `None` before any
        // save has been read, and an unknown flag is read as not open, since a wasted trip is
        // cheaper than stranding the run.
        if !self.anomaly_is_open().unwrap_or(false) {
            // **Everything under the trigger's level first, and the trigger only when nothing else
            // is left.**
            //
            // The dev, 2026-08-16, after a run walked out of a village straight into a level 7 crypt
            // and died there: *we don't have the warrior's gear to carry us through difficult
            // crypts, so instead of beelining for the nearest level 4 node, I now want the
            // pre-anomaly navigator to visit every node that is lower than level 4, so that we only
            // visit a level 4 node when the entire frontier is at least level 4.*
            //
            // Opening the portal needs a win at a node **above level 3**
            // (`crate::subworld::triggers_anomaly`, and `world_evil.lua:15-21`), so the trigger is
            // by definition the hardest thing on the board at that moment. Taking the nearest one
            // the moment it appears spends a fresh character on the worst fight available while
            // easier ground — gold, gear, hearts, and the map itself — sits unwalked beside it.
            //
            // A frontier node with no level at all counts as under the bar: no level means no combat
            // (`Place::has_combat`), so it is free to visit and cannot be the thing we are avoiding.
            //
            // This does not *refuse* the trigger, it queues it. When every frontier is level 4 or
            // above, the branch below runs exactly as it did.
            // Suppression rather than pre-emption. An earlier cut returned the gentle frontier here
            // and labelled it `Explore`, which stole the target from whichever branch below would
            // have claimed it — a free shrine came back as exploring, and eight tests said so. The
            // ladder is left to answer; this only declines to answer *with the trigger*.
            let mut candidates: Vec<&Place> = self
                .places
                .values()
                .filter(|p| p.key != here && p.triggers_anomaly() && !p.avoid && ok(p))
                .collect();
            // Any qualifying node does; prefer one we have not cleared, then the lowest level for a
            // predictable arrival, then by key so the choice is stable across runs.
            candidates.sort_by(|a, b| {
                a.completed
                    .cmp(&b.completed)
                    .then(a.remembered_level().cmp(&b.remembered_level()))
                    .then(a.key.cmp(&b.key))
            });
            if let Some(p) = candidates.first().filter(|_| !self.gentler_ground_remains(here, &ok)) {
                return Some(Plan { target: p.key.clone(), reason: Goal::OpenAnomaly, steered_by: None });
            }
        }

        // Shrines are worth going out of the way for, once the anomaly has opened and made
        // consecrating them possible at all. Getting one done means clearing its combat and then
        // `Visit` (`overworld/locations/shrine.lua:61-72`), which the loop reaches naturally: the
        // trip stops at the fight on the way in.
        //
        // Corruption changes the price of one, and "incomplete" is too blunt a proxy for it. A
        // shrine we revealed but never fought — perhaps far from the hell radius entirely — is
        // incomplete and perfectly ordinary; a corrupted one was complete until the radius reached
        // it. Both need a fight; only the second is the fight we want to avoid.
        //
        // [`Place::corrupted`] tells them apart from the save, so no prediction is needed.
        // `hellCheck` (`overworld/locations/hellportal.lua:16-23`) is in any case a radius about the
        // world ORIGIN with a perlin boundary — not about the anomaly — so predicting it would mean
        // mirroring the noise field as well as the geometry.
        //
        // Note what this does NOT do. "Take a corrupted shrine if it is on the direct path to the
        // anomaly" cannot be expressed here: while the anomaly is open and unfinished
        // [`Goal::CloseAnomaly`] outranks this branch, and once it is finished there is no path left. So
        // a corrupted shrine **whose fight is still owed** is never a *destination* — passing through
        // one is judged on arrival, by [`WorldMap::worth_consecrating_here`]. One already cleared is a
        // destination like any other, since nothing is left to avoid.
        // `anomaly_open`, `dist` and the shrine pick itself are all hoisted above the anomaly branch
        // now, since the open-portal case has to choose a shrine before that branch runs.
        //
        // **What is actually left to do at a shrine**, which `!consecrated` alone does not answer.
        // Two actions live there and they have different gates:
        //
        // * `Pray` needs the word solved and the area **unused** (`showPrayButton`,
        //   `shrine.lua:98-102`). Note what that condition does *not* require: for a major shrine the
        //   clause `hell == 0 or isConsecrated or isDesecrated` is satisfied by `hell == 0` outright,
        //   so praying is available **before** the anomaly opens, not after it. `areaUnused`
        //   (`overworldview.lua:215-218`) is the only thing that ever retires it.
        // * `Consecrate` needs `hell ~= 0` (`shrine.lua:93-96`, `activeIf` at `:244`), so it is
        //   impossible until the anomaly is open.
        //
        // A shrine offering neither is finished and must stop being a destination. It used to not,
        // and a live run showed what that costs: with `hell = 0` every shrine read as
        // "unconsecrated" forever, so the nearest one was always a valid target — and because this
        // branch excludes `here`, the node just left re-entered the candidate set at distance 1. The
        // run bounced `shrine1 -> l10 -> shrine1` twenty times and stopped having done nothing,
        // while the crypt it kept walking across never got fought because it was only ever a
        // waypoint between two shrines that each dissolved on arrival.
        //
        // The same bounce came back wearing different clothes, and `worth_a_trip` could not stop it.
        // A shrine whose puzzle we walked away from is genuinely unused — the game never set its
        // `_used` flag, because nothing was ever prayed — so it stays a perfectly valid destination
        // for a driver that has already decided never to enter it again. The caller marks the attempt
        // as spent; without [`WorldMap::abandon`] telling the planner too, the two disagree and the
        // run walks back and forth between the shrine it will not re-enter and the crypt on the way
        // to the next one. Thirty steps of `l10 -> shrine2 -> l10` is what that looks like.
        // Reached only with the portal **shut** — the open case is handled above the anomaly branch,
        // where it now outranks it. Same selection, so it is the same closure rather than a second
        // copy of the filter chain that could drift from this one.
        if let Some(p) = pick_shrine() {
            return Some(Plan { target: p.key.clone(), reason: Goal::Shrine, steered_by: None });
        }
        // Nothing qualifying is known yet, so grow the map. Unvisited places first — standing on one
        // is what produces a dump — and among those prefer the ones still hiding neighbours.
        // **Somewhere we have never stood**, which is narrower than [`Place::is_frontier`] and
        // deliberately so.
        //
        // A frontier is either unvisited *or* visited with neighbours the game refused to name. The
        // second half is worth remembering — "there is no shrine adjacent" is not a conclusion while
        // that count is nonzero — but it is **not worth walking back to**, and treating it as a
        // destination is a loop. Standing on a node again produces the same dump: the hidden count
        // is a cloud count, and clouds are cleared by `clearFogAroundPoint` (`wizard_tower.lua:61`),
        // never by arriving.
        //
        // Live, on 2026-08-08: `l41` was visited with hidden neighbours, so it stayed a frontier for
        // ever. Standing on `l35` the planner explored to `l41`; standing on `l41` the branch
        // excludes `here`, found no other free frontier, fell through to `easiest_hostile`, and
        // routed to `l9` — whose first hop is `l35`. `l35 -> l41 -> l35` until the run failed.
        //
        // Abandoned places drop out here too, matching the shrine branch. Same reason: the planner
        // and the driver must not disagree about what is still worth walking to.
        //
        // What this gives up, and it is real: after a wizard's tower clears fog, a node we have
        // already stood on may genuinely have new neighbours. Nothing clears fog yet
        // (`tower::press_reveal` is a stub), so there is no such case to lose today — and when there
        // is, the honest fix is to clear `visited` on the nodes the reveal touched rather than to
        // walk back to every one of them on spec.
        let gentle_only = self.gentler_ground_remains(here, &ok);
        let mut frontier: Vec<&Place> = self
            .places
            .values()
            .filter(|p| p.key != here && !p.visited && !p.avoid)
            .filter(|p| !self.abandoned.contains(&p.key))
            // Nothing to find there, so it is not a destination. It stays a waypoint: routing runs
            // over the edges, and this only decides what is worth walking *to*.
            .filter(|p| !p.nothing_left_to_reveal())
            .filter(|p| ok(p))
            // **The same bar exploring is asked to respect.** Suppressing the trigger branch alone
            // would only move the problem: the frontier below it holds the very same level 4+ nodes,
            // and `Explore` would walk onto one anyway. So while gentler ground remains, it is the
            // only ground exploring considers. See [`WorldMap::gentler_ground_remains`].
            .filter(|p| !gentle_only || !p.triggers_anomaly())
            .collect();
        // Order matters, and a live run showed why. From l10 with l1 and l18 both adjacent and both
        // unvisited, sorting by key chose `l1` — already **completed**, so it revealed nothing — and
        // the run then had to walk back through l10 to reach l18. Two of three hops wasted, on an
        // objective that is explicitly about speed.
        //
        // So: unvisited first, then **unfinished**, then nearest, then whatever has most left to
        // give.
        //
        // ## Why `completed` outranks distance, which it did not used to
        //
        // `visited` is set only by standing somewhere *this run* — nothing seeds it from the save.
        // So on a resume it is uniformly false, the first key ties for every place on the map, and
        // distance decides everything. A run that picks up an in-progress save therefore prefers
        // near ground it has already cleared to fresh ground slightly further off, and re-walks its
        // own footprints.
        //
        // `completed` is the half we do load (`overworld.completedAreas`), and in a subworld it is a
        // good proxy for having been there: peaceful subnodes carry
        // `competeOnVisit = subnodeIsPeaceful` (`forest.lua:107-130`), so arriving finishes them.
        // Promoting it puts the resume case back on the ordering the fresh case always had.
        //
        // Ranked, not filtered. On a resume we know completed places **by key with no edges**, so
        // excluding them outright could empty the frontier in a subworld we have partly cleared.
        // Preferring the unfinished keeps the cleared ground available as a fallback, which costs
        // nothing when there is anything better and saves the run when there is not.
        let far = usize::MAX;
        let dist = self.distances(here);
        // **Where we would be going if the map were connected**, and only asked here, on the last
        // branch, where it is the only thing left that can use it. It is the errand this pass has
        // just given up on for want of a route.
        //
        // Without it the fix trades one failure for a quieter one. The old behaviour aimed at the
        // unreachable target and let `next_hop`'s fallback step the way it lay, which at least
        // pointed somewhere; refusing to name the target at all would leave exploration with no
        // direction and no memory of what it was for.
        //
        // The recursion terminates at one level: the inner call reaches this line too, takes the
        // `false` arm, and asks nothing further.
        // The whole plan is kept, not just its target: the goal it names is what this hop is *for*,
        // and dropping it is what left every such hop labelled `Explore` — indistinguishable from a
        // run with nowhere to be.
        let declined: Option<Plan> = match need_route {
            true => self.plan(skip_hostile, false).filter(|p| !self.can_route_to(&p.target)),
            false => None,
        };
        let bearing = declined.as_ref().map(|p| p.target.clone());
        // A direction heuristic, and it used to be argued *against* here on the grounds that this
        // branch is reached only when there is nothing above to head toward. Route-gating made that
        // false: the branches above can now yield while knowing perfectly well where they wanted to
        // go, having only failed to find a way. `bearing` is that place, and this is the same
        // potential function `next_hop`'s fallback uses, applied one level up where it can pick
        // among *all* the frontiers rather than only the adjacent ones.
        //
        // Below distance on purpose. Hops are what a run actually spends; direction breaks the ties,
        // which on a surface where several neighbours sit one step away is most of the choices there
        // are. `None` for every candidate — no bearing, or a target no dump has placed — leaves the
        // key equal throughout and the ordering exactly as it was.
        let toward = |p: &Place| -> f64 {
            bearing.as_ref().and_then(|t| self.gap(&p.key, t)).unwrap_or(f64::MAX)
        };
        // **Direction outranks distance once the portal is live** — because that is where the
        // objective is. Not because it is safer, which is what this comment used to say.
        //
        // ## The safety argument was backwards, and the generator says so
        //
        // The claim here was "node level scales with distance from the origin, so exploring away
        // from the corruption walks into progressively worse fights". That holds only while the
        // portal is **shut** — which is precisely when this branch does not steer. Opening it runs
        // `overworld/generators/world.lua:496-501` over every corrupted location:
        //
        // ```lua
        //   location.level = math.max(3, location.baseLevel, 7-location.baseLevel)
        // ```
        //
        // `7-baseLevel` is an inversion, and it bites hardest exactly where `baseLevel` is lowest —
        // the nodes nearest the origin. A level 1 neighbour of the portal becomes 6; a level 2
        // becomes 5; `start` itself is set to 8 (`:507`). The level map stops being a bowl and
        // becomes a **U**: high at the rim, high at the core, a floor of 3 in the band between.
        //
        // So steering inward is steering *into* the worse fights, not away from them. It is still
        // right, for the only reason that survives: the anomaly is at the origin and finishing it is
        // the run. What does not survive is the idea that the heuristic protects us — a run that
        // aims at the corruption should expect the fights to get harder as it closes, and should
        // arrive healthy rather than assume the approach is the safe part.
        //
        // The death this comment cited is still real and still argues for steering: live 2026-08-10,
        // with no bearing at all, "nearest unvisited" drifted outward from `l19` to `l28` to `l49`,
        // a level 6 crypt, and the run died there. Both rims are dangerous. Only one of them has the
        // objective on it.
        //
        // Below distance is where this used to sit, on the argument that hops are what a run
        // actually spends. That holds while the anomaly is shut, when exploring is genuinely about
        // growing the map and one direction is as good as another. It stops holding the moment
        // there is somewhere to be.
        // **A bearing is a direction, not a name**, and this used to switch on the name.
        //
        // `bearing.is_some()` says only that somewhere was wanted. Whether it can *order* the
        // frontier is a different question: `toward` needs `gap` to place both ends, and it returns
        // `None` — leaving every candidate at `f64::MAX` and the comparison equal throughout —
        // whenever the bearing has no position. The anomaly's own position is estimated from the
        // centroid of corrupted nodes that are *already placed* (see [`WorldMap::pos_for`]), so a map
        // whose corrupted nodes came from save flags rather than from dumps yields nothing to aim at.
        //
        // In that state the old flag read `true` while the term did nothing at all, and the run
        // sorted by hops — nearest-unvisited — which is what it would have done with no steering
        // written. Live 2026-08-12 that walked out of the corruption and into a bandit camp.
        //
        // A bearing that lands on the node we are standing on is the same failure wearing a
        // different hat: every candidate is measured against a point already reached. `placed`
        // counting only *finite* distances is what excludes both, because `gap` to an unplaced
        // bearing and `gap` from an unplaced candidate are both `None`.
        //
        // Compared against the sentinel rather than with `is_finite`, which is the trap here:
        // `toward` substitutes `f64::MAX` for an unmeasurable candidate, and `f64::MAX` *is* finite.
        // The first version of this counted every candidate as placed and reported steering on a map
        // with nothing to aim at — the exact bug it was written to catch.
        let placed = frontier.iter().filter(|p| toward(p) < f64::MAX).count();
        let steer = self.anomaly_is_open().unwrap_or(false) && placed > 0;
        let steered_by =
            steer.then(|| (bearing.clone().unwrap_or_default(), placed, frontier.len()));
        frontier.sort_by(|a, b| {
            let by_hops =
                dist.get(&a.key).unwrap_or(&far).cmp(dist.get(&b.key).unwrap_or(&far));
            let by_bearing = toward(a).total_cmp(&toward(b));
            a.visited
                .cmp(&b.visited)
                .then(a.completed.cmp(&b.completed))
                .then(match steer {
                    true => by_bearing.then(by_hops),
                    false => by_hops.then(by_bearing),
                })
                .then(b.connections.cmp(&a.connections))
                .then(b.hidden.unwrap_or(0).cmp(&a.hidden.unwrap_or(0)))
                .then(a.key.cmp(&b.key))
        });
        // **What this hop is for**, which is a different question from whether it can be aimed.
        // `RouteTo` whenever an errand was declined for want of a route, steered or not; plain
        // `Explore` only when nothing was wanted in the first place.
        let reason = match declined {
            Some(p) => Goal::RouteTo(Box::new(p.reason)),
            None => Goal::Explore,
        };
        frontier
            .first()
            .map(|p| Plan { target: p.key.clone(), reason: reason.clone(), steered_by: steered_by.clone() })
            // **The release for [`SHRINES_BEFORE_THE_ANOMALY`].**
            //
            // Reached only when every branch above declined *and* there is no frontier left to
            // walk — so the run has consecrated what it could, explored what it could, and the bar
            // is still unmet. Nothing further will change that: exploring is the only thing that
            // reveals a new shrine, and it has just run out.
            //
            // Placed here rather than as a clause on the gate so that it cannot fire early. Written
            // into the condition it would have to reproduce "is there anything else at all worth
            // doing", which is precisely what the rest of this function computes; asking it *after*
            // the answer is known is the only version that stays true as branches are added.
            //
            // The run then walks into the anomaly under-prepared and probably dies, which is the
            // honest ending. A plan it can never satisfy is not the safer alternative — it is the
            // loop guard ending the run four laps later with nothing learned.
            .or_else(|| {
                self.anomaly()
                    .filter(|p| ok(p))
                    .map(|p| Plan { target: p.key.clone(), reason: Goal::CloseAnomaly, steered_by: None })
            })
    }

    /// The single adjacent node to step to next, and the plan it serves.
    ///
    /// Travel is taken one hop at a time rather than by naming a distant destination, because the
    /// destination may not be selectable — see [`crate::subworld::Rules::multi_hop_travel`], which
    /// is false wherever the map does not accumulate. Moving one hop is correct everywhere, so it
    /// is what we always do.
    ///
    /// The hop is chosen by breadth-first search over the edges **we** have recorded, which stay
    /// valid even where the layout is re-rolled
    /// ([`crate::subworld::Rules::edges_survive_reentry`]). That is not re-implementing the game's
    /// pathfinder: it only decides which neighbour points the right way, and the game still refuses
    /// the move if it disagrees.
    ///
    /// Falls back to any adjacent frontier when the target is not reachable through known edges —
    /// which is the normal case early on, when the way there simply has not been mapped yet.
    pub fn next_hop(&self) -> Option<Hop> {
        let here = self.here.as_deref()?;
        let plan = self.next_target()?;
        // Two passes. The first refuses to route through a lost woods we have already escaped, even
        // if that means a much longer way round. The second allows it, and exists only so that a
        // world where every route passes through one leaves us moving rather than stuck — being
        // swallowed again is bad, standing still forever is worse.
        if let Some(step) = self.first_step_toward(here, &plan.target, true) {
            return Some(Hop { step, plan, routed: true });
        }
        if let Some(step) = self.first_step_toward(here, &plan.target, false) {
            return Some(Hop { step, plan, routed: true });
        }
        // No known route. Step to an adjacent frontier instead: mapping outward is what will
        // eventually connect us to the target.
        //
        // The plan is carried through unchanged. Replacing it with the frontier node would make the
        // goal follow the footsteps — every hop would re-derive a nearby target and the run would
        // wander instead of working toward the trigger.
        //
        // **And this is where a direction is worth having.** We know where we are going and have no
        // way to get there, so the graph has nothing to say and the choice used to come down to the
        // lowest key — a run heading for the anomaly stepping whichever way the alphabet pointed.
        //
        // [`WorldMap::gap`] answers it whenever the target has been placed by some dump, which is
        // the one thing a route needs and a direction does not: the target may sit in a component we
        // cannot reach and its position is still true. This is the potential function
        // `docs/superpowers/notes/navigation-loops.md` says every navigation bug here came down to
        // lacking, and this is the branch that most needed one.
        //
        // Its limit is worth stating next to it. A place no dump has ever shown us has **no position
        // either**, so a target learned only from `completedAreas` — which is how the anomaly key
        // arrives on a fresh launch — gets `None` here and the ordering falls back to what it always
        // did. Coordinates speak about places we have seen, never about places we have merely heard
        // of. Getting direction on the anomaly before meeting it would need a different source.
        let me = self.places.get(here)?;
        let mut options: Vec<&Place> = me
            .neighbours
            .iter()
            .filter_map(|k| self.places.get(k))
            .filter(|p| p.is_frontier())
            .collect();
        let toward = |p: &Place| -> i64 {
            self.gap(&p.key, &plan.target).map(|d| d as i64).unwrap_or(i64::MAX)
        };
        // Avoided places sort last rather than out, so they remain a last resort.
        options.sort_by(|a, b| {
            a.avoid
                .cmp(&b.avoid)
                .then(a.visited.cmp(&b.visited))
                .then(toward(a).cmp(&toward(b)))
                .then(a.key.cmp(&b.key))
        });
        options.first().map(|p| Hop { step: p.key.clone(), plan, routed: false })
    }

    pub fn wants_a_heart(&self) -> bool {
        self.gold >= HEART_FLOOR
    }

    /// How many hearts this purse can take off a shelf, holding a bed back.
    ///
    /// **Settlements stock more than one.** The standing assumption was one each; the dev found it
    /// wrong on 2026-08-15, so the shop empties the shelf instead of taking a single item off it.
    /// Every one still leaves [`crate::rest::INN_COST`] behind — the reserve is a floor under the
    /// whole visit, not a check on the first purchase.
    pub fn hearts_affordable(&self) -> i64 {
        ((self.gold - crate::rest::INN_COST) / HEART_COST).max(0)
    }

    /// Should we stop for a bed at `key`, purely because we are standing at one?
    ///
    /// The dev's rule, 2026-08-15: **at a settlement, below full health, with the inn's price in
    /// pocket — rest.** Deliberately weaker than [`WorldMap::wants_rest`], which waits for half
    /// health or a four-point drop, because those are the bars for making a *detour*. Standing in
    /// the doorway is not a detour: six health for ten gold before the next fight is the cheapest
    /// trade on the board, and this run keeps dying two points short.
    ///
    /// Takes `&mut self` because deciding is also *recording*. Everything that walks us to the inn
    /// from inside — `inn_inside`, `seeking_a_rest`, `cross_toward` — is written around
    /// `wants_rest`, so a top-up that did not set it would be a decision the rest of the map never
    /// heard about. That is the planner/driver split this same commit is fixing elsewhere, and it is
    /// not worth re-introducing one flag lower down.
    ///
    /// `false` when health has never been read: an unknown reading is not evidence of a wound, and
    /// the errand costs a subworld crossing.
    pub fn top_up_at(&mut self, key: &str) -> bool {
        let settlement = self.places.get(key).map(|p| p.has_an_inn()).unwrap_or(false);
        let hurt = self.health.map(|h| !h.is_full()).unwrap_or(false);
        if !(settlement && hurt && self.gold >= crate::rest::INN_COST) {
            return false;
        }
        // **[`WorldMap::top_up`], not `wants_rest`** — task #72, and the whole point of the split.
        //
        // This wrote `wants_rest` until 2026-08-21, which made the doorway rule a detour rule. Live
        // in the 1519Z run: health 77/80, a three-point drop that neither `note_health`
        // (needs four) nor `note_health_level` (needs half) can act on, and the plan came out
        // `Rest -> l63` — a surface hop taken to heal three points for ten gold. The dev: *why did
        // we rest at Treasured Balsa despite barely having any missing health?*
        //
        // It also fired for settlements the caller then refused: `navigate.rs` computes this
        // *before* testing `trades()` and corruption, so a village under attack set the flag on the
        // way past, the top-up was declined, and the detour outlived it.
        self.top_up = true;
        true
    }

    /// Could the game route us through a level 4+ node on the way to `to`?
    ///
    /// **Every** shortest path is checked, not the one we would have walked, and that is forced by
    /// how the game chooses. `canTravelToIndirect` (`overworldview.lua:1330-1373`) is a
    /// breadth-first search — `toCheck` is popped from the back (`table.remove(toCheck)`) and pushed
    /// at the front (`table.insert(toCheck, 1, location2)`), so a ring is exhausted before the next
    /// begins — and `pathHash[key]` is written only `if not pathHash[key]`, so the predecessor that
    /// sticks is the shallowest one. The path handed to `travelTo` is therefore a **shortest** path.
    ///
    /// Which one, we cannot know: ties are broken by `pairs(location.connections)`, whose order Lua
    /// does not define. So the only safe question is whether *all* of them are gentle, and that is
    /// the dev's rule of 2026-08-17.
    ///
    /// The standard characterisation, over the edges we have recorded: `n` lies on some shortest
    /// path exactly when `d(from, n) + d(n, to) == d(from, to)`. Two breadth-first sweeps answer it
    /// for every node at once.
    ///
    /// `from` is exempt because we are already standing on it. `to` is exempt only when it is the
    /// errand's own destination — the planner chose that and has its own rules for choosing it — and
    /// **not** when it is an intermediate node this chain picked, because landing there is then our
    /// doing and nobody else's.
    ///
    /// ## What this deliberately does not do, and it is worth doing later
    ///
    /// A shortest path with a level 6 crypt on it blocks the hop outright, even when a slightly
    /// longer route round the crypt exists and the game would happily walk it if asked. The fix is
    /// to aim at an intermediate node that forces the detour, and then hop again — the dev's own
    /// suggestion, parked as post-MVP. Until then this is conservative in the direction of taking an
    /// ordinary step, which is what the run did before fast hops existed.
    pub(super) fn shortest_paths_are_gentle(&self, from: &str, to: &str, exempt_end: bool) -> bool {
        let out = self.hops_from(from);
        let back = self.hops_from(to);
        let Some(&total) = out.get(to) else { return false };
        for (key, &d) in &out {
            if back.get(key).map(|b| d + b) != Some(total) {
                continue; // not on any shortest path
            }
            if key == from || (exempt_end && key == to) {
                continue;
            }
            // An unknown node counts as dangerous: we cannot read a level off a heading we do not
            // have, and a hop is an optimisation — declining one costs a press, taking a bad one can
            // cost the run.
            //
            // `remembered_level` rather than `level`, or a crypt cleared by an **earlier** run and
            // recalled from the cache reads as level 0 and we hop straight over a fight (#79). A
            // crypt cleared by *this* run still reads 0 and still hops, because there the game is
            // the one saying so — see [`Place::remembered_level`].
            let gentle =
                self.places.get(key).map(|p| p.remembered_level().unwrap_or(0) <= 3).unwrap_or(false);
            if !gentle {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observe::adjacency::Node;
    use crate::overworld::fixtures::*;
    use crate::overworld::*;

    /// **A corrupted shrine becomes a target exactly when the clean ones cannot reach the bar.**
    /// Task #74.
    ///
    /// The dev, 2026-08-21: *a corrupted shrine should become a target when it is the last to
    /// satisfy the requirement. That also means that if two shrines are in the corruption, then they
    /// become targets after two consecrations of uncorrupted shrines. Generalize this to any number
    /// of corrupted shrines.*
    ///
    /// Both sentences describe the same condition — **the clean candidates have run out and the bar
    /// is still short** — and the second is what rules out the other reading. "We can already see
    /// we will need them" would admit a corrupted shrine at nought consecrations whenever the clean
    /// ones are too few, and the dev put the moment at *after two consecrations*: take the free ones
    /// first, always. That ordering is not a nicety. A corrupted shrine costs a fight that can end
    /// the run, and a free consecration banked before it is a free consecration kept.
    #[test]
    fn a_corrupted_shrine_is_a_target_only_once_the_clean_ones_run_out() {
        // `clean` uncorrupted and `foul` corrupted, all reachable, portal open.
        let world = |clean: usize, foul: usize| {
            let mut m = WorldMap::new();
            let mut names: Vec<String> = Vec::new();
            for i in 0..clean + foul {
                names.push(format!("shrine{}", i + 1));
            }
            m.fold(&dump(
                "here",
                "camp",
                names.iter().map(|k| node(k, "Gransmoor shrine")).collect(),
            ));
            for (i, k) in names.iter().enumerate() {
                m.entry(k).corrupted = i >= clean;
            }
            m.here = Some("here".into());
            m.hell = Some(0.1);
            m
        };
        let consecrate = |m: &mut WorldMap, n: usize| {
            for i in 0..n {
                let p = m.entry(&format!("shrine{}", i + 1));
                p.consecrated = true;
                p.used = true;
            }
        };
        let target = |m: &WorldMap| m.next_target().map(|p| (p.reason, p.target));

        // **The dev's second sentence**, at the bar's own value: two clean, the rest corrupted.
        // Before the clean ones are spent, a corrupted shrine is not a destination.
        let mut m = world(2, 2);
        assert_eq!(
            target(&m),
            Some((Goal::Shrine, "shrine1".into())),
            "the clean one first, even though a corrupted one is exactly as near"
        );
        consecrate(&mut m, 1);
        assert_eq!(target(&m), Some((Goal::Shrine, "shrine2".into())), "and the second clean one");

        // Both clean ones are spent and the bar is still short: now they are targets.
        consecrate(&mut m, 2);
        assert!(m.consecrations() < SHRINES_BEFORE_THE_ANOMALY, "the premise: still short");
        assert_eq!(
            target(&m),
            Some((Goal::Shrine, "shrine3".into())),
            "nothing clean is left and the bar is unmet, so the fight is worth it"
        );

        // **Generalised.** Plenty of clean shrines and the corrupted ones are never wanted: the bar
        // is met before they are reached, and `CloseAnomaly` takes over.
        let mut m = world(SHRINES_BEFORE_THE_ANOMALY, 3);
        consecrate(&mut m, SHRINES_BEFORE_THE_ANOMALY);
        m.entry("start").heading = "The Rift — level 8 anomaly".into();
        m.entry("start").corrupted = true;
        m.fold(&dump("here", "camp", vec![node("start", "The Rift — level 8 anomaly")]));
        assert_eq!(
            target(&m).map(|(r, _)| r),
            Some(Goal::CloseAnomaly),
            "the bar is met, so a corrupted shrine is not a detour on the way to the portal"
        );

        // **A corrupted shrine that cannot advance the bar is never bought.** `is_shrine` answers by
        // heading too, so `s1` is a candidate; `can_be_consecrated` reads the key, so it can never
        // be consecrated and no number of them satisfies the gate. Found by an older fixture rather
        // than by this one, which was too tidy — every shrine in it is keyed `shrineN`.
        let mut m = world(0, 0);
        m.fold(&dump("here", "camp", vec![node("s1", "Faraway shrine")]));
        m.entry("s1").corrupted = true;
        assert!(m.corrupted_shrines_are_needed(), "the exception is open");
        assert_ne!(target(&m).map(|(r, _)| r), Some(Goal::Shrine), "and it still does not apply");

        // And with the bar met, a corrupted shrine stays off the list even with clean ones gone —
        // which is the half that stops this from becoming the 2026-08-15 shrine-chase again.
        let mut m = world(SHRINES_BEFORE_THE_ANOMALY, 2);
        consecrate(&mut m, SHRINES_BEFORE_THE_ANOMALY);
        assert_ne!(
            target(&m).map(|(_, t)| t),
            Some(format!("shrine{}", SHRINES_BEFORE_THE_ANOMALY + 1)),
            "no corrupted shrine is wanted once the requirement is satisfied"
        );
    }

    /// **The three changes of 2026-08-21 that share `pick_shrine`, exercised together.**
    ///
    /// Written as an interaction check rather than for a fault: #70 removed the fight-free route
    /// filter, #74 admitted corrupted shrines once the clean supply is spent, and the bar dropped to
    /// three, all on the same afternoon and none of it run. Each has its own test; this asks what
    /// they do *at once*, which is the case a live run will actually present.
    ///
    /// The combination is deliberately the most permissive one reachable: portal open, bar short,
    /// nothing clean left, and the only candidate corrupted and behind a level 9 forest. Every guard
    /// that used to refuse it has been removed by one of the three, so if the run can be sent
    /// somewhere it cannot act, this is where.
    #[test]
    fn the_shrine_rules_of_2026_08_21_do_not_combine_into_an_unreachable_target() {
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("l53", "Beeford Hedge — level 9 forest")]));
        m.fold(&dump(
            "l53",
            "Beeford Hedge — level 9 forest",
            vec![node("shrine2", "Gransmoor shrine")],
        ));
        m.here = Some("here".into());
        m.hell = Some(0.1);
        m.entry("shrine2").corrupted = true;

        // The premises, each named so a fixture that drifts says which rule stopped applying.
        assert!(m.corrupted_shrines_are_needed(), "#74: nothing clean and the bar is short");
        assert!(
            !m.reachable_without_a_fight("here", "shrine2"),
            "#70: the only way there is a level 9 forest, and that no longer disqualifies it"
        );

        let plan = m.next_target().expect("a plan");
        assert_eq!(plan.reason, Goal::Shrine);
        assert_eq!(plan.target, "shrine2");

        // **The half that matters.** A target the driver cannot set off toward is worse than no
        // target: it is the shape of every loop this project has had. `next_hop` is what the step
        // actually asks, so ask it.
        let hop = m.next_hop().expect("a hop toward the shrine we just nominated");
        assert_eq!(hop.plan.target, "shrine2");
        assert_eq!(hop.step, "l53", "through the forest, which is the fight we agreed to pay");

        // And with the bar met, the same map must stop offering it — the guard that keeps #74 from
        // becoming the shrine-chase of 2026-08-15.
        ready_for_the_anomaly(&mut m);
        assert!(!m.corrupted_shrines_are_needed());
        assert_ne!(
            m.next_target().map(|p| p.reason),
            Some(Goal::Shrine),
            "satisfied, so a corrupted shrine behind a level 9 forest is a detour again"
        );
    }

    /// **The portal waits for [`SHRINES_BEFORE_THE_ANOMALY`]**, and what the run does while short.
    ///
    /// The dev, 2026-08-20, after the run that reached the anomaly and died at turn 24: *before the
    /// anomaly fight, ensure that at least 4 major shrines have been consecrated. This means that we
    /// need to keep exploring if the anomaly opens before we've revealed 4 shrines.* Lowered to
    /// three on 2026-08-21.
    ///
    /// **Written against the constant, not against a number.** It named four in its title and
    /// counted to three by hand, so lowering the bar broke it in a way that read as a regression —
    /// `assertion left != right failed: CloseAnomaly, CloseAnomaly`, which says nothing about the
    /// bar at all. Off-by-one is the point of a bar, so the off-by-one is now derived from it.
    #[test]
    fn the_portal_waits_for_its_consecrations_and_we_explore_until_then() {
        let build = || {
            let mut m = WorldMap::new();
            m.fold(&dump(
                "here",
                "camp",
                vec![node("rift", "The Rift anomaly"), node("l2", "Bainton Clump road")],
            ));
            m.here = Some("here".into());
            m.hell = Some(0.1);
            m
        };

        // Nothing consecrated: the portal is on the map, reachable, and still not the errand.
        let m = build();
        assert_eq!(m.consecrations(), 0);
        assert!(m.anomaly().is_some(), "the portal really is there to be chosen");
        let plan = m.next_target().expect("a plan");
        assert_ne!(plan.reason, Goal::CloseAnomaly, "under the bar, the portal waits");
        assert_eq!(plan.reason, Goal::Explore, "and exploring is what the dev asked for instead");

        // One short is still short. Off by one is the whole point of a bar.
        let mut m = build();
        let short = SHRINES_BEFORE_THE_ANOMALY - 1;
        for i in 1..=short {
            let p = m.entry(&format!("shrine{i}"));
            p.consecrated = true;
            p.used = true;
        }
        assert_eq!(m.consecrations(), short);
        assert_ne!(m.next_target().unwrap().reason, Goal::CloseAnomaly);

        // The last one opens the gate.
        let mut m = build();
        ready_for_the_anomaly(&mut m);
        assert_eq!(m.consecrations(), SHRINES_BEFORE_THE_ANOMALY);
        let plan = m.next_target().expect("a plan");
        assert_eq!(plan.reason, Goal::CloseAnomaly);
        assert_eq!(plan.target, "rift");
    }

    /// **The release**, without which the gate is a stall rather than a rule.
    ///
    /// A world that never yields enough shrines — too few placed, or all of them behind ground we
    /// cannot cross — must still end its run at the anomaly rather than walking an exhausted
    /// frontier for ever. That is the case the loop guard would otherwise have to end.
    #[test]
    fn with_nothing_left_to_explore_the_portal_is_taken_anyway() {
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("rift", "The Rift anomaly")]));
        m.here = Some("here".into());
        m.hell = Some(0.1);
        assert_eq!(m.consecrations(), 0, "still under the bar, and nothing will change that");

        // Exhaust the frontier: everywhere is visited and nowhere is hiding a neighbour, so
        // exploring has nothing left to offer.
        for key in ["here", "rift"] {
            let p = m.entry(key);
            p.visited = true;
            p.hidden = Some(0);
            p.connections = 1;
        }

        let plan = m.next_target().expect("a plan even so");
        assert_eq!(
            plan.reason,
            Goal::CloseAnomaly,
            "nothing left to prepare with, so the run goes and dies honestly"
        );
        assert_eq!(plan.target, "rift");
    }

    /// **Bank Well-Rested stacks before a deliberate deep fight**, and every clause of the dev's
    /// rule of 2026-08-20.
    #[test]
    fn a_deep_fight_sends_us_to_an_inn_for_stacks_first() {
        let build = || {
            let mut m = WorldMap::new();
            m.fold(&dump(
                "here",
                "camp",
                vec![
                    node("rift", "The Rift — level 8 anomaly"),
                    node("l11", "Rowlston Covert village"),
                ],
            ));
            m.here = Some("here".into());
            m.hell = Some(0.1);
            m.gold = HEART_FLOOR - 1;
            // **The shelf is bare, and that is what isolates this test.** A village with a heart
            // still on it does two things at once: `Goal::Heart` outranks the portal, and the
            // heart's reserve stops the bank spending. Both are the dev's ruling of 2026-08-20 and
            // both are pinned in `the_heart_outranks_the_bank_for_the_same_purse`. Here the
            // question is what the *portal* does to the plan, so the heart is taken off the board.
            m.bought_the_heart("l11");
            ready_for_the_anomaly(&mut m);
            m
        };

        // Spent out, which is how the run of 2026-08-20 arrived at the portal.
        let m = build();
        assert_eq!(m.stacks_short_ahead(), 16, "twice the level 8 anomaly");
        let plan = m.next_target().expect("a plan");
        assert_eq!(plan.reason, Goal::StockUp, "the bed comes before the fight");
        assert_eq!(plan.target, "l11", "and it is the village that has one");

        // A bank already deep enough leaves the errand exactly as it was.
        let mut m = build();
        m.well_rested = crate::rest::stacks_wanted(8);
        assert_eq!(m.stacks_short_ahead(), 0);
        assert_eq!(m.next_target().unwrap().reason, Goal::CloseAnomaly, "nothing left to buy");

        // **The dev's floor.** Below the inn's price the requirement is unmeetable, not merely
        // unmet, and holding the run at it would stall instead of preparing.
        let mut m = build();
        m.gold = crate::rest::INN_COST - 1;
        assert_eq!(m.next_target().unwrap().reason, Goal::CloseAnomaly, "broke, so get on with it");

        // Eleven stacks — what a real run reached, in `at-woodland-shrine-unprayed` — is still five
        // short of the anomaly, so the errand still fires.
        let mut m = build();
        m.well_rested = 11;
        assert_eq!(m.stacks_short_ahead(), 5);
        assert_eq!(m.next_target().unwrap().reason, Goal::StockUp);
    }

    /// **Hearts outrank the bank, because they share one purse.** The dev's ruling, 2026-08-20.
    ///
    /// Four maximum health is permanent for the rest of the run; a bank of stacks is spent by one
    /// fight. Left to compete, sixteen stacks at ten gold apiece walk a hundred-gold heart's budget
    /// out of the purse on the way past, and nothing in the ordering noticed — which is the open
    /// question this closes.
    #[test]
    fn the_heart_outranks_the_bank_for_the_same_purse() {
        let build = |gold: i64| {
            let mut m = WorldMap::new();
            m.fold(&dump(
                "here",
                "camp",
                vec![
                    node("rift", "The Rift — level 8 anomaly"),
                    node("l11", "Rowlston Covert village"),
                ],
            ));
            m.here = Some("here".into());
            m.hell = Some(0.1);
            m.gold = gold;
            ready_for_the_anomaly(&mut m);
            m
        };

        // The purse from the live save on 2026-08-20: plenty for both, so both happen — and the
        // heart goes first because it outranks on the ladder.
        let m = build(465);
        assert_eq!(m.stacks_short_ahead(), 16, "the want is unchanged by the reserve");
        assert_eq!(m.stacks_to_buy(), 16, "and 465 less the reserve still covers all of it");
        assert_eq!(m.next_target().unwrap().reason, Goal::Heart, "the heart is the errand");

        // **The case the ruling is about.** Enough for a heart and a bed, and not a coin more: every
        // stack bought here is a heart not bought.
        let m = build(HEART_FLOOR);
        assert_eq!(m.stacks_short_ahead(), 16, "still wanted");
        assert_eq!(m.stacks_to_buy(), 0, "and not one of them may be paid for");
        assert!(!m.wants_a_bed(), "so the inn is not an errand while the shelf is stocked");

        // Ten over the reserve buys exactly one stack, and no more.
        let m = build(HEART_FLOOR + crate::rest::INN_COST);
        assert_eq!(m.stacks_to_buy(), 1);

        // **The reserve is the heart's, so it lifts when the heart is gone.** The same purse that
        // could buy nothing above now buys eleven.
        let mut m = build(HEART_FLOOR);
        m.bought_the_heart("l11");
        assert_eq!(m.stacks_to_buy(), 11, "110 gold is eleven rests once nothing is being saved for");
        assert!(m.wants_a_bed());

        // **Being hurt is not subject to any of this.** The ruling ranks two kinds of preparation;
        // a wound outranks both, and `wants_rest` keeps its own plain ten-gold gate.
        let mut m = build(HEART_FLOOR);
        assert!(!m.wants_a_bed(), "the control: nothing wants a bed at this purse yet");
        m.note_health_level(crate::rest::Health { current: 1, max: 52 });
        assert!(m.wants_rest(), "the fixture must really be hurt");
        assert!(m.wants_a_bed(), "and a hurt run still goes to bed with the heart unbought");
    }

    /// A forest does not hold the run at an inn, however deep it is.
    #[test]
    fn a_level_nine_forest_is_not_worth_banking_for() {
        let mut m = WorldMap::new();
        m.fold(&dump(
            "here",
            "camp",
            vec![
                node("shrine7", "Cottam Boscage — level 9 forest"),
                node("l11", "Rowlston Covert village"),
            ],
        ));
        m.here = Some("here".into());
        m.gold = 500;
        assert_eq!(m.well_rested, 0, "an empty bank, so only the forest rule can be answering");
        assert_eq!(
            m.stacks_short_ahead(),
            0,
            "the forest is walked through, not cleared — the dev's correction"
        );
        assert!(!m.wants_a_bed(), "so nothing sends us to a bed");
    }

    /// Knowing the roster does not let the anomaly through the gate.
    ///
    /// Worth its own assertion because the two numbers are one word apart and the dev's phrasing
    /// used "revealed" for what the code counts as consecrated. Seven shrines known and none
    /// consecrated is exactly the state the 2026-08-20 run was in when it walked into the portal.
    #[test]
    fn a_full_roster_with_nothing_consecrated_still_holds_the_portal() {
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("rift", "The Rift anomaly")]));
        m.here = Some("here".into());
        m.apply_save(
            &crate::game::save::parse(
                "return { overworld = { areaFlags = {
                     hell = 0.1,
                     shrine1_explored = 1, shrine2_explored = 1, shrine3_explored = 1,
                     shrine4_explored = 1, shrine5_explored = 1, shrine6_explored = 1,
                     shrine7_explored = 1,
                 } } }",
            )
            .unwrap(),
        );
        assert_eq!(m.shrines_known(), 7);
        assert_eq!(m.consecrations(), 0);
        assert_ne!(
            m.next_target().unwrap().reason,
            Goal::CloseAnomaly,
            "knowing where they are is not having done them"
        );
    }

    /// A shrine that would open the anomaly is not an errand while the anomaly is shut.
    ///
    /// **The fixture is the run of 2026-08-17 0436Z**, which is why it is a level 9 forest and not
    /// a round number. The log reads:
    ///
    /// ```text
    /// 17. l55 -> **shrine7** (for shrine7, Shrine)
    /// 18. skipped the anomaly cinematic
    /// 19. fighting `shrine7` (Cottam Boscage — level 9 forest)
    /// ```
    ///
    /// at 52/52 health, with the portal shut when the target was chosen. Both cost filters in
    /// `pick_shrine` are written `!anomaly_open || …`, so before the portal opens nothing priced
    /// the trip at all.
    ///
    /// The level 1 shrine is the control, and it is the half that stops this being "switch the
    /// shrine errand off pre-anomaly". The dev, on the woodland shrine the same day: *triggering a
    /// fight at a shrine is fine.* A fight is fine; ending the pre-anomaly phase to get one is not.
    ///
    /// The rule is [`Place::triggers_anomaly`] alone — see the filter for why `&& !completed` was a
    /// clause about a world that cannot exist, and why this test does not build one.
    #[test]
    fn a_shrine_that_would_open_the_anomaly_is_not_a_pre_anomaly_errand() {
        let mut m = WorldMap::new();
        m.fold(&dump(
            "l55",
            "Emswell campfire",
            vec![
                node("shrine7", "Cottam Boscage — level 9 forest"),
                node("shrine1", "Gripthorpe Brush shrine"),
            ],
        ));
        m.here = Some("l55".into());
        m.hell = Some(0.0);

        assert!(m.get("shrine7").unwrap().triggers_anomaly(), "the premise: level 9 opens it");
        assert!(!m.get("shrine1").unwrap().triggers_anomaly(), "and a plain shrine does not");

        // The gentle shrine is the errand, so the branch is alive and both are reachable.
        let plan = m.next_target().expect("a plan");
        assert_eq!(plan.reason, Goal::Shrine);
        assert_eq!(plan.target, "shrine1", "the shrine that costs nothing to reach");

        // Take it away and the level 9 one must not inherit the errand.
        m.entry("shrine1").used = true;
        if let Some(plan) = m.next_target() {
            assert_ne!(
                plan.target, "shrine7",
                "walking here opens the portal, which is the phase we are still in: {plan:?}"
            );
        }

        // **No `completed` case here, deliberately.** The obvious third assertion — clear `shrine7`
        // and watch it stop being excluded — is a green test in a world that cannot exist: while
        // `hell == 0` a level 4+ node is uncompleted without exception, because clearing it needs
        // arriving and arriving opens the portal. That is precisely the mistake the note on
        // [`Place::triggers_anomaly`] was kept to record, and it was written here anyway on the
        // first attempt before the dev caught it.
    }

    /// With the portal open, a prayed-at **minor** shrine must stop being a destination.
    ///
    /// The pairing this pins is stated at `worth_a_trip` itself: the goal filter and
    /// [`WorldMap::worth_consecrating_here`] must agree, and this project has now watched them drift
    /// apart twice in opposite directions. The second time was self-inflicted on 2026-08-17 —
    /// `can_be_consecrated` went into the arrival test alone, leaving the goal filter proposing a
    /// trip that arrival would always decline.
    ///
    /// A minor shrine can never take `_consecrated`, so `!p.consecrated` is true for ever; without
    /// the gate this is not a bounce that ends, it is one that cannot.
    ///
    /// The major shrine in the same fixture is the control. It differs only in its key, and it *is*
    /// still a destination — so this cannot pass by the shrine branch having switched itself off.
    #[test]
    fn an_open_portal_does_not_make_a_prayed_minor_shrine_a_destination_for_ever() {
        let mut m = WorldMap::new();
        m.fold(&dump(
            "l10",
            "Trenwick — level 1 crypt",
            vec![node("shrine1", "Swanland shrine"), node("l4sub9", "Bainton Clump woodland shrine")],
        ));
        m.here = Some("l10".into());
        m.hell = Some(0.1);
        for k in ["shrine1", "l4sub9"] {
            m.entry(k).completed = true;
            m.entry(k).visited = true;
            // Prayed at, so the only thing either could still owe is a consecration.
            m.entry(k).used = true;
        }

        // The major shrine still owes one, so the branch is alive and the fixture is routable.
        let plan = m.next_target().expect("a plan");
        assert_eq!(plan.reason, Goal::Shrine, "the major shrine is genuinely unconsecrated");
        assert_eq!(plan.target, "shrine1");

        // Retire it, and nothing may fall through to the woodland shrine.
        m.entry("shrine1").consecrated = true;
        if let Some(plan) = m.next_target() {
            assert_ne!(
                plan.reason,
                Goal::Shrine,
                "a minor shrine can never be consecrated, so this errand could never end: {plan:?}"
            );
            assert_ne!(plan.target, "l4sub9", "and it must not be the target under any goal");
        }
    }

    #[test]
    fn routing_prefers_a_trigger_node_over_plain_exploration() {
        let mut m = WorldMap::new();
        m.fold(&dump(
            "start",
            "camp",
            vec![node("l4", "Grim Barrow — level 4 crypt"), node("l2", "Quiet Glade meadow")],
        ));

        // **The dev's rule of 2026-08-16 reversed what this used to assert.** Opening the portal
        // needs a win above level 3, so the trigger is the hardest fight available; a meadow with no
        // level is free. The meadow goes first, and `l4` is not even offered as somewhere to
        // explore while it stands.
        let plan = m.next_target().unwrap();
        assert_eq!(plan.target, "l2", "the gentle ground before the level 4 crypt");
        assert_ne!(plan.reason, Goal::OpenAnomaly);

        // With the meadow walked and nothing left under the bar, the trigger is the plan again.
        m.entry("l2").visited = true;
        m.entry("l2").hidden = Some(0);
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::OpenAnomaly);
        assert_eq!(plan.target, "l4");
    }

    #[test]
    fn once_the_anomaly_has_opened_the_trigger_is_no_longer_a_goal() {
        let mut m = WorldMap::new();
        // Standing on `start`, which the anomaly promoted to the portal under us.
        m.fold(&dump("start", "camp", vec![node("l4", "Grim Barrow — level 4 crypt")]));
        m.hell = Some(0.1); // the anomaly already opened
        let plan = m.next_target().unwrap();
        assert_ne!(plan.reason, Goal::OpenAnomaly, "chasing a spent trigger wastes the run");
    }

    #[test]
    fn the_anomaly_is_known_before_any_dump_names_it() {
        // `start` is promoted to the portal in place, so once `hell` is nonzero we know where to go
        // without exploring for it -- and after corruption, `start` is usually "(unheaded)" in our
        // map because we only heard of it through save flags.
        let mut m = WorldMap::new();
        ready_for_the_anomaly(&mut m);
        m.fold(&dump("l39", "Eight Timberland — level 4 forest", vec![node("l29", "Rookdale — level 3 crypt")]));
        m.entry("start").corrupted = true;
        m.hell = Some(0.1);
        assert_eq!(m.anomaly().map(|p| p.key.as_str()), Some("start"));
        assert!(m.anomaly_is_assumed(), "no heading has confirmed it yet");

        // **Knowing where it is and being able to go there are different things**, and the errand
        // follows the second. Heard of through a save flag, `start` has no edges at all, so naming
        // it as the target would only have handed the move to `next_hop`'s fallback while the log
        // claimed an objective. Explore, which is what mends the gap.
        assert_eq!(
            m.next_target().unwrap().reason,
            Goal::RouteTo(Box::new(Goal::CloseAnomaly)),
            "no route, so no nomination — but the errand it stands for is still on the record"
        );

        // Learn one edge to it and it becomes the errand, with nothing else about the map changed.
        m.fold(&dump("l29", "Rookdale — level 3 crypt", vec![node("start", "")]));
        m.here = Some("l39".into());
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::CloseAnomaly);
        assert_eq!(plan.target, "start");
    }

    #[test]
    fn the_anomaly_itself_outranks_everything() {
        let mut m = WorldMap::new();
        ready_for_the_anomaly(&mut m);
        m.fold(&dump(
            "start",
            "camp",
            vec![node("l4", "Grim Barrow — level 4 crypt"), node("hp", "The Rift anomaly")],
        ));
        // There is no portal to go to until the anomaly is actually open.
        assert!(m.anomaly().is_none());
        m.hell = Some(0.1);
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::CloseAnomaly);
        assert_eq!(plan.target, "hp", "a named portal beats the `start` prior");
    }

    #[test]
    fn exploration_prefers_unvisited_then_the_most_hidden() {
        let mut m = WorldMap::new();
        let mut a = dump("start", "camp", vec![node("l1", "a meadow"), node("l2", "b meadow")]);
        a.hidden = 0;
        m.fold(&a);
        // Visit l1 and find it still hiding two neighbours.
        let mut b = dump("l1", "a meadow", vec![]);
        b.hidden = 2;
        m.fold(&b);
        // l2 is unvisited, so it wins over l1 despite l1's hidden neighbours.
        m.here = Some("l1".into());
        let plan = m.next_target().unwrap();
        assert_eq!(plan.target, "l2");
        assert_eq!(plan.reason, Goal::Explore);
    }

    #[test]
    fn we_never_propose_travelling_to_where_we_already_are() {
        let mut m = WorldMap::new();
        let mut a = dump("l4", "Grim Barrow — level 4 crypt", vec![]);
        a.hidden = 1;
        m.fold(&a);
        // Standing on the only trigger node, the plan must not be to travel to it.
        assert_ne!(m.next_target().map(|p| p.target), Some("l4".into()));
    }

    #[test]
    fn a_hop_steps_toward_a_distant_target_rather_than_naming_it() {
        // start — l1 — l4(level 4). Standing at start, the move is to l1, but the PLAN is still
        // the trigger node: where the map does not accumulate we could not select l4 at all, and
        // even elsewhere it may be off-screen.
        let mut m = WorldMap::new();
        m.fold(&dump("start", "camp", vec![node("l1", "a crypt — level 0 crypt")]));
        m.fold(&dump(
            "l1",
            "a crypt — level 0 crypt",
            vec![node("start", "camp"), node("l4", "Grim Barrow — level 4 crypt")],
        ));
        m.here = Some("start".into());
        let hop = m.next_hop().unwrap();
        assert_eq!(hop.step, "l1", "step to the neighbour on the way");
        assert_eq!(hop.plan.target, "l4");
        assert_eq!(hop.plan.reason, Goal::OpenAnomaly);
    }

    #[test]
    fn an_adjacent_target_is_stepped_to_directly() {
        let mut m = WorldMap::new();
        m.fold(&dump("start", "camp", vec![node("l4", "Grim Barrow — level 4 crypt")]));
        let hop = m.next_hop().unwrap();
        assert_eq!(hop.step, "l4");
        assert_eq!(hop.plan.target, "l4");
        assert!(hop.routed, "a step we can justify by an edge");
    }

    #[test]
    fn with_no_known_route_we_step_to_an_adjacent_frontier() {
        // The target was heard about from somewhere else and no path to it is mapped. Standing
        // still would be the wrong answer; stepping outward is what mends the gap.
        let mut m = WorldMap::new();
        m.fold(&dump("far", "elsewhere", vec![node("l4", "Grim Barrow — level 4 crypt")]));
        let mut here = dump("start", "camp", vec![node("l2", "Quiet Glade meadow")]);
        here.hidden = 1;
        m.fold(&here);
        m.here = Some("start".into());
        let hop = m.next_hop().unwrap();
        assert_eq!(hop.step, "l2", "the move is the only way onward we know");

        // **The move is unchanged and the reason for it is not**, which is the whole of #24. `l4`
        // is a level 4 trigger we cannot reach, so it stops being what we claim to be doing;
        // `next_hop`'s any-frontier fallback used to supply this same step under a goal that was
        // not being pursued. Now the goal is the step.
        // Since 2026-08-16 the errand is exploring rather than routing to the trigger: `l4` is a
        // level 4 node and `l2` is not, so the trigger is queued behind the gentler ground rather
        // than declined for want of a route. The move and the honesty about it are unchanged.
        assert_eq!(hop.plan.reason, Goal::Explore);
        assert_eq!(hop.plan.target, "l2", "we say where we are actually going");
    }

    #[test]
    fn a_known_lost_woods_is_routed_around_even_though_it_is_shorter() {
        let mut m = two_routes();
        // Before we know better, the short way through the woods wins.
        assert_eq!(m.next_hop().unwrap().step, "woods");
        m.entry("woods").avoid = true;
        let hop = m.next_hop().unwrap();
        assert_eq!(hop.step, "a", "go the long way rather than back into the woods");
        assert_eq!(hop.plan.target, "goal", "the goal is unchanged");
    }

    #[test]
    fn a_lost_woods_is_still_used_when_it_is_the_only_way() {
        let mut m = WorldMap::new();
        m.fold(&dump("start", "camp", vec![node("woods", "Mistwood forest")]));
        m.fold(&dump("woods", "Mistwood forest", vec![node("start", "camp"), node("goal", "Grim Barrow — level 4 crypt")]));
        m.here = Some("start".into());
        m.entry("woods").avoid = true;
        // Being swallowed again is bad; standing still forever is worse.
        assert_eq!(m.next_hop().unwrap().step, "woods");
    }

    #[test]
    fn an_avoided_place_is_never_chosen_as_a_destination() {
        let mut m = WorldMap::new();
        let mut d = dump("start", "camp", vec![node("woods", "Mistwood forest"), node("a", "a meadow")]);
        d.hidden = 0;
        m.fold(&d);
        m.entry("woods").avoid = true;
        let plan = m.next_target().unwrap();
        assert_eq!(plan.target, "a", "exploring INTO a known lost woods is never the plan");
    }

    #[test]
    fn losing_four_health_sends_us_to_rest_before_exploring() {
        let m = hurt_at_l1();
        assert!(m.wants_rest());
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Rest);
        // The village. The campfire is nearer and free and would be the better answer, but arriving
        // at one does nothing yet — see `rest::CAMPFIRE_REST_IS_BUILT`.
        assert_eq!(plan.target, "l10", "the inn, while the campfire press is unwritten");
    }

    /// **The dev's question, reproduced.** *"After we completed the level 7 crypt, why did the
    /// navigator choose such a distant village to rest at instead of the ones we bought healthBuffs
    /// from?"*
    ///
    /// Because two inns of equal rank were separated by `pa.key.cmp(&pb.key)` — alphabetically —
    /// and `l11` sorts before `l19`. The map here is the one from
    /// `spike-run-20260815-1913Z.md` steps 49-53, cut down to the part that decides: standing at
    /// `l1`, with `l19` two hops away through `l9` and `l11` four hops away through `l9`, `l10` and
    /// `l4`.
    ///
    /// The second assertion is the positive control, and without it this test proves nothing: it
    /// pins the fact that the two candidates really are in the order that used to lose, so a fix
    /// that quietly stopped offering `l11` at all would not pass by accident.
    #[test]
    fn the_nearer_of_two_inns_wins_however_the_keys_happen_to_sort() {
        let mut m = WorldMap::new();
        m.fold(&dump("l1", "Weedley Copse crypt", vec![node("l9", "Kelk Wold — level 2 forest")]));
        m.fold(&dump(
            "l9",
            "Kelk Wold — level 2 forest",
            vec![
                node("l1", "Weedley Copse crypt"),
                node("l19", "Dane village"),
                node("l10", "Ulrome — level 3 forest"),
            ],
        ));
        m.fold(&dump("l10", "Ulrome — level 3 forest", vec![node("l4", "Bainton Clump — level 1 forest")]));
        m.fold(&dump("l4", "Bainton Clump — level 1 forest", vec![node("l11", "Rowlston Covert village")]));
        // Back where the run was when it chose, and hurt enough to want a bed.
        m.fold(&dump("l1", "Weedley Copse crypt", vec![node("l9", "Kelk Wold — level 2 forest")]));
        m.gold = 50;
        m.note_health(
            crate::rest::Health { current: 20, max: 20 },
            crate::rest::Health { current: 9, max: 20 },
        );
        assert!(m.wants_rest());

        assert!(
            "l11" < "l19",
            "the control: the far inn is the one an alphabetical tie-break would pick"
        );
        let plan = m.next_target().expect("a bed should be planned");
        assert_eq!(plan.reason, Goal::Rest);
        assert_eq!(plan.target, "l19", "two hops beats four, whatever the keys say");
    }

    #[test]
    fn a_corrupted_village_is_not_a_rest_stop() {
        // Met live: `Ulrome — level 6 village [corrupted]`. The heading still says "village", so
        // without the corruption check the run walks to an inn that is under attack or destroyed.
        let mut m = hurt_at_l1();
        m.entry("start").used = true; // the campfire is spent, so only the village is left
        m.gold = 50;
        assert_eq!(m.next_target().unwrap().target, "l10");
        m.entry("l10").corrupted = true;
        assert_ne!(m.next_target().unwrap().reason, Goal::Rest, "its inn is overrun");

        // Locked, not lost: fighting the corruption out frees the inn again.
        m.entry("l10").completed = true;
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Rest);
        assert_eq!(plan.target, "l10", "a freed inn is a rest stop again");
    }

    /// A village under attack, or lost, is neither a bed nor a shop.
    ///
    /// The dev, 2026-08-16: *the inn at a village that is under attack or lost cannot be rested at.*
    /// `getAreaButtons` (`overworld/generators/village.lua:84-98`) swaps the button set for every
    /// building in the village at once, from two `areaFlags` and nothing else.
    ///
    /// The under-attack case is the one worth a test rather than a comment: `Enter` still reads
    /// `Enter` and opens `ui.building_empty` instead of `ui.inn` (`:371-388`). A driver that ignored
    /// this would cross the village, press a button, succeed, and rest nothing — which is the
    /// campfire stall again with a working button in place of a missing one.
    #[test]
    fn a_village_under_attack_or_lost_is_not_somewhere_to_rest_or_shop() {
        let village = |flag: &str| {
            let mut m = WorldMap::new();
            m.fold(&dump(
                "l1",
                "Weedley Copse crypt",
                vec![node("l10", "Ulrome village"), node("l11", "Rowlston village")],
            ));
            m.note_health(
                crate::rest::Health { current: 12, max: 12 },
                crate::rest::Health { current: 7, max: 12 },
            );
            m.gold = 200;
            if !flag.is_empty() {
                m.apply_save(
                    &crate::game::save::parse(&format!(
                        "return {{ player = {{ gold = 200 }}, overworld = {{
                             areaFlags = {{ hell = 0, {flag} }} }} }}"
                    ))
                    .unwrap(),
                );
            }
            m
        };

        // Untouched, `l10` is the nearer bed and the planner says so.
        assert_eq!(village("").next_target().unwrap().target, "l10", "the premise");

        // `l10_attack` — villagers are fighting for it right now. `Enter` opens an empty room.
        let besieged = village("l10_attack = { attackingEnemies = 3 }");
        assert!(besieged.get("l10").unwrap().under_attack);
        assert!(!besieged.get("l10").unwrap().trades(), "an empty room is not an inn");
        let plan = besieged.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Rest);
        assert_eq!(plan.target, "l11", "the other village, which still has a bed");

        // `l10_attacked` — it was lost. Only `Loot` is left, and that is #26, not a rest.
        let sacked = village("l10_attacked = 4");
        assert!(sacked.get("l10").unwrap().sacked);
        assert_eq!(sacked.next_target().unwrap().target, "l11", "a sacked village is not a bed");

        // And the shelf goes with the bed, because the general store carries the same three button
        // sets (`village.lua:323`, `:332`).
        assert!(!sacked.get("l10").unwrap().stocks_a_heart(), "nor a shop");
        assert!(village("").get("l10").unwrap().stocks_a_heart(), "the control");
    }

    /// A campfire is never the rest destination, in any of the states that used to choose it.
    ///
    /// This test used to be about *cost* — a used campfire with no firewood restores nothing, so pay
    /// for the inn instead; carry firewood and the free one wins again. Both readings were right and
    /// both are now beside the point, because arriving at a campfire does nothing at all:
    /// `rest::CAMPFIRE_REST_IS_BUILT` is false and the driver has no handler to press.
    ///
    /// The dev, 2026-08-16, after a run walked to `l7` for a rest and straight back out again: *the
    /// campfire stalled the run; we can sidestep this by not trying to rest there.* Kept as a test
    /// rather than deleted so that turning the constant back on has something to fail against.
    #[test]
    fn a_campfire_is_never_the_rest_destination_while_the_press_is_unwritten() {
        let mut m = hurt_at_l1();
        m.entry("start").used = true;
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Rest);
        assert_eq!(plan.target, "l10", "the inn is the only rest we can actually take");

        // Firewood used to put it back on the table and outrank paying. It no longer does.
        m.fuel = 2;
        assert_eq!(m.next_target().unwrap().target, "l10", "fuel does not build the press");

        // Nor does an untouched one, which is the free-and-nearest case and the most tempting.
        m.entry("start").used = false;
        assert_eq!(m.next_target().unwrap().target, "l10", "and neither does a fresh campfire");
    }

    #[test]
    fn with_nothing_usable_resting_is_not_a_plan_at_all() {
        let mut m = hurt_at_l1();
        m.entry("start").used = true;
        m.gold = 0;
        // Used campfire, no fuel, no gold: better to get on with exploring than walk somewhere
        // that will turn us away.
        assert_ne!(m.next_target().unwrap().reason, Goal::Rest);
    }

    #[test]
    fn a_village_is_not_a_plan_without_the_ten_gold() {
        let mut m = hurt_at_l1();
        // Strip the campfire so only the village remains, then arrive skint.
        m.places.remove("start");
        m.gold = 9;
        let plan = m.next_target().unwrap();
        assert_ne!(plan.reason, Goal::Rest, "nine gold buys a wasted walk and no health");
        m.gold = 10;
        assert_eq!(m.next_target().unwrap().reason, Goal::Rest);
    }

    #[test]
    fn healing_up_clears_the_intent() {
        let mut m = hurt_at_l1();
        m.rested(crate::rest::Health { current: 12, max: 12 });
        assert!(!m.wants_rest());
        assert_ne!(m.next_target().unwrap().reason, Goal::Rest);
    }

    /// **Reverses `the_anomaly_still_outranks_resting`**, on the user's instruction: resting is the
    /// highest priority outside combat whenever health is low.
    ///
    /// The old assertion read "health is worth a detour, but not at the cost of the objective",
    /// which is the wrong trade in this game. The anomaly does not expire and health does not come
    /// back on its own, so the detour costs only time — while arriving at a level 8 fight on a third
    /// health costs the save. Kept as an explicit test rather than deleted, so the reversal is
    /// visible to whoever reads this next.
    #[test]
    fn resting_outranks_even_a_live_anomaly() {
        let mut m = hurt_at_l1();
        m.fold(&dump("l1", "Weedley Copse crypt", vec![node("hp", "The Rift anomaly")]));
        m.hell = Some(0.1);
        assert_eq!(m.next_target().unwrap().reason, Goal::Rest);
    }

    /// The other half of that trade: with nowhere to rest we still move. But we move to the
    /// **cheapest fight**, not to the objective — and specifically NOT to the anomaly, whose heading
    /// carries no level and so once sorted as the gentlest thing on the map.
    #[test]
    fn with_nowhere_safe_we_take_the_cheapest_fight_not_the_anomaly() {
        let mut m = hurt_at_l1();
        m.fold(&dump("l1", "Weedley Copse crypt", vec![node("hp", "The Rift anomaly")]));
        m.hell = Some(0.1);
        m.gold = 0; // no inn will serve us
        m.fuel = 0; // and no campfire fuel
        for p in m.places.values_mut() {
            p.corrupted = true; // every village locked behind its own clearing
            p.completed = false;
            p.visited = true;
            p.hidden = Some(0); // and nothing left to explore
        }
        let plan = m.next_target().unwrap();
        assert_ne!(plan.target, "hp", "the anomaly is the one fight we must not pick as 'easiest'");
        assert!(matches!(plan.reason, Goal::EasiestHostile { .. }), "got {:?}", plan.reason);

        // Once health is back, the anomaly is the objective again.
        m.rested(crate::rest::Health { current: 12, max: 12 });
        assert_eq!(m.next_target().unwrap().reason, Goal::CloseAnomaly);
    }

    /// Hurt, and the nearest anomaly trigger is an uncleared corrupted village: prefer the other
    /// one, because getting back out of the corrupted one costs a fight.
    #[test]
    fn while_hurt_a_hostile_subworld_is_not_the_way_to_open_the_anomaly() {
        let mut m = hurt_at_l1();
        m.hell = Some(0.0); // anomaly still to be opened
        // Take resting off the table, so the planner reaches the branch under test rather than
        // simply walking to the campfire the fixture provides.
        for p in m.places.values_mut() {
            p.used = true;
        }
        m.gold = 0;
        m.fuel = 0;
        m.fold(&dump(
            "l1",
            "Weedley Copse crypt",
            vec![node("v1", "Ulrome — level 6 village"), node("c1", "Rookdale — level 9 crypt")],
        ));
        {
            let v = m.entry("v1");
            v.corrupted = true;
            v.completed = false;
        }
        // Both trigger the anomaly (level > 3), and while hurt we take NEITHER. The crypt used to be
        // chosen here, on the reading that only the corrupted village was dangerous -- which was the
        // whole bug: a level 9 crypt is not a safe consolation prize, it is the worse fight of the
        // two. Exploring is preferred while any free frontier remains, because a frontier might hold
        // the rest site that fixes the actual problem.
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Explore);
        assert_ne!(plan.target, "v1");
        assert_ne!(plan.target, "c1");

        // With the map fully explored there is no such escape, and the choice moves to the
        // cheapest-fight ranking. It takes neither of the two anomaly triggers: the fixture's `l4`
        // is a level 1 forest, and a level 1 anything is a better place to bleed than a level 6
        // village or a level 9 crypt.
        for p in m.places.values_mut() {
            p.visited = true;
            p.hidden = Some(0);
        }
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::EasiestHostile { level: Some(1) });
        assert_eq!(plan.target, "l4");

        // And the original claim, asserted where it actually lives now. Between the two triggers the
        // corrupted village still loses -- `Risk::Corrupt` is the worst rung there is, so the crypt
        // wins despite being level 9 to the village's 6.
        assert!(m.get("c1").unwrap().risk() < m.get("v1").unwrap().risk());
    }

    /// ...but with genuinely nothing else to do, being hurt must not stop the run entirely.
    ///
    /// Note what "nothing else" has to mean now that the rule is global: exploring counts as
    /// something else, and is *preferred*, because a frontier might hold the rest site we are
    /// looking for. So this fixture leaves no frontier at all — every place visited, nothing
    /// hidden — which is the only state in which diving into hostile ground is the best move left.
    #[test]
    fn a_hostile_trigger_is_still_taken_when_it_is_the_only_one() {
        let mut m = hurt_at_l1();
        m.hell = Some(0.0);
        m.fold(&dump("l1", "Weedley Copse crypt", vec![node("v1", "Ulrome — level 6 village")]));
        // No rest anywhere, and every other neighbour too small to trigger the anomaly.
        for p in m.places.values_mut() {
            p.used = true;
            p.visited = true;
            p.hidden = Some(0); // no frontier left, so exploring is not an option
        }
        m.gold = 0;
        m.fuel = 0;
        {
            let v = m.entry("v1");
            v.corrupted = true;
            v.completed = false;
        }
        let plan = m.next_target().unwrap();
        assert_eq!(
            plan.reason,
            Goal::EasiestHostile { level: Some(1) },
            "with nowhere safe, the reason is 'cheapest fight', not 'objective'"
        );
        // `l4` is `Bainton Clump — level 1 forest` from the fixture, and it only became a candidate
        // when the filter started asking the arrival question: it is visited, so `hostile_to_enter`
        // says nothing about it, but its heading carries a level so a fight is still owed. Taking a
        // level 1 forest over a corrupted level 6 village is the point of the whole ranking.
        assert_eq!(plan.target, "l4", "a preference must not strand the run, nor pick the worst fight");
    }

    #[test]
    fn the_two_passes_between_them_consider_every_place() {
        // `plan(true)` and `easiest_hostile` must tile the map: a place refused by both is a place
        // the planner can never reach, and a run that found only those would stop with `NoPlan`
        // while somewhere perfectly reachable sat on the map.
        //
        // Exercised rather than asserted about, by building a map on which EVERY place owes a fight
        // -- so the first pass has nothing at all and the second has to carry all of it.
        let mut m = WorldMap::new();
        m.fold(&dump(
            "here",
            "Somewhere crossroads",
            vec![
                node("c1", "Yokefleet — level 6 crypt"),
                node("f1", "Asselby Bush — level 4 forest"),
                node("v1", "Ulrome — level 6 village"),
            ],
        ));
        m.entry("v1").corrupted = true;
        m.entry("unheaded").key = "unheaded".into(); // known by key only, so `Arrival::Unknown`
        m.note_health_level(crate::rest::Health { current: 1, max: 20 });
        for p in m.places.values_mut() {
            p.used = true; // no rest available anywhere
        }
        assert!(
            m.places.values().filter(|p| p.key != "here").all(WorldMap::owes_a_fight),
            "fixture must leave the first pass nothing"
        );
        let plan = m.next_target().expect("the second pass must catch what the first refused");
        assert_eq!(plan.reason, Goal::EasiestHostile { level: Some(4) });
        assert_eq!(plan.target, "f1", "the forest, over two level 6s and an unknown");
    }

    /// The live failure of 2026-08-08, rebuilt from the map that run printed.
    #[test]
    fn a_free_shrine_beats_exploring_onto_a_level_6_crypt() {
        let mut m = WorldMap::new();
        m.fold(&dump(
            "l41",
            "Grimston crossroads",
            vec![
                node("l50", "Burtonfields — level 6 crypt"),
                node("shrine5", "Gembling shrine"),
            ],
        ));
        // 1/20 -- below half, so `health_is_low` sets the intent however we arrived.
        m.note_health_level(crate::rest::Health { current: 1, max: 20 });
        assert!(m.wants_rest());
        // No campfire and no village anywhere, which was the real map's situation: of 22 places, not
        // one was a rest site. So the `Rest` branch finds nothing and falls through -- and what it
        // falls through to used to be `Explore`, which had no danger term at all and picked the
        // crypt. The run walked onto it at 1/20 and stopped with `TooHurtToFight`.
        let plan = m.next_target().unwrap();
        assert_ne!(plan.target, "l50", "a level 6 crypt is not an exploration");
        assert_eq!(plan.target, "shrine5");
        // A shrine, not merely a safe square: `Gembling shrine` has no level, so arriving is free,
        // and an uncorrupted shrine pays in wildcard tiles -- which is what a hurt run wants most.
        assert_eq!(plan.reason, Goal::Shrine);
    }

    /// Hurt, nowhere to rest, and an **uncorrupted** shrine within reach: take the shrine, not the
    /// cheapest fight.
    ///
    /// It costs no fight and pays in wildcard tiles (`utils/blessings.lua:95-110`), which is the one
    /// thing that makes the *next* fight winnable. A hurt run choosing a level 1 skirmish over a
    /// free blessing would have the trade exactly backwards.
    #[test]
    fn an_uncorrupted_shrine_beats_the_cheapest_fight_while_hurt() {
        let mut m = hurt_at_l1();
        m.hell = Some(0.1); // anomaly open, so consecrating is possible
        m.fold(&dump(
            "l1",
            "Weedley Copse crypt",
            vec![node("shrine2", "Gransmoor shrine"), node("c1", "Rookdale — level 2 crypt")],
        ));
        // No rest anywhere, and nothing left to explore.
        for p in m.places.values_mut() {
            p.used = true;
            p.visited = true;
            p.hidden = Some(0);
        }
        m.gold = 0;
        m.fuel = 0;
        // Everything hostile EXCEPT the shrine.
        for p in m.places.values_mut() {
            if p.key != "shrine2" {
                p.corrupted = true;
                p.completed = false;
            }
        }
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Shrine);
        assert_eq!(plan.target, "shrine2");
    }

    /// The anomaly must never be picked as the "easiest" fight. Its heading has no `— level N`, so
    /// a naive `unwrap_or(0)` ranked the hardest fight on the map as the gentlest.
    #[test]
    fn the_anomaly_is_never_the_cheapest_fight() {
        let mut m = hurt_at_l1();
        m.hell = Some(0.1);
        m.fold(&dump(
            "l1",
            "Weedley Copse crypt",
            vec![node("hp", "The Rift anomaly"), node("c1", "Rookdale — level 9 crypt")],
        ));
        for p in m.places.values_mut() {
            p.corrupted = true;
            p.completed = false;
            p.used = true;
            p.visited = true;
            p.hidden = Some(0);
        }
        m.gold = 0;
        m.fuel = 0;
        let plan = m.next_target().unwrap();
        assert_ne!(plan.target, "hp", "the anomaly must never rank as the cheapest fight");
        // The fixture's `l4` is a level 1 forest, which is genuinely the cheapest thing here.
        assert_eq!(plan.reason, Goal::EasiestHostile { level: Some(1) });
        assert_eq!(plan.target, "l4");
    }

    #[test]
    fn an_unconsecrated_shrine_is_worth_the_detour() {
        // Real heading from the captured island.
        let mut m = WorldMap::new();
        m.fold(&dump(
            "l19",
            "Gipsyville — level 2 crypt",
            vec![node("shrine2", "Gransmoor shrine"), node("l29", "Rookdale — level 3 crypt")],
        ));
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Shrine);
        assert_eq!(plan.target, "shrine2");

        // Once there is nothing left to do there, it stops pulling us off course.
        //
        // This used to set `consecrated` alone, and that was not enough of a condition — it only
        // looked sufficient while `Pray` was unimplemented. Consecrating and praying are separate
        // actions with separate gates, and `showPrayButton` (`shrine.lua:98-102`) retires on
        // `areaUnused`, so a consecrated shrine nobody has prayed at still has a blessing waiting.
        m.entry("shrine2").consecrated = true;
        m.entry("shrine2").used = true;
        assert_ne!(m.next_target().unwrap().reason, Goal::Shrine);
    }

    #[test]
    fn a_prayed_shrine_stops_being_a_destination_while_the_anomaly_is_shut() {
        // The regression for a live stall. With `hell = 0` nothing can be consecrated, so a test of
        // `!consecrated` can never be discharged and the shrine stays "outstanding" forever. Praying
        // is the only thing available, and once it is done there is genuinely nothing left there.
        let mut m = WorldMap::new();
        m.fold(&dump(
            "shrine1",
            "Swanland shrine",
            vec![node("l10", "Trenwick — level 1 crypt")],
        ));
        m.entry("shrine1").completed = true;
        m.fold(&dump("l10", "Trenwick — level 1 crypt", vec![node("shrine2", "Foggathorpe shrine")]));

        // Standing at l10, the unprayed shrine1 next door is a fair target.
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Shrine);
        assert_eq!(plan.target, "shrine1");

        // Having prayed there, it must not pull us back.
        m.entry("shrine1").used = true;
        let plan = m.next_target().unwrap();
        assert_ne!(plan.target, "shrine1", "a prayed shrine is finished while hell == 0");
    }

    #[test]
    fn a_prayed_shrine_is_worth_returning_to_once_the_anomaly_opens() {
        // The other half: `used` retires `Pray`, not `Consecrate`. Once `hell ~= 0` the same shrine
        // has work again, and writing it off permanently would forfeit it.
        let mut m = WorldMap::new();
        m.fold(&dump("l10", "Trenwick — level 1 crypt", vec![node("shrine1", "Swanland shrine")]));
        m.entry("shrine1").completed = true;
        m.entry("shrine1").used = true;
        // Asserted on the *reason*, not the target: an unvisited shrine is still a frontier, so
        // exploration may legitimately route through it. What must not happen is going there
        // *because it is a shrine*, which is the claim that there is shrine work to do.
        assert_ne!(m.next_target().unwrap().reason, Goal::Shrine, "nothing to do there yet");

        // `hell ~= 0` is the anomaly being open, which is what unlocks consecration.
        m.hell = Some(0.1);
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Shrine);
        assert_eq!(plan.target, "shrine1", "consecrating is now possible");
    }

    #[test]
    fn two_finished_shrines_either_side_of_a_crypt_do_not_ping_pong() {
        // The stall exactly as it was met: shrine1 and shrine2 with l10 between them, the run
        // walking shrine1 -> l10 -> shrine1 -> l10 for twenty steps. The crypt was never fought
        // because it was only ever a waypoint, and stepping *back* to a completed shrine is always
        // legal, so departure was never blocked.
        let mut m = WorldMap::new();
        m.fold(&dump("shrine1", "Swanland shrine", vec![node("l10", "Trenwick — level 1 crypt")]));
        m.fold(&dump(
            "l10",
            "Trenwick — level 1 crypt",
            vec![node("shrine1", "Swanland shrine"), node("shrine2", "Foggathorpe shrine")],
        ));
        for k in ["shrine1", "shrine2"] {
            m.entry(k).completed = true;
            m.entry(k).used = true;
            // Both have been stood on, so neither is a frontier and exploration has no reason to
            // route through them either. That isolates the claim to the shrine branch.
            m.entry(k).visited = true;
        }
        // Standing on the crypt, with both shrines finished, nothing may send us back to one.
        let plan = m.next_target();
        if let Some(plan) = plan {
            assert_ne!(plan.reason, Goal::Shrine, "still treating a finished shrine as work");
            assert!(
                !plan.target.starts_with("shrine"),
                "went back to a finished shrine: {plan:?}"
            );
        }
    }

    #[test]
    fn a_shrine_we_walked_away_from_stops_being_a_destination() {
        // The second bounce, which `worth_a_trip` could not catch. shrine2's puzzle could not be
        // read, so the run left without praying — and because nothing was prayed, the game never set
        // `_used`. The shrine is therefore genuinely unused, genuinely nearest, and genuinely one the
        // driver will never enter again. The live run walked `l10 -> shrine2 -> l10` for thirty
        // steps until its budget ran out.
        let mut m = WorldMap::new();
        m.fold(&dump(
            "l10",
            "Trenwick — level 1 crypt",
            vec![node("shrine2", "Foggathorpe shrine"), node("shrine3", "Skipsea shrine")],
        ));
        for k in ["shrine2", "shrine3"] {
            m.entry(k).completed = true;
            m.entry(k).visited = true;
        }
        // Standing on the crypt: with nothing abandoned, the nearest unused shrine is fair game.
        assert_eq!(m.next_target().map(|p| p.reason), Some(Goal::Shrine));

        m.abandon("shrine2");
        let plan = m.next_target().expect("shrine3 is still worth the trip");
        assert_eq!(plan.reason, Goal::Shrine);
        assert_eq!(plan.target, "shrine3", "kept choosing the shrine it refuses to enter");

        // And with both spent, the shrine branch must yield entirely rather than pick the least-bad.
        m.abandon("shrine3");
        if let Some(plan) = m.next_target() {
            assert_ne!(plan.reason, Goal::Shrine, "no shrine is left to visit: {plan:?}");
        }
    }

    /// Resuming a save: prefer ground we have not finished over ground we have, even if it is further.
    ///
    /// `visited` is set only by standing somewhere *this run*, and nothing seeds it from the save.
    /// So on a resume the first sort key ties for every place on the map and distance used to decide
    /// everything — which sent a resumed run back over its own cleared nodes because they were
    /// nearer. `completed` is the half we do load, so it is what carries the knowledge across.
    #[test]
    fn a_resumed_run_prefers_unfinished_ground_to_nearer_finished_ground() {
        let mut m = WorldMap::new();
        // `near` is one hop away and `far` is two, so distance and completion genuinely disagree.
        // An earlier version of this test put both at one hop, where the two orderings give the
        // same answer — it passed against the code it was written to catch.
        m.fold(&dump("here", "Somewhere", vec![node("near", "Nearby crossroads")]));
        m.entry("far").heading = "Distant crossroads".into();
        m.entry("near").neighbours.insert("far".into());
        m.entry("far").neighbours.insert("near".into());
        // One hop and two, at the uncleared-ground price every edge in this fixture carries.
        assert_eq!(m.distances("here").get("near"), Some(&CROSSING));
        assert_eq!(m.distances("here").get("far"), Some(&(2 * CROSSING)));
        // Nothing has been stood on this run but `here`: that is what a resume looks like.
        assert!(!m.get("near").unwrap().visited && !m.get("far").unwrap().visited);
        // What the save told us, and the only thing that separates them.
        m.entry("near").completed = true;

        let target = m.next_target().expect("something to explore").target;
        assert_eq!(target, "far", "cleared ground is not worth walking to first");
    }

    /// With nothing at all we can route to, aim at the unreachable target and step the way it lies.
    ///
    /// This is the branch coordinates were wanted for, and #24 narrowed rather than removed it:
    /// something routable now wins the nomination outright, so the fallback is reached only when
    /// *nothing* is. The choice among adjacent nodes used to come down to the lowest key, so a run
    /// heading for the anomaly stepped whichever way the alphabet pointed.
    ///
    /// **The earlier fixture for this could not fail.** It reached `start` through a second dump
    /// that shared a node with the first, and `fold` records edges both ways — so a route existed,
    /// `first_step_toward` answered `west` on its own, and the bearing was never consulted. Two
    /// leaves either side of a crossroads and an anomaly known only from a save flag is the real
    /// shape of it.
    #[test]
    fn with_no_route_we_step_the_way_the_target_lies() {
        let mut m = WorldMap::new();
        ready_for_the_anomaly(&mut m);
        // Two leaves. Nothing left to reveal at either, so the planner will not explore to them --
        // and they are the only two moves there are.
        m.fold(&dump("here", "The Wold crossroads", vec![
            Node { key: "west".into(), heading: "West Field".into(),
                   x: -100.0, y: 0.0, connections: 1 },
            Node { key: "east".into(), heading: "East Field".into(),
                   x: 100.0, y: 0.0, connections: 1 },
        ]));
        // The anomaly as a live run meets it: heard of through `start_first_corrupt_time`, with no
        // heading and no edges, so no route. Its bearing comes from the corrupted blob, which is
        // west of here.
        m.apply_save(&crate::game::save::parse(
            "return { overworld = { areaFlags = {
                 hell = 0.1, start_first_corrupt_time = 12, west_first_corrupt_time = 12,
             } } }",
        ).unwrap());

        assert!(!m.can_route_to("start"), "the fixture must actually have no route");
        let hop = m.next_hop().expect("a step");
        assert_eq!(hop.plan.reason, Goal::CloseAnomaly, "nothing routable, so aim at it anyway");
        assert_eq!(hop.plan.target, "start");
        assert!(m.gap("west", "start").unwrap() < m.gap("east", "start").unwrap());
        assert_eq!(hop.step, "west", "toward the anomaly, not the lower key");
        assert!(!hop.routed, "and the log has to say so");
    }

    /// **The detour itself**, which is what #16 is named for.
    ///
    /// The dev's rule: *when the goal is not rest and a chest is visible, detour to it, open it,
    /// then rejoin the path.* Both conditions are asserted here, because the leaf exception below
    /// only makes a chest *reachable* — on its own it would leave a chest unvisited whenever
    /// something else was the errand, which is every step after the portal opens.
    #[test]
    fn an_unopened_chest_earns_a_detour_unless_we_are_hurt() {
        let build = || {
            let mut m = WorldMap::new();
            m.fold(&dump(
                "here",
                "camp",
                vec![node("l5", "Riccall chest"), node("l6", "Bainton Clump — level 1 forest")],
            ));
            m.here = Some("here".into());
            m
        };

        let m = build();
        let plan = m.next_target().expect("a plan");
        assert_eq!(plan.reason, Goal::Chest);
        assert_eq!(plan.target, "l5");

        // Hurt, and the chest is a fight: `Open` starts a combat scenario, so the bed comes first.
        let mut hurt = build();
        hurt.gold = crate::rest::INN_COST;
        hurt.fold(&dump("here", "camp", vec![node("l7", "Greenoak campfire")]));
        hurt.fuel = 1;
        hurt.note_health_level(crate::rest::Health { current: 3, max: 20 });
        assert!(hurt.wants_rest());
        assert_ne!(hurt.next_target().expect("a plan").reason, Goal::Chest, "a fight is not a rest");

        // And an opened one is not a destination at all.
        let mut done = build();
        done.entry("l5").completed = true;
        assert_ne!(done.next_target().expect("a plan").reason, Goal::Chest);
    }

    /// A heart before the anomaly, and every clause of the dev's rule.
    ///
    /// Given the evening a run died going the right way: two level 6 corrupted crypts back to back,
    /// and the second ran the board dry against a five-deep queue. Four maximum health for a hundred
    /// gold is the cheapest preparation there is, and the window is while the road is still free.
    #[test]
    fn a_heart_is_worth_the_walk_when_the_road_to_it_is_free() {
        let village = |k: &str| node(k, "Rowlston Covert village");
        let build = || {
            let mut m = WorldMap::new();
            m.fold(&dump("here", "camp", vec![village("l11"), node("l4", "Riccall — level 6 crypt")]));
            m.here = Some("here".into());
            m.hell = Some(0.1);
            m.gold = HEART_FLOOR;
            m
        };

        let m = build();
        let plan = m.next_target().expect("a plan");
        assert_eq!(plan.reason, Goal::Heart, "the anomaly is open, the gold is there, the road is free");
        assert_eq!(plan.target, "l11");

        // **And with the portal shut**, which is the dev's correction of 2026-08-16. The clause
        // used to be "the goal must already be the anomaly", read as "the anomaly is open" — and
        // the fights that open it are the level 6 ones this rule was written after.
        let mut shut = build();
        shut.hell = Some(0.0);
        assert!(!shut.anomaly_is_open().unwrap_or(true));
        let plan = shut.next_target().expect("a plan");
        assert_eq!(plan.reason, Goal::Heart, "the heart does not wait for the portal");
        assert_eq!(plan.target, "l11");

        // A pound short and it is not a plan. **The bar is the price plus a bed**, not the price:
        // the dev raised it to 110 on 2026-08-15 after watching the errand spend a run down to
        // nothing, which trades four maximum health for the six-a-press that keeps it alive.
        let mut m = build();
        m.gold = HEART_FLOOR - 1;
        assert_ne!(m.next_target().unwrap().reason, Goal::Heart, "the price alone is not enough");
        assert_eq!(HEART_FLOOR, HEART_COST + crate::rest::INN_COST, "the reserve is exactly one night");

        // A fight on the way and it is not a detour, it is the fight.
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("l4", "Riccall — level 6 crypt")]));
        m.fold(&dump("l4", "Riccall — level 6 crypt", vec![village("l11")]));
        m.here = Some("here".into());
        m.hell = Some(0.1);
        m.gold = HEART_FLOOR;
        assert_ne!(m.next_target().unwrap().reason, Goal::Heart, "the crypt is in the way");

        // And a village we have already emptied is not a destination.
        let mut m = build();
        m.bought_the_heart("l11");
        assert_ne!(m.next_target().unwrap().reason, Goal::Heart, "the shelf is bare");
    }

    /// **A bed at every settlement noun; a heart at only two of them.** Task #75.
    ///
    /// The two questions were one predicate, and `is_settlement` — `village || town` — was answering
    /// both. It is the heart's bar and it is right for that: `healthBuff` is what makes a village a
    /// village, and a hamlet is defined by having neither buff
    /// (`overworld/locations/village.lua:6-14`). It is the wrong bar for a bed, because `store_inn`
    /// is unconditional in every settlement's roster (`overworld/generators/village.lua:684-685`).
    ///
    /// A town has a bed and *may* have a heart — `world.lua:520-529` hands out `gearSlotsBuff` and
    /// `healthBuff` in separate passes over the same list, so `getTypeName` returning `town` on the
    /// first tells us nothing about the second. Targeting one for a heart is therefore a gamble the
    /// planner takes knowingly; targeting one for a bed is not a gamble at all.
    #[test]
    fn a_hamlet_has_a_bed_and_never_a_heart() {
        let hurt_beside = |heading: &str| {
            let mut m = WorldMap::new();
            m.fold(&dump("here", "camp", vec![node("l27", heading)]));
            m.here = Some("here".into());
            m.gold = 500;
            m.hell = Some(0.1);
            ready_for_the_anomaly(&mut m);
            m.note_health_level(crate::rest::Health { current: 4, max: 20 });
            m
        };

        for heading in ["Rowlston Covert village", "Enholmes town", "Fordon hamlet"] {
            let m = hurt_beside(heading);
            let plan = m.next_target().unwrap_or_else(|| panic!("no plan beside `{heading}`"));
            assert_eq!(plan.reason, Goal::Rest, "`{heading}` has an inn like every settlement");
            assert_eq!(plan.target, "l27");
        }

        // The heart is the other question, and the hamlet is where the two part company.
        let heart_beside = |heading: &str| {
            let mut m = WorldMap::new();
            m.fold(&dump("here", "camp", vec![node("l27", heading)]));
            m.here = Some("here".into());
            m.gold = 500;
            m.hell = Some(0.1);
            ready_for_the_anomaly(&mut m);
            m
        };
        for heading in ["Rowlston Covert village", "Enholmes town"] {
            assert!(heart_beside(heading).get("l27").unwrap().stocks_a_heart(), "{heading}");
        }
        let m = heart_beside("Fordon hamlet");
        assert!(!m.get("l27").unwrap().stocks_a_heart(), "a hamlet has neither buff, so no heart");
        assert_ne!(
            m.next_target().map(|p| p.reason),
            Some(Goal::Heart),
            "and it is never worth the walk for one"
        );
    }

    /// Standing at a settlement, short of full, with the price in pocket: rest.
    ///
    /// The dev's rule, 2026-08-15. Deliberately weaker than [`WorldMap::wants_rest`] — half health
    /// or a four-point drop are the bars for a *detour*, and standing in the doorway is not one.
    #[test]
    fn passing_a_settlement_at_less_than_full_health_is_worth_a_bed() {
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("l28", "Enholmes town")]));
        // **Set, and load-bearing.** Without it `next_target` has no origin, every branch below
        // falls through to `Explore`, and the two assertions further down would both pass without
        // testing anything. The control caught exactly that while this test was being written.
        m.here = Some("here".into());
        m.gold = 50;
        m.health = Some(crate::rest::Health { current: 18, max: 20 });
        assert!(!m.wants_rest(), "two points down is nowhere near the detour bar");
        assert!(m.top_up_at("l28"), "but we are standing at the door");

        // **And it stays off the detour bar.** Task #72. This asserted `wants_rest()` until
        // 2026-08-21, on the reasoning that the errand machinery had to hear about the decision —
        // true, but it heard through the wrong channel. `wants_rest` is what the surface planner's
        // `Goal::Rest` branch reads, so the doorway rule was minting detours: live at 77/80 the plan
        // came out `Rest -> l63`, a surface hop to heal three points for ten gold.
        assert!(!m.wants_rest(), "a scratch is not a reason to walk anywhere");
        assert!(m.wants_a_bed(), "but it is a reason to use the bed we are standing on");
        assert_ne!(
            m.next_target().map(|p| p.reason),
            Some(Goal::Rest),
            "and no trip is planned for it"
        );

        // The control, and the half that must not regress: real damage still buys the detour.
        //
        // Against a **village**, not the town above. `rest::site` matches `village`, `inn` and
        // `campfire` and does not know the word `town` — so no town is ever a rest site, which is
        // task #75 and not this one. The control was silently green against the town until that
        // turned up, which is the second thing it has caught here.
        m.fold(&dump("here", "camp", vec![node("l27", "Rowlston Covert village")]));
        m.note_health_level(crate::rest::Health { current: 4, max: 20 });
        assert!(m.wants_rest());
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Rest, "a wound still walks");
        assert_eq!(plan.target, "l27");

        // Both are cleared by the same events, so a top-up cannot outlive the bed it was about.
        m.rest_errand_over();
        assert!(!m.wants_rest());
        assert!(!m.wants_a_bed(), "the top-up flag is cleared alongside it, not left standing");

        // Full health buys nothing.
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("l28", "Enholmes town")]));
        m.gold = 50;
        m.health = Some(crate::rest::Health { current: 20, max: 20 });
        assert!(!m.top_up_at("l28"));

        // Nor does a purse that cannot pay the innkeeper.
        m.health = Some(crate::rest::Health { current: 18, max: 20 });
        m.gold = crate::rest::INN_COST - 1;
        assert!(!m.top_up_at("l28"));
        m.gold = crate::rest::INN_COST;
        assert!(m.top_up_at("l28"), "exactly the price is enough for a bed — `getCanRest` is `>=`");

        // And a forest is not a settlement, whatever our health is.
        m.fold(&dump("here", "camp", vec![node("l16", "Bursall Hedge — level 2 forest")]));
        m.gold = 500;
        assert!(!m.top_up_at("l16"));
    }

    /// The shelf is emptied, and the bed is never spent.
    #[test]
    fn the_purse_always_keeps_one_night_back() {
        let mut m = WorldMap::new();
        m.gold = 110;
        assert_eq!(m.hearts_affordable(), 1, "110 buys one and leaves the inn's ten");
        m.gold = 209;
        assert_eq!(m.hearts_affordable(), 1, "nine short of the second");
        m.gold = 210;
        assert_eq!(m.hearts_affordable(), 2);
        m.gold = 1040;
        assert_eq!(m.hearts_affordable(), 10);
        m.gold = 0;
        assert_eq!(m.hearts_affordable(), 0, "never negative, whatever the purse");
    }

    /// `l4`: remembered as `Riccall crypt`, met as `Riccall — level 6 crypt`.
    ///
    /// The level is in a heading only while a fight is owed (`AreaHeading`,
    /// `overworldview.lua:388-389`), so a heading cached while the node was clear says nothing about
    /// the node after corruption reset it.
    ///
    /// **And corruption is what closes that gap, not suspicion of the cache.** The dev's rule: the
    /// cache says what exists, the dump says what it is, and the save says what is corrupted. A
    /// remembered name is not evidence of danger and is not treated as any.
    #[test]
    fn corruption_is_what_makes_a_remembered_heading_a_fight() {
        let cached = format!(
            "{CACHE_VERSION}
p	l4	Riccall crypt	951	275	4	0
p	l11	Rowlston Covert village	900	300	2	0
e	l4	l11
"
        );
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("l4", "")]));
        assert!(m.absorb_cache(&cached) > 0);
        m.here = Some("here".into());
        m.gold = HEART_COST + 1;

        // Remembered and uncorrupted: no reason to think it is a fight, and no pessimism about it.
        m.hell = Some(0.1);
        assert!(!m.get("l4").expect("recalled").may_be_a_fight(), "a remembered name is not a threat");
        assert!(m.reachable_without_a_fight("here", "l11"), "so the village behind it is a free trip");

        // The save says otherwise, and the save is read fresh every step.
        m.apply_save(
            &crate::game::save::parse(
                "return { overworld = { areaFlags = { hell = 0.1, l4_first_corrupt_time = 12 } } }",
            )
            .unwrap(),
        );
        assert!(m.get("l4").unwrap().corrupted);
        assert!(m.get("l4").unwrap().may_be_a_fight(), "corruption is what turns it back into one");
        assert!(!m.reachable_without_a_fight("here", "l11"), "and the trip stops being free");
    }

    /// The last decision of the run of 2026-08-09, rebuilt from `spike-run-raw.log:1336-1344`.
    ///
    /// Standing on `l28`, the dump named `shrine6` for the first time — two connections, uncorrupted,
    /// unconsecrated, one step away — alongside `l49`, a level 6 crypt. The planner named `start`,
    /// which sat in the map unheaded and edgeless, stepped onto the crypt because no route to `start`
    /// existed, and the run died there. A branch that could make no progress outranked one that could.
    ///
    /// **`shrine6` was known for exactly this one decision**, not for the five hops before it — the
    /// earlier `(for start, Anomaly)` lines had no shrine in the map to skip. Worth being precise
    /// about: what #24 recovers here is the last move, and what it changes about the other four is
    /// only that they stop claiming an errand and explore under their own name.
    #[test]
    fn a_shrine_we_can_reach_beats_an_anomaly_we_cannot() {
        let mut m = WorldMap::new();
        m.fold(&dump("l28", "Enholmes town", vec![
            node("l27", "Barkerdale village"),
            node("l19", "Dane village"),
            node("l49", "Yokefleet — level 6 crypt"),
            node("shrine6", "Borsea shrine"),
        ]));
        // `start` as the run actually held it: heard of through `start_first_corrupt_time`, no
        // heading, no edges. `hell = 0.1` means the portal is open -- see `anomaly_is_open`.
        m.apply_save(&crate::game::save::parse(
            "return { overworld = { areaFlags = { hell = 0.1, start_first_corrupt_time = 12 } } }",
        ).unwrap());
        m.here = Some("l28".into());

        assert_eq!(m.anomaly().map(|p| p.key.as_str()), Some("start"), "still known for what it is");
        assert!(!m.can_route_to("start"), "and still nothing we can walk to");
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Shrine, "the branch that can make progress gets its turn");
        assert_eq!(plan.target, "shrine6");
        assert_eq!(m.next_hop().unwrap().step, "shrine6", "not the level 6 crypt next door");
    }

    /// Giving up on an errand we cannot route to does not mean giving up its direction.
    ///
    /// The half of #24 that is easy to lose. Refusing to *nominate* an unreachable target is right;
    /// letting exploration then wander is how the fix would have paid for a truthful log with a
    /// slower run. Both frontiers here are one hop away and equally informative, so nothing but the
    /// bearing separates them — and the keys are chosen so the alphabet points the wrong way, which
    /// is what the ordering fell back on before.
    #[test]
    fn we_explore_toward_the_errand_we_could_not_route_to() {
        let mut m = WorldMap::new();
        ready_for_the_anomaly(&mut m);
        m.fold(&dump("here", "The Wold crossroads", vec![
            Node { key: "zwest".into(), heading: "West Field".into(),
                   x: -100.0, y: 0.0, connections: 3 },
            Node { key: "aeast".into(), heading: "East Field".into(),
                   x: 100.0, y: 0.0, connections: 3 },
        ]));
        m.apply_save(&crate::game::save::parse(
            "return { overworld = { areaFlags = {
                 hell = 0.1, start_first_corrupt_time = 12, zwest_first_corrupt_time = 12,
             } } }",
        ).unwrap());

        assert!(!m.can_route_to("start"), "so the anomaly branch yields");
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::RouteTo(Box::new(Goal::CloseAnomaly)));
        assert_eq!(plan.target, "zwest", "the alphabet says `aeast`; the corruption says west");
    }

    /// The fourth bounce, live on 2026-08-08: `l35 -> l41 -> l35`, until the run failed.
    #[test]
    fn a_node_we_have_already_stood_on_is_not_somewhere_to_explore() {
        // Rebuilt from that run's map. `l41` is visited and complete, with neighbours still under
        // cloud, so `is_frontier` was true for ever -- and standing there cannot change it, because
        // the hidden count is a cloud count and arriving does not lift clouds.
        //
        // The loop needed both halves. From `l35` the planner explored to `l41`; from `l41` the
        // branch excludes `here`, found nothing else free, fell through to `easiest_hostile` and
        // routed to `l9` -- whose first hop is `l35`. Neither plan was wrong on its own.
        let mut m = WorldMap::new();
        m.fold(&dump(
            "l35",
            "Thorpe crypt",
            vec![node("l41", "Grimston crossroads"), node("l9", "Saltagh Park — level 1 forest")],
        ));
        for k in ["l35", "l41"] {
            let p = m.entry(k);
            p.completed = true;
            p.visited = true;
            p.hidden = Some(2); // neighbours the game refused to name
        }
        m.note_health_level(crate::rest::Health { current: 1, max: 20 });

        // `l41` is still a frontier in the sense that matters for conclusions -- we cannot say what
        // is next to it -- and that stays true.
        assert!(m.get("l41").unwrap().is_frontier(), "hidden neighbours are still unknown");
        // But it is not somewhere to go. The only plan left is the cheapest fight, and crucially it
        // is the SAME plan from either end of the old cycle, so the run makes progress.
        let from_l35 = m.next_target().unwrap();
        assert_eq!(from_l35.reason, Goal::EasiestHostile { level: Some(1) });
        assert_eq!(from_l35.target, "l9");
        m.here = Some("l41".into());
        assert_eq!(m.next_target().unwrap().target, "l9", "the plan must not flip when we move");
    }

    /// The third bounce, live on 2026-08-08: `l10 -> shrine2 -> l10`, twelve times.
    #[test]
    fn a_used_but_unconsecrated_shrine_is_a_real_errand_and_ends_when_it_is_done() {
        // Both shrines prayed at in an earlier run and neither consecrated -- straight out of the
        // sandbox save, which carries `shrine2_used = true` and `shrine3_used = true` with no
        // `_consecrated` flag for either. `worth_a_trip`'s second clause is therefore true forever,
        // and the planner kept routing to them while `drive` declined every arrival, because the
        // branch that acts on a shrine needed `!used`.
        //
        // The clause was right and the driver was missing. Consecrating is now implemented, so what
        // this pins is that the errand EXISTS -- and that it ends.
        let mut m = WorldMap::new();
        m.fold(&dump(
            "l10",
            "Trenwick crypt",
            vec![node("shrine2", "Foggathorpe shrine"), node("shrine3", "Skipsea shrine")],
        ));
        for k in ["shrine2", "shrine3"] {
            let p = m.entry(k);
            p.completed = true;
            p.visited = true;
            p.used = true;
            p.consecrated = false;
        }
        m.hell = Some(0.1); // the anomaly is open, so consecrating is possible at all
        assert!(m.anomaly_is_open().unwrap(), "fixture must have the door open");

        // There is something to do at both, and `worth_consecrating_here` is what says so -- the
        // function that existed and was called from nowhere while the run bounced.
        assert!(m.worth_consecrating_here("shrine2"));
        assert!(m.worth_consecrating_here("shrine3"));
        assert_eq!(m.next_target().map(|p| p.reason), Some(Goal::Shrine));

        // Each one is spent by arriving, whether or not the press worked. Standing on a shrine and
        // doing nothing now abandons it, so a decline can no longer feed the loop.
        m.abandon("shrine2");
        assert_eq!(m.next_target().unwrap().target, "shrine3");
        m.abandon("shrine3");
        if let Some(plan) = m.next_target() {
            assert_ne!(plan.reason, Goal::Shrine, "the errand must end: {plan:?}");
        }

        // Consecrating is what actually retires one, and it retires it for good.
        let mut m2 = m;
        m2.entry("shrine2").consecrated = true;
        assert!(!m2.worth_consecrating_here("shrine2"), "nothing left to do here");
    }

    #[test]
    fn an_open_portal_makes_a_clean_shrine_the_errand_and_a_corrupted_one_no_errand_at_all() {
        // Two opposite rules meet once the door opens, and this pins both.
        //
        // An **uncorrupted** shrine outranks the portal: consecrating is only possible while
        // `hell ~= 0`, it costs no fight, and it pays in wildcard tiles before a level 8 fight. A
        // **corrupted** one has to be fought through again, so it is never a destination -- it is
        // worth something only when we are already standing on it.
        //
        // here — shrine2(shrine) is a dead end; here — shrine1(shrine) — rift is the route.
        //
        // **The shrines are keyed `shrineN` because the game keys them that way**, and since
        // 2026-08-17 that is load-bearing rather than cosmetic: `majorShrine` is set from
        // `key:sub(1,6)=='shrine'` (`overworld/generators/world.lua:87-90`), and `Consecrate` needs
        // it (`shrine.lua:93-96`), so [`Place::can_be_consecrated`] reads the key. These used to be
        // `mid` and `detour`, which no overworld shrine can be called — a fixture in a state play
        // cannot produce, quietly asserting on it.
        let mut m = WorldMap::new();
        ready_for_the_anomaly(&mut m);
        m.fold(&dump(
            "here",
            "camp",
            vec![node("shrine2", "Faraway shrine"), node("shrine1", "Midway shrine")],
        ));
        m.fold(&dump(
            "shrine1",
            "Midway shrine",
            vec![node("here", "camp"), node("rift", "The Rift anomaly")],
        ));
        m.here = Some("here".into());
        m.hell = Some(0.1);

        // Clean shrines reachable, so the shrine is the errand -- ahead of the portal itself. This
        // assertion was the opposite before 2026-08-10; the run that changed it walked past three
        // shrines and died in a level 6 crypt with the portal never once nominated.
        assert_eq!(m.next_target().unwrap().reason, Goal::Shrine);

        // The route is a separate question from the goal, and it is unchanged.
        let route = m.anomaly_route().unwrap();
        assert!(route.contains(&"shrine1".to_string()), "route: {route:?}");
        assert!(!route.contains(&"shrine2".to_string()), "the dead-end shrine is not on the way");

        // Corrupt both. Now neither is a destination, and the portal is what is left -- which is
        // also the control: without the corruption filter this would still say `Shrine`, so the
        // first assertion above would pass for the wrong reason.
        m.entry("shrine1").corrupted = true;
        m.entry("shrine2").corrupted = true;
        assert_eq!(
            m.next_target().unwrap().reason,
            Goal::CloseAnomaly,
            "a corrupted shrine is never walked to, so the portal is the errand again"
        );

        // Arrival is the other axis and it did not move: the one on the way earns its fight
        // because we are crossing it regardless, the dead end does not.
        assert!(m.worth_consecrating_here("shrine1"), "we are walking through it regardless");
        assert!(
            !m.worth_consecrating_here("shrine2"),
            "a corrupted dead end is not worth the fight"
        );

        // **And once the fight is won the objection is spent.** Corruption is a bill, not a
        // property; `completed` means it has been paid and cannot be charged twice. The dead end is
        // still a dead end and still off every route — that is what makes this the right control.
        m.entry("shrine2").completed = true;
        assert!(
            m.worth_consecrating_here("shrine2"),
            "the fight this was avoiding has already been fought"
        );
    }

    #[test]
    fn a_corrupted_shrine_is_skipped_where_a_merely_unfought_one_is_not() {
        // The distinction "incomplete" cannot make: a shrine revealed far from the hell radius and
        // never fought is an ordinary detour, while one the radius reset is the fight to avoid.
        let mut m = WorldMap::new();
        let mut d = dump("here", "camp", vec![node("s1", "Faraway shrine"), node("l2", "Quiet Glade meadow")]);
        d.hidden = 1;
        m.fold(&d);
        m.hell = Some(0.1);

        // Unfought but uncorrupted, and off-route: still worth going for.
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Shrine);
        assert_eq!(plan.target, "s1");

        // Same shrine, once corruption has reset it: now a fight we no longer need.
        m.entry("s1").corrupted = true;
        assert_ne!(m.next_target().unwrap().reason, Goal::Shrine);
    }

    #[test]
    fn a_map_kept_between_runs_gives_the_planner_a_route_it_had_walked() {
        // The live failure of 2026-08-14, in miniature. An earlier run walked `here -> mid -> s1`
        // and stood on the shrine; a later run starts knowing only what the save carries — that
        // `s1` exists, is complete, is corrupted and is not consecrated — and therefore cannot
        // route to it. `RouteTo(Shrine)` for a whole run, and a free consecration never taken.
        let mut walked = WorldMap::new();
        walked.fold(&dump("here", "camp", vec![node("mid", "Quiet Glade meadow")]));
        walked.fold(&dump("mid", "Quiet Glade meadow", vec![node("s1", "Faraway shrine")]));
        let text = walked.cache_text();

        // The restart: flags only, exactly as `apply_save` leaves it.
        let mut fresh = WorldMap::new();
        fresh.fold(&dump("here", "camp", vec![]));
        fresh.hell = Some(0.1);
        {
            // Cleared, and **uncorrupted**: since 2026-08-15 a corrupted shrine is not a target at
            // all, and this test is about the cache restoring a route rather than about corruption.
            let p = fresh.entry("s1");
            p.completed = true;
        }
        assert!(
            !matches!(fresh.next_target().map(|p| p.reason), Some(Goal::Shrine)),
            "the premise: with no edges the shrine cannot be planned"
        );

        // Now hand it back what it had already learned.
        assert!(fresh.absorb_cache(&text) > 0, "the cache carried no edges");
        // **And say which of that ground is cleared**, which is the save's job rather than the
        // cache's. Since 2026-08-15 a recalled heading cannot prove a node is safe to cross — the
        // level is in the heading only while a fight is owed (`AreaHeading`,
        // `overworldview.lua:388-389`), so a heading remembered from before corruption reads tamer
        // than the node now is. Without this the fixture asserts something it never meant to: that
        // unwalked ground with a remembered name is known to be fightless.
        for k in ["here", "mid"] {
            fresh.entry(k).completed = true;
        }
        let plan = fresh.next_target().expect("a plan");
        assert_eq!(plan.reason, Goal::Shrine, "the route was known all along");
        assert_eq!(plan.target, "s1");
    }

    #[test]
    fn a_corrupted_shrine_is_not_a_destination_but_is_still_consecrated_on_arrival() {
        // **The rule changed on 2026-08-15 and this test changed with it.** It used to assert that a
        // corrupted shrine whose fight we had already won was a destination again, which was the
        // dev's earlier correction — corruption is a level, not a locked door. The refinement:
        // corruption still is not a locked door, but it is not a reason to make the trip either.
        // Only unconsecrated, uncorrupted shrines are targets while the anomaly is open; a corrupted
        // one is consecrated **if we happen to be standing on it**, having gone there for something
        // else.
        //
        // What that protects: the run of 2026-08-15 walked `shrine5 -> l50 -> l41 -> l51 -> l35 ->
        // l49` chasing shrines, into a level 6 crypt, with the anomaly untouched.
        //
        // Both halves are asserted on one map, because the two used to drift apart and the drift is
        // what left `shrine1` unconsecrated for four runs.
        // Keyed `shrine1` rather than `s1`, because `Consecrate` is gated on `majorShrine` and the
        // game derives that from the key (`world.lua:87-90`) — see [`Place::can_be_consecrated`].
        let mut m = WorldMap::new();
        let mut d =
            dump("here", "camp", vec![node("shrine1", "Faraway shrine"), node("l2", "Quiet Glade meadow")]);
        d.hidden = 1;
        m.fold(&d);
        m.hell = Some(0.1);
        m.entry("shrine1").corrupted = true;
        // **The bar has to be met, or #74 makes this shrine a target and the test is about a
        // different rule.** Corruption stops being a veto once the clean supply is spent and
        // `SHRINES_BEFORE_THE_ANOMALY` is still short — which, on a fixture holding exactly one
        // shrine and it corrupted, is true from the first line. Satisfying the gate isolates the
        // 2026-08-15 rule this test is actually about.
        ready_for_the_anomaly(&mut m);

        assert_ne!(m.next_target().unwrap().reason, Goal::Shrine, "the fight is still owed");
        assert!(
            !m.worth_consecrating_here("shrine1"),
            "and the arrival test agrees while it is owed"
        );

        m.entry("shrine1").completed = true;
        assert_ne!(
            m.next_target().unwrap().reason,
            Goal::Shrine,
            "cleared or not, a corrupted shrine is not somewhere we set off for"
        );
        assert!(
            m.worth_consecrating_here("shrine1"),
            "but standing on it, the consecration is free and we take it"
        );

        // Uncorrupt the same shrine and it becomes a destination, which is what pins the filter to
        // corruption rather than to something else about the fixture.
        m.entry("shrine1").corrupted = false;
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Shrine);
        assert_eq!(plan.target, "shrine1");
    }

    /// A shrine behind a fight is still a destination **once the portal is open**. Task #70.
    ///
    /// This asserted the opposite until 2026-08-21, on the rule *go toward the anomaly unless there
    /// is an accessible shrine that does not require combat*. The dev reversed it after watching the
    /// 1519Z run: *shrines should never be blocked by level unless the anomaly has not yet opened.*
    ///
    /// The reversal is not a change of mind about cost, it is the gate changing what a shrine **is**.
    /// When that rule was written a shrine was optional reward, and paying a crypt for one spent the
    /// level 8 fight's budget early. [`SHRINES_BEFORE_THE_ANOMALY`] made four of them a
    /// precondition, and the argument at the `CloseAnomaly` branch already says so: *the shrines are
    /// not a reward to collect on the way, they are the preparation the fight needs.* Preparation we
    /// are required to buy cannot also be refused for costing something.
    ///
    /// Live: the eastern shrines of the 1519Z run were revealed from the south and struck off the
    /// candidate list while a forest stood between, then consecrated from the north twenty steps
    /// later. The whole detour bought nothing — one step after the second shrine the run took a
    /// level 6 forest fight anyway, to explore.
    #[test]
    fn a_shrine_behind_a_crypt_is_still_worth_the_trip_once_the_portal_is_open() {
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("l49", "Yokefleet — level 6 crypt")]));
        m.fold(&dump("l49", "Yokefleet — level 6 crypt", vec![node("shrine2", "Faraway shrine")]));
        m.here = Some("here".into());
        m.hell = Some(0.1);

        // The route still costs a fight, and we go anyway. Both halves matter: a fixture that had
        // quietly become fight-free would pass this for the wrong reason.
        assert!(!m.reachable_without_a_fight("here", "shrine2"), "the only way through is the crypt");
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::Shrine);
        assert_eq!(plan.target, "shrine2");
    }

    /// **Before the portal opens, level still blocks** — and that is a different rule, kept.
    ///
    /// #70 removed the *route* test, not the pre-anomaly one. `triggers_anomaly` exists because
    /// arriving at a level 4+ node is what opens the portal (`world_evil.lua:15-18`), so a shrine
    /// above level 3 is not a free errand while `hell == 0` — it is the trigger, and taking it ends
    /// the phase it belongs to. The run of 2026-08-17 walked into `shrine7`, *Cottam Boscage — level
    /// 9 forest*, at 52/52 exactly this way.
    #[test]
    fn before_the_portal_a_deep_shrine_is_still_refused() {
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("shrine7", "Cottam Boscage — level 9 forest")]));
        m.here = Some("here".into());
        m.hell = Some(0.0);

        assert_ne!(
            m.next_target().map(|p| p.reason),
            Some(Goal::Shrine),
            "a level 9 shrine is the anomaly trigger, not a cheap errand"
        );
    }

    /// "No route I can see" and "no route" are different claims, and only one of them is ours.
    ///
    /// The dev's correction, 2026-08-15, after a run bought one heart and went straight at a level 7
    /// crypt: *it should be the navigator's responsibility to probe and build the in-memory cache of
    /// which nodes are accessible without combat, not to assume from the cache that they are
    /// inaccessible.*
    ///
    /// The live map is the fixture. `l28 Enholmes town` declared **four** connections with exactly
    /// **one** recorded, so three of its roads had never been looked down — and the planner reported
    /// it unreachable-without-a-fight as though that were a fact about the map.
    #[test]
    fn an_unlooked_road_is_unknown_access_and_a_closed_region_is_blocked() {
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("free", "Quiet Glade meadow")]));
        m.fold(&dump("free", "Quiet Glade meadow", vec![node("l49", "Yokefleet — level 6 crypt")]));
        m.fold(&dump("l49", "Yokefleet — level 6 crypt", vec![node("l28", "Enholmes town")]));
        m.here = Some("here".into());

        // Closed first: every free node has all the roads the game says it has, so the crypt really
        // is the only way and saying so is honest.
        for k in ["here", "free"] {
            let n = m.entry(k).neighbours.len() as u32;
            m.entry(k).connections = n;
        }
        assert_eq!(m.access_without_a_fight("here", "l28"), Access::Blocked);

        // Now put a road we have never looked down inside the region — the `l28` case, one node
        // earlier. The answer must stop being a claim about the map.
        //
        // **On ground we have not stood on**, which is what changed with #73. Bumping `free`'s own
        // degree used to do it, but `free` is a dump's `here_key` and so is `visited`: a gap that
        // survived standing there is a secret neighbour, not an unlooked road, and it no longer
        // counts. `lane` is named by the dump and never walked to, which is the ordinary shape of a
        // frontier and needs no hand-set degree at all.
        m.fold(&dump(
            "free",
            "Quiet Glade meadow",
            vec![node("l49", "Yokefleet — level 6 crypt"), node("lane", "Fordon Lane meadow")],
        ));
        assert!(!m.get("lane").unwrap().visited, "the premise: named, never stood on");
        assert_eq!(m.access_without_a_fight("here", "l28"), Access::Unknown);

        // And an unlooked road never upgrades a *known* fight into a maybe: the crypt on the only
        // recorded route is still a crypt.
        assert_eq!(m.access_without_a_fight("here", "l49"), Access::Blocked, "the fight is not in doubt");
    }

    /// On `Unknown`, the heart errand walks to the unlooked road instead of abandoning the errand.
    #[test]
    fn a_heart_behind_the_fog_is_probed_rather_than_written_off() {
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("free", "Quiet Glade meadow")]));
        m.fold(&dump("free", "Quiet Glade meadow", vec![node("l49", "Yokefleet — level 6 crypt")]));
        m.fold(&dump("l49", "Yokefleet — level 6 crypt", vec![node("l28", "Enholmes town")]));
        m.here = Some("here".into());
        m.hell = Some(0.1);
        m.gold = 500;
        assert!(m.wants_a_heart(), "anomaly open and the price is affordable");

        // Closed: nothing to look at, so the errand cannot be served and the run gets on with the
        // anomaly. This half is what says the probe is evidence-driven rather than a wander.
        for k in ["here", "free"] {
            let n = m.entry(k).neighbours.len() as u32;
            m.entry(k).connections = n;
        }
        assert_ne!(
            m.next_target().map(|p| p.reason),
            Some(Goal::Heart),
            "a closed fight-free region really has no heart in it"
        );

        // One unlooked road, and the same map is worth walking into. **Named and never walked to**
        // since #73 — a gap on `free`, which we have stood on, is a secret neighbour rather than an
        // unlooked road, and buys no second visit.
        m.fold(&dump(
            "free",
            "Quiet Glade meadow",
            vec![node("l49", "Yokefleet — level 6 crypt"), node("lane", "Fordon Lane meadow")],
        ));
        let plan = m.next_target().expect("something to do");
        assert_eq!(plan.reason, Goal::Heart, "the walk is still the heart errand, and says so");
        assert_eq!(plan.target, "lane", "the probe is the node with the road we have not tried");
    }

    /// **A node we have stood on is not a probe candidate at all.** Task #73.
    ///
    /// The 1519Z bounce, `l11 Argham crossroads` against `l13 Bilton crypt`, and the shape that
    /// showed the preference added earlier the same day is not enough:
    ///
    /// ```text
    ///   83. l44 -> **l11**  (for l11, Heart)
    ///   84. l11 -> **l13**  (for l36, Explore)
    ///   85. l13 -> **l11**  (for l11, Heart)
    ///   86. l11 -> **l13**  (for l36, Explore)
    ///   87. l13 -> **l11**  (for l11, Heart)
    ///   88. `l11` is written off — stood on 2 times with nothing learned
    /// ```
    ///
    /// Three laps. `min_by_key((visited, dist))` ranks candidates against each other, so with
    /// **one** candidate it changes nothing — and one is the ordinary case. The dev, 2026-08-21,
    /// choosing between ranking and filtering with this run in front of them: *one probe per node,
    /// remembered.*
    #[test]
    fn a_probe_is_not_offered_the_same_node_twice() {
        let mut m = WorldMap::new();
        m.fold(&dump("l13", "Bilton — level 4 crypt", vec![node("l11", "Argham crossroads")]));
        m.here = Some("l13".into());
        let dist = m.distances("l13");

        // One candidate, and it declares a road we have never looked down.
        m.entry("l11").connections = 4;
        assert!(m.has_unexplored_roads("l11"), "the premise: the game says there is more here");
        assert_eq!(
            m.probe_toward_the_unknown("l13", &dist).map(|p| p.key.as_str()),
            Some("l11"),
            "so the first probe goes"
        );

        // Stand on it. The dump names one neighbour and the declared degree is still four, so the
        // gap survived the one act that could have closed it — a secret neighbour, which
        // `verboseAdjacencyData` prints as `Hidden location` and never names until a tower reveals
        // it (`overworldview.lua:554-556`, task #14).
        m.fold(&dump("l11", "Argham crossroads", vec![node("l13", "Bilton — level 4 crypt")]));
        m.entry("l11").connections = 4;
        m.here = Some("l13".into());

        assert!(!m.has_unexplored_roads("l11"), "a gap that survives a visit is not unexplored");
        assert_eq!(
            m.probe_toward_the_unknown("l13", &dist),
            None,
            "and there is no second probe to bounce off"
        );
    }

    /// **The probe goes outward, and never back to where it has stood.**
    ///
    /// The `l11 Argham crossroads` / `l5 Dalton Copse` bounce of 2026-08-21, in the smallest shape
    /// that produces it. Standing on a node is what makes the game name its neighbours, so a gap
    /// that survives a visit is one visiting cannot close — a secret neighbour, most often. The
    /// probe kept nominating it, the errand at the far end nominated the way back, and it took two
    /// full laps and a write-off to break.
    ///
    /// Named `..._prefers_...` when it was a ranking. #73 made it a filter, because a ranking has
    /// nothing to decide when there is one candidate — see
    /// [`WorldMap::has_unexplored_roads`] and the second half of this test.
    #[test]
    fn the_heart_probe_never_returns_to_ground_it_has_stood_on() {
        let build = || {
            let mut m = WorldMap::new();
            // Two ways out of the camp, both fight-free, both with a road we have not looked down.
            // **`stood` is strictly nearer**, or the ranking proves nothing: a tie on distance is
            // broken by iteration order, and `fresh` happens to sort first. The bounce is precisely
            // the case where the node we have stood on is the *closest* one still declaring a road.
            m.fold(&dump("here", "camp", vec![node("stood", "Argham crossroads")]));
            m.fold(&dump(
                "stood",
                "Argham crossroads",
                vec![node("here", "camp"), node("fresh", "Quiet Glade meadow")],
            ));
            m.here = Some("here".into());
            m.hell = Some(0.1);
            m.gold = 500;
            // A village exists but no route to it is known, which is what sends the errand probing.
            m.entry("far").heading = "Rowlston Covert village".into();
            for k in ["stood", "fresh"] {
                let n = m.entry(k).neighbours.len() as u32;
                m.entry(k).connections = n + 1;
            }
            m
        };

        // `stood` is nearer by key order and has been walked; `fresh` has not.
        let mut m = build();
        m.entry("stood").visited = true;
        let plan = m.next_target().expect("a plan");
        assert_eq!(plan.reason, Goal::Heart, "the walk is still the heart errand");
        assert_eq!(
            plan.target, "fresh",
            "a node we have already stood on has no more roads to give us"
        );

        // **And since #73 it is a refusal, not a preference.** This asserted the opposite until
        // 2026-08-21: with nothing else left, the visited node was still nominated, on the dev's
        // rule of 2026-08-15 that the navigator probes rather than concluding from an incomplete
        // cache.
        //
        // The 1519Z run is why it changed. A ranking only decides between candidates, and the
        // ordinary case has **one** — `l11` against `l13` went three full laps with this very
        // preference in the binary. Concluding *after* a visit is the 2026-08-15 responsibility
        // discharged, not dodged: standing there is the probe, and it came back empty.
        let mut m = build();
        m.entry("stood").visited = true;
        let n = m.entry("fresh").neighbours.len() as u32;
        m.entry("fresh").connections = n;
        assert_ne!(
            m.next_target().map(|p| p.reason),
            Some(Goal::Heart),
            "every road has been walked, so the errand stops rather than re-walking one"
        );
    }

    /// Not a crypt, and still a fight — the four nodes the first rule waved through.
    ///
    /// Rebuilt from the route the run of 2026-08-15 took out of `shrine1`: a bandit camp, a spider
    /// forest and a graveyard, chasing a shrine while the only corrupted node it could reach sat one
    /// hop the other way.
    #[test]
    fn a_bandit_camp_is_a_fight_even_though_it_is_not_a_crypt() {
        for heading in [
            "Norton Firth — level 3 bandit camp",
            "Sancton Regrow — level 3 spider forest",
            "Fosholme Growth — level 5 graveyard",
            "Swanland — level 6 shrine",
        ] {
            let mut m = WorldMap::new();
            m.fold(&dump("here", "camp", vec![node("mid", heading)]));
            m.fold(&dump("mid", heading, vec![node("s1", "Faraway shrine")]));
            m.here = Some("here".into());
            m.hell = Some(0.1);
            assert!(
                !m.reachable_without_a_fight("here", "s1"),
                "`{heading}` is a fight on the way, whatever it is called"
            );
        }
    }

    /// And the shrine's own node counts, which is why the destination is not exempt.
    #[test]
    fn a_shrine_we_would_have_to_fight_for_is_not_free_either() {
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("s1", "Swanland — level 6 shrine")]));
        m.here = Some("here".into());
        m.hell = Some(0.1);
        assert!(!m.reachable_without_a_fight("here", "s1"), "the shrine itself is the fight");
        m.entry("s1").completed = true;
        assert!(m.reachable_without_a_fight("here", "s1"), "and cleared, it is free again");
    }

    #[test]
    fn opening_the_anomaly_comes_before_shrines() {
        // Not merely a priority call: `showConsecrateButton` needs `hell ~= 0`
        // (`shrine.lua:93-96`), so a run that did shrines first would reach each one unable to
        // finish it.
        let mut m = WorldMap::new();
        m.fold(&dump(
            "here",
            "camp",
            vec![node("shrine2", "Gransmoor shrine"), node("l39", "Eight Timberland — level 4 forest")],
        ));
        m.hell = Some(0.0);
        // **What this measures changed on 2026-08-16 and the game fact behind it did not.**
        // Consecrating still needs `hell ~= 0`, so a run cannot finish a shrine before the portal
        // opens — but it was never this branch that enforced that, and the shrine branch below is
        // deliberately reachable with the portal shut, because `Pray` is available there and worth
        // having. What used to keep the run off shrines early was `OpenAnomaly` outranking them,
        // and the dev's rule has queued the trigger behind every node under level 4 — which a
        // shrine, carrying no level at all, is.
        //
        // So the assertion is the half that is still this test's own: the trigger is taken as soon
        // as nothing gentler is left, and not before.
        assert_ne!(
            m.next_target().unwrap().reason,
            Goal::OpenAnomaly,
            "a level 4 forest is not the first stop while an unwalked shrine stands"
        );
        m.entry("shrine2").visited = true;
        m.entry("shrine2").hidden = Some(0);
        let plan = m.next_target().unwrap();
        assert_eq!(plan.reason, Goal::OpenAnomaly);
        assert_eq!(plan.target, "l39");
    }

    #[test]
    fn exploration_prefers_the_nearer_frontier() {
        // Both candidates unvisited, so the comparison is purely distance: `near` is one hop,
        // `far` is two, behind an already-exhausted `through`.
        let mut m = WorldMap::new();
        m.fold(&dump("here", "camp", vec![node("near", "a meadow"), node("through", "b meadow")]));
        let mut done = dump("through", "b meadow", vec![node("here", "camp"), node("far", "c meadow")]);
        done.hidden = 0;
        m.fold(&done);
        m.here = Some("here".into());
        assert_eq!(m.next_target().unwrap().target, "near", "one hop beats two");
    }

    #[test]
    fn a_completed_neighbour_loses_to_an_unfinished_one_at_the_same_distance() {
        // The live misstep: from l10, both l1 and l18 were adjacent and unvisited, but l1 was
        // already completed and had nothing to reveal. Sorting by key took it, and the run walked
        // back through l10 to reach l18 -- two hops wasted.
        let mut m = WorldMap::new();
        m.fold(&dump(
            "l10",
            "Ulrome village",
            vec![node("l1", "Weedley Copse crypt"), node("l18", "Stanningholme — level 2 crypt")],
        ));
        m.entry("l1").completed = true;
        assert_eq!(m.next_target().unwrap().target, "l18", "a finished node reveals nothing");
    }

    #[test]
    fn a_real_dump_folds_into_the_map_it_describes() {
        // Captured live at l1. The synthetic tests above pick keys like "l4" for level-4 nodes,
        // which reads as if keys encoded levels -- they do not, and this fixture is the proof:
        // the node keyed `l4` is `Bainton Clump — level 1 forest`. Routing must never infer a level
        // from a key.
        let raw = std::fs::read_to_string("tests/fixtures/overworld-dump-l1.txt").unwrap();
        let lines: Vec<String> = raw.lines().map(|s| s.to_string()).collect();
        let dumps = crate::observe::adjacency::parse(&lines);
        assert_eq!(dumps.len(), 1, "one complete block in the fixture");

        let mut m = WorldMap::new();
        m.fold(&dumps[0]);
        assert_eq!(m.here(), Some("l1"));
        assert_eq!(m.len(), 5, "the node we stand on plus its four neighbours");

        let l4 = m.get("l4").unwrap();
        assert_eq!(l4.level(), Some(1), "keyed l4, but level 1");
        assert!(!l4.triggers_anomaly(), "level 1 is nowhere near the > 3 the event needs");
        assert!(l4.type_is("forest"));

        // The em-dash is the combat marker, so a campfire and a village carry no level at all.
        assert_eq!(m.get("start").unwrap().level(), None);
        assert_eq!(m.get("l10").unwrap().level(), None);
        assert!(m.get("l1").unwrap().has_combat());

        // Nothing here can open the anomaly, so the only sensible plan is to go and look.
        assert_eq!(m.next_target().unwrap().reason, Goal::Explore);
    }

    #[test]
    fn a_fully_explored_map_with_nothing_to_do_says_so() {
        let mut m = WorldMap::new();
        let mut a = dump("start", "camp", vec![node("l1", "a meadow")]);
        a.hidden = 0;
        m.fold(&a);
        let mut b = dump("l1", "a meadow", vec![node("start", "camp")]);
        b.hidden = 0;
        m.fold(&b);
        m.here = Some("l1".into());
        m.hell = Some(0.1);
        m.entry(ANOMALY_KEY).completed_corrupt = true;
        // Everything is visited, nothing hides neighbours, the anomaly is beaten: no target.
        assert_eq!(m.next_target(), None, "walking on would be pointless, and saying so is the answer");
    }

    /// The shortfall and the fight it prices have to agree, or the line is no better than the one
    /// it replaced. `16` is `2 x 8 - 0` and equally `2 x 9 - 2`.
    #[test]
    fn the_deepest_fight_is_the_one_the_shortfall_is_priced_against() {
        let mut m = WorldMap::new();
        m.entry("start").heading = "Cottam — level 8 anomaly".into();
        m.entry("l61").heading = "Meaux — level 7 crypt".into();
        m.entry("l23").heading = "Smithy — level 3 crypt".into();
        assert_eq!(m.deepest_fight(), Some(("start", 8)));
        assert_eq!(m.stacks_short_ahead(), crate::rest::stacks_wanted(8));

        // Clearing it moves both together, which is the property that makes them comparable.
        m.entry("start").completed = true;
        assert_eq!(m.deepest_fight(), Some(("l61", 7)));
        assert_eq!(m.stacks_short_ahead(), crate::rest::stacks_wanted(7));

        // And a map with nothing deep enough says so rather than naming a fight it is not pricing.
        m.entry("l61").completed = true;
        assert_eq!(m.deepest_fight(), None, "level 3 is below the banking floor");
        assert_eq!(m.stacks_short_ahead(), 0);
    }

    /// The 1519Z line, reconstructed: `16 stack(s) short` came from an empty bank against the
    /// level 8 anomaly, and the number alone cannot say that.
    #[test]
    fn an_empty_bank_and_a_level_eight_anomaly_are_the_sixteen_the_run_printed() {
        let mut m = WorldMap::new();
        m.entry("start").heading = "Cottam — level 8 anomaly".into();
        assert_eq!(m.well_rested, 0);
        assert_eq!(m.stacks_short_ahead(), 16);
        // The save's own reading is what settles it, and 17 banked is not 16 short.
        m.well_rested = 17;
        assert_eq!(m.stacks_short_ahead(), 0, "a full bank wants nothing");
    }

    /// **#73's probe magnet is gone, measured against the two runs it caught.**
    ///
    /// `l11 Argham crossroads` declares more roads than any dump names, so
    /// `connections > neighbours` held there **permanently** and standing on it could not close the
    /// gap. `next_target` excludes `here`, so the pull vanished on arrival and reappeared one node
    /// away: the dev watched the run walk `l11 -> l13 -> l11 -> l13` and reported it twice. The fix
    /// (`1fc548e`) makes a visited node stop counting as unexplored.
    ///
    /// Replayed here: fold each run's surface dumps and ask the **new** planner what it would do at
    /// each position the game really produced. `l11` appears in no immediate reversal in either
    /// run, against four in each run as they actually happened.
    ///
    /// ## What this replay cannot see, which is more than the crossing one
    ///
    /// It never calls `apply_save`, so completions, consecrations, gold and health are absent and
    /// the planner keeps re-targeting things the run had already finished. The replay's *own*
    /// reversals are all of that kind — `l43 -> shrine3` then `shrine3 -> l43`, a shrine it thinks
    /// is still unconsecrated — or ordinary goal-completion turns like `l37 -> e4` then
    /// `e4 -> l12`. So this asserts the narrow thing it can support and not "the planner never
    /// reverses", which would be false and would deserve to be.
    #[test]
    fn the_crossroads_no_longer_pulls_the_run_back_and_forth() {
        let is_reversal = |a: &(String, String), b: &(String, String)| a.0 == b.1 && a.1 == b.0;
        let mut runs = 0;
        for stem in ["spike-run-20260821-1519Z", "spike-run-20260821-0313Z"] {
            let Ok(log) = std::fs::read_to_string(format!("{stem}.log")) else {
                eprintln!("SKIP: {stem}.log is not present");
                continue;
            };
            let lines: Vec<String> = log.lines().map(|l| l.to_string()).collect();
            let dumps = crate::observe::adjacency::Reader::new().push(&lines);
            assert!(dumps.len() > 100, "{stem}: expected a whole run");

            let mut m = WorldMap::new();
            let mut seq: Vec<(String, String)> = Vec::new();
            for a in &dumps {
                m.fold(a);
                if a.subworld.is_some() {
                    continue;
                }
                let Some(h) = m.next_hop() else { continue };
                if h.step == a.here_key {
                    continue;
                }
                let d = (a.here_key.clone(), h.step.clone());
                if seq.last() != Some(&d) {
                    seq.push(d);
                }
            }
            assert!(seq.len() > 20, "{stem}: only {} surface decisions to judge", seq.len());
            assert!(
                seq.iter().any(|(f, _)| f == "l11"),
                "{stem}: the run never stood on `l11`, so this proves nothing about the magnet"
            );

            let magnetic: Vec<_> = seq
                .windows(2)
                .filter(|w| is_reversal(&w[0], &w[1]))
                .filter(|w| w[0].0 == "l11" || w[0].1 == "l11")
                .collect();
            assert!(
                magnetic.is_empty(),
                "{stem}: `l11` is still pulling the run back and forth: {magnetic:?}"
            );
            runs += 1;
        }
        if runs == 0 {
            return;
        }

        // The control: the runs did bounce there, four times each, and the report says so.
        let Ok(report) = std::fs::read_to_string("spike-run-20260821-1519Z.md") else {
            eprintln!("SKIP the control: the report is not present");
            return;
        };
        let mut hops: Vec<(String, String)> = Vec::new();
        for l in report.lines() {
            // `NN. from -> **to** (for target, Reason)`
            let Some((_, rest)) = l.split_once(". ") else { continue };
            let Some((from, rest)) = rest.split_once(" -> **") else { continue };
            let Some((to, _)) = rest.split_once("**") else { continue };
            if from.contains(' ') || to.contains(' ') {
                continue;
            }
            hops.push((from.to_string(), to.to_string()));
        }
        let bounced = hops.windows(2).filter(|w| is_reversal(&w[0], &w[1])).count();
        assert!(
            bounced >= 4,
            "the 1519Z report should show the `l11` bounce; found {bounced} reversals in {} hops, \
             so this detector is measuring nothing",
            hops.len()
        );
    }

    /// **#70 against the run that motivated it**, from that run's own world rather than a fixture.
    ///
    /// `pick_shrine` used to carry `reachable_without_a_fight`, and the argument for removing it was
    /// that it does not price anything: `may_be_a_fight` is `heading_has_combat || corrupted`, a
    /// boolean, so a level 1 forest disqualified a route exactly as hard as a level 9 crypt. That
    /// argument was made from the code. This makes it from the map the 1519Z run built.
    ///
    /// Standing where the run stood at its step 14 — which it spent on `Goal::Explore` — with the
    /// portal open, as that run's header records (`anomaly open Some(true)`), and with seven shrines
    /// already known and one of the four consecrations banked:
    ///
    /// ```text
    ///   shrine5   Harswell Coppice — level 7 forest    dist 12   fight-free false
    ///   shrine6   Fitling shrine                       dist 24   fight-free false
    ///   shrine3   Burshill shrine                      dist 42   fight-free true
    ///   shrine4   Thornthorpe shrine                   dist 42   fight-free true
    /// ```
    ///
    /// The nearest shrine in the world is three and a half times closer than the pair the run
    /// eventually took, and the old filter refused it. The run's report agrees: its **only** two
    /// `Shrine` goals are steps 68 and 73, both taken while already standing next to the shrine, and
    /// `shrine5` is never a target at all. It carries zero `RouteTo` lines in 178 steps, which is
    /// what pins this on the fight filter rather than on `ok`'s route test — a candidate that
    /// survived to `ok` and failed there would have said so.
    ///
    /// ## What this reads, and what it therefore cannot claim
    ///
    /// The cache is the world **as the run left it**, so every node it cleared has lost the
    /// `— level N` from its heading — which is why `shrine3` and `shrine4` read fight-free here and
    /// very likely did not at step 14. That makes this the *most favourable* version of that world
    /// for the old rule, and it still fails: `shrine5` was never visited, so it alone kept its
    /// level, and it alone is what the nearest-shrine question turns on.
    ///
    /// The file is `map-cache/1519Z-world-0.txt`, a copy taken 2026-08-21 — because
    /// `map-cache/world-0.txt` is rewritten by every run, and a fresh profile writes a different
    /// world entirely. Both are gitignored, so this skips where they are absent, as the log replays
    /// do.
    #[test]
    fn the_nearest_shrine_of_the_1519z_world_is_one_the_old_filter_refused() {
        let Ok(text) = std::fs::read_to_string("map-cache/1519Z-world-0.txt") else {
            eprintln!("SKIP: map-cache/1519Z-world-0.txt is not present");
            return;
        };
        let mut m = WorldMap::new();
        assert!(m.absorb_cache(&text) > 1000, "expected the whole 1519Z world, not a fragment");
        m.here = Some("l50".into());
        m.hell = Some(0.1);

        // `shrine5` is the shrine that run never reached, so it is the one whose heading still
        // carries the level. If this ever stops holding, the file is a different world and every
        // number below is about something else.
        assert_eq!(
            m.get("shrine5").map(|p| p.heading.as_str()),
            Some("Harswell Coppice — level 7 forest"),
            "wrong world: `shrine5` is not the level 7 forest the 1519Z run left unvisited"
        );

        let d = m.distances("l50");
        let nearest = m
            .places
            .values()
            .filter(|p| p.is_shrine() && p.parent.is_none())
            .filter_map(|p| d.get(&p.key).map(|n| (*n, p.key.clone())))
            .min()
            .expect("the 1519Z world has shrines with routes to them");
        assert_eq!(nearest, (12, "shrine5".into()), "the nearest shrine, and by how much");
        assert_eq!(d.get("shrine3"), Some(&42), "the pair the run actually took");
        assert_eq!(d.get("shrine4"), Some(&42));

        // **A route exists**, so the route test that is still in `ok` is not what refused it.
        assert!(d.contains_key("shrine5"));
        // **And the filter that is gone did refuse it.** This is the removed predicate, spelled out:
        // `.filter(|p| !anomaly_open || self.reachable_without_a_fight(here, &p.key))`.
        assert!(
            !m.reachable_without_a_fight("l50", "shrine5"),
            "if this is fight-free the old filter kept it and #70 changed nothing here"
        );

        // Today it is the target, from the same place the run spent on exploring.
        let plan = m.next_target().expect("a world with an open portal and shrines has a target");
        assert_eq!(
            (plan.reason, plan.target.as_str()),
            (Goal::Shrine, "shrine5"),
            "the portal is open and the nearest shrine is 12 hops away"
        );
    }
}
