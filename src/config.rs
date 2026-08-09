use serde::Deserialize;
use std::path::PathBuf;

/// `Default` is derived deliberately: later tasks add fields, and test helpers
/// construct Config with `..Default::default()` so they don't break each time.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    /// Directory containing the game's main.lua (e.g. ../sternly-worded-adventures)
    pub game_dir: PathBuf,
    /// Path to lovec.exe. MUST be the console build; love.exe will not write to our pipe.
    pub lovec_path: PathBuf,
    /// Optional path to mirror the raw verbose log to, for post-hoc inspection.
    #[serde(default)]
    pub log_mirror: Option<PathBuf>,
    /// Explicit save directory. Leave unset to derive it from how we launch:
    /// unfused (`lovec.exe <dir>`) writes to %APPDATA%\LOVE\SternlyWordedAdventures,
    /// while the fused Steam build writes to %APPDATA%\SternlyWordedAdventures.
    #[serde(default)]
    pub save_dir: Option<PathBuf>,
    /// How long a run may hold the mouse and keyboard, in minutes. `0` means no limit.
    ///
    /// The step cap this replaces was a guess about how long a run *ought* to take, and it kept
    /// cutting runs off mid-discovery. Time is the honest bound for a program driving real input:
    /// it maps to the thing actually being spent, and it does not shorten a run that is making
    /// progress the way a step count does.
    ///
    /// Here rather than in the code so that changing it does not need a rebuild — the number is a
    /// judgement about how long you are prepared to be away from the machine, which is yours and
    /// not the program's. `.diggle-stop` in the working directory ends a run early from any state.
    #[serde(default)]
    pub run_minutes: Option<u64>,
}

/// Used when `run_minutes` is absent. Long enough for a full run with rest detours — the run of
/// 2026-08-09 got through 45 steps, two forests, three villages and a mausoleum in under fifteen.
pub const DEFAULT_RUN_MINUTES: u64 = 60;

impl Config {
    pub fn load(path: &std::path::Path) -> Result<Self, crate::Error> {
        let text = std::fs::read_to_string(path)?;
        toml::from_str(&text).map_err(|e| crate::Error::Config(e.to_string()))
    }
}
