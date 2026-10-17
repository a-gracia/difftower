//! Per-user preferences, such as panel widths and the diff layout, kept in
//! `$XDG_CONFIG_HOME/difftower/settings.json`. Repository settings shared
//! with a team live in `.difftower.toml` instead.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Width of the file list, in pixels.
    pub sidebar_width: f32,
    /// Width of the terminal, in pixels.
    pub terminal_width: f32,
    pub show_terminal: bool,
    /// Side by side instead of unified.
    pub split: bool,
    pub soft_wrap: bool,
    /// Look for a newer release on start.
    pub check_updates: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            sidebar_width: 280.,
            terminal_width: 560.,
            show_terminal: true,
            split: false,
            soft_wrap: false,
            check_updates: true,
        }
    }
}

impl Settings {
    /// The saved settings, or the defaults when there are none or they
    /// cannot be read; settings never stop the app from starting.
    pub fn load() -> Self {
        file().map(|path| Self::load_from(&path)).unwrap_or_default()
    }

    fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        match file() {
            Some(path) => self.save_to(&path),
            None => Ok(()),
        }
    }

    fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).expect("settings serialize");
        // Write then rename, so a crash never leaves a half-written file.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(tmp, path)
    }
}

fn file() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(config.join("difftower").join("settings.json"))
}

#[cfg(test)]
mod tests {
    use super::Settings;

    #[test]
    fn round_trips_and_tolerates_missing_or_broken_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("difftower/settings.json");
        assert_eq!(Settings::load_from(&path), Settings::default());

        let settings = Settings { sidebar_width: 312., split: true, ..Settings::default() };
        settings.save_to(&path).unwrap();
        assert_eq!(Settings::load_from(&path), settings);

        // Fields added in later versions take their defaults.
        std::fs::write(&path, r#"{"sidebar_width": 200}"#).unwrap();
        assert_eq!(Settings::load_from(&path), Settings { sidebar_width: 200., ..Settings::default() });

        std::fs::write(&path, "not json").unwrap();
        assert_eq!(Settings::load_from(&path), Settings::default());
    }
}
