# Contributing

## Bug reports

[Open an issue](https://github.com/TonyWong0086/Evict/issues/new) and include:

- What you did and what happened.
- The program involved, with its version.
- Your Windows version (run `winver`).
- A screenshot if it's a display problem. Crop out anything private.

If Evict deleted something it shouldn't have, put that in the title.

## Feature requests

Open an issue and describe the problem you want solved.

## Pull requests

For anything larger than a small fix, open an issue first.

1. Fork the repo and branch from `main`.
2. Make your change in the same style as the surrounding code.
3. Run:

   ```
   cargo build
   cargo test
   cargo clippy
   ```

4. Open the pull request with a description of the change and how you tested it.

Rules for changes:

- Anything that deletes files or keys has to go through the leftovers dialog.
- Test removal on programs you don't need.
- No installer, background service, telemetry or network requests.
- Ask in an issue before adding a dependency.
- No emoji in UI text.

## License

Contributions are released under [GPL-3.0-or-later](LICENSE), the same as the rest of the project.
