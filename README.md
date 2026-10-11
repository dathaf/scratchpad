# Scratchpad (Rust)

Kleines Notizfenster für Windows 11 mit durchscheinendem Hintergrund (Acryl, Mica oder Liquid),
direkt gegen die Windows-API geschrieben (kein .NET, keine Laufzeitumgebung).

- eine einzige Notiz, die sich selbst speichert; Aufruf per Tastenkürzel (Standard `Alt` + `X`)
- Start in etwa 0,1 s, rund 9 MB Arbeitsspeicher, eine `.exe` von etwa 500 KB
- leichtes Markdown: Überschriften, **fett**, `Code`, abhakbare Listen, anklickbare Links;
  die Zeichen dafür verschwinden, solange der Cursor woanders steht
- Suchen und Ersetzen, Schriftart und Zeilenabstand, Titelleiste mit runden Knöpfen links oder Symbolen rechts

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
| `src/menu.rs` | Das „⋯“-Menü (eigenes, selbst gezeichnetes Popup-Fenster) |
| `src/editor.rs` | Textfeld: Cursor, Markieren, Rückgängig, Umbruch, Scrollen (auf DirectWrite) |
| `src/gfx.rs` | Zeichenfläche: Direct2D in einen Speicherpuffer mit Transparenz |
| `src/settings.rs` | Einstellungen und Notizdatei |
| `src/sys.rs` | Registry (Farbschema, Akzentfarbe, Autostart), Icon, dunkle Menüs |
| `build.rs` | Bettet `app.ico` und `app.manifest` in die `.exe` ein (ohne Ressourcen-Compiler) |

## Daten

`%APPDATA%\Scratchpad\scratch.txt` (Notiz) und `settings.json` (Einstellungen).

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

## Lizenz

MIT, siehe [LICENSE](LICENSE).
