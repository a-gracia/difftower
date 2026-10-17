//! Deno's test runner, `deno test`: `Deno.test` and its steps, and
//! `describe`/`it` from `@std/testing/bdd`.

use std::{collections::HashMap, path::Path};

use anyhow::Result;

use super::jest::{is_script, js_tests};
use super::{
    Blocks, Found, Kind, Outcome, Parser, Started, TestCase, TestTarget, file_of, quote,
    regex_escape, target_args, walk,
};

pub(crate) struct Deno;

impl Kind for Deno {
    fn detect(&self, root: &Path) -> bool {
        root.join("deno.json").is_file() || root.join("deno.jsonc").is_file()
    }

    fn default_command(&self, _root: &Path) -> String {
        "deno".to_owned()
    }

    fn discover(&self, root: &Path, _command: &str) -> Result<Vec<TestCase>> {
        Ok(js_tests(root, &walk(root, is_deno_test)))
    }

    fn start(&self, _root: &Path, command: &str, target: &TestTarget) -> Result<Started> {
        let args = match target {
            TestTarget::Cases { cases } => {
                // Deno filters by the name of a top-level test; its steps
                // run with it.
                let mut files: Vec<String> =
                    cases.iter().map(|c| file_of(&c.id).to_owned()).collect();
                files.dedup();
                let mut names: Vec<String> = cases
                    .iter()
                    .map(|c| {
                        let name =
                            c.id.split_once("::")
                                .map_or(c.id.as_str(), |(_, name)| name);
                        regex_escape(name.split(" > ").next().unwrap_or(name))
                    })
                    .collect();
                names.dedup();
                let files = files.iter().map(|f| quote(f)).collect::<Vec<_>>().join(" ");
                format!(
                    "--filter {} {files}",
                    quote(&format!("/^({})$/", names.join("|")))
                )
            }
            _ => target_args(target, |case| case.id.clone()),
        };
        Ok(Started {
            script: format!("{command} test --allow-all {args}"),
            parser: Box::new(DenoParser::default()),
        })
    }
}

/// `test.ts`, `a_test.ts` or `a.test.ts`, as `deno test` finds them.
fn is_deno_test(file: &str) -> bool {
    let name = file.rsplit('/').next().unwrap_or(file);
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    is_script(file) && (stem == "test" || stem.ends_with("_test") || stem.ends_with(".test"))
}

/// `running 3 tests from ./a_test.ts`, then `name ... ok (3ms)`, steps
/// indented under their test, and an ERRORS section where each failure is
/// headed `test ... step => location`.
#[derive(Default)]
struct DenoParser {
    file: String,
    /// The tests and steps open at each indentation level.
    stack: Vec<String>,
    /// Full names, as the ERRORS section writes them, to ids.
    ids: HashMap<String, String>,
    in_errors: bool,
    blocks: Blocks,
}

impl Parser for DenoParser {
    fn feed(&mut self, line: &str) -> Vec<Found> {
        if let Some(rest) = line.strip_prefix("running ")
            && let Some((_, file)) = rest.split_once(" from ") {
                self.file = file
                    .trim()
                    .strip_prefix("./")
                    .unwrap_or(file.trim())
                    .to_owned();
                self.stack.clear();
                return Vec::new();
            }
        if line.trim() == "ERRORS" {
            self.in_errors = true;
            return Vec::new();
        }
        if self.in_errors {
            if line.trim() == "FAILURES" {
                self.in_errors = false;
                return self.blocks.end();
            }
            // `with steps ... failing step => ./a_test.ts:10:11`.
            if let Some((name, _)) = line.split_once(" => ").filter(|_| !line.starts_with(' ')) {
                let id = self.ids.get(name).cloned().into_iter().collect();
                return self.blocks.start(id);
            }
            self.blocks.push(line);
            return Vec::new();
        }
        let level = (line.len() - line.trim_start().len()) / 2;
        let text = line.trim_start();
        if let Some(name) = text.strip_suffix(" ...") {
            self.stack.truncate(level);
            self.stack.push(name.to_owned());
            return Vec::new();
        }
        let Some(at) = text.rfind(" ... ") else {
            return Vec::new();
        };
        let (name, result) = (&text[..at], &text[at + 5..]);
        let outcome = if result.starts_with("ok") {
            Outcome::Passed
        } else if result.starts_with("FAILED") {
            Outcome::Failed
        } else if result.starts_with("ignored") {
            Outcome::Skipped
        } else {
            return Vec::new();
        };
        self.stack.truncate(level);
        let mut names = self.stack.clone();
        names.push(name.to_owned());
        let id = format!("{}::{}", self.file, names.join(" > "));
        self.ids.insert(names.join(" ... "), id.clone());
        vec![Found::Result(id, outcome)]
    }

    fn finish(&mut self) -> Vec<Found> {
        self.blocks.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::test_support::{failures, parse, results};

    #[test]
    fn reads_deno_output() {
        // From `deno test` 2.9.
        let output = "\
Check tests/math_test.ts
running 4 tests from ./tests/math_test.ts
adds ... ok (11ms)
with steps ...
  first step ... ok (1ms)
  failing step ... FAILED (322ms)
with steps ... FAILED (due to 1 failed step) (336ms)
object form ... ignored (0ms)
group ...
  bdd fails ... FAILED (16ms)
group ... FAILED (due to 1 failed step) (36ms)

 ERRORS

with steps ... failing step => ./tests/math_test.ts:10:11
error: AssertionError: Values are not equal.

-   1
+   2
    at file:///srv/app/tests/math_test.ts:11:5

group ... bdd fails => https://jsr.io/@std/testing/1.0.21/_test_suite.ts:415:15
error: AssertionError: Values are not equal.

 FAILURES

with steps ... failing step => ./tests/math_test.ts:10:11

FAILED | 1 passed (2 steps) | 2 failed (2 steps) | 1 ignored (539ms)";
        let found = parse(Box::new(DenoParser::default()), output);
        assert_eq!(
            results(&found),
            [
                ("tests/math_test.ts::adds", Outcome::Passed),
                (
                    "tests/math_test.ts::with steps > first step",
                    Outcome::Passed
                ),
                (
                    "tests/math_test.ts::with steps > failing step",
                    Outcome::Failed
                ),
                ("tests/math_test.ts::with steps", Outcome::Failed),
                ("tests/math_test.ts::object form", Outcome::Skipped),
                ("tests/math_test.ts::group > bdd fails", Outcome::Failed),
                ("tests/math_test.ts::group", Outcome::Failed),
            ]
        );
        let failed = failures(&found);
        assert_eq!(failed.len(), 2);
        assert_eq!(failed[0].0, "tests/math_test.ts::with steps > failing step");
        assert_eq!(
            failed[0].1.first().unwrap(),
            "error: AssertionError: Values are not equal."
        );
        assert_eq!(failed[1].0, "tests/math_test.ts::group > bdd fails");
        assert!(
            is_deno_test("src/a_test.ts")
                && is_deno_test("test.ts")
                && !is_deno_test("src/latest.ts")
        );
    }
}
