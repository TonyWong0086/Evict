# Evict

A Windows uninstaller written in Rust, in the spirit of Geek Uninstaller, with everything free.

- **Programs**: every installed desktop program with its icon, version, publisher, real size on disk, install date and folder. Sort by any column; right-click for Uninstall, Force Removal, Remove Entry, Modify, Installation Folder, Registry Entry, Program Website and Google for. Ctrl+click to select several.
- **Leftover cleanup**: after an uninstall (or instead of one, with Force Removal), Evict lists the folders and registry keys left behind. Nothing is deleted until you review and tick it.
- **Real sizes**: measures what's actually on disk, mods and caches included, rather than the size the installer reported. Install folders come from the registry, Steam's own library files, or the program's exe; app data from same-named folders in AppData, ProgramData and the top levels of every fixed drive.
- **Store apps**: list and remove Microsoft Store apps.
- **Startup**: turn programs that run at sign-in on or off (the same switch Task Manager uses), or delete them.
- **Install monitor**: snapshots the registry and watches the disk while an installer runs, then saves what it added as a trace. Uninstalling a traced program removes exactly what was installed.

## Build

```
cargo build --release
```

The release build asks for administrator rights at start (removing programs and cleaning Program Files needs them); debug builds don't.

`cargo test` runs the unit tests; `cargo test -- --ignored --nocapture` runs a read-only check against the current machine.
