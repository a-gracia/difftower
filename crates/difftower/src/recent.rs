//! Repositories opened recently, most recent first. Each entry is a local
//! path or `host:path`, exactly as it can be opened again.

use std::path::PathBuf;

const LIMIT: usize = 15;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Recent {
    entries: Vec<String>,
}

impl Recent {
    pub fn load() -> Self {
        let text = file().and_then(|path| std::fs::read_to_string(path).ok()).unwrap_or_default();
        Self::parse(&text)
    }

    fn parse(text: &str) -> Self {
        Self { entries: text.lines().filter(|l| !l.is_empty()).map(str::to_owned).collect() }
    }

    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    /// Moves `location` to the top.
    pub fn push(&mut self, location: &str) {
        self.entries.retain(|e| e != location);
        self.entries.insert(0, location.to_owned());
        self.entries.truncate(LIMIT);
    }

    pub fn remove(&mut self, location: &str) {
        self.entries.retain(|e| e != location);
    }

    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = file() else { return Ok(()) };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, self.entries.iter().map(|e| format!("{e}\n")).collect::<String>())
    }
}

/// `$XDG_STATE_HOME/difftower/recent`, defaulting to `~/.local/state`.
fn file() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))?;
    Some(state.join("difftower").join("recent"))
}

/// A short form of a location for display: the home directory becomes `~`.
pub fn display(location: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if location == home => "~".into(),
        Ok(home) if location.starts_with(&format!("{home}/")) => format!("~{}", &location[home.len()..]),
        _ => location.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn most_recent_first_without_duplicates() {
        let mut recent = Recent::parse("/a\nbox:/srv/b\n");
        recent.push("/c");
        recent.push("box:/srv/b");
        assert_eq!(recent.entries(), ["box:/srv/b", "/c", "/a"]);
        recent.remove("/c");
        assert_eq!(recent.entries(), ["box:/srv/b", "/a"]);
    }

    #[test]
    fn keeps_a_bounded_list() {
        let mut recent = Recent::default();
        for i in 0..20 {
            recent.push(&format!("/repo{i}"));
        }
        assert_eq!(recent.entries().len(), LIMIT);
        assert_eq!(recent.entries()[0], "/repo19");
    }
}
