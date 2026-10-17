//! JUnit XML reports, which many runners write: each test with its outcome
//! and, for failures, what went wrong.

use super::Outcome;

/// One `<testcase>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Case {
    /// Its `classname`, or the enclosing suite's name.
    pub class: String,
    pub name: String,
    /// The `file` attribute of the case or of its suite, if any.
    pub file: Option<String>,
    pub line: Option<u32>,
    pub outcome: Outcome,
    /// For a failure or error: its message, then its text.
    pub lines: Vec<String>,
}

/// Every test case in `xml`, in order; nested suites included.
pub(crate) fn parse(xml: &str) -> anyhow::Result<Vec<Case>> {
    let doc = roxmltree::Document::parse(xml)?;
    let mut cases = Vec::new();
    for node in doc.descendants().filter(|n| n.has_tag_name("testcase")) {
        let suite = node.ancestors().find(|n| n.has_tag_name("testsuite"));
        let attr = |name: &str| {
            node.attribute(name).or_else(|| suite.and_then(|s| s.attribute(name))).map(str::to_owned)
        };
        let problem = node.children().find(|c| c.has_tag_name("failure") || c.has_tag_name("error"));
        let skipped = node.children().any(|c| c.has_tag_name("skipped"));
        let outcome = match (problem, skipped) {
            (Some(_), _) => Outcome::Failed,
            (None, true) => Outcome::Skipped,
            (None, false) => Outcome::Passed,
        };
        let mut lines = Vec::new();
        if let Some(problem) = problem {
            let message = problem.attribute("message").unwrap_or("").trim();
            let text = problem.text().unwrap_or("").trim_end();
            // The message often opens the text as well.
            if !message.is_empty() && !text.trim_start().starts_with(message) {
                lines.extend(message.lines().map(str::to_owned));
            }
            lines.extend(text.lines().map(str::to_owned));
            while lines.first().is_some_and(|l| l.trim().is_empty()) {
                lines.remove(0);
            }
        }
        cases.push(Case {
            class: node
                .attribute("classname")
                .or_else(|| suite.and_then(|s| s.attribute("name")))
                .unwrap_or("")
                .to_owned(),
            name: node.attribute("name").unwrap_or("").to_owned(),
            file: attr("file"),
            line: attr("line").and_then(|l| l.parse().ok()),
            outcome,
            lines,
        });
    }
    Ok(cases)
}

/// Every case in the report files `paths` that exist; unreadable ones are
/// skipped, a run that crashed may leave half a file.
pub(crate) fn read_all<'a>(paths: impl IntoIterator<Item = &'a std::path::Path>) -> Vec<Case> {
    paths
        .into_iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .filter_map(|xml| parse(&xml).ok())
        .flatten()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_cases_outcomes_and_failures() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuites>
  <testsuite name="Unit" file="tests/MathTest.php">
    <testsuite name="MathTest" file="tests/MathTest.php">
      <testcase name="testAdds" class="MathTest" classname="MathTest" file="tests/MathTest.php" line="12" time="0.01"/>
      <testcase name="testSubtracts" classname="MathTest" line="20">
        <failure type="AssertionFailedError" message="Failed asserting that 3 is identical to 4.">MathTest::testSubtracts
Failed asserting that 3 is identical to 4.

tests/MathTest.php:22</failure>
      </testcase>
      <testcase name="testLater" classname="MathTest"><skipped/></testcase>
    </testsuite>
  </testsuite>
</testsuites>"#;
        let cases = parse(xml).unwrap();
        assert_eq!(cases.len(), 3);
        assert_eq!(
            (cases[0].class.as_str(), cases[0].name.as_str(), cases[0].file.as_deref(), cases[0].line),
            ("MathTest", "testAdds", Some("tests/MathTest.php"), Some(12))
        );
        assert_eq!(cases[1].outcome, Outcome::Failed);
        // The suite's file stands for a case without one.
        assert_eq!(cases[1].file.as_deref(), Some("tests/MathTest.php"));
        assert_eq!(
            cases[1].lines,
            [
                "Failed asserting that 3 is identical to 4.",
                "MathTest::testSubtracts",
                "Failed asserting that 3 is identical to 4.",
                "",
                "tests/MathTest.php:22"
            ]
        );
        assert_eq!(cases[2].outcome, Outcome::Skipped);
    }
}
