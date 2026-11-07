//! A timing log, for finding where the advisor loses time between the game's write and the list.
//!
//! The user has waited more than ten seconds for a list during their turn, while ranking alone
//! measures 55–350 ms. Each line here is one step a save passes through — the folder notification,
//! the wait for the writes to stop, the read and parse, the queue to the worker, the plan, the
//! search, the annotation, and the window's redraw — stamped with local wall-clock time, so a slow
//! turn can be found by when it happened and the gap read off between neighbouring lines.
//!
//! **Opt-in**: the advisor starts it only when run with `--timing-log`.
//!
//! Written to `%LOCALAPPDATA%\DiggleAdvisor\timing.log` ([`path`]), beside the settings: **never
//! into the save folder**, which the advisor only reads (see [`super::watch`]). Appended across
//! launches, each marked by a `started` line, and moved to `timing.prev.log` once it passes
//! [`ROTATE_AT`]. Until [`init`] is called every [`note`] is a no-op, so the tests and Diggle pay
//! nothing for it.

use std::fmt::Arguments;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

/// The log is moved aside at start-up once it is this big, keeping one older file.
pub const ROTATE_AT: u64 = 4 * 1024 * 1024;

fn sink() -> &'static Mutex<Option<File>> {
    static SINK: OnceLock<Mutex<Option<File>>> = OnceLock::new();
    SINK.get_or_init(|| Mutex::new(None))
}

/// Where the log lives, given `%LOCALAPPDATA%`.
pub fn path(local_appdata: &Path) -> PathBuf {
    local_appdata.join("DiggleAdvisor").join("timing.log")
}

/// Starts logging to `file`. A failure leaves logging off: timing is never worth a broken window.
pub fn init(file: &Path) {
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::metadata(file).is_ok_and(|m| m.len() > ROTATE_AT) {
        let _ = std::fs::rename(file, file.with_extension("prev.log"));
    }
    if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(file) {
        *sink().lock().unwrap() = Some(f);
        note("main", format_args!("started, pid {}", std::process::id()));
    }
}

/// Writes one line: the local time, which thread of the advisor, and `what`.
pub fn note(who: &str, what: Arguments) {
    let mut guard = sink().lock().unwrap();
    let Some(f) = guard.as_mut() else { return };
    // One write per line, unbuffered: a crash or a kill must not lose the lines before it, and
    // there are a handful per save.
    let _ = f.write_all(format!("{}  {who:<6} {what}\r\n", clock()).as_bytes());
}

/// Whether [`init`] succeeded, for skipping work done only to be logged.
pub fn on() -> bool {
    sink().lock().unwrap().is_some()
}

/// Milliseconds, to one decimal place.
pub fn ms(d: Duration) -> String {
    format!("{:.1}ms", d.as_secs_f64() * 1000.0)
}

/// Milliseconds since `t`.
pub fn since(t: Instant) -> String {
    ms(t.elapsed())
}

/// How long before now `t` was, for a file's modification time. Negative (a clock step, or a
/// filesystem stamping ahead) is printed as such rather than hidden.
pub fn age(t: SystemTime) -> String {
    match SystemTime::now().duration_since(t) {
        Ok(d) => ms(d),
        Err(e) => format!("-{}", ms(e.duration())),
    }
}

/// The local time of day, `HH:MM:SS.mmm`.
fn clock() -> String {
    // SAFETY: GetLocalTime only fills in a struct.
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{:02}:{:02}:{:02}.{:03}", t.wHour, t.wMinute, t.wSecond, t.wMilliseconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_in_milliseconds() {
        assert_eq!(ms(Duration::from_micros(1500)), "1.5ms");
        assert_eq!(ms(Duration::from_secs(12)), "12000.0ms");
        assert!(age(SystemTime::now() + Duration::from_secs(5)).starts_with('-'));
    }

    #[test]
    fn the_clock_has_a_fixed_shape() {
        let c = clock();
        assert_eq!(c.len(), 12, "{c}");
        assert_eq!(&c[2..3], ":");
        assert_eq!(&c[8..9], ".");
    }
}
