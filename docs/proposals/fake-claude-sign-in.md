# Proposal: let fake-claude play `claude mcp login --no-browser`

**Status:** approved by Davy on 2026-10-18 (decision card in the sign-in thread) and applied as written.

**File:** `crates/fake-claude/src/main.rs` (protected). Needed by the sign-in slice (decision 005).

**Why.** The real `claude mcp login <name> --no-browser` needs a terminal on stdin (it exits 1 without one), prints a link, then waits for either the browser's callback or a pasted redirect address on stdin. Klefki's sign-in runs it in a pseudo-terminal and types the pasted address into it. Today fake-claude cannot read stdin and does not care whether it has a terminal, so these cannot be tested:
- Klefki really gives it a terminal (ConPTY on Windows, a pty elsewhere), on all three CI systems;
- exactly what Klefki types into the terminal (only the checked address and Enter, nothing else);
- the link is found in output that a pseudo-terminal has re-rendered (ConPTY redraws output and may wrap long lines).

**Change.** Three optional rule fields; existing scenarios behave as before.

```rust
    /// Like the real `mcp login`: exit 1 with Claude's message when stdin is not a terminal.
    require_tty: bool,
    /// After `stdout_file`, wait for one line on stdin and write it, without its line ending, to this path. Exits 1
    /// if stdin closes first.
    read_line_to: Option<String>,
    /// Written to stdout after that line was read.
    after_line_stdout_file: Option<String>,
```

In `main`, after the rule is found:

```rust
    if rule.require_tty && !std::io::stdin().is_terminal() {
        eprintln!("Couldn't complete authentication: stdin isn't a terminal, so authentication can't be completed here.");
        log("end", &argv);
        std::process::exit(1);
    }
```

and after `stdout_file` and `flood_bytes` are written and flushed:

```rust
    if let Some(to) = &rule.read_line_to {
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).unwrap_or(0) == 0 {
            log("end", &argv);
            std::process::exit(1);
        }
        let _ = std::fs::write(to, line.trim_end_matches(['\r', '\n']));
        if let Some(f) = &rule.after_line_stdout_file {
            let _ = out.write_all(&read_relative(&base, f));
            let _ = out.flush();
        }
    }
```

(`use std::io::IsTerminal;` is std, no new dependency.)

**Test side (not protected).** `cli/login.rs` tests and engine tests drive these: a scenario that prints a captured `--no-browser` transcript (decision 005) with `require_tty`, then `read_line_to`; the test pastes an address and checks the file holds exactly it. No test is weakened or skipped.
