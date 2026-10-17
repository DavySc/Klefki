# Proposal: write each fake-claude log line with one `write` call

**File:** `crates/fake-claude/src/main.rs`, `fn log` (protected).

**Problem.** `writeln!(f, "{line}")` on an unbuffered `File` issues several `write` syscalls (the text, then the newline).
When engine workers start several stub processes at once, appends from different processes interleave inside a line.
Observed on Linux, reproducible 3 of 3 runs of `engine::tests::at_most_three_checks_run_at_once`:

    bad log line "{\"argv\":[\"mcp\",\"get\",\"s2\"],\"cwd{\":\"\"/tmp/.tmpK9aRQ8/home\"argv,\"\":event\"[...

The test is not wrong; the stub corrupts its own log.

**Change.** Build the whole line, including `\n`, and write it with a single `write_all` on a file opened with `append(true)`.
A single `O_APPEND` write of a small buffer is atomic with respect to other appenders on Linux/macOS, and on Windows
append writes are serialised by the OS for a single call.

```rust
let mut buf = line.to_string();
buf.push('\n');
if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
    let _ = f.write_all(buf.as_bytes());
}
```

**Test side (not protected, already done).** `gets()` in `engine.rs` now names the offending line when parsing fails.
No test is weakened or skipped.
