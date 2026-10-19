# Scrub internal names and an old proposal (2026-10-19)

Goal: the owner asked to remove every reference to internal environment, project and product names, and an old proposal, so the repository can go public. The git history is cleaned separately (not part of this pull request).

## Done
- Removed the owner's earlier feature proposal (a status-line and diagnosis plan tied to internal tooling) and every pointer to it: `docs/PLAN.md` (`klefki-cli` lists list/status/export), decision 006 and the `cli-list-status` progress file, `docs/progress.md` (the "OAuth on rows" label is now "OAuth on rows"), and a comment in `config/inventory.rs`.
- Example server names that came from real environments are now neutral: `REMOTE_1`, `REMOTE_2`, `acme-prod`, `acme-dev`, `acme.example.com`, `issue-tracker` at `it.example.com`. Names of equal length were picked where tests compare laid-out text.
- `model.rs`: the two v1 connection ids whose example names changed were made again with v1's encoding (`JSON.stringify`, base64url without padding); the same formula reproduced the previous ids exactly, checked with Node before the change.
- `docs/progress.md`: dropped the name of the repository v1's source came from.
- Screenshots `edit-form-general.png`, `edit-form-env.png`, `auth-tab-no-secret-store.png`: the example name repainted (same font and size), nothing else changed.

## Checks
- `cargo fmt --check` 0; `cargo clippy --workspace --all-targets -- -D warnings` 0; `cargo test --workspace` 0 (797 passed); `cargo deny check` 0.
- Case-insensitive search of the tree for the old names and the proposal's terms: no hits.

## Left
- The git history is cleaned separately, from a mirror copy, with the owner's go-ahead before any force-push. Old text in pull request descriptions and comments on GitHub is not changed by that.
