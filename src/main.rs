//! Scratchpad: a tiny always-there note window (Acryl/Mica, macOS- or Windows-style title bar),
//! written directly against the Windows API so it starts instantly and needs no runtime.

#![windows_subsystem = "windows"]

mod editor;
mod gfx;
mod menu;
mod settings;
mod sys;

use std::cell::RefCell;
use std::ffi::c_void;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct2D::Common::*;
use windows::Win32::Graphics::Direct2D::*;
use windows::Win32::Graphics::DirectWrite::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::DataExchange::*;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::Controls::MARGINS;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use editor::{Edit, Editor};
use gfx::{contains, fade, gray, rect, rgb, rgba, Color, Gfx};
use settings::Settings;
use sys::wide;

// Layout, in device-independent pixels.
const TITLE_H: f32 = 40.0;
const WIN_BTN_W: f32 = 46.0;
// Same height as the title bar, so title and tools sit at the same height in both button styles.
const WIN_BTN_H: f32 = TITLE_H;
const TEXT_X: f32 = 24.0;
const TEXT_TOP: f32 = 48.0;
const VIEW_TOP: f32 = 42.0;
const STATUS_H: f32 = 30.0;
const RESIZE_BORDER: f32 = 5.0;
const MIN_W: f32 = 320.0;
const MIN_H: f32 = 220.0;

const T_SAVE: usize = 1;
const T_BLINK: usize = 2;
const T_ANIM: usize = 3;
const T_SAVED_LABEL: usize = 4;

const WM_MOUSELEAVE: u32 = 0x02A3;
const WM_TRAY: u32 = WM_APP + 1;
const WM_SHOW_REQUEST: u32 = WM_APP + 2;
const WM_LATE_INIT: u32 = WM_APP + 3;

// Menu command ids.
const ID_THEME: usize = 100; // +0 Auto, +1 Light, +2 Dark
const ID_MATERIAL: usize = 110; // +0 Acrylic, +1 Mica
const ID_TITLE_MODE: usize = 120; // +0 Always, +1 Hover, +2 Never
const ID_BUTTONS_MODE: usize = 130;
const ID_BUTTON_STYLE: usize = 140; // +0 Mac, +1 Windows
const ID_HOTKEY: usize = 150;
const ID_AUTOSTART: usize = 151;
const ID_QUIT: usize = 152;
const ID_CUT: usize = 160;
const ID_COPY: usize = 161;
const ID_PASTE: usize = 162;
const ID_SELECT_ALL: usize = 163;
const ID_OPEN: usize = 170;

const ID_FIND: usize = 190;
const ID_AUTO_HIDE: usize = 191;
const ID_FOLDER_PICK: usize = 192;
const ID_FOLDER_DEFAULT: usize = 193;

const ID_INFO: usize = 194;
// "Next value" commands of the menu's cycling rows.
const ID_CYCLE_THEME: usize = 200;
const ID_CYCLE_MATERIAL: usize = 201;

const ID_CYCLE_TITLE: usize = 203;
const ID_CYCLE_BUTTONS: usize = 204;
const ID_CYCLE_STYLE: usize = 205;

/// Fonts offered for the note (only the installed ones are shown), and line heights as
/// multiples of the font size (0 = the font's own).
const FONT_CHOICES: [&str; 9] = ["Cascadia Mono", "Cascadia Code", "Consolas", "JetBrains Mono", "Fira Code",
    "Courier New", "Segoe UI Variable Text", "Segoe UI", "Georgia"];
const SPACINGS: [f32; 3] = [0.0, 1.5, 1.8];
const ID_CYCLE_FONT: usize = 206;
const ID_CYCLE_SPACING: usize = 207;
const ID_TINT_SLIDER: usize = 208;
const ID_MARKDOWN: usize = 195;
const ID_REPLACE: usize = 196;
const MENU_CLASS: PCWSTR = w!("ScratchpadMenu");
/// Shown in the Info card. Chosen by the app's owner: 1.666 as a nod to Doom, "p" for
/// performance. Cargo.toml carries the same number without the letter (1.666.0), because
/// Cargo does not accept a suffix like this.
const VERSION: &str = "1.7p";

const THEMES: [&str; 3] = ["Auto", "Light", "Dark"];
const MATERIALS: [&str; 3] = ["Acrylic", "Mica", "Glass"];
const MODES: [&str; 3] = ["Always", "Hover", "Never"];
const STYLES: [&str; 2] = ["Mac", "Windows"];

const DWMWA_USE_IMMERSIVE_DARK_MODE_: i32 = 20;
const DWMWA_SYSTEMBACKDROP_TYPE_: i32 = 38;
const DWMSBT_NONE: i32 = 1;
const DWMSBT_MAINWINDOW: i32 = 2; // Mica
const DWMSBT_TRANSIENTWINDOW: i32 = 3; // Acrylic

#[derive(Clone, Copy, PartialEq)]
enum Btn {
    Close,
    Minimize,
    Maximize,
    Pin,
    Menu,
}

/// Actions that make Windows call back into the window procedure (showing, menus, …).
/// They run after the handler has released its borrow of the app state.
enum Cmd {
    Show,
    ShowFirst,
    Hide,
    Minimize,
    ToggleMaximize,
    Topmost(bool),
    SetRect(RECT),
    TrayMenu,
    EditMenu,
    Exit,
    OpenUrl(String),
    PickFolder,
    /// Move the note to this folder (None = default folder).
    SetFolder(Option<PathBuf>),
}

/// Find bar state. While it is open, typing goes to the query instead of the note.
struct Search {
    query: Vec<u16>,
    replacement: Vec<u16>,
    /// The "replace with" row is shown (Ctrl+H).
    replace_mode: bool,
    /// The caret is in the replacement field instead of the query.
    in_replacement: bool,
    matches: Vec<usize>,
    current: usize,
}

/// Lower-case form of one UTF-16 unit, for case-insensitive search.
fn lower(c: u16) -> u16 {
    char::from_u32(c as u32)
        .and_then(|ch| {
            let mut lowered = ch.to_lowercase();
            let first = lowered.next()?;
            (lowered.next().is_none() && (first as u32) < 0x1_0000).then_some(first as u16)
        })
        .unwrap_or(c)
}

fn timestamp() -> String {
    let t = unsafe { GetLocalTime() };
    format!("{:04}-{:02}-{:02} {:02}.{:02}.{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

#[derive(Clone, Copy)]
struct Palette {
    fg: Color,
    muted: Color,
    placeholder: Color,
    hover: Color,
    thumb: Color,
    inactive_dot: Color,
    dark: bool,
    accent: Color,
    /// Selected menu row: the base accent colour, with white text on it.
    highlight: Color,
    card_bg: Color,
    card_border: Color,
    fallback_bg: Color,
}

fn palette(dark: bool) -> Palette {
    let accent = |shade: usize, fallback: Color| sys::accent_color(shade).map(|(r, g, b)| rgb(r, g, b)).unwrap_or(fallback);
    if dark {
        Palette {
            dark: true,
            fg: rgb(0xF2, 0xF2, 0xF7),
            muted: gray(0x99, 0xFF),
            placeholder: gray(0x55, 0xFF),
            hover: gray(0x14, 0xFF),
            thumb: gray(0x44, 0xFF),
            inactive_dot: gray(0x33, 0xFF),
            // Windows uses a lighter accent shade on dark surfaces.
            accent: accent(1, rgb(0x60, 0xCD, 0xFF)),
            highlight: accent(3, rgb(0x00, 0x78, 0xD4)),
            card_bg: rgba(0x2B, 0x2B, 0x2E, 0xF7),
            card_border: gray(0x24, 0xFF),
            fallback_bg: rgb(0x20, 0x20, 0x20),
        }
    } else {
        Palette {
            dark: false,
            fg: rgb(0x1C, 0x1C, 0x1E),
            muted: gray(0x8C, 0x00),
            placeholder: gray(0x55, 0x00),
            hover: gray(0x0F, 0x00),
            thumb: gray(0x38, 0x00),
            inactive_dot: gray(0x26, 0x00),
            accent: accent(4, rgb(0x00, 0x5F, 0xB8)),
            highlight: accent(3, rgb(0x00, 0x78, 0xD4)),
            card_bg: rgba(0xF6, 0xF6, 0xF6, 0xF7),
            card_border: gray(0x1A, 0x00),
            fallback_bg: rgb(0xF3, 0xF3, 0xF3),
        }
    }
}

struct Fonts {
    dwrite: IDWriteFactory,
    mono_family: String,
    title: IDWriteTextFormat,
    status: IDWriteTextFormat,
    icon: IDWriteTextFormat,
    icon_small: IDWriteTextFormat,
    overlay_title: IDWriteTextFormat,
    overlay_hint: IDWriteTextFormat,
    placeholder: IDWriteTextFormat,
    search: IDWriteTextFormat,
    /// Installed fonts offered in the menu.
    choices: Vec<String>,
}

impl Fonts {
    unsafe fn new(font_size: f32) -> Self {
        let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).expect("DirectWrite");
        let ui = Self::first_installed(&dwrite, &["Segoe UI Variable Text", "Segoe UI"]);
        let display = Self::first_installed(&dwrite, &["Segoe UI Variable Display", "Segoe UI"]);
        let icons = Self::first_installed(&dwrite, &["Segoe Fluent Icons", "Segoe MDL2 Assets"]);
        let mono_family = Self::first_installed(&dwrite, &["Cascadia Mono", "Consolas", "Courier New"]);
        let make = |family: &str, size: f32, weight: DWRITE_FONT_WEIGHT| Self::format(&dwrite, family, size, weight);
        let search = make(&ui, 13.0, DWRITE_FONT_WEIGHT_NORMAL);
        let _ = search.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
        Self {
            search,
            title: make(&display, 13.0, DWRITE_FONT_WEIGHT_SEMI_BOLD),
            status: make(&ui, 11.5, DWRITE_FONT_WEIGHT_NORMAL),
            icon: make(&icons, 13.0, DWRITE_FONT_WEIGHT_NORMAL),
            icon_small: make(&icons, 10.0, DWRITE_FONT_WEIGHT_NORMAL),
            overlay_title: make(&ui, 14.0, DWRITE_FONT_WEIGHT_SEMI_BOLD),
            overlay_hint: make(&ui, 12.0, DWRITE_FONT_WEIGHT_NORMAL),
            placeholder: make(&mono_family, font_size, DWRITE_FONT_WEIGHT_NORMAL),
            choices: Self::installed(&dwrite, &FONT_CHOICES),
            mono_family,
            dwrite,
        }
    }

    /// Those of `families` that are installed, in the given order.
    unsafe fn installed(dwrite: &IDWriteFactory, families: &[&str]) -> Vec<String> {
        let mut collection = None;
        let _ = dwrite.GetSystemFontCollection(&mut collection, false);
        let Some(collection) = collection else { return Vec::new() };
        families.iter().filter(|family| {
            let (mut index, mut exists) = (0u32, BOOL(0));
            collection.FindFamilyName(PCWSTR(wide(family).as_ptr()), &mut index, &mut exists).is_ok() && exists.as_bool()
        }).map(|family| family.to_string()).collect()
    }

    unsafe fn first_installed(dwrite: &IDWriteFactory, families: &[&str]) -> String {
        let mut collection = None;
        if dwrite.GetSystemFontCollection(&mut collection, false).is_ok() {
            if let Some(collection) = collection {
                for family in families {
                    let (mut index, mut exists) = (0u32, BOOL(0));
                    if collection.FindFamilyName(PCWSTR(wide(family).as_ptr()), &mut index, &mut exists).is_ok() && exists.as_bool() {
                        return family.to_string();
                    }
                }
            }
        }
        families.last().unwrap().to_string()
    }

    unsafe fn format(dwrite: &IDWriteFactory, family: &str, size: f32, weight: DWRITE_FONT_WEIGHT) -> IDWriteTextFormat {
        dwrite
            .CreateTextFormat(PCWSTR(wide(family).as_ptr()), None, weight, DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL, size, w!("de-de"))
            .expect("CreateTextFormat")
    }

    /// Format for the editor itself: wraps anywhere if a word is longer than the line.
    /// `spacing` > 0 sets the line height to that multiple of the font size.
    unsafe fn editor_format(&self, family: &str, size: f32, spacing: f32) -> IDWriteTextFormat {
        let format = Self::format(&self.dwrite, family, size, DWRITE_FONT_WEIGHT_NORMAL);
        let _ = format.SetWordWrapping(DWRITE_WORD_WRAPPING_EMERGENCY_BREAK);
        let _ = format.SetIncrementalTabStop(size * 0.6 * 4.0); // four characters of a monospace font
        if spacing > 0.0 {
            let _ = format.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, size * spacing, size * spacing * editor::BASELINE);
        }
        format
    }
}

struct App {
    hwnd: HWND,
    gfx: Option<Gfx>,
    fonts: Fonts,
    editor: Editor,
    settings: Settings,
    pal: Palette,
    backdrop_ok: bool,
    cmds: Vec<Cmd>,

    dpi: f32,
    w: f32,
    h: f32,

    active: bool,
    focused: bool,
    caret_on: bool,
    dirty: bool,
    exiting: bool,

    // Title bar state.
    chrome_hover: bool,
    lights_hover: bool,
    hot: Option<Btn>,
    pressed: Option<Btn>,
    menu_open: bool,
    a_title: f32,
    a_buttons: f32,
    a_tools: f32,

    // Mouse interaction in the editor.
    selecting: bool,
    thumb_drag: Option<f32>,
    last_click: Instant,
    last_click_pos: (f32, f32),
    click_count: u32,

    hotkey_ok: bool,
    recording: bool,
    hint: String,
    stats: String,
    saved_label: String,

    note_path: PathBuf,
    /// Modification time of the note file as we last read or wrote it; a different time
    /// on disk means another program (e.g. a sync from another computer) changed it.
    note_mtime: Option<SystemTime>,
    search: Option<Search>,
    /// A system dialog is open; losing focus to it must not auto-hide the window.
    dialog_open: bool,
    info_open: bool,
    /// Pre-rendered light fade for the glass look (for dark theme?, strip); belongs to `gfx`.
    sheen: Option<(bool, ID2D1Bitmap)>,

    /// The "⋯" menu: its popup window is created on first use and then reused.
    menu_hwnd: HWND,
    menu: Option<menu::Menu>,
    /// Address of the RefCell holding this App, handed to further windows of the app.
    cell_ptr: isize,
    /// Worker thread that prepares Direct2D during startup (see gfx::Prepared).
    warmup: Option<std::thread::JoinHandle<Option<gfx::Prepared>>>,

    tray_icon: HICON,
    taskbar_created: u32,
}

fn loword(v: isize) -> i32 {
    (v & 0xFFFF) as i16 as i32
}

fn hiword(v: isize) -> i32 {
    ((v >> 16) & 0xFFFF) as i16 as i32
}

impl App {
    unsafe fn new(hwnd: HWND, settings: Settings, warmup: std::thread::JoinHandle<Option<gfx::Prepared>>) -> Self {
        let fonts = Fonts::new(settings.font_size);
        let note_path = settings.note_path();
        let family = settings.font.clone().unwrap_or_else(|| fonts.mono_family.clone());
        let format = fonts.editor_format(&family, settings.font_size, settings.line_spacing);
        let mut editor = Editor::new(fonts.dwrite.clone(), format, &settings::load_note(&note_path));
        editor.set_style(settings.line_spacing, settings.markdown);
        let mut client = RECT::default();
        let _ = GetClientRect(hwnd, &mut client);
        let dpi = GetDpiForWindow(hwnd) as f32;
        let mut app = Self {
            hwnd,
            gfx: None,
            fonts,
            editor,
            pal: palette(true),
            // The system backdrop exists from Windows 11 22H2 (build 22621) on.
            backdrop_ok: sys::windows_build() >= 22621,
            cmds: Vec::new(),
            dpi,
            w: client.right as f32 * 96.0 / dpi,
            h: client.bottom as f32 * 96.0 / dpi,
            active: false,
            focused: false,
            caret_on: true,
            dirty: false,
            exiting: false,
            chrome_hover: false,
            lights_hover: false,
            hot: None,
            pressed: None,
            menu_open: false,
            a_title: 0.0,
            a_buttons: 0.0,
            a_tools: 0.0,
            selecting: false,
            thumb_drag: None,
            last_click: Instant::now(),
            last_click_pos: (0.0, 0.0),
            click_count: 0,
            hotkey_ok: false,
            recording: false,
            hint: String::new(),
            stats: String::new(),
            saved_label: String::new(),
            note_mtime: settings::modified(&note_path),
            note_path,
            search: None,
            dialog_open: false,
            info_open: false,
            sheen: None,
            menu_hwnd: HWND::default(),
            menu: None,
            cell_ptr: 0,
            warmup: Some(warmup),
            tray_icon: sys::load_icon(GetSystemMetrics(SM_CXSMICON)),
            taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
            settings,
        };
        (app.a_title, app.a_buttons, app.a_tools) = app.chrome_targets();
        app.editor.set_width(app.text_width());
        app.update_stats();
        app.apply_theme();
        app.apply_backdrop();
        // No open/close animation: the window should be there the instant the hotkey is pressed.
        let no_transitions: i32 = 1;
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_TRANSITIONS_FORCEDISABLED, &no_transitions as *const i32 as *const c_void, 4);
        app.hotkey_ok = sys::register_hotkey(hwnd, app.settings.hotkey_modifiers, app.settings.hotkey_key);
        // The tray icon talks to Explorer (several ms); do it once the first frame is on screen.
        let _ = PostMessageW(hwnd, WM_LATE_INIT, WPARAM(0), LPARAM(0));
        if app.settings.topmost {
            app.cmds.push(Cmd::Topmost(true));
        }
        app
    }

    // ---------- Geometry ----------

    fn scale(&self) -> f32 {
        self.dpi / 96.0
    }

    fn text_width(&self) -> f32 {
        self.w - TEXT_X - 24.0
    }

    fn view_bottom(&self) -> f32 {
        self.h - STATUS_H
    }

    /// Height of the visible text area, used for scrolling.
    fn view_height(&self) -> f32 {
        (self.view_bottom() - self.text_top()).max(10.0)
    }

    /// The text moves down while the find bar is open, so the bar never covers a hit.
    fn search_offset(&self) -> f32 {
        match &self.search { Some(s) if s.replace_mode => 76.0, Some(_) => 40.0, None => 0.0 }
    }

    fn text_top(&self) -> f32 {
        TEXT_TOP + self.search_offset()
    }

    fn mac_style(&self) -> bool {
        self.settings.button_style != "Windows"
    }

    fn buttons_enabled(&self) -> bool {
        self.settings.buttons_mode != "Never"
    }

    fn rect_win_button(&self, index: usize) -> D2D_RECT_F {
        rect(self.w - WIN_BTN_W * (3 - index) as f32, 0.0, WIN_BTN_W, WIN_BTN_H)
    }

    /// Vertical centre of title, tools and window buttons, identical in both button styles.
    fn title_center_y(&self) -> f32 {
        TITLE_H / 2.0
    }

    /// Pin and menu button. They sit at the edge opposite the window buttons: on the right in
    /// Mac style, on the left in Windows style. `index_from_edge` 0 is the one next to the edge.
    fn rect_tool(&self, index_from_edge: usize) -> D2D_RECT_F {
        let offset = 30.0 * index_from_edge as f32;
        let left = if self.mac_style() { self.w - 10.0 - 28.0 - offset } else { 12.0 + offset };
        rect(left, self.title_center_y() - 12.0, 28.0, 24.0)
    }

    fn rect_menu_button(&self) -> D2D_RECT_F {
        self.rect_tool(0)
    }

    fn rect_pin_button(&self) -> D2D_RECT_F {
        self.rect_tool(1)
    }

    fn light_center(index: usize) -> (f32, f32) {
        (20.0 + 20.0 * index as f32, TITLE_H / 2.0)
    }

    fn rect_lights(&self) -> D2D_RECT_F {
        rect(10.0, 10.0, 60.0, 20.0)
    }

    fn button_at(&self, x: f32, y: f32) -> Option<Btn> {
        if contains(&self.rect_menu_button(), x, y) {
            return Some(Btn::Menu);
        }
        if contains(&self.rect_pin_button(), x, y) {
            return Some(Btn::Pin);
        }
        if !self.buttons_enabled() {
            return None;
        }
        let order = if self.mac_style() { [Btn::Close, Btn::Minimize, Btn::Maximize] } else { [Btn::Minimize, Btn::Maximize, Btn::Close] };
        for (i, button) in order.into_iter().enumerate() {
            let hit = if self.mac_style() {
                let (cx, cy) = Self::light_center(i);
                (x - cx).abs() <= 8.0 && (y - cy).abs() <= 8.0
            } else {
                contains(&self.rect_win_button(i), x, y)
            };
            if hit {
                return Some(button);
            }
        }
        None
    }

    fn scrollbar(&mut self) -> Option<(f32, f32, f32, f32)> {
        // (track top, track height, thumb top, thumb height)
        let view = self.view_height();
        let content = self.editor.content_height();
        if content <= view + 0.5 {
            return None;
        }
        let track_top = VIEW_TOP + 2.0;
        let track_h = self.view_bottom() - 2.0 - track_top;
        let thumb_h = (track_h * view / content).max(24.0).min(track_h);
        let max_scroll = self.editor.max_scroll(view);
        let thumb_top = track_top + (track_h - thumb_h) * if max_scroll > 0.0 { self.editor.scroll / max_scroll } else { 0.0 };
        Some((track_top, track_h, thumb_top, thumb_h))
    }

    unsafe fn invalidate(&self) {
        let _ = InvalidateRect(self.hwnd, None, false);
    }

    // ---------- Theme ----------

    fn is_dark(&self) -> bool {
        match self.settings.theme.as_str() {
            "Dark" => true,
            "Light" => false,
            _ => sys::system_uses_dark_theme(),
        }
    }

    unsafe fn apply_theme(&mut self) {
        let dark = self.is_dark();
        self.pal = palette(dark);
        if let Some(gfx) = &self.gfx {
            gfx.link_brush.SetColor(&self.pal.accent);
            gfx.muted_brush.SetColor(&self.pal.muted);
        }
        // Switches the backdrop tint between light and dark.
        let value: i32 = dark as i32;
        let _ = DwmSetWindowAttribute(self.hwnd, DWMWINDOWATTRIBUTE(DWMWA_USE_IMMERSIVE_DARK_MODE_),
            &value as *const i32 as *const c_void, 4);
        sys::set_menu_theme(match self.settings.theme.as_str() { "Dark" => 2, "Light" => 3, _ => 1 });
        self.invalidate();
    }

    unsafe fn apply_backdrop(&self) {
        if !self.backdrop_ok {
            return;
        }
        // "Glass" swaps the system backdrop for a plain, untinted blur of what is behind the window.
        let glass = self.settings.material == "Glass";
        sys::set_blur_behind(self.hwnd, glass);
        // Acryl and Mica need the frame extended over the whole window to show through our
        // transparent pixels; with the blur it is the other way round (extended, it turns black).
        // In glass mode the drawn rim should be the only edge, so ask Windows not to draw its own
        // window border. Some setups ignore this (the border stays); the rim is therefore placed
        // flush against the window edge, where it reads as one bevel together with that border.
        const DWMWA_COLOR_NONE: u32 = 0xFFFF_FFFE;
        const DWMWA_COLOR_DEFAULT: u32 = 0xFFFF_FFFF;
        let border: u32 = if glass { DWMWA_COLOR_NONE } else { DWMWA_COLOR_DEFAULT };
        let _ = DwmSetWindowAttribute(self.hwnd, DWMWA_BORDER_COLOR, &border as *const u32 as *const c_void, 4);
        let m = if glass { 0 } else { -1 };
        let _ = DwmExtendFrameIntoClientArea(self.hwnd, &MARGINS { cxLeftWidth: m, cxRightWidth: m, cyTopHeight: m, cyBottomHeight: m });
        let value: i32 = match self.settings.material.as_str() {
            "Glass" => DWMSBT_NONE,
            "Mica" => DWMSBT_MAINWINDOW,
            _ => DWMSBT_TRANSIENTWINDOW,
        };
        let _ = DwmSetWindowAttribute(self.hwnd, DWMWINDOWATTRIBUTE(DWMWA_SYSTEMBACKDROP_TYPE_),
            &value as *const i32 as *const c_void, 4);
    }

    // ---------- Title bar visibility ----------

    fn chrome_targets(&self) -> (f32, f32, f32) {
        let show = |mode: &str| if mode == "Always" || self.chrome_hover || self.menu_open { 1.0 } else { 0.0 };
        // Pin and menu must stay reachable, so "never" still shows them on hover.
        let tools_mode = if self.settings.buttons_mode == "Always" { "Always" } else { "Hover" };
        (show(&self.settings.title_mode), show(&self.settings.buttons_mode), show(tools_mode))
    }

    unsafe fn update_chrome(&mut self) {
        SetTimer(self.hwnd, T_ANIM, 15, None);
        self.invalidate();
    }

    unsafe fn tick_animation(&mut self) {
        let targets = self.chrome_targets();
        let step = 15.0 / 140.0;
        let approach = |value: &mut f32, target: f32| {
            *value = if *value < target { (*value + step).min(target) } else { (*value - step).max(target) };
            *value == target
        };
        let done = [approach(&mut self.a_title, targets.0), approach(&mut self.a_buttons, targets.1), approach(&mut self.a_tools, targets.2)];
        if done.iter().all(|d| *d) {
            let _ = KillTimer(self.hwnd, T_ANIM);
        }
        self.invalidate();
    }

    /// Recomputes hover state from the real pointer position, so it is correct for client,
    /// non-client and "mouse left the window" messages alike.
    unsafe fn update_hover(&mut self) {
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        let over = GetAncestor(WindowFromPoint(point), GA_ROOT) == self.hwnd;
        let _ = ScreenToClient(self.hwnd, &mut point);
        let (x, y) = (point.x as f32 / self.scale(), point.y as f32 / self.scale());

        let hover = over && y < TITLE_H;
        let hot = if over { self.button_at(x, y) } else { None };
        let lights = over && self.mac_style() && self.buttons_enabled() && contains(&self.rect_lights(), x, y);
        if hover != self.chrome_hover {
            self.chrome_hover = hover;
            self.update_chrome();
        }
        if hot != self.hot || lights != self.lights_hover {
            self.hot = hot;
            self.lights_hover = lights;
            self.invalidate();
        }
    }

    unsafe fn track_leave(&self, non_client: bool) {
        let mut track = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: if non_client { TME_LEAVE | TME_NONCLIENT } else { TME_LEAVE },
            hwndTrack: self.hwnd,
            dwHoverTime: 0,
        };
        let _ = TrackMouseEvent(&mut track);
    }

    // ---------- Drawing ----------

    /// `area` (pixels): only this part needs repainting, e.g. the blinking caret. Everything
    /// is still issued to Direct2D, but clipped, so it costs a fraction of a full frame.
    unsafe fn draw(&mut self, mut area: Option<RECT>) {
        let (pw, ph) = ((self.w * self.scale()).round() as u32, (self.h * self.scale()).round() as u32);
        if self.gfx.is_none() {
            area = None; // a new surface is empty: paint all of it
            // At startup the worker thread has usually finished preparing Direct2D by now.
            let prepared = self.warmup.take().and_then(|worker| worker.join().ok().flatten());
            let Some(prepared) = prepared.or_else(|| gfx::Prepared::new(None).ok()) else { return };
            match Gfx::new(prepared, self.hwnd, pw, ph, self.dpi) {
                Ok(gfx) => {
                    gfx.link_brush.SetColor(&self.pal.accent);
                    gfx.muted_brush.SetColor(&self.pal.muted);
                    self.editor.set_brushes(gfx.link_brush.clone(), gfx.muted_brush.clone());
                    self.sheen = None; // bitmaps belong to the surface they were created on
                    self.gfx = Some(gfx);
                }
                Err(_) => return,
            }
        }
        if matches!(area, Some(a) if a.left <= 0 && a.top <= 0 && a.right >= pw as i32 && a.bottom >= ph as i32) {
            area = None;
        }
        let g = self.gfx.take().unwrap();
        g.dc.BeginDraw();
        if let Some(a) = area {
            let s = self.scale();
            let clip = D2D_RECT_F { left: a.left as f32 / s, top: a.top as f32 / s, right: a.right as f32 / s, bottom: a.bottom as f32 / s };
            g.dc.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_ALIASED);
        }
        // The tint decides how much of the backdrop shows through; without a backdrop it is solid.
        let mut tint = if self.backdrop_ok { self.settings.tint.min(100) as f32 / 100.0 } else { 1.0 };
        if self.glass() {
            // Clear glass has no tint of its own; a little is needed to keep the text readable.
            let base = if self.pal.dark { 0.30 } else { 0.38 };
            tint = base + (1.0 - base) * tint;
        }
        g.dc.Clear(Some(&fade(self.pal.fallback_bg, tint)));
        if self.glass() {
            self.draw_glass_sheen(&g);
        }
        self.draw_title_bar(&g);
        self.draw_editor(&g);
        self.draw_status(&g);
        if self.recording {
            self.draw_hotkey_overlay(&g);
        }
        if self.info_open {
            self.draw_info_overlay(&g);
        }
        if self.glass() {
            self.draw_glass_rim(&g);
        }
        if area.is_some() {
            g.dc.PopAxisAlignedClip();
        }
        if g.dc.EndDraw(None, None).is_ok() && g.present(area).is_ok() {
            self.gfx = Some(g);

        } else {
            // Device lost (driver update, GPU switch, …): rebuild the surface on the next paint.
            self.invalidate();
        }
    }

    // ---------- Glass look ("Glas" material) ----------

    fn glass(&self) -> bool {
        self.backdrop_ok && self.settings.material == "Glass"
    }

    fn white(&self, dark_alpha: f32, light_alpha: f32) -> Color {
        Color { r: 1.0, g: 1.0, b: 1.0, a: if self.pal.dark { dark_alpha } else { light_alpha } }
    }

    /// Soft light falling in from the top edge, as on a pane of glass.
    unsafe fn draw_glass_sheen(&mut self, g: &Gfx) {
        let dark = self.pal.dark;
        // The fade is computed once per theme and then only stretched over the top of the window.
        if self.sheen.as_ref().map(|(for_dark, _)| *for_dark) != Some(dark) {
            self.sheen = g.fade_strip(if dark { 0.10 } else { 0.46 }).ok().map(|strip| (dark, strip));
        }
        if let Some((_, strip)) = &self.sheen {
            g.draw_stretched(strip, rect(0.0, 0.0, self.w, 170.0f32.min(self.h * 0.6)));
        }
    }

    /// Bright line along the window edge: the light caught by the rim of the glass.
    unsafe fn draw_glass_rim(&self, g: &Gfx) {
        // Flush with the window edge and with the same 8-DIP corner radius Windows 11 uses, so
        // there is no gap between it and a window border Windows may draw around it.
        let radius = if IsZoomed(self.hwnd).as_bool() { 0.0 } else { 8.0 };
        let edge = rect(0.5, 0.5, self.w - 1.0, self.h - 1.0);
        g.stroke_round_gradient(edge, radius, self.white(0.42, 0.95), self.white(0.14, 0.42), 1.0);
    }

    /// A raised panel (find bar, overlay cards): frosted and rimmed in glass mode, plain otherwise.
    unsafe fn draw_card(&self, g: &Gfx, r: D2D_RECT_F, radius: f32) {
        let p = self.pal;
        if self.glass() {
            let top = fade(p.card_bg, 0.72);
            g.fill_round_gradient(r, radius, Color { r: (top.r + 0.06).min(1.0), g: (top.g + 0.06).min(1.0), b: (top.b + 0.06).min(1.0), ..top }, top);
            g.stroke_round_gradient(r, radius, self.white(0.34, 0.95), self.white(0.06, 0.30), 1.0);
        } else {
            g.fill_round(r, radius, p.card_bg);
            g.stroke_round(r, radius, p.card_border, 1.0);
        }
    }

    unsafe fn draw_title_bar(&mut self, g: &Gfx) {
        let p = self.pal;
        let mac = self.mac_style();

        if self.settings.title_mode != "Never" && self.a_title > 0.01 {
            let color = fade(p.fg, self.a_title * if self.active { 0.85 } else { 0.45 });
            // Centred in both button styles.
            let line = rect(0.0, 0.0, self.w, self.title_center_y() * 2.0);
            g.text("Scratchpad", &self.fonts.title, line, color, DWRITE_TEXT_ALIGNMENT_CENTER);
        }

        let a = self.a_buttons;
        if self.buttons_enabled() && a > 0.01 {
            if mac {
                self.draw_traffic_lights(g, a);
            } else {
                let maximized = IsZoomed(self.hwnd).as_bool();
                let glyphs = ["\u{E921}", if maximized { "\u{E923}" } else { "\u{E922}" }, "\u{E8BB}"];
                for (i, button) in [Btn::Minimize, Btn::Maximize, Btn::Close].into_iter().enumerate() {
                    let r = self.rect_win_button(i);
                    let mut color = p.fg;
                    if self.hot == Some(button) {
                        if button == Btn::Close {
                            g.fill_rect(r, fade(rgb(0xC4, 0x2B, 0x1C), a));
                            color = rgb(255, 255, 255);
                        } else {
                            g.fill_rect(r, fade(p.hover, a));
                        }
                    }
                    g.text(glyphs[i], &self.fonts.icon_small, r, fade(color, a), DWRITE_TEXT_ALIGNMENT_CENTER);
                }
            }
        }

        let a = self.a_tools;
        if a > 0.01 && self.glass() {
            // Glass capsule holding pin and menu button, like toolbar groups on macOS.
            let (pin, menu) = (self.rect_pin_button(), self.rect_menu_button());
            let (left, right) = (pin.left.min(menu.left), pin.right.max(menu.right));
            let capsule = rect(left - 5.0, pin.top - 2.0, right - left + 10.0, 28.0);
            g.fill_round_gradient(capsule, 14.0, fade(self.white(0.13, 0.62), a), fade(self.white(0.05, 0.38), a));
            g.stroke_round_gradient(capsule, 14.0, fade(self.white(0.36, 0.95), a), fade(self.white(0.05, 0.30), a), 1.0);
        }
        if a > 0.01 {
            let tools = [
                (Btn::Pin, self.rect_pin_button(), if self.settings.topmost { "\u{E840}" } else { "\u{E718}" }, self.settings.topmost),
                (Btn::Menu, self.rect_menu_button(), "\u{E712}", self.menu_open),
            ];
            for (button, r, glyph, checked) in tools {
                let hot = self.hot == Some(button);
                if hot || checked {
                    g.fill_round(r, 6.0, fade(p.hover, a));
                }
                let color = if checked { p.accent } else if hot { p.fg } else { p.muted };
                g.text(glyph, &self.fonts.icon, r, fade(color, a), DWRITE_TEXT_ALIGNMENT_CENTER);
            }
        }
    }

    /// macOS-style close / minimise / zoom buttons; the symbols only appear on hover.
    unsafe fn draw_traffic_lights(&self, g: &Gfx, a: f32) {
        let lights = [
            (Btn::Close, rgb(0xFF, 0x5F, 0x57), rgb(0xE2, 0x46, 0x3F), rgb(0x8C, 0x1A, 0x10)),
            (Btn::Minimize, rgb(0xFE, 0xBC, 0x2E), rgb(0xE1, 0xA1, 0x16), rgb(0x98, 0x57, 0x12)),
            (Btn::Maximize, rgb(0x28, 0xC8, 0x40), rgb(0x1A, 0xAB, 0x29), rgb(0x0B, 0x65, 0x15)),
        ];
        let dimmed = !self.active && !self.lights_hover;
        for (i, (button, fill, ring, glyph)) in lights.into_iter().enumerate() {
            let (cx, cy) = Self::light_center(i);
            if dimmed {
                g.fill_circle(cx, cy, 6.0, fade(self.pal.inactive_dot, a));
                continue;
            }
            g.fill_circle(cx, cy, 6.0, fade(ring, a));
            g.fill_circle(cx, cy, 5.5, fade(fill, a));
            if self.lights_hover {
                let glyph = fade(glyph, a);
                match button {
                    Btn::Close => {
                        g.line(cx - 2.4, cy - 2.4, cx + 2.4, cy + 2.4, glyph, 1.2);
                        g.line(cx + 2.4, cy - 2.4, cx - 2.4, cy + 2.4, glyph, 1.2);
                    }
                    Btn::Minimize => g.line(cx - 3.0, cy, cx + 3.0, cy, glyph, 1.2),
                    _ => self.draw_zoom_glyph(g, cx, cy, glyph),
                }
            }
            if self.pressed == Some(button) {
                g.fill_circle(cx, cy, 6.0, rgba(0, 0, 0, 51));
            }
        }
    }

    /// Two small triangles pointing to opposite corners, like the macOS zoom button.
    unsafe fn draw_zoom_glyph(&self, g: &Gfx, cx: f32, cy: f32, color: Color) {
        let point = |dx: f32, dy: f32| D2D_POINT_2F { x: cx + dx, y: cy + dy };
        let Ok(geometry) = g.factory.CreatePathGeometry() else { return };
        let Ok(sink) = geometry.Open() else { return };
        for triangle in [[(-2.4, -2.4), (1.2, -2.4), (-2.4, 1.2)], [(2.4, 2.4), (-1.2, 2.4), (2.4, -1.2)]] {
            sink.BeginFigure(point(triangle[0].0, triangle[0].1), D2D1_FIGURE_BEGIN_FILLED);
            sink.AddLines(&[point(triangle[1].0, triangle[1].1), point(triangle[2].0, triangle[2].1)]);
            sink.EndFigure(D2D1_FIGURE_END_CLOSED);
        }
        if sink.Close().is_ok() {
            g.brush.SetColor(&color);
            g.dc.FillGeometry(&geometry, &g.brush, None);
        }
    }

    unsafe fn draw_editor(&mut self, g: &Gfx) {
        let p = self.pal;
        let clip_top = VIEW_TOP + self.search_offset();
        let text_top = self.text_top();
        let view = rect(0.0, clip_top, self.w, self.view_bottom() - clip_top);
        g.dc.PushAxisAlignedClip(&view, D2D1_ANTIALIAS_MODE_ALIASED);
        if self.editor.text.is_empty() {
            let line = rect(TEXT_X, text_top, self.w - TEXT_X, self.settings.font_size * 1.35);
            g.text("Schreib einfach los …", &self.fonts.placeholder, line, p.placeholder, DWRITE_TEXT_ALIGNMENT_LEADING);
        }
        let caret = (self.focused && self.caret_on && !self.recording && !self.info_open && self.search.is_none()).then_some(p.accent);
        self.editor.draw(g, TEXT_X, text_top, clip_top, self.view_bottom(), p.fg, fade(p.accent, 0.35), fade(p.accent, 0.16), caret);
        g.dc.PopAxisAlignedClip();

        if let Some((_, _, thumb_top, thumb_h)) = self.scrollbar() {
            g.fill_round(rect(self.w - 10.0, thumb_top, 6.0, thumb_h), 3.0, p.thumb);
        }
        self.draw_search_bar(g);
    }

    fn rect_search_bar(&self) -> D2D_RECT_F {
        let width = 300.0f32.min(self.w - 36.0);
        let replace = self.search.as_ref().is_some_and(|s| s.replace_mode);
        rect(self.w - 18.0 - width, VIEW_TOP + 4.0, width, if replace { 68.0 } else { 32.0 })
    }

    /// The "replace" and "replace all" buttons in the second row of the find bar.
    fn rect_replace_buttons(&self) -> (D2D_RECT_F, D2D_RECT_F) {
        let bar = self.rect_search_bar();
        if !self.search.as_ref().is_some_and(|s| s.replace_mode) {
            return (rect(0.0, 0.0, 0.0, 0.0), rect(0.0, 0.0, 0.0, 0.0));
        }
        let top = bar.top + 39.0;
        (rect(bar.right - 122.0, top, 70.0, 22.0), rect(bar.right - 48.0, top, 40.0, 22.0))
    }

    unsafe fn draw_search_bar(&self, g: &Gfx) {
        let Some(search) = &self.search else { return };
        let p = self.pal;
        let bar = self.rect_search_bar();
        self.draw_card(g, bar, if self.glass() { 16.0 } else { 8.0 });

        let count = if search.query.is_empty() {
            String::new()
        } else if search.matches.is_empty() {
            "Keine Treffer".to_string()
        } else {
            format!("{} / {}", search.current + 1, search.matches.len())
        };
        let count_width = if count.is_empty() { 8.0 } else { 86.0 };
        g.text(&count, &self.fonts.status, rect(bar.right - count_width - 10.0, bar.top, count_width, 32.0), p.muted, DWRITE_TEXT_ALIGNMENT_TRAILING);

        // One input row: icon, text or placeholder, and the caret if this row has it.
        let row = |top: f32, icon: &str, text: &[u16], placeholder: &str, reserved_right: f32, has_caret: bool| {
            g.text(icon, &self.fonts.icon, rect(bar.left + 4.0, top, 28.0, 32.0), p.muted, DWRITE_TEXT_ALIGNMENT_CENTER);
            let field = rect(bar.left + 32.0, top, bar.right - bar.left - 32.0 - reserved_right, 32.0);
            g.dc.PushAxisAlignedClip(&field, D2D1_ANTIALIAS_MODE_ALIASED);
            if text.is_empty() {
                g.text(placeholder, &self.fonts.search, field, p.placeholder, DWRITE_TEXT_ALIGNMENT_LEADING);
            } else {
                g.text(&String::from_utf16_lossy(text), &self.fonts.search, field, p.fg, DWRITE_TEXT_ALIGNMENT_LEADING);
            }
            if has_caret && self.caret_on && self.focused {
                // Caret at the end of the text: measure it to find its right edge.
                let mut metrics = DWRITE_TEXT_METRICS::default();
                if let Ok(layout) = self.fonts.dwrite.CreateTextLayout(text, &self.fonts.search, 10_000.0, 32.0) {
                    let _ = layout.GetMetrics(&mut metrics);
                }
                g.fill_rect(rect(field.left + metrics.widthIncludingTrailingWhitespace, top + 8.0, 1.5, 16.0), p.accent);
            }
            g.dc.PopAxisAlignedClip();
        };
        row(bar.top, "\u{E721}", &search.query, "Suchen", count_width + 14.0, !search.in_replacement);
        if search.replace_mode {
            g.fill_rect(rect(bar.left + 10.0, bar.top + 33.5, bar.right - bar.left - 20.0, 1.0), p.card_border);
            row(bar.top + 34.0, "\u{E70F}", &search.replacement, "Ersetzen durch", 130.0, search.in_replacement);
            let (one, all) = self.rect_replace_buttons();
            for (button, label) in [(one, "Ersetzen"), (all, "Alle")] {
                g.fill_round(button, 6.0, p.hover);
                g.text(label, &self.fonts.status, button, p.fg, DWRITE_TEXT_ALIGNMENT_CENTER);
            }
        }
    }

    unsafe fn draw_status(&self, g: &Gfx) {
        let line = rect(TEXT_X, self.h - STATUS_H + 2.0, self.w - TEXT_X - 22.0, 18.0);
        g.text(&self.stats, &self.fonts.status, line, self.pal.muted, DWRITE_TEXT_ALIGNMENT_LEADING);
        if !self.saved_label.is_empty() {
            g.text(&self.saved_label, &self.fonts.status, line, self.pal.muted, DWRITE_TEXT_ALIGNMENT_TRAILING);
        }
    }

    unsafe fn draw_info_overlay(&self, g: &Gfx) {
        let p = self.pal;
        g.fill_rect(rect(0.0, TITLE_H, self.w, self.h - TITLE_H), rgba(0, 0, 0, 0x55));
        let (cw, ch) = (280.0f32.min(self.w - 24.0), 132.0);
        let card = rect((self.w - cw) / 2.0, TITLE_H + (self.h - TITLE_H - ch) / 2.0, cw, ch);
        self.draw_card(g, card, if self.glass() { 18.0 } else { 12.0 });
        let line = |top: f32| rect(card.left, card.top + top, cw, 22.0);
        g.text("Scratchpad", &self.fonts.overlay_title, line(20.0), p.fg, DWRITE_TEXT_ALIGNMENT_CENTER);
        g.text(&format!("Version {VERSION}"), &self.fonts.overlay_hint, line(44.0), p.muted, DWRITE_TEXT_ALIGNMENT_CENTER);
        g.text("Made by Rick ❤️ Claudecode", &self.fonts.overlay_hint, line(86.0), p.fg, DWRITE_TEXT_ALIGNMENT_CENTER);
    }

    unsafe fn draw_hotkey_overlay(&self, g: &Gfx) {
        let p = self.pal;
        g.fill_rect(rect(0.0, TITLE_H, self.w, self.h - TITLE_H), rgba(0, 0, 0, 0x55));
        let (cw, ch) = (300.0f32.min(self.w - 24.0), 104.0);
        let card = rect((self.w - cw) / 2.0, TITLE_H + (self.h - TITLE_H - ch) / 2.0, cw, ch);
        self.draw_card(g, card, if self.glass() { 18.0 } else { 12.0 });
        g.text("Neues Tastenkürzel drücken", &self.fonts.overlay_title,
            rect(card.left, card.top + 14.0, cw, 24.0), p.fg, DWRITE_TEXT_ALIGNMENT_CENTER);
        g.text(&self.hint, &self.fonts.overlay_hint,
            rect(card.left + 16.0, card.top + 40.0, cw - 32.0, ch - 50.0), p.muted, DWRITE_TEXT_ALIGNMENT_CENTER);
    }

    // ---------- Editing ----------

    fn update_stats(&mut self) {
        let text = &self.editor.text;
        let is_space = |c: u16| matches!(c, 9 | 10 | 13 | 32 | 0xA0);
        let words = text.iter().enumerate().filter(|(i, &c)| !is_space(c) && (*i == 0 || is_space(text[i - 1]))).count();
        self.stats = format!("{} {}  ·  {} Zeichen", words, if words == 1 { "Wort" } else { "Wörter" }, text.len());
    }

    /// Pixel rectangle that contains the blinking caret (in the note or in the find bar).
    fn caret_area(&mut self) -> RECT {
        let (left, top, right, bottom) = if self.search.is_some() {
            let bar = self.rect_search_bar();
            (bar.left, bar.top, bar.right, bar.bottom)
        } else {
            let (x, y, height) = self.editor.caret_position();
            let top = self.text_top() + y - self.editor.scroll;
            (TEXT_X + x - 2.0, top - 1.0, TEXT_X + x + 3.0, top + height + 1.0)
        };
        let s = self.scale();
        RECT { left: (left * s).floor() as i32, top: (top * s).floor() as i32, right: (right * s).ceil() as i32, bottom: (bottom * s).ceil() as i32 }
    }

    unsafe fn reset_blink(&mut self) {
        self.caret_on = true;
        if self.focused {
            SetTimer(self.hwnd, T_BLINK, GetCaretBlinkTime().clamp(200, 2000), None);
        }
    }

    unsafe fn after_edit(&mut self) {
        self.dirty = true;
        // Counting words walks the whole text. Instant for normal notes; for very long ones it
        // waits for the short pause before saving, so typing never pays for it.
        if self.editor.text.len() < 20_000 {
            self.update_stats();
        }
        SetTimer(self.hwnd, T_SAVE, 500, None);
        self.after_move();
    }

    unsafe fn after_move(&mut self) {
        let view = self.view_height();
        self.editor.scroll_to_caret(view);
        self.reset_blink();
        self.invalidate();
    }

    unsafe fn save_note(&mut self, show_label: bool) {
        if !self.dirty {
            return;
        }
        // Someone else changed the file since we read it (sync from another computer):
        // keep their version next to the note instead of silently overwriting it.
        if settings::modified(&self.note_path) != self.note_mtime {
            settings::backup_note(&self.note_path, &timestamp());
        }
        // Windows line endings on disk, so the file opens cleanly everywhere.
        match settings::save_note(&self.note_path, &self.editor.text_string().replace('\n', "\r\n")) {
            Ok(()) => {
                self.dirty = false;
                self.note_mtime = settings::modified(&self.note_path);
                self.saved_label = if show_label { "Gespeichert".into() } else { String::new() };
            }
            Err(error) => self.saved_label = format!("Speichern fehlgeschlagen: {error}"),
        }
        SetTimer(self.hwnd, T_SAVED_LABEL, 1500, None);
        self.invalidate();
    }

    /// Picks up a note that was changed on disk (e.g. synced from another computer).
    /// With unsaved local edits the file is left alone; save_note() then keeps both versions.
    unsafe fn reload_if_changed(&mut self) {
        let on_disk = settings::modified(&self.note_path);
        if self.dirty || on_disk.is_none() || on_disk == self.note_mtime {
            return;
        }
        self.note_mtime = on_disk;
        let text = settings::load_note(&self.note_path);
        if text != self.editor.text_string() {
            self.editor.replace_text(&text);
            self.search = None;
            self.editor.highlights.clear();
            self.update_stats();
            self.after_move();
        }
    }

    /// Moves the note to another folder. `load_existing`: use the note already lying there
    /// instead of writing the current text into it.
    unsafe fn set_note_folder(&mut self, folder: Option<PathBuf>, load_existing: bool) {
        let _ = KillTimer(self.hwnd, T_SAVE);
        self.save_note(false); // the old place keeps the latest text
        self.settings.note_folder = folder.map(|f| f.to_string_lossy().into_owned());
        self.settings.save();
        self.note_path = self.settings.note_path();
        if load_existing {
            self.note_mtime = None;
            self.dirty = false;
            self.reload_if_changed();
        } else {
            settings::backup_note(&self.note_path, &timestamp());
            self.note_mtime = settings::modified(&self.note_path);
            self.dirty = true;
            self.save_note(true);
        }
        self.invalidate();
    }

    // ---------- Search ----------

    /// `replace`: also show the "replace with" row (Ctrl+H).
    unsafe fn open_search(&mut self, replace: bool) {
        // Start with the selected word, like most editors do.
        let selected = self.editor.selected_text();
        let previous = self.search.take();
        let query = if !selected.is_empty() && selected.len() <= 80 && !selected.contains(&10) {
            selected
        } else {
            previous.as_ref().map(|s| s.query.clone()).unwrap_or_default()
        };
        let replacement = previous.map(|s| s.replacement).unwrap_or_default();
        self.search = Some(Search { query, replacement, replace_mode: replace, in_replacement: false, matches: Vec::new(), current: 0 });
        self.run_search();
    }

    unsafe fn close_search(&mut self) {
        self.search = None;
        self.editor.highlights.clear();
        self.reset_blink();
        self.invalidate();
    }

    /// Recomputes all hits and jumps to the first one at or after the current position.
    unsafe fn run_search(&mut self) {
        let Some(search) = &mut self.search else { return };
        let needle: Vec<u16> = search.query.iter().map(|&c| lower(c)).collect();
        search.matches.clear();
        if !needle.is_empty() && needle.len() <= self.editor.text.len() {
            let haystack: Vec<u16> = self.editor.text.iter().map(|&c| lower(c)).collect();
            let mut i = 0;
            while i + needle.len() <= haystack.len() {
                if haystack[i] == needle[0] && haystack[i..i + needle.len()] == needle[..] {
                    search.matches.push(i);
                    i += needle.len();
                } else {
                    i += 1;
                }
            }
        }
        let from = self.editor.selection().0;
        search.current = search.matches.iter().position(|&m| m >= from).unwrap_or(0);
        let length = needle.len();
        self.editor.highlights = search.matches.iter().map(|&m| (m, m + length)).collect();
        self.show_match();
    }

    unsafe fn show_match(&mut self) {
        if let Some(search) = &self.search {
            if let Some(&start) = search.matches.get(search.current) {
                self.editor.select_range(start, start + search.query.len());
            }
        }
        self.after_move();
    }

    unsafe fn search_step(&mut self, forward: bool) {
        if let Some(search) = &mut self.search {
            let count = search.matches.len();
            if count > 0 {
                search.current = (search.current + if forward { 1 } else { count - 1 }) % count;
            }
        }
        self.show_match();
    }

    /// Replaces the current hit and moves on to the next one.
    unsafe fn replace_current(&mut self) {
        let Some(search) = &self.search else { return };
        let Some(&start) = search.matches.get(search.current) else { return };
        let (length, replacement) = (search.query.len(), search.replacement.clone());
        self.editor.select_range(start, start + length);
        self.editor.insert(&replacement, Edit::Other);
        self.after_edit();
        self.run_search();
    }

    /// Replaces every hit in one step (a single Ctrl+Z brings the old text back).
    unsafe fn replace_all(&mut self) {
        let Some(search) = &self.search else { return };
        if search.matches.is_empty() {
            return;
        }
        let (length, count) = (search.query.len(), search.matches.len());
        let old = &self.editor.text;
        let mut new = Vec::with_capacity(old.len());
        let mut from = 0;
        for &start in &search.matches {
            new.extend_from_slice(&old[from..start]);
            new.extend_from_slice(&search.replacement);
            from = start + length;
        }
        new.extend_from_slice(&old[from..]);
        self.editor.select_all();
        self.editor.insert(&new, Edit::Other);
        self.editor.set_caret(0, false);
        self.after_edit();
        self.run_search();
        self.saved_label = format!("{count} ersetzt");
        SetTimer(self.hwnd, T_SAVED_LABEL, 1500, None);
    }

    /// Changes the field of the find bar that currently has the caret.
    unsafe fn edit_search_field(&mut self, edit: impl FnOnce(&mut Vec<u16>)) {
        let Some(search) = &mut self.search else { return };
        let in_replacement = search.in_replacement;
        edit(if in_replacement { &mut search.replacement } else { &mut search.query });
        if in_replacement {
            self.reset_blink();
            self.invalidate();
        } else {
            self.run_search();
        }
    }

    /// Keys while the find bar is open.
    unsafe fn on_search_key(&mut self, vk: u32) {
        let ctrl = sys::key_down(VK_CONTROL) && !sys::key_down(VK_MENU);
        let (replace_mode, in_replacement) = self.search.as_ref().map(|s| (s.replace_mode, s.in_replacement)).unwrap_or_default();
        match VIRTUAL_KEY(vk as u16) {
            VK_ESCAPE => self.close_search(),
            VK_TAB if replace_mode => {
                if let Some(search) = &mut self.search {
                    search.in_replacement = !search.in_replacement;
                }
                self.reset_blink();
                self.invalidate();
            }
            VK_RETURN if replace_mode && ctrl => self.replace_all(),
            VK_RETURN if in_replacement => self.replace_current(),
            VK_RETURN | VK_F3 => self.search_step(!sys::key_down(VK_SHIFT)),
            VK_DOWN => self.search_step(true),
            VK_UP => self.search_step(false),
            VK_BACK => self.edit_search_field(|field| {
                if ctrl {
                    field.clear();
                } else if let Some(last) = field.pop() {
                    // Remove both halves of a surrogate pair (emoji).
                    if (0xDC00..=0xDFFF).contains(&last) {
                        field.pop();
                    }
                }
            }),
            _ if ctrl && vk == 0x56 => {
                let pasted = self.clipboard_text();
                self.edit_search_field(|field| field.extend(pasted.into_iter().take_while(|&c| c != 10)));
            }
            _ if ctrl && vk == 0x48 => self.open_search(true),
            _ if ctrl && vk == 0x46 => self.open_search(false),
            _ if ctrl && vk == 0x51 => self.cmds.push(Cmd::Exit),
            _ => {}
        }
    }

    /// Mouse click inside the find bar: buttons, or put the caret into the clicked row.
    unsafe fn on_search_click(&mut self, x: f32, y: f32) {
        let (replace_one, replace_all) = self.rect_replace_buttons();
        if contains(&replace_one, x, y) {
            self.replace_current();
        } else if contains(&replace_all, x, y) {
            self.replace_all();
        } else if let Some(search) = &mut self.search {
            search.in_replacement = search.replace_mode && y >= VIEW_TOP + 4.0 + 34.0;
            self.reset_blink();
            self.invalidate();
        }
    }

    /// Saves the note, window placement and settings (before hiding or quitting).
    unsafe fn persist(&mut self) {
        let _ = KillTimer(self.hwnd, T_SAVE);
        self.save_note(false);
        let mut placement = WINDOWPLACEMENT { length: std::mem::size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
        if GetWindowPlacement(self.hwnd, &mut placement).is_ok() {
            let r = placement.rcNormalPosition;
            self.settings.win_rect = Some([r.left, r.top, r.right, r.bottom]);
            if IsWindowVisible(self.hwnd).as_bool() {
                self.settings.maximized = placement.showCmd == SW_SHOWMAXIMIZED.0 as u32
                    || (placement.showCmd == SW_SHOWMINIMIZED.0 as u32 && (placement.flags.0 & WPF_RESTORETOMAXIMIZED.0) != 0);
            }
        }
        self.settings.save();
    }

    fn font_family(&self) -> String {
        self.settings.font.clone().unwrap_or_else(|| self.fonts.mono_family.clone())
    }

    /// Applies font family, size, line spacing and the Markdown switch from the settings.
    unsafe fn apply_font(&mut self) {
        let (family, size, spacing) = (self.font_family(), self.settings.font_size, self.settings.line_spacing);
        self.editor.set_format(self.fonts.editor_format(&family, size, spacing));
        self.editor.set_style(spacing, self.settings.markdown);
        self.fonts.placeholder = Fonts::format(&self.fonts.dwrite, &family, size, DWRITE_FONT_WEIGHT_NORMAL);
        self.settings.save();
        self.after_move();
    }

    unsafe fn change_font_size(&mut self, size: f32) {
        self.settings.font_size = size.clamp(10.0, 40.0);
        self.apply_font();
    }

    /// Ctrl+S: the note saves itself anyway, but confirm it the way people expect.
    unsafe fn save_now(&mut self) {
        let _ = KillTimer(self.hwnd, T_SAVE);
        if self.dirty {
            self.save_note(true);
        } else {
            self.saved_label = "Gespeichert".into();
            SetTimer(self.hwnd, T_SAVED_LABEL, 1500, None);
            self.invalidate();
        }
    }

    unsafe fn copy(&self) {
        if !self.editor.has_selection() {
            return;
        }
        let mut data: Vec<u16> = Vec::new();
        for c in self.editor.selected_text() {
            if c == 10 {
                data.push(13);
            }
            data.push(c);
        }
        data.push(0);
        if OpenClipboard(self.hwnd).is_err() {
            return;
        }
        let _ = EmptyClipboard();
        if let Ok(memory) = GlobalAlloc(GMEM_MOVEABLE, data.len() * 2) {
            let target = GlobalLock(memory) as *mut u16;
            if !target.is_null() {
                std::ptr::copy_nonoverlapping(data.as_ptr(), target, data.len());
                let _ = GlobalUnlock(memory);
                // On success the clipboard owns the memory.
                let _ = SetClipboardData(13 /* CF_UNICODETEXT */, HANDLE(memory.0));
            }
        }
        let _ = CloseClipboard();
    }

    /// Clipboard text with line breaks normalised to '\n'.
    unsafe fn clipboard_text(&self) -> Vec<u16> {
        let mut text: Vec<u16> = Vec::new();
        if OpenClipboard(self.hwnd).is_err() {
            return text;
        }
        if let Ok(handle) = GetClipboardData(13 /* CF_UNICODETEXT */) {
            let memory = HGLOBAL(handle.0);
            let source = GlobalLock(memory) as *const u16;
            if !source.is_null() {
                let mut i = 0;
                while *source.add(i) != 0 {
                    let c = *source.add(i);
                    if c != 13 {
                        text.push(c);
                    }
                    i += 1;
                }
                let _ = GlobalUnlock(memory);
            }
        }
        let _ = CloseClipboard();
        text
    }

    unsafe fn paste(&mut self) {
        let text = self.clipboard_text();
        if !text.is_empty() {
            self.editor.insert(&text, Edit::Other);
            self.after_edit();
        }
    }

    unsafe fn cut(&mut self) {
        self.copy();
        if self.editor.delete_selection() {
            self.after_edit();
        }
    }

    /// Returns true if the key was handled.
    unsafe fn on_key(&mut self, vk: u32) -> bool {
        if self.recording {
            self.record_hotkey(vk);
            return true;
        }
        if self.info_open {
            self.info_open = false; // any key dismisses the info card
            self.invalidate();
            return true;
        }
        if self.menu.is_some() {
            self.on_menu_key(vk);
            return true;
        }
        if self.search.is_some() {
            self.on_search_key(vk);
            return true;
        }
        let shift = sys::key_down(VK_SHIFT);
        // AltGr arrives as Ctrl+Alt; those are characters ("@", "}", …), never shortcuts.
        let ctrl = sys::key_down(VK_CONTROL) && !sys::key_down(VK_MENU);
        let key = VIRTUAL_KEY(vk as u16);
        let view = self.view_height();

        let mut edited = false;
        match key {
            VK_LEFT => self.editor.move_left(shift, ctrl),
            VK_RIGHT => self.editor.move_right(shift, ctrl),
            VK_UP => self.editor.move_lines(-1.0, shift),
            VK_DOWN => self.editor.move_lines(1.0, shift),
            VK_PRIOR => self.editor.move_page(view, false, shift),
            VK_NEXT => self.editor.move_page(view, true, shift),
            VK_HOME => self.editor.move_home(shift, ctrl),
            VK_END => self.editor.move_end(shift, ctrl),
            VK_BACK => edited = self.editor.backspace(ctrl),
            VK_DELETE => edited = self.editor.delete(ctrl),
            // Ctrl+Enter ticks or unticks the task the caret is in.
            VK_RETURN if ctrl => {
                if let Some(inner) = self.editor.checkbox_in_caret_line().filter(|_| self.settings.markdown) {
                    self.editor.toggle_checkbox(inner);
                    edited = true;
                }
            }
            VK_RETURN => {
                self.editor.enter(self.settings.markdown);
                edited = true;
            }
            VK_TAB if !ctrl => {
                self.editor.insert(&[9], Edit::Type);
                edited = true;
            }
            VK_ESCAPE => {
                // Like Spotlight: Esc puts the window away.
                self.cmds.push(Cmd::Hide);
                return true;
            }
            _ if ctrl => match vk {
                0x46 => {
                    self.open_search(false);
                    return true;
                }
                0x48 => {
                    self.open_search(true);
                    return true;
                }
                0x53 => {
                    self.save_now();
                    return true;
                }
                0x42 if self.settings.markdown => {
                    self.editor.toggle_bold();
                    edited = true;
                }
                0x41 => self.editor.select_all(),
                0x43 => self.copy(),
                0x58 => {
                    self.cut();
                    return true;
                }
                0x56 => {
                    self.paste();
                    return true;
                }
                0x5A if shift => edited = self.editor.redo(),
                0x5A => edited = self.editor.undo(),
                0x59 => edited = self.editor.redo(),
                0x51 => {
                    self.cmds.push(Cmd::Exit);
                    return true;
                }
                0xBB | 0x6B => {
                    self.change_font_size(self.settings.font_size + 1.0);
                    return true;
                }
                0xBD | 0x6D => {
                    self.change_font_size(self.settings.font_size - 1.0);
                    return true;
                }
                0x30 | 0x60 => {
                    self.change_font_size(settings::DEFAULT_FONT_SIZE);
                    return true;
                }
                _ => return false,
            },
            _ => return false,
        }
        if edited { self.after_edit() } else { self.after_move() }
        true
    }

    unsafe fn on_char(&mut self, c: u16) {
        // Control characters come from shortcuts (Ctrl+A = 1, Backspace = 8, …) handled in on_key.
        if self.recording || self.info_open || self.menu.is_some() || c < 32 || c == 127 {
            return;
        }
        if self.search.is_some() {
            self.edit_search_field(|field| field.push(c));
            return;
        }
        self.editor.insert(&[c], Edit::Type);
        self.after_edit();
    }

    // ---------- Mouse ----------

    fn link_under(&mut self, x: f32, y: f32) -> Option<String> {
        let scroll = self.editor.scroll;
        let index = self.editor.char_at(x - TEXT_X, y - self.text_top() + scroll)?;
        self.editor.link_at(index)
    }

    fn editor_index_at(&mut self, x: f32, y: f32) -> usize {
        let scroll = self.editor.scroll;
        self.editor.index_at(x - TEXT_X, y - self.text_top() + scroll)
    }

    unsafe fn on_mouse_down(&mut self, x: f32, y: f32) {
        if self.recording {
            return;
        }
        if self.info_open {
            self.info_open = false;
            self.invalidate();
            return;
        }
        if self.menu.is_some() {
            // Any click in the window closes the menu; on its own button that is all it does.
            let on_menu_button = self.button_at(x, y) == Some(Btn::Menu);
            self.close_menu();
            if on_menu_button {
                return;
            }
        }
        if let Some(button) = self.button_at(x, y) {
            self.pressed = Some(button);
            SetCapture(self.hwnd);
            self.invalidate();
            return;
        }
        if y < VIEW_TOP || y >= self.view_bottom() {
            return;
        }

        if x >= self.w - 14.0 {
            if let Some((track_top, track_h, thumb_top, thumb_h)) = self.scrollbar() {
                // Click on the track jumps there; then (or on the thumb) dragging scrolls.
                let grab = if y >= thumb_top && y < thumb_top + thumb_h { y - thumb_top } else { thumb_h / 2.0 };
                self.thumb_drag = Some(grab);
                SetCapture(self.hwnd);
                self.drag_thumb(y, track_top, track_h, thumb_h);
                return;
            }
        }

        if self.search.is_some() {
            if contains(&self.rect_search_bar(), x, y) {
                self.on_search_click(x, y);
                return;
            }
            self.close_search(); // clicking into the text continues editing
        }

        // A click on a Markdown checkbox ticks or unticks it.
        if self.settings.markdown {
            let scroll = self.editor.scroll;
            let hit = self.editor.char_at(x - TEXT_X, y - self.text_top() + scroll);
            if let Some(inner) = hit.and_then(|index| self.editor.checkbox_at(index)) {
                self.editor.toggle_checkbox(inner);
                self.after_edit();
                return;
            }
        }

        // Ctrl+click opens a web address.
        if sys::key_down(VK_CONTROL) && !sys::key_down(VK_MENU) {
            if let Some(url) = self.link_under(x, y) {
                self.cmds.push(Cmd::OpenUrl(url));
                return;
            }
        }

        let now = Instant::now();
        let repeated = now.duration_since(self.last_click) < Duration::from_millis(GetDoubleClickTime() as u64)
            && (x - self.last_click_pos.0).abs() < 4.0
            && (y - self.last_click_pos.1).abs() < 4.0;
        self.click_count = if repeated { self.click_count + 1 } else { 1 };
        self.last_click = now;
        self.last_click_pos = (x, y);

        let index = self.editor_index_at(x, y);
        match self.click_count {
            1 => {
                self.editor.set_caret(index, sys::key_down(VK_SHIFT));
                self.selecting = true;
            }
            2 => self.editor.select_word_at(index),
            _ => {
                // Triple click: the whole line.
                self.editor.set_caret(index, false);
                self.editor.move_home(false, false);
                self.editor.move_end(true, false);
            }
        }
        SetCapture(self.hwnd);
        self.reset_blink();
        self.invalidate();
    }

    unsafe fn drag_thumb(&mut self, y: f32, track_top: f32, track_h: f32, thumb_h: f32) {
        let Some(grab) = self.thumb_drag else { return };
        let view = self.view_height();
        let range = (track_h - thumb_h).max(1.0);
        let fraction = ((y - grab - track_top) / range).clamp(0.0, 1.0);
        self.editor.scroll = fraction * self.editor.max_scroll(view);
        self.invalidate();
    }

    unsafe fn on_mouse_move(&mut self, x: f32, y: f32) {
        if self.thumb_drag.is_some() {
            if let Some((track_top, track_h, _, thumb_h)) = self.scrollbar() {
                self.drag_thumb(y, track_top, track_h, thumb_h);
            }
        } else if self.selecting {
            let index = self.editor_index_at(x, y);
            self.editor.set_caret(index, true);
            self.after_move();
        } else {
            self.track_leave(false);
            self.update_hover();
        }
    }

    unsafe fn on_mouse_up(&mut self, x: f32, y: f32) {
        let pressed = self.pressed.take();
        self.selecting = false;
        self.thumb_drag = None;
        let _ = ReleaseCapture();
        if pressed.is_some() && self.button_at(x, y) == pressed {
            match pressed.unwrap() {
                Btn::Close => self.cmds.push(Cmd::Hide),
                Btn::Minimize => self.cmds.push(Cmd::Minimize),
                Btn::Maximize => self.cmds.push(Cmd::ToggleMaximize),
                Btn::Menu => self.toggle_menu(),
                Btn::Pin => {
                    self.settings.topmost = !self.settings.topmost;
                    self.settings.save();
                    self.cmds.push(Cmd::Topmost(self.settings.topmost));
                }
            }
        }
        self.update_hover();
        self.invalidate();
    }

    unsafe fn on_wheel(&mut self, delta: i32) {
        if sys::key_down(VK_CONTROL) {
            self.change_font_size(self.settings.font_size + if delta > 0 { 1.0 } else { -1.0 });
            return;
        }
        let line = self.editor.line_height();
        let view = self.view_height();
        self.editor.scroll_by(-(delta as f32 / 120.0) * 3.0 * line, view);
        self.invalidate();
    }

    // ---------- Global hotkey ----------

    fn hotkey_text(&self) -> String {
        sys::format_hotkey(self.settings.hotkey_modifiers, self.settings.hotkey_key)
    }

    unsafe fn start_recording(&mut self) {
        self.recording = true;
        // Free the current combination so it can be pressed again while recording.
        sys::unregister_hotkey(self.hwnd);
        self.hint = format!("Aktuell: {}\nEsc bricht ab", self.hotkey_text());
        self.invalidate();
    }

    /// Cancels recording and restores the previous shortcut.
    unsafe fn stop_recording(&mut self) {
        self.recording = false;
        self.hotkey_ok = sys::register_hotkey(self.hwnd, self.settings.hotkey_modifiers, self.settings.hotkey_key);
        self.update_tray_tip();
        self.invalidate();
    }

    unsafe fn record_hotkey(&mut self, vk: u32) {
        if sys::is_modifier_key(vk) {
            return; // wait for the actual key
        }
        let modifiers = sys::current_modifiers();
        if vk == VK_ESCAPE.0 as u32 && modifiers == 0 {
            self.stop_recording();
            return;
        }
        if modifiers == 0 || modifiers == sys::MOD_SHIFT_ {
            self.hint = "Bitte mit Strg, Alt oder Win kombinieren.\nEsc bricht ab".into();
        } else if !sys::register_hotkey(self.hwnd, modifiers, vk) {
            self.hint = format!("{} ist schon von einer anderen App belegt.\nEsc bricht ab", sys::format_hotkey(modifiers, vk));
        } else {
            self.settings.hotkey_modifiers = modifiers;
            self.settings.hotkey_key = vk;
            self.settings.save();
            self.hotkey_ok = true;
            self.recording = false;
            self.update_tray_tip();
        }
        self.invalidate();
    }

    // ---------- Tray icon ----------

    unsafe fn tray_data(&self) -> NOTIFYICONDATAW {
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: WM_TRAY,
            hIcon: self.tray_icon,
            ..Default::default()
        };
        let tip = if self.hotkey_ok { format!("Scratchpad ({})", self.hotkey_text()) } else { "Scratchpad".to_string() };
        for (slot, c) in data.szTip.iter_mut().zip(tip.encode_utf16().take(127)) {
            *slot = c;
        }
        data
    }

    unsafe fn add_tray(&self) {
        let _ = Shell_NotifyIconW(NIM_ADD, &self.tray_data());
    }

    unsafe fn update_tray_tip(&self) {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &self.tray_data());
    }

    unsafe fn remove_tray(&self) {
        let _ = Shell_NotifyIconW(NIM_DELETE, &self.tray_data());
    }

    // ---------- Menus ----------

    unsafe fn on_command(&mut self, id: usize) {
        match id {
            ID_THEME..=102 => {
                self.settings.theme = THEMES[id - ID_THEME].into();
                self.apply_theme();
            }
            ID_MATERIAL..=112 => {
                self.settings.material = MATERIALS[id - ID_MATERIAL].into();
                self.apply_backdrop();
            }
            ID_TITLE_MODE..=122 => self.settings.title_mode = MODES[id - ID_TITLE_MODE].into(),
            ID_BUTTONS_MODE..=132 => self.settings.buttons_mode = MODES[id - ID_BUTTONS_MODE].into(),
            ID_BUTTON_STYLE..=141 => self.settings.button_style = STYLES[id - ID_BUTTON_STYLE].into(),
            ID_HOTKEY => self.start_recording(),
            ID_AUTOSTART => sys::set_autostart(!sys::autostart_enabled()),
            ID_QUIT => self.cmds.push(Cmd::Exit),
            ID_OPEN => self.cmds.push(Cmd::Show),
            ID_CUT => self.cut(),
            ID_COPY => self.copy(),
            ID_PASTE => self.paste(),
            ID_SELECT_ALL => self.editor.select_all(),
            ID_MARKDOWN => {
                self.settings.markdown = !self.settings.markdown;
                self.apply_font();
            }
            ID_FIND => self.open_search(false),
            ID_REPLACE => self.open_search(true),
            ID_AUTO_HIDE => self.settings.auto_hide = !self.settings.auto_hide,
            ID_FOLDER_PICK => self.cmds.push(Cmd::PickFolder),
            ID_FOLDER_DEFAULT => self.cmds.push(Cmd::SetFolder(None)),
            ID_INFO => self.info_open = true,
            _ => return,
        }
        if (ID_THEME..ID_HOTKEY).contains(&id) || id == ID_AUTO_HIDE {
            self.settings.save();
            self.update_chrome();
        }
        self.invalidate();
    }

    // ---------- Window messages ----------

    unsafe fn on_size(&mut self, width_px: i32, height_px: i32) {
        self.dpi = GetDpiForWindow(self.hwnd) as f32;
        self.w = width_px as f32 / self.scale();
        self.h = height_px as f32 / self.scale();
        if let Some(gfx) = &mut self.gfx {
            if gfx.resize(width_px as u32, height_px as u32, self.dpi).is_err() {
                self.gfx = None;
            }
        }
        self.editor.set_width(self.text_width());
        let view = self.view_height();
        self.editor.scroll_by(0.0, view);
        self.draw(None);
    }

    unsafe fn hit_test(&mut self, lparam: LPARAM) -> LRESULT {
        let mut point = POINT { x: loword(lparam.0), y: hiword(lparam.0) };
        let _ = ScreenToClient(self.hwnd, &mut point);
        let (x, y) = (point.x as f32 / self.scale(), point.y as f32 / self.scale());
        if y < 0.0 || y >= TITLE_H {
            return LRESULT(HTCLIENT as isize);
        }
        if self.button_at(x, y).is_some() || (self.mac_style() && self.buttons_enabled() && contains(&self.rect_lights(), x, y)) {
            return LRESULT(HTCLIENT as isize);
        }
        // Left, right and bottom still have the invisible system resize frame; the top edge is ours.
        if y < RESIZE_BORDER && !IsZoomed(self.hwnd).as_bool() {
            let code = if x < RESIZE_BORDER * 2.0 { HTTOPLEFT } else if x >= self.w - RESIZE_BORDER * 2.0 { HTTOPRIGHT } else { HTTOP };
            return LRESULT(code as isize);
        }
        LRESULT(HTCAPTION as isize)
    }

    unsafe fn handle(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        let mouse = |app: &App| (loword(lparam.0) as f32 / app.scale(), hiword(lparam.0) as f32 / app.scale());
        match msg {
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                BeginPaint(self.hwnd, &mut paint); // also tells us which part is invalid
                self.draw(Some(paint.rcPaint));
                let _ = EndPaint(self.hwnd, &paint);
            }
            WM_SIZE => {
                if wparam.0 as u32 != SIZE_MINIMIZED {
                    self.on_size(loword(lparam.0).max(1), hiword(lparam.0).max(1));
                }
            }
            WM_DPICHANGED => self.cmds.push(Cmd::SetRect(*(lparam.0 as *const RECT))),
            WM_GETMINMAXINFO => {
                let info = lparam.0 as *mut MINMAXINFO;
                (*info).ptMinTrackSize = POINT { x: (MIN_W * self.scale()) as i32, y: (MIN_H * self.scale()) as i32 };
            }
            WM_NCHITTEST => {
                let system = DefWindowProcW(self.hwnd, msg, wparam, lparam);
                return Some(if system.0 == HTCLIENT as isize { self.hit_test(lparam) } else { system });
            }
            WM_NCMOUSEMOVE => {
                // The title bar is non-client area; ask Windows to tell us when the pointer leaves it.
                self.track_leave(true);
                self.update_hover();
                return None;
            }
            WM_NCMOUSELEAVE | WM_MOUSELEAVE => self.update_hover(),
            WM_NCLBUTTONDOWN => {
                // Starting to drag or resize the window dismisses menu and info card.
                self.close_menu();
                if self.info_open {
                    self.info_open = false;
                    self.invalidate();
                }
                return None;
            }
            WM_MOUSEMOVE => {
                let (x, y) = mouse(self);
                self.on_mouse_move(x, y);
            }
            WM_LBUTTONDOWN => {
                let (x, y) = mouse(self);
                self.on_mouse_down(x, y);
            }
            WM_LBUTTONUP => {
                let (x, y) = mouse(self);
                self.on_mouse_up(x, y);
            }
            WM_RBUTTONUP => {
                let (_, y) = mouse(self);
                if !self.recording && y >= VIEW_TOP && y < self.view_bottom() {
                    self.cmds.push(Cmd::EditMenu);
                }
            }
            WM_MOUSEWHEEL => self.on_wheel(hiword(wparam.0 as isize)),
            WM_SETCURSOR => {
                if loword(lparam.0) as u32 != HTCLIENT {
                    return None;
                }
                let mut point = POINT::default();
                let _ = GetCursorPos(&mut point);
                let _ = ScreenToClient(self.hwnd, &mut point);
                let (x, y) = (point.x as f32 / self.scale(), point.y as f32 / self.scale());
                let over_text = !self.recording && y >= VIEW_TOP && y < self.view_bottom() && x < self.w - 14.0;
                let over_link = over_text && sys::key_down(VK_CONTROL) && self.link_under(x, y).is_some();
                let over_checkbox = over_text && self.settings.markdown && {
                    let scroll = self.editor.scroll;
                    let hit = self.editor.char_at(x - TEXT_X, y - self.text_top() + scroll);
                    hit.and_then(|index| self.editor.checkbox_at(index)).is_some()
                };
                let shape = if over_link || over_checkbox { IDC_HAND } else if over_text { IDC_IBEAM } else { IDC_ARROW };
                if let Ok(cursor) = LoadCursorW(None, shape) {
                    SetCursor(cursor);
                }
                return Some(LRESULT(1));
            }
            WM_KEYDOWN => {
                if !self.on_key(wparam.0 as u32) {
                    return None;
                }
            }
            WM_SYSKEYDOWN => {
                if self.recording {
                    self.record_hotkey(wparam.0 as u32);
                } else if wparam.0 as u32 == VK_F4.0 as u32 {
                    // No system menu any more, so Alt+F4 is handled here: hide like the close button.
                    self.cmds.push(Cmd::Hide);
                } else {
                    return None;
                }
            }
            WM_SYSCHAR => {} // Alt+letter would only beep
            WM_CHAR => self.on_char(wparam.0 as u16),
            WM_SETFOCUS => {
                self.focused = true;
                self.reset_blink();
                self.invalidate();
            }
            WM_KILLFOCUS => {
                self.focused = false;
                let _ = KillTimer(self.hwnd, T_BLINK);
                self.invalidate();
            }
            WM_ACTIVATE => {
                self.active = loword(wparam.0 as isize) != 0;
                if !self.active && self.recording {
                    self.stop_recording();
                }
                if !self.active {
                    self.close_menu();
                }
                if self.active {
                    self.reload_if_changed();
                } else if self.settings.auto_hide && !self.settings.topmost && !self.dialog_open && !self.menu_open
                    && IsWindowVisible(self.hwnd).as_bool() && !IsIconic(self.hwnd).as_bool()
                {
                    // Another window took over: put the scratchpad away (unless it is pinned).
                    self.cmds.push(Cmd::Hide);
                }
                self.invalidate();
                return None;
            }
            WM_TIMER => match wparam.0 {
                T_SAVE => {
                    let _ = KillTimer(self.hwnd, T_SAVE);
                    self.update_stats();
                    self.save_note(true);
                }
                T_BLINK => {
                    // Only the caret changes: repaint just that sliver, not the whole window.
                    self.caret_on = !self.caret_on;
                    let area = self.caret_area();
                    let _ = InvalidateRect(self.hwnd, Some(&area), false);
                }
                T_ANIM => self.tick_animation(),
                T_SAVED_LABEL => {
                    let _ = KillTimer(self.hwnd, T_SAVED_LABEL);
                    if !self.saved_label.starts_with("Speichern fehlgeschlagen") {
                        self.saved_label.clear();
                    }
                    self.invalidate();
                }
                _ => {}
            },
            WM_HOTKEY => {
                let in_front = IsWindowVisible(self.hwnd).as_bool() && !IsIconic(self.hwnd).as_bool() && GetForegroundWindow() == self.hwnd;
                self.cmds.push(if in_front { Cmd::Hide } else { Cmd::Show });
            }
            WM_TRAY => match loword(lparam.0) as u32 {
                WM_LBUTTONUP => self.cmds.push(Cmd::Show),
                WM_RBUTTONUP | WM_CONTEXTMENU => self.cmds.push(Cmd::TrayMenu),
                _ => {}
            },
            WM_SHOW_REQUEST => self.cmds.push(Cmd::Show),
            WM_LATE_INIT => self.add_tray(),
            // Closing (traffic light, taskbar) only hides; the app keeps running for the hotkey.
            WM_CLOSE => self.cmds.push(if self.exiting { Cmd::Exit } else { Cmd::Hide }),
            WM_QUERYENDSESSION => {
                // Windows is logging off or shutting down: save everything now.
                self.persist();
                return Some(LRESULT(1));
            }
            WM_SETTINGCHANGE | WM_DWMCOLORIZATIONCOLORCHANGED => {
                self.apply_theme();
                return None;
            }
            _ if msg == self.taskbar_created => self.add_tray(), // Explorer restarted
            _ => return None,
        }
        Some(LRESULT(0))
    }
}

/// Runs queued commands with no borrow held, because each of them makes Windows
/// send further messages to this window.
unsafe fn run_commands(hwnd: HWND, cell: &RefCell<App>) {
    loop {
        let commands = match cell.try_borrow_mut() {
            Ok(mut app) => std::mem::take(&mut app.cmds),
            Err(_) => return, // an outer handler is still running; it will run them
        };
        if commands.is_empty() {
            return;
        }
        for command in commands {
            match command {
                Cmd::Show => {
                    let _ = ShowWindow(hwnd, if IsIconic(hwnd).as_bool() { SW_RESTORE } else { SW_SHOW });
                    let _ = SetForegroundWindow(hwnd);
                }
                Cmd::ShowFirst => {
                    let maximized = cell.borrow().settings.maximized;
                    // Show without activating first: activation sets up keyboard input, which
                    // takes far longer than drawing. The content is on screen before that starts.
                    let _ = ShowWindow(hwnd, if maximized { SW_SHOWMAXIMIZED } else { SW_SHOWNA });
                    // Paint right away instead of waiting for the first WM_PAINT from the message loop.
                    let _ = UpdateWindow(hwnd);
                    let _ = SetForegroundWindow(hwnd);
                }
                Cmd::Hide => {
                    {
                        let mut app = cell.borrow_mut();
                        app.close_menu();
                        app.info_open = false;
                        app.persist();
                    }
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
                Cmd::Minimize => {
                    let _ = ShowWindow(hwnd, SW_MINIMIZE);
                }
                Cmd::ToggleMaximize => {
                    let _ = ShowWindow(hwnd, if IsZoomed(hwnd).as_bool() { SW_RESTORE } else { SW_MAXIMIZE });
                }
                Cmd::Topmost(on) => {
                    let _ = SetWindowPos(hwnd, if on { HWND_TOPMOST } else { HWND_NOTOPMOST }, 0, 0, 0, 0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                    let _ = InvalidateRect(hwnd, None, false);
                }
                Cmd::SetRect(r) => {
                    let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top,
                        SWP_NOZORDER | SWP_NOACTIVATE);
                    let mut client = RECT::default();
                    let _ = GetClientRect(hwnd, &mut client);
                    cell.borrow_mut().on_size(client.right.max(1), client.bottom.max(1));
                }
                Cmd::TrayMenu => cell.borrow_mut().open_menu(menu::Which::Tray),
                Cmd::EditMenu => cell.borrow_mut().open_menu(menu::Which::Edit),
                Cmd::OpenUrl(url) => {
                    ShellExecuteW(hwnd, w!("open"), PCWSTR(wide(&url).as_ptr()), None, None, SW_SHOWNORMAL);
                }
                Cmd::PickFolder => {
                    cell.borrow_mut().dialog_open = true;
                    let folder = pick_folder(hwnd);
                    let mut app = cell.borrow_mut();
                    app.dialog_open = false;
                    if let Some(folder) = folder {
                        app.cmds.push(Cmd::SetFolder(Some(folder)));
                    }
                }
                Cmd::SetFolder(folder) => {
                    let target = settings::note_path_in(&folder.clone().unwrap_or_else(settings::data_dir));
                    if target == cell.borrow().note_path {
                        continue;
                    }
                    // A note already lying in that folder: which one should win?
                    let mut load_existing = false;
                    if target.exists() {
                        cell.borrow_mut().dialog_open = true;
                        let answer = MessageBoxW(hwnd,
                            w!("In diesem Ordner liegt bereits eine Notiz (scratch.txt).\n\nJa: diese Notiz laden. Die aktuelle Notiz bleibt am alten Ort erhalten.\n\nNein: sie mit der aktuellen Notiz überschreiben. Von der vorhandenen wird vorher eine Sicherungskopie angelegt."),
                            w!("Scratchpad"), MB_YESNOCANCEL | MB_ICONQUESTION);
                        cell.borrow_mut().dialog_open = false;
                        if answer == IDCANCEL {
                            continue;
                        }
                        load_existing = answer == IDYES;
                    }
                    cell.borrow_mut().set_note_folder(folder, load_existing);
                }
                Cmd::Exit => {
                    {
                        let mut app = cell.borrow_mut();
                        app.exiting = true;
                        app.persist();
                        app.remove_tray();
                        sys::unregister_hotkey(hwnd);
                    }
                    let _ = DestroyWindow(hwnd);
                }
            }
        }
    }
}

/// System folder picker; None if cancelled.
unsafe fn pick_folder(hwnd: HWND) -> Option<PathBuf> {
    let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
    let options = dialog.GetOptions().ok()?;
    dialog.SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;
    let _ = dialog.SetTitle(w!("Ordner für die Notiz wählen"));
    dialog.Show(hwnd).ok()?; // fails when the user cancels
    let path = dialog.GetResult().ok()?.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
    let folder = path.to_string().ok().map(PathBuf::from);
    CoTaskMemFree(Some(path.0 as *const c_void));
    folder
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_NCCALCSIZE if wparam.0 != 0 => {
            // Keep the invisible system resize frame left, right and bottom, but remove the
            // title bar: the client area starts at the very top of the window.
            let params = lparam.0 as *mut NCCALCSIZE_PARAMS;
            let top = (*params).rgrc[0].top;
            DefWindowProcW(hwnd, msg, wparam, lparam);
            let dpi = GetDpiForWindow(hwnd);
            // A maximized window overhangs the screen by its frame; compensate at the top too.
            let overhang = if IsZoomed(hwnd).as_bool() {
                GetSystemMetricsForDpi(SM_CYSIZEFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi)
            } else {
                0
            };
            (*params).rgrc[0].top = top + overhang;
            return LRESULT(0);
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_DESTROY => {
            PostQuitMessage(0);
            return LRESULT(0);
        }
        _ => {}
    }

    let cell = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<App>;
    if cell.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let cell = &*cell;
    // A message that arrives while a handler is running (Windows calling back into us)
    // gets default handling instead of a second mutable borrow.
    let result = match cell.try_borrow_mut() {
        Ok(mut app) => app.handle(msg, wparam, lparam),
        Err(_) => None,
    };
    run_commands(hwnd, cell);
    result.unwrap_or_else(|| DefWindowProcW(hwnd, msg, wparam, lparam))
}

fn initial_rect(settings: &Settings) -> RECT {
    unsafe {
        if let Some([left, top, right, bottom]) = settings.win_rect {
            let saved = RECT { left, top, right, bottom };
            // Only reuse the saved place if that monitor still exists.
            if right - left >= 100 && bottom - top >= 100 && !MonitorFromRect(&saved, MONITOR_DEFAULTTONULL).is_invalid() {
                return saved;
            }
        }
        let scale = GetDpiForSystem() as f32 / 96.0;
        let (width, height) = ((560.0 * scale) as i32, (680.0 * scale) as i32);
        let left = (GetSystemMetrics(SM_CXSCREEN) - width) / 2;
        let top = (GetSystemMetrics(SM_CYSCREEN) - height) / 2;
        RECT { left, top, right: left + width, bottom: top + height }
    }
}

unsafe fn run() -> Result<()> {
    let background = std::env::args().any(|arg| arg == sys::BACKGROUND_ARG);

    // Only one instance, so two windows never overwrite the same note.
    // A test copy (SCRATCHPAD_TEST set, with its own data folder) may run beside the real one.
    let (mutex_name, event_name) = if std::env::var_os("SCRATCHPAD_TEST").is_some() {
        (w!("Local\\Scratchpad.Test.SingleInstance"), w!("Local\\Scratchpad.Test.Show"))
    } else {
        (w!("Local\\Scratchpad.SingleInstance"), w!("Local\\Scratchpad.Show"))
    };
    let _mutex = CreateMutexW(None, true, mutex_name);
    let already_running = GetLastError() == ERROR_ALREADY_EXISTS;
    let show_request = CreateEventW(None, false, false, event_name)?;
    if already_running {
        // Already running (possibly hidden): ask that instance to show its window.
        if !background {
            let _ = AllowSetForegroundWindow(ASFW_ANY);
            let _ = SetEvent(show_request);
        }
        return Ok(());
    }

    // Prepare Direct2D in parallel; it is the slowest single step and independent of the window.
    let warmup = std::thread::spawn(|| gfx::Prepared::new(None).ok());

    let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    let instance: HINSTANCE = GetModuleHandleW(None)?.into();
    let class_name = w!("ScratchpadWindow");
    let class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: instance,
        hCursor: LoadCursorW(None, IDC_ARROW)?,
        hIcon: sys::load_icon(GetSystemMetrics(SM_CXICON)),
        hIconSm: sys::load_icon(GetSystemMetrics(SM_CXSMICON)),
        lpszClassName: class_name,
        ..Default::default()
    };
    RegisterClassExW(&class);
    // Popup window of the "⋯" menu (see menu.rs); CS_DROPSHADOW is not needed, it draws its own.
    let menu_class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(menu::menu_wndproc),
        hInstance: instance,
        hCursor: LoadCursorW(None, IDC_ARROW)?,
        lpszClassName: MENU_CLASS,
        ..Default::default()
    };
    RegisterClassExW(&menu_class);

    let settings = Settings::load();
    let r = initial_rect(&settings);
    // No WS_SYSMENU: the title bar buttons are drawn by the app itself.
    let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), class_name, w!("Scratchpad"),
        WS_THICKFRAME | WS_CAPTION | WS_MINIMIZEBOX | WS_MAXIMIZEBOX,
        r.left, r.top, r.right - r.left, r.bottom - r.top, None, None, instance, None)?;

    // Extending the frame over the whole window makes DWM honour our per-pixel alpha, so the
    // backdrop shows through. Then apply the frame from WM_NCCALCSIZE.
    let _ = DwmExtendFrameIntoClientArea(hwnd, &MARGINS { cxLeftWidth: -1, cxRightWidth: -1, cyTopHeight: -1, cyBottomHeight: -1 });
    let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);

    // The app state lives for the whole process.
    let cell: &'static RefCell<App> = Box::leak(Box::new(RefCell::new(App::new(hwnd, settings, warmup))));
    cell.borrow_mut().cell_ptr = cell as *const RefCell<App> as isize;
    SetWindowLongPtrW(hwnd, GWLP_USERDATA, cell as *const RefCell<App> as isize);

    // A second start signals this event; forward it to the window as a message.
    let (event, window) = (show_request.0 as usize, hwnd.0 as usize);
    std::thread::spawn(move || loop {
        WaitForSingleObject(HANDLE(event as *mut c_void), INFINITE);
        let _ = PostMessageW(HWND(window as *mut c_void), WM_SHOW_REQUEST, WPARAM(0), LPARAM(0));
    });

    if !background {
        cell.borrow_mut().cmds.push(Cmd::ShowFirst);
    }
    run_commands(hwnd, cell);

    let mut message = MSG::default();
    while GetMessageW(&mut message, None, 0, 0).as_bool() {
        let _ = TranslateMessage(&message);
        DispatchMessageW(&message);
    }
    Ok(())
}

fn main() {
    unsafe {
        let _ = run();
    }
}
