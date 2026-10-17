use std::{
    any::Any,
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow, bail};
use base64::Engine as _;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::{
    Backend, Change, Status,
    backend::{OnTestEvent, dir_name},
    testing::{Runner, TestEvent, TestSuite, TestTarget},
    protocol::{Call, Message, PROTOCOL, Request},
};

/// Where the agent binaries live on the remote host.
const AGENT_DIR: &str = "~/.cache/difftower";
const CALL_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);

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
            "ServerAliveInterval=5".to_owned(),
            "ServerAliveCountMax=3".to_owned(),
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

/// One ssh session running the agent; replaced when the connection drops.
struct Link {
    generation: u64,
    stdin: ChildStdin,
    child: Child,
}

struct Shared {
    target: SshTarget,
    /// Kept to reinstall the agent if a reconnection finds it gone.
    agent: &'static [u8],
    /// `None` while reconnecting.
    link: Mutex<Option<Link>>,
    pending: Mutex<HashMap<u64, mpsc::Sender<Result<Value, String>>>>,
    on_change: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// Test runs in progress on the host, by run id.
    runs: Mutex<HashMap<u64, Arc<dyn Fn(TestEvent) + Send + Sync>>>,
    /// Set when the [`Remote`] is dropped, to stop reconnecting.
    closed: AtomicBool,
}

impl Shared {
    fn lost(&self) -> String {
        format!("connection to {} lost, reconnecting…", self.target.host)
    }

    /// Sends `request` without waiting for an answer; false if the
    /// connection is down.
    fn send(&self, id: u64, request: Request) -> bool {
        let Ok(mut line) = serde_json::to_string(&Call { id, request }) else { return false };
        line.push('\n');
        match &mut *self.link.lock().unwrap() {
            Some(link) => link.stdin.write_all(line.as_bytes()).and_then(|()| link.stdin.flush()).is_ok(),
            None => false,
        }
    }

    fn notify_change(&self) {
        if let Some(on_change) = &*self.on_change.lock().unwrap() {
            on_change();
        }
    }
}

/// A repository served by `difftower-agent` on a remote host over SSH.
///
/// When the connection drops, it reconnects in the background, waiting
/// longer between attempts, and reports a change once it is back so the
/// interface refreshes. Calls fail meanwhile.
pub struct Remote {
    root: String,
    repos: Vec<String>,
    shared: Arc<Shared>,
    next_id: AtomicU64,
}

/// An agent that answered, before its messages are read.
struct Started {
    root: String,
    repos: Vec<String>,
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Remote {
    /// Connects to the host, installs the agent there if this version is
    /// missing, and opens the repository. `progress` receives each step.
    pub fn connect(target: SshTarget, agent: &'static [u8], progress: &dyn Fn(&str)) -> Result<Self> {
        let started = start(&target, agent, progress)?;
        let (root, repos) = (started.root.clone(), started.repos.clone());
        let shared = Arc::new(Shared {
            target,
            agent,
            link: Mutex::new(None),
            pending: Mutex::new(HashMap::new()),
            on_change: Mutex::new(None),
            runs: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
        });
        attach(&shared, started, 1);
        Ok(Self { root, repos, shared, next_id: AtomicU64::new(1) })
    }

    pub fn target(&self) -> &SshTarget {
        &self.shared.target
    }

    fn call<T: DeserializeOwned>(&self, request: Request) -> Result<T> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.shared.pending.lock().unwrap().insert(id, tx);
        if !self.shared.send(id, request) {
            self.shared.pending.lock().unwrap().remove(&id);
            bail!(self.shared.lost());
        }

        match rx.recv_timeout(CALL_TIMEOUT) {
            Ok(Ok(value)) => Ok(serde_json::from_value(value)?),
            Ok(Err(err)) => Err(anyhow!(err)),
            Err(_) => {
                self.shared.pending.lock().unwrap().remove(&id);
                bail!("{} did not answer in time", self.shared.target.host)
            }
        }
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::SeqCst);
        if let Some(mut link) = self.shared.link.lock().unwrap().take() {
            let _ = link.child.kill();
            let _ = link.child.wait();
        }
    }
}

/// Makes sure this agent version is installed, then starts it on the
/// repository and waits for its greeting.
fn start(target: &SshTarget, agent: &[u8], progress: &dyn Fn(&str)) -> Result<Started> {
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
    let (root, repos) = match serde_json::from_str::<Message>(&hello) {
        Ok(Message::Hello { protocol, root, repos }) if protocol == PROTOCOL => (root, repos),
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
    let stdin = child.stdin.take().expect("piped");
    Ok(Started { root, repos, child, stdin, stdout })
}

/// Makes a started agent the live connection and reads its messages until
/// it goes away.
fn attach(shared: &Arc<Shared>, started: Started, generation: u64) {
    let Started { child, stdin, stdout, .. } = started;
    *shared.link.lock().unwrap() = Some(Link { generation, stdin, child });
    let reader = shared.clone();
    std::thread::spawn(move || {
        read_messages(stdout, &reader);
        connection_lost(&reader, generation);
    });
}

fn read_messages(stdout: impl BufRead, shared: &Shared) {
    for line in stdout.lines() {
        let Ok(line) = line else { break };
        match serde_json::from_str::<Message>(&line) {
            Ok(Message::Reply { id, result }) => {
                if let Some(tx) = shared.pending.lock().unwrap().remove(&id) {
                    let _ = tx.send(result);
                }
            }
            Ok(Message::Changed) => shared.notify_change(),
            Ok(Message::Test { run, event }) => {
                let finished = matches!(event, TestEvent::Finished { .. });
                let on_event = if finished {
                    shared.runs.lock().unwrap().remove(&run)
                } else {
                    shared.runs.lock().unwrap().get(&run).cloned()
                };
                if let Some(on_event) = on_event {
                    on_event(event);
                }
            }
            _ => {}
        }
    }
}

fn connection_lost(shared: &Arc<Shared>, generation: u64) {
    {
        let mut link = shared.link.lock().unwrap();
        // Dropped on purpose, or already replaced.
        if link.as_ref().map(|l| l.generation) != Some(generation) {
            return;
        }
        if let Some(mut old) = link.take() {
            let _ = old.child.kill();
            let _ = old.child.wait();
        }
    }
    let reason = shared.lost();
    for (_, tx) in shared.pending.lock().unwrap().drain() {
        let _ = tx.send(Err(reason.clone()));
    }
    // The agent's runs died with it.
    let runs: Vec<_> = shared.runs.lock().unwrap().drain().collect();
    for (_, on_event) in runs {
        on_event(TestEvent::Output { line: reason.clone() });
        on_event(TestEvent::Finished { success: false });
    }
    // Lets the interface refresh, fail, and show that it is reconnecting.
    shared.notify_change();
    let shared = shared.clone();
    std::thread::spawn(move || reconnect(&shared, generation + 1));
}

/// Retries until the host answers again or the [`Remote`] is dropped.
fn reconnect(shared: &Arc<Shared>, generation: u64) {
    let mut delay = Duration::from_secs(1);
    loop {
        let deadline = Instant::now() + delay;
        while Instant::now() < deadline {
            if shared.closed.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        match start(&shared.target, shared.agent, &|_| {}) {
            Ok(mut started) => {
                if shared.closed.load(Ordering::SeqCst) {
                    let _ = started.child.kill();
                    return;
                }
                attach(shared, started, generation);
                shared.notify_change();
                return;
            }
            Err(_) => delay = (delay * 2).min(MAX_RECONNECT_DELAY),
        }
    }
}

/// Stops a remote test run when dropped before it finished.
struct RemoteRun {
    shared: Arc<Shared>,
    run: u64,
}

impl Drop for RemoteRun {
    fn drop(&mut self) {
        if self.shared.runs.lock().unwrap().contains_key(&self.run) {
            // Its reply goes unread; the run still reports that it finished.
            self.shared.send(0, Request::StopTests { run: self.run });
        }
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
        Some(self.target().host.clone())
    }

    fn root(&self) -> String {
        self.root.clone()
    }

    fn repos(&self) -> Vec<String> {
        self.repos.clone()
    }

    fn status(&self) -> Result<Status> {
        self.call(Request::Status)
    }

    fn revert(&self, changes: &[Change], end_session: bool) -> Result<()> {
        self.call(Request::Revert { changes: changes.to_vec(), end_session })
    }

    fn undo(&self) -> Result<()> {
        self.call(Request::Undo)
    }

    fn start_session(&self) -> Result<()> {
        self.call(Request::StartSession)
    }

    fn end_session(&self) -> Result<()> {
        self.call(Request::EndSession)
    }

    fn accept(&self, paths: &[String]) -> Result<()> {
        self.call(Request::Accept { paths: paths.to_vec() })
    }

    fn read_file(&self, path: &str) -> Result<Vec<u8>> {
        let encoded: String = self.call(Request::ReadFile { path: path.to_owned() })?;
        Ok(base64::engine::general_purpose::STANDARD.decode(encoded)?)
    }

    fn file_diff(&self, change: &Change) -> Result<String> {
        self.call(Request::FileDiff { change: change.clone() })
    }

    fn save_rules(&self, rules: &[String]) -> Result<()> {
        self.call(Request::SaveRules { rules: rules.to_vec() })
    }

    fn tests(&self) -> Result<Vec<TestSuite>> {
        self.call(Request::Tests)
    }

    fn run_tests(
        &self,
        repo: &str,
        runner: Runner,
        target: TestTarget,
        on_event: OnTestEvent,
    ) -> Result<Box<dyn Any + Send>> {
        let run = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.shared.runs.lock().unwrap().insert(run, Arc::from(on_event));
        let started: Result<()> = self.call(Request::RunTests { run, repo: repo.to_owned(), runner, target });
        if let Err(err) = started {
            self.shared.runs.lock().unwrap().remove(&run);
            return Err(err);
        }
        Ok(Box::new(RemoteRun { shared: self.shared.clone(), run }))
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

/// FNV-1a, stable across builds unlike `DefaultHasher`.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3))
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
