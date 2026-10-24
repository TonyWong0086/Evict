# Evict

[![Latest release](https://img.shields.io/github/v/release/TonyWong0086/Evict?style=for-the-badge&label=Latest%20release)](https://github.com/TonyWong0086/Evict/releases/latest)
[![License](https://img.shields.io/github/license/TonyWong0086/Evict?style=for-the-badge&color=grey)](LICENSE)

A Windows uninstaller that also removes the files and registry keys a program leaves behind.

Single exe, no installer, no telemetry, free.

## Usage

> [!Warning]
> Evict deletes files and registry keys. You review everything before it goes, but use it at your own risk. Report problems in [Issues](https://github.com/TonyWong0086/Evict/issues).

1. Download `evict.exe` from the [latest release](https://github.com/TonyWong0086/Evict/releases/latest).
2. Run it and accept the administrator prompt.
3. Right-click a program and choose **Uninstall**.

The exe isn't code-signed, so Windows may show "Windows protected your PC". Click **More info**, then **Run anyway**.

<details>
  <summary>Build from source</summary><br/>

  Needs Rust and the MSVC toolchain.

  ```
  cargo build --release
  ```

  Output: `target\release\evict.exe`. Debug builds don't ask for admin.

  ```
  cargo test
  cargo test -- --ignored --nocapture
  ```

  The second command is a read-only check against the programs on your machine.
</details>

## Features

#### Uninstall

- Runs the program's uninstaller and waits until it and its child processes exit.
- Then scans for leftover files, folders and registry keys.
- Force removal skips a broken or missing uninstaller and goes straight to the scan.
- Select several programs to uninstall them in a row. If you cancel one uninstaller, only that program is skipped.

#### Leftovers

- Folders and keys named after the program in Program Files, AppData, ProgramData and the registry.
- Shortcuts, Run entries, App Paths and services that still point at the old folder.
- Files another installed program still uses are kept and never offered for removal.

#### Program list

- Sizes are measured on disk, including data folders. See [Sizes](#sizes).
- Shows icon, version, publisher, size, install date and folder. Sort by any column.
- Finds entries whose program is already gone and removes them.
- Export to CSV or JSON.

#### Other tabs

- **Store apps:** remove Store apps. Common preinstalled ones are marked so you can select them in one click.
- **Startup:** turn sign-in programs on or off, with the same High / Medium / Low impact rating Windows uses.
- **Install monitor:** record what an installer adds, so uninstalling later removes exactly that.
- **History:** every removal with its date, item count and space freed.

## Keyboard

| Key | Action |
|---|---|
| Delete | Uninstall |
| Shift+Delete | Force removal |
| Up / Down | Move selection, Shift to extend |
| Ctrl+A | Select all |
| Ctrl+F or start typing | Search |
| Esc | Clear search |
| F5 | Refresh |

Window size, column widths, sort order and the last tab are saved between runs.

## Safety

- Nothing is deleted until you confirm the list. You can expand any folder or key first.
- Drive roots, `Windows`, your user folder and the Program Files / AppData / ProgramData roots are never removed.
- A folder only counts as a leftover if its name matches the program's exactly, ignoring version numbers and suffixes like "x64".
- If a program still looks installed after its uninstaller, its leftovers aren't scanned.
- Same-named folders found elsewhere on a drive count toward the size but are never offered for deletion.

## Sizes

<details>
  <summary>How sizes are measured</summary><br/>

  A program's size is the sum of:

  1. Its install folder. If the registry doesn't say where that is, Evict checks game library files, the program's subfolder inside a shared vendor folder, or the folder of its exe.
  2. Folders named after it in AppData, ProgramData and the top two levels of each fixed drive.

  Programs installed inside another program's folder are subtracted from it. If nothing can be measured, the size the installer reported is used.
</details>

## Source layout

<details>
  <summary>Files in <code>src/</code></summary><br/>

  | File | Contents |
  |---|---|
  | `main.rs` | Window, tabs, program table, right-click menu |
  | `programs.rs` | Installed programs, folders, sizes, icons, leftovers |
  | `cleanup.rs` | Uninstall progress and leftovers dialogs |
  | `startup.rs` | Startup entries and impact |
  | `monitor.rs` | Install monitor |
  | `history.rs` | Uninstall history |
  | `settings.rs` | Saved layout |
  | `theme.rs` | Fonts and spacing |
  | `win.rs` | Win32 calls |
</details>

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). For security issues, see [SECURITY.md](SECURITY.md).

## License

[GPL-3.0-or-later](LICENSE). If you distribute a modified version, you have to publish its source under the same license.
