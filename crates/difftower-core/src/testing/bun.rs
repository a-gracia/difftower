//! Bun's test runner, `bun test`.

use std::path::Path;

use anyhow::Result;

use super::jest::{js_tests, some_test_mentions, spaced, test_files};
use super::{
    Found, Kind, Outcome, Parser, Started, TestCase, TestTarget, file_of, quote, read,
    regex_escape, target_args, vitest::strip_duration,
};

pub(crate) struct Bun;

impl Kind for Bun {
    fn detect(&self, root: &Path) -> bool {
        let lock = root.join("bun.lock").is_file() || root.join("bun.lockb").is_file();
        lock && (read(root, "package.json").contains("bun test")
            || read(root, "bunfig.toml").contains("[test]")
            || some_test_mentions(root, "bun:test"))
    }

    fn default_command(&self, _root: &Path) -> String {
        "bun".to_owned()
    }

    fn discover(&self, root: &Path, _command: &str) -> Result<Vec<TestCase>> {
        Ok(js_tests(root, &test_files(root)))
    }

    fn start(&self, _root: &Path, command: &str, target: &TestTarget) -> Result<Started> {
        let args = match target {
            TestTarget::Cases { cases } => {
                let mut files: Vec<String> =
                    cases.iter().map(|c| file_of(&c.id).to_owned()).collect();
                files.dedup();
                let names: Vec<String> =
                    cases.iter().map(|c| regex_escape(&spaced(&c.id))).collect();
                // Bun reads a path without `./` or `/` as a name filter.
                let files = files
                    .iter()
                    .map(|f| quote(&format!("./{f}")))
                    .collect::<Vec<_>>()
                    .join(" ");
                format!("{files} -t {}", quote(&format!("^({})$", names.join("|"))))
            }
            TestTarget::Paths { paths } => paths
                .iter()
                .map(|p| quote(&format!("./{p}")))
                .collect::<Vec<_>>()
                .join(" "),
            TestTarget::All => target_args(target, |case| case.id.clone()),
        };
        // Bun prints only failures when it sees it runs under an AI agent,
        // as when Difftower was opened from one.
        Ok(Started {
            script: format!("env -u CLAUDECODE {command} test {args}"),
            parser: Box::new(BunParser::default()),
        })
    }
}

/// A `file:` line before each file's tests, then `(pass) group > name
/// [0.12ms]` for each test, the error of a failed test printed before it.
#[derive(Default)]
struct BunParser {
    file: String,
    /// What was printed since the last result: a failure's error.
    lines: Vec<String>,
}

impl Parser for BunParser {
    fn feed(&mut self, line: &str) -> Vec<Found> {
        if let Some(file) = line
            .strip_suffix(':')
            .filter(|f| !f.contains(' ') && super::jest::is_script(f))
        {
            self.file = file.strip_prefix("./").unwrap_or(file).to_owned();
            self.lines.clear();
            return Vec::new();
        }
        let marks = [
            ("(pass) ", Outcome::Passed),
            ("(fail) ", Outcome::Failed),
            ("(skip) ", Outcome::Skipped),
            ("(todo) ", Outcome::Skipped),
        ];
        let Some((rest, outcome)) = marks
            .iter()
            .find_map(|(mark, outcome)| Some((line.strip_prefix(mark)?, *outcome)))
        else {
            self.lines.push(line.to_owned());
            return Vec::new();
        };
        // ` [0.12ms]` after the name.
        let name = match rest.rsplit_once(" [") {
            Some((name, time)) if time.ends_with(']') => name,
            _ => strip_duration(rest),
        };
        let id = format!("{}::{name}", self.file);
        let lines = std::mem::take(&mut self.lines);
        let mut found = Vec::new();
        if outcome == Outcome::Failed {
            let mut lines: Vec<String> = lines
                .into_iter()
                .skip_while(|l| l.trim().is_empty())
                .collect();
            while lines.last().is_some_and(|l| l.trim().is_empty()) {
                lines.pop();
            }
            found.push(Found::Failure(id.clone(), lines));
        }
        found.push(Found::Result(id, outcome));
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::test_support::{failures, parse, results};

    #[test]
    fn reads_bun_output() {
        // From `bun test` 1.4.
        let output = "\
bun test v1.4.3 (c6da4a4d3)

test/math.test.ts:
(pass) math > adds [0.15ms]
8 |     it(\"fails here\", () => {
9 |       expect(1 + 2).toBe(4);
                        ^
error: expect(received).toBe(expected)

Expected: 4
Received: 3

      at <anonymous> (/srv/app/test/math.test.ts:9:21)
(fail) math > nested group > fails here [2.34ms]
(skip) math > nested group > is skipped
(pass) top level [0.23ms]

 2 pass
 1 skip
 1 fail";
        let found = parse(Box::new(BunParser::default()), output);
        assert_eq!(
            results(&found),
            [
                ("test/math.test.ts::math > adds", Outcome::Passed),
                (
                    "test/math.test.ts::math > nested group > fails here",
                    Outcome::Failed
                ),
                (
                    "test/math.test.ts::math > nested group > is skipped",
                    Outcome::Skipped
                ),
                ("test/math.test.ts::top level", Outcome::Passed),
            ]
        );
        let failed = failures(&found);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].1[0], "8 |     it(\"fails here\", () => {");
        assert_eq!(
            failed[0].1.last().unwrap(),
            "      at <anonymous> (/srv/app/test/math.test.ts:9:21)"
        );
    }
}
