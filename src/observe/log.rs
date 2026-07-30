//! The live log channel: a console we own, which the game writes its `_VERBOSE` output into.
//!
//! Measured working 2026-07-29 (see `docs/superpowers/plans/2026-07-29-log-channel-console-flag.md`).
//! The mechanism is LÖVE's own `--console` flag: `boot.lua:185-188` calls `love._openConsole`,
//! which does `AttachConsole(ATTACH_PARENT_PROCESS)` then `freopen("CONOUT$", "w", stdout)`
//! (`../love/src/modules/love/love.cpp:562-628`). So the game reopens stdout onto **our**
//! console, and MSVC's CRT does not fully buffer a character device — output arrives per call,
//! not at exit.
//!
//! Two non-obvious requirements, both learned the hard way:
//!
//! - The child must be spawned `DETACHED_PROCESS`. `AttachConsole` fails ACCESS_DENIED if the
//!   caller already HAS a console, and LÖVE then returns *without* the `freopen`. See
//!   [`crate::game::launch`].
//! - `SetConsoleOutputCP(65001)`. conhost decodes the child's bytes with the console's OUTPUT
//!   codepage; at the OEM default the em-dash in `Weedley Copse — level 0 crypt` arrives as
//!   `ΓÇö`. The em-dash is not decoration — it is the marker that a location has combat
//!   (`overworldview.lua:388-389`).

use std::io::Write;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Console::{
    AllocConsole, FillConsoleOutputAttribute, FillConsoleOutputCharacterW, FreeConsole,
    GetConsoleScreenBufferInfo, GetStdHandle, ReadConsoleOutputCharacterW, SetConsoleCursorPosition,
    SetConsoleOutputCP, SetConsoleScreenBufferSize, CONSOLE_SCREEN_BUFFER_INFO, COORD,
    STD_OUTPUT_HANDLE,
};

/// Rows of scrollback. An arrival prints ~5 lines and a finished pan reprints the block, so
/// this is thousands of events; [`Console::read_new`] recycles the buffer before it can wrap.
const BUFFER_ROWS: i16 = 9999;
const BUFFER_COLS: i16 = 200;

/// A console this process owns, allocated so that a detached child can attach to it.
///
/// Allocating a console replaces this process's standard handles, so our own stdout would
/// otherwise go silent. The original handle is captured first and exposed via [`Console::echo`],
/// which is what lets a CLI keep printing normally while the log channel is open.
pub struct Console {
    /// Whatever stdout was before we took a console — under a harness, a pipe. `FreeConsole`
    /// does not invalidate a pipe handle, so this keeps working.
    prior_stdout: Option<HANDLE>,
    /// Rows already returned by [`Console::read_new`].
    consumed: i16,
}

impl Console {
    /// Drops any inherited console and allocates one we own.
    ///
    /// Call once, before launching the game. Idempotency is not attempted: a second call would
    /// discard the buffer the game is writing into.
    pub fn take() -> Result<Self, crate::Error> {
        let prior = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) }.ok().filter(|h| !h.is_invalid());
        unsafe {
            let _ = FreeConsole(); // legitimately fails if we had none
            AllocConsole().map_err(|e| crate::Error::Win32(e.to_string()))?;
            let _ = SetConsoleOutputCP(65001); // CP_UTF8 — see module docs
        }
        let me = Console { prior_stdout: prior, consumed: 0 };
        me.resize(BUFFER_COLS, BUFFER_ROWS)?;
        Ok(me)
    }

    fn resize(&self, cols: i16, rows: i16) -> Result<(), crate::Error> {
        let h = self.conout()?;
        let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
        unsafe { GetConsoleScreenBufferInfo(h, &mut info) }
            .map_err(|e| crate::Error::Win32(e.to_string()))?;
        // Never shrink below the window, or the call fails.
        let cols = cols.max(info.srWindow.Right - info.srWindow.Left + 1);
        let rows = rows.max(info.srWindow.Bottom - info.srWindow.Top + 1);
        unsafe { SetConsoleScreenBufferSize(h, COORD { X: cols, Y: rows }) }
            .map_err(|e| crate::Error::Win32(e.to_string()))
    }

    /// `CONOUT$` names the attached console's screen buffer. `GetStdHandle` is not reliable
    /// after the allocate dance, so always go through the name.
    fn conout(&self) -> Result<HANDLE, crate::Error> {
        let name: Vec<u16> = "CONOUT$\0".encode_utf16().collect();
        unsafe {
            CreateFileW(
                PCWSTR(name.as_ptr()),
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        }
        .map_err(|e| crate::Error::Win32(e.to_string()))
    }

    /// Writes to the stdout we had before taking a console. Errors are swallowed: losing our
    /// own diagnostics must never take down a run.
    pub fn echo(&self, s: &str) {
        if let Some(h) = self.prior_stdout {
            let bytes = s.as_bytes();
            let mut written = 0u32;
            unsafe {
                let _ = WriteFile(h, Some(bytes), Some(&mut written), None);
            }
        }
    }

    /// Every line currently in the buffer, up to and including the cursor row.
    pub fn read_all(&self) -> Result<Vec<String>, crate::Error> {
        let h = self.conout()?;
        let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
        unsafe { GetConsoleScreenBufferInfo(h, &mut info) }
            .map_err(|e| crate::Error::Win32(e.to_string()))?;
        self.read_rows(h, &info, 0, info.dwCursorPosition.Y + 1)
    }

    fn read_rows(
        &self,
        h: HANDLE,
        info: &CONSOLE_SCREEN_BUFFER_INFO,
        from: i16,
        to: i16,
    ) -> Result<Vec<String>, crate::Error> {
        let width = info.dwSize.X;
        let mut buf = vec![0u16; width as usize];
        let mut out = Vec::new();
        for y in from..to.min(info.dwSize.Y) {
            let mut read = 0u32;
            unsafe {
                ReadConsoleOutputCharacterW(h, &mut buf, COORD { X: 0, Y: y }, &mut read)
                    .map_err(|e| crate::Error::Win32(e.to_string()))?;
            }
            out.push(String::from_utf16_lossy(&buf[..read as usize]).trim_end().to_string());
        }
        Ok(out)
    }

    /// Lines written since the previous call.
    ///
    /// A line still being written is deliberately left behind: the cursor row is excluded
    /// whenever the cursor is mid-line. Callers that frame on a terminator
    /// (`Local overworld data end`) therefore never see a half block.
    ///
    /// Recycles the buffer as it approaches full rather than letting conhost scroll, because a
    /// scroll would silently shift every row index this tracks.
    pub fn read_new(&mut self) -> Result<Vec<String>, crate::Error> {
        let h = self.conout()?;
        let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
        unsafe { GetConsoleScreenBufferInfo(h, &mut info) }
            .map_err(|e| crate::Error::Win32(e.to_string()))?;
        let cursor = info.dwCursorPosition;
        // NEVER consume the cursor row, even when the cursor is at column 0 and the row therefore
        // looks finished. That row is where the next `print` will land. Taking it consumed it while
        // empty, and the header line the game wrote there a moment later was lost for good — which
        // silently truncated an arrival dump to a headerless fragment and made a SUCCESSFUL travel
        // report as "no arrival dump within 45 s", twice.
        //
        // A row is only complete once the cursor has moved past it.
        let end = cursor.Y;

        if end < self.consumed {
            // Something reset the buffer under us (a `cls`, or LÖVE resizing it). Resync
            // rather than return garbage, and say so.
            self.consumed = 0;
        }
        let lines = self.read_rows(h, &info, self.consumed, end)?;
        self.consumed = end.max(self.consumed);

        if self.consumed >= info.dwSize.Y - 64 {
            self.clear()?;
        }
        Ok(lines)
    }

    /// Blanks the buffer and homes the cursor, so row indices start over.
    pub fn clear(&mut self) -> Result<(), crate::Error> {
        let h = self.conout()?;
        let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
        unsafe { GetConsoleScreenBufferInfo(h, &mut info) }
            .map_err(|e| crate::Error::Win32(e.to_string()))?;
        let cells = info.dwSize.X as u32 * info.dwSize.Y as u32;
        let home = COORD { X: 0, Y: 0 };
        let mut done = 0u32;
        unsafe {
            let _ = FillConsoleOutputCharacterW(h, ' ' as u16, cells, home, &mut done);
            let _ = FillConsoleOutputAttribute(h, info.wAttributes.0, cells, home, &mut done);
            SetConsoleCursorPosition(h, home).map_err(|e| crate::Error::Win32(e.to_string()))?;
        }
        self.consumed = 0;
        Ok(())
    }
}

/// Mirrors everything the channel produces to a file, so a failed run is still diagnosable.
pub struct LogMirror {
    file: std::fs::File,
}

impl LogMirror {
    pub fn create(path: &std::path::Path) -> Result<Self, crate::Error> {
        Ok(Self { file: std::fs::File::create(path)? })
    }

    pub fn write(&mut self, lines: &[String]) {
        for l in lines {
            let _ = writeln!(self.file, "{l}");
        }
        let _ = self.file.flush();
    }
}
