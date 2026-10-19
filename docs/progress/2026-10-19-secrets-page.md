# Secrets page (2026-10-19)
Goal: the Secrets page from the left menu, proposed by the Export thread: which servers use secrets and where each
one lives, so they can be managed. Scope chosen to fit PLAN.md: the M3 "orphaned-secrets view" (secrets.rs: orphans
are only listed, deleting one is an explicit user action) plus a read-only map of every secret a definition uses.

- **Scope.** Per server, each secret: env and header values, secret-looking arguments, address sign-in and query
  parameters, a command line or `headersHelper` with a credential in it, other secret-looking settings, and the OAuth
  client secret. Where it lives: typed into `.claude.json` / `.mcp.json` (with the `${VAR}` the edit form would
  suggest), from environment variables, made by a command (`headersHelper`), or in the system's secret store (stored
  or not). Unused client secrets in the store (no server with OAuth and no kept definition uses them) are listed
  with Forget, which asks first. Open goes to the server on Connections at the tab where the secret is edited.
  Not in this slice: moving a typed-in secret out for the user (a write: needs its own plan/review), checking
  whether an environment variable is set (Klefki's environment is not Claude Code's).
- **No value leaves core.** `secret_uses.rs` sends names and places only; detection reuses `config/secretlike.rs`
  (the edit form's hint, masked arguments, Export's redaction), so the page agrees with the rest of Klefki.
- **Store listing** (`SecretStore::list`): one search of Klefki's own entries (service `dev.klefki`; on Windows a
  name pattern, as Credential Manager names entries `<user>.<service>` and splits at the last dot when listing).
  No secret is read, so no per-item keychain prompt. Unit-tested against keyring-core's mock store; the three real
  stores are not exercised by tests (tests never touch the real keychain). **Manual check wanted on Windows/macOS:**
  open Secrets with a stored client secret and see "In the Windows Credential Manager" / "In the macOS Keychain".
- **Cost.** Nothing at start-up: read when the page is opened (each file once, the store listed on a thread), let
  go when it is left; read again on a snapshot or an applied change while shown.
- **Test changed, for human review:** `a_page_that_is_not_built_says_what_it_will_do_and_leads_back` now uses
  Scope drift, the one page still not built (it used Secrets).
- **Tests:** core `secret_uses` (7: env, headers, defaults, args/address/helper, OAuth, sanitised names, store
  merge, `~`), `secrets` (list via the mock store, Windows split), engine (page report without any value, Forget
  only while unused, store problem only when a server needs it, a kept definition's secret not offered to forget;
  the last fails with the kept list left out). App (9 egui_kittest: read on open only, what/where shown, Open lands
  at Env/Auth, Forget asks first, re-read on change, store problem with Try again, empty, filter, sample data).
- **Checks** (Linux): `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test
  --workspace && cargo deny check` exit 0 (668 core, 323 app, 18 + 25 cli); `cargo clippy -p klefki -- -D warnings`
  exit 0. Ran the app under Xvfb on a sample config; screenshots at 1200 and 800×480 in the project's reviews folder.
- **PR #34.** Cross-review (another thread) at 485acb9: one blocking point, eight non-blocking, folded in once:
  - **B1** Forget could delete a secret another Claude Code configuration (`CLAUDE_CONFIG_DIR`) still uses: one
    store, ids not per configuration. Fixed with the reviewer's smallest option: the report names the file it
    checked (`Secrets::config`), and the section and the confirmation say no server *there* uses it and that a
    server in another configuration may. The safer option (a per-configuration record of the secrets Klefki
    stored) is left as a follow-up; it would hide secrets stored before the record existed.
  - **N1/N2** A file change reads the definitions again (`Command::SecretsAfterFileChange`) and reuses the store's
    last listing: listing can ask to unlock the Linux keyring, and Claude Code saves often. Opening the page,
    Try again, Forget and an applied change list it again.
  - **N3** Typed-in rows muted (icon kept); the banner alone in the warning colour. **N4** the store problem's
    reason shown under it, not only on hover. **N5** Forget secret is a danger button. **N6** the banner says to
    set the variable where Claude Code starts. **N7** a client secret that is not stored is left out (public
    clients have none). **N8** the page names the store one way throughout (`store_name`).
  - Tests: engine `unused_means_unused_in_this_configuration_and_the_report_names_it` (the reviewer's probe),
    `a_file_change_reads_the_definitions_again_but_not_the_store`; app tests assert the B1 wording, the reason
    line and `SecretsAfterFileChange` on a snapshot.
  - Checks after the fold-in and main 950b096 merged in (Linux): full verify + `cargo clippy -p klefki` exit 0 (673
    core, 324 app, 18 + 25 cli).
- **Next:** merge main in, re-verify, reviewer re-checks B1, merge.
