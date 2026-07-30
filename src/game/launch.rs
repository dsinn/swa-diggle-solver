//! Launching the game so that its `_VERBOSE` output lands in a console we can read.
//!
//! Every detail here is load-bearing and was established by measurement — see
//! [`crate::observe::log`] for the mechanism and
//! `docs/superpowers/plans/2026-07-29-log-channel-console-flag.md` for the evidence.

use crate::config::Config;
use crate::observe::log::Console;
use crate::win::window::GameWindow;
use std::time::{Duration, Instant};
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::Threading::{
    CreateProcessW, TerminateProcess, WaitForSingleObject, DETACHED_PROCESS, PROCESS_INFORMATION,
    STARTUPINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

/// Builds the raw command line. Split out so the argument ORDER is unit-testable without
/// spawning anything — the ordering rule below is easy to break and silent when broken.
///
/// - `--verbose` sets `_VERBOSE` (`main.lua:37`), which is what gates every dump we want.
///   It is **not** a LÖVE option: `arg.lua:105-165` falls through to `elseif not game then
///   game = i`, so placed *before* the game directory LÖVE would treat it as the game path.
/// - `--console` is a real LÖVE option (`arg.lua:116`), consumed by `parseOptions` and stripped
///   from the game's own `arg`, so its position is free. It is what makes LÖVE reopen stdout
///   onto our console at all.
///
/// A raw command line rather than an argv list because `CreateProcessW` takes one string, and
/// quoting has to be ours to control: the exe path and game directory contain spaces, the flags
/// must not be quoted.
pub fn build_command_line(cfg: &Config) -> String {
    format!(
        "\"{}\" \"{}\" --verbose --console",
        cfg.lovec_path.display(),
        cfg.game_dir.display()
    )
}

pub struct GameProcess {
    pid: u32,
    handle: HANDLE,
}

impl GameProcess {
    /// Spawns the game attached to `console`.
    ///
    /// Requires a [`Console`] by reference rather than documenting it as a precondition: LÖVE
    /// attaches to the **parent's** console, so without one there is nowhere for its output to
    /// go and the whole launch is pointless.
    ///
    /// Refuses to start a second instance. Two share one save directory, and `overworld:save()`
    /// (`overworld.lua:562-568`) overwrites rather than merges, so a long-lived instance
    /// silently clobbers the other on its next screen exit.
    pub fn launch(cfg: &Config, _console: &Console) -> Result<Self, crate::Error> {
        crate::win::process::refuse_if_running("lovec.exe", &[])?;

        let exe: Vec<u16> = cfg
            .lovec_path
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut cmdline: Vec<u16> =
            build_command_line(cfg).encode_utf16().chain(std::iter::once(0)).collect();

        let mut si = STARTUPINFOW::default();
        si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        // NO `STARTF_USESTDHANDLES`, and no inherited handles. Naming stdout for the child is
        // what defeated the first attempt: LÖVE's `AttachConsole(ATTACH_PARENT_PROCESS)` returns
        // ACCESS_DENIED when the child already has a console, and `love.cpp:571-578` then
        // returns *without* the `freopen`. DETACHED_PROCESS leaves the child console-less so the
        // attach succeeds and the redirection actually happens.
        let mut pi = PROCESS_INFORMATION::default();
        unsafe {
            CreateProcessW(
                PCWSTR(exe.as_ptr()),
                PWSTR(cmdline.as_mut_ptr()),
                None,
                None,
                false,
                DETACHED_PROCESS,
                None,
                PCWSTR::null(),
                &si,
                &mut pi,
            )
            .map_err(|e| crate::Error::Win32(e.to_string()))?;
            let _ = CloseHandle(pi.hThread);
        }
        Ok(Self { pid: pi.dwProcessId, handle: pi.hProcess })
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn is_running(&self) -> bool {
        let state = unsafe { WaitForSingleObject(self.handle, 0) };
        state != WAIT_OBJECT_0
    }

    pub fn window(&self) -> Option<GameWindow> {
        crate::win::window::find_by_pid(self.pid)
    }

    /// Waits for the game to open its window.
    ///
    /// Worth having as its own step: silence from a process that never started is
    /// uninterpretable, and mistaking the two has produced two invalid verdicts on this project.
    pub fn wait_for_window(&self, timeout: Duration) -> Result<GameWindow, crate::Error> {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if let Some(w) = self.window() {
                return Ok(w);
            }
            if !self.is_running() {
                return Err(crate::Error::Win32(format!(
                    "game (pid {}) exited before opening a window",
                    self.pid
                )));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        Err(crate::Error::Win32(format!(
            "game (pid {}) opened no window within {:?}",
            self.pid, timeout
        )))
    }

    /// Asks the window to close and WAITS for the process, falling back to termination.
    ///
    /// Returns whether it exited on its own. Waiting matters twice over: the CRT flushes on a
    /// graceful exit but not on `TerminateProcess`, and a fixed sleep instead of a wait is how
    /// an earlier spike produced an unfounded "nothing arrives even at exit".
    pub fn close(&mut self, timeout: Duration) -> bool {
        if let Some(w) = self.window() {
            unsafe {
                let _ = PostMessageW(w.hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        let exited =
            unsafe { WaitForSingleObject(self.handle, timeout.as_millis() as u32) } == WAIT_OBJECT_0;
        if !exited {
            self.kill();
        }
        exited
    }

    /// Last resort. Skips the CRT flush, so anything unwritten is lost.
    pub fn kill(&mut self) {
        unsafe {
            let _ = TerminateProcess(self.handle, 0);
            let _ = WaitForSingleObject(self.handle, 5000);
        }
    }
}

impl Drop for GameProcess {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// The original pipe-based launcher, kept **only** so the historical spikes still build and can
/// be re-run as records.
///
/// Superseded by [`GameProcess`]: stdout on a pipe is fully buffered by the CRT, and this game's
/// output is far too sparse to ever fill 4 KB — 14 s of running produced 0 bytes, and the 16
/// bytes that did appear came from the shutdown flush. Do not use it for anything that needs to
/// read the log. It also does not pass `--console`, and it takes no [`Console`], so it cannot.
pub struct PipedGameProcess {
    child: std::process::Child,
}

impl PipedGameProcess {
    pub fn launch(cfg: &Config) -> Result<Self, crate::Error> {
        Self::launch_with_env(cfg, &[])
    }

    /// SDL reads its hints from the environment, which is how the click spikes influenced SDL's
    /// input behaviour without modifying the game.
    pub fn launch_with_env(cfg: &Config, env: &[(&str, &str)]) -> Result<Self, crate::Error> {
        let mut cmd = std::process::Command::new(&cfg.lovec_path);
        cmd.arg(&cfg.game_dir).arg("--verbose");
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null());
        Ok(Self { child: cmd.spawn()? })
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Takes ownership of the stdout pipe. Callable once.
    pub fn stdout(&mut self) -> Option<std::process::ChildStdout> {
        self.child.stdout.take()
    }

    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub fn kill(&mut self) -> Result<(), crate::Error> {
        let _ = self.child.kill();
        let _ = self.child.wait();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn cfg() -> Config {
        Config {
            game_dir: PathBuf::from(r"C:\my games\swa"),
            lovec_path: PathBuf::from(r"C:\love 11.5\lovec.exe"),
            ..Default::default()
        }
    }

    #[test]
    fn command_line_quotes_paths_but_not_flags() {
        assert_eq!(
            build_command_line(&cfg()),
            "\"C:\\love 11.5\\lovec.exe\" \"C:\\my games\\swa\" --verbose --console"
        );
    }

    #[test]
    fn verbose_comes_after_the_game_directory() {
        // Before it, LÖVE takes `--verbose` AS the game path (`arg.lua:105-165`) and the game
        // never loads. Nothing else in the system would point at the argument order.
        let line = build_command_line(&cfg());
        let dir = line.find(r"swa").expect("game dir present");
        let verbose = line.find("--verbose").expect("verbose present");
        assert!(dir < verbose, "game directory must precede --verbose: {line}");
    }

    #[test]
    fn console_flag_is_present() {
        // Without it `love._openConsole` is never called (`boot.lua:185-188`) and the log
        // channel is silent -- the exact defect that made the first spike fail.
        assert!(build_command_line(&cfg()).contains("--console"));
    }
}
