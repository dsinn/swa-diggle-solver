//! Arrival events — the beggar, the highwayman, the thugs, the woodsman, and the anomaly itself.
//!
//! Travelling is not just walking. Arriving somewhere can raise an event screen with a title, a
//! block of prose, and a set of choices, and the overworld does not come back until one is taken.
//! Any traversal loop that does not handle these will sit waiting for a map that is no longer on
//! screen.
//!
//! `ui/eventscreen.lua:140-145` announces them under `_VERBOSE`:
//!
//! ```text
//! Event:  Lost in the mists!
//! While travelling the misty road into Bainton Clump you suddenly become aware…
//! Choices = {
//!     {
//!         text = "Continue",
//!         posX = 960,
//!         posY = 745,
//!     },
//! }
//! ```
//!
//! Two properties make this tractable, and both come from the source rather than from hope:
//!
//! - **Only *active* choices are listed.** The `if _VERBOSE and isActive` guard (`:124`) means a
//!   choice we cannot afford or cannot take never appears, so anything in this list is clickable.
//! - **`table.repr` is `print(table.serialize(…))`** (`utils/table.lua:435-437`) — the same
//!   serializer that writes the save files. So the `Choices` block is Lua source, and
//!   [`crate::game::save::parse`] reads it directly rather than by inventing a second parser for a
//!   format we already understand.
//!
//! Positions are screen coordinates computed at print time (`buttonX*getWidth()`), so they are
//! aimable as-is — the same deal as the reward screen's item list.

use crate::game::save::parse;

/// One thing the player can do about an event. Present only if the game says it is active.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub text: String,
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub title: String,
    /// The prose. Kept because it is how an event is recognised when the title is generic.
    pub text: String,
    pub choices: Vec<Choice>,
}

impl Event {
    /// The choice that just continues, if there is one.
    ///
    /// Events raised by arrival — including the anomaly's "You feel the ground rumble" — offer a
    /// single `Continue` bound to `affirmative` (`overworld/events/arrived/world_evil.lua:26-29`).
    /// Matching on the text rather than assuming index 0 keeps a multi-choice event from being
    /// dismissed by accident.
    pub fn continue_choice(&self) -> Option<&Choice> {
        self.choices.iter().find(|c| {
            let t = c.text.trim().to_ascii_lowercase();
            t == "continue" || t == "ok" || t == "onwards"
        })
    }

    /// True when there is exactly one thing to do, so taking it needs no policy.
    pub fn is_forced(&self) -> bool {
        self.choices.len() == 1
    }

    /// The first choice that does no harm, which is the one to take by default.
    ///
    /// Taking `choices[0]` blindly is not safe. Some events — corrupted villages especially — offer
    /// attacking or killing a villager, and under the right conditions that option comes **first**.
    /// It is not recoverable by restoring a checkpoint if nobody notices, and it changes the run's
    /// karma and the world around it.
    ///
    /// The screening is by text, which is a blunt instrument in the safe direction: refusing a
    /// harmless option costs us an event, while taking a harmful one cannot be undone. If nothing
    /// survives, the caller is expected to leave the event alone rather than guess.
    pub fn safe_choice(&self) -> Option<&Choice> {
        self.choices.iter().find(|c| !harmful(&c.text))
    }

    /// [`safe_choice`], additionally declining to start a fight when `avoid_combat` is set.
    ///
    /// [`harmful`] screens on a **moral** axis — do not murder a villager — and that is the only axis
    /// it was ever built for. `[Combat] - Cut it down.` is not murder, so it passed, and a live run
    /// took it at **1 of 20 health**, walked into `Mourning wood`, and never came out
    /// (`spike-run-raw.log:59-73`). Tactical risk is a second axis and needs its own screen.
    ///
    /// ## The fallback, and why it is not simply `None`
    ///
    /// When every surviving option starts a fight, this takes one anyway rather than returning
    /// nothing. That is deliberate and it is the lesser of two bad outcomes: `handle_event`'s caller
    /// treats an unanswered event as *dismissed* and walks on to the map path, leaving the event
    /// dialogue on screen with no map underneath it — the exact stranding this whole area keeps
    /// producing. A fight we are likely to lose is recoverable from a checkpoint; a run wedged on a
    /// dialogue it will not answer burns its whole budget.
    ///
    /// The caller is expected to say so in the log when this happens, because it is a decision worth
    /// seeing rather than a default worth hiding.
    pub fn safe_choice_avoiding_combat(&self, avoid_combat: bool) -> Option<&Choice> {
        self.choices
            .iter()
            .find(|c| !harmful(&c.text) && !(avoid_combat && starts_combat(&c.text)))
            .or_else(|| self.safe_choice())
    }
}

/// Does this option start a fight?
///
/// The game tags them itself, so this reads a marker rather than guessing at prose. Choice text is
/// built as a table of coloured fragments with a literal `'[Combat]'` among them — 21 of them across
/// six event files, e.g. `overworld/events/arrived/hidden_path.lua:27`:
///
/// ```lua
/// {   text = {cost, '[Combat]', black, " - Cut it down."},
/// ```
///
/// The tag survives into the console's `Choices` block verbatim, which is where we read it. Related
/// markers exist and are deliberately not matched here: `[Murderer]` is [`harmful`]'s business, and
/// `[Curse]` is neither. `[Murderer][Combat]` carries both and is caught by both.
///
/// Substring rather than a parse, because the tags are concatenated with no separator and always
/// precede the prose. A choice whose *prose* contained the literal text `[Combat]` would be a false
/// positive; nothing in the game writes one, and the cost would be declining an event.
pub fn starts_combat(text: &str) -> bool {
    text.contains("[Combat]")
}

/// Does this option's text describe harming someone?
///
/// Deliberately over-broad. A false positive means an event goes unanswered and gets reported; a
/// false negative means the run murders a villager.
pub fn harmful(text: &str) -> bool {
    const WORDS: &[&str] = &[
        "kill", "murder", "slay", "attack", "strike", "stab", "shoot", "execute", "behead",
        "sacrifice", "rob", "steal", "loot", "burn", "betray", "threaten", "extort",
    ];
    let t = text.to_ascii_lowercase();
    let words: Vec<&str> = t.split(|c: char| !c.is_ascii_alphabetic()).filter(|w| !w.is_empty()).collect();
    words.iter().enumerate().any(|(i, w)| {
        WORDS.contains(w) && !reads_as_a_noun(words.get(i.wrapping_sub(1)).copied(), i)
    })
}

/// Is a harmful word being *named* rather than *done*?
///
/// The list above screens for actions we refuse to take, but the same words are ordinary nouns.
/// "Ask about the attack." is a question about something that already happened; refusing it is a
/// false positive that leaves a real event unanswered. The discriminator is the preceding word: a
/// determiner or preposition in front makes it a noun, and nothing else does.
///
/// Deliberately asymmetric. An unrecognised context stays *harmful*, so the list only ever loses its
/// grip on constructions explicitly named here — a false positive costs one unanswered event, a
/// false negative kills a villager.
fn reads_as_a_noun(previous: Option<&str>, index: usize) -> bool {
    // Nothing in front of it: an imperative, which is how every option we must refuse is phrased —
    // "Kill him.", "Attack the guard."
    if index == 0 {
        return false;
    }
    const NOUN_MARKERS: &[&str] = &[
        // Determiners.
        "the", "a", "an", "this", "that", "these", "those", "their", "his", "her", "its", "your",
        "my", "our", "no", "any",
        // Prepositions — "about the attack", "news of the murder", "after the raid".
        "about", "of", "from", "after", "before", "during", "for", "on", "in", "at",
    ];
    previous.is_some_and(|p| NOUN_MARKERS.contains(&p))
}

const START: &str = "Event:";
const CHOICES: &str = "Choices = {";

/// Every complete event block in these lines.
///
/// A block is complete only once its `Choices` table has closed; an event whose choices have not
/// arrived yet is skipped rather than reported with none, because "no choices" and "choices not
/// printed yet" would otherwise be indistinguishable — and acting on the first would hang the run.
pub fn parse_events(lines: &[String]) -> Vec<Event> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim_end();
        let Some(rest) = line.strip_prefix(START) else {
            i += 1;
            continue;
        };
        let title = rest.trim().to_string();
        let mut text = String::new();
        let mut j = i + 1;
        // Prose runs until the choices table opens. A second `Event:` before that means the first
        // never completed, so abandon it.
        while j < lines.len() && !lines[j].trim_end().starts_with(CHOICES) {
            if lines[j].trim_end().starts_with(START) {
                break;
            }
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(lines[j].trim());
            j += 1;
        }
        if j >= lines.len() || !lines[j].trim_end().starts_with(CHOICES) {
            i += 1;
            continue;
        }
        // The table is serialized at depth 0, so its closing brace is a bare `}` in column 0.
        let mut k = j + 1;
        while k < lines.len() && lines[k].trim_end() != "}" {
            k += 1;
        }
        if k >= lines.len() {
            break; // still arriving
        }
        let mut src = String::from("return {\n");
        for l in &lines[j + 1..k] {
            src.push_str(l);
            src.push('\n');
        }
        src.push_str("}\n");
        if let Ok(t) = parse(&src) {
            let choices: Vec<Choice> = t
                .arr
                .iter()
                .filter_map(|v| {
                    let c = v.as_table()?;
                    Some(Choice {
                        text: c.str_at("text").unwrap_or("").to_string(),
                        x: c.int_at("posX")? as i32,
                        y: c.int_at("posY")? as i32,
                    })
                })
                .collect();
            out.push(Event { title, text: text.trim().to_string(), choices });
        }
        i = k + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(|l| l.to_string()).collect()
    }

    const LOST: &str = r#"Event:  Lost in the mists!
While travelling the misty road into Bainton Clump you suddenly become aware that you've lost track of the path.
Choices = {
    {
        text = "Continue",
        posX = 960,
        posY = 745,
    },
}
"#;

    /// Verbatim from the run it cost. `spike-run-raw.log:59-72`, at 1 of 20 health.
    const STUMP: &str = r#"Event:  Stump in the road
One of the paths leaving the crossroads has a stump blocking the path.
Choices = {
    {
        posY = 594,
        text = "[Combat] - Cut it down.",
        posX = 960,
    },
    {
        posY = 756,
        text = "Leave.",
        posX = 960,
    },
}
"#;

    #[test]
    fn a_combat_option_is_refused_while_hurt_and_taken_when_healthy() {
        let e = parse_events(&lines(STUMP)).pop().expect("the event parses");
        // What actually happened: `harmful` has no "cut", so the moral screen passed it through and
        // the run started a fight it could not win.
        assert!(!harmful("[Combat] - Cut it down."), "the moral screen is not the one that catches this");
        assert_eq!(e.safe_choice().unwrap().text, "[Combat] - Cut it down.");

        // Hurt: take the way out instead.
        assert_eq!(e.safe_choice_avoiding_combat(true).unwrap().text, "Leave.");
        // Healthy: unchanged, because declining every fight would refuse most of the game.
        assert_eq!(e.safe_choice_avoiding_combat(false).unwrap().text, "[Combat] - Cut it down.");
    }

    #[test]
    fn the_combat_tag_is_read_wherever_it_sits_among_the_other_markers() {
        // `overworld/events/arrived/spider_forest.lua:62` and `dying_paladin.lua:259` both carry
        // `[Murderer][Combat]`; `hidden_path.lua:49` carries `[Combat][Curse]`. The tags concatenate
        // with no separator, so position cannot be assumed.
        assert!(starts_combat("[Combat] - Cut it down."));
        assert!(starts_combat(r#"[Murderer][Combat] - "Are you alone?""#));
        assert!(starts_combat(r#"[Combat][Curse] - "I have a way with wood.""#));
        assert!(!starts_combat("Leave."));
        assert!(!starts_combat("[Curse] - Read the book."));
        // `[Murderer]` alone is the moral screen's business, not this one.
        assert!(!starts_combat("[Murderer] - Attack them."));
    }

    /// When every option is a fight, one gets taken rather than the event being left unanswered.
    ///
    /// Pins the trade-off deliberately, because the safer-*looking* behaviour is the worse one: an
    /// unanswered event is treated as dismissed by the caller, which walks on to the map path with a
    /// dialogue still on screen and no map beneath it.
    #[test]
    fn a_fight_is_still_taken_when_every_option_is_one() {
        let src = r#"Event:  Ambushed
They are already drawing steel.
Choices = {
    {
        posY = 594,
        text = "[Combat] - Stand and fight.",
        posX = 960,
    },
    {
        posY = 756,
        text = "[Combat] - Charge them.",
        posX = 960,
    },
}
"#;
        let e = parse_events(&lines(src)).pop().expect("the event parses");
        let picked = e.safe_choice_avoiding_combat(true).expect("something must be picked");
        assert!(
            starts_combat(&picked.text),
            "with no non-combat option, the fallback must still answer the dialogue"
        );
    }

    #[test]
    fn reads_a_single_choice_event() {
        let evs = parse_events(&lines(LOST));
        assert_eq!(evs.len(), 1);
        let e = &evs[0];
        assert_eq!(e.title, "Lost in the mists!");
        assert!(e.text.starts_with("While travelling"));
        assert_eq!(e.choices, vec![Choice { text: "Continue".into(), x: 960, y: 745 }]);
        assert!(e.is_forced());
        assert_eq!(e.continue_choice().unwrap().x, 960);
    }

    #[test]
    fn reads_several_choices_and_does_not_treat_it_as_forced() {
        // A highwayman-style event: paying and refusing are different decisions, and dismissing it
        // by grabbing the first button would make that decision silently.
        let src = r#"Event:  A highwayman blocks the road
He wants coin.
Choices = {
    {
        text = "Pay 50 gold",
        posX = 960,
        posY = 745,
    },
    {
        text = "Refuse",
        posX = 960,
        posY = 907,
    },
}
"#;
        let e = &parse_events(&lines(src))[0];
        assert_eq!(e.choices.len(), 2);
        assert!(!e.is_forced());
        assert_eq!(e.continue_choice(), None, "neither option is a plain continue");
        assert_eq!(e.choices[1].y, 907);
    }

    #[test]
    fn the_villager_is_never_the_first_resort() {
        // A corrupted village can offer violence, and under the right conditions it comes FIRST --
        // which is precisely what taking `choices[0]` would have done.
        let src = r#"Event:  A frightened villager
He begs for help.
Choices = {
    {
        text = "Kill him",
        posX = 960,
        posY = 745,
    },
    {
        text = "Help him",
        posX = 960,
        posY = 907,
    },
}
"#;
        let e = &parse_events(&lines(src))[0];
        assert_eq!(e.choices[0].text, "Kill him", "the harmful option really is first");
        assert_eq!(e.safe_choice().map(|c| c.text.as_str()), Some("Help him"));
    }

    #[test]
    fn screening_is_blunt_in_the_safe_direction() {
        assert!(harmful("Attack the guard"));
        assert!(harmful("Rob them"));
        assert!(harmful("Kill him"));
        // Substrings must not trigger it -- "skill" is not "kill", and refusing every option would
        // leave the run unable to answer anything.
        // Named, not done. Both seen live at the Ulrome gate, where refusing the first left the
        // event unanswered and stalled the run at a screen it could have walked through.
        assert!(!harmful("Ask about the attack."));
        assert!(!harmful("Ask why they aren't defending."));
        // The same noun still bites when it is an imperative, which is the case that matters.
        assert!(harmful("Attack."));
        assert!(harmful("Join in and attack"));
        // A preposition in front is what marks it as a noun — but only in front of the word itself.
        assert!(!harmful("Ask about the murder"));
        assert!(harmful("Try to murder him"));
        assert!(!harmful("Test your skill"));
        assert!(!harmful("Continue"));
        assert!(!harmful("Pay 50 gold"));
    }

    #[test]
    fn an_event_offering_only_harm_is_left_alone() {
        let src = r#"Event:  Ambush
No good options.
Choices = {
    {
        text = "Attack",
        posX = 960,
        posY = 745,
    },
}
"#;
        let e = &parse_events(&lines(src))[0];
        assert!(e.is_forced());
        assert_eq!(e.safe_choice(), None, "forced does not mean acceptable");
    }

    #[test]
    fn an_incomplete_block_is_not_reported() {
        // The console is polled, so a block routinely arrives split. Reporting an event with zero
        // choices would be indistinguishable from one that genuinely offers nothing, and the loop
        // would act on it.
        let partial = r#"Event:  Lost in the mists!
Some prose.
Choices = {
    {
        text = "Continue",
"#;
        assert!(parse_events(&lines(partial)).is_empty());
    }

    #[test]
    fn surrounding_console_noise_is_ignored() {
        let mut src = String::from("Local overworld data:\tWorld loaded\tl1\tsomewhere\n");
        src.push_str(LOST);
        src.push_str("ach_lostWoods\talready achieved\n");
        assert_eq!(parse_events(&lines(&src)).len(), 1);
    }

    #[test]
    fn two_events_in_one_batch_both_come_back() {
        let src = format!("{LOST}{LOST}");
        assert_eq!(parse_events(&lines(&src)).len(), 2);
    }
}
