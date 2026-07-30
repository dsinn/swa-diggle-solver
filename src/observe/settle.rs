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
    // Never sample during a transition — a floor measured mid-animation is inflated
    // and will mask real reactions (Task 24, step 00).
    let _ = wait_for_quiescence(win, 0.02, Duration::from_secs(5))?;
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

/// Detects the end of a **global translation** — the walk to a new enemy — from a series of
/// consecutive frame-diff fractions.
///
/// Waiting for the frame to go still does not work here and never will: characters idle-animate and
/// parts of the background animate independently, so the diff never reaches zero. What distinguishes
/// the walk is *how much* of the frame moves — the entire background translates, so the diff is
/// large; afterwards only local animation remains, so it drops to a small residue.
///
/// The threshold is therefore **relative to the motion actually observed**, not an absolute number.
/// A fixed cutoff would need retuning per scene, per parallax, and per enemy; this self-calibrates
/// from the peak of the current transition. It also refuses to declare success on a transition it
/// never saw start, which is what stops it from reporting "settled" during the pause before the walk
/// begins.
pub struct TranslationWatch {
    peak: f64,
    quiet: usize,
    saw_motion: bool,
}

/// A diff must exceed this for a global translation to be considered underway at all. Ambient
/// character animation moves a few percent of the frame; a translating background moves most of it.
const MOTION_PEAK: f64 = 0.25;
/// Settled once the diff falls to this fraction of the peak.
const SETTLE_RATIO: f64 = 0.25;
/// Consecutive quiet samples required, so one lucky frame does not end the wait early.
const QUIET_SAMPLES: usize = 2;

impl Default for TranslationWatch {
    fn default() -> Self {
        Self::new()
    }
}

impl TranslationWatch {
    pub fn new() -> Self {
        Self { peak: 0.0, quiet: 0, saw_motion: false }
    }

    /// Feeds the next consecutive frame-diff fraction. Returns true once the translation has ended.
    ///
    /// Before any large motion is seen, quiet samples still count — so a call made after the walk
    /// has already finished settles promptly instead of hanging forever waiting for a transition
    /// that is over.
    pub fn push(&mut self, diff: f64) -> bool {
        if diff > self.peak {
            self.peak = diff;
        }
        if self.peak >= MOTION_PEAK {
            self.saw_motion = true;
        }
        let threshold =
            if self.saw_motion { self.peak * SETTLE_RATIO } else { MOTION_PEAK * SETTLE_RATIO };
        if diff <= threshold {
            self.quiet += 1;
        } else {
            self.quiet = 0;
        }
        self.quiet >= QUIET_SAMPLES
    }

    /// Whether a large translation was ever observed. Distinguishes "the walk finished" from
    /// "there was no walk" — the same distinction that, collapsed, has produced false verdicts
    /// elsewhere in this project.
    pub fn saw_motion(&self) -> bool {
        self.saw_motion
    }
}

/// Did an action actually change the screen, as opposed to ambient animation?
pub fn reacted(before: &Frame, after: &Frame, floor: f64) -> bool {
    before.diff_fraction(after, FULL) > (floor * REACT_MULTIPLE).max(REACT_FLOOR)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds a series and reports the index at which it first settles.
    fn settle_at(series: &[f64]) -> Option<usize> {
        let mut w = TranslationWatch::new();
        series.iter().position(|d| w.push(*d))
    }

    #[test]
    fn waits_through_a_translation_then_settles_on_the_residue() {
        // The walk: most of the frame moving, decaying to ambient character animation. It must not
        // settle while the background is still translating.
        let series = [0.05, 0.62, 0.71, 0.68, 0.40, 0.12, 0.06, 0.05];
        let at = settle_at(&series).expect("must settle once the walk ends");
        assert!(at >= 6, "settled too early, at index {at}: {series:?}");
    }

    #[test]
    fn ambient_animation_alone_never_looks_like_a_translation() {
        // Characters and background elements animate constantly. That is not a walk, and the
        // watcher must not claim it saw one.
        let mut w = TranslationWatch::new();
        for d in [0.04, 0.07, 0.05, 0.06, 0.05] {
            w.push(d);
        }
        assert!(!w.saw_motion(), "ambient animation must not register as a translation");
    }

    #[test]
    fn settles_promptly_when_there_was_no_walk_to_wait_for() {
        // Called when the screen is already quiet -- e.g. the second word of a fight, where no new
        // enemy walked in. Hanging here would stall the whole combat loop.
        assert_eq!(settle_at(&[0.03, 0.03]), Some(1));
    }

    #[test]
    fn a_single_quiet_frame_does_not_end_the_wait() {
        // Mid-walk the diff can dip for one sample; ending there would type into a moving screen.
        let mut w = TranslationWatch::new();
        assert!(!w.push(0.70));
        assert!(!w.push(0.05), "one quiet sample is not enough");
        assert!(w.push(0.05), "two consecutive quiet samples settle it");
    }

    #[test]
    fn the_threshold_scales_with_the_observed_peak() {
        // A gentler transition (a shorter walk) settles at a proportionally lower residue, which is
        // the point of calibrating from the peak instead of hardcoding a cutoff.
        let mut gentle = TranslationWatch::new();
        gentle.push(0.30);
        assert!(!gentle.push(0.10), "0.10 is above 25% of a 0.30 peak");
        let mut strong = TranslationWatch::new();
        strong.push(0.80);
        strong.push(0.10);
        assert!(strong.push(0.10), "0.10 is below 25% of a 0.80 peak");
    }
}
