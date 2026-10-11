# Scratchpad (Rust)

A small note window for Windows 11 with a translucent backdrop (Acrylic, Mica or Liquid), written
directly against the Windows API (no .NET, no runtime). The interface is in German or English.

- a single note that saves itself; opened with a global hotkey (default `Alt` + `X`)
- starts in about 0.1 s, uses about 9 MB of memory, one `.exe` of about 500 KB
- light Markdown: headings, **bold**, `code`, tickable checklists, clickable links;
  the syntax characters hide while the caret is elsewhere
- search and replace, font and line spacing, title bar with round buttons on the left or symbols on the right

## Building

Needs Rust with the GNU toolchain (`stable-x86_64-pc-windows-gnu`); no Visual Studio Build Tools.

```
cargo build --release
copy target\release\Scratchpad.exe dist\Scratchpad.exe
```

Quit the running app first with `Ctrl` + `Q`, otherwise the `.exe` cannot be overwritten.

## Layout

| File | Contents |
|---|---|
| `src/main.rs` | Window, title bar, search, tray icon, hotkeys, running in the background |
| `src/menu.rs` | The "⋯" menu (the app's own, self-drawn popup window) |
| `src/editor.rs` | Text field: caret, selection, undo, wrapping, scrolling (on DirectWrite) |
| `src/gfx.rs` | Drawing surface: Direct2D into a memory buffer with transparency |
| `src/settings.rs` | Settings and the note file |
| `src/lang.rs` | The two interface languages |
| `src/sys.rs` | Registry (colour scheme, accent colour, autostart), icon, dark menus |
| `build.rs` | Embeds `app.ico` and `app.manifest` into the `.exe` (without a resource compiler) |

## Data

`%APPDATA%\Scratchpad\scratch.txt` (the note) and `settings.json` (the settings).

The note's folder can be changed in the menu under "Note folder", for example to a synced folder.
If another computer changes the file, it is reloaded when the window is activated; if that collides
with local changes, the other version is kept as `scratch (backup …).txt`.

On the first save of a day the app copies the previous state to `Backups\scratch YYYY-MM-DD.txt`
next to the note and keeps the last 10 of these copies.

## Testing

`cargo test` checks the editor logic. For tests on the window, set the environment variable
`SCRATCHPAD_TEST` and point `APPDATA` at an empty folder: a test copy then runs beside the real app.

## Versions

Every shipped version has a Git tag (`v1.7p`, `v1.8p`, …). `git log --oneline` lists them,
`git checkout v1.7p` brings back an old state.

The **p** stands for **performance**. It is a promise rather than a decoration: no version ships
before typing, caret movement and drawing have been timed again, and a feature that would slow
the app down noticeably does not get in. At the time of writing a keystroke takes about 0.03 ms
and a redraw under 1 ms, so the app is roughly a few hundred times faster than you can type.

## License

MIT, see [LICENSE](LICENSE).
