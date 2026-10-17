//! pytest, which also runs unittest's tests.

use std::path::Path;

use anyhow::{Result, bail};

use super::{Blocks, Found, Kind, Outcome, Parser, Started, TestCase, TestTarget, read, shell_run, tail, target_args};

pub(crate) struct Pytest;

impl Kind for Pytest {
    fn detect(&self, root: &Path) -> bool {
        let python = ["pyproject.toml", "setup.py", "setup.cfg", "requirements.txt", "pytest.ini", "tox.ini"];
        let config = read(root, "pyproject.toml").contains("[tool.pytest")
            || root.join("pytest.ini").is_file()
            || root.join("conftest.py").is_file();
        let tests = ["tests", "test"].iter().any(|dir| root.join(dir).is_dir())
            && python.iter().any(|name| root.join(name).is_file());
        config || tests
    }

    fn default_command(&self, root: &Path) -> String {
        ["venv", ".venv", "env"]
            .iter()
            .rev()
            .find(|dir| root.join(dir).join("bin/python").is_file())
            .map_or("python3 -m pytest".to_owned(), |dir| format!("{dir}/bin/python -m pytest"))
    }

    fn discover(&self, root: &Path, command: &str) -> Result<Vec<TestCase>> {
        // A file that does not import, often one an agent is editing, must
        // not hide every other test.
        let script = format!("{command} --collect-only -q -p no:cacheprovider --continue-on-collection-errors");
        let (out, ok) = shell_run(root, &script)?;
        let tests = parse_collection(&out);
        if !ok && tests.is_empty() {
            bail!("`{script}` failed: {}", tail(&out));
        }
        Ok(tests)
    }

    fn start(&self, _root: &Path, command: &str, target: &TestTarget) -> Result<Started> {
        let args = target_args(target, |case| case.id.clone());
        Ok(Started {
            script: format!("{command} -v -rA --color=no {args}"),
            parser: Box::new(PytestParser::default()),
        })
    }
}

#[derive(Default)]
struct PytestParser {
    /// Inside the FAILURES or ERRORS section.
    in_section: bool,
    /// The block being read, titled as pytest titles it.
    title: Option<String>,
    blocks: Blocks,
}

impl PytestParser {
    /// Ends the block being read, naming its test by its traceback.
    fn end(&mut self) -> Vec<Found> {
        let Some(title) = self.title.take() else { return self.blocks.end() };
        let found = self.blocks.end();
        let lines = match found.first() {
            Some(Found::Failure(_, lines)) => lines.clone(),
            _ => Vec::new(),
        };
        pytest_id(&title, &lines).map(|id| Found::Failure(id, lines)).into_iter().collect()
    }
}

impl Parser for PytestParser {
    fn feed(&mut self, line: &str) -> Vec<Found> {
        let mut found: Vec<Found> = parse_result(line).map(|(id, o)| Found::Result(id, o)).into_iter().collect();
        // `___ test_name ___` blocks under `=== FAILURES ===`.
        if let Some(title) = line.strip_prefix('=').filter(|_| line.ends_with('=')) {
            found.extend(self.end());
            let title = title.trim_matches('=').trim();
            self.in_section = title == "FAILURES" || title == "ERRORS";
            return found;
        }
        if !self.in_section {
            return found;
        }
        if line.starts_with("___") && line.ends_with("___") {
            found.extend(self.end());
            let name = line.trim_matches('_').trim();
            let name = ["ERROR at setup of ", "ERROR at teardown of "]
                .iter()
                .find_map(|prefix| name.strip_prefix(prefix))
                .unwrap_or(name);
            self.title = Some(name.to_owned());
            // A placeholder id: the real one comes from the traceback.
            self.blocks.start(vec![String::new()]);
            return found;
        }
        self.blocks.push(line);
        found
    }

    fn finish(&mut self) -> Vec<Found> {
        self.end()
    }
}

fn parse_collection(out: &str) -> Vec<TestCase> {
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
fn parse_result(line: &str) -> Option<(String, Outcome)> {
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
    let is_test = |file: &str| {
        let name = file.rsplit('/').next().unwrap_or(file);
        name.starts_with("test_") || name.ends_with("_test.py")
    };
    let file = files.iter().copied().find(|file| is_test(file)).or(files.first().copied())?;
    Some(format!("{file}::{}{params}", head.replace('.', "::")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::test_support::{failures, parse};

    #[test]
    fn reads_pytest_output() {
        let collected = "tests/a.py::test_x\ntests/b.py::Group::test_y[1]\n\n420 tests collected in 1.67s\n";
        let tests = parse_collection(collected);
        assert_eq!(tests.len(), 2);
        assert_eq!((tests[1].file.as_str(), tests[1].name.as_str()), ("tests/b.py", "Group::test_y[1]"));

        assert_eq!(
            parse_result("tests/a.py::test_x PASSED                [100%]"),
            Some(("tests/a.py::test_x".into(), Outcome::Passed))
        );
        assert_eq!(
            parse_result("FAILED tests/a.py::test_x - assert 1 == 2"),
            Some(("tests/a.py::test_x".into(), Outcome::Failed))
        );
        assert_eq!(
            parse_result("[gw0] [ 50%] PASSED tests/a.py::test_x"),
            Some(("tests/a.py::test_x".into(), Outcome::Passed))
        );
        // Parameters with spaces, and with ` - ` inside them.
        let id = "tests/a.py::test_cited[data0-Das, B. M. - 9th ed.]";
        assert_eq!(parse_result(&format!("{id} PASSED     [ 10%]")), Some((id.into(), Outcome::Passed)));
        assert_eq!(parse_result(&format!("PASSED {id}")), Some((id.into(), Outcome::Passed)));
        assert_eq!(
            parse_result(&format!("FAILED {id} - AssertionError: no [cite]")),
            Some((id.into(), Outcome::Failed))
        );
        assert_eq!(
            parse_result("tests/a.py::test_x[-{a} + 2 ** 2-1.0] SKIPPED (slow) [ 20%]"),
            Some(("tests/a.py::test_x[-{a} + 2 ** 2-1.0]".into(), Outcome::Skipped))
        );
        assert_eq!(parse_result("collecting ... collected 1 item"), None);
    }

    #[test]
    fn reads_pytest_failures() {
        let output = "\
=================================== FAILURES ===================================
___________________________________ test_bad ___________________________________

    def test_bad():
>       assert 1 == 2
E       assert 1 == 2

tests/a.py:2: AssertionError
_______________________________ Group.test_p[1.5] _______________________________
lib/helper.py:9: in check
tests/test_b.py:7: ValueError
=========================== short test summary info ============================";
        let found = parse(Box::new(PytestParser::default()), output);
        let found = failures(&found);
        let ids: Vec<_> = found.iter().map(|(id, _)| *id).collect();
        // The first frame in a test file decides; a helper module is not one.
        assert_eq!(ids, ["tests/a.py::test_bad", "tests/test_b.py::Group::test_p[1.5]"]);
        assert_eq!(found[0].1.last().unwrap(), "tests/a.py:2: AssertionError");
    }
}
