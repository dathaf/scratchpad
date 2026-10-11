//! Settings and note file. Settings always live in %APPDATA%\Scratchpad (shared with the
//! C# version); the note can be moved to another folder, e.g. one that is synced.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub const DEFAULT_FONT_SIZE: f32 = 14.0;
const NOTE_FILE: &str = "scratch.txt";

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase", default)]
pub struct Settings {
    pub theme: String,        // Auto | Light | Dark
    pub material: String,     // Acrylic | Mica
    pub font_size: f32,
    pub topmost: bool,
    pub maximized: bool,
    pub hotkey_modifiers: u32,
    pub hotkey_key: u32,
    pub title_mode: String,   // Always | Hover | Never
    pub buttons_mode: String, // Always | Hover | Never
    pub button_style: String, // Dots | Symbols
    /// Normal (restored) window rectangle in screen pixels: left, top, right, bottom.
    pub win_rect: Option<[i32; 4]>,
    /// Hide the window as soon as another window is activated (unless pinned).
    pub auto_hide: bool,
    /// Opacity of the background tint in percent: 0 = backdrop fully visible, 100 = solid.
    pub tint: u32,
    /// Folder holding the note; None = %APPDATA%\Scratchpad.
    pub note_folder: Option<String>,
    /// Font family of the note; None = the default monospace font.
    pub font: Option<String>,
    /// Line height as a multiple of the font size; 0 = the font's own line height.
    pub line_spacing: f32,
    /// Style headings, checklists, bold and code written in Markdown.
    pub markdown: bool,
    pub language: String, // German | English
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "Auto".into(),
            material: "Acrylic".into(),
            font_size: DEFAULT_FONT_SIZE,
            topmost: false,
            maximized: false,
            hotkey_modifiers: 0x1, // MOD_ALT
            hotkey_key: 0x58,      // X
            title_mode: "Always".into(),
            buttons_mode: "Always".into(),
            button_style: "Dots".into(),
            win_rect: None,
            auto_hide: false,
            tint: 0,
            note_folder: None,
            font: None,
            line_spacing: 0.0,
            markdown: true,
            language: if crate::sys::system_is_german() { "German" } else { "English" }.into(),
        }
    }
}

pub fn data_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("Scratchpad")
}

fn settings_path() -> PathBuf {
    data_dir().join("settings.json")
}

impl Settings {
    pub fn load() -> Self {
        let _ = std::fs::create_dir_all(data_dir());
        std::fs::read_to_string(settings_path())
            .ok()
            // The C# version writes a UTF-8 BOM.
            .and_then(|s| serde_json::from_str(s.trim_start_matches('\u{feff}')).ok())
            .unwrap_or_default()
    }

    /// Settings are best effort; a failed write must never disturb editing.
    pub fn save(&self) {
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(settings_path(), json);
        }
    }

    pub fn note_dir(&self) -> PathBuf {
        self.note_folder.as_ref().map(PathBuf::from).unwrap_or_else(data_dir)
    }

    pub fn note_path(&self) -> PathBuf {
        self.note_dir().join(NOTE_FILE)
    }
}

pub fn note_path_in(folder: &Path) -> PathBuf {
    folder.join(NOTE_FILE)
}

pub fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Writes atomically (temp file + rename) so a crash never leaves a half-written note.
pub fn save_note(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    let tmp = path.with_extension("txt.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

pub fn load_note(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes);
    text.trim_start_matches('\u{feff}').replace('\r', "")
}

/// Keeps a copy of the note file before it gets replaced by something the user may not
/// expect (overwritten in a new folder, or changed by another computer in the meantime).
pub fn backup_note(path: &Path, stamp: &str) {
    if path.exists() {
        let _ = std::fs::copy(path, path.with_file_name(format!("scratch (backup {stamp}).txt")));
    }
}

/// How many daily copies of the note are kept.
const DAILY_BACKUPS: usize = 10;

/// Keeps the note as it was before the first change of `day` (YYYY-MM-DD) in the folder
/// "Backups" next to it, one file per day, and drops all but the newest ones.
/// Best effort like the settings: a failed copy must never disturb saving.
pub fn daily_backup(path: &Path, day: &str) {
    let Some(parent) = path.parent() else { return };
    let folder = parent.join("Backups");
    // Up to 1.8 the folder had a German name.
    let old = parent.join("Sicherungen");
    if old.is_dir() && !folder.exists() {
        let _ = std::fs::rename(&old, &folder);
    }
    let copy = folder.join(format!("scratch {day}.txt"));
    if copy.exists() || std::fs::metadata(path).map_or(true, |note| note.len() == 0) {
        return;
    }
    if std::fs::create_dir_all(&folder).is_err() || std::fs::copy(path, &copy).is_err() {
        return;
    }
    let Ok(entries) = std::fs::read_dir(&folder) else { return };
    let mut copies: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|file| file.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with("scratch ") && name.ends_with(".txt")))
        .collect();
    copies.sort(); // the date in the name sorts oldest first
    for old in copies.iter().rev().skip(DAILY_BACKUPS) {
        let _ = std::fs::remove_file(old);
    }
}
