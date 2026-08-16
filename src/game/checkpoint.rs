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

/// Empties the sandbox save completely, so the next launch starts a fresh profile.
///
/// **Everything, and `persistentSaveData` is the point of it.** The dev asked for exactly that on
/// 2026-08-15. [`restore`] clears only [`KNOWN`] because it is about to lay a checkpoint down on
/// top; this is about leaving nothing behind, so it enumerates the directory instead of a list of
/// names. A list would quietly spare whatever we had not thought of — `screenshots`, `heroCards`
/// (`ui/elements/herodisplay.lua:276-284` writes them), anything a later version of the game adds.
///
/// ## What clearing each thing costs
///
/// - `mainSaveData` and `combatSaveData` — the run. Without them the start menu's shared slot reads
///   **`Start`** rather than `Restart` (`ui/startmenu.lua:199`) and confirming a champion no longer
///   raises the eulogy dialogue (`utils/classes.lua:81-87`), which is a screen the driver has no
///   handler for. This much is what "start fresh" needs.
/// - `persistentSaveData` — **unlocks**. Every class returns to whatever `isUnlocked` says on a
///   clean profile, so the warrior is not on offer until he is earned again.
/// - `saveStats` — the run history, and **it decides which champions hero select offers**.
///   `rngSeed = #(love.filesystem.getDirectoryItems'saveStats')` (`ui/heroselect.lua:218`), and at
///   zero the screen asks for **one** hero rather than three (`:56`). So a cleared profile shows a
///   single card, which is a state [`crate::heroselect`] handles — the outer positions read as
///   nothing and the middle one wins the tie — but it is not a state that exercises it.
///
/// The directory itself is left in place; LÖVE will refill it.
pub fn clear(save_dir: &Path) -> Result<Vec<String>, crate::Error> {
    guard(save_dir)?;
    refuse_if_game_running()?;
    if !save_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut removed = Vec::new();
    for e in std::fs::read_dir(save_dir)?.filter_map(|e| e.ok()) {
        let path = e.path();
        if path.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
        removed.push(e.file_name().to_string_lossy().into_owned());
    }
    removed.sort();
    Ok(removed)
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
        "at {loc}, {completed} areas complete, anomaly {}, {}",
        describe_hell(hell),
        if mid_fight { "MID-FIGHT" } else { "no fight in progress" }
    )
}

/// Says what a `hell` reading MEANS, not what it is.
///
/// The bare number reads like a progress meter you compare against some high threshold, and it is
/// not one: `hellOpens` writes `0.1` (`utils\events.lua:39`) and the game's own test is
/// `overworldview.areaFlag'hell' ~= 0` (`overworld\locations\shrine.lua:50`), matched by
/// [`crate::overworld::WorldMap::anomaly_is_open`]. Zero is the only closed state.
///
/// So `hell=0.1` has been read as "corruption is barely started, the portal is shut" — by me, more
/// than once, while planning a run around it. Every checkpoint in the store shows `0.1`, including
/// ones taken deep into a run, which is the tell: if it were a meter it would have moved. The value
/// still prints, because it does grow and a future question may want it, but it prints *behind* the
/// answer rather than in place of it.
fn describe_hell(hell: Option<f64>) -> String {
    match hell {
        Some(h) if h != 0.0 => format!("OPEN (hell={h})"),
        Some(h) => format!("closed (hell={h})"),
        None => "closed (hell unset)".into(),
    }
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

    /// A sandbox-shaped directory under the test's own temporary space, with a profile in it.
    fn a_sandbox_with_a_profile(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("diggle-clear-{tag}-{}", std::process::id()))
            .join("LOVE")
            .join("SternlyWordedAdventures");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
        std::fs::create_dir_all(dir.join("saveStats")).expect("temp dir");
        std::fs::create_dir_all(dir.join("screenshots")).expect("temp dir");
        for f in ["mainSaveData", "persistentSaveData", "statsIndex"] {
            std::fs::write(dir.join(f), b"return {}").expect("temp file");
        }
        std::fs::write(dir.join("saveStats").join("0000"), b"return {}").expect("temp file");
        dir
    }

    /// **Everything means everything**, which is the dev's request and the difference between this
    /// and what [`restore`] clears.
    ///
    /// Asserted by enumerating what is left rather than by checking for the files this test happened
    /// to write: a `clear` that spared something we never thought to name would still pass a list of
    /// known names, and sparing the unnamed is exactly the failure mode.
    #[test]
    fn clearing_leaves_nothing_at_all_behind() {
        let dir = a_sandbox_with_a_profile("all");
        // A directory and a nested file, because the two are removed by different calls.
        assert!(dir.join("saveStats").join("0000").is_file());

        let removed = clear(&dir).expect("the sandbox should be clearable");
        assert!(removed.contains(&"persistentSaveData".to_string()), "unlocks must go too");
        assert!(removed.contains(&"saveStats".to_string()), "and the history that seeds hero select");
        assert!(removed.contains(&"screenshots".to_string()), "and whatever else was in there");

        let left: Vec<String> = std::fs::read_dir(&dir)
            .expect("the directory itself stays")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(left.is_empty(), "the save should be empty, found {left:?}");
        assert!(dir.is_dir(), "the directory itself is left for LOVE to refill");

        // Idempotent: nothing to remove is not an error.
        assert_eq!(clear(&dir).expect("a second clear is harmless"), Vec::<String>::new());
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    /// The guard is not weaker for being the destructive path — it is the same one, and this pins
    /// that `clear` actually asks it before touching anything.
    #[test]
    fn clearing_refuses_the_real_steam_save() {
        let real = Path::new(r"C:\Users\x\AppData\Roaming\SternlyWordedAdventures");
        assert!(clear(real).is_err(), "the real Steam save must never be cleared");
        assert!(clear(Path::new(r"C:\temp\whatever")).is_err());
    }

    #[test]
    fn the_value_the_anomaly_opens_at_is_reported_as_open() {
        // 0.1 is what `hellOpens` writes, and it is the number every checkpoint in the store shows.
        // Reading it as "barely started" is the mistake this line exists to stop, so assert on the
        // word rather than on the digits.
        assert!(describe_hell(Some(0.1)).starts_with("OPEN"));
        assert!(describe_hell(Some(1.0)).starts_with("OPEN"));
    }

    #[test]
    fn only_zero_and_a_missing_flag_are_closed() {
        assert!(describe_hell(Some(0.0)).starts_with("closed"));
        assert!(describe_hell(None).starts_with("closed"));
    }

    #[test]
    fn the_reading_itself_survives_for_a_question_this_summary_does_not_answer() {
        // It grows as the run goes on; the summary answers open/closed, not how far along.
        assert!(describe_hell(Some(0.19)).contains("0.19"));
    }

    #[test]
    fn names_cannot_escape_the_store() {
        // A checkpoint name reaches the filesystem, so path separators must not survive it.
        assert_eq!(sanitize("../../etc"), "------etc");
        assert_eq!(sanitize("pre-anomaly_1"), "pre-anomaly_1");
    }
}
