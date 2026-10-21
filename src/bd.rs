use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};

use crate::model::{Bead, Status};

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

pub fn release(repo: &Path, ids: &[&str], note: &str) -> Result<()> {
    let mut args = vec!["update"];
    args.extend_from_slice(ids);
    args.extend_from_slice(&["--status", "open", "--assignee", "", "--append-notes", note]);
    run(repo, &args).map(drop)
}

/// `bd show --json` nests dependencies in a different shape than `bd list`, so
/// only the fields a release decision needs are read.
#[derive(serde::Deserialize)]
struct ShownStatus {
    id: String,
    #[serde(default)]
    status: Status,
}

/// Reopens whichever of `ids` are still `in_progress` right now. Reads fresh state
/// rather than trusting the last `bd list`, because an agent may have closed a
/// bead moments before its run ended and a blind release would reopen it.
pub fn release_still_claimed(repo: &Path, ids: &[String], note: &str) -> Result<Vec<String>> {
    let mut args = vec!["--readonly", "show"];
    args.extend(ids.iter().map(String::as_str));
    args.push("--json");
    let held = still_claimed(&run(repo, &args)?)?;
    if !held.is_empty() {
        let refs: Vec<&str> = held.iter().map(String::as_str).collect();
        release(repo, &refs, note)?;
    }
    Ok(held)
}

fn still_claimed(show_json: &str) -> Result<Vec<String>> {
    let shown: Vec<ShownStatus> =
        serde_json::from_str(show_json.trim()).context("bd show: invalid json")?;
    Ok(shown
        .into_iter()
        .filter(|b| b.status == Status::InProgress)
        .map(|b| b.id)
        .collect())
}

/// `bd delete` also drops every dependency link touching the deleted beads, so
/// beads they blocked become unblocked without further edits.
pub fn delete(repo: &Path, ids: &[String]) -> Result<()> {
    let mut args = vec!["delete"];
    args.extend(ids.iter().map(String::as_str));
    args.push("--force");
    run(repo, &args).map(drop)
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

    /// Shape captured from `bd show --json` (bd 1.2.2): dependencies carry `id` and
    /// `dependency_type`, not `bd list`'s `depends_on_id`/`type`.
    #[test]
    fn still_claimed_reads_bd_show_output_with_dependencies() {
        let out = r#"[{"id":"sm-c6w","status":"in_progress","dependencies":[{"id":"sm-7bp","status":"closed","dependency_type":"blocks"}]},{"id":"sm-u97","status":"closed","dependencies":[]}]"#;
        assert_eq!(still_claimed(out).unwrap(), ["sm-c6w"]);
    }
}
