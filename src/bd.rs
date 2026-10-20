use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};

use crate::model::Bead;

pub fn run(repo: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("bd")
        .args(args)
        .current_dir(repo)
        .output()
        .context("failed to run bd")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let first = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        bail!(
            "bd {} failed: {}",
            args.first().unwrap_or(&""),
            first.trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Returns the `.beads` directory. `bd where` resolves to the primary checkout even
/// from a worktree, so its parent is the repo root abacus treats as canonical.
pub fn beads_dir(start: &Path) -> Result<PathBuf> {
    let out = run(start, &["where", "--json"])?;
    let v: serde_json::Value = serde_json::from_str(&out).context("bd where: invalid json")?;
    let path = v
        .get("path")
        .and_then(|p| p.as_str())
        .ok_or_else(|| anyhow!("bd where: no path in output"))?;
    Ok(PathBuf::from(path))
}

pub fn parse_list(out: &str) -> Result<Vec<Bead>> {
    let trimmed = out.trim();
    if trimmed.is_empty() || trimmed == "null" {
        return Ok(Vec::new());
    }
    serde_json::from_str(trimmed).context("bd list: invalid json")
}

pub fn list_all(repo: &Path) -> Result<Vec<Bead>> {
    parse_list(&run(
        repo,
        &["--readonly", "list", "--all", "--json", "--limit", "0"],
    )?)
}

pub fn claim(repo: &Path, id: &str, note: &str) -> Result<()> {
    run(repo, &["update", id, "--claim", "--append-notes", note]).map(drop)
}

pub fn release(repo: &Path, id: &str, note: &str) -> Result<()> {
    run(
        repo,
        &[
            "update",
            id,
            "--status",
            "open",
            "--assignee",
            "",
            "--append-notes",
            note,
        ],
    )
    .map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_null_list_output_is_no_beads() {
        assert!(parse_list("").unwrap().is_empty());
        assert!(parse_list("null\n").unwrap().is_empty());
        assert_eq!(parse_list(r#"[{"id":"a"}]"#).unwrap().len(), 1);
    }
}
