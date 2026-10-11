# Scratchpad (Rust)

Kleines Notizfenster für Windows 11 mit Acryl-/Mica-Hintergrund, direkt gegen die Windows-API
geschrieben (kein .NET, keine Laufzeitumgebung). Nachfolger der C#-Version in `..\Scratchpad`.

## Bauen

Benötigt Rust mit der GNU-Toolchain (`stable-x86_64-pc-windows-gnu`), keine Visual Studio Build Tools.

```
cargo build --release
copy target\release\Scratchpad.exe dist\Scratchpad.exe
```

Die laufende App vorher mit `Strg` + `Q` beenden, sonst lässt sich die `.exe` nicht überschreiben.

## Aufbau

| Datei | Inhalt |
|---|---|
| `src/main.rs` | Fenster, Titelleiste, Suche, Tray-Symbol, Tastenkürzel, Hintergrundbetrieb |
| `src/menu.rs` | Das „⋯“-Menü im Mac-Stil (eigenes, selbst gezeichnetes Popup-Fenster) |
| `src/editor.rs` | Textfeld: Cursor, Markieren, Rückgängig, Umbruch, Scrollen (auf DirectWrite) |
| `src/gfx.rs` | Zeichenfläche: Direct2D in einen Speicherpuffer mit Transparenz |
| `src/settings.rs` | Einstellungen und Notizdatei |
| `src/sys.rs` | Registry (Farbschema, Akzentfarbe, Autostart), Icon, dunkle Menüs |
| `build.rs` | Bettet `app.ico` und `app.manifest` in die `.exe` ein (ohne Ressourcen-Compiler) |

## Daten

`%APPDATA%\Scratchpad\scratch.txt` (Notiz) und `settings.json` (Einstellungen), dieselben Dateien
wie bei der C#-Version. Beide Versionen können nicht gleichzeitig laufen.

Der Ordner der Notiz lässt sich im Menü unter „Speicherort“ ändern, z. B. auf einen synchronisierten
Ordner. Ändert ein anderer Rechner die Datei, wird sie beim Aktivieren des Fensters neu geladen;
kollidiert das mit eigenen Änderungen, bleibt die fremde Fassung als `scratch (Sicherung …).txt` erhalten.

Beim ersten Speichern eines Tages legt die App den Stand davor als `Sicherungen\scratch JJJJ-MM-TT.txt`
neben die Notiz und behält die letzten 10 dieser Kopien.

## Testen

`cargo test` prüft die Editor-Logik. Für Tests am Fenster die Umgebungsvariable `SCRATCHPAD_TEST`
setzen und `APPDATA` auf einen leeren Ordner zeigen lassen: Dann läuft eine Testkopie neben der echten App.

## Versionen

Der Code liegt in Git; jede ausgelieferte Version hat eine Marke (`v1.7p`, `v1.8p`, …).
Mit `git log --oneline` sieht man die Liste, mit `git checkout v1.7p` holt man einen alten Stand zurück.
