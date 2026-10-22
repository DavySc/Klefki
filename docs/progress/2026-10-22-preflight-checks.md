# Pre-flight checks (debugging proposal P3)

Goal: proposal P3 of the 2026-10-21 debugging research (the owner chose the debugging proposals before roadmap item
5, in the order P3, P2, P1, P6, P5, P4). Say what in a server's definition keeps it from starting, before anything
runs: a program that is not there, a `${VAR}` nothing sets, `docker run` without `-i`, a `cmd /c` that Git Bash saved
as `cmd C:/`, an empty environment variable. Does not read Claude Code's log folder (P1, not approved yet).

## What Claude Code does (measured with Claude Code 2.1.308, Linux, throwaway `CLAUDE_CONFIG_DIR`)
- An unset `${VAR}` in `args` or `env` is passed to the server as written: the server printed
  `ARG=${KLEFKI_UNSET_X} ENV=${KLEFKI_UNSET_Y}`.
- `${VAR:-dflt}` with the variable unset passes `dflt`.
- A program that is only in a folder of the server's own `env.PATH` starts: Claude Code looks it up on the PATH the
  server is given.

## Done
- `crates/core/src/config/preflight.rs` (tests in `preflight/tests.rs`): `findings(entry, trusted, probe)`, five
  checks, each a sentence saying what is wrong and one saying what to do, sanitised and cut.
  - Not found: a bare name is looked up in the folders of the PATH (the server's own `env.PATH` when it sets one,
    else Klefki's), with `PATHEXT`'s endings on Windows and the executable bit elsewhere; a full path is looked at
    itself. Each name is looked up once per reading. Relative paths stay the relative-path hint's; a command with
    `$` in it is left to the unset-variable check.
  - Safety: only file metadata is read. A `.mcp.json` is untrusted, so for a project server only a bare name is
    looked up, in Klefki's own PATH; a full path or a PATH it sets is never looked at. Network paths (`\\host`,
    `//host`, `\\?\`, `\\.\`, `\??\`) are never looked at, in a command or a PATH folder, for any server.
  - Git Bash's `C:/` for `/c` after `cmd`: shares `paste::git_bash_drive` with paste, which already puts it back.
  - `docker`/`podman` `run` (or `container run`) with no `-i`, `--interactive` or short group with `i` after it.
  - `${NAME}` with no `:-default` and no such variable in Klefki's environment, in a stdio server's command, args
    and env, or a remote server's URL and headers. The text says it is unset where Klefki was started. A default
    (`${NAME:-value}`) is suggested only for a name that is not a secret's (`secretlike::sounds_secret`, or a
    word `PAT`), since it would be written in the file in plain text.
  - Environment variables that are empty or only spaces.
- `api::ServerRow::preflight: Vec<Preflight>` (`kind`, `problem`, `fix`). Filled by `inventory::load_probed` for
  user, local and project servers; `inventory::load` (preparing a change, export) looks nothing up. The engine passes
  its PATH and a new `EngineConfig::vars` (the process's variables; tests give their own).
- App: one badge beside the name, named after the first finding (`no program`, `Git Bash /c`, `missing -i`,
  `unset variable`; an empty variable gets no badge, as one is often meant), every problem on hover; the details'
  General tab lists each problem with its fix under "Before it can start".
  Not shown on a row Claude Code loads nowhere. "Not found" is not shown once the server started (Connected,
  tools failed, Needs auth): Claude Code found the program after all, on another PATH.
- New look picture `preflight-findings.png`; no other picture or golden file changed.

## Review (#93)
- A1: no `${NAME:-value}` suggestion for a secret's name (test with four secret names, one plain among secrets,
  and `PATTERN`, which is not `PAT`). A2: the NT forms `\??\` and `/??/` count as network paths (test cases).
- Folded in B1 to B4: "isn't set where Klefki was started", clearer badge labels ("program missing" did not fit
  beside the name in the 1100 px list and fell into "+1", so `no program`), no badge for an empty variable, a
  heading over the findings.
- Follow-up named by the review, not done: whether native-Windows Claude Code needs `cmd /c` for a program found only
  as `.cmd`/`.bat` (decision 003 found plain `npx` works); needs a check with real Claude Code on Windows.
- `secretlike::sounds_secret` does not know `PAT`; teaching it changes masking elsewhere, so it is left for its own
  slice.

## Not in this slice
- `klefki-cli`: its notes would change the `list` and `status` golden files; P9 (exit codes) is the natural place.
- macOS: Klefki started from Finder or the Dock gets launchd's short PATH, so its checks, and now this hint, miss
  programs a terminal finds (npx from Homebrew). The hint says where it looked; making Klefki read the login shell's
  PATH is its own decision.

## Checks
See the pull request description.

## Next
Pull request, cross-review, merge; then P2 (timeout explainer).
