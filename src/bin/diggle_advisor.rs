//! Diggle Advisor: Diggle's word ranking in a window, for a human playing the game themselves.
//!
//! ## It never touches the game
//!
//! No `SendInput`, no message to the game's window, no screen capture, no console. It never calls
//! `SetForegroundWindow`; it has focus only when clicked. Everything it knows comes from the save
//! folder `%APPDATA%\SternlyWordedAdventures` ([`diggle_solver::advisor::watch::save_dirs`]),
//! read under the rules in [`diggle_solver::advisor::watch`]. Never the `LOVE` subfolder.
//!
//! ## What it shows
//!
//! - **Out of combat** (a banner, no header line) when `combatSaveData` does not exist. That is later than the fight really
//!   ends — see [`diggle_solver::advisor::mode::classify`] for why, and why it is temporary.
//! - **Waiting** while a fight is between turns: the last list stays up, greyed.
//! - **Your turn**: every playable word, in the order Diggle prefers them
//!   ([`diggle_solver::search::ranked`]). The top row is the word Diggle would type. Each letter
//!   sits on the colour of the tile it would be typed into; the columns give the expected damage,
//!   the enemy's lexicon bonus when it is not 1, the health and armour it should give, and what it does.
//!
//! **Stalling** on the halfling's free turns is the checkbox "Stall on halfling's even turns": even
//! as the game's Turn box counts them, the turns its `immuneTurnMod2` passive means the enemy cannot
//! hit back. Ticked (the default), an enemy that could run is kept rather than scared off on those
//! turns: the top rows hurt it without frightening it, or deal nothing, and a scare or a kill comes
//! only after them ([`diggle_solver::search::Goal::Hold`], [`diggle_solver::search::Modifiers::stall`]).
//! Unticked, those turns are ranked like any other. Changing it re-ranks the turn on screen at once.
//!
//! **Not modelled**: Diggle's murder backoff, which aims lower after the game warns that a word
//! would kill a frightened enemy. That warning reaches Diggle through the console, which this does
//! not read. You see it on screen.
//!
//! ## The window
//!
//! Its size, "Stay on top", "Stall on halfling's even turns", and whether the log box is shown and
//! how tall it is are remembered between launches ([`diggle_solver::advisor::settings`]). The log
//! box starts collapsed; drag the gap under it to resize it. Settings are written half a second after the last change
//! ([`SAVE_DELAY_MS`]), so a drag is one write rather than hundreds.
//!
//! With `--timing-log`, each save's way from the game's write to the list on screen is timed, step
//! by step, in `%LOCALAPPDATA%\DiggleAdvisor\timing.log` ([`diggle_solver::advisor::telemetry`]).
//!
//! ## Options
//!
//! - `--game-dir <path>`: the game checkout. Otherwise `game_dir` from `config.toml` in the working
//!   directory, else `../sternly-worded-adventures`.
//! - `--dir <path>` (repeatable): watch these folders instead of the save folder. For testing
//!   against a copied save.
//! - `--timing-log`: write the timing log above. Off unless asked for — the user's call,
//!   2026-11-07.

#![windows_subsystem = "windows"]

use diggle_solver::advisor::mode::Mode;
use diggle_solver::advisor::settings::{self, Settings};
use diggle_solver::advisor::telemetry;
use diggle_solver::advisor::view::{Resources, View};
use diggle_solver::advisor::watch::{self, Event};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{mpsc, Mutex, OnceLock};
use windows::core::{w, Interface, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{BOOL, COLORREF, HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_IGNORE, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
    D2D_POINT_2F, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Bitmap, ID2D1DCRenderTarget, ID2D1Factory, ID2D1RenderTarget,
    D2D1_BITMAP_INTERPOLATION_MODE_LINEAR, D2D1_BITMAP_PROPERTIES, D2D1_DRAW_TEXT_OPTIONS_CLIP,
    D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
    D2D1_RENDER_TARGET_USAGE_NONE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteInlineObject, IDWriteInlineObject_Impl,
    IDWriteTextFormat, IDWriteTextRenderer, DWRITE_BREAK_CONDITION, DWRITE_BREAK_CONDITION_NEUTRAL,
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_INLINE_OBJECT_METRICS, DWRITE_OVERHANG_METRICS,
    DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TEXT_RANGE,
    DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::DirectWrite::{IDWriteTextLayout, DWRITE_HIT_TEST_METRICS};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, FillRect, GetSysColorBrush,
    GetTextExtentPoint32W, GetTextMetricsW, InvalidateRect, SelectObject, SetBkMode, SetTextColor,
    COLOR_BTNFACE, COLOR_WINDOW, DT_CENTER, DT_SINGLELINE, DT_TOP, HFONT, HGDIOBJ, TEXTMETRICW,
    TRANSPARENT,
};
use windows::Win32::Graphics::Gdi::{GetDC, ReleaseDC, ScreenToClient};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, CreateDIBSection, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, DestroyIcon, HICON, ICONINFO, ICON_BIG, ICON_SMALL, WM_SETICON,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    ImageList_Create, InitCommonControlsEx, BST_CHECKED, CDDS_ITEMPREPAINT, CDDS_PREPAINT,
    CDDS_SUBITEM, CDRF_DODEFAULT, CDRF_NEWFONT, CDRF_NOTIFYITEMDRAW, CDRF_NOTIFYSUBITEMDRAW,
    CDRF_SKIPDEFAULT, ICC_LISTVIEW_CLASSES, ICC_STANDARD_CLASSES, ICC_TAB_CLASSES, ILC_COLOR32,
    INITCOMMONCONTROLSEX, LVCFMT_LEFT, LVCFMT_RIGHT, LVCF_FMT, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW,
    LVIF_TEXT, LVIR_BOUNDS, LVM_GETSUBITEMRECT, LVM_INSERTCOLUMNW, LVM_SETEXTENDEDLISTVIEWSTYLE,
    LVM_SETIMAGELIST, LVM_SETITEMCOUNT, LVN_GETDISPINFOW, LVSIL_SMALL, LVS_EX_DOUBLEBUFFER,
    LVS_EX_FULLROWSELECT, LVS_NOSORTHEADER, LVS_OWNERDATA, LVS_REPORT, NMHDR, NMLVCUSTOMDRAW,
    NMLVDISPINFOW, NM_CUSTOMDRAW, WC_LISTVIEWW,
};
use windows::Win32::UI::Controls::{
    ImageList_Destroy, HIMAGELIST, LVM_GETCOLUMNWIDTH, LVM_SETCOLUMNWIDTH, TOOLTIPS_CLASSW,
    TTDT_AUTOPOP, TTF_IDISHWND, TTF_SUBCLASS, TTM_ADDTOOLW, TTM_SETDELAYTIME, TTM_SETMAXTIPWIDTH,
    TTS_ALWAYSTIP, TTS_NOPREFIX, TTTOOLINFOW,
};
use windows::Win32::UI::Controls::{
    LVHITTESTINFO, LVM_SUBITEMHITTEST, TOOLTIP_FLAGS, TTM_NEWTOOLRECTW, TTM_POP, TTM_RELAYEVENT,
    TTM_UPDATETIPTEXTW, WM_MOUSELEAVE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, SetWindowLongPtrW, GWLP_WNDPROC, WM_MOUSEFIRST, WM_MOUSELAST, WNDPROC,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetCursorPos, GetMessageW,
    GetWindowRect, KillTimer, LoadCursorW, MoveWindow, PostMessageW, PostQuitMessage,
    RegisterClassW, SendMessageW, SetCursor, SetTimer, SetWindowPos, SetWindowTextW, ShowWindow,
    TranslateMessage, BM_GETCHECK, BM_SETCHECK, BN_CLICKED, BS_AUTOCHECKBOX, CW_USEDEFAULT,
    ES_AUTOVSCROLL, ES_MULTILINE, ES_READONLY, HMENU, HTCLIENT, HWND_NOTOPMOST, HWND_TOPMOST,
    IDC_ARROW, IDC_SIZENS, MINMAXINFO, MSG, SIZE_RESTORED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SW_HIDE, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CAPTURECHANGED, WM_COMMAND,
    WM_CREATE, WM_DESTROY, WM_GETFONT, WM_GETMINMAXINFO, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_NOTIFY, WM_SETCURSOR, WM_SETFONT, WM_SIZE, WM_TIMER, WNDCLASSW, WS_BORDER,
    WS_CHILD, WS_CLIPCHILDREN, WS_EX_TOPMOST, WS_OVERLAPPEDWINDOW, WS_POPUP, WS_TABSTOP,
    WS_VISIBLE, WS_VSCROLL,
};

/// Posted to our own window when [`VIEW`] has changed. Never sent anywhere else.
const WM_VIEW: u32 = WM_APP + 1;
const ID_TOPMOST: usize = 101;
const ID_LOG: usize = 102;
const ID_STALL: usize = 103;
const ID_UNDERLINE: usize = 104;
/// The timer that writes the settings once they have stopped changing.
const ID_SAVE: usize = 1;
const SAVE_DELAY_MS: u32 = 500;
/// Static-control styles (`winuser.h`), which the `windows` crate does not expose as constants.
const SS_CENTER: u32 = 0x1;
const SS_CENTERIMAGE: u32 = 0x200;

/// What the worker is told: a change in the save folder, or a change of the window's options.
enum Input {
    Watch(Event),
    Options(diggle_solver::turn::Options),
}

/// The window's way to tell the worker that the options changed. Set once, before the window opens.
static TO_WORKER: OnceLock<Mutex<mpsc::Sender<Input>>> = OnceLock::new();

/// Tells the worker the options changed. A failure means it has stopped, and there is nothing to
/// re-rank.
fn tell_worker(options: diggle_solver::turn::Options) {
    if let Some(tx) = TO_WORKER.get() {
        let _ = tx.lock().unwrap().send(Input::Options(options));
    }
}

/// The options [`Settings`] holds, as the turn planner takes them.
fn options_of(s: &Settings) -> diggle_solver::turn::Options {
    diggle_solver::turn::Options { stall_on_free_turns: s.stall_on_free_turns }
}

/// The latest view, written by the worker and read by the window.
static VIEW: OnceLock<Mutex<View>> = OnceLock::new();

fn view() -> &'static Mutex<View> {
    VIEW.get_or_init(|| Mutex::new(View::default()))
}

struct Ui {
    status: HWND,
    notes: HWND,
    topmost: HWND,
    log_toggle: HWND,
    stall: HWND,
    /// "Underline mouse tiles". Its tooltip is owned by the main window and goes with it.
    underline: HWND,
    list: HWND,
    /// The tooltip over the list's items: one tool, moved onto whichever item is under the mouse.
    /// See [`hover`].
    list_tip: HWND,
    banner: HWND,
    tile_font: HFONT,
    wildcard_font: HFONT,
    /// Direct2D text for the cells that carry emoji. `None` if it would not start, and those
    /// cells fall back to the list's own GDI text, emoji in monochrome.
    colour_text: Option<ColourText>,
    /// The window's icon out of combat and in a fight; see [`WINDOW_ICON`]. `None` if one could
    /// not be made, and the window keeps the icon it has.
    window_icons: [Option<WindowIcon>; 2],
    /// Which of [`Self::window_icons`] is on the window: `Some(true)` the fight's.
    icon_shown: std::cell::Cell<Option<bool>>,
}

/// The window's remembered settings and the state of a splitter drag. Window thread only.
struct State {
    settings: Settings,
    /// Where they are saved; `None` when `LOCALAPPDATA` is unset, and they are not kept.
    file: Option<PathBuf>,
    /// Settings changed since the last write.
    dirty: bool,
    /// While the gap under the log is being dragged: how far below the log's bottom edge the
    /// cursor was when the drag began, so the edge does not jump to the cursor.
    drag: Option<i32>,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

fn settings_now() -> Settings {
    STATE.with(|s| s.borrow().as_ref().map(|s| s.settings.clone()).unwrap_or_default())
}

/// Changes the settings and starts (or restarts) the save timer, if `change` changed them.
unsafe fn update(hwnd: HWND, change: impl FnOnce(&mut Settings)) {
    let changed = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let Some(s) = s.as_mut() else { return false };
        let before = s.settings.clone();
        change(&mut s.settings);
        let changed = s.settings != before;
        s.dirty |= changed;
        changed
    });
    if changed {
        // Setting a timer that is already running restarts it: this is the debounce.
        SetTimer(hwnd, ID_SAVE, SAVE_DELAY_MS, None);
    }
}

/// Writes the settings if they changed since the last write. A failure is dropped: losing a
/// window size is not worth interrupting the player for.
fn save_now() {
    STATE.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut().filter(|s| s.dirty) {
            s.dirty = false;
            if let Some(file) = &s.file {
                let _ = s.settings.save(file);
            }
        }
    });
}

/// Columns: header, width, right-aligned.
///
/// The second, unnamed, is the 🖱️ mark on a row with a tile to click — the user's design,
/// 2026-11-07: a column of its own between the rank and the tiles, empty on most rows.
const COLUMNS: [(&str, i32, bool); 8] = [
    ("#", 44, true),
    ("", 30, false),
    ("Word", 470, false),
    ("Damage", 84, true),
    ("Bonus", 90, false),
    ("Gain", 120, true),
    ("Spell", 120, false),
    ("Effect", 190, false),
];
const WORD_COLUMN: i32 = 2;
const MOUSE_COLUMN: i32 = 1;
/// Lettering on the word column's tiles, in pixels. Bigger than the rest of the list on purpose:
/// the word is what the player reads.
const TILE_TEXT: i32 = 26;
/// Row height, to fit [`TILE_TEXT`] on a tile with a little margin.
const ROW_HEIGHT: i32 = 36;
/// "Underline mouse tiles": the green bar's gap under the tile, its height, and the margin below
/// it. The rows grow by all three while the box is ticked — the user's call, 2026-11-07, for
/// option A of the mockups with a margin under the bar.
const UNDERLINE_GAP: i32 = 2;
const UNDERLINE_BAR: i32 = 3;
const UNDERLINE_MARGIN: i32 = 3;
/// The bar's green: bright and a little yellow, clear of every tile face (the nearest, Turquoise
/// Chrysocolla, is a blue-green). The user's choice of colour, 2026-11-07.
const UNDERLINE: (u8, u8, u8) = (0, 192, 64);
const UNDERLINE_TIP: &str = "Draws a green bar under each tile the keyboard would not pick.\nType the word in order and click the underlined tiles instead.";

/// How tall the rows are, with or without the underline's room.
fn row_height(underline: bool) -> i32 {
    match underline {
        true => ROW_HEIGHT + UNDERLINE_GAP + UNDERLINE_BAR + UNDERLINE_MARGIN,
        false => ROW_HEIGHT,
    }
}

/// Sets the list's row height. A ListView's rows are as tall as its small icons, so this swaps in
/// an empty image list of that height; re-sending the font is what makes the list measure again.
unsafe fn set_row_height(list: HWND, height: i32, font: WPARAM) {
    let rows = ImageList_Create(1, height, ILC_COLOR32, 0, 0);
    let old = SendMessageW(list, LVM_SETIMAGELIST, WPARAM(LVSIL_SMALL as usize), LPARAM(rows.0));
    if old.0 != 0 {
        let _ = ImageList_Destroy(HIMAGELIST(old.0));
    }
    SendMessageW(list, WM_SETFONT, font, LPARAM(1));
}

struct Args {
    game_dir: PathBuf,
    dirs: Vec<PathBuf>,
    /// `--timing-log`: write [`telemetry`]'s log.
    timing_log: bool,
}

fn args() -> Args {
    let mut game_dir = None;
    let mut dirs = Vec::new();
    let mut timing_log = false;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--game-dir" => game_dir = it.next().map(PathBuf::from),
            "--dir" => dirs.extend(it.next().map(PathBuf::from)),
            "--timing-log" => timing_log = true,
            _ => {}
        }
    }
    let game_dir = game_dir
        .or_else(|| {
            diggle_solver::config::Config::load(std::path::Path::new("config.toml"))
                .ok()
                .map(|c| c.game_dir)
        })
        .unwrap_or_else(|| PathBuf::from("../sternly-worded-adventures"));
    if dirs.is_empty() {
        if let Ok(appdata) = std::env::var("APPDATA") {
            dirs = watch::save_dirs(&PathBuf::from(appdata));
        }
    }
    Args { game_dir, dirs, timing_log }
}

/// When the last view was published, so the window can log how long it waited to draw it.
static PUBLISHED: Mutex<Option<std::time::Instant>> = Mutex::new(None);

/// Publishes a view and tells the window. `hwnd` is carried as an integer so it can cross threads.
fn publish(hwnd: isize, v: View) {
    *view().lock().unwrap() = v;
    *PUBLISHED.lock().unwrap() = Some(std::time::Instant::now());
    // SAFETY: posting to our own window; a failure only means it has closed.
    unsafe {
        let _ = PostMessageW(HWND(hwnd as _), WM_VIEW, WPARAM(0), LPARAM(0));
    }
}

/// Loads the game data, watches the folders, and turns each change into a [`View`].
///
/// Events arriving while a turn is being ranked are coalesced: only the newest snapshot is ranked,
/// and a result whose save has already moved on is dropped rather than shown.
///
/// A change of options re-ranks the last snapshot, so ticking a box changes the list on screen
/// without waiting for the game to write again. Options are never coalesced away: each is applied
/// as it arrives, and only the ranking it asks for is shared with a snapshot arriving alongside.
fn worker(
    hwnd: isize, args: Args, tx: mpsc::Sender<Input>, rx: mpsc::Receiver<Input>,
    mut options: diggle_solver::turn::Options,
) {
    let loading = std::time::Instant::now();
    let res = match Resources::load(&args.game_dir) {
        Ok(r) => {
            telemetry::note(
                "work",
                format_args!("game data loaded in {}", telemetry::since(loading)),
            );
            r
        }
        Err(e) => {
            let header = format!("cannot load the game data from {}: {e}", args.game_dir.display());
            return publish(hwnd, View { header, ..View::default() });
        }
    };
    if args.dirs.is_empty() {
        return publish(hwnd, View { header: "APPDATA is not set".into(), ..View::default() });
    }
    // The watcher speaks `Event`; this thread listens for `Input`. One forwarding thread joins
    // them, and ends when either side hangs up.
    let (events, from_watch) = mpsc::channel();
    watch::spawn(args.dirs, watch::arcade_saves(&args.game_dir), events);
    std::thread::spawn(move || {
        for e in from_watch {
            if tx.send(Input::Watch(e)).is_err() {
                return;
            }
        }
    });
    let mut peaks: HashMap<String, i64> = HashMap::new();
    let mut current = View::default();
    // The last snapshot ranked, for re-ranking it when the options change.
    let mut last: Option<diggle_solver::advisor::mode::Snapshot> = None;
    let mut pending: Option<Input> = None;
    // Watch events received, numbered as the watcher numbers them (`watch::SENT`): one channel,
    // in order.
    let mut got = 0u64;
    loop {
        let Some(first) = pending.take().or_else(|| rx.recv().ok()) else { return };
        let mut event: Option<Event> = None;
        let mut rerank = false;
        let mut skipped = Vec::new();
        for input in std::iter::once(first).chain(std::iter::from_fn(|| rx.try_recv().ok())) {
            match input {
                Input::Watch(e) => {
                    got += 1;
                    if event.is_some() {
                        skipped.push(got - 1);
                    }
                    event = Some(e)
                }
                Input::Options(o) => {
                    rerank |= o != options;
                    options = o;
                }
            }
        }
        let event = match (event, rerank, &last) {
            (Some(e), ..) => e,
            (None, true, Some(snap)) => Event::Snapshot(snap.clone()),
            _ => continue,
        };
        if !skipped.is_empty() {
            telemetry::note("work", format_args!("skipping #{skipped:?}, a newer one is queued"));
        }
        match event {
            Event::Snapshot(snap) => {
                let working = std::time::Instant::now();
                let next = View::next(&current, &snap, &res, &mut peaks, options);
                telemetry::note(
                    "work",
                    format_args!(
                        "#{got}{}: {:?} in {}",
                        if rerank { " re-ranked for new options" } else { "" },
                        next.mode,
                        telemetry::since(working)
                    ),
                );
                last = Some(snap);
                if let Ok(newer) = rx.try_recv() {
                    let stale = match &newer {
                        Input::Watch(e) => {
                            e.supersedes_a_turn() && !matches!(next.mode, Mode::OutOfCombat)
                        }
                        // Ranked under the options just replaced, so not worth showing.
                        Input::Options(o) => *o != options,
                    };
                    pending = Some(newer);
                    // Peaks only rise within a fight, so the stale turn's reading stays true.
                    if stale {
                        telemetry::note(
                            "work",
                            format_args!("#{got} dropped unshown: something newer arrived"),
                        );
                        continue;
                    }
                }
                current = next;
                publish(hwnd, current.clone());
            }
            Event::Unreadable { dir, error } => publish(hwnd, current.unreadable(&dir, &error)),
        }
    }
}

fn main() -> windows::core::Result<()> {
    let args = args();
    let local = std::env::var_os("LOCALAPPDATA");
    if let (true, Some(d)) = (args.timing_log, &local) {
        telemetry::init(&telemetry::path(std::path::Path::new(d)));
    }
    let file = local.map(|d| settings::path(std::path::Path::new(&d)));
    let saved = file.as_deref().map(Settings::load).unwrap_or_default();
    STATE.with(|s| {
        *s.borrow_mut() = Some(State { settings: saved.clone(), file, dirty: false, drag: None })
    });
    // SAFETY: standard Win32 window set-up and message loop, all on this thread.
    unsafe {
        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            // Tab classes register the tooltip, which the underline box carries.
            dwICC: ICC_LISTVIEW_CLASSES | ICC_STANDARD_CLASSES | ICC_TAB_CLASSES,
        };
        let _ = InitCommonControlsEx(&icc);
        let instance = GetModuleHandleW(None)?;
        let class = w!("DiggleAdvisor");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hbrBackground: GetSysColorBrush(COLOR_BTNFACE),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            if saved.topmost { WS_EX_TOPMOST } else { WINDOW_EX_STYLE(0) },
            class,
            w!("Diggle Advisor"),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            saved.width,
            saved.height,
            None,
            None,
            instance,
            None,
        )?;
        let raw = hwnd.0 as isize;
        let (tx, rx) = mpsc::channel();
        let _ = TO_WORKER.set(Mutex::new(tx.clone()));
        let options = options_of(&saved);
        std::thread::spawn(move || worker(raw, args, tx, rx, options));
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

unsafe fn child(
    parent: HWND, class: PCWSTR, text: PCWSTR, style: u32, id: usize,
) -> windows::core::Result<HWND> {
    CreateWindowExW(
        WINDOW_EX_STYLE(0),
        class,
        text,
        WS_CHILD | WS_VISIBLE | WINDOW_STYLE(style),
        0,
        0,
        0,
        0,
        parent,
        HMENU(id as _),
        GetModuleHandleW(None)?,
        None,
    )
}

unsafe fn create(hwnd: HWND) -> windows::core::Result<Ui> {
    let font = CreateFontW(-15, 0, 0, 0, 400, 0, 0, 0, 0, 0, 0, 0, 0, w!("Segoe UI"));
    let tile_font = CreateFontW(-TILE_TEXT, 0, 0, 0, 700, 0, 0, 0, 0, 0, 0, 0, 0, w!("Segoe UI"));
    // Wildcards are scribbled in the game (`ui/graphics/tiles/scribble_*.png`). Segoe Print is a
    // handwriting face that has shipped with Windows since Vista.
    let wildcard_font =
        CreateFontW(-TILE_TEXT, 0, 0, 0, 700, 0, 0, 0, 0, 0, 0, 0, 0, w!("Segoe Print"));
    let banner_font = CreateFontW(-28, 0, 0, 0, 600, 0, 0, 0, 0, 0, 0, 0, 0, w!("Segoe UI"));
    let status = child(hwnd, w!("STATIC"), w!(""), 0, 0)?;
    let notes = child(
        hwnd,
        w!("EDIT"),
        w!(""),
        (ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32 | WS_VSCROLL.0 | WS_BORDER.0,
        0,
    )?;
    let topmost = child(
        hwnd,
        w!("BUTTON"),
        w!("Stay on top"),
        BS_AUTOCHECKBOX as u32 | WS_TABSTOP.0,
        ID_TOPMOST,
    )?;
    let log_toggle =
        child(hwnd, w!("BUTTON"), w!("Show log"), BS_AUTOCHECKBOX as u32 | WS_TABSTOP.0, ID_LOG)?;
    let stall = child(
        hwnd,
        w!("BUTTON"),
        w!("Stall on halfling's even turns"),
        BS_AUTOCHECKBOX as u32 | WS_TABSTOP.0,
        ID_STALL,
    )?;
    let underline = child(
        hwnd,
        w!("BUTTON"),
        w!("Underline mouse tiles"),
        BS_AUTOCHECKBOX as u32 | WS_TABSTOP.0,
        ID_UNDERLINE,
    )?;
    let saved = settings_now();
    for (h, on) in [
        (topmost, saved.topmost),
        (log_toggle, saved.log_open),
        (stall, saved.stall_on_free_turns),
        (underline, saved.underline_mouse_tiles),
    ] {
        let check = if on { BST_CHECKED.0 } else { 0 };
        SendMessageW(h, BM_SETCHECK, WPARAM(check as usize), LPARAM(0));
    }
    let list = child(
        hwnd,
        WC_LISTVIEWW,
        w!(""),
        LVS_REPORT | LVS_OWNERDATA | LVS_NOSORTHEADER | WS_BORDER.0 | WS_TABSTOP.0,
        0,
    )?;
    let banner = child(hwnd, w!("STATIC"), w!("Out of combat"), SS_CENTER | SS_CENTERIMAGE, 0)?;
    for h in [status, notes, topmost, log_toggle, stall, underline, list] {
        SendMessageW(h, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
    }
    SendMessageW(banner, WM_SETFONT, WPARAM(banner_font.0 as usize), LPARAM(1));
    // A ListView's rows are as tall as its small icons. An empty one-pixel-wide image list is the
    // way to make rows tall enough for the tiles without enlarging every other column's text.
    set_row_height(list, row_height(saved.underline_mouse_tiles), WPARAM(font.0 as usize));
    tooltip_for(hwnd, underline, UNDERLINE_TIP)?;
    let list_tip = item_tooltip(hwnd, list)?;
    SendMessageW(
        list,
        LVM_SETEXTENDEDLISTVIEWSTYLE,
        WPARAM(0),
        LPARAM((LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER) as isize),
    );
    for (i, (name, width, right)) in COLUMNS.iter().enumerate() {
        let mut text: Vec<u16> = name.encode_utf16().chain([0]).collect();
        let col = LVCOLUMNW {
            mask: LVCF_TEXT | LVCF_WIDTH | LVCF_FMT,
            fmt: if *right { LVCFMT_RIGHT } else { LVCFMT_LEFT },
            cx: *width,
            pszText: PWSTR(text.as_mut_ptr()),
            ..Default::default()
        };
        SendMessageW(list, LVM_INSERTCOLUMNW, WPARAM(i), LPARAM(&col as *const _ as isize));
    }
    let ui = Ui {
        status,
        notes,
        topmost,
        log_toggle,
        stall,
        underline,
        list,
        list_tip,
        banner,
        tile_font,
        wildcard_font,
        colour_text: ColourText::new().ok(),
        window_icons: [WindowIcon::new(WINDOW_ICON), WindowIcon::new(WINDOW_ICON_COMBAT)],
        icon_shown: std::cell::Cell::new(None),
    };
    // The window opens out of combat, on the banner, until the first save says otherwise.
    show_icon(hwnd, &ui, false);
    Ok(ui)
}

/// The list's item tooltip: one tool on the list, with no area until [`hover`] gives it the
/// item under the mouse, and the list subclassed so that [`hover`] sees the mouse first.
///
/// Relayed rather than `TTF_SUBCLASS`, so the tool is moved before the tooltip sees the move that
/// moved it: it then hides as the mouse leaves one item, and shows after its usual delay on the
/// next, as if each item were a control of its own.
unsafe fn item_tooltip(owner: HWND, list: HWND) -> windows::core::Result<HWND> {
    let tip = CreateWindowExW(
        WS_EX_TOPMOST,
        TOOLTIPS_CLASSW,
        None,
        WS_POPUP | WINDOW_STYLE(TTS_ALWAYSTIP | TTS_NOPREFIX),
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        owner,
        None,
        GetModuleHandleW(None)?,
        None,
    )?;
    let mut empty = [0u16];
    let info = tool_info(list, RECT::default(), PWSTR(empty.as_mut_ptr()));
    SendMessageW(tip, TTM_ADDTOOLW, WPARAM(0), LPARAM(&info as *const _ as isize));
    SendMessageW(tip, TTM_SETMAXTIPWIDTH, WPARAM(0), LPARAM(480));
    let old = SetWindowLongPtrW(list, GWLP_WNDPROC, list_proc as *const () as isize);
    LIST_PROC.store(old, std::sync::atomic::Ordering::Relaxed);
    Ok(tip)
}

/// The one tool [`item_tooltip`] keeps on the list, at `rect` in the list's client coordinates.
/// `cbSize` is version 5's, as in [`tooltip_for`].
fn tool_info(list: HWND, rect: RECT, text: PWSTR) -> TTTOOLINFOW {
    TTTOOLINFOW {
        cbSize: (std::mem::size_of::<TTTOOLINFOW>() - std::mem::size_of::<*mut core::ffi::c_void>())
            as u32,
        uFlags: TOOLTIP_FLAGS(0),
        hwnd: list,
        uId: 1,
        rect,
        lpszText: text,
        ..Default::default()
    }
}

/// The list's own window procedure, which [`list_proc`] passes everything on to.
static LIST_PROC: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

thread_local! {
    /// The item the tooltip is on: row, column and item index. `None` when it is on nothing.
    static HOVERED: std::cell::Cell<Option<(usize, i32, usize)>> = const { std::cell::Cell::new(None) };
}

unsafe extern "system" fn list_proc(
    hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM,
) -> LRESULT {
    if (WM_MOUSEFIRST..=WM_MOUSELAST).contains(&msg) || msg == WM_MOUSELEAVE {
        // `try_borrow`: the list is sent messages while the window procedure holds the UI.
        UI.with(|u| {
            if let Ok(ui) = u.try_borrow() {
                if let Some(ui) = ui.as_ref() {
                    hover(ui, msg, wparam, lparam);
                }
            }
        });
    }
    let old: WNDPROC = std::mem::transmute(LIST_PROC.load(std::sync::atomic::Ordering::Relaxed));
    CallWindowProcW(old, hwnd, msg, wparam, lparam)
}

/// Moves the item tooltip onto the item under the mouse, then hands the tooltip the mouse message.
unsafe fn hover(ui: &Ui, msg: u32, wparam: WPARAM, lparam: LPARAM) {
    let found = if msg == WM_MOUSELEAVE {
        None
    } else {
        if msg == WM_MOUSEMOVE {
            let mut track = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: ui.list,
                dwHoverTime: 0,
            };
            let _ = TrackMouseEvent(&mut track);
        }
        let x = (lparam.0 & 0xffff) as i16 as i32;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
        item_at(ui, x, y)
    };
    let key = found.as_ref().map(|f| f.0);
    if key != HOVERED.with(|h| h.get()) {
        HOVERED.with(|h| h.set(key));
        let (rect, tip) = found.map(|f| (f.1, f.2)).unwrap_or_default();
        let mut text: Vec<u16> = tip.encode_utf16().chain([0]).collect();
        let info = tool_info(ui.list, rect, PWSTR(text.as_mut_ptr()));
        SendMessageW(ui.list_tip, TTM_POP, WPARAM(0), LPARAM(0));
        SendMessageW(ui.list_tip, TTM_NEWTOOLRECTW, WPARAM(0), LPARAM(&info as *const _ as isize));
        SendMessageW(
            ui.list_tip,
            TTM_UPDATETIPTEXTW,
            WPARAM(0),
            LPARAM(&info as *const _ as isize),
        );
    }
    if msg != WM_MOUSELEAVE {
        let relay = MSG {
            hwnd: ui.list,
            message: msg,
            wParam: wparam,
            lParam: lparam,
            ..Default::default()
        };
        SendMessageW(ui.list_tip, TTM_RELAYEVENT, WPARAM(0), LPARAM(&relay as *const _ as isize));
    }
}

/// The item with a tooltip at `(x, y)` in the list: which it is, where it is drawn, and its tip.
unsafe fn item_at(ui: &Ui, x: i32, y: i32) -> Option<((usize, i32, usize), RECT, String)> {
    let colour = ui.colour_text.as_ref()?;
    let mut hit =
        LVHITTESTINFO { pt: windows::Win32::Foundation::POINT { x, y }, ..Default::default() };
    let row =
        SendMessageW(ui.list, LVM_SUBITEMHITTEST, WPARAM(0), LPARAM(&mut hit as *mut _ as isize)).0;
    let sub = hit.iSubItem;
    if row < 0 || !EMOJI_COLUMNS.contains(&sub) {
        return None;
    }
    let items = cell_items(&view().lock().unwrap(), row as usize, sub);
    let band = text_band(ui, row as usize, sub);
    let right = COLUMNS.get(sub as usize).is_some_and(|c| c.2);
    colour
        .item_rects(&band, &items, right)
        .into_iter()
        .zip(items)
        .enumerate()
        .find(|(_, (rc, item))| {
            !item.tip.is_empty() && x >= rc.left && x < rc.right && y >= rc.top && y < rc.bottom
        })
        .map(|(i, (rc, item))| ((row as usize, sub, i), rc, item.tip))
}

/// Where a ColourText cell's text is drawn: the band the tiles fill, at the top of the row.
unsafe fn text_band(ui: &Ui, row: usize, sub: i32) -> RECT {
    let rc = cell_rect(ui.list, row, sub);
    RECT { bottom: rc.bottom.min(rc.top + ROW_HEIGHT), ..rc }
}

/// One cell's rectangle in the list's client coordinates.
///
/// `LVM_GETSUBITEMRECT` with `LVIR_BOUNDS` answers column 0 with the **whole row**, so its right
/// edge is cut back to the column's own width. Without that, the `#` column's number, flush right,
/// was drawn at the far right of the row and painted over by the Effect column.
unsafe fn cell_rect(list: HWND, row: usize, sub: i32) -> RECT {
    let mut rc = RECT { top: sub, left: LVIR_BOUNDS as i32, ..Default::default() };
    SendMessageW(list, LVM_GETSUBITEMRECT, WPARAM(row), LPARAM(&mut rc as *mut _ as isize));
    if sub == 0 {
        let width = SendMessageW(list, LVM_GETCOLUMNWIDTH, WPARAM(0), LPARAM(0)).0 as i32;
        rc.right = rc.left + width;
    }
    rc
}

/// A tooltip on `control`, shown while the mouse is over it — a checkbox's label is part of the
/// checkbox, so this covers both.
///
/// The window has no manifest, so it gets comctl32 version 5, whose `TTTOOLINFOW` ends before
/// `lpReserved`: that version refuses the full-size structure. `cbSize` is the shorter one, which
/// version 6 accepts too.
unsafe fn tooltip_for(owner: HWND, control: HWND, text: &str) -> windows::core::Result<HWND> {
    let tip = CreateWindowExW(
        WS_EX_TOPMOST,
        TOOLTIPS_CLASSW,
        None,
        WS_POPUP | WINDOW_STYLE(TTS_ALWAYSTIP | TTS_NOPREFIX),
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        owner,
        None,
        GetModuleHandleW(None)?,
        None,
    )?;
    // Kept for the tooltip's lifetime: it holds the pointer, not a copy. Leaked once per window.
    let text: &'static mut [u16] = text.encode_utf16().chain([0]).collect::<Vec<_>>().leak();
    let info = TTTOOLINFOW {
        cbSize: (std::mem::size_of::<TTTOOLINFOW>() - std::mem::size_of::<*mut core::ffi::c_void>())
            as u32,
        uFlags: TTF_IDISHWND | TTF_SUBCLASS,
        hwnd: owner,
        uId: control.0 as usize,
        lpszText: PWSTR(text.as_mut_ptr()),
        ..Default::default()
    };
    SendMessageW(tip, TTM_ADDTOOLW, WPARAM(0), LPARAM(&info as *const _ as isize));
    // Wide enough for each line of the text; the line break is the text's own.
    SendMessageW(tip, TTM_SETMAXTIPWIDTH, WPARAM(0), LPARAM(480));
    // Long enough to read two lines.
    SendMessageW(tip, TTM_SETDELAYTIME, WPARAM(TTDT_AUTOPOP as usize), LPARAM(15_000));
    Ok(tip)
}

const PAD: i32 = 8;
/// The header row: the header text, and the three checkboxes on its right.
const HEADER_HEIGHT: i32 = 40;
const TOPMOST_WIDTH: i32 = 120;
const LOG_TOGGLE_WIDTH: i32 = 100;
const STALL_WIDTH: i32 = 230;
const UNDERLINE_WIDTH: i32 = 180;
/// The list keeps at least this much height however tall the log is dragged.
const MIN_LIST_HEIGHT: i32 = 80;

/// The log box's top and height in a client area `h` tall, when it is shown: its saved height,
/// squeezed if the window is too short for it and a usable list.
fn log_band(h: i32, saved: i32) -> (i32, i32) {
    let top = PAD + HEADER_HEIGHT + 4;
    let most = (h - top - PAD - PAD - MIN_LIST_HEIGHT).max(settings::MIN_LOG_HEIGHT);
    (top, saved.clamp(settings::MIN_LOG_HEIGHT, most))
}

fn client_height(hwnd: HWND) -> i32 {
    let mut rc = RECT::default();
    // SAFETY: reads our own window's client rectangle.
    let _ = unsafe { GetClientRect(hwnd, &mut rc) };
    rc.bottom - rc.top
}

/// Whether client point `y` is on the gap under the shown log: the splitter.
fn on_splitter(hwnd: HWND, y: i32) -> bool {
    let s = settings_now();
    let (top, height) = log_band(client_height(hwnd), s.log_height);
    s.log_open && y >= top + height && y < top + height + PAD
}

unsafe fn layout(hwnd: HWND, ui: &Ui) {
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
    let s = settings_now();
    let boxes = UNDERLINE_WIDTH + PAD + STALL_WIDTH + PAD + LOG_TOGGLE_WIDTH + PAD + TOPMOST_WIDTH;
    let _ = MoveWindow(ui.status, PAD, PAD, (w - 3 * PAD - boxes).max(0), HEADER_HEIGHT, true);
    let _ = MoveWindow(ui.underline, w - PAD - boxes, PAD, UNDERLINE_WIDTH, 24, true);
    let stall_left = w - PAD - boxes + UNDERLINE_WIDTH + PAD;
    let _ = MoveWindow(ui.stall, stall_left, PAD, STALL_WIDTH, 24, true);
    let log_left = w - PAD - TOPMOST_WIDTH - PAD - LOG_TOGGLE_WIDTH;
    let _ = MoveWindow(ui.log_toggle, log_left, PAD, LOG_TOGGLE_WIDTH, 24, true);
    let _ = MoveWindow(ui.topmost, w - PAD - TOPMOST_WIDTH, PAD, TOPMOST_WIDTH, 24, true);
    let (log_top, log_height) = log_band(h, s.log_height);
    let top = if s.log_open {
        let _ = MoveWindow(ui.notes, PAD, log_top, (w - 2 * PAD).max(0), log_height, true);
        log_top + log_height + PAD
    } else {
        log_top
    };
    let _ = ShowWindow(ui.notes, if s.log_open { SW_SHOW } else { SW_HIDE });
    for c in [ui.list, ui.banner] {
        let _ = MoveWindow(c, PAD, top, (w - 2 * PAD).max(0), (h - top - PAD).max(0), true);
    }
    // The system STATIC class has neither CS_HREDRAW nor CS_VREDRAW, so a resize repaints only
    // the strip it uncovers. The banner's text is centred and the header's wraps, so both belong
    // somewhere new after a resize: without this, shrinking left the banner's text where the old
    // centre was (off the bottom, once the window was short enough) and growing did not bring it
    // back.
    for c in [ui.banner, ui.status] {
        let _ = InvalidateRect(c, None, true);
    }
}

/// Re-lays the window out, from inside a message handler.
unsafe fn relayout(hwnd: HWND) {
    UI.with(|u| {
        if let Some(ui) = u.borrow().as_ref() {
            layout(hwnd, ui);
        }
    });
}

/// The client y out of a mouse message's `lparam`, signed: it goes negative above the window
/// while the mouse is captured.
fn mouse_y(lparam: LPARAM) -> i32 {
    ((lparam.0 >> 16) & 0xffff) as i16 as i32
}

/// Shows the newest view: the panel for its mode, and the text around it.
///
/// One panel per mode. A shrine panel is the next one to add here.
unsafe fn refresh(hwnd: HWND, ui: &Ui) {
    let v = view().lock().unwrap().clone();
    let title = match v.folder.as_str() {
        "" => "Diggle Advisor".to_string(),
        f => format!("Diggle Advisor - {f}"),
    };
    let _ = SetWindowTextW(hwnd, &HSTRING::from(title.as_str()));
    let _ = SetWindowTextW(ui.status, &HSTRING::from(v.header.as_str()));
    let _ = SetWindowTextW(ui.notes, &HSTRING::from(v.notes.as_str()));
    let show_list = !matches!(v.mode, Mode::OutOfCombat);
    show_icon(hwnd, ui, show_list);
    let _ = ShowWindow(ui.list, if show_list { SW_SHOW } else { SW_HIDE });
    let _ = ShowWindow(ui.banner, if show_list { SW_HIDE } else { SW_SHOW });
    SendMessageW(ui.list, LVM_SETITEMCOUNT, WPARAM(v.rows.len()), LPARAM(0));
    fit_word_column(ui, &v);
    let _ = InvalidateRect(ui.list, None, true);
}

fn colorref((r, g, b): (u8, u8, u8)) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

/// Halfway to light grey: how a stale list's tiles and rings are drawn.
fn faded((r, g, b): (u8, u8, u8)) -> (u8, u8, u8) {
    let mix = |c: u8| ((c as u16 + 200) / 2) as u8;
    (mix(r), mix(g), mix(b))
}

/// The columns drawn by [`ColourText`]: every one but the word. Most carry emoji — the 🖱️ mark,
/// Damage (the rapier's 🤺), Bonus, Gain (❤️ and 🛡️) and Spell — and drawing the rest the same
/// way keeps all the text level with the tiles when the rows grow for the underline.
const EMOJI_COLUMNS: [i32; 7] = [0, 1, 3, 4, 5, 6, 7];

/// Colour emoji, which GDI cannot draw: GDI renders a colour font's glyphs in one colour, so the
/// list's own text would show 🔥 as a black outline. Direct2D's text drawing with
/// `D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT` draws them in colour, onto the list's own device
/// context through a DC render target.
struct ColourText {
    target: ID2D1DCRenderTarget,
    write: IDWriteFactory,
    format: IDWriteTextFormat,
    /// The same, set flush right, for the right-aligned columns.
    right: IDWriteTextFormat,
    /// The item icons that stand in for their emoji, each with the emoji it replaces. Made on the
    /// first draw, once the target is bound; empty if they could not be made, and then the emoji
    /// show as they always did.
    icons: RefCell<Option<Vec<(Vec<u16>, IDWriteInlineObject)>>>,
}

/// The game's own art for the two infinitive items, drawn in place of their emoji (🧤 ⚖️), which
/// look nothing like them: `ui/graphics/items/gauntlet-inf-32.png` (`items/infinitives.lua:236`)
/// and `ui/graphics/items/scales-justice-32.png` (`items/scalesofjustice.lua:64`). Copied into
/// `assets/advisor/` and built into the exe, so the Advisor needs no game checkout to show them.
/// Every gauntlet shows the full gauntlet, even a partial one with its own art
/// (`gauntlet-inf-01111-32.png` and the rest, `items/infinitives.lua:323`).
const ICONS: [(diggle_solver::pick::InfinitiveSource, &[u8]); 2] = [
    (
        diggle_solver::pick::InfinitiveSource::Gauntlet,
        include_bytes!("../../assets/advisor/gauntlet-inf-32.png"),
    ),
    (
        diggle_solver::pick::InfinitiveSource::Justice,
        include_bytes!("../../assets/advisor/scales-justice-32.png"),
    ),
];

/// How big an item icon is drawn, in pixels: the size of the emoji around it, the user's call,
/// 2026-11-07. Measured on screen, the list's emoji draw up to 15 px (💀 15×15, 🔥 12×15), and
/// the scales fill their whole 32 px square, so 16 is within a pixel. It is also exactly half the
/// art, so the linear filter averages each 2×2 block evenly; 15 would scale by 2.13 and blur more.
/// 32 was tried: faithful to the last pixel, but twice the height of the emoji.
const ICON_PX: f32 = 16.0;

/// A PNG as straight (not premultiplied) RGBA: (width, height, pixels).
fn decode_png(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    // The item icons are paletted with a transparent entry: EXPAND turns that into RGBA.
    decoder.set_transformations(
        png::Transformations::EXPAND | png::Transformations::STRIP_16 | png::Transformations::ALPHA,
    );
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    buf.truncate(info.buffer_size());
    Some((info.width, info.height, buf))
}

/// A PNG as premultiplied BGRA, the layout a Direct2D bitmap takes: (width, height, pixels).
fn decode_icon(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let (width, height, mut buf) = decode_png(bytes)?;
    for px in buf.chunks_exact_mut(4) {
        let a = px[3] as u16;
        let pre = |c: u8| ((c as u16 * a + 127) / 255) as u8;
        let (r, g, b) = (pre(px[0]), pre(px[1]), pre(px[2]));
        px.copy_from_slice(&[b, g, r, px[3]]);
    }
    Some((width, height, buf))
}

/// The window's icon: a robot inside the game's crystal ball, the item that shows the best-scoring
/// word (`items/activatedgear.lua:5-13`). Drawn for the Advisor on the ball's own art,
/// `ui/graphics/items/crystal-ball-32.png` (`items/activatedgear.lua:22`): the robot after the
/// Apple and Samsung 🤖 designs, in colours that stand out against the glass, with the ball's gold
/// stand redrawn in front of it so it sits inside. Out of combat it is [`WINDOW_ICON`], the robot
/// at 65% over the glass with the ball's reflections across it; in a fight it is
/// [`WINDOW_ICON_COMBAT`], solid and glowing. The user's design, 2026-11-08.
const WINDOW_ICON: &[u8] = include_bytes!("../../assets/advisor/crystal-ball-robot-32.png");
const WINDOW_ICON_COMBAT: &[u8] =
    include_bytes!("../../assets/advisor/crystal-ball-robot-glowing-32.png");

/// One design at the two sizes Windows asks a window for: 32 px for the taskbar and Alt+Tab, 16 px
/// for the title bar. The window has no DPI manifest, so Windows asks for these two at any display
/// scale and stretches the whole window itself.
struct WindowIcon {
    big: HICON,
    small: HICON,
}

impl WindowIcon {
    /// `None` if the PNG does not decode or Windows will not make the icon. The window then keeps
    /// the default icon.
    unsafe fn new(png: &[u8]) -> Option<Self> {
        let (w, h, rgba) = decode_png(png)?;
        let big = make_icon(w, h, &rgba)?;
        let (sw, sh, small) = halve(w, h, &rgba);
        match make_icon(sw, sh, &small) {
            Some(small) => Some(WindowIcon { big, small }),
            None => {
                let _ = DestroyIcon(big);
                None
            }
        }
    }
}

impl Drop for WindowIcon {
    fn drop(&mut self) {
        // SAFETY: both icons were made by `make_icon` and are destroyed once, here.
        unsafe {
            let _ = DestroyIcon(self.big);
            let _ = DestroyIcon(self.small);
        }
    }
}

/// Straight RGBA at half the size, each pixel the average of a 2×2 block. Colours are averaged by
/// alpha, so a clear pixel's colour does not bleed into the edge. The 16 px title-bar icon is
/// made this way rather than left to Windows, whose stretch drops three pixels in four.
fn halve(w: u32, h: u32, rgba: &[u8]) -> (u32, u32, Vec<u8>) {
    let (hw, hh) = (w / 2, h / 2);
    let mut out = Vec::with_capacity((hw * hh * 4) as usize);
    for y in 0..hh {
        for x in 0..hw {
            let mut sum = [0u32; 4];
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let i = (((2 * y + dy) * w + 2 * x + dx) * 4) as usize;
                let a = rgba[i + 3] as u32;
                for c in 0..3 {
                    sum[c] += rgba[i + c] as u32 * a;
                }
                sum[3] += a;
            }
            let a = sum[3];
            if a == 0 {
                out.extend([0; 4]);
            } else {
                let c = |s: u32| ((s + a / 2) / a) as u8;
                out.extend([c(sum[0]), c(sum[1]), c(sum[2]), ((a + 2) / 4) as u8]);
            }
        }
    }
    (hw, hh, out)
}

/// An icon from straight RGBA: a 32-bit top-down DIB with its alpha. Icons take straight alpha,
/// not the premultiplied a Direct2D bitmap does.
unsafe fn make_icon(w: u32, h: u32, rgba: &[u8]) -> Option<HICON> {
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w as i32,
            biHeight: -(h as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
    let colour = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
    if bits.is_null() {
        let _ = DeleteObject(colour);
        return None;
    }
    let dst = std::slice::from_raw_parts_mut(bits as *mut u8, (w * h * 4) as usize);
    for (d, s) in dst.chunks_exact_mut(4).zip(rgba.chunks_exact(4)) {
        d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
    }
    // Where the colour bitmap has alpha the mask is ignored, but `CreateIconIndirect` wants one.
    // Monochrome rows are padded to whole 16-bit words.
    let zeros = vec![0u8; ((w + 15) / 16 * 2 * h) as usize];
    let mask = CreateBitmap(w as i32, h as i32, 1, 1, Some(zeros.as_ptr() as *const _));
    let icon = CreateIconIndirect(&ICONINFO {
        fIcon: true.into(),
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: mask,
        hbmColor: colour,
    });
    // The icon keeps its own copies of both bitmaps.
    let _ = DeleteObject(colour);
    let _ = DeleteObject(mask);
    icon.ok()
}

/// Shows the icon for the mode: the glowing robot in a fight, the plain one out of it. Sets it
/// only when it changes, so the taskbar is not redrawn on every save.
unsafe fn show_icon(hwnd: HWND, ui: &Ui, combat: bool) {
    if ui.icon_shown.get() == Some(combat) {
        return;
    }
    let Some(icon) = ui.window_icons[combat as usize].as_ref() else { return };
    SendMessageW(hwnd, WM_SETICON, WPARAM(ICON_BIG as usize), LPARAM(icon.big.0 as isize));
    SendMessageW(hwnd, WM_SETICON, WPARAM(ICON_SMALL as usize), LPARAM(icon.small.0 as isize));
    ui.icon_shown.set(Some(combat));
}

/// An item icon set into a line of text in place of a character run: DirectWrite lays it out as
/// one glyph [`ICON_PX`] square and calls [`Self::Draw`] where it lands.
#[windows_core::implement(IDWriteInlineObject)]
struct InlineIcon {
    target: ID2D1RenderTarget,
    bitmap: ID2D1Bitmap,
}

impl IDWriteInlineObject_Impl for InlineIcon_Impl {
    fn Draw(
        &self, _context: *const std::ffi::c_void, _renderer: Option<&IDWriteTextRenderer>, x: f32,
        y: f32, _sideways: BOOL, _rtl: BOOL, _effect: Option<&windows_core::IUnknown>,
    ) -> windows::core::Result<()> {
        // Direct2D's own renderer hands over the icon's top-left corner on the target.
        let to = D2D_RECT_F { left: x, top: y, right: x + ICON_PX, bottom: y + ICON_PX };
        // SAFETY: called by DirectWrite inside the BeginDraw/EndDraw of `ColourText::draw`.
        unsafe {
            self.target.DrawBitmap(
                &self.bitmap,
                Some(&to),
                1.0,
                D2D1_BITMAP_INTERPOLATION_MODE_LINEAR,
                None,
            );
        }
        Ok(())
    }

    fn GetMetrics(&self) -> windows::core::Result<DWRITE_INLINE_OBJECT_METRICS> {
        Ok(DWRITE_INLINE_OBJECT_METRICS {
            width: ICON_PX,
            height: ICON_PX,
            // Sits on the baseline with a little below it, as the emoji beside it do.
            baseline: ICON_PX * 0.85,
            supportsSideways: BOOL(0),
        })
    }

    fn GetOverhangMetrics(&self) -> windows::core::Result<DWRITE_OVERHANG_METRICS> {
        Ok(DWRITE_OVERHANG_METRICS::default())
    }

    fn GetBreakConditions(
        &self, before: *mut DWRITE_BREAK_CONDITION, after: *mut DWRITE_BREAK_CONDITION,
    ) -> windows::core::Result<()> {
        // SAFETY: DirectWrite passes two writable out-pointers.
        unsafe {
            *before = DWRITE_BREAK_CONDITION_NEUTRAL;
            *after = DWRITE_BREAK_CONDITION_NEUTRAL;
        }
        Ok(())
    }
}

impl ColourText {
    fn new() -> windows::core::Result<Self> {
        // SAFETY: plain factory and resource creation; nothing is shared across threads.
        unsafe {
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_IGNORE,
                },
                dpiX: 0.0,
                dpiY: 0.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let target = factory.CreateDCRenderTarget(&props)?;
            let write: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            // The same face and size as the list's other columns (15 px Segoe UI, at 96 DPI).
            let make = || -> windows::core::Result<IDWriteTextFormat> {
                let format = write.CreateTextFormat(
                    w!("Segoe UI"),
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    15.0,
                    w!(""),
                )?;
                format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
                format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
                Ok(format)
            };
            let (format, right) = (make()?, make()?);
            right.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
            Ok(ColourText { target, write, format, right, icons: RefCell::new(None) })
        }
    }

    /// The item icons as inline objects, each with the emoji it stands in for. Bitmaps belong to
    /// a render target, so this runs once the target is bound. An icon that fails is left out,
    /// and its emoji shows instead.
    unsafe fn make_icons(&self) -> Vec<(Vec<u16>, IDWriteInlineObject)> {
        let Ok(target) = self.target.cast::<ID2D1RenderTarget>() else { return Vec::new() };
        let props = D2D1_BITMAP_PROPERTIES {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
        };
        ICONS
            .iter()
            .filter_map(|(source, bytes)| {
                let (w, h, pixels) = decode_icon(bytes)?;
                let bitmap = target
                    .CreateBitmap(
                        D2D_SIZE_U { width: w, height: h },
                        Some(pixels.as_ptr() as *const _),
                        w * 4,
                        &props,
                    )
                    .ok()?;
                let icon: IDWriteInlineObject =
                    InlineIcon { target: target.clone(), bitmap }.into();
                Some((source.emoji().encode_utf16().collect(), icon))
            })
            .collect()
    }

    /// The layout `draw` draws for `units` in a `w` by `h` cell, the item icons set in place of
    /// their emoji once they have been made. Also what [`ColourText::item_rects`] measures, so a
    /// tooltip's area is where the item is drawn.
    unsafe fn layout(
        &self, units: &[u16], w: f32, h: f32, right: bool,
    ) -> Option<IDWriteTextLayout> {
        let format = if right { &self.right } else { &self.format };
        let layout = self.write.CreateTextLayout(units, format, (w - 12.0).max(0.0), h).ok()?;
        if let Some(icons) = self.icons.borrow().as_ref() {
            for (emoji, icon) in icons {
                let mut at = 0;
                while let Some(i) =
                    units[at..].windows(emoji.len()).position(|run| run == &emoji[..])
                {
                    let range = DWRITE_TEXT_RANGE {
                        startPosition: (at + i) as u32,
                        length: emoji.len() as u32,
                    };
                    let _ = layout.SetInlineObject(icon, range);
                    at += i + emoji.len();
                }
            }
        }
        Some(layout)
    }

    /// Where each of `items` is drawn when the cell is drawn into `rc`, in `rc`'s coordinates.
    unsafe fn item_rects(
        &self, rc: &RECT, items: &[diggle_solver::advisor::annotate::Item], right: bool,
    ) -> Vec<RECT> {
        let text = diggle_solver::advisor::annotate::join(items);
        let units: Vec<u16> = text.encode_utf16().collect();
        let (w, h) = ((rc.right - rc.left) as f32, (rc.bottom - rc.top) as f32);
        let Some(layout) = self.layout(&units, w, h, right) else { return Vec::new() };
        let mut at = 0u32;
        items
            .iter()
            .map(|item| {
                let len = item.text.encode_utf16().count() as u32;
                let mut metrics = [DWRITE_HIT_TEST_METRICS::default(); 4];
                let mut n = 0u32;
                let found =
                    layout.HitTestTextRange(at, len, 0.0, 0.0, Some(&mut metrics), &mut n).is_ok()
                        && n > 0;
                at += len + 1;
                if !found {
                    return RECT::default();
                }
                let m = &metrics[..n.min(4) as usize];
                let left = m.iter().map(|m| m.left).fold(f32::MAX, f32::min);
                let right = m.iter().map(|m| m.left + m.width).fold(f32::MIN, f32::max);
                // `draw` sets the layout 6 px in from the cell's left edge.
                RECT {
                    left: rc.left + 6 + left.floor() as i32,
                    right: rc.left + 6 + right.ceil() as i32,
                    top: rc.top,
                    bottom: rc.bottom,
                }
            })
            .collect()
    }

    /// Draws `text` into `rc` of `hdc`, black, or greyed for a stale list; flush right when
    /// `right` is set.
    unsafe fn draw(
        &self, hdc: windows::Win32::Graphics::Gdi::HDC, rc: &RECT, text: &str, live: bool,
        right: bool,
    ) {
        FillRect(hdc, rc, GetSysColorBrush(COLOR_WINDOW));
        if self.target.BindDC(hdc, rc).is_err() {
            return;
        }
        let (w, h) = ((rc.right - rc.left) as f32, (rc.bottom - rc.top) as f32);
        let colour = |r: f32, g: f32, b: f32, a: f32| D2D1_COLOR_F { r, g, b, a };
        self.target.BeginDraw();
        self.target.Clear(Some(&colour(1.0, 1.0, 1.0, 1.0)));
        let units: Vec<u16> = text.encode_utf16().collect();
        let ink = self.target.CreateSolidColorBrush(&colour(0.0, 0.0, 0.0, 1.0), None);
        {
            // Made here, where the target is bound; [`ColourText::layout`] only uses them.
            let mut icons = self.icons.borrow_mut();
            icons.get_or_insert_with(|| self.make_icons());
        }
        let layout = self.layout(&units, w, h, right);
        if let (Ok(ink), Some(layout)) = (ink, layout) {
            self.target.DrawTextLayout(
                D2D_POINT_2F { x: 6.0, y: 0.0 },
                &layout,
                &ink,
                D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT | D2D1_DRAW_TEXT_OPTIONS_CLIP,
            );
        }
        // A stale list is greyed by washing it with translucent white, which greys the coloured
        // glyphs as well as the black ones.
        if !live {
            if let Ok(wash) = self.target.CreateSolidColorBrush(&colour(1.0, 1.0, 1.0, 0.55), None)
            {
                self.target
                    .FillRectangle(&D2D_RECT_F { left: 0.0, top: 0.0, right: w, bottom: h }, &wash);
            }
        }
        let _ = self.target.EndDraw(None, None);
    }
}

/// The text of one cell of the virtual list: its items, a space apart.
fn cell_text(v: &View, item: usize, sub: i32) -> String {
    match sub {
        WORD_COLUMN => v.rows.get(item).map(|r| r.row.word.clone()).unwrap_or_default(),
        _ => diggle_solver::advisor::annotate::join(&cell_items(v, item, sub)),
    }
}

/// One cell's items, each with its tooltip: see [`diggle_solver::advisor::annotate::Item`].
fn cell_items(v: &View, item: usize, sub: i32) -> Vec<diggle_solver::advisor::annotate::Item> {
    use diggle_solver::advisor::annotate as a;
    let Some(r) = v.rows.get(item) else { return Vec::new() };
    let plain = |text: String| vec![a::Item { text, tip: String::new() }];
    match sub {
        0 => plain((item + 1).to_string()),
        MOUSE_COLUMN => a::mouse_items(&r.row),
        WORD_COLUMN => plain(r.row.word.clone()),
        // The fencer before the number, so the numbers stay lined up on the right.
        3 => a::damage_items(&r.row),
        4 => r.row.bonus.as_ref().map(a::bonus_items).unwrap_or_default(),
        5 => a::gain_items(r.row.gain),
        6 => r.row.spells.clone(),
        _ => plain(r.row.effect.to_string()),
    }
}

/// The fire ring's colour and width. Pure red, wide enough to read at a glance.
const FIRE: (u8, u8, u8) = (220, 0, 0);
const FIRE_RING: i32 = 3;
/// The special-tile ring's width: as wide as the fire ring, so its colour can be read.
const SPECIAL_RING: i32 = 3;

unsafe fn fill(hdc: windows::Win32::Graphics::Gdi::HDC, rc: &RECT, colour: (u8, u8, u8)) {
    let brush = CreateSolidBrush(colorref(colour));
    FillRect(hdc, rc, brush);
    let _ = DeleteObject(brush);
}

fn inset(rc: &RECT, by: i32) -> RECT {
    RECT { left: rc.left + by, top: rc.top + by, right: rc.right - by, bottom: rc.bottom - by }
}

/// A ring `width` pixels wide just inside `rc`.
unsafe fn frame(
    hdc: windows::Win32::Graphics::Gdi::HDC, rc: &RECT, width: i32, colour: (u8, u8, u8),
) {
    let w = width;
    for side in [
        RECT { bottom: rc.top + w, ..*rc },
        RECT { top: rc.bottom - w, ..*rc },
        RECT { right: rc.left + w, ..*rc },
        RECT { left: rc.right - w, ..*rc },
    ] {
        fill(hdc, &side, colour);
    }
}

/// Draws the word column: each letter on the colour of the tile it would be typed into.
unsafe fn draw_word(ui: &Ui, cd: &NMLVCUSTOMDRAW) {
    let v = view().lock().unwrap();
    let Some(r) = v.rows.get(cd.nmcd.dwItemSpec) else { return };
    let hdc = cd.nmcd.hdc;
    let mut rc = RECT { top: WORD_COLUMN, left: LVIR_BOUNDS as i32, ..Default::default() };
    SendMessageW(
        ui.list,
        LVM_GETSUBITEMRECT,
        WPARAM(cd.nmcd.dwItemSpec),
        LPARAM(&mut rc as *mut _ as isize),
    );
    FillRect(hdc, &rc, GetSysColorBrush(COLOR_WINDOW));
    let old = SelectObject(hdc, HGDIOBJ(ui.tile_font.0));
    SetBkMode(hdc, TRANSPARENT);
    // Where capitals sit in a line of each font: below the internal leading (room for accents),
    // down to the baseline. Centring that band on the tile is what looks centred; centring the
    // whole line box, as `DT_VCENTER` does, leaves the letters low.
    let caps_mid = |font: HFONT| {
        SelectObject(hdc, HGDIOBJ(font.0));
        let mut tm = TEXTMETRICW::default();
        let _ = GetTextMetricsW(hdc, &mut tm);
        (tm.tmInternalLeading + tm.tmAscent) / 2
    };
    let (tile_mid, wildcard_mid) = (caps_mid(ui.tile_font), caps_mid(ui.wildcard_font));
    let underline = settings_now().underline_mouse_tiles;
    let mut x = rc.left + WORD_MARGIN;
    for (n, cell) in r.cells.iter().enumerate() {
        let (font, caps_mid) = match cell.wildcard {
            true => (ui.wildcard_font, wildcard_mid),
            false => (ui.tile_font, tile_mid),
        };
        SelectObject(hdc, HGDIOBJ(font.0));
        let mut text: Vec<u16> = cell.letter.encode_utf16().collect();
        let width = tile_width(hdc, &text);
        // The tile fills the row's first `ROW_HEIGHT` pixels; below is the underline's room.
        let bottom = rc.bottom.min(rc.top + ROW_HEIGHT) - 1;
        let mut tile = RECT { left: x, right: x + width, top: rc.top + 1, bottom };
        let shade = |c| if v.live { c } else { faded(c) };
        let colour = shade(cell.colour);
        // The game's own lettering colour for this material (see the palette), greyed with the
        // rest of a stale list.
        let ink = shade(cell.ink);
        fill(hdc, &tile, colour);
        if underline && r.row.letters.get(n).is_some_and(|l| l.click) {
            let top = tile.bottom + UNDERLINE_GAP;
            let bar = RECT { top, bottom: top + UNDERLINE_BAR, ..tile };
            fill(hdc, &bar, shade(UNDERLINE));
        }
        // Fire is an outer ring of red; the lettering keeps the contrast of its tile.
        if cell.burning {
            frame(hdc, &tile, FIRE_RING, shade(FIRE));
        }
        // A bordered tile is ringed in its metal's colour, and braced wood or a bleed or toxin
        // metal in near-black (see the palette), at the tile's edge or just inside the fire ring.
        if let Some(ring) = cell.ring {
            let inner = if cell.burning { inset(&tile, FIRE_RING) } else { tile };
            frame(hdc, &inner, SPECIAL_RING, shade(ring));
        }
        SetTextColor(hdc, colorref(ink));
        tile.top = (tile.top + tile.bottom) / 2 - caps_mid;
        DrawTextW(hdc, &mut text, &mut tile, DT_CENTER | DT_TOP | DT_SINGLELINE);
        x += width + TILE_GAP;
    }
    SelectObject(hdc, old);
}

/// Space between tiles, and the margin before the first.
const TILE_GAP: i32 = 2;
const WORD_MARGIN: i32 = 4;

/// One tile's width for its letter, in whatever font `hdc` has selected: the lettering, room for
/// both rings, and a little air.
unsafe fn tile_width(hdc: windows::Win32::Graphics::Gdi::HDC, text: &[u16]) -> i32 {
    let mut size = SIZE::default();
    let _ = GetTextExtentPoint32W(hdc, text, &mut size);
    size.cx.max(12) + 2 * (FIRE_RING + SPECIAL_RING) + 6
}

/// Widens the Word column to fit the longest word on the list, measured as [`draw_word`] draws
/// it, and returns it to its default width when nothing needs more. Never narrower than the
/// default, so the other columns do not jump about from turn to turn.
unsafe fn fit_word_column(ui: &Ui, v: &View) {
    let hdc = GetDC(ui.list);
    if hdc.is_invalid() {
        return;
    }
    let old = SelectObject(hdc, HGDIOBJ(ui.tile_font.0));
    let needed = v
        .rows
        .iter()
        .map(|r| {
            r.cells
                .iter()
                .map(|cell| {
                    let font = if cell.wildcard { ui.wildcard_font } else { ui.tile_font };
                    SelectObject(hdc, HGDIOBJ(font.0));
                    let text: Vec<u16> = cell.letter.encode_utf16().collect();
                    tile_width(hdc, &text) + TILE_GAP
                })
                .sum::<i32>()
                + 2 * WORD_MARGIN
        })
        .max()
        .unwrap_or(0);
    SelectObject(hdc, old);
    ReleaseDC(ui.list, hdc);
    let width = needed.max(COLUMNS[WORD_COLUMN as usize].1);
    let current =
        SendMessageW(ui.list, LVM_GETCOLUMNWIDTH, WPARAM(WORD_COLUMN as usize), LPARAM(0)).0 as i32;
    if width != current {
        SendMessageW(
            ui.list,
            LVM_SETCOLUMNWIDTH,
            WPARAM(WORD_COLUMN as usize),
            LPARAM(width as isize),
        );
    }
}

unsafe fn notify(ui: &Ui, lparam: LPARAM) -> Option<LRESULT> {
    let hdr = &*(lparam.0 as *const NMHDR);
    if hdr.hwndFrom != ui.list {
        return None;
    }
    match hdr.code {
        LVN_GETDISPINFOW => {
            let info = &mut *(lparam.0 as *mut NMLVDISPINFOW);
            if (info.item.mask & LVIF_TEXT).0 != 0 && info.item.cchTextMax > 0 {
                let text = cell_text(
                    &view().lock().unwrap(),
                    info.item.iItem as usize,
                    info.item.iSubItem,
                );
                let max = info.item.cchTextMax as usize - 1;
                let units: Vec<u16> = text.encode_utf16().take(max).chain([0]).collect();
                std::ptr::copy_nonoverlapping(units.as_ptr(), info.item.pszText.0, units.len());
            }
            Some(LRESULT(0))
        }
        NM_CUSTOMDRAW => {
            let cd = &mut *(lparam.0 as *mut NMLVCUSTOMDRAW);
            let stage = cd.nmcd.dwDrawStage;
            if stage == CDDS_PREPAINT {
                return Some(LRESULT(CDRF_NOTIFYITEMDRAW as isize));
            }
            if stage == CDDS_ITEMPREPAINT {
                return Some(LRESULT(CDRF_NOTIFYSUBITEMDRAW as isize));
            }
            if stage.0 == CDDS_ITEMPREPAINT.0 | CDDS_SUBITEM.0 {
                if cd.iSubItem == WORD_COLUMN {
                    draw_word(ui, cd);
                    return Some(LRESULT(CDRF_SKIPDEFAULT as isize));
                }
                if let Some(colour) =
                    ui.colour_text.as_ref().filter(|_| EMOJI_COLUMNS.contains(&cd.iSubItem))
                {
                    let (text, live) = {
                        let v = view().lock().unwrap();
                        (cell_text(&v, cd.nmcd.dwItemSpec, cd.iSubItem), v.live)
                    };
                    let rc = cell_rect(ui.list, cd.nmcd.dwItemSpec, cd.iSubItem);
                    let right = COLUMNS.get(cd.iSubItem as usize).is_some_and(|c| c.2);
                    // The text sits in the band the tiles fill, not the whole row, so it stays
                    // level with them when the row grows for the underline.
                    FillRect(cd.nmcd.hdc, &rc, GetSysColorBrush(COLOR_WINDOW));
                    let band = RECT { bottom: rc.bottom.min(rc.top + ROW_HEIGHT), ..rc };
                    colour.draw(cd.nmcd.hdc, &band, &text, live, right);
                    return Some(LRESULT(CDRF_SKIPDEFAULT as isize));
                }
                let live = view().lock().unwrap().live;
                cd.clrText = colorref(if live { (0, 0, 0) } else { (150, 150, 150) });
                return Some(LRESULT(CDRF_NEWFONT as isize));
            }
            Some(LRESULT(CDRF_DODEFAULT as isize))
        }
        _ => None,
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CREATE => match create(hwnd) {
            Ok(ui) => {
                layout(hwnd, &ui);
                refresh(hwnd, &ui);
                UI.with(|u| *u.borrow_mut() = Some(ui));
                LRESULT(0)
            }
            Err(_) => LRESULT(-1),
        },
        WM_SIZE => {
            relayout(hwnd);
            // Only a restored window's size is kept: reopening at a maximised or minimised size
            // would be wrong once it is restored.
            if wparam.0 == SIZE_RESTORED as usize {
                let mut rc = RECT::default();
                if GetWindowRect(hwnd, &mut rc).is_ok() {
                    update(hwnd, |s| {
                        s.width = rc.right - rc.left;
                        s.height = rc.bottom - rc.top;
                    });
                }
            }
            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            let info = &mut *(lparam.0 as *mut MINMAXINFO);
            info.ptMinTrackSize.x = settings::MIN_WIDTH;
            info.ptMinTrackSize.y = settings::MIN_HEIGHT;
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == ID_SAVE => {
            let _ = KillTimer(hwnd, ID_SAVE);
            save_now();
            LRESULT(0)
        }
        WM_SETCURSOR if HWND(wparam.0 as _) == hwnd && (lparam.0 & 0xffff) as u32 == HTCLIENT => {
            let mut pt = Default::default();
            let _ = GetCursorPos(&mut pt);
            let _ = ScreenToClient(hwnd, &mut pt);
            if on_splitter(hwnd, pt.y) {
                if let Ok(cursor) = LoadCursorW(None, IDC_SIZENS) {
                    SetCursor(cursor);
                    return LRESULT(1);
                }
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_LBUTTONDOWN if on_splitter(hwnd, mouse_y(lparam)) => {
            let s = settings_now();
            let (top, height) = log_band(client_height(hwnd), s.log_height);
            let grab = mouse_y(lparam) - (top + height);
            STATE.with(|st| {
                if let Some(st) = st.borrow_mut().as_mut() {
                    st.drag = Some(grab);
                }
            });
            SetCapture(hwnd);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let drag = STATE.with(|st| st.borrow().as_ref().and_then(|st| st.drag));
            if let Some(grab) = drag {
                let h = client_height(hwnd);
                let (top, _) = log_band(h, 0);
                // Clamped here, so the height saved is the height on screen.
                let (_, height) = log_band(h, mouse_y(lparam) - grab - top);
                update(hwnd, |s| s.log_height = height);
                relayout(hwnd);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            STATE.with(|st| {
                if let Some(st) = st.borrow_mut().as_mut() {
                    st.drag = None;
                }
            });
            LRESULT(0)
        }
        WM_COMMAND if (wparam.0 & 0xffff) == ID_LOG && (wparam.0 >> 16) as u32 == BN_CLICKED => {
            let on = UI.with(|u| {
                u.borrow().as_ref().is_some_and(|ui| {
                    SendMessageW(ui.log_toggle, BM_GETCHECK, WPARAM(0), LPARAM(0)).0
                        == BST_CHECKED.0 as isize
                })
            });
            update(hwnd, |s| s.log_open = on);
            relayout(hwnd);
            LRESULT(0)
        }
        WM_COMMAND if (wparam.0 & 0xffff) == ID_STALL && (wparam.0 >> 16) as u32 == BN_CLICKED => {
            let on = UI.with(|u| {
                u.borrow().as_ref().is_some_and(|ui| {
                    SendMessageW(ui.stall, BM_GETCHECK, WPARAM(0), LPARAM(0)).0
                        == BST_CHECKED.0 as isize
                })
            });
            update(hwnd, |s| s.stall_on_free_turns = on);
            tell_worker(options_of(&settings_now()));
            LRESULT(0)
        }
        WM_COMMAND
            if (wparam.0 & 0xffff) == ID_UNDERLINE && (wparam.0 >> 16) as u32 == BN_CLICKED =>
        {
            UI.with(|u| {
                if let Some(ui) = u.borrow().as_ref() {
                    let on = SendMessageW(ui.underline, BM_GETCHECK, WPARAM(0), LPARAM(0)).0
                        == BST_CHECKED.0 as isize;
                    update(hwnd, |s| s.underline_mouse_tiles = on);
                    let font = SendMessageW(ui.list, WM_GETFONT, WPARAM(0), LPARAM(0));
                    set_row_height(ui.list, row_height(on), WPARAM(font.0 as usize));
                    let _ = InvalidateRect(ui.list, None, true);
                }
            });
            LRESULT(0)
        }
        WM_VIEW => {
            let waited = PUBLISHED.lock().unwrap().map(|t| t.elapsed());
            let drawing = std::time::Instant::now();
            UI.with(|u| {
                if let Some(ui) = u.borrow().as_ref() {
                    refresh(hwnd, ui);
                }
            });
            telemetry::note(
                "window",
                format_args!(
                    "shown {} after publishing, updated in {}",
                    waited.map(telemetry::ms).unwrap_or_default(),
                    telemetry::since(drawing)
                ),
            );
            LRESULT(0)
        }
        WM_COMMAND
            if (wparam.0 & 0xffff) == ID_TOPMOST && (wparam.0 >> 16) as u32 == BN_CLICKED =>
        {
            UI.with(|u| {
                if let Some(ui) = u.borrow().as_ref() {
                    let on = SendMessageW(ui.topmost, BM_GETCHECK, WPARAM(0), LPARAM(0)).0
                        == BST_CHECKED.0 as isize;
                    update(hwnd, |s| s.topmost = on);
                    let after = if on { HWND_TOPMOST } else { HWND_NOTOPMOST };
                    let _ = SetWindowPos(
                        hwnd,
                        after,
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                    );
                }
            });
            LRESULT(0)
        }
        WM_NOTIFY => {
            let handled = UI.with(|u| u.borrow().as_ref().and_then(|ui| notify(ui, lparam)));
            handled.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wparam, lparam))
        }
        WM_DESTROY => {
            let _ = KillTimer(hwnd, ID_SAVE);
            save_now();
            // Dropped here, while the window is still being destroyed, not with the thread's
            // locals after `main` returns. Its Direct2D and DirectWrite objects — the item icons
            // above all — released that late, in process shutdown, fail fast: every close ended in
            // 0xC0000409 (STATUS_STACK_BUFFER_OVERRUN), the user's report, 2026-11-08. Nothing reads
            // the UI after this; the list's own handler finds it gone and passes its messages on.
            UI.with(|u| u.borrow_mut().take());
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both icons built into the exe decode, at the game's 32 px, with see-through corners: a
    /// bad copy would otherwise only show as the emoji coming back.
    #[test]
    fn the_built_in_icons_decode_with_transparency() {
        for (source, bytes) in ICONS {
            let (w, h, px) = decode_icon(bytes).unwrap_or_else(|| panic!("{source:?} decodes"));
            assert_eq!((w, h), (32, 32), "{source:?}");
            assert_eq!(px.len(), 32 * 32 * 4, "{source:?}");
            assert_eq!(&px[..4], &[0, 0, 0, 0], "{source:?}: the top-left corner is clear");
            assert!(px.chunks_exact(4).any(|p| p[3] == 255), "{source:?} has opaque pixels");
        }
    }

    /// Both window icons decode at 32 px with clear corners, and Windows makes both sizes of each.
    #[test]
    fn the_window_icons_decode_and_become_icons() {
        for (name, bytes) in [("plain", WINDOW_ICON), ("combat", WINDOW_ICON_COMBAT)] {
            let (w, h, px) = decode_png(bytes).unwrap_or_else(|| panic!("{name} decodes"));
            assert_eq!((w, h), (32, 32), "{name}");
            assert_eq!(px[3], 0, "{name}: the top-left corner is clear");
            // SAFETY: icons made and dropped on this thread.
            assert!(unsafe { WindowIcon::new(bytes) }.is_some(), "{name} becomes an icon");
        }
        assert_ne!(WINDOW_ICON, WINDOW_ICON_COMBAT, "the fight's icon is its own art");
    }

    /// Halving averages colour by alpha: a clear pixel's colour does not darken the edge.
    #[test]
    fn halving_weights_colour_by_alpha() {
        // One opaque red pixel beside three clear black ones.
        let px = [255, 0, 0, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(halve(2, 2, &px), (1, 1, vec![255, 0, 0, 64]));
        assert_eq!(halve(2, 2, &[0; 16]), (1, 1, vec![0; 4]), "all clear stays clear");
    }
}
