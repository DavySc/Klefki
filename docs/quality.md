# Quality suite

What guards Klefki's behaviour, and how to use it when changing code, above all when simplifying it: a refactor
must leave every test, golden file and baseline as it was.

## Every pull request (CI on Windows, macOS and Linux)
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` (with the app's
  `disallowed-methods`, so the UI does no IO), `cargo test --workspace`. Threads also run `cargo deny check`.
- Unit and engine tests in each module, the engine's model test (`engine/machine.rs`, proptest: random sequences of
  changes never lose a server or its secret), the app's egui_kittest tests in `shell.rs` and the other pages, and
  `klefki-cli`'s end-to-end tests in `crates/cli/tests/cli.rs`. Tests use a temporary `CLAUDE_CONFIG_DIR`, the
  `fake-claude` stand-in and an in-memory secret store, never the real configuration or keychain.
- Golden files: what users see and what lands on disk, byte for byte.
  - `crates/core/src/engine/tests/golden/`: for each kind of change, the review (summary, warnings, notes, what it
    would run, every diff, the button), the outcome, and every file it may write afterwards.
  - `crates/cli/tests/golden/`: every `klefki-cli` command's stdout, stderr and exit code.
  - A difference fails with a diff. When a change is meant, run the tests with `UPDATE_EXPECT=1` and review the
    golden files in the pull request. **In a refactor, a golden file that changes means behaviour changed: stop.**
- Look tests (`crates/app/src/shell/tests/look.rs`, Linux only): 62 pictures in `crates/app/src/shell/tests/look/`,
  compared pixel for pixel (threshold 0). Pages, detail tabs, statuses (checking, failed, a diagnosis), empty and
  error states and the update in the top bar at 1100×800; every dialog (Add in each mode, Import, Duplicate, Move,
  Export, casing, profiles, sign-in, the reviews including Delete and "nothing to do", and the outcome messages) at
  the smallest window, 800×480; the list in the four themes, and the dialogs with colours of their own (review,
  outcome, sign-in) in light too. Drawn with wgpu on Mesa's OpenGL software renderer (pinned in the test, so Vulkan
  drivers do not change it). A one-step colour change or a 1 px spacing change fails them.
  - A failure leaves `<name>.new.png` and `<name>.diff.png` beside the picture (ignored by git); CI uploads them as
    the `look-changes` artifact.
  - When a change to the look is meant: `UPDATE_SNAPSHOTS=1 cargo test -p klefki look`, and show the pictures in
    the pull request. **In a refactor, a picture that changes means the look changed: stop.**
  - A new screen, dialog or state gets a picture in the same pull request.
  - Mesa drift: the pictures match Ubuntu 24.04's Mesa (25.2.8), which is why ci.yml and quality.yml pin
    `runs-on: ubuntu-24.04`. On 2026-10-26 `ubuntu-latest` moved to Ubuntu 26.04 (Mesa 26.0.8) and 9 look tests
    failed by a few pixels on every pull request, so Linux jobs stay on 24.04. When GitHub retires the 24.04
    image, retake the pictures on the new Mesa in their own pull request. If a runner image moves to another Mesa,
    every look test fails at once on unrelated pull requests, with differences scattered over edges and text
    rather than in one place. Then fix it in its own pull request: take the pictures from that run's
    `look-changes` artifact (the `.new.png` files, renamed), look at them side by side with the old ones, and say
    which Mesa they come from. Never run `UPDATE_SNAPSHOTS=1` on a machine whose Mesa differs from CI's; the cloud
    sessions' container has the same one.

## By hand or weekly (the `quality` workflow, Linux only)
macOS minutes cost ten times as much, so heavy checks run on Linux, weekly on main, or from Actions > quality > Run
workflow on a branch.
- **coverage**: `cargo llvm-cov` line coverage per file; summary on the run page, `lcov.info` as an artifact.
  Locally: `cargo install cargo-llvm-cov`, `rustup component add llvm-tools-preview`, `cargo llvm-cov --workspace
  --summary-only`. The numbers include the test modules in each file, so a file's own code is covered a little
  less than shown.
- **baseline**: `scripts/perf-baseline.py`: start-up time and memory of the release app and `klefki-cli list` on a
  70-row and a 950-row configuration. For the app it times both the window appearing (which happens before the
  configuration is read) and the engine sending the UI its first snapshot (which includes loading; the app writes
  the file named by `KLEFKI_STARTUP_MARK` at that point). Compare a branch with main on the same machine only; software OpenGL is most
  of the app's memory on a machine without a GPU.
- **mutants** (by hand only): `cargo-mutants` changes the code one small edit at a time (a condition flipped, a
  function returning a default) and checks that some test fails. A *missed* mutant is code no test really checks.
  `diff` mutates only the lines the branch changes against main, which is what a refactor needs; a file path
  mutates one file. The whole workspace has about 7,100 mutants at a minute or more each, so it is never run
  whole. Locally (with `cargo-nextest` installed, which `.cargo/mutants.toml` asks for: it stops at the first failing
  test, where `cargo test` would let every broken engine test wait out its 60 s deadline): `cargo mutants
  --in-diff <(git diff origin/main...)` or `cargo mutants -f <file>`.

## For a simplification slice
1. One area per branch; behaviour unchanged.
2. Before: for the files the slice changes, read coverage and run mutants on them. Missed mutants in code the
   slice moves or rewrites get a test first, in its own commit, seen failing against a mutant.
3. After: the full verify (CLAUDE.md), no golden file or look picture changed, no new missed mutants, and, for anything on the start-up path (engine start, configuration
   loading, the app's first frame), `scripts/perf-baseline.py` on main and on the branch, both in the pull request.
   For mutants: a slice that changes code runs the `quality` workflow with `mutants: diff`. A slice that only moves
   code (A1, A2) would mutate every moved line, hours beyond the job's limit: instead compare the missed mutants of
   the moved code's file before (on main) and after (the new file), with `mutants:` set to each file, or, for a
   pure move with no changed line, rely on the unchanged tests and golden files.
