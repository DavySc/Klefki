//! Spotting paths in a command definition that are relative to the folder the server is started in. Claude Code
//! starts a stdio server in the folder the session was started in (it ignores a `cwd` key), so for a user server,
//! which every project loads, `node build/index.js` works only in a folder that has `build/index.js`. A check run
//! from the home folder can pass where a session in a project fails. Only a hint; nothing here changes a value.
use super::secretlike;
use crate::sanitize::{clip, sanitize_line};
use serde_json::Value;

/// At most this many parts are named; the rest are not worth more room.
pub(crate) const MAX_SHOWN: usize = 5;
const MAX_CHARS: usize = 80;

/// The parts of a stdio definition that name a path relative to the folder the server is started in, as they are
/// shown: the command, arguments, and `value (in NAME)` for environment variables, cleaned and cut, at most
/// [`MAX_SHOWN`], each once. Arguments and variables that look like secrets are never named. Empty for anything
/// that is not a stdio definition.
pub(crate) fn relative_paths(entry: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let Some(obj) = entry.as_object() else {
        return out;
    };
    if !matches!(
        obj.get("type").and_then(Value::as_str),
        None | Some("stdio")
    ) {
        return out;
    }
    let Some(command) = obj.get("command").and_then(Value::as_str) else {
        return out;
    };
    let mut add = |part: &str| {
        let shown = clip(&sanitize_line(part), MAX_CHARS);
        if out.len() < MAX_SHOWN && !out.contains(&shown) {
            out.push(shown);
        }
    };
    // A bare program name is looked up on PATH; one with a folder in it is not.
    if command.contains(['/', '\\']) && !anchored(command) {
        add(command);
    }
    let args: Vec<&str> = obj
        .get("args")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    // `uv --directory /srv/weather run weather.py`: the rest is found from that folder, not from where it starts.
    if !folder_option(&args).is_some_and(anchored) {
        let package = downloaded_package(command, &args);
        let shown = secretlike::shown_args(args.iter().copied());
        for (i, (arg, shown)) in args.iter().zip(&shown).enumerate() {
            // What is hidden as a secret is never named.
            if Some(i) != package && *arg == shown && relative_arg(arg) {
                add(arg);
            }
        }
    }
    if let Some(Value::Object(env)) = obj.get("env") {
        for (name, value) in env {
            let Some(value) = value.as_str() else {
                continue;
            };
            if secretlike::sounds_secret(name) || secretlike::suggestion(name, value).is_some() {
                continue;
            }
            if relative_word(value) {
                add(&format!("{value} (in {name})"));
            }
        }
    }
    out
}

/// Programs that download and run a package named by their first word that is not an option (`npx -y chart.js`).
const RUNNERS: [&str; 4] = ["npx", "bunx", "pnpx", "uvx"];
/// Package managers that do the same after one of these words (`pnpm dlx x`, `bun x x`).
const MANAGERS: [&str; 3] = ["pnpm", "yarn", "bun"];

/// Where in `args` the package a runner downloads is named, if `command` is one: a package name, not a path.
fn downloaded_package(command: &str, args: &[&str]) -> Option<usize> {
    let program = command.rsplit(['/', '\\']).next().unwrap_or(command);
    let program = program
        .split_once('.')
        .map_or(program, |(stem, _)| stem)
        .to_ascii_lowercase();
    let mut words = args.iter().enumerate().filter(|(_, a)| !a.starts_with('-'));
    if RUNNERS.contains(&program.as_str()) {
        return words.next().map(|(i, _)| i);
    }
    if MANAGERS.contains(&program.as_str())
        && words.next().is_some_and(|(_, w)| matches!(*w, "dlx" | "x"))
    {
        return words.next().map(|(i, _)| i);
    }
    None
}

/// Options that set the folder a program works from, so that what follows is found from there.
const FOLDER_OPTIONS: [&str; 9] = [
    "--directory",
    "--dir",
    "--cwd",
    "--chdir",
    "--prefix",
    "--project",
    "--working-directory",
    "--workdir",
    "-C",
];

/// The value of the first folder option in `args` (`--directory /x` or `--directory=/x`), if any.
fn folder_option<'a>(args: &[&'a str]) -> Option<&'a str> {
    let mut words = args.iter();
    while let Some(arg) = words.next() {
        if FOLDER_OPTIONS.contains(arg) {
            return words.next().copied();
        }
        if let Some((option, value)) = arg.split_once('=')
            && FOLDER_OPTIONS.contains(&option)
        {
            return Some(value);
        }
    }
    None
}

/// Whether `word` does not depend on the folder it is read from: a full path (`/x`, `C:\\x`, `\\\\nas\\x`), one a
/// shell or Claude Code fills in (`~`, `$HOME`), an npm scope (`@x/y`), or anything with a colon in it (an address
/// `https://`, `github:o/r`, a drive, a port, an image tag), which is not a plain relative path. A word starting
/// with `./` or `../` is decided before this: a Docker volume `./data:/data` is read from the current folder.
fn anchored(word: &str) -> bool {
    word.starts_with(['/', '\\', '~', '@']) || word.contains(['$', ':'])
}

/// An argument that is a relative path, or an option whose value is one (`--config=./mcp.json`). A bare option
/// (`-y`, `--stdio`) is not.
fn relative_arg(arg: &str) -> bool {
    if arg.starts_with('-') {
        return arg
            .split_once('=')
            .is_some_and(|(_, value)| relative_word(value));
    }
    relative_word(arg)
}

/// File types a server's command line names as files. Only these: `mcp/fetch` (a container image), `owner/repo`
/// and `example.com` are not paths, so a word without a folder prefix counts only when it names such a file.
const FILE_TYPES: [&str; 37] = [
    "js", "mjs", "cjs", "ts", "mts", "cts", "jsx", "tsx", "py", "pyz", "pyc", "rb", "pl", "php",
    "sh", "bash", "zsh", "ps1", "bat", "cmd", "exe", "jar", "wasm", "lua", "json", "jsonc",
    "json5", "yaml", "yml", "toml", "ini", "cfg", "conf", "env", "db", "sqlite", "sqlite3",
];

/// Whether `word` is a path relative to the folder it is read from: `./x`, `..\\x`, or a file of a known type, alone
/// or in a folder (`server.py`, `build/index.js`). Not the folder itself or one above it (`.`, `./`, `../..`):
/// every folder has those, so on a user server they mean "the project the session is in". Not a pattern
/// (`**/*.ts`), which names no one file.
fn relative_word(word: &str) -> bool {
    if word.contains(['*', '?'])
        || word
            .split(['/', '\\'])
            .all(|part| matches!(part, "." | ".." | ""))
    {
        return false;
    }
    if ["./", "../", ".\\", "..\\"]
        .iter()
        .any(|prefix| word.starts_with(prefix))
    {
        return true;
    }
    if word.is_empty() || anchored(word) {
        return false;
    }
    let file = word.rsplit(['/', '\\']).next().unwrap_or(word);
    file.rsplit_once('.').is_some_and(|(_, kind)| {
        FILE_TYPES
            .iter()
            .any(|known| known.eq_ignore_ascii_case(kind))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn found(entry: Value) -> Vec<String> {
        relative_paths(&entry)
    }

    fn args(args: &[&str]) -> Vec<String> {
        found(json!({"command": "node", "args": args}))
    }

    #[test]
    fn a_script_or_folder_named_from_where_the_server_starts_is_found() {
        assert_eq!(args(&["build/index.js"]), ["build/index.js"]);
        assert_eq!(args(&["./server.py"]), ["./server.py"]);
        assert_eq!(args(&["..\\tools\\mcp.js"]), ["..\\tools\\mcp.js"]);
        assert_eq!(args(&["server.py"]), ["server.py"], "a bare file name too");
        assert_eq!(args(&["--config=./mcp.json"]), ["--config=./mcp.json"]);
        assert_eq!(
            args(&["dist/index.mjs", "data/db.sqlite"]),
            ["dist/index.mjs", "data/db.sqlite"]
        );
    }

    #[test]
    fn a_relative_command_is_found_and_a_program_found_on_path_is_not() {
        assert_eq!(found(json!({"command": "./bin/server"})), ["./bin/server"]);
        assert_eq!(
            found(json!({"command": "tools\\mcp.exe"})),
            ["tools\\mcp.exe"]
        );
        for command in ["npx", "node", "uvx", "python3.11", "server.exe"] {
            assert!(found(json!({"command": command})).is_empty(), "{command}");
        }
    }

    #[test]
    fn full_paths_packages_images_and_addresses_are_not_relative() {
        for arg in [
            "/opt/mcp/index.js",
            "C:\\Tools\\mcp.js",
            "c:/tools/mcp.js",
            "\\\\nas\\share\\mcp.js",
            "~/mcp/index.js",
            "${HOME}/mcp/index.js",
            "$HOME/mcp.js",
            "@modelcontextprotocol/server-filesystem",
            "@scope/pkg@1.2.3",
            "mcp/fetch",
            "ghcr.io/github/github-mcp-server",
            "owner/repo",
            "git+https://github.com/o/r.git",
            "https://example.com/a.json",
            "github:owner/repo",
            "file:./x.js",
            "mcp-server-fetch",
            "example.com",
            "-y",
            "--stdio",
            "--port=8080",
            "sentence-transformers/all-MiniLM-L6-v2",
            "text/plain",
        ] {
            assert!(args(&[arg]).is_empty(), "{arg}: {:?}", args(&[arg]));
        }
    }

    #[test]
    fn a_folder_option_with_a_full_path_makes_the_rest_relative_to_it() {
        // The MCP quickstart's own example: uv runs weather.py from the folder it is given.
        assert!(
            args(&["--directory", "/abs/weather", "run", "weather.py"]).is_empty(),
            "{:?}",
            args(&["--directory", "/abs/weather", "run", "weather.py"])
        );
        assert!(args(&["--cwd=C:/abs", "index.js"]).is_empty());
        assert!(args(&["-C", "/abs", "server.py"]).is_empty());
        // A folder option that is relative itself is the problem.
        assert_eq!(
            args(&["--directory", ".", "run", "weather.py"]),
            ["weather.py"]
        );
        assert_eq!(args(&["--directory", "./srv", "run", "x"]), ["./srv"]);
    }

    #[test]
    fn the_session_folder_itself_is_not_flagged_as_every_folder_has_it() {
        // Review B1: on a user server `.` is usually the point, "whatever project I am in".
        for words in [
            &["-y", "@modelcontextprotocol/server-filesystem", "."][..],
            &["-y", "@modelcontextprotocol/server-filesystem", "./"],
            &["mcp-server-git", "--repository", "."],
            &["--root", ".."],
            &["--root", "..\\"],
            &["--root=../.."],
            &[".\\"],
        ] {
            assert!(args(words).is_empty(), "{words:?}: {:?}", args(words));
        }
        let e =
            found(json!({"command": "python", "args": ["-m", "srv"], "env": {"PYTHONPATH": "."}}));
        assert!(e.is_empty(), "{e:?}");
        // What is in it, or next to it, is another matter.
        assert_eq!(args(&["./x", "../y/z.js"]), ["./x", "../y/z.js"]);
    }

    #[test]
    fn the_package_a_runner_downloads_is_not_a_path() {
        // Review N2: the first word after npx and the like names a package, even one called like a file.
        let ran = |command: &str, args: &[&str]| found(json!({"command": command, "args": args}));
        assert!(ran("npx", &["-y", "chart.js"]).is_empty());
        assert!(ran("npx.cmd", &["-y", "mcp-server.sh", "--stdio"]).is_empty());
        assert!(ran("bunx", &["tool.js"]).is_empty());
        assert!(ran("uvx", &["mcp.py"]).is_empty());
        assert!(ran("pnpm", &["dlx", "x.js"]).is_empty());
        assert!(ran("yarn", &["dlx", "x.js"]).is_empty());
        // Only that word: a file given to the package is still found from where it starts.
        assert_eq!(ran("npx", &["-y", "pkg", "./cfg.json"]), ["./cfg.json"]);
        assert_eq!(
            ran("node", &["index.js"]),
            ["index.js"],
            "node runs a file, not a package"
        );
        assert_eq!(ran("pnpm", &["run", "x.js"]), ["x.js"]);
    }

    #[test]
    fn patterns_are_not_files() {
        // Review N3.
        assert!(args(&["--glob", "**/*.ts", "--exclude", "*.json", "file?.py"]).is_empty());
    }

    #[test]
    fn environment_variables_holding_a_relative_path_are_found() {
        let e = found(json!({"command": "npx", "args": ["-y", "pkg"],
            "env": {"CONFIG": "./mcp.json", "DB": "data/app.db", "LEVEL": "debug", "ROOT": "/srv", "HOME_DIR": "~/x"}}));
        assert_eq!(e, ["./mcp.json (in CONFIG)", "data/app.db (in DB)"]);
    }

    #[test]
    fn secrets_are_never_named() {
        let e = found(
            json!({"command": "node", "args": ["--token", "ghp_abcdefghijklmnopqrstuvwxyz0123456789.json", "a.js"],
            "env": {"API_KEY": "./sk-abcdefghijklmnopqrstuvwxyz012345", "X": "./x.json"}}),
        );
        assert_eq!(e, ["a.js", "./x.json (in X)"], "{e:?}");
    }

    #[test]
    fn remote_and_broken_definitions_have_none() {
        assert!(found(json!({"type": "http", "url": "https://x/./a.js"})).is_empty());
        assert!(found(json!({"type": "sse", "url": "https://x/a.js"})).is_empty());
        assert!(found(json!({"args": ["a.js"]})).is_empty(), "no command");
        assert!(found(json!("node a.js")).is_empty());
        assert!(found(json!({"command": "node", "args": "a.js"})).is_empty());
        assert!(
            found(json!({"type": "stdio", "command": "node", "args": [1, null, "a.js"]}))
                == ["a.js"]
        );
    }

    #[test]
    fn many_parts_are_cut_named_once_and_cleaned() {
        let many: Vec<String> = (0..40).map(|i| format!("s{i}.js")).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        assert_eq!(args(&many).len(), MAX_SHOWN);
        assert_eq!(args(&["a.js", "a.js", "b.js"]), ["a.js", "b.js"]);
        let shown = args(&["./x\u{1b}[31m\u{202e}.js"]);
        assert_eq!(shown.len(), 1);
        assert!(
            !shown[0].contains('\u{1b}') && !shown[0].contains('\u{202e}'),
            "{shown:?}"
        );
        let long = format!("./{}.js", "a".repeat(500));
        assert!(args(&[&long])[0].chars().count() <= MAX_CHARS + 1);
    }
}
