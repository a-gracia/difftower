//! Minitest outside Rails: `test/**/*_test.rb` or `test_*.rb`, loaded by a
//! small Ruby script so any file, folder or test runs alone, whatever the
//! Rakefile does.

use std::{collections::HashMap, path::Path};

use anyhow::Result;

use super::{
    Found, Kind, Parser, Started, TestCase, TestTarget, file_of, quote, rails, read, regex_escape, target_args,
};

pub(crate) struct Minitest;

/// Requires the test files given before `--`, or every one, and leaves the
/// options after it to minitest.
const LOADER: &str = r#"i = ARGV.index("--") || ARGV.size; files = ARGV[0...i]; ARGV.replace(ARGV[(i + 1)..] || []); files = ["test"] if files.empty?; files.flat_map { |f| File.directory?(f) ? Dir[File.join(f, "**", "{*_test,test_*}.rb")] : [f] }.sort.each { |f| require File.expand_path(f) }"#;

impl Kind for Minitest {
    fn detect(&self, root: &Path) -> bool {
        if root.join("bin/rails").is_file() || !root.join("test").is_dir() {
            return false;
        }
        let uses = root.join("Rakefile").is_file() || read(root, "Gemfile").contains("minitest");
        uses && ignore::WalkBuilder::new(root.join("test"))
            .build()
            .flatten()
            .any(|entry| entry.file_name().to_str().is_some_and(is_test_file))
    }

    fn default_command(&self, root: &Path) -> String {
        let ruby = "ruby -Itest -Ilib";
        if root.join("Gemfile").is_file() { format!("bundle exec {ruby}") } else { ruby.to_owned() }
    }

    fn discover(&self, root: &Path, _command: &str) -> Result<Vec<TestCase>> {
        Ok(tests(root))
    }

    fn start(&self, root: &Path, command: &str, target: &TestTarget) -> Result<Started> {
        // Files to load, then minitest's options: `-n` picks the tests.
        let (files, filter) = match target {
            TestTarget::Cases { cases } => {
                let mut files: Vec<String> = cases.iter().map(|case| file_of(&case.id).to_owned()).collect();
                files.sort();
                files.dedup();
                let names: Vec<String> = cases.iter().map(|case| regex_escape(key(&case.id))).collect();
                let filter = format!(" -n {}", quote(&format!("/^(?:{})$/", names.join("|"))));
                (files.iter().map(|f| quote(f)).collect::<Vec<_>>().join(" "), filter)
            }
            _ => (target_args(target, |case| case.id.clone()), String::new()),
        };
        let names = tests(root).into_iter().map(|t| (key(&t.id).to_owned(), t.id)).collect();
        Ok(Started {
            script: format!("{command} -e {} -- {files} -- -v{filter}", quote(LOADER)),
            parser: Box::new(PlainMinitestParser::new(names)),
        })
    }
}

fn is_test_file(name: &str) -> bool {
    name.ends_with(".rb") && (name.ends_with("_test.rb") || name.starts_with("test_"))
}

/// Every test under `test/`, read from the files.
fn tests(root: &Path) -> Vec<TestCase> {
    super::walk(root, |file| {
        file.starts_with("test/") && is_test_file(file.rsplit('/').next().unwrap_or(file))
    })
    .iter()
    .flat_map(|file| rails::parse_file(file, &std::fs::read_to_string(root.join(file)).unwrap_or_default()))
    .collect()
}

/// `Class#method`, the part of a test id minitest prints.
fn key(id: &str) -> &str {
    id.split_once("::").map_or(id, |(_, key)| key)
}

/// Minitest's own reporter: as Rails', but its blocks are numbered,
/// `  1) Failure:`, skips get one too, and none ends with a rerun command.
struct PlainMinitestParser {
    inner: rails::MinitestParser,
    /// Inside a `Skipped:` block, which is no failure.
    skipping: bool,
}

impl PlainMinitestParser {
    fn new(names: HashMap<String, String>) -> Self {
        Self { inner: rails::MinitestParser::with_names(names), skipping: false }
    }
}

impl Parser for PlainMinitestParser {
    fn feed(&mut self, line: &str) -> Vec<Found> {
        let header = line
            .trim_start()
            .split_once(") ")
            .filter(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
            .map(|(_, rest)| rest);
        match header {
            Some(kind @ ("Failure:" | "Error:")) => {
                self.skipping = false;
                return self.inner.feed(kind);
            }
            Some("Skipped:") => {
                self.skipping = true;
                return self.inner.finish();
            }
            _ => {}
        }
        // `3 runs, 5 assertions, 1 failures, 0 errors, 0 skips` closes the last block.
        if line.split_once(" runs, ").is_some_and(|(n, _)| n.parse::<u32>().is_ok()) {
            return self.inner.finish();
        }
        if self.skipping {
            return Vec::new();
        }
        self.inner.feed(line)
    }

    fn finish(&mut self) -> Vec<Found> {
        self.inner.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{
        Outcome,
        test_support::{failures, parse, results},
    };

    #[test]
    fn reads_minitest_output() {
        // Captured from minitest 5 with `-v`.
        let output = "\
Run options: -v --seed 60798

# Running:

Math2::CalcTest#test_fails = 0.00 s = F
Math2::CalcTest#test_skipped = 0.00 s = S
Math2::CalcTest#test_errors? = 0.00 s = E
Math2::CalcTest#test_adds = 0.00 s = .
OtherTest#test_ok = 0.00 s = .

Finished in 0.025134s, 198.9336 runs/s, 119.3601 assertions/s.

  1) Failure:
Math2::CalcTest#test_fails [test/models/calc_test.rb:10]:
Expected: 4
  Actual: 3

  2) Skipped:
Math2::CalcTest#test_skipped [test/models/calc_test.rb:18]:
later

  3) Error:
Math2::CalcTest#test_errors?:
RuntimeError: boom
    test/models/calc_test.rb:14:in 'Math2::CalcTest#test_errors?'

5 runs, 3 assertions, 1 failures, 1 errors, 1 skips";
        let file = "test/models/calc_test.rb";
        let names: HashMap<String, String> = ["test_fails", "test_skipped", "test_errors?", "test_adds"]
            .iter()
            .map(|m| (format!("Math2::CalcTest#{m}"), format!("{file}::Math2::CalcTest#{m}")))
            .chain([("OtherTest#test_ok".to_owned(), "test/other_test.rb::OtherTest#test_ok".to_owned())])
            .collect();
        let found = parse(Box::new(PlainMinitestParser::new(names)), output);
        assert_eq!(
            results(&found),
            [
                ("test/models/calc_test.rb::Math2::CalcTest#test_fails", Outcome::Failed),
                ("test/models/calc_test.rb::Math2::CalcTest#test_skipped", Outcome::Skipped),
                ("test/models/calc_test.rb::Math2::CalcTest#test_errors?", Outcome::Failed),
                ("test/models/calc_test.rb::Math2::CalcTest#test_adds", Outcome::Passed),
                ("test/other_test.rb::OtherTest#test_ok", Outcome::Passed),
            ]
        );
        let failed = failures(&found);
        assert_eq!(failed.len(), 2);
        assert_eq!(failed[0].0, "test/models/calc_test.rb::Math2::CalcTest#test_fails");
        assert_eq!(failed[0].1, ["Expected: 4", "  Actual: 3"]);
        assert_eq!(failed[1].0, "test/models/calc_test.rb::Math2::CalcTest#test_errors?");
        assert_eq!(
            failed[1].1,
            ["RuntimeError: boom", "    test/models/calc_test.rb:14:in 'Math2::CalcTest#test_errors?'"]
        );
    }

    #[test]
    fn detects_plain_minitest_but_not_rails() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("test/models")).unwrap();
        std::fs::write(root.join("test/models/calc_test.rb"), "").unwrap();
        assert!(!Minitest.detect(root));
        std::fs::write(root.join("Rakefile"), "").unwrap();
        assert!(Minitest.detect(root));
        std::fs::create_dir(root.join("bin")).unwrap();
        std::fs::write(root.join("bin/rails"), "").unwrap();
        assert!(!Minitest.detect(root));
    }

    #[test]
    fn runs_cases_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let target = TestTarget::Cases {
            cases: vec![
                crate::testing::CaseRef { id: "test/a_test.rb::A::BTest#test_x?".into(), line: Some(3) },
                crate::testing::CaseRef { id: "test/a_test.rb::A::BTest#test_y".into(), line: Some(6) },
            ],
        };
        let started = Minitest.start(dir.path(), "ruby -Itest -Ilib", &target).unwrap();
        assert!(started.script.ends_with(r"-- 'test/a_test.rb' -- -v -n '/^(?:A::BTest\#test_x\?|A::BTest\#test_y)$/'"));
    }
}
