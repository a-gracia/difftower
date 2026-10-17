//! Messages between Difftower and `difftower-agent`, one JSON object per line
//! over the agent's stdin and stdout.

use serde::{Deserialize, Serialize};

use crate::{Change, Runner, TestEvent, TestTarget};

/// Bumped whenever a message changes shape.
pub const PROTOCOL: u32 = 7;

#[derive(Debug, Serialize, Deserialize)]
pub struct Call {
    pub id: u64,
    pub request: Request,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Status,
    Revert { changes: Vec<Change>, end_session: bool },
    Undo,
    /// Answered with the file's bytes in standard base64.
    ReadFile { path: String },
    /// Answered with the patch as a string.
    FileDiff { change: Change },
    StartSession,
    EndSession,
    Accept { paths: Vec<String> },
    SaveRules { rules: Vec<String> },
    /// Answered with every [`crate::TestSuite`].
    Tests,
    /// Answered at once; the run reports with [`Message::Test`] under `run`.
    RunTests { run: u64, repo: String, runner: Runner, target: TestTarget },
    /// Answered at once; the run still reports that it finished.
    StopTests { run: u64 },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Message {
    /// First message once the agent opened the repository.
    Hello {
        protocol: u32,
        root: String,
        /// Defaulted so an agent from an older protocol still reports its version.
        #[serde(default)]
        repos: Vec<String>,
    },
    /// The agent could not start; it exits after sending this.
    Fatal { error: String },
    /// The working tree or HEAD changed.
    Changed,
    Reply { id: u64, result: Result<serde_json::Value, String> },
    /// Progress of the test run started as `run`.
    Test { run: u64, event: TestEvent },
}
