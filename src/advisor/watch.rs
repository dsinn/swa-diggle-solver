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
//! **Both save folders are watched** — the Steam build's `%APPDATA%\SternlyWordedAdventures` and
//! `lovec.exe`'s `%APPDATA%\LOVE\SternlyWordedAdventures` ([`crate::game::savedir`]) — and the one
//! written most recently is followed, so it works however the game was started. The Steam folder
//! is the real save: everything above is why reading it is safe, and nothing here ever writes.

use super::mode::{Snapshot, Stamp};
use crate::game::save::Table;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Duration;
use windows::core::HSTRING;
use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::Storage::FileSystem::{
    FindCloseChangeNotification, FindFirstChangeNotificationW, FindNextChangeNotification,
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE,
};
use windows::Win32::System::Threading::WaitForMultipleObjects;

/// The file the combat panels are driven by.
pub const COMBAT_SAVE: &str = "combatSaveData";

/// How long the folder must be quiet before a read. LÖVE writes the whole file in one go, so a
/// short pause after the last change is enough to be past it; long enough to cover a write that
/// arrives as several notifications, short enough that the list follows the board without a lag
/// anyone would notice.
pub const DEBOUNCE: Duration = Duration::from_millis(150);

/// How often the folders are looked at with no notification. Only metadata is looked at, so this
/// costs the game nothing; it covers a missed notification and a folder created after start-up
/// (`LOVE` makes the save folder on its first save).
pub const FALLBACK: Duration = Duration::from_secs(2);

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
pub fn spawn(dirs: Vec<PathBuf>, tx: Sender<Event>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut following = newest_dir(&dirs).unwrap_or_else(|| dirs[0].clone());
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
            let path = following.join(COMBAT_SAVE);
            let event = match load_if_changed(&path, &mut last) {
                Load::Unchanged => None,
                Load::Absent => Some(Event::Snapshot(Snapshot {
                    dir: following.clone(),
                    combat: None,
                    stamp: None,
                })),
                Load::Fresh((t, stamp)) => Some(Event::Snapshot(Snapshot {
                    dir: following.clone(),
                    combat: Some(t),
                    stamp: Some(stamp),
                })),
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
        let snap = Event::Snapshot(Snapshot { dir: "x".into(), combat: None, stamp: None });
        let broken = Event::Unreadable { dir: "x".into(), error: "eof".into() };
        assert!(snap.supersedes_a_turn());
        assert!(!broken.supersedes_a_turn());
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
