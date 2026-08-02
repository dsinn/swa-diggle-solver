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
//! The one residual failure, a click landing on the *wrong* tile, needs no new mechanism either:
//! selection is a toggle (`wordboard.lua:132`), so clicking the offender deselects it.
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
use crate::win::input::click_at;
use crate::win::window::GameWindow;
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
    /// The on-screen keyboard would not take the letter. Almost always a restricted wildcard whose
    /// pattern does not admit it (`onscreenKeypress`, `rpg.lua:664`), which is a planning error
    /// rather than an input one — and the keyboard is still up, so the caller must clear it.
    LetterRefused { tile: usize, letter: char },
    Win(crate::Error),
}

impl std::fmt::Display for SelectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SelectError::Stuck(i) => write!(f, "tile {i} would not select"),
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
    radius: i32,
}

impl<'a> Board<'a> {
    pub fn new(win: &'a GameWindow, geom: &Geometry) -> Result<Self, crate::Error> {
        let (cw, ch) = win.client_size()?;
        Ok(Board {
            win,
            centres: layout::tile_centres(geom, cw, ch),
            rect: layout::board_rect(geom, cw, ch),
            keyboard: crate::typist::wildcard::region(cw, ch),
            // 55% of a tile: inside the face, so a small mapping error still samples the right tile
            // and a large one samples the gap.
            radius: (layout::tile_radius(cw, ch) * 0.55).round() as i32,
        })
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
    pub fn select_word(&self, plan: &[(usize, Option<char>)]) -> Result<(), SelectError> {
        let baseline = self.read()?;

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
        std::thread::sleep(Duration::from_millis(250));
        let restless: Vec<usize> = {
            let second = self.read()?;
            baseline
                .iter()
                .zip(second.iter())
                .enumerate()
                .filter(|(_, (a, b))| (*a - *b).abs() > CHANGED)
                .map(|(i, _)| i)
                .collect()
        };

        let mut want: Vec<usize> = Vec::with_capacity(plan.len());

        for &(i, wildcard) in plan {
            if let Some(letter) = wildcard {
                // A wildcard runs its own confirmed sequence and rejoins here with the board showing
                // exactly `want` plus this tile, so the rest of the word proceeds against the same
                // baseline as if an ordinary click had happened.
                self.place_wildcard(i, letter, &baseline, &want, &restless)?;
                want.push(i);
                continue;
            }
            let mut ok = false;
            for _ in 0..CLICK_ATTEMPTS {
                let changed = self.click_and_confirm(i, &baseline)?;
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

    fn flat(w: i32, h: i32, v: u8) -> Frame {
        Frame { width: w, height: h, bgra: vec![v; (w * h * 4) as usize] }
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
