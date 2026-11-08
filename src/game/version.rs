//! The game's version, and data read from the game's files kept in memory while it holds.
//!
//! The lexica and the bestiary are plain data in the game's Lua files, and they only change when
//! the game does. Reading them on every turn cost 25–90 ms, and up to 5 s while another program
//! was writing to the same disk (the Advisor's timing log, 2026-11-08: Deadwood's plan took
//! 4956 ms while a build ran). So each is read once per game folder and version, the user's call,
//! 2026-11-08: "lexica are static once we launch the game".

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The game's version, as `main.lua:2` declares it (`local version = 53.8`), the one
/// `getGameVersion` returns (`main.lua:20`). `None` if the file or the line is missing.
pub fn version(game_dir: &Path) -> Option<String> {
    let src = std::fs::read_to_string(game_dir.join("main.lua")).ok()?;
    parse_version(&src)
}

fn parse_version(src: &str) -> Option<String> {
    src.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("local version")?.trim_start().strip_prefix('=')?;
        let v = rest.split("--").next()?.trim();
        (!v.is_empty()).then(|| v.to_string())
    })
}

/// One thing loaded from the game's files, per game folder, kept while the game's version stays
/// the same. A folder whose version cannot be read is loaded afresh every time, as before.
pub struct ByVersion<T> {
    loaded: Mutex<Vec<(PathBuf, String, Arc<T>)>>,
}

impl<T> ByVersion<T> {
    pub const fn new() -> Self {
        ByVersion { loaded: Mutex::new(Vec::new()) }
    }

    /// What `load` makes of `game_dir`, from memory if the game's version is the one it was
    /// loaded at. A failed load is not kept, so the next call tries again.
    pub fn get<E>(
        &self, game_dir: &Path, load: impl FnOnce(&Path) -> Result<T, E>,
    ) -> Result<Arc<T>, E> {
        let Some(version) = version(game_dir) else { return load(game_dir).map(Arc::new) };
        let mut loaded = self.loaded.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, v, data)) = loaded.iter().find(|(dir, _, _)| dir == game_dir) {
            if *v == version {
                return Ok(data.clone());
            }
        }
        let data = Arc::new(load(game_dir)?);
        loaded.retain(|(dir, _, _)| dir != game_dir);
        loaded.push((game_dir.to_path_buf(), version, data.clone()));
        Ok(data)
    }
}

impl<T> Default for ByVersion<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn the_version_is_main_luas_local() {
        let src = "local activeMode, gameModeType\nlocal version = 53.8\n\nfunction getGameVersion() return version end\n";
        assert_eq!(parse_version(src).as_deref(), Some("53.8"));
        assert_eq!(parse_version("local version=54 -- next\n").as_deref(), Some("54"));
        assert_eq!(parse_version("local versions = 1\n"), None);
        assert_eq!(parse_version("return {}\n"), None);
    }

    /// Loaded once while the version holds, again when it changes, and every time when it cannot
    /// be read.
    #[test]
    fn data_is_reloaded_only_when_the_version_changes() {
        let dir = std::env::temp_dir().join(format!("diggle-version-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let main = dir.join("main.lua");
        let cache = ByVersion::<u32>::new();
        let loads = Cell::new(0);
        let load = |_: &Path| -> Result<u32, ()> {
            loads.set(loads.get() + 1);
            Ok(loads.get())
        };

        std::fs::write(&main, "local version = 53.8\n").unwrap();
        assert_eq!(*cache.get(&dir, load).unwrap(), 1);
        assert_eq!(*cache.get(&dir, load).unwrap(), 1, "the same version: kept");
        std::fs::write(&main, "local version = 53.9\n").unwrap();
        assert_eq!(*cache.get(&dir, load).unwrap(), 2, "a new version: read again");
        assert_eq!(*cache.get(&dir, load).unwrap(), 2);
        std::fs::remove_file(&main).unwrap();
        assert_eq!(*cache.get(&dir, load).unwrap(), 3, "no version: read every time");
        assert_eq!(*cache.get(&dir, load).unwrap(), 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_load_is_not_kept() {
        let dir = std::env::temp_dir().join(format!("diggle-version-fail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.lua"), "local version = 1\n").unwrap();
        let cache = ByVersion::<u32>::new();
        assert!(cache.get(&dir, |_| Err::<u32, ()>(())).is_err());
        assert_eq!(*cache.get(&dir, |_| Ok::<u32, ()>(7)).unwrap(), 7);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
