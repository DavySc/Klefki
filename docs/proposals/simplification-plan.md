# Simplification plan (proposal, 2026-10-20)

The owner asked to simplify the code as far as possible without losing robustness, functionality or the handling
of unforeseen situations, with the test suite made ready first and ripgrep and rust-analyzer as models. This is
phase 1's result: what we take from those two projects, what the suite now guards, and a ranked list of
simplifications, each with its risk and the tests that guard it. **Nothing here is started before the owner
approves it**; then each item is its own slice and pull request, behaviour unchanged.

## 1. What we adopt from ripgrep and rust-analyzer, and what does not fit

Sources: rust-analyzer's [style guide](https://github.com/rust-lang/rust-analyzer/blob/master/docs/book/src/contributing/style.md)
and [architecture](https://github.com/rust-lang/rust-analyzer/blob/master/docs/book/src/contributing/architecture.md)
(both read 2026-10-20), its root `Cargo.toml` lints, and ripgrep's
[`Cargo.toml`](https://github.com/BurntSushi/ripgrep/blob/master/Cargo.toml),
[CI](https://github.com/BurntSushi/ripgrep/blob/master/.github/workflows/ci.yml) and
[`tests/`](https://github.com/BurntSushi/ripgrep/tree/master/tests).

### Adopt
| From | Practice | For Klefki |
|---|---|---|
| rust-analyzer, architecture "Testing" | "Tests are data driven and do not test the API", with `expect-test` snapshots; tests at a few boundaries | Done in #65: golden files at the two boundaries users meet (the engine's plans and files, the CLI's output). New tests should go through small `check(input, expected)` helpers, so internals can change without rewriting tests. |
| rust-analyzer, architecture "Testing" | "All code invariants are tested by `#[test]` tests"; "tests do not depend on any kind of external resources" | Already so (temp `CLAUDE_CONFIG_DIR`, fake-claude, memory secret store). Keep it: no separate scripts that CI must remember. |
| rust-analyzer, style "`#[ignore]`", "`#[should_panic]`" | Never ignore a test; assert the wrong behaviour with a note instead. No `should_panic`: user input must never panic | Already CLAUDE.md's rule; the suite has no `#[ignore]`, `#[should_panic]` or `#[allow]`. |
| rust-analyzer, style "Functions Over Objects", "Early Returns", "Prefer Separate Functions Over Parameters", "Helper Functions" | Small free functions, guard clauses first, one function per behaviour instead of a flag | The guide for splitting the long functions in section 3 (B1). |
| rust-analyzer, style "Avoid Monomorphization" and "Crates.io Dependencies" | Few generics across crate boundaries; few dependencies, and read `Cargo.lock` for transitive ones that make no sense | Klefki already keeps generics small. Dependency trims are in C1, measured by the baseline. |
| rust-analyzer, root `Cargo.toml` | One `[workspace.lints]` table for the whole workspace | B3: move lint settings there and turn on a few that find real problems (measured below). |
| rust-analyzer, architecture "Error Handling" | Long-running process: a panic in one feature must not bring down the rest; partial results over all-or-nothing | Already so (a panicking check gives its slot back; the inventory reports problems per file). Keep it when merging code paths. |
| rust-analyzer, architecture "Observability" | The event loop takes a closed `enum` of events | Already so (`Command`/`Event`). Keep it as the one way in and out of the engine. |
| rust-analyzer, architecture "Bird's Eye View", "Code Map" | A short architecture document: what each part does, and its invariants | B5: `docs/ARCHITECTURE.md`, a one-page map; `PLAN.md` keeps the roadmap. |
| ripgrep, `tests/` | End-to-end tests drive the real binary in a temporary folder and compare whole output (`eqnice!`) | Klefki's CLI tests did the first part; #65 adds whole-output comparison. |
| ripgrep, CI | `cargo doc --no-deps --document-private-items` in CI | B4: Klefki's comments use many `[`links`]`; a broken one should fail. Linux only. |
| ripgrep, CI | Fuzz targets are at least built in CI | Later, optional: the untrusted-input parsers (`.mcp.json`, `mcp get` output, the paste lexer) already have proptest; fuzzing needs nightly, so not now. |

### Does not fit
- **rust-analyzer's many crates and salsa** (incremental computation): Klefki is about 40,000 lines of code and
  45,000 of tests in three crates; splitting further adds API boundaries that need care (its own "Scale of Changes")
  for no gain in build time or clarity at this size. Modules are enough (section 3, A1 and A2).
- **ripgrep's reusable library crates** (`globset`, `ignore`, …): they exist to be published and reused. Klefki
  publishes nothing.
- **Skipping slow tests by an environment variable** (rust-analyzer's `RUN_SLOW_TESTS`): CLAUDE.md forbids skipping
  tests; the heavy checks go to the separate `quality` workflow instead.
- **`cov_mark`** (marks a branch to find its test): puts test hooks into product code and adds a dependency;
  `cargo-mutants` finds untested branches without either.
- **`panic = "abort"` and a separate `release-lto` profile** (ripgrep): Klefki relies on unwinding to recover from a
  panicking job, and its release build is already the only optimised one.
- **Codegen and `xtask`**: Klefki generates almost nothing.

## 2. What guards a refactor now
See `docs/quality.md`.
- Every pull request: fmt, clippy `-D warnings` with the UI's `disallowed-methods`, deny, 1,410 tests on three
  systems (app 420, core 931, cli 25 + 34), including the model test and the golden files from #65.
- Baselines measured on main 96f604d (Linux container, 4 cores, Mesa llvmpipe):
  - Coverage (lines, `cargo llvm-cov`, test modules included): 96.6% overall. Below 90%: `update.rs` 80%,
    `api.rs` 79% (the real start paths), `install.rs` 89%, `swap.rs` 90% (OAuth remove and add-back), `fsx.rs` 88%
    (the file-access allow list), `engine/sign_in.rs` 88%, `cli/tree.rs` 87%, `cli/main.rs` and `cli/profile.rs`
    84%, `config/mod.rs` 74%, `app/main.rs` 37%.
  - Mutation testing, a sample (`sanitize.rs` and `config/jsonspan.rs`, the untrusted-text cleaner and the
    byte-level JSON reader under every write; 135 mutants, 88 minutes): 117 caught, 9 hung (counted as caught: a
    test would hang), 2 did not compile, 7 missed. Six of the missed are in `sanitize`'s fast path for plain text
    (`is_plain`, `plain_run`, the loop bound): turning the fast path off gives the same output, only slower, so no
    test can see it; these are equivalent mutants, and a missed mutant needs reading, not a reflex test. The
    seventh is real: `jsonspan::value_end` with `<=` would index past the end of an object or array that is never
    closed, and no test feeds it one (a truncated `.claude.json`). Such findings are B2's work.
  - Whole workspace: about 7,100 mutants. With `cargo test`, a mutant that breaks the engine made dozens of tests
    each wait out a 60 s deadline, so mutants "timed out" instead of being caught; `.cargo/mutants.toml` therefore
    runs them with `cargo-nextest`, which stops at the first failure (a mutant then takes 20 to 60 s here). Still
    too slow to run whole: by file, or by the lines a branch changes.
  - Start-up and memory (`scripts/perf-baseline.py --runs 7`, release build, no Claude CLI so no checks run),
    median (min to max):

    | Measure | 70 rows | 950 rows |
    |---|---|---|
    | `klefki-cli list`, ms | 3.8 (2.6 to 20.9) | 13.6 (12.5 to 16.1) |
    | `klefki-cli list` peak RSS, MB | 14.0 | 14.5 |
    | app, start to window mapped, ms | 184 (174 to 2,934, the first run cold) | 184 (159 to 204) |
    | app RSS after 2 s, MB | 126.5 | 129.6 |
    | app private dirty memory, MB | 40.9 | 43.9 |

    These match the 2026-10-19 performance review (123 to 133 MB RSS, 39 MB private dirty, first frame 110 to
    230 ms): Klefki's own share is small; Mesa's software OpenGL is most of the RSS.
- The `quality` workflow re-runs all three, weekly or by hand, Linux only.

## 3. Ranked simplifications
Ranked by value against risk. Size is production lines unless said. Every item: behaviour unchanged, no golden
file changes, full verify, cross-review, and the per-slice steps in `docs/quality.md`.

### A. Structure (mechanical, low risk, large gain in readability)
**A1. Split `app/src/shell.rs`** (9,350 lines of code plus 10,900 of tests; `impl ShellApp` alone is 5,450 lines;
`apply_event` 540, `detail_tab` 410, `add_dialog` 350, `export_dialog` 340). One module per dialog or page (add,
duplicate, move, export, import, plan review, detail, server list, top bar), each holding its form state, its
`show` function and its tests; `ShellApp` keeps the routing. Pure moves and visibility changes.
- Risk: low for behaviour (the compiler checks every move); high for merge conflicts with other sessions, so one
  module per pull request, when no other open pull request touches `shell.rs`.
- Guards: the 420 app tests (egui_kittest), unchanged; clippy's `disallowed-methods` still covers every new file.

**A2. Split `core/src/engine.rs`** (3,540 lines of code plus 8,500 of tests in the same file): the command loop,
the check queue and workers, prepare and apply, diagnose. Tests move next to what they test.
- Risk: low to medium (the queue's locking must move as a whole).
- Guards: the 931 core tests, the model test, the 16 plan golden files, the CLI golden files.

**A3. One set of wording helpers.** "a, b and 3 more" is written eight times (`shell.rs` four, `profiles.rs`,
`cli/render.rs`, `ops.rs`, `paste.rs`), next to `list_in_words`, `names_in_a_line`, `some_names` (app) and
`and_list`, `some_of`, `count` (core); `folder_name` three times, the copies differing for a root path; `sentence`
twice (one adds a full stop). Keep one pure copy in core, exported through `api`.
- Risk: low in code, but **the copies cut off at different counts (3, 5, 6 and 8 names)**. Keeping every count as
  a parameter changes nothing; choosing one count is a visible change and the owner's call.
- Guards: golden files (plan summaries, CLI), the app's text assertions, a new table test of the helper.

**A4. Long functions into named steps** (rust-analyzer's "functions over objects", "early returns"):
`inventory::build` 450 lines, `ops::use_profile` 290, `paste::add` 270, `swap::run` 220, `paste::lex_with` 215,
`restore::compare` 190, `cli args::parse` 170. One file per slice.
- Risk: medium for `swap::run` (OAuth remove and add-back with rollback) and `inventory::build` (what Claude Code
  loads); low for the rest.
- Guards: per file. `swap.rs` is at 90% coverage: its failure paths get tests first (B2).

### B. Test and quality gaps to close first, and lint settings
**B1. App test helpers.** About 60 app tests find buttons by their icon glyph, and many repeat set-up. A few
`check`-style helpers (open a dialog with this configuration, press this named control) and accessible names for
the icon buttons (also a UX finding, WCAG 4.1.2) make A1 safer and shorter.
- Risk: low. Guards: the tests themselves keep passing at every step.

**B2. Raise the weakest files before they are touched:** `swap.rs`, `fsx.rs`, `update.rs`, `install.rs`,
`engine/sign_in.rs`, `cli/main.rs`, `cli/profile.rs`: coverage under 90% and mutants missed (section 2). Tests
only, each seen failing against a mutant.

**B3. `[workspace.lints]`** in the root `Cargo.toml`, as rust-analyzer does. Measured on main: all of
`clippy::pedantic` gives about 700 warnings, nearly all style (139 emptiness checks spelled another way, 89 long
literals, 41 `must_use`); not worth turning on whole, and rust-analyzer does not either. Worth it, each in its
own slice:
- `clippy::cast_possible_truncation`, `cast_sign_loss`, `cast_possible_wrap`: 60 casts (core 29, app 24) that
  can silently change a number. Each becomes `try_from` with a fallback, or a checked cast with a comment. This is
  a robustness gain, not only style.
- `unreachable_pub`: 128 items marked `pub` that nothing outside their crate can use (app 95, cli 29, core 4).
  Turning them into `pub(crate)` shows the real API (core's `api`) at a glance.
- `clippy::print_stdout`, `print_stderr` and `dbg_macro` denied in core and the app (no hits outside tests today):
  keeps output in the CLI and logs where it belongs.

**B4. Documentation links checked** (done in part 2): `cargo doc --no-deps --document-private-items` with warnings
as errors on CI's Linux job. Main had three broken references, fixed there. Two of them pointed at functions that
exist only in tests (`jsonspan::object_at`, `update::check`); test-only helpers in product files are part of A2.

**B5. `docs/ARCHITECTURE.md`**: one page, what each crate and the main modules do, the data flow (UI ->
`Command` -> engine -> `Event` -> snapshot) and the invariants, pointing to CLAUDE.md. `PLAN.md` (249 lines, partly
history) keeps only the roadmap.

### C. Needs the owner's decision
**C1. Dependency and size trims** from the 2026-10-19 performance review, none started: subset the Phosphor icon
font (about 460 KiB less), store theme and window size through core instead of eframe's `persistence` (drops
`ron`), build `ConnectionId` without serde (18% of loading at 950 rows). Each is measured with
`scripts/perf-baseline.py` before and after.

**C2. Wording thresholds** (A3): one cut-off for "and N more" everywhere, or keep each place's own.

## 4. Order
1. #65 (golden files) and the `quality` workflow with this plan (part 2) merged.
2. B2 for the files A4 touches, B1, B4, B3.
3. A3, then A2, then A1 module by module, then A4 file by file.
4. C1 items one at a time if the owner wants them.
