# Tests for the weakest files (simplification slice B2, 2026-10-21)

Goal: the plan's B2 (`docs/proposals/simplification-plan.md`): before A4 and the other refactors touch them, tests
for what no test checked in `swap.rs`, `config/fsx.rs`, `update.rs`, `install.rs`, `engine/sign_in.rs`,
`cli/main.rs`, `cli/profile.rs`, and the `jsonspan::value_end` gap. Tests only: no product code changed, no golden
file or look picture changed.

## Baseline (cargo-mutants 27.1.0 with nextest, on main 4c4b2ab)
| File | Mutants | Missed |
|---|---|---|
| `cli/src/main.rs` + `cli/src/profile.rs` | 112 | 26 |
| `config/fsx.rs`, `update.rs`, `engine/sign_in.rs` | 152 | 0 |
| `install.rs` | 42 | 4 (the 256 MiB download cap) |
| `config/jsonspan.rs` (`value_end`) | | 1 (known: `<=` reads past an unclosed value) |
| `swap.rs` | 233 | 54 |

## Done
- `jsonspan`: a list or object never closed is an error, not a read past the end (fb6da79). Seen failing against
  the `<=` mutant (panics at index out of bounds).
- `install`: the download is capped at 256 MiB (7148d75).
- `klefki-cli` (f24db54): `tests/cli.rs` gains a `Terminal` helper (portable-pty, the crate core's sign-in already
  uses; dev-dependency on Unix only) for what happens when someone is at a terminal: export asks before replacing
  a file and replaces only on yes, and does not ask when only the input is a terminal; status shows and clears its
  progress line (and "server"/"servers"); `use` asks with the summary and, when the file changes while the question
  is open, says "nothing was saved" at once. Plus: the organisation's servers in export, warnings on stderr, an
  output that cannot be written (`/dev/full`, Linux), the unknown-profile messages, a project known by a link.
  `Env::vars()` holds the environment both helpers use.
- `swap.rs` (3bacd8d): 24 unit tests with a fake Claude Code that edits the temporary `.claude.json` as `mcp remove`
  and `mcp add-json` do: the review's refusals and notes, saved/moved/copied in failures, what Claude Code said and
  wrote, a project spelled with a trailing slash, plain moves (three backups) and a missing project folder, put back,
  and reading, listing and dropping kept definitions.

## After
- CLI: 42 mutants on the lines that had missed ones: 40 caught, 2 missed. Both are equivalent: `id == plan.id`
  guards in `profile::use_profile`, where only one plan is ever prepared.
- `swap.rs`: 72 mutants on those lines: 68 caught, 4 missed:
  - `same_settings` line 1099 (3 mutants): they differ only when Klefki gives Claude Code a stdio definition, which
    never happens (only OAuth servers go through Claude Code, and those are http or sse). **Flagged for the owner,
    not pinned by a test:** the condition reads inverted against its comment ("Claude Code writes no `type` for a
    stdio server"): a missing `type` is accepted for http but refused for stdio. Harmless today; worth a look in A4.
  - `Recovery::save` line 1169, guard `AlreadyExists` → `true`: only another file-system error while saving shows
    it (the loop would spin); tests running as root cannot make one reliably.
- install and jsonspan: seen failing by hand against the mutants (`256 * 1024 + 1024` for the cap; `<=` in
  `value_end`).

- Review (Opus cross-review, no blocking findings) polish folded in: the status progress line pads over a longer
  earlier line (mutant `pad = 0` now fails `status_at_a_terminal_shows_its_progress_then_clears_it`), a kept
  definition just under 4x the largest entry is read (mutant `3 *` in `MAX_RECOVERY_BYTES` now fails), and the
  stderr-not-a-terminal test is named for what it shows. Both seen failing by hand.

## Checks
- `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny
  check` on 3bacd8d: exit 0 (app 438, core 960, cli 25 + 43 tests). Again after merging main b88c21a (A3 wording
  helpers): exit 0 (app 440, core 965, cli 25 + 43).
- No file under `crates/core/src/engine/tests/golden`, `crates/cli/tests/golden` or
  `crates/app/src/shell/tests/look` differs from main.

## Next
- Owner's call on `same_settings`' stdio condition (above).
