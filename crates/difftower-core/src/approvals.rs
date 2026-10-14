use std::{collections::HashMap, path::PathBuf};

use anyhow::Result;

use crate::{Change, Repo};

/// Changes to protected files that the user accepted.
///
/// An approval covers one exact patch: if the agent edits the file again,
/// the new patch is no longer approved. Stored in the git directory so it
/// survives restarts without touching the working tree.
pub struct Approvals {
    path: PathBuf,
    entries: HashMap<String, u64>,
}

impl Approvals {
    pub fn load(repo: &Repo) -> Self {
        let path = repo.git_dir().join("difftower").join("approvals");
        let entries = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let (hash, file) = line.split_once('\t')?;
                Some((file.to_owned(), u64::from_str_radix(hash, 16).ok()?))
            })
            .collect();
        Self { path, entries }
    }

    pub fn is_approved(&self, change: &Change) -> bool {
        self.entries.get(&change.path) == Some(&patch_hash(&change.patch))
    }

    pub fn approve(&mut self, change: &Change) -> Result<()> {
        self.entries.insert(change.path.clone(), patch_hash(&change.patch));
        self.save()
    }

    pub fn revoke(&mut self, path: &str) -> Result<()> {
        if self.entries.remove(path).is_some() {
            self.save()?;
        }
        Ok(())
    }

    /// Drops approvals for files that no longer have the approved patch.
    pub fn retain_current(&mut self, changes: &[Change]) -> Result<()> {
        let before = self.entries.len();
        self.entries.retain(|path, hash| {
            changes.iter().any(|c| &c.path == path && patch_hash(&c.patch) == *hash)
        });
        if self.entries.len() != before {
            self.save()?;
        }
        Ok(())
    }

    fn save(&self) -> Result<()> {
        let mut entries: Vec<_> = self.entries.iter().collect();
        entries.sort();
        let text: String = entries.iter().map(|(path, hash)| format!("{hash:016x}\t{path}\n")).collect();
        std::fs::write(&self.path, text)?;
        Ok(())
    }
}

/// FNV-1a, stable across builds unlike `DefaultHasher`.
fn patch_hash(patch: &str) -> u64 {
    patch.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}
