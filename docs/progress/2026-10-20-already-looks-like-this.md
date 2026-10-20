# "The configuration already looks like this" (2026-10-20)

Goal: the owner saw "The configuration already looks like this" for several profiles in one project and found it
confusing. Find the cause and fix it.

## Cause
- "Use profile…" without "Only these servers there" only turns the profile's servers on. Servers are on unless
  switched off, so for almost any profile it writes nothing, and the review opened anyway with a heading saying what
  would happen, the bare sentence "The configuration already looks like this", and a disabled button. Not a stale
  comparison: the plan was right, the way it was said was not.
- Found while reproducing: with "only", "This also turns off X" named servers that were off already (the card or
  review then said servers were turned off by a change that wrote nothing). An existing test,
  `using_a_profile_alone_turns_off_every_other_server_there`, asserted that wrong sentence (b is off in its fixture);
  its assertion now expects only the servers that are on.

## Done
- Core: `PlanView::already`, one sentence per change that writes nothing, naming the servers and where ("loc is
  already on in a.", "The servers of Work, gw and loc, are already on in a. Other servers there are left as they
  are.", "... already the only ones on in b."), members in the profile's order. `Snapshot::switched_off` (each
  project's `disabledMcpServers`) so "This also turns off" names only servers that are on now.
- App: a profile's Use that would write nothing opens no review: the card says "Nothing to change", what is already
  so, what still keeps a server from loading, and, unticked, how to turn the others off. Cleared when the place or
  "only" changes. If the page was left before the answer, the review says it. Any other review that changes nothing
  names what is already so.
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

## Open
- Asked the owner whether "Only these servers there" should be ticked by default (recommended: yes).
- Overlaps PR #58 (profiles palette), which also changes the profile card in `crates/app/src/profiles.rs`; whichever
  lands second merges main in.
- Toolbar Turn on/Turn off stay enabled for servers already in that state; their review now names what is already so.
