# Export: save to a file, 2026-10-19
Goal: the roadmap item after Export (#12). Export's block can be saved to a file from the dialog, and `klefki-cli export --output FILE` writes it. Davy's export rules stay: plain settings as defaults, anything secret-looking as `${VAR}`. Also: screen readers hear "Copied" (an open item of the Export slice).

## Done
- Core `export_file.rs`: `save` writes the block (plus a final line break) through `fsx::write_whole`: a temporary file beside the target, flushed, then moved into place. Without `replace` the move itself refuses an existing file (`persist_noclobber`), so there is no gap between a look and the write. A link is never followed or replaced, a folder or a credential file is refused. Claude Code's own files (`.claude.json` and its backups, anything inside its configuration folder or any `.claude` folder) and `.mcp.json` files are refused even with `replace`: they change only through a reviewed plan, and Klefki creates nothing in project scope. A missing folder is said plainly.
- Save dialog: rfd 0.17 with only `xdg-portal` (no GTK). Windows uses the common item dialog, macOS NSSavePanel, Linux the desktop portal through libdbus (loaded only when the dialog opens) or zenity. Each asks before replacing a file. Two new crates (rfd, pollster); `cargo deny` is clean. Release binary on Linux: 17,804,280 → 17,880,384 bytes (+76 KB, 0.4 %).
- Engine: `Command::ChooseExportFile { name }` runs the dialog on a thread of its own and answers `Event::ExportFileChosen(FileChoice)`: `Chosen(path)`, `Cancelled`, or `NoDialog { suggested }` when the dialog closed in under 150 ms (nobody cancels that fast; a system without a dialog answers at once). `Command::SaveExport { path, text, replace }` answers `Event::ExportSaved`. The dialog is injected in `EngineConfig::choose_file`, so tests never show one.
- App: "Save to file…" beside Copy. The first file name is `<server>-mcp.json` for one server and `mcp-servers.json` for more. A picked file is written with `replace` (the system's dialog has asked). "Saved to <path>" or "Not saved: <why>" shows under the buttons. Without a dialog, a "Save to" path field opens with the documents folder and that name. Enter saves, quotes around a pasted path are dropped, and a path that is not whole is refused with a sentence. An existing file asks "<name> is there already. Replace it?" with Replace and Keep it. An answer for an Export that was closed is ignored. When the dialog's size changes, one more frame is drawn, so it is centred before the next click.
- Screen readers: the Copy and Copy names buttons, the save status and the replace question are polite live regions (`accesskit` `Live::Polite`), so "Copied" and "Saved to …" are read out without moving focus (WCAG 2.2, 4.1.3 Status Messages).
- CLI: `export -o/--output FILE [--force]`. stdout stays empty; stderr says `Saved <name or N servers> to FILE.` and lists the variables as before. An existing file: asks at a terminal, otherwise names `--force`. Exit 4 when nothing was written. Help, README, PLAN.md and decision 006 updated.

## Checks
(filled in below)

## Open
- On Windows and macOS the dialog has no parent window, so it is not modal to Klefki's window (Klefki stays usable while it is open; Save to file… is off meanwhile).
- The 150 ms rule cannot tell a dialog that failed slowly from a cancel: such a system shows nothing, as after a cancel.

## Next
Cross-review, CI on 3 OSes, merge.
