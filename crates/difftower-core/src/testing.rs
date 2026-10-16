//! A repository's tests, found and run with the runner the project already
//! uses: pytest, vitest or Rails' minitest.
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

use std::{
    collections::HashMap,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
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
    Rails,
}

impl Runner {
    pub fn label(self) -> &'static str {
        match self {
            Runner::Pytest => "pytest",
            Runner::Vitest => "vitest",
            Runner::Rails => "rails",
        }
    }
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

impl TestTarget {
    /// The runner's arguments: paths, pytest node ids, or `file:line`
    /// for vitest and Rails.
    fn args(&self, runner: Runner) -> String {
        let args: Vec<String> = match self {
            TestTarget::All => Vec::new(),
            TestTarget::Paths { paths } => paths.clone(),
            TestTarget::Cases { cases } => cases
                .iter()
                .map(|case| match runner {
                    Runner::Pytest => case.id.clone(),
                    Runner::Vitest | Runner::Rails => file_and_line(&case.id, case.line),
                })
                .collect(),
        };
        args.iter().map(|arg| quote(arg)).collect::<Vec<_>>().join(" ")
    }
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
        let command = config.tests.command.unwrap_or_else(|| default_command(root, runner));
        return Ok(vec![(runner, command)]);
    }
    let found = detect(root);
    // A command without a runner applies when there is only one.
    Ok(match (config.tests.command, found.as_slice()) {
        (Some(command), [runner]) => vec![(*runner, command)],
        _ => found.into_iter().map(|runner| (runner, default_command(root, runner))).collect(),
    })
}

fn detect(root: &Path) -> Vec<Runner> {
    let mut runners = Vec::new();
    let read = |name: &str| std::fs::read_to_string(root.join(name)).unwrap_or_default();
    if root.join("bin/rails").is_file() && root.join("test").is_dir() {
        runners.push(Runner::Rails);
    }
    let python = ["pyproject.toml", "setup.py", "setup.cfg", "requirements.txt", "pytest.ini", "tox.ini"];
    let pytest_config = read("pyproject.toml").contains("[tool.pytest")
        || root.join("pytest.ini").is_file()
        || root.join("conftest.py").is_file();
    let python_tests = ["tests", "test"].iter().any(|dir| root.join(dir).is_dir())
        && python.iter().any(|name| root.join(name).is_file());
    if pytest_config || python_tests {
        runners.push(Runner::Pytest);
    }
    let vitest_config = ["vitest.config.ts", "vitest.config.js", "vitest.config.mts", "vitest.config.mjs"]
        .iter()
        .any(|name| root.join(name).is_file());
    if vitest_config || read("package.json").contains("\"vitest\"") {
        runners.push(Runner::Vitest);
    }
    runners
}

fn default_command(root: &Path, runner: Runner) -> String {
    match runner {
        Runner::Pytest => ["venv", ".venv", "env"]
            .iter()
            .rev()
            .find(|dir| root.join(dir).join("bin/python").is_file())
            .map_or("python3 -m pytest".to_owned(), |dir| format!("{dir}/bin/python -m pytest")),
        Runner::Vitest if root.join("node_modules/.bin/vitest").is_file() => "node_modules/.bin/vitest".to_owned(),
        Runner::Vitest => "npx vitest".to_owned(),
        Runner::Rails => "bin/rails test".to_owned(),
    }
}

/// Lists the tests `runner` finds at `root`.
pub fn discover(root: &Path, runner: Runner, command: &str) -> Result<Vec<TestCase>> {
    let mut tests = match runner {
        Runner::Pytest => {
            // A file that does not import, often one an agent is editing,
            // must not hide every other test.
            let script = format!("{command} --collect-only -q -p no:cacheprovider --continue-on-collection-errors");
            let (out, ok) = shell_run(root, &script)?;
            let tests = parse_pytest_collection(&out);
            if !ok && tests.is_empty() {
                bail!("`{script}` failed: {}", tail(&out));
            }
            tests
        }
        Runner::Vitest => {
            let out = shell_output(root, &format!("{command} list --json"))?;
            parse_vitest_list(&out, root)?
        }
        Runner::Rails => rails_tests(root)?,
    };
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
    let args = target.args(runner);
    let (args, names) = match runner {
        Runner::Pytest => (format!("-v -rA --color=no {args}"), HashMap::new()),
        Runner::Vitest => (format!("run --reporter=verbose {args}"), HashMap::new()),
        Runner::Rails => {
            // Minitest names a test `Class#method`; results need the file too.
            let names = rails_tests(root)?.into_iter().map(|t| (rails_key(&t.id).to_owned(), t.id)).collect();
            (format!("-v {args}"), names)
        }
    };
    let mut child = shell(root, &format!("{command} {args}"))
        .stdout(Stdio::piped())
        .spawn()
        .context("could not start the test runner")?;
    let stdout = child.stdout.take().expect("piped");
    let finished = Arc::new(AtomicBool::new(false));
    let done = finished.clone();
    let pid = child.id();
    let mut failures = Failures::new(runner, names.clone());
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let line = strip_ansi(&String::from_utf8_lossy(&buf));
            let line = line.trim_end_matches(['\n', '\r']).to_owned();
            let result = match runner {
                Runner::Pytest => parse_pytest_result(&line),
                Runner::Vitest => parse_vitest_result(&line),
                Runner::Rails => parse_rails_result(&line)
                    .and_then(|(key, outcome)| Some((names.get(&key)?.clone(), outcome))),
            };
            let blocks = failures.feed(&line);
            on_event(TestEvent::Output { line });
            if let Some((id, outcome)) = result {
                on_event(TestEvent::Result { id, outcome });
            }
            for (id, lines) in blocks {
                on_event(TestEvent::Failure { id, lines });
            }
        }
        for (id, lines) in failures.finish() {
            on_event(TestEvent::Failure { id, lines });
        }
        let success = child.wait().is_ok_and(|status| status.success());
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
        .process_group(0);
    cmd
}

/// The output of `script`, and whether it succeeded.
fn shell_run(root: &Path, script: &str) -> Result<(String, bool)> {
    let out = shell(root, script).stdout(Stdio::piped()).output().context("could not start the test runner")?;
    Ok((strip_ansi(&String::from_utf8_lossy(&out.stdout)), out.status.success()))
}

fn shell_output(root: &Path, script: &str) -> Result<String> {
    let (text, ok) = shell_run(root, script)?;
    if !ok {
        bail!("`{script}` failed: {}", tail(&text));
    }
    Ok(text)
}

/// The last lines of a runner's output, where it says what went wrong.
fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.lines().rev().filter(|l| !l.trim().is_empty()).take(6).collect();
    lines.into_iter().rev().collect::<Vec<_>>().join("\n")
}

/// `file:line`, which vitest and Rails run alone.
fn file_and_line(id: &str, line: Option<u32>) -> String {
    let file = id.split_once("::").map_or(id, |(file, _)| file);
    match line {
        Some(line) => format!("{file}:{line}"),
        None => file.to_owned(),
    }
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        if chars.next_if_eq(&'[').is_some() {
            // Parameters, then one final letter.
            while let Some(c) = chars.next() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        }
    }
    out
}

fn parse_pytest_collection(out: &str) -> Vec<TestCase> {
    // Ids up to the first blank line; the warnings summary below repeats some.
    out.lines()
        .skip_while(|line| line.trim().is_empty())
        .take_while(|line| !line.trim().is_empty())
        .filter(|line| !line.starts_with(' ') && line.contains("::"))
        .map(|id| {
            let (file, name) = id.split_once("::").expect("checked");
            TestCase { id: id.to_owned(), file: file.to_owned(), name: name.to_owned(), line: None }
        })
        .collect()
}

/// `tests/a.py::test_x PASSED [ 50%]` while running, or
/// `FAILED tests/a.py::test_x - reason` in the summary at the end. Node
/// ids of parametrized tests may hold spaces: `test_x[a b]`.
fn parse_pytest_result(line: &str) -> Option<(String, Outcome)> {
    const WORDS: [(&str, Outcome); 6] = [
        ("PASSED", Outcome::Passed),
        ("XFAIL", Outcome::Passed),
        ("FAILED", Outcome::Failed),
        ("ERROR", Outcome::Failed),
        ("XPASS", Outcome::Failed),
        ("SKIPPED", Outcome::Skipped),
    ];
    // With pytest-xdist, `[gw0] [ 50%] PASSED id`: the worker and progress go.
    let mut line = line.trim_end();
    while let Some(rest) = line.strip_prefix('[') {
        line = rest.split_once("] ")?.1.trim_start();
    }
    for (word, outcome) in WORDS {
        if let Some(rest) = line.strip_prefix(word).and_then(|rest| rest.strip_prefix(' ')) {
            return summary_id(rest).map(|id| (id.to_owned(), outcome));
        }
    }
    for (word, outcome) in WORDS {
        let Some(at) = line.rfind(&format!(" {word}")) else { continue };
        let after = &line[at + 1 + word.len()..];
        let id = line[..at].trim_end();
        if (after.is_empty() || after.starts_with(' ')) && id.contains("::") && !id.contains("::-") {
            return Some((id.to_owned(), outcome));
        }
    }
    None
}

/// The node id in a summary line, before its ` - reason`.
fn summary_id(rest: &str) -> Option<&str> {
    let id = match rest.find('[') {
        // Up to the bracket that closes the parameters: the first one that
        // ends the line or comes before the reason, which may hold brackets.
        Some(open) => {
            let close = rest[open..]
                .match_indices(']')
                .map(|(ix, _)| open + ix)
                .find(|&ix| rest[ix + 1..].is_empty() || rest[ix + 1..].starts_with(" - "))?;
            &rest[..=close]
        }
        None => rest.split(" - ").next()?,
    };
    id.contains("::").then_some(id)
}

#[derive(Deserialize)]
struct VitestListed {
    name: String,
    file: String,
    location: Option<VitestLocation>,
}

#[derive(Deserialize)]
struct VitestLocation {
    line: u32,
}

fn parse_vitest_list(out: &str, root: &Path) -> Result<Vec<TestCase>> {
    let start = out.find('[').context("vitest listed no tests")?;
    let end = out.rfind(']').context("vitest listed no tests")?;
    let listed: Vec<VitestListed> = serde_json::from_str(&out[start..=end]).context("unexpected output from vitest list")?;
    let roots: Vec<PathBuf> = [Some(root.to_path_buf()), root.canonicalize().ok()].into_iter().flatten().collect();
    Ok(listed
        .into_iter()
        .map(|t| {
            let path = Path::new(&t.file);
            let file = roots
                .iter()
                .find_map(|root| path.strip_prefix(root).ok())
                .map_or(t.file.clone(), |p| p.display().to_string());
            TestCase { id: format!("{file}::{}", t.name), file, name: t.name, line: t.location.map(|l| l.line) }
        })
        .collect())
}

/// ` ✓ src/a.test.ts > group > name 12ms`, `×` when it failed and `↓`
/// when skipped.
fn parse_vitest_result(line: &str) -> Option<(String, Outcome)> {
    let line = line.trim_start();
    let mut chars = line.chars();
    let outcome = match chars.next()? {
        '✓' | '√' => Outcome::Passed,
        '×' | '✗' => Outcome::Failed,
        '↓' => Outcome::Skipped,
        _ => return None,
    };
    let rest = chars.as_str().trim_start();
    let (file, name) = rest.split_once(" > ")?;
    // Run by `file:line`, vitest names the file that way.
    let file = match file.rsplit_once(':') {
        Some((path, line)) if line.parse::<u32>().is_ok() => path,
        _ => file,
    };
    // A trailing duration: ` 12ms` or ` 1.2s`.
    let name = match name.rsplit_once(' ') {
        Some((head, tail))
            if tail.ends_with('s') && tail.trim_end_matches(['m', 's']).parse::<f64>().is_ok() =>
        {
            head
        }
        _ => name,
    };
    Some((format!("{file}::{name}"), outcome))
}

/// Every test under `test/`, read from the files, so listing needs neither
/// Ruby nor a database. System tests are left out, as `bin/rails test` does.
fn rails_tests(root: &Path) -> Result<Vec<TestCase>> {
    let mut tests = Vec::new();
    let walker = ignore::WalkBuilder::new(root.join("test")).hidden(false).build();
    for entry in walker {
        let entry = entry?;
        let path = entry.path();
        let Ok(rel) = path.strip_prefix(root) else { continue };
        let file = rel.display().to_string();
        if !file.ends_with("_test.rb") || file.starts_with("test/system/") {
            continue;
        }
        let text = std::fs::read_to_string(path).unwrap_or_default();
        tests.extend(parse_rails_file(&file, &text));
    }
    Ok(tests)
}

fn parse_rails_file(file: &str, text: &str) -> Vec<TestCase> {
    // Enclosing modules and classes, with their indentation.
    let mut scopes: Vec<(usize, String)> = Vec::new();
    let mut tests = Vec::new();
    for (ix, line) in text.lines().enumerate() {
        let indent = line.len() - line.trim_start().len();
        let trimmed = line.trim_start();
        let scope = trimmed.strip_prefix("class ").or_else(|| trimmed.strip_prefix("module "));
        if let Some(rest) = scope {
            let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':').collect();
            if name.starts_with(|c: char| c.is_ascii_uppercase()) {
                scopes.retain(|(level, _)| *level < indent);
                scopes.push((indent, name));
            }
            continue;
        }
        let method = if let Some(rest) = trimmed.strip_prefix("test ").or_else(|| trimmed.strip_prefix("test(")) {
            let rest = rest.trim_start();
            let Some(quote) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else { continue };
            let Some(end) = rest[1..].find(quote) else { continue };
            let name = &rest[1..1 + end];
            (format!("test_{}", name.split_whitespace().collect::<Vec<_>>().join("_")), name.to_owned())
        } else if let Some(rest) = trimmed.strip_prefix("def test_") {
            let name: String =
                rest.chars().take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '?' | '!')).collect();
            (format!("test_{name}"), format!("test_{name}"))
        } else {
            continue;
        };
        scopes.retain(|(level, _)| *level < indent);
        let class = scopes.iter().map(|(_, name)| name.as_str()).collect::<Vec<_>>().join("::");
        if class.is_empty() {
            continue;
        }
        let (method, name) = method;
        tests.push(TestCase {
            id: format!("{file}::{class}#{method}"),
            file: file.to_owned(),
            name,
            line: Some(ix as u32 + 1),
        });
    }
    tests
}

/// `Class#method`, the part of a Rails test id minitest prints.
fn rails_key(id: &str) -> &str {
    id.split_once("::").map_or(id, |(_, key)| key)
}

/// `UserTest#test_name = 0.01 s = .`, with `F`, `E` or `S` for failed,
/// errored and skipped tests.
fn parse_rails_result(line: &str) -> Option<(String, Outcome)> {
    let (rest, mark) = line.trim_end().rsplit_once(" = ")?;
    let outcome = match mark {
        "." => Outcome::Passed,
        "F" | "E" => Outcome::Failed,
        "S" | "N" => Outcome::Skipped,
        _ => return None,
    };
    let (key, time) = rest.rsplit_once(" = ")?;
    time.strip_suffix(" s")?.parse::<f64>().ok()?;
    key.contains('#').then(|| (key.to_owned(), outcome))
}


/// Collects what a runner prints about each failed test, a block per
/// test after the results.
struct Failures {
    runner: Runner,
    /// Rails' `Class#method` to test ids.
    names: HashMap<String, String>,
    /// pytest: inside the FAILURES or ERRORS section.
    in_section: bool,
    /// The tests the block being read is about, as the runner names them.
    current: Vec<String>,
    lines: Vec<String>,
    /// Rails: the next line names the failed test.
    expect_name: bool,
}

impl Failures {
    fn new(runner: Runner, names: HashMap<String, String>) -> Self {
        Self { runner, names, in_section: false, current: Vec::new(), lines: Vec::new(), expect_name: false }
    }

    /// Reads one line; returns the blocks it completed, by test id.
    fn feed(&mut self, line: &str) -> Vec<(String, Vec<String>)> {
        match self.runner {
            Runner::Pytest => self.feed_pytest(line),
            Runner::Vitest => self.feed_vitest(line),
            Runner::Rails => self.feed_rails(line),
        }
    }

    fn finish(&mut self) -> Vec<(String, Vec<String>)> {
        self.flush()
    }

    fn flush(&mut self) -> Vec<(String, Vec<String>)> {
        let names = std::mem::take(&mut self.current);
        let mut lines = std::mem::take(&mut self.lines);
        while lines.last().is_some_and(|l| l.trim().is_empty()) {
            lines.pop();
        }
        names
            .into_iter()
            .filter_map(|name| {
                let id = match self.runner {
                    Runner::Pytest => pytest_id(&name, &lines)?,
                    Runner::Vitest => name,
                    Runner::Rails => self.names.get(&name)?.clone(),
                };
                Some((id, lines.clone()))
            })
            .collect()
    }

    /// `___ test_name ___` blocks under `=== FAILURES ===`.
    fn feed_pytest(&mut self, line: &str) -> Vec<(String, Vec<String>)> {
        if let Some(title) = line.strip_prefix('=').filter(|_| line.ends_with('=')) {
            let done = self.flush();
            let title = title.trim_matches('=').trim();
            self.in_section = title == "FAILURES" || title == "ERRORS";
            return done;
        }
        if !self.in_section {
            return Vec::new();
        }
        if line.starts_with("___") && line.ends_with("___") {
            let done = self.flush();
            let name = line.trim_matches('_').trim();
            let name = ["ERROR at setup of ", "ERROR at teardown of "]
                .iter()
                .find_map(|prefix| name.strip_prefix(prefix))
                .unwrap_or(name);
            self.current = vec![name.to_owned()];
            return done;
        }
        if !self.current.is_empty() {
            self.lines.push(line.to_owned());
        }
        Vec::new()
    }

    /// ` FAIL  file > group > name` headers, several for one shared error,
    /// up to a `⎯⎯⎯` rule.
    fn feed_vitest(&mut self, line: &str) -> Vec<(String, Vec<String>)> {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("FAIL ") {
            let done = if self.lines.is_empty() { Vec::new() } else { self.flush() };
            if let Some((file, name)) = rest.trim().split_once(" > ") {
                self.current.push(format!("{file}::{name}"));
            }
            return done;
        }
        if trimmed.starts_with('⎯') || trimmed.starts_with("Test Files ") {
            return self.flush();
        }
        if !self.current.is_empty() {
            self.lines.push(line.to_owned());
        }
        Vec::new()
    }

    /// `Failure:` or `Error:`, then `Class#method [file:line]:`, up to the
    /// `bin/rails test file:line` that reruns it.
    fn feed_rails(&mut self, line: &str) -> Vec<(String, Vec<String>)> {
        if line == "Failure:" || line == "Error:" {
            let done = self.flush();
            self.expect_name = true;
            return done;
        }
        if std::mem::take(&mut self.expect_name) {
            let name = line.split(" [").next().unwrap_or(line).trim_end_matches(':');
            self.current = vec![name.to_owned()];
            return Vec::new();
        }
        if self.current.is_empty() {
            return Vec::new();
        }
        self.lines.push(line.to_owned());
        if line.starts_with("bin/rails test ") {
            return self.flush();
        }
        Vec::new()
    }
}

/// The node id of the pytest test a failure block titled `name` is about:
/// `Class.test[param]` becomes `Class::test[param]`, in the first test file
/// the traceback points into, or else its first file.
fn pytest_id(name: &str, lines: &[String]) -> Option<String> {
    let (head, params) = name.split_at(name.find('[').unwrap_or(name.len()));
    let files: Vec<&str> = lines
        .iter()
        .filter_map(|line| {
            let (file, rest) = line.split_once(':')?;
            (file.ends_with(".py") && !file.contains(' ') && rest.split(':').next()?.parse::<u32>().is_ok())
                .then_some(file)
        })
        .collect();
    let is_test = |file: &&str| {
        let name = file.rsplit('/').next().unwrap_or(file);
        name.starts_with("test_") || name.ends_with("_test.py")
    };
    let file = files.iter().copied().find(|file| is_test(&file)).or(files.first().copied())?;
    Some(format!("{file}::{}{params}", head.replace('.', "::")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_pytest_output() {
        let collected = "tests/a.py::test_x\ntests/b.py::Group::test_y[1]\n\n420 tests collected in 1.67s\n";
        let tests = parse_pytest_collection(collected);
        assert_eq!(tests.len(), 2);
        assert_eq!((tests[1].file.as_str(), tests[1].name.as_str()), ("tests/b.py", "Group::test_y[1]"));

        assert_eq!(
            parse_pytest_result("tests/a.py::test_x PASSED                [100%]"),
            Some(("tests/a.py::test_x".into(), Outcome::Passed))
        );
        assert_eq!(
            parse_pytest_result("FAILED tests/a.py::test_x - assert 1 == 2"),
            Some(("tests/a.py::test_x".into(), Outcome::Failed))
        );
        assert_eq!(
            parse_pytest_result("[gw0] [ 50%] PASSED tests/a.py::test_x"),
            Some(("tests/a.py::test_x".into(), Outcome::Passed))
        );
        // Parameters with spaces, and with ` - ` inside them.
        let id = "tests/a.py::test_cited[data0-Das, B. M. - 9th ed.]";
        assert_eq!(parse_pytest_result(&format!("{id} PASSED     [ 10%]")), Some((id.into(), Outcome::Passed)));
        assert_eq!(parse_pytest_result(&format!("PASSED {id}")), Some((id.into(), Outcome::Passed)));
        assert_eq!(
            parse_pytest_result(&format!("FAILED {id} - AssertionError: no [cite]")),
            Some((id.into(), Outcome::Failed))
        );
        assert_eq!(
            parse_pytest_result("tests/a.py::test_x[-{a} + 2 ** 2-1.0] SKIPPED (slow) [ 20%]"),
            Some(("tests/a.py::test_x[-{a} + 2 ** 2-1.0]".into(), Outcome::Skipped))
        );
        assert_eq!(parse_pytest_result("collecting ... collected 1 item"), None);
    }

    #[test]
    fn reads_vitest_output() {
        let listed = r#"[{"name":"group > adds","file":"/srv/app/src/a.test.ts","location":{"line":34,"column":3}}]"#;
        let tests = parse_vitest_list(listed, Path::new("/srv/app")).unwrap();
        assert_eq!(
            tests,
            [TestCase {
                id: "src/a.test.ts::group > adds".into(),
                file: "src/a.test.ts".into(),
                name: "group > adds".into(),
                line: Some(34)
            }]
        );
        let colored = strip_ansi(" \u{1b}[32m✓\u{1b}[39m src/a.test.ts\u{1b}[2m > \u{1b}[22mgroup\u{1b}[2m > \u{1b}[22madds\u{1b}[32m 12\u{1b}[2mms\u{1b}[22m\u{1b}[39m");
        assert_eq!(parse_vitest_result(&colored), Some(("src/a.test.ts::group > adds".into(), Outcome::Passed)));
        assert_eq!(
            parse_vitest_result(" × src/a.test.ts > group > takes 2 s 3ms"),
            Some(("src/a.test.ts::group > takes 2 s".into(), Outcome::Failed))
        );
        assert_eq!(parse_vitest_result(" ✓ src/a.test.ts (17 tests) 30ms"), None);
        assert_eq!(
            parse_vitest_result(" ↓ src/a.test.ts:12 > group > adds"),
            Some(("src/a.test.ts::group > adds".into(), Outcome::Skipped))
        );
    }

    #[test]
    fn reads_rails_tests_and_results() {
        let file = r#"
require "test_helper"

module Admin
  class UsersTest < ActiveSupport::TestCase
    test "lists   everyone, even the closed" do
    end

    def test_old_style?
    end
  end
end

class MailerTest < ActionMailer::TestCase
  test 'sends it' do
  end
end
"#;
        let tests = parse_rails_file("test/x_test.rb", file);
        let ids: Vec<_> = tests.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "test/x_test.rb::Admin::UsersTest#test_lists_everyone,_even_the_closed",
                "test/x_test.rb::Admin::UsersTest#test_old_style?",
                "test/x_test.rb::MailerTest#test_sends_it",
            ]
        );
        assert_eq!((tests[0].name.as_str(), tests[0].line), ("lists   everyone, even the closed", Some(6)));

        assert_eq!(
            parse_rails_result("Admin::UsersTest#test_old_style? = 0.12 s = F"),
            Some(("Admin::UsersTest#test_old_style?".into(), Outcome::Failed))
        );
        assert_eq!(parse_rails_result("Finished in 1.2 s = ."), None);
    }

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
        let finished = rx
            .iter()
            .find(|e| matches!(e, TestEvent::Finished { .. }));
        assert_eq!(finished, Some(TestEvent::Finished { success: false }));
    }

    fn failures(runner: Runner, names: &[(&str, &str)], output: &str) -> Vec<(String, Vec<String>)> {
        let names = names.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let mut parser = Failures::new(runner, names);
        let mut found: Vec<_> = output.lines().flat_map(|line| parser.feed(line)).collect();
        found.extend(parser.finish());
        found
    }

    #[test]
    fn reads_pytest_failures() {
        let output = "\
tests/a.py::test_bad FAILED
=================================== FAILURES ===================================
___________________________________ test_bad ___________________________________

    def test_bad():
>       assert 1 == 2
E       assert 1 == 2

tests/a.py:2: AssertionError
_______________________________ Group.test_p[1.5] _______________________________
lib/helper.py:9: in check
tests/test_b.py:7: ValueError
=========================== short test summary info ============================
FAILED tests/a.py::test_bad - assert 1 == 2";
        let found = failures(Runner::Pytest, &[], output);
        let ids: Vec<_> = found.iter().map(|(id, _)| id.as_str()).collect();
        // The first frame in a test file decides; a helper module is not one.
        assert_eq!(ids, ["tests/a.py::test_bad", "tests/test_b.py::Group::test_p[1.5]"]);
        assert_eq!(found[0].1.last().unwrap(), "tests/a.py:2: AssertionError");
    }

    #[test]
    fn reads_vitest_failures() {
        let output = "\
 × src/a.test.ts > group > adds 3ms
⎯⎯⎯⎯⎯⎯⎯ Failed Tests 2 ⎯⎯⎯⎯⎯⎯⎯

 FAIL  src/a.test.ts > group > adds
 FAIL  src/a.test.ts > group > adds again
AssertionError: expected 3 to be 4
 ❯ src/a.test.ts:5:15

⎯⎯⎯⎯⎯⎯⎯[1/1]⎯

 Test Files  1 failed (1)";
        let found = failures(Runner::Vitest, &[], output);
        let ids: Vec<_> = found.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, ["src/a.test.ts::group > adds", "src/a.test.ts::group > adds again"]);
        assert_eq!(found[0].1, ["AssertionError: expected 3 to be 4", " ❯ src/a.test.ts:5:15"]);
    }

    #[test]
    fn reads_rails_failures() {
        let output = "\
UserTest#test_x = 0.01 s = F

Failure:
UserTest#test_x [test/models/user_test.rb:10]:
Expected: 1
  Actual: 2

bin/rails test test/models/user_test.rb:8

Error:
UserTest#test_y:
NameError: nope

bin/rails test test/models/user_test.rb:12
";
        let names = [("UserTest#test_x", "test/models/user_test.rb::UserTest#test_x"), ("UserTest#test_y", "test/models/user_test.rb::UserTest#test_y")];
        let found = failures(Runner::Rails, &names, output);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].0, "test/models/user_test.rb::UserTest#test_x");
        assert_eq!(found[0].1.first().unwrap(), "Expected: 1");
        assert_eq!(found[1].1, ["NameError: nope", "", "bin/rails test test/models/user_test.rb:12"]);
    }
}
