//! The advisor window's own settings: its size, "Stay on top", and the log box. Kept between
//! launches in `%LOCALAPPDATA%\DiggleAdvisor\settings.toml` ([`path`]).
//!
//! Nothing here goes near the game's folders: the save folder is watched, and a write there would
//! read as the game moving on. A missing or unreadable file means the defaults; a field that is
//! missing means its default, so a file from an older advisor still loads.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The window's outer size in pixels, as last restored (never its maximised size).
    pub width: i32,
    pub height: i32,
    pub topmost: bool,
    /// Whether the log box is shown. Hidden by default: the list is what the player reads.
    pub log_open: bool,
    /// The log box's height in pixels while it is shown.
    pub log_height: i32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { width: 1250, height: 720, topmost: false, log_open: false, log_height: 96 }
    }
}

/// The smallest window [`Settings::load`] will restore, so a bad file cannot make it vanish.
pub const MIN_WIDTH: i32 = 400;
pub const MIN_HEIGHT: i32 = 240;
/// The log box is never shorter than this while shown: about two lines.
pub const MIN_LOG_HEIGHT: i32 = 40;

/// Where the settings live, given `%LOCALAPPDATA%`.
pub fn path(local_appdata: &Path) -> PathBuf {
    local_appdata.join("DiggleAdvisor").join("settings.toml")
}

impl Settings {
    /// Reads `file`, or the defaults if it is missing or will not parse. Sizes are clamped to the
    /// minimums.
    pub fn load(file: &Path) -> Settings {
        let s: Settings = std::fs::read_to_string(file)
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default();
        Settings {
            width: s.width.max(MIN_WIDTH),
            height: s.height.max(MIN_HEIGHT),
            log_height: s.log_height.max(MIN_LOG_HEIGHT),
            ..s
        }
    }

    /// Writes to `file`, creating its folder. Through a temporary file and a rename, so a crash
    /// mid-write leaves the old settings rather than half of the new.
    pub fn save(&self, file: &Path) -> std::io::Result<()> {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = toml::to_string(self).map_err(std::io::Error::other)?;
        let tmp = file.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("diggle-advisor-settings-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        path(&dir)
    }

    #[test]
    fn missing_file_is_the_defaults() {
        assert_eq!(Settings::load(&scratch("missing")), Settings::default());
        assert!(!Settings::default().log_open, "the log starts collapsed");
    }

    #[test]
    fn round_trips_and_creates_the_folder() {
        let file = scratch("round-trip");
        let s =
            Settings { width: 900, height: 500, topmost: true, log_open: true, log_height: 150 };
        s.save(&file).unwrap();
        assert_eq!(Settings::load(&file), s);
    }

    #[test]
    fn missing_fields_take_their_defaults() {
        let file = scratch("partial");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "topmost = true\n").unwrap();
        let s = Settings::load(&file);
        assert!(s.topmost);
        assert_eq!(s.width, Settings::default().width);
    }

    #[test]
    fn garbage_is_the_defaults_and_tiny_sizes_are_clamped() {
        let file = scratch("bad");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "not = [toml").unwrap();
        assert_eq!(Settings::load(&file), Settings::default());
        std::fs::write(&file, "width = 5\nheight = -1\nlog_height = 0\n").unwrap();
        let s = Settings::load(&file);
        assert_eq!((s.width, s.height, s.log_height), (MIN_WIDTH, MIN_HEIGHT, MIN_LOG_HEIGHT));
    }
}
