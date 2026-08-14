//! Choosing an answer when overkill is worth something.
//!
//! Normally the best answer is the highest-scoring one. Carrying **well rested** changes that,
//! because killing an enemy with more damage than it has left converts the excess into healing —
//! and spends a charge doing it.
//!
//! ## The mechanic, from `rpgview.lua`
//!
//! On the killing blow (`:1078-1088`):
//!
//! ```lua
//! estimates[player].heal = estimates[player].heal
//!     + math.floor((estimates[currentEnemy].damage - currentEnemy.health)/2)
//! ```
//!
//! So the heal is **half the overkill, rounded down** — not the whole of it. It applies only when
//! the enemy dies, is blocked outright by `bleed`, and is cancelled by `overkillNoHeal` or
//! `overkillHealToGold` (a curse that pays gold instead, `items/curses.lua:111`).
//!
//! The charge is spent at `:1204-1210`:
//!
//! ```lua
//! if gain>0 then
//!     if player.statusEffects.wellRestedCampfire then affectPlayerStatus('wellRestedCampfire', -1)
//!     elseif player.statusEffects.wellRestedInn then affectPlayerStatus('wellRestedInn', -1)
//! end
//! ```
//!
//! **Only when the heal is positive.** That is the detail worth having: since the heal is
//! `floor(overkill/2)`, an overkill of **1 heals nothing and costs nothing**. Aiming for exact
//! damage is therefore stricter than the mechanic requires — the free band is `overkill <= 1`, which
//! is twice as many answers to choose from.
//!
//! ## The policy
//!
//! A charge is worth the same whether it heals 1 or 20, so spending one to top up a scratch is the
//! waste to avoid. Hence: barely hurt, keep the charge; badly hurt, spend it on a full heal; if no
//! answer heals fully, the charge buys less than a good turn is worth, so score normally.

/// Health, as the run knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vitals {
    pub current: i64,
    pub max: i64,
}

impl Vitals {
    pub fn missing(&self) -> i64 {
        (self.max - self.current).max(0)
    }

    /// Is more than half of our health gone? The threshold for spending a charge.
    ///
    /// Strictly more, so exactly half missing is still "barely hurt" and keeps the charge.
    pub fn badly_hurt(&self) -> bool {
        self.missing() * 2 > self.max
    }
}

/// What the turn is trying to achieve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aim {
    /// Highest score. The default, and what full health or no charge always means.
    Best,
    /// Kill without triggering a heal, to keep the charge. Overkill of 0 or 1.
    Frugal,
    /// Spend a charge on a full top-up, taking the first answer that manages it.
    HealFully,
}

/// Heal from a killing blow, per `rpgview.lua:1086`.
///
/// Zero when the blow does not kill, and zero for an overkill of 1 — the floored halving is the
/// whole reason a near-exact answer is free.
pub fn heal_from(damage: i64, enemy_health: i64) -> i64 {
    if damage < enemy_health {
        return 0;
    }
    (damage - enemy_health) / 2
}

/// Would this answer spend a well-rested charge?
pub fn spends_a_charge(damage: i64, enemy_health: i64) -> bool {
    heal_from(damage, enemy_health) > 0
}

/// Charges to keep banked. Above this, hoarding has nothing left to protect.
///
/// One. The whole argument for frugality is that a charge is scarce, and scarcity is a claim about
/// the *last* one — a run holding several is not protecting a resource, it is declining to use one.
///
/// [`FREE`] is deliberately far above this: a gear flag that heals without spending has no last
/// charge to keep.
pub const KEEP_IN_RESERVE: i64 = 1;

/// Enough missing health for a charge to buy something worth having.
///
/// Two, because the heal is `floor(overkill/2)`: topping up a single point is the "spending a charge
/// on a scratch" this module was written to avoid, and that objection survives having plenty.
const WORTH_A_CHARGE: i64 = 2;

/// Passed as `charges` when overkill heals without spending anything — a gear flag rather than a
/// status (`rpgview.lua:1204-1210` decrements only `statusEffects`).
pub const FREE: i64 = i64::MAX;

/// What to aim for this turn.
///
/// `charges` is how many `wellRested*` stacks are banked, [`FREE`] when a gear flag grants the heal
/// outright, and 0 when overkill will not heal at all — the caller folds in the gear flags and the
/// `overkillNoHeal` / `overkillHealToGold` cancellations, because those are properties of the
/// loadout rather than of the choice. `bleeding` is separate because it blocks the heal at
/// `rpgview.lua:1080` while leaving the charge untouched, so a bleeding turn should simply play
/// normally.
///
/// ## Why the count matters, and what taking a bool cost
///
/// This used to ask only *whether* a charge existed, so one stack and four were the same input. The
/// policy above it — barely hurt, keep the charge — is a scarcity argument, and it kept being
/// applied to a run that was not short of charges.
///
/// Live 2026-08-14: **12/20 health with ×4 banked**, and every kill for the whole run reported
/// `keeps the rest charge`. `badly_hurt` is `missing * 2 > max`, and `8 * 2 = 16` is not over 20, so
/// 40% health counted as barely hurt while four charges went unspent into a level 8 fight. Both
/// halves of that were wrong, and only one of them is a threshold.
pub fn aim(v: Vitals, charges: i64, bleeding: bool) -> Aim {
    if charges <= 0 || bleeding || v.missing() == 0 {
        return Aim::Best;
    }
    // Badly hurt spends whatever it has, down to the last charge: at that point the run is closer to
    // ending than the charge is to being needed later.
    if v.badly_hurt() {
        return Aim::HealFully;
    }
    // Not badly hurt, but holding more than the reserve — so spending one costs nothing we are
    // keeping. This is the case a bool could not see.
    match charges > KEEP_IN_RESERVE && v.missing() >= WORTH_A_CHARGE {
        true => Aim::HealFully,
        false => Aim::Frugal,
    }
}

/// One answer under consideration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Answer {
    /// Damage this answer would deal.
    pub damage: i64,
    /// What the normal scorer thinks of it.
    pub score: i64,
}

/// Picks an answer for `aim`, given the candidates in the order the solver ranks them.
///
/// "First" means first in the given order, which is the caller's preference order — so a tie is
/// broken toward whatever the scorer already liked. Returns `None` only when there are no
/// candidates at all: every aim falls back to the best-scoring answer rather than refusing a turn,
/// because a turn not taken is worse than a turn taken imperfectly.
pub fn choose(candidates: &[Answer], aim: Aim, enemy_health: i64, v: Vitals) -> Option<Answer> {
    let best = || candidates.iter().max_by_key(|a| a.score).copied();
    match aim {
        Aim::Best => best(),
        // A kill that heals nothing keeps the charge; among those, still take the best score.
        // Non-killing answers are left in: not every turn can or should be the killing blow.
        Aim::Frugal => candidates
            .iter()
            .filter(|a| !spends_a_charge(a.damage, enemy_health))
            .max_by_key(|a| a.score)
            .copied()
            .or_else(best),
        // First that covers the deficit, in the caller's order -- deliberately not the biggest heal.
        // Overkill beyond a full top-up is wasted, and the charge costs the same either way.
        Aim::HealFully => candidates
            .iter()
            .find(|a| heal_from(a.damage, enemy_health) >= v.missing())
            .copied()
            .or_else(best),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HURT: Vitals = Vitals { current: 10, max: 12 };
    const BADLY: Vitals = Vitals { current: 4, max: 12 };
    const FULL: Vitals = Vitals { current: 12, max: 12 };

    #[test]
    fn the_heal_is_half_the_overkill_rounded_down() {
        // rpgview.lua:1086. Exactly the floored halving -- the reason an overkill of 1 is free.
        assert_eq!(heal_from(10, 10), 0);
        assert_eq!(heal_from(11, 10), 0);
        assert_eq!(heal_from(12, 10), 1);
        assert_eq!(heal_from(20, 10), 5);
        // Not a kill, so no heal at all.
        assert_eq!(heal_from(9, 10), 0);
    }

    #[test]
    fn one_point_of_overkill_costs_nothing() {
        // The charge is spent only when `gain>0` (rpgview.lua:1204). Aiming for exact damage is
        // therefore stricter than the game requires, and needlessly discards good answers.
        assert!(!spends_a_charge(10, 10));
        assert!(!spends_a_charge(11, 10));
        assert!(spends_a_charge(12, 10));
    }

    /// The last charge — the state every one of these tests used to describe, when the input was a
    /// bool and "a charge" could only mean one.
    const LAST: i64 = 1;

    #[test]
    fn full_health_plays_normally() {
        assert_eq!(aim(FULL, LAST, false), Aim::Best);
        // Nor does having plenty invent a reason to spend one.
        assert_eq!(aim(FULL, 4, false), Aim::Best);
    }

    #[test]
    fn a_scratch_keeps_the_last_charge() {
        assert_eq!(aim(HURT, LAST, false), Aim::Frugal);
        // Exactly half missing is still a scratch: the threshold is strictly more than half.
        assert_eq!(aim(Vitals { current: 6, max: 12 }, LAST, false), Aim::Frugal);
    }

    #[test]
    fn badly_hurt_spends_the_charge() {
        assert_eq!(aim(BADLY, LAST, false), Aim::HealFully);
    }

    #[test]
    fn bleeding_plays_normally_because_the_heal_is_blocked() {
        // rpgview.lua:1080 skips the whole heal branch while bleeding, so there is nothing to
        // protect and nothing to spend.
        assert_eq!(aim(BADLY, LAST, true), Aim::Best);
    }

    #[test]
    fn without_a_charge_nothing_changes() {
        assert_eq!(aim(BADLY, 0, false), Aim::Best);
    }

    /// The live state of 2026-08-14, and the reason the count is an input at all.
    #[test]
    fn a_stack_of_charges_is_spent_rather_than_guarded() {
        // 12/20 with four banked. `badly_hurt` is false — `8 * 2` is not over 20 — so the old rule
        // called this a scratch and every kill of that run reported `keeps the rest charge`.
        let live = Vitals { current: 12, max: 20 };
        assert!(!live.badly_hurt(), "the premise: this does not read as badly hurt");
        assert_eq!(aim(live, 4, false), Aim::HealFully);

        // The control, and the whole point of `KEEP_IN_RESERVE`: the same wound with one charge
        // left still keeps it. Only the count differs between these two lines.
        assert_eq!(aim(live, KEEP_IN_RESERVE, false), Aim::Frugal);

        // A gear flag spends nothing, so it is never hoarded.
        assert_eq!(aim(live, FREE, false), Aim::HealFully);
    }

    #[test]
    fn plenty_of_charges_still_will_not_pay_for_a_single_point() {
        // Spending a charge to heal one point is the waste this module was written about, and
        // having four does not make it worth doing. `WORTH_A_CHARGE` is what holds that line.
        assert_eq!(aim(Vitals { current: 19, max: 20 }, 4, false), Aim::Frugal);
        assert_eq!(aim(Vitals { current: 18, max: 20 }, 4, false), Aim::HealFully);
    }

    #[test]
    fn frugal_takes_the_best_answer_that_does_not_heal() {
        // Enemy on 10. The 30-damage answer scores best but would heal 10 and burn the charge for a
        // 2-point top-up; the 11-damage answer overkills by 1, heals nothing, and is free.
        let c = [
            Answer { damage: 30, score: 50 },
            Answer { damage: 11, score: 20 },
            Answer { damage: 4, score: 5 },
        ];
        let picked = choose(&c, Aim::Frugal, 10, HURT).unwrap();
        assert_eq!(picked.damage, 11);
    }

    #[test]
    fn frugal_still_swings_when_every_answer_would_heal() {
        // Refusing to act would be worse than spending a charge.
        let c = [Answer { damage: 30, score: 50 }, Answer { damage: 20, score: 10 }];
        assert_eq!(choose(&c, Aim::Frugal, 10, HURT).unwrap().score, 50);
    }

    #[test]
    fn heal_fully_takes_the_first_that_covers_the_deficit() {
        // Missing 8, enemy on 10, so a full heal needs 16 overkill => 26 damage. The 40 would heal
        // 15 -- seven of it wasted, at the same cost in charges.
        let c = [
            Answer { damage: 12, score: 90 },
            Answer { damage: 26, score: 40 },
            Answer { damage: 40, score: 80 },
        ];
        let picked = choose(&c, Aim::HealFully, 10, BADLY).unwrap();
        assert_eq!(picked.damage, 26);
    }

    #[test]
    fn heal_fully_falls_back_to_the_best_score() {
        // Nothing reaches a full top-up, so the charge buys less than a good turn is worth.
        let c = [Answer { damage: 12, score: 90 }, Answer { damage: 14, score: 30 }];
        assert_eq!(choose(&c, Aim::HealFully, 10, BADLY).unwrap().score, 90);
    }

    #[test]
    fn no_candidates_is_the_only_refusal() {
        assert_eq!(choose(&[], Aim::Best, 10, FULL), None);
    }
}
