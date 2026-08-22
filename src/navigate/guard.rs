//! Sterile revisits: how often the run has stood somewhere with nothing gained in between.
//!
//! Lifted out of `navigate` on 2026-08-21 (#76), unchanged. It is the one piece of the driver that
//! needs no screen — a map from node to visit count and a [`crate::overworld::Progress`] to compare
//! against — which is why it was written as its own type in the first place, and what makes it the
//! cheapest thing here to move.

// Named by the doc comments below, and staying in the parent.
#[allow(unused_imports)]
use super::Run;

/// How many times a node may be stood on, with nothing gained in between, before the run gives up.
///
/// Four, and the number is chosen from the three loops we have actually met rather than from taste.
/// Every one of them was a two- or three-node cycle, so four visits to the same node is at least one
/// full lap after the first repeat — enough that a legitimate there-and-back cannot trip it, few
/// enough that a loop is caught in under a minute instead of over an evening.
///
/// Legitimate revisits do happen: a crossing probes, a steer overshoots, a village is re-entered for
/// a second errand. All of them *achieve* something — a node learned, a shop emptied, a place
/// written off — and any achievement clears the counts entirely. What this counts is specifically
/// revisiting with the run no further along than before.
pub(super) const LOOP_GIVE_UP: usize = 4;

/// Sterile revisits after which a **frontier** is written off rather than merely counted.
///
/// Two, because two is the first visit that *demonstrates* a repeat instead of suspecting one — the
/// same reading of the counter [`LOOP_GIVE_UP`] already uses, taken one lap earlier.
///
/// ## The loop this exists for, and why the guards already in place could not stop it
///
/// Run of 2026-08-16 1043Z, inside `l4`, ten crossings and then the give-up:
///
/// ```text
///   at l4sub14  ->  Crossing::Steer  ->  l4sub23      (the door is printed here)
///   at l4sub23  ->  Crossing::Probe  ->  l4sub14      (no route to the door from here)
/// ```
///
/// **Two different rules, each correct, pointing at each other.** The steer leg was protected by
/// `steered_gap`, a high-water mark that only ever fell, so a steer could not be re-earned by
/// walking backwards — the `l40` fix of 2026-08-15. It said nothing about the *other* leg, and the
/// frontier walk is what keeps electing `l4sub14`.
///
/// **Both the arm and its measure are gone as of #57 (2026-08-21)**, folded into one ranking with a
/// `doorward` term, so the trace above can no longer be produced — a single procedure has no other
/// leg to point at. This entry is left as written because the *second* failure it describes is
/// untouched by that and is what this constant is for.
///
/// It elects it forever because `Place::is_frontier` is `!visited || hidden > 0`, and **standing on
/// a node does not lower `hidden`** when the game goes on withholding a neighbour. So a node can be
/// permanently "somewhere the map might still open up" while having nothing left to give. The
/// frontier walk's own guarantee — BFS distance to the chosen frontier strictly decreases — is about
/// *reaching* it, and holds perfectly well while the run learns nothing.
///
/// The answer is the one `docs/superpowers/notes/navigation-loops.md` gives for every loop here:
/// memory, not a ranking. `WorldMap::abandon` is that memory, it already exists, and every chooser
/// that matters honours it — the crossing's `usable` filter, the frontier search, target selection,
/// and `committed_exit`. What was missing was anything that ever *wrote* to it for this reason.
///
/// **Frontiers only, deliberately.** A node we keep returning to for some other reason may well be a
/// road we have to pass through, and `step_avoiding` routes around abandoned nodes — writing one off
/// could cut the only path. A frontier's whole appeal is that it might teach us something, so a
/// frontier that has twice taught us nothing is exactly the thing that can be given up with nothing
/// lost. Anything else still runs into [`LOOP_GIVE_UP`], which is unchanged and still the backstop.
///
/// **One case this does not break, and the test says so out loud.** `cross_toward` ends in an
/// unfiltered pick — `place.neighbours.iter().min()` — because standing still in a subworld is worse
/// than any one bad step. So when the written-off node is the *only* neighbour, the crossing takes
/// it regardless and the cycle continues to [`LOOP_GIVE_UP`], exactly as it did before. That is the
/// right trade and not an oversight: the alternative is a run that stops with somewhere to go.
pub(super) const LOOP_WRITE_OFF: usize = 2;

/// Counts sterile revisits: how often we have stood somewhere with the run no further along.
///
/// Its own type rather than two fields on [`Run`] so the rule can be tested without a game window,
/// which is the difference between a guard with a regression test and a guard nobody can exercise
/// until it misfires live.
#[derive(Debug, Default)]
pub struct LoopGuard {
    seen: std::collections::HashMap<String, usize>,
    last: Option<crate::overworld::Progress>,
}

impl LoopGuard {
    /// Record standing at `here` with the run having achieved `now`, and say how many times this
    /// node has been stood on **since the run last achieved anything**.
    ///
    /// Zero only on the very first call and on any call where `now` has moved — there is nothing to
    /// compare against at those points. Every later arrival counts, so a two-node cycle reaches
    /// [`LOOP_GIVE_UP`] on its second full lap.
    ///
    /// **Any change to `now` clears every count.** That is the monotone half of the rule: real
    /// progress cannot be undone by walking, so a count cleared by an achievement can never be
    /// re-earned by going round the same circle again. It is also what keeps honest work off the
    /// counter — a crossing that probes, a village re-entered for a second errand, a corridor walked
    /// twice all *achieve* something, and anything achieved resets this to zero.
    pub fn visit(&mut self, here: &str, now: crate::overworld::Progress) -> usize {
        if self.last != Some(now) {
            self.last = Some(now);
            self.seen.clear();
            return 0;
        }
        let n = self.seen.entry(here.to_string()).or_insert(0);
        *n += 1;
        *n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guard that should have caught all three ping-pongs, against the shape all three had.
    ///
    /// `l19`↔`l10`, `shrine2`↔`l10`, `l18`↔`l10` — 2026-08-15 and 16, three different root causes,
    /// none of them noticed by the program. Each was spotted by the dev watching the screen.
    ///
    /// The cycle here is the last one: `l18 -> l10 -> l18 -> …`, with nothing achieved on any lap.
    #[test]
    fn a_circle_with_nothing_gained_is_noticed_and_an_honest_walk_is_not() {
        let nothing = crate::overworld::Progress {
            known: 128,
            completed: 57,
            consecrated: 1,
            used: 2,
            gold: 294,
            portal_open: true,
            shops_emptied: 4,
            written_off: 3,
        };

        let mut g = LoopGuard::default();
        // The very first call only establishes the baseline — there is nothing yet to compare to.
        assert_eq!(g.visit("l18", nothing), 0, "the first look sets the baseline");
        // After that every arrival counts, because every one of them is an arrival with nothing
        // gained since the last. A two-node cycle therefore trips on its second full lap.
        let mut laps = 0;
        for _ in 0..4 {
            g.visit("l10", nothing);
            laps = g.visit("l18", nothing);
        }
        assert!(laps >= LOOP_GIVE_UP, "four sterile visits must trip the guard, got {laps}");

        // **And an honest walk is not a loop, however much it revisits.** A crossing probes, backs
        // up and tries another way; what makes it honest is that it *learns* — each new node folded
        // in moves `known`. Any change at all wipes the slate.
        let mut g = LoopGuard::default();
        let mut learning = nothing;
        for i in 0..10 {
            learning.known += 1;
            assert_eq!(g.visit("l4_plaza", learning), 0, "still learning on pass {i}");
        }

        // The slate stays wiped only while progress keeps coming. Stopping is what counts.
        let mut g = LoopGuard::default();
        let mut once = nothing;
        once.completed += 1;
        assert_eq!(g.visit("l9", once), 0, "a fight won clears everything");
        assert_eq!(g.visit("l9", once), 1, "and standing still afterwards starts counting again");
    }
}
