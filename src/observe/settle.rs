use crate::win::capture::{capture_window, Frame, Region};
use crate::win::window::GameWindow;
use std::time::{Duration, Instant};

pub const FULL: Region = Region { nx: 0.0, ny: 0.0, nw: 1.0, nh: 1.0 };
/// A real screen transition must clear both the ambient floor by this factor...
pub const REACT_MULTIPLE: f64 = 4.0;
/// ...and this absolute fraction of changed pixels.
pub const REACT_FLOOR: f64 = 0.02;

/// Measures how much THIS screen changes on its own, with no input. Must be
/// re-sampled after every transition: ambient animation differs per screen, and a
/// stale floor is what made Spike 4 misread 20 identical frames as 20 transitions.
pub fn sample_noise_floor(
    win: &GameWindow, samples: usize, gap: Duration,
) -> Result<f64, crate::Error> {
    let mut prev = capture_window(win)?;
    let mut worst = 0.0f64;
    for _ in 0..samples {
        std::thread::sleep(gap);
        let next = capture_window(win)?;
        worst = worst.max(prev.diff_fraction(&next, FULL));
        prev = next;
    }
    Ok(worst)
}

/// Waits until consecutive frames stop differing by more than the floor, i.e. any
/// transition animation has finished. Returns the settled frame.
pub fn wait_for_quiescence(
    win: &GameWindow, floor: f64, timeout: Duration,
) -> Result<Frame, crate::Error> {
    let start = Instant::now();
    let mut prev = capture_window(win)?;
    loop {
        std::thread::sleep(Duration::from_millis(200));
        let next = capture_window(win)?;
        if prev.diff_fraction(&next, FULL) <= floor.max(0.001) {
            return Ok(next);
        }
        if start.elapsed() >= timeout {
            return Ok(next);
        }
        prev = next;
    }
}

/// Did an action actually change the screen, as opposed to ambient animation?
pub fn reacted(before: &Frame, after: &Frame, floor: f64) -> bool {
    before.diff_fraction(after, FULL) > (floor * REACT_MULTIPLE).max(REACT_FLOOR)
}
