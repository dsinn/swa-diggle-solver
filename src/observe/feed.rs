//! The console, drained and remembered.
//!
//! Every live loop in this project needs the same three things: pull whatever the game has printed
//! since last time, keep a copy on disk for post-mortems, and ask whether some announcement has
//! appeared. Each spike grew its own `pump!` macro closing over three locals, which is why none of
//! them could share a combat loop or a travel step.
//!
//! The one subtlety worth stating: [`Feed::mark`] and [`Feed::seen_since`] exist because "has the
//! reward screen appeared" is almost never the right question — `Item selection:` from the *last*
//! fight is still in the buffer. What is wanted is "has it appeared **since I acted**", and getting
//! that wrong reads a stale line as a fresh result.

use crate::observe::log::{Console, LogMirror};

pub struct Feed {
    console: Console,
    mirror: Option<LogMirror>,
    lines: Vec<String>,
}

impl Feed {
    pub fn new(console: Console, mirror: Option<LogMirror>) -> Self {
        Feed { console, mirror, lines: Vec::new() }
    }

    /// Reads whatever is new, mirrors it, and returns the lines just added.
    pub fn pump(&mut self) -> &[String] {
        let start = self.lines.len();
        if let Ok(new) = self.console.read_new() {
            if !new.is_empty() {
                if let Some(m) = self.mirror.as_mut() {
                    m.write(&new);
                }
                self.lines.extend(new);
            }
        }
        &self.lines[start..]
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// The console itself, which launching a game needs a reference to.
    ///
    /// A session can outlive one game process — the anomaly skip closes the game mid-cinematic and
    /// relaunches it — and the console must survive that, or the second launch has nothing to
    /// attach to and the log channel goes silent.
    pub fn console(&self) -> &Console {
        &self.console
    }

    /// Where the buffer currently ends — pass to [`Feed::seen_since`] before acting.
    pub fn mark(&self) -> usize {
        self.lines.len()
    }

    /// Has `needle` appeared since `mark`?
    ///
    /// Prefer this to [`Feed::seen`] for anything that recurs. A fight leaves `Item selection:` in
    /// the buffer, and the next fight would match it immediately.
    pub fn seen_since(&self, mark: usize, needle: &str) -> bool {
        self.lines[mark.min(self.lines.len())..].iter().any(|l| l.contains(needle))
    }

    /// Has `needle` appeared at any point? Only safe for announcements that happen once.
    pub fn seen(&self, needle: &str) -> bool {
        self.lines.iter().any(|l| l.contains(needle))
    }

    /// Lines added since `mark`, for parsing a block that has just arrived.
    pub fn since(&self, mark: usize) -> &[String] {
        &self.lines[mark.min(self.lines.len())..]
    }
}
