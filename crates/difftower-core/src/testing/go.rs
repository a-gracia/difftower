//! Go's `go test`.
//!
//! Tests are read from the `_test.go` files: every `func TestX(t
//! *testing.T)` and `func FuzzX(f *testing.F)`, its id the file and name:
//! `calc/calc_test.go::TestAdd`. Subtests run with their test and are not
//! listed apart.
//!
//! `go test -v` names a test without its package, and prints the package
//! only when it is done, so results are held until that line says which
//! directory they belong to.

use std::{collections::HashMap, path::Path};

use anyhow::Result;

use super::{Found, Kind, Outcome, Parser, Started, TestCase, TestTarget, quote, read, walk};

pub(crate) struct Go;

impl Kind for Go {
    fn detect(&self, root: &Path) -> bool {
        root.join("go.mod").is_file() || root.join("go.work").is_file()
    }

    fn default_command(&self, _root: &Path) -> String {
        "go test".to_owned()
    }

    fn discover(&self, root: &Path, _command: &str) -> Result<Vec<TestCase>> {
        Ok(scan(root))
    }

    fn start(&self, root: &Path, command: &str, target: &TestTarget) -> Result<Started> {
        let tests = scan(root);
        Ok(Started { script: script(command, &tests, target), parser: Box::new(GoParser::new(root, &tests)) })
    }
}

/// Every test in the `_test.go` files under `root`.
fn scan(root: &Path) -> Vec<TestCase> {
    walk(root, |file| file.ends_with("_test.go") && !file.split('/').any(|part| part == "testdata"))
        .iter()
        .flat_map(|file| parse_file(file, &read(root, file)))
        .collect()
}

fn parse_file(file: &str, text: &str) -> Vec<TestCase> {
    text.lines()
        .enumerate()
        .filter_map(|(ix, line)| {
            let rest = line.strip_prefix("func ")?;
            let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            let args = rest[name.len()..].trim_start().strip_prefix('(')?;
            let kind = ["Test", "Fuzz"].into_iter().find(|kind| name.starts_with(kind))?;
            // `TestX`, not `Testify`: what follows the prefix is not a lowercase letter.
            if name[kind.len()..].starts_with(|c: char| c.is_lowercase()) {
                return None;
            }
            let param = if kind == "Test" { "*testing.T" } else { "*testing.F" };
            args.replace(' ', "").contains(param).then(|| TestCase {
                id: format!("{file}::{name}"),
                file: file.to_owned(),
                name,
                line: Some(ix as u32 + 1),
            })
        })
        .collect()
}

/// The directory of a file, `.` for the root.
fn dir_of(file: &str) -> &str {
    file.rsplit_once('/').map_or(".", |(dir, _)| dir)
}

/// The package argument for a directory: `./calc`, or `.`.
fn package_arg(dir: &str) -> String {
    if dir == "." { ".".to_owned() } else { format!("./{dir}") }
}

/// `-run` matching exactly `names`.
fn run_filter(names: &[String]) -> String {
    quote(&format!("^({})$", names.join("|")))
}

fn script(command: &str, tests: &[TestCase], target: &TestTarget) -> String {
    let base = format!("{command} -v");
    let mut commands: Vec<String> = Vec::new();
    // Tests to run by name, per package directory, in order.
    let mut by_dir: Vec<(String, Vec<String>)> = Vec::new();
    let mut add = |dir: &str, name: &str| match by_dir.iter_mut().find(|(d, _)| d == dir) {
        Some((_, names)) if !names.iter().any(|n| n == name) => names.push(name.to_owned()),
        Some(_) => {}
        None => by_dir.push((dir.to_owned(), vec![name.to_owned()])),
    };
    match target {
        TestTarget::All => return format!("{base} ./..."),
        TestTarget::Paths { paths } => {
            for path in paths {
                let path = path.trim_end_matches('/');
                if path.is_empty() || path == "." {
                    return format!("{base} ./...");
                }
                if path.ends_with(".go") {
                    for test in tests.iter().filter(|t| t.file == path) {
                        add(dir_of(&test.file), &test.name);
                    }
                } else {
                    commands.push(format!("{base} {}", quote(&format!("./{path}/..."))));
                }
            }
        }
        TestTarget::Cases { cases } => {
            for case in cases {
                let (file, name) = case.id.split_once("::").unwrap_or(("", case.id.as_str()));
                add(dir_of(file), name);
            }
        }
    }
    for (dir, names) in &by_dir {
        commands.push(format!("{base} {} -run {}", quote(&package_arg(dir)), run_filter(names)));
    }
    match commands.len() {
        0 => format!("{base} ./..."),
        1 => commands.remove(0),
        _ => {
            // Every command runs; the run fails if any does.
            let mut script = "s=0".to_owned();
            for cmd in commands {
                script.push_str(&format!("; {cmd} || s=1"));
            }
            script.push_str("; exit $s");
            script
        }
    }
}

/// Reads `go test -v`: `=== RUN   TestX`, the test's output, `--- PASS:
/// TestX (0.00s)` (or `FAIL`, `SKIP`), and at last `ok  \tpkg` or
/// `FAIL\tpkg` naming the package of everything before.
struct GoParser {
    /// Import path prefix to directory, per module: `example.com/app` to `.`.
    modules: Vec<(String, String)>,
    /// (directory, name) to test id.
    ids: HashMap<(String, String), String>,
    /// The test the output being read belongs to.
    current: Option<String>,
    /// Outcomes of the package being read, in order.
    outcomes: Vec<(String, Outcome)>,
    /// What each test of the package printed.
    output: HashMap<String, Vec<String>>,
}

impl GoParser {
    fn new(root: &Path, tests: &[TestCase]) -> Self {
        let ids = tests.iter().map(|t| ((dir_of(&t.file).to_owned(), t.name.clone()), t.id.clone())).collect();
        Self { modules: modules(root), ids, current: None, outcomes: Vec::new(), output: HashMap::new() }
    }

    /// The directory of the package at `import`, relative to the root.
    fn dir(&self, import: &str) -> Option<String> {
        self.modules
            .iter()
            .filter_map(|(module, dir)| {
                let rest = import.strip_prefix(module.as_str())?;
                let rest = if rest.is_empty() { "" } else { rest.strip_prefix('/')? };
                Some((module.len(), match (dir.as_str(), rest) {
                    (dir, "") => dir.to_owned(),
                    (".", rest) => rest.to_owned(),
                    (dir, rest) => format!("{dir}/{rest}"),
                }))
            })
            .max_by_key(|(len, _)| *len)
            .map(|(_, dir)| dir)
    }

    /// The package `import` is done: what its tests did.
    fn package_done(&mut self, import: &str) -> Vec<Found> {
        let dir = self.dir(import);
        let outcomes = std::mem::take(&mut self.outcomes);
        let mut output = std::mem::take(&mut self.output);
        self.current = None;
        let Some(dir) = dir else { return Vec::new() };
        let mut found = Vec::new();
        for (name, outcome) in outcomes {
            let Some(id) = self.ids.get(&(dir.clone(), name.clone())) else { continue };
            found.push(Found::Result(id.clone(), outcome));
            if outcome == Outcome::Failed {
                let mut lines = output.remove(&name).unwrap_or_default();
                while lines.last().is_some_and(|l| l.trim().is_empty()) {
                    lines.pop();
                }
                found.push(Found::Failure(id.clone(), lines));
            }
        }
        found
    }
}

/// The modules under `root`: their import paths and directories.
fn modules(root: &Path) -> Vec<(String, String)> {
    let mut dirs = vec![".".to_owned()];
    // A workspace lists its modules: `use ./api` or a `use ( … )` block.
    let work = read(root, "go.work");
    let mut in_block = false;
    for line in work.lines() {
        let line = line.split("//").next().unwrap_or("").trim();
        let entry = if in_block {
            if line == ")" {
                in_block = false;
                continue;
            }
            Some(line)
        } else if line == "use (" {
            in_block = true;
            None
        } else {
            line.strip_prefix("use ")
        };
        if let Some(dir) = entry.filter(|d| !d.is_empty() && *d != "(") {
            let dir = dir.trim_start_matches("./").trim_end_matches('/');
            dirs.push(if dir.is_empty() { ".".to_owned() } else { dir.to_owned() });
        }
    }
    dirs.into_iter()
        .filter_map(|dir| {
            let path = if dir == "." { "go.mod".to_owned() } else { format!("{dir}/go.mod") };
            let module = read(root, &path).lines().find_map(|l| l.trim().strip_prefix("module ").map(|m| m.trim().trim_matches('"').to_owned()))?;
            Some((module, dir))
        })
        .collect()
}

/// The top-level test of a test or subtest name: `TestX/case` is `TestX`.
fn top(name: &str) -> &str {
    name.split('/').next().unwrap_or(name)
}

impl Parser for GoParser {
    fn feed(&mut self, line: &str) -> Vec<Found> {
        // `ok  \texample.com/app/calc\t0.02s`, `FAIL\texample.com/app [build
        // failed]`, `?   \texample.com/app/cmd\t[no test files]`.
        for prefix in ["ok  \t", "FAIL\t", "?   \t"] {
            if let Some(rest) = line.strip_prefix(prefix) {
                let import = rest.split(['\t', ' ']).next().unwrap_or(rest);
                return self.package_done(import);
            }
        }
        let trimmed = line.trim_start();
        for frame in ["=== RUN   ", "=== CONT  ", "=== PAUSE ", "=== NAME  "] {
            if let Some(name) = trimmed.strip_prefix(frame) {
                self.current = Some(top(name.trim()).to_owned());
                return Vec::new();
            }
        }
        for (frame, outcome) in [("--- PASS: ", Outcome::Passed), ("--- FAIL: ", Outcome::Failed), ("--- SKIP: ", Outcome::Skipped)] {
            if let Some(rest) = trimmed.strip_prefix(frame) {
                let name = rest.split(" (").next().unwrap_or(rest);
                // A subtest's line is indented: its test reports for it.
                if line.starts_with("--- ") && !name.contains('/') {
                    self.outcomes.push((name.to_owned(), outcome));
                    // A panic is printed after the line; it stays with the test.
                    self.current = Some(name.to_owned());
                } else if let Some(current) = &self.current {
                    self.output.entry(current.clone()).or_default().push(line.to_owned());
                }
                return Vec::new();
            }
        }
        if line == "PASS" || line == "FAIL" {
            return Vec::new();
        }
        if let Some(current) = &self.current {
            self.output.entry(current.clone()).or_default().push(line.to_owned());
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{
        CaseRef,
        test_support::{failures, parse, results},
    };

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let files: &[(&str, &str)] = &[
            ("go.mod", "module example.com/demo\n\ngo 1.22\n"),
            ("root_test.go", "package demo\n\nimport \"testing\"\n\nfunc TestRoot(t *testing.T) {}\n"),
            (
                "calc/calc_test.go",
                "package calc\n\nimport \"testing\"\n\nfunc TestAdd(t *testing.T) {\n}\n\nfunc TestAddFails(t *testing.T) {\n}\n\nfunc TestSkipped(t *testing.T) {\n}\n\nfunc TestTable(t *testing.T) {\n}\n\nfunc Testify(t *testing.T) {}\nfunc TestMain(m *testing.M) {}\nfunc FuzzParse(f *testing.F) {}\n",
            ),
            (
                "internal/text/text_test.go",
                "package text_test\n\nimport (\n\t\"testing\"\n)\n\nfunc TestUpper(t *testing.T) { t.Log(\"hello\") }\nfunc TestPanics(t *testing.T) { var m map[string]int; m[\"x\"] = 1 }\n",
            ),
            ("internal/text/testdata/x_test.go", "func TestIgnored(t *testing.T) {}\n"),
        ];
        for (path, text) in files {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        dir
    }

    #[test]
    fn lists_tests_from_the_files() {
        let dir = fixture();
        let ids: Vec<String> = scan(dir.path()).into_iter().map(|t| t.id).collect();
        assert_eq!(
            ids,
            [
                "calc/calc_test.go::TestAdd",
                "calc/calc_test.go::TestAddFails",
                "calc/calc_test.go::TestSkipped",
                "calc/calc_test.go::TestTable",
                "calc/calc_test.go::FuzzParse",
                "internal/text/text_test.go::TestUpper",
                "internal/text/text_test.go::TestPanics",
                "root_test.go::TestRoot",
            ]
        );
    }

    #[test]
    fn runs_what_is_asked() {
        let dir = fixture();
        let tests = scan(dir.path());
        assert_eq!(script("go test", &tests, &TestTarget::All), "go test -v ./...");
        let cases = TestTarget::Cases {
            cases: ["calc/calc_test.go::TestAdd", "calc/calc_test.go::TestTable", "root_test.go::TestRoot"]
                .iter()
                .map(|id| CaseRef { id: id.to_string(), line: None })
                .collect(),
        };
        assert_eq!(
            script("go test", &tests, &cases),
            "s=0; go test -v './calc' -run '^(TestAdd|TestTable)$' || s=1; go test -v '.' -run '^(TestRoot)$' || s=1; exit $s"
        );
        let paths = TestTarget::Paths { paths: vec!["internal".into()] };
        assert_eq!(script("go test", &tests, &paths), "go test -v './internal/...'");
        let file = TestTarget::Paths { paths: vec!["internal/text/text_test.go".into()] };
        assert_eq!(script("go test", &tests, &file), "go test -v './internal/text' -run '^(TestUpper|TestPanics)$'");
    }

    /// What `go test -v ./...` printed for the fixture, the panic's trace cut.
    const OUTPUT: &str = "\
=== RUN   TestRoot
--- PASS: TestRoot (0.00s)
PASS
ok  \texample.com/demo\t0.016s
=== RUN   TestAdd
--- PASS: TestAdd (0.00s)
=== RUN   TestAddFails
    calc_test.go:14: Add(1, 2) = 3, want 4
--- FAIL: TestAddFails (0.00s)
=== RUN   TestSkipped
    calc_test.go:19: not yet
--- SKIP: TestSkipped (0.00s)
=== RUN   TestTable
=== RUN   TestTable/one_plus_two
=== RUN   TestTable/bad
    calc_test.go:26: want 5
--- FAIL: TestTable (0.00s)
    --- PASS: TestTable/one_plus_two (0.00s)
    --- FAIL: TestTable/bad (0.00s)
FAIL
FAIL\texample.com/demo/calc\t0.026s
=== RUN   TestUpper
    text_test.go:7: hello
--- PASS: TestUpper (0.00s)
=== RUN   TestPanics
--- FAIL: TestPanics (0.00s)
panic: assignment to entry in nil map [recovered, repanicked]

goroutine 3 [running]:
example.com/demo/internal/text_test.TestPanics(0x3d1852d3c008?)
\t/tmp/x/internal/text/text_test.go:8 +0x28
FAIL\texample.com/demo/internal/text\t0.028s
FAIL
";

    #[test]
    fn reads_results_and_failures_by_package() {
        let dir = fixture();
        let found = parse(Box::new(GoParser::new(dir.path(), &scan(dir.path()))), OUTPUT);
        assert_eq!(
            results(&found),
            [
                ("root_test.go::TestRoot", Outcome::Passed),
                ("calc/calc_test.go::TestAdd", Outcome::Passed),
                ("calc/calc_test.go::TestAddFails", Outcome::Failed),
                ("calc/calc_test.go::TestSkipped", Outcome::Skipped),
                ("calc/calc_test.go::TestTable", Outcome::Failed),
                ("internal/text/text_test.go::TestUpper", Outcome::Passed),
                ("internal/text/text_test.go::TestPanics", Outcome::Failed),
            ]
        );
        let failed = failures(&found);
        assert_eq!(failed[0], ("calc/calc_test.go::TestAddFails", &["    calc_test.go:14: Add(1, 2) = 3, want 4".to_owned()][..]));
        assert_eq!(
            failed[1].1,
            [
                "    calc_test.go:26: want 5",
                "    --- PASS: TestTable/one_plus_two (0.00s)",
                "    --- FAIL: TestTable/bad (0.00s)"
            ]
        );
        assert_eq!(failed[2].1[0], "panic: assignment to entry in nil map [recovered, repanicked]");
        assert_eq!(failed[2].1.last().unwrap(), "\t/tmp/x/internal/text/text_test.go:8 +0x28");
    }

    #[test]
    fn finds_packages_of_a_workspace() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.work"), "go 1.22\n\nuse (\n\t./api\n\t./web // the site\n)\n").unwrap();
        for (d, m) in [("api", "example.com/api"), ("web", "example.com/web")] {
            std::fs::create_dir(dir.path().join(d)).unwrap();
            std::fs::write(dir.path().join(d).join("go.mod"), format!("module {m}\n")).unwrap();
        }
        let parser = GoParser::new(dir.path(), &[]);
        assert_eq!(parser.dir("example.com/api/handlers").as_deref(), Some("api/handlers"));
        assert_eq!(parser.dir("example.com/web").as_deref(), Some("web"));
        assert_eq!(parser.dir("other.org/x"), None);
    }
}
