use crate::win::window::GameWindow;
use std::time::Duration;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, PostMessageW, SetForegroundWindow, WM_CHAR, WM_KEYDOWN, WM_KEYUP,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE,
};

/// Moves the real cursor, then injects a real left click **at driver level**.
///
/// This is NOT what the click spikes tested. They posted `WM_LBUTTONDOWN`, and SDL does not take
/// mouse buttons from posted window messages — it reads hardware input — so "posted clicks do not
/// work" was a true but much narrower finding than "clicking does not work". `SendInput` injects
/// below the message queue, so SDL sees an ordinary click. It had never been tried here.
///
/// The cost is that this genuinely drives the machine's pointer: for the moment it runs, the user
/// does not have their mouse. That was previously a hard constraint; it is not, because hotspot
/// navigation already warps the cursor on every arrow press (`utils/input.lua:94-98`).
///
/// Coordinates are SCREEN pixels. Button events carry no position — `SendInput` applies them at the
/// current cursor location — so the warp must land first, which also avoids the virtual-desktop
/// normalisation that `MOUSEEVENTF_ABSOLUTE` would require on a multi-monitor desktop with negative
/// coordinates.
pub fn warp_cursor(x: i32, y: i32) -> Result<(), crate::Error> {
    unsafe { windows::Win32::UI::WindowsAndMessaging::SetCursorPos(x, y) }
        .map_err(|e| crate::Error::Win32(e.to_string()))
}

/// Types text at **driver level**, as `SendInput` unicode events.
///
/// The same distinction that made clicking work applies here. `PostMessageInput::type_text` posts
/// `WM_CHAR` into the window's queue, which is the analogue of the posted mouse clicks SDL ignored.
/// `KEYEVENTF_UNICODE` goes in below the queue, so SDL raises a genuine `SDL_TEXTINPUT` and the
/// game's `rpg.textinput` (`rpg.lua:801`) sees it — which is the only path that selects tiles.
///
/// Letters must NOT go out as key events: `love.keypressed` drives menu actions, not tile selection.
///
/// The delay between characters is deliberate. Selection is stateful — each character consults the
/// tiles already chosen (the ligature clauses at `rpg.lua:816-838`) — so the game must process one
/// character before the next arrives.
pub fn type_text_injected(text: &str, per_char: Duration) -> Result<(), crate::Error> {
    for ch in text.chars() {
        let mut units = [0u16; 2];
        for &unit in ch.encode_utf16(&mut units).iter() {
            let key = |flags| INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY(0),
                        wScan: unit,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };
            let events = [key(KEYEVENTF_UNICODE), key(KEYEVENTF_UNICODE | KEYEVENTF_KEYUP)];
            let sent = unsafe { SendInput(&events, std::mem::size_of::<INPUT>() as i32) };
            if sent != events.len() as u32 {
                return Err(crate::Error::Win32(format!(
                    "SendInput accepted {sent} of {} key events for {ch:?}",
                    events.len()
                )));
            }
        }
        std::thread::sleep(per_char);
    }
    Ok(())
}

fn mouse_event(flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Moves and clicks in **one** `SendInput` batch, at absolute screen coordinates.
///
/// `warp_cursor` then `inject_left_click` is two separate trips through the input system, and the
/// click carries no position — it lands wherever the cursor has got to. That is a race, and the only
/// defence was a sleep between them. Batching move-down-up into a single `SendInput` makes the
/// ordering the operating system's problem: the events are delivered in sequence, so the click
/// cannot overtake the move, and no sleep is needed at all.
///
/// Coordinates are normalized against the **virtual desktop**, which is what makes this safe on a
/// multi-monitor setup where secondary displays sit at negative coordinates.
pub fn click_at(x: i32, y: i32) -> Result<(), crate::Error> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN,
    };
    let (vx, vy, vw, vh) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    if vw <= 1 || vh <= 1 {
        return Err(crate::Error::Win32("virtual desktop has no extent".into()));
    }
    // The -1 matters: the absolute range is 0..=65535 inclusive, so dividing by the raw width puts
    // the last pixel column just past the end.
    let nx = ((x - vx) as f64 * 65535.0 / (vw - 1) as f64).round() as i32;
    let ny = ((y - vy) as f64 * 65535.0 / (vh - 1) as f64).round() as i32;

    let event = |flags, dx, dy| INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx, dy, mouseData: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    };
    let events = [
        event(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK, nx, ny),
        event(MOUSEEVENTF_LEFTDOWN, 0, 0),
        event(MOUSEEVENTF_LEFTUP, 0, 0),
    ];
    let sent = unsafe { SendInput(&events, std::mem::size_of::<INPUT>() as i32) };
    if sent != events.len() as u32 {
        return Err(crate::Error::Win32(format!(
            "SendInput accepted {sent} of {} events",
            events.len()
        )));
    }
    Ok(())
}

/// [`click_at`], refusing to fire unless the game is what sits under the target point.
///
/// Our clicks are *positional*: `SendInput` delivers them to whatever window is topmost at that
/// screen coordinate, with no reference to the game at all. Keystrokes have never had this problem —
/// they go through `PostMessageW` to a specific HWND (see [`PostMessageInput::press_key`]) and cannot
/// leave the process. That asymmetry is why a run could pause a video in a browser while the game
/// carried on responding normally: the click went to the browser, the keys did not.
///
/// It costs one `WindowFromPoint` per click and removes a whole class of accident — a window
/// appearing over the game, the user alt-tabbing mid-run, the game being minimised. Every one of
/// those turns a click meant for a button into a click on someone else's application.
///
/// `GA_ROOT` because `WindowFromPoint` returns the deepest child under the point, which for a LOVE
/// window is not necessarily the HWND we hold.
pub fn click_at_in(win: &crate::win::window::GameWindow, x: i32, y: i32) -> Result<(), crate::Error> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::{GetAncestor, WindowFromPoint, GA_ROOT};
    let under = unsafe { GetAncestor(WindowFromPoint(POINT { x, y }), GA_ROOT) };
    if under != win.hwnd {
        return Err(crate::Error::Win32(format!(
            "refusing to click ({x}, {y}): {under:?} is in front of the game, not {:?}",
            win.hwnd
        )));
    }
    click_at(x, y)
}

/// Injects `count` left clicks at the current cursor position.
///
/// `count = 2` is how to reach `doubleClickEffect` (`overworldview.lua:1446-1468`), which on a
/// location that is not the player's own falls through to `core.travelTo` — with no reachability
/// guard, so only ever aim it at a node the log has just reported as adjacent.
pub fn inject_left_click(count: usize) -> Result<(), crate::Error> {
    for i in 0..count {
        let events = [
            mouse_event(MOUSEEVENTF_LEFTDOWN),
            mouse_event(MOUSEEVENTF_LEFTUP),
        ];
        let sent = unsafe { SendInput(&events, std::mem::size_of::<INPUT>() as i32) };
        if sent != events.len() as u32 {
            return Err(crate::Error::Win32(format!(
                "SendInput accepted {sent} of {} events",
                events.len()
            )));
        }
        if i + 1 < count {
            // Inside the OS double-click time, or the game sees two separate single clicks.
            std::thread::sleep(Duration::from_millis(60));
        }
    }
    Ok(())
}

pub const VK_RETURN: u16 = 0x0D;
pub const SC_RETURN: u16 = 0x1C;
pub const VK_SPACE: u16 = 0x20;
pub const SC_SPACE: u16 = 0x39;
// Arrow keys are "extended" keys: bit 24 of lParam must be set on both
// WM_KEYDOWN and WM_KEYUP (see PostMessageInput::press_extended_key).
/// Bound to `backOptions` (`utils/defaultbinds/keyboard.lua:9`), so it is `goBack()` where a screen
/// defines one and the **options menu** otherwise.
///
/// Normally not to be sent: on a screen with no `goBack` it opens options, and a run that does not
/// know it is in there will keep pressing at a map that is no longer on screen. It exists for one
/// deliberate use — reaching the options menu's `Menu` button to get back to the main menu, which
/// is the game's own way of skipping the anomaly cinematic. Send it only where the next click is
/// already decided.
pub const VK_ESCAPE: u16 = 0x1B;
pub const SC_ESCAPE: u16 = 0x01;
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

    /// Holds an extended key down for `dur` before releasing it.
    ///
    /// Some inputs are gestures, not events. The overworld map pan stores a direction on
    /// key-down (`overworldview.hotspotDirection`, `:1115-1123`) and clears it on key-up
    /// (`hotspotDirectionRelease`, `:1125-1133`), integrating it in `core:update` with
    /// acceleration. Distance travelled is therefore a function of HOLD DURATION, and
    /// `press_extended_key`'s fixed 60 ms cannot express it.
    pub fn hold_extended_key(
        &self,
        vk: u16,
        scancode: u16,
        dur: Duration,
    ) -> Result<(), crate::Error> {
        let sc = scancode as isize;
        const EXTENDED: isize = 0x0100_0000;
        let down = LPARAM((sc << 16) | 0x0000_0001 | EXTENDED);
        let up = LPARAM((sc << 16) | (0xC000_0001u32 as isize) | EXTENDED);
        unsafe {
            let _ = PostMessageW(self.win.hwnd, WM_KEYDOWN, WPARAM(vk as usize), down);
            std::thread::sleep(dur);
            let _ = PostMessageW(self.win.hwnd, WM_KEYUP, WPARAM(vk as usize), up);
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
