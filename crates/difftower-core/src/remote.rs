use std::{
    any::Any,
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};

use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::{
    Backend, Change, Status, TreeId,
    approvals::fnv,
    backend::dir_name,
    protocol::{Call, Message, PROTOCOL, Request},
};

/// Where the agent binaries live on the remote host.
const AGENT_DIR: &str = "~/.cache/difftower";
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// A repository on a remote host, written `host:path` or `user@host:path`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshTarget {
    pub host: String,
    /// As given; relative paths and `~` resolve against the remote home.
    pub path: String,
}

impl SshTarget {
    pub fn parse(arg: &str) -> Option<Self> {
        let (host, path) = arg.split_once(':')?;
        if host.is_empty() || host.contains('/') {
            return None;
        }
        let path = if path.is_empty() { "." } else { path };
        Some(Self { host: host.to_owned(), path: path.to_owned() })
    }

    /// Options shared by every `ssh` call, so they all multiplex over one
    /// connection: only the first one pays for the handshake.
    fn options() -> Vec<String> {
        let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
        [
            "ControlMaster=auto".to_owned(),
            format!("ControlPath={runtime}/difftower-%C"),
            "ControlPersist=60".to_owned(),
            "ServerAliveInterval=15".to_owned(),
            "ConnectTimeout=10".to_owned(),
        ]
        .into_iter()
        .flat_map(|option| ["-o".to_owned(), option])
        .collect()
    }

    /// `ssh`, or the program in `DIFFTOWER_SSH` (used by tests).
    fn program() -> String {
        std::env::var("DIFFTOWER_SSH").unwrap_or_else(|_| "ssh".into())
    }

    /// A non-interactive command on the host; it fails instead of prompting.
    fn command(&self, remote_command: &str) -> Command {
        let mut cmd = Command::new(Self::program());
        cmd.args(Self::options())
            .args(["-o", "BatchMode=yes", "-T", &self.host, remote_command]);
        cmd
    }

    /// The program and arguments for an interactive shell in `dir` on the host.
    pub fn shell(&self, dir: &str) -> (String, Vec<String>) {
        let mut args = Self::options();
        args.extend([
            "-t".to_owned(),
            self.host.clone(),
            format!("cd {} && exec \"${{SHELL:-/bin/sh}}\" -l", shell_quote(dir)),
        ]);
        (Self::program(), args)
    }
}

struct Shared {
    pending: Mutex<HashMap<u64, mpsc::Sender<Result<Value, String>>>>,
    on_change: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// Why the connection ended, once it has.
    closed: Mutex<Option<String>>,
}

/// A repository served by `difftower-agent` on a remote host over SSH.
pub struct Remote {
    target: SshTarget,
    root: String,
    stdin: Mutex<ChildStdin>,
    child: Mutex<Child>,
    shared: Arc<Shared>,
    next_id: AtomicU64,
}

impl Remote {
    /// Connects to the host, installs the agent there if this version is
    /// missing, and opens the repository. `progress` receives each step.
    pub fn connect(target: SshTarget, agent: &[u8], progress: &dyn Fn(&str)) -> Result<Self> {
        let name = format!("agent-{}-{:016x}", env!("CARGO_PKG_VERSION"), fnv(agent));
        let binary = format!("{AGENT_DIR}/{name}");

        progress(&format!("Connecting to {}", target.host));
        let probe = run(target.command(&format!("uname -sm; if test -x {binary}; then echo present; fi")), None)?;
        let mut lines = probe.lines();
        let platform = lines.next().unwrap_or_default().trim();
        if platform != "Linux x86_64" {
            bail!("{} runs {platform:?}; Difftower supports Linux x86_64 hosts for now", target.host);
        }
        if !lines.any(|line| line.trim() == "present") {
            progress("Installing the Difftower agent on the host");
            let install = format!(
                "mkdir -p {AGENT_DIR} && cd {AGENT_DIR} && cat > {name}.tmp && chmod 755 {name}.tmp \
                 && mv -f {name}.tmp {name} && for f in agent-*; do [ \"$f\" = {name} ] || rm -f -- \"$f\"; done"
            );
            run(target.command(&install), Some(agent)).context("could not install the agent")?;
        }

        progress("Opening the repository");
        let mut child = target
            .command(&format!("{binary} serve {}", shell_quote(&target.path)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to run ssh")?;
        let mut stdout = BufReader::new(child.stdout.take().expect("piped"));
        let mut stderr = child.stderr.take().expect("piped");

        let mut hello = String::new();
        stdout.read_line(&mut hello)?;
        let root = match serde_json::from_str::<Message>(&hello) {
            Ok(Message::Hello { protocol, root }) if protocol == PROTOCOL => root,
            Ok(Message::Hello { protocol, .. }) => {
                bail!("the agent speaks protocol {protocol}, expected {PROTOCOL}")
            }
            Ok(Message::Fatal { error }) => bail!("{error}"),
            _ => {
                let _ = child.kill();
                let mut detail = String::new();
                let _ = stderr.read_to_string(&mut detail);
                bail!("the agent did not start: {}", detail.trim())
            }
        };

        // Drain stderr so a chatty agent can never block on a full pipe.
        std::thread::spawn(move || std::io::copy(&mut stderr, &mut std::io::sink()));

        let shared = Arc::new(Shared {
            pending: Mutex::new(HashMap::new()),
            on_change: Mutex::new(None),
            closed: Mutex::new(None),
        });
        let reader_shared = shared.clone();
        let host = target.host.clone();
        std::thread::spawn(move || read_messages(stdout, &reader_shared, &host));

        Ok(Self {
            target,
            root,
            stdin: Mutex::new(child.stdin.take().expect("piped")),
            child: Mutex::new(child),
            shared,
            next_id: AtomicU64::new(1),
        })
    }

    pub fn target(&self) -> &SshTarget {
        &self.target
    }

    fn call<T: DeserializeOwned>(&self, request: Request) -> Result<T> {
        if let Some(reason) = self.shared.closed.lock().unwrap().clone() {
            bail!(reason);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.shared.pending.lock().unwrap().insert(id, tx);

        let mut line = serde_json::to_string(&Call { id, request })?;
        line.push('\n');
        let written = {
            let mut stdin = self.stdin.lock().unwrap();
            stdin.write_all(line.as_bytes()).and_then(|()| stdin.flush())
        };
        if let Err(err) = written {
            self.shared.pending.lock().unwrap().remove(&id);
            bail!("connection to {} lost: {err}", self.target.host);
        }

        match rx.recv_timeout(CALL_TIMEOUT) {
            Ok(Ok(value)) => Ok(serde_json::from_value(value)?),
            Ok(Err(err)) => Err(anyhow!(err)),
            Err(_) => {
                self.shared.pending.lock().unwrap().remove(&id);
                bail!("{} did not answer in time", self.target.host)
            }
        }
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        let _ = self.child.lock().unwrap().kill();
    }
}

fn read_messages(stdout: impl BufRead, shared: &Shared, host: &str) {
    for line in stdout.lines() {
        let Ok(line) = line else { break };
        match serde_json::from_str::<Message>(&line) {
            Ok(Message::Reply { id, result }) => {
                if let Some(tx) = shared.pending.lock().unwrap().remove(&id) {
                    let _ = tx.send(result);
                }
            }
            Ok(Message::Changed) => {
                if let Some(on_change) = &*shared.on_change.lock().unwrap() {
                    on_change();
                }
            }
            _ => {}
        }
    }
    let reason = format!("connection to {host} lost");
    *shared.closed.lock().unwrap() = Some(reason.clone());
    for (_, tx) in shared.pending.lock().unwrap().drain() {
        let _ = tx.send(Err(reason.clone()));
    }
}

/// Clears the change callback when the UI stops watching.
struct WatchHandle(Arc<Shared>);

impl Drop for WatchHandle {
    fn drop(&mut self) {
        *self.0.on_change.lock().unwrap() = None;
    }
}

impl Backend for Remote {
    fn name(&self) -> String {
        dir_name(&self.root)
    }

    fn host(&self) -> Option<String> {
        Some(self.target.host.clone())
    }

    fn root(&self) -> String {
        self.root.clone()
    }

    fn status(&self) -> Result<Status> {
        self.call(Request::Status)
    }

    fn revert(&self, base: &TreeId, change: &Change) -> Result<()> {
        self.call(Request::Revert { base: base.clone(), change: change.clone() })
    }

    fn read_file(&self, path: &str) -> Result<Vec<u8>> {
        let encoded: String = self.call(Request::ReadFile { path: path.to_owned() })?;
        Ok(base64::engine::general_purpose::STANDARD.decode(encoded)?)
    }

    fn save_rules(&self, rules: &[String]) -> Result<()> {
        self.call(Request::SaveRules { rules: rules.to_vec() })
    }

    fn save_approvals(&self, text: &str) -> Result<()> {
        self.call(Request::SaveApprovals { text: text.to_owned() })
    }

    fn watch(&self, on_change: Box<dyn Fn() + Send + Sync>) -> Result<Box<dyn Any + Send>> {
        *self.shared.on_change.lock().unwrap() = Some(on_change);
        Ok(Box::new(WatchHandle(self.shared.clone())))
    }
}

/// Runs a command to completion, feeding it `input`, and returns its stdout.
fn run(mut cmd: Command, input: Option<&[u8]>) -> Result<String> {
    cmd.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().context("failed to run ssh")?;
    if let Some(input) = input {
        let mut stdin = child.stdin.take().expect("piped");
        stdin.write_all(input)?;
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        match stderr.trim() {
            "" => bail!("ssh exited with {}", out.status),
            stderr => bail!("{stderr}"),
        }
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Quotes a path for a POSIX shell, keeping a leading `~` expandable.
pub(crate) fn shell_quote(path: &str) -> String {
    let quote = |s: &str| format!("'{}'", s.replace('\'', r"'\''"));
    match path.strip_prefix('~') {
        Some("") => "~".to_owned(),
        Some(rest) if rest.starts_with('/') => format!("~/{}", quote(&rest[1..])),
        _ => quote(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_targets() {
        let target = SshTarget::parse("me@box:/srv/app").unwrap();
        assert_eq!((target.host.as_str(), target.path.as_str()), ("me@box", "/srv/app"));
        assert_eq!(SshTarget::parse("box:").unwrap().path, ".");
        assert_eq!(SshTarget::parse("./dir:x"), None);
        assert_eq!(SshTarget::parse("/abs/path"), None);
    }

    #[test]
    fn quotes_paths_for_the_shell() {
        assert_eq!(shell_quote("/srv/it's"), r"'/srv/it'\''s'");
        assert_eq!(shell_quote("~/code/app"), "~/'code/app'");
        assert_eq!(shell_quote("~"), "~");
    }
}
