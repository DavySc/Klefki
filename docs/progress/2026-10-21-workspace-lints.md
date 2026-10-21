# Workspace lints (slice B3 of the simplification plan)

Goal: one `[workspace.lints]` table, with the three groups from `docs/proposals/simplification-plan.md` section 3 (B3).
Behaviour and look unchanged.

## Done
- Root `Cargo.toml`: `[workspace.lints]`; each crate has `[lints] workspace = true`.
  - `unreachable_pub` (warn, so `-D warnings` fails it): 128 items made `pub(crate)` (rustfix; app 95, cli 29, core 4).
  - `clippy::cast_possible_truncation`, `cast_sign_loss`, `cast_possible_wrap` (warn): the 26 casts in product code became
    `try_from` with a saturating fallback (activity, fsx, model, cli check, command scoring, selection step, fake-claude).
    Four float-to-integer casts keep `as` (it saturates, there is no `try_from`), each with an `allow` and a reason; the
    `const fn hex` in `theme.rs` too (`try_from` is not const).
    Test builds allow the three casts per crate (`cfg_attr(test, allow(.., reason))`): they build sizes and seeds with `as`.
    Product code is still checked by the non-test clippy run.
  - `clippy::print_stdout`, `print_stderr`, `dbg_macro` (deny): the CLI and fake-claude are programs whose output is the
    product, so they allow the two print lints at the crate root with a reason; core and app have none, apart from two test
    helpers in core with a reason each.
- No `clippy.toml`, `deny.toml` or protected file touched.

## Checks (Linux, this branch)
- `cargo fmt --check`: 0. `cargo clippy --workspace --all-targets -- -D warnings`: 0.
- `cargo test --workspace`: app 435, cli 25 + 34, core 937 passed, 0 failed (includes the golden and look tests; no golden
  file or picture changed: `git status` shows none).
- `cargo deny check`: advisories, bans, licenses, sources ok.

## Not run
- `quality` workflow (mutants): no logic moved except the casts above; `activity.rs`, `model.rs` and `fsx.rs` changed lines are covered by existing tests.
- Windows and macOS: CI.

## Next
Cross-review; the author merges after approval and green CI on all three systems.
