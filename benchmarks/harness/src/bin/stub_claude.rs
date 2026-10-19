//! A stand-in for `claude` for the end-to-end tests. It makes no network call and runs no model.
//!
//! Its own flags come first, then what `claude` would get (`-p <prompt> --model ...`):
//!
//! ```text
//! --record FILE           write the arguments and a few facts about the environment as JSON
//! --stream FILE           print FILE's lines to stdout, flushing each
//! --transcript FILE       copy FILE to $HOME/.claude/projects/-work/session-1.jsonl
//! --subagent FILE         copy FILE to .../session-1/subagents/agent-<n>.jsonl (repeatable)
//! --spend-forever TOKENS  print an assistant event of TOKENS output tokens every 20 ms, a new
//!                         message id each time, until killed (or 30 s, so a broken cap cannot hang)
//! --spend-subagent TOKENS append an assistant message of TOKENS output tokens to a subagent
//!                         transcript every 20 ms (the main stream stays silent), until killed or 30 s
//! --wait-for FILE        after the stream, block until FILE exists (30 s at most), then exit
//! --sleep SECS            print nothing and sleep (a run that has to be timed out)
//! --exit N                exit with N
//! ```

use std::io::Write;

fn main() {
    let mut args = std::env::args().skip(1).peekable();
    let mut record = None;
    let mut stream = None;
    let mut transcript = None;
    let mut subagents: Vec<String> = Vec::new();
    let mut spend = None;
    let mut sleep = None;
    let mut wait_for = None;
    let mut sub_spend = None;
    let mut exit = 0;
    let mut claude_args: Vec<String> = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--record" => record = args.next(),
            "--stream" => stream = args.next(),
            "--transcript" => transcript = args.next(),
            "--subagent" => subagents.extend(args.next()),
            "--spend-forever" => spend = args.next().and_then(|v| v.parse::<u64>().ok()),
            "--spend-subagent" => sub_spend = args.next().and_then(|v| v.parse::<u64>().ok()),
            "--wait-for" => wait_for = args.next(),
            "--sleep" => sleep = args.next().and_then(|v| v.parse::<u64>().ok()),
            "--exit" => exit = args.next().and_then(|v| v.parse().ok()).unwrap_or(1),
            _ => {
                claude_args.push(a);
                claude_args.extend(args.by_ref());
            }
        }
    }

    let home = std::env::var("HOME").unwrap_or_default();
    if let Some(path) = record {
        let env_names: Vec<String> = std::env::vars()
            .map(|(k, _)| k)
            .filter(|k| {
                let up = k.to_ascii_uppercase();
                up.starts_with("CLAUDE") || up.starts_with("LUMEN_") || up.starts_with("ANTHROPIC")
            })
            .collect();
        let doc = serde_json::json!({
            "args": claude_args,
            "home": home,
            "cwd": std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default(),
            "env_names": env_names,
            "env": std::env::vars().filter(|(k, _)| {
                matches!(k.as_str(), "DISABLE_AUTOUPDATER" | "DO_NOT_TRACK" | "CAVEMAN_DEFAULT_MODE" | "LUMEN_UPDATE_CHECK")
            }).collect::<std::collections::BTreeMap<_, _>>(),
        });
        std::fs::write(path, doc.to_string()).expect("write the record");
    }

    let projects = std::path::Path::new(&home)
        .join(".claude")
        .join("projects")
        .join("-work");
    if let Some(t) = transcript {
        std::fs::create_dir_all(&projects).expect("create the projects dir");
        std::fs::copy(t, projects.join("session-1.jsonl")).expect("copy the transcript");
    }
    for (i, s) in subagents.iter().enumerate() {
        let dir = projects.join("session-1").join("subagents");
        std::fs::create_dir_all(&dir).expect("create the subagents dir");
        std::fs::copy(s, dir.join(format!("agent-a{}.jsonl", i + 1))).expect("copy a subagent");
    }

    let out = std::io::stdout();
    if let Some(path) = stream {
        let text = std::fs::read_to_string(path).expect("read the stream fixture");
        let mut o = out.lock();
        for line in text.lines() {
            writeln!(o, "{line}").expect("write");
            o.flush().expect("flush");
        }
    }
    if let Some(tokens) = spend {
        let started = std::time::Instant::now();
        let mut n = 0u64;
        while started.elapsed().as_secs() < 30 {
            n += 1;
            let ev = serde_json::json!({
                "type": "assistant",
                "message": {
                    "id": format!("msg_spend_{n}"),
                    "role": "assistant",
                    "model": "claude-sonnet-5-5",
                    "usage": { "input_tokens": 0, "output_tokens": tokens,
                               "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0 }
                }
            });
            let mut o = out.lock();
            if writeln!(o, "{ev}").and_then(|_| o.flush()).is_err() {
                return;
            }
            drop(o);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    if let Some(tokens) = sub_spend {
        let dir = projects.join("session-1").join("subagents");
        std::fs::create_dir_all(&dir).expect("create the subagents dir");
        let path = dir.join("agent-spend.jsonl");
        let started = std::time::Instant::now();
        let mut n = 0u64;
        while started.elapsed().as_secs() < 30 {
            n += 1;
            let ev = serde_json::json!({
                "type": "assistant",
                "message": {
                    "id": format!("msg_sub_{n}"), "role": "assistant", "model": "claude-sonnet-5-5",
                    "usage": { "input_tokens": 0, "output_tokens": tokens,
                               "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0 }
                }
            });
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .expect("open the subagent transcript");
            writeln!(f, "{ev}").expect("write");
            drop(f);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }
    if let Some(path) = wait_for {
        let started = std::time::Instant::now();
        while !std::path::Path::new(&path).exists() && started.elapsed().as_secs() < 30 {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    if let Some(s) = sleep {
        std::thread::sleep(std::time::Duration::from_secs(s));
    }
    std::process::exit(exit);
}
