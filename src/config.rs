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
}

impl Config {
    pub fn load(path: &std::path::Path) -> Result<Self, crate::Error> {
        let text = std::fs::read_to_string(path)?;
        toml::from_str(&text).map_err(|e| crate::Error::Config(e.to_string()))
    }
}
