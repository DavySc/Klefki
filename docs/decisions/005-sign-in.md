# 005: Signing in to a server from Klefki (`claude mcp login --no-browser`)

Date: 2026-10-18. Claude Code 2.1.302, Linux. PLAN.md M2 asks to spike how `--no-browser` handles the OAuth callback port before building sign-in and re-authorize.

## Setup
- `spikes/sign-in/oauthsrv.py`: a local MCP server on `127.0.0.1:8765` that answers 401 with `resource_metadata`, and an authorization server on the same port (metadata, dynamic client registration, PKCE, `/authorize` that redirects at once, `/token`).
- `spikes/sign-in/ptyrun.py <seconds> <mode> -- claude mcp login <name> --no-browser`: runs the command in a pseudo-terminal, follows the printed sign-in link itself (as a browser would), then either calls the callback (`curl`), pastes the redirect URL back (`paste`), pastes a wrong one (`badpaste`, `garbage`), or presses Ctrl+C (`ctrlc`).
- `spikes/sign-in/piperun.py <mode> <name>`: the same with plain pipes instead of a terminal.
- Everything ran with a throwaway `HOME` and `CLAUDE_CONFIG_DIR`.

## Findings
1. **`--no-browser` still listens for the callback.** Claude binds `127.0.0.1:<port>` (checked in `/proc/net/tcp`; not `0.0.0.0`) and prints the link. When the browser runs on the same machine, signing in there completes the login with no paste. The paste prompt is only the fallback.
2. **The port.** Without `oauth.callbackPort`, the port is random per client registration (57878, 51326, 64868, … in different runs) and stays the same while that registration is kept. `claude mcp logout` drops the registration too, so the next login gets a new client id and port. With `--callback-port 8766`, the redirect is `http://localhost:8766/callback`.
3. **A fixed port that is taken fails at once**, exit 1: `Couldn't complete authentication for "fixed": OAuth callback port 8766 is already in use — another process may be holding it. Run lsof -ti:8766 -sTCP:LISTEN to find it.`
4. **A terminal is required.** With stdin as a pipe, Claude prints the link, then `stdin isn't a terminal, so authentication can't be completed here` and exits 1 in about 0.5 s; the callback port is closed by then. So Klefki needs a pseudo-terminal (ConPTY on Windows), as PLAN.md planned.
5. **The output** (colours on, even with `NO_COLOR` unset in the terminal):
   ```
   Starting authentication for "spike"…
   Visit this URL to authorize:
     ESC]8;;<link>BEL ESC[94m<link> ESC[39m ESC]8;;BEL
   Waiting for authorization… (^C to cancel)
   ESC[1G ESC[0J Or paste the redirect URL here: ESC[33G
   ```
   The link carries `response_type=code`, `client_id`, PKCE, `redirect_uri=http://localhost:<port>/callback`, `state` and `resource=<server URL>`. A stored client registration may also print an `[mcp-sdk] SEP-2352 …` notice line first.
6. **Results.** Success: `Authenticated with "spike". Its tools are now available in Claude Code.`, exit 0, and `claude mcp get` then says `Connected`. A paste with the wrong `state` or not a URL: `That doesn't look like a redirect URL for this sign-in — paste the full address from the browser page this sign-in opened.` and the prompt again (it keeps waiting). Ctrl+C: exit 130. A stdio server: `"stdiosrv" doesn't support OAuth login — it's only available for HTTP and SSE servers.`, exit 1. An unknown name: `No MCP server named "nope". Configured servers: …`, exit 1.
7. **Re-authorize is the same command.** `login` on a server that is already signed in runs the whole flow again (same client id, new tokens). No logout first.
8. Like `mcp get`, `login` finds a local server through the working directory: it must run in the project's folder.

## Decisions
- Sign-in runs `claude mcp login <name> --no-browser` in a pseudo-terminal (portable-pty), with the same working directory rules as a status check, no shell, `NO_COLOR=1`, and `MCP_CLIENT_SECRET` removed from its environment.
- Klefki opens the link itself, only after checking it: https, no user name or password in it, and `redirect_uri` on `localhost`, `127.0.0.1` or `[::1]` with a `state`. The app never sees a link it could open on its own; it asks the core to open the one it checked.
- The callback listener does the work in the common case, so the dialog says "finish signing in in your browser" and waits. The paste field is a fallback, folded away.
- A pasted address is checked before it is typed into the terminal: no control characters, the same scheme, host, port and path as the `redirect_uri`, and the same `state`. Nothing else is ever written to the terminal, so a paste cannot inject keystrokes.
- One sign-in at a time (each is a Node process of about 190 MB and a listening port). It ends after 5 minutes, on Cancel, or when Klefki closes; the process is killed.
- Done is exit 0, then a status check to confirm.
- The terminal transcript is not shown: it holds nothing the user needs beyond the result line, and the echoed paste holds the authorization code. Claude's last message is shown when it fails.
- Not done here: checking the link's host against the server's OAuth metadata (PLAN.md). That needs an HTTP client in the core, which does not exist yet (the planned `net/` module). The checks above already tie the link to this sign-in; the dialog names the host the browser was sent to.
