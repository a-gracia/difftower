use std::{path::PathBuf, sync::Arc};

use anyhow::Result;
use difftower_core::{Backend, Remote, SshTarget, open_path};

use crate::terminal::TerminalCommand;

/// The agent installed on remote hosts, built by `build.rs`.
const AGENT: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/difftower-agent"));

/// An opened repository, ready for a workspace.
pub struct Opened {
    pub backend: Arc<dyn Backend>,
    pub shell: TerminalCommand,
    /// How to open the same repository again, for the recent list.
    pub location: String,
}

/// Opens a local path or `[user@]host:path`, either a repository or a
/// directory of repositories. Blocks while connecting, so call it off the
/// UI thread; `progress` receives each step.
pub fn open(location: &str, progress: &dyn Fn(&str)) -> Result<Opened> {
    let location = location.trim();
    let path = expand_home(location);
    // A path that exists locally wins over a `host:path` reading of it.
    let target = SshTarget::parse(location).filter(|_| !path.exists());
    match target {
        None => {
            let backend: Arc<dyn Backend> = open_path(&path)?.into();
            let root = PathBuf::from(backend.root());
            Ok(Opened {
                location: root.display().to_string(),
                shell: TerminalCommand::Shell { cwd: root },
                backend,
            })
        }
        Some(target) => {
            let remote = Remote::connect(target, AGENT, progress)?;
            let (program, args) = remote.target().shell(&remote.root());
            Ok(Opened {
                location: format!("{}:{}", remote.target().host, remote.root()),
                shell: TerminalCommand::Program { program, args },
                backend: Arc::new(remote),
            })
        }
    }
}

fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix('~'), std::env::var_os("HOME")) {
        (Some(""), Some(home)) => PathBuf::from(home),
        (Some(rest), Some(home)) if rest.starts_with('/') => PathBuf::from(home).join(&rest[1..]),
        _ => PathBuf::from(path),
    }
}
