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
    Win(crate::Error),
}

impl std::fmt::Display for SelectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SelectError::Stuck(i) => write!(f, "tile {i} would not select"),
            SelectError::Stray { wanted, got } => {
                write!(f, "clicking tile {wanted} also changed {got:?}")
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

/// Drives tile selection on a live board.
pub struct Board<'a> {
    win: &'a GameWindow,
    /// Tile centres in client coordinates, indexed as the board dump is.
    centres: Vec<(i32, i32)>,
    /// The rectangle the cheap capture reads, and the offset tile coordinates need.
    rect: (i32, i32, i32, i32),
    radius: i32,
}

impl<'a> Board<'a> {
    pub fn new(win: &'a GameWindow, geom: &Geometry) -> Result<Self, crate::Error> {
        let (cw, ch) = win.client_size()?;
        Ok(Board {
            win,
            centres: layout::tile_centres(geom, cw, ch),
            rect: layout::board_rect(geom, cw, ch),
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

    /// Puts a word on the board, confirming every tile before moving to the next.
    ///
    /// `plan` is dump indices in **word order**. The baseline is taken once, before anything is
    /// clicked, so "selected" always means "differs from the untouched board" — comparing against
    /// the previous step instead would let a missed click look like a successful one.
    pub fn select_word(&self, plan: &[usize]) -> Result<(), SelectError> {
        let baseline = self.read()?;
        let mut want: Vec<usize> = Vec::with_capacity(plan.len());

        for &i in plan {
            let mut ok = false;
            for _ in 0..CLICK_ATTEMPTS {
                let changed = self.click_and_confirm(i, &baseline)?;
                // Everything selected must be something we asked for. A stray selection cannot be
                // repaired by clicking more, so it stops the word rather than corrupting it.
                let stray: Vec<usize> =
                    changed.iter().copied().filter(|j| !want.contains(j) && *j != i).collect();
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
