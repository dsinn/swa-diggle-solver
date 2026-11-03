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
//! - **Out of combat** when `combatSaveData` does not exist. That is later than the fight really
//!   ends — see [`diggle_solver::advisor::mode::classify`] for why, and why it is temporary.
//! - **Waiting** while a fight is between turns: the last list stays up, greyed.
//! - **Your turn**: every playable word, in the order Diggle prefers them
//!   ([`diggle_solver::search::ranked`]). The top row is the word Diggle would type. Each letter
//!   sits on the colour of the tile it would be typed into; the columns give the expected damage,
//!   the enemy's lexicon bonus when it is not 1, the health it should give back, and what it does.
//!
//! **Not modelled**: Diggle's murder backoff, which aims lower after the game warns that a word
//! would kill a frightened enemy. That warning reaches Diggle through the console, which this does
//! not read. You see it on screen.
//!
//! ## Options
//!
//! - `--game-dir <path>`: the game checkout. Otherwise `game_dir` from `config.toml` in the working
//!   directory, else `../sternly-worded-adventures`.
//! - `--dir <path>` (repeatable): watch these folders instead of the save folder. For testing
//!   against a copied save.

#![windows_subsystem = "windows"]

use diggle_solver::advisor::mode::Mode;
use diggle_solver::advisor::view::{Resources, View};
use diggle_solver::advisor::watch::{self, Event};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{mpsc, Mutex, OnceLock};
use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory, D2D1_DRAW_TEXT_OPTIONS_CLIP,
    D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
    D2D1_RENDER_TARGET_USAGE_NONE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_WORD_WRAPPING_NO_WRAP,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    CreateFontW, CreateSolidBrush, DeleteObject, DrawTextW, FillRect, GetSysColorBrush,
    GetTextExtentPoint32W, GetTextMetricsW, InvalidateRect, SelectObject, SetBkMode, SetTextColor,
    COLOR_BTNFACE, COLOR_WINDOW, DT_CENTER, DT_SINGLELINE, DT_TOP, HFONT, HGDIOBJ, TEXTMETRICW,
    TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    ImageList_Create, InitCommonControlsEx, BST_CHECKED, CDDS_ITEMPREPAINT, CDDS_PREPAINT,
    CDDS_SUBITEM, CDRF_DODEFAULT, CDRF_NEWFONT, CDRF_NOTIFYITEMDRAW, CDRF_NOTIFYSUBITEMDRAW,
    CDRF_SKIPDEFAULT, ICC_LISTVIEW_CLASSES, ICC_STANDARD_CLASSES, ILC_COLOR32,
    INITCOMMONCONTROLSEX, LVCFMT_LEFT, LVCFMT_RIGHT, LVCF_FMT, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW,
    LVIF_TEXT, LVIR_BOUNDS, LVM_GETSUBITEMRECT, LVM_INSERTCOLUMNW, LVM_SETEXTENDEDLISTVIEWSTYLE,
    LVM_SETIMAGELIST, LVM_SETITEMCOUNT, LVN_GETDISPINFOW, LVSIL_SMALL, LVS_EX_DOUBLEBUFFER,
    LVS_EX_FULLROWSELECT, LVS_NOSORTHEADER, LVS_OWNERDATA, LVS_REPORT, NMHDR, NMLVCUSTOMDRAW,
    NMLVDISPINFOW, NM_CUSTOMDRAW, WC_LISTVIEWW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetMessageW, LoadCursorW,
    MoveWindow, PostMessageW, PostQuitMessage, RegisterClassW, SendMessageW, SetWindowPos,
    SetWindowTextW, ShowWindow, TranslateMessage, BM_GETCHECK, BN_CLICKED, BS_AUTOCHECKBOX,
    CW_USEDEFAULT, ES_AUTOVSCROLL, ES_MULTILINE, ES_READONLY, HMENU, HWND_NOTOPMOST, HWND_TOPMOST,
    IDC_ARROW, MSG, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE, SW_SHOW, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_APP, WM_COMMAND, WM_CREATE, WM_DESTROY, WM_NOTIFY, WM_SETFONT, WM_SIZE,
    WNDCLASSW, WS_BORDER, WS_CHILD, WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};

/// Posted to our own window when [`VIEW`] has changed. Never sent anywhere else.
const WM_VIEW: u32 = WM_APP + 1;
const ID_TOPMOST: usize = 101;
/// Static-control styles (`winuser.h`), which the `windows` crate does not expose as constants.
const SS_CENTER: u32 = 0x1;
const SS_CENTERIMAGE: u32 = 0x200;

/// The latest view, written by the worker and read by the window.
static VIEW: OnceLock<Mutex<View>> = OnceLock::new();

fn view() -> &'static Mutex<View> {
    VIEW.get_or_init(|| Mutex::new(View::default()))
}

struct Ui {
    status: HWND,
    notes: HWND,
    topmost: HWND,
    list: HWND,
    banner: HWND,
    tile_font: HFONT,
    wildcard_font: HFONT,
    /// Direct2D text for the cells that carry emoji. `None` if it would not start, and those
    /// cells fall back to the list's own GDI text, emoji in monochrome.
    colour_text: Option<ColourText>,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

/// Columns: header, width, right-aligned.
const COLUMNS: [(&str, i32, bool); 7] = [
    ("#", 44, true),
    ("Word", 470, false),
    ("Damage", 70, true),
    ("Bonus", 150, false),
    ("Heals", 56, true),
    ("Spell", 120, false),
    ("Effect", 190, false),
];
const WORD_COLUMN: i32 = 1;
/// Lettering on the word column's tiles, in pixels. Bigger than the rest of the list on purpose:
/// the word is what the player reads.
const TILE_TEXT: i32 = 26;
/// Row height, to fit [`TILE_TEXT`] on a tile with a little margin.
const ROW_HEIGHT: i32 = 36;

struct Args {
    game_dir: PathBuf,
    dirs: Vec<PathBuf>,
}

fn args() -> Args {
    let mut game_dir = None;
    let mut dirs = Vec::new();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--game-dir" => game_dir = it.next().map(PathBuf::from),
            "--dir" => dirs.extend(it.next().map(PathBuf::from)),
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
    Args { game_dir, dirs }
}

/// Publishes a view and tells the window. `hwnd` is carried as an integer so it can cross threads.
fn publish(hwnd: isize, v: View) {
    *view().lock().unwrap() = v;
    // SAFETY: posting to our own window; a failure only means it has closed.
    unsafe {
        let _ = PostMessageW(HWND(hwnd as _), WM_VIEW, WPARAM(0), LPARAM(0));
    }
}

/// Loads the game data, watches the folders, and turns each change into a [`View`].
///
/// Events arriving while a turn is being ranked are coalesced: only the newest snapshot is ranked,
/// and a result whose save has already moved on is dropped rather than shown.
fn worker(hwnd: isize, args: Args) {
    let res = match Resources::load(&args.game_dir) {
        Ok(r) => r,
        Err(e) => {
            let header = format!("cannot load the game data from {}: {e}", args.game_dir.display());
            return publish(hwnd, View { header, ..View::default() });
        }
    };
    if args.dirs.is_empty() {
        return publish(hwnd, View { header: "APPDATA is not set".into(), ..View::default() });
    }
    let (tx, rx) = mpsc::channel();
    watch::spawn(args.dirs, tx);
    let mut peaks: HashMap<String, i64> = HashMap::new();
    let mut current = View::default();
    let mut pending: Option<Event> = None;
    loop {
        let Some(mut event) = pending.take().or_else(|| rx.recv().ok()) else { return };
        while let Ok(newer) = rx.try_recv() {
            event = newer;
        }
        match event {
            Event::Snapshot(snap) => {
                let next = View::next(&current, &snap, &res, &mut peaks);
                if let Ok(newer) = rx.try_recv() {
                    let stale =
                        newer.supersedes_a_turn() && !matches!(next.mode, Mode::OutOfCombat);
                    pending = Some(newer);
                    // Peaks only rise within a fight, so the stale turn's reading stays true.
                    if stale {
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
    // SAFETY: standard Win32 window set-up and message loop, all on this thread.
    unsafe {
        let icc = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_LISTVIEW_CLASSES | ICC_STANDARD_CLASSES,
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
            WINDOW_EX_STYLE(0),
            class,
            w!("Diggle Advisor"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1250,
            720,
            None,
            None,
            instance,
            None,
        )?;
        let raw = hwnd.0 as isize;
        std::thread::spawn(move || worker(raw, args));
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
    let list = child(
        hwnd,
        WC_LISTVIEWW,
        w!(""),
        LVS_REPORT | LVS_OWNERDATA | LVS_NOSORTHEADER | WS_BORDER.0 | WS_TABSTOP.0,
        0,
    )?;
    let banner = child(hwnd, w!("STATIC"), w!("Out of combat"), SS_CENTER | SS_CENTERIMAGE, 0)?;
    for h in [status, notes, topmost, list] {
        SendMessageW(h, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
    }
    SendMessageW(banner, WM_SETFONT, WPARAM(banner_font.0 as usize), LPARAM(1));
    // A ListView's rows are as tall as its small icons. An empty one-pixel-wide image list is the
    // way to make rows tall enough for the tiles without enlarging every other column's text.
    let rows = ImageList_Create(1, ROW_HEIGHT, ILC_COLOR32, 0, 0);
    SendMessageW(list, LVM_SETIMAGELIST, WPARAM(LVSIL_SMALL as usize), LPARAM(rows.0));
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
    Ok(Ui {
        status,
        notes,
        topmost,
        list,
        banner,
        tile_font,
        wildcard_font,
        colour_text: ColourText::new().ok(),
    })
}

unsafe fn layout(hwnd: HWND, ui: &Ui) {
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
    let pad = 8;
    let _ = MoveWindow(ui.status, pad, pad, (w - 3 * pad - 120).max(0), 40, true);
    let _ = MoveWindow(ui.topmost, w - pad - 120, pad, 120, 24, true);
    let _ = MoveWindow(ui.notes, pad, pad + 44, (w - 2 * pad).max(0), 64, true);
    let top = pad + 44 + 64 + pad;
    for c in [ui.list, ui.banner] {
        let _ = MoveWindow(c, pad, top, (w - 2 * pad).max(0), (h - top - pad).max(0), true);
    }
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
    let _ = ShowWindow(ui.list, if show_list { SW_SHOW } else { SW_HIDE });
    let _ = ShowWindow(ui.banner, if show_list { SW_HIDE } else { SW_SHOW });
    SendMessageW(ui.list, LVM_SETITEMCOUNT, WPARAM(v.rows.len()), LPARAM(0));
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

/// The columns whose text carries emoji, drawn by [`ColourText`]: Bonus and Spell.
const EMOJI_COLUMNS: [i32; 2] = [3, 5];

/// Colour emoji, which GDI cannot draw: GDI renders a colour font's glyphs in one colour, so the
/// list's own text would show 🔥 as a black outline. Direct2D's text drawing with
/// `D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT` draws them in colour, onto the list's own device
/// context through a DC render target.
struct ColourText {
    target: ID2D1DCRenderTarget,
    format: IDWriteTextFormat,
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
            Ok(ColourText { target, format })
        }
    }

    /// Draws `text` into `rc` of `hdc`, black, or greyed for a stale list.
    unsafe fn draw(
        &self, hdc: windows::Win32::Graphics::Gdi::HDC, rc: &RECT, text: &str, live: bool,
    ) {
        FillRect(hdc, rc, GetSysColorBrush(COLOR_WINDOW));
        if self.target.BindDC(hdc, rc).is_err() {
            return;
        }
        let (w, h) = ((rc.right - rc.left) as f32, (rc.bottom - rc.top) as f32);
        let colour = |r: f32, g: f32, b: f32, a: f32| D2D1_COLOR_F { r, g, b, a };
        self.target.BeginDraw();
        self.target.Clear(Some(&colour(1.0, 1.0, 1.0, 1.0)));
        if let Ok(ink) = self.target.CreateSolidColorBrush(&colour(0.0, 0.0, 0.0, 1.0), None) {
            let units: Vec<u16> = text.encode_utf16().collect();
            self.target.DrawText(
                &units,
                &self.format,
                &D2D_RECT_F { left: 6.0, top: 0.0, right: w - 4.0, bottom: h },
                &ink,
                D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT | D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
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

/// The text of one cell of the virtual list.
fn cell_text(v: &View, item: usize, sub: i32) -> String {
    let Some(r) = v.rows.get(item) else { return String::new() };
    match sub {
        0 => (item + 1).to_string(),
        1 => r.row.word.clone(),
        2 => r.row.score.to_string(),
        3 => r.row.bonus.as_ref().map(|b| b.to_string()).unwrap_or_default(),
        4 => (r.row.heal > 0).then(|| format!("+{}", r.row.heal)).unwrap_or_default(),
        5 => r.row.spells.join(", "),
        _ => r.row.effect.to_string(),
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
    let mut x = rc.left + 4;
    for cell in &r.cells {
        let (font, caps_mid) = match cell.wildcard {
            true => (ui.wildcard_font, wildcard_mid),
            false => (ui.tile_font, tile_mid),
        };
        SelectObject(hdc, HGDIOBJ(font.0));
        let mut text: Vec<u16> = cell.letter.encode_utf16().collect();
        let mut size = SIZE::default();
        let _ = GetTextExtentPoint32W(hdc, &text, &mut size);
        let width = size.cx.max(12) + 2 * (FIRE_RING + SPECIAL_RING) + 6;
        let mut tile = RECT { left: x, right: x + width, top: rc.top + 1, bottom: rc.bottom - 1 };
        let shade = |c| if v.live { c } else { faded(c) };
        let colour = shade(cell.colour);
        // The game's own lettering colour for this material (see the palette), greyed with the
        // rest of a stale list.
        let ink = shade(cell.ink);
        fill(hdc, &tile, colour);
        // Fire is an outer ring of red; the lettering keeps the contrast of its tile.
        if cell.burning {
            frame(hdc, &tile, FIRE_RING, shade(FIRE));
        }
        // A special tile — braced, inlaid, serrated, gem, bordered — is ringed in its own colour
        // (a metal border's, inlaid wood's gold, else near-black), at the tile's edge or just
        // inside the fire ring.
        if let Some(ring) = cell.ring {
            let inner = if cell.burning { inset(&tile, FIRE_RING) } else { tile };
            frame(hdc, &inner, SPECIAL_RING, shade(ring));
        }
        SetTextColor(hdc, colorref(ink));
        tile.top = (tile.top + tile.bottom) / 2 - caps_mid;
        DrawTextW(hdc, &mut text, &mut tile, DT_CENTER | DT_TOP | DT_SINGLELINE);
        x += width + 2;
    }
    SelectObject(hdc, old);
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
                    let mut rc =
                        RECT { top: cd.iSubItem, left: LVIR_BOUNDS as i32, ..Default::default() };
                    SendMessageW(
                        ui.list,
                        LVM_GETSUBITEMRECT,
                        WPARAM(cd.nmcd.dwItemSpec),
                        LPARAM(&mut rc as *mut _ as isize),
                    );
                    colour.draw(cd.nmcd.hdc, &rc, &text, live);
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
            UI.with(|u| {
                if let Some(ui) = u.borrow().as_ref() {
                    layout(hwnd, ui);
                }
            });
            LRESULT(0)
        }
        WM_VIEW => {
            UI.with(|u| {
                if let Some(ui) = u.borrow().as_ref() {
                    refresh(hwnd, ui);
                }
            });
            LRESULT(0)
        }
        WM_COMMAND
            if (wparam.0 & 0xffff) == ID_TOPMOST && (wparam.0 >> 16) as u32 == BN_CLICKED =>
        {
            UI.with(|u| {
                if let Some(ui) = u.borrow().as_ref() {
                    let on = SendMessageW(ui.topmost, BM_GETCHECK, WPARAM(0), LPARAM(0)).0
                        == BST_CHECKED.0 as isize;
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
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
