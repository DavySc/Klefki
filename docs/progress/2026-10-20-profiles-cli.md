# Profiles, slice 3: `klefki-cli profiles` and `use` (2026-10-20)

Goal: roadmap item 4, profiles from `klefki-cli` (PLAN.md). The owner chose "list and use" on 2026-10-20: `use` may
change the configuration, through the same reviewed plan as the app. Decisions 006 and 012.

## Done
- CLI: `profiles [--json]` lists each profile, its servers and where they are the only ones on. Reads only.
- CLI: `use PROFILE [--project DIR | --all] [--only] [--yes]` prints the plan (summary, warnings, notes, each file's
  changes), asks `[y/N]` at a terminal, saves nothing without one unless `--yes` (exit 4), and applies through the
  headless engine (backup, Activity log). The profile is matched case aside with a suggestion; the project defaults
  to the current folder, matched as given and then with links resolved.
- Core: `api::load_profiles()`; `KLEFKI_DATA_DIR` names Klefki's own folder instead of the machine's (as
  `KLEFKI_MANAGED_DIR` does for the organisation's), used by the CLI tests.
- Core fix: the review of using a profile alone said servers already off there were turned off too. Reproducer
  `using_a_profile_alone_names_only_what_it_really_turns_off` (with the variant where nothing else is on) failed
  first. Slice 1's `using_a_profile_alone_turns_off_every_other_server_there` expected that wrong sentence (b is off
  in /q in its fixture); its expectation now leaves b out, and says why. Its switch assertions are unchanged.
- Docs: help text, decisions 006 and 012.

## Checks
- New tests: `profiles_lists_and_use_takes_one_profile_and_where` (args), and in `tests/cli.rs`
  `profiles_lists_each_profile_with_its_servers`, `use_shows_the_change_and_saves_it_only_when_told_to`,
  `use_without_a_project_uses_the_current_folders`, `use_names_a_profile_that_is_not_there_and_suggests_one`,
  `use_never_prints_a_secret_from_the_configuration`.
- Full verify exit 0 (with #58's review fixes merged in): core 899, app 407, cli 25 + 31, deny ok.
- By hand at a terminal (`script` pty, temp `CLAUDE_CONFIG_DIR` and `KLEFKI_DATA_DIR`): `n` saved nothing (exit
  4), `y` saved and named the backup (exit 0), a second run said "Nothing to change"; `profiles` then listed the
  project where each is on alone.
- Review of #59 (75aca1f): (1, blocking) `use` in a folder inside a git repository refused the repository's
  project, which Claude Code uses there; `use_in_a_folder_of_a_git_repository_uses_the_repositorys_project` (with
  the variant of the same folder before it is a git repository, still refused) failed first. (2) The question now
  names what is decided, e.g. "Use the profile Work in q, and turn off every other server there? [y/N]" (tried at a
  pty). (4) Usage errors of `use` show its own usage line (`use_says_what_it_asks_about_and_its_own_usage`, failed
  first). Left as follow-ups: (3) the diff repeats the JSON path per line (the app's review shares the format), (5)
  say why a profile is never on alone.

## Next
- Show on the Connections page which profiles a project is on alone in.
