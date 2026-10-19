# App icon

A hub with a keyhole, wired to four servers: three running, one switched off. Drawn for Klefki in its own palette (Catppuccin mauve, pink, green and blue on the Mocha tile).

- `klefki.svg`: the icon, for 32 px and up.
- `klefki-small.svg`: a bolder version for 16 and 24 px, where the spokes and thin outline would blur.
- `klefki-128.png`: the window and taskbar icon, baked into the binary (`crates/app/src/main.rs`).
- `klefki-256.png`: the README header.
- `klefki.ico` (16 to 256 px) and `klefki.icns` (16 to 1024 px): for a Windows executable resource and a macOS app bundle. The build does not embed them yet; macOS shows a window icon only from a bundle.

Edit the SVGs, then run `python3 scripts/export-icon.py` to regenerate the rest.
