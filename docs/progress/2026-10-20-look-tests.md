# Look tests: the UI keeps its look through the simplification (2026-10-20)

Goal: the owner asked that the look (UI and UX) stays the same through the refactor. Behaviour was guarded (app
tests, golden files); the look was not. Part 3 of the safety net (parts 1 and 2: `2026-10-20-golden-tests.md`,
`2026-10-20-quality-workflow.md`).

## Done
- `crates/app/src/shell/look.rs`: 13 tests, 34 pictures in `crates/app/src/shell/look/` (4.6 MB): the server list in
  dark and light, cool and warm, at 800×480, filtered and with two selected; every detail tab of a user, a project
  and an organisation server; every page (profiles, Claude Desktop, scope drift, secrets, settings, activity log,
  with the sample data of each page's own tests); the command palette; Add, Duplicate, Move, Export (reading and
  done) and the casing dialog; the edit form and its JSON view; the review of a change in dark and light. The
  fixture configuration, with the engine replaced by a detached one, so every dialog opens as in the app.
- egui_kittest's `snapshot` and `wgpu` features, Linux only (`[target.'cfg(target_os = "linux")'.dev-dependencies]`),
  so Windows and macOS CI build nothing more. The renderer is pinned to OpenGL (Mesa llvmpipe), so a machine with
  Vulkan drivers draws the same; threshold 0.
- `sample()` of the activity, Claude Desktop and secrets tests made `pub(crate)` for the look tests to reuse.
- CI (Linux): `libegl1 libgl1-mesa-dri`; on failure the changed pictures and their diffs are uploaded.
- `docs/quality.md`: the look tests, how to update them, and "a changed picture in a refactor means stop".

## Checks
- The tests catch changes: a tab spacing 4.0 → 5.0 failed 3 tests (≈300 to 490 pixels per picture); one palette
  colour changed by 1/255 (`DARK.row_alt` 0x232334 → 0x232335) failed the 3 list tests (it passed at the default
  threshold 0.6, hence threshold 0). Both reverted.
- Five runs in a row, unchanged code: all 13 pass (4.6 to 5.1 s).
- Full verify plus the doc check: exit 0 (app 429, core 937, cli 25 + 34; deny ok).
- Unverified until CI runs: that ubuntu-latest draws the same pixels (the container is Ubuntu 24.04 with Mesa
  25.2.8; CI's image should match).

## Next
- If CI's pictures differ, take them from the `look-changes` artifact or set a small threshold, whichever the
  difference shows.
