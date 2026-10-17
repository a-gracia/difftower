//! Node's built-in test runner, `node --test`, read through its JUnit
//! reporter while its spec reporter prints the run.

use std::path::{Path, PathBuf};

use anyhow::Result;

use super::jest::{is_script, js_tests, relative, roots, some_test_mentions, spaced};
use super::{
    Found, Kind, Outcome, Parser, Started, TestCase, TestTarget, file_of, quote, read,
    regex_escape, report_path, walk,
};

pub(crate) struct Node;

impl Kind for Node {
    fn detect(&self, root: &Path) -> bool {
        read(root, "package.json").contains("node --test") || some_test_mentions(root, "node:test")
    }

    fn default_command(&self, _root: &Path) -> String {
        "node".to_owned()
    }

    fn discover(&self, root: &Path, _command: &str) -> Result<Vec<TestCase>> {
        Ok(js_tests(root, &walk(root, is_node_test)))
    }

    fn start(&self, root: &Path, command: &str, target: &TestTarget) -> Result<Started> {
        let report = report_path("node.xml");
        let files = |paths: &[String]| {
            // `node --test` takes files or globs, not folders.
            let mut files = Vec::new();
            for path in paths {
                if root.join(path).is_dir() {
                    let dir = format!("{}/", path.trim_end_matches('/'));
                    files.extend(walk(root, |f| f.starts_with(&dir) && is_node_test(f)));
                } else {
                    files.push(path.clone());
                }
            }
            files.dedup();
            files.iter().map(|f| quote(f)).collect::<Vec<_>>().join(" ")
        };
        let args = match target {
            TestTarget::All => String::new(),
            TestTarget::Paths { paths } => files(paths),
            TestTarget::Cases { cases } => {
                let paths: Vec<String> = cases.iter().map(|c| file_of(&c.id).to_owned()).collect();
                let names: Vec<String> =
                    cases.iter().map(|c| regex_escape(&spaced(&c.id))).collect();
                format!(
                    "--test-name-pattern={} {}",
                    quote(&format!("^({})$", names.join("|"))),
                    files(&paths)
                )
            }
        };
        Ok(Started {
            script: format!(
                "{command} --test --test-reporter=spec --test-reporter-destination=stdout \
                 --test-reporter=junit --test-reporter-destination={} {args}",
                quote(&report.display().to_string())
            ),
            parser: Box::new(NodeParser {
                roots: roots(root),
                report,
            }),
        })
    }
}

/// The files `node --test` runs: `a.test.js`, `a-test.mjs`, `a_test.cjs`,
/// `test-a.js`, `test.js`, or any script under a `test` folder.
fn is_node_test(file: &str) -> bool {
    let name = file.rsplit('/').next().unwrap_or(file);
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    is_script(file)
        && !file.ends_with(".d.ts")
        && (stem == "test"
            || stem.starts_with("test-")
            || [".test", "-test", "_test"]
                .iter()
                .any(|s| stem.ends_with(s))
            || file.starts_with("test/")
            || file.contains("/test/"))
}

struct NodeParser {
    roots: Vec<PathBuf>,
    report: PathBuf,
}

impl Parser for NodeParser {
    fn feed(&mut self, _line: &str) -> Vec<Found> {
        Vec::new()
    }

    fn finish(&mut self) -> Vec<Found> {
        let xml = std::fs::read_to_string(&self.report).unwrap_or_default();
        let _ = std::fs::remove_file(&self.report);
        parse_report(&xml, &self.roots)
    }
}

/// Node's JUnit report: suites nest as `describe` blocks do, and each case
/// carries its file.
fn parse_report(xml: &str, roots: &[PathBuf]) -> Vec<Found> {
    let Ok(doc) = roxmltree::Document::parse(xml) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for case in doc.descendants().filter(|n| n.has_tag_name("testcase")) {
        let Some(file) = case.attribute("file").and_then(|f| relative(roots, f)) else {
            continue;
        };
        let mut names: Vec<&str> = case
            .ancestors()
            .filter(|n| n.has_tag_name("testsuite"))
            .filter_map(|n| n.attribute("name"))
            .collect();
        names.reverse();
        names.push(case.attribute("name").unwrap_or(""));
        let id = format!("{file}::{}", names.join(" > "));
        let failure = case
            .children()
            .find(|c| c.has_tag_name("failure") || c.has_tag_name("error"));
        let outcome = match failure {
            Some(_) => Outcome::Failed,
            None if case.children().any(|c| c.has_tag_name("skipped")) => Outcome::Skipped,
            None => Outcome::Passed,
        };
        if let Some(failure) = failure {
            let text = failure.text().unwrap_or("").trim_matches('\n');
            let lines = if text.trim().is_empty() {
                failure
                    .attribute("message")
                    .unwrap_or("")
                    .lines()
                    .map(str::to_owned)
                    .collect()
            } else {
                text.lines().map(str::to_owned).collect()
            };
            found.push(Found::Failure(id.clone(), lines));
        }
        found.push(Found::Result(id, outcome));
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::test_support::{failures, results};

    #[test]
    fn reads_node_reports() {
        // Trimmed from `node --test --test-reporter=junit` on Node 26.
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<testsuites>
	<testsuite name="math" tests="2" failures="1" skipped="1">
		<testcase name="adds" time="0.005643" classname="test" file="/srv/app/test/math.test.js"/>
		<testsuite name="nested group" tests="2" failures="1" skipped="1">
			<testcase name="fails here" classname="test" file="/srv/app/test/math.test.js" failure="Expected values to be strictly equal:">
				<failure type="testCodeFailure" message="Expected values to be strictly equal:&#10;&#10;3 !== 4">
Error [ERR_TEST_FAILURE]: Expected values to be strictly equal:

3 !== 4
				</failure>
			</testcase>
			<testcase name="is skipped" classname="test" file="/srv/app/test/math.test.js">
				<skipped type="skipped" message="true"/>
			</testcase>
		</testsuite>
	</testsuite>
	<testcase name="top level" classname="test" file="/srv/app/test/math.test.js"/>
</testsuites>"#;
        let found = parse_report(xml, &[PathBuf::from("/srv/app")]);
        assert_eq!(
            results(&found),
            [
                ("test/math.test.js::math > adds", Outcome::Passed),
                (
                    "test/math.test.js::math > nested group > fails here",
                    Outcome::Failed
                ),
                (
                    "test/math.test.js::math > nested group > is skipped",
                    Outcome::Skipped
                ),
                ("test/math.test.js::top level", Outcome::Passed),
            ]
        );
        assert_eq!(
            failures(&found)[0].1[0],
            "Error [ERR_TEST_FAILURE]: Expected values to be strictly equal:"
        );
        assert!(
            is_node_test("test/math.js")
                && is_node_test("src/a.test.mjs")
                && !is_node_test("src/a.js")
        );
    }
}
