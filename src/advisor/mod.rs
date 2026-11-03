//! The advisor's library half: everything it decides, kept out of the Win32 binary so it can be
//! tested. See `src/bin/diggle_advisor.rs` for the window and the rules it keeps about the game's
//! files.

pub mod annotate;
pub mod mode;
pub mod palette;
pub mod spells;
pub mod view;
pub mod watch;
