//! Control experiment for Spike 1b (see spike_conpty.rs).
//!
//! The original spike proved bytes flow on the ConPTY output pipe (VT negotiation
//! sequences arrived), but never proved that a CHILD PROCESS's stdout was actually
//! wired to that pty. If the harness attaches the pseudoconsole incorrectly, a child
//! would have no usable console, its writes would go nowhere, and we'd see exactly
//! what the original spike saw: handshake bytes and nothing else.
//!
//! This binary swaps the child for `cmd.exe /c "echo ... & ping ... & echo ..."` --
//! a program guaranteed to be chatty and guaranteed to write through the C runtime
//! while it is running. The pipe setup, pseudoconsole creation, attribute list, and
//! reader thread are unchanged from spike_conpty.rs; only the command line passed to
//! CreateProcessW (and the fact that we now watch for two needles) differs.
//!
//! Run: cargo run --bin spike_conpty_control

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

const NEEDLE_1: &str = "CONTROL_NEEDLE_1";
const NEEDLE_2: &str = "CONTROL_NEEDLE_2";
const DEADLINE: Duration = Duration::from_secs(20);

#[derive(Debug, Default, Clone, Copy)]
struct NeedleResult {
    found_at: Option<Duration>,
    alive_when_found: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut pi = PROCESS_INFORMATION::default();
    let mut hpc = HPCON::default();
    let mut in_write = HANDLE::default();

    let result: Result<(NeedleResult, NeedleResult, usize), Box<dyn std::error::Error>> =
        (|| unsafe {
            // Two pipes: one the pty reads our input from, one we read the pty's output
            // from. Identical to spike_conpty.rs.
            let (mut in_read, mut iw) = (HANDLE::default(), HANDLE::default());
            let (mut out_read, mut out_write) = (HANDLE::default(), HANDLE::default());
            CreatePipe(&mut in_read, &mut iw, None, 0)?;
            CreatePipe(&mut out_read, &mut out_write, None, 0)?;
            in_write = iw;

            let size = COORD { X: 200, Y: 50 };
            hpc = CreatePseudoConsole(size, in_read, out_write, 0)?;

            // NOTE: per Microsoft's own "Creating a Pseudoconsole session" doc, the
            // caller's copies of in_read/out_write should be freed AFTER CreateProcess
            // succeeds, not immediately here. Moved below, after CreateProcessW.

            // Build the attribute list carrying the pseudoconsole. First call (with a
            // null list) is expected to "fail" -- it only exists to report the
            // required size.
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
            // REVERTED: Microsoft's own sample at
            // learn.microsoft.com/windows/console/creating-a-pseudoconsole-session
            // passes `hpc` directly as lpValue (`UpdateProcThreadAttribute(..., hpc,
            // sizeof(hpc), ...)`), not `&hpc`. HPCON is itself a pointer-sized opaque
            // handle (like HANDLE), and for this specific attribute the API wants the
            // handle's bit pattern used directly as the PVOID value -- not a pointer
            // to a variable holding it. Passing `&hpc` (verified by direct test) makes
            // CreateProcessW attach an invalid pseudoconsole reference, causing the
            // child to receive no usable console at all (0 bytes captured, no leak to
            // the real terminal either -- consistent with Windows' documented failure
            // mode "given an invalid pseudoconsole handle for startup").
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

            // The only deliberate change from spike_conpty.rs: swap the game for a
            // guaranteed-chatty control child. echo/ping write through the C runtime
            // while running; the ping spaces the two needles ~5s apart so we can
            // confirm delivery while the process is alive, not only at exit.
            let cmdline =
                r#"cmd.exe /c "echo CONTROL_NEEDLE_1 & ping -n 6 127.0.0.1 & echo CONTROL_NEEDLE_2""#;
            let mut cmd: Vec<u16> = cmdline.encode_utf16().collect();
            cmd.push(0);

            // BUG FIX #3: `&si.StartupInfo` borrows only the nested STARTUPINFOW
            // sub-object; rustc attaches noalias/dereferenceable(size_of::<STARTUPINFOW>())
            // provenance to that reference regardless of optimization level, which is
            // narrower than the full STARTUPINFOEXW CreateProcessW actually reads once
            // EXTENDED_STARTUPINFO_PRESENT is set (it reads lpAttributeList, which
            // lives past the end of that narrower borrow). This is a documented
            // windows-rs gotcha (microsoft/windows-rs#1397) with exactly this
            // symptom: the attribute list is silently ignored and the child inherits
            // the parent's real console instead of the pseudoconsole. The fix is to
            // take the address of the WHOLE STARTUPINFOEXW and round-trip it through
            // usize to erase that narrow provenance before casting down to
            // *const STARTUPINFOW.
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
            eprintln!(
                "DEBUG hpc.0={:#x} pid={} tid={}",
                hpc.0, pi.dwProcessId, pi.dwThreadId
            );

            DeleteProcThreadAttributeList(attrs);

            // Now that the child has been created attached to the pseudoconsole, free
            // our copies of the ends the pty owns -- per Microsoft's documented
            // ordering ("Upon completion of the CreateProcess call ... the handles
            // given during creation should be freed from this process").
            let _ = CloseHandle(in_read);
            let _ = CloseHandle(out_write);

            // Drain the pty output on a background thread. Identical to
            // spike_conpty.rs.
            let (tx, rx) = mpsc::channel::<Vec<u8>>();
            let reader_raw = out_read.0 as usize;
            std::thread::spawn(move || {
                eprintln!("DEBUG reader thread started, handle={:#x}", reader_raw);
                let reader = HANDLE(reader_raw as *mut std::ffi::c_void);
                let mut buf = [0u8; 4096];
                loop {
                    let mut read = 0u32;
                    let r = ReadFile(reader, Some(&mut buf), Some(&mut read), None);
                    if let Err(e) = &r {
                        eprintln!("DEBUG ReadFile error: {e:?}");
                    }
                    if r.is_err() || read == 0 {
                        eprintln!("DEBUG reader thread exiting, read={read}");
                        break;
                    }
                    eprintln!("DEBUG reader thread got {read} bytes");
                    if tx.send(buf[..read as usize].to_vec()).is_err() {
                        break;
                    }
                }
            });

            use windows::Win32::System::Threading::GetExitCodeProcess;
            let mut acc = String::new();
            let mut r1 = NeedleResult::default();
            let mut r2 = NeedleResult::default();
            while started.elapsed() < DEADLINE && (r1.found_at.is_none() || r2.found_at.is_none())
            {
                if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(250)) {
                    acc.push_str(&String::from_utf8_lossy(&chunk));
                    // Same technique as the original: the alive-check happens in the
                    // same iteration the needle is detected, before any further
                    // reads, sleeps, or cleanup.
                    if r1.found_at.is_none() && acc.contains(NEEDLE_1) {
                        let mut exit_code: u32 = 0;
                        let alive = GetExitCodeProcess(pi.hProcess, &mut exit_code).is_ok()
                            && exit_code == STILL_ACTIVE;
                        r1 = NeedleResult {
                            found_at: Some(started.elapsed()),
                            alive_when_found: alive,
                        };
                    }
                    if r2.found_at.is_none() && acc.contains(NEEDLE_2) {
                        let mut exit_code: u32 = 0;
                        let alive = GetExitCodeProcess(pi.hProcess, &mut exit_code).is_ok()
                            && exit_code == STILL_ACTIVE;
                        r2 = NeedleResult {
                            found_at: Some(started.elapsed()),
                            alive_when_found: alive,
                        };
                    }
                }
            }

            eprintln!("DEBUG raw bytes captured: {:?}", acc.as_bytes());
            // Deliberately NOT closing out_read here -- same race as documented in
            // spike_conpty.rs: the background reader thread may still be blocked
            // inside a synchronous ReadFile on this exact handle value. Closing it
            // out from under that pending read is what hung the original spike for
            // 3+ minutes. Left unclosed; reclaimed for free at process exit.
            Ok((r1, r2, acc.len()))
        })();

    // Everything above happens BEFORE we terminate.
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

    let (r1, r2, bytes_captured) = match result {
        Ok(v) => v,
        Err(e) => {
            eprintln!("spike_conpty_control: error before verdict could be determined: {e}");
            return Err(e);
        }
    };

    let control_pass = r1.alive_when_found && r1.found_at.is_some();
    let report = format!(
        "# Spike 1b control -- ConPTY harness soundness check\n\n\
         CONTROL VERDICT: {}\n\n\
         - needle 1 ({NEEDLE_1:?}): found={} alive_when_found={} at={}\n\
         - needle 2 ({NEEDLE_2:?}): found={} alive_when_found={} at={}\n\
         - total bytes captured before terminate: {}\n",
        if control_pass { "PASS (harness sound)" } else { "FAIL (harness broken)" },
        r1.found_at.is_some(),
        r1.alive_when_found,
        r1.found_at.map_or("never".into(), |d| format!("{:.2}s", d.as_secs_f64())),
        r2.found_at.is_some(),
        r2.alive_when_found,
        r2.found_at.map_or("never".into(), |d| format!("{:.2}s", d.as_secs_f64())),
        bytes_captured,
    );

    print!("{report}");
    if !control_pass {
        std::process::exit(1);
    }
    Ok(())
}
