# Scrub internal names and an old proposal (2026-10-19)

Goal: the owner asked to remove every reference to internal environment, project and product names, and an old proposal, so the repository can go public. The git history is cleaned separately (not part of this pull request).

## Done
- Removed an old proposal and every pointer to it.
- Example server names that came from real environments are now neutral.
- `model.rs`: the two v1 connection ids whose names changed were made again with v1's encoding (`JSON.stringify`, base64url without padding); the same formula reproduces the old ids exactly, checked with Node before the change.
- `docs/progress.md`: dropped the internal repository name of v1's source.
- Three screenshots: the example name repainted, nothing else changed.

## Checks
- `cargo fmt --check` 0; `cargo clippy --workspace --all-targets -- -D warnings` 0; `cargo test --workspace` 0 (797 passed); `cargo deny check` 0.
- Case-insensitive search of the tree for the old names and the proposal's terms: no hits.

## Left
- The git history is cleaned separately.
