//! A repository's tests, found and run with the runner the project already
//! uses. Each runner lives in a module of its own, behind [`Kind`]: how to
//! tell a project uses it, how to list its tests, and how to run some of
//! them and read what it prints.
//!
//! Each runner is detected from the project's files, or set in
//! `.difftower.toml`, which can also give the command that runs it:
//!
//! ```toml
//! [tests]
//! runner = "rails"
//! command = "PARALLEL_WORKERS=2 nice -n 19 bin/rails test"
//! ```
//!
//! Commands run in an interactive login shell at the repository root, as
//! editors resolve a terminal's environment: version managers such as
//! rbenv, pyenv or nvm are often set up only in `~/.bashrc`, which other
//! shells skip, and without them the project's Ruby or Python is missing.

mod bun;
mod cargo;
mod deno;
mod dotnet;
mod gradle;
mod go;
mod jest;
pub(crate) mod junit;
mod maven;
mod minitest;
mod mix;
mod mocha;
mod node;
mod pest;
mod phpunit;
mod playwright;
mod pytest;
mod rails;
mod rspec;
mod vitest;

use std::{
    io::{BufRead, BufReader},
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};

use crate::guard::CONFIG_FILE;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Runner {
    Pytest,
    Vitest,
    Jest,
    Bun,
    Deno,
    Node,
    Mocha,
    Playwright,
    Rails,
    Rspec,
    Minitest,
    Cargo,
    Go,
    Phpunit,
    Pest,
    Dotnet,
    Gradle,
    Maven,
    Mix,
}

impl Runner {
    /// Every runner, in the order they are looked for.
    pub const ALL: [Runner; 19] = [
        Runner::Pytest,
        Runner::Vitest,
        Runner::Jest,
        Runner::Bun,
        Runner::Deno,
        Runner::Node,
        Runner::Mocha,
        Runner::Playwright,
        Runner::Rails,
        Runner::Rspec,
        Runner::Minitest,
        Runner::Cargo,
        Runner::Go,
        Runner::Phpunit,
        Runner::Pest,
        Runner::Dotnet,
        Runner::Gradle,
        Runner::Maven,
        Runner::Mix,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Runner::Pytest => "pytest",
            Runner::Vitest => "vitest",
            Runner::Jest => "jest",
            Runner::Bun => "bun",
            Runner::Deno => "deno",
            Runner::Node => "node",
            Runner::Mocha => "mocha",
            Runner::Playwright => "playwright",
            Runner::Rails => "rails",
            Runner::Rspec => "rspec",
            Runner::Minitest => "minitest",
            Runner::Cargo => "cargo",
            Runner::Go => "go",
            Runner::Phpunit => "phpunit",
            Runner::Pest => "pest",
            Runner::Dotnet => "dotnet",
            Runner::Gradle => "gradle",
            Runner::Maven => "maven",
            Runner::Mix => "mix",
        }
    }

    fn kind(self) -> &'static dyn Kind {
        match self {
            Runner::Pytest => &pytest::Pytest,
            Runner::Vitest => &vitest::Vitest,
            Runner::Jest => &jest::Jest,
            Runner::Bun => &bun::Bun,
            Runner::Deno => &deno::Deno,
            Runner::Node => &node::Node,
            Runner::Mocha => &mocha::Mocha,
            Runner::Playwright => &playwright::Playwright,
            Runner::Rails => &rails::Rails,
            Runner::Rspec => &rspec::Rspec,
            Runner::Minitest => &minitest::Minitest,
            Runner::Cargo => &cargo::Cargo,
            Runner::Go => &go::Go,
            Runner::Phpunit => &phpunit::Phpunit,
            Runner::Pest => &pest::Pest,
            Runner::Dotnet => &dotnet::Dotnet,
            Runner::Gradle => &gradle::Gradle,
            Runner::Maven => &maven::Maven,
            Runner::Mix => &mix::Mix,
        }
    }
}

/// One way of running tests.
pub(crate) trait Kind: Sync {
    /// Whether the project at `root` uses it, judging by its files alone:
    /// this runs for every repository opened, so it starts no program.
    fn detect(&self, root: &Path) -> bool;

    /// The command that starts it at `root`, without arguments.
    fn default_command(&self, root: &Path) -> String;

    /// Every test at `root`, in any order; files relative to `root`.
    fn discover(&self, root: &Path, command: &str) -> Result<Vec<TestCase>>;

    /// The script that runs `target`, given the command that starts the
    /// runner, and what reads its output.
    fn start(&self, root: &Path, command: &str, target: &TestTarget) -> Result<Started>;
}

/// A run about to start: see [`Kind::start`].
pub(crate) struct Started {
    /// Run in the user's shell at the repository root, output and errors
    /// read together.
    pub script: String,
    pub parser: Box<dyn Parser>,
}

/// Reads a run's output as it comes, a line at a time, colors removed.
pub(crate) trait Parser: Send {
    fn feed(&mut self, line: &str) -> Vec<Found>;

    /// How `line` reads in the output shown to the user: as printed, unless
    /// the runner prints for machines; `None` hides it.
    fn display(&self, line: &str) -> Option<String> {
        Some(line.to_owned())
    }

    /// The run has exited: what is left, such as failures still being read
    /// or the results of a report file the runner wrote.
    fn finish(&mut self) -> Vec<Found> {
        Vec::new()
    }
}

/// What a [`Parser`] learned, by [`TestCase::id`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Found {
    Result(String, Outcome),
    Failure(String, Vec<String>),
}

/// One test.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestCase {
    /// `file::name`, unique among all tests shown together.
    pub id: String,
    /// The file defining it, as a path shown in the interface.
    pub file: String,
    /// How the test calls itself, without the file.
    pub name: String,
    pub line: Option<u32>,
}

/// The tests one runner finds in one repository.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TestSuite {
    /// The repository's directory, as in [`crate::Backend::repos`].
    pub repo: String,
    pub runner: Runner,
    pub tests: Vec<TestCase>,
    /// Why the tests could not be listed, if they could not.
    pub error: Option<String>,
}

/// What to run within a suite.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TestTarget {
    All,
    /// Test files, or directories of them.
    Paths { paths: Vec<String> },
    /// Some tests, by [`TestCase::id`] and line.
    Cases { cases: Vec<CaseRef> },
}

/// A test to run: its [`TestCase::id`] and line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseRef {
    pub id: String,
    pub line: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Passed,
    Failed,
    Skipped,
}

/// What a test run reports while it goes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TestEvent {
    /// A line the runner printed, colors removed.
    Output { line: String },
    /// A test finished.
    Result { id: String, outcome: Outcome },
    /// What the runner printed about a test that failed.
    Failure { id: String, lines: Vec<String> },
    /// The run is over; nothing follows.
    Finished { success: bool },
}

#[derive(Default, Deserialize)]
struct Config {
    #[serde(default)]
    tests: TestsConfig,
}

#[derive(Default, Deserialize)]
struct TestsConfig {
    runner: Option<Runner>,
    command: Option<String>,
}

/// The runners of the repository at `root`, each with the command that
/// starts it.
pub fn runners(root: &Path) -> Result<Vec<(Runner, String)>> {
    let config: Config = match std::fs::read_to_string(root.join(CONFIG_FILE)) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("invalid {CONFIG_FILE}"))?,
        Err(_) => Config::default(),
    };
    if let Some(runner) = config.tests.runner {
        let command = config.tests.command.unwrap_or_else(|| runner.kind().default_command(root));
        return Ok(vec![(runner, command)]);
    }
    let found: Vec<Runner> = Runner::ALL.into_iter().filter(|runner| runner.kind().detect(root)).collect();
    // A command without a runner applies when there is only one.
    Ok(match (config.tests.command, found.as_slice()) {
        (Some(command), [runner]) => vec![(*runner, command)],
        _ => found.into_iter().map(|runner| (runner, runner.kind().default_command(root))).collect(),
    })
}

/// Lists the tests `runner` finds at `root`.
pub fn discover(root: &Path, runner: Runner, command: &str) -> Result<Vec<TestCase>> {
    let mut tests = runner.kind().discover(root, command)?;
    tests.sort_by(|a, b| (&a.file, a.line, &a.name).cmp(&(&b.file, b.line, &b.name)));
    tests.dedup_by(|a, b| a.id == b.id);
    Ok(tests)
}

/// A test run in progress; dropping it stops the run.
pub struct TestRun {
    pid: u32,
    finished: Arc<AtomicBool>,
}

impl Drop for TestRun {
    fn drop(&mut self) {
        if self.finished.load(Ordering::SeqCst) {
            return;
        }
        // The shell leads its own process group: stop everything in it,
        // letting runners clean up first. An interactive shell ignores
        // SIGTERM, and a runner it starts after this would live on, so what
        // is left gets SIGKILL after a grace period.
        let (pid, finished) = (self.pid, self.finished.clone());
        let signal = move |name: &str| Command::new("kill").arg(format!("-{name}")).arg(format!("-{pid}")).status();
        let _ = signal("TERM");
        std::thread::spawn(move || {
            for _ in 0..30 {
                if finished.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            let _ = signal("KILL");
        });
    }
}

/// Starts running `target`, reporting through `on_event` from another
/// thread until [`TestEvent::Finished`].
pub fn run(
    root: &Path,
    runner: Runner,
    command: &str,
    target: &TestTarget,
    on_event: Box<dyn Fn(TestEvent) + Send + Sync>,
) -> Result<TestRun> {
    let Started { script, mut parser } = runner.kind().start(root, command, target)?;
    let mut child =
        shell(root, &script).stdout(Stdio::piped()).spawn().context("could not start the test runner")?;
    let stdout = child.stdout.take().expect("piped");
    let finished = Arc::new(AtomicBool::new(false));
    let done = finished.clone();
    let pid = child.id();
    std::thread::spawn(move || {
        let report = |found: Vec<Found>| {
            for found in found {
                on_event(match found {
                    Found::Result(id, outcome) => TestEvent::Result { id, outcome },
                    Found::Failure(id, lines) => TestEvent::Failure { id, lines },
                });
            }
        };
        let mut reader = BufReader::new(stdout);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let line = strip_ansi(&String::from_utf8_lossy(&buf));
            let line = line.trim_end_matches(['\n', '\r']);
            // A line rewritten with `\r`, as progress is, shows its last state.
            let line = line.rsplit('\r').next().unwrap_or(line).to_owned();
            let found = parser.feed(&line);
            if let Some(line) = parser.display(&line) {
                on_event(TestEvent::Output { line });
            }
            report(found);
        }
        // Report files are complete once the runner has exited.
        let success = child.wait().is_ok_and(|status| status.success());
        report(parser.finish());
        done.store(true, Ordering::SeqCst);
        on_event(TestEvent::Finished { success });
    });
    Ok(TestRun { pid, finished })
}

/// The user's shell, interactive and login, running `script` at `root`, its
/// errors mixed into its output, leading a process group of its own.
fn shell(root: &Path, script: &str) -> Command {
    use std::os::unix::process::CommandExt as _;
    let (program, flags) = match std::env::var("SHELL") {
        // Shells that read the same script syntax; fish does not.
        Ok(shell) if shell.ends_with("/bash") || shell.ends_with("/zsh") => (shell, "-ilc"),
        _ if Path::new("/bin/bash").exists() => ("/bin/bash".to_owned(), "-ilc"),
        _ => ("/bin/sh".to_owned(), "-lc"),
    };
    let mut cmd = Command::new(program);
    // Without a terminal, an interactive shell complains about job control
    // on stderr before the script redirects it; that stays out of the output.
    cmd.arg(flags)
        .arg(format!("exec 2>&1; cd {} && {script}", quote(&root.display().to_string())))
        .current_dir(root)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        // Plain output to parse; vitest colors pipes otherwise.
        .env("NO_COLOR", "1")
        .env("FORCE_COLOR", "0")
        .env("PY_COLORS", "0")
        .env("CI", "1")
        .process_group(0);
    cmd
}

/// The output of `script`, and whether it succeeded.
pub(crate) fn shell_run(root: &Path, script: &str) -> Result<(String, bool)> {
    let out = shell(root, script).stdout(Stdio::piped()).output().context("could not start the test runner")?;
    Ok((strip_ansi(&String::from_utf8_lossy(&out.stdout)), out.status.success()))
}

pub(crate) fn shell_output(root: &Path, script: &str) -> Result<String> {
    let (text, ok) = shell_run(root, script)?;
    if !ok {
        bail!("`{script}` failed: {}", tail(&text));
    }
    Ok(text)
}

/// The last lines of a runner's output, where it says what went wrong.
pub(crate) fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().rev().filter(|l| !l.trim().is_empty()).take(6).collect();
    lines.into_iter().rev().collect::<Vec<_>>().join("\n")
}

/// The file of a test id: what comes before `::`.
pub(crate) fn file_of(id: &str) -> &str {
    id.split_once("::").map_or(id, |(file, _)| file)
}

/// `file:line`, which many runners run alone.
pub(crate) fn file_and_line(id: &str, line: Option<u32>) -> String {
    let file = file_of(id);
    match line {
        Some(line) => format!("{file}:{line}"),
        None => file.to_owned(),
    }
}

/// `target` as the runner's arguments, each quoted for the shell: the paths,
/// or each case as `case` makes it.
pub(crate) fn target_args(target: &TestTarget, case: impl Fn(&CaseRef) -> String) -> String {
    let args: Vec<String> = match target {
        TestTarget::All => Vec::new(),
        TestTarget::Paths { paths } => paths.clone(),
        TestTarget::Cases { cases } => cases.iter().map(case).collect(),
    };
    args.iter().map(|arg| quote(arg)).collect::<Vec<_>>().join(" ")
}

pub(crate) fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// A regular expression matching `s` literally.
pub(crate) fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\.+*?()|[]{}^$#&-~/".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The files under `root` whose path relative to it `keep` accepts, as
/// such paths, skipping what git ignores and dependency folders.
pub(crate) fn walk(root: &Path, keep: impl Fn(&str) -> bool) -> Vec<String> {
    const SKIP: [&str; 8] = ["node_modules", "vendor", "target", "build", "dist", "_build", "deps", ".git"];
    let mut files = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .filter_entry(|entry| !SKIP.contains(&entry.file_name().to_str().unwrap_or("")))
        .build();
    for entry in walker.flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else { continue };
        let rel = rel.display().to_string();
        if keep(&rel) {
            files.push(rel);
        }
    }
    files.sort();
    files
}

/// A file at `root`, or empty.
pub(crate) fn read(root: &Path, name: &str) -> String {
    std::fs::read_to_string(root.join(name)).unwrap_or_default()
}

/// A fresh path in the temporary directory for a report file a runner
/// writes; the parser that reads it removes it.
pub(crate) fn report_path(name: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, Ordering::SeqCst);
    std::env::temp_dir().join(format!("difftower-{}-{n}-{name}", std::process::id()))
}

pub(crate) fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        if chars.next_if_eq(&'[').is_some() {
            // Parameters, then one final letter.
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        }
    }
    out
}

/// Gathers what a runner prints about each failed test, a block of lines
/// per test, for runners that print such blocks.
#[derive(Default)]
pub(crate) struct Blocks {
    /// The tests the block being read is about.
    current: Vec<String>,
    lines: Vec<String>,
}

impl Blocks {
    /// Starts a block about `ids`, returning the one it ends.
    pub fn start(&mut self, ids: Vec<String>) -> Vec<Found> {
        let done = self.end();
        self.current = ids;
        done
    }

    /// Adds another test to the block being started, before its lines.
    pub fn also(&mut self, id: String) {
        self.current.push(id);
    }

    pub fn reading(&self) -> bool {
        !self.current.is_empty()
    }

    pub fn has_lines(&self) -> bool {
        !self.lines.is_empty()
    }

    pub fn push(&mut self, line: &str) {
        if self.reading() {
            self.lines.push(line.to_owned());
        }
    }

    /// Ends the block being read, its trailing blank lines dropped.
    pub fn end(&mut self) -> Vec<Found> {
        let ids = std::mem::take(&mut self.current);
        let mut lines = std::mem::take(&mut self.lines);
        while lines.last().is_some_and(|l| l.trim().is_empty()) {
            lines.pop();
        }
        ids.into_iter().map(|id| Found::Failure(id, lines.clone())).collect()
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::{Found, Parser};

    /// Everything `parser` finds in `output`.
    pub fn parse(mut parser: Box<dyn Parser>, output: &str) -> Vec<Found> {
        let mut found: Vec<Found> = output.lines().flat_map(|line| parser.feed(line)).collect();
        found.extend(parser.finish());
        found
    }

    pub fn results(found: &[Found]) -> Vec<(&str, super::Outcome)> {
        found
            .iter()
            .filter_map(|f| match f {
                Found::Result(id, outcome) => Some((id.as_str(), *outcome)),
                _ => None,
            })
            .collect()
    }

    pub fn failures(found: &[Found]) -> Vec<(&str, &[String])> {
        found
            .iter()
            .filter_map(|f| match f {
                Found::Failure(id, lines) => Some((id.as_str(), lines.as_slice())),
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_runners_and_reads_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("package.json"), r#"{"devDependencies":{"vitest":"^5"}}"#).unwrap();
        std::fs::create_dir(root.join("tests")).unwrap();
        std::fs::write(root.join("pyproject.toml"), "[project]\n").unwrap();
        assert_eq!(
            runners(root).unwrap(),
            [(Runner::Pytest, "python3 -m pytest".to_owned()), (Runner::Vitest, "npx vitest".to_owned())]
        );
        std::fs::write(root.join(CONFIG_FILE), "[tests]\nrunner = \"vitest\"\ncommand = \"pnpm vitest\"\n").unwrap();
        assert_eq!(runners(root).unwrap(), [(Runner::Vitest, "pnpm vitest".to_owned())]);
    }

    #[test]
    fn runs_and_reports_each_test() {
        let dir = tempfile::tempdir().unwrap();
        // A stand-in runner printing what pytest prints.
        let script = "printf 'tests/a.py::test_ok PASSED [50%%]\\ntests/a.py::test_bad FAILED [100%%]\\n'; exit 1 #";
        let (tx, rx) = std::sync::mpsc::channel();
        let _run = run(
            dir.path(),
            Runner::Pytest,
            script,
            &TestTarget::All,
            Box::new(move |event| tx.send(event).unwrap()),
        )
        .unwrap();
        let events: Vec<TestEvent> = rx.iter().take_while(|e| !matches!(e, TestEvent::Finished { .. })).collect();
        let results: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                TestEvent::Result { id, outcome } => Some((id.as_str(), *outcome)),
                _ => None,
            })
            .collect();
        assert_eq!(results, [("tests/a.py::test_ok", Outcome::Passed), ("tests/a.py::test_bad", Outcome::Failed)]);
    }

    #[test]
    fn dropping_a_run_stops_it() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let run = run(
            dir.path(),
            Runner::Pytest,
            "sleep 30 #",
            &TestTarget::All,
            Box::new(move |event| {
                let _ = tx.send(event);
            }),
        )
        .unwrap();
        drop(run);
        let finished = rx.iter().find(|e| matches!(e, TestEvent::Finished { .. }));
        assert_eq!(finished, Some(TestEvent::Finished { success: false }));
    }

    #[test]
    fn every_runner_has_a_distinct_label() {
        let mut labels: Vec<_> = Runner::ALL.iter().map(|r| r.label()).collect();
        labels.sort();
        labels.dedup();
        assert_eq!(labels.len(), Runner::ALL.len());
        // The label is the name `.difftower.toml` takes.
        for runner in Runner::ALL {
            let parsed: Runner = serde_json::from_str(&format!("\"{}\"", runner.label())).unwrap();
            assert_eq!(parsed, runner);
        }
    }
}
