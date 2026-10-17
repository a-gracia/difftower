//! Gradle, and what it shares with Maven: JUnit tests in Java and Kotlin,
//! read from `src/test/` without compiling, and a reader that maps the
//! classes reports name back to those files.
//!
//! Gradle's own reports lose the method of a parametrized test, so an init
//! script logs each test to a file of ours, and also has Gradle print one
//! line per test, read as the run goes.

use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
};

use anyhow::Result;

use super::{Found, Kind, Outcome, Parser, Started, TestCase, TestTarget, quote, report_path, walk};

pub(crate) struct Gradle;

impl Kind for Gradle {
    fn detect(&self, root: &Path) -> bool {
        ["build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts"]
            .iter()
            .any(|name| root.join(name).is_file())
    }

    fn default_command(&self, root: &Path) -> String {
        if root.join("gradlew").is_file() { "./gradlew".to_owned() } else { "gradle".to_owned() }
    }

    fn discover(&self, root: &Path, _command: &str) -> Result<Vec<TestCase>> {
        Ok(jvm_tests(root).into_iter().map(|t| t.case).collect())
    }

    fn start(&self, root: &Path, command: &str, target: &TestTarget) -> Result<Started> {
        let tests = jvm_tests(root);
        let filters: Vec<String> = match target {
            TestTarget::All => Vec::new(),
            // A class takes its nested classes along.
            TestTarget::Paths { .. } => classes_under(&tests, target).into_iter().collect(),
            TestTarget::Cases { .. } => {
                selected(&tests, target).map(|t| format!("{}.{}", t.class, t.method)).collect()
            }
        };
        let report = report_path("gradle-tests.txt");
        let init = report_path("init.gradle");
        std::fs::write(&init, init_script(&report))?;
        let args: String = filters.iter().map(|f| format!(" --tests {}", quote(f))).collect();
        Ok(Started {
            script: format!(
                "{command} test --console=plain --no-configuration-cache --init-script {}{args}",
                quote(&init.display().to_string())
            ),
            parser: Box::new(GradleParser { jvm: JvmParser::new(&tests), report, init }),
        })
    }
}

/// Sets every test task to go on when a filter matches nothing in one of
/// several projects, to print a line per test, and to log each test with
/// its outcome and failure to `report`.
fn init_script(report: &Path) -> String {
    let path = report.display().to_string().replace('\\', "\\\\").replace('\'', "\\'");
    format!(
        r#"def difftowerReport = new File('{path}')
allprojects {{
    tasks.withType(Test).configureEach {{
        filter.failOnNoMatchingTests = false
        testLogging {{
            events 'passed', 'skipped', 'failed'
            exceptionFormat 'short'
        }}
        afterTest {{ desc, result ->
            synchronized (difftowerReport) {{
                difftowerReport << "test\t${{result.resultType}}\t${{desc.className}}\t${{desc.name}}\n"
                result.exceptions.each {{ e ->
                    def text = e.toString() + '\n' + e.stackTrace.take(15).collect {{ '    at ' + it }}.join('\n')
                    text.eachLine {{ difftowerReport << "line\t${{it}}\n" }}
                }}
            }}
        }}
    }}
}}
"#
    )
}

struct GradleParser {
    jvm: JvmParser,
    report: PathBuf,
    init: PathBuf,
}

impl Parser for GradleParser {
    /// `UserTest > Inner > deep() PASSED`, or for a parametrized test
    /// `UserTest > positive(int) > [1] 1 PASSED`.
    fn feed(&mut self, line: &str) -> Vec<Found> {
        let Some((head, status)) = line.trim_end().rsplit_once(' ') else { return Vec::new() };
        let outcome = match status {
            "PASSED" => Outcome::Passed,
            "FAILED" => Outcome::Failed,
            "SKIPPED" => Outcome::Skipped,
            _ => return Vec::new(),
        };
        let parts: Vec<&str> = head.split(" > ").collect();
        let Some(at) = parts.iter().position(|part| part.ends_with(')') && part.contains('(')) else {
            return Vec::new();
        };
        if at == 0 {
            return Vec::new();
        }
        let key = format!("{}#{}", parts[..at].join(" > "), method_of(parts[at]));
        let Some(id) = self.jvm.by_simple.get(&key).cloned() else { return Vec::new() };
        self.jvm.record(id, outcome, Vec::new())
    }

    fn finish(&mut self) -> Vec<Found> {
        let text = std::fs::read_to_string(&self.report).unwrap_or_default();
        let _ = std::fs::remove_file(&self.report);
        let _ = std::fs::remove_file(&self.init);
        let mut found = Vec::new();
        let mut current: Option<(String, Outcome, Vec<String>)> = None;
        let flush = |current: &mut Option<(String, Outcome, Vec<String>)>, found: &mut Vec<Found>, jvm: &mut JvmParser| {
            if let Some((id, outcome, lines)) = current.take() {
                found.extend(jvm.record(id, outcome, lines));
            }
        };
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("line\t") {
                if let Some((_, _, lines)) = &mut current {
                    lines.push(rest.to_owned());
                }
                continue;
            }
            let Some(rest) = line.strip_prefix("test\t") else { continue };
            flush(&mut current, &mut found, &mut self.jvm);
            let mut fields = rest.splitn(3, '\t');
            let (Some(result), Some(class), Some(name)) = (fields.next(), fields.next(), fields.next()) else {
                continue;
            };
            let outcome = match result {
                "SUCCESS" => Outcome::Passed,
                "FAILURE" => Outcome::Failed,
                _ => Outcome::Skipped,
            };
            if let Some(id) = self.jvm.by_class.get(&format!("{class}#{}", method_of(name))) {
                current = Some((id.clone(), outcome, Vec::new()));
            }
        }
        flush(&mut current, &mut found, &mut self.jvm);
        found.extend(self.jvm.failures());
        found
    }
}

/// A JUnit test found in a source file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct JvmTest {
    /// The binary name of its class: `com.x.UserTest$Inner`.
    pub class: String,
    pub method: String,
    pub case: TestCase,
}

/// Every JUnit test in Java and Kotlin files under a `src/test/` folder, in
/// the repository or any of its modules.
pub(crate) fn jvm_tests(root: &Path) -> Vec<JvmTest> {
    let files = walk(root, |file| {
        (file.starts_with("src/test/") || file.contains("/src/test/"))
            && (file.ends_with(".java") || file.ends_with(".kt"))
    });
    files
        .iter()
        .flat_map(|file| parse_jvm_file(file, &std::fs::read_to_string(root.join(file)).unwrap_or_default()))
        .collect()
}

const TEST_ANNOTATIONS: [&str; 5] = ["Test", "ParameterizedTest", "RepeatedTest", "TestFactory", "TestTemplate"];

/// The tests in one Java or Kotlin file: methods annotated as JUnit 4 or 5
/// tests, in classes nested any depth.
pub(crate) fn parse_jvm_file(file: &str, text: &str) -> Vec<JvmTest> {
    let stem = file.rsplit('/').next().unwrap_or(file).split('.').next().unwrap_or(file);
    let mut package = String::new();
    // Enclosing classes, with the brace depth inside each.
    let mut classes: Vec<(String, usize)> = Vec::new();
    let mut pending_class: Option<String> = None;
    let mut pending_test = false;
    let mut depth = 0usize;
    let mut in_comment = false;
    let mut tests = Vec::new();
    for (ix, raw) in text.lines().enumerate() {
        let line = strip_comments(raw, &mut in_comment);
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("package ") {
            package = rest.trim_end_matches(';').trim().to_owned();
            continue;
        }
        let (code, annotations) = split_annotations(trimmed);
        if annotations.iter().any(|a| TEST_ANNOTATIONS.contains(&a.rsplit('.').next().unwrap_or(a))) {
            pending_test = true;
        }
        let words: Vec<&str> = code.split_whitespace().collect();
        if let Some(at) = words.iter().position(|w| *w == "class" || *w == "object") {
            let name: String = words
                .get(at + 1)
                .map(|w| w.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect())
                .unwrap_or_default();
            if !name.is_empty() {
                pending_class = Some(name);
                pending_test = false;
            }
        } else if pending_test && let Some(method) = method_name(code) {
            pending_test = false;
            if !classes.is_empty() {
                let names: Vec<&str> = classes.iter().map(|(name, _)| name.as_str()).collect();
                let class = if package.is_empty() {
                    names.join("$")
                } else {
                    format!("{package}.{}", names.join("$"))
                };
                // The file's own class goes without saying.
                let shown = if names[0] == stem { &names[1..] } else { &names[..] };
                let name = shown.iter().copied().chain([method.as_str()]).collect::<Vec<_>>().join(" > ");
                tests.push(JvmTest {
                    class,
                    method: method.clone(),
                    case: TestCase {
                        id: format!("{file}::{name}"),
                        file: file.to_owned(),
                        name,
                        line: Some(ix as u32 + 1),
                    },
                });
            }
        }
        for c in braces(&line) {
            if c == '{' {
                depth += 1;
                if let Some(name) = pending_class.take() {
                    classes.push((name, depth));
                }
            } else {
                classes.retain(|(_, level)| *level < depth);
                depth = depth.saturating_sub(1);
            }
        }
    }
    tests
}

/// `line` without `//` and `/* */` comments, tracking a block comment that
/// goes on to the next lines.
fn strip_comments(line: &str, in_comment: &mut bool) -> String {
    let mut out = String::new();
    let mut rest = line;
    loop {
        if *in_comment {
            match rest.find("*/") {
                Some(end) => {
                    rest = &rest[end + 2..];
                    *in_comment = false;
                }
                None => return out,
            }
        }
        let block = rest.find("/*");
        let lone = rest.find("//");
        match (block, lone) {
            (Some(b), Some(l)) if l < b => return out + &rest[..l],
            (None, Some(l)) => return out + &rest[..l],
            (Some(b), _) => {
                out.push_str(&rest[..b]);
                rest = &rest[b + 2..];
                *in_comment = true;
            }
            (None, None) => return out + rest,
        }
    }
}

/// The braces of `line` outside string and character literals.
pub(crate) fn braces(line: &str) -> Vec<char> {
    let mut out = Vec::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for c in line.chars() {
        match quote {
            Some(q) => {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == q {
                    quote = None;
                }
            }
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '{' || c == '}' => out.push(c),
            None => {}
        }
    }
    out
}

/// `line` without its leading annotations, and their names.
fn split_annotations(line: &str) -> (&str, Vec<&str>) {
    let mut rest = line.trim_start();
    let mut names = Vec::new();
    while let Some(after) = rest.strip_prefix('@') {
        let len = after.find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.')).unwrap_or(after.len());
        names.push(&after[..len]);
        rest = &after[len..];
        if rest.starts_with('(') {
            // The arguments, up to the matching parenthesis.
            let mut level = 0;
            let mut end = rest.len();
            for (ix, c) in rest.char_indices() {
                match c {
                    '(' => level += 1,
                    ')' => {
                        level -= 1;
                        if level == 0 {
                            end = ix + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            rest = &rest[end..];
        }
        rest = rest.trim_start();
    }
    (rest, names)
}

/// The method declared on `code`: `void adds() {` or Kotlin's
/// `fun adds() {` and ``fun `adds two`() {``.
fn method_name(code: &str) -> Option<String> {
    if let Some(at) = code.find("fun ") {
        let rest = code[at + 4..].trim_start();
        if let Some(quoted) = rest.strip_prefix('`') {
            return quoted.split_once('`').map(|(name, _)| name.to_owned());
        }
        let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        return (!name.is_empty()).then_some(name);
    }
    let open = code.find('(')?;
    let name: String = code[..open]
        .trim_end()
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    (!name.is_empty() && !name.starts_with(|c: char| c.is_ascii_digit())).then_some(name)
}

/// The method a report names: `adds()`, `positive(int)[1]`, `adds`.
pub(crate) fn method_of(name: &str) -> &str {
    name.split(['(', '[']).next().unwrap_or(name).trim()
}

/// The tests among `tests` that `target`'s cases name.
pub(crate) fn selected<'a>(tests: &'a [JvmTest], target: &TestTarget) -> impl Iterator<Item = &'a JvmTest> {
    let ids: BTreeSet<&str> = match target {
        TestTarget::Cases { cases } => cases.iter().map(|c| c.id.as_str()).collect(),
        _ => BTreeSet::new(),
    };
    tests.iter().filter(move |t| ids.contains(t.case.id.as_str()))
}

/// The top-level classes of the tests in `target`'s files and folders.
pub(crate) fn classes_under(tests: &[JvmTest], target: &TestTarget) -> BTreeSet<String> {
    let TestTarget::Paths { paths } = target else { return BTreeSet::new() };
    tests
        .iter()
        .filter(|t| {
            paths.iter().any(|p| {
                let p = p.trim_end_matches('/');
                t.case.file == p || t.case.file.starts_with(&format!("{p}/")) || p.is_empty() || p == "."
            })
        })
        .map(|t| t.class.split('$').next().unwrap_or(&t.class).to_owned())
        .collect()
}

/// Maps the names reports give back to test ids and keeps, for a test that
/// runs several times, as a parametrized one, its worst outcome.
pub(crate) struct JvmParser {
    /// `com.x.UserTest$Inner#deep` to its id.
    pub by_class: HashMap<String, String>,
    /// `UserTest > Inner#deep`, as Gradle prints it, to its id.
    pub by_simple: HashMap<String, String>,
    outcomes: HashMap<String, Outcome>,
    failures: Vec<(String, Vec<String>)>,
}

impl JvmParser {
    pub fn new(tests: &[JvmTest]) -> Self {
        let mut by_class = HashMap::new();
        let mut by_simple = HashMap::new();
        for t in tests {
            by_class.insert(format!("{}#{}", t.class, t.method), t.case.id.clone());
            let simple = t.class.rsplit('.').next().unwrap_or(&t.class).replace('$', " > ");
            by_simple.insert(format!("{simple}#{}", t.method), t.case.id.clone());
        }
        Self { by_class, by_simple, outcomes: HashMap::new(), failures: Vec::new() }
    }

    /// Notes a run of test `id`; its result when that changes what is shown.
    pub fn record(&mut self, id: String, outcome: Outcome, lines: Vec<String>) -> Vec<Found> {
        if outcome == Outcome::Failed && !lines.is_empty() {
            match self.failures.iter_mut().find(|(known, _)| *known == id) {
                Some((_, known)) => {
                    known.push(String::new());
                    known.extend(lines);
                }
                None => self.failures.push((id.clone(), lines)),
            }
        }
        let worst = match (self.outcomes.get(&id), outcome) {
            (Some(Outcome::Failed), _) | (_, Outcome::Failed) => Outcome::Failed,
            (Some(Outcome::Passed), _) | (_, Outcome::Passed) => Outcome::Passed,
            _ => Outcome::Skipped,
        };
        if self.outcomes.insert(id.clone(), worst) == Some(worst) {
            return Vec::new();
        }
        vec![Found::Result(id, worst)]
    }

    /// The failures recorded, once each.
    pub fn failures(&mut self) -> Vec<Found> {
        std::mem::take(&mut self.failures).into_iter().map(|(id, lines)| Found::Failure(id, lines)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{
        CaseRef,
        test_support::{failures, parse, results},
    };

    const JAVA: &str = r#"package com.x;

import org.junit.jupiter.api.*;
import static org.junit.jupiter.api.Assertions.*;

/* A class { with braces } in a comment */
class UserTest {
    @Test
    void adds() {
        assertEquals("{", "{");
    }

    @Disabled @Test void later() {}

    @ParameterizedTest
    @ValueSource(ints = {1, 2})
    void positive(int n) {
        assertTrue(n > 0);
    }

    void helper() {}

    @Nested
    class Inner {
        @Test
        void deep() {}
    }

    @Test
    void after() {}
}
"#;

    #[test]
    fn reads_java_tests() {
        let tests = parse_jvm_file("src/test/java/com/x/UserTest.java", JAVA);
        let found: Vec<_> =
            tests.iter().map(|t| (t.class.as_str(), t.method.as_str(), t.case.name.as_str(), t.case.line)).collect();
        assert_eq!(
            found,
            [
                ("com.x.UserTest", "adds", "adds", Some(9)),
                ("com.x.UserTest", "later", "later", Some(13)),
                ("com.x.UserTest", "positive", "positive", Some(17)),
                ("com.x.UserTest$Inner", "deep", "Inner > deep", Some(26)),
                ("com.x.UserTest", "after", "after", Some(30)),
            ]
        );
        assert_eq!(tests[3].case.id, "src/test/java/com/x/UserTest.java::Inner > deep");
    }

    #[test]
    fn reads_kotlin_tests() {
        let kotlin = r#"package com.x

import org.junit.jupiter.api.Test

class PriceTest {
    @Test
    fun `adds tax`() {
        val s = "${1}"
    }

    @org.junit.jupiter.api.Test fun rounds() {}
}
"#;
        let tests = parse_jvm_file("app/src/test/kotlin/com/x/PriceTest.kt", kotlin);
        let names: Vec<_> = tests.iter().map(|t| (t.class.as_str(), t.case.name.as_str())).collect();
        assert_eq!(names, [("com.x.PriceTest", "adds tax"), ("com.x.PriceTest", "rounds")]);
    }

    fn sample() -> Vec<JvmTest> {
        let mut tests = parse_jvm_file("src/test/java/com/x/UserTest.java", JAVA);
        tests.extend(parse_jvm_file(
            "src/test/java/com/x/OtherTest.java",
            "package com.x;\npublic class OtherTest {\n    @Test\n    public void works() {}\n}\n",
        ));
        tests
    }

    #[test]
    fn filters_by_path_and_case() {
        let tests = sample();
        let target = TestTarget::Paths { paths: vec!["src/test/java/com/x/UserTest.java".into()] };
        assert_eq!(classes_under(&tests, &target).into_iter().collect::<Vec<_>>(), ["com.x.UserTest"]);
        let target = TestTarget::Paths { paths: vec!["src/test".into()] };
        assert_eq!(classes_under(&tests, &target).len(), 2);
        let target = TestTarget::Cases {
            cases: vec![CaseRef { id: "src/test/java/com/x/UserTest.java::Inner > deep".into(), line: Some(26) }],
        };
        let picked: Vec<_> = selected(&tests, &target).map(|t| format!("{}.{}", t.class, t.method)).collect();
        assert_eq!(picked, ["com.x.UserTest$Inner.deep"]);
    }

    #[test]
    fn reads_gradle_output_and_report() {
        // Captured from Gradle 9.8 with JUnit 5.11 and the init script.
        let output = "\
> Task :test
UserTest > adds() PASSED
UserTest > later() SKIPPED
UserTest > positive(int) > [1] 1 PASSED
UserTest > positive(int) > [2] 2 FAILED
    org.opentest4j.AssertionFailedError at UserTest.java:16
UserTest > Inner > deep() PASSED
OtherTest > works() PASSED
7 tests completed, 1 failed, 1 skipped";
        let report = report_path("gradle-test.txt");
        std::fs::write(
            &report,
            "test\tSUCCESS\tcom.x.UserTest\tadds()\n\
             test\tSKIPPED\tcom.x.UserTest\tlater()\n\
             test\tSUCCESS\tcom.x.UserTest\tpositive(int)[1]\n\
             test\tFAILURE\tcom.x.UserTest\tpositive(int)[2]\n\
             line\torg.opentest4j.AssertionFailedError: expected: <true> but was: <false>\n\
             line\t    at app//com.x.UserTest.positive(UserTest.java:19)\n\
             test\tSUCCESS\tcom.x.UserTest$Inner\tdeep()\n\
             test\tSUCCESS\tcom.x.OtherTest\tworks()\n",
        )
        .unwrap();
        let init = report_path("init.gradle");
        let parser = GradleParser { jvm: JvmParser::new(&sample()), report: report.clone(), init };
        let found = parse(Box::new(parser), output);
        let file = "src/test/java/com/x/UserTest.java";
        let id = |name: &str| format!("{file}::{name}");
        let other = "src/test/java/com/x/OtherTest.java::works".to_owned();
        let results: Vec<_> = results(&found).into_iter().map(|(id, o)| (id.to_owned(), o)).collect();
        assert_eq!(
            results,
            [
                (id("adds"), Outcome::Passed),
                (id("later"), Outcome::Skipped),
                (id("positive"), Outcome::Passed),
                (id("positive"), Outcome::Failed),
                (id("Inner > deep"), Outcome::Passed),
                (other, Outcome::Passed),
            ]
        );
        let failures = failures(&found);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, id("positive"));
        assert_eq!(failures[0].1[0], "org.opentest4j.AssertionFailedError: expected: <true> but was: <false>");
        assert!(!report.exists(), "the report is removed once read");
    }
}
