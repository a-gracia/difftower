use std::collections::HashMap;

use crate::Change;

/// Changes to protected files that the user accepted.
///
/// An approval covers one exact patch: if the agent edits the file again,
/// the new patch is no longer approved. The backend stores it in the git
/// directory, so it survives restarts without touching the working tree.
#[derive(Clone, Default)]
pub struct Approvals {
    entries: HashMap<String, u64>,
}

impl Approvals {
    pub fn parse(text: &str) -> Self {
        let entries = text
            .lines()
            .filter_map(|line| {
                let (hash, file) = line.split_once('\t')?;
                Some((file.to_owned(), u64::from_str_radix(hash, 16).ok()?))
            })
            .collect();
        Self { entries }
    }

    pub fn to_text(&self) -> String {
        let mut entries: Vec<_> = self.entries.iter().collect();
        entries.sort();
        entries.iter().map(|(path, hash)| format!("{hash:016x}\t{path}\n")).collect()
    }

    pub fn is_approved(&self, change: &Change) -> bool {
        self.entries.get(&change.path) == Some(&fnv(change.patch.as_bytes()))
    }

    pub fn approve(&mut self, change: &Change) {
        self.entries.insert(change.path.clone(), fnv(change.patch.as_bytes()));
    }

    /// Returns whether anything was removed.
    pub fn revoke(&mut self, path: &str) -> bool {
        self.entries.remove(path).is_some()
    }

    /// Drops approvals for files that no longer have the approved patch.
    /// Returns whether anything was removed.
    pub fn retain_current(&mut self, changes: &[Change]) -> bool {
        let before = self.entries.len();
        self.entries.retain(|path, hash| {
            changes.iter().any(|c| &c.path == path && fnv(c.patch.as_bytes()) == *hash)
        });
        self.entries.len() != before
    }
}

/// FNV-1a, stable across builds unlike `DefaultHasher`.
pub(crate) fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}
