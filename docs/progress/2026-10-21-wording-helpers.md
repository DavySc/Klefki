# One set of wording helpers in core (simplification A3)

Goal: slice A3 of `docs/proposals/simplification-plan.md`. The engine, the app and the CLI each had their own copies
of "a, b and 3 more", `folder_name`, `sentence` and `count`. Keep one pure copy in core, exported through `api`.
Behaviour and look unchanged; each place keeps its own cut-off for a long list (C2, one cut-off for all, is not
approved).

## Done
- `crates/core/src/words.rs`, exported as `klefki_core::api::words`: `and_list`, `and_list_upto(items, max, named)`,
  `comma_list_upto(items, max, named)`, `and_more(shown, total)`, `count`, `folder_name`, `parent_folder`,
  `capitalized`, `sentence`, with a table test of each at and just past every cut-off the callers use.
- Replaced, each keeping its count:
  - core: `ops::and_list`, `ops::count`, `ops::folder_name` (gone; `ops::some_of` is now one line, 6 named in full,
    past that 5 and how many more), `form::sentence`, `paste::listed`.
  - app: `shell::sentence` (now `words::capitalized`), `list_in_words`, `names_in_a_line` (8), `some_names` (6/5,
    commas only), `parent_folder`, the inline split in `group_of`, `plugin_on_text` and
    `in_projects_text` (5); `profiles::folder_name`, `parent_of`, `named_few` (3), the profile card's summary (5).
  - cli: `render::named` (3).
- Left as they are: lines that end a cut list ("… and 4 more" under warnings, the old switch-off lists, the
  activity log) and the hover list of whole paths (one per line): each is a separate line, not a list in a sentence.
- `folder_name` stays twice, on the owner's call (2026-10-21): the copies differ only for a project at the root `/`,
  which core names `/` and the app leaves empty (group header "Local · "). The app keeps one copy of its own
  (`shell::folder_name`, used by the list and the profiles page, which had two), pinned by
  `a_project_at_the_root_keeps_an_empty_name`. `each_list_keeps_its_own_cut_off` passes on main and here: no
  cut-off moved.

## Review (#71, approved, no blocking findings)
- Folded in the two optional notes: `ops::some_of`'s doc says the app cuts at the same count with commas only;
  `words::folder_name`'s doc points the app to `shell::folder_name` for a root project.

## Checks
See the pull request description. After the review fixes: fmt, clippy `-D warnings`, `cargo test --workspace`
(app 440, core 942, cli 25 + 34) and `cargo doc` for core, all clean; main had not moved (3d1c88f).

## Next
Merge, then A2 (split `engine.rs`) and A1 (split `shell.rs`).
