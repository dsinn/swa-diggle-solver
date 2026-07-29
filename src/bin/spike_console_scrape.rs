//! Can we read the game's `_VERBOSE` output by giving it a REAL console and scraping the
//! console screen buffer?
//!
//! Why this might work where the pipe did not: the CRT picks its buffering mode from the KIND
//! of handle stdout is. A pipe gets fully buffered (4 KB) and this game's output is far too
//! sparse to ever fill it -- 14 s of running produced 0 bytes, and only a shutdown flush
//! yielded `Lore screen: `. A character device (a console) gets line-buffered, so each `print`
//! should appear immediately. Nothing about the game changes: `_VERBOSE` is already enabled by
//! the documented `--verbose` flag (`main.lua:37`); we only change how the process is LAUNCHED.
//!
//! Attempt #1 of two, deliberately independent of ConPTY (which defeated an earlier spike for
//! reasons never established).
//!
//! ## Two harness bugs this spike hit, both caught by its positive control
//!
//! 1. `std::process::Command` cannot express what we need. Its default `Stdio::inherit`
//!    duplicates our handles into the child AND sets `STARTF_USESTDHANDLES`, so the child
//!    writes to OUR pipe and ignores the console it was given. Symptom: the control needles
//!    appeared in the parent's own output while the child's console buffer stayed empty.
//!    `Stdio::null()` does not help -- that still sets the flag, just pointing at NUL. Hence
//!    `CreateProcessW` by hand, with `dwFlags = 0`.
//!
//! 2. `AttachConsole(child_pid)` returns ACCESS_DENIED here even immediately after a
//!    `FreeConsole()` that returned Ok -- most likely console-session isolation from running
//!    under the harness's pipe. So we do NOT attach to anyone else's console. Instead we
//!    ALLOCATE OUR OWN, and spawn the game as a child that inherits it. Reading your own
//!    console's `CONOUT$` requires no attach. Console attachment is inherited independently of
//!    handle inheritance, so `bInheritHandles = false` is still correct.
//!
//! MANDATORY POSITIVE CONTROL: phase 1 runs `cmd.exe` printing known needles through the same
//! path. If those needles cannot be read back, the harness is broken and any verdict about the
//! game would be meaningless -- so the spike says so and refuses to launch the game.
//!
//! Run: cargo run --release --bin spike_console_scrape -- config.toml
//!
//! Reports to a FILE, not stdout: allocating a new console invalidates our own stdout.

use diggle_solver::config::Config;
use std::io::Write;
use std::path::Path;
use std::time::Duration;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Console::{
    AllocConsole, FreeConsole, GetConsoleScreenBufferInfo, ReadConsoleOutputCharacterW,
    SetConsoleScreenBufferSize, CONSOLE_SCREEN_BUFFER_INFO, COORD,
};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::System::Threading::{
    CreateProcessW, TerminateProcess, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION,
    STARTF_USESTDHANDLES, STARTUPINFOW,
};

const REPORT: &str = "spike-frames-live/console-scrape-report.md";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Drops whatever console we inherited and allocates a fresh one that WE own.
fn take_own_console() -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        let _ = FreeConsole(); // may legitimately fail if we had none
        AllocConsole()?;
    }
    Ok(())
}

/// Spawns a process whose stdout/stderr are EXPLICITLY our console's screen buffer.
///
/// Takes a raw command line rather than an argument list. Quoting every argument broke the
/// control: `cmd.exe "/c" "echo A & echo B"` makes cmd try to execute the whole quoted string
/// as a program name. Callers do their own quoting, because only they know what the target
/// parses.
///
/// Leaving `STARTF_USESTDHANDLES` unset was NOT sufficient. In that configuration the child's
/// error output still reached the harness's pipe while the console we had just allocated stayed
/// empty -- the child took the parent's std handle VALUES rather than opening the console
/// afresh. So we now name the handles: open `CONOUT$`/`CONIN$` on our own console, mark them
/// inheritable, and pass them in. This also pins the property under test -- the child's stdout
/// is unambiguously a character device, which is what should make the CRT line-buffer.
fn spawn_on_our_console(
    exe: &str,
    cmdline: &str,
) -> Result<(u32, HANDLE), Box<dyn std::error::Error>> {
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: true.into(),
    };
    let conout = open_console_handle("CONOUT$", &sa)?;
    let conin = open_console_handle("CONIN$", &sa)?;

    let exe_w = wide(exe);
    let mut cmd_w = wide(cmdline);

    let mut si = STARTUPINFOW::default();
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    si.dwFlags = STARTF_USESTDHANDLES;
    si.hStdInput = conin;
    si.hStdOutput = conout;
    si.hStdError = conout;

    let mut pi = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessW(
            PCWSTR(exe_w.as_ptr()),
            windows::core::PWSTR(cmd_w.as_mut_ptr()),
            None,
            None,
            true, // must inherit, so the console handles above reach the child
            PROCESS_CREATION_FLAGS(0),
            None,
            PCWSTR::null(),
            &si,
            &mut pi,
        )?;
    }
    Ok((pi.dwProcessId, pi.hProcess))
}

fn open_console_handle(
    name: &str,
    sa: &SECURITY_ATTRIBUTES,
) -> Result<HANDLE, Box<dyn std::error::Error>> {
    let w = wide(name);
    Ok(unsafe {
        CreateFileW(
            PCWSTR(w.as_ptr()),
            (GENERIC_READ | GENERIC_WRITE).0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            Some(sa),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )?
    })
}

/// Opens OUR console's screen buffer. `GetStdHandle` is unreliable after the console dance;
/// `CONOUT$` is the documented name for the attached console.
fn open_conout() -> Result<HANDLE, Box<dyn std::error::Error>> {
    let name = wide("CONOUT$");
    Ok(unsafe {
        CreateFileW(
            PCWSTR(name.as_ptr()),
            (GENERIC_READ | GENERIC_WRITE).0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )?
    })
}

fn grow_buffer(rows: i16) -> Result<(i16, i16), Box<dyn std::error::Error>> {
    let h = open_conout()?;
    let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
    unsafe { GetConsoleScreenBufferInfo(h, &mut info)? };
    let width = info.dwSize.X.max(120);
    unsafe { SetConsoleScreenBufferSize(h, COORD { X: width, Y: rows })? };
    Ok((width, rows))
}

/// Reads our console's screen buffer as text, up to the cursor row.
fn scrape() -> Result<String, Box<dyn std::error::Error>> {
    let h = open_conout()?;
    let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
    unsafe { GetConsoleScreenBufferInfo(h, &mut info)? };
    let width = info.dwSize.X;
    let rows = (info.dwCursorPosition.Y + 1).min(info.dwSize.Y);

    let mut out = String::new();
    let mut buf = vec![0u16; width as usize];
    for y in 0..rows {
        let mut read = 0u32;
        unsafe { ReadConsoleOutputCharacterW(h, &mut buf, COORD { X: 0, Y: y }, &mut read)? };
        out.push_str(String::from_utf16_lossy(&buf[..read as usize]).trim_end());
        out.push('\n');
    }
    Ok(out)
}

fn nonblank(s: &str) -> usize {
    s.lines().filter(|l| !l.trim().is_empty()).count()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg_path = std::env::args().nth(1).unwrap_or_else(|| "config.toml".into());
    std::fs::create_dir_all("spike-frames-live")?;
    let cfg = Config::load(Path::new(&cfg_path))?;

    let mut log = String::from("# Spike: read game stdout by scraping a console we own\n\n");

    take_own_console()?;
    match grow_buffer(3000) {
        Ok((w, r)) => log.push_str(&format!("allocated own console; buffer {w} x {r}\n\n")),
        Err(e) => log.push_str(&format!("allocated own console; buffer grow failed: {e}\n\n")),
    }

    // ---------------- PHASE 1: POSITIVE CONTROL ----------------
    const NEEDLE_A: &str = "DIGGLE_NEEDLE_ALPHA";
    const NEEDLE_B: &str = "DIGGLE_NEEDLE_BETA";
    log.push_str("## Phase 1 - positive control (cmd.exe on our console)\n\n");

    // No quoting around the /c payload: cmd would treat the quoted string as a program name.
    let control_line =
        format!(r"C:\Windows\System32\cmd.exe /c echo {NEEDLE_A} & echo {NEEDLE_B}");
    let (control_pid, control_handle) =
        spawn_on_our_console(r"C:\Windows\System32\cmd.exe", &control_line)?;
    std::thread::sleep(Duration::from_millis(1500));
    let control_text = scrape().unwrap_or_else(|e| format!("<scrape failed: {e}>"));
    unsafe {
        let _ = TerminateProcess(control_handle, 0);
    }

    let control_ok = control_text.contains(NEEDLE_A) && control_text.contains(NEEDLE_B);
    log.push_str(&format!("control pid {control_pid}\n\n```\n{control_text}```\n\n"));
    log.push_str(&format!(
        "needle A: {}\nneedle B: {}\n\n**POSITIVE CONTROL: {}**\n\n",
        control_text.contains(NEEDLE_A),
        control_text.contains(NEEDLE_B),
        if control_ok { "PASS" } else { "FAIL" }
    ));

    if !control_ok {
        log.push_str(
            "Cannot read a console we own, running a command that definitely printed. \
             Stopping WITHOUT launching the game: any verdict about the game would be invalid.\n",
        );
        std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
        return Ok(());
    }

    // ---------------- PHASE 2: THE GAME ----------------
    log.push_str("## Phase 2 - the game with --verbose\n\n");
    let game_dir = cfg.game_dir.to_string_lossy().into_owned();
    let lovec = cfg.lovec_path.to_string_lossy().into_owned();
    // Quote the exe and game dir (both contain spaces); --verbose must stay bare.
    let game_line = format!("\"{lovec}\" \"{game_dir}\" --verbose");
    let (game_pid, game_handle) = spawn_on_our_console(&lovec, &game_line)?;
    log.push_str(&format!("launched pid {game_pid} on our console\n\n"));

    // SECOND CONTROL: prove the game actually RAN. "No output" is uninterpretable if the
    // process died on launch -- that is precisely why the ConPTY verdict was invalid. A
    // visible window is proof it got as far as rendering, which is well past the point where
    // `Lore screen: ` is printed.
    let mut window_after: Option<u64> = None;
    for i in 1..=60 {
        if diggle_solver::win::window::find_by_pid(game_pid).is_some() {
            window_after = Some(i * 250);
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    match window_after {
        Some(ms) => log.push_str(&format!("game window appeared after ~{ms} ms — IT RAN\n\n")),
        None => log.push_str(
            "**NO GAME WINDOW APPEARED in 15 s** — the game did not run. \
             Any conclusion about its output from this run is INVALID.\n\n",
        ),
    }

    // Sample over time. The question is not only WHETHER output appears but WHEN: the pipe
    // showed 0 bytes for 14 s, whereas line buffering should show text within a second or two.
    let mut samples: Vec<(u64, usize)> = Vec::new();
    let mut last = String::new();
    for i in 1..=7u64 {
        std::thread::sleep(Duration::from_millis(2000));
        match scrape() {
            Ok(t) => {
                samples.push((i * 2, nonblank(&t)));
                last = t;
            }
            Err(e) => log.push_str(&format!("t={}s scrape error: {e}\n", i * 2)),
        }
    }

    log.push_str("| t (s) | non-blank lines |\n|---|---|\n");
    for (t, n) in &samples {
        log.push_str(&format!("| {t} | {n} |\n"));
    }

    // `Lore screen` is the one string the pipe ever produced (at shutdown), so it is the
    // minimum bar. `Local overworld data` / `posX` are the payload navigation wants
    // (overworldview.lua:1025-1035).
    let markers = [
        "Lore screen",
        "Local overworld data",
        "Adjacent connections",
        "posX",
        "Start menu",
        "Choose your champion",
    ];
    log.push_str("\n## Markers in the final sample\n\n");
    for m in markers {
        log.push_str(&format!("- `{m}`: {}\n", last.contains(m)));
    }
    log.push_str(&format!("\n## Final console contents\n\n```\n{last}```\n"));

    // DISCRIMINATING TEST. Two very different worlds produce "nothing in the console":
    //   (a) prints happen but are still FULLY BUFFERED even on a character device
    //   (b) no prints happen at all (e.g. _VERBOSE never set, or output goes elsewhere)
    // A graceful exit flushes the CRT; TerminateProcess does not. So close the window politely
    // and look again. Text appearing only now means (a); still nothing means (b).
    let mut after_exit = String::new();
    if let Some(w) = diggle_solver::win::window::find_by_pid(game_pid) {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                w.hwnd,
                windows::Win32::UI::WindowsAndMessaging::WM_CLOSE,
                windows::Win32::Foundation::WPARAM(0),
                windows::Win32::Foundation::LPARAM(0),
            );
        }
        std::thread::sleep(Duration::from_millis(4000));
        after_exit = scrape().unwrap_or_default();
    }
    let flushed_at_exit = after_exit.contains("Lore screen")
        || after_exit.contains("Local overworld data")
        || nonblank(&after_exit) > nonblank(&last);
    log.push_str(&format!(
        "\n## After graceful close (flush test)\n\nnon-blank before: {}, after: {}\n\
         flushed-at-exit: **{}**\n\n```\n{}```\n",
        nonblank(&last),
        nonblank(&after_exit),
        flushed_at_exit,
        after_exit
    ));

    let verdict = if window_after.is_none() {
        "INVALID - the game never opened a window, so its silence proves nothing"
    } else if last.contains("Local overworld data") || last.contains("Lore screen") {
        "PASS - game _VERBOSE output is readable live from a console we own"
    } else if samples.iter().any(|(_, n)| *n > 1) {
        "PARTIAL - console has text but no recognised _VERBOSE marker; inspect contents"
    } else {
        "FAIL - console stayed empty; line buffering did not materialise"
    };
    log.push_str(&format!("\n## Verdict\n\n**{verdict}**\n"));

    unsafe {
        let _ = TerminateProcess(game_handle, 0);
    }
    std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
    Ok(())
}
