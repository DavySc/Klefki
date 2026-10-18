# Review of core, part 1: redaction and pipe wait (2026-10-18)
Goal: the overnight review of `crates/core`, `crates/cli` and `crates/fake-claude` on main (`e169d1b`) for correctness, measured against the project's performance and correctness goals. This file covers the first PR; the rest of the review, and findings left for Davy, follow in later parts.

Branch `claude/review-core-y2uzla`, from main `e169d1b`. Baseline on that main: full verify exit 0, 695 tests.

## Done
- **Diagnose let secrets through when the server printed JSON** (`diagnose/redact.rs`, 4e5e950). Claude Code logs a stdio server's stderr inside a quoted string (`fixtures/debug/failed-stdio.log`), so every quote in JSON the server printed arrives as `\"`. The rules matched plain quotes only, so `{\"apiKey\":\"…\"}`, a `password`, a `headers` or `env` object and an `Authorization` value were shown in the client's log lines and, unescaped, under what the server said. The rules now take an escaped quote wherever they take a quote. Also added the token shapes `config::secretlike` already masks in arguments: `sk_live_`, `github_pat_`, `glpat-`, `AIza`, `npm_`.
- **One pipe grace period, not one per stream** (`cli/runner.rs`, 957fbb7). After the child ended, stdout and then stderr each waited up to 2 s for their pipe to close, so a process left behind holding both pipes held every answer back 4 s (for a check, or `claude --version` on the engine thread). They now share one deadline.

## Tests (each failed first)
- `diagnose::report::tests::secrets_the_server_printed_as_json_never_reach_the_report`: five secrets in escaped JSON in a `Server stderr:` line; none reach the report, names stay readable. Failed: `SECRETKEY111 leaked`.
- `diagnose::redact::tests::escaped_quotes_hide_nothing` (variants: `api_key`, `client_secret` with a space, `x-api-key`, `Cookie`, `headers` and `env` objects). Failed on the first.
- `diagnose::redact::tests::well_known_token_shapes` gained the five shapes. Failed on `sk_live_`.
- `cli::runner::tests::pipes_left_open_after_the_child_ends_are_waited_for_once`: the test binary runs itself as a parent that starts a copy holding both pipes for 6 s and ends; the answer must come within 4 s and keep what both streams said. Failed: "took 4.01s". Now 2.0 s.

## Checks
- Verify all (`cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny check`) on 957fbb7: exit 0, 699 tests passed (695 + 4 new), deny "advisories ok, bans ok, licenses ok, sources ok".
- Only core changed, so the app's `cargo clippy -p klefki -- -D warnings` boundary is untouched (passes as part of the workspace clippy).

## Review round 1 (from "Review performance")
- Blocking: JSON escaped twice (a structured logger's JSON inside a JSON string, then inside Claude's quoted log string) still leaked; the rules allowed one backslash before a quote. They now allow any number (`\\*`). New cases in `escaped_quotes_hide_nothing` (`apiKey`, a `headers` object and `Authorization`, each escaped twice); failed first with `LEAKDOUBLE1 survived`.
- Folded in: a value that opens with a quote is hidden up to the next quote, spaces included (`{"password":"correct horse battery"}` showed `*** horse battery`); test `a_quoted_value_is_hidden_to_its_closing_quote`, failed first. Stripe's `sk_test_` and `rk_live_` keys are masked here and in `config::secretlike`; failed first.
- Left as noted by the reviewer: masking also takes the backslash of a closing `\"`, which is harmless.
- Verify all on the new head: exit 0, 700 tests passed, deny ok.

## Next
The rest of the review (config writes, engine, parsing, sign-in, `klefki-cli`) continues in this thread; clear fixes go in part 2, findings for Davy in `reviews/2026-10-19-core/findings.md` in the project folder.
