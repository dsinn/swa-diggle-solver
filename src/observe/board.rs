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

/// One tile as the board reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct Tile {
    pub letter: String,
    /// Present only for special tiles, which the game writes as `{letter, extra}`
    /// (`tileboard.lua:2447`). Carries `burn`, `unselectable`, `ligature`, `destroy`.
    pub extra: Option<Table>,
}

impl Tile {
    /// A wildcard tile. The game stores these as `"."` (`tileboard.lua:406` treats `.` as selectable
    /// alongside letters), and a wildcard takes a typed letter directly.
    pub fn is_wildcard(&self) -> bool {
        self.letter == "."
    }

    pub fn burn(&self) -> Option<i64> {
        self.extra.as_ref()?.int_at("burn")
    }

    /// Unselectable tiles cannot form part of a word, so they must be excluded before searching.
    pub fn selectable(&self) -> bool {
        match &self.extra {
            Some(e) => e.path("unselectable").is_none(),
            None => true,
        }
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
}

const MARKER: &str = "board state = {";

fn tiles_from(table: &Table) -> Vec<Tile> {
    table
        .arr
        .iter()
        .filter_map(|v| match v {
            Value::Str(s) => Some(Tile { letter: s.clone(), extra: None }),
            // Special tiles are `{letter, extra}`.
            Value::Table(t) => {
                let letter = t.arr.first()?.as_str()?.to_string();
                let extra = t.arr.get(1).and_then(|v| v.as_table()).cloned();
                Some(Tile { letter, extra })
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
        assert!(b.tiles.iter().all(|t| t.extra.is_none()));
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
