//! `difftower-agent serve <path>`: serves a repository, or a directory of
//! repositories, to Difftower over
//! stdin and stdout. Difftower installs and starts it on remote hosts
//! through SSH; it exits when the connection closes.

use std::{
    any::Any,
    collections::HashMap,
    io::{BufRead, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
};

use anyhow::{Result, bail};
use base64::Engine as _;
use difftower_core::{
    Backend, open_path,
    protocol::{Call, Message, PROTOCOL, Request},
};
use serde_json::Value;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = match args.as_slice() {
        [cmd, path] if cmd == "serve" => expand_home(path),
        _ => {
            eprintln!("usage: difftower-agent serve <path>");
            std::process::exit(2);
        }
    };
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let backend: Arc<dyn Backend> = match open_path(&path) {
        Ok(backend) => backend.into(),
        Err(err) => {
            send(&out, &Message::Fatal { error: format!("{err:#}") });
            std::process::exit(1);
        }
    };
    send(&out, &Message::Hello { protocol: PROTOCOL, root: backend.root(), repos: backend.repos() });

    let changed_out = out.clone();
    let _watch = backend.watch(Box::new(move || send(&changed_out, &Message::Changed)));

    // Test runs in progress; dropping one stops it.
    let runs: Arc<Mutex<HashMap<u64, Box<dyn Any + Send>>>> = Arc::default();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let call: Call = match serde_json::from_str(&line) {
            Ok(call) => call,
            Err(err) => {
                eprintln!("difftower-agent: bad request: {err}");
                continue;
            }
        };
        let id = call.id;
        match call.request {
            // Listing tests takes seconds; other requests go on meanwhile.
            Request::Tests => {
                let (backend, out) = (backend.clone(), out.clone());
                std::thread::spawn(move || {
                    let result = backend.tests().map_err(|err| format!("{err:#}"));
                    let result = result.and_then(|suites| serde_json::to_value(suites).map_err(|err| err.to_string()));
                    send(&out, &Message::Reply { id, result });
                });
            }
            Request::RunTests { run, repo, runner, target } => {
                let (events_out, finished_runs) = (out.clone(), runs.clone());
                let on_event = Box::new(move |event: difftower_core::TestEvent| {
                    let finished = matches!(event, difftower_core::TestEvent::Finished { .. });
                    send(&events_out, &Message::Test { run, event });
                    if finished {
                        finished_runs.lock().unwrap().remove(&run);
                    }
                });
                let result = match backend.run_tests(&repo, runner, target, on_event) {
                    Ok(handle) => {
                        runs.lock().unwrap().insert(run, handle);
                        Ok(Value::Null)
                    }
                    Err(err) => Err(format!("{err:#}")),
                };
                send(&out, &Message::Reply { id, result });
            }
            Request::StopTests { run } => {
                let handle = runs.lock().unwrap().remove(&run);
                drop(handle);
                send(&out, &Message::Reply { id, result: Ok(Value::Null) });
            }
            request => {
                let result = handle(&*backend, request).map_err(|err| format!("{err:#}"));
                send(&out, &Message::Reply { id, result });
            }
        }
    }
}

fn handle(backend: &dyn Backend, request: Request) -> Result<Value> {
    Ok(match request {
        Request::Status => serde_json::to_value(backend.status()?)?,
        Request::Revert { changes, end_session } => to_null(backend.revert(&changes, end_session)?),
        Request::Undo => to_null(backend.undo()?),
        Request::StartSession => to_null(backend.start_session()?),
        Request::EndSession => to_null(backend.end_session()?),
        Request::Accept { paths } => to_null(backend.accept(&paths)?),
        Request::ReadFile { path } => {
            if path.split('/').any(|part| part == "..") {
                bail!("refusing to read outside the repository: {path}");
            }
            let bytes = backend.read_file(&path)?;
            Value::String(base64::engine::general_purpose::STANDARD.encode(bytes))
        }
        Request::FileDiff { change } => Value::String(backend.file_diff(&change)?),
        Request::SaveRules { rules } => to_null(backend.save_rules(&rules)?),
        Request::Tests | Request::RunTests { .. } | Request::StopTests { .. } => unreachable!("handled in main"),
    })
}

fn to_null(_: ()) -> Value {
    Value::Null
}

fn send(out: &Mutex<std::io::Stdout>, message: &Message) {
    let mut line = serde_json::to_string(message).expect("messages serialize");
    line.push('\n');
    let mut out = out.lock().unwrap();
    // A closed stdout means Difftower went away; stdin will end too.
    let _ = out.write_all(line.as_bytes()).and_then(|()| out.flush());
}

fn expand_home(path: &str) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match (path.strip_prefix('~'), home) {
        (Some(""), Some(home)) => home,
        (Some(rest), Some(home)) if rest.starts_with('/') => home.join(&rest[1..]),
        _ => PathBuf::from(path),
    }
}
