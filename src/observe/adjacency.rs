//! Parses the overworld adjacency dump that `core.verboseAdjacencyData` prints
//! (`overworldview.lua:1022-1053`).
//!
//! This is the payload the whole log channel exists for: node **keys**, **headings** (which
//! carry the location type) and **positions already in screen coordinates** —
//! `xoffset + location.posX*zoomMult`. Nothing else we can observe supplies identity; sprite
//! matching on the map layer does not work (design v2 §6.3).
//!
//! It is printed on world load (`:1607`), on arrival (`:1442`), and when a smooth screen
//! centring finishes (`:1255`).
//!
//! **That third one is not our arrow-key pans.** `:1255` fires only when `offsetTransition`
//! reaches 1, and `offsetTransition` is set solely by `core.centreScreenOn` with a falsy
//! `instant` (`:1544-1555`). Holding an arrow key goes through `hotspotDirection`
//! (`:1115-1123`), which never touches it. So panning gives us **no** re-read.
//!
//! What does: the `showAreaButtonsButton` at client (32, 918) (`:483-495`), whose tooltip is
//! literally "Show functions for current location and centre the screen". Its `mousereleased`
//! calls `refreshAreaButtons`, then `centreScreenOnPlayer()` **non-instant** — so activating it
//! recentres on the player and produces a fresh dump. It selects the player's own location, so
//! it cannot start a journey. That makes it a safe, repeatable "tell me the map again" primitive,
//! which is worth more than a timing-based gesture.
//!
//! ## Why parsing is label-anchored rather than field-split
//!
//! Lua's `print` joins arguments with tabs, and conhost expands those tabs against 8-column
//! stops when the text lands in the screen buffer. The resulting gaps are between **one and
//! seven spaces** depending on where the previous field happened to end — the real line is
//!
//! ```text
//!           posX: 777.81645222258 posY:   486.88382683585 connections:    4
//! ```
//!
//! with single spaces after `posX:` and `posY:`. So "split on runs of two or more spaces" is
//! wrong. Instead we anchor on the literal labels the game prints and take the next token.
//! Keys never contain spaces; headings may, so a heading is always "the rest of the line".

/// A location adjacent to the player, as the game itself describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    /// Stable identifier, e.g. `l1`, `start`. Never contains whitespace.
    pub key: String,
    /// `AreaHeading` output (`overworldview.lua:383-393`).
    pub heading: String,
    /// Screen x, ready to aim at — no offset or zoom maths needed on our side.
    pub x: f64,
    pub y: f64,
    /// How many locations this one connects to. A dead end is 1.
    pub connections: u32,
}

impl Node {
    /// True when the heading uses the combat form, `<name> — level <n> <type>`
    /// (`overworldview.lua:388-389`). The em-dash IS the combat marker, which is why the
    /// console codepage matters ([`crate::observe::log`]).
    pub fn has_combat(&self) -> bool {
        self.level().is_some()
    }

    /// Combat level from the heading, if it has the combat form.
    pub fn level(&self) -> Option<u32> {
        let rest = self.heading.split(" — level ").nth(1)?;
        rest.split_whitespace().next()?.parse().ok()
    }

    /// The location type, e.g. `crypt`.
    ///
    /// Only recoverable from the **combat** form. The non-combat form is `name .. ' ' .. type`
    /// with no separator, and both halves can contain spaces (`woodland shrine`,
    /// `general store`), so it cannot be split without hardcoding the game's type table — which
    /// would silently drift. Ask [`Node::type_is`] instead.
    pub fn type_name(&self) -> Option<&str> {
        let rest = self.heading.split(" — level ").nth(1)?;
        let after_level = rest.split_once(' ')?.1;
        Some(after_level.trim())
    }

    /// Whether this location is of the named type, by suffix.
    ///
    /// Works for both heading forms, and avoids mirroring the game's type list. Suffix matching
    /// is how a policy asks the question it actually has — `type_is("shrine")` for the §1
    /// shrine objective — without caring whether it is a `woodland shrine` or another variety.
    pub fn type_is(&self, type_name: &str) -> bool {
        self.heading.trim_end().ends_with(type_name)
    }
}

/// A way out of a subworld, printed in the `Subworld exit positions:` section. These carry a
/// position and the key they lead *to*, but are not themselves adjacent nodes.
#[derive(Debug, Clone, PartialEq)]
pub struct Exit {
    pub x: f64,
    pub y: f64,
    /// The location this exit heads toward.
    pub to_key: String,
    pub to_heading: String,
}

/// One complete dump.
#[derive(Debug, Clone, PartialEq)]
pub struct Adjacency {
    /// Why it printed: `World loaded`, `Arrived at location`, `Arrived at location with event`,
    /// or `Screen pan finished`. Tells us which event we are looking at.
    pub reason: String,
    /// Where the player is now.
    pub here_key: String,
    pub here_heading: String,
    /// Present when the player is inside a subworld: the parent node's key and heading.
    pub subworld: Option<(String, String)>,
    pub nodes: Vec<Node>,
    /// Adjacent locations the game refused to describe because they are cloud-covered or not
    /// yet visible (`:1032`). We know only how many. Load-bearing for the objective: a nonzero
    /// count means the map is hiding options, so "no shrine adjacent" is not yet a conclusion.
    pub hidden: usize,
    pub exits: Vec<Exit>,
    pub hidden_exits: usize,
}

const START: &str = "Local overworld data:";
const END: &str = "Local overworld data end";
const HIDDEN: &str = "Hidden location";

/// Every `reason` string the game can print, longest first so that
/// `Arrived at location with event` is not mistaken for `Arrived at location`.
/// Sites: `overworldview.lua:1255`, `:1442`, `:1607`.
const REASONS: [&str; 4] = [
    "Arrived at location with event",
    "Screen pan finished",
    "Arrived at location",
    "World loaded",
];

/// Splits `key<gap>heading`, where the gap is whatever the tab expanded to.
fn key_and_rest(s: &str) -> Option<(String, String)> {
    let s = s.trim_start();
    let (key, rest) = match s.split_once(char::is_whitespace) {
        Some(pair) => pair,
        // A key with no heading is malformed, but returning it beats dropping the line.
        None if !s.is_empty() => (s, ""),
        None => return None,
    };
    Some((key.to_string(), rest.trim().to_string()))
}

/// The token following `label` on this line.
fn after_label<'a>(line: &'a str, label: &str) -> Option<&'a str> {
    line.split_once(label)?.1.trim_start().split_whitespace().next()
}

/// Everything following `label`, trimmed.
fn rest_after_label<'a>(line: &'a str, label: &str) -> Option<&'a str> {
    Some(line.split_once(label)?.1.trim())
}

/// Which section of a block we are in. `Hidden location` is printed by both loops, so it can
/// only be attributed correctly by tracking this.
#[derive(PartialEq)]
enum Section {
    Head,
    Connections,
    Exits,
}

/// Extracts every **complete** dump from a batch of console lines.
///
/// Incomplete trailing content is ignored rather than guessed at: [`END`] frames the block, so a
/// caller polling the console never has to reason about half-written output. An unterminated
/// block followed by a new [`START`] is discarded — that only happens if the buffer was
/// recycled mid-print.
pub fn parse(lines: &[String]) -> Vec<Adjacency> {
    let mut out = Vec::new();
    let mut cur: Option<Adjacency> = None;
    let mut section = Section::Head;
    // Set when a node's key/heading line has been read and we are awaiting its posX line.
    let mut pending: Option<(String, String)> = None;

    for line in lines {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }

        if let Some(rest) = t.strip_prefix(START) {
            // Discards any unterminated predecessor, deliberately.
            let rest = rest.trim_start();
            let (reason, tail) = REASONS
                .iter()
                .find_map(|r| rest.strip_prefix(r).map(|tail| (r.to_string(), tail)))
                // Unknown reason: fall back to widest-gap splitting so a new call site added to
                // the game degrades the reason string rather than losing the whole block.
                .unwrap_or_else(|| match rest.split_once("  ") {
                    Some((r, tail)) => (r.trim().to_string(), tail),
                    None => (String::new(), rest),
                });
            let (here_key, here_heading) = key_and_rest(tail).unwrap_or_default();
            cur = Some(Adjacency {
                reason,
                here_key,
                here_heading,
                subworld: None,
                nodes: Vec::new(),
                hidden: 0,
                exits: Vec::new(),
                hidden_exits: 0,
            });
            section = Section::Head;
            pending = None;
            continue;
        }

        let Some(a) = cur.as_mut() else { continue };

        if t == END {
            out.push(cur.take().unwrap());
            continue;
        }
        if t.starts_with("In subworld:") {
            if let Some(pair) = rest_after_label(t, "In subworld:").and_then(key_and_rest) {
                a.subworld = Some(pair);
            }
            continue;
        }
        if t.starts_with("Adjacent connections:") {
            section = Section::Connections;
            continue;
        }
        if t.starts_with("Subworld exit positions:") {
            section = Section::Exits;
            continue;
        }
        if t == HIDDEN {
            match section {
                Section::Exits => a.hidden_exits += 1,
                _ => a.hidden += 1,
            }
            continue;
        }

        if t.starts_with("posX:") {
            let x = after_label(t, "posX:").and_then(|v| v.parse::<f64>().ok());
            let y = after_label(t, "posY:").and_then(|v| v.parse::<f64>().ok());
            let (Some(x), Some(y)) = (x, y) else { continue };

            if let Some(to) = rest_after_label(t, "heading to:").and_then(key_and_rest) {
                a.exits.push(Exit { x, y, to_key: to.0, to_heading: to.1 });
            } else if let Some((key, heading)) = pending.take() {
                let connections = after_label(t, "connections:")
                    .and_then(|v| v.parse::<u32>().ok())
                    .unwrap_or(0);
                a.nodes.push(Node { key, heading, x, y, connections });
            }
            continue;
        }

        // Anything else inside the connections section is a `key<tab>heading` line.
        if section == Section::Connections {
            pending = key_and_rest(t);
        }
    }
    out
}

/// Stateful wrapper over [`parse`] for callers polling the console.
///
/// [`parse`] is correct but stateless, and a dump does not arrive atomically: polling every 400 ms
/// routinely splits one block across two batches. The half carrying the `Local overworld data:`
/// header is then discarded as incomplete and the remainder never matches a header, so the whole
/// dump vanishes. That is exactly how a **successful** travel was reported as "no arrival dump
/// within 45 s" — the game had printed the arrival, we had read it, and we threw it away.
///
/// This keeps the unterminated tail and prepends it to the next batch.
#[derive(Default)]
pub struct Reader {
    pending: Vec<String>,
}

impl Reader {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds newly read console lines and returns every dump completed by them.
    pub fn push(&mut self, lines: &[String]) -> Vec<Adjacency> {
        self.pending.extend(lines.iter().cloned());
        let out = parse(&self.pending);

        // Retain from the last unterminated START onward. Everything before it is either parsed or
        // noise, and dropping it keeps this from growing without bound during a long session.
        let last_start = self.pending.iter().rposition(|l| l.trim().starts_with(START));
        let last_end = self.pending.iter().rposition(|l| l.trim() == END);
        self.pending = match (last_start, last_end) {
            (Some(s), Some(e)) if s > e => self.pending.split_off(s),
            (Some(s), None) => self.pending.split_off(s),
            _ => Vec::new(),
        };
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim from a live run (`spike-frames-live/console-flag-report.md`, 2026-07-29),
    /// tabs already expanded by conhost. Using the real bytes is the point: the single space
    /// after `posX:` is exactly what a field-splitting parser gets wrong.
    const LIVE: &str = "\
Local overworld data:   World loaded    start   Cottam campfire
    Adjacent connections:
        l1      Weedley Copse — level 0 crypt
          posX: 777.81645222258 posY:   486.88382683585 connections:    4
Local overworld data end";

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(|l| l.to_string()).collect()
    }

    #[test]
    fn parses_a_real_dump() {
        let got = parse(&lines(LIVE));
        assert_eq!(got.len(), 1);
        let a = &got[0];
        assert_eq!(a.reason, "World loaded");
        assert_eq!(a.here_key, "start");
        assert_eq!(a.here_heading, "Cottam campfire");
        assert_eq!(a.hidden, 0);
        assert_eq!(a.nodes.len(), 1);
        let n = &a.nodes[0];
        assert_eq!(n.key, "l1");
        assert_eq!(n.heading, "Weedley Copse — level 0 crypt");
        assert!((n.x - 777.81645222258).abs() < 1e-9);
        assert!((n.y - 486.88382683585).abs() < 1e-9);
        assert_eq!(n.connections, 4);
    }

    #[test]
    fn reads_combat_level_and_type_from_the_heading() {
        let n = &parse(&lines(LIVE))[0].nodes[0];
        assert!(n.has_combat());
        assert_eq!(n.level(), Some(0));
        assert_eq!(n.type_name(), Some("crypt"));
        assert!(n.type_is("crypt"));
        assert!(!n.type_is("shrine"));
    }

    #[test]
    fn a_non_combat_heading_has_no_level_but_is_still_typed_by_suffix() {
        // `AreaHeading` omits the em-dash and level when the location has no combat, so name
        // and type run together. Suffix matching is the only safe question to ask.
        let n = Node {
            key: "l2".into(),
            heading: "Ganstead woodland shrine".into(),
            x: 0.0,
            y: 0.0,
            connections: 2,
        };
        assert!(!n.has_combat());
        assert_eq!(n.level(), None);
        assert_eq!(n.type_name(), None);
        assert!(n.type_is("shrine"));
        assert!(n.type_is("woodland shrine"));
    }

    #[test]
    fn counts_hidden_neighbours_separately_from_subworld_exits() {
        // Cloud-covered and invisible neighbours print a bare marker with no key (`:1032`), and
        // the SAME marker appears in the exits loop -- so attribution depends on the section.
        let text = "\
Local overworld data:   Arrived at location     l1      Weedley Copse — level 0 crypt
    In subworld:        w3      Weedley Copse
    Adjacent connections:
Hidden location
        l4      Ganstead woodland shrine
          posX: 100.5   posY:   200.25   connections:    2
Hidden location
    Subworld exit positions:
          posX: 300.0   posY:   400.0    heading to:     w4      Cottam village
Hidden location
Local overworld data end";
        let a = &parse(&lines(text))[0];
        assert_eq!(a.reason, "Arrived at location");
        assert_eq!(a.subworld, Some(("w3".into(), "Weedley Copse".into())));
        assert_eq!(a.hidden, 2);
        assert_eq!(a.hidden_exits, 1);
        assert_eq!(a.nodes.len(), 1);
        assert_eq!(a.nodes[0].key, "l4");
        assert_eq!(a.exits.len(), 1);
        assert_eq!(a.exits[0].to_key, "w4");
        assert_eq!(a.exits[0].to_heading, "Cottam village");
        assert_eq!(a.exits[0].x, 300.0);
    }

    #[test]
    fn distinguishes_arrival_with_an_event() {
        // `:1442` picks between two reasons that share a prefix. Getting this backwards would
        // mean missing that an event screen is about to steal the foreground.
        let text = format!(
            "{START}   Arrived at location with event   l1      Somewhere\n\
             {END}"
        );
        assert_eq!(parse(&lines(&text))[0].reason, "Arrived at location with event");
    }

    #[test]
    fn ignores_an_incomplete_trailing_block() {
        // The console is polled while the game is mid-print. A block is only real once the
        // terminator lands, or a caller would act on half a node list.
        let partial = format!("{LIVE}\n{START}   Screen pan finished     start   Cottam campfire");
        let got = parse(&lines(&partial));
        assert_eq!(got.len(), 1, "the unterminated second block must not be returned");
        assert_eq!(got[0].reason, "World loaded");
    }

    #[test]
    fn reader_stitches_a_dump_split_across_two_reads() {
        // The failure this exists for: a real travel arrival was split by a console poll, the
        // header half was discarded as incomplete, and a SUCCESSFUL travel was reported as
        // "no arrival dump within 45 s".
        let mut r = Reader::new();
        let first = lines("Local overworld data:   Arrived at location  l1  Weedley Copse — level 0 crypt\n    Adjacent connections:");
        assert!(r.push(&first).is_empty(), "nothing is complete yet");

        let second = lines(
            "        start   Cottam campfire\n\
             \x20         posX: 930     posY:   504     connections:    1\n\
             Local overworld data end",
        );
        let got = r.push(&second);
        assert_eq!(got.len(), 1, "the stitched block must be returned exactly once");
        assert_eq!(got[0].here_key, "l1");
        assert_eq!(got[0].nodes.len(), 1);
        assert_eq!(got[0].nodes[0].key, "start");
    }

    #[test]
    fn reader_does_not_return_the_same_dump_twice() {
        let mut r = Reader::new();
        assert_eq!(r.push(&lines(LIVE)).len(), 1);
        assert_eq!(r.push(&lines("")).len(), 0, "a completed dump must not be re-emitted");
    }

    #[test]
    fn skips_unrelated_console_noise() {
        let text = format!("DIGGLE_NEEDLE_ALPHA\nLOVE 11.5 (Mysterious Mysteries)\n{LIVE}");
        assert_eq!(parse(&lines(&text)).len(), 1);
    }

    #[test]
    fn survives_an_unknown_reason() {
        // If the game gains a call site, degrade the reason string rather than lose the nodes.
        let text = "\
Local overworld data:   Teleported sideways     start   Cottam campfire
    Adjacent connections:
        l1      Weedley Copse — level 0 crypt
          posX: 777.8   posY:   486.9   connections:    4
Local overworld data end";
        let a = &parse(&lines(text))[0];
        assert_eq!(a.reason, "Teleported sideways");
        assert_eq!(a.nodes.len(), 1);
    }
}
