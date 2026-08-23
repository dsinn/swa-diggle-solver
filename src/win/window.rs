/// A button's position as declared in the game source.
///
/// `ss_*` are the normalized screen-space multipliers passed as the button's x/y.
/// `os_*` are the `xOffset`/`yOffset` effects, measured in whole button widths.
/// `w`/`h` come from buttonTypeData (ui/elements/button.lua:16).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ButtonSpec {
    pub ss_x: f64,
    pub ss_y: f64,
    pub os_x: f64,
    pub os_y: f64,
    pub w: f64,
    pub h: f64,
}

/// utils/input.lua:20 — the default scaleType is min(w/1920, h/1080).
pub fn raw_scale(width: i32, height: i32) -> f64 {
    (width as f64 / 1920.0).min(height as f64 / 1080.0)
}

/// Resolves a button's centre in client-area pixels.
///
/// Derived from buildDrawDataTable (main.lua:246). LOVE's newTransform(x, y, r,
/// sx, sy, ox, oy) maps a local point p to (x,y) + s*(p - o). The button's local
/// space spans (0,0)..(w,h) — see the hit test in ui/elements/button.lua:93 — and
/// the origin is o = (-w*(os_x-0.5), -h*(os_y-0.5)), so the local centre (w/2,h/2)
/// lands at (client_w*ss_x + s*w*os_x, client_h*ss_y + s*h*os_y).
pub fn button_center(spec: &ButtonSpec, width: i32, height: i32) -> (i32, i32) {
    let s = raw_scale(width, height);
    let cx = width as f64 * spec.ss_x + s * spec.w * spec.os_x;
    let cy = height as f64 * spec.ss_y + s * spec.h * spec.os_y;
    (cx as i32, cy as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The start menu's Continue button: button('Continue', 0, 0.75, {xOffset = 0.75})
    /// with the `default` button type, 250x100 (ui/elements/button.lua:17).
    fn continue_button() -> ButtonSpec {
        ButtonSpec { ss_x: 0.0, ss_y: 0.75, os_x: 0.75, os_y: 0.0, w: 250.0, h: 100.0 }
    }

    #[test]
    fn scale_is_the_smaller_of_the_two_ratios() {
        assert_eq!(raw_scale(1920, 1080), 1.0);
        assert_eq!(raw_scale(1600, 900), 1600.0 / 1920.0);
        assert_eq!(raw_scale(3840, 1080), 1.0);
    }

    #[test]
    fn resolves_a_button_centre_at_native_resolution() {
        // cx = 1920*0 + 1.0*250*0.75 = 187.5 ; cy = 1080*0.75 + 1.0*100*0 = 810
        assert_eq!(button_center(&continue_button(), 1920, 1080), (187, 810));
    }

    #[test]
    fn button_centre_tracks_the_scale_at_other_resolutions() {
        // scale = 1600/1920 = 0.8333 ; cx = 0.8333*250*0.75 = 156.2 ; cy = 900*0.75
        assert_eq!(button_center(&continue_button(), 1600, 900), (156, 675));
    }

    #[test]
    fn offsets_shift_the_centre_by_whole_button_widths() {
        // The Start/Restart button sits at the same y with xOffset = 2.
        let start = ButtonSpec { os_x: 2.0, ..continue_button() };
        assert_eq!(button_center(&start, 1920, 1080), (500, 810));
    }
}

use windows::Win32::Foundation::{BOOL, FALSE, HWND, LPARAM, POINT, RECT, TRUE};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClientRect, GetWindowThreadProcessId, IsWindowVisible,
};

#[derive(Debug, Clone, Copy)]
pub struct GameWindow {
    pub hwnd: HWND,
}

struct SearchCtx {
    want_pid: u32,
    found: Option<HWND>,
}

unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let ctx = &mut *(lparam.0 as *mut SearchCtx);
    if IsWindowVisible(hwnd).as_bool() {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == ctx.want_pid {
            ctx.found = Some(hwnd);
            return FALSE; // stop enumerating
        }
    }
    TRUE
}

/// Finds the first visible top-level window owned by `pid`.
pub fn find_by_pid(pid: u32) -> Option<GameWindow> {
    let mut ctx = SearchCtx { want_pid: pid, found: None };
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut ctx as *mut _ as isize));
    }
    ctx.found.map(|hwnd| GameWindow { hwnd })
}

impl GameWindow {
    pub fn client_size(&self) -> Result<(i32, i32), crate::Error> {
        let mut r = RECT::default();
        unsafe { GetClientRect(self.hwnd, &mut r) }
            .map_err(|e| crate::Error::Win32(e.to_string()))?;
        Ok((r.right - r.left, r.bottom - r.top))
    }

    pub fn button_center(&self, spec: &ButtonSpec) -> Result<(i32, i32), crate::Error> {
        let (w, h) = self.client_size()?;
        Ok(button_center(spec, w, h))
    }

    /// Screen coordinates of the client area's top-left corner.
    ///
    /// Everything we compute from the game source (button centres, hotspots, capture
    /// regions) is in CLIENT pixels, but `GetCursorPos` — our only oracle for which
    /// hotspot is highlighted — reports SCREEN pixels. Those two agree only when the
    /// window happens to sit at the desktop origin. On a multi-monitor desktop with a
    /// display left of or above the primary, screen coordinates go negative and the
    /// unconverted comparison silently comes out wrong by hundreds of pixels.
    pub fn client_origin(&self) -> Result<(i32, i32), crate::Error> {
        let mut p = POINT::default();
        unsafe { ClientToScreen(self.hwnd, &mut p) }
            .ok()
            .map_err(|e| crate::Error::Win32(e.to_string()))?;
        Ok((p.x, p.y))
    }

    /// Converts a client-pixel point to screen pixels, for comparison against
    /// `GetCursorPos`. See `client_origin`.
    pub fn client_to_screen(&self, x: i32, y: i32) -> Result<(i32, i32), crate::Error> {
        let (ox, oy) = self.client_origin()?;
        Ok((ox + x, oy + y))
    }
}
