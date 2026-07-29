use crate::win::window::GameWindow;
use std::time::Duration;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, PostMessageW, SetForegroundWindow, WM_CHAR, WM_KEYDOWN, WM_KEYUP,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
};

pub const VK_RETURN: u16 = 0x0D;
pub const SC_RETURN: u16 = 0x1C;
pub const VK_SPACE: u16 = 0x20;
pub const SC_SPACE: u16 = 0x39;
// Arrow keys are "extended" keys: bit 24 of lParam must be set on both
// WM_KEYDOWN and WM_KEYUP (see PostMessageInput::press_extended_key).
pub const VK_UP: u16 = 0x26;
pub const SC_UP: u16 = 0x48;
pub const VK_DOWN: u16 = 0x28;
pub const SC_DOWN: u16 = 0x50;
pub const VK_LEFT: u16 = 0x25;
pub const SC_LEFT: u16 = 0x4B;
pub const VK_RIGHT: u16 = 0x27;
pub const SC_RIGHT: u16 = 0x4D;

fn pack_xy(x: i32, y: i32) -> isize {
    ((y as isize) << 16) | (x as isize & 0xFFFF)
}

pub trait Input {
    fn press_key(&self, vk: u16, scancode: u16) -> Result<(), crate::Error>;
    fn click(&self, x: i32, y: i32) -> Result<(), crate::Error>;
}

pub struct PostMessageInput {
    win: GameWindow,
}

impl PostMessageInput {
    pub fn new(win: GameWindow) -> Self {
        Self { win }
    }

    /// Give the game real focus WITHOUT moving the physical cursor. SDL may require
    /// focus before it will deliver mouse buttons.
    ///
    /// Returns SetForegroundWindow's own success/failure. Windows restricts this
    /// call (a process that doesn't own the foreground window and didn't receive
    /// the last input event generally cannot steal it), so a caller MUST check
    /// this rather than assume the call worked.
    pub fn focus(&self) -> bool {
        unsafe { SetForegroundWindow(self.win.hwnd).as_bool() }
    }

    /// Whether the game window currently holds OS foreground focus. Use this to
    /// verify `focus()` (or anything else) actually put the game in the
    /// foreground before trusting a click/focus-dependent result.
    pub fn has_foreground(&self) -> bool {
        unsafe { GetForegroundWindow() == self.win.hwnd }
    }

    /// Types text one character at a time via WM_CHAR.
    ///
    /// This is a DIFFERENT path from `press_key`, and the distinction matters:
    ///   love.keypressed <- SDL_KEYDOWN   <- WM_KEYDOWN (needs a real scancode)
    ///   love.textinput  <- SDL_TEXTINPUT <- WM_CHAR
    /// Combat selects tiles through `rpg.textinput` (rpg.lua:801), which is driven by
    /// love.textinput — so letters must go out as WM_CHAR. Sending them as WM_KEYDOWN
    /// would fire keypressed and select nothing.
    pub fn type_text(&self, text: &str) -> Result<(), crate::Error> {
        for ch in text.chars() {
            unsafe {
                let _ = PostMessageW(self.win.hwnd, WM_CHAR, WPARAM(ch as usize), LPARAM(1));
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        Ok(())
    }

    /// Extended keys (arrows, Home/End/Insert/Delete, etc.) require bit 24 of
    /// lParam set on both WM_KEYDOWN and WM_KEYUP; WM_KEYDOWN/UP without it (as
    /// used by `press_key`) is only correct for non-extended keys.
    pub fn press_extended_key(&self, vk: u16, scancode: u16) -> Result<(), crate::Error> {
        let sc = scancode as isize;
        const EXTENDED: isize = 0x0100_0000;
        let down = LPARAM((sc << 16) | 0x0000_0001 | EXTENDED);
        let up = LPARAM((sc << 16) | (0xC000_0001u32 as isize) | EXTENDED);
        unsafe {
            let _ = PostMessageW(self.win.hwnd, WM_KEYDOWN, WPARAM(vk as usize), down);
            std::thread::sleep(Duration::from_millis(60));
            let _ = PostMessageW(self.win.hwnd, WM_KEYUP, WPARAM(vk as usize), up);
        }
        Ok(())
    }
}

impl Input for PostMessageInput {
    /// love.keypressed is driven by SDL_KEYDOWN, which needs a real scancode in
    /// lParam bits 16-23. WM_CHAR is NOT sufficient.
    fn press_key(&self, vk: u16, scancode: u16) -> Result<(), crate::Error> {
        let sc = scancode as isize;
        let down = LPARAM((sc << 16) | 0x0000_0001);
        let up = LPARAM((sc << 16) | (0xC000_0001u32 as isize));
        unsafe {
            let _ = PostMessageW(self.win.hwnd, WM_KEYDOWN, WPARAM(vk as usize), down);
            std::thread::sleep(Duration::from_millis(60));
            let _ = PostMessageW(self.win.hwnd, WM_KEYUP, WPARAM(vk as usize), up);
        }
        Ok(())
    }

    /// Hover first: this game's buttons use onMouseOver / showIf state, so a click
    /// with no preceding motion may not register.
    fn click(&self, x: i32, y: i32) -> Result<(), crate::Error> {
        unsafe {
            let _ = PostMessageW(self.win.hwnd, WM_MOUSEMOVE, WPARAM(0), LPARAM(pack_xy(x, y)));
            std::thread::sleep(Duration::from_millis(150));
            let _ = PostMessageW(self.win.hwnd, WM_LBUTTONDOWN, WPARAM(1), LPARAM(pack_xy(x, y)));
            std::thread::sleep(Duration::from_millis(80));
            let _ = PostMessageW(self.win.hwnd, WM_LBUTTONUP, WPARAM(0), LPARAM(pack_xy(x, y)));
        }
        Ok(())
    }
}
