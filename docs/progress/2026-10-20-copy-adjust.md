# Duplicate: change the base address, callback port and client of the copy (2026-10-20)
Goal (Davy): in the Duplicate dialog, change the base address (the start the server address and the OAuth metadata
address share), the OAuth callback port and the client id before the copy is made. Overlapping servers often differ
only there.

## Done
- Core `adjust.rs`: `Adjust { base: Rebase { from, to }, callback_port, client: Client { id, secret } }`,
  `shared_base(url, metadata)` (common start up to a `/`, never less than the host; another host: the address alone),
  and `CopyFields`, the dialog's fields built from the definition `Event::Entry` sends, with the Add form's checks.
- `Change::Duplicate` takes `adjust`. A plain server: `Op::CopyServer { base }`, rebased when the docstore expands the
  copy (still one write, secrets never leave the docstore). One that signs in: `Swap::adjust`, applied in review,
  preview and run; a new client hands Claude Code the new client's secret (or none), never the original's; Klefki
  files it under the copy. A new callback port gets the same "port in use" warning as an add.
- App: the Duplicate dialog reads the definition (`Command::Inspect`) and shows Base address (or Address), with the
  addresses the copy gets under it; Client ID, a Client secret field once the client id differs, Callback port (checked
  when left). Problems are said once a field is left. On a short window the body scrolls; the "already exists" line
  and the buttons stay in view.
- Decision 007 has a paragraph on it.

## Checks
- New tests, seen failing with `Adjust::apply` made a no-op (5 engine tests): adjust (7), engine (6: plain copy with a
  new base keeps header values; refusals before anything changes; OAuth copy with new base and port keeps the
  secret; new client hands only its own secret; client without secret gets none; a held port is warned about), app (4).
- Model test: new step `CopyAdjusted` (new address, maybe another client with or without a secret). A wrong
  prediction fails it at once; `PROPTEST_CASES=4000` passed (217 s).
- Full verify: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace
  && cargo deny check` exit 0 (374 + 21 + 26 + 832 passed).
- Live, Claude Code 2.1.304, temp `HOME`/`CLAUDE_CONFIG_DIR`, gnome-keyring under Xvfb: sso (query string, client
  acme-1, port 8123, metadata on the same host) duplicated with base `eu.acme.com/v2` (https added when left), client
  acme-2 with a new secret, port 9001. `.claude.json` got exactly the rewritten definition (query kept), the original
  unchanged; Claude Code's credentials hold the new secret under the copy's key and the original's under its own;
  the Auth tab shows Klefki's copy. `add-json` also takes `"oauth": {}`. Screenshots:
  `/mnt/project-files/reviews/2026-10-20-copy-adjust/` (04-06 are from before errors waited for leaving a field).

## Open
- The summary line ("Duplicate sso as sso_eu for the user") does not name the changes; the diff under it shows them.
- Not changeable here: scopes and headers (the edit form does those after the copy).

## Next
Cross-review, then merge.
