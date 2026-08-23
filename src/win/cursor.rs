//! Where the game says its own controls are — task #19's oracle, and what it is not.
//!
//! ## The premise this was filed under was wrong
//!
//! #19 was written as "find buttons with the cursor oracle instead of clicking blind", on the
//! reasonable guess that the cursor changes shape over a live control the way a web page's does. It
//! does not. `setCursor` (`main.lua:22-27`) picks one of two arrows by *window scale* and is called
//! exactly twice, at load and on a config change (`:76`, `:538`). Nothing varies it per control, so
//! there is no shape to read.
//!
//! ## What is really there, and it is better
//!
//! The game drives its own pointer. `input.setHotspotHighlight` (`utils/input.lua:94-98`) does
//! `love.mouse.setPosition(unpack(hotspot))` and then `love.mouse.setVisible(false)` — so while
//! hotspot navigation is active, **the OS cursor is parked exactly on the centre of a control the
//! game computed, and the pointer is hidden**. Both halves are readable from outside: `GetCursorPos`
//! gives the position, and `GetCursorInfo`'s `CURSOR_SHOWING` flag gives the visibility.
//!
//! That is a coordinate the game worked out from its own layout, at the current window size, with no
//! template, no threshold, and no vulnerability to the greyed/highlighted artwork problem that has
//! cost this project three separate bugs.
//!
//! ## What it cannot tell us, which is the disappointing half
//!
//! A hotspot exists for a **shown** control, not a pressable one:
//!
//! ```lua
//! getHotspots = function(self)
//!     return not not (not effects.showIf or effects.showIf and effects.showIf())
//! end
//! ```
//!
//! `ui/elements/button.lua:296-298`. There is no `activeIf` in it, so a greyed button is a hotspot
//! like any other. This oracle answers *where*, never *whether pressing does anything* — the same
//! information the artwork already carries, delivered more precisely. Affordance still has to come
//! from the save, which is the division the shrine work arrived at independently: the save says an
//! action is possible, the screen says it is ready.
//!
//! ## And it is dormant more often than not
//!
//! `hotPointHighlight` starts `nil`; `snapToNearestHotspot` only acts `if hotPointHighlight or
//! force` (`:167`) and **no caller in the game passes `force`**. Worse, both `keydirections` (`:188`)
//! and `setHotspotHighlight` (`:99`) index `hotPointHighlight[5]` with no nil check. So on a screen
//! where no highlight has been established, pressing an arrow key is a Lua error in a game we are
//! forbidden to modify.
//!
//! **Which is why nothing here presses anything.** [`highlighted`] reads, and reads only. The hidden
//! pointer is the interlock: if the cursor is hidden the system is live and its position means
//! something; if it is showing, there is no highlight and we must not send a direction key. Walking
//! the ring to enumerate every control is the obvious next step and is deliberately not taken until
//! a live run has shown the interlock behaving.

use crate::win::window::GameWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorInfo, GetCursorPos, CURSORINFO, CURSOR_SHOWING,
};

/// The pointer's position in SCREEN pixels.
pub fn position() -> Result<(i32, i32), crate::Error> {
    let mut p = windows::Win32::Foundation::POINT::default();
    unsafe { GetCursorPos(&mut p) }.map_err(|e| crate::Error::Win32(e.to_string()))?;
    Ok((p.x, p.y))
}

/// Is the pointer hidden — which is to say, **is hotspot navigation currently driving it**?
///
/// `love.mouse.setVisible(false)` is called with every highlight and `true` when one is cleared
/// (`utils/input.lua:96-98`), so this is the game's own record of whether a control is selected,
/// read through the window manager rather than guessed from pixels.
pub fn is_hidden() -> Result<bool, crate::Error> {
    let mut info =
        CURSORINFO { cbSize: std::mem::size_of::<CURSORINFO>() as u32, ..Default::default() };
    unsafe { GetCursorInfo(&mut info) }.map_err(|e| crate::Error::Win32(e.to_string()))?;
    Ok(info.flags != CURSOR_SHOWING)
}

/// The centre of the control the game currently has selected, in CLIENT pixels.
///
/// `None` when no highlight is active, which is the ordinary state after any real mouse movement:
/// `love.mousemoved` clears the highlight on a non-zero delta (`main.lua:420`). It is also `None`
/// when the pointer is hidden but outside the window, which is not a state the game produces and is
/// treated as "no answer" rather than clamped.
pub fn highlighted(win: &GameWindow) -> Result<Option<(i32, i32)>, crate::Error> {
    if !is_hidden()? {
        return Ok(None);
    }
    let (sx, sy) = position()?;
    let (ox, oy) = win.client_origin()?;
    let (cw, ch) = win.client_size()?;
    let (x, y) = (sx - ox, sy - oy);
    match x >= 0 && y >= 0 && x < cw && y < ch {
        true => Ok(Some((x, y))),
        false => Ok(None),
    }
}

/// How far a click we are about to send sits from the control the game has selected.
///
/// The whole point of the oracle in its first, read-only form: a cross-check on arithmetic. Every
/// coordinate this project clicks is computed from `ss`/`os` multipliers copied out of the Lua by
/// hand, and a copy that is wrong looks exactly like a copy that is right until a run dies on it.
/// `None` means there is nothing to compare against, which is not a failure.
pub fn miss_by(win: &GameWindow, aimed: (i32, i32)) -> Option<f64> {
    let at = highlighted(win).ok().flatten()?;
    let (dx, dy) = ((at.0 - aimed.0) as f64, (at.1 - aimed.1) as f64);
    Some((dx * dx + dy * dy).sqrt())
}
