//! Moving the hotspot highlight to a known control.
//!
//! Arrow keys call `input.setHotspotHighlight` (`utils/input.lua:94-98`), which calls
//! `love.mouse.setPosition` — so navigation **warps the real OS cursor**. That is a genuine cost
//! to the user, not an implementation detail; warn before doing it. It is also unavoidable:
//! posted mouse-button events are never delivered by SDL (design v2 §3), so this is the only
//! positional actuation available.
//!
//! The walk is greedy, which is not always enough. Some screens are laid out so that the nearest
//! step is not toward the target, and the highlight can oscillate; on the overworld a hotspot's
//! owner can define `hotspotDirection`, which consumes arrow keys entirely
//! (`utils/input.lua:186-190`) so the highlight never moves at all. Hence the contract here is
//! **report, never assume**: callers must check [`NavOutcome::on_target`] before activating
//! anything. Pressing `Return` on an unverified highlight is how a run gets destroyed — on the
//! start menu, Continue and Restart share a row, and Restart eulogizes the save
//! (`ui/heroselect.lua:271`).

use crate::win::input::{
    PostMessageInput, SC_DOWN, SC_LEFT, SC_RIGHT, SC_UP, VK_DOWN, VK_LEFT, VK_RIGHT, VK_UP,
};
use crate::win::window::GameWindow;
use std::time::Duration;

/// How long to let the highlight settle after each press before reading the cursor.
const SETTLE: Duration = Duration::from_millis(350);
/// Give up rather than thrash. Twelve steps crosses any screen we have seen.
const MAX_STEPS: usize = 12;

#[derive(Debug, Clone)]
pub struct NavOutcome {
    /// Whether the highlight ended within tolerance of the target, in client pixels.
    pub on_target: bool,
    /// Where it ended, in **client** pixels.
    pub landed: (i32, i32),
    /// Every position visited, for diagnosing an oscillation.
    pub trail: Vec<(i32, i32)>,
}

fn cursor_screen() -> (i32, i32) {
    let mut p = windows::Win32::Foundation::POINT::default();
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut p);
    }
    (p.x, p.y)
}

/// Walks the highlight toward a target given in **client** pixels.
///
/// Targets come from the game's source, which is client-space; `GetCursorPos` is screen-space.
/// Conflating them was a real bug that only worked because the window happened to launch at
/// (0,0) — see design v2 §4.1.
pub fn to_client_point(
    win: &GameWindow, input: &PostMessageInput, tx: i32, ty: i32, tolerance: i32,
) -> Result<NavOutcome, crate::Error> {
    let (ox, oy) = win.client_origin()?;
    let target = (ox + tx, oy + ty);
    let mut trail = Vec::new();

    // One press establishes a highlight. With none, `Return` falls through to the mode's
    // `affirmative` instead of activating a control (design v2 §3.1) — which on the overworld
    // means starting a journey.
    input.press_extended_key(VK_DOWN, SC_DOWN)?;
    std::thread::sleep(SETTLE);

    // Pure greed oscillates. Going for the dominant axis first is right, but when that axis is
    // blocked the highlight ping-pongs between two hotspots forever and never tries the other
    // one — observed as (187,38) <-> (1674,38) while the target sat at (960,540), because `dx`
    // dominated at both ends. So: prefer the dominant axis, and when a press fails to reduce the
    // distance, take the other axis on the next step instead of repeating the mistake.
    let mut prefer_secondary = false;
    let mut prev_dist = i32::MAX;

    for _ in 0..MAX_STEPS {
        let c = cursor_screen();
        trail.push((c.0 - ox, c.1 - oy));
        let (dx, dy) = (target.0 - c.0, target.1 - c.1);
        if dx.abs() <= tolerance && dy.abs() <= tolerance {
            break;
        }
        let dist = dx.abs() + dy.abs();
        // No improvement means the axis we just used is blocked in that direction.
        prefer_secondary = if dist < prev_dist { false } else { !prefer_secondary };
        prev_dist = dist;

        let horizontal_first = dx.abs() >= dy.abs();
        let horizontal = if prefer_secondary { !horizontal_first } else { horizontal_first };
        let (vk, sc) = if horizontal {
            if dx > 0 {
                (VK_RIGHT, SC_RIGHT)
            } else {
                (VK_LEFT, SC_LEFT)
            }
        } else if dy > 0 {
            (VK_DOWN, SC_DOWN)
        } else {
            (VK_UP, SC_UP)
        };
        input.press_extended_key(vk, sc)?;
        std::thread::sleep(SETTLE);
    }

    let c = cursor_screen();
    let landed = (c.0 - ox, c.1 - oy);
    trail.push(landed);
    Ok(NavOutcome {
        on_target: (landed.0 - tx).abs() <= tolerance && (landed.1 - ty).abs() <= tolerance,
        landed,
        trail,
    })
}
