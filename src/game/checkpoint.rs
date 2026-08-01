//! Snapshot and restore the sandbox save, so a one-shot moment can be rehearsed.
//!
//! Some things happen exactly once per island. The anomaly opens when you *arrive* at a
//! non-subworld combat node of level > 3 while `areaFlag'hell' == 0`
//! (`overworld/events/arrived/world_evil.lua:15-21`) — and once it has opened, that flag is set and
//! the moment is gone until a new island. Testing an end-to-end run against a moving save means one
//! attempt, then waiting for the next island to try again.
//!
//! A checkpoint is just a copy of the save directory. Restoring it puts the world back exactly where
//! it was, so the same arrival, the same fight, or the same reward screen can be replayed until the
//! handling is right. This is the difference between debugging the anomaly sequence and hoping it
//! works first time.
//!
//! ## Two guards, both non-negotiable
//!
//! - **Never the real save.** `%APPDATA%\SternlyWordedAdventures` (no `LOVE` folder) is the user's
//!   Steam save with genuine progress. Diggle's sandbox is `%APPDATA%\LOVE\SternlyWordedAdventures`.
//!   [`guard`] refuses any directory that is not the sandbox, so a mistyped path cannot overwrite
//!   real progress. Restoring is destructive and there is no undo.
//! - **Never while the game runs.** LÖVE writes `mainSaveData` on screen exit and `overworld:save()`
//!   at its own moments, so a snapshot taken mid-run captures a torn state, and a restore under a
//!   live game is simply overwritten when it next saves.

use std::path::{Path, PathBuf};

/// Files LÖVE keeps for this game. `combatSaveData` is present only mid-run, which is exactly why a
/// checkpoint has to record its **absence** as faithfully as its presence: restoring a save that
/// still has one would resume a fight the world no longer expects.
const KNOWN: &[&str] = &["mainSaveData", "persistentSaveData", "statsIndex", "combatSaveData"];

/// Rejects any directory that is not Diggle's sandbox save.
///
/// The check is on the `LOVE` parent, not on the name alone — the real Steam save has the same leaf
/// name and differs only by that folder.
pub fn guard(save_dir: &Path) -> Result<(), crate::Error> {
    let is_sandbox = save_dir
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.eq_ignore_ascii_case("LOVE"))
        .unwrap_or(false);
    let named = save_dir
        .file_name()
        .map(|n| n.eq_ignore_ascii_case("SternlyWordedAdventures"))
        .unwrap_or(false);
    if is_sandbox && named {
        return Ok(());
    }
    Err(crate::Error::Config(format!(
        "refusing to touch {}: checkpoints operate only on the sandbox save \
         (…\\LOVE\\SternlyWordedAdventures). The folder without LOVE is the real Steam save.",
        save_dir.display()
    )))
}

/// Copies the sandbox save into `store/name`, replacing any checkpoint already there.
pub fn save(save_dir: &Path, store: &Path, name: &str) -> Result<PathBuf, crate::Error> {
    guard(save_dir)?;
    refuse_if_game_running()?;
    if !save_dir.is_dir() {
        return Err(crate::Error::SaveDirMissing(save_dir.to_path_buf()));
    }
    let dest = store.join(sanitize(name));
    if dest.exists() {
        std::fs::remove_dir_all(&dest)?;
    }
    std::fs::create_dir_all(&dest)?;
    copy_tree(save_dir, &dest)?;
    Ok(dest)
}

/// Restores `store/name` over the sandbox save.
///
/// Destructive by design and by necessity: a merge would leave a `combatSaveData` from the live
/// state beside a `mainSaveData` from the checkpoint, which is a world state the game never
/// produced. Every known file is cleared first so absence is restored as faithfully as presence.
pub fn restore(store: &Path, name: &str, save_dir: &Path) -> Result<(), crate::Error> {
    guard(save_dir)?;
    refuse_if_game_running()?;
    let src = store.join(sanitize(name));
    if !src.is_dir() {
        return Err(crate::Error::Config(format!("no checkpoint named {name:?} in {}", store.display())));
    }
    std::fs::create_dir_all(save_dir)?;
    for f in KNOWN {
        let p = save_dir.join(f);
        if p.exists() {
            std::fs::remove_file(&p)?;
        }
    }
    let stats = save_dir.join("saveStats");
    if stats.is_dir() {
        std::fs::remove_dir_all(&stats)?;
    }
    copy_tree(&src, save_dir)
}

/// Checkpoint names, newest first by modification time.
pub fn list(store: &Path) -> Result<Vec<(String, std::time::SystemTime)>, crate::Error> {
    if !store.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for e in std::fs::read_dir(store)?.filter_map(|e| e.ok()) {
        if e.path().is_dir() {
            let when = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            out.push((e.file_name().to_string_lossy().into_owned(), when));
        }
    }
    out.sort_by(|a, b| b.1.cmp(&a.1));
    Ok(out)
}

/// Summarises what a checkpoint holds, without needing the game.
///
/// Reads the fields that decide whether a checkpoint is the one you want: where the player is, what
/// is complete, whether the anomaly has already opened, and whether a fight is in progress.
pub fn describe(dir: &Path) -> String {
    let main = dir.join("mainSaveData");
    let Ok(t) = crate::game::save::load(&main) else {
        return "unreadable mainSaveData".into();
    };
    let loc = t.str_at("overworld.playerLocation").unwrap_or("?");
    let completed = t
        .table_at("overworld.completedAreas")
        .map(|c| c.map.len())
        .unwrap_or(0);
    // `hell` is the anomaly gate: nonzero means it has already opened and the trigger is spent.
    // Read as a FLOAT — `hellOpens` sets it to 0.1, and `as_int` reports that as absent, which had
    // this printing `hell=unset` for a world whose anomaly was wide open.
    let hell = t.table_at("overworld.areaFlags").and_then(|f| f.get("hell").and_then(|v| v.as_f64()));
    let mid_fight = dir.join("combatSaveData").is_file();
    format!(
        "at {loc}, {completed} areas complete, hell={}, {}",
        hell.map(|h| h.to_string()).unwrap_or("unset".into()),
        if mid_fight { "MID-FIGHT" } else { "no fight in progress" }
    )
}

fn sanitize(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect()
}

fn refuse_if_game_running() -> Result<(), crate::Error> {
    crate::win::process::refuse_if_running("lovec.exe", &[])
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), crate::Error> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)?.filter_map(|e| e.ok()) {
        let src = e.path();
        let dst = to.join(e.file_name());
        if src.is_dir() {
            copy_tree(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_real_steam_save_is_refused() {
        // The one mistake this module must make impossible. Same leaf name, no LOVE parent.
        let real = Path::new(r"C:\Users\x\AppData\Roaming\SternlyWordedAdventures");
        assert!(guard(real).is_err(), "the real Steam save must never be a checkpoint target");
    }

    #[test]
    fn the_sandbox_is_accepted() {
        let sandbox = Path::new(r"C:\Users\x\AppData\Roaming\LOVE\SternlyWordedAdventures");
        assert!(guard(sandbox).is_ok());
    }

    #[test]
    fn an_unrelated_directory_is_refused() {
        assert!(guard(Path::new(r"C:\temp\whatever")).is_err());
    }

    #[test]
    fn names_cannot_escape_the_store() {
        // A checkpoint name reaches the filesystem, so path separators must not survive it.
        assert_eq!(sanitize("../../etc"), "------etc");
        assert_eq!(sanitize("pre-anomaly_1"), "pre-anomaly_1");
    }
}
