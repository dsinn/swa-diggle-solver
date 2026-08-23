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
//! Positions are screen coordinates computed at print time — but they are **not aimable as-is**,
//! which is what this comment used to claim. `buttonX*getWidth()` is the button's *anchor* and omits
//! `xOffset`, so on an event with a portrait it lands on the plaque's left edge, where the hit test
//! rejects it. [`Choice::click_point`] has the derivation and the live evidence. The `posX = 960`
//! above is the centred layout, where anchor and centre coincide — which is why the mistake survived
//! this long.

use crate::game::save::parse;

/// One thing the player can do about an event. Present only if the game says it is active.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub text: String,
    /// The **anchor** the console printed, which is not the button's centre. See [`Choice::click_point`].
    pub x: i32,
    pub y: i32,
}

impl Choice {
    /// Where to actually click, which is not where the console says the choice is.
    ///
    /// `ui/eventscreen.lua:126-128` prints
    ///
    /// ```lua
    /// posX = buttonX*love.graphics.getWidth(),
    /// posY = buttonY*love.graphics.getHeight(),
    /// ```
    ///
    /// — the screen-space anchor, with **`xOffset` left out**. The button's real centre is
    /// `ss_x*W + s*w*os_x` ([`crate::win::window::button_center`]), so the printed figure is short by
    /// a full half-width whenever `xOffset` is nonzero.
    ///
    /// There are exactly two layouts (`:117-118`), and which one is used turns on whether the event
    /// has a portrait:
    ///
    /// ```text
    ///   img:     buttonX = 0.075, xOffset = 0.5   printed 144, centre 644, left edge 144
    ///   no img:  buttonX = 0.5,   xOffset = 0     printed 960, centre 960, left edge 460
    /// ```
    ///
    /// So on a **portrait** event the printed coordinate is the plaque's own left edge — and
    /// `ui/elements/button.lua:93` tests `x > 0 and y > 0 and x < width and y < height`, strictly, so
    /// a click landing at local x = 0 is rejected. Off by one pixel, and a full 500 from the middle.
    ///
    /// This is why every event worked until one did not. The doc example at the top of this module,
    /// `posX = 960`, is the centred layout, where anchor and centre coincide by accident. A live run
    /// met the Woodsman — the project's first portrait event — clicked its left edge four times, and
    /// each time `act::EVENT_CHOICE` scored the plaque at **1.0000**, still there. Before that check
    /// existed the run had simply logged `answered` and walked on.
    pub fn click_point(&self, client_w: i32, client_h: i32) -> (i32, i32) {
        let s = crate::win::window::raw_scale(client_w, client_h);
        let w = client_w as f64;
        let anchor = self.x as f64;
        // Which layout printed this? Nearest of the two candidates, rather than a magic cut point --
        // both are exact multiples of the width, so they are never close together.
        let portrait = (anchor - 0.075 * w).abs() < (anchor - 0.5 * w).abs();
        // `event` buttons are 1000 wide (`ui/elements/button.lua:18`).
        let x = if portrait { anchor + s * 1000.0 * 0.5 } else { anchor };
        // `y` needs no correction: the layout passes no `yOffset`, so the anchor is the centre.
        (x.round() as i32, self.y)
    }
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
        self.choices
            .iter()
            .find(|c| !harmful(&c.text) && !murderous(&c.text) && !costs_everything(&c.text))
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
            .find(|c| {
                !harmful(&c.text)
                    // **Who the fight is with**, which is a different question from whether there is
                    // one: see [`murderous`] and the dev's ruling quoted there. This is the screen
                    // that the dying paladin got past, and it is not conditional on our health —
                    // being at full strength is not a licence.
                    && !murderous(&c.text)
                    && !costs_everything(&c.text)
                    // Priced goods, screened out with the same standing as being robbed: see
                    // [`costs_gold`]. The purse belongs to the heart errand and the inn.
                    && !costs_gold(&c.text)
                    && !(avoid_combat && starts_combat(&c.text))
            })
            .or_else(|| self.safe_choice())
            // Nothing left that is neither harmful nor ruinous, so the fight it is. This is the
            // highwayman: "your money or your life", with the money screened out.
            //
            // **And a fight with a highwayman is not a fight to the death.** `Goal::for_enemy`
            // returns `Goal::Scare` when the enemy has nerve to break, so the search aims for a
            // damage band that routs them rather than a kill — the dev's rule for the MVP, and
            // already built. Nothing here needs to ask for it; declining to pay is enough to reach
            // the code that does.
            // **And never a murder, even here.** The highwayman's fight is the case this arm was
            // written for and it stays available; what may not happen is reaching the end of the
            // options and settling on the one the game tags `[Murderer]`. If that leaves nothing,
            // the caller reports an unanswered event, which is recoverable — the alternative is not.
            .or_else(|| self.choices.iter().find(|c| starts_combat(&c.text) && !murderous(&c.text)))
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
/// Does this choice hand over everything we own?
///
/// The highwayman's first option, `[-All gold] - Your money`
/// (`overworld/events/arrived/highwaymen.lua:37`), whose handler is
/// `cost = -overworld.getPlayerGold()` — the whole purse, not a price.
///
/// Screened because a live run took it. `harmful` covers *moral* harm and `starts_combat` covers
/// tactical risk; being robbed blind is neither, so it read as the safe option and 763 gold went in
/// one keystroke. The damage is not the gold as such: the run was walking to `l19 Dane village` to
/// rest, and an inn charges ten (`crate::rest::INN_COST`). The guard that kept the character alive
/// spent the thing the journey was for.
///
/// Matched on the `[-All gold]` tag rather than on prose. The game builds choice text from coloured
/// fragments and that bracketed tag is one of them, so it is as close to a machine-readable marker
/// as this interface offers — and it does not need translating, unlike "Your money".
pub fn costs_everything(text: &str) -> bool {
    text.contains("[-All gold]")
}

/// Does this choice want gold for goods — the travelling merchant and his like?
///
/// **We do not shop at events.** The dev's rule, 2026-08-15, after a run paid the merchant:
/// *when we encounter the merchant that sells us items with -500 gold, -300 gold, and -200 options,
/// simply refuse to buy anything.*
///
/// Live that day at Scorborough Spinny, verbatim off the console:
///
/// ```text
/// ["[-500g] - Get two random gear items.",
///  "[-300g] - Pick one of two random gear items.",
///  "[-200g] - Get one random gear item.",
///  "Leave."]
/// ```
///
/// The run took the first and five hundred gold went on random gear. That is five hearts, or fifty
/// nights at an inn, for items nothing in this program knows how to evaluate — and the purse is
/// what the heart errand and the rest errand both spend. `Leave.` is free, always last, and always
/// there.
///
/// Matched on the price tag rather than on prose, the same way [`costs_everything`] is: the game
/// builds choice text from coloured segments and the bracketed cost is the part that is structural.
/// A bare `[-…g]` or `[-… gold]` is a price; anything else is left to the other predicates.
pub fn costs_gold(text: &str) -> bool {
    let mut rest = text;
    while let Some(open) = rest.find("[-") {
        let after = &rest[open + 2..];
        let Some(close) = after.find(']') else { break };
        let tag = after[..close].trim();
        let digits = tag.trim_end_matches(|c: char| c.is_ascii_alphabetic() || c.is_whitespace());
        let unit = tag[digits.len()..].trim().to_ascii_lowercase();
        if !digits.is_empty()
            && digits.chars().all(|c| c.is_ascii_digit())
            && (unit == "g" || unit == "gold")
        {
            return true;
        }
        rest = &after[close..];
    }
    false
}

/// Does this option **murder someone who is not fighting us**?
///
/// The dev's rule, 2026-08-23, after a run killed a dying paladin: *I only want to avoid murder.
/// Combat in other dialogues, as long as it's not for murdering a human being (such as the
/// woodsman), should be fine. The crossroads' tree, the highwayman, and the muggers are examples of
/// acceptable combat.* So this is a screen on **who**, not on whether a fight starts —
/// [`starts_combat`] stays a separate axis and is no longer a reason to decline on its own.
///
/// ## Why [`harmful`] cannot do this job, proved by the event that got past it
///
/// `dying_paladin.lua` offers the same murder twice, and which one you are shown depends on your
/// gear rather than on the act:
///
/// ```lua
/// text = {cost, '[Combat]',           black, ' - "Blood. The book hungers for blood."'},  -- :245
/// text = {cost, '[Murderer][Combat]', black, ' - Attack them.'},                          -- :259
/// ```
///
/// **Both call `scenarios.paladinEvent(location)`.** `:249`'s `showIf` is
/// `not playerHasGearFlag'curseCollectBlood'`, so a run carrying that curse — as the 0547Z run did —
/// is shown only the untagged one, whose prose contains no verb `harmful` knows. It was taken at
/// 52/52 health, because the only thing that had ever declined a fight was being hurt.
///
/// `spider_forest.lua:62,73` is the same pair around the **woodsman**, an NPC who sells antivenom
/// and warns you about the spiders: `[Murderer][Combat] - "Are you alone?"` and
/// `[Combat] - "The book hungers for blood."`, both calling `scenarios.woodsmanEvent`.
///
/// ## The three things it screens, and each is enumerated from the game rather than guessed
///
/// - **The `[Murderer]` tag.** The game's own declaration, and `harmful` never saw it: the word
///   split turns `[Murderer]` into `murderer`, and the list holds `murder`. So
///   `spider_forest.lua:62`'s `- "Are you alone?"` was tagged as murder and read as harmless.
/// - **Blood-lust prose**, which is how every `curseCollectBlood` variant is worded. All five in the
///   game, each verified to call an attack scenario:
///   `dying_paladin:245` *"Blood. The book hungers for blood."* (paladinEvent),
///   `spider_forest:73` *"The book hungers for blood."* (woodsmanEvent),
///   `village_attack:42` *"That's unfortunate, you bleed right?"* (villageAttack),
///   `village_attack:229` *"I smell blood!"* (villageAttack),
///   `recalled:49` *"Blood. Want blood."* (attackVillage).
///   No other combat option in the game mentions blood — the treant is *"Cut it down."*, the
///   highwayman *"No, your life"*, the defences *"Rush in to defend."* and *"Defend the village."*.
/// - **Siding with an attack on a village.** `village_attack:260` *"Join the attack."* sets
///   `playerAttack = true` and runs `villageAttack`; `:271` *"Revel in the chaos."* sets the same
///   flag without a fight, which is complicity rather than combat and so would never have been
///   screened at all. `harmful` misses the first because `the` in front of `attack` makes it read as
///   a noun — the very exemption [`reads_as_a_noun`] exists for.
///
/// **Deliberately over-broad in the safe direction**, as [`harmful`] is: a false positive costs one
/// unanswered event, and a false negative kills someone.
///
/// What it does **not** screen, because the dev named these as acceptable: the treant at the
/// crossroads (`hidden_path.lua:27,49,76,148`), the highwayman (`highwaymen.lua:53,58`), and
/// defending a village under attack (`village_attack.lua:214`, `recalled.lua:39`).
pub fn murderous(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("[murderer]")
        || t.contains("blood")
        || t.contains("bleed")
        || t.contains("join the attack")
        || t.contains("revel in the chaos")
}

pub fn harmful(text: &str) -> bool {
    const WORDS: &[&str] = &[
        "kill",
        "murder",
        "slay",
        "attack",
        "strike",
        "stab",
        "shoot",
        "execute",
        "behead",
        "sacrifice",
        "rob",
        "steal",
        "loot",
        "burn",
        "betray",
        "threaten",
        "extort",
    ];
    let t = text.to_ascii_lowercase();
    let words: Vec<&str> =
        t.split(|c: char| !c.is_ascii_alphabetic()).filter(|w| !w.is_empty()).collect();
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
        assert!(
            !harmful("[Combat] - Cut it down."),
            "the moral screen is not the one that catches this"
        );
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
        // The merchant is not harmful, he is expensive — a different predicate. See `costs_gold`.
        assert!(!harmful("[-500g] - Get two random gear items."));
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

    #[test]
    fn a_portrait_events_choice_is_clicked_in_the_middle_not_on_its_edge() {
        // The Woodsman, live: the console printed 144 and the plaque spans 144..1144, so the anchor
        // IS the left edge. `ui/elements/button.lua:93` tests `x > 0` strictly, so a click there is
        // rejected -- which is exactly what four attempts at 1.0000 measured.
        let c = Choice { text: "[Shop] - \"Show me what you've got.\"".into(), x: 144, y: 594 };
        assert_eq!(c.click_point(1920, 1080), (644, 594), "half a button width to the right");

        // The layout with no portrait prints the true centre, which is why every earlier event
        // worked and this bug stayed hidden.
        let centred = Choice { text: "Continue".into(), x: 960, y: 745 };
        assert_eq!(centred.click_point(1920, 1080), (960, 745), "anchor and centre coincide here");
    }

    #[test]
    fn the_correction_scales_with_the_window() {
        // `xOffset` is multiplied by the scale, so a smaller client moves the centre by less.
        // 1600x900 -> s = 0.8333, so the shift is 1000*0.5*0.8333 = 417 rather than 500.
        let c = Choice { text: "x".into(), x: 120, y: 495 };
        assert_eq!(c.click_point(1600, 900), (537, 495));
    }

    /// The highwayman: "your money or your life", and we choose neither the money nor dying.
    /// **The dying paladin, verbatim off the console of the 0547Z run, which killed him.**
    ///
    /// The dev: *we slayed the Paladin in cold blood.* Both options were printed, the peaceful one
    /// second, and `safe_choice_avoiding_combat` is a `find` — so with `avoid_combat` false at 52/52
    /// health the `[Combat]` line was simply first past the filter.
    ///
    /// The wording is the `curseCollectBlood` variant of `dying_paladin.lua:259`'s
    /// `[Murderer][Combat] - Attack them.` and calls the same `paladinEvent` scenario, so the tag we
    /// would have screened on is absent for a reason that has nothing to do with the act.
    #[test]
    fn the_dying_paladin_is_not_killed_at_any_health() {
        let ev = Event {
            title: "Injured paladin at Youlthorpe crypt".into(),
            text: String::new(),
            choices: vec![
                Choice {
                    text: "[Combat] - \"Blood. The book hungers for blood.\"".into(),
                    x: 960,
                    y: 675,
                },
                Choice { text: "Tell them you'll rush back with aid.".into(), x: 960, y: 729 },
            ],
        };
        assert!(murderous(&ev.choices[0].text), "the blood variant is the murder");
        // The health axis is not what decides this, which was the whole defect.
        for hurt in [true, false] {
            let picked = ev.safe_choice_avoiding_combat(hurt).expect("answerable");
            assert_eq!(picked.text, "Tell them you'll rush back with aid.", "hurt={hurt}");
        }
        assert_eq!(ev.safe_choice().unwrap().text, "Tell them you'll rush back with aid.");
    }

    /// The woodsman, the dev's own example, and the same shape as the paladin.
    ///
    /// `spider_forest.lua:62,73` — `[Murderer][Combat] - "Are you alone?"` and
    /// `[Combat] - "The book hungers for blood."`, both calling `scenarios.woodsmanEvent`. The first
    /// is the hole [`harmful`] left open on its own: the word split makes `[Murderer]` into
    /// `murderer`, and the list holds `murder`, so a murder with innocuous prose read as harmless.
    #[test]
    fn the_woodsman_survives_both_ways_of_offering_his_death() {
        assert!(murderous("[Murderer][Combat] - \"Are you alone?\""), "the tag is the game's word");
        assert!(!harmful("[Murderer][Combat] - \"Are you alone?\""), "which `harmful` cannot see");
        assert!(murderous("[Combat] - \"The book hungers for blood.\""));

        let ev = Event {
            title: "Woodsman in Bempton Silva".into(),
            text: String::new(),
            choices: vec![
                Choice {
                    text: "[Combat] - \"The book hungers for blood.\"".into(),
                    x: 960,
                    y: 594,
                },
                Choice { text: "\"What are you selling?\"".into(), x: 960, y: 648 },
            ],
        };
        assert_eq!(
            ev.safe_choice_avoiding_combat(false).unwrap().text,
            "\"What are you selling?\""
        );
    }

    /// **The other side of the dev's ruling, and the half that is easy to break by accident.**
    ///
    /// 2026-08-23: *Combat in other dialogues, as long as it's not for murdering a human being,
    /// should be fine. The crossroads' tree, the highwayman, and the muggers are examples of
    /// acceptable combat.* So a fight is not a reason to decline, and each of these is a real option
    /// from the game that must still be reachable.
    #[test]
    fn the_fights_the_dev_named_are_still_taken() {
        for ok in [
            "[Combat] - Cut it down.", // hidden_path.lua:27, the crossroads' tree
            "[Combat][Curse] - \"I have a way with wood.\"", // :49
            "[Combat][Curse] - \"The trees are after me!\"", // :76
            "[Combat] - 'Sling' it.",  // :148
            "[Combat] - \"No, your life\"", // highwaymen.lua:53
            "[Combat] - \"I'd rather die\"", // :58
            "[Combat] - Rush in to defend.", // village_attack.lua:214
            "[Combat] - Defend the village.", // recalled.lua:39
        ] {
            assert!(!murderous(ok), "{ok} is a fight, not a murder");
            assert!(starts_combat(ok), "{ok} should still read as a fight");
        }

        // And end to end: the stump is still cut down when we are well.
        let ev = Event {
            title: "Stump in the road".into(),
            text: "One of the paths leaving the crossroads has a stump blocking the path.".into(),
            choices: vec![
                Choice { text: "[Combat] - Cut it down.".into(), x: 960, y: 594 },
                Choice { text: "Leave it.".into(), x: 960, y: 648 },
            ],
        };
        assert!(starts_combat(&ev.safe_choice_avoiding_combat(false).unwrap().text));
        // Hurt, the older rule still applies and we walk away instead.
        assert_eq!(ev.safe_choice_avoiding_combat(true).unwrap().text, "Leave it.");
    }

    /// Siding with an attack on a village, in both the shapes the game offers it.
    ///
    /// `village_attack.lua:260` *"Join the attack."* runs `villageAttack` with `playerAttack = true`;
    /// `:271` *"Revel in the chaos."* sets the same flag with no fight at all, so nothing that
    /// screens combat would ever have caught it. The blood-lust wordings at `:42` and `:229` and
    /// `recalled.lua:49` are the `curseCollectBlood` variants of the same act.
    #[test]
    fn joining_an_attack_on_a_village_is_a_murder_however_it_is_worded() {
        for m in [
            "[Combat] - Join the attack.",
            "Revel in the chaos.",
            "[Combat] - \"That's unfortunate, you bleed right?\"",
            "[Combat] - \"I smell blood!\"",
            "[Combat] - \"Blood. Want blood.\"",
        ] {
            assert!(murderous(m), "{m}");
        }
        // `harmful` lets the first through, which is what makes this its own screen: `the` in front
        // of `attack` reads as a noun, the exemption `reads_as_a_noun` exists for.
        assert!(!harmful("[Combat] - Join the attack."));

        let ev = Event {
            title: "Attack on Bainton Clump".into(),
            text: "The village is under attack!".into(),
            choices: vec![
                Choice { text: "[Combat] - Join the attack.".into(), x: 960, y: 594 },
                Choice { text: "Revel in the chaos.".into(), x: 960, y: 648 },
                Choice { text: "[Combat] - Rush in to defend.".into(), x: 960, y: 702 },
            ],
        };
        assert_eq!(
            ev.safe_choice_avoiding_combat(false).unwrap().text,
            "[Combat] - Rush in to defend.",
            "the one option that is not siding with the attackers"
        );
    }

    /// The last-resort arm may reach for a fight, and still not for a murder.
    ///
    /// Its reason for existing is the highwayman — see the fallback's own note — and that case is
    /// unaffected. What must not happen is running out of options and settling on the one the game
    /// tags `[Murderer]`: an unanswered event is recoverable and a death is not.
    #[test]
    fn the_combat_fallback_will_not_settle_on_a_murder() {
        let ev = Event {
            title: "Injured paladin at Youlthorpe crypt".into(),
            text: String::new(),
            choices: vec![
                Choice { text: "[Murderer][Combat] - Attack them.".into(), x: 960, y: 594 },
                Choice { text: "[-All gold] - Hand it over".into(), x: 960, y: 648 },
            ],
        };
        assert_eq!(ev.safe_choice_avoiding_combat(true), None, "nothing here may be taken");
        assert_eq!(ev.safe_choice(), None);
    }

    #[test]
    fn the_highwayman_is_fought_rather_than_paid() {
        // Real choices from `overworld/events/arrived/highwaymen.lua:37,52`, as the console printed
        // them live. A run at 1/20 took the first and lost all 763 gold -- and with it the ten an
        // inn charges, which was the entire point of the journey it was on.
        let ev = Event {
            title: "Highwayman near Cowlam crypt".into(),
            text: "Stand and deliver! Your money or your life.".into(),
            choices: vec![
                Choice { text: "[-All gold] - Your money".into(), x: 960, y: 594 },
                Choice { text: "[Combat] - \"No, your life\"".into(), x: 960, y: 756 },
            ],
        };

        assert!(costs_everything(&ev.choices[0].text));
        // Hurt, which is when the combat screen is active and the bug fired.
        let picked = ev.safe_choice_avoiding_combat(true).expect("an event must be answerable");
        assert!(starts_combat(&picked.text), "picked {:?}", picked.text);
        // And at full health too -- paying everything is never the cheap option.
        assert!(starts_combat(&ev.safe_choice_avoiding_combat(false).unwrap().text));
        assert!(starts_combat(&ev.safe_choice().unwrap().text));
    }

    /// The screen is for *all* our gold, not for prices.
    #[test]
    fn an_ordinary_price_is_not_a_shakedown() {
        // Shops and tolls quote a number. Only the highwayman's tag means "everything you have",
        // and screening on a number would refuse every purchase the run ever wants to make.
        assert!(!costs_everything("[-50 gold] - Pay the toll"));
        assert!(!costs_everything("Pay 50 gold"));
        assert!(!costs_everything("[Shop] - \"Show me what you've got.\""));
        assert!(costs_everything("[-All gold] - Your money"));
    }

    /// The travelling merchant, off the console verbatim, and the answer is `Leave.`
    ///
    /// The dev's rule after a run paid him five hundred gold for random gear: refuse to buy
    /// anything. That purse is what the heart errand and every inn are spent from, and nothing in
    /// this program can evaluate a gear item.
    #[test]
    fn the_travelling_merchant_is_refused() {
        let ev = Event {
            title: "Travelling merchant at Scorborough Spinny north guard post".into(),
            text: String::new(),
            choices: vec![
                Choice { text: "[-500g] - Get two random gear items.".into(), x: 960, y: 432 },
                Choice {
                    text: "[-300g] - Pick one of two random gear items.".into(),
                    x: 960,
                    y: 486,
                },
                Choice { text: "[-200g] - Get one random gear item.".into(), x: 960, y: 540 },
                Choice { text: "Leave.".into(), x: 960, y: 594 },
            ],
        };
        // The run of 2026-08-15 took the first of these. It is the most expensive option on the
        // screen and it was chosen because nothing screened it out.
        assert_eq!(ev.safe_choice_avoiding_combat(false).unwrap().text, "Leave.");
        assert_eq!(ev.safe_choice_avoiding_combat(true).unwrap().text, "Leave.");
    }

    /// A price tag is a price tag; prose about gold is not.
    #[test]
    fn a_bracketed_price_is_what_marks_a_purchase() {
        assert!(costs_gold("[-500g] - Get two random gear items."));
        assert!(costs_gold("[-50 gold] - Pay the toll"));
        assert!(!costs_gold("Leave."));
        assert!(!costs_gold("Pay 50 gold"), "unbracketed prose is not the game's own tag");
        assert!(!costs_gold("[Shop] - \"Show me what you've got.\""));
        // The highwayman is a different refusal with a different reason, and neither should start
        // catching the other: this one is "we are not shopping", that one is "we are being robbed".
        assert!(!costs_gold("[-All gold] - Your money"));
        // A reward is not a cost. `[+…]` must not trip a predicate that reads `[-`.
        assert!(!costs_gold("[+200g] - Take the purse"));
    }
}
