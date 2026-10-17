//! Maven, through Surefire: tests found as for Gradle, results read from
//! the XML reports Surefire writes for each class, in every module.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::SystemTime,
};

use anyhow::Result;

use super::{
    Found, Kind, Parser, Started, TestCase, TestTarget,
    gradle::{JvmParser, classes_under, jvm_tests, method_of, selected},
    junit, quote, walk,
};

pub(crate) struct Maven;

impl Kind for Maven {
    fn detect(&self, root: &Path) -> bool {
        root.join("pom.xml").is_file()
    }

    fn default_command(&self, root: &Path) -> String {
        if root.join("mvnw").is_file() { "./mvnw".to_owned() } else { "mvn".to_owned() }
    }

    fn discover(&self, root: &Path, _command: &str) -> Result<Vec<TestCase>> {
        Ok(jvm_tests(root).into_iter().map(|t| t.case).collect())
    }

    fn start(&self, root: &Path, command: &str, target: &TestTarget) -> Result<Started> {
        let tests = jvm_tests(root);
        let filter = match target {
            TestTarget::All => None,
            // Surefire leaves nested classes out unless named.
            TestTarget::Paths { .. } => Some(
                classes_under(&tests, target)
                    .into_iter()
                    .flat_map(|class| [class.clone(), format!("{class}$*")])
                    .collect::<Vec<_>>()
                    .join(","),
            ),
            TestTarget::Cases { .. } => {
                let mut methods: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
                for t in selected(&tests, target) {
                    methods.entry(t.class.as_str()).or_default().push(t.method.as_str());
                }
                Some(
                    methods
                        .into_iter()
                        .map(|(class, methods)| format!("{class}#{}", methods.join("+")))
                        .collect::<Vec<_>>()
                        .join(","),
                )
            }
        };
        let filter = match filter {
            // Nothing matched: a pattern that runs nothing, not everything.
            Some(filter) if filter.is_empty() => format!(" -Dtest={}", quote("DifftowerNoSuchTest")),
            Some(filter) => format!(" -Dtest={}", quote(&filter)),
            None => String::new(),
        };
        Ok(Started {
            script: format!(
                "{command} -B -Dstyle.color=never test -Dsurefire.failIfNoSpecifiedTests=false -DfailIfNoTests=false{filter}"
            ),
            parser: Box::new(MavenParser {
                jvm: JvmParser::new(&tests),
                root: root.to_path_buf(),
                started: SystemTime::now(),
            }),
        })
    }
}

struct MavenParser {
    jvm: JvmParser,
    root: PathBuf,
    /// Reports older than this are from earlier runs.
    started: SystemTime,
}

impl Parser for MavenParser {
    fn feed(&mut self, _line: &str) -> Vec<Found> {
        Vec::new()
    }

    fn finish(&mut self) -> Vec<Found> {
        let reports = report_files(&self.root, self.started);
        let mut found = Vec::new();
        for case in junit::read_all(reports.iter().map(PathBuf::as_path)) {
            let key = format!("{}#{}", case.class, method_of(&case.name));
            if let Some(id) = self.jvm.by_class.get(&key).cloned() {
                found.extend(self.jvm.record(id, case.outcome, case.lines));
            }
        }
        found.extend(self.jvm.failures());
        found
    }
}

/// The Surefire reports written since `since`, in the module at `root` and
/// in each module below it.
fn report_files(root: &Path, since: SystemTime) -> Vec<PathBuf> {
    let modules = walk(root, |file| file == "pom.xml" || file.ends_with("/pom.xml"));
    let mut reports = Vec::new();
    for pom in modules {
        let dir = root.join(pom).with_file_name("target").join("surefire-reports");
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let fresh = entry.metadata().and_then(|m| m.modified()).is_ok_and(|time| time >= since);
            if name.starts_with("TEST-") && name.ends_with(".xml") && fresh {
                reports.push(path);
            }
        }
    }
    reports
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{
        Outcome,
        gradle::parse_jvm_file,
        test_support::{failures, parse, results},
    };

    /// Captured from Surefire 3.5 with JUnit 5.11: nested classes put every
    /// case of the outer class in their own report.
    const REPORT: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<testsuite name="com.x.UserTest$Inner" tests="6" errors="0" skipped="1" failures="1">
  <testcase name="adds" classname="com.x.UserTest" time="0.194"/>
  <testcase name="fails" classname="com.x.UserTest" time="0.063">
    <failure message="math ==&gt; expected: &lt;3&gt; but was: &lt;2&gt;" type="org.opentest4j.AssertionFailedError"><![CDATA[org.opentest4j.AssertionFailedError: math ==> expected: <3> but was: <2>
	at com.x.UserTest.fails(UserTest.java:16)
]]></failure>
  </testcase>
  <testcase name="later" classname="com.x.UserTest" time="0.0">
    <skipped message="void com.x.UserTest.later() is @Disabled"/>
  </testcase>
  <testcase name="positive(int)[1]" classname="com.x.UserTest" time="0.083"/>
  <testcase name="positive(int)[2]" classname="com.x.UserTest" time="0.007"/>
  <testcase name="deep" classname="com.x.UserTest$Inner" time="0.011"/>
</testsuite>"#;

    const SOURCE: &str = "package com.x;\nclass UserTest {\n    @Test void adds() {}\n    @Test void fails() {}\n    \
        @Disabled @Test void later() {}\n    @ParameterizedTest\n    @ValueSource(ints = {1, 2})\n    void positive(int n) {}\n    \
        @Nested\n    class Inner {\n        @Test void deep() {}\n    }\n}\n";

    #[test]
    fn reads_fresh_surefire_reports_in_every_module() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let file = "core/src/test/java/com/x/UserTest.java";
        std::fs::create_dir_all(root.join("core/src/test/java/com/x")).unwrap();
        std::fs::create_dir_all(root.join("core/target/surefire-reports")).unwrap();
        std::fs::write(root.join("pom.xml"), "<project/>").unwrap();
        std::fs::write(root.join("core/pom.xml"), "<project/>").unwrap();
        std::fs::write(root.join(file), SOURCE).unwrap();
        let stale = root.join("core/target/surefire-reports/TEST-com.x.Stale.xml");
        std::fs::write(&stale, REPORT).unwrap();
        let old = SystemTime::now() - std::time::Duration::from_secs(60);
        std::fs::File::options().write(true).open(&stale).unwrap().set_modified(old).unwrap();
        let started = SystemTime::now() - std::time::Duration::from_secs(1);
        std::fs::write(root.join("core/target/surefire-reports/TEST-com.x.UserTest$Inner.xml"), REPORT).unwrap();

        let tests = parse_jvm_file(file, SOURCE);
        assert_eq!(tests.len(), 5);
        let parser = MavenParser { jvm: JvmParser::new(&tests), root: root.to_path_buf(), started };
        let found = parse(Box::new(parser), "[INFO] BUILD FAILURE");
        let id = |name: &str| format!("{file}::{name}");
        let results: Vec<_> = results(&found).into_iter().map(|(id, o)| (id.to_owned(), o)).collect();
        assert_eq!(
            results,
            [
                (id("adds"), Outcome::Passed),
                (id("fails"), Outcome::Failed),
                (id("later"), Outcome::Skipped),
                (id("positive"), Outcome::Passed),
                (id("Inner > deep"), Outcome::Passed),
            ]
        );
        let failures = failures(&found);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].0, id("fails"));
        assert!(failures[0].1.iter().any(|l| l.contains("UserTest.java:16")));
    }

    #[test]
    fn builds_surefire_filters() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let file = "src/test/java/com/x/UserTest.java";
        std::fs::create_dir_all(root.join("src/test/java/com/x")).unwrap();
        std::fs::write(root.join(file), SOURCE).unwrap();
        let script = |target: TestTarget| Maven.start(root, "mvn", &target).unwrap().script;
        assert!(script(TestTarget::All).ends_with("-DfailIfNoTests=false"));
        assert!(
            script(TestTarget::Paths { paths: vec!["src/test/java/com".into()] })
                .ends_with("-Dtest='com.x.UserTest,com.x.UserTest$*'")
        );
        let cases = vec![
            crate::testing::CaseRef { id: format!("{file}::adds"), line: None },
            crate::testing::CaseRef { id: format!("{file}::positive"), line: None },
            crate::testing::CaseRef { id: format!("{file}::Inner > deep"), line: None },
        ];
        assert!(
            script(TestTarget::Cases { cases }).ends_with("-Dtest='com.x.UserTest#adds+positive,com.x.UserTest$Inner#deep'")
        );
    }
}
