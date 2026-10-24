# Evict

**A Windows uninstaller that removes the program *and* everything it leaves behind. Written in Rust, free, with no Pro tier.**

Evict does what Geek Uninstaller does, plus the features Geek keeps behind its paid version: a real-time installation monitor and a startup manager. It's one ~5 MB exe with no installer, no telemetry and no ads.

---

## Features

| | |
|---|---|
| **Program list** | Every installed desktop program with its icon, version, publisher, real size, install date and folder. Sort by any column, search as you type, Ctrl+click to select several. |
| **Clean uninstall** | Runs the program's own uninstaller, **waits for it and every process it spawns to finish**, then scans for leftover files, folders and registry keys. You review a File System / Registry tree and tick what goes. |
| **Force removal** | For programs whose uninstaller is broken or missing: skip it and go straight to the leftover scan. |
| **Real sizes** | Sizes are measured on disk, mods and caches included, not taken from what the installer reported. See [How sizes work](#how-sizes-work). |
| **Install monitor** | Snapshots the registry and watches the disk while an installer runs, then saves everything it added as a *trace*. Uninstalling a traced program removes exactly what was installed. *(Pro in Geek)* |
| **Startup manager** | Turn programs that run at sign-in on or off (the same switch Task Manager uses), or delete the entry. *(Pro in Geek)* |
| **Store apps** | List and remove Microsoft Store apps. |
| **Right-click menu** | Uninstall, Force Removal, Remove Entry, Modify, Installation Folder, Registry Entry, Program Website, Google for. |

## Safety

Evict deletes things, so it's built to be cautious:

- **Nothing is deleted without your review.** Every removal goes through the leftovers dialog, where you can expand each folder and key to see what's inside.
- **Shared folders are never offered.** If another program lives in or under a folder (e.g. `Program Files\Adobe`), it isn't treated as a leftover.
- **Protected locations are off-limits:** drive roots, `Windows`, your user folder, and the Program Files / AppData / ProgramData roots themselves.
- **Exact name matching only.** A folder counts as a leftover only when its name matches the program's (ignoring version numbers and suffixes like "Inc." or "x64"). No fuzzy guessing.
- **Cancelled uninstalls are detected.** If the program still looks installed afterwards, nothing is pre-ticked.
- **Drive-wide matches are for sizing only.** A same-named folder like `D:\Blender` might be your own projects, so Evict counts it in the size but never offers to delete it.

## How sizes work

For each program, Evict adds up:

1. **The install folder.** It comes from the registry or, when that's missing or wrong, from Steam's library files (Steam games don't record one), the program's own subfolder inside a shared vendor folder, or the folder of its exe. It also steps out of version subfolders (`Roblox\Versions\version-…` → `Roblox`), so caches stored next to them count.
2. **Data folders named after the program** in AppData (Roaming, Local, LocalLow), ProgramData, and the top two levels of every fixed drive.

Other programs installed inside a folder (games inside Steam) are subtracted. Programs with nothing to measure fall back to the size their installer reported.

## Build

Requires Rust (stable) and the MSVC toolchain.

```
cargo build --release
```

The exe is `target\release\evict.exe`. Release builds ask for administrator rights at start, since removing programs and cleaning Program Files needs them; debug builds don't.

```
cargo test                            # unit tests
cargo test -- --ignored --nocapture   # read-only check against this machine
```

## Project layout

| File | What's in it |
|---|---|
| `src/main.rs` | Window, tabs, program table, right-click menu |
| `src/programs.rs` | Reading installed programs, install folders, sizes, leftover matching |
| `src/cleanup.rs` | Uninstall progress and leftovers dialogs |
| `src/monitor.rs` | Installation monitor and traces |
| `src/startup.rs` | Startup entries |
| `src/win.rs` | Win32 glue: icons, opening links, drives, waiting on uninstallers |

## License

Evict is free software, licensed under the [GNU General Public License v3.0 or later](LICENSE).

You can use, study, share and modify it. If you distribute a modified version, you must release its source under the same license, so Evict and anything built from it stay free.
