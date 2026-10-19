# Review of performance, part 1: pasting and the watcher (2026-10-18)
Goal: the overnight performance review of main (`e169d1b`): start-up, RAM, frame time, UI-thread work, allocations, process spawning, dependency weight, and code that can be simpler. This file covers the first PR; findings for Davy (changes in files #26 touches, trade-offs, anything user-noticeable) are in `reviews/2026-10-19-performance/findings.md` in the project folder.

Branch `claude/review-performance-hwr3ft`, from main `e169d1b`. Baseline on that main: full verify exit 0, 695 tests.

## Done
- **A large paste froze the add dialog** (`paste.rs`). Skipped lines and shell variable names were de-duplicated by searching every earlier one, and the dialog reads the text again after every key typed. A 255 KiB paste of short lines took 2.3 s to read (release), and one command with 33k distinct `$VARS` took 1.5 s. Both now take about 20 ms. The dialog also drew one note per skipped line (38,886 notes, 2.3 MB of text, laid out every frame) and one note listing every variable (295 KB): skipped lines are now named for the first 5 and counted after that ("Skipped 35 more lines that are not claude mcp add commands."), and a note names the first 8 variables and counts the rest ("… and 32 more"). The form's own variable note (`form.rs`) shares the same list. Ordinary pastes read as before.
- **Review round 1 (app UI thread, on `920161a`)**: a mistyped command after more than five lines of a pasted guide was only counted, where main named it. Skipped lines that start with `claude` are now named first. A count never stands in for one item: up to 6 lines and 9 variable names are all named, and only 2 or more are counted (Baymard, "6 Guidelines for Truncation Design"). Not taken (optional): one note that leads with the total, because each skipped line stays readable on its own line, and the count now follows them directly instead of after a server's notes.
- **The first load with many projects waited on the watcher** (`watch.rs`). `Watch::extend` looked for each new file among all files added before it, on the engine thread before the list is sent. With 300 projects (603 files) that was 14 to 22 ms of each start-up and of each reload that adds projects; with a set of the known files it is 0.2 to 0.5 ms (microbenchmark at that shape, 65 MB `.claude.json`).

## Tests (each failed first)
- `paste::tests::the_largest_paste_is_read_at_typing_speed`: a full-size paste of distinct lines and one of distinct variables, read within 2 s (debug) with at most 6 notes. Failed: 38,886 notes after 7.5 s. Now 0.2 s.
- `paste::tests::many_skipped_lines_are_counted_not_all_listed` (variant: the same line 40 times is one note) and `paste::tests::many_shell_variables_are_counted_not_all_listed` (variant: a name used twice is listed once). Failed: 41 notes; a note naming all 40 variables.
- `form::tests::variables_are_left_for_claude_code_to_fill_in` gained 20 variables: 8 named, "and 12 more". Failed: all 20 named.
- `paste::tests::a_mistyped_command_is_named_however_much_text_comes_before_it` (the reviewer's paste, and the typo as the last line of a 40-line guide) and `paste::tests::one_more_is_named_not_counted` (6 lines, 9 and 10 names). Both failed on `920161a`.
- `watch::tests::many_new_files_are_watched_at_once`: 10,000 new files, then the same again, within 2 s (debug), each once and in order. Failed: took 24.7 s. Now well under 1 s. Variant `watch::tests::a_file_named_twice_is_watched_once` guards the new set (passed before and after).

## Checks
- Verify all (`cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo deny check`): exit 0, 700 tests passed (695 + 5 new), deny "advisories ok, bans ok, licenses ok, sources ok". After merging main (`1f480c7`, with #27): exit 0, 705 tests. After review round 1: exit 0, 707 tests.
- Release timings of `Pasted::parse` before → after (255 KiB): distinct lines 2,294 → 21 ms; distinct variables 1,500 → 20 ms; the same line repeated 11 → 7 ms; 7,493 commands (refused, over 100) 22 → 14 ms.

## Baseline measured on main (Linux, Xvfb, Mesa llvmpipe, release build)
- 30 user servers, 10 projects: window in 48-86 ms, list drawn at about 100-150 ms. RSS 125-128 MB, of which Mesa's LLVM and gallium are about 68 MB and the Rust heap 3.7 MB. Idle CPU 0 ms over 5 s.
- 65 MB `.claude.json`, 4,000 servers: list drawn at about 240 ms; the heap peaks at 74 MB while the file is read, then drops to 8 MB. Frames under 9 ms (the first with 4,000 rows) and 0.3 ms after.
- With the real `claude` on PATH, background checks run one at a time: the tree peaks at 365 MB summed RSS (289 MB PSS) and the 30 checks end after about 20 s. Each `claude mcp get` takes about 0.5 s and 150 MB PSS.

## Next
More PRs from this review follow from this branch once this one is merged; the rest is in the findings file.
