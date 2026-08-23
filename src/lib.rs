pub mod act;
pub mod buyer;
pub mod combat;
pub mod config;
pub mod fight;
pub mod flee;
pub mod game;
pub mod gear;
pub mod geometry;
pub mod heroselect;
pub mod innplay;
pub mod itemchoice;
pub mod items;
pub mod layout;
pub mod letters;
pub mod lexica;
pub mod navigate;
pub mod observe;
pub mod overworld;
pub mod parity;
pub mod pick;
pub mod rest;
pub mod rested;
pub mod score;
pub mod search;
pub mod shopplay;
pub mod shrine;
pub mod shrineplay;
pub mod stamp;
pub mod subworld;
pub mod tables;
pub mod timing;
pub mod tower;
pub mod typist;
pub mod win;

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("APPDATA environment variable is not set")]
    NoAppData,
    #[error("save directory does not exist: {0}")]
    SaveDirMissing(PathBuf),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("lua: {0}")]
    Lua(#[from] mlua::Error),
    #[error("config: {0}")]
    Config(String),
    #[error("win32: {0}")]
    Win32(String),
}

#[cfg(test)]
mod source_hygiene {
    /// **A run of spaces in the middle of a message is a lost line continuation**, and it prints.
    ///
    /// Rust's `\` at end of line swallows the newline *and* the indentation that follows, which is
    /// how every long message in this crate is wrapped. Lose the backslash and the indentation stops
    /// being invisible: the string keeps it, and the run report gets `reading the` followed by
    /// twenty-two spaces and then `slot anyway`.
    ///
    /// Nine of them had accumulated by 2026-08-23, from `047c65a` onwards. Nothing catches this on
    /// the way past — it compiles, the tests pass, and the damage is only visible in output nobody
    /// diffs — so it is caught here instead. The proximate cause each time was a shell heredoc
    /// eating the backslash on the way into an edit script, which is a hazard of the tooling rather
    /// than of the language, and it has struck often enough to be worth a standing check.
    ///
    /// Scanned in the shipping half only. A fixture reproducing the game's own aligned output is a
    /// legitimate reason to hold a run of spaces, and every one in this crate is in a test.
    #[test]
    fn no_message_carries_a_lost_line_continuation() {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for e in std::fs::read_dir(dir).expect("readable").flatten() {
                let p = e.path();
                match p.is_dir() {
                    true if p.file_name().is_some_and(|n| n == "bin") => {}
                    true => walk(&p, out),
                    false if p.extension().is_some_and(|x| x == "rs") => out.push(p),
                    false => {}
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        walk(&root, &mut files);
        assert!(files.len() > 20, "the walk found almost nothing: {}", files.len());

        let mut offenders = Vec::new();
        let mut scanned = 0usize;
        for f in &files {
            let text = std::fs::read_to_string(f).expect("readable");
            let ship = shipping_half(&text);
            for (i, line) in ship.lines().enumerate() {
                // Only a line that *opens* with a literal, which is what a wrapped message looks
                // like. A literal sharing a line with code is an argument, not prose.
                let Some(body) = line.trim_start().strip_prefix('"') else { continue };
                scanned += 1;
                // Counted in characters, not byte offsets: an em-dash is three bytes and this file
                // is full of them, which is a false positive waiting to happen. A run only counts
                // when text stands on both sides of it — trailing space before the newline is the
                // continuation working, not failing.
                let (mut run, mut text_before, mut gap) = (0usize, false, false);
                for c in body.chars() {
                    match c {
                        ' ' if text_before => run += 1,
                        ' ' => {}
                        _ => {
                            gap |= run >= 3;
                            run = 0;
                            text_before = true;
                        }
                    }
                }
                if gap {
                    offenders.push(format!("{}:{}  {}", f.display(), i + 1, line.trim()));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "a message holds a run of spaces, which is an eaten `\\` and it prints:\n{}",
            offenders.join("\n")
        );
        // **The positive control, and it is not decoration.** The first version of this test cut
        // each file at its first `#[cfg(test)]`, which in `act/mod.rs` is line 31 — the declaration
        // of a test module that lives in another file. It therefore read nothing of that file, and
        // passed cleanly with a known offender at line 487 put back on purpose. A floor on how much
        // was actually read is what turned that from a green tick into a failure.
        assert!(
            scanned > 200,
            "the sweep only read {scanned} wrapped messages, so something is cutting the files short"
        );
    }

    /// Everything before the crate's own test module, and **only** that.
    ///
    /// `#[cfg(test)]` appears in two shapes and they are not interchangeable: `mod tests {` opens
    /// the test half, while `mod threshold_tests;` merely points at another file and is followed by
    /// several hundred lines of shipping code. Splitting on the attribute alone treats the second as
    /// the first and throws the file away.
    fn shipping_half(text: &str) -> &str {
        const OPENS: &str = "\n#[cfg(test)]\nmod ";
        let mut from = 0;
        while let Some(rel) = text[from..].find(OPENS) {
            let at = from + rel;
            let line = at + OPENS.len() - "mod ".len();
            let end = text[line..].find('\n').map_or(text.len(), |n| line + n);
            if text[line..end].trim_end().ends_with('{') {
                return &text[..at];
            }
            from = at + 1;
        }
        text
    }
}
