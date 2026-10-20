# Look tests: the UI keeps its look through the simplification (2026-10-20)

Goal: the owner asked that the look (UI and UX) stays the same through the refactor. Behaviour was guarded (app
tests, golden files); the look was not. Part 3 of the safety net (parts 1 and 2: `2026-10-20-golden-tests.md`,
`2026-10-20-quality-workflow.md`).

## Done
- `crates/app/src/shell/tests/look.rs` (inside the shell tests, so it reuses their helpers): 19 tests, 58 pictures in
  `crates/app/src/shell/tests/look/` (6.4 MB).
  - At 1100×800: the server list in dark and light, cool and warm, filtered, with two selected, with a check running
    and a failed server; no servers; an unreadable `.claude.json` with no Claude CLI; an update in the top bar; every
    detail tab of a user, a project and an organisation server; a failure's Status tab and a diagnosis; every page
    (with the sample data of each page's own tests); the edit form and its JSON view.
  - At 800×480: the list; the command palette; Add (empty, pasted, filled in for http with OAuth), Import,
    Duplicate, Move, Export (reading and done), the casing dialog; the profiles page with a profile in use, New
    profile, Delete profile; the review of a change, of a deletion and of nothing to do; the saved and failed
    outcome messages; sign-in starting, waiting and failed. Review, outcome and sign-in in light too.
  - The fixture configuration with a detached engine, so every dialog opens as in the app.
- egui_kittest's `snapshot` and `wgpu` features, Linux only (`[target.'cfg(target_os = "linux")'.dev-dependencies]`),
  so Windows and macOS CI build nothing more. The renderer is pinned to OpenGL (Mesa llvmpipe), so a machine with
  Vulkan drivers draws the same; threshold 0.
- `sample()` of the activity, Claude Desktop and secrets tests made `pub(crate)` for the look tests to reuse.
- CI (Linux): `libegl1 libgl1-mesa-dri`; on failure the changed pictures and their diffs are uploaded. `.gitignore`
  ignores `*.new.png`, `*.diff.png`, `*.old.png`.
- `docs/quality.md`: the look tests, how to update them, Mesa drift, and "a changed picture in a refactor means stop".

## Checks
- The tests catch changes: a tab spacing 4.0 → 5.0 failed 3 tests; one palette colour changed by 1/255
  (`DARK.row_alt`) failed the list tests (it passed at the default threshold 0.6, hence threshold 0). The reviewer
  flipped the lowest bit of each of the 20 dark colours: every flip failed a test.
- Review round 1 (Opus cross-review): four changes went unnoticed by the first 34 pictures (Delete's button in the
  primary colour, sign-in spacing 14 → 15, the outcome frame colour, diagnosis spacing 10 → 11). With the 58
  pictures each one fails a test (`reviews_and_how_they_end`, `signing_in`, `reviews_and_how_they_end`,
  `a_diagnosis`). All reverted.
- CI on ef4baa7: green on all three systems; Linux ran the look tests and drew the same pixels as the container
  (Ubuntu 24.04, Mesa 25.2.8).
- Full verify plus the doc check: see the pull request.

## Next
- After the refactor (not this slice): the list's fourth column (Target) shows only "." at 1100×800; the reviewer
  flags it as lost information (WCAG 1.4.10). The pictures pin it as it is.
