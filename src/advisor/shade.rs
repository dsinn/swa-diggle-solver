//! "Collapse candidates out of combat": the window rolls up to its header and checkboxes while
//! no fight is on, and drops back to the height it had when a fight starts. Pure, so every
//! transition can be tested; the window feeds it heights and times and moves itself to what it
//! says ([`Shade::want`], [`Shade::tick`]).
//!
//! Heights are the window's outer height in pixels. Only the height moves; the top edge stays put.
//!
//! The transitions, all through [`Shade::want`]:
//!
//! | now                | asked to close                  | asked to open                      |
//! |--------------------|---------------------------------|------------------------------------|
//! | open               | remember the height, roll up    | nothing                            |
//! | rolling up         | nothing (follow a new width)    | turn round, back to the height kept|
//! | closed             | nothing (follow a new width)    | drop to the height kept            |
//! | dropping           | turn round, roll up again       | nothing                            |
//!
//! Turning round starts from wherever the window is, so a reversal never jumps. The height kept is
//! the one from before the first roll-up: a fight that starts and ends mid-animation does not
//! replace it with a height halfway down.

use std::time::{Duration, Instant};

/// How long a roll-up or a drop takes.
pub const DURATION: Duration = Duration::from_millis(800);

/// Eases both ways with an elastic overshoot: from 0 at `n = 0` to 1 at `n = 1`, overshooting
/// both ends by about 12%. The user's Lua `math.easeBothElastic`, 2026-11-08, transcribed.
pub fn ease_both_elastic(n: f64) -> f64 {
    const C: f64 = 1.396_263_4; // 2π / 4.5
    if n <= 0.0 {
        0.0
    } else if n >= 1.0 {
        1.0
    } else if n < 0.5 {
        -(2f64.powf(20.0 * n - 10.0) * ((20.0 * n - 11.125) * C).sin()) / 2.0
    } else {
        (2f64.powf(-20.0 * n + 10.0) * ((20.0 * n - 11.125) * C).sin()) / 2.0 + 1.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Open,
    Closed,
    Moving { from: i32, to: i32, start: Instant, closing: bool },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shade {
    phase: Phase,
    /// The height to drop back to: the window's height when it last started rolling up from open.
    restore: i32,
}

impl Default for Shade {
    fn default() -> Self {
        Shade { phase: Phase::Open, restore: 0 }
    }
}

impl Shade {
    /// Fully open and still: the window's height is the user's own, worth saving.
    pub fn is_open(&self) -> bool {
        self.phase == Phase::Open
    }

    /// Fully rolled up and still.
    pub fn is_closed(&self) -> bool {
        self.phase == Phase::Closed
    }

    pub fn is_moving(&self) -> bool {
        matches!(self.phase, Phase::Moving { .. })
    }

    /// Asks for the window closed or open. `height` is its height now and `closed` the height it
    /// rolls up to at its current width. Returns whether an animation started (or turned round),
    /// so the caller starts its timer. A roll-up under way follows a new `closed` height.
    pub fn want(&mut self, close: bool, height: i32, closed: i32, now: Instant) -> bool {
        let moving = |to, closing| Phase::Moving { from: height, to, start: now, closing };
        match (self.phase, close) {
            (Phase::Open, true) => {
                self.restore = height;
                self.phase = moving(closed, true);
                true
            }
            (Phase::Moving { closing: false, .. }, true) => {
                self.phase = moving(closed, true);
                true
            }
            (Phase::Closed | Phase::Moving { closing: true, .. }, false) => {
                self.phase = moving(self.restore, false);
                true
            }
            (Phase::Moving { from, start, closing: true, .. }, true) => {
                self.phase = Phase::Moving { from, to: closed, start, closing: true };
                false
            }
            (Phase::Open | Phase::Closed | Phase::Moving { .. }, _) => false,
        }
    }

    /// The height for `now` while moving, and whether the move is over (it then rests open or
    /// closed). `None` when still.
    pub fn tick(&mut self, now: Instant) -> Option<(i32, bool)> {
        let Phase::Moving { from, to, start, closing } = self.phase else { return None };
        let t = now.saturating_duration_since(start).as_secs_f64() / DURATION.as_secs_f64();
        if t >= 1.0 {
            self.phase = if closing { Phase::Closed } else { Phase::Open };
            return Some((to, true));
        }
        let h = from as f64 + (to - from) as f64 * ease_both_elastic(t);
        Some((h.round() as i32, false))
    }

    /// Ends a move at once, at its destination: for the first decision after launch, when the
    /// window should simply open as it was left rather than bounce. `None` when still.
    pub fn finish(&mut self, now: Instant) -> Option<i32> {
        self.tick(now + DURATION).map(|(h, _)| h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TALL: i32 = 720;
    const SHORT: i32 = 120;

    fn at(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    #[test]
    fn the_ease_runs_from_0_to_1_and_overshoots_both_ends() {
        assert_eq!(ease_both_elastic(0.0), 0.0);
        assert_eq!(ease_both_elastic(1.0), 1.0);
        assert!((ease_both_elastic(0.5) - 0.5).abs() < 1e-9, "symmetric about the middle");
        let samples: Vec<f64> = (1..1000).map(|i| ease_both_elastic(i as f64 / 1000.0)).collect();
        let (lo, hi) = samples.iter().fold((1.0f64, 0.0f64), |(l, h), &v| (l.min(v), h.max(v)));
        assert!(lo < -0.1 && lo > -0.15, "undershoots below 0: {lo}");
        assert!(hi > 1.1 && hi < 1.15, "overshoots past 1: {hi}");
    }

    #[test]
    fn rolls_up_and_drops_back_to_the_height_it_had() {
        let t0 = Instant::now();
        let mut s = Shade::default();
        assert!(s.want(true, TALL, SHORT, t0));
        assert_eq!(s.tick(t0), Some((TALL, false)));
        assert_eq!(s.tick(at(t0, 800)), Some((SHORT, true)));
        assert!(s.is_closed());
        assert_eq!(s.tick(at(t0, 900)), None, "still once closed");

        assert!(s.want(false, SHORT, SHORT, at(t0, 1000)));
        assert_eq!(s.tick(at(t0, 1800)), Some((TALL, true)));
        assert!(s.is_open());
    }

    #[test]
    fn asking_for_what_it_already_is_or_is_becoming_does_nothing() {
        let t0 = Instant::now();
        let mut s = Shade::default();
        assert!(!s.want(false, TALL, SHORT, t0), "open, asked to open: e.g. unticked in a fight");
        assert!(s.want(true, TALL, SHORT, t0));
        assert!(!s.want(true, 500, SHORT, at(t0, 100)), "already rolling up");
        s.tick(at(t0, 800));
        assert!(!s.want(true, SHORT, SHORT, at(t0, 900)), "already closed");
        assert!(s.want(false, SHORT, SHORT, at(t0, 1000)));
        assert!(!s.want(false, 300, SHORT, at(t0, 1100)), "already dropping");
    }

    #[test]
    fn a_reversal_starts_where_the_window_is_and_keeps_the_first_height() {
        let t0 = Instant::now();
        let mut s = Shade::default();
        s.want(true, TALL, SHORT, t0);
        // A fight starts mid-roll-up, with the window at 400.
        assert!(s.want(false, 400, SHORT, at(t0, 300)));
        assert_eq!(s.tick(at(t0, 300)), Some((400, false)), "no jump");
        // And ends again mid-drop: rolls up from there, remembering 720, not 400 or 550.
        assert!(s.want(true, 550, SHORT, at(t0, 500)));
        assert_eq!(s.tick(at(t0, 500)), Some((550, false)));
        s.tick(at(t0, 1300));
        assert!(s.is_closed());
        s.want(false, SHORT, SHORT, at(t0, 1400));
        assert_eq!(s.tick(at(t0, 2200)), Some((TALL, true)));
    }

    #[test]
    fn a_roll_up_under_way_follows_a_new_closed_height() {
        let t0 = Instant::now();
        let mut s = Shade::default();
        s.want(true, TALL, SHORT, t0);
        assert!(!s.want(true, 600, 150, at(t0, 200)), "not restarted");
        assert_eq!(s.tick(at(t0, 800)), Some((150, true)));
    }

    #[test]
    fn finish_lands_at_once() {
        let t0 = Instant::now();
        let mut s = Shade::default();
        s.want(true, TALL, SHORT, t0);
        assert_eq!(s.finish(t0), Some(SHORT));
        assert!(s.is_closed());
        assert_eq!(s.finish(t0), None);
    }
}
