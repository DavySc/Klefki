# Windows: `klefki-cli list` test expects a full path the CLI shortens (2026-10-19)

Goal: `list_shows_every_server_grouped_by_scope_and_project_without_running_anything` failed on a local Windows run. The CLI shortens paths under the home folder to `~/…` (`render::short_path`, home from `std::env::home_dir()`). The test redirects `HOME`, which Windows ignores (Rust 1.85+ reads `USERPROFILE` or the profile folder), so the home there is the real profile, and the default temporary folder lives under it. The heading reads `Local in ~/AppData/Local/Temp/…/repo`, while the test looked for the full path. CI on Windows passes only because its temporary folder is spelled with the short name (`C:/Users/RUNNER~1/…`) and the profile with the long one (`C:\Users\runneradmin`), so no prefix matches. That last part is read from the code, not seen on a CI log.

The product's shortening is right; the test was wrong. Fixed in the test only.

## Done
- `crates/cli/tests/cli.rs`: `Env::home()` (the home the commands see: the redirected one, or on Windows the real one) and `Env::shown_project(home)` (the project as a heading names it). The list test asserts the heading through them, so it holds whatever the temporary path looks like.
- Regression test `list_shortens_a_project_under_the_home_folder` (not on Windows, where the home cannot be moved): `HOME` set to the temporary root, so the project lies under it as on Windows. Headings show `~/repo`; JSON keeps the full path. With the old full-path expectation it fails (`a Local group naming …/repo in: … Local in ~/repo`), seen on Linux before the fix.

## Checks (Linux)
- `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny check`: exit 0, 798 passed.
- Not run here: Windows. The pull request's Windows CI covers the RUNNER~1 case; the long-name case needs a local Windows run.

## Next
- The owner re-runs `cargo test -p klefki-cli` on Windows.
