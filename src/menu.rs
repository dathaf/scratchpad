//! The app's own menus: a small layered popup window the app draws itself
//! (rounded card, soft shadow or glass, accent-coloured highlight). Three menus use it:
//! the settings menu (gear button), the right-click menu in the text and the notification-area menu.
//!
//! The first two never take the keyboard focus (the main window stays active and forwards
//! keys); the notification-area menu has no active main window to rely on, so it is
//! activated itself and closes when it loses the focus.
//!
//! The drawing surface only exists while a menu is open.

use crate::*;

const WIDTH: f32 = 284.0;
const ROW_H: f32 = 26.0;
const SEPARATOR_H: f32 = 11.0;
const PADDING: f32 = 5.0;
/// Transparent margin around the card that holds its shadow.
const SHADOW: f32 = 20.0;
/// Horizontal extent of a slider's track inside its row, measured from the row's edges.
const SLIDER_LEFT: f32 = 112.0;
const SLIDER_RIGHT: f32 = 56.0;
/// Width of the "more" button at the right end of a row that has one.
const MORE_W: f32 = 28.0;

#[derive(Clone, Copy, PartialEq)]
pub enum Which {
    Main,
    /// Right click in the text.
    Edit,
    /// Right click on the notification-area icon.
    Tray,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// Runs a command and closes the menu.
    Action,
    /// Steps through a setting's values; the menu stays open.
    Cycle,
    /// On/off setting with a checkmark; the menu stays open.
    Check,
    /// 0–100 value changed by dragging; the menu stays open.
    Slider,
    Separator,
}

struct MenuItem {
    kind: Kind,
    label: &'static str,
    /// Shown right-aligned: the current value or a keyboard shortcut.
    value: String,
    id: usize,
    checked: bool,
    enabled: bool,
    /// Slider position, 0–100.
    level: u32,
    /// Command of a small "more" button at the row's right end (0 = none); it closes the menu.
    more: usize,
}

pub struct Menu {
    which: Which,
    items: Vec<MenuItem>,
    hover: Option<usize>,
    gfx: Gfx,
    /// Top-left corner of the popup window on screen, in pixels.
    origin: POINT,
    /// Transparent margin around the card: room for the drawn shadow, or 0 in glass mode,
    /// where Windows rounds the popup's corners and adds the shadow itself.
    margin: f32,
    /// DPI of the monitor the menu is on (the notification area can be on another monitor than the window).
    dpi: f32,
    /// A slider is being dragged.
    dragging: bool,
    /// The pointer is on the hovered row's "more" button.
    on_more: bool,
}

fn item(kind: Kind, label: &'static str, value: impl Into<String>, id: usize) -> MenuItem {
    MenuItem { kind, label, value: value.into(), id, checked: false, enabled: true, level: 0, more: 0 }
}

fn separator() -> MenuItem {
    item(Kind::Separator, "", "", 0)
}

fn position(values: &[&str], current: &str) -> usize {
    values.iter().position(|v| *v == current).unwrap_or(0)
}

fn menu_height(items: &[MenuItem]) -> f32 {
    PADDING * 2.0 + items.iter().map(|i| if i.kind == Kind::Separator { SEPARATOR_H } else { ROW_H }).sum::<f32>()
}

impl App {
    fn menu_items(&self, which: Which) -> Vec<MenuItem> {
        let s = &self.settings;
        let check = |label, id, checked| MenuItem { checked, ..item(Kind::Check, label, "", id) };
        match which {
            Which::Edit => {
                let selection = self.editor.has_selection();
                vec![
                    MenuItem { enabled: selection, ..item(Kind::Action, tr("Ausschneiden", "Cut"), tr("Strg + X", "Ctrl + X"), ID_CUT) },
                    MenuItem { enabled: selection, ..item(Kind::Action, tr("Kopieren", "Copy"), tr("Strg + C", "Ctrl + C"), ID_COPY) },
                    item(Kind::Action, tr("Einfügen", "Paste"), tr("Strg + V", "Ctrl + V"), ID_PASTE),
                    separator(),
                    item(Kind::Action, tr("Alles auswählen", "Select all"), tr("Strg + A", "Ctrl + A"), ID_SELECT_ALL),
                    item(Kind::Action, if selection { tr("Auswahl duplizieren", "Duplicate selection") } else { tr("Zeile duplizieren", "Duplicate line") }, tr("Strg + D", "Ctrl + D"), ID_DUPLICATE),
                ]
            }
            Which::Tray => vec![
                item(Kind::Action, tr("Scratchpad öffnen", "Open Scratchpad"), self.hotkey_text(), ID_OPEN),
                check(tr("Mit Windows starten", "Start with Windows"), ID_AUTOSTART, sys::autostart_enabled()),
                separator(),
                item(Kind::Action, tr("Beenden", "Quit"), "", ID_QUIT),
            ],
            Which::Main => {
                let mode = |m: &str| [tr("Immer", "Always"), tr("Bei Maus", "On hover"), tr("Nie", "Never")][position(&MODES, m)];
                let folder = match &s.note_folder {
                    Some(folder) => PathBuf::from(folder).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| folder.clone()),
                    None => tr("Standard", "Default").to_string(),
                };
                let spacing = ["Normal", tr("Mittel", "Medium"), tr("Weit", "Wide")][SPACINGS.iter().position(|v| *v >= s.line_spacing).unwrap_or(0)];
                let mut items = vec![
                    item(Kind::Cycle, tr("Darstellung", "Appearance"), [tr("Automatisch", "Automatic"), tr("Hell", "Light"), tr("Dunkel", "Dark")][position(&THEMES, &s.theme)], ID_CYCLE_THEME),
                    item(Kind::Cycle, "Material", [tr("Acryl", "Acrylic"), "Mica", "Liquid"][position(&MATERIALS, &s.material)], ID_CYCLE_MATERIAL),
                    MenuItem { level: s.tint.min(100), ..item(Kind::Slider, tr("Deckkraft", "Opacity"), format!("{} %", s.tint.min(100)), ID_TINT_SLIDER) },
                    separator(),
                    item(Kind::Cycle, tr("Titel", "Title"), mode(&s.title_mode), ID_CYCLE_TITLE),
                    item(Kind::Cycle, tr("Fensterknöpfe", "Window buttons"), mode(&s.buttons_mode), ID_CYCLE_BUTTONS),
                    item(Kind::Cycle, tr("Knopf-Stil", "Button style"), [tr("Punkte", "Dots"), tr("Symbole", "Symbols")][position(&STYLES, &s.button_style)], ID_CYCLE_STYLE),
                    separator(),
                    MenuItem { more: ID_FONT_PICK, ..item(Kind::Cycle, tr("Schrift", "Font"), self.font_family(), ID_CYCLE_FONT) },
                    item(Kind::Cycle, tr("Zeilenabstand", "Line spacing"), spacing, ID_CYCLE_SPACING),
                    check(tr("Markdown-Formatierung", "Markdown styling"), ID_MARKDOWN, s.markdown),
                    check(tr("Klammern automatisch schließen", "Close brackets automatically"), ID_AUTO_PAIRS, s.auto_pairs),
                    separator(),
                    item(Kind::Action, tr("Suchen …", "Find …"), tr("Strg + F", "Ctrl + F"), ID_FIND),
                    item(Kind::Action, tr("Ersetzen …", "Replace …"), tr("Strg + H", "Ctrl + H"), ID_REPLACE),
                    separator(),
                    item(Kind::Action, tr("Speicherort …", "Note folder …"), folder, ID_FOLDER_PICK),
                ];
                if s.note_folder.is_some() {
                    items.push(item(Kind::Action, tr("Standardordner verwenden", "Use default folder"), "", ID_FOLDER_DEFAULT));
                }
                let hotkey = format!("{}{}", self.hotkey_text(), if self.hotkey_ok { "" } else { tr(" (belegt)", " (in use)") });
                items.extend([
                    item(Kind::Action, tr("Tastenkürzel ändern …", "Change hotkey …"), hotkey, ID_HOTKEY),
                    check(tr("Automatisch ausblenden", "Hide automatically"), ID_AUTO_HIDE, s.auto_hide),
                    check(tr("Mit Windows starten", "Start with Windows"), ID_AUTOSTART, sys::autostart_enabled()),
                    separator(),
                    item(Kind::Cycle, tr("Sprache", "Language"), ["Deutsch", "English"][position(&LANGUAGES, &s.language)], ID_CYCLE_LANGUAGE),
                    separator(),
                    item(Kind::Action, tr("Info", "About"), "", ID_INFO),
                    item(Kind::Action, tr("Scratchpad beenden", "Quit Scratchpad"), tr("Strg + Q", "Ctrl + Q"), ID_QUIT),
                ]);
                items
            }
        }
    }

    pub unsafe fn toggle_menu(&mut self) {
        if self.menu.is_some() { self.close_menu() } else { self.open_menu(Which::Main) }
    }

    pub unsafe fn open_menu(&mut self, which: Which) {
        self.close_menu();
        // Reuse the window's Direct2D factory; in the background, before the window was ever drawn, make one.
        let prepared = match self.gfx.as_ref() {
            Some(g) => gfx::Prepared::new(Some(&g.factory)),
            None => gfx::Prepared::new(None),
        };
        let Ok(prepared) = prepared else { return };
        if self.menu_hwnd.is_invalid() {
            let created = CreateWindowExW(WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST, MENU_CLASS, None, WS_POPUP,
                0, 0, 0, 0, self.hwnd, None, GetModuleHandleW(None).unwrap_or_default(), None);
            let Ok(hwnd) = created else { return };
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, self.cell_ptr);
            self.menu_hwnd = hwnd;
        }
        // Menus of the visible window must not steal its focus (typing and Esc keep working there);
        // the notification-area menu needs the focus itself.
        let takes_focus = which == Which::Tray;
        let style = WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | if takes_focus { WINDOW_EX_STYLE(0) } else { WS_EX_NOACTIVATE };
        SetWindowLongPtrW(self.menu_hwnd, GWL_EXSTYLE, style.0 as isize);

        // Where: under the menu button (aligned with its outer edge), or at the mouse pointer.
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        let button = self.rect_menu_button();
        let mut anchor = match which {
            Which::Main => {
                let x = if self.dots_style() { button.right } else { button.left + WIDTH };
                let mut point = POINT { x: (x * self.scale()).round() as i32, y: ((button.bottom + 4.0) * self.scale()).round() as i32 };
                let _ = ClientToScreen(self.hwnd, &mut point);
                point
            }
            _ => cursor,
        };
        let monitor_handle = MonitorFromPoint(anchor, MONITOR_DEFAULTTONEAREST);
        let mut monitor = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(monitor_handle, &mut monitor);
        let (mut dpi_x, mut dpi_y) = (0u32, 0u32);
        let dpi = match GetDpiForMonitor(monitor_handle, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) {
            Ok(()) if dpi_x > 0 && which == Which::Tray => dpi_x as f32,
            _ => self.dpi,
        };

        // Glass: the popup is exactly the card, blurred behind and rounded by Windows.
        // Otherwise it is larger than the card and draws its own shadow into the margin.
        let glass = self.glass();
        let margin = if glass { 0.0 } else { SHADOW };
        sys::set_blur_behind(self.menu_hwnd, glass);
        let corners: i32 = if glass { DWMWCP_ROUND.0 } else { DWMWCP_DONOTROUND.0 };
        let _ = DwmSetWindowAttribute(self.menu_hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &corners as *const i32 as *const c_void, 4);

        let items = self.menu_items(which);
        let px = |dip: f32| (dip * dpi / 96.0).round() as i32;
        let (width, height, shadow) = (px(WIDTH + margin * 2.0), px(menu_height(&items) + margin * 2.0), px(margin));
        match which {
            Which::Main => {}
            Which::Edit => anchor.x += width - shadow * 2, // card's left edge at the pointer
            Which::Tray => anchor.y -= height - shadow * 2, // card's bottom-right corner at the pointer
        }
        let work = monitor.rcWork;
        let x = (anchor.x + shadow - width).min(work.right + shadow - width).max(work.left - shadow);
        let y = (anchor.y - shadow).min(work.bottom + shadow - height).max(work.top - shadow);

        let Ok(gfx) = Gfx::new(prepared, self.menu_hwnd, width as u32, height as u32, dpi) else { return };
        self.menu = Some(Menu { which, items, hover: None, gfx, origin: POINT { x, y }, margin, dpi, dragging: false, on_more: false });
        self.menu_open = which == Which::Main; // keeps the title bar visible while its menu is open
        self.draw_menu();
        if takes_focus {
            let _ = ShowWindow(self.menu_hwnd, SW_SHOW);
            let _ = SetForegroundWindow(self.menu_hwnd);
        } else {
            let _ = ShowWindow(self.menu_hwnd, SW_SHOWNOACTIVATE);
        }
        self.update_chrome();
    }

    pub unsafe fn close_menu(&mut self) {
        let Some(menu) = self.menu.take() else { return };
        if menu.dragging {
            let _ = ReleaseCapture();
            self.settings.save();
        }
        self.menu_open = false;
        let _ = ShowWindow(self.menu_hwnd, SW_HIDE);
        self.update_hover();
        self.update_chrome();
    }

    unsafe fn draw_menu(&self) {
        let Some(menu) = &self.menu else { return };
        let (g, p) = (&menu.gfx, self.pal);
        let height = menu_height(&menu.items);
        g.dc.BeginDraw();
        g.dc.Clear(Some(&rgba(0, 0, 0, 0)));

        let margin = menu.margin;
        let card = rect(margin, margin, WIDTH, height);
        if margin == 0.0 {
            // Glass: a translucent pane over the blurred background, with sheen and a lit rim.
            g.dc.Clear(Some(&fade(p.card_bg, if p.dark { 0.62 } else { 0.58 })));
            g.fill_round_gradient(rect(0.0, 0.0, WIDTH, height.min(110.0)), 0.0, self.white(0.08, 0.50), self.white(0.0, 0.0));
            g.stroke_round_gradient(rect(0.75, 0.75, WIDTH - 1.5, height - 1.5), 7.5, self.white(0.40, 0.95), self.white(0.07, 0.32), 1.0);
        } else {
            // Soft shadow: a few stacked, growing, nearly transparent rounded rectangles.
            for layer in 1..=9 {
                let spread = layer as f32 * 1.8;
                g.fill_round(rect(margin - spread, margin - spread + 5.0, WIDTH + spread * 2.0, height + spread * 2.0), 10.0 + spread, rgba(0, 0, 0, 8));
            }
            g.fill_round(card, 10.0, Color { a: 1.0, ..p.card_bg });
            g.stroke_round(card, 10.0, p.card_border, 1.0);
        }

        let mut y = card.top + PADDING;
        for (index, item) in menu.items.iter().enumerate() {
            if item.kind == Kind::Separator {
                g.fill_rect(rect(card.left + 12.0, y + 5.0, WIDTH - 24.0, 1.0), p.card_border);
                y += SEPARATOR_H;
                continue;
            }
            let row = rect(card.left + 5.0, y, WIDTH - 10.0, ROW_H);
            // A slider is operated by dragging, so it gets no "click me" highlight.
            let hot = menu.hover == Some(index) && item.enabled && item.kind != Kind::Slider;
            // On the "more" button only the button lights up, so it reads as a target of its own.
            let more = rect(row.right - MORE_W, y + 2.0, MORE_W - 2.0, ROW_H - 4.0);
            let on_more = hot && menu.on_more && item.more != 0;
            let hot = hot && !on_more;
            if hot {
                g.fill_round(row, 5.0, p.highlight);
            } else if on_more {
                g.fill_round(more, 6.0, p.hover); // like the title bar's own buttons
            }
            let (text, secondary) = if hot {
                (rgb(255, 255, 255), gray(0xD0, 0xFF))
            } else if item.enabled {
                (p.fg, p.muted)
            } else {
                (p.placeholder, p.placeholder)
            };
            if item.checked {
                g.text("\u{E73E}", &self.fonts.icon_small, rect(row.left + 4.0, y, 18.0, ROW_H), text, DWRITE_TEXT_ALIGNMENT_CENTER);
            }
            g.text(item.label, &self.fonts.search, rect(row.left + 24.0, y, WIDTH - 44.0, ROW_H), text, DWRITE_TEXT_ALIGNMENT_LEADING);
            let reserved = if item.more != 0 { MORE_W - 6.0 } else { 0.0 };
            if !item.value.is_empty() {
                g.text(&item.value, &self.fonts.search, rect(row.left + 24.0, y, WIDTH - 46.0 - reserved, ROW_H), secondary, DWRITE_TEXT_ALIGNMENT_TRAILING);
            }
            if item.more != 0 {
                let color = if on_more { p.fg } else if hot { text } else { p.muted };
                g.text("\u{E712}", &self.fonts.icon_small, more, color, DWRITE_TEXT_ALIGNMENT_CENTER);
            }
            if item.kind == Kind::Slider {
                let (left, right, middle) = (row.left + SLIDER_LEFT, row.right - SLIDER_RIGHT, y + ROW_H / 2.0);
                let knob = left + (right - left) * item.level as f32 / 100.0;
                g.fill_round(rect(left, middle - 2.0, right - left, 4.0), 2.0, p.thumb);
                g.fill_round(rect(left, middle - 2.0, knob - left, 4.0), 2.0, p.highlight);
                g.fill_circle(knob, middle, 7.0, gray(0x40, 0x00));
                g.fill_circle(knob, middle, 6.2, rgb(255, 255, 255));
            }
            y += ROW_H;
        }
        if g.dc.EndDraw(None, None).is_ok() {
            let _ = g.present_layered(menu.origin.x, menu.origin.y);
        }
    }

    /// Row under a point given in the popup's own DIPs.
    fn menu_row_at(&self, x: f32, y: f32) -> Option<usize> {
        let menu = self.menu.as_ref()?;
        if x < menu.margin || x >= menu.margin + WIDTH {
            return None;
        }
        let mut top = menu.margin + PADDING;
        for (index, item) in menu.items.iter().enumerate() {
            let height = if item.kind == Kind::Separator { SEPARATOR_H } else { ROW_H };
            if y >= top && y < top + height {
                return (item.kind != Kind::Separator && item.enabled).then_some(index);
            }
            top += height;
        }
        None
    }

    unsafe fn set_menu_hover(&mut self, hover: Option<usize>, on_more: bool) {
        if let Some(menu) = &mut self.menu {
            if menu.hover != hover || menu.on_more != on_more {
                menu.hover = hover;
                menu.on_more = on_more;
                self.draw_menu();
            }
        }
    }

    /// Rebuilds the rows after a setting changed (same rows, new values) and redraws.
    unsafe fn refresh_menu(&mut self) {
        let Some(which) = self.menu.as_ref().map(|m| m.which) else { return };
        let items = self.menu_items(which);
        if let Some(menu) = &mut self.menu {
            menu.items = items;
        }
        self.draw_menu();
    }

    unsafe fn activate_menu_item(&mut self, index: usize, backwards: bool) {
        let Some((kind, id)) = self.menu.as_ref().and_then(|m| m.items.get(index)).map(|i| (i.kind, i.id)) else { return };
        match kind {
            Kind::Separator | Kind::Slider => {}
            Kind::Action => {
                self.close_menu();
                self.on_command(id);
            }
            Kind::Cycle | Kind::Check => {
                if kind == Kind::Cycle { self.cycle_setting(id, backwards) } else { self.on_command(id) }
                self.refresh_menu();
            }
        }
    }

    unsafe fn cycle_setting(&mut self, id: usize, backwards: bool) {
        let step = |count: usize, current: usize| if backwards { (current + count - 1) % count } else { (current + 1) % count };
        let s = &self.settings;
        let (first, count, current) = match id {
            ID_CYCLE_THEME => (ID_THEME, THEMES.len(), position(&THEMES, &s.theme)),
            ID_CYCLE_MATERIAL => (ID_MATERIAL, MATERIALS.len(), position(&MATERIALS, &s.material)),
            ID_CYCLE_TITLE => (ID_TITLE_MODE, MODES.len(), position(&MODES, &s.title_mode)),
            ID_CYCLE_BUTTONS => (ID_BUTTONS_MODE, MODES.len(), position(&MODES, &s.buttons_mode)),
            ID_CYCLE_STYLE => (ID_BUTTON_STYLE, STYLES.len(), position(&STYLES, &s.button_style)),
            ID_CYCLE_LANGUAGE => (ID_LANGUAGE, LANGUAGES.len(), position(&LANGUAGES, &s.language)),
            ID_CYCLE_FONT => {
                let choices = &self.fonts.choices;
                if !choices.is_empty() {
                    let family = self.font_family();
                    let current = choices.iter().position(|c| *c == family).unwrap_or(0);
                    self.settings.font = Some(choices[step(choices.len(), current)].clone());
                    self.apply_font();
                }
                return;
            }
            ID_CYCLE_SPACING => {
                let current = SPACINGS.iter().position(|v| *v >= s.line_spacing).unwrap_or(0);
                self.settings.line_spacing = SPACINGS[step(SPACINGS.len(), current)];
                self.apply_font();
                return;
            }
            _ => return,
        };
        self.on_command(first + step(count, current));
    }

    /// Sets the slider in row `index` from a pointer x position (popup DIPs) or by a keyboard step.
    unsafe fn set_slider(&mut self, index: usize, x: Option<f32>, step: i32) {
        let Some(menu) = &self.menu else { return };
        let Some(item) = menu.items.get(index).filter(|i| i.kind == Kind::Slider) else { return };
        let level = match x {
            Some(x) => {
                let (left, right) = (menu.margin + 5.0 + SLIDER_LEFT, menu.margin + WIDTH - 5.0 - SLIDER_RIGHT);
                ((x - left) / (right - left) * 100.0).round().clamp(0.0, 100.0) as u32
            }
            None => (item.level as i32 + step).clamp(0, 100) as u32,
        };
        if item.id == ID_TINT_SLIDER && level != self.settings.tint {
            self.settings.tint = level;
            self.invalidate();
            self.refresh_menu();
        }
    }

    /// Keys while a menu is open (forwarded by the main window, or its own when it has the focus).
    pub unsafe fn on_menu_key(&mut self, vk: u32) {
        let Some(menu) = &self.menu else { return };
        let hover = menu.hover;
        let rows: Vec<(Kind, bool)> = menu.items.iter().map(|i| (i.kind, i.enabled)).collect();
        let count = rows.len();
        // Next selectable row, wrapping around and skipping separators and disabled rows.
        let step = |forward: bool| {
            let mut index = hover.unwrap_or(if forward { count - 1 } else { 0 });
            for _ in 0..count {
                index = if forward { (index + 1) % count } else { (index + count - 1) % count };
                if rows[index].0 != Kind::Separator && rows[index].1 {
                    return Some(index);
                }
            }
            None
        };
        let kind = hover.map(|i| rows[i].0);
        let more = hover.and_then(|i| menu.items.get(i)).map_or(0, |i| i.more);
        match VIRTUAL_KEY(vk as u16) {
            VK_ESCAPE => self.close_menu(),
            VK_DOWN => self.set_menu_hover(step(true), false),
            VK_UP => self.set_menu_hover(step(false), false),
            // The keyboard's way to a row's "more" button; Space and the arrows step the value.
            VK_RETURN if more != 0 => {
                self.close_menu();
                self.on_command(more);
            }
            VK_LEFT | VK_RIGHT if kind == Some(Kind::Slider) => {
                self.set_slider(hover.unwrap(), None, if vk == VK_RIGHT.0 as u32 { 5 } else { -5 });
                self.settings.save();
            }
            VK_RETURN | VK_SPACE | VK_RIGHT => {
                if let Some(index) = hover {
                    self.activate_menu_item(index, false);
                }
            }
            VK_LEFT if kind == Some(Kind::Cycle) => self.activate_menu_item(hover.unwrap(), true),
            _ => {}
        }
    }

    /// Messages of the popup window itself. Returns true if handled.
    pub unsafe fn on_menu_message(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> bool {
        let Some(menu) = &self.menu else { return false };
        let scale = menu.dpi / 96.0;
        let (x, y) = (loword(lparam.0) as f32 / scale, hiword(lparam.0) as f32 / scale);
        let (dragging, hover, takes_focus) = (menu.dragging, menu.hover, menu.which == Which::Tray);
        let row = self.menu_row_at(x, y);
        let more = row.and_then(|i| menu.items.get(i)).map_or(0, |i| i.more);
        let on_more = more != 0 && x >= menu.margin + WIDTH - 5.0 - MORE_W;
        let is_slider = |app: &App, index: Option<usize>| {
            index.and_then(|i| app.menu.as_ref()?.items.get(i)).is_some_and(|i| i.kind == Kind::Slider)
        };
        match msg {
            WM_MOUSEMOVE if dragging => self.set_slider(hover.unwrap_or(0), Some(x), 0),
            WM_MOUSEMOVE => {
                let mut track = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: self.menu_hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut track);
                self.set_menu_hover(row, on_more);
            }
            WM_MOUSELEAVE if !dragging => self.set_menu_hover(None, false),
            WM_LBUTTONDOWN if is_slider(self, row) => {
                SetCapture(self.menu_hwnd);
                if let Some(menu) = &mut self.menu {
                    menu.dragging = true;
                    menu.hover = row;
                }
                self.set_slider(row.unwrap(), Some(x), 0);
            }
            WM_LBUTTONUP if dragging => {
                if let Some(menu) = &mut self.menu {
                    menu.dragging = false;
                }
                let _ = ReleaseCapture();
                self.settings.save();
            }
            WM_LBUTTONUP if on_more => {
                self.close_menu();
                self.on_command(more);
            }
            WM_LBUTTONUP => {
                if let Some(index) = row {
                    self.activate_menu_item(index, false);
                }
            }
            // Right click steps a setting backwards.
            WM_RBUTTONUP => {
                if let Some(index) = row {
                    self.activate_menu_item(index, true);
                }
            }
            WM_KEYDOWN => self.on_menu_key(wparam.0 as u32),
            // The notification-area menu owns the focus; clicking anywhere else closes it.
            WM_ACTIVATE if takes_focus && loword(wparam.0 as isize) == 0 => self.close_menu(),
            _ => return false,
        }
        true
    }
}

pub unsafe extern "system" fn menu_wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_MOUSEACTIVATE && (GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_NOACTIVATE.0) != 0 {
        return LRESULT(MA_NOACTIVATE as isize); // clicking the menu must not steal the window's focus
    }
    let cell = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<App>;
    if cell.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let cell = &*cell;
    let (handled, main_window) = match cell.try_borrow_mut() {
        Ok(mut app) => (app.on_menu_message(msg, wparam, lparam), app.hwnd),
        Err(_) => (false, HWND::default()),
    };
    if !main_window.is_invalid() {
        run_commands(main_window, cell);
    }
    if handled { LRESULT(0) } else { DefWindowProcW(hwnd, msg, wparam, lparam) }
}
