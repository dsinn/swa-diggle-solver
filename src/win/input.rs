use crate::win::window::GameWindow;
use std::time::Duration;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    PostMessageW, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
};

pub const VK_RETURN: u16 = 0x0D;
pub const SC_RETURN: u16 = 0x1C;
pub const VK_SPACE: u16 = 0x20;
pub const SC_SPACE: u16 = 0x39;

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
