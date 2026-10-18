# 006: Safe edits, move and copy of servers that sign in (2026-10-18)

## Context
Klefki's docstore refuses to write any entry with `oauth`: Claude Code keeps such a server's client secret and tokens
outside `~/.claude.json`, and only its own commands keep them in step. v1.0.0 changed servers by remove-then-add with
no rollback, and lost servers when the add failed. M2 asks for edits of OAuth servers, move and copy between the user
and local scope, and PLAN's model-based test as the acceptance bar.

## What Claude Code 2.1.302 does (spike, temp `HOME` and `CLAUDE_CONFIG_DIR`)
- The client secret and tokens are stored under `name|sha256(JSON{type,url,headers||{}})[:16]` (the `md` function in
  the binary). The key does not include the scope, so a user and a local server with the same name, type, address and
  headers share one secret and one sign-in.
- `claude mcp remove -s X -- name` deletes that key's credentials, so it also signs out the other server sharing it.
- `claude mcp add-json` without `--client-secret` leaves stored credentials alone; with it, it replaces the secret.
- `add-json` writes only the keys it knows: `alwaysAllow`, `cwd` on stdio and unknown keys are dropped; `timeout`
  is kept.
- `claude mcp remove` without `-s` removes the user server silently; `-s local` takes the project from the cwd.

## Decision
- **OAuth servers** (edit, move, copy) go through `claude mcp remove` then `claude mcp add-json --client-secret`, with
  Klefki's keyring copy of the secret passed only in `MCP_CLIENT_SECRET`. `-s` is always given; the cwd is the project
  folder for local, the home folder for user. A missing project folder is refused before Claude Code runs.
- **Rollback:** before the remove, the old definition goes to a private recovery file next to the backups
  (`recovery/<ms>-<n>.json`, private folder and file). If the add fails, the old definition is added back with the
  same secret, and the recovery file is deleted once it is back. If putting it back fails too, the file stays: the
  Connections page shows "N servers could not be put back" with Put back (`Change::PutBack`) and Forget
  (`Change::ForgetKept`, destructive, reviewed). Klefki's keyring copy is only moved or deleted after success.
- **What counts is the file:** after each command Klefki re-reads `~/.claude.json` instead of trusting the exit code
  or running `mcp get`. A remove that exits 0 with the entry still there fails ("still there", nothing changed); an
  add that times out but wrote the entry succeeds. Settings Claude Code dropped are named in the Applied notes.
- **Plan-time review** refuses a change that would replace another server's secret (a new sign-in key already in use
  elsewhere), and warns that the server is signed out (and that a server sharing its key is too). A server with no
  Klefki copy of its secret is warned about. An unchanged OAuth edit runs nothing.
- **Non-OAuth move** is one atomic docstore plan (`CopyServer` + `RemoveServer`): nothing is removed if the copy
  cannot be written.
- **Settling the journal:** before Claude Code's commands run, Klefki's own pending verify for `.claude.json` is
  replayed now and retired; otherwise a late replay of an earlier plan could undo what Claude Code just wrote. Found
  by the model test; two regression tests.
- **Docstore keeps its OAuth refusal.** Only `Purpose::Preview` (diff for the review, never applied) may name OAuth
  entries.

## Acceptance: the model test
`crates/core/src/engine/machine.rs`, plain `proptest` over `Vec<Step>` (edit, move, copy, rename, delete, toggle, put
back, forget), each with a fault: works, fails before or after writing, times out before or after, never starts, or
an outside write overtakes it. The engine runs with an in-process stand-in for Claude Code (`Sim`, behind the
`swap::Claude` trait) that keeps credentials by the real sign-in key and drops unknown keys like 2.1.302. After every
step the whole world is read back and compared to the prediction: servers, kept definitions, Klefki's secrets and
Claude's credentials; no project-scope entry or `.mcp.json`; the rest of the file and its key order kept; no secret
in argv; refusals sanitised. A failure keeps every server in place or in a kept definition with its secret. Failing
seeds are kept in `crates/core/proptest-regressions/`.

Why not as PLAN wrote it:
- `proptest-state-machine` was not needed: a plain strategy over steps shrinks just as well, and is one fewer dependency.
- Not against fake-claude: fake-claude cannot write the config (it is protected and never reads it), so it cannot
  play remove or add. The real `swap::Program` path runs against fake-claude in engine tests (argv, cwd, secret only
  in the environment, "still there", a hang stopped by the timeout), with the test writing Claude's part.
- No `get` faults: Klefki no longer runs `mcp get` to check a change.

## For human review
- Invariant touched: OAuth entries are now *removed* through `claude mcp remove` (before, Klefki never removed them).
  They are still written only through `add-json --client-secret`.
- Alternative not taken: letting the docstore remove OAuth entries would keep Claude's credentials (no sign-in again
  after a move), but it weakens a check, so it is proposed, not done.
- Proposal: a fake-claude mode that writes the config like the real one, so the model test can run end to end on it.
