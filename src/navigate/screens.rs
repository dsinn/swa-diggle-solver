//! The screen vocabulary: what the game can be showing, and what that means for the run.
//!
//! Lifted out of `navigate` on 2026-08-21 (#76). Four things that answer one question between
//! them — *the game is showing me this; now what?*
//!
//! - [`Doorway`] answers it at the main menu, before there is a run at all.
//! - [`ESCAPES`] answers it for the dead ends: one screen, one button, done.
//! - [`answer_for`] answers it for **every** [`Screen`], exhaustively, so a new variant cannot be
//!   added without somebody deciding what happens when the game shows it.
//! - [`precheck`] is the one line of that map [`drive`](super::drive) consults first.
//!
//! None of them touches a window, which is why they are the part of the driver with real tests.
//! #19 (find buttons with the cursor oracle) adds screens here rather than to the loop.

use super::Stop;
// Both are linked from the doc comments below and neither is called from here; they stay in the
// parent, and rustdoc needs them in scope for `[`Run::doorway`]` and `[`drive`]` to resolve.
#[allow(unused_imports)]
use super::{drive, Run};
use crate::act::{Button, Screen};

/// What is in front of a run at the moment it starts looking. See [`Run::doorway`].
///
/// A value rather than a ladder of `if`s, for the reason task #38 gives about arrivals: a ladder
/// restates its preconditions at every rung and its order is an accident of where each check was
/// added. Startup is small enough to be written the right way now — three questions, one answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Doorway {
    /// A text screen's continue arrow. Whatever is behind it cannot be read until it is gone.
    Text,
    /// `Continue` — there is a save, and a run to resume.
    Resume,
    /// `Start` — no save, so this is a fresh adventure. **Not** `Restart`, which eulogises.
    Fresh,
    /// None of the three. The splash is still up, or something we have no fingerprint for is.
    Nothing,
}

impl Doorway {
    /// The ordering, separated from the reading so it can be asserted without a game.
    ///
    /// The same split `act::identify_from_scores` uses, for the same reason: which screen wins when
    /// two fingerprints are live is a *decision*, and a decision that can only be exercised by
    /// launching the game is a decision nothing checks.
    ///
    /// **Text first.** A lore card covers the menu, so asking about `Continue` while one is up is
    /// asking about a button behind a picture. Everything downstream already works this way — *text
    /// gates options* at an arrival — and a run that pressed the menu through a card would be
    /// pressing a coordinate rather than a button.
    ///
    /// **Resume before fresh**, because they are mutually exclusive in the game
    /// (`ui/startmenu.lua:186,199` — `Continue` is `activeIf mainSaveData`, and the slot beside it
    /// says `Restart` in exactly that case) and taking the resume first means a save is never
    /// stepped over.
    pub fn from_readings(text: bool, resume: bool, fresh: impl FnOnce() -> bool) -> Doorway {
        if text {
            Doorway::Text
        } else if resume {
            Doorway::Resume
        } else if fresh() {
            Doorway::Fresh
        } else {
            Doorway::Nothing
        }
    }
}

pub struct Escape {
    /// What [`crate::act::identify`] calls it.
    pub screen: Screen,
    pub button: &'static Button,
    pub threshold: f64,
    /// Names the screen in the log line and in the failure, e.g. `the shrine screen`.
    pub what: &'static str,
}

/// Every dead-end screen, and how to get off it.
///
/// The prose on each entry is the reason it exists. Two of the three were reached *by accident* by a
/// live run, and the failure each produced pointed somewhere else entirely — which is worth more to
/// the next reader than the four lines of clicking it replaces.
pub const ESCAPES: &[Escape] = &[
    // From here the area-button coordinate means `Stats`, so a run that does not notice presses
    // `Stats` instead of travelling. The way out is the back arrow in the bottom-right corner.
    Escape {
        screen: Screen::Character,
        button: &crate::act::CHARACTER_BACK,
        threshold: crate::act::CHARACTER_BACK_PRESENT,
        what: "the character screen",
    },
    // Reached by accident straight after finishing a shrine and clearing the text screen behind it.
    // Like the character screen it is a dead end with no map, and the failure it produced was
    // thoroughly misleading: four locate-me probes at 0.15 and a stop reading `no pan dump after
    // locate-me`, which describes the map being unreadable rather than absent, and sends you looking
    // at panning.
    Escape {
        screen: Screen::StatsHistory,
        button: &crate::act::STATS_BACK,
        threshold: crate::act::STATS_BACK_PRESENT,
        what: "the stats history page",
    },
    // **Not an exit — the one entry here that presses on rather than backs out.**
    //
    // A fight at a corrupted shrine ends leaving the shrine screen up with `Consecrate` lit, and
    // live 2026-08-12 the run pressed back instead: the classifier had no check for the button, so
    // the back plaque answered first and the offer went unseen. `Consecrate` is only ever drawn when
    // it will do something -- `majorShrine and hell ~= 0` (`shrine.lua:92-95`) -- so finding it lit
    // on a screen we are already standing on means the trip and any fight are paid for and the only
    // thing left is to take it.
    //
    // The mechanism is the same as every other escape: press the button, look again. What differs is
    // the intent, and the reason it can share the mechanism is that pressing `Consecrate` also
    // leaves — `shrine.lua:288` ends in `setActiveMode(overworld)`.
    // **The `Consecrate` entry was removed on 2026-08-15, and the premise above is why.**
    //
    // "`Consecrate` is only ever drawn when it will do something" is not what the source says. The
    // gate is `showConsecrateButton` = `ShowAGoodButton() and majorShrine` (`shrine.lua:92-95`), and
    // `ShowAGoodButton()` is `hasWon() and not heretic` (`:36-40`) — so the button is drawn **greyed
    // until the shrine's word is solved**, and being on screen says nothing about being pressable.
    //
    // Live 2026-08-15 at `shrine1`: this entry fired, logged `left the shrine screen, by
    // consecrating it`, and one step later the shrine driver found the slot at **0.8564** — which
    // `SHRINE_CONSECRATE_PRESENT`'s own table names as the greyed state — and reported
    // `shrine: left unconsecrated`. The run then believed it had failed, and because `worth_a_trip`
    // is `!p.used` and consecrating never sets `used`, it walked four hops to `shrine2` and on
    // toward `shrine6`, out of the corruption it was supposed to be closing.
    //
    // Consecration belongs to the shrine driver, which solves the word first and therefore knows the
    // button is earned before it presses. One actor per action: an escape route exists to get *off*
    // a screen, and the moment it also performs the objective, two things race for the same button
    // and the loser reports a failure that did not happen.
    // Normally the moment after `Pray`, where the slot now holds a greyed `Consecrate` and the only
    // thing left to do is leave. `shrineplay::play` deliberately stops at the Pray press and hands
    // the aftermath back here, so this is the ordinary exit rather than an error path.
    Escape {
        screen: Screen::Shrine,
        button: &crate::act::SHRINE_GOBACK,
        threshold: crate::act::SHRINE_GOBACK_PRESENT,
        what: "the shrine screen",
    },
    // **The same exit, for the screen that names the button.**
    //
    // Removing the old `Consecrate` entry was right and left a hole: `identify` reports
    // `ShrineConsecrate` in preference to `Shrine` whenever the slot is occupied, so the more
    // specific variant shadowed the escape above and the run had no answer for a screen it was
    // standing on. `answer_for` said `Elsewhere("the shrine driver")`, but the shrine driver enters
    // from the *map* by pressing `Visit` — it has no way to take over a screen we are already on.
    //
    // Live 2026-08-15, restored at `shrine1` mid-fight: the fight finished, the game left the shrine
    // screen up, and the run spent every step alternating `ShrineConsecrate` -> locate-me clicks
    // that landed on the shrine's own chrome -> `StatsHistory` -> back -> `ShrineConsecrate`, until
    // it stopped with `no pan dump after locate-me, 4 times over`. Seven steps, no progress, and a
    // stop message about panning.
    //
    // **This presses `Go back`, not `Consecrate`,** which is the whole difference from the entry
    // that was removed. Leaving is always safe and always available; taking the consecration
    // requires knowing the word is solved, which only the shrine driver knows. The invariant holds —
    // nothing outside that driver presses `Consecrate` — and the screen stops being a trap.
    Escape {
        screen: Screen::ShrineConsecrate,
        button: &crate::act::SHRINE_GOBACK,
        threshold: crate::act::SHRINE_GOBACK_PRESENT,
        what: "the shrine screen, leaving the slot alone",
    },
];

/// Where a recognised screen gets answered.
///
/// The point of naming this at all is [`answer_for`]'s `match`, which is exhaustive: a variant added
/// to [`Screen`] does not compile until somebody decides what happens when the game shows it.
///
/// That is the check this project was missing. `Screen` had no in-combat variant, so a run that
/// entered a fight from an overworld event fell through to "assume map" and spent its whole budget
/// probing for one; the fix cost a live run and a dead character to find. Under an exhaustive match
/// the same omission is a build failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// One of the [`ESCAPES`]: press its button and look again.
    Escape,
    /// [`drive`] hands it to [`crate::fight::Fight::run`].
    Fight,
    /// [`drive`] has a branch of its own for it.
    Bespoke,
    /// Answered before `drive`'s screen check ever sees it. The string names what does it, so that
    /// "handled" is a claim with an address rather than a shrug.
    Elsewhere(&'static str),
    /// The ordinary case: no fingerprint, so get on with planning a hop.
    Map,
    /// Recognised by [`crate::act::identify`] and answered by nobody.
    ///
    /// Not a placeholder to be filled in silently — [`tests::the_unanswered_screens_are_only_the_ones
    /// _we_have_admitted_to`] pins the list, so shrinking it is deliberate and growing it is loud.
    Unanswered,
}

/// What answers each screen, as of now.
///
/// Exhaustive by construction. Prefer `Elsewhere` with a name over `Bespoke` when the answer is not
/// in [`drive`] at all: several screens never reach the screen check because something upstream has
/// already dealt with them, and recording *where* is the difference between this being a map and
/// being a wish.
pub const fn answer_for(screen: Screen) -> Answer {
    match screen {
        // `Shrine` is the escape **fallback**, and still has to be: the check behind it is "there is
        // a back plaque here", which has fired on an inn. But `drive` looks first, and if the *save*
        // says a shrine underfoot is worth consecrating it plays it where it stands rather than
        // pressing back — see the branch above `ESCAPES`. The escape is what happens when the save
        // says there is nothing here, which is the only case it was ever right for.
        Screen::Character | Screen::StatsHistory | Screen::Shrine => Answer::Escape,
        // Not escaped any more: pressing `Consecrate` is the shrine driver's job, because only it
        // has solved the word that makes the button pressable. See the note where its `Escape` used
        // to be.
        Screen::ShrineConsecrate => Answer::Escape,
        // `drive` buys at it and leaves; see the store arrival branch. Not an escape, because
        // leaving before buying is the one outcome the whole trip was against.
        Screen::Shop => Answer::Bespoke,
        // The one `drive` plays out itself, because nothing else is watching for it.
        Screen::CombatEntered => Answer::Fight,
        // Reached through the affirmative slot rather than through `identify`: `drive` waits on
        // `Finish` and hands the fight over on `waited.found()`.
        Screen::CombatWaiting => Answer::Elsewhere("drive's wait on the Finish slot"),
        // Both are inside a fight's own loop -- `take_reward` for the item screen, and the postgame
        // dismissal after it. `drive` never sees either.
        Screen::ItemChoice => Answer::Elsewhere("Fight::take_reward"),
        Screen::Postgame => Answer::Elsewhere("Fight::run, after the reward"),
        // Clicked through by `start_new_run`, which knows it is there because `Pregame screen:`
        // arrives on the console afterwards. It is never identified by sight.
        Screen::HeroSelect => Answer::Elsewhere("start_new_run"),
        Screen::MainMenu | Screen::Pregame | Screen::Unlock => Answer::Bespoke,
        Screen::Unknown => Answer::Map,
        // Death has no answer in the navigator. `Outcome::Died` catches it from the console *during*
        // a fight (`fight.rs`), which is where it has always happened so far -- but a run standing on
        // a death screen outside one would fall through to the map path and probe for a map that is
        // not there. The fingerprint for it exists (`slot_is_eulogise`); nothing consults it.
        //
        // Left honest rather than quietly mapped to `Bespoke`. It is a real gap and the test below
        // makes sure it stays visible.
        Screen::Dead => Answer::Unanswered,
    }
}

/// What [`drive`] must do about a screen before anything else looks at it.
///
/// Split out as a pure function so the wiring is testable: without it, "an unanswered screen stops
/// the run" would be a claim about a loop that needs a live game to enter. [`answer_for`] is a map,
/// and a map nothing reads is a comment.
///
/// Only [`Answer::Unanswered`] stops. [`Answer::Elsewhere`] deliberately does not: seeing one of
/// those in `drive` means the component that owns it has already finished — a reward screen still up
/// after a fight, say — and those resolve on the next iteration rather than being errors.
pub fn precheck(screen: Screen) -> Option<Stop> {
    match answer_for(screen) {
        Answer::Unanswered => Some(Stop::Unanswered(screen)),
        Answer::Escape | Answer::Fight | Answer::Bespoke | Answer::Elsewhere(_) | Answer::Map => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A text screen outranks the menu behind it**, which is the whole reason the startup path
    /// is a dispatch rather than two scores in a row.
    ///
    /// The run of 2026-08-16 is the case: a fresh profile put the auto-save card in front of the
    /// menu, the menu checks were asked first, and the run aborted reporting `Start` at 0.0570 — a
    /// true reading of a button that was behind a picture.
    #[test]
    fn a_text_screen_is_answered_before_anything_it_is_covering() {
        // The failing run's state: a card up, and whatever the menu templates say beneath it.
        assert_eq!(Doorway::from_readings(true, true, || true), Doorway::Text);
        assert_eq!(Doorway::from_readings(true, false, || false), Doorway::Text);
    }

    /// A save is never stepped over. `Continue` and `Start` cannot both be live —
    /// `ui/startmenu.lua:186,199` — but if a reading ever said so, resuming is the safe half:
    /// the slot beside `Continue` reads `Restart` in that state, and pressing it eulogises the run.
    #[test]
    fn resuming_outranks_starting_over() {
        assert_eq!(Doorway::from_readings(false, true, || true), Doorway::Resume);
        assert_eq!(Doorway::from_readings(false, false, || true), Doorway::Fresh);
    }

    /// Nothing recognised is an ordinary answer, not a failure: the publisher splash is several
    /// seconds long and nothing is pressable during it.
    #[test]
    fn recognising_nothing_is_a_state_and_not_an_error() {
        assert_eq!(Doorway::from_readings(false, false, || false), Doorway::Nothing);
    }

    /// The later reads are not paid for once an earlier one has answered.
    #[test]
    fn the_menu_is_not_scored_when_a_text_screen_is_up() {
        let mut asked = false;
        let _ = Doorway::from_readings(true, false, || {
            asked = true;
            true
        });
        assert!(!asked, "scoring `Start` behind a lore card is work spent on a wrong answer");
    }

    /// `Screen::ALL` has to actually list every variant, or every test below silently checks less
    /// than it claims.
    ///
    /// The compiler cannot prove this — Rust will not enumerate an enum — so what it proves instead
    /// is the half that matters: `answer_for` is an exhaustive match, so a new variant cannot reach
    /// this test without someone having decided what answers it. This catches the *other* omission,
    /// forgetting to add it here afterwards, by requiring the two to agree in size.
    #[test]
    fn every_screen_is_listed_exactly_once() {
        let mut seen: Vec<Screen> = Vec::new();
        for &s in Screen::ALL {
            assert!(!seen.contains(&s), "{s:?} appears twice in Screen::ALL");
            seen.push(s);
        }
        // Every escape's screen must be in the list, which is the cheapest way to notice the list
        // going stale in the direction that matters.
        for e in ESCAPES {
            assert!(Screen::ALL.contains(&e.screen), "{:?} is escapable but not in Screen::ALL", e.screen);
        }
    }

    /// Both ways into a fight must be answered by the loop, not by whoever pressed the button.
    ///
    /// This is the invariant the chest broke. The pregame and the first turn of combat are the two
    /// states an area button can produce, and for as long as the caller handled them itself it could
    /// only recognise the door it knew about — so `Open`, which announces nothing, stalled a run with
    /// the board already in the feed.
    ///
    /// [`Answer::Elsewhere`] is the failure this guards against: it means "some other component owns
    /// this", which for these two would put the knowledge back at a call site. A run can arrive on
    /// either screen without having pressed anything, so neither has a single owner to give it to.
    #[test]
    fn the_two_faces_of_entering_combat_are_both_answered_by_the_loop() {
        for s in [Screen::Pregame, Screen::CombatEntered] {
            let answer = answer_for(s);
            assert!(
                matches!(answer, Answer::Bespoke | Answer::Fight),
                "{s:?} must be handled at the top of `drive`, not delegated: got {answer:?}"
            );
            assert!(precheck(s).is_none(), "{s:?} must not stop the run");
        }
    }

    /// The escape table and the answer map must agree about which screens are escapable.
    ///
    /// They are two statements of the same fact written in different places, which is exactly the
    /// shape that drifts. Either one alone would be believed.
    #[test]
    fn the_escape_table_and_the_answer_map_agree() {
        for e in ESCAPES {
            assert_eq!(
                answer_for(e.screen),
                Answer::Escape,
                "{:?} is in ESCAPES but answer_for does not call it an escape",
                e.screen
            );
        }
        for &s in Screen::ALL {
            if answer_for(s) == Answer::Escape {
                assert!(
                    ESCAPES.iter().any(|e| e.screen == s),
                    "{s:?} is answered as an escape but has no ESCAPES entry, so `drive` will fall \
                     through it to the map path"
                );
            }
        }
    }

    /// No two escapes may claim the same screen: the lookup takes the first and the second would be
    /// dead code that reads as though it were live.
    #[test]
    fn no_two_escapes_claim_the_same_screen() {
        for (i, a) in ESCAPES.iter().enumerate() {
            for b in &ESCAPES[i + 1..] {
                assert_ne!(a.screen, b.screen, "{:?} is claimed twice", a.screen);
            }
        }
    }

    /// Every escape's button must be in `act::ALL`, which is what subjects it to the registry's own
    /// invariants — chiefly that its search box can contain its template, the mismatch that once let
    /// a button measure perfectly offline while never matching in a live run.
    #[test]
    fn a_lit_consecrate_is_answered_by_pressing_it_not_by_leaving() {
        use crate::act::{Screen, SHRINE_CONSECRATE, SHRINE_GOBACK};

        // Both plaques are on the shrine screen at the same time, in slots that do not overlap --
        // `Consecrate` on the right, the back arrow on the left. So `identify` can satisfy either
        // check on the same frame and the ONLY thing separating them is which is asked first. That
        // is what makes the ordering load-bearing rather than incidental, and on 2026-08-12 the back
        // plaque won and a consecration was thrown away.
        let (cx0, _, cx1, _) = SHRINE_CONSECRATE.search;
        let (bx0, _, bx1, _) = SHRINE_GOBACK.search;
        assert!(cx0 > bx1 || bx0 > cx1, "the two slots must be distinct for both to be present");

        // And **nothing on the escape path may press it**. The button is drawn greyed until the
        // shrine's word is solved (`ShowAGoodButton()`, `shrine.lua:36-40`), so an actor that has
        // not solved it cannot know whether pressing does anything — which is how a consecration
        // came to be reported twice, once as done and once as failed. The shrine driver owns it.
        //
        // The screen still needs a way out, which is the part the first version of this got wrong.
        // Removing the entry altogether left `ShrineConsecrate` with no answer at all, and since
        // `identify` prefers it to `Shrine` whenever the slot is occupied, it shadowed the ordinary
        // shrine escape and trapped a run for its whole length. So: an escape, pressing **Go back**.
        let entry = ESCAPES
            .iter()
            .find(|e| e.screen == Screen::ShrineConsecrate)
            .expect("the screen must have a way out or it is a trap");
        assert_eq!(
            entry.button.name, SHRINE_GOBACK.name,
            "leaving is always safe; pressing `Consecrate` needs a solved word and only the shrine              driver knows that"
        );
        assert!(matches!(answer_for(Screen::ShrineConsecrate), Answer::Escape));
    }

    #[test]
    fn every_escape_button_is_in_the_registry() {
        for e in ESCAPES {
            assert!(
                crate::act::ALL.iter().any(|b| std::ptr::eq(*b, e.button)),
                "{} is escaped by a button outside act::ALL",
                e.what
            );
        }
    }

    /// **And the other direction, which is the one that traps runs.**
    ///
    /// `answer_for` is exhaustive over [`Screen`], so a new variant cannot be added without somebody
    /// deciding what happens. That check has a blind side: deciding `Answer::Escape` and then not
    /// writing the [`ESCAPES`] entry compiles, passes every test, and produces a screen the run
    /// recognises and cannot leave.
    ///
    /// Which is not hypothetical. `ShrineConsecrate` was moved off `Escape` on 2026-08-15 for a good
    /// reason and back onto it later the same day for a better one; in between, `identify` preferred
    /// it to `Shrine` whenever the slot was occupied, so it shadowed the ordinary shrine exit and a
    /// live run spent its whole length bouncing between that screen and the stats page.
    ///
    /// Two lists agreeing by hand is the shape of half the faults in this file. This is the pair that
    /// can be checked, so it is checked.
    #[test]
    fn every_screen_that_says_escape_has_somewhere_to_go() {
        for &s in Screen::ALL {
            if matches!(answer_for(s), Answer::Escape) {
                assert!(
                    ESCAPES.iter().any(|e| e.screen == s),
                    "{s:?} is answered by escaping and has no escape — it is a trap"
                );
            }
        }
    }

    /// The screens nothing answers, written down.
    ///
    /// This is the test the project actually needed. `Screen` had no in-combat variant, so a run that
    /// entered a fight from an overworld event fell through to "assume map", spent its budget probing
    /// for one, and the gap was found by a live run and a dead character. A list that has to be
    /// edited on purpose turns that into a failing assertion.
    ///
    /// Shrinking this list is the goal. Growing it should require saying so here.
    #[test]
    fn the_unanswered_screens_are_only_the_ones_we_have_admitted_to() {
        let unanswered: Vec<Screen> =
            Screen::ALL.iter().copied().filter(|&s| answer_for(s) == Answer::Unanswered).collect();
        assert_eq!(
            unanswered,
            vec![Screen::Dead],
            "the set of screens nothing answers has changed; if that is deliberate, say so here"
        );
    }

    /// The map has to be *read* by something, or it is a comment that looks like a control.
    ///
    /// This is the half [`answer_for`] could not give on its own: it can be exhaustive and correct
    /// and still change nothing about what the run does. `precheck` is what `drive` actually calls,
    /// so testing it tests the wiring rather than the intention.
    #[test]
    fn an_unanswered_screen_stops_the_run_and_every_other_screen_does_not() {
        for &s in Screen::ALL {
            match answer_for(s) {
                Answer::Unanswered => assert_eq!(
                    precheck(s),
                    Some(Stop::Unanswered(s)),
                    "{s:?} is unanswered but `drive` would carry on past it"
                ),
                _ => assert!(
                    precheck(s).is_none(),
                    "{s:?} has an answer, so `drive` must not stop on it"
                ),
            }
        }
    }

    /// `Elsewhere` screens must never stop the run.
    ///
    /// Worth its own assertion rather than leaving it to the sweep above, because it is the tempting
    /// mistake: they are not handled *here*, which reads like "not handled". Seeing one in `drive`
    /// means the component that owns it has just finished — a reward screen still up after a fight —
    /// and the next iteration clears it. Stopping would turn ordinary transitions into dead runs.
    #[test]
    fn a_screen_answered_elsewhere_is_not_a_reason_to_stop() {
        let elsewhere: Vec<Screen> = Screen::ALL
            .iter()
            .copied()
            .filter(|&s| matches!(answer_for(s), Answer::Elsewhere(_)))
            .collect();
        assert!(!elsewhere.is_empty(), "the sweep is vacuous if nothing is answered elsewhere");
        for s in elsewhere {
            assert!(precheck(s).is_none(), "{s:?} is answered elsewhere but `drive` stops on it");
        }
    }
}
