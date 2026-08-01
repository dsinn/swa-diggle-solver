//! Enemies that leave rather than die.
//!
//! Some enemies quit once they are hurt enough, and pushing them over that line is cheaper than
//! killing them — it ends the fight sooner and, on the same turn, stops them hitting back.
//!
//! ## The three thresholds
//!
//! `rpgview.lua:1643-1646` — the enemy runs if it carries one of these and is not `immobile`:
//!
//! ```lua
//! currentEnemy.statusEffects.fear    and currentEnemy.health*2 < currentEnemy.maxHealth
//! or currentEnemy.statusEffects.terror  and currentEnemy.health ~= currentEnemy.maxHealth
//! or currentEnemy.statusEffects.caution and currentEnemy.health == 1
//! ```
//!
//! A cultist carries `fear = -1` (`rpg/enemies/humans.lua:54-58`), so it is the strictly-below-half
//! case. `terror` leaves at the first scratch, `caution` only at one health.
//!
//! ## Why it is worth aiming for
//!
//! The same condition appears in `enemyCanHit` (`:1046-1052`) under the comment `-- Enemy runs
//! away`, computed against the health the enemy *would* have. So the turn that pushes it past the
//! threshold is also a turn it does not get to attack. Fleeing is both faster and safer than
//! grinding the last half of its health away.
//!
//! ## The catch, which is why this does not simply always apply
//!
//! A fled enemy is counted as **skipped** (`stats.addSkippedEnemies`, `:1647`), and its completion
//! flag is only credited when the scenario says so:
//!
//! ```lua
//! if areaData.scenario.fleeingCountsForCompletion and currentEnemy.completionFlag then
//! ```
//!
//! Completion is not cosmetic for us: `canTravelToDirect` refuses to step off an incomplete node
//! (`overworldview.lua:1316-1321`), which is exactly what pins a run inside a corrupted village.
//!
//! **In a corrupted village this does not block us** — a ruling from the game's author, and the
//! source agrees for ordinary attackers. Defending completes a node by `kills` exhausting that
//! node's share of the attackers (`overworld/generators/village.lua:40-52`); `completionFlag` is not
//! consulted on that path at all.
//!
//! The one place it *is* consulted is the boss entry:
//!
//! ```lua
//! if runSaveData.rpg.player.newFlags.completionFlag and ...completionFlag>=1 then
//!     attackData[location.key..'_boss']=nil
//! end
//! ```
//!
//! and `village.lua` never sets `fleeingCountsForCompletion`, so a *boss* frightened off would not
//! clear its entry and the node would stay incomplete. Ordinary attackers are safe to scare;
//! something holding a `_boss` slot should be killed. We cannot see that flag from outside, so the
//! caller carries the choice.

/// Which quitting rule an enemy is under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nerve {
    /// `fear` — leaves once strictly under half health.
    Fear,
    /// `terror` — leaves at the first point of damage.
    Terror,
    /// `caution` — leaves at exactly one health.
    Caution,
}

impl Nerve {
    /// Reads the status name the game uses.
    pub fn from_status(name: &str) -> Option<Nerve> {
        match name {
            "fear" => Some(Nerve::Fear),
            "terror" => Some(Nerve::Terror),
            "caution" => Some(Nerve::Caution),
            _ => None,
        }
    }

    /// Would an enemy left on `health` walk away?
    ///
    /// Transcribed from `rpgview.lua:1644-1646` rather than paraphrased, because each threshold is
    /// a different comparison and "about half" would be wrong for all three.
    pub fn would_leave(self, health: i64, max_health: i64) -> bool {
        if health <= 0 {
            // Dead, not fled. The game guards this with the `death`/`deathHit` state check at :1643.
            return false;
        }
        match self {
            Nerve::Fear => health * 2 < max_health,
            Nerve::Terror => health != max_health,
            Nerve::Caution => health == 1,
        }
    }

    /// The most health we may leave an enemy on and still have it quit, if any.
    pub fn leaves_at_or_below(self, max_health: i64) -> Option<i64> {
        let top = match self {
            // Strictly less than half: for an odd max, integer division already lands below.
            Nerve::Fear => (max_health - 1) / 2,
            Nerve::Terror => max_health - 1,
            Nerve::Caution => 1,
        };
        (top >= 1).then_some(top)
    }
}

/// An enemy we are deciding how to hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Enemy {
    pub health: i64,
    pub max_health: i64,
    /// `None` when it has no quitting status, or when it is `immobile` — which suppresses the
    /// behaviour entirely (`rpgview.lua:1646`).
    pub nerve: Option<Nerve>,
}

/// Damage that would make this enemy quit without killing it, if such a hit exists.
///
/// Returns the *smallest* qualifying damage: the point of aiming for a flee is to spend as little as
/// possible on an enemy we are not going to kill.
pub fn damage_to_scare(e: Enemy) -> Option<i64> {
    let nerve = e.nerve?;
    let top = nerve.leaves_at_or_below(e.max_health)?;
    if e.health <= top {
        // Already past the line; it should be leaving of its own accord.
        return Some(0);
    }
    let needed = e.health - top;
    // Must not kill: a corpse is not a deserter, and killing is the outcome the caller was
    // presumably already scoring for.
    (needed < e.health).then_some(needed)
}

/// Picks the first answer that scares the enemy off, in the caller's preference order.
///
/// "First" rather than "biggest": anything past the threshold is wasted, and the enemy leaves either
/// way. Returns `None` when nothing qualifies, leaving the caller on its normal rules.
pub fn choose_to_scare(damages: &[i64], e: Enemy) -> Option<usize> {
    let needed = damage_to_scare(e)?;
    damages.iter().position(|&d| d >= needed && d < e.health)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CULTIST: Enemy = Enemy { health: 12, max_health: 12, nerve: Some(Nerve::Fear) };

    #[test]
    fn the_cultists_threshold_is_strictly_below_half() {
        // health*2 < maxHealth, so 6 of 12 is NOT below half and does not flee; 5 does.
        assert!(!Nerve::Fear.would_leave(6, 12));
        assert!(Nerve::Fear.would_leave(5, 12));
        // Odd max: 3 of 7 flees (6 < 7), 4 does not (8 !< 7).
        assert!(Nerve::Fear.would_leave(3, 7));
        assert!(!Nerve::Fear.would_leave(4, 7));
    }

    #[test]
    fn terror_leaves_at_the_first_scratch_and_caution_only_at_one() {
        assert!(Nerve::Terror.would_leave(11, 12));
        assert!(!Nerve::Terror.would_leave(12, 12));
        assert!(Nerve::Caution.would_leave(1, 12));
        assert!(!Nerve::Caution.would_leave(2, 12));
    }

    #[test]
    fn a_dead_enemy_has_not_fled() {
        // The game checks the death state before the flee branch (:1643); without this the two
        // outcomes would be conflated and a kill would be reported as a skip.
        assert!(!Nerve::Fear.would_leave(0, 12));
        assert!(!Nerve::Terror.would_leave(-3, 12));
    }

    #[test]
    fn scaring_a_full_health_cultist_costs_seven_of_twelve() {
        // Must reach 5 or less, from 12.
        assert_eq!(damage_to_scare(CULTIST), Some(7));
    }

    #[test]
    fn an_enemy_already_under_the_line_needs_nothing() {
        let hurt = Enemy { health: 4, ..CULTIST };
        assert_eq!(damage_to_scare(hurt), Some(0));
    }

    #[test]
    fn no_status_means_no_shortcut() {
        assert_eq!(damage_to_scare(Enemy { nerve: None, ..CULTIST }), None);
    }

    #[test]
    fn caution_on_a_one_health_maximum_cannot_be_scared_without_killing() {
        // leaves_at_or_below is 1, and the enemy is already at 1 -- there is no non-lethal hit.
        let tiny = Enemy { health: 1, max_health: 1, nerve: Some(Nerve::Caution) };
        assert_eq!(damage_to_scare(tiny), Some(0));
        // But from a max of 1 with full health there is no room: any hit kills.
        assert_eq!(Nerve::Caution.leaves_at_or_below(1), Some(1));
    }

    #[test]
    fn the_first_sufficient_answer_wins_not_the_biggest() {
        // Needs 7, must stay under 12. The 20 would kill; the 9 is more than necessary but comes
        // first, and the enemy leaves either way.
        let damages = [3, 20, 9, 7];
        assert_eq!(choose_to_scare(&damages, CULTIST), Some(2));
    }

    #[test]
    fn nothing_qualifies_when_every_answer_kills_or_falls_short() {
        let damages = [1, 2, 30];
        assert_eq!(choose_to_scare(&damages, CULTIST), None);
    }

    #[test]
    fn status_names_come_straight_from_the_game() {
        assert_eq!(Nerve::from_status("fear"), Some(Nerve::Fear));
        assert_eq!(Nerve::from_status("terror"), Some(Nerve::Terror));
        assert_eq!(Nerve::from_status("caution"), Some(Nerve::Caution));
        assert_eq!(Nerve::from_status("bleed"), None);
    }
}

/// Should we prefer scaring this enemy off over killing it?
///
/// Yes whenever it can be frightened and is human. A cultist carries
/// `onDeathFlags = {'Murder', 'Bleeds'}` (`rpg/enemies/humans.lua:58`) — killing one is recorded as
/// murder, and it is avoidable, because the same enemy carries `fear`. The point is not squeamishness
/// about a fight we would win; it is that the kill has a cost the flee does not, and the flee is
/// also cheaper in damage and denies the enemy its attack.
///
/// The target is then the FIRST answer leaving it under the threshold and **still alive** — which is
/// what [`choose_to_scare`] picks, since it requires `damage < health`.
pub fn prefer_scaring(e: Enemy, human: bool) -> bool {
    human && e.nerve.is_some()
}

#[cfg(test)]
mod human_tests {
    use super::*;

    #[test]
    fn a_human_that_can_be_feared_is_scared_not_killed() {
        let cultist = Enemy { health: 12, max_health: 12, nerve: Some(Nerve::Fear) };
        assert!(prefer_scaring(cultist, true));
        // A skeleton with the same status is not a murder, so scaring is merely an option.
        assert!(!prefer_scaring(cultist, false));
        // A human with no nerve has to be killed; there is nothing else on offer.
        assert!(!prefer_scaring(Enemy { nerve: None, ..cultist }, true));
    }

    #[test]
    fn the_chosen_answer_leaves_a_cultist_alive_and_under_half() {
        let cultist = Enemy { health: 12, max_health: 12, nerve: Some(Nerve::Fear) };
        // 12 kills, 6 leaves it on 6 which is NOT under half, 7 leaves it on 5 which is.
        let damages = [12, 6, 7, 9];
        let i = choose_to_scare(&damages, cultist).unwrap();
        assert_eq!(damages[i], 7, "first answer that is enough and not lethal");
        let left = cultist.health - damages[i];
        assert!(left > 0, "still alive");
        assert!(Nerve::Fear.would_leave(left, cultist.max_health), "and under the threshold");
    }
}
