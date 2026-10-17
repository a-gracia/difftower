//! Rails' minitest, through `bin/rails test`.

use std::{collections::HashMap, path::Path};

use anyhow::Result;

use super::{
    Blocks, Found, Kind, Outcome, Parser, Started, TestCase, TestTarget, file_and_line, target_args, walk,
};

pub(crate) struct Rails;

impl Kind for Rails {
    fn detect(&self, root: &Path) -> bool {
        root.join("bin/rails").is_file() && root.join("test").is_dir()
    }

    fn default_command(&self, _root: &Path) -> String {
        "bin/rails test".to_owned()
    }

    fn discover(&self, root: &Path, _command: &str) -> Result<Vec<TestCase>> {
        Ok(tests(root, "test/"))
    }

    fn start(&self, root: &Path, command: &str, target: &TestTarget) -> Result<Started> {
        let args = target_args(target, |case| file_and_line(&case.id, case.line));
        Ok(Started { script: format!("{command} -v {args}"), parser: Box::new(MinitestParser::new(root, "test/")) })
    }
}

/// Every test under `dir`, read from the files, so listing needs neither
/// Ruby nor a database. System tests are left out, as `bin/rails test` does.
pub(crate) fn tests(root: &Path, dir: &str) -> Vec<TestCase> {
    let files = walk(root, |file| file.starts_with(dir) && file.ends_with("_test.rb") && !file.starts_with("test/system/"));
    files
        .iter()
        .flat_map(|file| parse_file(file, &std::fs::read_to_string(root.join(file)).unwrap_or_default()))
        .collect()
}

pub(crate) fn parse_file(file: &str, text: &str) -> Vec<TestCase> {
    // Enclosing modules and classes, with their indentation.
    let mut scopes: Vec<(usize, String)> = Vec::new();
    let mut tests = Vec::new();
    for (ix, line) in text.lines().enumerate() {
        let indent = line.len() - line.trim_start().len();
        let trimmed = line.trim_start();
        let scope = trimmed.strip_prefix("class ").or_else(|| trimmed.strip_prefix("module "));
        if let Some(rest) = scope {
            let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == ':').collect();
            if name.starts_with(|c: char| c.is_ascii_uppercase()) {
                scopes.retain(|(level, _)| *level < indent);
                scopes.push((indent, name));
            }
            continue;
        }
        let method = if let Some(rest) = trimmed.strip_prefix("test ").or_else(|| trimmed.strip_prefix("test(")) {
            let rest = rest.trim_start();
            let Some(quote) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else { continue };
            let Some(end) = rest[1..].find(quote) else { continue };
            let name = &rest[1..1 + end];
            (format!("test_{}", name.split_whitespace().collect::<Vec<_>>().join("_")), name.to_owned())
        } else if let Some(rest) = trimmed.strip_prefix("def test_") {
            let name: String =
                rest.chars().take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '?' | '!')).collect();
            (format!("test_{name}"), format!("test_{name}"))
        } else {
            continue;
        };
        scopes.retain(|(level, _)| *level < indent);
        let class = scopes.iter().map(|(_, name)| name.as_str()).collect::<Vec<_>>().join("::");
        if class.is_empty() {
            continue;
        }
        let (method, name) = method;
        tests.push(TestCase {
            id: format!("{file}::{class}#{method}"),
            file: file.to_owned(),
            name,
            line: Some(ix as u32 + 1),
        });
    }
    tests
}

/// `Class#method`, the part of a test id minitest prints.
fn key(id: &str) -> &str {
    id.split_once("::").map_or(id, |(_, key)| key)
}

/// `UserTest#test_name = 0.01 s = .`, with `F`, `E` or `S` for failed,
/// errored and skipped tests: minitest's verbose output.
fn parse_result(line: &str) -> Option<(String, Outcome)> {
    let (rest, mark) = line.trim_end().rsplit_once(" = ")?;
    let outcome = match mark {
        "." => Outcome::Passed,
        "F" | "E" => Outcome::Failed,
        "S" | "N" => Outcome::Skipped,
        _ => return None,
    };
    let (key, time) = rest.rsplit_once(" = ")?;
    time.strip_suffix(" s")?.parse::<f64>().ok()?;
    key.contains('#').then(|| (key.to_owned(), outcome))
}

/// Minitest's verbose output: results, then `Failure:` or `Error:` blocks
/// naming `Class#method`, each up to the command that reruns it.
pub(crate) struct MinitestParser {
    /// `Class#method` to test ids.
    names: HashMap<String, String>,
    blocks: Blocks,
    /// The next line names the failed test.
    expect_name: bool,
}

impl MinitestParser {
    /// Knows the tests under `dir`, to give the file of each.
    pub fn new(root: &Path, dir: &str) -> Self {
        let names = tests(root, dir).into_iter().map(|t| (key(&t.id).to_owned(), t.id)).collect();
        Self::with_names(names)
    }

    pub fn with_names(names: HashMap<String, String>) -> Self {
        Self { names, blocks: Blocks::default(), expect_name: false }
    }
}

impl Parser for MinitestParser {
    fn feed(&mut self, line: &str) -> Vec<Found> {
        if let Some((key, outcome)) = parse_result(line) {
            return self.names.get(&key).map(|id| Found::Result(id.clone(), outcome)).into_iter().collect();
        }
        if line == "Failure:" || line == "Error:" {
            self.expect_name = true;
            return self.blocks.end();
        }
        if std::mem::take(&mut self.expect_name) {
            let name = line.split(" [").next().unwrap_or(line).trim_end_matches(':');
            // A test not found in the files, say generated, has no block.
            let ids = self.names.get(name).cloned().into_iter().collect();
            return self.blocks.start(ids);
        }
        self.blocks.push(line);
        // `bin/rails test file:line`, or `rake test …` outside Rails.
        if self.blocks.reading() && (line.starts_with("bin/rails test ") || line.starts_with("rails test ")) {
            return self.blocks.end();
        }
        Vec::new()
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
    fn reads_rails_tests_and_results() {
        let file = r#"
require "test_helper"

module Admin
  class UsersTest < ActiveSupport::TestCase
    test "lists   everyone, even the closed" do
    end

    def test_old_style?
    end
  end
end

class MailerTest < ActionMailer::TestCase
  test 'sends it' do
  end
end
"#;
        let tests = parse_file("test/x_test.rb", file);
        let ids: Vec<_> = tests.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "test/x_test.rb::Admin::UsersTest#test_lists_everyone,_even_the_closed",
                "test/x_test.rb::Admin::UsersTest#test_old_style?",
                "test/x_test.rb::MailerTest#test_sends_it",
            ]
        );
        assert_eq!((tests[0].name.as_str(), tests[0].line), ("lists   everyone, even the closed", Some(6)));

        assert_eq!(
            parse_result("Admin::UsersTest#test_old_style? = 0.12 s = F"),
            Some(("Admin::UsersTest#test_old_style?".into(), Outcome::Failed))
        );
        assert_eq!(parse_result("Finished in 1.2 s = ."), None);
    }

    #[test]
    fn reads_rails_failures() {
        let output = "\
UserTest#test_x = 0.01 s = F

Failure:
UserTest#test_x [test/models/user_test.rb:10]:
Expected: 1
  Actual: 2

bin/rails test test/models/user_test.rb:8

Error:
UserTest#test_y:
NameError: nope

bin/rails test test/models/user_test.rb:12
";
        let names = [
            ("UserTest#test_x", "test/models/user_test.rb::UserTest#test_x"),
            ("UserTest#test_y", "test/models/user_test.rb::UserTest#test_y"),
        ];
        let names = names.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        let found = parse(Box::new(MinitestParser::with_names(names)), output);
        assert_eq!(results(&found), [("test/models/user_test.rb::UserTest#test_x", Outcome::Failed)]);
        let found = failures(&found);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].0, "test/models/user_test.rb::UserTest#test_x");
        assert_eq!(found[0].1.first().unwrap(), "Expected: 1");
        assert_eq!(found[1].1, ["NameError: nope", "", "bin/rails test test/models/user_test.rb:12"]);
    }
}
