# App icon

A hub with a keyhole, wired to four servers: three running, one switched off. Drawn for Klefki in its own palette (Catppuccin mauve, pink, green and blue on the Mocha tile).

- `klefki.svg`: the icon, for 30 px and up.
- `klefki-small.svg`: a bolder version for 16 to 24 px, where the spokes and thin outline would blur.
- `klefki-256.png`: baked into the binary (`crates/app/src/main.rs`) as the window, taskbar and macOS Dock icon, and shown in the README. The app can hand the window only one image, so Windows scales this one down for the title bar; the small art reaches only the `.ico` and `.icns`.
- `klefki.ico` (16 to 256 px, every size Windows asks for at 100 to 250 % scaling) and `klefki.icns` (16 to 1024 px, @1x and @2x): for the Windows executable (embedded by `crates/app/build.rs`, so Explorer and the Start menu show it) and the macOS app bundle and installers (`[package.metadata.packager]` in `crates/app/Cargo.toml`).

Edit the SVGs, then run `python3 scripts/export-icon.py` to regenerate the rest.
