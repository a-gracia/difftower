//! PHPUnit.
//!
//! Test methods are read from the `*Test.php` files, so listing needs no
//! PHP. A run prints the testdox format and writes a JUnit report, read at
//! the end for each test's outcome and failure.

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::{
    Found, Kind, Outcome, Parser, Started, TestCase, TestTarget, file_of, junit, quote, read, regex_escape,
    report_path, walk,
};

pub(crate) struct Phpunit;

impl Kind for Phpunit {
    fn detect(&self, root: &Path) -> bool {
        let composer = read(root, "composer.json");
        let configured = root.join("phpunit.xml").is_file()
            || root.join("phpunit.xml.dist").is_file()
            || composer.contains("phpunit/phpunit");
        configured && !composer.contains("pestphp/pest")
    }

    fn default_command(&self, root: &Path) -> String {
        if root.join("vendor/bin/phpunit").is_file() { "vendor/bin/phpunit" } else { "phpunit" }.to_owned()
    }

    fn discover(&self, root: &Path, _command: &str) -> Result<Vec<TestCase>> {
        Ok(tests(root))
    }

    fn start(&self, root: &Path, command: &str, target: &TestTarget) -> Result<Started> {
        let report = report_path("phpunit.xml");
        Ok(Started {
            script: format!(
                "{command} --testdox --colors=never --log-junit {} {}",
                quote(&report.display().to_string()),
                php_args(target, |id| id.split_once("::").map_or(id, |(_, name)| name).to_owned())
            ),
            parser: Box::new(JunitParser { root: root.to_path_buf(), report, tests: tests(root) }),
        })
    }
}

/// The arguments that run `target`: paths, or the files of some tests with
/// a `--filter` matching `Class::name`, data sets included. `name` gives
/// the part of an id that follows `::` in PHPUnit's own test name.
pub(super) fn php_args(target: &TestTarget, name: impl Fn(&str) -> String) -> String {
    match target {
        TestTarget::All => String::new(),
        TestTarget::Paths { paths } => paths.iter().map(|p| quote(p)).collect::<Vec<_>>().join(" "),
        TestTarget::Cases { cases } => {
            let mut files: Vec<&str> = cases.iter().map(|case| file_of(&case.id)).collect();
            files.sort();
            files.dedup();
            let names: Vec<String> = cases.iter().map(|case| regex_escape(&name(&case.id))).collect();
            let filter = format!("/::(?:{})(?: with data set .*)?$/", names.join("|"));
            let files: Vec<String> = files.iter().map(|f| quote(f)).collect();
            format!("{} --filter {}", files.join(" "), quote(&filter))
        }
    }
}

/// Every test method under the repository, outside `vendor/`.
fn tests(root: &Path) -> Vec<TestCase> {
    walk(root, |file| file.ends_with("Test.php"))
        .iter()
        .flat_map(|file| parse_file(file, &std::fs::read_to_string(root.join(file)).unwrap_or_default()))
        .collect()
}

/// `test…` methods and those marked `#[Test]` or `@test`, in classes that
/// are not abstract.
fn parse_file(file: &str, text: &str) -> Vec<TestCase> {
    let mut tests = Vec::new();
    let mut in_class = false;
    let mut marked = false;
    for (ix, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("abstract class ") {
            in_class = false;
            continue;
        }
        if trimmed.starts_with("class ") || trimmed.starts_with("final class ") || trimmed.starts_with("final readonly class ") {
            in_class = true;
            marked = false;
            continue;
        }
        if trimmed.contains("#[Test") || trimmed.contains("Attributes\\Test]") || trimmed.contains("@test") {
            marked = true;
            continue;
        }
        let Some(at) = trimmed.find("function ") else {
            // Other attributes and comments sit between a mark and its method.
            if !(trimmed.starts_with("#[") || trimmed.starts_with('*') || trimmed.starts_with("/*") || trimmed.starts_with("//") || trimmed.is_empty()) {
                marked = false;
            }
            continue;
        };
        let head = &trimmed[..at];
        let name: String =
            trimmed[at + "function ".len()..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        let public = !(head.contains("private") || head.contains("protected"));
        let test = in_class && public && !head.contains("static") && (name.starts_with("test") || marked);
        marked = false;
        if test && !name.is_empty() {
            tests.push(TestCase {
                id: format!("{file}::{name}"),
                file: file.to_owned(),
                name,
                line: Some(ix as u32 + 1),
            });
        }
    }
    tests
}

/// `path` relative to `root`, as the report may give it absolute.
pub(super) fn relative(root: &Path, path: &str) -> String {
    let roots: Vec<PathBuf> = [Some(root.to_path_buf()), root.canonicalize().ok()].into_iter().flatten().collect();
    roots
        .iter()
        .find_map(|root| Path::new(path).strip_prefix(root).ok())
        .map_or(path.to_owned(), |p| p.display().to_string())
}

/// A case's test name without its data set: `testPairs with data set "one"`.
pub(super) fn base_name(name: &str) -> &str {
    name.split_once(" with data set ").map_or(name, |(base, _)| base)
}

/// One result per test from report cases, each turned into an id by
/// `id_of`: failed when any data set failed, its failures together.
pub(super) fn aggregate(cases: Vec<junit::Case>, id_of: impl Fn(&junit::Case) -> String) -> Vec<Found> {
    let mut tests: Vec<(String, Outcome, Vec<String>)> = Vec::new();
    for case in cases {
        let id = id_of(&case);
        let ix = match tests.iter().position(|(known, _, _)| *known == id) {
            Some(ix) => ix,
            None => {
                tests.push((id, case.outcome, Vec::new()));
                tests.len() - 1
            }
        };
        let (_, outcome, lines) = &mut tests[ix];
        *outcome = match (*outcome, case.outcome) {
            (Outcome::Failed, _) | (_, Outcome::Failed) => Outcome::Failed,
            (Outcome::Passed, _) | (_, Outcome::Passed) => Outcome::Passed,
            _ => Outcome::Skipped,
        };
        if case.outcome == Outcome::Failed {
            if let Some((_, set)) = case.name.split_once(" with data set ") {
                lines.push(format!("With data set {set}:"));
            }
            lines.extend(case.lines);
        }
    }
    let mut found = Vec::new();
    for (id, outcome, lines) in tests {
        found.push(Found::Result(id.clone(), outcome));
        if outcome == Outcome::Failed {
            found.push(Found::Failure(id, lines));
        }
    }
    found
}

/// Nothing to read as it goes; the JUnit report says it all at the end.
struct JunitParser {
    root: PathBuf,
    report: PathBuf,
    tests: Vec<TestCase>,
}

impl Parser for JunitParser {
    fn feed(&mut self, _line: &str) -> Vec<Found> {
        Vec::new()
    }

    fn finish(&mut self) -> Vec<Found> {
        let cases = junit::read_all([self.report.as_path()]);
        let _ = std::fs::remove_file(&self.report);
        let root = self.root.clone();
        let cases = cases
            .into_iter()
            .map(|mut case| {
                // `Tests\Unit\CalcTest::testAdds` opens the text; paths are absolute.
                if case.lines.first().is_some_and(|l| l.ends_with(&format!("::{}", case.name))) {
                    case.lines.remove(0);
                }
                let prefix = format!("{}/", root.display());
                for line in &mut case.lines {
                    *line = line.replace(&prefix, "");
                }
                case
            })
            .collect();
        aggregate(cases, |case| {
            let name = base_name(&case.name);
            let file = case.file.as_deref().map(|f| relative(&self.root, f));
            match file {
                Some(file) => format!("{file}::{name}"),
                None => self
                    .tests
                    .iter()
                    .find(|t| t.name == name)
                    .map_or_else(|| format!("{}::{name}", case.class), |t| t.id.clone()),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{
        CaseRef,
        test_support::{failures, results},
    };

    const TEST: &str = r#"<?php
namespace Tests\Unit;

use App\Calc;
use PHPUnit\Framework\Attributes\Test;
use PHPUnit\Framework\Attributes\DataProvider;
use PHPUnit\Framework\TestCase;

final class CalcTest extends TestCase
{
    public function testAdds(): void
    {
        $this->assertSame(3, (new Calc())->add(1, 2));
    }

    #[Test]
    public function fails_on_purpose(): void
    {
        $this->assertSame(4, (new Calc())->add(1, 2));
    }

    /** @test */
    public function it_is_skipped(): void
    {
        $this->markTestSkipped('later');
    }

    public static function pairs(): array { return ['one' => [1, 1, 2], 'two' => [2, 2, 4]]; }

    #[DataProvider('pairs')]
    public function testPairs(int $a, int $b, int $sum): void
    {
        $this->assertSame($sum, (new Calc())->add($a, $b));
    }

    private function testHelper(): void {}

    public function testErrors(): void
    {
        throw new \RuntimeException('boom');
    }
}
"#;

    #[test]
    fn reads_test_methods() {
        let tests = parse_file("tests/Unit/CalcTest.php", TEST);
        let found: Vec<_> = tests.iter().map(|t| (t.name.as_str(), t.line.unwrap())).collect();
        assert_eq!(
            found,
            [("testAdds", 11), ("fails_on_purpose", 17), ("it_is_skipped", 23), ("testPairs", 31), ("testErrors", 38)]
        );
        assert_eq!(tests[0].id, "tests/Unit/CalcTest.php::testAdds");
    }

    #[test]
    fn reads_the_junit_report() {
        let dir = tempfile::tempdir().unwrap();
        let root = Path::new("/app");
        let report = dir.path().join("phpunit.xml");
        // Captured from PHPUnit 13.4 on the class above.
        std::fs::write(
            &report,
            r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites>
  <testsuite name="/app/phpunit.xml" tests="5" assertions="4" errors="1" failures="1" skipped="0" time="0.022839">
    <testsuite name="Unit" tests="5" assertions="4" errors="1" failures="1" skipped="0" time="0.022839">
      <testsuite name="Tests\Unit\CalcTest" file="/app/tests/Unit/CalcTest.php" tests="5" assertions="4" errors="1" failures="1" skipped="0" time="0.022839">
        <testcase name="testAdds" file="/app/tests/Unit/CalcTest.php" line="11" class="Tests\Unit\CalcTest" classname="Tests.Unit.CalcTest" assertions="1" time="0.002506"/>
        <testcase name="fails_on_purpose" file="/app/tests/Unit/CalcTest.php" line="17" class="Tests\Unit\CalcTest" classname="Tests.Unit.CalcTest" assertions="1" time="0.014761">
          <failure type="PHPUnit\Framework\ExpectationFailedException">Tests\Unit\CalcTest::fails_on_purpose
Failed asserting that 3 is identical to 4.

/app/tests/Unit/CalcTest.php:19</failure>
        </testcase>
        <testsuite name="Tests\Unit\CalcTest::testPairs" tests="2" assertions="2" errors="0" failures="0" skipped="0" time="0.003286">
          <testcase name="testPairs with data set &quot;one&quot;" file="/app/tests/Unit/CalcTest.php" line="31" class="Tests\Unit\CalcTest" classname="Tests.Unit.CalcTest" assertions="1" time="0.001705"/>
          <testcase name="testPairs with data set &quot;two&quot;" file="/app/tests/Unit/CalcTest.php" line="31" class="Tests\Unit\CalcTest" classname="Tests.Unit.CalcTest" assertions="1" time="0.001581"/>
        </testsuite>
        <testcase name="testErrors" file="/app/tests/Unit/CalcTest.php" line="36" class="Tests\Unit\CalcTest" classname="Tests.Unit.CalcTest" assertions="0" time="0.002286">
          <error type="RuntimeException">Tests\Unit\CalcTest::testErrors
RuntimeException: boom

/app/tests/Unit/CalcTest.php:38</error>
        </testcase>
      </testsuite>
    </testsuite>
  </testsuite>
</testsuites>"#,
        )
        .unwrap();
        let mut parser = JunitParser { root: root.to_path_buf(), report: report.clone(), tests: Vec::new() };
        let found = parser.finish();
        let f = "tests/Unit/CalcTest.php::";
        let expected = [
            (format!("{f}testAdds"), Outcome::Passed),
            (format!("{f}fails_on_purpose"), Outcome::Failed),
            (format!("{f}testPairs"), Outcome::Passed),
            (format!("{f}testErrors"), Outcome::Failed),
        ];
        let expected: Vec<_> = expected.iter().map(|(id, o)| (id.as_str(), *o)).collect();
        assert_eq!(results(&found), expected);
        let failed = failures(&found);
        assert_eq!(failed[0].1, ["Failed asserting that 3 is identical to 4.", "", "tests/Unit/CalcTest.php:19"]);
        assert_eq!(failed[1].1, ["RuntimeException: boom", "", "tests/Unit/CalcTest.php:38"]);
        assert!(!report.exists());
    }

    #[test]
    fn runs_paths_and_cases() {
        let cases = TestTarget::Cases {
            cases: vec![
                CaseRef { id: "tests/Unit/CalcTest.php::testAdds".into(), line: Some(11) },
                CaseRef { id: "tests/Unit/CalcTest.php::testPairs".into(), line: Some(31) },
            ],
        };
        // Checked against PHPUnit 13.4: the three tests run, data sets included.
        assert_eq!(
            php_args(&cases, |id| id.split_once("::").unwrap().1.to_owned()),
            r"'tests/Unit/CalcTest.php' --filter '/::(?:testAdds|testPairs)(?: with data set .*)?$/'"
        );
        let paths = TestTarget::Paths { paths: vec!["tests/Unit".into()] };
        assert_eq!(php_args(&paths, |id| id.to_owned()), "'tests/Unit'");
    }

    #[test]
    fn detects_phpunit_but_not_pest() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert!(!Phpunit.detect(root));
        std::fs::write(root.join("composer.json"), r#"{"require-dev":{"phpunit/phpunit":"^11"}}"#).unwrap();
        assert!(Phpunit.detect(root));
        std::fs::write(root.join("composer.json"), r#"{"require-dev":{"pestphp/pest":"^3","phpunit/phpunit":"^11"}}"#)
            .unwrap();
        assert!(!Phpunit.detect(root));
    }
}
