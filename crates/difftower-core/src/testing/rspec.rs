//! RSpec.
//!
//! Examples are read from the spec files, so listing needs neither Ruby nor
//! a database. A run prints the documentation format, read as it comes, and
//! writes the JSON format to a report file, read at the end for exact
//! outcomes and failures.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::Result;
use serde::Deserialize;

use super::{
    Found, Kind, Outcome, Parser, Started, TestCase, TestTarget, file_and_line, quote, read, report_path,
    target_args, walk,
};

pub(crate) struct Rspec;

impl Kind for Rspec {
    fn detect(&self, root: &Path) -> bool {
        root.join("spec").is_dir() && (root.join(".rspec").is_file() || read(root, "Gemfile").contains("rspec"))
    }

    fn default_command(&self, root: &Path) -> String {
        if root.join("bin/rspec").is_file() {
            "bin/rspec".to_owned()
        } else if root.join("Gemfile").is_file() {
            "bundle exec rspec".to_owned()
        } else {
            "rspec".to_owned()
        }
    }

    fn discover(&self, root: &Path, _command: &str) -> Result<Vec<TestCase>> {
        Ok(tests(root))
    }

    fn start(&self, root: &Path, command: &str, target: &TestTarget) -> Result<Started> {
        let report = report_path("rspec.json");
        let args = target_args(target, |case| file_and_line(&case.id, case.line));
        Ok(Started {
            script: format!(
                "{command} --no-color --format documentation --format json --out {} {args}",
                quote(&report.display().to_string())
            ),
            parser: Box::new(RspecParser::new(tests(root), report)),
        })
    }
}

/// Every example under `spec/`.
fn tests(root: &Path) -> Vec<TestCase> {
    walk(root, |file| file.starts_with("spec/") && file.ends_with("_spec.rb"))
        .iter()
        .flat_map(|file| parse_file(file, &std::fs::read_to_string(root.join(file)).unwrap_or_default()))
        .collect()
}

const GROUPS: [&str; 13] = [
    "describe",
    "context",
    "feature",
    "xdescribe",
    "xcontext",
    "xfeature",
    "fdescribe",
    "fcontext",
    "ffeature",
    "example_group",
    "shared_examples",
    "shared_examples_for",
    "shared_context",
];

const EXAMPLES: [&str; 14] = [
    "it", "specify", "example", "scenario", "its", "xit", "xspecify", "xexample", "xscenario", "fit", "fspecify",
    "fexample", "fscenario", "pending",
];

/// A group's description, and whether its last argument is a constant, as
/// RSpec joins `describe Calc` and `describe "#add"` into `Calc#add`.
#[derive(Clone)]
struct Group {
    full: String,
    module_last: bool,
    /// Inside shared examples: run only where included, so not listed.
    shared: bool,
}

fn parse_file(file: &str, text: &str) -> Vec<TestCase> {
    let mut scopes: Vec<(usize, Group)> = Vec::new();
    let mut tests = Vec::new();
    for (ix, line) in text.lines().enumerate() {
        let indent = line.len() - line.trim_start().len();
        let trimmed = line.trim_start();
        let trimmed = trimmed.strip_prefix("RSpec.").unwrap_or(trimmed);
        // A block opens: an example or group on one line has `do` or `{`.
        let opens = trimmed.contains(" do") || trimmed.contains('{');
        let Some((word, rest)) = keyword(trimmed) else { continue };
        if !opens {
            continue;
        }
        scopes.retain(|(level, _)| *level < indent);
        let parent = scopes.last().map(|(_, group)| group.clone());
        let args = arguments(rest);
        if GROUPS.contains(&word) {
            let own = describe(&args);
            let full = match &parent {
                Some(parent) if !parent.full.is_empty() => {
                    let first = args.first().map_or("", |(text, _)| text.as_str());
                    let glue = if parent.module_last && starts_method(first) { "" } else { " " };
                    format!("{}{glue}{own}", parent.full)
                }
                _ => own,
            };
            let shared = word.starts_with("shared_") || parent.as_ref().is_some_and(|p| p.shared);
            let module_last = args.last().is_some_and(|(_, module)| *module);
            scopes.push((indent, Group { full, module_last, shared }));
        } else if EXAMPLES.contains(&word) {
            let Some(parent) = parent.filter(|p| !p.shared) else { continue };
            let line = ix as u32 + 1;
            let own = args.first().filter(|(_, module)| !module).map(|(text, _)| text.clone());
            let name = match own {
                Some(own) if parent.full.is_empty() => own,
                Some(own) => format!("{} {own}", parent.full),
                // Named after its expectation once it has run: the line stands in.
                None => format!("{} (line {line})", parent.full),
            };
            tests.push(TestCase { id: format!("{file}::{name}"), file: file.to_owned(), name, line: Some(line) });
        }
    }
    tests
}

/// The keyword starting `line` and what follows it, if it is a group or an
/// example keyword followed by its arguments.
fn keyword(line: &str) -> Option<(&'static str, &str)> {
    let end = line.find(|c: char| !(c.is_alphanumeric() || c == '_')).unwrap_or(line.len());
    let word = &line[..end];
    let rest = &line[end..];
    if !(rest.starts_with(' ') || rest.starts_with('(') || rest.starts_with('{')) {
        return None;
    }
    GROUPS.iter().chain(EXAMPLES.iter()).find(|w| **w == word).map(|w| (*w, rest))
}

/// The leading string and constant arguments: `"x"`, `Calc` or `Calc, "y"`,
/// each with whether it is a constant.
fn arguments(rest: &str) -> Vec<(String, bool)> {
    let mut args = Vec::new();
    let mut rest = rest.trim_start().trim_start_matches('(').trim_start();
    loop {
        let mut chars = rest.chars();
        match chars.next() {
            Some(quote @ ('"' | '\'')) => {
                let body = chars.as_str();
                let mut end = None;
                let mut escaped = false;
                for (ix, c) in body.char_indices() {
                    if escaped {
                        escaped = false;
                    } else if c == '\\' {
                        escaped = true;
                    } else if c == quote {
                        end = Some(ix);
                        break;
                    }
                }
                let Some(end) = end else { break };
                args.push((body[..end].replace(&format!("\\{quote}"), &quote.to_string()), false));
                rest = &body[end + 1..];
            }
            Some(c) if c.is_ascii_uppercase() => {
                let end = rest.find(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':')).unwrap_or(rest.len());
                args.push((rest[..end].to_owned(), true));
                rest = &rest[end..];
            }
            _ => break,
        }
        rest = rest.trim_start();
        match rest.strip_prefix(',') {
            Some(after) => rest = after.trim_start(),
            None => break,
        }
    }
    args
}

/// A group's own description from its arguments, joined as RSpec joins them.
fn describe(args: &[(String, bool)]) -> String {
    let mut out = String::new();
    let mut module_last = false;
    for (text, module) in args {
        if !out.is_empty() && !(module_last && starts_method(text)) {
            out.push(' ');
        }
        out.push_str(text);
        module_last = *module;
    }
    out
}

fn starts_method(text: &str) -> bool {
    text.starts_with('#') || text.starts_with('.') || text.starts_with("::")
}

/// Reads the documentation format while the run goes, then the JSON report.
struct RspecParser {
    /// Ids by file and line, and by full description when that is unique.
    by_line: HashMap<(String, u32), String>,
    by_name: HashMap<String, String>,
    report: PathBuf,
    /// The groups printed above the current line, with their indentation.
    groups: Vec<(usize, String)>,
    /// Past the examples, into the pending and failure summaries.
    done: bool,
}

impl RspecParser {
    fn new(tests: Vec<TestCase>, report: PathBuf) -> Self {
        let mut by_name: HashMap<String, Option<String>> = HashMap::new();
        let mut by_line = HashMap::new();
        for test in tests {
            if let Some(line) = test.line {
                by_line.insert((test.file.clone(), line), test.id.clone());
            }
            by_name.entry(test.name).and_modify(|id| *id = None).or_insert(Some(test.id));
        }
        let by_name = by_name.into_iter().filter_map(|(name, id)| Some((name, id?))).collect();
        Self { by_line, by_name, report, groups: Vec::new(), done: false }
    }
}

impl Parser for RspecParser {
    fn feed(&mut self, line: &str) -> Vec<Found> {
        let trimmed = line.trim();
        if self.done || trimmed.is_empty() {
            return Vec::new();
        }
        if !line.starts_with(' ') && ["Pending:", "Failures:", "Finished in ", "Failed examples:"].iter().any(|s| trimmed.starts_with(s)) {
            self.done = true;
            return Vec::new();
        }
        let indent = line.len() - line.trim_start().len();
        let (text, outcome) = if let Some(at) = trimmed.rfind(" (FAILED - ") {
            (&trimmed[..at], Some(Outcome::Failed))
        } else if let Some(at) = trimmed.rfind(" (PENDING: ") {
            (&trimmed[..at], Some(Outcome::Skipped))
        } else {
            (trimmed, None)
        };
        self.groups.retain(|(level, _)| *level < indent);
        let full = match self.groups.last() {
            Some((_, parent)) => {
                let glue = if starts_method(text) { "" } else { " " };
                format!("{parent}{glue}{text}")
            }
            None => text.to_owned(),
        };
        match self.by_name.get(&full) {
            Some(id) => vec![Found::Result(id.clone(), outcome.unwrap_or(Outcome::Passed))],
            // Not an example we know: a group, unless it says how it ended.
            None => {
                if outcome.is_none() {
                    self.groups.push((indent, full));
                }
                Vec::new()
            }
        }
    }

    fn finish(&mut self) -> Vec<Found> {
        let text = std::fs::read_to_string(&self.report).unwrap_or_default();
        let _ = std::fs::remove_file(&self.report);
        let Ok(report) = serde_json::from_str::<Report>(&text) else { return Vec::new() };
        let mut found = Vec::new();
        for example in report.examples {
            let file = example.file_path.trim_start_matches("./").to_owned();
            let id = self
                .by_line
                .get(&(file.clone(), example.line_number))
                .or_else(|| self.by_name.get(&example.full_description))
                .cloned()
                .unwrap_or_else(|| format!("{file}::{}", example.full_description));
            let outcome = match example.status.as_str() {
                "passed" => Outcome::Passed,
                "failed" => Outcome::Failed,
                _ => Outcome::Skipped,
            };
            found.push(Found::Result(id.clone(), outcome));
            if let Some(exception) = example.exception.filter(|_| outcome == Outcome::Failed) {
                let mut lines = vec![format!("{}:", exception.class)];
                lines.extend(exception.message.trim_matches('\n').lines().map(|l| format!("  {l}")));
                // The project's own frames; gems' are noise.
                let frames = exception.backtrace.unwrap_or_default();
                let own: Vec<&String> = frames.iter().filter(|f| !f.contains("/gems/")).take(8).collect();
                if !own.is_empty() {
                    lines.push(String::new());
                    lines.extend(own.into_iter().map(|f| format!("# {}", f.trim_start_matches("./"))));
                }
                found.push(Found::Failure(id, lines));
            }
        }
        found
    }
}

#[derive(Deserialize)]
struct Report {
    examples: Vec<Example>,
}

#[derive(Deserialize)]
struct Example {
    full_description: String,
    status: String,
    file_path: String,
    line_number: u32,
    exception: Option<Exception>,
}

#[derive(Deserialize)]
struct Exception {
    class: String,
    message: String,
    backtrace: Option<Vec<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::test_support::{failures, results};

    const SPEC: &str = r##"require_relative "../../lib/calc"

RSpec.describe Calc do
  describe "#add" do
    it "adds two numbers" do
      expect(Calc.new.add(1, 2)).to eq(3)
    end

    it "fails on purpose" do
      expect(Calc.new.add(1, 2)).to eq(4)
    end

    context "with negatives" do
      it 'handles them' do
        expect(Calc.new.add(-1, -2)).to eq(-3)
      end
      xit "is skipped" do
      end
    end
  end
end

describe "plain group" do
  it("works") { expect(1).to eq(1) }
end
"##;

    #[test]
    fn reads_examples_from_spec_files() {
        let tests = parse_file("spec/models/calc_spec.rb", SPEC);
        let names: Vec<_> = tests.iter().map(|t| (t.name.as_str(), t.line.unwrap())).collect();
        // As RSpec's own `full_description`.
        assert_eq!(
            names,
            [
                ("Calc#add adds two numbers", 5),
                ("Calc#add fails on purpose", 9),
                ("Calc#add with negatives handles them", 14),
                ("Calc#add with negatives is skipped", 17),
                ("plain group works", 24),
            ]
        );
        assert_eq!(tests[0].id, "spec/models/calc_spec.rb::Calc#add adds two numbers");
    }

    #[test]
    fn skips_shared_examples_and_calls_inside_examples() {
        let spec = r#"
RSpec.describe User, type: :model do
  shared_examples "a model" do
    it "is valid" do
    end
  end

  it "waits" do
    pending "not yet"
    skip "later"
  end

  it { is_expected.to be_valid }

  describe ".find", "by id" do
    specify do
    end
  end
end
"#;
        let tests = parse_file("spec/user_spec.rb", spec);
        let names: Vec<_> = tests.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["User waits", "User (line 13)", "User.find by id (line 16)"]);
    }

    #[test]
    fn reads_rspec_output_and_report() {
        let dir = tempfile::tempdir().unwrap();
        let report = dir.path().join("rspec.json");
        // Captured from rspec 3.13 on the spec above.
        std::fs::write(
            &report,
            r#"{"version":"3.13.6","examples":[
{"id":"./spec/models/calc_spec.rb[1:1:1]","description":"adds two numbers","full_description":"Calc#add adds two numbers","status":"passed","file_path":"./spec/models/calc_spec.rb","line_number":5,"run_time":0.0038,"pending_message":null},
{"id":"./spec/models/calc_spec.rb[1:1:2]","description":"fails on purpose","full_description":"Calc#add fails on purpose","status":"failed","file_path":"./spec/models/calc_spec.rb","line_number":9,"run_time":0.05,"pending_message":null,"exception":{"class":"RSpec::Expectations::ExpectationNotMetError","message":"\nexpected: 4\n     got: 3\n\n(compared using ==)\n","backtrace":["./spec/models/calc_spec.rb:10:in 'block (3 levels) in <top (required)>'","/usr/lib/ruby/gems/3.3.0/gems/rspec-core-3.13.6/lib/rspec/core/example.rb:263:in 'instance_exec'"]}},
{"id":"./spec/models/calc_spec.rb[1:1:3:1]","description":"handles them","full_description":"Calc#add with negatives handles them","status":"passed","file_path":"./spec/models/calc_spec.rb","line_number":14,"run_time":0.001,"pending_message":null},
{"id":"./spec/models/calc_spec.rb[1:1:3:2]","description":"is skipped","full_description":"Calc#add with negatives is skipped","status":"pending","file_path":"./spec/models/calc_spec.rb","line_number":17,"run_time":0.0001,"pending_message":"Temporarily skipped with xit"},
{"id":"./spec/models/calc_spec.rb[2:1]","description":"works","full_description":"plain group works","status":"passed","file_path":"./spec/models/calc_spec.rb","line_number":24,"run_time":0.0009,"pending_message":null}
],"summary":{"duration":0.07,"example_count":5,"failure_count":1,"pending_count":1,"errors_outside_of_examples_count":0},"summary_line":"5 examples, 1 failure, 1 pending"}"#,
        )
        .unwrap();
        let output = "\
Calc
  #add
    adds two numbers
    fails on purpose (FAILED - 1)
    with negatives
      handles them
      is skipped (PENDING: Temporarily skipped with xit)

plain group
  works

Pending: (Failures listed here are expected and do not affect your suite's status)

  1) Calc#add with negatives is skipped
     # Temporarily skipped with xit

Failures:

  1) Calc#add fails on purpose
     Failure/Error: expect(Calc.new.add(1, 2)).to eq(4)

Finished in 0.07337 seconds (files took 1.3 seconds to load)
5 examples, 1 failure, 1 pending";
        let tests = parse_file("spec/models/calc_spec.rb", SPEC);
        let mut parser = RspecParser::new(tests, report.clone());
        let live: Vec<Found> = output.lines().flat_map(|line| parser.feed(line)).collect();
        let f = "spec/models/calc_spec.rb::";
        let expected = [
            (format!("{f}Calc#add adds two numbers"), Outcome::Passed),
            (format!("{f}Calc#add fails on purpose"), Outcome::Failed),
            (format!("{f}Calc#add with negatives handles them"), Outcome::Passed),
            (format!("{f}Calc#add with negatives is skipped"), Outcome::Skipped),
            (format!("{f}plain group works"), Outcome::Passed),
        ];
        let expected: Vec<_> = expected.iter().map(|(id, o)| (id.as_str(), *o)).collect();
        assert_eq!(results(&live), expected);

        let end = parser.finish();
        assert_eq!(results(&end), expected);
        let failed = failures(&end);
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].0, format!("{f}Calc#add fails on purpose"));
        assert_eq!(
            failed[0].1,
            [
                "RSpec::Expectations::ExpectationNotMetError:",
                "  expected: 4",
                "       got: 3",
                "  ",
                "  (compared using ==)",
                "",
                "# spec/models/calc_spec.rb:10:in 'block (3 levels) in <top (required)>'",
            ]
        );
        assert!(!report.exists(), "the report is removed once read");
    }

    #[test]
    fn detects_rspec_projects() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join("spec")).unwrap();
        assert!(!Rspec.detect(root));
        std::fs::write(root.join("Gemfile"), "gem \"rspec-rails\"\n").unwrap();
        assert!(Rspec.detect(root));
        assert_eq!(Rspec.default_command(root), "bundle exec rspec");
    }
}
