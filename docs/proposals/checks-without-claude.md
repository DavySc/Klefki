# Checks without `claude mcp get` (proposal, 2026-10-25)

Proposal only; no code or invariant changed. The owner chose the hybrid on 2026-10-26; two decisions are still open (end).

## In short
Today every status check starts Claude Code (`claude mcp get`, or one `claude mcp list` for four or more servers in a
folder). Claude Code then starts the server, says hello in MCP and asks for its tools. Klefki could do that last part
itself for plain stdio servers: about 0.45 s and 196 MB less per check, measured below.

Recommended: a **hybrid**. Klefki starts a user or local stdio server itself, the way Claude Code would, and shows
"Connected" when it answers. Anything else still goes to Claude Code: every server Klefki does not find connected
(so a red or orange status is always Claude Code's own verdict), HTTP and SSE servers, sign-in, Diagnose, "Check
with Claude Code" on request, and on Windows a command that is a `.cmd` or `.bat`. Build it after a short Rust
spike that compares both verdicts on a set of test servers and on the owner's own configuration.

## What a check costs today
Measured 2026-10-25 on Linux (4 cores), Claude Code **2.1.310**, the native build (an ELF binary, not the npm
package the 2026-10-19 evaluation measured). A throwaway `HOME` and `CLAUDE_CONFIG_DIR`. Peak memory is the largest
single process of the run (`ru_maxrss`); when Claude Code and the server run together, both count.

| Run | Time | Peak memory |
|---|---|---|
| `claude mcp get` of a tiny Python stdio server | median 0.47 s (7 runs, 0.45-0.55 s; first run 0.92 s) | 196 MB (Claude Code) |
| `claude mcp get` of a real Node server (`@modelcontextprotocol/server-memory`, installed locally) | 0.64-0.74 s (5 runs) | 196 MB Claude Code **plus** 72 MB server |
| `claude mcp list` of 12 tiny servers, with Klefki's list settings | 0.60-0.70 s (3 runs) | 202 MB plus the servers |
| Same handshake done directly (spawn, `initialize`, `notifications/initialized`, `tools/list`, end), tiny server | 23-33 ms (5 runs) | 11 MB (the Python server and the Python test client) |
| Same, real Node server | 0.20-0.24 s (5 runs) | 72 MB (the server itself) |
| `claude --version` | 10 ms | 34 MB |

So the server's own start is the same either way, and Claude Code adds about 0.45 s and 196 MB per run. Done inside
Klefki, the client is one thread and two pipes. The 2026-10-19 figures (0.9 s, 237 MB) were the npm build; the
"27 s for 30 servers" there is already down to about 0.7 s by the `mcp list` batching that landed since.

What the gain is, then:
- **Memory peak:** about 200 MB less whenever a check runs (start-up, after every edit or switch, F5). This is the
  biggest effect, and it is why `BACKGROUND_CHECKS` is 1 today.
- **Latency of a single check** (after an edit, a switch, a sign-in, a click): about 0.45 s less on Linux.
  Windows is probably more (unmeasured; Node and antivirus start-up are slower there).
- **Fewer side effects:** each Claude Code run rewrites `policy-limits.json` and `remote-settings.json`, and writes a
  log under Claude Code's `mcp-logs-<server>/` that then shows as a "last start" in Klefki's own Status tab (a known
  leftover of P1). A direct check writes nothing.

## What Claude Code does when it starts a stdio server (measured, 2.1.310)
A server that wrote down its argv, folder, environment and every message it got:
- **Messages:** first `server/discover` (protocol `2026-07-28`, id `server-discover-probe-1`). With no usable answer
  it falls back to `initialize` with protocol `2025-11-25`, then `notifications/initialized`, then `tools/list`.
  Connected means `tools/list` answered; "Connected, tools failed" means it did not. A server that ignores
  `server/discover` costs 3.4 s the first time; Claude Code then remembers the answer per server in
  `<config>/state/mcp-discover-verdicts.json`, and later checks take 0.45 s.
- **Folder:** the folder Claude Code was started in (Klefki: the home folder for user servers, the project for local
  ones).
- **Environment**, in this order of who wins (later wins):
  1. Klefki's own environment (Klefki passes it to `claude`).
  2. The `env` of the settings files: the user's `settings.json`, the organisation's managed settings, and, **only
     when the project's trust dialog was accepted** (`hasTrustDialogAccepted`), the project's `.claude/settings.json`
     and `.claude/settings.local.json`. Untrusted, they never reached the server. Trusted, a real session
     (`claude -p`, one run) passed them on. `claude mcp get` did in one configuration and did not in another, on
     every run (the spike below). So for project settings, today's check is not a reliable picture of a session
     either. (P4 saw them never reach the server with 2.1.308.)
  3. Claude Code's own: `CLAUDECODE=1`, `CLAUDE_CODE_ENTRYPOINT`, `AI_AGENT`, `CLAUDE_PROJECT_DIR`,
     `CLAUDE_CODE_SESSION_ID` (a new id), `NoDefaultCurrentDirectoryInExePath=1`, `COREPACK_ENABLE_AUTO_PIN=0`,
     `LC_CTYPE`.
  4. The definition's `env`, with `${VAR}` and `${VAR:-default}` expanded. Expansion sees the settings files' `env`
     too: `${FROM_USER_SETTINGS}` and `${FROM_MANAGED}` in `args` were filled in. An unset `${VAR}` stays as
     written (P3).
- **Program lookup:** on the PATH the server is given, its own `env.PATH` included (P3).
- `claude mcp get` itself prints the definition's values unexpanded, and `${VAR:-default}` without its default.

## The guarantees today's check gives, and what a direct check keeps

| # | Today's check proves | Direct check | How |
|---|---|---|---|
| G1 | Claude Code picks the definition: a local server beats a user one of the same name, plugin duplicates, organisation servers | **Keeps** | The inventory already resolves this (`ServerRow::not_loaded`, decisions 008-011) and today refuses to check a row Claude Code does not load. The direct check uses the same row. |
| G2 | Policy (`allowedMcpServers`, `deniedMcpServers`) | **Keeps** | The inventory's blocked-by-policy (decision 010) already gates checks. |
| G3 | On or off where it runs (`disabledMcpServers`) | **Keeps** | Same: only enabled rows are checked. |
| G4 | A project server waiting for approval is not run | **Keeps** | Project servers stay check-on-request, approved only, full command shown first. |
| G5 | The environment Claude Code gives the server | **Recovers**, with care | Build it in core from the same four sources, in the order above. The spike agreed with Claude Code on all of them, and on project settings it matches a session better than `mcp get` does. `timeout.rs` already reads the same settings files (for `MCP_TIMEOUT`); it reads only one key today, the whole `env` object is new. The trust rule and Claude Code's own variables are measured facts that can change between versions. |
| G6 | The program is found the way Claude Code finds it | **Keeps on Linux and macOS**; **Windows open** | Unix: run the program name with the final PATH (Rust's `Command` searches the child's PATH when it is set). Windows: Claude Code runs a bare `npx` (decision 003), which is `npx.cmd`; a direct check would need `cmd.exe`. Proposed: hand `.cmd` and `.bat` to Claude Code until measured on Windows (see safety). |
| G7 | It starts in the right folder | **Keeps** | The folder Klefki already picks (`checks::where_to_run`). |
| G8 | The MCP handshake Claude Code does | **Keeps** | Send `initialize` (`2025-11-25`), `notifications/initialized`, `tools/list`. A server that speaks only the newer protocol refuses `initialize` (spike: Claude Code connected, the direct check failed); the hybrid then asks Claude Code. Slice 2 should send `server/discover` first, as Claude Code does, so that such a server is found Connected directly. |
| G9 | Claude Code's time limit (`MCP_TIMEOUT`, where it is set) | **Keeps** | `timeout::limit` already gives it; "Check again with 2 minutes" works the same. |
| G10 | Nothing it started outlives the check | **Keeps, stronger** | `cli/tree.rs` ends the whole process group or job object. Claude Code ends only the server, not what that started. |
| G11 | HTTP and SSE servers: reachable, signed in (OAuth tokens in Claude Code's store), `headersHelper` | **Loses** | Klefki never reads Claude Code's tokens and makes no network calls of its own outside updates. These stay with Claude Code. The planned network probe (roadmap 6) explains failures but gives no verdict. |
| G12 | Plugin servers, organisation servers, claude.ai connectors | **Nothing to lose** | Not checked in the background today either (only user and local rows are). |
| G13 | The installed Claude Code version can start it (new behaviour in a new version, new variables, new protocol) | **Loses for servers found Connected** | Partly recovered: a failure always goes to Claude Code; "Check with Claude Code", Diagnose and Diagnose in a project stay on Claude Code; the last starts (P1) show how real sessions went. A parity run per Claude Code version (in the spike, then by hand on new versions) catches drift. |
| G14 | A red or orange status is Claude Code's own word | **Keeps (hybrid only)** | Every answer other than Connected is re-checked with `claude mcp get`, and that answer is shown. Failures are rare, so this costs little. |

The one guarantee that changes in kind is G13: a green "Connected" would be "it answered when Klefki started it the
way Claude Code does" and no longer "Claude Code connected to it". The Status tab should say which ("Checked by
Klefki" / "Checked by Claude Code") so nobody has to guess.

## Options, ranked

1. **Hybrid: direct check first, Claude Code for everything else.** Direct for enabled, loaded user and local stdio
   servers (and approved project servers on request); Claude Code for every answer other than Connected, HTTP/SSE,
   `.cmd`/`.bat` on Windows, sign-in, Diagnose and on request. Keeps G1-G10 and G14, loses G11 (unchanged: those stay
   on Claude Code) and weakens G13 for green rows only. Gain: the common case (servers that work) never starts
   Claude Code. Cost: an MCP stdio client and an environment builder in core (L, two or three slices). The same
   client is what the tools view (roadmap 3, already approved in full) needs, so that work is shared.
2. **Remember verdicts across restarts.** Store each verdict with a fingerprint of what it was made for (definition,
   the resolved environment's hash, the program's file stamp, Claude Code's version). On start, show it as "checked N
   minutes ago" and skip checks while it is recent. Works with or without option 1, medium size, and on its own
   removes most start-up checks. Was item 2 of the 2026-10-19 evaluation; still not built.
3. **Use the last starts (P1) as a passive signal.** "Connected in a session 5 minutes ago" from Claude Code's own logs,
   with no check at all. Free and honest, but only about the past: no use after an edit, and Klefki's own checks
   write those logs today. A good companion to option 2 (a recent good start counts as recent), not a replacement.
4. **Direct check only, no confirmation by Claude Code.** Fastest and simplest, but a red status would then be
   Klefki's guess at Claude Code's behaviour. Not recommended.
5. **HTTP probe as a check.** It cannot see sign-in state without Claude Code's tokens, so it cannot say
   "Connected" or "Needs auth" the way Claude Code does. Keep it for explaining failures (roadmap 6).
6. **Tune what is there.** `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1` for checks saves about 0.1 s but stops
   `policy-limits.json` refreshing (still open from 2026-10-19). Small and does not touch the memory peak.

## Spike results (2026-10-26)
The owner chose the hybrid with a spike first. The spike was a scratch Rust program (not committed): it reads the
definition from `.claude.json`, builds the environment by the rules above, starts the command in its own process
group with no shell, sends `initialize`, `notifications/initialized` and `tools/list` with Claude Code's time limit,
caps each reply line at 1 MB, then ends the group. It ran next to `claude mcp get` (2.1.310, Linux) on 25 test
servers. `MCP_TIMEOUT` was 4000 in the user's settings so that the hanging server ends quickly.

| Server | `claude mcp get` | Direct | Agree |
|---|---|---|---|
| answers normally | Connected | Connected | yes |
| exits at once with an error | Failed (`CONNECTION_CLOSED`) | Failed, with its stderr line | yes |
| refuses `initialize` | Failed (`-32602`) | Failed (`-32602`) | yes |
| `tools/list` fails | Connected, tools failed | Degraded | yes |
| exits right after `initialize` | Connected, tools failed (`Connection closed`) | Degraded | yes |
| answers with an older protocol version | Connected | Connected | yes |
| prints a non-JSON line first | Connected | Connected | yes |
| sends a 3 MB line first | Connected | Connected (line skipped) | yes |
| starts a child that keeps running | Connected | Connected | yes |
| ignores `server/discover` | Connected (1.8 s) | Connected | yes |
| speaks only the newer protocol | Connected | **Failed** | no: hybrid asks Claude Code; fixed by sending `server/discover` |
| answers after 2 s | Connected | Connected | yes |
| never answers | Failed (timed out) | Timed out after 4 s | yes |
| needs a variable from the user's `settings.json` | Connected | Connected | yes |
| needs a variable from managed settings | Connected | Connected | yes |
| needs `CLAUDECODE=1` | Connected | Connected | yes |
| `${VAR}` from settings in `args` | Connected | Connected | yes |
| unset `${VAR}` stays as written | Connected | Connected | yes |
| `${VAR:-default}` | Connected | Connected | yes |
| `${HOME}` in its `env` | Connected | Connected | yes |
| program only on its own `env.PATH` | Connected | Connected | yes |
| program not found | Failed (`ENOENT`) | Failed (could not start) | yes |
| local server needing the project's `settings.json` `env`, project trusted | **Failed** | Connected | no: a session passes the value on, so the direct check is the right one |
| the same with `settings.local.json` | **Failed** | Connected | no: same |
| both of those, project not trusted | Failed | Failed | yes |

23 of 25 agree. The two that do not are the ones the direct check gets right and `claude mcp get` does not.
The newer-protocol one is handled by the hybrid until slice 2 sends `server/discover`.

Other things the spike showed:
- A child the server starts (`sleep 300` here) outlived a plain `claude mcp get` in 3 of 3 runs, and was ended by
  the direct check in 3 of 3. Klefki's runner already ends the group today, so this changes nothing for users.
- Time per check: 49-58 ms with the tiny server (`mcp get`: about 0.5 s), 0.21 s with the Node server (first run
  1.0 s, a cold disk), against 0.69-0.74 s for `mcp get`. Memory: the server's own (70 MB for the Node server); the
  spike program stayed under the 11 MB the measuring method itself costs. `mcp get` peaked at 199 MB on top of the
  server.
- Not done: the owner's own configuration, and Windows and macOS. Slice 3 can carry a hidden "compare both" mode for
  one run on the owner's machine, showing only the pairs of verdicts.

## Safety of Klefki starting server commands itself

What does not change: which servers get started, and when. Today Klefki already makes Claude Code start every
enabled user and local stdio server in the background, and project servers only on request. The direct check starts
exactly those, under exactly those rules. So a server's own side effects (writing files, starting a container,
downloading with `npx -y`) are the same as today. What changes is who builds the command and environment.

- **Running the wrong thing.** The new risk is a resolution bug: running a definition Claude Code would not (an
  unapproved project server, one that is off there, one replaced by another of the same name). Guard: the direct
  check takes the same gate as today's (`where_to_run`, `not_loaded`, enabled, scope), one test per refusal, and
  parity tests against Claude Code's verdicts.
- **No shell.** The command is run directly with its arguments as argv entries, as today's `claude` spawn is. Server
  names never reach a command line (the direct check does not need the name at all).
- **Windows.** `npx`, `pnpm` and the like are `.cmd` files; running one needs `cmd.exe`, the source of the
  argument-injection bugs this project fixed in v1. Rust's standard library now escapes arguments for `.cmd` and
  `.bat` and refuses those it cannot escape safely, but parity with Claude Code there is unmeasured. Proposed for the
  first slices: a command that resolves to `.cmd` or `.bat` is checked by Claude Code. Also, like Claude Code
  (`NoDefaultCurrentDirectoryInExePath=1`), never look for the program in the working folder: a repository could
  plant an `npx.exe` there.
- **Secrets in the environment.** A stdio server's `env` values are already plain text in `.claude.json`, and the
  settings files' `env` may hold tokens. They go only into the child's environment: never argv, never logs, never
  the Activity log, never shown. `MCP_CLIENT_SECRET` is never passed on (the runner already removes it). The child
  gets the same environment Claude Code would give it, no more.
- **Untrusted replies.** Every line the server writes is untrusted: a cap per line and per check (as
  `MAX_CHECK_OUTPUT`), parsed as JSON only, never shown raw. Only the verdict and a short sanitised reason are kept;
  stderr is sanitised and redacted with the server's own values, as the last starts do.
- **Ending it.** Close stdin, wait briefly, then end the whole tree (`cli/tree.rs`: process group on Unix, job object
  on Windows), also when the time limit runs out or Klefki exits. The existing signal and exit handlers already
  cover trees started through the runner.
- **Consent.** Today's background checks of user and local servers need no consent, and the direct check keeps that.
  A setting "Check servers with Claude Code only" turns direct checks off for anyone who prefers that.
- **Resources.** With Claude Code gone from each check, two or three can run at once; the cap stays, because a
  server can be heavy itself (a container, a JVM).
- **Network.** None by Klefki: a stdio check is pipes only.

### Invariant text that would change
CLAUDE.md today: "Claude is spawned with no shell, no `.cmd` or `.bat`, and names matching
`^[A-Za-z0-9_-]{1,64}$`." Decision 014 adds "the invariant that only Claude is ever spawned stands". Proposed new
line under Invariants (flagged for review), next to the existing one:

> - Besides Claude, Klefki starts a stdio server's own command, only to check it (and for the tools view): the
>   servers today's checks have Claude Code start, as the inventory resolves them (enabled, loaded, not blocked; a
>   project server only on request, approved, its full command shown first). No shell, never found in the working
>   folder; on Windows a `.cmd` or `.bat` is checked through Claude. Its environment is Claude Code's (Klefki's, the
>   settings files' `env`, the definition's with `${VAR}` expanded), passed only to the child, never in argv or logs.
>   Only `initialize`, `notifications/initialized` and `tools/list` are sent; replies are untrusted (size-limited,
>   sanitised). The whole process tree ends with the check. Any answer other than Connected is checked again with
>   `claude mcp get`, and that answer is shown.

Decision 014's sentence would get a note pointing at the new decision.

## Slice plan
1. **Spike (S). Done on Linux, 2026-10-26 (results above).** A scratch Rust client on `cli/tree.rs` and the environment rules above, not wired into the app.
   Run it next to `claude mcp get` on a matrix of test servers (connects, exits at once, bad `initialize`, tools
   fail, slow, hangs, stderr noise, needs a settings-file variable, needs its own `env.PATH`, `${VAR}` set and unset,
   project trusted and not) and on the owner's real configuration (verdict pairs only, no values). Record time and
   peak memory per check on Linux; ask the owner for one Windows run. Outcome: a verdict-agreement table and a
   go/no-go.
2. **Core client and environment (M).** `mcp/stdio.rs` (handshake, limits, timeout, tree end) and
   `config/server_env.rs` (settings `env` in Claude Code's order with the trust rule, Claude Code's own variables,
   `${VAR}` expansion shared with pre-flight). Tests from the measured facts; fixtures from 2.1.310. No app change.
3. **Hybrid in the engine (M).** Route eligible checks to the direct client, confirm non-Connected answers with
   `claude mcp get` (batched with `mcp list` as today), raise the cap for direct checks, the "Claude Code only"
   setting, "Checked by Klefki/Claude Code" on the Status tab, the CLAUDE.md line and a decision record. Look and
   golden tests.
4. **Remember verdicts (M)**, option 2, with the fingerprint above.
5. **Windows `.cmd`** after the owner's measurement; the tools view then reuses the client from slice 2.

## Decisions for the owner
1. Direction: **decided 2026-10-26: hybrid, spike first.**
2. The invariant line above (wording can change in slice 3; the principle is what needs approval).
3. Windows: hand `.cmd`/`.bat` commands to Claude Code at first (recommended), or include them from the start.

## How this was measured
Scratch scripts, not committed: a Python stdio server that logs argv, folder, environment and messages; a Python
client doing the handshake; a wrapper reading the child's peak memory with `resource.getrusage`. Managed settings were
put in `/etc/claude-code` for one run and removed straight after. Not measured: macOS, Windows, a real interactive
session (only `claude mcp get`), and servers fetched by `npx` from the registry (the test environment strips the
proxy, so `npx -y` hung for both paths; the server was installed locally instead).
