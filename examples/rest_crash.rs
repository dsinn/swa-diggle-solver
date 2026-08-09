//! Reproduces the error a rest raises, using the game's own function and the real save files.
//!
//! Run with `cargo run --example rest_crash`. It launches nothing and touches nothing: it loads
//! `overworld/events/rested.lua` from `game_dir` into a LuaJIT state — the same runtime LÖVE uses,
//! since `mlua` is vendored with the `luajit` feature — and calls `requireCheck` the way
//! `FindValidEvents` does (`overworld/event.lua:6`).
//!
//! Only two modules are stubbed, and only because `rested.lua` pulls them in at load time:
//! `overworldview`, whose `areaFlag` is the real one-liner from `overworldview.lua:213`, and
//! `utils.events` for its colour table. **`requireCheck` itself is the game's, unedited.**
//!
//! Three cases are run, so a failure means something:
//!
//! 1. **the save as it is** — the claim under test;
//! 2. **with the two missing flags seeded** — the positive control. If this also errored, the
//!    harness would be broken rather than the game;
//! 3. **with `shrinePremonition` cleared** — the other way out, and a check that it really does
//!    short-circuit before the nil.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg = diggle_solver::config::Config::load(std::path::Path::new("config.toml"))?;
    let game_dir = std::fs::canonicalize(&cfg.game_dir)?;
    let save_dir = match &cfg.save_dir {
        Some(d) => d.clone(),
        None => std::path::PathBuf::from(std::env::var("APPDATA")?)
            .join("LOVE")
            .join("SternlyWordedAdventures"),
    };
    let lua_path = |p: &std::path::Path| p.to_string_lossy().replace('\\', "/");
    println!("game:  {}", lua_path(&game_dir));
    println!("saves: {}\n", lua_path(&save_dir));

    let lua = mlua::Lua::new();
    let harness = r#"
local gameDir, saveDir = ...

persistent = dofile(saveDir..'/persistentSaveData')
local main  = dofile(saveDir..'/mainSaveData')
local areaFlags = main.overworld.areaFlags

-- `overworldview.areaFlag` is the whole of overworldview.lua:213.
local shims = {
    overworldview = { areaFlag = function(flag) return areaFlags[flag] end },
    ['utils.events'] = { textColours = { cost = {}, black = {}, white = {}, npc = {} } },
}
local realRequire = require
require = function(name) return shims[name] or realRequire(name) end

-- The game's global; `love.math.random` is a plain RNG for our purposes.
love = { math = { random = math.random } }

local events = dofile(gameDir..'/overworld/events/rested.lua')
local check  = events[1].requireCheck

-- Whatever `doRest` was passed. The field is only tested for truthiness at the first clause.
local location = { key = 'l10sub4', seed = 0.5 }

-- Returns a fixed-shape row. `select(2, f())` inside a table constructor is truncated to one
-- value unless it is last, which printed the flag where the error message should have been.
local function try(label)
    local ok, err = pcall(check, location)
    return { label, ok, tostring(err) }
end

local out = {}
out[1] = try('the save as it is')
out[1][4] = tostring(areaFlags.shrinePremonition)
out[1][5] = tostring(persistent.unlockedModes)
out[1][6] = tostring(areaFlags.shrinePremonitionData)

persistent.unlockedModes = { physicsDream = true }
areaFlags.shrinePremonitionData = {}
out[2] = try('with both flags seeded (positive control)')

persistent.unlockedModes = nil
areaFlags.shrinePremonitionData = nil
areaFlags.shrinePremonition = nil
out[3] = try('with shrinePremonition cleared')

return out
"#;

    let out: mlua::Table = lua
        .load(harness)
        .call((lua_path(&game_dir), lua_path(&save_dir)))
        .map_err(|e| format!("harness: {e}"))?;

    for case in out.sequence_values::<mlua::Table>() {
        let case = case?;
        let label: String = case.get(1)?;
        let ok: bool = case.get(2)?;
        let result: String = case.get(3)?;
        println!("{label}:");
        if ok {
            println!("  requireCheck returned {result}");
        } else {
            println!("  **ERROR** {result}");
        }
        // Only the first case reports what the save actually holds.
        if let (Ok(p), Ok(u), Ok(d)) =
            (case.get::<String>(4), case.get::<String>(5), case.get::<String>(6))
        {
            println!("  shrinePremonition = {p}");
            println!("  persistent.unlockedModes = {u}");
            println!("  shrinePremonitionData = {d}");
        }
        println!();
    }
    Ok(())
}
