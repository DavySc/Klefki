//! Throwaway spike: run a program in a ConPTY, feed it scripted keystrokes and print the rendered screen.
//! usage: pty-drive <cwd> <program> [--env K=V]... -- step...
//!   steps: sleep=MS | send=TEXT (\r \n \x1b \t escapes) | dump | wait=TEXT:MS (wait until TEXT is on screen)
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn unescape(s: &str) -> String {
    s.replace("\\r", "\r")
        .replace("\\n", "\n")
        .replace("\\t", "\t")
        .replace("\\x1b", "\x1b")
        .replace("\\x03", "\x03")
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cwd = &args[0];
    let program = &args[1];
    let mut envs = Vec::new();
    let mut i = 2;
    while i < args.len() && args[i] != "--" {
        if args[i] == "--env" {
            let kv = &args[i + 1];
            let (k, v) = kv.split_once('=').unwrap();
            envs.push((k.to_owned(), v.to_owned()));
            i += 2;
        } else {
            i += 1;
        }
    }
    let steps = &args[(i + 1).min(args.len())..];

    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize { rows: 40, cols: 120, pixel_width: 0, pixel_height: 0 })
        .unwrap();
    let mut cmd = CommandBuilder::new(program);
    cmd.cwd(cwd);
    for (k, v) in &envs {
        cmd.env(k, v);
    }
    cmd.env("TERM", "xterm-256color");
    let mut child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave);

    let parser = Arc::new(Mutex::new(vt100::Parser::new(40, 120, 0)));
    let mut reader = pair.master.try_clone_reader().unwrap();
    let p2 = parser.clone();
    let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(pair.master.take_writer().unwrap()));
    let w2 = writer.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            let chunk = &buf[..n];
            // ConPTY waits for answers to these terminal queries before it draws anything.
            for (query, reply) in [(&b"[6n"[..], &b"[1;1R"[..]), (&b"[c"[..], &b"[?1;2c"[..])] {
                if chunk.windows(query.len()).any(|w| w == query) {
                    let mut w = w2.lock().unwrap();
                    let _ = w.write_all(reply);
                    let _ = w.flush();
                }
            }
            p2.lock().unwrap().process(chunk);
        }
    });

    let screen = |p: &Arc<Mutex<vt100::Parser>>| -> String {
        let g = p.lock().unwrap();
        let s = g.screen().contents();
        s.lines().map(|l| l.trim_end()).collect::<Vec<_>>().join("\n").trim_end().to_owned()
    };

    for step in steps {
        if let Some(ms) = step.strip_prefix("sleep=") {
            std::thread::sleep(Duration::from_millis(ms.parse().unwrap()));
        } else if let Some(t) = step.strip_prefix("send=") {
            let mut w = writer.lock().unwrap();
            w.write_all(unescape(t).as_bytes()).unwrap();
            w.flush().unwrap();
        } else if step == "dump" {
            println!("----- screen -----\n{}\n------------------", screen(&parser));
        } else if let Some(w) = step.strip_prefix("wait=") {
            let (text, ms) = w.rsplit_once(':').unwrap();
            let deadline = Instant::now() + Duration::from_millis(ms.parse().unwrap());
            let mut ok = false;
            while Instant::now() < deadline {
                if screen(&parser).contains(text) {
                    ok = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            println!("[wait for {text:?}: {}]", if ok { "found" } else { "TIMEOUT" });
        }
    }
    let _ = child.kill();
    std::process::exit(0);
}
