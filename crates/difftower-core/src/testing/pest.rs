//! Pest, on PHPUnit.
//!
//! Tests are read from the `*Test.php` files: `test()`, `it()` and the
//! `describe()` blocks around them, named as Pest names them. A run prints
//! Pest's output, read as it comes, and writes a JUnit report, read at the
//! end for each test's outcome and failure.

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::{
    Found, Kind, Outcome, Parser, Started, TestCase, TestTarget, junit,
    phpunit::{aggregate, base_name, php_args, relative},
    quote, read, report_path, vitest::strip_duration, walk,
};

pub(crate) struct Pest;

impl Kind for Pest {
    fn detect(&self, root: &Path) -> bool {
        read(root, "composer.json").contains("pestphp/pest")
    }

    fn default_command(&self, root: &Path) -> String {
        if root.join("vendor/bin/pest").is_file() { "vendor/bin/pest" } else { "pest" }.to_owned()
    }

    fn discover(&self, root: &Path, _command: &str) -> Result<Vec<TestCase>> {
        Ok(tests(root))
    }

    fn start(&self, root: &Path, command: &str, target: &TestTarget) -> Result<Started> {
        let report = report_path("pest.xml");
        Ok(Started {
            script: format!(
                "{command} --colors=never --log-junit {} {}",
                quote(&report.display().to_string()),
                php_args(target, |id| id.split_once("::").map_or(id, |(_, name)| name).to_owned())
            ),
            parser: Box::new(PestParser { root: root.to_path_buf(), report, tests: tests(root), file: None }),
        })
    }
}

fn tests(root: &Path) -> Vec<TestCase> {
    walk(root, |file| file.starts_with("tests/") && file.ends_with("Test.php"))
        .iter()
        .flat_map(|file| parse_file(file, &std::fs::read_to_string(root.join(file)).unwrap_or_default()))
        .collect()
}

/// Pest's names: `adds` for `test('adds')`, `it adds` for `it('adds')`, and
/// `` `group` → it adds`` inside `describe('group')`.
fn parse_file(file: &str, text: &str) -> Vec<TestCase> {
    let mut groups: Vec<(usize, String)> = Vec::new();
    let mut tests = Vec::new();
    for (ix, line) in text.lines().enumerate() {
        let indent = line.len() - line.trim_start().len();
        let trimmed = line.trim_start();
        let (word, rest) = if let Some(rest) = trimmed.strip_prefix("describe(") {
            ("describe", rest)
        } else if let Some(rest) = trimmed.strip_prefix("test(") {
            ("test", rest)
        } else if let Some(rest) = trimmed.strip_prefix("it(") {
            ("it", rest)
        } else {
            continue;
        };
        let Some(text) = string(rest) else { continue };
        groups.retain(|(level, _)| *level < indent);
        let mut name: String = groups.iter().map(|(_, g)| format!("`{g}` → ")).collect();
        match word {
            "describe" => {
                groups.push((indent, text));
                continue;
            }
            "it" => name.push_str(&format!("it {text}")),
            _ => name.push_str(&text),
        }
        tests.push(TestCase { id: format!("{file}::{name}"), file: file.to_owned(), name, line: Some(ix as u32 + 1) });
    }
    tests
}

/// The PHP string literal `rest` opens with.
fn string(rest: &str) -> Option<String> {
    let mut chars = rest.trim_start().chars();
    let quote = chars.next().filter(|c| *c == '\'' || *c == '"')?;
    let mut out = String::new();
    let mut escaped = false;
    for c in chars {
        if escaped {
            if c != quote && c != '\\' {
                out.push('\\');
            }
            out.push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == quote {
            return Some(out);
        } else {
            out.push(c);
        }
    }
    None
}

/// `Tests\Unit\CalcTest`, the class Pest makes of `tests/Unit/CalcTest.php`.
fn class_of(file: &str) -> String {
    let path = file.strip_suffix(".php").unwrap_or(file);
    let mut parts: Vec<String> = path.split('/').map(str::to_owned).collect();
    if let Some(first) = parts.first_mut() {
        let mut chars = first.chars();
        *first = chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default();
    }
    parts.join("\\")
}

struct PestParser {
    root: PathBuf,
    report: PathBuf,
    tests: Vec<TestCase>,
    /// The file of the class whose results are being printed.
    file: Option<String>,
}

impl PestParser {
    /// The test a printed name is about: `group → it adds`, perhaps with
    /// ` with (1, 2)` for a data set or ` → reason` when skipped.
    fn find(&self, printed: &str) -> Option<&TestCase> {
        let file = self.file.as_deref()?;
        self.tests
            .iter()
            .filter(|t| t.file == file)
            .filter(|t| {
                let name = t.name.replace('`', "");
                printed == name
                    || printed.strip_prefix(&name).is_some_and(|rest| rest.starts_with(" with (") || rest.starts_with(" → "))
            })
            .max_by_key(|t| t.name.len())
    }
}

impl Parser for PestParser {
    fn feed(&mut self, line: &str) -> Vec<Found> {
        let trimmed = line.trim();
        // `PASS  Tests\Unit\CalcTest` or `FAIL  …` heads each class's results.
        for head in ["PASS ", "FAIL ", "WARN "] {
            if let Some(class) = trimmed.strip_prefix(head) {
                let class = class.trim();
                let class = class.strip_prefix("P\\").unwrap_or(class);
                self.file = self.tests.iter().map(|t| &t.file).find(|f| class_of(f) == class).cloned();
                return Vec::new();
            }
        }
        let mut chars = trimmed.chars();
        let outcome = match chars.next() {
            Some('✓') => Outcome::Passed,
            Some('⨯' | '✗' | '×') => Outcome::Failed,
            Some('-' | '↓') => Outcome::Skipped,
            _ => return Vec::new(),
        };
        let printed = strip_duration(chars.as_str().trim()).trim_end();
        self.find(printed).map(|t| Found::Result(t.id.clone(), outcome)).into_iter().collect()
    }

    fn finish(&mut self) -> Vec<Found> {
        let cases = junit::read_all([self.report.as_path()]);
        let _ = std::fs::remove_file(&self.report);
        let cases = cases
            .into_iter()
            .map(|mut case| {
                // The text opens with the test's name glued to the message.
                if let Some(first) = case.lines.first_mut()
                    && let Some(rest) = first.strip_prefix(&case.name)
                {
                    *first = rest.to_owned();
                }
                case
            })
            .collect();
        aggregate(cases, |case| {
            let name = base_name(&case.name);
            // `tests/Unit/CalcTest.php::name`: the file, then the name again.
            let file = case.file.as_deref().map(|f| relative(&self.root, f.split_once("::").map_or(f, |(f, _)| f)));
            match file {
                Some(file) => format!("{file}::{name}"),
                None => format!("{}::{name}", case.class),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::test_support::{failures, results};

    const TEST: &str = r#"<?php

use App\Calc;

test('adds two numbers', function () {
    expect((new Calc())->add(1, 2))->toBe(3);
});

it('fails on purpose', function () {
    expect((new Calc())->add(1, 2))->toBe(4);
});

describe('negatives', function () {
    it('handles them', function () {
        expect((new Calc())->add(-1, -2))->toBe(-3);
    });

    test("is skipped", function () {})->skip('later');
});

it('adds pairs', function (int $a, int $b) {
    expect((new Calc())->add($a, $b))->toBe($a + $b);
})->with([[1, 2], [3, 4]]);
"#;

    #[test]
    fn reads_tests_as_pest_names_them() {
        let tests = parse_file("tests/Unit/CalcTest.php", TEST);
        let names: Vec<_> = tests.iter().map(|t| (t.name.as_str(), t.line.unwrap())).collect();
        assert_eq!(
            names,
            [
                ("adds two numbers", 5),
                ("it fails on purpose", 9),
                ("`negatives` → it handles them", 14),
                ("`negatives` → is skipped", 18),
                ("it adds pairs", 21),
            ]
        );
        assert_eq!(class_of("tests/Unit/CalcTest.php"), r"Tests\Unit\CalcTest");
    }

    #[test]
    fn reads_pest_output_and_report() {
        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join("pest.xml");
        // Captured from Pest 5.3 on the file above.
        std::fs::write(
            &report,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites>
  <testsuite name="/app/phpunit.xml" tests="6" assertions="5" errors="0" failures="1" skipped="1" time="0.17">
    <testsuite name="Test Suite" tests="6" assertions="5" errors="0" failures="1" skipped="1" time="0.17">
      <testsuite name="Tests\Unit\CalcTest" file="tests/Unit/CalcTest.php" tests="6" assertions="5" errors="0" failures="1" skipped="1" time="0.141882">
        <testcase name="adds two numbers" file="tests/Unit/CalcTest.php::adds two numbers" class="Tests\Unit\CalcTest" classname="Tests.Unit.CalcTest" assertions="1" time="0.077013"/>
        <testcase name="it fails on purpose" file="tests/Unit/CalcTest.php::it fails on purpose" class="Tests\Unit\CalcTest" classname="Tests.Unit.CalcTest" assertions="1" time="0.046939">
          <failure type="PHPUnit\Framework\ExpectationFailedException">it fails on purposeFailed asserting that 3 is identical to 4.
at tests/Unit/CalcTest.php:10</failure>
        </testcase>
        <testcase name="`negatives` → it handles them" file="tests/Unit/CalcTest.php::`negatives` → it handles them" class="Tests\Unit\CalcTest" classname="Tests.Unit.CalcTest" assertions="1" time="0.003417"/>
        <testcase name="`negatives` → is skipped" file="tests/Unit/CalcTest.php::`negatives` → is skipped" class="Tests\Unit\CalcTest" classname="Tests.Unit.CalcTest" assertions="0" time="0.006379">
          <skipped/>
        </testcase>
        <testsuite name="it adds pairs" tests="2" assertions="2" errors="0" failures="0" skipped="0" time="0.008134">
          <testcase name="it adds pairs with data set &quot;(1, 2)&quot;" file="tests/Unit/CalcTest.php::it adds pairs with data set &quot;(1, 2)&quot;" class="Tests\Unit\CalcTest" classname="Tests.Unit.CalcTest" assertions="1" time="0.004071"/>
          <testcase name="it adds pairs with data set &quot;(3, 4)&quot;" file="tests/Unit/CalcTest.php::it adds pairs with data set &quot;(3, 4)&quot;" class="Tests\Unit\CalcTest" classname="Tests.Unit.CalcTest" assertions="1" time="0.004064"/>
        </testsuite>
      </testsuite>
    </testsuite>
  </testsuite>
</testsuites>"#,
        )
        .unwrap();
        let output = "
   FAIL  Tests\\Unit\\CalcTest
  ✓ adds two numbers                                                     0.08s
  ⨯ it fails on purpose                                                  0.05s
  ✓ negatives → it handles them
  - negatives → is skipped → later                                       0.01s
  ✓ it adds pairs with (1, 2)
  ✓ it adds pairs with (3, 4)
  ────────────────────────────────────────────────────────────────────────────
   FAILED  Tests\\Unit\\CalcTest > it fails on purpose
  Failed asserting that 3 is identical to 4.

  Tests:    1 failed, 1 skipped, 5 passed (6 assertions)";
        let mut parser = PestParser {
            root: PathBuf::from("/app"),
            report: report.clone(),
            tests: parse_file("tests/Unit/CalcTest.php", TEST),
            file: None,
        };
        let live: Vec<Found> = output.lines().flat_map(|line| parser.feed(line)).collect();
        let f = "tests/Unit/CalcTest.php::";
        let ids = |list: &[(&str, Outcome)]| list.iter().map(|(n, o)| (format!("{f}{n}"), *o)).collect::<Vec<_>>();
        let live_expected = ids(&[
            ("adds two numbers", Outcome::Passed),
            ("it fails on purpose", Outcome::Failed),
            ("`negatives` → it handles them", Outcome::Passed),
            ("`negatives` → is skipped", Outcome::Skipped),
            ("it adds pairs", Outcome::Passed),
            ("it adds pairs", Outcome::Passed),
        ]);
        let got: Vec<_> = results(&live).into_iter().map(|(id, o)| (id.to_owned(), o)).collect();
        assert_eq!(got, live_expected);

        let end = parser.finish();
        let got: Vec<_> = results(&end).into_iter().map(|(id, o)| (id.to_owned(), o)).collect();
        let mut end_expected = live_expected.clone();
        end_expected.pop();
        assert_eq!(got, end_expected);
        let failed = failures(&end);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].1, ["Failed asserting that 3 is identical to 4.", "at tests/Unit/CalcTest.php:10"]);
    }

    #[test]
    fn runs_cases_with_a_filter() {
        use crate::testing::CaseRef;
        let target = TestTarget::Cases {
            cases: vec![
                CaseRef { id: "tests/Unit/CalcTest.php::it fails on purpose".into(), line: Some(9) },
                CaseRef { id: "tests/Unit/CalcTest.php::`negatives` → it handles them".into(), line: Some(14) },
            ],
        };
        // Checked against Pest 5.3.
        assert_eq!(
            php_args(&target, |id| id.split_once("::").unwrap().1.to_owned()),
            "'tests/Unit/CalcTest.php' --filter '/::(?:it fails on purpose|`negatives` → it handles them)(?: with data set .*)?$/'"
        );
    }
}
