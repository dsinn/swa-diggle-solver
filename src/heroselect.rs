//! Reading a champion card well enough to refuse it.
//!
//! Hero select offers three champions and the run has to click one. Until now it clicked the middle
//! card — [`crate::navigate::start_new_run`] said so plainly: *"The middle card is picked because it
//! needs no arithmetic, not because it is best."* That was honest and it was also a coin toss, and
//! the dev has since named the classes it must not land on:
//!
//! > **always avoid the demonkin and cultist; they play very differently and we do not have routing
//! > set up for them. The warrior that we played is quite powerful; he should be our first pick if
//! > he appears.**
//!
//! ## Why this reads pictures and not words
//!
//! Every card writes its class in plain English — `('The %s'):format(classData.name)`
//! (`ui/elements/herodisplay.lua:173`). Reading it means OCR, or a text template per class rendered
//! in the game's own font at the game's own size, which cannot be built without first capturing
//! every class on a live screen. Neither is worth it, because the card carries the same fact as
//! **art shipped in the game directory**.
//!
//! `ui/elements/herodisplay.lua:196` draws each of a card's items with
//! `love.graphics.draw(imageCache(item.icon), x, y)` — no scale arguments, no tint — and at
//! 1920x1080 the card is laid out 1:1 (`charWidth = 450`, and the three cards were measured live at
//! 450 pixels apart). So an item's icon file **is** the pixels on screen. Measured against
//! `tests/frames/16-selected.png`, which is a real hero-select capture:
//!
//! ```text
//!   bedding-roll-32.png          1.0000  err 0.0013  at (829,700)  <- the card's own passive
//!   class-demonkin-profane-32    0.0000              anywhere in that row
//!   class-hench-32               0.0566              anywhere in that row
//! ```
//!
//! That is the whole method: exact, or plainly not there. [`MARKER_PRESENT`] carries the rest of the
//! sweep, including the two icons that made the threshold a real question.
//!
//! ## Why the `Passives:` row, and only that row
//!
//! A card has five rows — Consumables, Equipment, Passives, Boons, Curses
//! (`herodisplay.lua:14-36`) — and the class fingerprint has to come from a row the roll cannot
//! touch. `ui/heroselect.lua:86-175` rewrites `potentialBoons` and `potentialCurses` heavily and
//! never once appends to `hero.passives`, so the Passives row is exactly what the class's
//! `newGameData` declared, in declaration order.
//!
//! This is not a nicety. `hench` — the warrior's marker — declares
//! `sources = { startBoon = 'rare' }` and `class = exclude{ scribe }` (`items/boardshapes.lua:79-85`),
//! so it can be rolled onto almost any other class's card as a **boon**. A sweep of the whole card
//! for `class-hench-32.png` would call a demonkin a warrior; a sweep of the Passives row cannot.
//!
//! The first two rows always draw — `if setData[1] or set.slotKey`, and both declare a `slotKey`
//! (`herodisplay.lua:16-24`) — so Passives is the third row whenever it exists, at a fixed height.
//!
//! ## What a marker proves, and what it does not
//!
//! Each marker is one item key ([`MARKERS`]), resolved to its picture through the item catalogue so
//! that moved art fails a test rather than a run. Read against every class's starting passives:
//!
//! - `demonkinProfane` — the demonkin's, and nothing else's.
//! - `cultistForesight` and `cultistMagicBoost` — both the cultist's. Two independent markers for
//!   one refusal.
//! - `hench` — starts on the **warrior and the woodsman**, and no one else. So it does not name a
//!   class by itself, which is why `trappingKit` and `fireBuilder` are read alongside it: the
//!   woodsman starts with both, the warrior with neither.
//! - `bedroll` — a control. It marks no class (five of the thirteen start with one) and exists so a
//!   log line can distinguish *"the row was read and held other classes' icons"* from *"the row was
//!   never read at all"*.
//!
//! Every one of the thirteen classes starts with at least one passive — the paladin's single
//! `capitalPassport` is the thinnest, `rpg/classes/paladin.lua:88` — so the Passives row is always
//! drawn and the Boons row can never slide up into the place this reads. A test re-derives all of
//! that from the class files rather than trusting this paragraph.
//!
//! ## What happens when nothing is recognised
//!
//! Every card `Open`, and the middle one is taken — which is exactly the behaviour this replaced. A
//! reader that has silently broken therefore degrades to the old coin toss rather than stopping a
//! run, and says so in the log. The one case that does stop a run is every card refused: there is no
//! champion left to click, and a reroll (`ui/heroselect.lua:302-308`, available once) is not built.

use crate::items::Catalogue;
use crate::observe::template::{find_at_scale_in, Template};
use crate::win::capture::Frame;
use std::path::Path;

/// What finding a marker in a card's `Passives:` row settles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Means {
    /// A class this solver has no routing for. Never clicked.
    Refuse(&'static str),
    /// The dev's first pick — unless a [`Means::Shared`] marker on the same card takes it back.
    Prefer(&'static str),
    /// A class that also starts with a [`Means::Prefer`] marker, and the reason that marker cannot
    /// stand alone. Finding one of these takes the preference back.
    Shared(&'static str),
    /// Proof the row was read. Names no class.
    Control,
}

/// One item whose picture, in the `Passives:` row, says something about the class.
#[derive(Debug, Clone, Copy)]
pub struct Marker {
    /// The item key, resolved to art through [`Catalogue::icon`].
    pub item: &'static str,
    pub means: Means,
}

/// Every marker read from a card. See the module header for why each one is here.
///
/// The cultist's fourth passive, `heretic`, was a marker and was removed: measured against
/// `tests/frames/16-selected.png` it scored **0.9015** on a card that does not have it — the worst
/// false positive of anything tried, and no accident. Its art is 20% opaque, the sparsest of the
/// candidates, so it has few pixels to disagree with and they are dark ones landing on a dark
/// panel. `cultistMagicBoost` (57% opaque, 0.0051 on the same row) replaces it.
pub const MARKERS: &[Marker] = &[
    Marker { item: "demonkinProfane", means: Means::Refuse("demonkin") },
    Marker { item: "cultistForesight", means: Means::Refuse("cultist") },
    Marker { item: "cultistMagicBoost", means: Means::Refuse("cultist") },
    Marker { item: "hench", means: Means::Prefer("warrior") },
    Marker { item: "trappingKit", means: Means::Shared("woodsman") },
    Marker { item: "fireBuilder", means: Means::Shared("woodsman") },
    Marker { item: "bedroll", means: Means::Control },
];

/// Card width, and therefore the gap between card centres — `herodisplay.lua:13`, `charWidth = 450`.
/// Confirmed live: the three cards were measured at 508, 958 and 1408.
pub const CARD_SPACING: i32 = 450;

/// Screen centre at 1920x1080, which is where the middle card sits. `ui/heroselect.lua:355` places
/// each card at x-index `i-((#selectedHeroes-1)/2)-1`, so a lone card is centred and three straddle.
pub const SCREEN_CENTRE_X: i32 = 960;

/// Where a row's first icon is blitted, relative to its card's centre.
///
/// `herodisplay.lua:180` sets `local x = x-128` before the row and steps 32 per item. The **-131**
/// is measured rather than derived from that: `bedding-roll-32.png` matched at x=829 on a card
/// centred at 960 in `tests/frames/16-selected.png`.
pub const ICON_DX: i32 = -131;

/// Top edge of the `Passives:` row's icons, measured on the same frame.
pub const PASSIVES_ICON_Y: i32 = 700;

/// Icon pitch along a row — `herodisplay.lua:199`, `x = x+32`.
pub const ICON_PITCH: i32 = 32;

/// How many icons a row draws before it gives up and prints `+N` — `herodisplay.lua:187`,
/// `if ii==9 then break end`.
pub const ICON_SLOTS: i32 = 8;

/// Search slack around the expected position, in pixels.
///
/// Eight, because the rows are 52 apart and the icons 32 tall (`herodisplay.lua:47`), leaving 20
/// pixels of gap: ±8 cannot reach into the Equipment row above or the Boons row below. It is
/// generous for what it has to absorb — the two-pixel disagreement between the card centres as
/// derived (510/960/1410) and as measured live (508/958/1408).
pub const SLACK: i32 = 8;

/// Inlier fraction at which a marker counts as present.
///
/// Calibrated against the nearest confusable state rather than against background: a *different*
/// item's icon in the same row, at the same size, over the same wooden panel. The full sweep over
/// `tests/frames/16-selected.png`, whose card holds `bedroll` and nothing else:
///
/// ```text
///   bedding-roll-32              1.0000  err 0.0013   <- the one that is really there
///   curse-heretic-32             0.9015  err 0.0828
///   trapping-kit-32              0.8409  err 0.0807
///   class-halfling-short-32      0.4375
///   environment-campfire-32      0.3087
///   class-cultist-foresight-32   0.2950
///   class-hench-32               0.0566
///   class-demonkin-profane-32    0.0000
/// ```
///
/// **0.98**, and the first draft's 0.90 was wrong. The gap is not where it looks: sparse art is
/// confusable art, and `curse-heretic-32` cleared 0.90 on a row it has no business matching. What
/// separates true from false here is not a wide margin but the *kind* of match — a true positive is
/// an exact blit of the file onto the panel, so it reads 1.0000 at err 0.0013, and anything short of
/// near-perfect is something else. Erring high is also the safe direction: an unrecognised card is
/// `Open`, which costs a preference, while a false `Refused` throws away a champion we could play.
pub const MARKER_PRESENT: f64 = 0.98;

/// Where the three cards are centred. The outer two are simply empty when the game offers one card
/// (`ui/heroselect.lua:56`, which asks for a single hero while `rngSeed == 0`).
pub fn card_centres() -> [i32; 3] {
    [SCREEN_CENTRE_X - CARD_SPACING, SCREEN_CENTRE_X, SCREEN_CENTRE_X + CARD_SPACING]
}

/// The client rectangle that holds every card's `Passives:` row — `(x, y, w, h)`.
///
/// Capturing the band rather than the window is the standing rule for anything on a clock: a
/// full-window grab is expensive, and nothing outside these 48 rows is read.
pub fn passives_band() -> (i32, i32, i32, i32) {
    let c = card_centres();
    let x0 = c[0] + ICON_DX - SLACK;
    let x1 = c[2] + ICON_DX + ICON_PITCH * (ICON_SLOTS - 1) + ICON_PITCH + SLACK;
    let y0 = PASSIVES_ICON_Y - SLACK;
    let y1 = PASSIVES_ICON_Y + ICON_PITCH + SLACK;
    (x0, y0, x1 - x0, y1 - y0)
}

/// What a run may do with one card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The warrior. Taken ahead of anything else.
    Preferred,
    /// Nothing seen that decides it either way.
    Open,
    /// Named as a class with no routing. Never clicked.
    Refused(&'static str),
}

/// One champion card, as read.
#[derive(Debug, Clone)]
pub struct Card {
    /// Client x of the card's centre.
    pub centre_x: i32,
    /// Every marker's best score in this card's row, in [`MARKERS`] order — the whole measurement,
    /// not just the ones that passed, so a log line shows what a failed reading actually saw.
    pub scores: Vec<(&'static str, f64)>,
    /// Markers that could not be looked for at all: art missing, or a key the catalogue never saw.
    pub problems: Vec<String>,
    pub verdict: Verdict,
}

impl Card {
    /// Whether a marker scored at or above [`MARKER_PRESENT`].
    pub fn has(&self, item: &str) -> bool {
        self.scores.iter().any(|(k, s)| *k == item && *s >= MARKER_PRESENT)
    }

    /// A one-line summary for the run log.
    pub fn summary(&self) -> String {
        let seen: Vec<String> = self
            .scores
            .iter()
            .filter(|(_, s)| *s >= MARKER_PRESENT)
            .map(|(k, s)| format!("{k} {s:.4}"))
            .collect();
        let best = self.scores.iter().map(|(_, s)| *s).fold(0.0f64, f64::max);
        format!(
            "x={} {:?} [{}]{}",
            self.centre_x,
            self.verdict,
            if seen.is_empty() { format!("nothing, best {best:.4}") } else { seen.join(", ") },
            if self.problems.is_empty() {
                String::new()
            } else {
                format!(" ({})", self.problems.join("; "))
            }
        )
    }
}

/// Scores every marker against one card's `Passives:` row and rules on the card.
///
/// `frame` is the band from [`passives_band`]; `origin` is the client position of its top-left
/// corner, so the same reader works on a full-window capture by passing `(0, 0)`.
pub fn read_card(
    frame: &Frame, origin: (i32, i32), catalogue: &Catalogue, art_dir: &Path, centre_x: i32,
) -> Card {
    let mut scores = Vec::new();
    let mut problems = Vec::new();
    // Candidate top-left positions: the whole row of slots, plus slack, in frame coordinates.
    let x0 = centre_x + ICON_DX - SLACK - origin.0;
    let x1 = centre_x + ICON_DX + ICON_PITCH * (ICON_SLOTS - 1) + SLACK - origin.0;
    let y0 = PASSIVES_ICON_Y - SLACK - origin.1;
    let y1 = PASSIVES_ICON_Y + SLACK - origin.1;

    for marker in MARKERS {
        let Some(icon) = catalogue.icon(marker.item) else {
            problems.push(format!("`{}` declares no icon", marker.item));
            continue;
        };
        let tpl = match Template::load(&art_dir.join(icon)) {
            Ok(t) => t,
            Err(e) => {
                problems.push(format!("{icon}: {e}"));
                continue;
            }
        };
        // Scale 1.0 only. The card is drawn 1:1 at 1920x1080, and a sweep would only offer the
        // matcher more chances to find a near-miss it should be reporting as an absence.
        let best = find_at_scale_in(frame, &tpl, 1.0, 1, Some((x0, y0, x1, y1)))
            .map(|m| m.inliers)
            .unwrap_or(0.0);
        scores.push((marker.item, best));
    }

    let mut card = Card { centre_x, scores, problems, verdict: Verdict::Open };
    card.verdict = rule(&card);
    card
}

/// The dev's policy, applied to one card's markers.
fn rule(card: &Card) -> Verdict {
    for marker in MARKERS {
        if let Means::Refuse(class) = marker.means {
            if card.has(marker.item) {
                return Verdict::Refused(class);
            }
        }
    }
    // A `Prefer` marker only holds if no class that shares it is also on show. `hench` starts on
    // the warrior and the woodsman alike; `trappingKit` is what tells them apart.
    let shared = MARKERS.iter().any(|m| matches!(m.means, Means::Shared(_)) && card.has(m.item));
    let preferred = MARKERS.iter().any(|m| matches!(m.means, Means::Prefer(_)) && card.has(m.item));
    if preferred && !shared {
        return Verdict::Preferred;
    }
    Verdict::Open
}

/// Picks a card: the warrior if one is on offer, otherwise anything not refused.
///
/// Ties break toward the middle, which is what keeps the single-card screen working — the two outer
/// positions are then empty, read as `Open`, and lose the tie to the card that is actually there.
///
/// `Err` only when every card is refused. There is no champion left to click and the reroll button
/// is not built, so the honest move is to stop rather than to play a class with no routing.
pub fn choose(cards: &[Card]) -> Result<usize, String> {
    let middle = (cards.len() as i32 - 1) / 2;
    let best = cards
        .iter()
        .enumerate()
        .filter(|(_, c)| !matches!(c.verdict, Verdict::Refused(_)))
        .min_by_key(|(i, c)| {
            (if c.verdict == Verdict::Preferred { 0 } else { 1 }, (*i as i32 - middle).abs())
        });
    match best {
        Some((i, _)) => Ok(i),
        None => Err(format!(
            "every champion offered is one we have no routing for: {}",
            cards.iter().map(|c| c.summary()).collect::<Vec<_>>().join(" | ")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn game_dir() -> PathBuf {
        PathBuf::from("../sternly-worded-adventures")
    }

    fn card(centre_x: i32, present: &[&'static str]) -> Card {
        let scores = MARKERS
            .iter()
            .map(|m| (m.item, if present.contains(&m.item) { 1.0 } else { 0.2 }))
            .collect();
        let mut c = Card { centre_x, scores, problems: Vec::new(), verdict: Verdict::Open };
        c.verdict = rule(&c);
        c
    }

    fn frame(name: &str) -> Option<Frame> {
        let path = PathBuf::from("tests").join("frames").join(name);
        let dec = png::Decoder::new(std::fs::File::open(path).ok()?);
        let mut rdr = dec.read_info().ok()?;
        let mut buf = vec![0; rdr.output_buffer_size()];
        let info = rdr.next_frame(&mut buf).ok()?;
        let n = info.color_type.samples();
        let mut bgra = Vec::with_capacity((info.width * info.height * 4) as usize);
        for px in buf.chunks_exact(n) {
            bgra.extend_from_slice(&[px[2], px[1], px[0], 255]);
        }
        Some(Frame { width: info.width as i32, height: info.height as i32, bgra })
    }

    #[test]
    fn the_two_refused_classes_are_never_chosen() {
        let cards = vec![
            card(510, &["demonkinProfane", "bedroll"]),
            card(960, &["cultistForesight", "cultistMagicBoost"]),
            card(1410, &["bedroll"]),
        ];
        assert_eq!(cards[0].verdict, Verdict::Refused("demonkin"));
        assert_eq!(cards[1].verdict, Verdict::Refused("cultist"));
        assert_eq!(choose(&cards), Ok(2), "the only card left is the one to take");
    }

    #[test]
    fn the_warrior_is_taken_over_a_nearer_card() {
        let cards =
            vec![card(510, &["hench", "bedroll"]), card(960, &["bedroll"]), card(1410, &[])];
        assert_eq!(cards[0].verdict, Verdict::Preferred);
        assert_eq!(choose(&cards), Ok(0), "distance from the middle must not outrank the warrior");
    }

    /// `hench` alone is the woodsman as often as it is the warrior, and the woodsman is nobody's
    /// first pick — so the marker they share must not carry the preference on its own.
    #[test]
    fn a_woodsman_is_not_mistaken_for_the_warrior() {
        let woodsman = card(510, &["hench", "trappingKit", "fireBuilder", "bedroll"]);
        assert_eq!(woodsman.verdict, Verdict::Open, "hench with trappingKit is the woodsman");
        let cards = vec![woodsman, card(960, &["bedroll"]), card(1410, &[])];
        assert_eq!(choose(&cards), Ok(1), "with no warrior on offer, the middle card wins the tie");
    }

    /// A reader that sees nothing must land exactly where the old blind version did.
    #[test]
    fn seeing_nothing_falls_back_to_the_middle_card() {
        let cards = vec![card(510, &[]), card(960, &[]), card(1410, &[])];
        assert!(cards.iter().all(|c| c.verdict == Verdict::Open));
        assert_eq!(choose(&cards), Ok(1));
    }

    #[test]
    fn a_screen_of_nothing_but_refusals_stops_the_run() {
        let cards = vec![
            card(510, &["demonkinProfane"]),
            card(960, &["cultistMagicBoost"]),
            card(1410, &["cultistForesight"]),
        ];
        assert!(choose(&cards).is_err());
    }

    /// The band must not reach into the rows either side of the passives.
    #[test]
    fn the_search_band_stays_inside_its_own_row() {
        /// `herodisplay.lua:47`, `local itemLineHeight = 52`.
        const ROW_PITCH: i32 = 52;
        let (_, y, _, h) = passives_band();
        assert!(y > PASSIVES_ICON_Y - ROW_PITCH + ICON_PITCH, "reaches the Equipment row above");
        assert!(y + h < PASSIVES_ICON_Y + ROW_PITCH, "reaches the Boons row below");
    }

    /// **The measurement the whole method rests on**, against a real hero-select capture.
    ///
    /// `tests/frames/16-selected.png` is a lone adventurer card, and in v52.2 that class started
    /// with `bedroll` and nothing else. So the frame carries a positive control and a negative one
    /// at the same time: the card's own passive must be found exactly where the constants say it is,
    /// and every class marker must be plainly absent from the same row. Without the first half, the
    /// second would prove nothing — an empty result from a reader looking at the wrong 48 rows looks
    /// identical.
    #[test]
    fn a_real_card_reads_its_own_passive_and_no_one_elses() {
        let Some(f) = frame("16-selected.png") else {
            eprintln!("SKIP: tests/frames/16-selected.png is missing");
            return;
        };
        if !game_dir().join("items/boardshapes.lua").is_file() {
            eprintln!("SKIP: game source not present at {}", game_dir().display());
            return;
        }
        let cat = Catalogue::load(&game_dir()).expect("the item catalogue should load");
        let card = read_card(&f, (0, 0), &cat, &game_dir(), SCREEN_CENTRE_X);
        eprintln!("  {}", card.summary());
        assert!(card.problems.is_empty(), "every marker must be lookupable: {:?}", card.problems);

        let score = |k: &str| card.scores.iter().find(|(i, _)| *i == k).map(|(_, s)| *s).unwrap();
        assert!(
            score("bedroll") >= MARKER_PRESENT,
            "the card's own passive scored {:.4}; the row is not being read",
            score("bedroll")
        );
        for m in MARKERS.iter().filter(|m| m.means != Means::Control) {
            assert!(
                score(m.item) < MARKER_PRESENT,
                "`{}` scored {:.4} on a card that does not have it",
                m.item,
                score(m.item)
            );
        }
        assert_eq!(card.verdict, Verdict::Open, "an adventurer is neither refused nor preferred");
    }

    /// Every class's starting passives, scraped from `rpg/classes/*.lua`.
    ///
    /// The list is written both ways — one per line, and `passives = {'capitalPassport'},` all on
    /// one (`rpg/classes/paladin.lua:88`) — and a first version of this test read only the first
    /// form. On the paladin it swallowed the *next* block instead and reported that the class starts
    /// with `hench`, which is a boon of its (`:96`). That would have looked exactly like the game
    /// having changed under us.
    fn starting_passives() -> Option<Vec<(String, Vec<String>)>> {
        let dir = game_dir().join("rpg/classes");
        if !dir.is_dir() {
            eprintln!("SKIP: game source not present at {}", game_dir().display());
            return None;
        }
        let mut out: Vec<(String, Vec<String>)> = Vec::new();
        for e in std::fs::read_dir(&dir).expect("classes should be readable").filter_map(|e| e.ok())
        {
            let path = e.path();
            if path.extension().and_then(|s| s.to_str()) != Some("lua") {
                continue;
            }
            let key = path.file_stem().unwrap().to_string_lossy().into_owned();
            let src = std::fs::read_to_string(&path).expect("a class file should be readable");
            // From `passives = {` to the matching `}`, however many lines that spans.
            let body = src
                .split_once("passives = {")
                .and_then(|(_, rest)| rest.split_once('}').map(|(body, _)| body))
                .unwrap_or("");
            let passives = body
                .split(',')
                .map(|s| s.trim().trim_matches(|c| c == '\'' || c == '"').to_string())
                .filter(|s| !s.is_empty())
                .collect();
            out.push((key, passives));
        }
        assert!(out.len() > 10, "only found {} classes; the scrape is wrong", out.len());
        Some(out)
    }

    /// The classes the markers claim to name are still the classes that start with them.
    ///
    /// Read from the class files rather than restated here, because the whole policy rests on
    /// exclusivity: if a future version gives `demonkinProfane` to somebody else, or teaches a third
    /// class `hench`, this must fail rather than a run quietly picking a class it cannot play.
    #[test]
    fn the_markers_still_belong_to_the_classes_they_name() {
        let Some(starts) = starting_passives() else { return };
        let owners = |item: &str| -> Vec<String> {
            starts
                .iter()
                .filter(|(_, p)| p.iter().any(|x| x == item))
                .map(|(k, _)| k.clone())
                .collect()
        };
        // Which classes a `Prefer` marker is allowed to be ambiguous between: the one it names, plus
        // every class a `Shared` marker exists to rule out.
        let mut excused: Vec<String> = MARKERS
            .iter()
            .filter_map(|m| match m.means {
                Means::Shared(c) => Some(c.to_string()),
                _ => None,
            })
            .collect();
        for m in MARKERS {
            let who = owners(m.item);
            match m.means {
                Means::Refuse(class) | Means::Shared(class) => {
                    assert_eq!(who, vec![class.to_string()], "`{}` no longer names {class}", m.item)
                }
                Means::Prefer(class) => {
                    excused.push(class.to_string());
                    for c in &who {
                        assert!(
                            excused.contains(c),
                            "`{}` also starts on the {c}, which no `Means::Shared` marker rules out",
                            m.item
                        );
                    }
                    assert!(
                        who.contains(&class.to_string()),
                        "`{}` no longer starts on the {class}",
                        m.item
                    );
                }
                Means::Control => assert!(who.len() > 1, "a control must not name a class"),
            }
        }
    }

    /// The `Passives:` row is always drawn, so the Boons row can never take its place.
    ///
    /// `herodisplay.lua:181` skips a row entirely when it is empty and has no slot count, and only
    /// the first two rows declare one. A class with no starting passives would therefore put its
    /// boons where this reader looks — and boons are rolled, so `hench` could arrive on anyone's
    /// card (`items/boardshapes.lua:82`, `sources = { startBoon = 'rare' }`).
    #[test]
    fn no_class_starts_without_a_passive() {
        let Some(starts) = starting_passives() else { return };
        for (class, passives) in &starts {
            assert!(
                !passives.is_empty(),
                "the {class} would draw its Boons row where Passives goes"
            );
        }
    }
}
