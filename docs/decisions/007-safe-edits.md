# 007: Safe edits, move and copy of servers that sign in (2026-10-18)

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
- A local server is filed under the physical folder of the cwd (on Linux; a symlinked project key is not found
  again). On Linux the client secret is kept in plain text in `$CLAUDE_CONFIG_DIR/.credentials.json`: Klefki never
  writes it there or reads it, but handing the secret to `add-json` puts it there. That is Claude Code's choice.

## Decision
- **OAuth servers** (edit, move, copy) go through `claude mcp remove` then `claude mcp add-json --client-secret`, with
  Klefki's keyring copy of the secret passed only in `MCP_CLIENT_SECRET`. `-s` is always given; the cwd is the project
  folder for local, the home folder for user. A missing project folder, or (unix) a project key reached through a
  link, is refused at review and checked again before the first command runs, so nothing is removed for an add that
  cannot work.
- **The review shows what Claude Code writes**, not the whole definition: only the keys `add-json` keeps, with a
  warning naming the ones it drops (`alwaysAllow`, `cwd`, unknown keys). The recovery file keeps the whole definition.
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
- **A change that got through is not offered back:** a recovery file also records where the server was going; on
  listing, one whose server arrived there (and left where it was) is discarded. Covers Klefki stopping between the
  add and tidying up.
  It also covers a later change that brings the same about, such as a copy of the same definition to where a
  failed move was going: the server is where the user sent it, and Put back would bring it back where it was moved
  from. Only with its client secret (2026-10-19, found by model seed `19dc2de0`): when Klefki holds a copy where the
  server was, the file is discarded only once the same copy is where it went, and the one left behind is deleted
  first (a server put there later would claim it). Otherwise it stays listed: a copy from a server without a Klefki
  secret leaves Claude Code without one, and Put back is the only way left to hand it over. Klefki never reads
  Claude Code's credentials, so it cannot tell that case from stopping between the add and moving its own copy;
  that window (milliseconds) now leaves the definition offered too, which is safe: Put back or Forget.
- **A kept definition's place stays free** until it is put back or forgotten (2026-10-19): Klefki's copy of its
  client secret is filed under that place, and a copy, move, rename or add of a server there would replace or delete
  it (a copy without a Klefki secret deletes the one at its new place), or let that server claim it later (its Auth
  tab, a move away). Such a change is refused at review, and again just before Claude Code's command runs, with
  "Put it back or forget it on the Connections page first, or choose another name or place." Alternative not taken:
  keeping the kept secret under the recovery record's own key, so the place could be reused; it needs a second key
  scheme in the keyring and a migration, for a case the user can clear in one step.
- **The listing opens the system's store** when a change got through (2026-10-19): it was given only a test's store,
  so in the app it took every such change as done and never checked or moved Klefki's copy. A store that cannot be
  opened keeps the definition listed.
- **Forget waits for a shared sign-in** (2026-10-19, model seed `e896270e`): when Claude Code's remove of a kept
  server also took the client secret of a server elsewhere sharing its sign-in, Forget is refused (at review and
  again when applied) and says to put it back instead, which hands both their secret again. Forgetting it left the
  other server without a client secret in Claude Code, while Klefki held one it could not hand over. Not once the
  kept server is back where it was, or when nothing shares its sign-in any more (another address, say).
  Alternatives not taken: Forget re-adding the other server with its secret (more commands that can fail, and a
  second sign-in), or only warning (the server stays broken until fixed by hand).
- **Klefki's keyring copy** is moved (or a stale copy at the new place deleted) before the recovery file is
  discarded; a keyring failure after Claude Code succeeded is a note, not a failure.
- **Progress:** each of Claude Code's commands is announced (`Event::Applying`), and the dialog asks to keep Klefki
  open until it is done.
- **Journal replay with copies:** the pending verify stores copies already expanded to the definition, so a later
  move back into a place an earlier plan deleted is not undone when the copy's source is gone. Found by the model
  test (soak); docstore and machine regression tests. Likewise, a replay treats a server added by one plan and
  deleted or renamed by a later one as it ended, so the add does not bring it back under its old name.
- **A copy can change what overlapping servers differ in** (2026-10-20): the Duplicate dialog takes a new base
  address (the start the server's address and its sign-in metadata address share, up to a `/` and never less than
  the host; both are rewritten, a metadata address on another host or port is kept, and a base followed by more of
  the address may not end in `/` or hold `?` or `#`), callback port and client id
  (`adjust::Adjust`). The addresses come from the definition in the file (`Command::Inspect`), not from the list, which
  leaves out a query string. A copy with **another client id never gets the original's client secret**: it takes the
  one typed for the new client, or none (Claude Code then keeps none, and Klefki deletes a stale copy at the new
  place). With the same client it keeps the original's, as before. A plain server's copy is still one docstore write
  (`Op::CopyServer { base }`, rebased when expanded); one that signs in is still added by `add-json`. Checked against
  Claude Code 2.1.304: it takes the rewritten definition, `"oauth": {}` included, and keeps the new client's secret
  under the copy's own sign-in key. When the new host gets the original's header values or client secret, the review
  warns first, naming the host: a slip in it would hand them to a stranger.
- **Docstore keeps its OAuth refusal.** Only `Purpose::Preview` (diff for the review, never applied) may name OAuth
  entries.

## Acceptance: the model test
`crates/core/src/engine/machine.rs`, plain `proptest` over `Vec<Step>` (edit, move, copy, rename, delete, toggle, put
back, forget), each with a fault: works, fails before or after writing, times out before or after, never starts, or
an outside write overtakes it. The engine runs with an in-process stand-in for Claude Code (`Sim`, behind the
`swap::Claude` trait) that keeps credentials by the real sign-in key and drops unknown keys like 2.1.302. After every
step the whole world is read back and compared to the prediction: servers, kept definitions, Klefki's secrets and
Claude's credentials; no project-scope entry or `.mcp.json`; the rest of the file and its key order kept; no secret
in argv; refusals sanitised. Klefki's own keyring can fail to set or delete. A failure keeps every server in place or in a kept definition with
its secret, and Put back (with Claude Code working and the place free) and Forget must then succeed: a refusal
fails the test, except Forget while a server shares the kept one's sign-in (above). Failing
seeds are kept in `crates/core/proptest-regressions/`.

Why not as PLAN wrote it:
- `proptest-state-machine` was not needed: a plain strategy over steps shrinks just as well, and is one fewer dependency.
- Not against fake-claude: fake-claude cannot write the config (it is protected and never reads it), so it cannot
  play remove or add. The real `swap::Program` path runs against fake-claude in engine tests (argv, cwd, secret only
  in the environment, "still there", a hang stopped by the timeout), with the test writing Claude's part.
- No `get` faults: Klefki no longer runs `mcp get` to check a change.

## For human review
- The model test allows one state it otherwise forbids: a server sharing its sign-in with a kept definition (a copy
  under the same name) has no client secret in Claude Code until that definition is put back, because Claude Code's
  remove took both and only its add hands it back. The failure says so; the test requires that.
- Invariant touched: OAuth entries are now *removed* through `claude mcp remove` (before, Klefki never removed them).
  They are still written only through `add-json --client-secret`.
- Alternative not taken: letting the docstore remove OAuth entries would keep Claude's credentials (no sign-in again
  after a move), but it weakens a check, so it is proposed, not done.
- Proposal: a fake-claude mode that writes the config like the real one, so the model test can run end to end on it.
