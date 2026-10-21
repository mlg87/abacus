//! Release check and self-update (v0.2). Releases are `vX.Y.Z` tags that CI
//! cuts on merge to main from the version each PR sets in Cargo.toml, so the
//! tagged commit always builds the version its tag names.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::app::AppEvent;

const REPO_URL: &str = env!("CARGO_PKG_REPOSITORY");
/// A session left open for days still learns about a release; one
/// `git ls-remote` an hour costs nothing.
const CHECK_EVERY: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl Version {
    /// Plain `X.Y.Z`; pre-release and build suffixes are not releases here.
    pub fn parse(s: &str) -> Option<Version> {
        let mut parts = s.split('.').map(|p| p.parse::<u64>().ok());
        let v = Version {
            major: parts.next()??,
            minor: parts.next()??,
            patch: parts.next()??,
        };
        parts.next().is_none().then_some(v)
    }

    pub fn current() -> Version {
        Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo.toml version is plain X.Y.Z")
    }
}

/// Renders as the release tag, `vX.Y.Z`.
impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Update state the bottom bar shows; `None` on the app until a newer release is seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Available(Version),
    Installing(Version),
    /// Installed on disk; this process still runs the old binary.
    Installed(Version),
}

/// State after the checker reports `latest`. The checker keeps reporting the
/// same release every hour, and this process stays on the old binary after an
/// install, so an install in flight or an offer or install of `latest` (or
/// newer) must stand rather than turn back into an offer.
pub fn offer(state: Option<Status>, latest: Version) -> Status {
    match state {
        Some(s @ Status::Installing(_)) => s,
        Some(s @ (Status::Available(v) | Status::Installed(v))) if v >= latest => s,
        _ => Status::Available(latest),
    }
}

/// Highest `vX.Y.Z` in `git ls-remote --tags --refs` output, compared numerically.
fn latest_tag(ls_remote: &str) -> Option<Version> {
    ls_remote
        .lines()
        .filter_map(|l| l.split_once("\trefs/tags/v"))
        .filter_map(|(_, v)| Version::parse(v))
        .max()
}

/// git is already required, so the check needs no HTTP client or GitHub API
/// token, and has no API rate limit to hit.
fn latest_release() -> Result<Option<Version>> {
    let out = Command::new("git")
        .args(["ls-remote", "--tags", "--refs", REPO_URL, "v*"])
        // A credential prompt would write into the TUI's raw-mode terminal.
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .context("failed to run git ls-remote")?;
    if !out.status.success() {
        bail!(
            "git ls-remote {REPO_URL} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(latest_tag(&String::from_utf8_lossy(&out.stdout)))
}

/// Reports a release newer than this build now and then hourly. Failures
/// (offline, GitHub down) stay silent: the next round tries again.
pub fn spawn_checker(tx: Sender<AppEvent>) {
    let current = Version::current();
    std::thread::spawn(move || {
        loop {
            if let Ok(Some(latest)) = latest_release()
                && latest > current
                && tx.send(AppEvent::UpdateAvailable(latest)).is_err()
            {
                return;
            }
            std::thread::sleep(CHECK_EVERY);
        }
    });
}

/// Cargo install root that holds `exe` (`<root>/bin/abacus` next to cargo's
/// `.crates.toml`). Installing there replaces the binary on the user's PATH
/// even when it lives outside the default `~/.cargo`.
fn install_root(exe: &Path) -> Option<PathBuf> {
    let bin = exe
        .parent()
        .filter(|d| d.file_name() == Some("bin".as_ref()))?;
    let root = bin.parent()?;
    root.join(".crates.toml")
        .is_file()
        .then(|| root.to_path_buf())
}

/// `cargo install` of release `v`. Builds from the tag, since releases ship
/// source only.
pub fn install_command(v: Version) -> Command {
    let mut cmd = Command::new("cargo");
    cmd.args([
        "install",
        "--locked",
        "--git",
        REPO_URL,
        "--tag",
        &v.to_string(),
    ]);
    if let Some(root) = std::env::current_exe()
        .ok()
        .as_deref()
        .and_then(install_root)
    {
        cmd.arg("--root").arg(root);
    }
    cmd
}

pub fn command_line(cmd: &Command) -> String {
    std::iter::once(cmd.get_program())
        .chain(cmd.get_args())
        .map(|a| a.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Installs `v` off the UI thread. The build takes minutes, and its output
/// goes to `log` because it cannot share the TUI's terminal.
pub fn spawn_install(v: Version, log: PathBuf, tx: Sender<AppEvent>) {
    std::thread::spawn(move || {
        let result = install(v, &log).map_err(|e| format!("{e:#}"));
        let _ = tx.send(AppEvent::UpdateInstalled { version: v, result });
    });
}

fn install(v: Version, log: &Path) -> Result<()> {
    let file =
        std::fs::File::create(log).with_context(|| format!("cannot write {}", log.display()))?;
    let mut cmd = install_command(v);
    let line = command_line(&cmd);
    let status = cmd
        .stdin(Stdio::null())
        .stdout(file.try_clone()?)
        .stderr(file)
        .status()
        .with_context(|| format!("could not run cargo; run it yourself: {line}"))?;
    if !status.success() {
        bail!("cargo install failed ({status}); see {}", log.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn latest_tag_compares_numerically_and_skips_non_release_tags() {
        let out = "a\trefs/tags/v0.9.0\n\
                   b\trefs/tags/v0.10.0\n\
                   c\trefs/tags/v1.0.0-rc.1\n\
                   d\trefs/tags/vnext\n\
                   e\trefs/tags/v0.10\n";
        assert_eq!(latest_tag(out), Some(v("0.10.0")));
        assert_eq!(latest_tag(""), None);
    }

    #[test]
    fn a_seen_release_does_not_reoffer_what_is_installing_or_installed() {
        let (old, new) = (v("0.3.0"), v("0.4.0"));
        assert_eq!(offer(None, new), Status::Available(new));
        assert_eq!(
            offer(Some(Status::Available(old)), new),
            Status::Available(new)
        );
        assert_eq!(
            offer(Some(Status::Installing(old)), new),
            Status::Installing(old)
        );
        assert_eq!(
            offer(Some(Status::Installed(new)), new),
            Status::Installed(new)
        );
        assert_eq!(
            offer(Some(Status::Installed(old)), new),
            Status::Available(new)
        );
        assert_eq!(
            offer(Some(Status::Available(new)), old),
            Status::Available(new)
        );
    }

    #[test]
    fn install_root_is_the_cargo_root_holding_the_binary() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("bin")).unwrap();
        let exe = root.path().join("bin/abacus");
        // Not a cargo root until cargo has recorded an install there.
        assert_eq!(install_root(&exe), None);
        std::fs::write(root.path().join(".crates.toml"), "").unwrap();
        assert_eq!(install_root(&exe), Some(root.path().to_path_buf()));
        assert_eq!(install_root(&root.path().join("target/debug/abacus")), None);
    }
}
