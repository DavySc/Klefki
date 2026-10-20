# Simplification safety net, part 2: quality workflow and plan (2026-10-20)

Goal: the rest of phase 1 of the owner's simplification ask (see `2026-10-20-golden-tests.md` for part 1): heavy
checks that can be run on demand, a start-up and memory baseline, documentation links checked, and the ranked
simplification plan for the owner to approve. No product code changes beyond three doc comments.

## Done
- `.github/workflows/quality.yml`: Linux only, weekly on main and by hand (Actions > quality > Run workflow).
  `coverage` (cargo-llvm-cov, summary on the run page, `lcov.info` artifact), `baseline`
  (`scripts/perf-baseline.py`), and, by hand only, `mutants` (cargo-mutants on the lines a branch changes against
  main, or on one file). Tools installed with `cargo install --locked` at pinned versions; no new third-party
  actions. `.cargo/mutants.toml` leaves out `fake-claude` (the test stand-in).
- `scripts/perf-baseline.py`: release `klefki-cli list` time and peak RSS, and the app's time to a mapped window,
  RSS and private dirty memory, on generated 70- and 950-row configurations in a temporary folder, with no Claude
  CLI on PATH (no checks start). Starts its own Xvfb when there is no display.
- CI (Linux job only): `cargo doc --no-deps --document-private-items --workspace` with `-D warnings`, as ripgrep's
  CI does. Main had three broken doc references (two to test-only functions, one unclosed `<why>`): fixed.
- `docs/quality.md`: what guards behaviour, how to update golden files, the per-slice routine for refactors.
- `docs/proposals/simplification-plan.md`: what we adopt from ripgrep and rust-analyzer and what does not fit
  (cited), the measured baselines, and the ranked plan, each item with risk and guards. Waiting for the owner.

- Review of #67 (Opus cross-review): the app's "window mapped" time could not see configuration loading (a 1.5 s
  sleep in `inventory::build` left it at 161 ms). Core now writes the file named by `KLEFKI_STARTUP_MARK` once the
  first snapshot is sent (`engine_main`, after the first `refresh`); the script times it as "app first snapshot",
  samples memory at a fixed uptime (two seconds, or one second after window and snapshot if later; memory falls over
  the first two seconds, so a moving sample point moved private dirty from 41 to 54 MB), and starts Xvfb with
  `-displayfd` instead of a fixed `:97` and a sleep. Also folded in: coverage caveat for test-heavy files, casts in
  product code (26: app 14, core 11, cli 1), mutants for pure moves in the slice routine, B4 out of the order, core
  932 tests.

## Checks
- Coverage on main 96f604d: 96.6% of lines (test modules included); below 90%: `update.rs`, `api.rs`,
  `install.rs`, `swap.rs`, `fsx.rs`, `engine/sign_in.rs`, `cli/tree.rs`, `cli/main.rs`, `cli/profile.rs`.
- Baseline, 7 runs, release: app window 184 ms (both sizes), RSS 127 / 130 MB, private dirty 41 / 44 MB;
  `klefki-cli list` 3.8 / 13.6 ms.
- Doc build with `-D warnings`: exit 0 after the three fixes.
- Mutation sample (`sanitize.rs`, `config/jsonspan.rs`, 135 mutants with nextest, 88 min): 117 caught, 9 hung,
  2 unviable, 7 missed; six equivalent (fast path), one real gap (an unclosed object in `jsonspan::value_end`).
  The first attempt with `cargo test` reported caught mutants as 300 s timeouts (engine tests wait 60 s each).
- Full verify plus the doc build (`cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny check && RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items --workspace`): exit 0; app 420, core 932, cli 25 and 34, deny ok.
- After review: `scripts/perf-baseline.py --runs 7`: first snapshot 115 / 135 ms, window 171 / 188 ms, private
  dirty 40.8 / 43.9 MB. Against the build before the change the script stops with "never reported its first
  snapshot" (seen); with a 1.5 s sleep in `inventory::build` first snapshot read 1,630 / 1,615 ms, window 216 / 183.
- The `quality` workflow itself runs only once merged (GitHub offers "Run workflow" for workflows on the default branch): unverified until then.

## Next
- The owner reads the plan and approves, changes or drops items. Then one slice per item, in the plan's order.
