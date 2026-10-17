//! Test helpers: build `fake-claude` once and write scenario files. Test-only.
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// The built `fake-claude` binary. The first call runs `cargo build -p fake-claude` and reads the artifact path
/// from cargo's JSON output, so `cargo test -p klefki-core` works without building the whole workspace first.
pub(crate) fn fake_claude() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let out = Command::new(cargo)
            .args(["build", "-p", "fake-claude", "--message-format=json"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .expect("run cargo to build fake-claude");
        assert!(out.status.success(), "building fake-claude failed");
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["reason"] == "compiler-artifact" && v["target"]["name"] == "fake-claude")
            .find_map(|v| v["executable"].as_str().map(PathBuf::from))
            .expect("cargo reported the fake-claude executable")
    })
}

/// Writes `scenario.json` (and any files it refers to) into `dir`; returns the scenario path.
pub(crate) fn write_scenario(dir: &Path, json: &str, files: &[(&str, &[u8])]) -> PathBuf {
    for (name, bytes) in files {
        std::fs::write(dir.join(name), bytes).unwrap();
    }
    let path = dir.join("scenario.json");
    std::fs::write(&path, json).unwrap();
    path
}

/// Is a process with this id still running?
pub(crate) fn pid_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let out = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .expect("run tasklist");
        String::from_utf8_lossy(&out.stdout).contains(&pid.to_string())
    }
    #[cfg(unix)]
    {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}
