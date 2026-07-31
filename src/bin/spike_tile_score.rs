//! Differential test: the game's own `tiles.score` against our reimplementation.
//!
//! ## Why this exists
//!
//! Every number in [`diggle_solver::score`] is inference from reading Lua. We have never once
//! observed the game compute a score — the captured crypt fight was read at turn start and no word
//! was ever submitted. That is precisely the situation this project has been burned by before: a
//! model that looks right and has never been shown a positive control.
//!
//! It came to a head over borders. `utils/tiles.lua:48-50` does `local border =
//! tiles.getBorderData(tile)` and then `border.score`, while `getBorderData` returns **nil** unless
//! `tile.extra.border` is set — and `getSelectableLetterHash` (`tileboard.lua:1425-1428`) calls
//! `tilesUtil.score` on every selectable live tile, plain ones included. By that reading the game
//! should throw on the first plain tile of every turn. It has shipped that way since April 2026.
//! Either the reading is wrong or the model built on it is.
//!
//! ## Why this is cheap
//!
//! `mlua` is built against **LuaJIT**, the same runtime LÖVE embeds, and `tiles.score` needs only
//! three data modules: materials, borders, ligatures. None of them touch `love`. So the real
//! function can be loaded and called directly, with a `require` that reads the game's files and
//! reproduces `enumerate.hashRequire` (`utils/enumerate.lua:60-74`) without its `love.filesystem`
//! dependency.
//!
//! This is a **read-only** harness pointed at the game source. It never writes, and it never
//! launches the game.

use diggle_solver::config::Config;
use diggle_solver::game::save::Value;
use diggle_solver::observe::board::{Quality, Tile};
use diggle_solver::score::Scorer;
use mlua::{Lua, LuaOptions, StdLib};
use std::path::{Path, PathBuf};

/// Reproduces the game's module loading with the engine taken out.
///
/// `hashRequire` folds a directory of files into one table keyed by `data.key` or the file stem. The
/// cache entry is inserted **before** the directory is walked, because `material/default.lua`'s
/// `getMaterial` calls `require'rpg.effects.material'` and would otherwise recurse forever.
const PRELUDE: &str = r#"
-- Some effect files build draw data at load time (`rpg/effects/ligature/!!!.lua:25`). Nothing on
-- the scoring path reads it, so `love` is a stub that absorbs any indexing or call. If a value from
-- it ever reached the arithmetic we care about, it would fail loudly rather than pass silently.
local stub = {}
stub.__index = function() return setmetatable({}, stub) end
stub.__call  = function() return setmetatable({}, stub) end
love = setmetatable({}, stub)

local cache = {}

local function stem(filename) return (filename:gsub('%.lua$', '')) end

local function loadFile(path, name)
    local src = __read(path)
    local chunk, err = loadstring(src, name)
    if not chunk then error(name .. ': ' .. tostring(err)) end
    return chunk()
end

local hashDirs = {
    ['rpg.effects.material'] = 'rpg/effects/material',
    ['rpg.effects.border']   = 'rpg/effects/border',
    ['rpg.effects.ligature'] = 'rpg/effects/ligature',
}

function require(name)
    if cache[name] ~= nil then return cache[name] end
    local dir = hashDirs[name]
    if dir then
        local t = {}
        cache[name] = t          -- before the walk: default.lua's getMaterial requires this module
        for _, filename in ipairs(__list(dir)) do
            local key = stem(filename)
            local data = loadFile(dir .. '/' .. filename, key)
            if type(data) == 'table' then
                t[data.key or key] = data
            elseif data ~= nil then
                t[key] = data
            end
        end
        return t
    end
    local path = name:gsub('%.', '/') .. '.lua'
    local m = loadFile(path, name)
    cache[name] = m
    return m
end

return require'utils.tiles'
"#;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config::load(Path::new("config.toml"))?;
    let game_dir: PathBuf = cfg.game_dir.canonicalize()?;

    // String/table/math only: the game's data files need them, and nothing here needs io or os.
    let lua = Lua::new_with(
        StdLib::STRING | StdLib::TABLE | StdLib::MATH,
        LuaOptions::default(),
    )?;

    // Both file accessors are confined to the game directory. This harness reads the user's game
    // source and must not be able to wander out of it.
    let root = game_dir.clone();
    let read = lua.create_function(move |_, rel: String| {
        let p = root.join(&rel);
        let ok = p.canonicalize().map(|c| c.starts_with(&root)).unwrap_or(false);
        if !ok {
            return Err(mlua::Error::runtime(format!("refusing to read outside the game dir: {rel}")));
        }
        std::fs::read_to_string(&p).map_err(|e| mlua::Error::runtime(format!("{rel}: {e}")))
    })?;
    let root = game_dir.clone();
    let list = lua.create_function(move |lua, rel: String| {
        let mut names: Vec<String> = std::fs::read_dir(root.join(&rel))?
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".lua"))
            .collect();
        names.sort();
        lua.create_sequence_from(names)
    })?;
    lua.globals().set("__read", read)?;
    lua.globals().set("__list", list)?;

    let tiles: mlua::Table = lua.load(PRELUDE).eval()?;
    let score: mlua::Function = tiles.get("score")?;
    println!("loaded the game's utils/tiles.lua under LuaJIT\n");

    let scorer = Scorer::new(&cfg.game_dir)?;

    // Each case is (label, letter, extra pairs). `getFlag` is passed as nil, which is the no-gear
    // path -- exactly what our scorer models.
    let cases: Vec<(&str, &str, Vec<(&str, Value)>)> = vec![
        ("plain wood", "E", vec![]),
        ("plain bronze", "C", vec![]),
        ("plain silver2", "G", vec![]),
        ("plain silver3", "W", vec![]),
        ("plain gold letter", "Z", vec![]),
        ("premium X", "X", vec![]),
        ("premium J", "J", vec![]),
        ("premium Q", "Q", vec![]),
        ("wildcard", ".", vec![]),
        ("two-letter ligature", "TH", vec![]),
        ("three-letter ligature", "ING", vec![]),
        ("burning", "J", vec![("burn", Value::Int(3))]),
        ("gold bg on J", "J", vec![("bg", Value::Str("gold".into()))]),
        ("gold bg on E", "E", vec![("bg", Value::Str("gold".into()))]),
        ("wood bg + gold border", "E", vec![
            ("bg", Value::Str("wood".into())),
            ("border", Value::Str("gold".into())),
        ]),
        ("wood bg + iron border", "E", vec![
            ("bg", Value::Str("wood".into())),
            ("border", Value::Str("iron".into())),
        ]),
        ("gold border, no bg", "E", vec![("border", Value::Str("gold".into()))]),
        ("carbon 2", "E", vec![("carbon", Value::Int(2))]),
        ("woodornate scoreMult", "E", vec![("bg", Value::Str("woodornate".into()))]),
        ("ash ligature effect", "AE", vec![("ligature", Value::Str("ash".into()))]),
    ];

    let mut agree = 0usize;
    let mut differ = 0usize;
    let mut errored = 0usize;

    for (label, letter, pairs) in &cases {
        // Build the tile the way the game holds it: `extra` is always a table, never nil.
        let extra = lua.create_table()?;
        for (k, v) in pairs {
            match v {
                Value::Int(i) => extra.set(*k, *i)?,
                Value::Str(s) => extra.set(*k, s.as_str())?,
                _ => {}
            }
        }
        let tile = lua.create_table()?;
        tile.set("letter", *letter)?;
        tile.set("extra", extra)?;

        let theirs: Result<f64, mlua::Error> = score.call((tile, mlua::Value::Nil));

        // The same tile through our reader, so the comparison exercises Quality parsing too.
        let mut t = diggle_solver::game::save::Table::default();
        for (k, v) in pairs {
            t.map.insert((*k).to_string(), v.clone());
        }
        let ours = scorer.tile_score(&Tile {
            letter: letter.to_string(),
            quality: Quality::from_extra(&t),
        });

        match theirs {
            Ok(theirs) => {
                let same = (theirs - ours).abs() < 1e-9;
                if same {
                    agree += 1;
                } else {
                    differ += 1;
                }
                println!(
                    "{:<24} {:>5}  game {:>8.4}   ours {:>8.4}   {}",
                    label,
                    letter,
                    theirs,
                    ours,
                    if same { "ok" } else { "**DIFFER**" }
                );
            }
            Err(e) => {
                errored += 1;
                let msg = e.to_string();
                let first = msg.lines().next().unwrap_or("").trim();
                println!("{:<24} {:>5}  game ERROR: {first}   (ours {ours:.4})", label, letter);
            }
        }
    }

    println!("\n{agree} agree, {differ} differ, {errored} errored in the game's own code");
    if !scorer.unknown_materials().is_empty() {
        println!("our scorer met unknowns: {:?}", scorer.unknown_materials());
    }
    Ok(())
}
