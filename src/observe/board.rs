//! Reading the tile board off the live log channel.
//!
//! `tileboard.logVerbose` (`tileboard.lua:1288-1297`) prints the board at the start of every player
//! turn, and the captured form is **multi-line Lua source**, not a single line:
//!
//! ```text
//! Player turn 0 start;    board state = {
//!     "O",
//!     "Y",
//!     ...
//!     "J",
//! }
//! ```
//!
//! Two things follow, both measured from a real crypt fight rather than inferred from the writer:
//!
//! 1. **It is a Lua literal**, so the same reader that loads save files parses it
//!    ([`crate::game::save`]) — no bespoke tokenizer, and special tiles (`{letter, extra}`) come out
//!    correctly for free.
//! 2. **It spans ~18 console rows**, so a caller polling the console will routinely see it split
//!    across reads. The block is only parsed once its closing brace arrives, for the same reason
//!    [`crate::observe::adjacency::Reader`] exists: a half-read dump silently truncated an arrival
//!    report and turned a successful travel into a reported failure.
//!
//! With plain `--verbose` the list is truncated to `totalTileCount`
//! (`table.reduceDownTo`, `utils/table.lua:245-251`), so **the number of entries IS the board size** —
//! which is also the threshold for the refresh rule (design v2 §8). Measured on the level-0 crypt: 16.

use crate::game::save::{parse, Table, Value};

/// A tile's state, parsed out of the dump's open-ended `extra` bag.
///
/// A tile is **not** just a letter, and increasingly less so as a run goes on: gear upgrades a
/// tile's material (wood → bronze → silver → gold), enemies downgrade it, fire sets `burn`, and the
/// carbon-paper item accumulates a per-use penalty. All of that rides in `extra`, and all of it
/// changes what the tile is worth. Parsing it once into named fields — rather than reaching into the
/// table with string keys from three different modules — is what lets the scorer read a tile's
/// *quality* instead of guessing it from the letter.
///
/// `unmodelled` is the point of doing this at all. `extra` is whatever the game decided to write, so
/// a key we do not consume is a silent gap in the score. Naming the leftovers turns "we ignored
/// something" from invisible into reportable.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Quality {
    /// `extra.bg` — the material, when the tile has been upgraded or downgraded away from the one
    /// its letter implies. Absent means "whatever `letterMaterials` says".
    pub material: Option<String>,
    /// `extra.border` — an independent overlay with its own score, blended 0.75/0.25 with the
    /// material (`utils/tiles.lua:78-82`).
    pub border: Option<String>,
    /// `extra.ligature` — an effect name on a real tile (`ash`), or a letter-class pattern on a
    /// wildcard (`[AEIOU]`). The two uses are distinguished by the tile's letter, not by this field.
    pub ligature: Option<String>,
    /// Burning tiles score zero (`utils/tiles.lua:56-57`).
    pub burn: Option<i64>,
    /// Carbon paper: `scoreMult * 0.9^carbon`, a penalty that grows with each use.
    pub carbon: Option<i64>,
    /// Locked by the tile itself rather than by a row or column rule.
    pub unselectable: bool,
    /// Keys in `extra` that nothing here reads. Non-empty means a tile is carrying state that may
    /// change its score, and we are scoring it as if it were not.
    pub unmodelled: Vec<String>,
}

/// Keys we deliberately read, plus those known to carry no scoring weight.
///
/// `destroy` and `bg2`/`fg` style keys are presentation or scheduling, not score. Listing them
/// explicitly means a genuinely new key still shows up in [`Quality::unmodelled`].
const HANDLED: &[&str] =
    &["bg", "border", "ligature", "burn", "carbon", "unselectable", "destroy", "itemSource", "itemModifier"];

impl Quality {
    pub fn from_extra(extra: &Table) -> Self {
        Quality {
            material: extra.str_at("bg").map(str::to_string),
            border: extra.str_at("border").map(str::to_string),
            ligature: extra.str_at("ligature").map(str::to_string),
            burn: extra.int_at("burn"),
            carbon: extra.int_at("carbon"),
            unselectable: extra.path("unselectable").is_some(),
            unmodelled: extra
                .map
                .keys()
                .filter(|k| !HANDLED.contains(&k.as_str()))
                .cloned()
                .collect(),
        }
    }
}

/// One tile as the board reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct Tile {
    pub letter: String,
    /// Parsed once, at dump time. Special tiles are written `{letter, extra}`
    /// (`tileboard.lua:2447`); a plain tile is a bare string and gets the default.
    pub quality: Quality,
}

impl Tile {
    /// An ordinary tile with no state — the overwhelmingly common case.
    pub fn plain(letter: &str) -> Self {
        Tile { letter: letter.to_string(), quality: Quality::default() }
    }

    /// A wildcard tile. The game stores these as `"."` (`tileboard.lua:406` treats `.` as selectable
    /// alongside letters), and a wildcard takes a typed letter directly.
    pub fn is_wildcard(&self) -> bool {
        self.letter == "."
    }

    pub fn burn(&self) -> Option<i64> {
        self.quality.burn
    }

    /// Unselectable tiles cannot form part of a word, so they must be excluded before searching.
    pub fn selectable(&self) -> bool {
        !self.quality.unselectable
    }
}

/// A board as printed at the start of a player turn.
#[derive(Debug, Clone, PartialEq)]
pub struct BoardDump {
    /// The turn number from the log line. `PlayerTurn.onStart` increments *after* logging, so this
    /// is one less than `combatSaveData`'s `rpg.player.turnNumber`.
    pub turn: u32,
    pub tiles: Vec<Tile>,
}

impl BoardDump {
    /// Total tiles on the board — and, because the plain `--verbose` dump is truncated to
    /// `totalTileCount`, the board size itself.
    pub fn total_tile_count(&self) -> usize {
        self.tiles.len()
    }

    /// The refresh threshold: a board is judged poor when the best word found is shorter than this.
    pub fn refresh_threshold(&self) -> usize {
        self.total_tile_count() / 2
    }

    /// Letters available to form words, excluding unselectable tiles.
    pub fn available_letters(&self) -> Vec<&str> {
        self.tiles.iter().filter(|t| t.selectable()).map(|t| t.letter.as_str()).collect()
    }

    /// Tile state present on this board that nothing models, as `letter: key` pairs.
    ///
    /// Must be checked before trusting a score. A tile carrying an unread key is being scored as if
    /// that key were not there, which is the quiet kind of wrong.
    pub fn unmodelled(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .tiles
            .iter()
            .flat_map(|t| t.quality.unmodelled.iter().map(|k| format!("{}: {k}", t.letter)))
            .collect();
        out.sort();
        out.dedup();
        out
    }
}

const MARKER: &str = "board state = {";

fn tiles_from(table: &Table) -> Vec<Tile> {
    table
        .arr
        .iter()
        .filter_map(|v| match v {
            Value::Str(s) => Some(Tile::plain(s)),
            // Special tiles are `{letter, extra}`.
            Value::Table(t) => {
                let letter = t.arr.first()?.as_str()?.to_string();
                let quality = t
                    .arr
                    .get(1)
                    .and_then(|v| v.as_table())
                    .map(Quality::from_extra)
                    .unwrap_or_default();
                Some(Tile { letter, quality })
            }
            _ => None,
        })
        .collect()
}

/// Extracts every **complete** board dump from a batch of console lines.
///
/// Incomplete trailing blocks are ignored rather than guessed at, so a caller polling mid-print never
/// acts on a partial board — playing a word against half a board would be worse than waiting.
pub fn parse_dumps(lines: &[String]) -> Vec<BoardDump> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let Some(marker_at) = lines[i].find(MARKER) else {
            i += 1;
            continue;
        };
        // `Player turn N start;` — the number is the only digits before the marker.
        let head = &lines[i][..marker_at];
        let turn: u32 = head
            .split_whitespace()
            .find_map(|w| w.parse().ok())
            .unwrap_or(0);

        // Collect from the opening brace to the matching close. Depth-counted, because a special
        // tile opens a nested table.
        let mut src = String::from("{");
        let mut depth = 1usize;
        let mut j = i + 1;
        while j < lines.len() && depth > 0 {
            for c in lines[j].chars() {
                match c {
                    '{' => depth += 1,
                    '}' => depth = depth.saturating_sub(1),
                    _ => {}
                }
            }
            src.push_str(lines[j].trim());
            src.push('\n');
            j += 1;
        }
        if depth == 0 {
            if let Ok(t) = parse(&format!("return {src}")) {
                out.push(BoardDump { turn, tiles: tiles_from(&t) });
            }
        }
        i = j.max(i + 1);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim from a real level-0 crypt fight, captured 2026-07-30
    /// (`tests/fixtures/crypt-combat-log.txt`). Using the real bytes is the point: I had expected a
    /// single log line and it is eighteen.
    const REAL: &str = include_str!("../../tests/fixtures/crypt-combat-log.txt");

    fn lines(s: &str) -> Vec<String> {
        s.lines().map(|l| l.to_string()).collect()
    }

    #[test]
    fn parses_the_real_crypt_board() {
        let dumps = parse_dumps(&lines(REAL));
        assert_eq!(dumps.len(), 1, "one board dump in the fixture");
        let b = &dumps[0];
        assert_eq!(b.turn, 0, "logged before turnNumber is incremented");
        assert_eq!(
            b.tiles.iter().map(|t| t.letter.as_str()).collect::<Vec<_>>(),
            ["O", "Y", "C", "A", "A", "C", "T", "P", "O", "R", "L", "I", "G", "A", "H", "J"]
        );
    }

    #[test]
    fn board_size_comes_from_the_dump_itself() {
        // The plain `--verbose` dump is truncated to totalTileCount, so counting entries gives the
        // board size with no extra data -- and with it the refresh threshold.
        let b = &parse_dumps(&lines(REAL))[0];
        assert_eq!(b.total_tile_count(), 16, "4x4 crypt board");
        assert_eq!(b.refresh_threshold(), 8);
    }

    #[test]
    fn every_tile_on_a_plain_board_is_selectable() {
        let b = &parse_dumps(&lines(REAL))[0];
        assert_eq!(b.available_letters().len(), 16);
        assert!(b.tiles.iter().all(|t| t.quality == Quality::default()));
        assert!(b.unmodelled().is_empty(), "no unread tile state: {:?}", b.unmodelled());
    }

    #[test]
    fn special_tiles_keep_their_extra_data() {
        // `tileboard.lua:2447` writes {letter, extra} for tiles with state. A burning or
        // unselectable tile must not be mistaken for a plain letter.
        let text = "Player turn 3 start;\tboard state = {\n\
                    \x20   \"A\",\n\
                    \x20   { \"B\", { burn = 3 } },\n\
                    \x20   { \"C\", { unselectable = 2 } },\n\
                    }";
        let b = &parse_dumps(&lines(text))[0];
        assert_eq!(b.turn, 3);
        assert_eq!(b.tiles.len(), 3);
        assert_eq!(b.tiles[1].letter, "B");
        assert_eq!(b.tiles[1].burn(), Some(3));
        assert!(b.tiles[1].selectable(), "burning tiles can still be used");
        assert!(!b.tiles[2].selectable(), "unselectable tiles must be excluded");
        assert_eq!(b.available_letters(), ["A", "B"]);
    }

    #[test]
    fn tile_quality_is_parsed_into_named_fields() {
        // Gear upgrades a tile's material and enemies downgrade it, so a tile is a letter PLUS a
        // quality. Reading `bg` and `border` off the dump is what lets an upgraded tile score as the
        // gold it now is rather than as the wood its letter implies.
        let text = "Player turn 1 start;\tboard state = {\n\
                    \x20   { \"E\", { bg = \"gold\", border = \"silver\", carbon = 2 } },\n\
                    }";
        let q = &parse_dumps(&lines(text))[0].tiles[0].quality;
        assert_eq!(q.material.as_deref(), Some("gold"));
        assert_eq!(q.border.as_deref(), Some("silver"));
        assert_eq!(q.carbon, Some(2));
        assert!(q.unmodelled.is_empty());
    }

    #[test]
    fn tile_state_nothing_reads_is_reported() {
        // `extra` is an open bag -- the game writes whatever the tile carries. A key we do not
        // consume is a silent gap in the score, so the leftovers are named. This is the check that
        // turns "the game gained a mechanic" from invisible into a line of output.
        let text = "Player turn 1 start;\tboard state = {\n\
                    \x20   { \"E\", { frozen = 3 } },\n\
                    }";
        let b = &parse_dumps(&lines(text))[0];
        assert_eq!(b.tiles[0].quality.unmodelled, ["frozen"]);
        assert_eq!(b.unmodelled(), ["E: frozen"]);
    }

    #[test]
    fn an_incomplete_dump_is_not_returned() {
        // The dump spans ~18 console rows, so a poll routinely lands mid-block. Acting on a partial
        // board would mean scoring words against tiles that are not there.
        let partial = "Player turn 1 start;\tboard state = {\n    \"A\",\n    \"B\",";
        assert!(parse_dumps(&lines(partial)).is_empty());
    }

    #[test]
    fn ignores_the_other_verbose_output_around_it() {
        // The same channel carries `Player info:`, `Lore screen:` and the overworld adjacency dumps.
        let dumps = parse_dumps(&lines(REAL));
        assert_eq!(dumps.len(), 1);
        assert!(REAL.contains("Player info:"), "fixture really does contain other output");
        assert!(REAL.contains("Local overworld data:"));
    }
}
