//! Watching the save folders without getting in the game's way.
//!
//! The game must never notice the advisor, and the one way it could is through `combatSaveData`:
//! the reward screen does `assert(love.filesystem.remove('combatSaveData'))`
//! (`overworld.lua:1154`), and an assert that fails takes the game down. So:
//!
//! - **The folder is watched, not the file.** `FindFirstChangeNotificationW` opens the directory;
//!   the file is never opened to learn that it changed.
//! - **Nothing is read until its size or modification time moves** ([`should_read`]). Looking at
//!   metadata does not open the file.
//! - **A read is one `std::fs::read`**: open, read, close, parse from memory afterwards. Rust opens
//!   with `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`, so even that instant does not
//!   block the game's write or its delete.
//! - **Reads wait for the writes to stop** — [`DEBOUNCE`] with no further change — because LÖVE
//!   writes the file whole and a read in the middle would only see part of it.
//! - **A failed parse is not retried.** It is reported once and left until the file changes again
//!   ([`load_if_changed`]); a half-written file is always followed by the change that finishes it.
//!
//! **Only the fused game's folder is watched**, `%APPDATA%\SternlyWordedAdventures` — see
//! [`save_dirs`]. That is the user's real save: everything above is why reading it is safe, and
//! nothing here ever writes. Several folders can still be given (`--dir`, for testing), and then the
//! one written most recently is followed.

use super::mode::{Snapshot, Stamp};
use crate::game::save::Table;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{Duration, SystemTime};
use windows::core::HSTRING;
use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Storage::FileSystem::{
    FindCloseChangeNotification, FindFirstChangeNotificationW, FindNextChangeNotification,
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE,
};
use windows::Win32::System::Threading::WaitForMultipleObjects;

/// The file the combat panels are driven by in a normal run.
pub const COMBAT_SAVE: &str = "combatSaveData";

/// The save names of the arcade challenges, from the game's own list.
///
/// A challenge does not write `combatSaveData`: `rpg.save` writes `filename or 'combatSaveData'`
/// (`rpg.lua:43`), and starting a challenge sets `filename` to the challenge's own
/// (`ui/arcade.lua:807`, kept by `rpg.lua:84`) — Boggling anxiety's is `bogglingChallenge`. Read
/// from `ui/arcade.lua`'s `filename = '…'` lines, so a new challenge needs nothing here. Empty when
/// the file cannot be read, which leaves the advisor following normal runs only.
pub fn arcade_saves(game_dir: &Path) -> Vec<String> {
    let Ok(src) = std::fs::read_to_string(game_dir.join("ui").join("arcade.lua")) else {
        return Vec::new();
    };
    let mut names: Vec<String> = src
        .lines()
        .filter_map(|l| {
            let rest = l.trim_start().strip_prefix("filename")?.trim_start().strip_prefix('=')?;
            let rest = rest.trim_start();
            let quote = rest.chars().next().filter(|c| *c == '\'' || *c == '"')?;
            let name = rest[1..].split(quote).next()?;
            (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
                .then(|| name.to_string())
        })
        .filter(|n| n != COMBAT_SAVE)
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Which of the save files `names` in `dir` to follow: the one written most recently **after
/// `since`**, else `current`.
///
/// `since` is the last moment anything was seen of the file being followed — its last write, or
/// when it went. So the advisor moves to a challenge as soon as the challenge saves, and when the
/// challenge ends and its file is deleted, it stays on it (out of combat) rather than falling back
/// to a normal run's older save; that one is followed again once the game writes it. `None`, before
/// anything has been followed: the newest that exists.
pub fn choose_save(
    dir: &Path, names: &[String], current: &str, since: Option<SystemTime>,
) -> String {
    names
        .iter()
        .filter_map(|n| Some((stamp_of(&dir.join(n))?.1, n)))
        .filter(|(t, _)| since.is_none_or(|s| *t > s))
        .max_by_key(|(t, _)| *t)
        .map(|(_, n)| n.clone())
        .unwrap_or_else(|| current.to_string())
}

/// How long the folder must be quiet before a read. LÖVE writes the whole file in one go, so a
/// short pause after the last change is enough to be past it; long enough to cover a write that
/// arrives as several notifications, short enough that the list follows the board without a lag
/// anyone would notice.
pub const DEBOUNCE: Duration = Duration::from_millis(150);

/// How often the folders are looked at with no notification. Only metadata is looked at, so this
/// costs the game nothing; it covers a missed notification and a folder created after start-up
/// (LÖVE makes the save folder on its first save).
pub const FALLBACK: Duration = Duration::from_secs(2);

/// The folders the advisor watches by default: the fused game's, and only that.
///
/// **Never the `LOVE` subfolder.** `%APPDATA%\LOVE\SternlyWordedAdventures` is where `lovec.exe`
/// writes — Diggle's sandbox ([`crate::game::savedir::unfused`]) — and the user ruled it out: the
/// advisor is for the game they play, which is the fused build ([`crate::game::savedir::fused`]).
pub fn save_dirs(appdata: &Path) -> Vec<PathBuf> {
    vec![crate::game::savedir::fused(appdata)]
}

/// Read only when there is a file and its size or modification time is not the one last read.
pub fn should_read(prev: Option<Stamp>, now: Option<Stamp>) -> bool {
    now.is_some() && now != prev
}

fn stamp_of(path: &Path) -> Option<Stamp> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.len(), m.modified().ok()?))
}

/// The folder with the most recently modified file in it, among those that exist.
pub fn newest_dir(dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter()
        .filter_map(|d| {
            let newest = std::fs::read_dir(d)
                .ok()?
                .filter_map(|e| e.ok()?.metadata().ok()?.modified().ok())
                .max()?;
            Some((newest, d.clone()))
        })
        .max_by_key(|(t, _)| *t)
        .map(|(_, d)| d)
}

/// What looking at the combat save found.
#[derive(Debug)]
pub enum Load {
    /// Nothing has changed since the last look.
    Unchanged,
    /// The file is gone, and was not gone at the last look.
    Absent,
    Fresh((Table, Stamp)),
    /// It changed but could not be read or parsed. Not retried until it changes again.
    Unreadable(String),
}

/// Looks at `path`, reading it only if it changed since `last`.
///
/// `last` is what was last looked at: `None` before the first look, `Some(None)` for "absent",
/// `Some(Some(stamp))` for the version last read — **or last failed to read**, which is what stops
/// a broken file from being re-read every [`FALLBACK`].
pub fn load_if_changed(path: &Path, last: &mut Option<Option<Stamp>>) -> Load {
    let now = stamp_of(path);
    let Some(stamp) = now else {
        if *last == Some(None) {
            return Load::Unchanged;
        }
        *last = Some(None);
        return Load::Absent;
    };
    if last.is_some() && !should_read(last.flatten(), now) {
        return Load::Unchanged;
    }
    *last = Some(now);
    let parsed = std::fs::read(path)
        .map_err(|e| e.to_string())
        .and_then(|bytes| String::from_utf8(bytes).map_err(|e| e.to_string()))
        .and_then(|src| crate::game::save::parse(&src).map_err(|e| e.to_string()));
    match parsed {
        Ok(t) => Load::Fresh((t, stamp)),
        Err(e) => Load::Unreadable(e),
    }
}

/// What the watcher tells the window.
#[derive(Debug)]
pub enum Event {
    Snapshot(Snapshot),
    Unreadable { dir: PathBuf, error: String },
}

impl Event {
    /// Does this make a turn ranked from an earlier snapshot not worth showing? Only a newer
    /// snapshot does: a note that the save could not be read leaves the last good board standing.
    pub fn supersedes_a_turn(&self) -> bool {
        matches!(self, Event::Snapshot(_))
    }
}

/// One directory change handle, closed on drop.
struct Watch {
    dir: PathBuf,
    handle: HANDLE,
}

impl Drop for Watch {
    fn drop(&mut self) {
        // SAFETY: the handle came from FindFirstChangeNotificationW and is closed only here.
        unsafe {
            let _ = FindCloseChangeNotification(self.handle);
        }
    }
}

fn open_watch(dir: &Path) -> Option<Watch> {
    // SAFETY: plain Win32 call on a path string that outlives it.
    let handle = unsafe {
        FindFirstChangeNotificationW(
            &HSTRING::from(dir.as_os_str()),
            false,
            FILE_NOTIFY_CHANGE_LAST_WRITE | FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_SIZE,
        )
    }
    .ok()?;
    Some(Watch { dir: dir.to_path_buf(), handle })
}

/// Waits on every watch. `Some(i)` is the watch that fired (already re-armed), `None` a timeout.
fn wait(watches: &[Watch], timeout: Duration) -> Option<usize> {
    if watches.is_empty() {
        std::thread::sleep(timeout);
        return None;
    }
    let handles: Vec<HANDLE> = watches.iter().map(|w| w.handle).collect();
    // SAFETY: every handle is live for the duration of the call.
    let r = unsafe { WaitForMultipleObjects(&handles, false, timeout.as_millis() as u32) };
    if r == WAIT_TIMEOUT {
        return None;
    }
    let i = r.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
    let w = watches.get(i)?;
    // SAFETY: re-arms a live handle.
    unsafe {
        let _ = FindNextChangeNotification(w.handle);
    }
    Some(i)
}

/// Runs the watcher on its own thread until the receiving end hangs up.
///
/// `arcade` is [`arcade_saves`]: the challenge saves followed alongside [`COMBAT_SAVE`].
pub fn spawn(
    dirs: Vec<PathBuf>, arcade: Vec<String>, tx: Sender<Event>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let names: Vec<String> = std::iter::once(COMBAT_SAVE.to_string()).chain(arcade).collect();
        let mut following = newest_dir(&dirs).unwrap_or_else(|| dirs[0].clone());
        let mut file = choose_save(&following, &names, COMBAT_SAVE, None);
        // The last moment anything was seen of `file` — see `choose_save`.
        let mut since: Option<SystemTime> = None;
        let mut last: Option<Option<Stamp>> = None;
        let mut watches: Vec<Watch> = Vec::new();
        loop {
            // Open watches for folders that have appeared since the last pass.
            for d in &dirs {
                if d.is_dir() && !watches.iter().any(|w| &w.dir == d) {
                    if let Some(w) = open_watch(d) {
                        watches.push(w);
                    }
                }
            }
            let next = choose_save(&following, &names, &file, since);
            if next != file {
                file = next;
                last = None;
            }
            let path = following.join(&file);
            let event = match load_if_changed(&path, &mut last) {
                Load::Unchanged => None,
                Load::Absent => {
                    since = Some(SystemTime::now());
                    Some(Event::Snapshot(Snapshot {
                        dir: following.clone(),
                        file: file.clone(),
                        combat: None,
                        stamp: None,
                    }))
                }
                Load::Fresh((t, stamp)) => {
                    since = since.max(Some(stamp.1));
                    Some(Event::Snapshot(Snapshot {
                        dir: following.clone(),
                        file: file.clone(),
                        combat: Some(t),
                        stamp: Some(stamp),
                    }))
                }
                Load::Unreadable(error) => {
                    Some(Event::Unreadable { dir: following.clone(), error })
                }
            };
            if let Some(e) = event {
                if tx.send(e).is_err() {
                    return;
                }
            }
            let Some(fired) = wait(&watches, FALLBACK) else { continue };
            let mut active = watches[fired].dir.clone();
            // Let the writes finish: keep waiting while anything is still changing.
            while let Some(again) = wait(&watches, DEBOUNCE) {
                active = watches[again].dir.clone();
            }
            if active != following {
                following = active;
                file = choose_save(&following, &names, COMBAT_SAVE, None);
                since = None;
                last = None;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    /// Between two writes that must carry different modification times. NTFS stamps to 100ns, so
    /// this is generous; it is the margin for a coarser filesystem, at no real cost to the suite.
    const MTIME_STEP: Duration = Duration::from_millis(50);

    fn temp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("diggle-advisor-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A ranked turn is thrown away only for a newer save, never for a note about a broken one.
    #[test]
    fn only_a_newer_snapshot_supersedes_a_ranked_turn() {
        let snap = Event::Snapshot(Snapshot {
            dir: "x".into(),
            file: COMBAT_SAVE.into(),
            combat: None,
            stamp: None,
        });
        let broken = Event::Unreadable { dir: "x".into(), error: "eof".into() };
        assert!(snap.supersedes_a_turn());
        assert!(!broken.supersedes_a_turn());
    }

    /// Only the fused game's folder: never the `LOVE` subfolder that `lovec.exe` writes to.
    #[test]
    fn the_advisor_watches_only_the_fused_save_folder() {
        let appdata = std::path::Path::new(r"C:\Users\x\AppData\Roaming");
        assert_eq!(
            save_dirs(appdata),
            vec![std::path::PathBuf::from(r"C:\Users\x\AppData\Roaming\SternlyWordedAdventures")]
        );
    }

    #[test]
    fn should_read_only_when_metadata_moves() {
        let t = SystemTime::UNIX_EPOCH + Duration::from_secs(10);
        let later = t + Duration::from_secs(1);
        assert!(!should_read(Some((5, t)), None), "nothing to read");
        assert!(!should_read(Some((5, t)), Some((5, t))), "unchanged");
        assert!(should_read(Some((5, t)), Some((6, t))), "size moved");
        assert!(should_read(Some((5, t)), Some((5, later))), "time moved");
        assert!(should_read(None, Some((5, t))), "first sighting");
    }

    /// The folder the game wrote to last is the one being played, whatever else is in the other.
    #[test]
    fn the_most_recently_written_folder_is_followed() {
        let (a, b) = (temp("newest-a"), temp("newest-b"));
        std::fs::write(a.join("combatSaveData"), "return {}").unwrap();
        std::thread::sleep(MTIME_STEP);
        std::fs::write(b.join("mainSaveData"), "return {}").unwrap();
        assert_eq!(newest_dir(&[a.clone(), b.clone()]), Some(b.clone()));
        assert_eq!(newest_dir(&[a.join("missing")]), None);
    }

    /// An arcade challenge saves under its own name (`rpg.lua:43`, `ui/arcade.lua:807`), read from
    /// the game's list so a new challenge needs no change here.
    #[test]
    fn arcade_save_names_come_from_the_game() {
        let names = arcade_saves(std::path::Path::new("../sternly-worded-adventures"));
        assert!(names.iter().any(|n| n == "bogglingChallenge"), "{names:?}");
        assert!(names.iter().any(|n| n == "boggledChallenge"));
        assert!(!names.iter().any(|n| n == COMBAT_SAVE));
        assert!(arcade_saves(std::path::Path::new("missing")).is_empty());
    }

    /// The save written last is followed; an older one is not returned to just because the one
    /// being followed is gone — only once it is written again.
    #[test]
    fn the_save_written_last_is_followed() {
        let d = temp("choose");
        let names: Vec<String> = vec![COMBAT_SAVE.into(), "bogglingChallenge".into()];
        std::fs::write(d.join(COMBAT_SAVE), "return {}").unwrap();
        std::thread::sleep(MTIME_STEP);
        std::fs::write(d.join("bogglingChallenge"), "return {}").unwrap();
        assert_eq!(choose_save(&d, &names, COMBAT_SAVE, None), "bogglingChallenge");
        let written = stamp_of(&d.join("bogglingChallenge")).unwrap().1;
        assert_eq!(
            choose_save(&d, &names, "bogglingChallenge", Some(written)),
            "bogglingChallenge"
        );
        // The challenge ends and its file goes: stay on it, which reads as out of combat.
        std::fs::remove_file(d.join("bogglingChallenge")).unwrap();
        let gone = SystemTime::now();
        assert_eq!(choose_save(&d, &names, "bogglingChallenge", Some(gone)), "bogglingChallenge");
        // The normal run is played again.
        std::thread::sleep(MTIME_STEP);
        std::fs::write(d.join(COMBAT_SAVE), "return { }").unwrap();
        assert_eq!(choose_save(&d, &names, "bogglingChallenge", Some(gone)), COMBAT_SAVE);
        // Nothing at all: the combat save, as before.
        let empty = temp("choose-empty");
        assert_eq!(choose_save(&empty, &names, COMBAT_SAVE, None), COMBAT_SAVE);
    }

    /// A half-written save is reported once, then left alone until it changes again.
    #[test]
    fn a_parse_failure_keeps_the_previous_snapshot() {
        let d = temp("parse");
        let path = d.join("combatSaveData");
        std::fs::write(&path, "return { rpg = ").unwrap();
        let mut last = None;
        assert!(matches!(load_if_changed(&path, &mut last), Load::Unreadable(_)));
        assert!(matches!(load_if_changed(&path, &mut last), Load::Unchanged), "no retry loop");
        std::thread::sleep(MTIME_STEP);
        std::fs::write(&path, "return { rpg = { player = { turnState = 'WaitPhase' } } }").unwrap();
        assert!(matches!(load_if_changed(&path, &mut last), Load::Fresh(_)));
        std::fs::remove_file(&path).unwrap();
        assert!(matches!(load_if_changed(&path, &mut last), Load::Absent));
        assert!(
            matches!(load_if_changed(&path, &mut last), Load::Unchanged),
            "absence reported once"
        );
    }
}
