//! Test-only stand-in for the `claude` CLI (see docs/proposals/fake-claude.md).
//!
//! Behaviour comes entirely from the scenario file named by `FAKE_CLAUDE_SCENARIO`. It never reads the
//! real Claude configuration (`HOME`, `CLAUDE_CONFIG_DIR`) and never touches the network.
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Deserialize)]
struct Scenario {
    rules: Vec<Rule>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Rule {
    /// The rule applies when the real arguments start with these.
    args: Vec<String>,
    stdout_file: Option<String>,
    stderr_file: Option<String>,
    exit_code: i32,
    delay_ms: u64,
    /// Sleep until killed, to test timeouts.
    hang: bool,
    /// Write this many bytes to stdout, to test the output cap.
    flood_bytes: usize,
    /// Write our process id here, so a test can check the child really died.
    write_pid_to: Option<String>,
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn log(event: &str, argv: &[String]) {
    let Some(path) = std::env::var_os("FAKE_CLAUDE_LOG") else {
        return;
    };
    let secret = std::env::var("MCP_CLIENT_SECRET").ok();
    let line = serde_json::json!({
        "event": event,
        "argv": argv,
        "cwd": std::env::current_dir().ok().map(|p| p.display().to_string()),
        "secret_set": secret.is_some(),
        // The value itself is never written, only its hash, so tests can compare without leaking it.
        "secret_sha256": secret.map(|s| format!("{:x}", Sha256::digest(s.as_bytes()))),
        "no_color_set": std::env::var_os("NO_COLOR").is_some(),
        "ms": now_ms() as u64,
    });
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

fn read_relative(base: &Path, name: &str) -> Vec<u8> {
    std::fs::read(base.join(name)).unwrap_or_else(|e| {
        eprintln!("fake-claude: cannot read {name}: {e}");
        std::process::exit(3)
    })
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let Some(scenario_path) = std::env::var_os("FAKE_CLAUDE_SCENARIO").map(PathBuf::from) else {
        eprintln!("fake-claude: FAKE_CLAUDE_SCENARIO is not set; refusing to run");
        std::process::exit(2);
    };
    let base = scenario_path
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let scenario: Scenario = std::fs::read(&scenario_path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| {
            eprintln!("fake-claude: cannot read the scenario file");
            std::process::exit(2)
        });

    log("start", &argv);
    let Some(rule) = scenario
        .rules
        .iter()
        .find(|r| argv.len() >= r.args.len() && argv[..r.args.len()] == r.args[..])
    else {
        eprintln!("fake-claude: no rule matches {argv:?}");
        log("end", &argv);
        std::process::exit(2);
    };

    if let Some(p) = &rule.write_pid_to {
        let _ = std::fs::write(p, std::process::id().to_string());
    }
    if rule.delay_ms > 0 {
        std::thread::sleep(Duration::from_millis(rule.delay_ms));
    }
    if rule.hang {
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    let mut out = std::io::stdout().lock();
    if let Some(f) = &rule.stdout_file {
        let _ = out.write_all(&read_relative(&base, f));
    }
    if rule.flood_bytes > 0 {
        let chunk = [b'x'; 8192];
        let mut left = rule.flood_bytes;
        while left > 0 {
            let n = left.min(chunk.len());
            if out.write_all(&chunk[..n]).is_err() {
                break; // the reader stopped listening; fine
            }
            left -= n;
        }
    }
    let _ = out.flush();
    if let Some(f) = &rule.stderr_file {
        let _ = std::io::stderr().write_all(&read_relative(&base, f));
    }
    log("end", &argv);
    std::process::exit(rule.exit_code);
}
