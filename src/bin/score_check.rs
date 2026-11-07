//! Differential check: every word the advisor lists, scored by the **game's own Lua** and by ours.
//!
//! `score_check <combatSaveData> [--save-dir <dir>]`
//!
//! ## Why
//!
//! The user, 2026-11-07: words the advisor called kills *often fall one damage short*. Our scoring
//! is a reimplementation (see [`diggle_solver::score`]), and reading Lua had found the gaps it could.
//! This runs the real code instead — `utils/words.lua`'s `words.score` with every wordBonus
//! modifier file, the real materials' `active` rules, `passiveSpellEffectBoost`, and each spell's
//! own `check` and `estimate` — on the same tiles our search chose, under the save's gear flags.
//! **A word where we score higher than the game is the dangerous kind**: it is a kill we promise and
//! the game does not deliver. Those are listed first.
//!
//! ## What is real and what is stubbed
//!
//! Loaded from the game checkout: `utils/*` (words, tiles, effects, letters, dictionaries, lexica,
//! table, maths), `rpg/effects/*` (materials, borders, ligatures, modifiers, spells). Stubbed: the
//! engine (`love`), and the parts of `rpgview`/`tileboard` these read — gear flags, the turn, the
//! enemy's statuses and kind, the player's armour — answered from the save. Anything under `ui.`,
//! and any other engine module, is an inert stub; if a value from one reached the arithmetic it
//! would fail loudly.
//!
//! Compared: the **blow** — the damage the hit deals, before the weapon layer and without the
//! statuses a word applies, which the game does not put in this number either.
//!
//! Read-only: it reads the game source and the save, and writes nothing.

use diggle_solver::config::Config;
use diggle_solver::observe::board::Tile;
use diggle_solver::search::Candidate;
use mlua::{Lua, LuaOptions, StdLib};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const PRELUDE: &str = r#"
-- `conf.lua:1`: the game makes indexing nil yield nil, and leans on it — `tiles.score` reads
-- `border.score` with no border at all (`utils/tiles.lua:48-50`). Without this the real code throws
-- where the game does not.
debug.setmetatable(nil, {__index={}})
local stub = {}
stub.__index = function() return setmetatable({}, stub) end
stub.__call  = function() return setmetatable({}, stub) end
love = setmetatable({}, stub)
_VERBOSE = false

local cache = {}
local function stem(f) return (f:gsub('%.lua$', '')) end
local function loadFile(path, name)
    local chunk, err = loadstring(__read(path), name)
    if not chunk then error(name .. ': ' .. tostring(err)) end
    return chunk()
end

local function hashDir(dir)
    local t = {}
    for _, f in ipairs(__list(dir)) do
        local data = loadFile(dir .. '/' .. f, stem(f))
        if type(data) == 'table' then t[data.key or stem(f)] = data
        elseif data ~= nil then t[stem(f)] = data end
    end
    return t
end

-- `utils/enumerate.lua` walks folders through `love.filesystem`; these are its two entry points.
local enumerate = {
    luaFiles = function(folder)
        local out = {}
        for _, f in ipairs(__list(folder)) do
            local data = loadFile(folder .. '/' .. f, stem(f))
            if type(data) == 'table' then table.insert(out, data) end
        end
        return out
    end,
    hashRequire = function(folder) return hashDir(folder) end,
}

local hashDirs = {
    ['rpg.effects.material'] = 'rpg/effects/material',
    ['rpg.effects.border']   = 'rpg/effects/border',
    ['rpg.effects.ligature'] = 'rpg/effects/ligature',
    ['rpg.effects.spell']    = 'rpg/effects/spell',
}

local function real(name)
    return name:match('^utils%.') or name:match('^rpg%.effects')
end

function require(name)
    if cache[name] ~= nil then return cache[name] end
    if name == 'utils.enumerate' then cache[name] = enumerate; return enumerate end
    local dir = hashDirs[name]
    if dir then
        local t = {}
        cache[name] = t
        for k, v in pairs(hashDir(dir)) do t[k] = v end
        return t
    end
    if not real(name) then
        local s = setmetatable({}, stub)
        cache[name] = s
        return s
    end
    cache[name] = true -- a cycle sees a placeholder rather than recursing
    local m = loadFile(name:gsub('%.', '/') .. '.lua', name)
    cache[name] = m == nil and true or m
    return cache[name]
end

require'utils.table'
require'utils.maths'
"#;

/// Scores one word the game's way. `T` is the word's tiles, in order, as the game holds them.
const SCORE: &str = r#"
local words = require'utils.words'
local effects = require'utils.effects'
local materials = require'rpg.effects.material'
local ligatures = require'rpg.effects.ligature'
local spellEffects = require'rpg.effects.spell'
local wordCacheMods = require'rpg.effects.modifiers'.wordCache

return function(T)
    local wordTiles = T
    local s = ''
    for _, t in ipairs(wordTiles) do s = s .. (t.letterTemp or t.letter) end
    local LAZYWORD = s:upper()
    -- `wordboard.updateEffectValues` (`wordboard.lua:224-245`).
    local cache = words.basicCache(s)
    wordTiles.cache = cache
    for _, data in ipairs(wordCacheMods) do
        if rpgview.getPlayerGearFlag(data.flag) then data.wordCache(LAZYWORD, wordTiles, cache) end
    end
    local score = words.score(LAZYWORD, wordTiles, rpgview.getPlayerGearFlag)
    -- `updateSpecialEffectTileHighlights` and `isTileSpellEffectWord` (`wordboard.lua:100-124,346-356`).
    local canSpell = not rpgview.getPlayerGearFlag'noCastSpell'
    local school
    for _, tile in ipairs(wordTiles) do
        local m = materials[tile.extra.bg]
        if m and m.active and (canSpell or not m.categories.spell) and m.active(wordTiles, tile) then
            school = school or m.categories.spell
        end
        local l = ligatures[tile.extra.ligature]
        if l and l.active and (canSpell or not l.categories.spell) and l.active(wordTiles, tile) then
            school = school or l.categories.spell
        end
    end
    local plain = school and effects.passiveSpellEffectBoost(score, 1, school) or score
    -- `getMagicWordPowers` (`wordboard.lua:168-209`), each spell's damage from its own `estimate`.
    local offers = {}
    if canSpell then
        for flag, data in sortedpairs(spellEffects) do
            if rpgview.getPlayerGearFlag(flag) and data.check(LAZYWORD, wordTiles) then
                __estimate = 0
                if data.estimate then data.estimate(score, 1) end
                table.insert(offers, {flag, __estimate or 0})
            end
        end
    end
    local best, cast = plain, nil
    if __alwaysCast and #offers == 1 then
        best, cast = offers[1][2], offers[1][1]
    else
        for _, o in ipairs(offers) do
            if o[2] > best then best, cast = o[2], o[1] end
        end
    end
    return best, cast or '', score, school or ''
end
"#;

struct Args {
    save: PathBuf,
    save_dir: Option<PathBuf>,
}

fn args() -> Args {
    let mut it = std::env::args().skip(1);
    let mut save = None;
    let mut save_dir = None;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--save-dir" => save_dir = it.next().map(PathBuf::from),
            _ => save = Some(PathBuf::from(a)),
        }
    }
    Args { save: save.expect("usage: score_check <combatSaveData> [--save-dir <dir>]"), save_dir }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = args();
    // As the advisor finds it: `config.toml`'s `game_dir`, else the sibling checkout.
    let game_dir: PathBuf = Config::load(Path::new("config.toml"))
        .map(|c| c.game_dir)
        .unwrap_or_else(|_| PathBuf::from("../sternly-worded-adventures"))
        .canonicalize()?;
    let src = std::fs::read_to_string(&args.save)?;
    let cs = diggle_solver::game::save::parse(&src)?;
    let save_dir = args
        .save_dir
        .clone()
        .or_else(|| args.save.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    let always_cast = diggle_solver::spells::always_cast_from_config(&save_dir);

    // Ours: the advisor's list, every word, as the advisor builds it.
    let res = diggle_solver::advisor::view::Resources::load(&game_dir)?;
    let mut peaks = HashMap::new();
    let mut plan =
        diggle_solver::turn::plan(&cs, &game_dir, &res.letters, &mut peaks, Default::default())?
            .ok_or("the save has no board")?;
    let undead = res.bestiary.undead(&plan.name);
    let mut book = diggle_solver::spells::SpellBook::from_save(&cs, &res.lexica, undead);
    book.always_cast = always_cast;
    book.choose = true;
    plan.mods.spells = Some(book);
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let list = diggle_solver::search::ranked(
        &res.dict,
        &res.scorer,
        &plan.tiles,
        &plan.geom,
        &plan.mods,
        diggle_solver::search::Goal::MaxDamage,
        &plan.picking,
        threads,
    );
    println!(
        "{}: {} words on {} tiles, turn {}, alwaysCastSpell {always_cast}, enemy {} (undead {undead:?})",
        args.save.display(),
        list.len(),
        plan.tiles.len(),
        cs.int_at("rpg.player.turnNumber").unwrap_or(-1),
        plan.name
    );

    // Theirs.
    // SAFETY: `debug` is loaded only for `conf.lua`'s nil metatable, which the game's code depends
    // on (see PRELUDE). No `io`, `os` or `package`: the code reaches files only through `__read` and
    // `__list`, which are confined to the game directory.
    let lua = unsafe {
        Lua::unsafe_new_with(
            StdLib::STRING | StdLib::TABLE | StdLib::MATH | StdLib::DEBUG,
            LuaOptions::default(),
        )
    };
    let root = game_dir.clone();
    lua.globals().set(
        "__read",
        lua.create_function(move |_, rel: String| {
            let p = root.join(&rel);
            if !p.canonicalize().map(|c| c.starts_with(&root)).unwrap_or(false) {
                return Err(mlua::Error::runtime(format!("outside the game dir: {rel}")));
            }
            std::fs::read_to_string(&p).map_err(|e| mlua::Error::runtime(format!("{rel}: {e}")))
        })?,
    )?;
    let root = game_dir.clone();
    lua.globals().set(
        "__list",
        lua.create_function(move |lua, rel: String| {
            let mut names: Vec<String> = std::fs::read_dir(root.join(&rel))?
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".lua"))
                .collect();
            names.sort();
            lua.create_sequence_from(names)
        })?,
    )?;
    stub_game(&lua, &cs, undead.unwrap_or(false), always_cast)?;
    lua.load(PRELUDE).exec()?;
    let score: mlua::Function = lua.load(SCORE).eval()?;

    let mut rows = Vec::new();
    for c in &list {
        let theirs = game_score(&lua, &score, &plan.tiles, c)?;
        let ours = c.score - c.afflictions.total();
        rows.push((ours - theirs.0, c, theirs));
    }
    let over: Vec<_> = rows.iter().filter(|r| r.0 > 0).collect();
    let under: Vec<_> = rows.iter().filter(|r| r.0 < 0).collect();
    println!(
        "agree {}, ours HIGHER (game falls short) {}, ours lower {}",
        rows.len() - over.len() - under.len(),
        over.len(),
        under.len()
    );
    let show = |title: &str, mut v: Vec<&(i64, &Candidate, (i64, String, i64, String))>| {
        v.sort_by_key(|r| -r.0.abs());
        println!("\n{title}");
        for (d, c, (theirs, cast, raw, school)) in v.iter().take(25) {
            println!(
                "  {:+3}  {:<14} ours {:>4} ({:<10}) game {:>4} ({:<12} raw {raw}, school {school}) tiles {:?}",
                d,
                c.word,
                c.score - c.afflictions.total(),
                c.cast.unwrap_or("plain"),
                theirs,
                if cast.is_empty() { "plain" } else { cast.as_str() },
                c.typed.tiles
            );
        }
    };
    // The hardest few, agreeing or not, so a reader can see the bonuses actually fire.
    println!(
        "
hardest five:"
    );
    for (d, c, (theirs, cast, raw, _)) in rows.iter().take(5) {
        println!(
            "  {:<14} ours {:>4} game {theirs:>4} ({}, raw {raw}) diff {d:+}",
            c.word,
            c.score - c.afflictions.total(),
            if cast.is_empty() { "plain" } else { cast.as_str() }
        );
    }
    show("ours HIGHER than the game — promised damage the game does not deal:", over);
    show("ours lower than the game:", under);
    Ok(())
}

/// The engine half the scoring path reads, answered from the save.
fn stub_game(
    lua: &Lua, cs: &diggle_solver::game::save::Table, undead: bool, always_cast: bool,
) -> mlua::Result<()> {
    let flags = lua.create_table()?;
    if let Some(t) = cs.table_at("rpg.player.gearFlags") {
        for (k, v) in &t.map {
            if let Some(n) = v.as_f64() {
                flags.set(k.as_str(), n)?;
            } else {
                flags.set(k.as_str(), true)?;
            }
        }
    }
    let statuses = lua.create_table()?;
    if let Some(t) = cs.table_at("rpg.enemy.statusEffects") {
        for (k, v) in &t.map {
            statuses.set(k.as_str(), v.as_f64().unwrap_or(1.0))?;
        }
    }
    let pstatus = lua.create_table()?;
    if let Some(t) = cs.table_at("rpg.player.statusEffects") {
        for (k, v) in &t.map {
            pstatus.set(k.as_str(), v.as_f64().unwrap_or(1.0))?;
        }
    }
    let g = lua.globals();
    g.set("__flags", flags)?;
    g.set("__statuses", statuses)?;
    g.set("__pstatus", pstatus)?;
    g.set("__turn", cs.int_at("rpg.player.turnNumber").unwrap_or(1))?;
    g.set("__undead", undead)?;
    g.set("__alwaysCast", always_cast)?;
    g.set("__maxHealth", cs.int_at("rpg.player.maxHealth").unwrap_or(1))?;
    g.set("__armour", cs.int_at("rpg.player.armour").unwrap_or(0))?;
    lua.load(
        r#"
        rpgview = {
            getPlayerGearFlag = function(f) return __flags[f] end,
            getPlayerStatus = function(k) return __pstatus[k] end,
            getTurn = function() return __turn end,
            getEnemyRawStatuses = function() return __statuses end,
            getEnemyStatus = function(k) return __statuses[k] end,
            currentEnemyHasCat = function(c) return c == 'undead' and __undead end,
            getPlayerMaxArmour = function()
                return math.ceil(__maxHealth / (__flags.maxArmourHalved and 2 or 1))
            end,
            estimateDamage = function(v) __estimate = v end,
            setPlayerEstimate = function() end,
            affectEnemyEstimate = function() end,
            affectPlayerEstimate = function() end,
        }
        function rpgview.getPlayerMissingArmour()
            local max = rpgview.getPlayerMaxArmour()
            return math.min(math.max(max - __armour, 0), max)
        end
        tileboard = {
            getSelectedCornerTileCount = function() return 0 end,
            getCornerCount = function() return 4 end,
        }
        function getActiveMode() return { type = 'rpg' } end
        "#,
    )
    .exec()
}

/// The game's (damage, spell, raw score, school) for `c` on its own tiles.
fn game_score(
    lua: &Lua, score: &mlua::Function, board: &[Tile], c: &Candidate,
) -> mlua::Result<(i64, String, i64, String)> {
    let tiles = lua.create_table()?;
    for (n, (i, wild)) in c.typed.steps().enumerate() {
        let t = &board[i];
        let extra = lua.create_table()?;
        if let Some(m) = &t.quality.material {
            extra.set("bg", m.as_str())?;
        }
        if let Some(b) = &t.quality.border {
            extra.set("border", b.as_str())?;
        }
        if let Some(l) = &t.quality.ligature {
            extra.set("ligature", l.as_str())?;
        }
        if let Some(b) = t.quality.burn {
            extra.set("burn", b)?;
        }
        if let Some(k) = t.quality.carbon {
            extra.set("carbon", k)?;
        }
        let tile = lua.create_table()?;
        tile.set("letter", t.letter.as_str())?;
        tile.set("extra", extra)?;
        if let Some(ch) = wild {
            tile.set("letterTemp", ch.to_string())?;
        }
        tiles.set(n + 1, tile)?;
    }
    let (best, cast, raw, school): (f64, String, f64, String) = score.call(tiles)?;
    Ok((best as i64, cast, raw as i64, school))
}
