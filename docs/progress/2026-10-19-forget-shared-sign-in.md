# Forget waits while a kept server's sign-in is shared (2026-10-19)
Goal: model seed `e896270e` (found by another session, hit on Windows CI of a UI-only PR) fails on main 15ab60e.

- **Reproduced** on main 15ab60e with the seed in `crates/core/proptest-regressions/engine/machine.txt`, every run:
  b in pa copied into pb (one sign-in for both); an edit of b in pa fails twice, so Klefki keeps it, and Claude Code's
  remove took the client secret of both; Forget. b in pb then has no client secret in Claude Code, while Klefki
  holds one it cannot hand over. Older than #11; the seed is just rare.
- **Fix, the safer choice: refuse.** `swap::forget_problem`: Forget of a kept server that signs in, is not back
  where it was, and shares its sign-in with a server elsewhere is refused at review and again when applied, and says
  to put it back, which hands both their secret again. Asked Davy on the thread (decision card), going ahead with the
  recommended option. Alternatives: Forget re-adds the other server with its secret; or only warn. Decision 007.
- **Test changed, for human review:** the model required Forget to succeed whenever something was kept. It now
  requires the refusal (with its reason) in exactly the shared case above, and Done otherwise.
- **Tests:** the seed; machine `a_kept_definition_that_took_a_shared_client_secret_is_not_forgotten` (the case, then
  Put back restores both; variant: once the other b has another address, Forget works).
- **Checks** (Linux): `cargo fmt --check` 0; `cargo clippy --workspace --all-targets -- -D warnings` 0; `cargo test
  --workspace` 0 (596 core, 292 app, 22 + 16 cli); `cargo deny check` 0; `cargo clippy -p klefki -- -D warnings` 0.
  Model soak 2 × 4000 cases passed (134 s, 127 s). Windows and macOS: CI only.
- **Open:** Klefki cannot delete or rename a server that signs in, so the refusal names Put back as the way out;
  changing the other server's address also frees it.
- **PR #19**, CI green on all three systems at f1b18b9. **Cross-review** (another thread): correct, one fold-in done:
  the refusal says the other server's secret *may* be gone (Klefki cannot read Claude Code's credentials; the other
  server may have been added later with its own). The reviewer's "Put back, then Delete" does not apply: Klefki
  refuses to delete a server that signs in ("only Claude Code's own commands change it", seen in the variant test).
- **Next:** merge.
