# Security

## Supported versions

Only the [latest release](https://github.com/dathaf/scratchpad/releases/latest) gets fixes.
Scratchpad is a personal project without a support promise; a fix ships as a new version.

## Reporting a vulnerability

Please do not open a public issue for a security problem. Use GitHub's private form instead:
**Security → Report a vulnerability** on the
[repository page](https://github.com/dathaf/scratchpad/security/advisories/new). Only the
maintainer sees such a report.

Helpful in a report: the Scratchpad version (menu → About), the Windows version, and the steps
that show the problem. This is a one-person project, so an answer can take a few days.

## What the app does and does not do

Knowing this helps to judge whether something is a vulnerability:

- **No network access.** Scratchpad opens no connections, has no updater and sends no data.
  A link in the note is handed to Windows, which opens it in the default browser, and only
  after `Ctrl` + click.
- **Plain files.** The note, its backups and the settings are unencrypted files in
  `%APPDATA%\Scratchpad` (or the folder chosen in the menu). Anyone who can read the user's
  files can read the note; do not keep passwords in it.
- **No elevated rights.** The app runs with the user's own rights. "Start with Windows" writes
  one entry for the current user to the registry; nothing is installed system-wide.
- **Not code-signed.** Windows therefore warns on first start. From version 1.9.3p on, the exe
  of every release is built by GitHub from the tagged source; the build log of the
  [workflow run](https://github.com/dathaf/scratchpad/actions/workflows/build.yml) prints its
  SHA-256 checksum, which can be compared with the download:

  ```
  Get-FileHash Scratchpad.exe -Algorithm SHA256
  ```

## Out of scope

- The SmartScreen warning for an unsigned exe.
- Anything that needs an attacker who already has the user's rights on the PC (reading the note
  file, replacing the exe, changing `settings.json`).
- Problems in Windows itself or in the fonts installed on the system.
