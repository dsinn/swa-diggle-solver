//! Playing a combat turn: choose a word, put it on the board, submit it.
//!
//! ## The selection contract
//!
//! Every click is confirmed before the next is issued. That is the whole design, and it is what
//! makes the rest simple: a gap never opens, so there is nothing to un-do. No `Backspace`, no prefix
//! arithmetic, no reasoning about ordering — even though ordering genuinely matters, since
//! `wordTiles` is a sequence and `getWord()` concatenates in selection order (`wordboard.lua:273`).
//!
//! It costs about a second per word against ~350 ms for fire-and-forget, because the game's
//! selection animation takes 83 ms median (143 ms worst) to move a tile far enough to read — that is
//! the game, not our instrument: making the capture 6× cheaper barely changed it.
//!
//! ## When a word cannot be placed
//!
//! A word failing is ordinary, and it does not end the fight. The game ends a run when no word can
//! be **made** — not when one cannot be typed — and the board keeps shrinking under exploding tiles
//! and the zero-health last stand, so the turns where a stray is most likely are exactly the turns
//! worth playing. So a failure puts the board back rather than giving up on it.
//!
//! Putting it back is `backspace`, pressed until the **word bar** reads empty — see
//! [`Board::clear_selection`], which records why the one-press `clearWord` is not usable, and why
//! the bar rather than the board answers this. Clicking the offender back off would also work,
//! since selection is a toggle (`wordboard.lua:132`) — but that needs to know *what* is selected,
//! and after a stray that is precisely what is in doubt.
//! [`SelectFailure`] reports whether the clear was confirmed, because retrying onto a half-selected
//! board submits a different word than the one that was scored.
//!
//! ## Reading the board
//!
//! Selected tiles darken substantially. The check is a mean-luminance comparison against a baseline
//! taken before the word is built, over a box inside each tile's face — 12.5 µs per tile, against
//! 4.4 ms for the `BitBlt` that feeds it. The capture dominates so completely that there is no point
//! making the check cheaper; the only lever is taking fewer captures, and per-click verification
//! deliberately spends that lever on certainty.

use crate::geometry::Geometry;
use crate::layout;
use crate::win::capture::{capture_client_rect, Frame};
use crate::win::input::{click_at, Input, PostMessageInput, SC_BACK, VK_BACK};
use crate::win::window::GameWindow;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Luminance change that means a tile changed state.
///
/// Measured selections move 42–141; frame-to-frame noise from torch flicker and idle animation stays
/// under 8. Twelve sits clear of the noise with an order of magnitude of headroom on the signal.
pub const CHANGED: f64 = 12.0;

/// Luminance below which a tile position holds no tile at all — bare backboard.
///
/// Measured on a real half-filled board: empty slots read **17.0–21.8**, occupied ones **58.2–161.1**,
/// where the 58.2 was an already-*selected* tile (selection darkens it, so that is the hardest case
/// to call occupied). Forty sits in a 2.7× gap between the two populations.
pub const OCCUPIED: f64 = 40.0;

/// How often to re-capture while waiting for a click to show. The capture floor is 4.4 ms, but
/// polling that hard is near-continuous GDI work for no measurable latency gain.
pub const POLL: Duration = Duration::from_millis(15);

/// How long to wait for one click to become visible. Worst measured is 143 ms; this is ~4× that.
pub const CLICK_TIMEOUT: Duration = Duration::from_millis(600);

/// Attempts per tile before giving up on the word.
pub const CLICK_ATTEMPTS: usize = 3;

/// Gap between the two reads that learn which tiles move on their own.
pub const RESTLESS_SAMPLE: Duration = Duration::from_millis(250);

/// How long a `backspace` has to show on the board before the next one is sent.
///
/// Deselection is the same animation as selection, whose worst measured is 143 ms.
pub const CLEAR_SETTLE: Duration = Duration::from_millis(180);

/// Backspaces before declaring the board unfit to build another word on.
///
/// One per letter, so this has to clear the longest word the search will ever place plus the
/// two-letter tiles in it, with room for a press lost to focus. Well past that: the cost of the
/// ceiling being generous is time on a board that is already failing, and the cost of it being tight
/// is a fight abandoned with a word still on the board.
pub const CLEAR_PRESSES: usize = 32;

/// Mean luminance of a box centred on a point, clipped to the frame.
pub fn luma(frame: &Frame, cx: i32, cy: i32, radius: i32) -> f64 {
    let mut sum = 0f64;
    let mut n = 0f64;
    for y in (cy - radius).max(0)..=(cy + radius).min(frame.height - 1) {
        for x in (cx - radius).max(0)..=(cx + radius).min(frame.width - 1) {
            let i = ((y * frame.width + x) * 4) as usize;
            sum += 0.114 * frame.bgra[i] as f64
                + 0.587 * frame.bgra[i + 1] as f64
                + 0.299 * frame.bgra[i + 2] as f64;
            n += 1.0;
        }
    }
    if n == 0.0 {
        0.0
    } else {
        sum / n
    }
}

/// Spread of luminance across a box — the brightest pixel minus the darkest.
///
/// Where [`luma`] asks how bright something is, this asks how *structured* it is. A tile has a lit
/// face, an engraved letter and a shadowed border, so its spread is large; sky, sea and bare
/// backboard are smooth whatever their brightness. That difference is what tells a board from a
/// scene standing where a board will be.
pub fn luma_range(frame: &Frame, cx: i32, cy: i32, radius: i32) -> f64 {
    let (mut lo, mut hi) = (f64::MAX, f64::MIN);
    for y in (cy - radius).max(0)..=(cy + radius).min(frame.height - 1) {
        for x in (cx - radius).max(0)..=(cx + radius).min(frame.width - 1) {
            let v = luma(frame, x, y, 0);
            lo = lo.min(v);
            hi = hi.max(v);
        }
    }
    if lo > hi {
        0.0
    } else {
        hi - lo
    }
}

/// Vertical luminance range that makes a column of the word bar "busy" rather than bare plank.
///
/// Measured, not chosen — see [`bar_busy_columns`] for the figures.
pub const BAR_EDGE: f64 = 60.0;

/// How much of the word bar holds tiles, as a count of occupied pixel columns.
///
/// **A range, not a brightness.** The plank behind the bar is a smooth gradient, so top-to-bottom it
/// barely varies; a tile spans the full height of the strip with a bright face, a dark letter and
/// dark borders, so its columns vary enormously. That makes the measure indifferent to the scene
/// getting brighter or darker, which is exactly the failure mode being replaced — a mean-luminance
/// check that an animation could move.
///
/// The number is not a tile count and is not meant to be: tiles re-centre and shrink as the word
/// grows (`wordboard.lua:210`), and the drawn width eases toward its target over several frames
/// (`:702`), so any given reading is somewhere between two tile counts. What holds is that it is
/// **zero for an empty bar and strictly larger for each tile added**, which is all a caller needs to
/// ask "did that click register?".
pub fn bar_busy_columns(frame: &Frame) -> usize {
    (0..frame.width)
        .filter(|&x| {
            let (mut lo, mut hi) = (f64::MAX, f64::MIN);
            for y in 0..frame.height {
                let v = luma(frame, x, y, 0);
                lo = lo.min(v);
                hi = hi.max(v);
            }
            hi - lo > BAR_EDGE
        })
        .count()
}

/// Which tiles differ from the baseline — i.e. which are currently selected.
pub fn selected(frame: &Frame, tiles: &[(i32, i32)], baseline: &[f64], radius: i32) -> Vec<bool> {
    tiles
        .iter()
        .zip(baseline)
        .map(|(&(x, y), b)| (luma(frame, x, y, radius) - b).abs() > CHANGED)
        .collect()
}

/// What went wrong while putting a word on the board.
#[derive(Debug)]
pub enum SelectError {
    /// A tile would not select after every attempt. Carries the dump index.
    Stuck(usize),
    /// A click landed somewhere unplanned; the board is not in a state we can reason about.
    Stray { wanted: usize, got: Vec<usize> },
    /// Something was already selected before the word started. Carries the bar reading.
    WordBarNotEmpty(usize),
    /// The on-screen keyboard would not take the letter. Almost always a restricted wildcard whose
    /// pattern does not admit it (`onscreenKeypress`, `rpg.lua:664`), which is a planning error
    /// rather than an input one — and the keyboard is still up, so the caller must clear it.
    LetterRefused { tile: usize, letter: char },
    Win(crate::Error),
}


/// A word that could not be placed, and whether the board was left fit to try another one.
///
/// The distinction is the whole point of reporting it. A word failing is ordinary — a stray reading,
/// a click into an animation — and the fight is not over, because the game only ends a run when no
/// word can be *made*, not when one cannot be typed. But a retry is only safe on a board with
/// nothing selected: build a second word on top of a half-finished first and the submission is a
/// different word than the one that was scored.
#[derive(Debug)]
pub struct SelectFailure {
    pub error: SelectError,
    /// The selection was cleared and confirmed gone, so the next word starts from a clean board.
    pub board_is_clean: bool,
}

impl std::fmt::Display for SelectFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.board_is_clean {
            write!(f, "{}", self.error)
        } else {
            write!(f, "{} (and the board would not clear)", self.error)
        }
    }
}

impl std::fmt::Display for SelectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SelectError::Stuck(i) => write!(f, "tile {i} would not select"),
            SelectError::WordBarNotEmpty(n) => {
                write!(f, "the word bar already held a word ({n} columns) before this one started")
            }
            SelectError::Stray { wanted, got } => {
                write!(f, "clicking tile {wanted} also changed {got:?}")
            }
            SelectError::LetterRefused { tile, letter } => {
                write!(f, "wildcard {tile} would not take {letter:?}; keyboard still open")
            }
            SelectError::Win(e) => write!(f, "{e}"),
        }
    }
}

impl From<crate::Error> for SelectError {
    fn from(e: crate::Error) -> Self {
        SelectError::Win(e)
    }
}

/// How long the board takes to fade fully out or fully in.
///
/// Not a guess. `showKeyboard` calls `tileboard.hide'fade'`, which only sets `alphaHide`
/// (`tileboard.lua:2094-2096`); the movement is in `update` (`tileboard.lua:1678-1682`):
///
/// ```lua
/// if alphaHide and tileboardData.colour[4] ~= 0 then
///     tileboardData.colour[4] = math.max(0, tileboardData.colour[4]-delta*3)
/// elseif not alphaHide and tileboardData.colour[4] ~= 1 then
///     tileboardData.colour[4] = math.min(1, tileboardData.colour[4]+delta*3)
/// ```
///
/// Alpha travels 0..1 at 3 per second, scaled by `delta`, so the fade is **1/3 second** either way
/// and does not vary with frame rate. In practice the check trips sooner than this — the region stops
/// resembling its reference well before alpha reaches its endpoint.
pub const FADE: Duration = Duration::from_millis(334);

/// How long to wait for the on-screen keyboard to appear or go.
///
/// Four fades. A margin for a frame hitch or a slow poll, not for uncertainty about the duration —
/// and small enough that a genuinely refused letter is reported in seconds instead of tens of them,
/// since [`LETTER_ATTEMPTS`] pays this timeout on each try.
pub const KEYBOARD_TIMEOUT: Duration = Duration::from_millis(4 * 334);

/// Attempts at sending the letter before giving up on the word.
pub const LETTER_ATTEMPTS: usize = 3;

/// How long to let the board come back to the selection we expect after the keyboard closes.
///
/// A ceiling on the poll below, not a duration to wait out: the board is read until it shows exactly
/// the tiles we have selected and nothing else, and this only decides when to give up. Four fades,
/// same margin as [`KEYBOARD_TIMEOUT`], because nothing is being re-dealt — the same tiles are simply
/// becoming visible again.
pub const BOARD_RETURN_TIMEOUT: Duration = Duration::from_millis(4 * 334);

/// How the board compared with the selection we believe we made.
#[derive(Debug, PartialEq, Eq)]
enum Settled {
    /// Exactly the expected tiles are selected, and nothing else.
    Yes,
    /// Tiles we asked for that never appeared: the action did not take.
    Missing(Vec<usize>),
    /// Tiles selected that we never asked for.
    Unexpected(Vec<usize>),
}


/// Compares what the board shows as selected against what we asked for.
///
/// Split out from the poll so the judgement can be tested without a window — the loop around it is
/// only a clock.
fn compare_selection(changed: &[usize], expected: &[usize], restless: &[usize]) -> Settled {
    let missing: Vec<usize> = expected.iter().copied().filter(|j| !changed.contains(j)).collect();
    if !missing.is_empty() {
        // A tile we asked for and cannot see is the more useful complaint: it means the action did
        // not take, where an extra tile means it took somewhere else too.
        return Settled::Missing(missing);
    }
    let unexpected: Vec<usize> = changed
        .iter()
        .copied()
        .filter(|j| !expected.contains(j) && !restless.contains(j))
        .collect();
    if unexpected.is_empty() {
        Settled::Yes
    } else {
        Settled::Unexpected(unexpected)
    }
}

/// Drives tile selection on a live board.
pub struct Board<'a> {
    win: &'a GameWindow,
    /// Tile centres in client coordinates, indexed as the board dump is.
    centres: Vec<(i32, i32)>,
    /// The rectangle the cheap capture reads, and the offset tile coordinates need.
    rect: (i32, i32, i32, i32),
    /// The region watched to tell the on-screen keyboard from the tile board.
    keyboard: (i32, i32, i32, i32),
    /// The strip that shows the word as it is built — the one place a click can be confirmed
    /// against the game's own state instead of against a picture of the board.
    bar: (i32, i32, i32, i32),
    radius: i32,
    /// Where to photograph each click, when the run has asked for it. See
    /// [`crate::config::Config::debug_click_frames`].
    click_frames: Option<PathBuf>,
    /// Serial for those photographs, so their filenames sort into the order they were taken.
    shots: std::cell::Cell<usize>,
}

/// Names one debug frame: sequence first, so a directory listing is a timeline.
///
/// `seq` counts every photograph this board has taken, `tile` is the dump index being clicked, and
/// `attempt` is which of [`CLICK_ATTEMPTS`] this is — a stray on the first try and a stray on the
/// third are different stories, and without the attempt number the pair cannot be told apart.
pub fn click_frame_name(seq: usize, tile: usize, attempt: usize, when: &str) -> String {
    format!("click-{seq:03}-tile{tile:02}-try{attempt}-{when}.png")
}

impl<'a> Board<'a> {
    pub fn new(win: &'a GameWindow, geom: &Geometry) -> Result<Self, crate::Error> {
        let (cw, ch) = win.client_size()?;
        Ok(Board {
            win,
            centres: layout::tile_centres(geom, cw, ch),
            rect: layout::board_rect(geom, cw, ch),
            keyboard: crate::typist::wildcard::region(cw, ch),
            bar: layout::word_bar(cw, ch),
            // 55% of a tile: inside the face, so a small mapping error still samples the right tile
            // and a large one samples the gap.
            radius: (layout::tile_radius(cw, ch) * 0.55).round() as i32,
            click_frames: None,
            shots: std::cell::Cell::new(0),
        })
    }

    /// How much of the word bar is occupied right now. Zero means nothing is selected.
    pub fn bar_busy(&self) -> Result<usize, crate::Error> {
        let (x, y, w, h) = self.bar;
        Ok(bar_busy_columns(&capture_client_rect(self.win, x, y, w, h)?))
    }

    /// Waits for the word to grow by at least one tile, and says whether it did.
    ///
    /// **This is what "the click registered" means.** A tile going dark does not mean it: while the
    /// scene animates, tile luminance moves on its own, and on 2026-08-11 a resumed fight had its
    /// first two clicks discarded by a game not yet taking input while every one of them passed the
    /// luminance check. The bar is drawn from `wordTiles` alone (`wordboard.lua:686-691`), so it
    /// cannot be moved by an animation, only by a selection the game accepted.
    ///
    /// Growth rather than an exact count, because the drawn width eases toward its target over
    /// several frames (`wordboard.lua:702`) — a reading taken mid-ease sits between two tile counts.
    /// "More than before" is the strongest claim the measure supports, and it is the one needed.
    fn wait_for_bar_growth(&self, from: usize, timeout: Duration) -> Result<bool, crate::Error> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.bar_busy()? > from {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(POLL);
        }
    }

    /// Turns on the per-click photographs, writing them into `dir`.
    ///
    /// `None` leaves them off, so a caller can pass a config flag straight through without branching.
    pub fn with_click_frames(mut self, dir: Option<PathBuf>) -> Self {
        self.click_frames = dir;
        self
    }

    /// Photographs the whole window, if this board was asked to.
    ///
    /// **The whole window, not [`Board::rect`].** The board rect is what the click loop samples, and
    /// if the answer were in there the luminance readings would already have given it. The questions
    /// these frames exist to settle are outside it: did the cursor go where it was sent, is a
    /// keyboard or dialog covering the board, is an overlay being drawn across the tiles.
    ///
    /// Failures are swallowed. This is a diagnostic; a run must not die because a disk write failed,
    /// and the click it is watching has already happened either way.
    fn shoot(&self, tile: usize, attempt: usize, when: &str) {
        let Some(dir) = self.click_frames.as_ref() else { return };
        let seq = self.shots.get() + 1;
        self.shots.set(seq);
        if let Ok(f) = crate::win::capture::capture_window(self.win) {
            let _ = f.write_png(&dir.join(click_frame_name(seq, tile, attempt, when)));
        }
    }

    pub fn tile_count(&self) -> usize {
        self.centres.len()
    }

    /// Captures the board and reports each tile's luminance.
    pub fn read(&self) -> Result<Vec<f64>, crate::Error> {
        let (x, y, w, h) = self.rect;
        let frame = capture_client_rect(self.win, x, y, w, h)?;
        Ok(self
            .centres
            .iter()
            .map(|&(cx, cy)| luma(&frame, cx - x, cy - y, self.radius))
            .collect())
    }

    /// Which tiles currently differ from `baseline`.
    pub fn selected_now(&self, baseline: &[f64]) -> Result<Vec<bool>, crate::Error> {
        let now = self.read()?;
        Ok(now
            .iter()
            .zip(baseline)
            .map(|(a, b)| (a - b).abs() > CHANGED)
            .collect())
    }

    /// Which tile positions actually hold a tile.
    pub fn occupancy(&self) -> Result<Vec<bool>, crate::Error> {
        Ok(self.read()?.into_iter().map(|l| l > OCCUPIED).collect())
    }

    /// Blocks until every slot holds a tile **and** the board has stopped moving.
    ///
    /// Two separate conditions, and the first one is why the first version of this failed.
    ///
    /// **Stillness alone is not readiness: an empty board is perfectly static.** `preload` pushes
    /// every saved letter into `letterQueue` (`tileboard.lua:2520-2523`) and the board fills from
    /// that queue over time, so on resume the tiles are still dropping in. A quiescence check that
    /// samples tile centres sees dark, unchanging backboard and declares victory. The loop then
    /// clicked an empty slot — nothing happened — while later arrivals registered as tiles
    /// "changing", and a correct click was reported as hitting six wrong tiles.
    ///
    /// **And `turnState == PlayerTurn` does not imply either condition.** On a normal transition it
    /// implies both, since `PlayerPreTurn` only ends at `tileboard.boardIsStatic()`
    /// (`rpgview.lua:1500-1502`) — but resuming a save restores the state directly, skipping the
    /// gate. Same shape as a resumed fight printing no board dump: a save restores *state* without
    /// replaying the transitions that establish it. Never trust an entry gate you did not see fire.
    pub fn wait_until_ready(&self, timeout: Duration) -> Result<bool, crate::Error> {
        const QUIET: f64 = 4.0;
        const NEEDED: usize = 4;
        let deadline = Instant::now() + timeout;
        let mut previous = self.read()?;
        let mut quiet = 0usize;
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
            let now = self.read()?;
            let worst = now
                .iter()
                .zip(&previous)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f64, f64::max);
            let full = now.iter().all(|l| *l > OCCUPIED);
            previous = now;
            // Consecutive, not cumulative: a tile can pause mid-bounce, so one still frame is not
            // stillness. And an empty slot resets the count however still it is.
            quiet = if worst < QUIET && full { quiet + 1 } else { 0 };
            if quiet >= NEEDED {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn click(&self, i: usize) -> Result<(), crate::Error> {
        let (cx, cy) = self.centres[i];
        let (sx, sy) = self.win.client_to_screen(cx, cy)?;
        click_at(sx, sy)
    }

    /// Clicks one tile and waits until **that** tile changes.
    ///
    /// Returns the tiles that actually changed. Normally just `[i]`; anything else means the click
    /// landed somewhere unexpected and the caller must not keep building on it.
    fn click_and_confirm(&self, i: usize, baseline: &[f64]) -> Result<Vec<usize>, crate::Error> {
        self.click(i)?;
        let deadline = Instant::now() + CLICK_TIMEOUT;
        loop {
            std::thread::sleep(POLL);
            let changed: Vec<usize> = self
                .selected_now(baseline)?
                .into_iter()
                .enumerate()
                .filter(|(_, c)| *c)
                .map(|(j, _)| j)
                .collect();
            if changed.contains(&i) || Instant::now() >= deadline {
                return Ok(changed);
            }
        }
    }

    /// Captures the region that tells the on-screen keyboard from the tile board.
    fn read_keyboard_region(&self) -> Result<Frame, crate::Error> {
        let (x, y, w, h) = self.keyboard;
        capture_client_rect(self.win, x, y, w, h)
    }

    /// Waits until the keyboard region is, or is no longer, showing what `reference` showed.
    ///
    /// One primitive for both directions because they are the same measurement read two ways: the
    /// reference is always the tile board, captured moments earlier in the same window, so "near it"
    /// means the board is up and "far from it" means the keyboard is. See
    /// [`crate::typist::wildcard`] for why this is a self-comparison and not a stored template.
    ///
    /// Returns whether the wanted state arrived before the deadline.
    fn wait_for_board(
        &self, reference: &Frame, want_board: bool, timeout: Duration,
    ) -> Result<bool, crate::Error> {
        let deadline = Instant::now() + timeout;
        loop {
            let now = self.read_keyboard_region()?;
            let score = crate::observe::template::inliers_between(&now, reference);
            if (score >= crate::typist::wildcard::SAME) == want_board {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(POLL);
        }
    }

    /// Waits for the board to show exactly `expected` as selected, and reports what else it shows.
    ///
    /// The fade has a known length ([`FADE`]), and waiting it out would be enough today. It is the
    /// wrong thing to rely on: the duration is a constant in the game's `update`, an animation could
    /// be added ahead of it, and a slow frame extends it. So the fixed wait is only a floor — the
    /// exit condition is the board *agreeing with us*, which is the thing actually needed and stays
    /// true however the animation is changed.
    ///
    /// `restless` tiles are excluded, as they are everywhere else: an exploding tile pulses on its
    /// own and would never let the set settle.
    ///
    fn settle_to(
        &self, baseline: &[f64], expected: &[usize], restless: &[usize], timeout: Duration,
    ) -> Result<Settled, crate::Error> {
        std::thread::sleep(FADE);
        let deadline = Instant::now() + timeout;
        loop {
            let now = self.selected_now(baseline)?;
            let changed: Vec<usize> =
                now.iter().enumerate().filter(|(_, c)| **c).map(|(j, _)| j).collect();
            let verdict = compare_selection(&changed, expected, restless);
            if verdict == Settled::Yes || Instant::now() >= deadline {
                return Ok(verdict);
            }
            std::thread::sleep(POLL);
        }
    }

    /// Selects a wildcard tile and gives it a letter.
    ///
    /// The ordinary confirm-by-luminance cannot be used for the click: selecting a wildcard fades
    /// the whole board out, so *every* tile changes and the stray check would fire on all of them.
    /// The keyboard appearing is the confirmation instead — a positive signal for the same event,
    /// and a more specific one, since only a wildcard produces it.
    ///
    /// `baseline` is still the pre-word board, and the tile is checked against it at the end: the
    /// board comes back unchanged apart from this tile, which is now both selected and lettered.
    fn place_wildcard(
        &self, i: usize, letter: char, baseline: &[f64], want: &[usize], restless: &[usize],
    ) -> Result<(), SelectError> {
        // Everything selected so far, plus the tile this step is for.
        let expected: Vec<usize> = want.iter().copied().chain(std::iter::once(i)).collect();
        let verify = |s: Settled| match s {
            Settled::Yes => Ok(()),
            Settled::Missing(_) => Err(SelectError::Stuck(i)),
            Settled::Unexpected(got) => Err(SelectError::Stray { wanted: i, got }),
        };

        // The reference must be captured while the board is up, before anything is clicked.
        let board = self.read_keyboard_region()?;
        self.click(i)?;

        if !self.wait_for_board(&board, false, KEYBOARD_TIMEOUT)? {
            // No keyboard. Either the click missed, or this player has `submitWildcardRegex` and the
            // tile selects like any other (`wordboard.lua:140-143`). The board itself says which.
            return verify(self.settle_to(baseline, &expected, restless, BOARD_RETURN_TIMEOUT)?);
        }

        for _ in 0..LETTER_ATTEMPTS {
            crate::win::input::type_text_injected(
                &letter.to_string(),
                Duration::from_millis(40),
            )?;
            if self.wait_for_board(&board, true, KEYBOARD_TIMEOUT)? {
                // `hideKeyboard` calls `tileboard.unhide()` (`rpg.lua:704`), so the board fades back
                // in rather than reappearing. Rejoining the word before it agrees with us would
                // compare a half-faded board against the baseline, read every tile as changed, and
                // abort on a stray at the *next* click -- a failure that would look like the
                // ordinary click's fault rather than the wildcard's.
                return verify(self.settle_to(baseline, &expected, restless, BOARD_RETURN_TIMEOUT)?);
            }
        }
        Err(SelectError::LetterRefused { tile: i, letter })
    }

    /// Puts a word on the board, confirming every step before moving to the next.
    ///
    /// `plan` is `(dump index, letter to type into it if it is a wildcard)` in **word order**, which
    /// is what [`crate::typist::Typed::steps`] yields. The baseline is taken once, before anything is
    /// clicked, so "selected" always means "differs from the untouched board" — comparing against
    /// the previous step instead would let a missed click look like a successful one.
    pub fn select_word(&self, plan: &[(usize, Option<char>)]) -> Result<(), SelectFailure> {
        let dirty = |error| SelectFailure { error, board_is_clean: false };

        // **Nothing may be selected yet.** The baseline below is only meaningful against an empty
        // board, and a word left over from a previous attempt would be submitted along with this
        // one. The board's own pixels cannot tell us -- a selected tile just reads darker, which is
        // what the baseline was going to define -- but the word bar can, and it reads exactly zero
        // when empty.
        let busy = self.bar_busy().map_err(|e| dirty(e.into()))?;
        if busy > 0 {
            return Err(dirty(SelectError::WordBarNotEmpty(busy)));
        }

        let baseline = self.read().map_err(|e| dirty(e.into()))?;

        // Some tiles change without being touched. An exploding tile counts down with a pulsing red
        // glow, so it differs from any baseline within a frame or two and every later comparison
        // reads it as newly selected -- which aborted a live word with
        // `clicking tile 1 also changed [4]`, where tile 4 was the bomb and tile 1 had selected
        // perfectly well.
        //
        // Measured rather than guessed at: read the board twice, and anything that moved on its own
        // is excluded from stray detection for the rest of the word. The safety property survives,
        // because it never rested on watching other tiles -- a misplaced click still fails, as the
        // tile we asked for does not become selected.
        //
        // **This window was briefly widened to 750ms and is back, and neither the widening nor its
        // reversal was reasoned from the right thing.** The live failure both were aimed at --
        // `clicking tile 0 also changed [9]` -- was the dev using an item mid-word to rescue a run at
        // zero health. `randomPotionOld` (`items/potionsmisc.lua:362-372`) fires three to five random
        // tile effects, and the three tiles it hit are exactly the three that differ between that
        // turn's dump and the save written afterwards: `!` and `I` became wildcards, and an `H` was
        // gilded to silver.
        //
        // So **the stray check was right** -- the board really had changed under it -- and no window
        // length is the answer. A pre-sample cannot catch a change that begins after it, and tile 9
        // was still an ordinary `I` when this one was taken. What the failure actually calls for is
        // re-planning against a board read *again*, which is
        // [`crate::fight::Fight::wait_for_board_change`]'s job.
        //
        // Widening still bought nothing and cost half a second per word, so it stays at 250ms. This
        // sample is for tiles that are *already* moving -- an exploding tile, a countdown -- which is
        // the case it was built for and the one it handles.
        std::thread::sleep(RESTLESS_SAMPLE);
        let restless: Vec<usize> = {
            let second = self.read().map_err(|e| dirty(e.into()))?;
            baseline
                .iter()
                .zip(second.iter())
                .enumerate()
                .filter(|(_, (a, b))| (*a - *b).abs() > CHANGED)
                .map(|(i, _)| i)
                .collect()
        };

        match self.place_all(plan, &baseline, &restless) {
            Ok(()) => Ok(()),
            Err(error) => {
                // The fight is not over because a word could not be typed, so the board is put back
                // to something another word can be built on. Whether that worked is reported rather
                // than assumed — a caller must not retry onto a half-selected board.
                let board_is_clean = self.clear_selection().unwrap_or(false);
                Err(SelectFailure { error, board_is_clean })
            }
        }
    }

    /// Takes letters off the word until nothing reads as selected. `false` if it never got there.
    ///
    /// ## Why not `clearWord`, which would be one press
    ///
    /// `Delete` is bound to `clearWord` (`utils/defaultbinds/keyboard.lua:12`), and it looks like
    /// exactly the primitive this wants — the whole selection gone regardless of what is on it. It is
    /// **modifier-gated**: the same bind layer declares `_mod = { delete = 'rshift' }`, and
    /// `doUserFunctionWithBindsFromSet` (`main.lua:471-480`) requires
    /// `love.keyboard.isDown('rshift')` before it will run the action. Layer 2 binds no `delete` at
    /// all, so a bare press falls through both layers and does nothing.
    ///
    /// `backspace` has no `_mod` entry and layer 1 binds it to `wordboard.backspace()`
    /// (`rpg.lua:221`) on any screen with a word on it. One letter per press instead of one press
    /// full stop — which costs nothing here, because the loop has to re-read the board between
    /// presses either way to know when to stop.
    ///
    /// **Press and check, never press a computed number of times.** A `QU` tile is one selection and
    /// two letters, so the tile count is not the letter count and the difference depends on the word.
    /// Counting is how the murder path ended up pressing `tiles + 2` times and hoping.
    pub fn clear_selection(&self) -> Result<bool, crate::Error> {
        let keys = PostMessageInput::new(*self.win);
        keys.focus();
        for _ in 0..CLEAR_PRESSES {
            // **The bar, not the baseline.** This used to ask whether the tiles still differed from
            // `placed.baseline`, which cannot work in the one case that matters: a word fails to
            // place *because* the baseline was wrong, and then the clear inherits the same wrong
            // reference and can never see an empty board. Thirty-two backspaces later it reported
            // failure, and a failed clear ends the fight — so every run that met a bad baseline died
            // holding a retry it was never allowed to use.
            //
            // `wordboard.getWord()` is '' exactly when `wordTiles` is empty (`wordboard.lua:274`),
            // and the bar is drawn from `wordTiles`, so an empty bar IS an empty selection. No
            // reference frame, nothing to go stale.
            if self.bar_busy()? == 0 {
                return Ok(true);
            }
            keys.press_key(VK_BACK, SC_BACK)?;
            std::thread::sleep(CLEAR_SETTLE);
        }
        Ok(false)
    }

    /// The selection loop itself, split out so [`select_word`](Self::select_word) can clean up after
    /// any way it fails without threading the tidy-up through every early return.
    fn place_all(
        &self, plan: &[(usize, Option<char>)], baseline: &[f64], restless: &[usize],
    ) -> Result<(), SelectError> {
        let mut want: Vec<usize> = Vec::with_capacity(plan.len());

        for &(i, wildcard) in plan {
            if let Some(letter) = wildcard {
                // A wildcard runs its own confirmed sequence and rejoins here with the board showing
                // exactly `want` plus this tile, so the rest of the word proceeds against the same
                // baseline as if an ordinary click had happened.
                self.place_wildcard(i, letter, baseline, &want, restless)?;
                want.push(i);
                continue;
            }
            let mut ok = false;
            for attempt in 1..=CLICK_ATTEMPTS {
                // Either side of the click, and only when asked for. The stray check can say which
                // tile centres changed but never how, and "the click was sent" is not "the click
                // landed" — 2026-08-11 ended on a stray report with no way to tell those apart.
                self.shoot(i, attempt, "before");
                let bar_before = self.bar_busy()?;
                let changed = self.click_and_confirm(i, baseline)?;
                self.shoot(i, attempt, "after");

                // **Did the game take it?** A click that arrives before the scene is ready is
                // discarded in silence (`wordboard.lua:127`), and the tile-luminance check cannot
                // see that: an animation moves the tiles whether or not anything was selected. So a
                // click is only counted once the word itself has grown, and one that did not
                // register is simply thrown again — which is what an early click needs, without
                // anyone having to predict how early is too early.
                if !self.wait_for_bar_growth(bar_before, CLICK_TIMEOUT)? {
                    continue;
                }
                // Everything selected must be something we asked for. A stray selection cannot be
                // repaired by clicking more, so it stops the word rather than corrupting it.
                let stray: Vec<usize> = changed
                    .iter()
                    .copied()
                    .filter(|j| !want.contains(j) && *j != i && !restless.contains(j))
                    .collect();
                if !stray.is_empty() {
                    return Err(SelectError::Stray { wanted: i, got: stray });
                }
                if changed.contains(&i) {
                    ok = true;
                    break;
                }
            }
            if !ok {
                return Err(SelectError::Stuck(i));
            }
            want.push(i);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::capture::Frame;

    #[test]
    fn click_frames_sort_into_the_order_they_were_taken() {
        // A directory listing is how these get read, so the sequence has to lead and has to be
        // zero-padded -- otherwise shot 10 sorts between 1 and 2 and the timeline is a jumble.
        let mut names = vec![
            click_frame_name(10, 3, 1, "before"),
            click_frame_name(2, 15, 2, "after"),
            click_frame_name(1, 3, 1, "before"),
        ];
        names.sort();
        assert_eq!(
            names,
            vec![
                "click-001-tile03-try1-before.png",
                "click-002-tile15-try2-after.png",
                "click-010-tile03-try1-before.png",
            ]
        );
    }

    #[test]
    fn a_before_and_an_after_of_the_same_click_are_different_files() {
        // They differ only in `when`, and if that were dropped from the name the after would
        // overwrite the before -- losing the half that says what the click changed.
        assert_ne!(click_frame_name(1, 4, 1, "before"), click_frame_name(2, 4, 1, "after"));
    }

    fn flat(w: i32, h: i32, v: u8) -> Frame {
        Frame { width: w, height: h, bgra: vec![v; (w * h * 4) as usize] }
    }

    /// A frame whose columns in `busy` run dark-to-light top to bottom, and whose others are flat.
    fn bar_with_tiles(w: i32, h: i32, busy: std::ops::Range<i32>) -> Frame {
        let mut f = flat(w, h, 120);
        for y in 0..h {
            for x in busy.clone() {
                let v = if y < h / 2 { 20 } else { 230 };
                let i = ((y * w + x) * 4) as usize;
                f.bgra[i] = v;
                f.bgra[i + 1] = v;
                f.bgra[i + 2] = v;
            }
        }
        f
    }

    #[test]
    fn brightness_cannot_tell_a_tile_from_open_sky_and_texture_can() {
        // The 2026-08-12 failure in one assertion. A boss intro card leaves the combat HUD up and
        // the board undrawn, so the tile centres sample sky and sea -- brighter than any tile, and
        // perfectly smooth. `OCCUPIED` passed all sixteen slots on a screen with no board on it.
        let sky = flat(64, 64, 200);
        assert!(luma(&sky, 32, 32, 20) > OCCUPIED, "sky is brighter than the occupancy floor");
        assert!(luma_range(&sky, 32, 32, 20) < 1.0, "and it is featureless");

        // A tile: dark engraving on a lit face. Dimmer on average, unmistakable by structure.
        let tile = bar_with_tiles(64, 64, 20..44);
        assert!(luma_range(&tile, 32, 32, 20) > 100.0);
    }

    #[test]
    fn an_empty_word_bar_reads_zero() {
        // The precondition for starting a word rests on this being exactly zero, not merely small.
        assert_eq!(bar_busy_columns(&flat(400, 59, 120)), 0);
    }

    #[test]
    fn the_reading_grows_with_the_word() {
        // Not a tile count -- the drawn width eases (`wordboard.lua:702`) -- so the only property
        // the caller may rely on is that more tiles read as more.
        let one = bar_busy_columns(&bar_with_tiles(400, 59, 100..140));
        let three = bar_busy_columns(&bar_with_tiles(400, 59, 60..180));
        assert!(one > 0, "a tile must register");
        assert!(three > one, "three tiles must read higher than one: {three} vs {one}");
    }

    #[test]
    fn a_scene_getting_brighter_does_not_look_like_a_selection() {
        // The whole reason for measuring a range rather than a brightness. An animation lifting the
        // entire frame is what fooled the luminance check on 2026-08-11.
        assert_eq!(bar_busy_columns(&flat(400, 59, 120)), 0);
        assert_eq!(bar_busy_columns(&flat(400, 59, 200)), 0);
    }

    #[test]
    fn the_word_bar_sits_where_the_game_draws_it() {
        // `buildDrawDataTable(0, 118, 0.5, 0, 0, 1.41525)` (`wordboard.lua:16`) in the same
        // convention as every button: centre = ss*client + offset*size. Height is halved to keep
        // the plank's rails out -- with them in, an empty bar and an eight-letter word both read
        // 1748 of 1888.
        assert_eq!(crate::layout::word_bar(1920, 1080), (16, 137, 1888, 59));
    }

    #[test]
    fn a_board_showing_exactly_what_we_asked_for_has_settled() {
        assert_eq!(compare_selection(&[0, 3, 5], &[0, 3, 5], &[]), Settled::Yes);
        // Order is not part of the question: the board reports positions, not sequence.
        assert_eq!(compare_selection(&[5, 0, 3], &[3, 5, 0], &[]), Settled::Yes);
    }

    #[test]
    fn a_still_fading_board_is_neither_settled_nor_an_error_yet() {
        // Mid-fade every tile reads as changed. That must come back as Unexpected so the poll keeps
        // going -- returning Yes here is the bug that aborts the next click with a stray.
        let all: Vec<usize> = (0..8).collect();
        assert_eq!(compare_selection(&all, &[0, 3], &[]), Settled::Unexpected(vec![1, 2, 4, 5, 6, 7]));
    }

    #[test]
    fn a_tile_we_asked_for_and_cannot_see_outranks_an_extra_one() {
        // Both wrong at once: report the one that says the action did not take.
        assert_eq!(compare_selection(&[0, 9], &[0, 3], &[]), Settled::Missing(vec![3]));
    }

    #[test]
    fn a_restless_tile_never_stops_the_board_settling() {
        // An exploding tile pulses on its own, so it differs from the baseline on every read. Left
        // in, the poll could only ever time out.
        assert_eq!(compare_selection(&[0, 3, 4], &[0, 3], &[4]), Settled::Yes);
        // But it is still allowed to be one of the tiles we selected.
        assert_eq!(compare_selection(&[0, 4], &[0, 4], &[4]), Settled::Yes);
    }



    #[test]
    fn luma_of_a_flat_frame_is_that_value() {
        // 0.114+0.587+0.299 = 1.0, so a uniform BGRA value comes back unchanged.
        let f = flat(40, 40, 200);
        assert!((luma(&f, 20, 20, 5) - 200.0).abs() < 1e-9);
    }

    #[test]
    fn luma_clips_to_the_frame_rather_than_reading_out_of_bounds() {
        // A tile box at the edge of the board rect must not index past the buffer. This is the
        // difference between a slightly noisy reading and a panic mid-fight.
        let f = flat(40, 40, 100);
        assert!((luma(&f, 0, 0, 10) - 100.0).abs() < 1e-9);
        assert!((luma(&f, 39, 39, 10) - 100.0).abs() < 1e-9);
    }

    #[test]
    fn selection_is_a_difference_from_baseline_not_an_absolute() {
        // A dark board and a light board both work; what matters is the change. Absolute thresholds
        // would break the moment the parallax or time-of-day tint differed.
        let tiles = [(10, 10), (30, 10)];
        let dark = flat(40, 40, 40);
        let base_dark = vec![40.0, 40.0];
        assert_eq!(selected(&dark, &tiles, &base_dark, 4), vec![false, false]);

        let mut changed = flat(40, 40, 40);
        for y in 6..15 {
            for x in 6..15 {
                let i = ((y * 40 + x) * 4) as usize;
                changed.bgra[i..i + 4].copy_from_slice(&[200, 200, 200, 255]);
            }
        }
        assert_eq!(selected(&changed, &tiles, &base_dark, 4), vec![true, false]);
    }

    #[test]
    fn the_threshold_sits_between_measured_noise_and_measured_signal() {
        // Live figures: selections moved 42-141 luma, residual drift stayed under 8. The threshold
        // has to separate those two, and this pins that it does rather than leaving it to a comment.
        assert!(CHANGED > 8.0, "must clear measured noise");
        assert!(CHANGED < 42.0, "must trip on the weakest measured selection");
    }

    #[test]
    fn the_click_budget_covers_the_measured_worst_case() {
        // Worst observed click -> visible was 143 ms. A timeout below that would abandon tiles that
        // were about to appear, and retry them -- toggling them back off.
        assert!(CLICK_TIMEOUT.as_millis() >= 143 * 3, "timeout too tight for the measured worst case");
        assert!(POLL.as_millis() >= 10, "polling faster than this is GDI churn for no latency gain");
    }
}
