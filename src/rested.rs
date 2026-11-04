//! Choosing an answer when overkill is worth something.
//!
//! Normally the best answer is the highest-scoring one. Carrying **well rested** changes that,
//! because killing an enemy with more damage than it has left converts the excess into healing —
//! and spends a stack doing it.
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
//! Only when the heal is positive, so an overkill of 1 heals nothing and costs nothing.
//!
//! ## The policy
//!
//! Hurt, and the heal is live: aim for a kill that heals fully; if no answer does, score normally.
//! Stacks are **not** conserved. There used to be a "frugal kill" that aimed for an overkill of 0
//! or 1 to keep a stack on a scratch; it was dropped on 2026-11-04 because its arithmetic kept
//! missing in practice, and fixing it meant reworking the scoring.

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
}

/// What the turn is trying to achieve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aim {
    /// Highest score. What full health or a blocked heal means.
    Best,
    /// Spend a stack on a full top-up, taking the first answer that manages it.
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

/// What to aim for this turn.
///
/// The caller only asks when overkill heals at all — it folds in the gear flags and the
/// `overkillNoHeal` / `overkillHealToGold` cancellations. `bleeding` is separate because it blocks
/// the heal at `rpgview.lua:1080`, so a bleeding turn should simply play normally.
pub fn aim(v: Vitals, bleeding: bool) -> Aim {
    match bleeding || v.missing() == 0 {
        true => Aim::Best,
        false => Aim::HealFully,
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
        // First that covers the deficit, in the caller's order -- deliberately not the biggest heal.
        // Overkill beyond a full top-up is wasted, and the stack costs the same either way.
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
    fn full_health_plays_normally() {
        assert_eq!(aim(FULL, false), Aim::Best);
    }

    #[test]
    fn any_wound_aims_to_heal_fully() {
        // No stack is kept in reserve: a scratch is worth topping up too.
        assert_eq!(aim(HURT, false), Aim::HealFully);
        assert_eq!(aim(BADLY, false), Aim::HealFully);
    }

    #[test]
    fn bleeding_plays_normally_because_the_heal_is_blocked() {
        // rpgview.lua:1080 skips the whole heal branch while bleeding.
        assert_eq!(aim(BADLY, true), Aim::Best);
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
