# "The configuration already looks like this" (2026-10-20)

Goal: the owner saw "The configuration already looks like this" for several profiles in one project and found it
confusing. Find the cause and fix it.

## Cause
- "Use profile…" without "Only these servers there" only turns the profile's servers on. Servers are on unless
  switched off, so for almost any profile it writes nothing, and the review opened anyway with a heading saying what
  would happen, the bare sentence "The configuration already looks like this", and a disabled button. Not a stale
  comparison: the plan was right, the way it was said was not.
- Found while reproducing: with "only", "This also turns off X" named servers that were off already. #59 fixed the
  same on main (`off_now`) while this was in review; this branch now uses main's fix and drops its own.

## Done
- Core: `PlanView::already`, one sentence per change that writes nothing, naming the servers and where ("loc is
  already on in a.", "The servers of Work, gw and loc, are already on in a. Other servers there are left as they
  are.", "... already the only ones on in b."), members in the profile's order. Where none of its servers loads there,
  why ("None of the servers of Local loads in b ...", with the notes that say where they load). `PlanView::others_on`:
  used without "only" where other servers are on, so "only" would change something.
- App: a profile's Use that would write nothing opens no review: the card says "Nothing to change", what is already
  so, what still keeps a server from loading, and, only when other servers are on, how to turn them off. A polite live
  region, brought into view once. Not repeated when #58's "Only these servers are on in …" line already says it.
  Cleared when the place or "only" changes, and on every new snapshot (it was true of the files then). If the page was left before the answer, the review says it. Any other review that changes nothing
  names what is already so.
- CLI: `klefki-cli use` says the same lines instead of "already looks like this", and `--only` when it would help.
- Model test: a switch that writes nothing must say what is already so (checked to fail with the sentence removed).
- Decision 012 updated.

## Checks
- Reproducers first: `a_profile_already_in_effect_says_which_servers_are_on_where`,
  `a_change_that_is_already_true_is_reported_as_nothing_to_do` (extended) and `only_what_is_on_now_is_said_to_be_turned_off`
  failed on main d18cd96, pass now. App: `using_a_profile_that_changes_nothing_says_so_on_its_card`,
  `a_profile_s_no_op_after_the_page_is_left_is_said_in_the_review`, `only_a_profile_s_no_op_is_said_on_its_card`,
  `a_plan_that_changes_nothing_says_what_is_already_so`.
- Real app under Xvfb (temp HOME and CLAUDE_CONFIG_DIR, Claude Code 2.1.304): profile Writing (docs, memory) used in
  app unticked: card says they are already on; ticked: review turns off github and lint; used again ticked: card says
  they are already the only ones on, with no "turns off" warning.
- Full verify on d18cd96 + this branch: fmt, clippy -D warnings, `cargo test --workspace` (core 894, app 405, cli 26
  and 24 passed), `cargo deny check` (advisories, bans, licenses, sources ok): exit 0.

## Cross-review round 1 (Opus thread, on b68c3de)
- Blocking, fixed: conflict with #58/#59 (merged main 37cb4a4, then 42777f8); stale "already on" after another change
  (reproducer `what_a_profile_found_already_so_goes_when_the_files_change` failed without the fix); no reason when none
  of a profile's servers loads there, and a wrong "tick Only" hint (`the_hint_to_tick_only_is_given_only_when_other_
  servers_are_on`, engine test `Local` case); said twice next to #58's line (`a_profile_on_alone_is_not_said_twice`,
  failed without the fix).
- Non-blocking, done: live region and scroll into view.
- Re-verified with main 42777f8 merged: fmt, clippy -D warnings, `cargo test --workspace` (core 900, app 414, cli 25
  and 33), `cargo deny check`: exit 0. Real app: the message stays put for 15 s with no change; ticked where it is on
  alone, only "Nothing to change" shows under "Only these servers are on in app."

## Open
- Asked the owner whether "Only these servers there" should be ticked by default (recommended: yes).
- Toolbar Turn on/Turn off stay enabled for servers already in that state; their review now names what is already so.
