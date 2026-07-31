//! Reading the game's save files.
//!
//! A save is a **Lua source file returning a table literal**:
//!
//! ```lua
//! return {
//!     passives = { "bedroll" },
//!     overworld = { seed = 0, playerLocation = "start" },
//! }
//! ```
//!
//! So it is loaded with `mlua` rather than a hand-rolled parser — which is what `mlua` is in the
//! dependency list for. The chunk is evaluated in a state with **no standard library**: these files
//! are pure data, so nothing legitimate in them needs `io`, `os` or `package`, and a data file
//! should not be able to reach them.
//!
//! Which files matter, and when they are written:
//!
//! - `combatSaveData` — written by `rpg.save` (`rpg.lua:42-48`), and notably from
//!   `PlayerTurn.onStart` and `WaitPhase.onStart` (`rpgview.lua:1450`, `:1567`). So it is refreshed
//!   at exactly the moments the game hands control back to the player.
//! - `mainSaveData` — one writer, `overworld:save()` (`overworld.lua:562-568`), called on screen
//!   **exit**. A read taken right after an action legitimately shows the pre-action value; that is
//!   flush timing, not failure (design v2 §5).
//!
//! **Never touch `%APPDATA%\SternlyWordedAdventures`** — that is the user's real Steam save.
//! Diggle's sandbox is `%APPDATA%\LOVE\SternlyWordedAdventures`; see [`super::savedir`].

use mlua::{Lua, LuaOptions, StdLib};
use std::collections::BTreeMap;
use std::path::Path;

/// A Lua data value from a save file.
///
/// Integers are kept distinct from floats because the save mixes them freely — `health = 12` beside
/// `posX = 777.81645222258` — and a board full of letters is indexed by position, where silently
/// coercing to `f64` would be an invitation to off-by-one.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Num(f64),
    Str(String),
    Table(Table),
}

/// A Lua table, splitting the array part from the named part the way Lua itself does.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Table {
    /// 1-based sequence entries, stored 0-based.
    pub arr: Vec<Value>,
    pub map: BTreeMap<String, Value>,
}

impl Table {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.map.get(key)
    }

    /// Follows a dotted path, e.g. `"rpg.player.turnState"`.
    ///
    /// Returns `None` for a missing key rather than erroring: an absent field in a save is normal
    /// (the game omits nils), so absence is data, not a fault.
    pub fn path(&self, path: &str) -> Option<&Value> {
        let mut cur = self.map.get(path.split('.').next()?)?;
        for key in path.split('.').skip(1) {
            cur = match cur {
                Value::Table(t) => t.map.get(key)?,
                _ => return None,
            };
        }
        Some(cur)
    }

    pub fn str_at(&self, path: &str) -> Option<&str> {
        self.path(path)?.as_str()
    }

    pub fn int_at(&self, path: &str) -> Option<i64> {
        self.path(path)?.as_int()
    }

    pub fn table_at(&self, path: &str) -> Option<&Table> {
        match self.path(path)? {
            Value::Table(t) => Some(t),
            _ => None,
        }
    }
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Numbers arrive as either Lua integers or floats; accept both, but only when the float is
    /// exactly integral, so a genuine fractional value is never silently truncated.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Num(n) if n.fract() == 0.0 => Some(*n as i64),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Num(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_table(&self) -> Option<&Table> {
        match self {
            Value::Table(t) => Some(t),
            _ => None,
        }
    }
}

fn convert(v: mlua::Value, lenient: bool) -> Result<Value, crate::Error> {
    Ok(match v {
        mlua::Value::Nil => Value::Nil,
        mlua::Value::Boolean(b) => Value::Bool(b),
        mlua::Value::Integer(i) => Value::Int(i),
        mlua::Value::Number(n) => Value::Num(n),
        mlua::Value::String(s) => Value::Str(s.to_str()?.to_owned()),
        mlua::Value::Table(t) => Value::Table(convert_table(t, lenient)?),
        // Functions, userdata and threads cannot appear in a data-only save. Treating them as Nil
        // rather than erroring would hide a save that is not what we think it is. Game *modules*
        // are a different matter — see [`parse_module`] — so they pass `lenient`.
        other => {
            if lenient {
                Value::Nil
            } else {
                return Err(crate::Error::Config(format!(
                    "save contains a non-data value: {:?}",
                    other.type_name()
                )));
            }
        }
    })
}

fn convert_table(t: mlua::Table, lenient: bool) -> Result<Table, crate::Error> {
    let mut out = Table::default();
    // The sequence part first, so `arr` keeps save order. `pairs` order is unspecified in Lua, and
    // the tileboard's letters are positional — reading them out of order would scramble the board.
    for item in t.clone().sequence_values::<mlua::Value>() {
        out.arr.push(convert(item?, lenient)?);
    }
    let len = out.arr.len() as i64;
    for pair in t.pairs::<mlua::Value, mlua::Value>() {
        let (k, v) = pair?;
        match k {
            // Already captured by the sequence walk above.
            mlua::Value::Integer(i) if i >= 1 && i <= len => {}
            mlua::Value::Integer(i) => {
                out.map.insert(i.to_string(), convert(v, lenient)?);
            }
            mlua::Value::String(s) => {
                out.map.insert(s.to_str()?.to_owned(), convert(v, lenient)?);
            }
            mlua::Value::Number(n) => {
                out.map.insert(n.to_string(), convert(v, lenient)?);
            }
            _ => {}
        }
    }
    Ok(out)
}

fn eval(source: &str, lenient: bool) -> Result<Table, crate::Error> {
    // No stdlib: a data file has no business calling anything.
    let lua = Lua::new_with(StdLib::NONE, LuaOptions::default())
        .map_err(|e| crate::Error::Config(format!("lua init: {e}")))?;
    let value: mlua::Value = lua.load(source).eval()?;
    match convert(value, lenient)? {
        Value::Table(t) => Ok(t),
        other => Err(crate::Error::Config(format!(
            "source did not return a table, got {other:?}"
        ))),
    }
}

/// Parses save-file *source*. Separated from [`load`] so it is testable without a save on disk.
pub fn parse(source: &str) -> Result<Table, crate::Error> {
    eval(source, false)
}

/// Parses a **game source module** that is data with a thin dressing of code.
///
/// Several of the game's data tables are not quite pure data: `items/boardshapes.lua` opens with
/// `local utils = require'utils.items'` and sprinkles `purchaseFunction = get'give'.passive` through
/// the entries. The numbers we want — board size, corner coordinates — sit in the same literal.
///
/// Two accommodations, and no more than two:
///
/// - `require` is shimmed to a stub whose fields are functions returning empty tables, so the
///   dressing evaluates to nothing instead of erroring. It reads no files.
/// - Function values convert to [`Value::Nil`] rather than failing, because in a module they are
///   expected. [`parse`] stays strict, so a *save* containing a function is still an error.
///
/// The stdlib is still absent, so a module cannot reach `io` or `os` any more than a save can.
pub fn parse_module(source: &str) -> Result<Table, crate::Error> {
    // The stub has to survive being indexed *and then called* — `get'class'.exclude{...}`
    // (`items/boardshapes.lua:41`) does both — so every field of a stub is another stub function.
    // A metatable is the only way to say that without enumerating the component names, which would
    // break the next time the game adds one.
    const SHIM: &str = "\
local __meta = {}
local function __stub() return setmetatable({}, __meta) end
__meta.__index = function() return __stub end
__meta.__call = function() return __stub() end
local function require(name) return __stub() end
";
    eval(&format!("{SHIM}{source}"), true)
}

/// Reads and parses a save file.
///
/// A missing file is reported as such rather than as an empty table: `combatSaveData` is absent
/// whenever no run is in progress, and "no combat" must be distinguishable from "combat with no
/// data" — conflating absence with emptiness has produced false conclusions on this project more
/// than once.
pub fn load(path: &Path) -> Result<Table, crate::Error> {
    let source = std::fs::read_to_string(path)?;
    parse(&source)
}

/// True when a run is in progress, i.e. `combatSaveData` exists.
pub fn combat_in_progress(save_dir: &Path) -> bool {
    save_dir.join("combatSaveData").is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped like the real thing, including the mix of int and float and a nested table.
    const SAMPLE: &str = r#"
return {
    passives = { "bedroll" },
    gear = {},
    scenario = { enemiesMean = 6, enemiesSD = 1 },
    tileboard = { columns = { 0, 0, 0, 0 } },
    overworld = {
        seed = 0,
        activityCounter = 0.35,
        playerLocation = "start",
        completedAreas = { start = true },
    },
    rpg = { player = { health = 12, turnState = "PlayerTurn" } },
}
"#;

    #[test]
    fn reads_nested_values_by_path() {
        let t = parse(SAMPLE).unwrap();
        assert_eq!(t.str_at("overworld.playerLocation"), Some("start"));
        assert_eq!(t.int_at("scenario.enemiesMean"), Some(6));
        assert_eq!(t.str_at("rpg.player.turnState"), Some("PlayerTurn"));
        assert_eq!(t.path("overworld.activityCounter").unwrap().as_f64(), Some(0.35));
        assert_eq!(t.path("overworld.completedAreas.start").unwrap().as_bool(), Some(true));
    }

    #[test]
    fn a_missing_key_is_none_not_an_error() {
        // The game omits nil fields, so absence is ordinary data.
        let t = parse(SAMPLE).unwrap();
        assert_eq!(t.path("rpg.enemy"), None);
        assert_eq!(t.str_at("overworld.nope"), None);
        assert_eq!(t.path("overworld.playerLocation.deeper"), None, "indexing a scalar is None");
    }

    #[test]
    fn array_order_is_preserved() {
        // The tileboard is a positional list of letters; reading it in `pairs` order would
        // scramble the board.
        let t = parse(r#"return { tileboard = { "A", "B", "C", "D", "E" } }"#).unwrap();
        let letters: Vec<&str> =
            t.table_at("tileboard").unwrap().arr.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(letters, ["A", "B", "C", "D", "E"]);
    }

    #[test]
    fn keeps_the_named_part_alongside_the_array_part() {
        // Exactly the tileboard's shape: a list of letters that also carries `columns`.
        // `#letters` must not include `columns`, or the board/queue split point moves.
        let t = parse(r#"return { tileboard = { "A", "B", columns = { 2, 0 } } }"#).unwrap();
        let tb = t.table_at("tileboard").unwrap();
        assert_eq!(tb.arr.len(), 2, "columns must not land in the array part");
        assert_eq!(tb.get("columns").unwrap().as_table().unwrap().arr.len(), 2);
    }

    #[test]
    fn a_tile_with_extra_data_is_a_nested_table() {
        // `tileboard.lua:2447` writes {letter, extra} instead of a bare letter for special tiles,
        // so the list is heterogeneous and consumers must handle both forms.
        let t = parse(r#"return { tileboard = { "A", { "B", { burn = 3 } } } }"#).unwrap();
        let arr = &t.table_at("tileboard").unwrap().arr;
        assert_eq!(arr[0].as_str(), Some("A"));
        let special = arr[1].as_table().unwrap();
        assert_eq!(special.arr[0].as_str(), Some("B"));
        assert_eq!(special.arr[1].as_table().unwrap().int_at("burn"), Some(3));
    }

    #[test]
    fn stdlib_is_unavailable_to_a_save_file() {
        // A save is data. If one ever calls out to the runtime, that is a signal worth failing on,
        // not something to execute.
        let err = parse(r#"return { x = os.time() }"#).unwrap_err();
        assert!(
            format!("{err}").contains("lua"),
            "expected a Lua error, got: {err}"
        );
    }

    #[test]
    fn rejects_a_source_that_returns_a_scalar() {
        assert!(parse("return 5").is_err());
    }
}
