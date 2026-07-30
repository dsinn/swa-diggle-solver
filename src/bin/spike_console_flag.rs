//! Log channel, attempt #2: ask LÖVE for a console with its own `--console` flag.
//!
//! Executes `docs/superpowers/plans/2026-07-29-log-channel-console-flag.md`.
//!
//! ## What attempt #1 got wrong
//!
//! It handed the game a console and assumed the game would use it. LÖVE gates its console
//! setup on a command-line flag we never passed (`../love/src/modules/love/boot.lua:185-188`):
//!
//! ```lua
//! if love.arg.options.console.set and love._openConsole then
//!     love._openConsole()
//! ```
//!
//! `love._openConsole` (`../love/src/modules/love/love.cpp:562-628`) does
//! `AttachConsole(ATTACH_PARENT_PROCESS)` and then `freopen("CONOUT$", "w", stdout)`. Two
//! consequences shape this spike:
//!
//! 1. Pass `--console`, or none of that code runs.
//! 2. `AttachConsole` returns ACCESS_DENIED when the caller ALREADY has a console, and LÖVE
//!    then returns *without the freopen*. So the child must start with no console at all:
//!    `DETACHED_PROCESS`, and no `STARTF_USESTDHANDLES`.
//!
//! ## Honest statement of the hypothesis
//!
//! The missing flag is a real defect, but it may not be the only one. Attempt #1's
//! configuration -- child inheriting our console, stdout explicitly a `CONOUT$` handle --
//! should ALSO have produced visible output, because MSVC's CRT flushes stdout "after each
//! library call" when it writes to a character device. Its silence therefore has a competing
//! explanation: that the game printed nothing at all in the sampling window. The whole pipe
//! run only ever produced 16 bytes (`Lore screen: `, at shutdown), which is consistent with
//! one single print for the entire run.
//!
//! So this spike does not just retry with a flag. It separates the claims:
//!
//! - **Phase 1** -- can we read a console we own? (`cmd.exe`, known needles.)
//! - **Phase 2** -- can a LÖVE process write into a console we own? `lovec.exe --version`
//!   calls `love_openConsole` unconditionally (`../love/src/love.cpp:151-156`) and then
//!   `printf`s, so this tests the ENTIRE plumbing -- detached child, attach-to-parent,
//!   freopen, CRT flush -- with no dependency on the game or the flag.
//! - **Phase 3** -- the real test: the game, `--verbose --console`, sampled live.
//! - **Phase 4** -- only if phase 3 sees nothing: the same hand-built command line WITHOUT
//!   `--console`, stdout to a pipe. If `Lore screen: ` arrives there at exit, the command
//!   line is sound and the game does print, so the fault is in the console path. If it does
//!   not, the game printed nothing and phase 3 proved nothing either.
//!
//! - **Phase 5** (`drive` argument only) -- the LIVENESS test. Phases 2-4 can only ever show
//!   that output *reaches* the console, because `--version` exits immediately and its flush at
//!   exit is indistinguishable from a flush per call. To prove the channel is live we need a
//!   process that prints *and keeps running*. `core.verboseAdjacencyData` does exactly that:
//!   `overworldview.lua:1607` calls it with `'World loaded'`, and `:1255` again on every
//!   finished screen pan. So: keyboard-navigate to Continue, activate it, and watch.
//!
//!   This warps the real mouse pointer (`love.mouse.setPosition`, `utils/input.lua`) because
//!   posted mouse-button events are never delivered by SDL -- keyboard hotspot navigation is
//!   the only input path. It also refuses to press Return unless the highlight is verifiably
//!   on Continue: Restart sits on the same row at (500,810) and eulogizes the run.
//!
//! Run: cargo run --release --bin spike_console_flag -- config.toml [drive]
//!
//! Reports to a FILE: allocating a console invalidates our own stdout.

use diggle_solver::config::Config;
use diggle_solver::win::input::Input;
use std::io::Write;
use std::path::Path;
use std::time::Duration;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, GENERIC_READ, GENERIC_WRITE, HANDLE, LPARAM, WAIT_OBJECT_0, WPARAM,
};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Console::{
    AllocConsole, FreeConsole, GetConsoleScreenBufferInfo, ReadConsoleOutputCharacterW,
    SetConsoleOutputCP, SetConsoleScreenBufferSize, CONSOLE_SCREEN_BUFFER_INFO, COORD,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CreateProcessW, GetExitCodeProcess, TerminateProcess, WaitForSingleObject, DETACHED_PROCESS,
    PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

const REPORT: &str = "spike-frames-live/console-flag-report.md";
const SHOT: &str = "spike-frames-live/console-flag-window.bmp";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Drops whatever console we inherited and allocates a fresh one that WE own, so that a
/// `DETACHED_PROCESS` child's `AttachConsole(ATTACH_PARENT_PROCESS)` has something to attach to.
fn take_own_console() -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        let _ = FreeConsole(); // may legitimately fail if we had none
        AllocConsole()?;
        // The game prints UTF-8. conhost decodes incoming bytes with the console's OUTPUT
        // codepage, so at the default OEM page an em-dash in a location heading arrives as
        // `ΓÇö`. Headings are how we tell a shrine from a crypt, so decode them correctly
        // rather than un-mangling a guess later.
        let _ = SetConsoleOutputCP(65001); // CP_UTF8
    }
    Ok(())
}

fn open_console_handle(
    name: &str,
    sa: Option<&SECURITY_ATTRIBUTES>,
) -> Result<HANDLE, Box<dyn std::error::Error>> {
    let w = wide(name);
    Ok(unsafe {
        CreateFileW(
            PCWSTR(w.as_ptr()),
            (GENERIC_READ | GENERIC_WRITE).0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            sa.map(|s| s as *const _),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )?
    })
}

/// Spawns a child with **no console of its own**, so that if it calls
/// `AttachConsole(ATTACH_PARENT_PROCESS)` it lands on ours and reaches its `freopen`.
///
/// Deliberately does NOT set `STARTF_USESTDHANDLES`. Naming handles is what attempt #1 did,
/// and it takes LÖVE down the ACCESS_DENIED branch that skips the redirection. The child is
/// expected to open the console itself.
fn spawn_detached(exe: &str, cmdline: &str) -> Result<(u32, HANDLE), Box<dyn std::error::Error>> {
    let exe_w = wide(exe);
    let mut cmd_w = wide(cmdline);

    let mut si = STARTUPINFOW::default();
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;

    let mut pi = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessW(
            PCWSTR(exe_w.as_ptr()),
            windows::core::PWSTR(cmd_w.as_mut_ptr()),
            None,
            None,
            false,
            DETACHED_PROCESS,
            None,
            PCWSTR::null(),
            &si,
            &mut pi,
        )?;
    }
    Ok((pi.dwProcessId, pi.hProcess))
}

/// Phase 1 only: a child whose stdout is EXPLICITLY our console screen buffer. `cmd.exe` has
/// no console hack of its own, so it needs the handles named for it.
fn spawn_with_console_handles(
    exe: &str,
    cmdline: &str,
) -> Result<(u32, HANDLE), Box<dyn std::error::Error>> {
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: true.into(),
    };
    let conout = open_console_handle("CONOUT$", Some(&sa))?;
    let conin = open_console_handle("CONIN$", Some(&sa))?;

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
            true,
            PROCESS_CREATION_FLAGS(0),
            None,
            PCWSTR::null(),
            &si,
            &mut pi,
        )?;
    }
    Ok((pi.dwProcessId, pi.hProcess))
}

/// Phase 4 only: stdout to an anonymous pipe, reproducing the original (buffered) channel.
/// Returns everything the child wrote, read to EOF after it exits.
fn run_capturing_pipe(
    exe: &str,
    cmdline: &str,
    settle: Duration,
) -> Result<(bool, String), Box<dyn std::error::Error>> {
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: true.into(),
    };
    let mut rd = HANDLE::default();
    let mut wr = HANDLE::default();
    unsafe { CreatePipe(&mut rd, &mut wr, Some(&sa), 0)? };

    let exe_w = wide(exe);
    let mut cmd_w = wide(cmdline);
    let mut si = STARTUPINFOW::default();
    si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    si.dwFlags = STARTF_USESTDHANDLES;
    si.hStdOutput = wr;
    si.hStdError = wr;

    let mut pi = PROCESS_INFORMATION::default();
    unsafe {
        CreateProcessW(
            PCWSTR(exe_w.as_ptr()),
            windows::core::PWSTR(cmd_w.as_mut_ptr()),
            None,
            None,
            true,
            DETACHED_PROCESS,
            None,
            PCWSTR::null(),
            &si,
            &mut pi,
        )?;
        // Our copy of the write end must go, or ReadFile never sees EOF.
        let _ = CloseHandle(wr);
    }

    let mut ran = false;
    for _ in 0..60 {
        if diggle_solver::win::window::find_by_pid(pi.dwProcessId).is_some() {
            ran = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    std::thread::sleep(settle);
    close_gracefully(pi.dwProcessId, pi.hProcess, Duration::from_secs(10));

    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let mut got = 0u32;
        let ok = unsafe { ReadFile(rd, Some(&mut buf), Some(&mut got), None) }.is_ok();
        if !ok || got == 0 {
            break;
        }
        out.extend_from_slice(&buf[..got as usize]);
    }
    unsafe {
        let _ = CloseHandle(rd);
    }
    Ok((ran, String::from_utf8_lossy(&out).into_owned()))
}

/// Asks the window to close, then WAITS ON THE PROCESS. Attempt #1 slept a fixed 4 s and then
/// asserted "nothing arrives even at exit" -- an unfounded claim, because it never established
/// that the CRT had had its chance to flush. Returns true if the process actually exited.
fn close_gracefully(pid: u32, handle: HANDLE, timeout: Duration) -> bool {
    if let Some(w) = diggle_solver::win::window::find_by_pid(pid) {
        unsafe {
            let _ = PostMessageW(w.hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
    let exited = unsafe { WaitForSingleObject(handle, timeout.as_millis() as u32) } == WAIT_OBJECT_0;
    if !exited {
        unsafe {
            let _ = TerminateProcess(handle, 0);
        }
    }
    exited
}

fn grow_buffer(rows: i16) -> Result<(i16, i16), Box<dyn std::error::Error>> {
    let h = open_console_handle("CONOUT$", None)?;
    let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
    unsafe { GetConsoleScreenBufferInfo(h, &mut info)? };
    let width = info.dwSize.X.max(120);
    unsafe { SetConsoleScreenBufferSize(h, COORD { X: width, Y: rows })? };
    Ok((width, rows))
}

/// Reads our console's screen buffer as text, up to the cursor row.
fn scrape() -> Result<String, Box<dyn std::error::Error>> {
    let h = open_console_handle("CONOUT$", None)?;
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

fn cursor() -> (i32, i32) {
    let mut p = windows::Win32::Foundation::POINT::default();
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut p);
    }
    (p.x, p.y)
}

/// Walks the hotspot highlight toward a CLIENT-space target, greedily. Returns the trail for
/// the report and where it landed, in client space.
fn nav_to(
    win: &diggle_solver::win::window::GameWindow,
    input: &diggle_solver::win::input::PostMessageInput,
    tx: i32,
    ty: i32,
) -> Result<(bool, (i32, i32), String), Box<dyn std::error::Error>> {
    const VK_DOWN: u16 = 0x28;
    const SC_DOWN: u16 = 0x50;
    const VK_UP: u16 = 0x26;
    const SC_UP: u16 = 0x48;
    const VK_LEFT: u16 = 0x25;
    const SC_LEFT: u16 = 0x4B;
    const VK_RIGHT: u16 = 0x27;
    const SC_RIGHT: u16 = 0x4D;

    let target = win.client_to_screen(tx, ty)?;
    let (ox, oy) = win.client_origin()?;
    let mut trail = String::new();
    // One press establishes a highlight; with none, Return is a no-op.
    input.press_extended_key(VK_DOWN, SC_DOWN)?;
    std::thread::sleep(Duration::from_millis(350));
    for i in 0..12 {
        let c = cursor();
        trail.push_str(&format!("  {i:2} client=({},{})\n", c.0 - ox, c.1 - oy));
        let (dx, dy) = (target.0 - c.0, target.1 - c.1);
        if ((dx * dx + dy * dy) as f64).sqrt() <= 60.0 {
            break;
        }
        let (vk, sc) = if dx.abs() >= dy.abs() {
            if dx > 0 { (VK_RIGHT, SC_RIGHT) } else { (VK_LEFT, SC_LEFT) }
        } else if dy > 0 {
            (VK_DOWN, SC_DOWN)
        } else {
            (VK_UP, SC_UP)
        };
        input.press_extended_key(vk, sc)?;
        std::thread::sleep(Duration::from_millis(350));
    }
    let c = cursor();
    let landed = (c.0 - ox, c.1 - oy);
    let on_target = (landed.0 - tx).abs() <= 60 && (landed.1 - ty).abs() <= 60;
    Ok((on_target, landed, trail))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cfg_path = std::env::args().nth(1).unwrap_or_else(|| "config.toml".into());
    std::fs::create_dir_all("spike-frames-live")?;
    let cfg = Config::load(Path::new(&cfg_path))?;
    let game_dir = cfg.game_dir.to_string_lossy().into_owned();
    let lovec = cfg.lovec_path.to_string_lossy().into_owned();

    let drive = std::env::args().any(|a| a == "drive");

    let mut log = String::from("# Log channel attempt #2: LÖVE's `--console` flag\n\n");
    log.push_str(&format!("lovec: `{lovec}`\ngame dir: `{game_dir}`\ndrive: {drive}\n\n"));

    // Refuse to be the second instance. Both would share one save directory, and the loser is
    // overwritten rather than merged.
    if let Err(e) = diggle_solver::win::process::refuse_if_running("lovec.exe", &[]) {
        log.push_str(&format!("**ABORTED**\n\n{e}\n"));
        std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
        eprintln!("{e}");
        return Ok(());
    }

    take_own_console()?;
    match grow_buffer(9999) {
        Ok((w, r)) => log.push_str(&format!("allocated own console; buffer {w} x {r}\n\n")),
        Err(e) => log.push_str(&format!("allocated own console; buffer grow failed: {e}\n\n")),
    }

    // ------------- PHASE 1: can we read a console we own? -------------
    const NEEDLE_A: &str = "DIGGLE_NEEDLE_ALPHA";
    const NEEDLE_B: &str = "DIGGLE_NEEDLE_BETA";
    log.push_str("## Phase 1 — control: read back a console we own\n\n");
    let control_line = format!(r"C:\Windows\System32\cmd.exe /c echo {NEEDLE_A} & echo {NEEDLE_B}");
    let (_, ch) = spawn_with_console_handles(r"C:\Windows\System32\cmd.exe", &control_line)?;
    std::thread::sleep(Duration::from_millis(1500));
    let t1 = scrape().unwrap_or_else(|e| format!("<scrape failed: {e}>"));
    unsafe {
        let _ = TerminateProcess(ch, 0);
    }
    let c1 = t1.contains(NEEDLE_A) && t1.contains(NEEDLE_B);
    log.push_str(&format!(
        "needles read back: {c1}\n\n**CONTROL 1: {}**\n\n",
        if c1 { "PASS" } else { "FAIL" }
    ));
    if !c1 {
        log.push_str(&format!("```\n{t1}```\n\nHarness broken; stopping before the game.\n"));
        std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
        return Ok(());
    }

    // ------------- PHASE 2: can a LÖVE process write into it? -------------
    // `--version` calls love_openConsole unconditionally (love.cpp:151-156) then printf's, so
    // this exercises detached-child -> AttachConsole(parent) -> freopen -> flush with no
    // dependence on the game or on the flag being parsed.
    log.push_str("## Phase 2 — control: `lovec.exe --version` on our console, DETACHED\n\n");
    let before2 = scrape().unwrap_or_default();
    let ver_line = format!("\"{lovec}\" --version");
    let (_, vh) = spawn_detached(&lovec, &ver_line)?;
    let ver_exited = unsafe { WaitForSingleObject(vh, 8000) } == WAIT_OBJECT_0;
    std::thread::sleep(Duration::from_millis(500));
    let t2 = scrape().unwrap_or_default();
    let c2 = t2.contains("LOVE 11.5") || t2.contains("LOVE ");
    log.push_str(&format!(
        "process exited: {ver_exited}\nnew lines: {}\nversion string present: {c2}\n\n\
         **CONTROL 2: {}**\n\n",
        nonblank(&t2) as i64 - nonblank(&before2) as i64,
        if c2 { "PASS" } else { "FAIL" }
    ));
    log.push_str(&format!("```\n{}```\n\n", t2.trim_start_matches(before2.as_str())));

    // ------------- PHASE 3: the real test -------------
    log.push_str("## Phase 3 — the game with `--verbose --console`\n\n");
    // Order is load-bearing: `--verbose` is not a LÖVE option (arg.lua:105-165), so placed
    // before the game directory LÖVE would take it AS the game path. `--console` is an option
    // and may sit anywhere.
    let game_line = format!("\"{lovec}\" \"{game_dir}\" --verbose --console");
    log.push_str(&format!("command line: `{game_line}`\n\n"));
    let before3 = scrape().unwrap_or_default();
    let base3 = nonblank(&before3);
    let (game_pid, game_handle) = spawn_detached(&lovec, &game_line)?;
    log.push_str(&format!("launched pid {game_pid}, DETACHED_PROCESS, no STARTF_USESTDHANDLES\n\n"));

    // CONTROL 3: prove it RAN. Silence from a process that never started is uninterpretable --
    // exactly what invalidated the original ConPTY verdict.
    let mut window_after: Option<u64> = None;
    for i in 1..=60 {
        if diggle_solver::win::window::find_by_pid(game_pid).is_some() {
            window_after = Some(i * 250);
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    match window_after {
        Some(ms) => log.push_str(&format!("window appeared after ~{ms} ms — **CONTROL 3: PASS**\n\n")),
        None => log.push_str("**CONTROL 3: FAIL** — no window in 15 s; verdict INVALID\n\n"),
    }

    // ------------- PHASE 5: force a print while the process keeps RUNNING -------------
    // Without this, "console stayed empty" is uninterpretable: with mainSaveData present the
    // game boots straight to the main menu and never shows the lore screen, so it prints
    // nothing at all. Continue -> world load -> verboseAdjacencyData('World loaded')
    // (overworldview.lua:1607) is both the liveness test and the payload we actually want.
    if drive && window_after.is_some() {
        log.push_str("### Phase 5 — drive to the overworld (warps the real pointer)\n\n");
        std::thread::sleep(Duration::from_millis(3000)); // let the menu settle
        let w = diggle_solver::win::window::find_by_pid(game_pid).ok_or("window vanished")?;
        let input = diggle_solver::win::input::PostMessageInput::new(w);
        input.focus();
        std::thread::sleep(Duration::from_millis(500));
        // Continue is at client (187,810); Restart shares the row at (500,810) and eulogizes
        // the run (heroselect.lua:271). So navigate, then VERIFY before activating.
        match nav_to(&w, &input, 187, 810) {
            Ok((on_target, landed, trail)) => {
                log.push_str(&format!("```\n{trail}```\nlanded at client {landed:?}\n\n"));
                if on_target {
                    input.press_key(0x0D, 0x1C)?; // Return
                    log.push_str("highlight verified on Continue — pressed Return\n\n");
                } else {
                    log.push_str(
                        "**REFUSED to press Return**: highlight is not on Continue. Restart is on \
                         the same row and would eulogize the run.\n\n",
                    );
                }
            }
            Err(e) => log.push_str(&format!("nav failed: {e}\n\n")),
        }
    }

    // WHEN matters as much as WHETHER: the pipe showed 0 bytes for 14 s, whereas a character
    // device should show text within a second or two.
    let mut samples: Vec<(u64, usize)> = Vec::new();
    let mut last = String::new();
    for i in 1..=7u64 {
        std::thread::sleep(Duration::from_millis(2000));
        match scrape() {
            Ok(t) => {
                samples.push((i * 2, nonblank(&t).saturating_sub(base3)));
                last = t;
            }
            Err(e) => log.push_str(&format!("t={}s scrape error: {e}\n", i * 2)),
        }
    }
    log.push_str("| t (s) | new non-blank lines |\n|---|---|\n");
    for (t, n) in &samples {
        log.push_str(&format!("| {t} | {n} |\n"));
    }

    // A window is not proof of a HEALTHY window: if `--console` broke boot we would get LÖVE's
    // blue error screen, which is also a window. So look at it.
    if let Some(w) = diggle_solver::win::window::find_by_pid(game_pid) {
        if let Ok(f) = diggle_solver::win::capture::capture_window(&w) {
            let _ = f.write_bmp(Path::new(SHOT));
            log.push_str(&format!("\nwindow screenshot: `{SHOT}` (check for a LÖVE error screen)\n"));
        }
    }

    let markers = [
        "Lore screen",
        "Local overworld data",
        "Adjacent connections",
        "posX",
        "Start menu",
        "Choose your champion",
        "Console redirection",
    ];
    log.push_str("\n### Markers while running\n\n");
    for m in markers {
        log.push_str(&format!("- `{m}`: {}\n", last.contains(m)));
    }
    log.push_str(&format!(
        "- em-dash decoded correctly: {} (mojibake `ΓÇö` present: {})\n",
        last.contains('\u{2014}'),
        last.contains("ΓÇö")
    ));

    let exited = close_gracefully(game_pid, game_handle, Duration::from_secs(15));
    let mut code = 0u32;
    unsafe {
        let _ = GetExitCodeProcess(game_handle, &mut code);
    }
    std::thread::sleep(Duration::from_millis(500));
    let after = scrape().unwrap_or_default();
    log.push_str(&format!(
        "\n### After graceful close\n\nprocess actually exited: **{exited}** (exit code {code})\n\
         non-blank while running: {}, after exit: {}\n\n",
        nonblank(&last),
        nonblank(&after)
    ));
    log.push_str(&format!("### Full console contents\n\n```\n{after}```\n\n"));

    let live = last.contains("Lore screen")
        || last.contains("Local overworld data")
        || samples.last().map(|(_, n)| *n > 0).unwrap_or(false);
    let at_exit = !live && nonblank(&after) > nonblank(&last);

    // ------------- PHASE 4: only if the console saw nothing -------------
    if !live && !at_exit && !drive {
        log.push_str(
            "## Phase 4 — discriminator: same command line minus `--console`, stdout to a pipe\n\n\
             Separates \"the game printed nothing\" from \"the game printed but we could not see \
             it\". Attempt #1 conflated these.\n\n",
        );
        let pipe_line = format!("\"{lovec}\" \"{game_dir}\" --verbose");
        match run_capturing_pipe(&lovec, &pipe_line, Duration::from_secs(10)) {
            Ok((ran, text)) => {
                log.push_str(&format!(
                    "window appeared: {ran}\nbytes captured: {}\n\n```\n{text}\n```\n\n",
                    text.len()
                ));
                log.push_str(&format!(
                    "**CONTROL 4: {}** — {}\n\n",
                    if !text.trim().is_empty() { "PASS" } else { "FAIL" },
                    if !text.trim().is_empty() {
                        "the command line is sound and the game DOES print; the fault is in the console path"
                    } else {
                        "the game printed nothing at all, so phase 3's silence proves nothing about the console"
                    }
                ));
            }
            Err(e) => log.push_str(&format!("pipe control errored: {e}\n\n")),
        }
    }

    let verdict = if window_after.is_none() {
        "INVALID — the game never opened a window, so its silence proves nothing"
    } else if !c2 {
        "INVALID — control 2 failed: a LÖVE process cannot write into our console at all, \
         so phase 3 was never a fair test of the flag"
    } else if live {
        "PASS — game output is readable live from a console we own"
    } else if at_exit {
        "FAIL (buffered) — output appears only at exit, so the console did not defeat full \
         buffering; a live channel needs something else"
    } else if drive {
        "FAIL — the game reached the overworld yet nothing appeared live; the console does not \
         carry the game's stdout"
    } else {
        "UNTESTED — console stayed empty, but nothing forced the game to print. Control 2 shows \
         a LÖVE process CAN write here; re-run with `drive` to make the game print while it \
         keeps running. See phase 4."
    };
    log.push_str(&format!("## Verdict\n\n**{verdict}**\n"));

    std::fs::File::create(REPORT)?.write_all(log.as_bytes())?;
    Ok(())
}
