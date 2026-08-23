//! Spike 1b: does attaching the game to a pseudo-console (ConPTY) defeat the CRT
//! block-buffering that made --verbose useless on a plain pipe?
//!
//! Self-verifying: prints a VERDICT line, writes its findings document, exits
//! non-zero on failure.
//!
//! Run: cargo run --bin spike_conpty -- config.toml

use diggle_solver::config::Config;
use std::io::Write;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::ReadFile;
use windows::Win32::System::Console::{ClosePseudoConsole, CreatePseudoConsole, COORD, HPCON};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, InitializeProcThreadAttributeList,
    TerminateProcess, UpdateProcThreadAttribute, EXTENDED_STARTUPINFO_PRESENT,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, STARTUPINFOEXW, STARTUPINFOW,
};

/// PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE. Defined manually because the crate does not
/// export it as a named constant.
const ATTR_PSEUDOCONSOLE: usize = 0x0002_0016;

/// STILL_ACTIVE (winbase.h). Not exported by the `windows` crate as a constant we
/// could import, so defined manually. This is the well-known value 259.
const STILL_ACTIVE: u32 = 259;

const NEEDLE: &str = "Lore screen:";
const DEADLINE: Duration = Duration::from_secs(20);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).expect("usage: spike_conpty <config.toml>");
    let cfg = Config::load(std::path::Path::new(&path))?;

    // Force the lore screen so the game definitely prints (see Step 2 of the brief).
    // ONLY the unfused sandbox save dir. Never touch %APPDATA%\SternlyWordedAdventures
    // (no LOVE) -- that is the user's real Steam save.
    if let Ok(appdata) = std::env::var("APPDATA") {
        let p = std::path::Path::new(&appdata)
            .join("LOVE")
            .join("SternlyWordedAdventures")
            .join("persistentSaveData");
        let _ = std::fs::remove_file(&p);
    }

    let mut pi = PROCESS_INFORMATION::default();
    let mut hpc = HPCON::default();
    let mut in_write = HANDLE::default();

    let result: Result<(bool, Option<Duration>, usize), Box<dyn std::error::Error>> = (|| unsafe {
        // Two pipes: one the pty reads our input from, one we read the pty's output from.
        let (mut in_read, mut iw) = (HANDLE::default(), HANDLE::default());
        let (mut out_read, mut out_write) = (HANDLE::default(), HANDLE::default());
        CreatePipe(&mut in_read, &mut iw, None, 0)?;
        CreatePipe(&mut out_read, &mut out_write, None, 0)?;
        in_write = iw;

        let size = COORD { X: 200, Y: 50 };
        hpc = CreatePseudoConsole(size, in_read, out_write, 0)?;

        // NOTE: per Microsoft's "Creating a Pseudoconsole session" doc, the caller's
        // copies of in_read/out_write are freed AFTER CreateProcessW succeeds, not
        // immediately here (moved below).

        // Build the attribute list carrying the pseudoconsole. First call (with a null
        // list) is expected to "fail" -- it only exists to report the required size.
        let mut bytes: usize = 0;
        let _ = InitializeProcThreadAttributeList(
            LPPROC_THREAD_ATTRIBUTE_LIST(std::ptr::null_mut()),
            1,
            0,
            &mut bytes,
        );
        let mut buf = vec![0u8; bytes];
        let attrs = LPPROC_THREAD_ATTRIBUTE_LIST(buf.as_mut_ptr() as *mut _);
        InitializeProcThreadAttributeList(attrs, 1, 0, &mut bytes)?;
        // Matches Microsoft's own sample at
        // learn.microsoft.com/windows/console/creating-a-pseudoconsole-session, which
        // passes `hpc` directly as lpValue (`UpdateProcThreadAttribute(..., hpc,
        // sizeof(hpc), ...)`) -- the handle's bit pattern used directly as the PVOID
        // value, not a pointer to a variable holding it. An earlier revision of this
        // file tried `&hpc as *const HPCON as *const c_void` (pointer TO the handle)
        // on the theory that the original was backwards; empirically, verified via
        // spike_conpty_control.rs, that "fix" is wrong: it made the child receive no
        // usable console at all (0 bytes captured, no leak anywhere), a strictly worse
        // result than this version. Reverted.
        UpdateProcThreadAttribute(
            attrs,
            0,
            ATTR_PSEUDOCONSOLE,
            Some(hpc.0 as *const std::ffi::c_void),
            std::mem::size_of::<HPCON>(),
            None,
            None,
        )?;

        let mut si = STARTUPINFOEXW::default();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si.lpAttributeList = attrs;

        // Quote the exe and game dir: both contain spaces on this machine.
        let cmdline =
            format!("\"{}\" \"{}\" --verbose", cfg.lovec_path.display(), cfg.game_dir.display());
        let mut cmd: Vec<u16> = cmdline.encode_utf16().collect();
        cmd.push(0);

        // Take the address of the WHOLE STARTUPINFOEXW and round-trip it through usize
        // before casting down to *const STARTUPINFOW, rather than `&si.StartupInfo`
        // directly. This is the fix reported in microsoft/windows-rs#1397 for exactly
        // this symptom (child bypasses the pty, inherits the parent's real console):
        // `&si.StartupInfo` borrows only the nested sub-object, and rustc attaches
        // noalias/dereferenceable(size_of::<STARTUPINFOW>()) provenance to that
        // reference regardless of optimization level -- narrower than what
        // CreateProcessW actually reads once EXTENDED_STARTUPINFO_PRESENT is set (it
        // reads lpAttributeList too, past the end of that narrower borrow).
        let si_ptr = &si as *const STARTUPINFOEXW;
        let si_ptr_addr = si_ptr as usize;
        let si_w_ptr = si_ptr_addr as *const STARTUPINFOW;

        let started = Instant::now();
        CreateProcessW(
            None,
            PWSTR(cmd.as_mut_ptr()),
            None,
            None,
            false,
            EXTENDED_STARTUPINFO_PRESENT,
            None,
            None,
            si_w_ptr,
            &mut pi,
        )?;

        DeleteProcThreadAttributeList(attrs);

        // Now that the child has been created attached to the pseudoconsole, free our
        // copies of the ends the pty owns -- per Microsoft's documented ordering.
        let _ = CloseHandle(in_read);
        let _ = CloseHandle(out_write);

        // Drain the pty output on a background thread. HANDLE wraps a raw pointer and
        // is not Send, but a Win32 HANDLE is just an opaque integer that is valid to
        // use from any thread, so we ferry it across as a raw pointer value.
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        let reader_raw = out_read.0 as usize;
        std::thread::spawn(move || {
            let reader = HANDLE(reader_raw as *mut std::ffi::c_void);
            let mut buf = [0u8; 4096];
            loop {
                let mut read = 0u32;
                if ReadFile(reader, Some(&mut buf), Some(&mut read), None).is_err() || read == 0 {
                    break;
                }
                if tx.send(buf[..read as usize].to_vec()).is_err() {
                    break;
                }
            }
        });

        // The alive-check happens in the SAME iteration the needle is detected, before
        // any further reads, sleeps, or cleanup -- this is what makes "found while
        // alive" mean something: there is no window in which the process could have
        // exited and been mistaken for still running. GetExitCodeProcess against the
        // live handle is the ground truth the OS gives us for "is this process still
        // running right now".
        use windows::Win32::System::Threading::GetExitCodeProcess;
        let mut acc = String::new();
        let mut found_at: Option<Duration> = None;
        let mut alive_when_found = false;
        while started.elapsed() < DEADLINE && found_at.is_none() {
            if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(250)) {
                acc.push_str(&String::from_utf8_lossy(&chunk));
                if acc.contains(NEEDLE) {
                    found_at = Some(started.elapsed());
                    let mut exit_code: u32 = 0;
                    alive_when_found = GetExitCodeProcess(pi.hProcess, &mut exit_code).is_ok()
                        && exit_code == STILL_ACTIVE;
                }
            }
        }

        // Deliberately NOT closing out_read here: the background reader thread may
        // still be blocked inside a synchronous ReadFile on this exact handle value
        // (it's the same handle, not a duplicate, ferried by raw value into that
        // thread). Closing a handle out from under another thread's in-flight
        // synchronous I/O on Windows is a documented race -- CloseHandle can hang
        // waiting on the pending operation instead of returning immediately. This
        // was reproduced: the previous version of this spike hung indefinitely here
        // (0.015s of CPU time accumulated over 3+ minutes of wall time = blocked in
        // a syscall, not busy-looping the poll loop). The fix is to never explicitly
        // close out_read: the reader thread's ReadFile will unblock on its own once
        // ClosePseudoConsole (called in the outer cleanup, after this closure
        // returns) tears down the pty's write end, and the handle is reclaimed for
        // free when this process exits moments later.
        Ok((alive_when_found, found_at, acc.len()))
    })();

    // Everything above happens BEFORE we terminate, so a pass cannot be an artifact
    // of exit-time flushing: we captured the process's alive/dead state via
    // GetExitCodeProcess at the instant the needle appeared, prior to this cleanup.
    unsafe {
        if !pi.hProcess.is_invalid() {
            let _ = TerminateProcess(pi.hProcess, 1);
            let _ = CloseHandle(pi.hProcess);
        }
        if !pi.hThread.is_invalid() {
            let _ = CloseHandle(pi.hThread);
        }
        if hpc.0 != 0 {
            ClosePseudoConsole(hpc);
        }
        if !in_write.is_invalid() {
            let _ = CloseHandle(in_write);
        }
    }

    let (alive_when_found, found_at, bytes_captured) = match result {
        Ok(v) => v,
        Err(e) => {
            eprintln!("spike_conpty: error before verdict could be determined: {e}");
            return Err(e);
        }
    };

    let pass = alive_when_found && found_at.map_or(false, |d| d < Duration::from_secs(15));
    let report = format!(
        "# Spike 1b — ConPTY log delivery\n\n\
         VERDICT: {}\n\n\
         - needle: {NEEDLE:?}\n\
         - found while process alive: {alive_when_found}\n\
         - time to needle: {}\n\
         - total bytes captured before terminate: {}\n\n\
         Spike 1 established that on a plain pipe this text does not arrive until\n\
         the process exits gracefully. If this spike PASSES, a pseudo-console\n\
         defeats the CRT block-buffering and the log channel is recoverable with\n\
         no changes to the game: rework PipedGameProcess::launch onto ConPTY (Task 23).\n\n\
         If it FAILS, ConPTY does not help. Stop and escalate -- the remaining\n\
         options (modify the game, or scrape its debug console) are the human\n\
         partner's decision.\n",
        if pass { "PASS" } else { "FAIL" },
        found_at.map_or("never".into(), |d| format!("{:.2}s", d.as_secs_f64())),
        bytes_captured,
    );

    std::fs::create_dir_all("docs/superpowers/spikes")?;
    std::fs::File::create("docs/superpowers/spikes/01b-conpty.md")?.write_all(report.as_bytes())?;
    print!("{report}");
    if !pass {
        std::process::exit(1);
    }
    Ok(())
}
