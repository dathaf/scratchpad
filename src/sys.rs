//! Small Windows helpers: registry (theme, accent, autostart), hotkey names, icon, dark menus.

use std::ffi::c_void;
use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::*;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Null-terminated UTF-16 for Win32 calls.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---------- Registry ----------

fn reg_dword(path: PCWSTR, name: PCWSTR) -> Option<u32> {
    let mut data = 0u32;
    let mut size = 4u32;
    let result = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, path, name, RRF_RT_REG_DWORD, None,
            Some(&mut data as *mut u32 as *mut c_void), Some(&mut size))
    };
    result.is_ok().then_some(data)
}

pub fn system_uses_dark_theme() -> bool {
    reg_dword(w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"), w!("AppsUseLightTheme")) == Some(0)
}

/// One shade of the Windows accent colour. AccentPalette holds 8 RGBA shades:
/// [light3, light2, light1, base, dark1, dark2, dark3, …].
pub fn accent_color(shade: usize) -> Option<(u8, u8, u8)> {
    let mut data = [0u8; 32];
    let mut size = data.len() as u32;
    let result = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\Accent"),
            w!("AccentPalette"), RRF_RT_REG_BINARY, None, Some(data.as_mut_ptr() as *mut c_void), Some(&mut size))
    };
    (result.is_ok() && size as usize >= 32).then(|| (data[shade * 4], data[shade * 4 + 1], data[shade * 4 + 2]))
}

// ---------- Autostart ("--background" starts hidden) ----------

const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: PCWSTR = w!("Scratchpad");
pub const BACKGROUND_ARG: &str = "--background";

fn autostart_command() -> String {
    let exe = std::env::current_exe().unwrap_or_default();
    format!("\"{}\" {}", exe.display(), BACKGROUND_ARG)
}

pub fn autostart_enabled() -> bool {
    let mut buffer = [0u16; 1024];
    let mut size = (buffer.len() * 2) as u32;
    let result = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, RRF_RT_REG_SZ, None,
            Some(buffer.as_mut_ptr() as *mut c_void), Some(&mut size))
    };
    if result.is_err() {
        return false;
    }
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..len]).eq_ignore_ascii_case(&autostart_command())
}

pub fn set_autostart(enabled: bool) {
    unsafe {
        if enabled {
            let command = wide(&autostart_command());
            let _ = RegSetKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, REG_SZ.0,
                Some(command.as_ptr() as *const c_void), (command.len() * 2) as u32);
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE);
        }
    }
}

// ---------- Hotkey ----------

pub const MOD_ALT_: u32 = 0x1;
pub const MOD_CONTROL_: u32 = 0x2;
pub const MOD_SHIFT_: u32 = 0x4;
pub const MOD_WIN_: u32 = 0x8;
const MOD_NOREPEAT_: u32 = 0x4000;
pub const HOTKEY_ID: i32 = 1;

/// false if another application already owns this combination.
pub fn register_hotkey(hwnd: HWND, modifiers: u32, vk: u32) -> bool {
    unsafe { RegisterHotKey(hwnd, HOTKEY_ID, HOT_KEY_MODIFIERS(modifiers | MOD_NOREPEAT_), vk).is_ok() }
}

pub fn unregister_hotkey(hwnd: HWND) {
    unsafe {
        let _ = UnregisterHotKey(hwnd, HOTKEY_ID);
    }
}

pub fn key_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetKeyState(vk.0 as i32) < 0 }
}

pub fn current_modifiers() -> u32 {
    let mut mods = 0;
    if key_down(VK_MENU) { mods |= MOD_ALT_; }
    if key_down(VK_CONTROL) { mods |= MOD_CONTROL_; }
    if key_down(VK_SHIFT) { mods |= MOD_SHIFT_; }
    if key_down(VK_LWIN) || key_down(VK_RWIN) { mods |= MOD_WIN_; }
    mods
}

pub fn is_modifier_key(vk: u32) -> bool {
    matches!(vk, 0x10 | 0x11 | 0x12 | 0x5B | 0x5C | 0xA0..=0xA5)
}

pub fn format_hotkey(modifiers: u32, vk: u32) -> String {
    let mut parts: Vec<String> = Vec::new();
    if modifiers & MOD_CONTROL_ != 0 { parts.push("Strg".into()); }
    if modifiers & MOD_ALT_ != 0 { parts.push("Alt".into()); }
    if modifiers & MOD_SHIFT_ != 0 { parts.push("Umschalt".into()); }
    if modifiers & MOD_WIN_ != 0 { parts.push("Win".into()); }
    parts.push(key_name(vk));
    parts.join(" + ")
}

fn key_name(vk: u32) -> String {
    match vk {
        0x30..=0x5A => char::from_u32(vk).unwrap_or('?').to_string(), // 0–9, A–Z
        0x70..=0x87 => format!("F{}", vk - 0x6F),
        0x20 => "Leertaste".into(),
        _ => unsafe {
            // Everything else: ask Windows for the localized key name.
            let scan = MapVirtualKeyW(vk, MAPVK_VK_TO_VSC);
            let mut buffer = [0u16; 64];
            let len = GetKeyNameTextW((scan << 16) as i32, &mut buffer);
            if len > 0 { String::from_utf16_lossy(&buffer[..len as usize]) } else { format!("Taste {vk:02X}") }
        },
    }
}

// ---------- Icon ----------

/// The app icon in the given pixel size, from the icon resource build.rs embeds in the .exe (id 1).
pub fn load_icon(size: i32) -> HICON {
    unsafe {
        let Ok(module) = GetModuleHandleW(None) else { return HICON::default() };
        LoadImageW(module, PCWSTR(1 as *const u16), IMAGE_ICON, size, size, LR_DEFAULTCOLOR)
            .map(|handle| HICON(handle.0))
            .unwrap_or_default()
    }
}

// ---------- Dark popup menus ----------

/// Win32 popup menus only follow dark mode through two unexported uxtheme functions
/// (ordinals 135 and 136); the same calls Explorer and Notepad use.
/// mode: 1 = follow system, 2 = force dark, 3 = force light.
pub fn set_menu_theme(mode: i32) {
    unsafe {
        let Ok(uxtheme) = LoadLibraryW(w!("uxtheme.dll")) else { return };
        if let Some(set_preferred_app_mode) = GetProcAddress(uxtheme, PCSTR(135 as *const u8)) {
            let set_preferred_app_mode: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(set_preferred_app_mode);
            set_preferred_app_mode(mode);
        }
        if let Some(flush_menu_themes) = GetProcAddress(uxtheme, PCSTR(136 as *const u8)) {
            let flush_menu_themes: unsafe extern "system" fn() = std::mem::transmute(flush_menu_themes);
            flush_menu_themes();
        }
    }
}

// ---------- Plain blur behind the window ("Liquid" material) ----------

#[repr(C)]
struct AccentPolicy {
    state: i32,
    flags: i32,
    gradient_color: u32,
    animation_id: i32,
}

#[repr(C)]
struct CompositionAttributeData {
    attribute: i32,
    data: *mut c_void,
    size: usize,
}

/// Turns the compositor's blur-behind on or off for a window. Unlike the Acryl backdrop it
/// adds no tint and no noise, which is what makes the glass look clear.
/// Uses user32's unexported-in-headers SetWindowCompositionAttribute (the call the taskbar
/// and Start menu use); if it is missing the window simply stays without blur.
pub fn set_blur_behind(hwnd: HWND, enabled: bool) {
    const WCA_ACCENT_POLICY: i32 = 19;
    const ACCENT_DISABLED: i32 = 0;
    const ACCENT_ENABLE_BLURBEHIND: i32 = 3;
    unsafe {
        let Ok(user32) = GetModuleHandleW(w!("user32.dll")) else { return };
        let Some(function) = GetProcAddress(user32, s!("SetWindowCompositionAttribute")) else { return };
        let set_attribute: unsafe extern "system" fn(HWND, *mut CompositionAttributeData) -> i32 = std::mem::transmute(function);
        let mut policy = AccentPolicy {
            state: if enabled { ACCENT_ENABLE_BLURBEHIND } else { ACCENT_DISABLED },
            flags: 0,
            gradient_color: 0,
            animation_id: 0,
        };
        let mut data = CompositionAttributeData {
            attribute: WCA_ACCENT_POLICY,
            data: &mut policy as *mut AccentPolicy as *mut c_void,
            size: std::mem::size_of::<AccentPolicy>(),
        };
        set_attribute(hwnd, &mut data);
    }
}

// ---------- Windows version ----------

#[repr(C)]
struct OsVersionInfo {
    size: u32,
    major: u32,
    minor: u32,
    build: u32,
    platform: u32,
    csd: [u16; 128],
}

#[link(name = "ntdll")]
extern "system" {
    fn RtlGetVersion(info: *mut OsVersionInfo) -> i32;
}

/// Real build number (GetVersionEx lies to applications without a manifest).
pub fn windows_build() -> u32 {
    let mut info = OsVersionInfo { size: std::mem::size_of::<OsVersionInfo>() as u32, major: 0, minor: 0, build: 0, platform: 0, csd: [0; 128] };
    unsafe { RtlGetVersion(&mut info) };
    info.build
}
