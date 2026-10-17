//! Keeping Difftower current: on start, a look for a newer release, and for
//! a binary its user can write, as `install.sh` puts it in `~/.local/bin`,
//! replacing it in place. It uses the system's `curl`, `sha256sum` and
//! `tar`, as the installer does, and sends nothing but the request.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context as _, Result, bail};

const REPO: &str = "a-gracia/difftower";
const ASSET: &str = "difftower-x86_64-unknown-linux-gnu";
const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// Without the tag's leading `v`.
    pub version: String,
    /// Its notes on GitHub.
    pub url: String,
}

/// How this copy of Difftower can be updated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Install {
    /// The binary's folder is writable: it replaces itself.
    Writable(PathBuf),
    /// Installed where only root writes, as with `sudo`: the installer
    /// updates it.
    System,
    /// Built from source, as with `cargo dt`: no checks at all.
    Development,
}

pub fn install() -> Install {
    let Ok(exe) = std::env::current_exe() else { return Install::System };
    if exe.components().any(|c| c.as_os_str() == "target") {
        return Install::Development;
    }
    match exe.parent() {
        Some(dir) if writable(dir) => Install::Writable(exe),
        _ => Install::System,
    }
}

fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".difftower-write-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(probe);
    ok
}

/// A release newer than this one, if GitHub lists one. Blocking; any
/// failure, such as being offline, counts as none.
pub fn check() -> Option<Release> {
    let out = Command::new("curl")
        .args(["-fsSL", "-m", "10", "-H", "Accept: application/vnd.github+json"])
        .args(["-A", &format!("difftower/{CURRENT}")])
        .arg(format!("https://api.github.com/repos/{REPO}/releases/latest"))
        .output()
        .ok()?;
    let release = parse_latest(&String::from_utf8_lossy(&out.stdout))?;
    newer(&release.version, CURRENT).then_some(release)
}

fn parse_latest(json: &str) -> Option<Release> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let tag = value.get("tag_name")?.as_str()?;
    let url = value.get("html_url")?.as_str()?;
    Some(Release { version: tag.trim_start_matches('v').to_owned(), url: url.to_owned() })
}

/// Whether version `a` comes after `b`, comparing numbers part by part.
pub fn newer(a: &str, b: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> { v.split(['.', '-']).map_while(|p| p.parse().ok()).collect() };
    parts(a) > parts(b)
}

/// Downloads `release`, checks it against its published checksum and puts
/// it in place of `exe`, which keeps running until restarted. Blocking.
pub fn update(release: &Release, exe: &Path) -> Result<()> {
    let dir = std::env::temp_dir().join(format!("difftower-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let result = download_and_replace(release, exe, &dir);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn download_and_replace(release: &Release, exe: &Path, dir: &Path) -> Result<()> {
    let base = format!("https://github.com/{REPO}/releases/download/v{}", release.version);
    let archive = format!("{ASSET}.tar.gz");
    for name in [archive.clone(), format!("{archive}.sha256")] {
        run(Command::new("curl").args(["-fsSL", "-m", "300", "-o", &name, &format!("{base}/{name}")]).current_dir(dir))
            .with_context(|| format!("could not download {name}"))?;
    }
    run(Command::new("sha256sum").args(["-c", &format!("{archive}.sha256")]).current_dir(dir))
        .context("the download does not match its checksum")?;
    run(Command::new("tar").args(["-xzf", &archive]).current_dir(dir)).context("could not unpack the download")?;

    let new = dir.join(ASSET).join("difftower");
    let version = run(Command::new(&new).arg("--version")).context("the new version does not start")?;
    if !version.contains(&release.version) {
        bail!("the download is {}, not {}", version.trim(), release.version);
    }
    // Copied beside the old one, then renamed over it: the swap is atomic,
    // and the running process keeps its own copy until it exits.
    let staged = exe.with_file_name(".difftower.new");
    std::fs::copy(&new, &staged)?;
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    std::fs::rename(&staged, exe).context("could not replace the binary")?;
    Ok(())
}

fn run(cmd: &mut Command) -> Result<String> {
    let out = cmd.output()?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Starts the new binary with the same arguments; the caller then quits.
pub fn relaunch() -> Result<()> {
    let exe = std::env::current_exe()?;
    Command::new(exe).args(std::env::args_os().skip(1)).spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{newer, parse_latest};

    #[test]
    fn compares_versions_by_number() {
        assert!(newer("0.2.0", "0.1.9"));
        assert!(newer("0.10.0", "0.9.3"));
        assert!(newer("1.0.0", "0.99.99"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.1.0", "0.2.0"));
    }

    #[test]
    fn reads_the_latest_release() {
        let json = r#"{"tag_name": "v0.2.0", "html_url": "https://github.com/a-gracia/difftower/releases/tag/v0.2.0", "assets": []}"#;
        let release = parse_latest(json).unwrap();
        assert_eq!(release.version, "0.2.0");
        assert!(release.url.ends_with("/v0.2.0"));
        assert_eq!(parse_latest("not json"), None);
    }

    /// Against the real release on GitHub: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn replaces_a_binary_with_the_published_release() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("difftower");
        std::fs::write(&exe, b"old").unwrap();
        let release = super::Release { version: "0.1.0".into(), url: String::new() };
        super::update(&release, &exe).unwrap();
        let out = std::process::Command::new(&exe).arg("--version").output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "difftower 0.1.0");
        // Already the latest: nothing to offer.
        assert_eq!(super::check(), None);
    }
}
