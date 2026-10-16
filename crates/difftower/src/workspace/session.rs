//! What a workspace showed, restored when the same repository opens again:
//! the open file, the sidebar tab and the unfolded folders.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
pub(super) struct Session {
    pub selected: Option<String>,
    pub tab: Option<String>,
    pub expanded: Vec<String>,
}

impl Session {
    /// The session last saved for the repository named by `key`, such as
    /// `host:/path`, or an empty one.
    pub fn load(key: &str) -> Self {
        state_file("sessions", key)
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, key: &str) {
        write_state("sessions", key, self);
    }
}

/// `$XDG_STATE_HOME/difftower/<kind>/<hash of key>.json`, defaulting to
/// `~/.local/state`: one file per repository.
pub(super) fn state_file(kind: &str, key: &str) -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))?;
    Some(state.join("difftower").join(kind).join(format!("{:016x}.json", fnv(key.as_bytes()))))
}

/// Writes `value` where [`state_file`] says; state is a convenience, so a
/// failure is ignored.
pub(super) fn write_state(kind: &str, key: &str, value: &impl Serialize) {
    let Some(path) = state_file(kind, key) else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec(value) {
        let _ = std::fs::write(path, json);
    }
}

/// FNV-1a, stable across builds unlike `DefaultHasher`.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3))
}
